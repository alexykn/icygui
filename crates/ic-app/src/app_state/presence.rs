//! Quiet mode from the app's side (PERF-09): whether anybody looks at an
//! environment, and what the engines are told about it.
//!
//! An environment is *quiet* (`Command::SetQuiet(true)`) when the general
//! setting `quiet_when_hidden` is on (the default) and it isn't on screen:
//! every environment but the active one, and the active one too while the
//! window is hidden (closed to the tray, or out of sight as the system
//! reports it; `crate::background::presence` decides when). A window that
//! is merely unfocused keeps its environment live.
//!
//! Waking an environment up (the window comes back, or it is switched to)
//! counts as a new *wake*: the views ask again for what they show (the
//! opened object first, `Command::Focus`, then the rows on screen,
//! `Command::Hydrate`), whatever they asked for before.
//!
//! Started in the background (launch at login, `--background`), the
//! engines' first loads wait a size-proportional moment
//! (`ic_core::Start::Background`) until the window first shows, which
//! tells every engine to start now (`Command::StartNow`).

use ic_core::{Command, Start};
use ic_model::ObjectKey;

use super::{AppState, EngineSlot};

impl EngineSlot {
    /// Tells the engine whether it is quiet, unless it was told so last.
    /// Returns whether that changed.
    pub(super) fn set_quiet(&mut self, quiet: bool) -> bool {
        if self.quiet == quiet {
            return false;
        }
        self.quiet = quiet;
        self.send(Command::SetQuiet(quiet));
        true
    }

    /// Whether the engine was told to be quiet.
    #[cfg(test)]
    pub(crate) fn is_quiet(&self) -> bool {
        self.quiet
    }
}

impl AppState {
    /// The app started without its window (`--background` with a tray
    /// icon): every environment is quiet, and the engines' first loads
    /// wait until the window shows (`Start::Background`).
    pub(crate) fn start_hidden(&mut self) {
        self.window_hidden = true;
        self.user_present = false;
        self.announce_quiet();
    }

    /// How the engines started now start: in the background until the
    /// window has shown once.
    pub(crate) fn start_mode(&self) -> Start {
        if self.user_present {
            Start::User
        } else {
            Start::Background
        }
    }

    /// Whether the window is hidden (closed, or out of sight long enough).
    pub(crate) fn window_hidden(&self) -> bool {
        self.window_hidden
    }

    /// The window was shown (`false`) or has been hidden (`true`): the
    /// environment on screen follows. The first time the window shows
    /// after a start in the background, every engine starts its first load
    /// now. Returns whether it changed.
    pub(crate) fn set_window_hidden(&mut self, hidden: bool) -> bool {
        if self.window_hidden == hidden {
            return false;
        }
        tracing::debug!(hidden, "window");
        self.window_hidden = hidden;
        if !hidden && !self.user_present {
            self.user_present = true;
            self.send_to_every_engine(|| Command::StartNow);
        }
        self.announce_quiet();
        true
    }

    /// Whether environment `id` should be quiet now.
    pub(crate) fn quiet_wanted(&self, id: &str) -> bool {
        self.config.general.quiet_when_hidden && (!self.is_active(id) || self.window_hidden)
    }

    /// Tells every engine whether it is quiet (only those whose answer
    /// changed). The environment on screen waking up starts a new wake.
    pub(super) fn announce_quiet(&mut self) {
        let enabled = self.config.general.quiet_when_hidden;
        let active = enabled && self.window_hidden;
        if self.engine.set_quiet(active) && !active {
            self.woke();
        }
        for slot in self.parked.values_mut() {
            slot.set_quiet(enabled);
        }
    }

    /// Environment `id`'s engine just started (live, as every engine
    /// does): it is told whether it is quiet.
    pub(super) fn announce_quiet_to(&mut self, id: &str) {
        let quiet = self.quiet_wanted(id);
        if let Some(slot) = self.slot_mut(id) {
            slot.quiet = false;
            slot.set_quiet(quiet);
        }
    }

    /// The environment on screen woke up: what the views asked for before
    /// is asked for again.
    fn woke(&mut self) {
        self.wake += 1;
        self.engine.hydration.forget();
        tracing::debug!(wake = self.wake, "the environment on screen woke up");
    }

    /// Counts the times the environment on screen woke up (or another came
    /// on screen, or its engine started over): views ask again for what
    /// they show when it changes.
    pub(crate) fn wake(&self) -> u64 {
        self.wake
    }

    /// Asks the engine on screen for `key` in full at once, ahead of
    /// everything else (the object the user opens: a pane, a tab, a
    /// notification clicked; `Command::Focus`). Only while connected; the
    /// engine skips it when it holds the object current. Returns whether
    /// it was sent.
    pub(crate) fn focus(&self, key: &ObjectKey) -> bool {
        if self.engine.core.is_none() || !self.engine.connection.is_connected() {
            return false;
        }
        tracing::debug!(object = %key, "focus");
        self.send(Command::Focus(key.clone()));
        true
    }
}

#[cfg(test)]
mod tests {
    use ic_config::{AuthConfig, Config, Environment, General, UiState};
    use ic_model::Timestamp;

    use super::*;
    use crate::app_state::testing::Recorder;

    fn environment(name: &str) -> Environment {
        Environment::new(
            name,
            &format!("https://{name}:5665"),
            AuthConfig::Basic {
                username: "icygui".to_owned(),
            },
        )
    }

    /// `prod` on screen and `staging` off screen, each with a recording
    /// engine, started as `start` says.
    fn two(start_hidden: bool) -> (AppState, (String, Recorder), (String, Recorder)) {
        let mut state = AppState::live(Config::default(), UiState::default(), Timestamp::now());
        if start_hidden {
            state.start_hidden();
        }
        let (prod, staging) = (environment("prod"), environment("staging"));
        let ids = (prod.id.clone(), staging.id.clone());
        state.save_environment(prod, false);
        state.save_environment(staging, false);
        let (prod_core, staging_core) = (Recorder::default(), Recorder::default());
        state.set_core_for(&ids.0, Box::new(prod_core.clone()));
        state.set_core_for(&ids.1, Box::new(staging_core.clone()));
        (state, (ids.0, prod_core), (ids.1, staging_core))
    }

    #[test]
    fn environments_off_screen_are_quiet_from_the_start() {
        let (state, (prod, prod_core), (staging, staging_core)) = two(false);
        assert_eq!(state.start_mode(), Start::User);
        assert!(prod_core.sent().is_empty(), "the one on screen stays live");
        assert_eq!(staging_core.sent(), ["SetActive(false)", "SetQuiet(true)"]);
        assert!(!state.quiet_wanted(&prod));
        assert!(state.quiet_wanted(&staging));
        assert!(!state.slot(&prod).unwrap().is_quiet());
        assert!(state.slot(&staging).unwrap().is_quiet());
    }

    #[test]
    fn hiding_the_window_quiets_the_environment_on_screen() {
        let (mut state, (_, prod_core), (_, staging_core)) = two(false);
        staging_core.clear();
        let wake = state.wake();
        assert!(state.set_window_hidden(true));
        assert!(!state.set_window_hidden(true), "once");
        assert_eq!(prod_core.sent(), ["SetQuiet(true)"]);
        assert!(staging_core.sent().is_empty(), "quiet already");
        assert_eq!(state.wake(), wake, "going quiet isn't waking up");

        assert!(state.set_window_hidden(false));
        assert_eq!(prod_core.sent(), ["SetQuiet(true)", "SetQuiet(false)"]);
        assert!(staging_core.sent().is_empty(), "still off screen");
        assert_eq!(state.wake(), wake + 1, "a new wake: the views ask again");
    }

    #[test]
    fn switching_wakes_the_new_environment_and_quiets_the_old() {
        let (mut state, (prod, prod_core), (staging, staging_core)) = two(false);
        prod_core.clear();
        staging_core.clear();
        let wake = state.wake();
        assert!(state.switch_environment(&staging));
        assert_eq!(staging_core.sent(), ["SetActive(true)", "SetQuiet(false)"]);
        assert_eq!(prod_core.sent(), ["SetActive(false)", "SetQuiet(true)"]);
        assert_eq!(state.wake(), wake + 1);
        assert!(state.quiet_wanted(&prod));
        assert!(!state.quiet_wanted(&staging));

        // Hidden, switching changes nothing for the streams: both stay
        // quiet, and the one on screen wakes when the window comes back.
        assert!(state.set_window_hidden(true));
        prod_core.clear();
        staging_core.clear();
        assert!(state.switch_environment(&prod));
        assert_eq!(prod_core.sent(), ["SetActive(true)"]);
        assert_eq!(staging_core.sent(), ["SetActive(false)"]);
        assert!(state.set_window_hidden(false));
        assert_eq!(prod_core.sent(), ["SetActive(true)", "SetQuiet(false)"]);
        assert_eq!(staging_core.sent(), ["SetActive(false)"]);
    }

    #[test]
    fn the_setting_turns_quiet_mode_off_and_on() {
        let (mut state, (_, prod_core), (_, staging_core)) = two(false);
        assert!(state.set_window_hidden(true));
        prod_core.clear();
        staging_core.clear();
        let off = General {
            quiet_when_hidden: false,
            ..state.config().general.clone()
        };
        assert!(state.set_general(&off));
        for core in [&prod_core, &staging_core] {
            assert_eq!(core.sent().len(), 2);
            assert!(core.sent()[0].starts_with("UpdateGeneral("));
            assert_eq!(core.sent()[1], "SetQuiet(false)", "every environment live");
        }
        prod_core.clear();
        staging_core.clear();
        let on = General {
            quiet_when_hidden: true,
            ..off
        };
        assert!(state.set_general(&on));
        assert_eq!(prod_core.sent()[1], "SetQuiet(true)", "hidden: quiet again");
        assert_eq!(staging_core.sent()[1], "SetQuiet(true)");
        // Another setting leaves quiet mode alone.
        prod_core.clear();
        let retention = General {
            event_log_retention_hours: on.event_log_retention_hours + 1,
            ..on
        };
        assert!(state.set_general(&retention));
        assert_eq!(prod_core.sent().len(), 1, "UpdateGeneral only");
    }

    #[test]
    fn a_background_start_waits_for_the_window() {
        let (mut state, (_, prod_core), (_, staging_core)) = two(true);
        assert_eq!(state.start_mode(), Start::Background);
        assert!(state.window_hidden());
        assert_eq!(
            prod_core.sent(),
            ["SetQuiet(true)"],
            "nobody looks at the one on screen either"
        );
        assert_eq!(staging_core.sent(), ["SetActive(false)", "SetQuiet(true)"]);
        prod_core.clear();
        staging_core.clear();

        assert!(state.set_window_hidden(false));
        assert_eq!(
            state.start_mode(),
            Start::User,
            "later engines start at once"
        );
        assert_eq!(prod_core.sent(), ["StartNow", "SetQuiet(false)"]);
        assert_eq!(staging_core.sent(), ["StartNow"]);
        // Only the first time.
        state.set_window_hidden(true);
        prod_core.clear();
        staging_core.clear();
        state.set_window_hidden(false);
        assert_eq!(prod_core.sent(), ["SetQuiet(false)"]);
        assert!(staging_core.sent().is_empty());
    }

    #[test]
    fn a_replaced_engine_is_told_again() {
        let (mut state, (prod, _), (staging, staging_core)) = two(false);
        // Staging's engine restarts (another password): the new one starts
        // live and is told it is quiet.
        let old = state.take_core_of(&staging);
        assert!(old.is_some());
        state.reset_environment(&staging);
        let replacement = Recorder::default();
        state.set_core_for(&staging, Box::new(replacement.clone()));
        assert_eq!(replacement.sent(), ["SetActive(false)", "SetQuiet(true)"]);
        assert_eq!(staging_core.sent(), ["SetActive(false)", "SetQuiet(true)"]);
        // The one on screen, replaced while the window is hidden.
        state.set_window_hidden(true);
        let _ = state.take_core_of(&prod);
        state.reset_environment(&prod);
        let prod_core = Recorder::default();
        state.set_core_for(&prod, Box::new(prod_core.clone()));
        assert_eq!(prod_core.sent(), ["SetQuiet(true)"]);
    }

    /// The engine drops `Hydrate` while quiet: waking up asks for the
    /// rows on screen again, also those asked for a moment ago.
    #[test]
    fn waking_up_asks_for_the_rows_again() {
        let (mut state, (prod, prod_core), _) = two(false);
        state.apply_from(
            &prod,
            ic_core::CoreEvent::Connection(ic_core::ConnectionState::Connected {
                node: crate::app_state::connection::full_node("master"),
                version: "r2.15.6-1".to_owned(),
                since: Timestamp::now(),
            }),
        );
        let key = ObjectKey::service("h", "s");
        let now = std::time::Instant::now();
        assert!(matches!(
            state.hydrate(vec![key.clone()], now),
            super::super::Hydrated::Sent(_)
        ));
        assert_eq!(
            state.hydrate(vec![key.clone()], now),
            super::super::Hydrated::Nothing
        );
        state.set_window_hidden(true);
        state.set_window_hidden(false);
        assert!(matches!(
            state.hydrate(vec![key], now),
            super::super::Hydrated::Sent(_)
        ));
        assert_eq!(
            prod_core
                .sent()
                .iter()
                .filter(|command| command.starts_with("Hydrate("))
                .count(),
            2
        );
    }

    #[test]
    fn focus_goes_out_only_while_connected() {
        let (mut state, (prod, prod_core), _) = two(false);
        let key = ObjectKey::service("h", "s");
        assert!(!state.focus(&key), "not connected yet");
        state.apply_from(
            &prod,
            ic_core::CoreEvent::Connection(ic_core::ConnectionState::Connected {
                node: crate::app_state::connection::full_node("master"),
                version: "r2.15.6-1".to_owned(),
                since: Timestamp::now(),
            }),
        );
        assert!(state.focus(&key));
        assert_eq!(prod_core.sent(), [format!("{:?}", Command::Focus(key))]);
    }
}
