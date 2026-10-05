//! Settings survive being written and read back, field for field.

use std::fs;

use ic_config::{CONFIG_VERSION, Config, ConfigStore, Dashboard, View, migrate};
use ic_model::{ObjectKey, Timestamp};
use ic_rules::{ObjectMode, ObjectOverride, Rule, ScopeSetting};

use crate::fixtures::{THEMES, full_config};

/// Saves `config` in a fresh directory and loads it back.
fn save_and_load(config: &Config) -> Config {
    let dir = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(dir.path().join("config.toml"));
    store.save(config).unwrap();
    store.load().unwrap()
}

#[test]
fn awkward_strings_survive() {
    let awkward = [
        "",
        " leading and trailing ",
        r#"quotes " and 'single' and \ backslash"#,
        r#"triple """ and ''' quotes"#,
        "trailing quote\"",
        "multi\nline\r\nwindows\ttab",
        "trailing newline\n",
        "control \u{7} \u{1b}[31m \u{7f}",
        "unicode ✓ ümlaut 漢字 🚨",
        "{{{ icinga multi-line string }}}",
        "# not a comment",
        "[not.a.table]",
        "key = \"value\"",
    ];
    let mut config = full_config();
    let environment = &mut config.environments[0];
    environment.author = Some(awkward[5].to_owned());
    environment.notifications.objects.push(ObjectOverride {
        object: ObjectKey::service(awkward[7], awkward[8]),
        mode: ObjectMode::Mute,
        until: None,
    });
    let group = &mut environment.groups[1];
    for text in awkward {
        group.dashboards.push(Dashboard::new(
            "x",
            View {
                filter: text.to_owned(),
                ..View::default()
            },
        ));
        let mut dashboard = Dashboard::new("x", View::default());
        dashboard.name = text.to_owned();
        group.dashboards.push(dashboard);
    }
    assert_eq!(save_and_load(&config), config);
}

#[test]
fn extreme_numbers_survive() {
    let mut config = full_config();
    config.general.event_log_retention_hours = u32::MAX;
    config.general.reconcile_interval_secs = u32::MAX;
    let notifications = &mut config.environments[0].notifications;
    notifications.default_rule.min_duration_secs = u32::MAX;
    notifications.storm.threshold = u32::MAX;
    notifications.storm.window_secs = 0;
    notifications.quiet_hours.start_minute = u16::MAX;
    notifications.quiet_hours.end_minute = 0;
    for seconds in [0.0, -1.5, 0.1, 4_102_444_800.123_456, 1e15] {
        notifications.objects.push(ObjectOverride {
            object: ObjectKey::host(&format!("host-{seconds}")),
            mode: ObjectMode::Watch,
            until: Some(Timestamp::from_unix_seconds(seconds)),
        });
    }
    config.environments[0].groups[0].notifications = ScopeSetting::Custom(Rule {
        min_duration_secs: u32::MAX,
        ..Rule::default()
    });
    assert_eq!(save_and_load(&config), config);
}

#[test]
fn full_config_survives_save_and_load() {
    for theme in THEMES {
        let dir = tempfile::tempdir().unwrap();
        let store = ConfigStore::new(dir.path().join("config.toml"));
        let mut config = full_config();
        config.general.theme = theme;
        store.save(&config).unwrap();
        assert_eq!(store.load().unwrap(), config, "theme {theme:?}");
    }
}

#[test]
fn defaults_survive_save_and_load() {
    let dir = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(dir.path().join("config.toml"));
    store.save(&Config::default()).unwrap();
    assert_eq!(store.load().unwrap(), Config::default());
}

#[test]
fn full_config_survives_plain_toml() {
    let config = full_config();
    let text = toml::to_string(&config).unwrap();
    assert_eq!(toml::from_str::<Config>(&text).unwrap(), config);
    assert_eq!(migrate(toml::from_str(&text).unwrap()).unwrap(), config);
}

#[test]
fn reading_the_text_and_migrating_the_table_agree() {
    let dir = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(dir.path().join("config.toml"));
    store.save(&full_config()).unwrap();
    let text = fs::read_to_string(store.path()).unwrap();
    let table: toml::Table = toml::from_str(&text).unwrap();
    assert_eq!(migrate(table).unwrap(), store.load().unwrap());
}

#[test]
fn the_file_is_readable_toml() {
    let dir = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(dir.path().join("config.toml"));
    store.save(&full_config()).unwrap();
    let text = fs::read_to_string(store.path()).unwrap();
    for expected in [
        "# icygui settings. Passwords are kept in the system keychain, not in this file.\n",
        &format!("\nversion = {CONFIG_VERSION}\n"),
        "\n[general]\ntheme = \"system\"\n",
        "\n[[environments]]\n",
        "\n[environments.auth]\nkind = \"basic\"\nusername = \"icygui\"\n",
        "kind = \"client_certificate\"\n",
        "\n[[environments.groups.dashboards]]\n",
        "mode = \"custom\"\n",
        "object_kind = \"hosts\"\n",
        "key = \"last_state_change\"\n",
        "group_by = \"service_group\"\n",
        "filter = \"\"\"\nservice.vars.team == \"dba\"\n  || \"databases\" in service.groups\"\"\"\n",
        "days = [true, true, true, true, true, false, false]\n",
        "\n[environments.notifications.objects.object]\ntype = \"service\"\n",
    ] {
        assert!(text.contains(expected), "missing {expected:?} in:\n{text}");
    }
}
