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
use crate::error::ConfigError;
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
    groups: Vec<DashboardGroup>,
}

/// Writes dashboard groups as TOML for sharing: everything about them
/// (names, views, notification settings), marked with a `format` key and
/// the settings format version.
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
/// formats. Every group and dashboard gets a fresh id, so importing the same
/// file twice, or into the environment it came from, never clashes.
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
                "its `format` is {other}, not \"{EXPORT_FORMAT}\""
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
    fn an_empty_export_imports_nothing() {
        assert_eq!(import_groups(&export_groups(&[]).unwrap()).unwrap(), []);
    }
}
