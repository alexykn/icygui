//! UI state kept between runs: the window's size and position, the objects
//! open as tabs and the selected dashboard of every environment.
//!
//! It is kept apart from the settings file on purpose: it changes whenever
//! the window moves or a tab opens, which would churn the settings file and
//! its backup, and the settings file may be managed by Ansible or Nix and
//! read-only. Losing it costs nothing but the layout, so a state file that
//! can't be read is reported and replaced by defaults, never offered for
//! restoring.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ic_rules::DashboardRef;
use serde::{Deserialize, Serialize};

use crate::error::ConfigError;
use crate::files::{read_text, write_atomic};
use crate::migrate::deserialize_text;

/// Current state file format version.
pub const UI_STATE_VERSION: u32 = 1;

/// Written at the top of every saved state file.
const HEADER: &str = "\
# icygui window and tab state. Rewritten whenever it changes; safe to delete.

";

/// The smallest window side restored; smaller saved sizes are ignored.
const MIN_WINDOW_SIDE: f32 = 200.;

/// The largest window side restored (a garbage value must not ask the
/// window system for a window millions of pixels wide).
const MAX_WINDOW_SIDE: f32 = 100_000.;

/// The objects open as tabs per environment are capped at this many: a
/// state file edited by hand can't make the app open thousands of panes.
pub const MAX_TABS: usize = 64;

/// What the app remembers between runs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiState {
    /// Format version, see [`UI_STATE_VERSION`].
    pub version: u32,
    /// The main window's last size and position.
    pub window: Option<WindowState>,
    /// Per environment, by environment id.
    pub environments: BTreeMap<String, EnvironmentUiState>,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            version: UI_STATE_VERSION,
            window: None,
            environments: BTreeMap::new(),
        }
    }
}

impl UiState {
    /// The state of environment `id`, or an empty one.
    pub fn environment(&self, id: &str) -> EnvironmentUiState {
        self.environments.get(id).cloned().unwrap_or_default()
    }

    /// Replaces the state of environment `id`; an empty state removes the
    /// entry. Returns whether anything changed.
    pub fn set_environment(&mut self, id: &str, state: EnvironmentUiState) -> bool {
        if state == EnvironmentUiState::default() {
            return self.environments.remove(id).is_some();
        }
        if self.environments.get(id) == Some(&state) {
            return false;
        }
        self.environments.insert(id.to_owned(), state);
        true
    }

    /// Drops the entries of environments that no longer exist (`keep` says
    /// which ids do). Returns how many were dropped.
    pub fn retain_environments(&mut self, keep: impl Fn(&str) -> bool) -> usize {
        let before = self.environments.len();
        self.environments.retain(|id, _| keep(id));
        before - self.environments.len()
    }

    /// Makes loaded values safe to use: drops a window size or position
    /// that can't be a real window, and caps the tab lists.
    fn sanitize(&mut self) {
        if self.window.is_some_and(|window| !window.is_plausible()) {
            tracing::warn!("ignoring an implausible saved window size or position");
            self.window = None;
        }
        for state in self.environments.values_mut() {
            let mut seen = std::collections::BTreeSet::new();
            state
                .tabs
                .retain(|tab| !tab.trim().is_empty() && seen.insert(tab.clone()));
            state.tabs.truncate(MAX_TABS);
            let mut seen = std::collections::BTreeSet::new();
            state
                .lists
                .retain(|list| !list.trim().is_empty() && seen.insert(list.clone()));
            state.lists.truncate(MAX_TABS);
        }
    }
}

/// What the app remembers about one environment.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EnvironmentUiState {
    /// Objects open as tabs ("↗ open as tab"), in sidebar order, by full
    /// name (`db-prod-03`, `db-prod-03!postgres-replication`).
    pub tabs: Vec<String>,
    /// The handling and downtimes views open as tabs (v1, topic 14), by
    /// id (`handling`, `downtimes`; stage 2's `comments` and `acknowledged`
    /// open handling); the app ignores ids it doesn't know.
    pub lists: Vec<String>,
    /// The dashboard shown last.
    pub selected: Option<DashboardRef>,
    /// Each view's choices (chip, sort, *only mine*, timeline or list), by
    /// view id, kept when the view is closed or the app restarts.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub list_options: BTreeMap<String, ListOptionsState>,
}

/// A view's choices (v1, topic 14): the chip picked, its sort (by id,
/// `latest-activity`; the app ignores ids it doesn't know), *only mine*,
/// and the downtimes view's display (`timeline`, `list`). Stage 2's
/// `system_comments` is read and dropped (Icinga's own comments never
/// show).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ListOptionsState {
    /// The sort, by id; none: the chip's (or the display's) own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort: Option<String>,
    /// Only what the environment's author set.
    pub only_mine: bool,
    /// The chip picked, by id; none: *all*.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chip: Option<String>,
    /// The downtimes view's display, by id; none: the timeline.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
}

/// The main window's size and position in logical pixels, as the window
/// system reported them (on Wayland the position is unknown and ignored).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowState {
    /// Left edge.
    pub x: f32,
    /// Top edge.
    pub y: f32,
    /// Width.
    pub width: f32,
    /// Height.
    pub height: f32,
    /// Whether it was maximized; the size and position are then the ones
    /// it returns to.
    #[serde(default)]
    pub maximized: bool,
}

impl WindowState {
    /// Whether this can be a real window: finite numbers and a size
    /// between 200 and 100 000 pixels per side.
    pub fn is_plausible(&self) -> bool {
        let side = MIN_WINDOW_SIDE..=MAX_WINDOW_SIDE;
        [self.x, self.y, self.width, self.height]
            .iter()
            .all(|value| value.is_finite() && value.abs() <= MAX_WINDOW_SIDE * 10.)
            && side.contains(&self.width)
            && side.contains(&self.height)
    }
}

/// Loads and saves the UI state file (see [`crate::Paths::state_store`]).
///
/// Saving is atomic like the settings file's (temporary file, sync,
/// rename), user-only on Unix, and keeps the previous version as
/// `<name>.bak`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateStore {
    path: PathBuf,
}

impl StateStore {
    /// A store for the state file at `path`. Nothing is read until
    /// [`StateStore::load`].
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// The state file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads the state; a missing file gives [`UiState::default`]. Values
    /// that can't be used (a window of zero size, an absurd position) are
    /// dropped, and unknown keys are ignored.
    ///
    /// # Errors
    ///
    /// [`ConfigError::Io`] when the file exists but can't be read,
    /// [`ConfigError::Parse`] when it isn't valid TOML of the right shape,
    /// and [`ConfigError::UnsupportedVersion`] when a newer icygui wrote
    /// it. Callers use defaults then: the state is only the layout.
    pub fn load(&self) -> Result<UiState, ConfigError> {
        let Some(text) = read_text(&self.path)? else {
            return Ok(UiState::default());
        };
        let mut state: UiState = deserialize_text(&text, "UI state")?;
        if u64::from(state.version) > u64::from(UI_STATE_VERSION) {
            return Err(ConfigError::UnsupportedVersion {
                found: u64::from(state.version),
                supported: u64::from(UI_STATE_VERSION),
            });
        }
        state.version = UI_STATE_VERSION;
        state.sanitize();
        Ok(state)
    }

    /// Writes the state atomically, keeping the previous file as
    /// `<name>.bak`. Unchanged state writes nothing.
    ///
    /// # Errors
    ///
    /// [`ConfigError::Serialize`] when the state can't be written as TOML,
    /// [`ConfigError::Io`] when the directory or the file can't be written;
    /// the previous file is then left as it was.
    pub fn save(&self, state: &UiState) -> Result<(), ConfigError> {
        let state = UiState {
            version: UI_STATE_VERSION,
            ..state.clone()
        };
        let body =
            toml::to_string(&state).map_err(|error| ConfigError::Serialize(error.to_string()))?;
        let text = format!("{HEADER}{body}");
        let mut backup = self.path.as_os_str().to_owned();
        backup.push(".bak");
        write_atomic(
            &self.path,
            Path::new(&backup),
            text.as_bytes(),
            |_| false,
            |_| Ok(()),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn store(dir: &Path) -> StateStore {
        StateStore::new(dir.join("state.toml"))
    }

    fn window() -> WindowState {
        WindowState {
            x: 120.,
            y: 80.,
            width: 1440.,
            height: 900.,
            maximized: false,
        }
    }

    fn reference(group: &str, dashboard: &str) -> DashboardRef {
        DashboardRef {
            group_id: group.to_owned(),
            dashboard_id: dashboard.to_owned(),
        }
    }

    #[test]
    fn a_missing_file_is_the_default() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(store(dir.path()).load().unwrap(), UiState::default());
    }

    #[test]
    fn stage_two_list_choices_still_load() {
        // Stage 2's lists kept `system_comments`; topic 14 dropped it.
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        fs::write(
            store.path(),
            "version = 1\n[environments.e]\ntabs = []\nlists = [\"comments\"]\n\
             [environments.e.list_options.comments]\nsort = \"newest\"\nonly_mine = true\n\
             system_comments = true\n",
        )
        .unwrap();
        let state = store.load().unwrap();
        let saved = &state.environments["e"].list_options["comments"];
        assert_eq!(saved.sort.as_deref(), Some("newest"));
        assert!(saved.only_mine);
        assert_eq!(saved.chip, None);
    }

    #[test]
    fn round_trips_window_tabs_and_selection() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        let mut state = UiState {
            window: Some(WindowState {
                maximized: true,
                ..window()
            }),
            ..UiState::default()
        };
        assert!(state.set_environment(
            "env-1",
            EnvironmentUiState {
                tabs: vec![
                    "db-prod-03!postgres-replication".to_owned(),
                    "db-prod-03".to_owned(),
                ],
                lists: vec!["downtimes".to_owned(), "acknowledged".to_owned()],
                selected: Some(reference("g", "d")),
                list_options: BTreeMap::from([(
                    "downtimes".to_owned(),
                    ListOptionsState {
                        sort: Some("object".to_owned()),
                        only_mine: true,
                        chip: Some("upcoming".to_owned()),
                        mode: Some("list".to_owned()),
                    },
                )]),
            },
        ));
        store.save(&state).unwrap();
        assert_eq!(store.load().unwrap(), state);
        let text = fs::read_to_string(store.path()).unwrap();
        assert!(text.starts_with("# icygui window and tab state"), "{text}");
    }

    #[test]
    fn saving_is_private_and_keeps_a_backup() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        store.save(&UiState::default()).unwrap();
        let state = UiState {
            window: Some(window()),
            ..UiState::default()
        };
        store.save(&state).unwrap();
        assert!(dir.path().join("state.toml.bak").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = fs::metadata(store.path()).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn implausible_windows_and_odd_tabs_are_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        fs::write(
            store.path(),
            "version = 1\n\
             [window]\nx = 0.0\ny = 0.0\nwidth = 0.0\nheight = 900.0\n\
             [environments.e]\ntabs = [\"a\", \"b\", \" \", \"a\"]\nlists = [\"comments\", \"comments\", \"\"]\n",
        )
        .unwrap();
        let state = store.load().unwrap();
        assert_eq!(state.window, None);
        assert_eq!(state.environment("e").tabs, ["a", "b"]);
        assert_eq!(state.environment("e").lists, ["comments"]);

        let mut huge = UiState::default();
        huge.environments.insert(
            "e".to_owned(),
            EnvironmentUiState {
                tabs: (0..200).map(|index| format!("host-{index}")).collect(),
                selected: None,
                lists: Vec::new(),
                list_options: BTreeMap::new(),
            },
        );
        store.save(&huge).unwrap();
        assert_eq!(store.load().unwrap().environment("e").tabs.len(), MAX_TABS);
    }

    #[test]
    fn plausible_windows() {
        assert!(window().is_plausible());
        let bad = |change: fn(&mut WindowState)| {
            let mut window = window();
            change(&mut window);
            !window.is_plausible()
        };
        assert!(bad(|window| window.width = f32::NAN));
        assert!(bad(|window| window.height = 10.));
        assert!(bad(|window| window.x = f32::INFINITY));
        assert!(bad(|window| window.y = 1e9));
        assert!(bad(|window| window.width = 1e6));
        // Negative positions are fine: a display left of the primary one.
        let mut left = window();
        left.x = -1800.;
        assert!(left.is_plausible());
    }

    #[test]
    fn corrupt_and_newer_files_are_errors() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        fs::write(store.path(), "window = [").unwrap();
        assert!(matches!(store.load(), Err(ConfigError::Parse { .. })));
        fs::write(store.path(), "version = 9\n").unwrap();
        assert!(matches!(
            store.load(),
            Err(ConfigError::UnsupportedVersion {
                found: 9,
                supported: 1
            })
        ));
        // Saving over either works (the state is only the layout).
        store.save(&UiState::default()).unwrap();
        assert_eq!(store.load().unwrap(), UiState::default());
    }

    #[test]
    fn unknown_keys_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        fs::write(store.path(), "version = 1\ntheme = \"neon\"\n").unwrap();
        assert_eq!(store.load().unwrap(), UiState::default());
    }

    #[test]
    fn environments_are_set_and_pruned() {
        let mut state = UiState::default();
        let tabs = EnvironmentUiState {
            tabs: vec!["h".to_owned()],
            selected: None,
            lists: Vec::new(),
            list_options: BTreeMap::new(),
        };
        assert!(state.set_environment("a", tabs.clone()));
        assert!(!state.set_environment("a", tabs.clone()), "unchanged");
        assert!(state.set_environment("b", tabs.clone()));
        assert_eq!(state.retain_environments(|id| id == "a"), 1);
        assert_eq!(state.environments.len(), 1);
        assert!(
            state.set_environment("a", EnvironmentUiState::default()),
            "an empty state removes the entry"
        );
        assert!(state.environments.is_empty());
        assert_eq!(state.environment("a"), EnvironmentUiState::default());
    }
}
