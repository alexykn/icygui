//! Sharing dashboards: groups exported as a small TOML file and imported
//! into any environment, on any machine.
//!
//! An export looks like a settings file's `groups`, with two extra keys:
//!
//! ```toml
//! format = "icygui-dashboards"
//! version = 1
//!
//! [[groups]]
//! name = "databases"
//! …
//! ```

use serde::{Deserialize, Serialize};
use toml::{Table, Value};

use crate::config::new_id;
use crate::error::{ConfigError, MAX_VALUE_CHARS, excerpt};
use crate::migrate::{
    deserialize, deserialize_text, format_version, parse_table, strip_bom, upgrade_groups,
};
use crate::model::{CONFIG_VERSION, DashboardGroup};
use crate::validate::validate_groups;

/// The `format` value that marks a dashboard export.
const EXPORT_FORMAT: &str = "icygui-dashboards";

/// Names exports in log messages.
const EXPORT: &str = "dashboard export";

/// Written at the top of every export.
const HEADER: &str = "\
# icygui dashboards, exported for sharing. They can be imported into any
# environment; imported groups and dashboards get new ids.

";

#[derive(Serialize)]
struct ExportFile<'a> {
    format: &'static str,
    version: u32,
    groups: &'a [DashboardGroup],
}

#[derive(Deserialize)]
struct ImportFile {
    /// Checked by [`check_export`] before; declared so that it isn't
    /// reported as an unknown key.
    #[serde(default, rename = "format")]
    _format: Option<Value>,
    /// Read by [`format_version`] before; declared for the same reason.
    #[serde(default, rename = "version")]
    _version: Option<Value>,
    groups: Vec<DashboardGroup>,
}

/// Writes dashboard groups as TOML for sharing: everything about them
/// (names, every dashboard's views with their displays and options,
/// notification settings), marked with a `format` key and the settings
/// format version.
///
/// # Errors
///
/// [`ConfigError::Serialize`] when the groups can't be written as TOML.
pub fn export_groups(groups: &[DashboardGroup]) -> Result<String, ConfigError> {
    let file = ExportFile {
        format: EXPORT_FORMAT,
        version: CONFIG_VERSION,
        groups,
    };
    let body = toml::to_string(&file).map_err(|error| ConfigError::Serialize(error.to_string()))?;
    Ok(format!("{HEADER}{body}"))
}

/// Reads dashboard groups from an [export](export_groups), upgrading older
/// formats (an rc1 export's dashboards get one view each, as in the
/// settings). Every group, dashboard and view gets a fresh id, so importing
/// the same file twice, or into the environment it came from, never
/// clashes.
///
/// The `format` key may be missing (hand-written files); unknown keys are
/// ignored.
///
/// # Errors
///
/// - [`ConfigError::Parse`] when the text is not TOML or the groups don't
///   fit the expected structure;
/// - [`ConfigError::NotAnExport`] when it has another `format`, no `groups`
///   list, or is a settings file;
/// - [`ConfigError::InvalidVersion`] or [`ConfigError::UnsupportedVersion`]
///   for an unusable or newer `version`;
/// - [`ConfigError::Invalid`] when a group or dashboard has no name or an
///   impossible view.
pub fn import_groups(text: &str) -> Result<Vec<DashboardGroup>, ConfigError> {
    let text = strip_bom(text);
    let table = parse_table(text)?;
    check_export(&table)?;
    let version = format_version(&table)?;
    let original = table
        .get("groups")
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()));
    let upgraded = upgrade_groups(original.clone(), version)?;
    let mut groups = if upgraded == original {
        // Read from the text so errors point at a line.
        deserialize_text::<ImportFile>(text, EXPORT)?.groups
    } else {
        deserialize(upgraded, EXPORT)?
    };
    for group in &mut groups {
        group.id = new_id();
        for dashboard in &mut group.dashboards {
            dashboard.id = new_id();
            for view in &mut dashboard.views {
                view.id = new_id();
            }
        }
    }
    let issues = validate_groups(&groups);
    if issues.is_empty() {
        Ok(groups)
    } else {
        Err(ConfigError::Invalid(issues))
    }
}

fn check_export(table: &Table) -> Result<(), ConfigError> {
    match table.get("format") {
        None => {}
        Some(Value::String(format)) if format == EXPORT_FORMAT => {}
        Some(other) => {
            return Err(ConfigError::NotAnExport(format!(
                "its `format` is {}, not \"{EXPORT_FORMAT}\"",
                excerpt(&other.to_string(), MAX_VALUE_CHARS)
            )));
        }
    }
    match table.get("groups") {
        Some(Value::Array(_)) => Ok(()),
        Some(other) => Err(ConfigError::NotAnExport(format!(
            "`groups` is a {}, not a list of groups",
            other.type_str()
        ))),
        None if table.contains_key("environments") => Err(ConfigError::NotAnExport(
            "this is a settings file".to_owned(),
        )),
        None => Err(ConfigError::NotAnExport("it has no `groups`".to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reason(text: &str) -> String {
        match import_groups(text) {
            Err(ConfigError::NotAnExport(reason)) => reason,
            other => panic!("expected not-an-export for {text:?}, got {other:?}"),
        }
    }

    #[test]
    fn exports_start_with_the_header_and_markers() {
        let text = export_groups(&crate::default_groups()).unwrap();
        assert!(text.starts_with(HEADER));
        let table = parse_table(&text).unwrap();
        assert_eq!(table["format"].as_str(), Some(EXPORT_FORMAT));
        assert_eq!(
            table["version"].as_integer(),
            Some(i64::from(CONFIG_VERSION))
        );
        assert_eq!(table["groups"].as_array().map(Vec::len), Some(1));
    }

    #[test]
    fn rejects_other_files() {
        assert_eq!(
            reason("format = \"something-else\"\ngroups = []"),
            "its `format` is \"something-else\", not \"icygui-dashboards\""
        );
        assert_eq!(
            reason("format = 3\ngroups = []"),
            "its `format` is 3, not \"icygui-dashboards\""
        );
        assert_eq!(reason("title = \"x\""), "it has no `groups`");
        assert_eq!(reason(""), "it has no `groups`");
        assert_eq!(
            reason("groups = \"all\""),
            "`groups` is a string, not a list of groups"
        );
        assert_eq!(
            reason("version = 1\n[[environments]]\nname = \"prod\""),
            "this is a settings file"
        );
    }

    #[test]
    fn views_round_trip_through_an_export() {
        let mut groups = crate::default_groups();
        let views = vec![
            crate::View {
                name: "clusters".to_owned(),
                display: crate::ViewDisplay::SummaryTiles,
                groups: crate::ViewGroups {
                    host_groups: vec!["pg-*".to_owned(), "redis-cache".to_owned()],
                    order: crate::GroupOrder::Name,
                    ..crate::ViewGroups::default()
                },
                ..crate::View::default()
            },
            crate::View {
                name: "hosts by group".to_owned(),
                display: crate::ViewDisplay::HostGroupGrid,
                grid: crate::GridOptions {
                    cells: crate::GridCells::LabelledCells,
                    hide_healthy_groups: true,
                    ..crate::GridOptions::default()
                },
                ..crate::View::default()
            },
            crate::View {
                name: "failing".to_owned(),
                handled: crate::HandledSetting {
                    mode: crate::HandledMode::Hide,
                    hide: crate::HideHandled {
                        host_down: false,
                        ..crate::HideHandled::ALL
                    },
                },
                ..crate::View::default()
            },
            crate::View {
                name: "db events".to_owned(),
                display: crate::ViewDisplay::EventStream,
                stream: crate::StreamOptions {
                    lines: 15,
                    recoveries: true,
                    ..crate::StreamOptions::default()
                },
                ..crate::View::default()
            },
        ];
        groups[0]
            .dashboards
            .push(crate::Dashboard::with_views("databases", views));
        let imported = import_groups(&export_groups(&groups).unwrap()).unwrap();
        let original = &groups[0].dashboards[3];
        let copy = &imported[0].dashboards[3];
        assert_eq!(copy.views.len(), 4);
        for (copy, original) in copy.views.iter().zip(&original.views) {
            assert_ne!(copy.id, original.id, "fresh ids");
            assert_eq!(
                crate::View {
                    id: original.id.clone(),
                    ..copy.clone()
                },
                *original
            );
        }
    }

    #[test]
    fn rc1_exports_import_with_one_view_per_dashboard() {
        let text = "format = \"icygui-dashboards\"\nversion = 3\n\n[[groups]]\nname = \"db\"\n\n\
                    [[groups.dashboards]]\nname = \"all\"\n\n[groups.dashboards.view]\n\
                    problems_only = false\nhide_handled = false\n";
        let imported = import_groups(text).unwrap();
        let views = &imported[0].dashboards[0].views;
        assert_eq!(views.len(), 1);
        assert!(!views[0].id.is_empty());
        assert!(!views[0].problems_only);
        assert_eq!(views[0].handled, crate::HandledSetting::SHOW);
    }

    #[test]
    fn imports_need_a_view_per_dashboard() {
        let text = "[[groups]]\nname = \"db\"\n[[groups.dashboards]]\nname = \"x\"\nviews = []\n";
        match import_groups(text) {
            Err(ConfigError::Invalid(issues)) => {
                assert_eq!(issues[0].path, "groups[0].dashboards[0].views");
            }
            other => panic!("expected invalid groups, got {other:?}"),
        }
    }

    #[test]
    fn an_empty_export_imports_nothing() {
        assert_eq!(import_groups(&export_groups(&[]).unwrap()).unwrap(), []);
    }

    #[test]
    fn long_format_values_are_quoted_in_part() {
        let text = format!("format = \"{}\"\ngroups = []\n", "x".repeat(1_000_000));
        let reason = reason(&text);
        assert!(reason.len() < 300, "{} bytes", reason.len());
        assert!(reason.starts_with("its `format` is \"xxx"), "{reason}");
    }

    #[test]
    fn files_that_are_one_long_line_give_a_short_error() {
        // A minified JSON file picked by mistake.
        let text = format!("{{\"groups\": [{}]}}", "{\"name\": \"x\"},".repeat(200_000));
        match import_groups(&text) {
            Err(ConfigError::Parse { message }) => {
                assert!(message.len() < 1_000, "{} bytes", message.len());
                assert!(message.contains("line 1, column 1"), "{message}");
            }
            other => panic!("expected a parse error, got {other:?}"),
        }
    }
}
