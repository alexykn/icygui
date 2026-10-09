//! Loading real-world files: missing, partial, hand-written, old, newer,
//! corrupt; restoring the backup.

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use ic_config::{
    ApiUrl, AuthConfig, CONFIG_VERSION, Config, ConfigError, ConfigStore, General, ThemeChoice,
    TlsConfig, format_fingerprint, migrate,
};
use ic_rules::{NotificationSettings, ScopeSetting};

use crate::fixtures::{full_config, ids, store_with};
use crate::logs::{capture, warnings};

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
    assert_eq!(config.appearance.theme, ThemeChoice::Light);
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
    assert!(!environment.groups[0].dashboards[0].views[0].problems_only);
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
    assert!(
        config.general.quiet_when_hidden,
        "quiet mode is on unless turned off (files from before it had no key)"
    );
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
    assert_eq!(config.appearance.theme, ThemeChoice::Light);
    let dashboard = config.environments[0]
        .dashboard(GROUP_ID, DASHBOARD_ID)
        .unwrap();
    assert_eq!(dashboard.views[0].filter, r#"host.vars.role == "postgres""#);
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
fn version_1_files_get_url_lists_with_their_pins() {
    let dir = tempfile::tempdir().unwrap();
    let text = format!(
        r#"version = 1
active_environment = "{PROD_ID}"

[[environments]]
id = "{PROD_ID}"
name = "prod-cluster"
url = "https://10.0.0.5:5665"

[environments.auth]
kind = "basic"
username = "icygui"

[environments.tls]
ca_file = "/etc/icinga2/ca.crt"
pinned_sha256 = "{pin}"
server_name = "master-01.example.com"
use_system_roots = false
"#,
        pin = format_fingerprint(&[0xab; 32]),
    );
    let store = store_with(dir.path(), &text);
    let config = store.load().unwrap();
    let environment = &config.environments[0];
    assert_eq!(environment.id, PROD_ID, "the id, and so the password, stay");
    assert_eq!(
        environment.urls,
        [ApiUrl {
            url: "https://10.0.0.5:5665".to_owned(),
            pinned_sha256: Some(format_fingerprint(&[0xab; 32])),
            server_name: Some("master-01.example.com".to_owned()),
        }]
    );
    assert_eq!(
        environment.tls,
        TlsConfig {
            ca_file: Some("/etc/icinga2/ca.crt".into()),
            use_system_roots: false,
        }
    );
    assert!(config.validate().is_empty(), "{:?}", config.validate());

    // The next save writes the new layout and keeps the old file as the
    // backup; the result reads back the same.
    store.save(&config).unwrap();
    let saved = fs::read_to_string(store.path()).unwrap();
    assert!(
        saved.contains(&format!("\nversion = {CONFIG_VERSION}\n")),
        "{saved}"
    );
    assert!(saved.contains("[[environments.urls]]"), "{saved}");
    assert!(!saved.contains("\nurl = \"https://10.0.0.5:5665\"\n[environments.tls]"));
    assert_eq!(fs::read_to_string(store.backup_path()).unwrap(), text);
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
            "version = 3\n[appearance]\n\ntheme = \"sepia\"\n",
            "line 4",
            "unknown variant `sepia`",
        ),
        (
            "version = 1\n[general]\n\nlog_level = \"loud\"\n",
            "line 4",
            "unknown variant `loud`",
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
    let message = parse_message(store_with(dir.path(), "[appearance]\ntheme = 3\n").load());
    assert!(message.contains("line 2"), "{message}");
    // A theme from before version 3 moved, so its error names where it went.
    let message = parse_message(store_with(dir.path(), "[general]\ntheme = 3\n").load());
    assert!(message.contains("appearance.theme"), "{message}");
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
    assert_eq!(config.appearance.theme, ThemeChoice::Light);
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

/// A hand-written file without ids, as a configuration management tool
/// would write it.
const WITHOUT_IDS: &str = r#"
[[environments]]
name = "prod-cluster"
url = "https://master-01.example.com:5665"
auth = { kind = "basic", username = "icygui" }

[[environments.groups]]
name = "databases"

[[environments.groups.dashboards]]
name = "replication"

[[environments]]
name = "staging"
url = "https://staging.example.com:5665"
auth = { kind = "basic", username = "icygui" }
"#;

#[test]
fn derived_ids_survive_the_move_to_url_lists() {
    // Keychain accounts are environment ids: a hand-written file keeps its
    // derived ids when it is rewritten in the current format, with the old
    // `url` as the first of its `urls` and more URLs after it.
    let dir = tempfile::tempdir().unwrap();
    let old = ConfigStore::new(dir.path().join("old.toml"));
    fs::write(old.path(), WITHOUT_IDS).unwrap();
    let new = ConfigStore::new(dir.path().join("new.toml"));
    let rewritten = WITHOUT_IDS
        .replace(
            "url = \"https://master-01.example.com:5665\"",
            "urls = [\"https://master-01.example.com:5665\", \"https://master-02.example.com:5665\"]",
        )
        .replace(
            "url = \"https://staging.example.com:5665\"",
            "urls = [{ url = \"https://staging.example.com:5665\" }]",
        );
    fs::write(
        new.path(),
        format!("version = {CONFIG_VERSION}\n{rewritten}"),
    )
    .unwrap();
    let (old, new) = (old.load().unwrap(), new.load().unwrap());
    let ids = |config: &Config| -> Vec<String> {
        config
            .environments
            .iter()
            .flat_map(|environment| std::iter::once(environment.id.clone()).chain(ids(environment)))
            .collect()
    };
    assert_eq!(ids(&old), ids(&new));
    assert_eq!(new.environments[0].urls.len(), 2);
}

/// The names of the files in `dir`, sorted.
fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn repaired_ids_stay_the_same_when_the_file_cannot_be_saved() {
    // A file managed elsewhere (a read-only link into the Nix store, say):
    // a directory where the backup goes makes every save fail.
    let dir = tempfile::tempdir().unwrap();
    let store = store_with(dir.path(), WITHOUT_IDS);
    fs::create_dir(store.backup_path()).unwrap();

    let (first, events) = capture(|| store.load().unwrap());
    assert!(first.validate().is_empty(), "{:?}", first.validate());
    assert!(
        warnings(&events)
            .iter()
            .any(|warning| warning.starts_with("could not save the repaired ids")),
        "{events:?}"
    );
    assert_eq!(fs::read_to_string(store.path()).unwrap(), WITHOUT_IDS);
    // The ids are the keychain accounts: every start must find the same.
    for _ in 0..3 {
        assert_eq!(store.load().unwrap(), first);
    }
    assert_eq!(fs::read_to_string(store.path()).unwrap(), WITHOUT_IDS);
}

#[test]
fn rewritten_files_get_the_same_ids_again() {
    // Ansible or chezmoi writes the same id-less template again after
    // icygui saved its ids into the file.
    let dir = tempfile::tempdir().unwrap();
    let store = store_with(dir.path(), WITHOUT_IDS);
    let first = store.load().unwrap();
    assert_ne!(fs::read_to_string(store.path()).unwrap(), WITHOUT_IDS);
    fs::write(store.path(), WITHOUT_IDS).unwrap();
    assert_eq!(store.load().unwrap(), first);
}

#[test]
fn restoring_an_id_less_backup_keeps_the_ids() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_with(dir.path(), WITHOUT_IDS);
    let first = store.load().unwrap();
    // The repair saved the ids; the original is the backup.
    assert_eq!(
        fs::read_to_string(store.backup_path()).unwrap(),
        WITHOUT_IDS
    );
    fs::write(store.path(), "version = 1\n[general\n").unwrap();
    assert!(store.load().is_err());

    let restored = store.load_backup().unwrap().unwrap();
    assert_eq!(restored, first);
    store.save(&restored).unwrap();
    assert_eq!(store.load().unwrap(), first);
}

#[test]
fn backups_are_read_but_never_written() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_with(dir.path(), "version = 1\n");
    let check = |backup: &str| -> Result<Option<Config>, ConfigError> {
        fs::write(store.backup_path(), backup).unwrap();
        let result = store.load_backup();
        assert_eq!(fs::read_to_string(store.backup_path()).unwrap(), backup);
        assert_eq!(fs::read_to_string(store.path()).unwrap(), "version = 1\n");
        assert_eq!(entries(dir.path()), ["config.toml", "config.toml.bak"]);
        result
    };

    let message = parse_message(check("version = 1\n[general\n").map(Option::unwrap_or_default));
    assert!(message.contains("line 2"), "{message}");
    assert!(matches!(
        check(&format!("version = {}\n", CONFIG_VERSION + 1)),
        Err(ConfigError::UnsupportedVersion { .. })
    ));
    // Blank ids are repaired in memory only, to the ids `load` would give.
    let repaired = check(WITHOUT_IDS).unwrap().unwrap();
    assert!(repaired.validate().is_empty(), "{:?}", repaired.validate());
    let elsewhere = tempfile::tempdir().unwrap();
    let loaded = store_with(elsewhere.path(), WITHOUT_IDS).load();
    assert_eq!(loaded.unwrap(), repaired);
}

#[test]
fn files_this_version_cannot_read_survive_starting_fresh() {
    let newer = format!(
        "version = {}\n\n[[environments]]\nname = \"prod\"\n",
        CONFIG_VERSION + 1
    );
    for text in [
        newer.as_str(),
        "version = 1\n[general]\ntheme = = \"dark\"\n",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with(dir.path(), text);
        assert!(store.load().is_err());

        // Start fresh, then keep working: every later save rotates the
        // backup, but the unreadable file is kept apart.
        let mut config = Config::default();
        store.save(&config).unwrap();
        for theme in [ThemeChoice::Light, ThemeChoice::System, ThemeChoice::Dark] {
            config.appearance.theme = theme;
            store.save(&config).unwrap();
        }
        let kept: Vec<String> = entries(dir.path())
            .into_iter()
            .filter(|name| name.starts_with("config.toml.unreadable-"))
            .collect();
        assert_eq!(kept.len(), 1, "{:?}", entries(dir.path()));
        assert_eq!(fs::read_to_string(dir.path().join(&kept[0])).unwrap(), text);
        assert_eq!(store.load().unwrap(), config);
    }
}

#[test]
fn starting_fresh_keeps_the_backup_the_recovery_screen_offered() {
    let dir = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(dir.path().join("config.toml"));
    let good = full_config();
    store.save(&good).unwrap();
    store.save(&Config::default()).unwrap();
    assert_eq!(store.load_backup().unwrap(), Some(good.clone()));
    // A bad hand edit: the file no longer parses.
    fs::write(store.path(), "version = 1\n[general\n").unwrap();
    assert!(store.load().is_err());

    // "start fresh" saves the defaults over it.
    store.save(&Config::default()).unwrap();
    assert_eq!(store.load().unwrap(), Config::default());
    assert_eq!(
        store.load_backup().unwrap(),
        Some(good),
        "the last good settings are still there to restore"
    );
    assert!(
        entries(dir.path())
            .iter()
            .any(|name| name.starts_with("config.toml.unreadable-")),
        "{:?}",
        entries(dir.path())
    );
}

#[test]
fn readable_files_are_not_kept_apart() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_with(dir.path(), WITHOUT_IDS);
    let mut config = store.load().unwrap();
    config.appearance.theme = ThemeChoice::Light;
    store.save(&config).unwrap();
    assert_eq!(entries(dir.path()), ["config.toml", "config.toml.bak"]);
}

#[test]
fn saves_and_loads_at_the_same_time_never_see_a_partial_file() {
    let dir = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(dir.path().join("config.toml"));
    let first = full_config();
    let mut second = first.clone();
    second.appearance.theme = ThemeChoice::Light;
    second.environments.truncate(1);
    store.save(&first).unwrap();

    let done = AtomicBool::new(false);
    thread::scope(|scope| {
        let writers: Vec<_> = [&first, &second]
            .into_iter()
            .map(|config| {
                let store = store.clone();
                let other = if config == &first { &second } else { &first };
                scope.spawn(move || {
                    for _ in 0..40 {
                        store.save(config).unwrap();
                        store.save(other).unwrap();
                    }
                })
            })
            .collect();
        let reader = scope.spawn(|| {
            let mut loads = 0;
            while !done.load(Ordering::Acquire) || loads == 0 {
                let loaded = store.load().unwrap();
                assert!(loaded == first || loaded == second);
                loads += 1;
            }
            loads
        });
        for writer in writers {
            writer.join().unwrap();
        }
        done.store(true, Ordering::Release);
        assert!(reader.join().unwrap() > 0);
    });
    assert_eq!(entries(dir.path()), ["config.toml", "config.toml.bak"]);
}

#[cfg(unix)]
#[test]
fn settings_behind_a_broken_link_are_an_error_not_defaults() {
    use std::os::unix::fs::symlink;

    // The dotfiles repository moved, or its volume isn't mounted yet.
    let dir = tempfile::tempdir().unwrap();
    let link = dir.path().join("config.toml");
    let target = dir.path().join("dotfiles/icygui.toml");
    symlink(&target, &link).unwrap();
    let store = ConfigStore::new(link.clone());
    match store.load() {
        Err(ConfigError::Io { path, .. }) => assert_eq!(path, link),
        other => panic!("expected an I/O error, got {other:?}"),
    }
    assert!(store.save(&Config::default()).is_err());
    assert!(fs::symlink_metadata(&link).unwrap().is_symlink());
    assert!(!dir.path().join("dotfiles").exists());

    // Once the directory is back, starting fresh writes through the link.
    fs::create_dir(dir.path().join("dotfiles")).unwrap();
    assert!(store.load().is_err(), "the file itself is still missing");
    let config = full_config();
    store.save(&config).unwrap();
    assert!(fs::symlink_metadata(&link).unwrap().is_symlink());
    assert!(target.is_file());
    assert_eq!(store.load().unwrap(), config);
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
    newer.appearance.theme = ThemeChoice::Light;
    store.save(&older).unwrap();
    store.save(&newer).unwrap();
    fs::write(store.path(), "version = 1\n[general\n").unwrap();
    assert!(store.load().is_err());

    // Restore: read the backup and save it. The corrupt file is kept
    // apart, so nothing is lost, and the backup stays the good one.
    let restored = store.load_backup().unwrap().unwrap();
    assert_eq!(restored, older);
    store.save(&restored).unwrap();
    assert_eq!(store.load().unwrap(), older);
    assert_eq!(store.load_backup().unwrap(), Some(older));
    assert_eq!(kept_unreadable(dir.path()), ["version = 1\n[general\n"]);
}

#[test]
fn starting_fresh_keeps_the_corrupt_file_apart() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_with(dir.path(), "version = [\n");
    assert!(store.load().is_err());
    store.save(&Config::default()).unwrap();
    assert_eq!(store.load().unwrap(), Config::default());
    assert_eq!(kept_unreadable(dir.path()), ["version = [\n"]);
    assert!(
        !store.backup_path().exists(),
        "an unreadable file never becomes the backup"
    );
}

/// The contents of the unreadable files kept next to the settings.
fn kept_unreadable(dir: &Path) -> Vec<String> {
    entries(dir)
        .into_iter()
        .filter(|name| name.starts_with("config.toml.unreadable-"))
        .map(|name| fs::read_to_string(dir.join(name)).unwrap())
        .collect()
}

#[cfg(unix)]
#[test]
fn saved_files_are_private() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(dir.path().join("icygui/config.toml"));
    store.save(&full_config()).unwrap();
    store.save(&Config::default()).unwrap();
    let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(store.path()), 0o600);
    assert_eq!(mode(&store.backup_path()), 0o600);
    assert_eq!(mode(&dir.path().join("icygui")), 0o700);
}

/// Files from before topic 16 (format version 4 without `trouble` or
/// `health_page`) load with the trouble alerts' defaults and the approved
/// layout of the cluster health page, and a save leaves both out while
/// they are unchanged, so a later version's defaults still apply.
#[test]
fn environments_from_before_the_health_page_get_its_default_layout() {
    let dir = tempfile::tempdir().unwrap();
    let file = format!(
        r#"
version = 4
active_environment = "{PROD_ID}"

[[environments]]
id = "{PROD_ID}"
name = "prod-cluster"
urls = ["https://master-01.example.com:5665"]
auth = {{ kind = "basic", username = "icygui" }}
"#
    );
    let store = store_with(dir.path(), &file);
    let config = store.load().unwrap();
    let environment = &config.environments[0];
    assert_eq!(environment.health_page, ic_config::HealthPage::default());
    let kinds: Vec<ic_config::ViewDisplay> = environment
        .health_page
        .views
        .iter()
        .map(|view| view.display)
        .collect();
    assert_eq!(kinds, ic_config::ViewDisplay::HEALTH);
    assert_eq!(environment.trouble, ic_config::Trouble::default());
    assert_eq!(
        environment.trouble.heartbeats.variable_name(),
        "icygui_heartbeat"
    );
    assert!(config.validate().is_empty(), "{:?}", config.validate());
    store.save(&config).unwrap();
    let saved = fs::read_to_string(store.path()).unwrap();
    assert!(!saved.contains("health_page"), "{saved}");
    assert!(!saved.contains("trouble"), "{saved}");
    assert_eq!(store.load().unwrap(), config);
}

/// An edited health page and trouble settings are written and read back;
/// a hand-edited page keeps only the health kinds, once each, and a
/// sidebar dashboard loses a health view written into it by hand.
#[test]
fn edited_health_pages_round_trip_and_hand_edits_are_repaired() {
    let dir = tempfile::tempdir().unwrap();
    let file = format!(
        r#"
version = 4

[[environments]]
id = "{PROD_ID}"
name = "prod-cluster"
urls = ["https://master-01.example.com:5665"]
auth = {{ kind = "basic", username = "icygui" }}

[environments.trouble]
policy = "persistent"

[environments.trouble.heartbeats]
mode = "list"
list = ["icygui-hb-master-01!beat"]

[[environments.health_page.views]]
display = "global_switches"

[[environments.health_page.views]]
display = "checks"
health = {{ hidden_tiles = ["pending"], sparklines = false }}

[[environments.health_page.views]]
display = "list"

[[environments.health_page.views]]
display = "checks"

[[environments.groups]]
id = "{GROUP_ID}"
name = "databases"

[[environments.groups.dashboards]]
id = "{DASHBOARD_ID}"
name = "replication"

[[environments.groups.dashboards.views]]
display = "zones_and_endpoints"
"#
    );
    let store = store_with(dir.path(), &file);
    let config = store.load().unwrap();
    let environment = &config.environments[0];
    assert_eq!(environment.trouble.policy, ic_config::TroublePolicy::Persistent);
    assert_eq!(
        environment.trouble.heartbeats.mode,
        ic_config::HeartbeatMode::List
    );
    let page = &environment.health_page;
    let kinds: Vec<(&str, ic_config::ViewDisplay)> = page
        .views
        .iter()
        .map(|view| (view.id.as_str(), view.display))
        .collect();
    assert_eq!(
        kinds,
        [
            ("switches", ic_config::ViewDisplay::GlobalSwitches),
            ("checks", ic_config::ViewDisplay::Checks)
        ]
    );
    assert!(!page.views[1].health.sparklines);
    assert!(!page.views[1].health.shows(ic_config::HealthTile::Pending));
    let dashboard = environment.dashboard(GROUP_ID, DASHBOARD_ID).unwrap();
    assert_eq!(dashboard.views.len(), 1);
    assert_eq!(dashboard.views[0].display, ic_config::ViewDisplay::List);
    assert!(config.validate().is_empty(), "{:?}", config.validate());
    store.save(&config).unwrap();
    assert_eq!(store.load().unwrap(), config);
}
