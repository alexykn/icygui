//! Loading real-world files: missing, partial, hand-written, old, newer,
//! corrupt; restoring the backup.

use std::fs;

use ic_config::{
    AuthConfig, CONFIG_VERSION, Config, ConfigError, ConfigStore, General, ThemeChoice, TlsConfig,
    migrate,
};
use ic_rules::{NotificationSettings, ScopeSetting};

use crate::fixtures::{full_config, ids, store_with};

const PROD_ID: &str = "6f1c2a8e-0b7d-4c1e-9a3f-2d5b8c7e1f40";
const GROUP_ID: &str = "0c9e2f4a-5b6d-4e7f-8a9b-1c2d3e4f5a6b";
const DASHBOARD_ID: &str = "9a8b7c6d-5e4f-4a3b-2c1d-0e9f8a7b6c5d";

/// A small hand-written file: no version, but with ids.
fn unversioned() -> String {
    format!(
        r#"
active_environment = "{PROD_ID}"

[general]
theme = "light"

[[environments]]
id = "{PROD_ID}"
name = "prod-cluster"
url = "https://master-01.example.com:5665"
auth = {{ kind = "basic", username = "icygui" }}

[[environments.groups]]
id = "{GROUP_ID}"
name = "databases"

[[environments.groups.dashboards]]
id = "{DASHBOARD_ID}"
name = "replication"
view = {{ filter = 'host.vars.role == "postgres"' }}
"#
    )
}

fn parse_message(result: Result<Config, ConfigError>) -> String {
    match result {
        Err(ConfigError::Parse { message }) => message,
        other => panic!("expected a parse error, got {other:?}"),
    }
}

#[test]
fn a_missing_file_gives_defaults_and_creates_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(dir.path().join("icygui/config.toml"));
    assert_eq!(store.load().unwrap(), Config::default());
    assert!(!dir.path().join("icygui").exists());
}

#[test]
fn an_empty_file_gives_defaults() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        store_with(dir.path(), "").load().unwrap(),
        Config::default()
    );
    assert_eq!(
        store_with(dir.path(), "\n# nothing yet\n").load().unwrap(),
        Config::default()
    );
}

#[test]
fn unknown_keys_are_ignored_everywhere() {
    let dir = tempfile::tempdir().unwrap();
    let text = format!(
        r#"
version = 1
future_feature = {{ enabled = true }}

[general]
theme = "light"
colour_blind_mode = true

[[environments]]
id = "{PROD_ID}"
name = "prod-cluster"
url = "https://master-01.example.com:5665"
ssh_tunnel = "bastion"

[environments.auth]
kind = "basic"
username = "icygui"
realm = "icinga"

[environments.tls]
use_system_roots = true
min_version = "1.3"

[[environments.groups]]
id = "{GROUP_ID}"
name = "databases"
icon = "db"

[environments.groups.notifications]
mode = "on"
priority = 3

[[environments.groups.dashboards]]
id = "{DASHBOARD_ID}"
name = "replication"
layout = "grid"

[environments.groups.dashboards.view]
problems_only = false
columns = ["state", "host", "service"]

[environments.notifications]
enabled = false
channel = "slack"

[environments.notifications.default_rule]
escalate_after = 600

[environments.notifications.quiet_hours]
holidays = ["2026-12-25"]
"#
    );
    let config = store_with(dir.path(), &text).load().unwrap();
    assert_eq!(config.general.theme, ThemeChoice::Light);
    let environment = &config.environments[0];
    assert_eq!(environment.id, PROD_ID);
    assert_eq!(
        environment.auth,
        AuthConfig::Basic {
            username: "icygui".to_owned()
        }
    );
    assert!(environment.tls.use_system_roots);
    assert_eq!(environment.groups[0].notifications, ScopeSetting::On);
    assert!(!environment.groups[0].dashboards[0].view.problems_only);
    assert!(!environment.notifications.enabled);
}

#[test]
fn missing_sections_and_keys_take_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let text = format!(
        r#"
version = 1

[[environments]]
id = "{PROD_ID}"
name = "prod-cluster"
url = "https://master-01.example.com:5665"
"#
    );
    let config = store_with(dir.path(), &text).load().unwrap();
    assert_eq!(config.general, General::default());
    assert_eq!(config.active_environment, None);
    let environment = &config.environments[0];
    assert_eq!(
        environment.auth,
        AuthConfig::Basic {
            username: String::new()
        }
    );
    assert_eq!(environment.tls, TlsConfig::default());
    assert_eq!(environment.author, None);
    assert!(environment.groups.is_empty());
    assert_eq!(environment.notifications, NotificationSettings::default());
}

#[test]
fn unversioned_files_are_upgraded_in_memory() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_with(dir.path(), &unversioned());
    let config = store.load().unwrap();
    assert_eq!(config.version, CONFIG_VERSION);
    assert_eq!(config.active_environment.as_deref(), Some(PROD_ID));
    assert_eq!(config.general.theme, ThemeChoice::Light);
    let dashboard = config.environments[0]
        .dashboard(GROUP_ID, DASHBOARD_ID)
        .unwrap();
    assert_eq!(dashboard.view.filter, r#"host.vars.role == "postgres""#);
    assert!(config.validate().is_empty(), "{:?}", config.validate());

    // Loading alone doesn't rewrite the file; the next save upgrades it and
    // keeps the original as the backup.
    assert_eq!(fs::read_to_string(store.path()).unwrap(), unversioned());
    assert!(!store.backup_path().exists());
    store.save(&config).unwrap();
    let saved = fs::read_to_string(store.path()).unwrap();
    assert!(saved.contains(&format!("\nversion = {CONFIG_VERSION}\n")));
    assert_eq!(
        fs::read_to_string(store.backup_path()).unwrap(),
        unversioned()
    );
    assert_eq!(store.load().unwrap(), config);
}

#[test]
fn migrate_upgrades_an_unversioned_table() {
    let table: toml::Table = toml::from_str(&unversioned()).unwrap();
    let config = migrate(table).unwrap();
    assert_eq!(config.version, CONFIG_VERSION);
    assert_eq!(config.environments[0].id, PROD_ID);

    let explicit_zero: toml::Table =
        toml::from_str(&format!("version = 0\n{}", unversioned())).unwrap();
    assert_eq!(migrate(explicit_zero).unwrap(), config);
}

#[test]
fn newer_versions_are_rejected_and_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let newer = CONFIG_VERSION + 1;
    let text = format!("version = {newer}\n\n[general]\ntheme = \"sepia\"\n");
    let store = store_with(dir.path(), &text);
    match store.load() {
        Err(ConfigError::UnsupportedVersion { found, supported }) => {
            assert_eq!(found, u64::from(newer));
            assert_eq!(supported, u64::from(CONFIG_VERSION));
        }
        other => panic!("expected an unsupported version, got {other:?}"),
    }
    assert_eq!(fs::read_to_string(store.path()).unwrap(), text);
    assert!(!store.backup_path().exists());

    let table: toml::Table = toml::from_str(&text).unwrap();
    assert!(matches!(
        migrate(table),
        Err(ConfigError::UnsupportedVersion { .. })
    ));
}

#[test]
fn unusable_versions_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    for text in ["version = \"1\"", "version = -1", "version = 1.5"] {
        assert!(
            matches!(
                store_with(dir.path(), text).load(),
                Err(ConfigError::InvalidVersion(_))
            ),
            "{text}"
        );
    }
}

#[test]
fn corrupt_files_are_reported_with_a_line_and_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let text = "version = 1\n[general]\ntheme = = \"dark\"\n";
    let store = store_with(dir.path(), text);
    let message = parse_message(store.load());
    assert!(message.contains("line 3"), "{message}");
    assert_eq!(fs::read_to_string(store.path()).unwrap(), text);
    assert!(!store.backup_path().exists());
}

#[test]
fn values_of_the_wrong_kind_are_reported_with_a_line() {
    let dir = tempfile::tempdir().unwrap();
    let cases = [
        (
            "version = 1\n[general]\n\ntheme = \"sepia\"\n",
            "line 4",
            "unknown variant `sepia`",
        ),
        (
            "version = 1\n[general]\nclose_to_tray = \"yes\"\n",
            "line 3",
            "expected a boolean",
        ),
        (
            "version = 1\n[[environments]]\nname = \"prod\"\nauth = { kind = \"basic\" }\n",
            "line 4",
            "missing field `username`",
        ),
        (
            "version = 1\n[[environments]]\n[environments.notifications.quiet_hours]\ndays = [true, false]\n",
            "line 4",
            "invalid length 2",
        ),
    ];
    for (text, line, problem) in cases {
        let message = parse_message(store_with(dir.path(), text).load());
        assert!(message.contains(line), "{message}");
        assert!(message.contains(problem), "{message}");
    }
    // Unversioned files are read from the text too.
    let message = parse_message(store_with(dir.path(), "[general]\ntheme = 3\n").load());
    assert!(message.contains("line 2"), "{message}");
}

#[test]
fn byte_order_marks_are_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let config = store_with(
        dir.path(),
        "\u{feff}version = 1\n[general]\ntheme = \"light\"\n",
    )
    .load()
    .unwrap();
    assert_eq!(config.general.theme, ThemeChoice::Light);
}

#[test]
fn files_that_are_not_text_are_reported() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(&path, b"version = 1\n\x00\xff\xfe").unwrap();
    let message = parse_message(ConfigStore::new(path).load());
    assert!(message.contains("not UTF-8"), "{message}");
}

#[test]
fn a_directory_in_place_of_the_file_is_an_io_error() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("config.toml")).unwrap();
    let store = ConfigStore::new(dir.path().join("config.toml"));
    assert!(matches!(store.load(), Err(ConfigError::Io { .. })));
    assert!(matches!(
        store.save(&Config::default()),
        Err(ConfigError::Io { .. })
    ));
}

#[test]
fn hand_written_files_get_ids_that_stick() {
    let dir = tempfile::tempdir().unwrap();
    let text = r#"
[[environments]]
name = "prod-cluster"
url = "https://master-01.example.com:5665"
auth = { kind = "basic", username = "icygui" }

[[environments.groups]]
name = "databases"

[[environments.groups.dashboards]]
name = "replication"

[[environments.groups.dashboards]]
name = "hosts"
"#;
    let store = store_with(dir.path(), text);
    let first = store.load().unwrap();
    let environment = &first.environments[0];
    assert!(!environment.id.is_empty());
    assert!(ids(environment).iter().all(|id| !id.is_empty()));
    assert!(first.validate().is_empty(), "{:?}", first.validate());

    // The ids were saved: loading again gives the same ones, and the
    // original file is the backup.
    assert_eq!(store.load().unwrap(), first);
    assert_eq!(fs::read_to_string(store.backup_path()).unwrap(), text);
}

#[test]
fn duplicate_ids_are_repaired_on_load() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = full_config();
    let duplicate = config.environments[0].id.clone();
    config.environments[1].id.clone_from(&duplicate);
    let store = ConfigStore::new(dir.path().join("config.toml"));
    store.save(&config).unwrap();

    let loaded = store.load().unwrap();
    assert_eq!(
        loaded.environments[0].id, duplicate,
        "the first keeps its id"
    );
    assert_ne!(loaded.environments[1].id, duplicate);
    assert!(loaded.validate().is_empty(), "{:?}", loaded.validate());
    assert_eq!(store.load().unwrap(), loaded);
}

#[test]
fn the_backup_restores_settings_after_corruption() {
    let dir = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(dir.path().join("config.toml"));
    assert_eq!(store.load_backup().unwrap(), None);

    let older = full_config();
    let mut newer = older.clone();
    newer.general.theme = ThemeChoice::Light;
    store.save(&older).unwrap();
    store.save(&newer).unwrap();
    fs::write(store.path(), "version = 1\n[general\n").unwrap();
    assert!(store.load().is_err());

    // Restore: read the backup and save it. The corrupt file becomes the
    // backup, so nothing is lost.
    let restored = store.load_backup().unwrap().unwrap();
    assert_eq!(restored, older);
    store.save(&restored).unwrap();
    assert_eq!(store.load().unwrap(), older);
    assert_eq!(
        fs::read_to_string(store.backup_path()).unwrap(),
        "version = 1\n[general\n"
    );
}

#[test]
fn starting_fresh_keeps_the_corrupt_file_as_the_backup() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_with(dir.path(), "version = [\n");
    assert!(store.load().is_err());
    store.save(&Config::default()).unwrap();
    assert_eq!(store.load().unwrap(), Config::default());
    assert_eq!(
        fs::read_to_string(store.backup_path()).unwrap(),
        "version = [\n"
    );
}

#[cfg(unix)]
#[test]
fn saved_files_are_private() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(dir.path().join("icygui/config.toml"));
    store.save(&full_config()).unwrap();
    store.save(&Config::default()).unwrap();
    let mode = |path: &std::path::Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(store.path()), 0o600);
    assert_eq!(mode(&store.backup_path()), 0o600);
    assert_eq!(mode(&dir.path().join("icygui")), 0o700);
}
