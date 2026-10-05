//! `Config::validate` and `Environment::validate`.

use ic_config::{
    AuthConfig, Config, Dashboard, DashboardGroup, Environment, GroupBy, ObjectKind,
    ValidationIssue, View,
};
use ic_model::{ObjectKey, Timestamp};
use ic_rules::{ObjectMode, ObjectOverride};

use crate::fixtures::full_config;

/// The issues as `path: message` lines, for readable assertions.
fn issues(config: &Config) -> Vec<String> {
    config.validate().iter().map(ToString::to_string).collect()
}

/// The paths of the issues.
fn paths(config: &Config) -> Vec<String> {
    config
        .validate()
        .into_iter()
        .map(|issue| issue.path)
        .collect()
}

fn basic(username: &str) -> AuthConfig {
    AuthConfig::Basic {
        username: username.to_owned(),
    }
}

#[test]
fn the_fixture_and_defaults_are_valid() {
    assert_eq!(issues(&full_config()), Vec::<String>::new());
    assert_eq!(issues(&Config::default()), Vec::<String>::new());
    let environment = Environment::new("prod", "https://master-01:5665", basic("icygui"));
    assert_eq!(environment.validate(), []);
}

#[test]
fn duplicate_environment_ids_are_reported() {
    let mut config = full_config();
    let first = config.environments[0].id.clone();
    config.environments[1].id.clone_from(&first);
    assert_eq!(
        issues(&config),
        [format!(
            "environments[1].id: `{first}` is already used by environments[0]"
        )]
    );
}

#[test]
fn duplicate_group_and_dashboard_ids_are_reported() {
    let mut config = full_config();
    let environment = &mut config.environments[0];
    let group_id = environment.groups[0].id.clone();
    environment.groups[1].id.clone_from(&group_id);
    // Dashboard ids must be unique across the environment's groups.
    let dashboard_id = environment.groups[0].dashboards[0].id.clone();
    environment.groups[1].dashboards[2]
        .id
        .clone_from(&dashboard_id);
    assert_eq!(
        issues(&config),
        [
            format!(
                "environments[0].groups[1].id: `{group_id}` is already used by environments[0].groups[0]"
            ),
            format!(
                "environments[0].groups[1].dashboards[2].id: `{dashboard_id}` is already used by \
                 environments[0].groups[0].dashboards[0]"
            ),
        ]
    );
}

#[test]
fn blank_ids_are_reported() {
    let mut config = full_config();
    config.environments[1].id = String::new();
    config.environments[0].groups[0].id = " ".to_owned();
    config.environments[0].groups[0].dashboards[0].id = String::new();
    assert_eq!(
        paths(&config),
        [
            "environments[0].groups[0].id",
            "environments[0].groups[0].dashboards[0].id",
            "environments[1].id",
        ]
    );
}

#[test]
fn ids_may_repeat_across_environments() {
    let mut config = full_config();
    config.environments[1].groups = config.environments[0].groups.clone();
    assert_eq!(issues(&config), Vec::<String>::new());
}

#[test]
fn plain_http_urls_are_rejected() {
    let mut config = full_config();
    config.environments[0].url = "http://master-01.example.com:5665".to_owned();
    assert_eq!(
        issues(&config),
        ["environments[0].url: must use https: the Icinga 2 API only accepts TLS connections"]
    );
}

#[test]
fn urls_need_a_host() {
    for url in ["https://", "https://:5665", "https:///"] {
        let mut config = full_config();
        config.environments[0].url = url.to_owned();
        assert_eq!(
            issues(&config),
            ["environments[0].url: has no host name"],
            "{url}"
        );
    }
}

#[test]
fn other_bad_urls_are_rejected() {
    for (url, message) in [
        ("", "must not be empty"),
        (
            "master-01.example.com",
            "must be a full URL such as https://icinga.example.com:5665",
        ),
        (
            "https://master-01:5665/v1",
            "must not end in /v1: icygui adds the API paths itself",
        ),
        (
            "https://icygui:secret@master-01:5665",
            "must not contain a user name or password; set them in the authentication settings",
        ),
    ] {
        let mut config = full_config();
        config.environments[0].url = url.to_owned();
        assert_eq!(
            issues(&config),
            [format!("environments[0].url: {message}")],
            "{url}"
        );
    }
}

#[test]
fn names_must_not_be_empty() {
    let mut config = full_config();
    config.environments[0].name = String::new();
    config.environments[0].groups[1].name = "  ".to_owned();
    config.environments[1].groups[0].dashboards[2].name = "\t".to_owned();
    assert_eq!(
        issues(&config),
        [
            "environments[0].name: must not be empty",
            "environments[0].groups[1].name: must not be empty",
            "environments[1].groups[0].dashboards[2].name: must not be empty",
        ]
    );
}

#[test]
fn bad_fingerprints_are_reported() {
    let mut config = full_config();
    config.environments[0].tls.pinned_sha256 = Some("AB:CD:EF".to_owned());
    assert_eq!(
        issues(&config),
        [
            "environments[0].tls.pinned_sha256: is not a SHA-256 fingerprint: expected 64 hex \
             digits (32 bytes), found 6"
        ]
    );
    config.environments[0].tls.pinned_sha256 = Some(String::new());
    assert_eq!(
        issues(&config),
        ["environments[0].tls.pinned_sha256: is not a SHA-256 fingerprint: it is empty"]
    );
}

#[test]
fn authentication_needs_its_details() {
    let mut config = full_config();
    config.environments[0].auth = basic(" ");
    config.environments[1].auth = AuthConfig::ClientCertificate {
        cert_path: "certs/client.pem".into(),
        key_path: "~/certs/client.key".into(),
    };
    config.environments[1].author = None;
    assert_eq!(
        issues(&config),
        [
            "environments[0].auth.username: must not be empty",
            "environments[1].auth.cert_path: must be an absolute path",
            "environments[1].auth.key_path: must be an absolute path (`~` is not expanded)",
            "environments[1].author: must be set for client-certificate authentication, which has \
             no username to record as the author",
        ]
    );
    config.environments[1].auth = AuthConfig::ClientCertificate {
        cert_path: "".into(),
        key_path: "/certs/client.key".into(),
    };
    config.environments[1].author = Some("m.keller".to_owned());
    config.environments[0].auth = basic("icygui");
    assert_eq!(
        issues(&config),
        ["environments[1].auth.cert_path: must not be empty"]
    );
}

#[test]
fn tls_settings_are_checked() {
    let mut config = full_config();
    config.environments[0].tls.ca_file = Some("ca.crt".into());
    config.environments[0].tls.server_name = Some("https://master-01".to_owned());
    config.environments[1].tls.server_name = Some(" ".to_owned());
    assert_eq!(
        issues(&config),
        [
            "environments[0].tls.ca_file: must be an absolute path",
            "environments[0].tls.server_name: must be a host name or an IP address",
            "environments[1].tls.server_name: must not be empty when set",
        ]
    );
}

#[test]
fn the_active_environment_must_exist() {
    let mut config = full_config();
    config.active_environment = Some("gone".to_owned());
    assert_eq!(
        issues(&config),
        ["active_environment: no environment has the id `gone`"]
    );
    config.active_environment = None;
    assert_eq!(issues(&config), Vec::<String>::new());
}

#[test]
fn app_wide_settings_have_minimums() {
    let mut config = full_config();
    config.general.event_log_retention_hours = 0;
    config.general.reconcile_interval_secs = 5;
    assert_eq!(
        issues(&config),
        [
            "general.event_log_retention_hours: must be at least 1 hour",
            "general.reconcile_interval_secs: must be at least 10 seconds",
        ]
    );
}

#[test]
fn notification_settings_are_checked() {
    let mut config = full_config();
    let notifications = &mut config.environments[0].notifications;
    notifications.quiet_hours.start_minute = 24 * 60;
    notifications.quiet_hours.end_minute = u16::MAX;
    notifications.objects.push(ObjectOverride {
        object: ObjectKey::host("mq-prod-01"),
        mode: ObjectMode::Mute,
        until: Some(Timestamp::from_unix_seconds(1.0)),
    });
    notifications.objects.push(ObjectOverride {
        object: ObjectKey::service(" ", ""),
        mode: ObjectMode::Watch,
        until: None,
    });
    assert_eq!(
        issues(&config),
        [
            "environments[0].notifications.quiet_hours.start_minute: must be below 1440 (minutes \
             after midnight)",
            "environments[0].notifications.quiet_hours.end_minute: must be below 1440 (minutes \
             after midnight)",
            "environments[0].notifications.objects[2]: `mq-prod-01` already has an override at \
             objects[1]",
            "environments[0].notifications.objects[3].object: the host name must not be empty",
            "environments[0].notifications.objects[3].object: the service name must not be empty",
        ]
    );
}

#[test]
fn override_times_must_be_real() {
    let config: Config = toml::from_str(
        r#"
[[environments]]
id = "a"
name = "prod"
url = "https://master-01:5665"
auth = { kind = "basic", username = "icygui" }

[[environments.notifications.objects]]
object = { type = "host", name = "db-prod-03" }
mode = "mute"
until = nan
"#,
    )
    .unwrap();
    assert_eq!(
        issues(&config),
        ["environments[0].notifications.objects[0].until: must be a valid time"]
    );
}

#[test]
fn host_views_cannot_group_by_service_group() {
    let mut config = full_config();
    let view = &mut config.environments[0].groups[1].dashboards[1].view;
    assert_eq!(view.object_kind, ObjectKind::Hosts);
    view.group_by = GroupBy::ServiceGroup;
    assert_eq!(
        issues(&config),
        [
            "environments[0].groups[1].dashboards[1].view.group_by: hosts can't be grouped by service group"
        ]
    );
}

#[test]
fn environment_validation_uses_relative_paths() {
    let mut environment = Environment::new("", "http://master-01:5665", basic("icygui"));
    environment.id = String::new();
    let mut group = DashboardGroup::new("databases");
    group.dashboards.push(Dashboard::new("", View::default()));
    environment.groups.push(group);
    let issues: Vec<String> = environment
        .validate()
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(
        issues,
        [
            "id: must not be empty",
            "name: must not be empty",
            "url: must use https: the Icinga 2 API only accepts TLS connections",
            "groups[1].dashboards[0].name: must not be empty",
        ]
    );
}

#[test]
fn issues_have_a_path_and_a_message() {
    let mut config = full_config();
    config.environments[0].name = String::new();
    assert_eq!(
        config.validate(),
        [ValidationIssue {
            path: "environments[0].name".to_owned(),
            message: "must not be empty".to_owned(),
        }]
    );
}
