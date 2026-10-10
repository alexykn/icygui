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
mod heartbeats;
mod lab;
mod large;
mod prod_cluster;
mod staging;

use ic_model::{
    CheckableState, Comment, Dependency, Downtime, Endpoint, Host, HostGroup, HostState,
    InstanceStatus, ObjectKey, Service, ServiceGroup, ServiceState, StateType, Timestamp,
};

pub(crate) use build::format_perfdata;
pub use build::raw_check_result;
pub use heartbeats::HEARTBEAT_VARIABLE;
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
    /// Icinga's own `Notification` objects: who Icinga notifies about which
    /// host or service. The server sends them like Icinga does (hard state
    /// changes, recoveries to the users told about the problem) and
    /// reports them as `Notification` events.
    pub notifications: Vec<Notification>,
    /// The reference time of every timestamp in this scenario.
    pub time_base: Timestamp,
    /// The time a server moves `time_base` to when it loads the scenario:
    /// `None` (the default) for the server's start, so "critical for 14
    /// minutes" holds whenever it starts. [`Scenario::for_node`] sets it,
    /// so that the servers of one cluster agree on every timestamp (Icinga
    /// replicates `last_state_change` and the check results across the
    /// cluster).
    pub anchor: Option<Timestamp>,
    /// Objects the simulator never changes, so a demo keeps showing them.
    pub pinned: Vec<ObjectKey>,
    /// Services the server checks in real time at their `check_interval`,
    /// whether the simulation runs or not (heartbeats,
    /// [`Scenario::with_heartbeats`]): OK, or UNKNOWN while the endpoint
    /// they are pinned to (`command_endpoint`) isn't connected.
    pub realtime: Vec<ObjectKey>,
    /// The node's features the cluster health page shows (checker and
    /// notification by default): their objects exist, their status
    /// functions report them.
    pub features: ScenarioFeatures,
    /// Endpoints running another Icinga version than the scenario's
    /// (`status.version`): endpoint name and version string.
    pub endpoint_versions: Vec<(String, String)>,
}

/// Which of the node's features are enabled (each has one object named
/// like the feature: `checker`, `notification`, `icingadb`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScenarioFeatures {
    /// The `checker` feature.
    pub checker: bool,
    /// The `notification` feature.
    pub notification: bool,
    /// The `icingadb` feature.
    pub icingadb: bool,
}

impl Default for ScenarioFeatures {
    /// A master's usual features: checker and notification.
    fn default() -> Self {
        Self {
            checker: true,
            notification: true,
            icingadb: false,
        }
    }
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

/// An Icinga `Notification` object (`lib/icinga/notification.ti`): who
/// Icinga notifies about one host or service, and what it last did.
#[derive(Clone, Debug, PartialEq)]
pub struct Notification {
    /// Short name (`mail-oncall`); the full name is `host!name` or
    /// `host!service!name`.
    pub name: String,
    /// The host or service it belongs to.
    pub object: ObjectKey,
    /// The `NotificationCommand` (`mail-service-notification`).
    pub command: String,
    /// Users notified directly (`users`).
    pub users: Vec<String>,
    /// User groups whose members are notified too (`user_groups`).
    pub user_groups: Vec<String>,
    /// When Icinga last sent it (`last_notification`); `None` = never.
    pub last_notification: Option<Timestamp>,
    /// The users told about the current problem (`notified_problem_users`).
    pub notified_problem_users: Vec<String>,
}

impl Notification {
    /// Icinga's full name: `host!name` or `host!service!name`.
    #[must_use]
    pub fn full_name(&self) -> String {
        format!("{}!{}", self.object.full_name(), self.name)
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
                passive_checks_per_minute: 0.0,
                avg_latency: 0.0,
                max_latency: 0.0,
                avg_execution_time: 0.0,
                max_execution_time: 0.0,
                counts: ic_model::ObjectCounts::default(),
            },
            zones: Vec::new(),
            users: Vec::new(),
            notifications: Vec::new(),
            time_base: Timestamp::now(),
            anchor: None,
            pinned: Vec::new(),
            realtime: Vec::new(),
            features: ScenarioFeatures::default(),
            endpoint_versions: Vec::new(),
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

    /// Adds a `Notification` called `name` to every host and service, like
    /// `apply Notification "<name>" to Host` and `… to Service`: `users`
    /// picks the recipients per object (none: no notification for it).
    /// Hard problems that Icinga would have notified (unhandled, reachable,
    /// not flapping) already did, shortly after their hard state began.
    pub fn apply_notification(
        &mut self,
        name: &str,
        mut users: impl FnMut(&ObjectKey) -> Vec<String>,
    ) {
        let now = self.time_base.as_unix_seconds();
        let in_downtime = |object: &ObjectKey| {
            self.downtimes.iter().any(|downtime| {
                &downtime.object == object
                    && downtime.start_time.as_unix_seconds() <= now
                    && now < downtime.end_time.as_unix_seconds()
                    && (downtime.fixed || downtime.trigger_time.is_some())
            })
        };
        let host_problems: std::collections::HashSet<&str> = self
            .hosts
            .iter()
            .filter(|host| host.state.is_problem())
            .map(|host| host.name.as_str())
            .collect();
        let mut added = Vec::new();
        let checkables = self
            .hosts
            .iter()
            .map(|host| {
                (
                    host.key(),
                    CheckableState::Host(host.state),
                    &host.check,
                    false,
                )
            })
            .chain(self.services.iter().map(|service| {
                let host_problem = host_problems.contains(service.key.host.as_str());
                (
                    service.object_key(),
                    CheckableState::Service(service.state),
                    &service.check,
                    host_problem,
                )
            }));
        for (object, state, check, host_problem) in checkables {
            let recipients = users(&object);
            if recipients.is_empty() {
                continue;
            }
            let notified = state.is_problem()
                && check.state_type == StateType::Hard
                && check.features.notifications
                && check.reachable
                && !check.flapping
                && !check.acknowledgement.is_acknowledged()
                && !host_problem
                && !in_downtime(&object);
            let command = if object.as_service().is_some() {
                "mail-service-notification"
            } else {
                "mail-host-notification"
            };
            added.push(Notification {
                name: name.to_owned(),
                command: command.to_owned(),
                last_notification: notified.then(|| {
                    Timestamp::from_unix_seconds(
                        check.last_hard_state_change.as_unix_seconds() + 1.5,
                    )
                }),
                notified_problem_users: if notified {
                    recipients.clone()
                } else {
                    Vec::new()
                },
                users: recipients,
                user_groups: Vec::new(),
                object,
            });
        }
        self.notifications.extend(added);
    }

    /// The scenario as the cluster node `node_name` serves it, so that
    /// several mock servers form one cluster: a single master, an HA pair
    /// in one zone (both serve everything), or a master with a child zone.
    ///
    /// `/v1/status` reports `node_name`; the zones and endpoints stay as
    /// they are (every node knows the zone tree around it). A node in a
    /// top-level zone serves every object. A node in a child zone serves
    /// only the hosts and services whose `zone` is its zone or one below it
    /// (Icinga's config sync gives a satellite only those), with their
    /// comments, downtimes, Icinga notifications and the dependencies
    /// between them. A `node_name` that isn't one of the scenario's
    /// endpoints serves everything.
    ///
    /// Each server runs its own copy: changes made through one aren't seen
    /// by the others (Icinga's cluster would replicate them). The copies
    /// share one [`Scenario::anchor`] (the scenario's own, else its
    /// `time_base`), so every node reports the same timestamps however far
    /// apart the servers start.
    #[must_use]
    pub fn for_node(&self, node_name: &str) -> Self {
        let mut scenario = self.clone();
        scenario.anchor = Some(self.anchor.unwrap_or(self.time_base));
        node_name.clone_into(&mut scenario.status.node_name);
        let Some(zone) = self
            .endpoints
            .iter()
            .find(|endpoint| endpoint.name == node_name)
            .map(|endpoint| endpoint.zone.clone())
        else {
            return scenario;
        };
        let has_parent = self
            .zones
            .iter()
            .any(|candidate| candidate.name == zone && candidate.parent.is_some());
        if !has_parent {
            return scenario;
        }
        // The node's zone and every zone below it.
        let mut below = std::collections::BTreeSet::from([zone]);
        loop {
            let children: Vec<String> = self
                .zones
                .iter()
                .filter(|candidate| {
                    !below.contains(&candidate.name)
                        && candidate
                            .parent
                            .as_ref()
                            .is_some_and(|parent| below.contains(parent))
                })
                .map(|candidate| candidate.name.clone())
                .collect();
            if children.is_empty() {
                break;
            }
            below.extend(children);
        }
        let in_zone = |zone: Option<&String>| zone.is_some_and(|zone| below.contains(zone));
        scenario
            .hosts
            .retain(|host| in_zone(host.check.zone.as_ref()));
        let hosts: std::collections::BTreeSet<_> = scenario
            .hosts
            .iter()
            .map(|host| host.name.clone())
            .collect();
        scenario
            .services
            .retain(|service| hosts.contains(&service.key.host));
        let services: std::collections::BTreeSet<_> = scenario
            .services
            .iter()
            .map(|service| service.key.clone())
            .collect();
        let kept = |object: &ObjectKey| match object {
            ObjectKey::Host { name } => hosts.contains(name),
            ObjectKey::Service { key } => services.contains(key),
        };
        scenario.comments.retain(|comment| kept(&comment.object));
        scenario.downtimes.retain(|downtime| kept(&downtime.object));
        scenario
            .dependencies
            .retain(|dependency| kept(&dependency.child) && kept(&dependency.parent));
        scenario
            .notifications
            .retain(|notification| kept(&notification.object));
        scenario.pinned.retain(|object| kept(object));
        scenario.realtime.retain(|object| kept(object));
        scenario
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
        // The design's 12, plus topic 07's acknowledged problems
        // (pg-autovacuum, disk /srv).
        assert!(
            (11..=16).contains(&summary.services_critical),
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

    #[test]
    fn nodes_serve_their_zone_and_below() {
        let mut cluster = prod_cluster();
        cluster.endpoints.push(Endpoint {
            name: "master-02".to_owned(),
            zone: "master".to_owned(),
            connected: true,
        });
        // A top-level node serves everything, under its own name.
        for node in ["master-01", "master-02", "not-an-endpoint"] {
            let served = cluster.for_node(node);
            assert_eq!(served.status.node_name, node);
            assert_eq!(served.hosts.len(), cluster.hosts.len(), "{node}");
            assert_eq!(served.services.len(), cluster.services.len(), "{node}");
            assert_eq!(served.zones, cluster.zones);
            assert_eq!(served.endpoints, cluster.endpoints);
            // Every node's server reports the same timestamps.
            assert_eq!(served.anchor, Some(cluster.time_base), "{node}");
        }

        // The satellite serves the hosts in `ams` and nothing else.
        let satellite = cluster.for_node("sat-ams-01");
        assert_eq!(satellite.status.node_name, "sat-ams-01");
        assert!(!satellite.hosts.is_empty());
        assert!(satellite.hosts.len() < cluster.hosts.len());
        assert!(
            satellite
                .hosts
                .iter()
                .all(|host| host.check.zone.as_deref() == Some("ams"))
        );
        let hosts: HashSet<_> = satellite.hosts.iter().map(|host| &host.name).collect();
        assert!(
            satellite
                .services
                .iter()
                .all(|service| hosts.contains(&service.key.host))
        );
        assert_eq!(
            satellite.services.len(),
            cluster
                .services
                .iter()
                .filter(|service| hosts.contains(&service.key.host))
                .count()
        );
        assert!(
            satellite
                .comments
                .iter()
                .all(|comment| hosts.contains(comment.object.host_name()))
        );
        assert!(
            satellite
                .notifications
                .iter()
                .all(|notification| hosts.contains(notification.object.host_name()))
        );
        assert_eq!(satellite.zones, cluster.zones);
        check_consistency(&satellite);
    }
}
