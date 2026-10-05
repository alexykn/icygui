//! Scenarios: the objects a mock environment serves, as `ic_model` types.
//!
//! A scenario is plain data. [`crate::MockServer::start`] loads it into the
//! server's runtime state, filling in what Icinga computes itself (severity,
//! `handled`, reachability, check result details, legacy ids, ...).
//!
//! Timestamps are relative to [`Scenario::time_base`]: when a server starts,
//! every timestamp is shifted by `now - time_base`, so "critical for 14
//! minutes" is still true at startup no matter when the scenario was built.

mod build;
mod lab;
mod large;
mod prod_cluster;
mod staging;

use ic_model::{
    Comment, Dependency, Downtime, Endpoint, Host, HostGroup, HostState, InstanceStatus, ObjectKey,
    Service, ServiceGroup, ServiceState, Timestamp,
};

pub(crate) use build::format_perfdata;
pub use build::raw_check_result;
pub use lab::lab;
pub use large::{large, large_with_hosts};
pub use prod_cluster::prod_cluster;
pub use staging::staging;

/// The names [`by_name`] accepts.
pub const NAMES: [&str; 4] = ["prod-cluster", "staging", "lab", "large"];

/// A built-in scenario by name (`prod-cluster`, `staging`, `lab`, `large`);
/// `seed` only matters for `large`.
#[must_use]
pub fn by_name(name: &str, seed: u64) -> Option<Scenario> {
    match name {
        "prod-cluster" => Some(prod_cluster()),
        "staging" => Some(staging()),
        "lab" => Some(lab()),
        "large" => Some(large(seed)),
        _ => None,
    }
}

/// The objects of one environment.
#[derive(Clone, Debug)]
pub struct Scenario {
    /// Environment name (`prod-cluster`).
    pub name: String,
    /// Hosts.
    pub hosts: Vec<Host>,
    /// Services; each must belong to a host of the scenario.
    pub services: Vec<Service>,
    /// Host groups. Groups that hosts reference but that are missing here
    /// are created automatically.
    pub host_groups: Vec<HostGroup>,
    /// Service groups (missing ones are created automatically, too).
    pub service_groups: Vec<ServiceGroup>,
    /// Comments, including the `Acknowledgement` comments of acknowledged
    /// problems.
    pub comments: Vec<Comment>,
    /// Downtimes. `in_effect` is ignored: the server computes it.
    pub downtimes: Vec<Downtime>,
    /// Dependencies (parent → child), which make children unreachable while
    /// a parent is down.
    pub dependencies: Vec<Dependency>,
    /// Cluster endpoints; `zone` puts them into a zone's `endpoints`.
    pub endpoints: Vec<Endpoint>,
    /// What `/v1/status` reports about the instance (node name, version,
    /// global switches). Check rates and latencies are computed live.
    pub status: InstanceStatus,
    /// Zones (zones referenced by endpoints are created when missing).
    pub zones: Vec<Zone>,
    /// Icinga `User` objects (notification recipients), served read-only.
    pub users: Vec<User>,
    /// The reference time of every timestamp in this scenario.
    pub time_base: Timestamp,
    /// Objects the simulator never changes, so a demo keeps showing them.
    pub pinned: Vec<ObjectKey>,
}

/// A cluster zone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Zone {
    /// Zone name.
    pub name: String,
    /// Parent zone, if any.
    pub parent: Option<String>,
    /// Global zones only carry configuration.
    pub global: bool,
}

impl Zone {
    /// A zone with an optional parent.
    #[must_use]
    pub fn new(name: &str, parent: Option<&str>) -> Self {
        Self {
            name: name.to_owned(),
            parent: parent.map(str::to_owned),
            global: false,
        }
    }
}

/// An Icinga `User` object (who gets notified).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct User {
    /// Object name.
    pub name: String,
    /// Display name.
    pub display_name: String,
    /// E-mail address.
    pub email: String,
    /// User groups.
    pub groups: Vec<String>,
}

impl User {
    /// A user with an e-mail address.
    #[must_use]
    pub fn new(name: &str, display_name: &str, email: &str) -> Self {
        Self {
            name: name.to_owned(),
            display_name: display_name.to_owned(),
            email: email.to_owned(),
            groups: Vec::new(),
        }
    }
}

/// Object counts by state, for checks on scenarios.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[expect(
    missing_docs,
    reason = "the field names are the documentation: counts per state"
)]
pub struct Summary {
    pub hosts: usize,
    pub hosts_up: usize,
    pub hosts_down: usize,
    pub hosts_unreachable: usize,
    pub hosts_pending: usize,
    pub services: usize,
    pub services_ok: usize,
    pub services_warning: usize,
    pub services_critical: usize,
    pub services_unknown: usize,
    pub services_pending: usize,
}

impl Scenario {
    /// An empty scenario whose timestamps are relative to now.
    #[must_use]
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            hosts: Vec::new(),
            services: Vec::new(),
            host_groups: Vec::new(),
            service_groups: Vec::new(),
            comments: Vec::new(),
            downtimes: Vec::new(),
            dependencies: Vec::new(),
            endpoints: Vec::new(),
            status: InstanceStatus {
                node_name: "master-01".to_owned(),
                version: "r2.14.3-1".to_owned(),
                program_start: Timestamp::now(),
                notifications_enabled: true,
                host_checks_enabled: true,
                service_checks_enabled: true,
                event_handlers_enabled: true,
                flap_detection_enabled: true,
                perfdata_enabled: true,
                checks_per_minute: 0.0,
                avg_latency: 0.0,
                avg_execution_time: 0.0,
            },
            zones: Vec::new(),
            users: Vec::new(),
            time_base: Timestamp::now(),
            pinned: Vec::new(),
        }
    }

    /// The host with this name.
    #[must_use]
    pub fn host(&self, name: &str) -> Option<&Host> {
        self.hosts.iter().find(|h| h.name.as_str() == name)
    }

    /// The service `host!name`.
    #[must_use]
    pub fn service(&self, host: &str, name: &str) -> Option<&Service> {
        self.services
            .iter()
            .find(|s| s.key.host.as_str() == host && &*s.key.name == name)
    }

    /// Object counts by state.
    #[must_use]
    pub fn summary(&self) -> Summary {
        let mut summary = Summary {
            hosts: self.hosts.len(),
            services: self.services.len(),
            ..Summary::default()
        };
        for host in &self.hosts {
            match host.state {
                HostState::Up => summary.hosts_up += 1,
                HostState::Down => summary.hosts_down += 1,
                HostState::Unreachable => summary.hosts_unreachable += 1,
                HostState::Pending => summary.hosts_pending += 1,
            }
        }
        for service in &self.services {
            match service.state {
                ServiceState::Ok => summary.services_ok += 1,
                ServiceState::Warning => summary.services_warning += 1,
                ServiceState::Critical => summary.services_critical += 1,
                ServiceState::Unknown => summary.services_unknown += 1,
                ServiceState::Pending => summary.services_pending += 1,
            }
        }
        summary
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn check_consistency(scenario: &Scenario) {
        let hosts: HashSet<&str> = scenario.hosts.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(hosts.len(), scenario.hosts.len(), "duplicate host names");
        let mut services = HashSet::new();
        for service in &scenario.services {
            assert!(
                hosts.contains(service.key.host.as_str()),
                "{} has no host",
                service.key
            );
            assert!(
                services.insert(service.key.full_name()),
                "duplicate {}",
                service.key
            );
        }
        for dependency in &scenario.dependencies {
            for key in [&dependency.child, &dependency.parent] {
                match key {
                    ObjectKey::Host { name } => assert!(hosts.contains(name.as_str())),
                    ObjectKey::Service { key } => assert!(services.contains(&key.full_name())),
                }
            }
        }
        for comment in &scenario.comments {
            assert!(comment.name.starts_with(&comment.object.full_name()));
        }
        for downtime in &scenario.downtimes {
            assert!(downtime.name.starts_with(&downtime.object.full_name()));
        }
    }

    #[test]
    fn built_in_scenarios_are_consistent() {
        for scenario in [prod_cluster(), staging(), lab(), large(7)] {
            check_consistency(&scenario);
        }
    }

    #[test]
    fn prod_cluster_matches_the_design_summary() {
        let summary = prod_cluster().summary();
        assert!(
            (11..=14).contains(&summary.services_critical),
            "{summary:?}"
        );
        assert!((27..=31).contains(&summary.services_warning), "{summary:?}");
        assert!((22..=27).contains(&summary.services_unknown), "{summary:?}");
        assert_eq!(summary.hosts_down, 3, "{summary:?}");
        assert_eq!(summary.hosts_unreachable, 5, "{summary:?}");
        assert!(summary.hosts >= 140, "{summary:?}");
    }

    #[test]
    fn staging_and_lab_are_small() {
        let staging = staging().summary();
        assert!((8..=12).contains(&staging.hosts), "{staging:?}");
        assert_eq!(staging.services_warning, 3, "{staging:?}");
        assert_eq!(
            staging.services_critical + staging.services_unknown,
            0,
            "{staging:?}"
        );
        let lab = lab().summary();
        assert_eq!(lab.hosts, 2);
        assert_eq!(lab.hosts_pending, 1);
    }

    #[test]
    fn large_is_deterministic_and_like_the_measured_setup() {
        let a = large_with_hosts(200, 3);
        let b = large_with_hosts(200, 3);
        assert_eq!(a.summary(), b.summary());
        assert_eq!(
            a.services.iter().map(|s| s.state).collect::<Vec<_>>(),
            b.services.iter().map(|s| s.state).collect::<Vec<_>>()
        );
        assert_ne!(
            large_with_hosts(200, 4)
                .services
                .iter()
                .map(|s| s.state)
                .collect::<Vec<_>>(),
            a.services.iter().map(|s| s.state).collect::<Vec<_>>(),
            "the seed changes the scenario"
        );
        check_consistency(&a);
        let summary = a.summary();
        assert_eq!(summary.hosts, 200);
        assert_eq!(summary.services, 200 * 15);
        let problems =
            summary.services_warning + summary.services_critical + summary.services_unknown;
        #[expect(clippy::cast_precision_loss, reason = "test arithmetic")]
        let ratio = problems as f64 / summary.services as f64;
        assert!((0.035..=0.065).contains(&ratio), "{ratio}");
        for service in &a.services {
            assert!((service.check.check_interval - 300.0).abs() < f64::EPSILON);
            assert!((service.check.retry_interval - 60.0).abs() < f64::EPSILON);
            assert_eq!(service.check.max_attempts, 3);
            assert!(!service.vars.is_empty(), "{}", service.key);
            assert!(service.check.result.is_some(), "{}", service.key);
        }
        let with_perfdata = a
            .services
            .iter()
            .filter(|s| {
                s.check
                    .result
                    .as_ref()
                    .is_some_and(|r| !r.perfdata.is_empty())
            })
            .count();
        assert!(with_perfdata * 10 > a.services.len() * 7, "{with_perfdata}");
        for host in &a.hosts {
            assert!(host.vars.contains_key("runbook"), "{}", host.name);
            assert!((host.check.check_interval - 300.0).abs() < f64::EPSILON);
        }
    }

    #[test]
    fn large_has_two_thousand_hosts_with_fifteen_services() {
        let scenario = large(1);
        let summary = scenario.summary();
        assert_eq!(summary.hosts, 2_000);
        assert_eq!(summary.services, 30_000);
        assert!((5..=40).contains(&summary.hosts_down), "{summary:?}");
    }
}
