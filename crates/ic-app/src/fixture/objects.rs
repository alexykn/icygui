//! The design's `prod-cluster` sample objects: the problem rows of screen
//! 2a, db-prod-03's 23 services from screen 2c, the replication check of 2b
//! with its perfdata, comment and links, plus the OK services a real cluster
//! has around them.

use std::collections::BTreeMap;
use std::time::Duration;

use ic_model::{
    AckKind, CheckInfo, CheckResult, Comment, CommentKind, Dependency, Downtime, Endpoint, Host,
    HostGroup, HostState, InstanceStatus, Links, ObjectKey, Service, ServiceGroup, ServiceKey,
    ServiceState, StateType, Timestamp, parse_perfdata,
};
use serde_json::json;

use super::ENDPOINT;

pub(super) const MINUTE: f64 = 60.;
pub(super) const HOUR: f64 = 60. * MINUTE;
pub(super) const DAY: f64 = 24. * HOUR;

pub(super) fn ago(now: Timestamp, seconds: f64) -> Timestamp {
    Timestamp::from_unix_seconds(now.as_unix_seconds() - seconds)
}

/// Check details for an object that has been in its state for `since`
/// seconds, checked `checked` seconds ago.
pub(super) fn check_info(now: Timestamp, since: f64, checked: f64, interval: f64) -> CheckInfo {
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

pub(super) fn check_result(
    now: Timestamp,
    raw_output: &str,
    exit_status: i32,
    checked: f64,
) -> CheckResult {
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

#[derive(Clone, Copy)]
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
        output: "",
    }
}

const fn down(spec: HostSpec, state: HostState, since: f64) -> HostSpec {
    HostSpec {
        state,
        since,
        output: "PING CRITICAL - Packet loss = 100%",
        ..spec
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
    down(
        up("k8s-node-11", "10.0.4.31", "k8s"),
        HostState::Down,
        12. * MINUTE,
    ),
    up("web-edge-01", "10.0.1.11", "edge"),
    up("web-edge-02", "10.0.1.12", "edge"),
    up("lb-prod-02", "10.0.1.32", "lb"),
    up("api-gw-01", "10.0.1.41", "api"),
    up("backup-01", "10.0.5.11", "backup"),
    up("vpn-gw-01", "10.0.0.5", "vpn"),
    down(
        up("sw-core-ams-02", "10.8.0.2", "switch"),
        HostState::Down,
        11. * MINUTE,
    ),
    down(
        up("edge-ams-01", "10.8.1.21", "edge"),
        HostState::Unreachable,
        11. * MINUTE,
    ),
    down(
        up("edge-ams-02", "10.8.1.22", "edge"),
        HostState::Unreachable,
        11. * MINUTE,
    ),
    down(
        up("edge-ams-03", "10.8.1.23", "edge"),
        HostState::Unreachable,
        11. * MINUTE,
    ),
    down(
        up("edge-ams-04", "10.8.1.24", "edge"),
        HostState::Unreachable,
        11. * MINUTE,
    ),
    down(
        up("edge-ams-05", "10.8.1.25", "edge"),
        HostState::Unreachable,
        11. * MINUTE,
    ),
    down(
        up("edge-fra-04", "185.22.10.4", "edge"),
        HostState::Down,
        HOUR + 8. * MINUTE,
    ),
    down(
        up("store-ams-02", "10.8.1.12", "storage"),
        HostState::Down,
        3. * MINUTE,
    ),
];

pub(super) fn hosts(now: Timestamp) -> Vec<Host> {
    HOSTS
        .iter()
        .enumerate()
        .map(|(index, spec)| {
            let mut host = Host::new(spec.name);
            spec.address.clone_into(&mut host.address);
            host.state = spec.state;
            host.check = check_info(now, spec.since, 31., 60.);
            "hostalive".clone_into(&mut host.check.check_command);
            let exit_status = i32::from(spec.state != HostState::Up) * 2;
            let output = if spec.output.is_empty() {
                // Round-trip times between 0.31 and 0.9 ms; db-prod-03's
                // is the design's.
                let rta = if spec.name == "db-prod-03" {
                    0.42
                } else {
                    0.31 + f64::from(u8::try_from(index * 7 % 60).unwrap_or(0)) / 100.
                };
                format!("PING OK rta {rta:.2}ms")
            } else {
                spec.output.to_owned()
            };
            let mut result = check_result(now, &output, exit_status, 31.);
            result.perfdata = if spec.state == HostState::Up {
                parse_perfdata(&format!(
                    "rta={}ms;100;200;0 pl=0%;20;60;0",
                    output
                        .trim_start_matches("PING OK rta ")
                        .trim_end_matches("ms")
                ))
            } else {
                parse_perfdata("rta=0ms;100;200;0 pl=100%;20;60;0")
            };
            host.check.result = Some(result);
            if spec.state.is_problem() {
                host.check.attempt = host.check.max_attempts;
            }
            host.check.reachable = spec.state != HostState::Unreachable;
            host.vars.insert("env".to_owned(), json!("prod"));
            host.vars.insert("role".to_owned(), json!(spec.role));
            host.vars.insert("os".to_owned(), json!("linux"));
            host.groups = host_groups_for(spec.role);
            customize_host(&mut host);
            host
        })
        .collect()
}

fn customize_host(host: &mut Host) {
    match host.name.as_str() {
        "sw-core-ams-02" => {
            host.check.acknowledgement = AckKind::Sticky;
            host.vars.insert("os".to_owned(), json!("junos"));
        }
        "edge-fra-04" => host.check.downtime_depth = 1,
        "db-prod-03" => {
            host.vars.insert(
                "disks".to_owned(),
                json!({
                    "/": { "warn": "80%", "crit": "90%" },
                    "/var/lib/postgresql": { "warn": "85%", "crit": "95%" },
                }),
            );
            host.vars.insert(
                "postgres".to_owned(),
                json!({
                    "cluster": "orders-main",
                    "version": 16,
                    "standby_of": "db-prod-01",
                    "replication_slots": ["repl_db03"],
                }),
            );
            host.vars.insert("rack".to_owned(), json!("ams3-r12"));
            host.links = Links {
                notes: "Orders cluster standby. Fails over with db-prod-01.".to_owned(),
                notes_url: "https://wiki.example.com/db/orders-main".to_owned(),
                action_url: String::new(),
                icon_image: String::new(),
            };
            "2001:db8:2::13".clone_into(&mut host.address6);
        }
        _ => {}
    }
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
/// `(name, output, perfdata)`.
const COMMON: &[(&str, &str, &str)] = &[
    ("disk /", "DISK OK - 41% used", "/=16.4GiB;32;36;0;40"),
    (
        "load",
        "OK - load average 3.1, 2.8, 2.6",
        "load1=3.1;8;12;0 load5=2.8;6;10;0 load15=2.6;4;8;0",
    ),
    ("memory", "OK - 71% used", "used=71%;85;95;0;100"),
    ("ntp-offset", "OK - offset 0.004s", "offset=0.004s;0.5;1"),
    (
        "ssh",
        "SSH OK - OpenSSH_9.6p1 (protocol 2.0)",
        "time=0.012s;;;0;10",
    ),
];

/// OK services by host role, so host panes look like real hosts.
const ROLE_SERVICES: &[(&str, &[(&str, &str)])] = &[
    (
        "postgres",
        &[
            (
                "postgres",
                "POSTGRES OK - version 16.4, accepting connections",
            ),
            ("pg-connections", "OK - 42 of 200 per-db limit (orders)"),
        ],
    ),
    (
        "redis",
        &[
            ("redis", "REDIS OK - 1.2M keys, 0 rejected connections"),
            ("redis-memory", "OK - used_memory 54% of maxmemory"),
        ],
    ),
    (
        "rabbitmq",
        &[
            (
                "rabbitmq-node",
                "RABBITMQ OK - node running, 3 of 3 cluster members",
            ),
            ("rabbitmq-queue", "OK - all queues below 1,000 messages"),
        ],
    ),
    (
        "k8s",
        &[
            ("kubelet", "OK - kubelet healthy"),
            ("containerd", "OK - containerd running, 38 containers"),
            ("kube-proxy", "OK - kube-proxy healthy"),
        ],
    ),
    (
        "edge",
        &[
            ("nginx-workers", "OK - 16 of 16 workers running"),
            ("http", "HTTP OK - 200 in 0.008s"),
            ("cert-expiry", "OK - certificate expires in 74 days"),
        ],
    ),
    (
        "lb",
        &[
            ("haproxy-frontend", "HAPROXY OK - 3 of 3 frontends up"),
            ("haproxy-backend", "OK - all backends up"),
        ],
    ),
    (
        "api",
        &[
            ("http-health", "HTTP OK - 200 in 0.012s"),
            ("http-latency", "OK - p95 0.31s"),
        ],
    ),
    (
        "backup",
        &[("borg-last-run", "OK - last run 6h ago, 41 GiB")],
    ),
    (
        "vpn",
        &[
            ("openvpn", "OK - 41 clients connected"),
            ("cert-expiry", "OK - certificate expires in 140 days"),
        ],
    ),
    (
        "switch",
        &[
            ("snmp-interfaces", "OK - 48 of 48 interfaces up"),
            ("snmp-uptime", "OK - uptime 212 days"),
        ],
    ),
    (
        "storage",
        &[("zfs-pool", "OK - pool tank ONLINE, 61% used")],
    ),
];

/// The rest of db-prod-03's 23 services (the host pane's "+ 16 more ok").
const DB_PROD_03_EXTRA: &[&str] = &[
    "packages",
    "pg-autovacuum",
    "pg-backup",
    "pg-bloat",
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

/// db-prod-03's OK services with the ages screen 2c shows.
const DB_PROD_03_AGES: &[(&str, f64)] = &[
    ("disk /", 6. * DAY),
    ("load", 2. * DAY),
    ("memory", 9. * DAY),
    ("ntp-offset", 41. * DAY),
];

pub(super) fn services(now: Timestamp, hosts: &[Host]) -> Vec<Service> {
    let mut services: BTreeMap<ServiceKey, Service> = BTreeMap::new();
    let mut add = |service: Service| {
        services.insert(service.key.clone(), service);
    };
    for (index, host) in hosts.iter().enumerate() {
        // Spread the generic services' ages so the lists aren't uniform.
        let age = DAY * (2. + f64::from(u8::try_from(index % 9).unwrap_or(0)));
        for (name, output, perfdata) in COMMON {
            let mut common = service(now, host, name, ServiceState::Ok, age, output);
            if let Some(result) = common.check.result.as_mut() {
                result.perfdata = parse_perfdata(perfdata);
            }
            add(common);
        }
        let role = host
            .vars
            .get("role")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let role_services = ROLE_SERVICES
            .iter()
            .filter(|(name, _)| *name == role)
            .flat_map(|(_, services)| services.iter());
        for (name, output) in role_services {
            add(service(now, host, name, ServiceState::Ok, age / 2., output));
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

pub(super) fn service(
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
        ("db-prod-03", "postgres-replication") => customize_replication(now, service),
        ("db-prod-03", name) => {
            if let Some((_, age)) = DB_PROD_03_AGES.iter().find(|(known, _)| *known == name) {
                service.check.last_state_change = ago(now, *age);
                service.check.last_hard_state_change = ago(now, *age);
            }
            if name == "pg-connections" {
                service.groups = vec!["databases".to_owned()];
                if let Some(result) = service.check.result.as_mut() {
                    result.perfdata = parse_perfdata("connections=182;160;190;0;200");
                }
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
            service.check.flapping_current = 18.4;
            if let Some(result) = service.check.result.as_mut() {
                result.perfdata = parse_perfdata("p95=1.84s;1.5;3;0 p50=0.21s;;;0");
            }
        }
        ("mq-prod-01", "rabbitmq-queue") => {
            if let Some(result) = service.check.result.as_mut() {
                result.perfdata = parse_perfdata("'orders.retry'=18402;1000;10000;0");
            }
        }
        ("k8s-node-03", "kube-proxy") => {
            // Flapping between OK and warning all morning.
            service.check.flapping = true;
            service.check.flapping_current = 42.5;
            service.check.features.flap_detection = true;
        }
        _ => {}
    }
    if service.key.name.contains("cert") || &*service.key.name == "http-tls" {
        service.groups.push("certificates".to_owned());
    }
}

fn customize_replication(now: Timestamp, service: &mut Service) {
    "check_postgres".clone_into(&mut service.check.check_command);
    service.check.check_interval = 60.;
    service.check.retry_interval = 15.;
    service.check.command_endpoint = Some("sat-ams-01".to_owned());
    service.check.zone = Some("ams".to_owned());
    service.check.flapping_current = 4.2;
    service.check.features.flap_detection = true;
    service.groups = vec!["databases".to_owned(), "replication".to_owned()];
    service
        .vars
        .insert("pg_cluster".to_owned(), json!("orders-main"));
    service.vars.insert("lag_crit".to_owned(), json!(300));
    service.vars.insert("lag_warn".to_owned(), json!(60));
    service
        .vars
        .insert("runbook".to_owned(), json!("wiki/db/replication-lag"));
    service.links = Links {
        notes: "Streaming replication from db-prod-01. Lag above 300s means the standby can't take over without data loss.".to_owned(),
        notes_url: "https://wiki.example.com/db/replication-lag".to_owned(),
        // Icinga Web's syntax for several URLs, with macros as real
        // configs write them.
        action_url: "'https://grafana.example.com/d/pg-replication?var-host=$HOSTNAME$' \
                     'https://grafana.example.com/d/pg-cluster?var-cluster=$service.vars.pg_cluster$&var-host=$host.name$'"
            .to_owned(),
        icon_image: String::new(),
    };
    if let Some(result) = service.check.result.as_mut() {
        result.perfdata = parse_perfdata(
            "replication_lag=412s;60;300;0;3600 wal_retained=1.8GiB;4;8 active_connections=182;400;450;0",
        );
        "sat-ams-01".clone_into(&mut result.check_source);
        result.schedule_start = ago(now, 13.83);
        result.execution_start = ago(now, 13.82);
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

pub(super) fn comments(now: Timestamp) -> BTreeMap<ObjectKey, Vec<Comment>> {
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

pub(super) fn downtimes(now: Timestamp) -> BTreeMap<ObjectKey, Vec<Downtime>> {
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

pub(super) fn host_groups() -> Vec<HostGroup> {
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

pub(super) fn service_groups() -> Vec<ServiceGroup> {
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

pub(super) fn dependencies() -> Vec<Dependency> {
    (1..=5)
        .map(|index| {
            let child = format!("edge-ams-0{index}");
            Dependency {
                name: format!("{child}!uplink"),
                child: ObjectKey::host(&child),
                parent: ObjectKey::host("sw-core-ams-02"),
            }
        })
        .chain(std::iter::once(Dependency {
            name: "db-prod-03!primary".to_owned(),
            child: ObjectKey::host("db-prod-03"),
            parent: ObjectKey::host("db-prod-01"),
        }))
        .collect()
}

pub(super) fn endpoints() -> Vec<Endpoint> {
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

pub(super) fn status(now: Timestamp) -> InstanceStatus {
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
        counts: ic_model::ObjectCounts::default(),
    }
}
