//! `prod-cluster`: the design's sample data (`design/project/Icinga Client
//! v2.dc.html` and `Icinga Client.dc.html`) inside a realistic production
//! estate of about 150 hosts and 1400 services.

use std::time::Duration;

use ic_model::{ObjectKey, ServiceState};
use serde_json::json;

use super::build::{Builder, ScenarioDowntime, vars};
use super::{Scenario, User, Zone};

fn mins(minutes: u64) -> Duration {
    Duration::from_secs(minutes * 60)
}

fn hours(hours: u64) -> Duration {
    Duration::from_secs(hours * 3_600)
}

/// The standard services every Linux host gets.
const STANDARD: &[(&str, &str)] = &[
    ("ping4", "ping4"),
    ("ssh", "ssh"),
    ("load", "load"),
    ("disk /", "disk"),
    ("memory", "mem"),
    ("ntp-offset", "ntp_time"),
    ("apt", "apt"),
];

/// Role-specific services: (service name, check command, service groups).
#[expect(clippy::too_many_lines, reason = "a data table of services per role")]
fn role_services(role: &str) -> Vec<(&'static str, &'static str, &'static [&'static str])> {
    match role {
        "web-edge" => vec![
            ("http", "http", &["http"]),
            ("http-tls", "http", &["http", "tls-certificates"]),
            ("nginx-workers", "procs", &["http"]),
            ("cert-expiry", "ssl_cert", &["tls-certificates"]),
        ],
        "web" => vec![
            ("http", "http", &["http"]),
            ("nginx-workers", "procs", &["http"]),
            ("php-fpm", "procs", &["http"]),
        ],
        "api" => vec![
            ("http", "http", &["http"]),
            ("http-latency", "http", &["http"]),
            ("api-health", "http", &["http"]),
            ("jvm-heap", "jmx", &[]),
        ],
        "api-gateway" => vec![
            ("http", "http", &["http"]),
            ("http-latency", "http", &["http"]),
            ("http-tls", "http", &["http", "tls-certificates"]),
        ],
        "postgres" => vec![
            (
                "postgres-replication",
                "check_postgres",
                &["databases", "replication"],
            ),
            ("pg-connections", "check_postgres", &["databases"]),
            ("disk /var/lib/postgresql", "disk", &["databases"]),
            ("pg-locks", "check_postgres", &["databases"]),
            ("pg-bloat", "check_postgres", &["databases"]),
            ("pg-backup-age", "check_postgres", &["databases", "backups"]),
            (
                "pg-wal-archive",
                "check_postgres",
                &["databases", "replication"],
            ),
            ("pgbouncer", "procs", &["databases"]),
            ("pg-cache-hit", "check_postgres", &["databases"]),
            ("pg-xid-age", "check_postgres", &["databases"]),
            ("pg-autovacuum", "check_postgres", &["databases"]),
            ("pg-checkpoints", "check_postgres", &["databases"]),
            ("pg-deadlocks", "check_postgres", &["databases"]),
            ("pg-slow-queries", "check_postgres", &["databases"]),
            ("pg-sequences", "check_postgres", &["databases"]),
            (
                "pg-replication-slots",
                "check_postgres",
                &["databases", "replication"],
            ),
        ],
        "mongodb" => vec![
            ("mongodb-replset", "mongodb", &["databases", "replication"]),
            ("mongodb-connections", "mongodb", &["databases"]),
            ("disk /var/lib/mongodb", "disk", &["databases"]),
        ],
        "mysql" => vec![
            ("mysql-replication", "mysql", &["databases", "replication"]),
            ("mysql-connections", "mysql", &["databases"]),
            ("disk /var/lib/mysql", "disk", &["databases"]),
        ],
        "k8s-node" => vec![
            ("kubelet", "kubelet", &["kubernetes"]),
            ("containerd", "procs", &["kubernetes"]),
            ("disk /var", "disk", &["kubernetes"]),
            ("k8s-pods", "kubernetes", &["kubernetes"]),
        ],
        "k8s-control-plane" => vec![
            ("kube-apiserver", "http", &["kubernetes"]),
            ("etcd", "etcd", &["kubernetes"]),
            ("kube-scheduler", "kubernetes", &["kubernetes"]),
            ("kube-controller-manager", "kubernetes", &["kubernetes"]),
        ],
        "rabbitmq" => vec![
            ("rabbitmq-queue", "rabbitmq", &["queue"]),
            ("rabbitmq-cluster", "rabbitmq", &["queue"]),
            ("rabbitmq-memory", "rabbitmq", &["queue"]),
        ],
        "kafka" => vec![
            ("kafka-broker", "kafka", &["queue"]),
            ("kafka-isr", "kafka", &["queue"]),
            ("kafka-consumer-lag", "kafka", &["queue"]),
        ],
        "redis" => vec![
            ("redis-memory", "redis", &["cache"]),
            ("redis-replication", "redis", &["cache", "replication"]),
            ("redis-connections", "redis", &["cache"]),
        ],
        "haproxy" => vec![
            ("haproxy-backend", "haproxy", &["loadbalancing"]),
            ("haproxy-frontend", "haproxy", &["loadbalancing"]),
            ("keepalived", "procs", &["loadbalancing"]),
        ],
        "vpn" => vec![
            ("cert-expiry", "ssl_cert", &["tls-certificates"]),
            ("wireguard-peers", "wireguard", &[]),
            ("openvpn", "procs", &[]),
        ],
        "storage" => vec![
            ("zfs-pool", "zfs", &["storage"]),
            ("smart-disks", "smart", &["storage"]),
            ("disk /srv", "disk", &["storage"]),
        ],
        "nfs" => vec![
            ("nfs-exports", "nfs", &["storage"]),
            ("disk /srv", "disk", &["storage"]),
        ],
        "minio" => vec![
            ("minio-health", "http", &["storage"]),
            ("disk /data", "disk", &["storage"]),
        ],
        "backup" => vec![
            ("borg-last-run", "borg", &["backups"]),
            ("backup-disk", "disk", &["backups"]),
            ("restic-check", "restic", &["backups"]),
        ],
        "edge" => vec![
            ("http", "http", &["http"]),
            ("http-tls", "http", &["http", "tls-certificates"]),
            ("cert-expiry", "ssl_cert", &["tls-certificates"]),
            ("varnish", "procs", &["http"]),
        ],
        "monitoring" => vec![
            ("icinga", "icinga", &["monitoring"]),
            ("cluster-zone", "cluster-zone", &["monitoring"]),
        ],
        _ => Vec::new(),
    }
}

struct HostDef {
    name: String,
    address: String,
    role: &'static str,
    groups: Vec<&'static str>,
    zone: Option<&'static str>,
    extra_vars: Vec<(&'static str, serde_json::Value)>,
    linux: bool,
}

fn host(name: &str, address: &str, role: &'static str, groups: &[&'static str]) -> HostDef {
    HostDef {
        name: name.to_owned(),
        address: address.to_owned(),
        role,
        groups: groups.to_vec(),
        zone: None,
        extra_vars: Vec::new(),
        linux: true,
    }
}

#[expect(clippy::too_many_lines, reason = "a data table of the estate")]
fn estate() -> Vec<HostDef> {
    let mut hosts = Vec::new();
    for i in 1..=4 {
        hosts.push(host(
            &format!("web-edge-{i:02}"),
            &format!("10.0.1.{}", 10 + i),
            "web-edge",
            &["web-frontend"],
        ));
    }
    for i in 1..=20 {
        hosts.push(host(
            &format!("web-prod-{i:02}"),
            &format!("10.0.1.{}", 20 + i),
            "web",
            &["web-frontend"],
        ));
    }
    for i in 1..=2 {
        hosts.push(host(
            &format!("api-gw-{i:02}"),
            &format!("10.0.3.{}", 10 + i),
            "api-gateway",
            &["api"],
        ));
    }
    for i in 1..=16 {
        hosts.push(host(
            &format!("api-prod-{i:02}"),
            &format!("10.0.3.{}", 20 + i),
            "api",
            &["api"],
        ));
    }
    for i in 1..=6 {
        let mut def = host(
            &format!("db-prod-{i:02}"),
            &format!("10.0.2.{}", 10 + i),
            "postgres",
            &["databases"],
        );
        let cluster = if i <= 3 { "orders-main" } else { "billing" };
        def.extra_vars.push(("pg_cluster", json!(cluster)));
        def.extra_vars.push(("lag_crit", json!(300)));
        def.extra_vars
            .push(("runbook", json!("wiki/db/replication-lag")));
        if i == 3 {
            def.zone = Some("ams");
        }
        hosts.push(def);
    }
    for i in 1..=3 {
        hosts.push(host(
            &format!("db-mongo-{i:02}"),
            &format!("10.0.2.{}", 30 + i),
            "mongodb",
            &["databases"],
        ));
    }
    for i in 1..=3 {
        hosts.push(host(
            &format!("db-mysql-{i:02}"),
            &format!("10.0.2.{}", 40 + i),
            "mysql",
            &["databases"],
        ));
    }
    for i in 1..=37 {
        hosts.push(host(
            &format!("k8s-node-{i:02}"),
            &format!("10.0.4.{}", 20 + i),
            "k8s-node",
            &["kubernetes"],
        ));
    }
    for i in 1..=3 {
        hosts.push(host(
            &format!("k8s-cp-{i:02}"),
            &format!("10.0.4.{}", 10 + i),
            "k8s-control-plane",
            &["kubernetes"],
        ));
    }
    for i in 1..=5 {
        hosts.push(host(
            &format!("mq-prod-{i:02}"),
            &format!("10.0.5.{}", 10 + i),
            "rabbitmq",
            &["queue"],
        ));
    }
    for i in 1..=3 {
        hosts.push(host(
            &format!("kafka-{i:02}"),
            &format!("10.0.5.{}", 30 + i),
            "kafka",
            &["queue"],
        ));
    }
    for (prefix, net, zone) in [
        ("store-ams", "10.8.1", Some("ams")),
        ("store-fra", "10.9.1", None),
    ] {
        for i in 1..=4 {
            let mut def = host(
                &format!("{prefix}-{i:02}"),
                &format!("{net}.{}", 10 + i),
                "storage",
                &["storage"],
            );
            def.zone = zone;
            hosts.push(def);
        }
    }
    for i in 1..=2 {
        hosts.push(host(
            &format!("nfs-{i:02}"),
            &format!("10.0.6.{}", 10 + i),
            "nfs",
            &["storage"],
        ));
    }
    for i in 1..=4 {
        hosts.push(host(
            &format!("minio-{i:02}"),
            &format!("10.0.6.{}", 20 + i),
            "minio",
            &["storage"],
        ));
    }
    for i in 1..=2 {
        hosts.push(host(
            &format!("backup-{i:02}"),
            &format!("10.0.6.{}", 40 + i),
            "backup",
            &["storage"],
        ));
    }
    for i in 1..=10 {
        hosts.push(host(
            &format!("edge-fra-{i:02}"),
            &format!("185.22.10.{i}"),
            "edge",
            &["edge-fra"],
        ));
    }
    for i in 1..=12 {
        let mut def = host(
            &format!("edge-ams-{i:02}"),
            &format!("10.8.2.{}", 10 + i),
            "edge",
            &["edge-ams"],
        );
        def.zone = Some("ams");
        hosts.push(def);
    }
    for i in 1..=4 {
        hosts.push(host(
            &format!("cache-{i:02}"),
            &format!("10.0.7.{}", 10 + i),
            "redis",
            &["cache"],
        ));
    }
    for i in 1..=2 {
        hosts.push(host(
            &format!("lb-prod-{i:02}"),
            &format!("10.0.0.{}", 10 + i),
            "haproxy",
            &["loadbalancers"],
        ));
    }
    for i in 1..=2 {
        hosts.push(host(
            &format!("vpn-gw-{i:02}"),
            &format!("10.0.0.{}", 20 + i),
            "vpn",
            &["vpn"],
        ));
    }
    for (name, address, zone) in [
        ("sw-core-ams-01", "10.8.0.2", Some("ams")),
        ("sw-core-ams-02", "10.8.0.3", Some("ams")),
        ("sw-core-fra-01", "10.9.0.2", None),
        ("sw-core-fra-02", "10.9.0.3", None),
    ] {
        let mut def = host(name, address, "switch", &["network"]);
        def.zone = zone;
        def.linux = false;
        hosts.push(def);
    }
    hosts.push(host("master-01", "10.0.0.5", "monitoring", &["monitoring"]));
    let mut satellite = host("sat-ams-01", "10.8.0.5", "monitoring", &["monitoring"]);
    satellite.zone = Some("ams");
    hosts.push(satellite);
    hosts
}

/// Builds the `prod-cluster` scenario.
#[must_use]
#[expect(
    clippy::too_many_lines,
    reason = "the design's sample data is listed object by object"
)]
pub fn prod_cluster() -> Scenario {
    let mut b = Builder::new("prod-cluster", "master-01", "r2.14.3-1", 2_026);
    b.scenario.zones = vec![
        Zone::new("master", None),
        Zone::new("ams", Some("master")),
        Zone {
            name: "global-templates".to_owned(),
            parent: None,
            global: true,
        },
    ];
    b.endpoint("master-01", "master", false);
    b.endpoint("sat-ams-01", "ams", true);
    b.scenario.users = vec![
        User::new("dba-oncall", "DBA on-call", "dba-oncall@example.com"),
        User::new(
            "infra-oncall",
            "Infrastructure on-call",
            "infra-oncall@example.com",
        ),
        User::new(
            "infra-lead",
            "Infrastructure lead",
            "infra-lead@example.com",
        ),
        User::new("m.keller", "Mara Keller", "m.keller@example.com"),
        User::new("j.berg", "Jonas Berg", "j.berg@example.com"),
        User::new("a.ivanova", "Anna Ivanova", "a.ivanova@example.com"),
    ];

    for def in estate() {
        b.zone = def.zone.map(str::to_owned);
        b.check_source = def.zone.map(|_| "sat-ams-01".to_owned());
        let mut host_vars = vec![
            ("role", json!(def.role)),
            ("env", json!("prod")),
            ("os", json!(if def.linux { "Linux" } else { "IOS-XE" })),
        ];
        host_vars.extend(def.extra_vars.iter().cloned());
        let mut groups = def.groups.clone();
        if def.linux {
            groups.push("linux-servers");
        }
        b.host(&def.name, &def.address, &groups, vars(&host_vars));
        let service_vars = vars(&[("role", json!(def.role)), ("env", json!("prod"))]);
        if def.linux {
            for (name, command) in STANDARD {
                b.service(&def.name, name, command, &[], service_vars.clone());
            }
        } else {
            for (name, command) in [
                ("ping4", "ping4"),
                ("snmp-uptime", "snmp"),
                ("cpu", "snmp"),
                ("temperature", "snmp"),
                ("psu", "snmp"),
            ] {
                b.service(&def.name, name, command, &["network"], service_vars.clone());
            }
            let uplink = if def.name.contains("ams") {
                "uplink edge-ams"
            } else {
                "uplink edge-fra"
            };
            b.service(
                &def.name,
                uplink,
                "snmp-interface",
                &["network"],
                service_vars.clone(),
            );
        }
        for (name, command, groups) in role_services(def.role) {
            b.service(&def.name, name, command, groups, service_vars.clone());
        }
    }
    b.zone = None;
    b.check_source = None;

    // --- The design's problems ---------------------------------------------
    let db03 = "db-prod-03";
    b.check_source = Some("sat-ams-01".to_owned());
    b.problem(
        db03,
        "postgres-replication",
        ServiceState::Critical,
        "CRITICAL - standby lag 412s (> 300s)\n\
         primary  db-prod-01  lsn 4A/9C21F0D8\n\
         standby  db-prod-03  lsn 4A/2E77A120\n\
         slot     repl_db03   retained 1.8 GiB",
        &[
            "replication_lag=412s;60;300",
            "wal_retained=1.8GiB;4;8",
            "active_connections=182;400;450",
        ],
        mins(14),
        None,
    );
    let now = b.ago(Duration::ZERO).as_unix_seconds();
    if let Some(service) = b
        .scenario
        .services
        .iter_mut()
        .find(|s| s.key.host.as_str() == db03 && &*s.key.name == "postgres-replication")
    {
        service.check.check_interval = 60.0;
        service.check.retry_interval = 15.0;
        service.check.flapping_current = 4.2;
        service.vars = vars(&[
            ("role", json!("postgres")),
            ("env", json!("prod")),
            ("pg_cluster", json!("orders-main")),
            ("lag_crit", json!(300)),
            ("runbook", json!("wiki/db/replication-lag")),
        ]);
        "https://wiki.example.com/db/replication-lag".clone_into(&mut service.links.notes_url);
        if let Some(result) = &mut service.check.result {
            result.execution_end = ic_model::Timestamp::from_unix_seconds(now - 12.0);
            result.execution_start = ic_model::Timestamp::from_unix_seconds(now - 13.82);
            result.schedule_start = ic_model::Timestamp::from_unix_seconds(now - 13.824);
        }
        service.check.last_check = Some(ic_model::Timestamp::from_unix_seconds(now - 12.0));
        service.check.next_check = Some(ic_model::Timestamp::from_unix_seconds(now + 48.0));
    }
    b.problem(
        db03,
        "pg-connections",
        ServiceState::Warning,
        "WARNING - 182 of 200 per-db limit (orders)",
        &["connections=182;180;195;0;200"],
        mins(22),
        None,
    );
    b.comment(
        ObjectKey::service(db03, "postgres-replication"),
        "j.berg",
        "Failover drill on db-prod-01 at 15:00. Expect lag on 03.",
        mins(48),
        Some(mins(74)),
    );
    b.check_source = None;
    b.problem(
        "mq-prod-01",
        "rabbitmq-queue",
        ServiceState::Critical,
        "CRITICAL - queue orders.retry depth 18,402",
        &["depth=18402;5000;10000;0"],
        mins(6),
        None,
    );
    b.problem(
        "k8s-node-07",
        "disk /var",
        ServiceState::Critical,
        "DISK CRITICAL - /var 97% used (1.2 GiB free)",
        &["'/var'=38.8GiB;32;36;0;40"],
        mins(38),
        None,
    );
    b.problem(
        "k8s-node-07",
        "kubelet",
        ServiceState::Critical,
        "CRITICAL - connection refused (10.0.4.27:10250)",
        &[],
        mins(41),
        None,
    );
    b.problem(
        "web-edge-02",
        "http-tls",
        ServiceState::Critical,
        "SSL CRITICAL - certificate expires in 2 days",
        &["days_left=2;14;3;0"],
        hours(2) + mins(4),
        None,
    );
    b.acknowledge_until(
        &ObjectKey::service("web-edge-02", "http-tls"),
        "m.keller",
        "renewal ordered, new cert expected tomorrow (CHG-4459)",
        hours(2) + mins(10),
        true,
        Some(hours(21) + mins(48)),
    );
    b.problem(
        "lb-prod-02",
        "haproxy-backend",
        ServiceState::Warning,
        "WARNING - backend api: 2/6 servers down",
        &["servers_up=4;5;2;0;6"],
        mins(9),
        None,
    );
    b.problem(
        "api-gw-01",
        "http-latency",
        ServiceState::Warning,
        "WARNING - p95 1.84s (> 1.5s)",
        &["p95=1.84s;1.5;3;0", "p99=2.61s;;;0"],
        mins(21),
        None,
    );
    b.problem(
        "db-prod-01",
        "load",
        ServiceState::Warning,
        "WARNING - load average 14.2, 12.8, 11.1",
        &[
            "load1=14.200;8.000;16.000;0;",
            "load5=12.800;8.000;16.000;0;",
            "load15=11.100;8.000;16.000;0;",
        ],
        hours(1) + mins(2),
        None,
    );
    b.problem(
        "cache-02",
        "redis-memory",
        ServiceState::Warning,
        "WARNING - used_memory 81% of maxmemory",
        &["used_memory_pct=81%;80;90;0;100"],
        hours(3),
        None,
    );
    let start = b.ago(hours(2));
    let end = b.later(hours(1));
    b.downtime(
        ObjectKey::service("cache-02", "redis-memory"),
        "j.berg",
        "maxmemory raised; restart once the memory upgrade on cache-02 is done",
        start,
        end,
        true,
        0.0,
        None,
        false,
    );
    b.problem(
        "backup-01",
        "borg-last-run",
        ServiceState::Unknown,
        "UNKNOWN - repository lock held by PID 4412",
        &[],
        mins(52),
        None,
    );
    b.problem(
        "k8s-node-04",
        "kubelet",
        ServiceState::Unknown,
        "UNKNOWN - connection refused (10.0.4.24:10250)",
        &[],
        mins(4),
        Some(2),
    );
    b.problem(
        "k8s-node-02",
        "ntp-offset",
        ServiceState::Warning,
        "WARNING - offset 0.82s",
        &["offset=0.820000s;0.500000;1.000000;"],
        hours(2),
        None,
    );
    b.problem(
        "vpn-gw-01",
        "cert-expiry",
        ServiceState::Warning,
        "WARNING - certificate expires in 12 days",
        &["days_left=12;14;3;0"],
        hours(26),
        None,
    );
    b.problem(
        "sw-core-ams-02",
        "uplink edge-ams",
        ServiceState::Critical,
        "CRITICAL - Interface Po12 (uplink edge-ams) is down",
        &["in_octets=0c;;;0", "out_octets=0c;;;0"],
        mins(11),
        None,
    );

    // --- Host problems ---------------------------------------------------------
    b.host_down(
        "k8s-node-11",
        "PING CRITICAL - Packet loss = 100%",
        mins(12),
        true,
    );
    b.comment(
        ObjectKey::host("k8s-node-11"),
        "infra-oncall",
        "Node unresponsive, hardware ticket INC-20418 opened",
        mins(7),
        None,
    );
    b.host_down(
        "edge-fra-04",
        "PING CRITICAL - Packet loss = 100%",
        hours(1) + mins(8),
        true,
    );
    let start = b.ago(mins(46));
    let end = b.later(mins(74));
    let rack = b.downtime(
        ObjectKey::host("edge-fra-04"),
        "a.ivanova",
        "rack maintenance",
        start,
        end,
        true,
        0.0,
        None,
        false,
    );
    b.host_down(
        "store-ams-02",
        "PING CRITICAL - Packet loss = 100%",
        mins(3),
        true,
    );
    let unreachable: Vec<String> = (8..=12).map(|i| format!("edge-ams-{i:02}")).collect();
    for (i, name) in unreachable.iter().enumerate() {
        let address = format!("10.8.2.{}", 18 + i);
        b.host_down(
            name,
            &format!("CRITICAL - Host Unreachable ({address})"),
            mins(11),
            false,
        );
    }

    // Services of failed hosts: ping fails, agent checks can't connect.
    let failed: Vec<(String, &str)> = ["k8s-node-11", "edge-fra-04", "store-ams-02"]
        .iter()
        .map(|h| ((*h).to_owned(), "master-01"))
        .chain(unreachable.iter().map(|h| (h.clone(), "sat-ams-01")))
        .collect();
    for (host, endpoint) in &failed {
        let since = if host.starts_with("edge-ams") {
            mins(11)
        } else if host == "k8s-node-11" {
            mins(12)
        } else if host == "store-ams-02" {
            mins(3)
        } else {
            hours(1) + mins(8)
        };
        b.problem(
            host,
            "ping4",
            ServiceState::Critical,
            "PING CRITICAL - Packet loss = 100%",
            &[
                "rta=5000.000000ms;3000.000000;5000.000000;0.000000",
                "pl=100%;80;100;0",
            ],
            since,
            None,
        );
        let agent_checks: &[&str] = if host.starts_with("edge") {
            &["disk /", "load"]
        } else {
            &["disk /", "load", "memory"]
        };
        for service in agent_checks {
            b.problem(
                host,
                service,
                ServiceState::Unknown,
                &format!("Remote Icinga instance '{host}' is not connected to '{endpoint}'"),
                &[],
                since,
                None,
            );
        }
    }
    // The rack maintenance covers the host's services too (all_services).
    let edge_fra_services: Vec<String> = b
        .scenario
        .services
        .iter()
        .filter(|s| s.key.host.as_str() == "edge-fra-04")
        .map(|s| s.key.name.to_string())
        .collect();
    for service in edge_fra_services {
        b.downtime(
            ObjectKey::service("edge-fra-04", &service),
            "a.ivanova",
            "rack maintenance",
            start,
            end,
            true,
            0.0,
            Some(rack.clone()),
            false,
        );
    }

    // --- Dependencies -------------------------------------------------------------
    for i in 1..=7 {
        b.depends(
            ObjectKey::host(&format!("edge-ams-{i:02}")),
            ObjectKey::host("sw-core-ams-01"),
            "uplink",
        );
    }
    for name in &unreachable {
        b.depends(
            ObjectKey::host(name),
            ObjectKey::service("sw-core-ams-02", "uplink edge-ams"),
            "uplink",
        );
    }
    for i in 1..=10 {
        b.depends(
            ObjectKey::host(&format!("edge-fra-{i:02}")),
            ObjectKey::host("sw-core-fra-01"),
            "uplink",
        );
    }
    for i in 1..=4 {
        b.depends(
            ObjectKey::host(&format!("store-ams-{i:02}")),
            ObjectKey::host("sw-core-ams-01"),
            "uplink",
        );
        b.depends(
            ObjectKey::host(&format!("store-fra-{i:02}")),
            ObjectKey::host("sw-core-fra-02"),
            "uplink",
        );
    }

    // --- More realistic background problems --------------------------------------
    let warnings: &[(&str, &str)] = &[
        ("web-prod-05", "apt"),
        ("web-prod-11", "apt"),
        ("api-prod-03", "apt"),
        ("api-prod-09", "apt"),
        ("k8s-node-15", "apt"),
        ("k8s-node-22", "apt"),
        ("kafka-02", "apt"),
        ("nfs-01", "apt"),
        ("web-prod-14", "disk /"),
        ("k8s-node-29", "disk /"),
        ("minio-03", "disk /data"),
        ("api-prod-07", "memory"),
        ("k8s-node-33", "memory"),
        ("db-mongo-02", "mongodb-connections"),
        ("kafka-03", "kafka-isr"),
        ("store-fra-03", "smart-disks"),
        ("web-prod-08", "http"),
        ("k8s-node-18", "ntp-offset"),
        ("k8s-node-22", "load"),
        ("api-prod-12", "load"),
        ("db-prod-05", "pg-bloat"),
        ("cache-04", "redis-connections"),
    ];
    for (i, (host, service)) in warnings.iter().enumerate() {
        let since = Duration::from_secs(600 + 4_211 * (i as u64 + 1) % 86_000);
        b.generated_problem(host, service, ServiceState::Warning, since);
    }
    for (i, (host, service)) in [
        ("kafka-01", "kafka-consumer-lag"),
        ("minio-02", "minio-health"),
        ("db-mysql-03", "mysql-replication"),
        ("store-fra-01", "smart-disks"),
    ]
    .iter()
    .enumerate()
    {
        let since = Duration::from_secs(900 + 2_377 * (i as u64 + 1));
        b.generated_problem(host, service, ServiceState::Unknown, since);
    }

    // A planned maintenance from a ScheduledDowntime and a flexible downtime.
    let start = b.later(hours(9));
    let end = b.later(hours(11));
    b.downtime(
        ObjectKey::host("backup-02"),
        "icinga",
        "Nightly backup verification window",
        start,
        end,
        true,
        0.0,
        None,
        true,
    );
    let start = b.ago(mins(5));
    let end = b.later(hours(4));
    b.downtime(
        ObjectKey::host("db-mysql-02"),
        "j.berg",
        "Kernel update, reboot within the window",
        start,
        end,
        false,
        1_800.0,
        None,
        false,
    );

    downtimes_in_the_panes(&mut b);
    the_lists(&mut b);

    // Objects the demo should keep showing.
    let mut pinned: Vec<ObjectKey> = [
        "db-prod-03",
        "mq-prod-01",
        "k8s-node-07",
        "web-edge-02",
        "lb-prod-02",
        "api-gw-01",
        "db-prod-01",
        "cache-02",
        "backup-01",
        "k8s-node-04",
        "k8s-node-02",
        "vpn-gw-01",
        "sw-core-ams-02",
    ]
    .iter()
    .map(|h| ObjectKey::host(h))
    .collect();
    // The downtimes' objects that are OK (topic 01): kept OK, so they stay
    // in the frames they were set up for.
    pinned.push(ObjectKey::service("db-prod-05", "pg-locks"));
    pinned.push(ObjectKey::service("db-prod-05", "pg-bloat"));
    // The lists' acknowledged problems (topic 07) stay as they are.
    for (host, service) in [
        ("kafka-01", "kafka-consumer-lag"),
        ("k8s-node-18", "ntp-offset"),
        ("nfs-02", "disk /srv"),
    ] {
        pinned.push(ObjectKey::service(host, service));
    }
    for (host, _) in &failed {
        pinned.push(ObjectKey::host(host));
    }
    let pinned_hosts: std::collections::HashSet<String> =
        pinned.iter().map(ObjectKey::full_name).collect();
    for service in &b.scenario.services {
        if pinned_hosts.contains(service.key.host.as_str()) && service.state != ServiceState::Ok {
            pinned.push(service.object_key());
        }
    }
    for (host, _) in &failed {
        for service in b
            .scenario
            .services
            .iter()
            .filter(|s| s.key.host.as_str() == host)
        {
            pinned.push(service.object_key());
        }
    }
    pinned.sort();
    pinned.dedup();
    b.scenario.pinned = pinned;
    // Icinga's own notifications: the DBAs for the databases, the
    // infrastructure on-call for everything else.
    let databases: std::collections::HashSet<String> = b
        .scenario
        .hosts
        .iter()
        .filter(|host| host.groups.iter().any(|group| group == "databases"))
        .map(|host| host.name.to_string())
        .collect();
    b.scenario.apply_notification("mail-oncall", |object| {
        let names: &[&str] = match object {
            ObjectKey::Host { .. } => &["infra-oncall", "infra-lead"],
            ObjectKey::Service { key } if databases.contains(key.host.as_str()) => {
                &["dba-oncall", "m.keller"]
            }
            ObjectKey::Service { .. } => &["infra-oncall"],
        };
        names.iter().map(|name| (*name).to_owned()).collect()
    });
    b.finish()
}

/// The downtimes of topic 01 (`design/v1/01-downtimes.html`), one of each
/// case the pane draws:
///
/// - a service in a fixed downtime: redis-memory on cache-02, scheduled
///   with the rest of the scenario above;
/// - a host in downtime with all its services (k8s-node-07, whose disk
///   /var and kubelet are critical);
/// - a flexible downtime waiting for a problem (pg-locks on db-prod-05,
///   OK) and one a problem started (pg-bloat on db-prod-05);
/// - a downtime scheduled for tonight on an unhandled problem
///   (haproxy-backend on lb-prod-02);
/// - a host with three downtimes: in effect with its services, flexible
///   tonight, and a weekly one from the config (sw-core-ams-02);
/// - a downtime on the host only, not its services (k8s-node-02, whose
///   ntp-offset still warns and notifies).
fn downtimes_in_the_panes(b: &mut Builder) {
    let reimage = ScenarioDowntime {
        entry: Some(b.ago(mins(48))),
        ..ScenarioDowntime::fixed(
            ObjectKey::host("k8s-node-07"),
            "m.keller",
            "Node drained and cordoned for the kernel 6.8 rollout; reimage starts 14:30.",
            b.ago(mins(42)),
            b.later(mins(78)),
        )
    };
    host_with_services(b, "k8s-node-07", &reimage);

    b.downtime_with(ScenarioDowntime {
        fixed: false,
        duration: 7_200.0,
        entry: Some(b.ago(mins(17))),
        ..ScenarioDowntime::fixed(
            ObjectKey::service("db-prod-05", "pg-locks"),
            "dba-oncall",
            "VACUUM FULL on orders_archive; lock waits are expected while it runs.",
            b.ago(mins(12)),
            b.later(mins(228)),
        )
    });
    b.downtime_with(ScenarioDowntime {
        fixed: false,
        duration: 7_200.0,
        trigger: Some(b.ago(mins(48))),
        entry: Some(b.ago(mins(75))),
        ..ScenarioDowntime::fixed(
            ObjectKey::service("db-prod-05", "pg-bloat"),
            "dba-oncall",
            "pg_repack on orders; bloat warnings are expected until it finishes.",
            b.ago(mins(60)),
            b.later(mins(180)),
        )
    });

    b.downtime_with(ScenarioDowntime {
        entry: Some(b.ago(mins(190))),
        ..ScenarioDowntime::fixed(
            ObjectKey::service("lb-prod-02", "haproxy-backend"),
            "m.keller",
            "HAProxy 2.8 rollout on lb-prod-01/02 (CHG-4471); backends flap while the pool \
             reloads.",
            b.later(mins(468)),
            b.later(mins(528)),
        )
    });

    let swap = ScenarioDowntime {
        entry: Some(b.ago(mins(20))),
        ..ScenarioDowntime::fixed(
            ObjectKey::host("sw-core-ams-02"),
            "m.keller",
            "Line card swap in slot 3 (CHG-4468): Po12 to edge-ams is down during the swap.",
            b.ago(mins(12)),
            b.later(mins(48)),
        )
    };
    host_with_services(b, "sw-core-ams-02", &swap);
    b.downtime_with(ScenarioDowntime {
        fixed: false,
        duration: 3_600.0,
        entry: Some(b.ago(mins(15))),
        ..ScenarioDowntime::fixed(
            ObjectKey::host("sw-core-ams-02"),
            "m.keller",
            "IOS XE 17.9.5 upgrade if the vendor build passes the lab tonight.",
            b.later(mins(468)),
            b.later(mins(948)),
        )
    });
    b.downtime_with(ScenarioDowntime {
        schedule: Some("weekly-patching"),
        ..ScenarioDowntime::fixed(
            ObjectKey::host("sw-core-ams-02"),
            "icingaadmin",
            "Weekly patch window for the core switches.",
            b.later(hours(63) + mins(48)),
            b.later(hours(67) + mins(48)),
        )
    });

    let mut bmc = ScenarioDowntime::fixed(
        ObjectKey::host("k8s-node-02"),
        "m.keller",
        "BMC firmware update (CHG-4470); the host check may flap, its services stay monitored.",
        b.ago(mins(12)),
        b.later(mins(48)),
    );
    bmc.entry = Some(b.ago(mins(17)));
    b.downtime_with(bmc);
}

/// What the lists of topic 07 (`design/v1/07-comments-downtimes-lists.html`)
/// show besides the downtimes above: comments by several people (one
/// expiring), acknowledgements sticky or not, with an expiry or none, and
/// two more downtimes to come (one with every service of its host, one from
/// the config).
fn the_lists(b: &mut Builder) {
    the_lists_acknowledgements(b);
    the_lists_comments_and_downtimes(b);
}

/// The acknowledged list's problems (topic 07, frame 7f).
fn the_lists_acknowledgements(b: &mut Builder) {
    b.problem(
        "db-prod-01",
        "pg-autovacuum",
        ServiceState::Critical,
        "POSTGRES_AUTOVACUUM CRITICAL - orders: autovacuum running for 58 min",
        &["autovacuum_running=3480s;1800;3000"],
        hours(1),
        None,
    );
    b.acknowledge_until(
        &ObjectKey::service("db-prod-01", "pg-autovacuum"),
        "dba-oncall",
        "vacuum running on orders, about 30 minutes",
        hours(1),
        false,
        Some(mins(48)),
    );
    b.acknowledge_until(
        &ObjectKey::service("backup-01", "borg-last-run"),
        "dba-oncall",
        "lock from the migration test, clears after Thursday",
        mins(42),
        true,
        Some(hours(17) + mins(48)),
    );
    b.acknowledge(
        &ObjectKey::service("kafka-01", "kafka-consumer-lag"),
        "m.keller",
        "consumer group rebalancing after the deploy",
        mins(47),
        false,
    );
    b.acknowledge_until(
        &ObjectKey::service("k8s-node-18", "ntp-offset"),
        "j.berg",
        "chrony re-sync with the node reboot tonight",
        hours(1) + mins(42),
        true,
        Some(hours(8) + mins(48)),
    );
    b.problem(
        "nfs-02",
        "disk /srv",
        ServiceState::Critical,
        "DISK CRITICAL - /srv 96% used (31 GiB free)",
        &["'/srv'=769GiB;640;720;0;800"],
        hours(3),
        None,
    );
    b.acknowledge(
        &ObjectKey::service("nfs-02", "disk /srv"),
        "j.berg",
        "cleanup job running, 30 GiB to go",
        hours(2) + mins(52),
        false,
    );
    b.acknowledge_until(
        &ObjectKey::service("vpn-gw-01", "cert-expiry"),
        "m.keller",
        "new certificate arrives Friday; leave this until then",
        hours(21) + mins(32),
        true,
        Some(hours(45) + mins(48)),
    );
}

/// The comment list's comments and the downtimes still to come (topic 07,
/// frames 7a and 7d).
fn the_lists_comments_and_downtimes(b: &mut Builder) {
    b.comment(
        ObjectKey::service("vpn-gw-01", "cert-expiry"),
        "m.keller",
        "new certificate arrives Friday; leave this warning until then",
        hours(21) + mins(32),
        Some(hours(45) + mins(48)),
    );
    b.comment(
        ObjectKey::service("mq-prod-01", "rabbitmq-queue"),
        "m.keller",
        "consumer deploy rolled back, queue should drain within 20 min",
        mins(3),
        None,
    );
    b.comment(
        ObjectKey::service("k8s-node-04", "kubelet"),
        "j.berg",
        "node not drained yet, looking at containerd logs",
        mins(2),
        None,
    );
    b.comment(
        ObjectKey::host("backup-01"),
        "dba-oncall",
        "repository moves to backup-02 on Thursday night, see CHG-4480",
        hours(52) + mins(58),
        None,
    );
    let migration = ScenarioDowntime {
        entry: Some(b.ago(mins(35))),
        ..ScenarioDowntime::fixed(
            ObjectKey::host("backup-01"),
            "j.berg",
            "borg repository migration to backup-02",
            b.later(hours(10) + mins(48)),
            b.later(hours(12) + mins(48)),
        )
    };
    host_with_services(b, "backup-01", &migration);
    b.downtime_with(ScenarioDowntime {
        schedule: Some("weekly-patching"),
        ..ScenarioDowntime::fixed(
            ObjectKey::host("sw-core-ams-01"),
            "icingaadmin",
            "Weekly patch window for the core switches.",
            b.later(hours(63) + mins(48)),
            b.later(hours(67) + mins(48)),
        )
    });
}

/// A downtime on `host` with `all_services`: the host's, and one per
/// service with the host's as its parent, as Icinga schedules them.
fn host_with_services(b: &mut Builder, host: &str, spec: &ScenarioDowntime) {
    let parent = b.downtime_with(spec.clone());
    let services: Vec<String> = b
        .scenario
        .services
        .iter()
        .filter(|service| service.key.host.as_str() == host)
        .map(|service| service.key.name.to_string())
        .collect();
    for service in services {
        b.downtime_with(ScenarioDowntime {
            object: ObjectKey::service(host, &service),
            parent: Some(parent.clone()),
            ..spec.clone()
        });
    }
}
