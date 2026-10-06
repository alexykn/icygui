//! Passwords never end up in settings files, error messages or the log.

use std::fs;

use ic_config::{
    ApiUrl, AuthConfig, Config, ConfigError, ConfigStore, Environment, ValidationIssue,
};

use crate::fixtures::{full_config, store_with};
use crate::logs::{capture, warnings};

const PASSWORD: &str = "icinga-secret";

fn basic(username: &str) -> AuthConfig {
    AuthConfig::Basic {
        username: username.to_owned(),
    }
}

/// The issues of a refused save.
fn refused(result: Result<(), ConfigError>) -> Vec<ValidationIssue> {
    match result {
        Err(ConfigError::Invalid(issues)) => issues,
        other => panic!("expected the save to be refused, got {other:?}"),
    }
}

#[test]
fn urls_with_credentials_are_never_saved() {
    for url in [
        format!("https://root:{PASSWORD}@master-01:5665"),
        format!("http://root:{PASSWORD}@master-01:5665"),
        format!("root:{PASSWORD}@master-01:5665"),
        format!("https://root:{PASSWORD}@master 01:5665"),
        format!("https://{PASSWORD}@master-01:5665"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let store = ConfigStore::new(dir.path().join("config.toml"));
        let mut config = full_config();
        config.environments[1].urls[0].url.clone_from(&url);

        let error = store.save(&config).unwrap_err();
        let message = error.to_string();
        assert_eq!(
            message,
            "invalid content: environments[1].urls[0].url: must not contain a user name or password; \
             set them in the authentication settings",
            "{url}"
        );
        assert!(!format!("{error:?}").contains(PASSWORD));
        assert!(!store.path().exists(), "nothing is written");
        // Validation reports the same issue, so the settings UI can show it.
        assert_eq!(config.validate(), refused(Err(error)));
    }
}

#[test]
fn usernames_with_a_password_are_never_saved() {
    let dir = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(dir.path().join("config.toml"));
    let good = full_config();
    store.save(&good).unwrap();
    let before = fs::read(store.path()).unwrap();

    // curl's `-u user:password` form, which Icinga splits at the `:`.
    let mut bad = good.clone();
    bad.environments[0].auth = basic(&format!("icygui:{PASSWORD}"));
    let issues = refused(store.save(&bad));
    assert_eq!(
        issues,
        [ValidationIssue {
            path: "environments[0].auth.username".to_owned(),
            message: "must not contain `:`; enter the password separately, icygui keeps it in \
                      the system keychain"
                .to_owned(),
        }]
    );
    assert_eq!(bad.validate(), issues);
    assert_eq!(
        fs::read(store.path()).unwrap(),
        before,
        "the file is untouched"
    );
    assert!(!store.backup_path().exists());
}

#[test]
fn hand_written_passwords_are_not_copied_around() {
    // No ids, so loading would normally save the repaired file at once and
    // keep the original as the backup. With a password in it, nothing is
    // written: the password stays in the one file the user put it in.
    let dir = tempfile::tempdir().unwrap();
    let text = format!(
        r#"
[[environments]]
name = "prod"
url = "https://root:{PASSWORD}@master-01:5665"
auth = {{ kind = "basic", username = "root" }}
"#
    );
    let store = store_with(dir.path(), &text);
    let (first, events) = capture(|| store.load().unwrap());
    assert_eq!(fs::read_to_string(store.path()).unwrap(), text);
    assert!(!store.backup_path().exists());
    assert!(
        warnings(&events)
            .iter()
            .any(|warning| warning.starts_with("could not save the repaired ids")),
        "{events:?}"
    );
    for event in &events {
        assert!(!event.text.contains(PASSWORD), "{event:?}");
    }
    assert_eq!(
        first.validate()[0].to_string(),
        "environments[0].urls[0].url: must not contain a user name or password; set them in \
         the authentication settings"
    );
    // The ids are derived from the content, so they are the same next time.
    assert_eq!(store.load().unwrap(), first);
}

#[test]
fn passwords_in_unknown_keys_are_called_out_but_never_logged() {
    let dir = tempfile::tempdir().unwrap();
    let text = format!(
        r#"
version = 1

[[environments]]
id = "6f1c2a8e-0b7d-4c1e-9a3f-2d5b8c7e1f40"
name = "prod"
url = "https://master-01:5665"

[environments.auth]
kind = "basic"
username = "root"
password = "{PASSWORD}"

[environments.tls]
api_token = "{PASSWORD}"
min_version = "1.3"
"#
    );
    let (config, events) = capture(|| store_with(dir.path(), &text).load().unwrap());
    assert_eq!(config.environments[0].auth, basic("root"));
    let warnings = warnings(&events);
    let about = |key: &str| {
        warnings
            .iter()
            .find(|warning| warning.ends_with(&format!("key={key}")))
            .copied()
            .unwrap_or_else(|| panic!("no warning about {key}: {warnings:?}"))
    };
    for key in [
        "environments.0.auth.password",
        "environments.0.tls.api_token",
    ] {
        assert!(
            about(key).starts_with("ignoring a password-like key in the settings"),
            "{warnings:?}"
        );
    }
    assert!(
        about("environments.0.tls.min_version").starts_with("ignoring unknown key in the settings"),
        "{warnings:?}"
    );
    assert_eq!(warnings.len(), 3, "{warnings:?}");
    for event in &events {
        assert!(!event.text.contains(PASSWORD), "{event:?}");
    }
}

#[test]
fn errors_never_show_the_password() {
    let mut environment = Environment::new(
        "prod",
        &format!("https://icygui:{PASSWORD}@master-01:5665/?token={PASSWORD}#{PASSWORD}"),
        basic("icygui"),
    );
    let error = environment.urls[0].api_url().unwrap_err();
    assert_eq!(
        error.to_string(),
        "invalid API URL `https://***@master-01:5665/?…`: must not contain a user name or \
         password; set them in the authentication settings"
    );
    assert!(!format!("{error:?}").contains(PASSWORD));

    environment.urls[0].url = format!("https://master-01:5665/v1?password={PASSWORD}");
    let error = environment.urls[0].api_url().unwrap_err();
    assert!(!error.to_string().contains(PASSWORD), "{error}");
    assert!(!format!("{error:?}").contains(PASSWORD), "{error:?}");

    let mut config = Config::default();
    environment.urls[0].url = format!("https://root:{PASSWORD}@master-01:5665");
    environment
        .urls
        .push(ApiUrl::new(&format!("https://{PASSWORD}@master-02:5665")));
    config.environments.push(environment);
    for issue in config.validate() {
        assert!(!issue.to_string().contains(PASSWORD), "{issue}");
    }
}
