//! Key bindings beyond the defaults: the user's keymap file
//! (`keymap.toml`, read by `ic_config::read_keymap`) on top of the default
//! bindings, read at start and again whenever the window comes back to the
//! front after the file changed (*edit keymap file* in the settings opens
//! it in the default editor); and the list of every shortcut that the
//! settings' keymap page shows.
//!
//! GPUI keeps one keymap for the app: reading the file again clears it and
//! binds the defaults (remembered once at start) and then the file's
//! bindings, which win over the defaults for the same keys and context.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use gpui::{
    Action, App, DummyKeyboardMapper, Global, KeyBinding, KeyBindingContextPredicate, Keystroke,
    NoAction,
};
use ic_config::{Keymap, KeymapAction};

/// The default bindings, as bound at start.
struct Defaults(Vec<KeyBinding>);

impl Global for Defaults {}

/// The keymap file and what reading it gave.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct UserKeymap {
    /// The file.
    pub(crate) path: PathBuf,
    /// When it was last changed, as last read (`None`: there was none).
    modified: Option<SystemTime>,
    /// How many of its bindings are bound.
    pub(crate) bound: usize,
    /// What couldn't be bound, one line each.
    pub(crate) problems: Vec<String>,
}

impl Global for UserKeymap {}

/// Remembers the bindings made so far as the defaults, then binds the
/// keymap file at `path` on top of them. Call once, after every default
/// binding is made.
pub(crate) fn install(path: PathBuf, cx: &mut App) {
    let defaults: Vec<KeyBinding> = cx.key_bindings().borrow().bindings().cloned().collect();
    cx.set_global(Defaults(defaults));
    cx.set_global(UserKeymap {
        path,
        ..UserKeymap::default()
    });
    load(cx);
}

/// The keymap file and what reading it gave (`None` before
/// [`install`], as in tests).
pub(crate) fn user_keymap(cx: &App) -> Option<&UserKeymap> {
    cx.try_global::<UserKeymap>()
}

/// Reads the keymap file again if it changed since it was read (the
/// window came back to the front). Returns whether it was read.
pub(crate) fn reload_if_changed(cx: &mut App) -> bool {
    let Some(keymap) = cx.try_global::<UserKeymap>() else {
        return false;
    };
    if modified(&keymap.path) == keymap.modified {
        return false;
    }
    load(cx);
    true
}

/// Creates the keymap file with a commented template (nothing bound) if
/// there is none, and returns its path.
///
/// # Errors
///
/// The file or its directory can't be written.
pub(crate) fn ensure_file(path: &Path) -> std::io::Result<()> {
    if path.exists() {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, ic_config::KEYMAP_TEMPLATE)
}

/// Reads the keymap file and binds the defaults, then its bindings.
fn load(cx: &mut App) {
    let Some(path) = cx
        .try_global::<UserKeymap>()
        .map(|keymap| keymap.path.clone())
    else {
        return;
    };
    let stamp = modified(&path);
    let (bindings, mut problems) = match ic_config::read_keymap(&path) {
        Ok(Some(keymap)) => build(&keymap, cx),
        Ok(None) => (Vec::new(), Vec::new()),
        Err(error) => (Vec::new(), vec![error.to_string()]),
    };
    let defaults = cx
        .try_global::<Defaults>()
        .map(|defaults| defaults.0.clone())
        .unwrap_or_default();
    let bound = bindings.len();
    cx.clear_key_bindings();
    cx.bind_keys(defaults);
    cx.bind_keys(bindings);
    for problem in &problems {
        tracing::warn!(path = %path.display(), %problem, "a keymap binding was skipped");
    }
    if bound > 0 {
        tracing::info!(path = %path.display(), bound, "keymap file read");
    }
    problems.truncate(MAX_PROBLEMS);
    cx.set_global(UserKeymap {
        path,
        modified: stamp,
        bound,
        problems,
    });
}

/// At most this many problems are kept to show.
const MAX_PROBLEMS: usize = 20;

/// When the file was last changed (`None`: it isn't there).
fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
}

/// GPUI bindings for the file's bindings, and what couldn't be bound.
fn build(keymap: &Keymap, cx: &App) -> (Vec<KeyBinding>, Vec<String>) {
    let mut problems = keymap.problems.clone();
    let mut bindings = Vec::new();
    for binding in &keymap.bindings {
        let place = match &binding.context {
            Some(context) => format!("[{context}] \"{}\"", binding.keys),
            None => format!("\"{}\"", binding.keys),
        };
        let action: Box<dyn Action> = match &binding.action {
            KeymapAction::Unbind => Box::new(NoAction),
            KeymapAction::Action { name, argument } => {
                let argument = match argument.as_ref().map(serde_json::to_value) {
                    Some(Ok(value)) => Some(value),
                    Some(Err(error)) => {
                        problems.push(format!("{place}: {error}"));
                        continue;
                    }
                    None => None,
                };
                match cx.build_action(name, argument) {
                    Ok(action) => action,
                    Err(error) => {
                        problems.push(format!("{place}: {error}"));
                        continue;
                    }
                }
            }
        };
        let predicate = match binding
            .context
            .as_deref()
            .map(KeyBindingContextPredicate::parse)
        {
            Some(Ok(predicate)) => Some(predicate.into()),
            Some(Err(error)) => {
                problems.push(format!("{place}: the context doesn't parse ({error})"));
                continue;
            }
            None => None,
        };
        match KeyBinding::load(
            &binding.keys,
            action,
            predicate,
            false,
            None,
            &DummyKeyboardMapper,
        ) {
            Ok(binding) => bindings.push(binding),
            Err(error) => problems.push(format!("{place}: {error}")),
        }
    }
    (bindings, problems)
}

// --- The list of shortcuts ---------------------------------------------

/// A binding in effect, as the keymap page reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Bound {
    /// The action's full name (`icygui::ToggleMark`).
    pub(crate) action: &'static str,
    /// The keys, one label per keystroke of a sequence (`ctrl-k`).
    pub(crate) keys: Vec<String>,
    /// The context, as written (`DashboardView`), or `None` for anywhere.
    pub(crate) context: Option<String>,
}

/// The bindings in effect now: the app's own actions, those a later
/// binding of the same keys in the same context doesn't replace, and no
/// switched-off ones.
pub(crate) fn bindings_in_effect(cx: &App) -> Vec<Bound> {
    let keymap = cx.key_bindings();
    let keymap = keymap.borrow();
    let mut seen = std::collections::HashSet::new();
    let mut bound: Vec<Bound> = keymap
        .bindings()
        .rev()
        .filter_map(|binding| {
            let keys: Vec<String> = binding
                .keystrokes()
                .iter()
                .map(|keystroke| keystroke_label(keystroke.inner()))
                .collect();
            let context = binding.predicate().map(|predicate| predicate.to_string());
            if !seen.insert((keys.clone(), context.clone())) {
                return None;
            }
            let action = binding.action().name();
            (action.starts_with("icygui::") && !gpui::is_no_action(binding.action())).then_some(
                Bound {
                    action,
                    keys,
                    context,
                },
            )
        })
        .collect();
    bound.reverse();
    bound
}

/// A keystroke as the app shows key hints: `ctrl-shift-e`, `↓`, `esc` on
/// Linux; `⇧⌘E` on macOS.
pub(crate) fn keystroke_label(keystroke: &Keystroke) -> String {
    let key = match keystroke.key.as_str() {
        "down" => "↓".to_owned(),
        "up" => "↑".to_owned(),
        "left" => "←".to_owned(),
        "right" => "→".to_owned(),
        "escape" => "esc".to_owned(),
        key => key.to_owned(),
    };
    let modifiers = keystroke.modifiers;
    if cfg!(target_os = "macos") {
        let mut label = String::new();
        for (on, symbol) in [
            (modifiers.control, "⌃"),
            (modifiers.alt, "⌥"),
            (modifiers.shift, "⇧"),
            (modifiers.platform, "⌘"),
        ] {
            if on {
                label.push_str(symbol);
            }
        }
        let key = match key.as_str() {
            "enter" => "↵".to_owned(),
            "tab" => "⇥".to_owned(),
            key if key.chars().count() == 1 => key.to_uppercase(),
            key => key.to_owned(),
        };
        label.push_str(&key);
        label
    } else {
        let mut parts = Vec::new();
        for (on, name) in [
            (modifiers.control, "ctrl"),
            (modifiers.alt, "alt"),
            (modifiers.shift, "shift"),
            (modifiers.platform, "super"),
            (modifiers.function, "fn"),
        ] {
            if on {
                parts.push(name.to_owned());
            }
        }
        parts.push(key);
        parts.join("-")
    }
}

/// A row of the keymap page: what the keys do, the keys, and where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ShortcutRow {
    /// What it does: `command palette`, `next row, previous row`.
    pub(crate) label: String,
    /// The keys, each a hint of its own (`j`, `k`, `↓`, `↑`).
    pub(crate) keys: Vec<String>,
    /// The keys are a range: the first, `to`, the last (`ctrl-1` to
    /// `ctrl-9`).
    pub(crate) range: bool,
    /// Where they work: `anywhere`, `list, pane`.
    pub(crate) place: String,
}

impl ShortcutRow {
    /// Whether the row matches a filter (lower case): by what it does,
    /// its keys or where.
    pub(crate) fn matches(&self, filter: &str) -> bool {
        filter.is_empty()
            || self.label.to_lowercase().contains(filter)
            || self.place.to_lowercase().contains(filter)
            || self
                .keys
                .iter()
                .any(|key| key.to_lowercase().contains(filter))
    }
}

/// What the app's actions do, in the order the page lists them: actions
/// in one entry share a row (`next row, previous row`).
const CATALOGUE: &[(&str, &[&str])] = &[
    ("command palette", &["ToggleCommandPalette"]),
    ("settings", &["OpenSettings"]),
    ("new dashboard", &["NewDashboard"]),
    ("select dashboard", &["SelectDashboard"]),
    ("show or hide the sidebar", &["ToggleSidebar"]),
    (
        "next tab, previous tab",
        &["ActivateNextTab", "ActivatePreviousTab"],
    ),
    ("close the tab", &["CloseTab"]),
    ("quit", &["Quit"]),
    ("next row, previous row", &["SelectNext", "SelectPrevious"]),
    // Four keys don't fit the keys column: a row each.
    ("mark rows down", &["ExtendSelectionNext"]),
    ("mark rows up", &["ExtendSelectionPrevious"]),
    ("first row, last row", &["SelectFirst", "SelectLast"]),
    ("page up, page down", &["SelectPageUp", "SelectPageDown"]),
    ("mark the row", &["ToggleMark"]),
    ("mark every row", &["MarkAll"]),
    ("open the pane, as a tab", &["OpenSelected", "OpenAsTab"]),
    ("close the pane, clear the marks", &["Dismiss"]),
    ("acknowledge", &["Acknowledge"]),
    ("schedule downtime", &["ScheduleDowntime"]),
    ("check now", &["CheckNow"]),
    ("add comment", &["AddComment"]),
    (
        "next match, previous match",
        &["PaletteNext", "PalettePrevious"],
    ),
    ("run the selected command", &["PaletteConfirm"]),
    ("run a verb on all matches", &["PaletteRunAll"]),
    ("open as a tab", &["PaletteOpenTab"]),
    ("save, discard", &["SaveDashboard", "DiscardDashboard"]),
    (
        "next field, previous field",
        &["NextField", "PreviousField"],
    ),
    ("send", &["SendDialog", "SendDialogNow"]),
    ("run the command", &["ConfirmCommand"]),
    ("confirm", &["ConfirmModal"]),
    ("close the dialog", &["CloseModal"]),
    ("focus navbar", &["FocusNavbar"]),
    (
        "next category, previous category",
        &["NavNext", "NavPrevious"],
    ),
    ("open the category", &["NavOpen"]),
    ("search settings", &["FocusSettingsSearch"]),
    ("clear the search, close", &["SettingsEscape"]),
    ("switch, press", &["ControlActivate"]),
    (
        "next choice, previous choice",
        &["ControlNext", "ControlPrevious"],
    ),
    ("hide icygui, hide others", &["Hide", "HideOthers"]),
    ("minimize", &["Minimize"]),
];

/// Where a context's bindings work, in words.
fn place_of(context: Option<&str>) -> String {
    let Some(context) = context else {
        return "anywhere".to_owned();
    };
    let outer = context.split('>').next().unwrap_or(context).trim();
    match outer {
        "Workspace" => "anywhere",
        "DashboardView" => "list",
        "ObjectPane" => "pane",
        "CommandPalette" => "palette",
        "DashboardEditor" => "dashboard editor",
        "ActionDialog" | "ActionFieldless" | "ActionConfirm" => "action dialogs",
        "Modal" => "dialogs",
        "ConfirmDialog" => "confirmations",
        "SettingsPanel" | "SettingsNav" | "SettingsControl" => "settings",
        other => other,
    }
    .to_owned()
}

/// What an action does, in words: its catalogue entry, or its name split
/// into words (`icygui::FooBar` → `foo bar`).
fn label_of(action: &str) -> (usize, String) {
    let short = action.rsplit("::").next().unwrap_or(action);
    if let Some(index) = CATALOGUE
        .iter()
        .position(|(_, actions)| actions.contains(&short))
    {
        return (index, CATALOGUE[index].0.to_owned());
    }
    let mut words = String::new();
    for (index, character) in short.chars().enumerate() {
        if character.is_uppercase() && index > 0 {
            words.push(' ');
        }
        words.extend(character.to_lowercase());
    }
    (CATALOGUE.len(), words)
}

/// The keymap page's rows for `bound`: one per catalogue entry and place,
/// with every key of its actions there; rows whose entry and keys are the
/// same in several places become one (`list, pane`). Catalogue order, then
/// the rest by what they do. A row of two actions lists each action's
/// first key, then each one's second (`j k ↓ ↑`), as the label pairs them.
pub(crate) fn shortcut_rows(bound: &[Bound]) -> Vec<ShortcutRow> {
    // (catalogue index, label, place) → each action's keys.
    type Keys = Vec<(&'static str, Vec<String>)>;
    let mut by_action: Vec<(usize, String, String, Keys)> = Vec::new();
    for binding in bound {
        let (index, label) = label_of(binding.action);
        let place = place_of(binding.context.as_deref());
        let keys = binding.keys.join(" ");
        let found = by_action
            .iter()
            .position(|(i, l, p, _)| *i == index && *l == label && *p == place);
        let row = if let Some(row) = found {
            row
        } else {
            by_action.push((index, label, place, Vec::new()));
            by_action.len() - 1
        };
        let actions = &mut by_action[row].3;
        if actions.iter().any(|(_, known)| known.contains(&keys)) {
            continue;
        }
        match actions
            .iter_mut()
            .find(|(action, _)| *action == binding.action)
        {
            Some((_, known)) => known.push(keys),
            None => actions.push((binding.action, vec![keys])),
        }
    }
    let rows = by_action.into_iter().map(|(index, label, place, actions)| {
        let longest = actions
            .iter()
            .map(|(_, keys)| keys.len())
            .max()
            .unwrap_or(0);
        let keys: Vec<String> = (0..longest)
            .flat_map(|at| actions.iter().filter_map(move |(_, keys)| keys.get(at)))
            .cloned()
            .collect();
        (index, label, place, keys)
    });
    // The same keys for the same thing in several places: one row.
    let mut merged: Vec<(usize, String, String, Vec<String>)> = Vec::new();
    for (index, label, place, keys) in rows {
        match merged
            .iter_mut()
            .find(|(i, l, _, k)| *i == index && *l == label && *k == keys)
        {
            Some((_, _, places, _)) => {
                if !places.split(", ").any(|known| known == place) {
                    places.push_str(", ");
                    places.push_str(&place);
                }
            }
            None => merged.push((index, label, place, keys)),
        }
    }
    merged.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    merged
        .into_iter()
        .map(|(_, label, place, keys)| {
            let range = is_range(&keys);
            let (label, keys) = if label == "select dashboard" {
                let label = match (range, keys.len()) {
                    (true, _) => format!("select dashboard 1 to {}", keys.len()),
                    (false, 1) => label,
                    (false, _) => "select dashboards by number".to_owned(),
                };
                let keys = if range {
                    vec![keys[0].clone(), keys[keys.len() - 1].clone()]
                } else {
                    keys
                };
                (label, keys)
            } else {
                (label, keys)
            };
            ShortcutRow {
                label,
                keys,
                range,
                place,
            }
        })
        .collect()
}

/// Whether keys are one prefix with the digits 1, 2, 3 … in order
/// (`ctrl-1` … `ctrl-9`).
fn is_range(keys: &[String]) -> bool {
    if keys.len() < 3 {
        return false;
    }
    let Some(prefix) = keys[0].strip_suffix('1') else {
        return false;
    };
    keys.iter()
        .enumerate()
        .all(|(index, key)| *key == format!("{prefix}{}", index + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bound(action: &'static str, keys: &[&str], context: Option<&str>) -> Bound {
        Bound {
            action,
            keys: keys.iter().map(|key| (*key).to_owned()).collect(),
            context: context.map(str::to_owned),
        }
    }

    #[test]
    fn rows_join_related_actions_places_and_dashboard_numbers() {
        let mut list = vec![
            bound("icygui::SelectNext", &["j"], Some("DashboardView")),
            bound("icygui::SelectNext", &["↓"], Some("DashboardView")),
            bound("icygui::SelectPrevious", &["k"], Some("DashboardView")),
            bound("icygui::Acknowledge", &["a"], Some("DashboardView")),
            bound("icygui::Acknowledge", &["a"], Some("ObjectPane")),
            bound(
                "icygui::ToggleCommandPalette",
                &["ctrl-k"],
                Some("Workspace"),
            ),
            bound("icygui::SomethingNew", &["ctrl-g", "g"], None),
        ];
        for number in 1..=9 {
            let key = format!("ctrl-{number}");
            list.push(bound(
                "icygui::SelectDashboard",
                &[key.as_str()],
                Some("Workspace"),
            ));
        }
        let rows = shortcut_rows(&list);
        let labels: Vec<&str> = rows.iter().map(|row| row.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "command palette",
                "select dashboard 1 to 9",
                "next row, previous row",
                "acknowledge",
                "something new"
            ]
        );
        assert_eq!(rows[0].place, "anywhere");
        assert_eq!(rows[1].keys, ["ctrl-1", "ctrl-9"]);
        assert!(rows[1].range);
        assert_eq!(rows[2].keys, ["j", "k", "↓"], "first keys first");
        assert_eq!(rows[2].place, "list");
        assert_eq!(rows[3].place, "list, pane", "the same keys in two places");
        assert_eq!(rows[4].keys, ["ctrl-g g"], "a sequence is one key hint");
        assert!(rows[2].matches("row"));
        assert!(rows[2].matches("↓"));
        assert!(rows[3].matches("pane"));
        assert!(!rows[3].matches("palette"));
    }

    #[test]
    fn dashboard_numbers_out_of_order_are_listed() {
        let rows = shortcut_rows(&[
            bound("icygui::SelectDashboard", &["alt-1"], None),
            bound("icygui::SelectDashboard", &["alt-3"], None),
            bound("icygui::SelectDashboard", &["alt-4"], None),
        ]);
        assert_eq!(rows[0].label, "select dashboards by number");
        assert_eq!(rows[0].keys.len(), 3);
        assert!(!rows[0].range);
    }

    #[test]
    fn keys_read_like_the_apps_key_hints() {
        let label = |text: &str| keystroke_label(&Keystroke::parse(text).unwrap());
        if cfg!(target_os = "macos") {
            assert_eq!(label("cmd-k"), "⌘K");
            assert_eq!(label("cmd-shift-e"), "⇧⌘E");
        } else {
            assert_eq!(label("ctrl-k"), "ctrl-k");
            assert_eq!(label("ctrl-shift-e"), "ctrl-shift-e");
            assert_eq!(label("down"), "↓");
            assert_eq!(label("escape"), "esc");
            assert_eq!(label("ctrl-,"), "ctrl-,");
        }
    }

    #[test]
    fn a_template_file_is_written_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("icygui/keymap.toml");
        ensure_file(&path).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            ic_config::KEYMAP_TEMPLATE
        );
        std::fs::write(&path, "\"q\" = \"icygui::Quit\"").unwrap();
        ensure_file(&path).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "\"q\" = \"icygui::Quit\"",
            "an existing file is left alone"
        );
    }
}
