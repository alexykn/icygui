//! The configured environments (ENV-01, ENV-02): adding and editing them,
//! switching the active one, removing one, and pinning a server's
//! certificate (ENV-05).
//!
//! These methods change the settings and the state's view of the
//! connection; starting and stopping engines, the keychain and event logs
//! are `crate::live::Session`'s, which calls them.

use std::sync::Arc;

use ic_config::Environment;

use super::{AppState, ConnectionStatus, endpoint_of, user_of};

/// What saving an edited environment means for the running engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EnvironmentSaved {
    /// A new environment that became the active one (there was none):
    /// start an engine for it.
    AddedActive,
    /// A new environment; the active one stays.
    Added,
    /// The active environment's connection (URL, authentication, TLS or
    /// password) changed: restart its engine.
    Reconnect,
    /// The active environment changed in place (name, author): the core
    /// was told.
    InPlace,
    /// An inactive environment changed.
    Inactive,
    /// Nothing changed.
    Unchanged,
}

impl EnvironmentSaved {
    /// Whether an engine has to (re)start for the active environment.
    pub(crate) fn needs_engine(self) -> bool {
        matches!(self, Self::AddedActive | Self::Reconnect)
    }
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
    /// The first environment becomes the active one.
    pub(crate) fn save_environment(
        &mut self,
        edited: Environment,
        password_changed: bool,
    ) -> EnvironmentSaved {
        let active = self.config.active_environment.clone();
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
                self.config.active_environment = Some(id);
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
        updated.url = edited.url;
        updated.auth = edited.auth;
        updated.tls = edited.tls;
        updated.author = edited.author;
        if updated == *current && !password_changed {
            return EnvironmentSaved::Unchanged;
        }
        tracing::info!(environment = %updated.name, reconnect, "environment changed");
        *current = updated;
        let is_active = active.as_deref() == Some(edited.id.as_str());
        if !is_active {
            self.save_config();
            EnvironmentSaved::Inactive
        } else if reconnect {
            self.save_config();
            EnvironmentSaved::Reconnect
        } else {
            self.environment_changed();
            EnvironmentSaved::InPlace
        }
    }

    /// Makes `id` the active environment: the snapshot, permissions and
    /// notifications of the old one are dropped, and its selected
    /// dashboard and tabs come back from the UI state. Returns whether it
    /// changed (an unknown id changes nothing). The caller restarts the
    /// engine.
    pub(crate) fn switch_environment(&mut self, id: &str) -> bool {
        if self.config.environment(id).is_none()
            || self.config.active_environment.as_deref() == Some(id)
        {
            return false;
        }
        self.remember_environment_ui();
        self.config.active_environment = Some(id.to_owned());
        tracing::info!(environment = ?self.environment().map(|e| e.name.clone()), "switching environment");
        self.save_config();
        self.reset_connection();
        true
    }

    /// Removes the environment `id` and its UI state. Returns whether it
    /// was the active one (the first remaining one is active then), or
    /// `None` for an unknown id. The caller deletes its password and event
    /// log and, if it was active, restarts the engine.
    pub(crate) fn remove_environment(&mut self, id: &str) -> Option<bool> {
        let index = self
            .config
            .environments
            .iter()
            .position(|environment| environment.id == id)?;
        let removed = self.config.environments.remove(index);
        tracing::info!(environment = %removed.name, "environment removed");
        self.ui.retain_environments(|kept| kept != id);
        let was_active = self.config.active_environment.as_deref() == Some(id);
        if was_active {
            self.config.active_environment = self
                .config
                .environments
                .first()
                .map(|environment| environment.id.clone());
            self.reset_connection();
        }
        self.save_config();
        self.save_ui();
        Some(was_active)
    }

    /// Pins `fingerprint` as the only certificate environment `id`
    /// trusts (trust on first use). Returns whether that changed; the
    /// caller restarts the engine if it's the active environment.
    pub(crate) fn pin_certificate(&mut self, id: &str, fingerprint: &str) -> bool {
        let Some(environment) = self.config.environment_mut(id) else {
            return false;
        };
        if environment.tls.pinned_sha256.as_deref() == Some(fingerprint) {
            return false;
        }
        tracing::info!(environment = %environment.name, %fingerprint, "certificate pinned");
        environment.tls.pinned_sha256 = Some(fingerprint.to_owned());
        self.save_config();
        true
    }

    /// Starts over with the active environment's connection (another
    /// environment, or its engine restarts): no objects, permissions or
    /// notifications yet, its selected dashboard and tabs from the UI
    /// state.
    pub(crate) fn reset_connection(&mut self) {
        self.snapshot = Arc::default();
        self.permissions = None;
        self.hydration.forget();
        self.update_pending = false;
        self.last_refresh = None;
        self.notifications.clear();
        self.unread = 0;
        self.paused_until = None;
        self.requested = None;
        self.last_request = None;
        self.last_denial = None;
        self.tracker.clear();
        self.restore_environment_ui();
        self.connection = match self.environment() {
            Some(environment) => {
                ConnectionStatus::starting(&endpoint_of(environment), user_of(environment))
            }
            None => ConnectionStatus::idle(),
        };
        #[cfg(test)]
        self.evaluate_fixture_all();
    }
}

/// Whether two versions of an environment connect differently: another
/// id, URL, authentication or TLS setting.
pub(crate) fn connection_differs(old: &Environment, new: &Environment) -> bool {
    old.id != new.id || old.url != new.url || old.auth != new.auth || old.tls != new.tls
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
            url: "https://master-02:5665".to_owned(),
            ..renamed.clone()
        };
        assert_eq!(
            state.save_environment(moved, false),
            EnvironmentSaved::Reconnect
        );
        let mut pinned = state.environment().unwrap().clone();
        pinned.tls.pinned_sha256 = Some("AB".repeat(32));
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
        assert!(state.pin_certificate(&id, &fingerprint));
        assert!(!state.pin_certificate(&id, &fingerprint));
        assert!(!state.pin_certificate("missing", &fingerprint));
        assert_eq!(
            state.environment().unwrap().tls.pinned_sha256.as_deref(),
            Some(fingerprint.as_str())
        );
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
