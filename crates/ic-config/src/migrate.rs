//! Format versions: reading the `version` key, upgrading older layouts step
//! by step, and turning the result into typed settings.
//!
//! Unknown keys are ignored (and logged), so files written by a newer
//! icygui that only *added* settings still load. Changes older versions
//! can't read bump [`CONFIG_VERSION`] and add a step to [`MIGRATIONS`].

use serde::de::DeserializeOwned;
use serde::{Deserializer, Serialize};
use toml::{Table, Value};

use crate::error::{ConfigError, MAX_VALUE_CHARS, excerpt};
use crate::model::{CONFIG_VERSION, Config};

/// Names the settings in log messages.
const SETTINGS: &str = "settings";

/// One upgrade step: rewrites a table of format version `n` into the layout
/// of version `n + 1`. The caller updates the `version` key.
pub(crate) type Migration = fn(&mut Table) -> Result<(), ConfigError>;

/// `MIGRATIONS[n]` upgrades format version `n` to `n + 1`. The length is
/// tied to [`CONFIG_VERSION`], so bumping the version without adding a step
/// doesn't compile.
const MIGRATIONS: [Migration; CONFIG_VERSION as usize] = [v0_to_v1, v1_to_v2, v2_to_v3];

/// Settings read from text, and what reading them noticed. Nothing is
/// logged yet, so the caller decides whether it is worth reporting.
#[derive(Debug)]
pub(crate) struct Parsed {
    /// The settings, upgraded to the current format.
    pub(crate) config: Config,
    /// The keys that were ignored because the settings don't have them, as
    /// sorted dotted paths (`environments.0.nmae`).
    pub(crate) unknown_keys: Vec<String>,
    /// The format version the text declared (0 without a `version` key).
    pub(crate) version: u64,
}

/// Upgrades a parsed settings table to the current format and reads it.
///
/// A missing `version` key means version 0. Keys this version doesn't know
/// are ignored and logged. [`ConfigStore::load`](crate::ConfigStore::load)
/// runs the same steps on the file it reads.
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
    let version = format_version(&raw)?;
    let upgraded = upgrade(raw, &MIGRATIONS)?;
    let (mut config, unknown_keys) = read_upgraded(&upgraded, None)?;
    config.version = CONFIG_VERSION;
    let parsed = Parsed {
        config,
        unknown_keys,
        version,
    };
    parsed.report(SETTINGS);
    Ok(parsed.config)
}

/// Parses settings text in any supported format version, without logging
/// anything (see [`Parsed::report`]).
///
/// When the migrations leave the content as it was (always for the current
/// version), the typed settings are read straight from the text, so errors
/// point at a line and column.
pub(crate) fn parse_config(text: &str) -> Result<Parsed, ConfigError> {
    let text = strip_bom(text);
    let table = parse_table(text)?;
    let version = format_version(&table)?;
    let upgraded = upgrade(table.clone(), &MIGRATIONS)?;
    let same = same_content(&table, &upgraded);
    let (mut config, unknown_keys) = read_upgraded(&upgraded, same.then_some(text))?;
    config.version = CONFIG_VERSION;
    Ok(Parsed {
        config,
        unknown_keys,
        version,
    })
}

impl Parsed {
    /// Logs what reading the `what` noticed: the keys it ignored, with a
    /// warning of its own for ones that look like passwords, and an
    /// upgrade from an older format.
    pub(crate) fn report(&self, what: &str) {
        log_unknown_keys(&self.unknown_keys, what);
        if self.version < u64::from(CONFIG_VERSION) {
            tracing::info!(
                from = self.version,
                to = CONFIG_VERSION,
                "upgraded the {what} format; the next save writes the new format"
            );
        }
    }
}

/// Reads settings from a table already in the current layout, and the keys
/// it ignored. `text`, if given, holds the same content and is read
/// instead, so that errors have a line and column.
fn read_upgraded(
    upgraded: &Table,
    text: Option<&str>,
) -> Result<(Config, Vec<String>), ConfigError> {
    let (config, mut unknown_keys): (Config, _) = match text {
        Some(text) => {
            let deserializer = toml::Deserializer::parse(text)
                .map_err(|error| ConfigError::parse(&error, Some(text)))?;
            deserialize_collecting(deserializer)
                .map_err(|error| ConfigError::parse(&error, Some(text)))?
        }
        None => deserialize_collecting(upgraded.clone())
            .map_err(|error| ConfigError::parse(&error, None))?,
    };
    unknown_keys.extend(unknown_tagged_keys(upgraded, &config));
    unknown_keys.sort();
    unknown_keys.dedup();
    Ok((config, unknown_keys))
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
    toml::from_str(text).map_err(|error| ConfigError::parse(&error, Some(text)))
}

/// The format version a table declares; 0 when it has no `version` key.
pub(crate) fn format_version(table: &Table) -> Result<u64, ConfigError> {
    match table.get("version") {
        None => Ok(0),
        Some(Value::Integer(version)) => u64::try_from(*version)
            .map_err(|_| ConfigError::InvalidVersion(format!("{version} is negative"))),
        Some(other) => Err(ConfigError::InvalidVersion(format!(
            "expected a whole number, found {} `{}`",
            other.type_str(),
            excerpt(&other.to_string(), MAX_VALUE_CHARS)
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
        toml::Deserializer::parse(text).map_err(|error| ConfigError::parse(&error, Some(text)))?;
    let (value, unknown_keys) = deserialize_collecting(deserializer)
        .map_err(|error| ConfigError::parse(&error, Some(text)))?;
    log_unknown_keys(&unknown_keys, what);
    Ok(value)
}

/// Reads `T` from parsed TOML (a [`Table`] or a [`Value`]). Unknown keys
/// are logged and ignored; `what` names the content in the log.
pub(crate) fn deserialize<'de, T, D>(deserializer: D, what: &str) -> Result<T, ConfigError>
where
    T: DeserializeOwned,
    D: Deserializer<'de, Error = toml::de::Error>,
{
    let (value, unknown_keys) =
        deserialize_collecting(deserializer).map_err(|error| ConfigError::parse(&error, None))?;
    log_unknown_keys(&unknown_keys, what);
    Ok(value)
}

/// Reads `T` and returns the keys it ignored, as dotted paths with array
/// indices (`environments.0.nmae`).
///
/// Keys inside internally tagged enums (`auth`, see
/// [`unknown_tagged_keys`]) are not among them: serde buffers those tables
/// before the ignored keys could be seen.
fn deserialize_collecting<'de, T, D>(deserializer: D) -> Result<(T, Vec<String>), toml::de::Error>
where
    T: DeserializeOwned,
    D: Deserializer<'de, Error = toml::de::Error>,
{
    let mut unknown = Vec::new();
    let value = serde_ignored::deserialize(deserializer, |path| unknown.push(path.to_string()))?;
    Ok((value, unknown))
}

/// The unknown keys inside the settings' internally tagged tables, which
/// [`deserialize_collecting`] can't see: each environment's `auth` and the
/// `object` of each notification override. They are the keys of the table
/// in the file that the settings read from it don't have when written
/// back. `auth` is where a password is most likely to be put by mistake.
fn unknown_tagged_keys(upgraded: &Table, config: &Config) -> Vec<String> {
    let mut unknown = Vec::new();
    let Some(Value::Array(environments)) = upgraded.get("environments") else {
        return unknown;
    };
    for (index, (raw, environment)) in environments.iter().zip(&config.environments).enumerate() {
        let path = format!("environments.{index}");
        if let Some(Value::Table(auth)) = raw.get("auth") {
            missing_keys(
                auth,
                &environment.auth,
                &format!("{path}.auth"),
                &mut unknown,
            );
        }
        let objects = raw
            .get("notifications")
            .and_then(|notifications| notifications.get("objects"))
            .and_then(Value::as_array);
        for (entry_index, (raw, entry)) in objects
            .into_iter()
            .flatten()
            .zip(&environment.notifications.objects)
            .enumerate()
        {
            if let Some(Value::Table(object)) = raw.get("object") {
                let object_path = format!("{path}.notifications.objects.{entry_index}.object");
                missing_keys(object, &entry.object, &object_path, &mut unknown);
            }
        }
    }
    unknown
}

/// Adds to `unknown` the keys of `raw` (and of its nested tables) that
/// `typed`, written as TOML, doesn't have.
fn missing_keys(raw: &Table, typed: &impl Serialize, path: &str, unknown: &mut Vec<String>) {
    if let Ok(written) = Table::try_from(typed) {
        compare_keys(raw, &written, path, unknown);
    }
}

fn compare_keys(raw: &Table, written: &Table, path: &str, unknown: &mut Vec<String>) {
    for (key, value) in raw {
        let key_path = format!("{path}.{key}");
        match (value, written.get(key)) {
            (_, None) => unknown.push(key_path),
            (Value::Table(raw), Some(Value::Table(written))) => {
                compare_keys(raw, written, &key_path, unknown);
            }
            _ => {}
        }
    }
}

/// Logs each ignored key of the `what`. Keys that look like they hold a
/// password get a warning of their own: icygui never reads secrets from
/// its files, and the value should not stay in one.
pub(crate) fn log_unknown_keys(keys: &[String], what: &str) {
    for key in keys {
        if looks_secret(key) {
            tracing::warn!(
                %key,
                "ignoring a password-like key in the {what}: icygui keeps passwords in the \
                 system keychain and never reads them from files; remove it from the file"
            );
        } else {
            tracing::warn!(%key, "ignoring unknown key in the {what}");
        }
    }
}

/// Whether the last part of a dotted key path names a secret
/// (`environments.0.auth.password`, `api_token`, `client-secret`).
fn looks_secret(path: &str) -> bool {
    let name = path
        .rsplit('.')
        .next()
        .unwrap_or(path)
        .to_ascii_lowercase()
        .replace('-', "_");
    matches!(name.as_str(), "pass" | "pw" | "pwd")
        || [
            "password",
            "passwd",
            "passphrase",
            "secret",
            "token",
            "apikey",
            "api_key",
            "credential",
            "private_key",
        ]
        .iter()
        .any(|word| name.contains(word))
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
        table.insert(
            "version".to_owned(),
            version_value(u64::try_from(from + 1).unwrap_or(u64::MAX)),
        );
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

/// Version 2 lists an environment's API URLs (ENV-12): version 1's `url`
/// becomes the only entry of `urls`, and the pin and server name, which
/// belonged to that one server, move from `tls` into the entry. The CA
/// file and the system roots stay in `tls` (they hold for every URL).
///
/// Environments that aren't tables, or already have `urls`, are left as
/// they are; reading the settings reports what doesn't fit.
#[expect(
    clippy::unnecessary_wraps,
    reason = "every migration step has the same signature"
)]
fn v1_to_v2(table: &mut Table) -> Result<(), ConfigError> {
    let Some(Value::Array(environments)) = table.get_mut("environments") else {
        return Ok(());
    };
    for environment in environments {
        if let Value::Table(environment) = environment
            && !environment.contains_key("urls")
        {
            move_url_into_urls(environment);
        }
    }
    Ok(())
}

/// [`v1_to_v2`] for one environment table.
fn move_url_into_urls(environment: &mut Table) {
    let url = environment.remove("url");
    let (pin, server_name) = match environment.get_mut("tls") {
        Some(Value::Table(tls)) => (tls.remove("pinned_sha256"), tls.remove("server_name")),
        _ => (None, None),
    };
    if url.is_none() && pin.is_none() && server_name.is_none() {
        return;
    }
    let mut entry = Table::new();
    entry.insert(
        "url".to_owned(),
        url.unwrap_or_else(|| Value::String(String::new())),
    );
    if let Some(pin) = pin {
        entry.insert("pinned_sha256".to_owned(), pin);
    }
    if let Some(server_name) = server_name {
        entry.insert("server_name".to_owned(), server_name);
    }
    environment.insert("urls".to_owned(), Value::Array(vec![Value::Table(entry)]));
}

/// Version 3 keeps how the app looks in a table of its own,
/// `[appearance]` (the settings panel's appearance page): `general.theme`
/// moves there. An `appearance.theme` already in the file wins (a file
/// edited by hand after a newer icygui wrote it); a `general` that isn't
/// a table is left for reading to report.
#[expect(
    clippy::unnecessary_wraps,
    reason = "every migration step has the same signature"
)]
fn v2_to_v3(table: &mut Table) -> Result<(), ConfigError> {
    let Some(Value::Table(general)) = table.get_mut("general") else {
        return Ok(());
    };
    let Some(theme) = general.remove("theme") else {
        return Ok(());
    };
    // An `appearance` that isn't a table can't take it: reading reports
    // that one.
    if let Value::Table(appearance) = table
        .entry("appearance")
        .or_insert_with(|| Value::Table(Table::new()))
    {
        appearance.entry("theme").or_insert(theme);
    }
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
        let current = "version = 3\n\n[appearance]\ntheme = \"sepia\"\n";
        let error = parse_config(current).unwrap_err();
        assert!(error.to_string().contains("line 4"), "{error}");
        let error = migrate(table(current)).unwrap_err();
        assert!(error.to_string().contains("appearance.theme"), "{error}");
        // Without environments or a theme the upgrade changes nothing either.
        let error = parse_config("version = 1\n\n[general]\nlog_level = \"loud\"\n").unwrap_err();
        assert!(error.to_string().contains("line 4"), "{error}");
    }

    #[test]
    fn version_2_themes_move_into_the_appearance_table() {
        let parsed =
            parse_config("version = 2\n\n[general]\ntheme = \"light\"\nclose_to_tray = false\n")
                .unwrap();
        assert_eq!(parsed.version, 2);
        assert!(parsed.unknown_keys.is_empty(), "{:?}", parsed.unknown_keys);
        assert_eq!(parsed.config.appearance.theme, crate::ThemeChoice::Light);
        assert!(!parsed.config.general.close_to_tray, "the rest stays");
        assert_eq!(
            parsed.config.appearance.row_density,
            crate::RowDensity::Comfortable,
            "the new settings take their defaults"
        );

        // An appearance table already there keeps its own theme and the
        // rest of what it holds.
        let parsed = parse_config(
            "version = 2\n[general]\ntheme = \"light\"\n\
             [appearance]\ntheme = \"dark\"\nrow_density = \"compact\"\n",
        )
        .unwrap();
        assert_eq!(parsed.config.appearance.theme, crate::ThemeChoice::Dark);
        assert_eq!(
            parsed.config.appearance.row_density,
            crate::RowDensity::Compact
        );

        // Without a theme nothing moves; a new file follows the system.
        let parsed = parse_config("version = 2\n[general]\nclose_to_tray = true\n").unwrap();
        assert_eq!(parsed.config.appearance, crate::Appearance::default());
        assert_eq!(
            crate::Appearance::default().theme,
            crate::ThemeChoice::System
        );

        // A bad theme from version 2 is reported where it went.
        let error = parse_config("version = 2\n[general]\ntheme = \"sepia\"\n").unwrap_err();
        assert!(error.to_string().contains("appearance.theme"), "{error}");
        // A `general` or `appearance` that isn't a table is reported, not
        // rewritten.
        assert!(parse_config("version = 2\ngeneral = 3\n").is_err());
        let mut odd = table("version = 2\nappearance = 3\n[general]\ntheme = \"dark\"\n");
        v2_to_v3(&mut odd).unwrap();
        assert_eq!(odd["appearance"].as_integer(), Some(3));
    }

    #[test]
    fn version_1_urls_move_into_the_url_list() {
        let text = r#"
version = 1

[[environments]]
name = "prod"
url = "https://master-01:5665"

[environments.tls]
ca_file = "/etc/icinga2/ca.crt"
pinned_sha256 = "AB:CD"
server_name = "master-01.example.com"
use_system_roots = false

[[environments]]
name = "staging"
url = "https://staging:5665"

[[environments]]
name = "no url"
"#;
        let parsed = parse_config(text).unwrap();
        assert_eq!(parsed.version, 1);
        assert!(parsed.unknown_keys.is_empty(), "{:?}", parsed.unknown_keys);
        let [prod, staging, bare] = &parsed.config.environments[..] else {
            panic!("three environments");
        };
        assert_eq!(
            prod.urls,
            [crate::ApiUrl {
                url: "https://master-01:5665".to_owned(),
                pinned_sha256: Some("AB:CD".to_owned()),
                server_name: Some("master-01.example.com".to_owned()),
            }]
        );
        assert_eq!(
            prod.tls.ca_file.as_deref(),
            Some(std::path::Path::new("/etc/icinga2/ca.crt"))
        );
        assert!(!prod.tls.use_system_roots);
        assert_eq!(staging.urls, [crate::ApiUrl::new("https://staging:5665")]);
        assert!(bare.urls.is_empty());
        // Errors in a migrated environment still name the key.
        let error = parse_config("version = 1\n[[environments]]\nurl = 5665\n").unwrap_err();
        assert!(error.to_string().contains("urls"), "{error}");
    }

    #[test]
    fn version_0_files_reach_the_url_list_too() {
        let parsed = parse_config(
            "[[environments]]\nname = \"prod\"\nurl = \"https://m:5665\"\n\n\
             [environments.tls]\npinned_sha256 = \"AB\"\n",
        )
        .unwrap();
        assert_eq!(parsed.version, 0);
        let urls = &parsed.config.environments[0].urls;
        assert_eq!(urls.len(), 1);
        assert_eq!(urls[0].url, "https://m:5665");
        assert_eq!(urls[0].pinned_sha256.as_deref(), Some("AB"));
    }

    #[test]
    fn a_pin_without_a_url_is_kept() {
        let mut settings = table(
            "version = 1\n[[environments]]\nname = \"x\"\n[environments.tls]\nserver_name = \"m\"\n",
        );
        v1_to_v2(&mut settings).unwrap();
        let environment = settings["environments"][0].as_table().unwrap();
        assert_eq!(
            environment["urls"][0]["url"].as_str(),
            Some(""),
            "an empty URL validation reports"
        );
        assert_eq!(environment["urls"][0]["server_name"].as_str(), Some("m"));
        assert!(
            !environment["tls"]
                .as_table()
                .unwrap()
                .contains_key("server_name")
        );
    }

    #[test]
    fn url_lists_may_be_written_as_strings() {
        let parsed = parse_config(
            "version = 2\n[[environments]]\nname = \"prod\"\n\
             urls = [\"https://master-01:5665\", { url = \"https://master-02:5665\", pinned_sha256 = \"AB\", bogus = 1 }]\n",
        )
        .unwrap();
        let urls = &parsed.config.environments[0].urls;
        assert_eq!(urls[0], crate::ApiUrl::new("https://master-01:5665"));
        assert_eq!(urls[1].url, "https://master-02:5665");
        assert_eq!(urls[1].pinned_sha256.as_deref(), Some("AB"));
        assert_eq!(parsed.unknown_keys, ["environments.0.urls.1.bogus"]);
        let error = parse_config("version = 2\n[[environments]]\nurls = [5665]\n").unwrap_err();
        assert!(error.to_string().contains("a URL, or a table"), "{error}");
    }

    #[test]
    fn parsing_reports_the_version_and_unknown_keys() {
        let parsed = parse_config("[general]\nthem = \"light\"\n").unwrap();
        assert_eq!(parsed.version, 0);
        assert_eq!(parsed.config.version, CONFIG_VERSION);
        assert_eq!(parsed.unknown_keys, ["general.them"]);
        let parsed = parse_config("version = 2\n").unwrap();
        assert_eq!(parsed.version, 2);
        assert!(parsed.unknown_keys.is_empty());
    }

    #[test]
    fn unknown_keys_in_tagged_tables_are_found() {
        let text = r#"
version = 1

[[environments]]
name = "prod"

[environments.auth]
kind = "basic"
username = "root"
password = "hunter2"

[[environments.notifications.objects]]
mode = "mute"
object = { type = "service", key = { host = "h", name = "s", port = 1 }, colour = "red" }

[[environments.notifications.objects]]
mode = "watch"
object = { type = "host", name = "h" }

[[environments]]
name = "staging"
auth = { kind = "client_certificate", cert_path = "/c.pem", key_path = "/k.pem", key_password = "x" }
"#;
        let expected = [
            "environments.0.auth.password",
            "environments.0.notifications.objects.0.object.colour",
            "environments.0.notifications.objects.0.object.key.port",
            "environments.1.auth.key_password",
        ];
        assert_eq!(parse_config(text).unwrap().unknown_keys, expected);
        // Migrating the parsed table finds the same, without positions.
        let (_, unknown) = read_upgraded(&table(text), None).unwrap();
        assert_eq!(unknown, expected);
    }

    #[test]
    fn files_written_by_icygui_have_no_unknown_keys() {
        let mut config = Config::default();
        let mut prod = crate::Environment::new(
            "prod",
            "https://master-01:5665",
            crate::AuthConfig::Basic {
                username: "icygui".to_owned(),
            },
        );
        prod.notifications.objects.push(ic_rules::ObjectOverride {
            object: ic_model::ObjectKey::service("h", "s"),
            mode: ic_rules::ObjectMode::Mute,
            until: None,
        });
        prod.notifications.objects.push(ic_rules::ObjectOverride {
            object: ic_model::ObjectKey::host("h"),
            mode: ic_rules::ObjectMode::Watch,
            until: None,
        });
        let mut staging = prod.clone();
        staging.auth = crate::AuthConfig::ClientCertificate {
            cert_path: "/c.pem".into(),
            key_path: "/k.pem".into(),
        };
        config.environments = vec![prod, staging];
        let text = toml::to_string(&config).unwrap();
        assert_eq!(
            parse_config(&text).unwrap().unknown_keys,
            Vec::<String>::new()
        );
    }

    #[test]
    fn password_like_keys_are_recognised() {
        for key in [
            "environments.0.auth.password",
            "environments.0.auth.Password",
            "environments.0.tls.key_password",
            "environments.0.passwd",
            "environments.0.pass",
            "environments.0.api-token",
            "environments.0.client_secret",
            "environments.0.apikey",
            "environments.0.credentials",
            "environments.0.tls.private_key",
            "password",
        ] {
            assert!(looks_secret(key), "{key}");
        }
        for key in [
            "environments.0.nmae",
            "environments.0.passive",
            "environments.0.tls.ca_fil",
            "general.close_to_try",
        ] {
            assert!(!looks_secret(key), "{key}");
        }
    }
}
