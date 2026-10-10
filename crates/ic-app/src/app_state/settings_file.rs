//! The settings file edited by hand (*edit in settings file* in the
//! settings panel): when the window comes back to the front, or before
//! icygui writes the file, and the file reads differently from what icygui
//! last read or wrote there, its settings are taken over at once, as a
//! change in the panel would be.
//!
//! Only a real edit counts, and it is merged with what changed in the
//! window meanwhile ([`ic_config::merge_edit`]): a change made in the
//! window and not yet written stays (on the same setting it wins), so
//! neither side undoes the other. The merge is then written back.

use ic_config::Config;
use ic_core::Command;

use super::environments::connection_differs;
use super::{AppState, CoreLink};

/// What taking over the settings file means for the engines; the session
/// does it.
#[derive(Default)]
pub(crate) struct FileChanges {
    /// Environments whose connection changed: their engines restart.
    pub(crate) reconnect: Vec<String>,
    /// Environments no longer in the file, with their engines' links: they
    /// stop (their passwords and event logs stay, as for a file edited
    /// while icygui was closed).
    pub(crate) removed: Vec<(String, Option<Box<dyn CoreLink>>)>,
    /// Environments new in the file: their engines start.
    pub(crate) added: Vec<String>,
    /// *Start at login* changed to this: the login entry follows.
    pub(crate) launch_at_login: Option<bool>,
}

impl AppState {
    /// The settings as the file holds them, as far as icygui knows (`None`
    /// in the demo and in tests without a settings writer).
    pub(crate) fn settings_on_disk(&self) -> Option<Config> {
        self.persistence.as_ref()?.on_disk()
    }

    /// The settings file was read and holds `config` (at start, or after
    /// recovering it).
    pub(crate) fn settings_read(&self, config: &Config) {
        if let Some(persistence) = &self.persistence {
            persistence.read_from_disk(config.clone());
        }
    }

    /// Takes over settings edited in the file (`theirs`; `known` is what
    /// icygui last read or wrote there, `None` when it doesn't know):
    /// merged with the changes made in the window since `known`, then
    /// app-wide settings and the appearance at once, every environment's
    /// dashboards and rules in place (its engine told), a changed
    /// connection by a restart. The environment on screen stays on screen
    /// while it exists. The merge is written back to the file (a save
    /// that finds the file already holding it writes nothing). Returns what
    /// the engines need (the caller does it), or `None` when nothing
    /// changes in the window.
    pub(crate) fn take_settings_from_file(
        &mut self,
        theirs: Config,
        known: Option<&Config>,
    ) -> Option<FileChanges> {
        self.settings_read(&theirs);
        let mut file = match known {
            Some(known) => ic_config::merge_edit(known, &self.config, &theirs),
            None => theirs,
        };
        // The file says which environment was on screen when icygui last
        // wrote it; the window keeps showing the one it shows.
        let active = self.config.active_environment.clone();
        file.active_environment.clone_from(&active);
        if file == self.config {
            // Changes of the window's that the file lacks (a save refused
            // while the file was being edited) go into it.
            self.save_config();
            return None;
        }
        tracing::info!("the settings file was edited; taking it over");
        let mut changes = FileChanges::default();
        // The engines of environments gone from the file are taken while
        // their slots can still be found.
        let gone: Vec<String> = self
            .config
            .environments
            .iter()
            .filter(|environment| file.environment(&environment.id).is_none())
            .map(|environment| environment.id.clone())
            .collect();
        for id in gone {
            let core = self.take_core_of(&id);
            self.ui.retain_environments(|kept| kept != id);
            self.environment_pauses.remove(&id);
            self.parked.remove(&id);
            changes.removed.push((id, core));
        }
        let old = std::mem::replace(&mut self.config, file);
        for environment in &old.environments {
            match self.config.environment(&environment.id) {
                Some(edited) if connection_differs(environment, edited) => {
                    changes.reconnect.push(environment.id.clone());
                }
                Some(edited) if edited != environment => {
                    let id = environment.id.clone();
                    self.send_environment_to(&id);
                }
                _ => {}
            }
        }
        changes.added = self
            .config
            .environments
            .iter()
            .filter(|environment| old.environment(&environment.id).is_none())
            .map(|environment| environment.id.clone())
            .collect();
        if old.general.launch_at_login != self.config.general.launch_at_login {
            changes.launch_at_login = Some(self.config.general.launch_at_login);
        }
        if old.general != self.config.general {
            let general = self.config.general.clone();
            self.send_to_every_engine(|| Command::UpdateGeneral(general.clone()));
            if old.general.quiet_when_hidden != general.quiet_when_hidden {
                self.announce_quiet();
            }
            if old.general.log_level != general.log_level {
                crate::logging::set_level(general.log_level);
            }
        }
        if old.appearance.hide_handled != self.config.appearance.hide_handled {
            self.handled_defaults_changed();
        }
        let on_screen = active
            .as_deref()
            .is_some_and(|id| self.config.environment(id).is_some());
        if on_screen {
            self.repair_selection();
        } else {
            // The one on screen is gone from the file: the first one shows.
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
        // The merge, and which environment is on screen, go into the file.
        self.save_config();
        self.save_ui();
        Some(changes)
    }
}

#[cfg(test)]
mod tests {
    use ic_config::{InterfaceSize, LogLevel, RowDensity};

    use super::*;
    use crate::app_state::testing::Recorder;

    fn connected() -> (AppState, Recorder) {
        let mut state = AppState::fixture(ic_model::Timestamp::now());
        let recorder = Recorder::default();
        state.set_core(Box::new(recorder.clone()));
        (state, recorder)
    }

    #[test]
    fn a_file_like_icyguis_own_changes_nothing() {
        let (mut state, recorder) = connected();
        let file = state.config().clone();
        assert!(state.take_settings_from_file(file, None).is_none());
        assert!(recorder.sent().is_empty());
    }

    #[test]
    fn edited_settings_are_taken_over_in_place() {
        let (mut state, recorder) = connected();
        let id = state.active_environment_id().unwrap().to_owned();
        let mut file = state.config().clone();
        file.appearance.interface_size = InterfaceSize::Large;
        file.general.log_level = LogLevel::Debug;
        file.environments[0].notifications.storm.threshold = 12;
        // The file names another environment on screen: the window keeps
        // its own.
        file.active_environment = None;
        let changes = state.take_settings_from_file(file, None).unwrap();
        assert!(changes.reconnect.is_empty() && changes.added.is_empty());
        assert!(changes.removed.is_empty());
        assert_eq!(state.appearance().interface_size, InterfaceSize::Large);
        assert_eq!(state.config().general.log_level, LogLevel::Debug);
        assert_eq!(
            state.environment().unwrap().notifications.storm.threshold,
            12
        );
        assert_eq!(state.active_environment_id(), Some(id.as_str()));
        let sent = recorder.sent();
        assert!(
            sent.iter()
                .any(|command| command.starts_with("UpdateGeneral(")),
            "{sent:?}"
        );
        assert!(
            sent.iter()
                .any(|command| command.starts_with("UpdateEnvironment(")),
            "{sent:?}"
        );
    }

    #[test]
    fn an_edit_is_merged_with_the_windows_unsaved_changes() {
        let (mut state, _recorder) = connected();
        let known = state.config().clone();
        // Changed in the window, not yet written.
        let mut appearance = *state.appearance();
        appearance.row_density = RowDensity::Compact;
        state.set_appearance(appearance);
        // Edited by hand meanwhile.
        let mut file = known.clone();
        file.general.event_log_retention_hours = 100;
        file.general.launch_at_login = !known.general.launch_at_login;
        let changes = state.take_settings_from_file(file, Some(&known)).unwrap();
        assert_eq!(state.config().general.event_log_retention_hours, 100);
        assert_eq!(state.appearance().row_density, RowDensity::Compact);
        assert_eq!(
            changes.launch_at_login,
            Some(!known.general.launch_at_login),
            "the login entry follows the file"
        );
        // The file as icygui knows it now is the edited one.
        let mut again = state.config().clone();
        again.appearance.row_density = RowDensity::Comfortable;
        assert!(
            state
                .take_settings_from_file(again.clone(), Some(&again))
                .is_none(),
            "a file that only lacks the window's own changes takes nothing over"
        );
        assert_eq!(state.appearance().row_density, RowDensity::Compact);
    }

    #[test]
    fn a_changed_connection_restarts_and_new_environments_start() {
        let (mut state, _recorder) = connected();
        let id = state.active_environment_id().unwrap().to_owned();
        let mut file = state.config().clone();
        file.environments[0].urls[0].url = "https://master-02:5665".to_owned();
        let mut staging = file.environments[0].clone();
        staging.id = "staging".to_owned();
        staging.name = "staging".to_owned();
        file.environments.push(staging);
        let changes = state.take_settings_from_file(file, None).unwrap();
        assert_eq!(changes.reconnect, [id]);
        assert_eq!(changes.added, ["staging"]);
        assert_eq!(state.environments().len(), 2);
    }

    #[test]
    fn an_environment_gone_from_the_file_stops_and_the_next_shows() {
        let (mut state, _recorder) = connected();
        let id = state.active_environment_id().unwrap().to_owned();
        let mut file = state.config().clone();
        let mut staging = file.environments[0].clone();
        staging.id = "staging".to_owned();
        staging.name = "staging".to_owned();
        file.environments = vec![staging];
        let changes = state.take_settings_from_file(file, None).unwrap();
        assert_eq!(changes.removed.len(), 1);
        assert_eq!(changes.removed[0].0, id);
        assert!(changes.removed[0].1.is_some(), "its engine's link, to stop");
        assert_eq!(changes.added, ["staging"]);
        assert_eq!(state.active_environment_id(), Some("staging"));
    }
}
