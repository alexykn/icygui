//! `large`: production scale, for performance work (docs/performance.md).
//!
//! Built like `contract/scale/generate.py`, the configuration the real
//! Icinga measurements ran on: 2 000 hosts with the same 15 services each,
//! 5-minute check intervals (1-minute retries, 3 attempts), about 5 % of
//! the services and 1 % of the hosts in a problem state, plugin output with
//! performance data, and custom variables on every object. On top of that,
//! a realistic share of the problems is handled: some acknowledged, some
//! down hosts in a downtime. Deterministic for a given seed.

use std::time::Duration;

use ic_model::{HostGroup, ObjectKey, ServiceGroup, ServiceState};
use serde_json::json;

use super::build::{Builder, vars};
use super::{Scenario, User};

/// Number of hosts in [`large`].
const HOSTS: usize = 2_000;

/// Host roles, round-robin like `generate.py`.
const ROLES: [&str; 10] = [
    "web", "api", "db", "k8s", "mq", "cache", "lb", "edge", "backup", "storage",
];

/// The 15 services of every host: `(name, check command)`.
const SERVICES: [(&str, &str); 15] = [
    ("ping4", "ping4"),
    ("ssh", "ssh"),
    ("load", "load"),
    ("disk /", "disk"),
    ("disk /var", "disk"),
    ("memory", "mem"),
    ("ntp", "ntp_time"),
    ("procs", "procs"),
    ("swap", "swap"),
    ("users", "users"),
    ("http", "http"),
    ("cert", "ssl_cert"),
    ("backup", "backup"),
    ("raid", "raid"),
    ("smart", "smart"),
];

/// Service groups by check command, like Icinga's sample configuration.
fn service_groups(command: &str) -> &'static [&'static str] {
    match command {
        "ping4" => &["ping"],
        "disk" => &["disk"],
        "http" | "ssl_cert" => &["http"],
        _ => &[],
    }
}

/// Custom variables an apply rule would give a service.
fn service_vars(host: &str, role: &str, team: &str, service: &str) -> ic_model::Vars {
    let runbook = format!(
        "https://wiki.example.com/runbooks/{role}/{}",
        service.replace([' ', '/'], "-").trim_matches('-')
    );
    let mut pairs = vec![("team", json!(team)), ("runbook", json!(runbook))];
    match service {
        "disk /" | "disk /var" => {
            let partition = service.trim_start_matches("disk ");
            pairs.push(("disk_partitions", json!(partition)));
            pairs.push(("disk_wfree", json!("20%")));
            pairs.push(("disk_cfree", json!("10%")));
        }
        "http" | "cert" => {
            pairs.push(("http_vhost", json!(host)));
            pairs.push(("http_uri", json!("/health")));
        }
        "load" => {
            pairs.push(("load_wload1", json!(8)));
            pairs.push(("load_cload1", json!(12)));
        }
        _ => {}
    }
    vars(&pairs)
}

/// Custom variables of host number `index`.
fn host_vars(index: usize, role: &str, team: &str, name: &str) -> ic_model::Vars {
    vars(&[
        ("role", json!(role)),
        ("env", json!("prod")),
        ("rack", json!(format!("r{:02}", index % 40))),
        ("team", json!(team)),
        (
            "runbook",
            json!(format!("https://wiki.example.com/runbooks/{role}")),
        ),
        ("os", json!("Linux")),
        (
            "disks",
            json!({
                "disk /": { "disk_partitions": "/" },
                "disk /var": { "disk_partitions": "/var" },
            }),
        ),
        (
            "http_vhosts",
            json!({ "http": { "http_uri": "/health", "http_vhost": name } }),
        ),
        ("notification", json!({ "mail": { "groups": [team] } })),
    ])
}

/// Host groups per role, and service groups per kind of check.
fn groups(b: &mut Builder) {
    b.scenario.host_groups = ROLES
        .iter()
        .map(|role| HostGroup {
            name: (*role).to_owned(),
            display_name: format!("{} servers", role.to_uppercase()),
        })
        .chain(std::iter::once(HostGroup {
            name: "linux-servers".to_owned(),
            display_name: "Linux Servers".to_owned(),
        }))
        .collect();
    b.scenario.service_groups = [
        ("ping", "Ping Checks"),
        ("disk", "Disk Checks"),
        ("http", "HTTP Checks"),
    ]
    .iter()
    .map(|(name, display_name)| ServiceGroup {
        name: (*name).to_owned(),
        display_name: (*display_name).to_owned(),
    })
    .collect();
}

/// Builds the `large` scenario: 2 000 hosts × 15 services. The same seed
/// always gives the same objects and states.
#[must_use]
pub fn large(seed: u64) -> Scenario {
    large_with_hosts(HOSTS, seed)
}

/// The `large` scenario with `hosts` hosts instead of 2 000 (still 15
/// services each), so tests can exercise it at a fraction of the size.
#[must_use]
pub fn large_with_hosts(hosts: usize, seed: u64) -> Scenario {
    let mut b = Builder::new("large", "master-01", "r2.14.3-1", seed);
    b.intervals = Some((300.0, 60.0));
    b.endpoint("master-01", "master", false);
    groups(&mut b);

    let mut down = Vec::new();
    for i in 0..hosts {
        let role = ROLES[i % ROLES.len()];
        let team = format!("team-{}", i % 7);
        let name = format!("{role}-{i:04}.prod.example.com");
        let address = format!("10.{}.{}.{}", 20 + i / 62_500, (i / 250) % 250, i % 250 + 4);
        b.host(
            &name,
            &address,
            &[role, "linux-servers"],
            host_vars(i, role, &team, &name),
        );
        let first_service = b.scenario.services.len();
        for (service, command) in SERVICES {
            // Like generate.py: 2 % critical, 2 % warning, 1 % unknown.
            let dice = b.rng.unit();
            let state = if dice < 0.02 {
                ServiceState::Critical
            } else if dice < 0.04 {
                ServiceState::Warning
            } else if dice < 0.05 {
                ServiceState::Unknown
            } else {
                ServiceState::Ok
            };
            let since = Duration::from_secs(60 + b.rng.below(3 * 86_400));
            b.service_in_state(
                &name,
                service,
                command,
                service_groups(command),
                service_vars(&name, role, &team, service),
                state,
                since,
            );
            // About one problem in five is already being worked on (it was
            // acknowledged after it started).
            if state != ServiceState::Ok && b.rng.chance(0.2) {
                let sticky = b.rng.chance(0.3);
                let ago = Duration::from_secs(30 + b.rng.below(since.as_secs().min(3_600) - 30));
                b.acknowledge_last_service(&team, "Known issue, ticket filed", ago, sticky);
            }
        }
        if b.rng.chance(0.01) {
            let since = Duration::from_secs(120 + b.rng.below(7_200));
            b.host_down(&name, "PING CRITICAL - Packet loss = 100%", since, true);
            // Services of a host that is down are unreachable.
            for service in &mut b.scenario.services[first_service..] {
                service.check.reachable = false;
            }
            down.push(name);
        }
    }
    // Every second failed host is in a maintenance window.
    for name in down.iter().step_by(2) {
        let start = b.ago(Duration::from_mins(30));
        let end = b.later(Duration::from_mins(90));
        b.downtime(
            ObjectKey::host(name),
            "ops",
            "Hardware replacement",
            start,
            end,
            true,
            0.0,
            None,
            false,
        );
    }
    b.scenario.pinned = down.iter().map(|name| ObjectKey::host(name)).collect();
    // One notification per host and service, as `apply Notification … to
    // Service` gives a production setup.
    b.scenario.users = vec![User::new("oncall", "On-call", "oncall@example.com")];
    b.scenario
        .apply_notification("mail-oncall", |_| vec!["oncall".to_owned()]);
    b.finish()
}
