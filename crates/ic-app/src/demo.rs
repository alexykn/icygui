//! Built-in demo data: the design's `prod-cluster` sample objects as an
//! `ic_config::Config` and an `ic_core::snapshot::Snapshot`.
//!
//! It stands in for the live core until the runtime exists. Dashboards are
//! evaluated here with small Rust predicates that mirror their filter
//! expressions, using the same rules the core will (docs/architecture.md):
//! rows apply `problems_only` then `hide_handled`, sort by the view's key with
//! severity, recency and names as tie-breaks, and the summary counts every
//! match.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use ic_config::{
    AuthConfig, CONFIG_VERSION, Config, Dashboard, DashboardGroup, Environment, General, GroupBy,
    ObjectKind, Sort, SortKey, View,
};
use ic_core::snapshot::{DashboardResult, DashboardRow, Snapshot, Summary};
use ic_model::{
    AckKind, CheckInfo, CheckResult, CheckableState, Comment, CommentKind, Dependency, Downtime,
    Endpoint, Host, HostGroup, HostName, HostState, InstanceStatus, ObjectKey, Service,
    ServiceGroup, ServiceKey, ServiceState, StateType, Timestamp, parse_perfdata,
};
use ic_rules::{DashboardRef, ScopeSetting};
use serde_json::json;

/// The demo environment's id (stable, so tests and screenshots can refer to it).
pub(crate) const ENVIRONMENT_ID: &str = "demo-prod-cluster";
/// The endpoint the demo pretends to be connected to.
pub(crate) const ENDPOINT: &str = "master-01";

const MINUTE: f64 = 60.;
const HOUR: f64 = 60. * MINUTE;
const DAY: f64 = 24. * HOUR;

/// The demo's configuration, snapshot and initial selection.
#[derive(Clone, Debug)]
pub(crate) struct Demo {
    /// One environment, `prod-cluster`, with three dashboard groups.
    pub(crate) config: Config,
    /// Objects and evaluated dashboards.
    pub(crate) snapshot: Snapshot,
    /// The dashboard selected at start (`overview / production`).
    pub(crate) selected: DashboardRef,
}

/// Builds the demo as of `now`; times in state are relative to it.
pub(crate) fn build(now: Timestamp) -> Demo {
    let hosts = hosts(now);
    let services = services(now, &hosts);
    let mut snapshot = Snapshot {
        revision: 1,
        taken_at: now,
        hosts: Arc::new(
            hosts
                .into_iter()
                .map(|host| (host.name.clone(), Arc::new(host)))
                .collect(),
        ),
        services: Arc::new(
            services
                .into_iter()
                .map(|service| (service.key.clone(), Arc::new(service)))
                .collect(),
        ),
        comments: Arc::new(comments(now)),
        downtimes: Arc::new(downtimes(now)),
        host_groups: Arc::new(host_groups()),
        service_groups: Arc::new(service_groups()),
        dependencies: Arc::new(dependencies()),
        endpoints: Arc::new(endpoints()),
        status: Some(Arc::new(status(now))),
        dashboards: Arc::default(),
    };

    let mut groups = Vec::new();
    let mut results = BTreeMap::new();
    for (group_name, dashboards) in DASHBOARDS {
        let group_id = format!("demo-{group_name}");
        let mut group = DashboardGroup {
            id: group_id.clone(),
            name: (*group_name).to_owned(),
            collapsed: false,
            notifications: ScopeSetting::Inherit,
            dashboards: Vec::new(),
        };
        for spec in *dashboards {
            let dashboard = Dashboard {
                id: format!("{group_id}-{}", spec.name),
                name: spec.name.to_owned(),
                view: spec.view(),
                notifications: ScopeSetting::Inherit,
            };
            let reference = DashboardRef {
                group_id: group_id.clone(),
                dashboard_id: dashboard.id.clone(),
            };
            results.insert(reference, evaluate(&snapshot, &dashboard.view, spec.filter));
            group.dashboards.push(dashboard);
        }
        groups.push(group);
    }
    snapshot.dashboards = Arc::new(results);

    let environment = Environment {
        id: ENVIRONMENT_ID.to_owned(),
        name: "prod-cluster".to_owned(),
        url: "https://master-01.example.com:5665".to_owned(),
        auth: AuthConfig::Basic {
            username: "icygui".to_owned(),
        },
        groups,
        ..Environment::default()
    };
    let config = Config {
        version: CONFIG_VERSION,
        general: General::default(),
        active_environment: Some(ENVIRONMENT_ID.to_owned()),
        environments: vec![environment],
    };
    Demo {
        config,
        snapshot,
        selected: DashboardRef {
            group_id: "demo-overview".to_owned(),
            dashboard_id: "demo-overview-production".to_owned(),
        },
    }
}

/// Which objects a demo dashboard matches; mirrors its filter expression.
#[derive(Clone, Copy, Debug)]
enum DemoFilter {
    /// The empty filter: everything.
    All,
    /// `host.vars.env == "…"`.
    Env(&'static str),
    /// `host.vars.role in [...]`.
    Roles(&'static [&'static str]),
    /// Certificate checks.
    Certificates,
    /// `service.vars.deploy_check == true`.
    DeployChecks,
}

impl DemoFilter {
    fn expression(self) -> String {
        match self {
            Self::All => String::new(),
            Self::Env(env) => format!("host.vars.env == \"{env}\""),
            Self::Roles(roles) => {
                let quoted: Vec<_> = roles.iter().map(|role| format!("\"{role}\"")).collect();
                format!("host.vars.role in [{}]", quoted.join(", "))
            }
            Self::Certificates => {
                "match(\"*cert*\", service.name) || service.name == \"http-tls\"".to_owned()
            }
            Self::DeployChecks => "service.vars.deploy_check == true".to_owned(),
        }
    }

    fn matches(self, host: &Host, service: Option<&Service>) -> bool {
        let host_var = |name: &str| host.vars.get(name).and_then(|value| value.as_str());
        match self {
            Self::All => true,
            Self::Env(env) => host_var("env") == Some(env),
            Self::Roles(roles) => host_var("role").is_some_and(|role| roles.contains(&role)),
            Self::Certificates => service.is_some_and(|service| {
                service.key.name.contains("cert") || &*service.key.name == "http-tls"
            }),
            Self::DeployChecks => service.is_some_and(|service| {
                service
                    .vars
                    .get("deploy_check")
                    .and_then(serde_json::Value::as_bool)
                    == Some(true)
            }),
        }
    }
}

struct DashboardSpec {
    name: &'static str,
    kind: ObjectKind,
    filter: DemoFilter,
    problems_only: bool,
    hide_handled: bool,
    group_by: GroupBy,
}

impl DashboardSpec {
    fn view(&self) -> View {
        View {
            object_kind: self.kind,
            filter: self.filter.expression(),
            problems_only: self.problems_only,
            hide_handled: self.hide_handled,
            sort: Sort::default(),
            group_by: self.group_by,
        }
    }
}

const fn spec(
    name: &'static str,
    kind: ObjectKind,
    filter: DemoFilter,
    problems_only: bool,
    hide_handled: bool,
) -> DashboardSpec {
    DashboardSpec {
        name,
        kind,
        filter,
        problems_only,
        hide_handled,
        group_by: GroupBy::None,
    }
}

/// Groups (folders) and their dashboards, in sidebar order. The names are the
/// design's; per PLAN.md D1 they're folders inside one environment.
const DASHBOARDS: &[(&str, &[DashboardSpec])] = &[
    (
        "overview",
        &[
            spec(
                "overview",
                ObjectKind::Services,
                DemoFilter::All,
                true,
                true,
            ),
            spec(
                "production",
                ObjectKind::Services,
                DemoFilter::Env("prod"),
                true,
                false,
            ),
            DashboardSpec {
                group_by: GroupBy::Host,
                ..spec(
                    "databases",
                    ObjectKind::Services,
                    DemoFilter::Roles(&["postgres", "redis"]),
                    true,
                    false,
                )
            },
        ],
    ),
    (
        "platform",
        &[
            spec(
                "network",
                ObjectKind::Hosts,
                DemoFilter::Roles(&["switch", "edge", "lb", "vpn"]),
                true,
                false,
            ),
            spec(
                "kubernetes",
                ObjectKind::Services,
                DemoFilter::Roles(&["k8s"]),
                true,
                true,
            ),
            spec(
                "certificates",
                ObjectKind::Services,
                DemoFilter::Certificates,
                true,
                false,
            ),
            spec(
                "deploy-checks",
                ObjectKind::Services,
                DemoFilter::DeployChecks,
                false,
                true,
            ),
        ],
    ),
    (
        "lab",
        &[spec(
            "sandbox",
            ObjectKind::Services,
            DemoFilter::Env("lab"),
            true,
            true,
        )],
    ),
];

/// A matched host or service with what dashboards need to sort and count it.
#[derive(Clone, Debug)]
struct Match {
    key: ObjectKey,
    host_name: HostName,
    service_name: Option<Arc<str>>,
    state: CheckableState,
    problem: bool,
    handled: bool,
    severity: u32,
    last_state_change: Timestamp,
}

impl Match {
    fn host(host: &Host) -> Self {
        Self {
            key: host.key(),
            host_name: host.name.clone(),
            service_name: None,
            state: CheckableState::Host(host.state),
            problem: host.is_problem(),
            handled: host.is_handled(),
            severity: host.severity(),
            last_state_change: host.check.last_state_change,
        }
    }

    fn service(service: &Service, host: Option<&Host>) -> Self {
        let host_problem = host.is_some_and(Host::is_problem);
        Self {
            key: service.object_key(),
            host_name: service.key.host.clone(),
            service_name: Some(service.key.name.clone()),
            state: CheckableState::Service(service.state),
            problem: service.is_problem(),
            handled: service.is_handled(host_problem),
            severity: service.severity(),
            last_state_change: service.check.last_state_change,
        }
    }
}

fn evaluate(snapshot: &Snapshot, view: &View, filter: DemoFilter) -> DashboardResult {
    let matches: Vec<Match> = match view.object_kind {
        ObjectKind::Hosts => snapshot
            .hosts
            .values()
            .filter(|host| filter.matches(host, None))
            .map(|host| Match::host(host))
            .collect(),
        ObjectKind::Services => snapshot
            .services
            .values()
            .filter_map(|service| {
                let host = snapshot.host_of(&service.key).map(Arc::as_ref);
                let matched = host.is_some_and(|host| filter.matches(host, Some(service)));
                matched.then(|| Match::service(service, host))
            })
            .collect(),
    };
    let summary = summarize(&matches);
    let mut visible: Vec<Match> = matches
        .into_iter()
        .filter(|object| !view.problems_only || object.problem)
        .filter(|object| !view.hide_handled || !object.handled)
        .collect();
    visible.sort_by(|a, b| compare(a, b, view.sort));
    DashboardResult {
        rows: Arc::new(group_rows(visible, view.group_by)),
        summary,
        error: None,
    }
}

fn summarize(matches: &[Match]) -> Summary {
    let mut summary = Summary::default();
    let mut worst: Option<&Match> = None;
    for object in matches {
        let counter = match object.state {
            CheckableState::Service(ServiceState::Ok) | CheckableState::Host(HostState::Up) => {
                &mut summary.ok
            }
            CheckableState::Service(ServiceState::Warning) => &mut summary.warning,
            CheckableState::Service(ServiceState::Critical) => &mut summary.critical,
            CheckableState::Service(ServiceState::Unknown) => &mut summary.unknown,
            CheckableState::Host(HostState::Down) => &mut summary.down,
            CheckableState::Host(HostState::Unreachable) => &mut summary.unreachable,
            CheckableState::Service(ServiceState::Pending)
            | CheckableState::Host(HostState::Pending) => &mut summary.pending,
        };
        *counter += 1;
        if !object.problem {
            continue;
        }
        if object.handled {
            summary.handled += 1;
        } else {
            summary.unhandled += 1;
            if worst.is_none_or(|worst| object.severity > worst.severity) {
                worst = Some(object);
            }
        }
    }
    summary.worst_unhandled = worst.map(|object| object.state);
    summary
}

/// The view's sort key, then severity (desc), last state change (newest
/// first), host and service name.
fn compare(a: &Match, b: &Match, sort: Sort) -> Ordering {
    let primary = match sort.key {
        SortKey::Severity => a.severity.cmp(&b.severity),
        SortKey::LastStateChange => cmp_time(a.last_state_change, b.last_state_change),
        SortKey::Host => a.host_name.cmp(&b.host_name),
        SortKey::Service => a
            .service_name
            .as_deref()
            .unwrap_or(a.host_name.as_str())
            .cmp(b.service_name.as_deref().unwrap_or(b.host_name.as_str())),
    };
    let primary = if sort.descending {
        primary.reverse()
    } else {
        primary
    };
    primary
        .then_with(|| b.severity.cmp(&a.severity))
        .then_with(|| cmp_time(b.last_state_change, a.last_state_change))
        .then_with(|| a.host_name.cmp(&b.host_name))
        .then_with(|| a.service_name.cmp(&b.service_name))
}

fn group_rows(sorted: Vec<Match>, group_by: GroupBy) -> Vec<DashboardRow> {
    match group_by {
        // The demo only groups by host; host and service groups need the
        // group memberships the core evaluates.
        GroupBy::Host => {
            let mut groups: Vec<(HostName, u32, Vec<ObjectKey>)> = Vec::new();
            for object in sorted {
                match groups
                    .iter_mut()
                    .find(|(host, ..)| *host == object.host_name)
                {
                    Some((_, worst, keys)) => {
                        *worst = (*worst).max(object.severity);
                        keys.push(object.key);
                    }
                    None => groups.push((object.host_name, object.severity, vec![object.key])),
                }
            }
            groups.sort_by(|(a_host, a_worst, _), (b_host, b_worst, _)| {
                b_worst.cmp(a_worst).then_with(|| a_host.cmp(b_host))
            });
            groups
                .into_iter()
                .flat_map(|(host, _, keys)| {
                    let header = DashboardRow::Group {
                        label: host.to_string(),
                        count: keys.len(),
                    };
                    std::iter::once(header).chain(keys.into_iter().map(DashboardRow::Object))
                })
                .collect()
        }
        GroupBy::None | GroupBy::HostGroup | GroupBy::ServiceGroup => sorted
            .into_iter()
            .map(|object| DashboardRow::Object(object.key))
            .collect(),
    }
}

fn cmp_time(a: Timestamp, b: Timestamp) -> Ordering {
    a.as_unix_seconds().total_cmp(&b.as_unix_seconds())
}

// ---------------------------------------------------------------------------
// Objects
// ---------------------------------------------------------------------------

fn ago(now: Timestamp, seconds: f64) -> Timestamp {
    Timestamp::from_unix_seconds(now.as_unix_seconds() - seconds)
}

/// Check details for an object that has been in its state for `since`
/// seconds, checked `checked` seconds ago.
fn check_info(now: Timestamp, since: f64, checked: f64, interval: f64) -> CheckInfo {
    let last_check = ago(now, checked);
    CheckInfo {
        state_type: StateType::Hard,
        last_state_change: ago(now, since),
        last_hard_state_change: ago(now, since),
        last_check: Some(last_check),
        next_check: Some(
            last_check.plus(Duration::try_from_secs_f64(interval).unwrap_or_default()),
        ),
        attempt: 1,
        max_attempts: 3,
        check_interval: interval,
        retry_interval: interval / 4.,
        zone: Some("master".to_owned()),
        ..CheckInfo::default()
    }
}

fn check_result(now: Timestamp, raw_output: &str, exit_status: i32, checked: f64) -> CheckResult {
    let (output, long_output) = CheckResult::split_output(raw_output);
    let end = ago(now, checked);
    CheckResult {
        output,
        long_output,
        perfdata: Vec::new(),
        exit_status,
        schedule_start: ago(now, checked + 0.31),
        execution_start: ago(now, checked + 0.3),
        execution_end: end,
        check_source: ENDPOINT.to_owned(),
        active: true,
    }
}

struct HostSpec {
    name: &'static str,
    address: &'static str,
    role: &'static str,
    state: HostState,
    since: f64,
    output: &'static str,
}

const fn up(name: &'static str, address: &'static str, role: &'static str) -> HostSpec {
    HostSpec {
        name,
        address,
        role,
        state: HostState::Up,
        since: 41. * DAY,
        output: "PING OK - Packet loss = 0%, RTA = 0.42 ms",
    }
}

const HOSTS: &[HostSpec] = &[
    up("db-prod-01", "10.0.2.11", "postgres"),
    up("db-prod-02", "10.0.2.12", "postgres"),
    up("db-prod-03", "10.0.2.13", "postgres"),
    up("cache-02", "10.0.2.32", "redis"),
    up("mq-prod-01", "10.0.3.21", "rabbitmq"),
    up("k8s-node-02", "10.0.4.22", "k8s"),
    up("k8s-node-03", "10.0.4.23", "k8s"),
    up("k8s-node-04", "10.0.4.24", "k8s"),
    up("k8s-node-07", "10.0.4.27", "k8s"),
    HostSpec {
        state: HostState::Down,
        since: 12. * MINUTE,
        output: "PING CRITICAL - Packet loss = 100%",
        ..up("k8s-node-11", "10.0.4.31", "k8s")
    },
    up("web-edge-01", "10.0.1.11", "edge"),
    up("web-edge-02", "10.0.1.12", "edge"),
    up("lb-prod-02", "10.0.1.32", "lb"),
    up("api-gw-01", "10.0.1.41", "api"),
    up("backup-01", "10.0.5.11", "backup"),
    up("vpn-gw-01", "10.0.0.5", "vpn"),
    HostSpec {
        state: HostState::Down,
        since: 11. * MINUTE,
        output: "PING CRITICAL - Packet loss = 100%",
        ..up("sw-core-ams-02", "10.8.0.2", "switch")
    },
    HostSpec {
        state: HostState::Unreachable,
        since: 11. * MINUTE,
        output: "PING CRITICAL - Packet loss = 100%",
        ..up("edge-ams-01", "10.8.1.21", "edge")
    },
    HostSpec {
        state: HostState::Unreachable,
        since: 11. * MINUTE,
        output: "PING CRITICAL - Packet loss = 100%",
        ..up("edge-ams-02", "10.8.1.22", "edge")
    },
    HostSpec {
        state: HostState::Unreachable,
        since: 11. * MINUTE,
        output: "PING CRITICAL - Packet loss = 100%",
        ..up("edge-ams-03", "10.8.1.23", "edge")
    },
    HostSpec {
        state: HostState::Unreachable,
        since: 11. * MINUTE,
        output: "PING CRITICAL - Packet loss = 100%",
        ..up("edge-ams-04", "10.8.1.24", "edge")
    },
    HostSpec {
        state: HostState::Unreachable,
        since: 11. * MINUTE,
        output: "PING CRITICAL - Packet loss = 100%",
        ..up("edge-ams-05", "10.8.1.25", "edge")
    },
    HostSpec {
        state: HostState::Down,
        since: HOUR + 8. * MINUTE,
        output: "PING CRITICAL - Packet loss = 100%",
        ..up("edge-fra-04", "185.22.10.4", "edge")
    },
    HostSpec {
        state: HostState::Down,
        since: 3. * MINUTE,
        output: "PING CRITICAL - Packet loss = 100%",
        ..up("store-ams-02", "10.8.1.12", "storage")
    },
];

fn hosts(now: Timestamp) -> Vec<Host> {
    HOSTS
        .iter()
        .map(|spec| {
            let mut host = Host::new(spec.name);
            spec.address.clone_into(&mut host.address);
            host.state = spec.state;
            host.check = check_info(now, spec.since, 31., 60.);
            "hostalive".clone_into(&mut host.check.check_command);
            let exit_status = i32::from(spec.state != HostState::Up) * 2;
            host.check.result = Some(check_result(now, spec.output, exit_status, 31.));
            if spec.state.is_problem() {
                host.check.attempt = host.check.max_attempts;
            }
            host.check.reachable = spec.state != HostState::Unreachable;
            host.vars.insert("env".to_owned(), json!("prod"));
            host.vars.insert("role".to_owned(), json!(spec.role));
            host.vars.insert("os".to_owned(), json!("linux"));
            host.groups = host_groups_for(spec.role);
            host
        })
        .map(|mut host| {
            match host.name.as_str() {
                "sw-core-ams-02" => host.check.acknowledgement = AckKind::Sticky,
                "edge-fra-04" => host.check.downtime_depth = 1,
                "db-prod-03" => {
                    if let Some(result) = host.check.result.as_mut() {
                        result.perfdata = parse_perfdata("rta=0.42ms;100;200;0 pl=0%;20;60;0");
                    }
                }
                _ => {}
            }
            host
        })
        .collect()
}

fn host_groups_for(role: &str) -> Vec<String> {
    let group = match role {
        "postgres" | "redis" => "db-prod",
        "k8s" => "kubernetes",
        "edge" | "lb" | "switch" | "vpn" => "network",
        _ => "infrastructure",
    };
    vec!["linux-servers".to_owned(), group.to_owned()]
}

/// One service: `(host, name, state, seconds in state, output)`.
type ServiceSpec = (&'static str, &'static str, ServiceState, f64, &'static str);

/// The design's problem rows (screen 2a), the host pane's services (2c) and
/// turn 1's events.
const PROBLEMS: &[ServiceSpec] = &[
    (
        "db-prod-03",
        "postgres-replication",
        ServiceState::Critical,
        14. * MINUTE,
        "CRITICAL - standby lag 412s (> 300s)\nprimary  db-prod-01  lsn 4A/9C21F0D8\nstandby  db-prod-03  lsn 4A/2E77A120\nslot     repl_db03   retained 1.8 GiB",
    ),
    (
        "mq-prod-01",
        "rabbitmq-queue",
        ServiceState::Critical,
        6. * MINUTE,
        "CRITICAL - queue orders.retry depth 18,402",
    ),
    (
        "k8s-node-07",
        "disk /var",
        ServiceState::Critical,
        38. * MINUTE,
        "DISK CRITICAL - /var 97% used (1.2 GiB free)",
    ),
    (
        "web-edge-02",
        "http-tls",
        ServiceState::Critical,
        2. * HOUR + 4. * MINUTE,
        "SSL CRITICAL - certificate expires in 2 days",
    ),
    (
        "lb-prod-02",
        "haproxy-backend",
        ServiceState::Warning,
        9. * MINUTE,
        "WARNING - backend api: 2/6 servers down",
    ),
    (
        "api-gw-01",
        "http-latency",
        ServiceState::Warning,
        21. * MINUTE,
        "WARNING - p95 1.84s (> 1.5s)",
    ),
    (
        "db-prod-01",
        "load",
        ServiceState::Warning,
        HOUR + 2. * MINUTE,
        "WARNING - load average 14.2, 12.8, 11.1",
    ),
    (
        "cache-02",
        "redis-memory",
        ServiceState::Warning,
        3. * HOUR,
        "WARNING - used_memory 81% of maxmemory",
    ),
    (
        "backup-01",
        "borg-last-run",
        ServiceState::Unknown,
        52. * MINUTE,
        "UNKNOWN - repository lock held by PID 4412",
    ),
    (
        "k8s-node-04",
        "kubelet",
        ServiceState::Unknown,
        4. * MINUTE,
        "UNKNOWN - connection refused (10.0.4.24:10250)",
    ),
    (
        "k8s-node-02",
        "ntp-offset",
        ServiceState::Warning,
        2. * HOUR,
        "WARNING - offset 0.82s",
    ),
    (
        "vpn-gw-01",
        "cert-expiry",
        ServiceState::Warning,
        DAY + 3. * HOUR,
        "WARNING - certificate expires in 12 days",
    ),
    (
        "db-prod-03",
        "pg-connections",
        ServiceState::Warning,
        22. * MINUTE,
        "WARNING - 182 of 200 per-db limit (orders)",
    ),
    (
        "db-prod-03",
        "disk /var/lib/postgresql",
        ServiceState::Ok,
        6. * DAY,
        "DISK OK - 63% used",
    ),
    (
        "k8s-node-11",
        "kubelet",
        ServiceState::Critical,
        12. * MINUTE,
        "CRITICAL - connection refused (10.0.4.31:10250)",
    ),
    (
        "web-edge-01",
        "nginx-workers",
        ServiceState::Ok,
        6. * MINUTE,
        "OK - 16 of 16 workers running",
    ),
    (
        "web-edge-01",
        "cert-expiry",
        ServiceState::Ok,
        30. * DAY,
        "OK - certificate expires in 74 days",
    ),
    (
        "web-edge-02",
        "deploy-version",
        ServiceState::Ok,
        5. * HOUR,
        "OK - release 2026.10.1 (a41f9c2)",
    ),
    (
        "api-gw-01",
        "deploy-version",
        ServiceState::Ok,
        5. * HOUR,
        "OK - release 2026.10.1 (a41f9c2)",
    ),
    (
        "api-gw-01",
        "http-health",
        ServiceState::Ok,
        5. * HOUR,
        "HTTP OK - 200 in 0.012s",
    ),
];

/// Every host runs these; `PROBLEMS` replaces any with the same name.
const COMMON: &[(&str, &str)] = &[
    ("disk /", "DISK OK - 41% used"),
    ("load", "OK - load average 3.1, 2.8, 2.6"),
    ("memory", "OK - 71% used"),
    ("ntp-offset", "OK - offset 0.004s"),
    ("ssh", "SSH OK - OpenSSH_9.6p1 (protocol 2.0)"),
];

/// The rest of db-prod-03's 23 services (the host pane's "+ 16 more ok").
const DB_PROD_03_EXTRA: &[&str] = &[
    "apt",
    "pg-autovacuum",
    "pg-backup",
    "pg-bloat",
    "pg-cache-hit",
    "pg-checkpoints",
    "pg-deadlocks",
    "pg-locks",
    "pg-txid-wraparound",
    "pg-wal-archive",
    "procs",
    "smart /dev/nvme0n1",
    "swap",
    "systemd",
    "users",
];

fn services(now: Timestamp, hosts: &[Host]) -> Vec<Service> {
    let mut services: BTreeMap<ServiceKey, Service> = BTreeMap::new();
    let mut add = |service: Service| {
        services.insert(service.key.clone(), service);
    };
    for (index, host) in hosts.iter().enumerate() {
        // Spread the generic services' ages so the lists aren't uniform.
        let age = DAY * (2. + f64::from(u8::try_from(index % 9).unwrap_or(0)));
        for (name, output) in COMMON {
            add(service(now, host, name, ServiceState::Ok, age, output));
        }
        if host.name.as_str() == "db-prod-03" {
            for name in DB_PROD_03_EXTRA {
                add(service(now, host, name, ServiceState::Ok, 9. * DAY, "OK"));
            }
        }
        if host.name.as_str().starts_with("edge-ams-") {
            // Unreachable behind sw-core-ams-02: failing, but handled.
            add(service(
                now,
                host,
                "http",
                ServiceState::Critical,
                11. * MINUTE,
                "CRITICAL - Socket timeout after 10 seconds",
            ));
        }
    }
    for (host_name, name, state, since, output) in PROBLEMS {
        if let Some(host) = hosts.iter().find(|host| host.name.as_str() == *host_name) {
            add(service(now, host, name, *state, *since, output));
        }
    }
    for service in services.values_mut() {
        customize(now, service);
    }
    services.into_values().collect()
}

fn service(
    now: Timestamp,
    host: &Host,
    name: &str,
    state: ServiceState,
    since: f64,
    output: &str,
) -> Service {
    let mut service = Service::new(host.name.as_str(), name);
    service.state = state;
    service.check = check_info(now, since, 12., 60.);
    // Services depend on their host: Icinga marks them unreachable while the
    // host is down or unreachable.
    service.check.reachable = host.state == HostState::Up;
    default_command(name).clone_into(&mut service.check.check_command);
    if state.is_problem() {
        service.check.attempt = service.check.max_attempts;
    }
    let exit_status = match state {
        ServiceState::Ok | ServiceState::Pending => 0,
        ServiceState::Warning => 1,
        ServiceState::Critical => 2,
        ServiceState::Unknown => 3,
    };
    service.check.result = Some(check_result(now, output, exit_status, 12.));
    service.groups = Vec::new();
    service
}

fn default_command(name: &str) -> &'static str {
    match name.split_whitespace().next().unwrap_or_default() {
        "disk" => "disk",
        "load" => "load",
        "memory" => "mem",
        "ntp-offset" => "ntp_time",
        "ssh" => "ssh",
        "http" | "http-health" | "http-latency" | "http-tls" | "cert-expiry" => "http",
        name if name.starts_with("pg-") || name.starts_with("postgres") => "postgres",
        _ => "by_ssh",
    }
}

/// Details the design shows for particular services.
fn customize(now: Timestamp, service: &mut Service) {
    match (service.key.host.as_str(), &*service.key.name) {
        ("db-prod-03", "postgres-replication") => {
            "check_postgres".clone_into(&mut service.check.check_command);
            service.check.check_interval = 60.;
            service.check.retry_interval = 15.;
            service.check.command_endpoint = Some("sat-ams-01".to_owned());
            service.check.zone = Some("ams".to_owned());
            service.groups = vec!["databases".to_owned(), "replication".to_owned()];
            service
                .vars
                .insert("pg_cluster".to_owned(), json!("orders-main"));
            service.vars.insert("lag_crit".to_owned(), json!(300));
            service
                .vars
                .insert("runbook".to_owned(), json!("wiki/db/replication-lag"));
            if let Some(result) = service.check.result.as_mut() {
                result.perfdata = parse_perfdata(
                    "replication_lag=412s;60;300;0;3600 wal_retained=1.8GiB;4;8 active_connections=182;400;450;0",
                );
                "sat-ams-01".clone_into(&mut result.check_source);
                result.execution_start = ago(now, 13.82);
            }
        }
        ("k8s-node-04", "kubelet") => {
            // Still retrying: soft state, attempt 2 of 3.
            service.check.state_type = StateType::Soft;
            service.check.attempt = 2;
        }
        ("web-edge-02", "http-tls") => {
            service.check.acknowledgement = AckKind::Normal;
            service.check.max_attempts = 4;
            service.check.attempt = 4;
        }
        ("k8s-node-07", "disk /var") => {
            service.check.max_attempts = 5;
            service.check.attempt = 5;
            if let Some(result) = service.check.result.as_mut() {
                result.perfdata = parse_perfdata("/var=38.8GiB;32;36;0;40");
            }
        }
        ("cache-02", "redis-memory") => service.check.downtime_depth = 1,
        ("api-gw-01" | "web-edge-02", "deploy-version") | ("api-gw-01", "http-health") => {
            service.vars.insert("deploy_check".to_owned(), json!(true));
        }
        ("web-edge-01", "nginx-workers") => {
            service.vars.insert("deploy_check".to_owned(), json!(true));
            service.check.state_type = StateType::Hard;
        }
        ("api-gw-01", "http-latency") => {
            if let Some(result) = service.check.result.as_mut() {
                result.perfdata = parse_perfdata("p95=1.84s;1.5;3;0 p50=0.21s;;;0");
            }
        }
        _ => {}
    }
    if service.key.name.contains("cert") || &*service.key.name == "http-tls" {
        service.groups.push("certificates".to_owned());
    }
}

fn comment(
    object: ObjectKey,
    id: &str,
    author: &str,
    text: &str,
    kind: CommentKind,
    at: Timestamp,
) -> Comment {
    Comment {
        name: format!("{}!{id}", object.full_name()),
        object,
        author: author.to_owned(),
        text: text.to_owned(),
        kind,
        entry_time: at,
        expire_time: None,
        persistent: false,
    }
}

fn comments(now: Timestamp) -> BTreeMap<ObjectKey, Vec<Comment>> {
    let all = [
        comment(
            ObjectKey::service("db-prod-03", "postgres-replication"),
            "demo-comment-1",
            "j.berg",
            "Failover drill on db-prod-01 at 15:00. Expect lag on 03.",
            CommentKind::User,
            ago(now, 34. * MINUTE),
        ),
        comment(
            ObjectKey::service("web-edge-02", "http-tls"),
            "demo-ack-1",
            "m.keller",
            "renewal in progress",
            CommentKind::Acknowledgement,
            ago(now, HOUR + 50. * MINUTE),
        ),
        comment(
            ObjectKey::host("sw-core-ams-02"),
            "demo-ack-2",
            "a.ivanova",
            "line card replacement, vendor on site",
            CommentKind::Acknowledgement,
            ago(now, 9. * MINUTE),
        ),
    ];
    let mut by_object: BTreeMap<ObjectKey, Vec<Comment>> = BTreeMap::new();
    for comment in all {
        by_object
            .entry(comment.object.clone())
            .or_default()
            .push(comment);
    }
    by_object
}

fn downtime(
    object: ObjectKey,
    id: &str,
    author: &str,
    text: &str,
    started: f64,
    remaining: f64,
    now: Timestamp,
) -> Downtime {
    Downtime {
        name: format!("{}!{id}", object.full_name()),
        object,
        author: author.to_owned(),
        comment: text.to_owned(),
        start_time: ago(now, started),
        end_time: ago(now, -remaining),
        fixed: true,
        duration: started + remaining,
        entry_time: ago(now, started + 5. * MINUTE),
        trigger_time: Some(ago(now, started)),
        triggered_by: None,
        parent: None,
        in_effect: true,
        config_owned: false,
    }
}

fn downtimes(now: Timestamp) -> BTreeMap<ObjectKey, Vec<Downtime>> {
    let all = [
        downtime(
            ObjectKey::service("cache-02", "redis-memory"),
            "demo-downtime-1",
            "s.ortiz",
            "maxmemory resize",
            3. * HOUR,
            HOUR,
            now,
        ),
        downtime(
            ObjectKey::host("edge-fra-04"),
            "demo-downtime-2",
            "a.ivanova",
            "rack maintenance",
            HOUR + 10. * MINUTE,
            50. * MINUTE,
            now,
        ),
    ];
    all.into_iter()
        .map(|downtime| (downtime.object.clone(), vec![downtime]))
        .collect()
}

fn host_groups() -> Vec<HostGroup> {
    [
        ("db-prod", "Production databases"),
        ("infrastructure", "Infrastructure"),
        ("kubernetes", "Kubernetes nodes"),
        ("linux-servers", "Linux servers"),
        ("network", "Network"),
    ]
    .into_iter()
    .map(|(name, display_name)| HostGroup {
        name: name.to_owned(),
        display_name: display_name.to_owned(),
    })
    .collect()
}

fn service_groups() -> Vec<ServiceGroup> {
    [
        ("certificates", "Certificates"),
        ("databases", "Databases"),
        ("replication", "Replication"),
    ]
    .into_iter()
    .map(|(name, display_name)| ServiceGroup {
        name: name.to_owned(),
        display_name: display_name.to_owned(),
    })
    .collect()
}

fn dependencies() -> Vec<Dependency> {
    (1..=5)
        .map(|index| {
            let child = format!("edge-ams-0{index}");
            Dependency {
                name: format!("{child}!uplink"),
                child: ObjectKey::host(&child),
                parent: ObjectKey::host("sw-core-ams-02"),
            }
        })
        .collect()
}

fn endpoints() -> Vec<Endpoint> {
    [
        ("master-01", "master"),
        ("sat-ams-01", "ams"),
        ("sat-fra-01", "fra"),
    ]
    .into_iter()
    .map(|(name, zone)| Endpoint {
        name: name.to_owned(),
        zone: zone.to_owned(),
        connected: true,
    })
    .collect()
}

fn status(now: Timestamp) -> InstanceStatus {
    InstanceStatus {
        node_name: ENDPOINT.to_owned(),
        version: "r2.15.6-1".to_owned(),
        program_start: ago(now, 9. * DAY),
        notifications_enabled: true,
        host_checks_enabled: true,
        service_checks_enabled: true,
        event_handlers_enabled: true,
        flap_detection_enabled: true,
        perfdata_enabled: true,
        checks_per_minute: 1840.,
        avg_latency: 0.004,
        avg_execution_time: 0.31,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    fn demo() -> Demo {
        build(Timestamp::from_unix_seconds(1_790_000_000.))
    }

    fn result<'a>(demo: &'a Demo, group: &str, dashboard: &str) -> &'a DashboardResult {
        let reference = DashboardRef {
            group_id: format!("demo-{group}"),
            dashboard_id: format!("demo-{group}-{dashboard}"),
        };
        demo.snapshot.dashboards.get(&reference).unwrap()
    }

    #[test]
    fn one_environment_with_the_designs_folders() {
        let demo = demo();
        let config = &demo.config;
        assert_eq!(config.environments.len(), 1);
        assert_eq!(config.active_environment.as_deref(), Some(ENVIRONMENT_ID));
        let environment = &config.environments[0];
        assert_eq!(environment.name, "prod-cluster");
        let names: Vec<_> = environment.groups.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, ["overview", "platform", "lab"]);
        let dashboards: Vec<_> = environment
            .groups
            .iter()
            .flat_map(|group| group.dashboards.iter().map(|d| d.name.as_str()))
            .collect();
        assert_eq!(
            dashboards,
            [
                "overview",
                "production",
                "databases",
                "network",
                "kubernetes",
                "certificates",
                "deploy-checks",
                "sandbox"
            ]
        );
    }

    #[test]
    fn ids_are_unique_and_every_dashboard_has_a_result() {
        let demo = demo();
        let environment = &demo.config.environments[0];
        let mut ids = HashSet::new();
        for group in &environment.groups {
            assert!(ids.insert(group.id.clone()));
            for dashboard in &group.dashboards {
                assert!(ids.insert(dashboard.id.clone()));
                let reference = DashboardRef {
                    group_id: group.id.clone(),
                    dashboard_id: dashboard.id.clone(),
                };
                assert!(demo.snapshot.dashboards.contains_key(&reference));
            }
        }
        assert!(demo.snapshot.dashboards.contains_key(&demo.selected));
    }

    #[test]
    fn rows_reference_objects_in_the_snapshot() {
        let demo = demo();
        for result in demo.snapshot.dashboards.values() {
            for row in result.rows.iter() {
                if let DashboardRow::Object(key) = row {
                    let exists = match key {
                        ObjectKey::Host { name } => demo.snapshot.hosts.contains_key(name),
                        ObjectKey::Service { key } => demo.snapshot.services.contains_key(key),
                    };
                    assert!(exists, "{key} is missing");
                }
            }
        }
    }

    #[test]
    fn dashboards_cover_every_dot_colour() {
        let demo = demo();
        let worst = |group, dashboard| result(&demo, group, dashboard).summary.worst_unhandled;
        let critical = Some(CheckableState::Service(ServiceState::Critical));
        assert_eq!(worst("overview", "overview"), critical);
        assert_eq!(worst("overview", "production"), critical);
        assert_eq!(worst("overview", "databases"), critical);
        assert_eq!(
            worst("platform", "network"),
            Some(CheckableState::Host(HostState::Unreachable))
        );
        assert_eq!(
            worst("platform", "certificates"),
            Some(CheckableState::Service(ServiceState::Warning))
        );
        assert_eq!(worst("platform", "deploy-checks"), None);
        assert_eq!(worst("lab", "sandbox"), None);
        assert!(result(&demo, "platform", "deploy-checks").summary.ok > 0);
        assert_eq!(result(&demo, "lab", "sandbox").summary, Summary::default());
    }

    #[test]
    fn network_counts_the_unreachable_hosts_behind_the_switch() {
        let demo = demo();
        let summary = result(&demo, "platform", "network").summary;
        assert_eq!(summary.unreachable, 5);
        assert_eq!(
            summary.unhandled, 5,
            "the switch is acked, edge-fra-04 in downtime"
        );
        assert_eq!(summary.handled, 2);
        assert_eq!(summary.down, 2);
    }

    #[test]
    fn production_shows_the_designs_rows_including_handled_ones() {
        let demo = demo();
        let production = result(&demo, "overview", "production");
        let rows: Vec<String> = production
            .rows
            .iter()
            .map(|row| match row {
                DashboardRow::Object(key) => key.full_name(),
                DashboardRow::Group { label, .. } => label.clone(),
            })
            .collect();
        for expected in [
            "db-prod-03!postgres-replication",
            "mq-prod-01!rabbitmq-queue",
            "k8s-node-07!disk /var",
            "web-edge-02!http-tls",
            "cache-02!redis-memory",
            "vpn-gw-01!cert-expiry",
        ] {
            assert!(rows.iter().any(|row| row == expected), "{expected} missing");
        }
        // Unhandled criticals first (newest first among equals), handled last.
        assert_eq!(rows[0], "mq-prod-01!rabbitmq-queue");
        assert_eq!(rows[1], "db-prod-03!postgres-replication");
        let summary = production.summary;
        assert_eq!(
            summary.unhandled + summary.handled,
            u32::try_from(rows.len()).unwrap()
        );
    }

    #[test]
    fn hide_handled_drops_handled_rows_but_not_their_counts() {
        let demo = demo();
        let overview = result(&demo, "overview", "overview");
        assert_eq!(
            u32::try_from(overview.rows.len()).unwrap(),
            overview.summary.unhandled
        );
        assert!(overview.summary.handled > 0);
        assert!(!overview.rows.iter().any(|row| matches!(
            row,
            DashboardRow::Object(key) if key.full_name() == "web-edge-02!http-tls"
        )));
    }

    #[test]
    fn databases_are_grouped_by_host_worst_first() {
        let demo = demo();
        let rows = &result(&demo, "overview", "databases").rows;
        let headers: Vec<_> = rows
            .iter()
            .filter_map(|row| match row {
                DashboardRow::Group { label, count } => Some((label.as_str(), *count)),
                DashboardRow::Object(_) => None,
            })
            .collect();
        assert_eq!(
            headers,
            [("db-prod-03", 2), ("db-prod-01", 1), ("cache-02", 1)]
        );
    }

    #[test]
    fn db_prod_03_has_the_host_panes_23_services() {
        let demo = demo();
        let host = HostName::new("db-prod-03");
        assert_eq!(demo.snapshot.services_of(&host).count(), 23);
        let problems = demo
            .snapshot
            .services_of(&host)
            .filter(|service| service.is_problem())
            .count();
        assert_eq!(problems, 2);
    }

    #[test]
    fn the_replication_check_has_the_designs_details() {
        let demo = demo();
        let key = ServiceKey::new("db-prod-03", "postgres-replication");
        let service = &demo.snapshot.services[&key];
        let result = service.check.result.as_ref().unwrap();
        assert_eq!(result.output, "CRITICAL - standby lag 412s (> 300s)");
        assert_eq!(result.long_output.lines().count(), 3);
        assert_eq!(result.perfdata.len(), 3);
        assert_eq!(
            service.check.command_endpoint.as_deref(),
            Some("sat-ams-01")
        );
        assert_eq!((service.check.attempt, service.check.max_attempts), (3, 3));
        let comments = &demo.snapshot.comments[&service.object_key()];
        assert_eq!(comments[0].author, "j.berg");
    }

    #[test]
    fn sorting_honours_the_key_and_direction() {
        let now = Timestamp::from_unix_seconds(1000.);
        let make = |host: &str, severity: u32, age: f64| Match {
            key: ObjectKey::host(host),
            host_name: HostName::new(host),
            service_name: None,
            state: CheckableState::Host(HostState::Down),
            problem: true,
            handled: false,
            severity,
            last_state_change: ago(now, age),
        };
        let a = make("a", 10, 50.);
        let b = make("b", 20, 10.);
        let by_severity = Sort {
            key: SortKey::Severity,
            descending: true,
        };
        assert_eq!(compare(&a, &b, by_severity), Ordering::Greater);
        let by_host = Sort {
            key: SortKey::Host,
            descending: false,
        };
        assert_eq!(compare(&a, &b, by_host), Ordering::Less);
        let by_recency = Sort {
            key: SortKey::LastStateChange,
            descending: true,
        };
        assert_eq!(
            compare(&a, &b, by_recency),
            Ordering::Greater,
            "b changed later"
        );
        let same = make("a", 10, 50.);
        assert_eq!(compare(&a, &same, by_severity), Ordering::Equal);
    }

    #[test]
    fn filters_render_as_icinga_expressions() {
        assert_eq!(DemoFilter::All.expression(), "");
        assert_eq!(
            DemoFilter::Roles(&["switch", "edge"]).expression(),
            "host.vars.role in [\"switch\", \"edge\"]"
        );
        assert_eq!(
            DemoFilter::Env("prod").expression(),
            "host.vars.env == \"prod\""
        );
    }
}
