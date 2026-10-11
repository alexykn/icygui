//! Settings used across the tests.

use std::fs;
use std::path::Path;

use ic_config::{
    ApiUrl, Appearance, AuthConfig, CONFIG_VERSION, Config, ConfigStore, Dashboard, DashboardGroup,
    Environment, General, GridCells, GridColour, GridOptions, GroupBy, GroupOrder, GroupSource,
    HandledMode, HandledSetting, HideHandled, InterfaceSize, ListTimes, LogLevel, ObjectKind,
    RowDensity, Sort, SortKey, StreamEvents, StreamOptions, ThemeChoice, TlsConfig, View,
    ViewDisplay, ViewGroups, format_fingerprint,
};
use ic_model::{ObjectKey, Timestamp};
use ic_rules::{
    EventFilter, NotificationSettings, ObjectMode, ObjectOverride, QuietHours, Rule, ScopeSetting,
    StateFilter, StormControl,
};

/// Every theme, for round trips.
pub(crate) const THEMES: [ThemeChoice; 3] =
    [ThemeChoice::Dark, ThemeChoice::Light, ThemeChoice::System];

/// A valid configuration that uses every enum variant except the themes
/// (see [`THEMES`]) and sets every field away from its default somewhere.
pub(crate) fn full_config() -> Config {
    let prod = prod_cluster();
    Config {
        version: CONFIG_VERSION,
        general: General {
            close_to_tray: false,
            launch_at_login: true,
            event_log_retention_hours: 72,
            reconcile_interval_secs: 120,
            quiet_when_hidden: false,
            show_plugin_output: false,
            log_level: LogLevel::Debug,
        },
        appearance: Appearance {
            theme: ThemeChoice::Dark,
            interface_size: InterfaceSize::Large,
            row_density: RowDensity::Compact,
            list_times: ListTimes::Clock,
            hide_handled: HideHandled {
                in_downtime: false,
                ..HideHandled::ALL
            },
        },
        active_environment: Some(prod.id.clone()),
        environments: vec![prod, staging()],
    }
}

/// Basic auth, two URLs (the first pinned, with a server name), custom
/// notification rules and a group using every scope setting, object kind,
/// sort key and grouping.
fn prod_cluster() -> Environment {
    let mut prod = Environment::new(
        "prod-cluster",
        "https://master-01.example.com:5665",
        AuthConfig::Basic {
            username: "icygui".to_owned(),
        },
    );
    prod.author = Some("m.keller".to_owned());
    prod.urls[0].pinned_sha256 = Some(format_fingerprint(&[0xab; 32]));
    prod.urls[0].server_name = Some("master-01".to_owned());
    prod.urls
        .push(ApiUrl::new("https://master-02.example.com:5665"));
    prod.tls = TlsConfig {
        ca_file: Some("/etc/icinga2/pki/ca.crt".into()),
        use_system_roots: false,
    };
    prod.notifications = custom_notifications();
    prod.groups.push(databases());
    prod
}

/// Every notification setting away from its default, with a watched host
/// and a muted service.
fn custom_notifications() -> NotificationSettings {
    NotificationSettings {
        enabled: false,
        default_rule: Rule {
            states: StateFilter {
                critical: true,
                warning: true,
                unknown: false,
                down: true,
                unreachable: true,
                recovery: false,
            },
            hard_only: false,
            skip_handled: false,
            events: EventFilter {
                acknowledgements: true,
                downtimes: true,
                flapping: true,
            },
            min_duration_secs: 300,
            sound: false,
        },
        quiet_hours: QuietHours {
            enabled: true,
            start_minute: 23 * 60,
            end_minute: 6 * 60 + 30,
            days: [true, true, true, true, true, false, false],
            allow_critical: false,
        },
        storm: StormControl {
            window_secs: 30,
            threshold: 12,
        },
        objects: vec![
            ObjectOverride {
                object: ObjectKey::service("db-prod-03", "postgres-replication"),
                mode: ObjectMode::Mute,
                until: Some(Timestamp::from_unix_seconds(1_767_225_600.25)),
            },
            ObjectOverride {
                object: ObjectKey::host("mq-prod-01"),
                mode: ObjectMode::Watch,
                until: None,
            },
        ],
    }
}

/// A collapsed group with a custom rule and dashboards that use every
/// other scope setting, object kind, sort key and grouping.
fn databases() -> DashboardGroup {
    let mut databases = DashboardGroup::new("databases");
    databases.collapsed = true;
    databases.notifications = ScopeSetting::Custom(Rule {
        min_duration_secs: 60,
        ..Rule::default()
    });
    databases.dashboards = vec![
        Dashboard {
            notifications: ScopeSetting::On,
            ..Dashboard::new(
                "replication",
                View {
                    object_kind: ObjectKind::Services,
                    filter: r#"host.vars.role == "postgres" && service.state != 0"#.to_owned(),
                    problems_only: true,
                    handled: HandledSetting::SHOW,
                    sort: Sort {
                        key: SortKey::LastStateChange,
                        descending: false,
                    },
                    display: ViewDisplay::GroupedList,
                    group_by: GroupBy::Host,
                    ..View::default()
                },
            )
        },
        Dashboard {
            notifications: ScopeSetting::Off,
            ..Dashboard::new(
                "db hosts",
                View {
                    object_kind: ObjectKind::Hosts,
                    filter: r#"match("db-*", host.name)"#.to_owned(),
                    problems_only: false,
                    handled: HandledSetting::SETTINGS,
                    sort: Sort {
                        key: SortKey::Host,
                        descending: true,
                    },
                    display: ViewDisplay::GroupedList,
                    group_by: GroupBy::HostGroup,
                    ..View::default()
                },
            )
        },
        Dashboard::new(
            "by service group",
            View {
                object_kind: ObjectKind::Services,
                filter: "service.vars.team == \"dba\"\n  || \"databases\" in service.groups"
                    .to_owned(),
                problems_only: false,
                handled: HandledSetting {
                    mode: HandledMode::Hide,
                    hide: HideHandled {
                        host_down: false,
                        ..HideHandled::ALL
                    },
                },
                sort: Sort {
                    key: SortKey::Service,
                    descending: false,
                },
                display: ViewDisplay::GroupedList,
                group_by: GroupBy::ServiceGroup,
                ..View::default()
            },
        ),
        Dashboard::with_views("databases", multi_views()),
    ];
    databases
}

/// A dashboard's views of every display, with options away from their
/// defaults (v1, topic 04).
pub(crate) fn multi_views() -> Vec<View> {
    vec![
        View {
            name: "clusters".to_owned(),
            display: ViewDisplay::SummaryTiles,
            object_kind: ObjectKind::Services,
            groups: ViewGroups {
                host_groups: vec!["pg-*".to_owned(), "mysql-*".to_owned()],
                order: GroupOrder::Name,
                ..ViewGroups::default()
            },
            ..View::default()
        },
        View {
            name: "failing services".to_owned(),
            filter: "service.problem".to_owned(),
            collapsed: true,
            ..View::default()
        },
        View {
            name: "hosts by site".to_owned(),
            display: ViewDisplay::HostGroupGrid,
            object_kind: ObjectKind::Hosts,
            groups: ViewGroups {
                by: GroupSource::CustomVar,
                custom_var: "site".to_owned(),
                ..ViewGroups::default()
            },
            grid: GridOptions {
                colour: GridColour::HostOnly,
                cells: GridCells::LabelledCells,
                hide_healthy_groups: true,
                host_in_each_group: false,
            },
            ..View::default()
        },
        View {
            name: "db events".to_owned(),
            display: ViewDisplay::EventStream,
            filter: r#"host.vars.role in ["postgres", "mysql"]"#.to_owned(),
            stream: StreamOptions {
                events: StreamEvents {
                    flapping: true,
                    comments: false,
                    ..StreamEvents::default()
                },
                hard_states_only: false,
                recoveries: true,
                lines: 15,
            },
            ..View::default()
        },
    ]
}

/// Client-certificate auth with default TLS and notification settings.
fn staging() -> Environment {
    let mut staging = Environment::new(
        "staging",
        "https://staging.example.com:5665",
        AuthConfig::ClientCertificate {
            cert_path: "/home/m.keller/.icinga/client.pem".into(),
            key_path: "/home/m.keller/.icinga/client.key".into(),
        },
    );
    staging.author = Some("m.keller".to_owned());
    staging.tls = TlsConfig::default();
    staging
}

/// Writes `text` as the settings file in `dir` and returns its store.
pub(crate) fn store_with(dir: &Path, text: &str) -> ConfigStore {
    let path = dir.join("config.toml");
    fs::write(&path, text).unwrap();
    ConfigStore::new(path)
}

/// Every group and dashboard id of an environment.
pub(crate) fn ids(environment: &Environment) -> Vec<String> {
    environment
        .groups
        .iter()
        .flat_map(|group| {
            std::iter::once(group.id.clone()).chain(
                group
                    .dashboards
                    .iter()
                    .map(|dashboard| dashboard.id.clone()),
            )
        })
        .collect()
}
