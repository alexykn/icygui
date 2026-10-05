//! Format versions: reading the `version` key, upgrading older layouts step
//! by step, and turning the result into typed settings.
//!
//! Unknown keys are ignored (and logged), so files written by a newer
//! icygui that only *added* settings still load. Changes older versions
//! can't read bump [`CONFIG_VERSION`] and add a step to [`MIGRATIONS`].

use serde::Deserializer;
use serde::de::DeserializeOwned;
use toml::{Table, Value};

use crate::error::ConfigError;
use crate::model::{CONFIG_VERSION, Config};

/// Names the settings in log messages.
const SETTINGS: &str = "settings";

/// One upgrade step: rewrites a table of format version `n` into the layout
/// of version `n + 1`. The caller updates the `version` key.
pub(crate) type Migration = fn(&mut Table) -> Result<(), ConfigError>;

/// `MIGRATIONS[n]` upgrades format version `n` to `n + 1`. The length is
/// tied to [`CONFIG_VERSION`], so bumping the version without adding a step
/// doesn't compile.
const MIGRATIONS: [Migration; CONFIG_VERSION as usize] = [v0_to_v1];

/// Upgrades a parsed settings table to the current format and reads it.
///
/// A missing `version` key means version 0. Keys this version doesn't know
/// are ignored. [`ConfigStore::load`](crate::ConfigStore::load) runs the
/// same steps on the file it reads.
///
/// # Errors
///
/// - [`ConfigError::InvalidVersion`] when `version` is not a whole number
///   of zero or more;
/// - [`ConfigError::UnsupportedVersion`] when it is newer than
///   [`CONFIG_VERSION`];
/// - [`ConfigError::Parse`] when the content doesn't fit the settings
///   (wrong types, unknown enum values, missing required keys).
pub fn migrate(raw: Table) -> Result<Config, ConfigError> {
    let upgraded = upgrade(raw, &MIGRATIONS)?;
    let mut config: Config = deserialize(upgraded, SETTINGS)?;
    config.version = CONFIG_VERSION;
    Ok(config)
}

/// Parses settings text in any supported format version.
///
/// When the migrations leave the content as it was (always for the current
/// version), the typed settings are read straight from the text, so errors
/// point at a line and column.
pub(crate) fn parse_config(text: &str) -> Result<Config, ConfigError> {
    let text = strip_bom(text);
    let table = parse_table(text)?;
    let upgraded = upgrade(table.clone(), &MIGRATIONS)?;
    let mut config: Config = if same_content(&table, &upgraded) {
        deserialize_text(text, SETTINGS)?
    } else {
        deserialize(upgraded, SETTINGS)?
    };
    config.version = CONFIG_VERSION;
    Ok(config)
}

/// Upgrades a dashboard export's `groups` array from format `version` to
/// the current one. Groups use the settings format, so they go through the
/// settings migrations inside a minimal settings table.
pub(crate) fn upgrade_groups(groups: Value, version: u64) -> Result<Value, ConfigError> {
    let mut environment = Table::new();
    environment.insert("groups".to_owned(), groups);
    let mut settings = Table::new();
    settings.insert("version".to_owned(), version_value(version));
    settings.insert(
        "environments".to_owned(),
        Value::Array(vec![Value::Table(environment)]),
    );
    let mut upgraded = upgrade(settings, &MIGRATIONS)?;
    upgraded
        .remove("environments")
        .and_then(|environments| match environments {
            Value::Array(mut list) if !list.is_empty() => Some(list.swap_remove(0)),
            _ => None,
        })
        .and_then(|environment| match environment {
            Value::Table(mut table) => table.remove("groups"),
            _ => None,
        })
        .ok_or_else(|| ConfigError::Parse {
            message: format!("dashboard groups of format version {version} could not be upgraded"),
        })
}

/// Parses TOML text into a table; syntax errors carry line and column.
pub(crate) fn parse_table(text: &str) -> Result<Table, ConfigError> {
    toml::from_str(text).map_err(|error| ConfigError::parse(&error))
}

/// The format version a table declares; 0 when it has no `version` key.
pub(crate) fn format_version(table: &Table) -> Result<u64, ConfigError> {
    match table.get("version") {
        None => Ok(0),
        Some(Value::Integer(version)) => u64::try_from(*version)
            .map_err(|_| ConfigError::InvalidVersion(format!("{version} is negative"))),
        Some(other) => Err(ConfigError::InvalidVersion(format!(
            "expected a whole number, found {} `{other}`",
            other.type_str()
        ))),
    }
}

/// Reads `T` from TOML text, with line and column in errors. Unknown keys
/// are logged and ignored; `what` names the text in the log.
pub(crate) fn deserialize_text<T: DeserializeOwned>(
    text: &str,
    what: &str,
) -> Result<T, ConfigError> {
    let deserializer =
        toml::Deserializer::parse(text).map_err(|error| ConfigError::parse(&error))?;
    deserialize(deserializer, what)
}

/// Reads `T` from parsed TOML (a [`Table`] or a [`Value`]). Unknown keys
/// are logged and ignored; `what` names the content in the log.
pub(crate) fn deserialize<'de, T, D>(deserializer: D, what: &str) -> Result<T, ConfigError>
where
    T: DeserializeOwned,
    D: Deserializer<'de, Error = toml::de::Error>,
{
    let (value, unknown) = deserialize_collecting(deserializer)?;
    for key in unknown {
        tracing::warn!(%key, "ignoring unknown key in the {what}");
    }
    Ok(value)
}

/// Reads `T` and returns the keys it ignored, as dotted paths with array
/// indices (`environments.0.nmae`).
fn deserialize_collecting<'de, T, D>(deserializer: D) -> Result<(T, Vec<String>), ConfigError>
where
    T: DeserializeOwned,
    D: Deserializer<'de, Error = toml::de::Error>,
{
    let mut unknown = Vec::new();
    let value = serde_ignored::deserialize(deserializer, |path| unknown.push(path.to_string()))
        .map_err(|error| ConfigError::parse(&error))?;
    Ok((value, unknown))
}

/// Drops a leading byte order mark, which some editors write.
pub(crate) fn strip_bom(text: &str) -> &str {
    text.strip_prefix('\u{feff}').unwrap_or(text)
}

/// Runs `steps` from the table's version up to `steps.len()`, updating the
/// `version` key after each step.
fn upgrade(mut table: Table, steps: &[Migration]) -> Result<Table, ConfigError> {
    let found = format_version(&table)?;
    let supported = u64::try_from(steps.len()).unwrap_or(u64::MAX);
    let start = usize::try_from(found)
        .ok()
        .filter(|start| *start <= steps.len())
        .ok_or(ConfigError::UnsupportedVersion { found, supported })?;
    for (from, step) in steps.iter().enumerate().skip(start) {
        step(&mut table)?;
        let to = from + 1;
        table.insert(
            "version".to_owned(),
            version_value(u64::try_from(to).unwrap_or(u64::MAX)),
        );
        tracing::info!(from, to, "upgraded the settings format");
    }
    Ok(table)
}

/// Whether two tables hold the same content apart from their versions.
fn same_content(left: &Table, right: &Table) -> bool {
    without_version(left).eq(without_version(right))
}

fn without_version(table: &Table) -> impl Iterator<Item = (&String, &Value)> {
    table.iter().filter(|(key, _)| key.as_str() != "version")
}

fn version_value(version: u64) -> Value {
    Value::Integer(i64::try_from(version).unwrap_or(i64::MAX))
}

/// Version 0 is a file without a `version` key: written by hand or by a
/// pre-release build. Its layout already is version 1's, so there is
/// nothing to rewrite.
#[expect(
    clippy::unnecessary_wraps,
    reason = "every migration step has the same signature"
)]
fn v0_to_v1(_table: &mut Table) -> Result<(), ConfigError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(text: &str) -> Table {
        toml::from_str(text).unwrap()
    }

    fn append(marker: &'static str) -> impl Fn(&mut Table) -> Result<(), ConfigError> {
        move |table| {
            let trail = table
                .entry("trail")
                .or_insert_with(|| Value::Array(Vec::new()));
            if let Value::Array(items) = trail {
                items.push(Value::String(marker.to_owned()));
            }
            Ok(())
        }
    }

    fn trail(table: &Table) -> Vec<&str> {
        table["trail"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item.as_str().unwrap())
            .collect()
    }

    fn step_a(table: &mut Table) -> Result<(), ConfigError> {
        append("a")(table)
    }

    fn step_b(table: &mut Table) -> Result<(), ConfigError> {
        append("b")(table)
    }

    fn step_c(table: &mut Table) -> Result<(), ConfigError> {
        append("c")(table)
    }

    fn failing(_table: &mut Table) -> Result<(), ConfigError> {
        Err(ConfigError::Parse {
            message: "step failed".to_owned(),
        })
    }

    const STEPS: [Migration; 3] = [step_a, step_b, step_c];

    #[test]
    fn reads_the_format_version() {
        assert_eq!(format_version(&table("")).unwrap(), 0);
        assert_eq!(format_version(&table("version = 0")).unwrap(), 0);
        assert_eq!(format_version(&table("version = 7")).unwrap(), 7);
    }

    #[test]
    fn rejects_versions_that_are_not_whole_numbers() {
        for (text, reason) in [
            ("version = -1", "-1 is negative"),
            (
                "version = \"1\"",
                "expected a whole number, found string `\"1\"`",
            ),
            (
                "version = 1.0",
                "expected a whole number, found float `1.0`",
            ),
            (
                "version = true",
                "expected a whole number, found boolean `true`",
            ),
        ] {
            match format_version(&table(text)) {
                Err(ConfigError::InvalidVersion(message)) => assert_eq!(message, reason),
                other => panic!("{text}: expected an invalid version, got {other:?}"),
            }
        }
    }

    #[test]
    fn upgrades_through_every_step_in_order() {
        let upgraded = upgrade(table("x = 1"), &STEPS).unwrap();
        assert_eq!(trail(&upgraded), ["a", "b", "c"]);
        assert_eq!(upgraded["version"].as_integer(), Some(3));
        assert_eq!(upgraded["x"].as_integer(), Some(1));
    }

    #[test]
    fn upgrades_start_at_the_tables_version() {
        let upgraded = upgrade(table("version = 2"), &STEPS).unwrap();
        assert_eq!(trail(&upgraded), ["c"]);
        assert_eq!(upgraded["version"].as_integer(), Some(3));

        let current = upgrade(table("version = 3"), &STEPS).unwrap();
        assert_eq!(current, table("version = 3"));
    }

    #[test]
    fn newer_versions_are_rejected() {
        match upgrade(table("version = 4"), &STEPS) {
            Err(ConfigError::UnsupportedVersion { found, supported }) => {
                assert_eq!((found, supported), (4, 3));
            }
            other => panic!("expected an unsupported version, got {other:?}"),
        }
        assert!(matches!(
            upgrade(table("version = 9223372036854775807"), &STEPS),
            Err(ConfigError::UnsupportedVersion { .. })
        ));
    }

    #[test]
    fn failing_steps_stop_the_upgrade() {
        let steps: [Migration; 2] = [step_a, failing];
        match upgrade(table(""), &steps) {
            Err(ConfigError::Parse { message }) => assert_eq!(message, "step failed"),
            other => panic!("expected the step's error, got {other:?}"),
        }
    }

    #[test]
    fn the_real_migrations_reach_the_current_version() {
        let upgraded = upgrade(table(""), &MIGRATIONS).unwrap();
        assert_eq!(
            upgraded["version"].as_integer(),
            Some(i64::from(CONFIG_VERSION))
        );
        assert_eq!(MIGRATIONS.len(), CONFIG_VERSION as usize);
    }

    #[test]
    fn compares_content_without_versions() {
        assert!(same_content(
            &table("version = 0\na = 1"),
            &table("version = 1\na = 1")
        ));
        assert!(same_content(&table("a = 1"), &table("version = 1\na = 1")));
        assert!(!same_content(&table("a = 1"), &table("a = 2")));
        assert!(!same_content(&table("a = 1"), &table("a = 1\nb = 1")));
    }

    #[test]
    fn strips_a_byte_order_mark() {
        assert_eq!(strip_bom("\u{feff}version = 1"), "version = 1");
        assert_eq!(strip_bom("version = 1"), "version = 1");
    }

    #[test]
    fn upgrading_groups_keeps_them() {
        let groups = Value::Array(vec![Value::Table(table("name = \"databases\""))]);
        assert_eq!(upgrade_groups(groups.clone(), 0).unwrap(), groups);
        assert!(matches!(
            upgrade_groups(groups, u64::from(CONFIG_VERSION) + 1),
            Err(ConfigError::UnsupportedVersion { .. })
        ));
    }

    #[test]
    fn unknown_keys_are_collected_with_their_paths() {
        let text = "version = 1\nbogus = 1\n\n[general]\nclose_to_try = true\n\n\
                    [[environments]]\nnmae = \"x\"\n\n[[environments]]\nname = \"y\"\n\n\
                    [environments.tls]\nca_fil = \"/ca.crt\"\n";
        let expected = [
            "bogus",
            "environments.0.nmae",
            "environments.1.tls.ca_fil",
            "general.close_to_try",
        ];

        let from_text = toml::Deserializer::parse(text).unwrap();
        let (config, mut unknown): (Config, _) = deserialize_collecting(from_text).unwrap();
        unknown.sort();
        assert_eq!(unknown, expected);
        assert_eq!(config.environments[1].name, "y");
        assert!(
            config.general.close_to_tray,
            "the misspelt key changed nothing"
        );

        let (from_table, mut unknown): (Config, _) = deserialize_collecting(table(text)).unwrap();
        unknown.sort();
        assert_eq!(unknown, expected);
        assert_eq!(from_table, config);
    }

    #[test]
    fn current_files_are_read_from_the_text() {
        // Same content, so errors come with a line number.
        let error = parse_config("version = 1\n\n[general]\ntheme = \"sepia\"\n").unwrap_err();
        assert!(error.to_string().contains("line 4"), "{error}");
        let error = migrate(table("version = 1\n\n[general]\ntheme = \"sepia\"\n")).unwrap_err();
        assert!(error.to_string().contains("general.theme"), "{error}");
    }
}
