//! The keymap file, `keymap.toml` next to the settings file: the user's own
//! key bindings, added on top of the defaults (a binding for the same keys
//! in the same context wins over the default one).
//!
//! ```toml
//! # Bindings before the first table work everywhere.
//! "ctrl-shift-p" = "icygui::ToggleCommandPalette"
//!
//! # Each table is a key context: the list, a pane, the palette …
//! [DashboardView]
//! "space" = "icygui::ToggleMark"
//! "x" = "none"                                # unbinds the default
//!
//! [Workspace]
//! "alt-3" = ["icygui::SelectDashboard", 3]    # an action with an argument
//! ```
//!
//! This module only reads the file; which action names exist and whether
//! the keys parse is for the app to say (GPUI builds the actions).

use std::path::Path;

use toml::{Table, Value};

use crate::error::ConfigError;
use crate::files::read_text;
use crate::migrate::{parse_table, strip_bom};

/// What a file's binding does.
#[derive(Clone, Debug, PartialEq)]
pub enum KeymapAction {
    /// Nothing: the keys' default binding in that context is switched off
    /// (`"none"`).
    Unbind,
    /// Runs the action with this name (`icygui::ToggleMark`), with an
    /// argument for actions that take one (`["icygui::SelectDashboard",
    /// 3]`).
    Action {
        /// The action's full name.
        name: String,
        /// Its argument, if any.
        argument: Option<Value>,
    },
}

/// One binding of the keymap file.
#[derive(Clone, Debug, PartialEq)]
pub struct KeymapBinding {
    /// The key context (`DashboardView`, `ObjectPane`, …), or `None` for
    /// a binding that works everywhere.
    pub context: Option<String>,
    /// The keys, as GPUI writes them (`ctrl-shift-p`, `g g`).
    pub keys: String,
    /// What they do.
    pub action: KeymapAction,
}

/// What the keymap file holds: its bindings (those that work everywhere
/// first, then by context and keys), and what couldn't be read (one line
/// each, naming the binding).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Keymap {
    /// The bindings that could be read.
    pub bindings: Vec<KeymapBinding>,
    /// Entries that were skipped, and why.
    pub problems: Vec<String>,
}

/// What a new keymap file says: how to write bindings, all commented out.
pub const KEYMAP_TEMPLATE: &str = r#"# icygui keymap: your own key bindings, on top of the defaults.
# Settings → keymap lists every shortcut, with the action it runs and where it
# works. Changes apply when you come back to icygui.
#
# Bindings before the first table work everywhere:
#
#   "ctrl-shift-p" = "icygui::ToggleCommandPalette"
#
# Each table is a key context, and its bindings work there:
#
#   [DashboardView]                       # the list
#   "space" = "icygui::ToggleMark"
#   "x" = "none"                          # switches the default off
#
#   [Workspace]                           # the whole window
#   "alt-3" = ["icygui::SelectDashboard", 3]
#
# Contexts: Workspace (the window), DashboardView (the list), ObjectPane (an
# object open as a tab), CommandPalette, DashboardEditor, ActionDialog,
# SettingsPanel.
"#;

/// The word that unbinds keys.
const UNBIND: &str = "none";

/// Reads the keymap file at `path`; `None` when there is none.
///
/// # Errors
///
/// [`ConfigError::Io`] when it exists but can't be read, [`ConfigError::Parse`]
/// when it is not valid TOML (with the line). Entries of the wrong shape
/// don't fail the file: they are listed in [`Keymap::problems`].
pub fn read_keymap(path: &Path) -> Result<Option<Keymap>, ConfigError> {
    match read_text(path)? {
        Some(text) => parse_keymap(&text).map(Some),
        None => Ok(None),
    }
}

/// Reads keymap text (see [`read_keymap`]).
///
/// # Errors
///
/// [`ConfigError::Parse`] when it is not valid TOML.
pub fn parse_keymap(text: &str) -> Result<Keymap, ConfigError> {
    let table = parse_table(strip_bom(text))?;
    let mut keymap = Keymap::default();
    // Bindings outside tables first, as in the file.
    for (keys, value) in &table {
        if !value.is_table() {
            keymap.add(None, keys, value);
        }
    }
    for (context, value) in &table {
        if let Value::Table(bindings) = value {
            keymap.add_context(context, bindings);
        }
    }
    Ok(keymap)
}

impl Keymap {
    fn add_context(&mut self, context: &str, bindings: &Table) {
        for (keys, value) in bindings {
            if value.is_table() {
                self.problems.push(format!(
                    "[{context}] \"{keys}\": a table inside a context; contexts don't nest"
                ));
            } else {
                self.add(Some(context), keys, value);
            }
        }
    }

    fn add(&mut self, context: Option<&str>, keys: &str, value: &Value) {
        let place = match context {
            Some(context) => format!("[{context}] \"{keys}\""),
            None => format!("\"{keys}\""),
        };
        if keys.trim().is_empty() {
            self.problems.push(format!("{place}: no keys"));
            return;
        }
        let action = match value {
            Value::String(name) if name.trim() == UNBIND => KeymapAction::Unbind,
            Value::String(name) if !name.trim().is_empty() => KeymapAction::Action {
                name: name.trim().to_owned(),
                argument: None,
            },
            Value::Array(parts) => match &parts[..] {
                [Value::String(name), argument] if !name.trim().is_empty() => {
                    KeymapAction::Action {
                        name: name.trim().to_owned(),
                        argument: Some(argument.clone()),
                    }
                }
                _ => {
                    self.problems.push(format!(
                        "{place}: write an action with an argument as [\"icygui::Name\", argument]"
                    ));
                    return;
                }
            },
            _ => {
                self.problems.push(format!(
                    "{place}: expected an action name like \"icygui::ToggleMark\", or \"none\""
                ));
                return;
            }
        };
        self.bindings.push(KeymapBinding {
            context: context.map(str::to_owned),
            keys: keys.trim().to_owned(),
            action,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action(name: &str) -> KeymapAction {
        KeymapAction::Action {
            name: name.to_owned(),
            argument: None,
        }
    }

    #[test]
    fn reads_bindings_everywhere_and_per_context() {
        let keymap = parse_keymap(
            r#"
"ctrl-shift-p" = "icygui::ToggleCommandPalette"

[DashboardView]
"space" = "icygui::ToggleMark"
"x" = "none"

["ActionDialog > Input"]
"alt-3" = ["icygui::SelectDashboard", 3]
"#,
        )
        .unwrap();
        assert!(keymap.problems.is_empty(), "{:?}", keymap.problems);
        assert_eq!(
            keymap.bindings,
            [
                KeymapBinding {
                    context: None,
                    keys: "ctrl-shift-p".to_owned(),
                    action: action("icygui::ToggleCommandPalette"),
                },
                KeymapBinding {
                    context: Some("ActionDialog > Input".to_owned()),
                    keys: "alt-3".to_owned(),
                    action: KeymapAction::Action {
                        name: "icygui::SelectDashboard".to_owned(),
                        argument: Some(Value::Integer(3)),
                    },
                },
                KeymapBinding {
                    context: Some("DashboardView".to_owned()),
                    keys: "space".to_owned(),
                    action: action("icygui::ToggleMark"),
                },
                KeymapBinding {
                    context: Some("DashboardView".to_owned()),
                    keys: "x".to_owned(),
                    action: KeymapAction::Unbind,
                },
            ]
        );
    }

    #[test]
    fn entries_of_the_wrong_shape_are_skipped_and_named() {
        let keymap = parse_keymap(
            r#"
"a" = 3
"b" = ["icygui::SelectDashboard"]
"" = "icygui::Quit"
"c" = "icygui::Quit"

[Workspace]
"d" = ""
[Workspace.Nested]
"e" = "icygui::Quit"
"#,
        )
        .unwrap();
        assert_eq!(keymap.bindings.len(), 1, "{:?}", keymap.bindings);
        assert_eq!(keymap.bindings[0].keys, "c");
        assert_eq!(keymap.problems.len(), 5, "{:?}", keymap.problems);
        assert!(keymap.problems.iter().any(|p| p.starts_with("\"a\"")));
        assert!(
            keymap
                .problems
                .iter()
                .any(|p| p.starts_with("[Workspace] \"Nested\""))
        );
    }

    #[test]
    fn syntax_errors_fail_with_a_line_and_the_template_reads_empty() {
        let error = parse_keymap("\"a\" = = 1\n").unwrap_err();
        assert!(error.to_string().contains("line 1"), "{error}");
        assert_eq!(parse_keymap(KEYMAP_TEMPLATE).unwrap(), Keymap::default());
        assert_eq!(parse_keymap("\u{feff}").unwrap(), Keymap::default());
    }

    #[test]
    fn a_missing_file_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_keymap(&dir.path().join("keymap.toml")).unwrap(), None);
        std::fs::write(dir.path().join("keymap.toml"), "\"q\" = \"icygui::Quit\"").unwrap();
        let keymap = read_keymap(&dir.path().join("keymap.toml"))
            .unwrap()
            .unwrap();
        assert_eq!(keymap.bindings[0].action, action("icygui::Quit"));
    }
}
