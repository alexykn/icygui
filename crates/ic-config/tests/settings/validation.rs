//! `Config::validate` and `Environment::validate`.

use ic_config::{
    ApiUrl, AuthConfig, Config, Dashboard, DashboardGroup, Environment, GroupBy, ObjectKind,
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
    config.environments[0].urls[0].url = "http://master-01.example.com:5665".to_owned();
    assert_eq!(
        issues(&config),
        [
            "environments[0].urls[0].url: must use https: the Icinga 2 API only accepts TLS connections"
        ]
    );
}

#[test]
fn urls_need_a_host() {
    for url in ["https://", "https://:5665", "https:///"] {
        let mut config = full_config();
        config.environments[0].urls[0].url = url.to_owned();
        assert_eq!(
            issues(&config),
            ["environments[0].urls[0].url: has no host name"],
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
            "must not include /v1 or an API path: icygui adds them itself",
        ),
        (
            "https://master-01:5665/v1/objects/hosts",
            "must not include /v1 or an API path: icygui adds them itself",
        ),
        (
            "https://master-01:5665/V1/status/",
            "must not include /v1 or an API path: icygui adds them itself",
        ),
        (
            "https://icygui:secret@master-01:5665",
            "must not contain a user name or password; set them in the authentication settings",
        ),
    ] {
        let mut config = full_config();
        config.environments[0].urls[0].url = url.to_owned();
        assert_eq!(
            issues(&config),
            [format!("environments[0].urls[0].url: {message}")],
            "{url}"
        );
    }
}

#[test]
fn proxy_prefixes_are_fine() {
    for url in [
        "https://proxy.example.com/icinga",
        "https://gateway.example.com/v1/icinga/",
        "https://[2001:db8::1]:5665",
    ] {
        let mut config = full_config();
        config.environments[0].urls[0].url = url.to_owned();
        assert_eq!(issues(&config), Vec::<String>::new(), "{url}");
    }
}

#[test]
fn environments_list_one_to_sixteen_distinct_urls() {
    let mut config = full_config();
    config.environments[0].urls.clear();
    assert_eq!(
        issues(&config),
        ["environments[0].urls: must list at least one URL"]
    );

    config.environments[0].urls = (0..=ic_config::MAX_API_URLS)
        .map(|index| ApiUrl::new(&format!("https://master-{index:02}:5665")))
        .collect();
    assert_eq!(
        issues(&config),
        ["environments[0].urls: must list at most 16 URLs"]
    );

    // The same URL twice, also written differently, is listed once.
    config.environments[0].urls = vec![
        ApiUrl::new("https://master-01:5665"),
        ApiUrl::new("https://master-02:5665"),
        ApiUrl::new("HTTPS://Master-01:5665/"),
    ];
    assert_eq!(
        issues(&config),
        ["environments[0].urls[2].url: is already listed as URL 1"]
    );
}

#[test]
fn each_url_is_checked_on_its_own() {
    let mut config = full_config();
    config.environments[0].urls[1].url = "http://master-02:5665".to_owned();
    config.environments[0].urls[1].pinned_sha256 = Some("AB".to_owned());
    assert_eq!(
        issues(&config),
        [
            "environments[0].urls[1].url: must use https: the Icinga 2 API only accepts TLS \
             connections",
            "environments[0].urls[1].pinned_sha256: is not a SHA-256 fingerprint: expected 64 \
             hex digits (32 bytes), found 2",
        ]
    );
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
    config.environments[0].urls[0].pinned_sha256 = Some("AB:CD:EF".to_owned());
    assert_eq!(
        issues(&config),
        [
            "environments[0].urls[0].pinned_sha256: is not a SHA-256 fingerprint: expected 64 hex \
             digits (32 bytes), found 6"
        ]
    );
    config.environments[0].urls[0].pinned_sha256 = Some(String::new());
    assert_eq!(
        issues(&config),
        ["environments[0].urls[0].pinned_sha256: is not a SHA-256 fingerprint: it is empty"]
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
fn usernames_icinga_cannot_match_are_reported() {
    for (username, message) in [
        (
            "icygui:secret",
            "must not contain `:`; enter the password separately, icygui keeps it in the system \
             keychain",
        ),
        (
            ":",
            "must not contain `:`; enter the password separately, icygui keeps it in the system keychain",
        ),
        (" icygui", "must not start or end with whitespace"),
        ("icygui\n", "must not start or end with whitespace"),
        ("\t", "must not be empty"),
    ] {
        let mut config = full_config();
        config.environments[0].auth = basic(username);
        assert_eq!(
            issues(&config),
            [format!("environments[0].auth.username: {message}")],
            "{username:?}"
        );
    }
}

#[test]
fn tls_settings_are_checked() {
    let mut config = full_config();
    config.environments[0].tls.ca_file = Some("ca.crt".into());
    config.environments[0].urls[1].server_name = Some("https://master-01".to_owned());
    config.environments[1].urls[0].server_name = Some(" ".to_owned());
    assert_eq!(
        issues(&config),
        [
            "environments[0].urls[1].server_name: must be a host name or an IP address, not a URL",
            "environments[0].tls.ca_file: must be an absolute path",
            "environments[1].urls[0].server_name: must not be empty when set",
        ]
    );
}

#[test]
fn server_names_are_bare_host_names_or_addresses() {
    for name in [
        "master-01",
        "master-01.example.com",
        "master-01.example.com.",
        "icinga_master",
        "10.0.0.1",
        "::1",
        "2001:db8::1",
        "xn--mnchen-3ya.example",
    ] {
        let mut config = full_config();
        config.environments[0].urls[0].server_name = Some(name.to_owned());
        assert_eq!(issues(&config), Vec::<String>::new(), "{name}");
    }
    for (name, message) in [
        (" master-01", "must not start or end with whitespace"),
        ("master-01\n", "must not start or end with whitespace"),
        ("master-01:5665", "must not contain a port"),
        ("[::1]", "must be an IP address without brackets"),
        (
            "[2001:db8::1]:5665",
            "must be an IP address without brackets",
        ),
        (
            "master-01/",
            "must be a host name or an IP address, not a URL",
        ),
        ("master 01", "must be a host name or an IP address"),
        ("*.example.com", "must be a host name or an IP address"),
        (
            "münchen.example",
            "must be a host name in ASCII (international names in their xn-- form)",
        ),
    ] {
        let mut config = full_config();
        config.environments[0].urls[0].server_name = Some(name.to_owned());
        assert_eq!(
            issues(&config),
            [format!("environments[0].urls[0].server_name: {message}")],
            "{name:?}"
        );
    }
}

#[test]
fn long_values_are_quoted_in_part() {
    let mut config = full_config();
    let id = "x".repeat(100_000);
    config.environments[0].id.clone_from(&id);
    config.environments[1].id = id;
    config.active_environment = Some("y".repeat(100_000));
    let issues = config.validate();
    assert_eq!(issues.len(), 2, "{issues:?}");
    for issue in issues {
        assert!(issue.message.len() < 300, "{} bytes", issue.message.len());
    }
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
            "general.reconcile_interval_secs: must be 0 (adaptive) or at least 60 seconds",
        ]
    );
    // 0 is adaptive, and the default.
    config.general.event_log_retention_hours = 1;
    config.general.reconcile_interval_secs = 0;
    assert_eq!(issues(&config), Vec::<String>::new());
    assert_eq!(ic_config::General::default().reconcile_interval_secs, 0);
    config.general.reconcile_interval_secs = 60;
    assert_eq!(issues(&config), Vec::<String>::new());
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
urls = ["https://master-01:5665"]
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
    let view = &mut config.environments[0].groups[1].dashboards[1].views[0];
    assert_eq!(view.object_kind, ObjectKind::Hosts);
    view.group_by = GroupBy::ServiceGroup;
    assert_eq!(
        issues(&config),
        [
            "environments[0].groups[1].dashboards[1].views[0].group_by: hosts can't be grouped by service group"
        ]
    );
}

#[test]
fn dashboards_need_views_with_unique_ids() {
    let mut config = full_config();
    let databases = &mut config.environments[0].groups[1].dashboards;
    databases[0].views.clear();
    databases[3].views[1].id = databases[3].views[0].id.clone();
    databases[3].views[2].id = " ".to_owned();
    let path = "environments[0].groups[1].dashboards";
    let first = databases[3].views[0].id.clone();
    assert_eq!(
        issues(&config),
        [
            format!("{path}[0].views: must list at least one view"),
            format!("{path}[3].views[1].id: `{first}` is already used by {path}[3].views[0]"),
            format!("{path}[3].views[2].id: must not be empty"),
        ]
    );
    // Ids repeat freely across dashboards; at most MAX_VIEWS per dashboard.
    let mut config = full_config();
    let dashboards = &mut config.environments[0].groups[1].dashboards;
    dashboards[1].views[0].id = dashboards[0].views[0].id.clone();
    assert_eq!(issues(&config), Vec::<String>::new());
    let dashboards = &mut config.environments[0].groups[1].dashboards;
    let view = dashboards[3].views[1].clone();
    dashboards[3].views = (0..=ic_config::MAX_VIEWS)
        .map(|index| View {
            id: format!("v{index}"),
            ..view.clone()
        })
        .collect();
    assert_eq!(
        issues(&config),
        [format!(
            "environments[0].groups[1].dashboards[3].views: must list at most {} views",
            ic_config::MAX_VIEWS
        )]
    );
}

#[test]
fn grids_tiles_and_streams_need_usable_options() {
    let mut config = full_config();
    let views = &mut config.environments[0].groups[1].dashboards[3].views;
    // The tiles: an empty host group pattern; the grid: no custom var.
    views[0].groups.host_groups.push(" ".to_owned());
    views[2].groups.custom_var = "host.vars.".to_owned();
    views[3].stream.lines = 0;
    // A list ignores its grid and tiles options.
    views[1].groups.custom_var.clear();
    views[1].groups.by = ic_config::GroupSource::CustomVar;
    let path = "environments[0].groups[1].dashboards[3].views";
    assert_eq!(
        issues(&config),
        [
            format!("{path}[0].groups.host_groups[2]: must not be empty"),
            format!("{path}[2].groups.custom_var: must name a host custom variable to group by"),
            format!("{path}[3].stream.lines: must be between 1 and 200"),
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
            "urls[0].url: must use https: the Icinga 2 API only accepts TLS connections",
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
