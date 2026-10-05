//! Where files go, end to end.

use std::fs;

use ic_config::{Config, Paths};

use crate::fixtures::full_config;

#[test]
fn in_dir_puts_everything_under_one_root() {
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::in_dir(root.path());
    assert_eq!(paths.config_file, root.path().join("config.toml"));
    assert_eq!(paths.data_dir, root.path().join("data"));
    assert_eq!(paths.log_dir, root.path().join("logs"));

    paths.create_dirs().unwrap();
    let store = paths.config_store();
    assert_eq!(store.path(), paths.config_file);
    let config = full_config();
    store.save(&Config::default()).unwrap();
    store.save(&config).unwrap();
    assert_eq!(store.load().unwrap(), config);
    assert_eq!(store.load_backup().unwrap(), Some(Config::default()));

    let mut entries: Vec<String> = fs::read_dir(root.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    entries.sort();
    assert_eq!(entries, ["config.toml", "config.toml.bak", "data", "logs"]);
}

#[test]
fn saving_creates_a_missing_settings_directory() {
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::in_dir(&root.path().join("portable/icygui"));
    let store = paths.config_store();
    store.save(&full_config()).unwrap();
    assert!(paths.config_file.is_file());
}
