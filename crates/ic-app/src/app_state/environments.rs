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
        let quiet = self.engine.quiet && core.is_some();
        self.engine = match self.environment() {
            Some(environment) => EngineSlot::starting(environment),
            None => EngineSlot::idle(),
        };
        self.engine.core = core;
        self.engine.quiet = quiet;
        // The next engine knows nothing yet: the views ask it again for
        // what they show (the opened object, the rows on screen).
        self.wake += 1;
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

    use std::sync::Arc;

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
        let mut state = AppState::live(ic_config::Config::default(), UiState::default(), now());
        let prod = basic("prod", "https://master-01:5665");
        state.save_environment(prod.clone(), false);
        let recorder = Recorder::default();
        state.set_core(Box::new(recorder.clone()));
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
        // Another environment's engine runs too: it is told in place.
        assert_eq!(
            state.save_environment(other_renamed, false),
            EnvironmentSaved::InPlace
        );
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
        assert_eq!(
            state.snapshot().revision,
            0,
            "staging's engine sent nothing yet"
        );
        assert!(state.tabs().is_empty());
        assert_eq!(state.selected(), state.dashboard_at(1).as_ref());
        assert!(state.connection().is_starting());
        assert!(state.notice().is_none(), "nothing stops notifying (D2)");

        assert!(state.switch_environment(&prod_id));
        assert_eq!(state.selected(), Some(&second), "prod's selection is back");
        assert_eq!(state.tabs(), [ic_model::ObjectKey::host("db-prod-03")]);
        assert_eq!(
            state.snapshot().revision,
            7,
            "prod's objects are back at once"
        );
    }

    /// Every environment's engine runs (D2): what the inactive one
    /// reports is kept for it, and shows as soon as it is switched to.
    #[test]
    fn an_inactive_environment_keeps_what_its_engine_reports() {
        let mut state = AppState::live(ic_config::Config::default(), UiState::default(), now());
        let production = basic("prod", "https://master-01:5665");
        let staging = basic("staging", "https://staging:5665");
        let (prod_id, staging_id) = (production.id.clone(), staging.id.clone());
        state.save_environment(production, false);
        state.save_environment(staging, false);
        let prod = Recorder::default();
        state.set_core_for(&prod_id, Box::new(prod.clone()));
        assert!(prod.sent().is_empty(), "the active engine stays on screen");
        let staging_core = Recorder::default();
        state.set_core_for(&staging_id, Box::new(staging_core.clone()));
        assert_eq!(
            staging_core.sent(),
            ["SetActive(false)", "SetQuiet(true)"],
            "an engine off screen is told so, and that nobody looks (PERF-09)"
        );
        state.apply_from(
            &staging_id,
            ic_core::CoreEvent::Snapshot(Arc::new(ic_core::snapshot::Snapshot {
                revision: 3,
                ..ic_core::snapshot::Snapshot::default()
            })),
        );
        state.apply_from(
            &staging_id,
            ic_core::CoreEvent::Connection(ic_core::ConnectionState::Connected {
                node: crate::app_state::connection::full_node("staging"),
                version: "r2.15.6-1".to_owned(),
                since: now(),
            }),
        );
        assert_eq!(state.snapshot().revision, 0, "the window still shows prod");
        assert!(!state.connection().is_connected());
        assert_eq!(state.snapshot_of(&staging_id).unwrap().revision, 3);

        assert!(state.switch_environment(&staging_id));
        assert_eq!(state.snapshot().revision, 3, "staging shows at once");
        assert!(state.connection().is_connected());
        assert_eq!(
            staging_core.sent(),
            [
                "SetActive(false)",
                "SetQuiet(true)",
                "SetActive(true)",
                "SetQuiet(false)"
            ]
        );
        assert_eq!(prod.sent(), ["SetActive(false)", "SetQuiet(true)"]);
        // Events of the environment now off screen go to its slot.
        state.apply_from(
            &prod_id,
            ic_core::CoreEvent::Snapshot(Arc::new(ic_core::snapshot::Snapshot {
                revision: 9,
                ..ic_core::snapshot::Snapshot::default()
            })),
        );
        assert_eq!(state.snapshot().revision, 3);
        assert_eq!(state.snapshot_of(&prod_id).unwrap().revision, 9);
        // At quit every engine's link goes.
        assert_eq!(state.take_all_cores().len(), 2);
    }

    /// Two environments with a recording engine each, prod on screen and
    /// both connected.
    fn two_engines() -> (AppState, (String, Recorder), (String, Recorder)) {
        let mut state = AppState::live(ic_config::Config::default(), UiState::default(), now());
        let production = basic("prod", "https://master-01:5665");
        let staging = basic("staging", "https://staging:5665");
        let (prod_id, staging_id) = (production.id.clone(), staging.id.clone());
        state.save_environment(production, false);
        state.save_environment(staging, false);
        let (prod_core, staging_core) = (Recorder::default(), Recorder::default());
        state.set_core_for(&prod_id, Box::new(prod_core.clone()));
        state.set_core_for(&staging_id, Box::new(staging_core.clone()));
        for id in [&prod_id, &staging_id] {
            state.apply_from(
                id,
                ic_core::CoreEvent::Connection(ic_core::ConnectionState::Connected {
                    node: crate::app_state::connection::full_node("master"),
                    version: "r2.15.6-1".to_owned(),
                    since: now(),
                }),
            );
        }
        prod_core.clear();
        staging_core.clear();
        (state, (prod_id, prod_core), (staging_id, staging_core))
    }

    fn pause(until: Option<Timestamp>) -> String {
        format!("{:?}", ic_core::Command::PauseNotifications(until))
    }

    /// A5: the pause holds for every environment; an environment's own
    /// mute only for it. Each engine always gets the later of the two,
    /// also an engine that starts later.
    #[test]
    fn pauses_cover_every_environment_or_one() {
        let (mut state, (prod_id, prod), (staging_id, staging)) = two_engines();
        let real_now = Timestamp::now();
        let at = |seconds: f64| Timestamp::from_unix_seconds(real_now.as_unix_seconds() + seconds);
        let (hour, two_hours) = (at(3600.), at(7200.));

        state.pause_notifications(Some(hour));
        assert_eq!(prod.sent(), [pause(Some(hour))]);
        assert_eq!(staging.sent(), [pause(Some(hour))]);
        assert_eq!(state.active_pause(real_now), Some(hour));

        assert!(state.pause_environment(&staging_id, Some(two_hours)));
        assert!(
            !state.pause_environment(&staging_id, Some(two_hours)),
            "no change"
        );
        assert!(!state.pause_environment("missing", Some(hour)));
        assert_eq!(staging.sent().last(), Some(&pause(Some(two_hours))));
        assert_eq!(prod.sent().len(), 1, "prod isn't touched");
        assert_eq!(
            state.environment_paused_until(&staging_id, real_now),
            Some(two_hours)
        );
        assert_eq!(state.environment_paused_until(&prod_id, real_now), None);

        // Resuming every environment leaves staging's own mute.
        state.pause_notifications(None);
        assert_eq!(prod.sent().last(), Some(&pause(None)));
        assert_eq!(staging.sent().last(), Some(&pause(Some(two_hours))));
        assert_eq!(state.active_pause(real_now), None, "prod notifies");
        assert_eq!(
            state.effective_pause(&staging_id, real_now),
            Some(two_hours)
        );

        // Staging's next engine (its connection changed) is muted too.
        let next = Recorder::default();
        state.set_core_for(&staging_id, Box::new(next.clone()));
        assert_eq!(
            next.sent(),
            [
                "SetActive(false)".to_owned(),
                "SetQuiet(true)".to_owned(),
                pause(Some(two_hours))
            ]
        );
        // On screen, the clock shows staging's mute.
        assert!(state.switch_environment(&staging_id));
        assert_eq!(state.active_pause(real_now), Some(two_hours));

        assert!(state.pause_environment(&staging_id, None));
        assert_eq!(next.sent().last(), Some(&pause(None)));
        assert_eq!(state.active_pause(real_now), None);

        // A mute that is over is forgotten; a removed environment's too.
        assert!(state.pause_environment(&prod_id, Some(hour)));
        state.expire_pauses(at(3601.));
        assert_eq!(state.environment_paused_until(&prod_id, real_now), None);
        assert!(state.pause_environment(&prod_id, Some(hour)));
        assert_eq!(state.remove_environment(&prod_id), Some(false));
        assert!(!state.pause_environment(&prod_id, None));
    }

    /// App-wide settings (the event log's retention, the reconcile
    /// interval) reach every environment's engine.
    #[test]
    fn app_wide_settings_reach_every_engine() {
        let (mut state, (_, prod), (_, staging)) = two_engines();
        let general = ic_config::General {
            event_log_retention_hours: 72,
            ..state.config().general.clone()
        };
        assert!(state.set_general(&general));
        for engine in [&prod, &staging] {
            assert_eq!(engine.sent().len(), 1);
            assert!(engine.sent()[0].starts_with("UpdateGeneral("));
        }
    }

    /// A1: an acknowledgement asked for from another environment's
    /// notification goes to that environment's engine, never to the one
    /// on screen, without switching.
    #[test]
    fn an_acknowledgement_goes_to_the_engine_of_its_environment() {
        let (mut state, (prod_id, prod), (staging_id, staging)) = two_engines();
        let object = ic_model::ObjectKey::service("stg-db-01", "disk");
        let request = crate::actions::ActionRequest {
            action: crate::actions::ObjectAction::Acknowledge,
            targets: vec![object.clone()],
            review: false,
        };
        assert!(state.request_in(&staging_id, request.clone()).is_ok());
        assert_eq!(
            state.take_request(),
            Some((request.clone(), Some(staging_id.clone())))
        );
        // A request for the environment on screen stays unbound.
        assert!(state.request_in(&prod_id, request.clone()).is_ok());
        assert_eq!(state.take_request(), Some((request.clone(), None)));

        // A request bound to staging survives a switch; an unbound one not.
        assert!(state.request_in(&staging_id, request.clone()).is_ok());
        assert_eq!(state.drop_unbound_request(), None);
        assert!(state.has_request());
        let _ = state.take_request();

        let spec = crate::operate::ActionSpec::for_objects(
            crate::actions::ObjectAction::Acknowledge,
            ic_model::Action::Acknowledge {
                comment: "on it".to_owned(),
                sticky: false,
                persistent: false,
                expiry: None,
            },
            vec![object.clone()],
        );
        let id = state.submit_in(&staging_id, spec.clone()).unwrap();
        assert_eq!(staging.actions().len(), 1);
        assert_eq!(staging.actions()[0].0, id);
        assert!(prod.actions().is_empty(), "nothing goes to prod");
        assert_eq!(state.active_environment_id(), Some(prod_id.as_str()));
        assert_eq!(
            state.pending_label(&object),
            None,
            "no marker on prod's rows"
        );
        let toast = state.toasts().last().unwrap();
        assert_eq!(toast.title, "Acknowledging disk on stg-db-01 in staging");
        state.apply_from(
            &staging_id,
            ic_core::CoreEvent::ActionFinished {
                id,
                outcome: ic_core::ActionOutcome {
                    ok: 1,
                    failed: Vec::new(),
                    error: None,
                },
            },
        );
        let toast = state.toasts().last().unwrap();
        assert_eq!(toast.title, "Acknowledged disk on stg-db-01 in staging");

        // Not connected, or gone: nothing is sent.
        state.apply_from(
            &staging_id,
            ic_core::CoreEvent::Connection(ic_core::ConnectionState::Connecting { attempt: 1 }),
        );
        assert!(state.submit_in(&staging_id, spec.clone()).is_err());
        assert!(state.remove_environment(&staging_id).is_some());
        assert!(state.submit_in(&staging_id, spec).is_err());
        assert!(state.request_in(&staging_id, request).is_err());
        assert_eq!(staging.actions().len(), 1);
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
