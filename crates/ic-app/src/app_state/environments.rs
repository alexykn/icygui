//! The configured environments (ENV-01, ENV-02): adding and editing them,
//! switching the active one, removing one, and pinning a server's
//! certificate (ENV-05).
//!
//! These methods change the settings and the state's view of the
//! connection; starting and stopping engines, the keychain and event logs
//! are `crate::live::Session`'s, which calls them.

use ic_config::Environment;

use super::{AppState, EngineSlot};

/// What saving an edited environment means for its engine (every
/// environment runs one).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EnvironmentSaved {
    /// A new environment that became the active one (there was none):
    /// start its engine.
    AddedActive,
    /// A new environment; the active one stays. Start its engine.
    Added,
    /// The environment's connection (URL, authentication, TLS or password)
    /// changed: restart its engine.
    Reconnect,
    /// The environment changed in place (name, author): its engine was
    /// told.
    InPlace,
    /// Nothing changed.
    Unchanged,
}

impl AppState {
    /// Every configured environment, in settings order.
    pub(crate) fn environments(&self) -> &[Environment] {
        &self.config.environments
    }

    /// The active environment's id.
    pub(crate) fn active_environment_id(&self) -> Option<&str> {
        self.environment()
            .map(|environment| environment.id.as_str())
    }

    /// An environment by id.
    pub(crate) fn environment_by_id(&self, id: &str) -> Option<&Environment> {
        self.config.environment(id)
    }

    /// Saves an environment from the editor: adds it, or takes over its
    /// name, URL, authentication, TLS settings and author into the
    /// environment with its id, keeping that one's dashboards and
    /// notification rules. `password_changed` says a new password went to
    /// the keychain (which also needs a reconnect).
    ///
    /// The first environment becomes the active one. The caller starts or
    /// restarts the environment's engine as the answer says.
    pub(crate) fn save_environment(
        &mut self,
        edited: Environment,
        password_changed: bool,
    ) -> EnvironmentSaved {
        let Some(index) = self
            .config
            .environments
            .iter()
            .position(|environment| environment.id == edited.id)
        else {
            let id = edited.id.clone();
            tracing::info!(environment = %edited.name, "environment added");
            self.config.environments.push(edited);
            let saved = if self.environment().is_none() {
                self.activate(Some(id));
                self.reset_connection();
                EnvironmentSaved::AddedActive
            } else {
                EnvironmentSaved::Added
            };
            self.save_config();
            return saved;
        };
        let current = &mut self.config.environments[index];
        let reconnect = connection_differs(current, &edited) || password_changed;
        let mut updated = current.clone();
        updated.name = edited.name;
        updated.urls = edited.urls;
        updated.auth = edited.auth;
        updated.tls = edited.tls;
        updated.author = edited.author;
        if updated == *current && !password_changed {
            return EnvironmentSaved::Unchanged;
        }
        tracing::info!(environment = %updated.name, reconnect, "environment changed");
        *current = updated;
        self.save_config();
        if reconnect {
            EnvironmentSaved::Reconnect
        } else {
            // Its rules carry the name (storm summaries).
            self.send_environment_to(&edited.id);
            EnvironmentSaved::InPlace
        }
    }

    /// Makes `id` the active environment (ENV-01): its engine has been
    /// running all along (PLAN.md D2), so its snapshot, connection and
    /// notifications show at once; the old one's are kept for when it
    /// comes back, and its engine keeps watching and notifying. The
    /// selected dashboard and tabs come back from the UI state. Returns
    /// whether it changed (an unknown id changes nothing).
    pub(crate) fn switch_environment(&mut self, id: &str) -> bool {
        if self.config.environment(id).is_none()
            || self.config.active_environment.as_deref() == Some(id)
        {
            return false;
        }
        self.remember_environment_ui();
        self.activate(Some(id.to_owned()));
        tracing::info!(environment = ?self.environment().map(|e| e.name.clone()), "switching environment");
        self.save_config();
        self.forget_window_requests();
        self.restore_environment_ui();
        self.announce_active();
        #[cfg(test)]
        self.evaluate_fixture_all();
        true
    }

    /// Removes the environment `id`, its UI state and its pause. Returns
    /// whether it was the active one (the first remaining one is active
    /// then, at once if its engine runs), or `None` for an unknown id. The
    /// caller stops its engine first ([`AppState::take_core_of`]), then
    /// deletes its password and event log.
    pub(crate) fn remove_environment(&mut self, id: &str) -> Option<bool> {
        let index = self
            .config
            .environments
            .iter()
            .position(|environment| environment.id == id)?;
        let was_active = self.config.active_environment.as_deref() == Some(id);
        if let Some(core) = self.take_core_of(id) {
            tracing::warn!("the removed environment's engine was still linked; it stops now");
            drop(core);
        }
        let removed = self.config.environments.remove(index);
        tracing::info!(environment = %removed.name, "environment removed");
        self.ui.retain_environments(|kept| kept != id);
        self.environment_pauses.remove(id);
        self.parked.remove(id);
        if was_active {
            let next = self
                .config
                .environments
                .first()
                .map(|environment| environment.id.clone());
            self.config.active_environment = None;
            self.activate(next);
            self.forget_window_requests();
            self.restore_environment_ui();
            self.announce_active();
        }
        self.save_config();
        self.save_ui();
        Some(was_active)
    }

    /// Pins `fingerprint` as the only certificate environment `id`
    /// trusts at its URL `url` (trust on first use; pins are per URL,
    /// ENV-12). Returns whether that changed (an unknown environment or
    /// URL changes nothing); the caller restarts the engine if it's the
    /// active environment.
    pub(crate) fn pin_certificate(&mut self, id: &str, url: &str, fingerprint: &str) -> bool {
        let Some(environment) = self.config.environment_mut(id) else {
            return false;
        };
        let name = environment.name.clone();
        let Some(entry) = environment
            .urls
            .iter_mut()
            .find(|entry| entry.url.trim() == url.trim())
        else {
            return false;
        };
        if entry.pinned_sha256.as_deref() == Some(fingerprint) {
            return false;
        }
        tracing::info!(environment = %name, url = %entry.label(), %fingerprint, "certificate pinned");
        entry.pinned_sha256 = Some(fingerprint.to_owned());
        self.save_config();
        true
    }

    /// Starts over with the active environment's connection (its engine
    /// restarts): no objects, permissions or notifications yet, its
    /// selected dashboard and tabs from the UI state. The link to the
    /// engine stays (the session replaces it).
    pub(crate) fn reset_connection(&mut self) {
        let core = self.engine.core.take();
        self.engine = match self.environment() {
            Some(environment) => EngineSlot::starting(environment),
            None => EngineSlot::idle(),
        };
        self.engine.core = core;
        // The pauses are the app's: the next engine gets them
        // (`set_core_for`).
        self.forget_window_requests();
        self.restore_environment_ui();
        #[cfg(test)]
        self.evaluate_fixture_all();
    }

    /// Forgets what the window asked for in the environment that was on
    /// screen: a waiting action request and the actions' markers and
    /// toasts.
    fn forget_window_requests(&mut self) {
        self.requested = None;
        self.last_request = None;
        self.last_denial = None;
        self.tracker.clear();
    }
}

/// An environment's URLs in a line: the first as written, and how many
/// more follow (`https://master-01:5665 + 2 more`).
pub(crate) fn url_summary(environment: &Environment) -> String {
    let first = environment.primary_url().to_owned();
    match environment.urls.len() {
        0 | 1 => first,
        count => format!("{first} + {} more", count - 1),
    }
}

/// Whether two versions of an environment connect differently: another
/// id, URLs (their order, pins and server names), authentication or TLS
/// setting.
pub(crate) fn connection_differs(old: &Environment, new: &Environment) -> bool {
    old.id != new.id || old.connection_differs(new)
}

#[cfg(test)]
mod tests {
    use ic_config::{AuthConfig, Paths, UiState};
    use ic_model::Timestamp;

    use super::*;
    use crate::app_state::testing::Recorder;
    use crate::persist::Persistence;

    fn now() -> Timestamp {
        Timestamp::from_unix_seconds(1_790_000_000.)
    }

    fn basic(name: &str, url: &str) -> Environment {
        Environment::new(
            name,
            url,
            AuthConfig::Basic {
                username: "icygui".to_owned(),
            },
        )
    }

    fn live(dir: &std::path::Path) -> (AppState, Paths) {
        let paths = Paths::in_dir(dir);
        let mut state = AppState::live(ic_config::Config::default(), UiState::default(), now());
        let persistence =
            Persistence::start(paths.config_store(), paths.state_store(), Box::new(|_| {}))
                .unwrap();
        state.set_persistence(persistence);
        (state, paths)
    }

    #[test]
    fn the_first_environment_becomes_active_and_is_saved() {
        let dir = tempfile::tempdir().unwrap();
        let (mut state, paths) = live(dir.path());
        assert!(state.environment().is_none());
        let prod = basic("prod", "https://master-01:5665");
        let prod_id = prod.id.clone();
        assert_eq!(
            state.save_environment(prod, true),
            EnvironmentSaved::AddedActive
        );
        assert_eq!(state.active_environment_id(), Some(prod_id.as_str()));
        assert!(state.connection().is_starting(), "connects next");
        assert!(
            state.selected().is_some(),
            "its default dashboards are shown"
        );
        let staging = basic("staging", "https://staging:5665");
        assert_eq!(
            state.save_environment(staging, true),
            EnvironmentSaved::Added
        );
        assert_eq!(state.active_environment_id(), Some(prod_id.as_str()));
        assert!(state.flush_persistence(std::time::Duration::from_secs(5)));
        let saved = paths.config_store().load().unwrap();
        assert_eq!(saved.environments.len(), 2);
        assert_eq!(saved.active_environment.as_deref(), Some(prod_id.as_str()));
    }

    #[test]
    fn edits_reconnect_only_for_connection_changes() {
        let (mut state, recorder) = {
            let mut state = AppState::live(ic_config::Config::default(), UiState::default(), now());
            let recorder = Recorder::default();
            state.set_core(Box::new(recorder.clone()));
            (state, recorder)
        };
        let prod = basic("prod", "https://master-01:5665");
        state.save_environment(prod.clone(), false);
        let groups = state.environment().unwrap().groups.clone();

        // Only the name: in place, the core is told.
        let renamed = Environment {
            name: "production".to_owned(),
            groups: Vec::new(),
            ..prod.clone()
        };
        assert_eq!(
            state.save_environment(renamed.clone(), false),
            EnvironmentSaved::InPlace
        );
        assert_eq!(recorder.sent(), ["UpdateEnvironment(production)"]);
        assert_eq!(
            state.environment().unwrap().groups,
            groups,
            "the editor never changes the dashboards"
        );
        assert_eq!(
            state.save_environment(renamed.clone(), false),
            EnvironmentSaved::Unchanged
        );
        assert_eq!(
            state.save_environment(renamed.clone(), true),
            EnvironmentSaved::Reconnect
        );

        let moved = Environment {
            urls: vec![ic_config::ApiUrl::new("https://master-02:5665")],
            ..renamed.clone()
        };
        assert_eq!(
            state.save_environment(moved, false),
            EnvironmentSaved::Reconnect
        );
        let mut pinned = state.environment().unwrap().clone();
        pinned.urls[0].pinned_sha256 = Some("AB".repeat(32));
        assert_eq!(
            state.save_environment(pinned, false),
            EnvironmentSaved::Reconnect
        );

        let other = basic("lab", "https://lab:5665");
        state.save_environment(other.clone(), false);
        let other_renamed = Environment {
            name: "lab-2".to_owned(),
            ..other
        };
        assert_eq!(
            state.save_environment(other_renamed, false),
            EnvironmentSaved::Inactive
        );
        assert!(EnvironmentSaved::Reconnect.needs_engine());
        assert!(!EnvironmentSaved::InPlace.needs_engine());
    }

    #[test]
    fn switching_restores_each_environments_selection() {
        let dir = tempfile::tempdir().unwrap();
        let (mut state, _) = live(dir.path());
        let prod = basic("prod", "https://master-01:5665");
        let staging = basic("staging", "https://staging:5665");
        let (prod_id, staging_id) = (prod.id.clone(), staging.id.clone());
        state.save_environment(prod, false);
        state.save_environment(staging, false);
        // Select prod's second dashboard and open a tab.
        let second = state.dashboard_at(2).unwrap();
        state.select(second.clone());
        state.open_tab(ic_model::ObjectKey::host("db-prod-03"));
        state.set_snapshot(Arc::new(ic_core::snapshot::Snapshot {
            revision: 7,
            ..ic_core::snapshot::Snapshot::default()
        }));

        assert!(state.switch_environment(&staging_id));
        assert!(!state.switch_environment(&staging_id), "already active");
        assert!(!state.switch_environment("missing"));
        assert_eq!(state.active_environment_id(), Some(staging_id.as_str()));
        assert_eq!(state.snapshot().revision, 0, "the old objects are gone");
        assert!(state.tabs().is_empty());
        assert_eq!(state.selected(), state.dashboard_at(1).as_ref());
        assert!(state.connection().is_starting());

        assert!(state.switch_environment(&prod_id));
        assert_eq!(state.selected(), Some(&second), "prod's selection is back");
        assert_eq!(state.tabs(), [ic_model::ObjectKey::host("db-prod-03")]);
    }

    /// Only the active environment notifies (D2): a switch says which
    /// environment went quiet, every time.
    #[test]
    fn switching_says_which_environment_stopped_notifying() {
        let dir = tempfile::tempdir().unwrap();
        let (mut state, _) = live(dir.path());
        let prod = basic("prod", "https://master-01:5665");
        let staging = basic("staging", "https://staging:5665");
        let (prod_id, staging_id) = (prod.id.clone(), staging.id.clone());
        state.save_environment(prod, false);
        state.save_environment(staging, false);
        assert!(state.notice().is_none());

        assert!(state.switch_environment(&staging_id));
        let notice = state.notice().unwrap();
        assert_eq!(notice.title, "prod is no longer watched.");
        assert!(!notice.problem);
        let detail = notice.detail.as_deref().unwrap();
        assert!(detail.contains("Only the active environment"), "{detail}");
        assert!(detail.contains("nothing from prod notifies"), "{detail}");

        assert!(!state.switch_environment(&staging_id));
        assert_eq!(
            state.notice().unwrap().title,
            "prod is no longer watched.",
            "no switch, no new notice"
        );
        assert!(state.switch_environment(&prod_id));
        assert_eq!(
            state.notice().unwrap().title,
            "staging is no longer watched."
        );
    }

    #[test]
    fn removing_the_active_environment_activates_the_next() {
        let dir = tempfile::tempdir().unwrap();
        let (mut state, paths) = live(dir.path());
        let prod = basic("prod", "https://master-01:5665");
        let staging = basic("staging", "https://staging:5665");
        let (prod_id, staging_id) = (prod.id.clone(), staging.id.clone());
        state.save_environment(prod, false);
        state.save_environment(staging, false);
        assert_eq!(state.remove_environment(&staging_id), Some(false));
        assert_eq!(state.remove_environment(&staging_id), None);
        assert_eq!(state.active_environment_id(), Some(prod_id.as_str()));
        let lab = basic("lab", "https://lab:5665");
        let lab_id = lab.id.clone();
        state.save_environment(lab, false);
        assert_eq!(state.remove_environment(&prod_id), Some(true));
        assert_eq!(state.active_environment_id(), Some(lab_id.as_str()));
        assert_eq!(state.remove_environment(&lab_id), Some(true));
        assert!(state.environment().is_none());
        assert!(state.selected().is_none());
        assert!(state.flush_persistence(std::time::Duration::from_secs(5)));
        let saved = paths.config_store().load().unwrap();
        assert!(saved.environments.is_empty());
        assert_eq!(saved.active_environment, None);
    }

    #[test]
    fn pinning_records_the_fingerprint_once() {
        let mut state = AppState::live(ic_config::Config::default(), UiState::default(), now());
        let prod = basic("prod", "https://master-01:5665");
        let id = prod.id.clone();
        state.save_environment(prod, false);
        let fingerprint = ic_config::format_fingerprint(&[0xab; 32]);
        let url = "https://master-01:5665";
        assert!(state.pin_certificate(&id, url, &fingerprint));
        assert!(!state.pin_certificate(&id, url, &fingerprint));
        assert!(!state.pin_certificate("missing", url, &fingerprint));
        assert!(
            !state.pin_certificate(&id, "https://master-02:5665", &fingerprint),
            "a URL the environment doesn't list"
        );
        assert_eq!(
            state.environment().unwrap().urls[0]
                .pinned_sha256
                .as_deref(),
            Some(fingerprint.as_str())
        );
    }

    #[test]
    fn url_summaries_name_the_first_and_count_the_rest() {
        let mut prod = basic("prod", "https://master-01:5665");
        assert_eq!(url_summary(&prod), "https://master-01:5665");
        prod.urls
            .push(ic_config::ApiUrl::new("https://master-02:5665"));
        prod.urls
            .push(ic_config::ApiUrl::new("https://sat-ams-01:5665"));
        assert_eq!(url_summary(&prod), "https://master-01:5665 + 2 more");
    }

    #[test]
    fn connection_changes_are_told_apart() {
        let prod = basic("prod", "https://master-01:5665");
        let renamed = Environment {
            name: "x".to_owned(),
            author: Some("me".to_owned()),
            ..prod.clone()
        };
        assert!(!connection_differs(&prod, &renamed));
        let other_auth = Environment {
            auth: AuthConfig::Basic {
                username: "root".to_owned(),
            },
            ..prod.clone()
        };
        assert!(connection_differs(&prod, &other_auth));
    }
}
