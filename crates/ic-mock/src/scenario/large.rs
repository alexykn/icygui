//! `large`: about 2000 hosts with 10 services each and ~3% problems, for
//! performance work. Deterministic for a given seed.

use std::time::Duration;

use ic_model::{ObjectKey, ServiceState};
use serde_json::json;

use super::Scenario;
use super::build::{Builder, vars};

/// A host role: name, host group and three role-specific services
/// (`(service, check_command)`).
type Role = (
    &'static str,
    &'static str,
    [(&'static str, &'static str); 3],
);

const ROLES: &[Role] = &[
    (
        "web",
        "web-frontend",
        [
            ("http", "http"),
            ("nginx-workers", "procs"),
            ("php-fpm", "procs"),
        ],
    ),
    (
        "api",
        "api",
        [
            ("http", "http"),
            ("http-latency", "http"),
            ("jvm-heap", "jmx"),
        ],
    ),
    (
        "db",
        "databases",
        [
            ("pg-connections", "check_postgres"),
            ("postgres-replication", "check_postgres"),
            ("disk /var/lib/postgresql", "disk"),
        ],
    ),
    (
        "k8s",
        "kubernetes",
        [
            ("kubelet", "kubelet"),
            ("containerd", "procs"),
            ("disk /var", "disk"),
        ],
    ),
    (
        "cache",
        "cache",
        [
            ("redis-memory", "redis"),
            ("redis-replication", "redis"),
            ("redis-connections", "redis"),
        ],
    ),
    (
        "mq",
        "queue",
        [
            ("rabbitmq-queue", "rabbitmq"),
            ("rabbitmq-cluster", "rabbitmq"),
            ("rabbitmq-memory", "rabbitmq"),
        ],
    ),
    (
        "store",
        "storage",
        [
            ("zfs-pool", "zfs"),
            ("smart-disks", "smart"),
            ("disk /srv", "disk"),
        ],
    ),
    (
        "edge",
        "edge",
        [
            ("http-tls", "http"),
            ("cert-expiry", "ssl_cert"),
            ("varnish", "procs"),
        ],
    ),
];

const STANDARD: &[(&str, &str)] = &[
    ("ping4", "ping4"),
    ("ssh", "ssh"),
    ("load", "load"),
    ("disk /", "disk"),
    ("memory", "mem"),
    ("ntp-offset", "ntp_time"),
    ("apt", "apt"),
];

/// Number of hosts in the `large` scenario.
const HOSTS: usize = 2_000;

/// Builds the `large` scenario. The same seed always gives the same objects
/// and states.
#[must_use]
pub fn large(seed: u64) -> Scenario {
    let mut b = Builder::new("large", "master-01", "r2.14.3-1", seed);
    b.endpoint("master-01", "master", false);
    for i in 0..HOSTS {
        let (role, group, specific) = ROLES[i % ROLES.len()];
        let name = format!("{role}-{:04}", i / ROLES.len() + 1);
        let address = format!("10.{}.{}.{}", 100 + i / 65_536, (i / 256) % 256, i % 256);
        b.host(
            &name,
            &address,
            &[group, "linux-servers"],
            vars(&[
                ("role", json!(role)),
                ("env", json!("prod")),
                ("rack", json!(format!("r{:02}", i % 40))),
            ]),
        );
        let service_vars = vars(&[("env", json!("prod"))]);
        for (service, command) in STANDARD.iter().chain(specific.iter()) {
            let dice = b.rng.unit();
            let state = if dice < 0.018 {
                ServiceState::Warning
            } else if dice < 0.026 {
                ServiceState::Critical
            } else if dice < 0.03 {
                ServiceState::Unknown
            } else {
                ServiceState::Ok
            };
            let since = Duration::from_secs(60 + b.rng.below(86_400 * 3));
            b.service_in_state(
                &name,
                service,
                command,
                &[group],
                service_vars.clone(),
                state,
                since,
            );
        }
    }
    // A few hosts down.
    let down: Vec<String> = (0..6)
        .map(|_| {
            let index = b.rng.index(b.scenario.hosts.len());
            b.scenario.hosts[index].name.to_string()
        })
        .collect();
    for name in &down {
        let minutes = 2 + b.rng.below(120);
        b.host_down(
            name,
            "PING CRITICAL - Packet loss = 100%",
            Duration::from_secs(minutes * 60),
            true,
        );
    }
    b.scenario.pinned = down.iter().map(|h| ObjectKey::host(h)).collect();
    b.finish()
}
