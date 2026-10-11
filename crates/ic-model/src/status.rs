//! Instance status from `/v1/status` (read-only; the client never changes
//! Icinga's global switches).

use serde::{Deserialize, Serialize};

use crate::state::ServiceState;
use crate::time::Timestamp;

/// What the connected Icinga instance reports about itself.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "mirrors Icinga's independent global feature switches"
)]
pub struct InstanceStatus {
    /// The endpoint's node name (`master-01`).
    pub node_name: String,
    /// Icinga version string (`r2.15.0-1`).
    pub version: String,
    /// When the Icinga process started.
    pub program_start: Timestamp,
    /// Global `enable_notifications`.
    pub notifications_enabled: bool,
    /// Global `enable_host_checks`.
    pub host_checks_enabled: bool,
    /// Global `enable_service_checks`.
    pub service_checks_enabled: bool,
    /// Global `enable_event_handlers`.
    pub event_handlers_enabled: bool,
    /// Global `enable_flapping`.
    pub flap_detection_enabled: bool,
    /// Global `enable_perfdata`.
    pub perfdata_enabled: bool,
    /// Active host and service checks in the last minute.
    pub checks_per_minute: f64,
    /// Passive host and service check results in the last minute.
    #[serde(default)]
    pub passive_checks_per_minute: f64,
    /// Average check latency in seconds.
    pub avg_latency: f64,
    /// The largest check latency in seconds.
    #[serde(default)]
    pub max_latency: f64,
    /// Average check execution time in seconds.
    pub avg_execution_time: f64,
    /// The longest check execution time in seconds.
    #[serde(default)]
    pub max_execution_time: f64,
    /// How many hosts and services Icinga has, by state (`/v1/status/CIB`):
    /// the size of the installation before any object is loaded, and the
    /// state counts quiet mode's stall check compares between polls.
    pub counts: ObjectCounts,
}

/// Icinga's object counts by state (`/v1/status/CIB`, `num_hosts_*` and
/// `num_services_*`). Every host is up, down or unreachable (Icinga counts
/// an unreachable host there whatever its state); every service is ok,
/// warning, critical or unknown by its raw state, which is unknown until
/// its first check (so a pending service is counted as unknown, and in
/// `services_pending`; see [`ObjectCounts::service_index`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ObjectCounts {
    /// Reachable hosts that are up.
    pub hosts_up: u32,
    /// Reachable hosts that are down.
    pub hosts_down: u32,
    /// Unreachable hosts.
    pub hosts_unreachable: u32,
    /// Hosts without a check result yet.
    pub hosts_pending: u32,
    /// Services that are ok.
    pub services_ok: u32,
    /// Services in warning.
    pub services_warning: u32,
    /// Services that are critical.
    pub services_critical: u32,
    /// Services in unknown.
    pub services_unknown: u32,
    /// Services without a check result yet.
    pub services_pending: u32,
}

impl ObjectCounts {
    /// Every host.
    #[must_use]
    pub fn hosts(&self) -> u32 {
        self.hosts_up
            .saturating_add(self.hosts_down)
            .saturating_add(self.hosts_unreachable)
    }

    /// Every service.
    #[must_use]
    pub fn services(&self) -> u32 {
        self.services_ok
            .saturating_add(self.services_warning)
            .saturating_add(self.services_critical)
            .saturating_add(self.services_unknown)
    }

    /// The services by state (ok, warning, critical, unknown): each
    /// service's state change is a `StateChange` event, so these only move
    /// with one (unlike the hosts', which also move when a parent's outage
    /// makes a host unreachable without an event of its own).
    #[must_use]
    pub fn service_states(&self) -> [u32; 4] {
        [
            self.services_ok,
            self.services_warning,
            self.services_critical,
            self.services_unknown,
        ]
    }

    /// Where [`ObjectCounts::service_states`] counts a service in `state`
    /// (0 ok, 1 warning, 2 critical, 3 unknown): Icinga counts by the raw
    /// `state`, which is 3 (unknown) until the first check result, so a
    /// pending service is counted as unknown (checked against Icinga 2.15
    /// by the contract tests).
    #[must_use]
    pub fn service_index(state: ServiceState) -> usize {
        match state {
            ServiceState::Ok => 0,
            ServiceState::Warning => 1,
            ServiceState::Critical => 2,
            ServiceState::Unknown | ServiceState::Pending => 3,
        }
    }
}

/// A cluster endpoint's numbers as the node icygui talks to sees them
/// (`Endpoint` attributes `icinga_version`, `last_message_received`,
/// `messages_received_per_second`, `messages_sent_per_second` and
/// `connecting`): the cluster health page's endpoint columns (topic 06).
/// Icinga reports zeros for the node's own endpoint and for an endpoint
/// it never talked to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EndpointStats {
    /// Icinga's version as a number (`21506` for 2.15.6); 0 when unknown
    /// (never connected, or the node itself).
    pub version: u32,
    /// When the last message from it arrived; the epoch when none did.
    pub last_message: Timestamp,
    /// Messages received from it per second.
    pub messages_in: f64,
    /// Messages sent to it per second.
    pub messages_out: f64,
    /// A connection to it is being set up.
    pub connecting: bool,
}

/// An Icinga version (`2.15.6`), from the number an endpoint reports
/// (`21506`) or the string `/v1/status` reports (`v2.15.6`,
/// `r2.14.3-1`).
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct Version {
    /// Major version.
    pub major: u32,
    /// Minor version.
    pub minor: u32,
    /// Patch level.
    pub patch: u32,
}

impl Version {
    /// The version an endpoint's `icinga_version` names (`21506` is
    /// 2.15.6: major × 10 000 + minor × 100 + patch); `None` for 0.
    #[must_use]
    pub fn from_number(number: u32) -> Option<Self> {
        (number > 0).then_some(Self {
            major: number / 10_000,
            minor: number / 100 % 100,
            patch: number % 100,
        })
    }

    /// The version in Icinga's version string (`v2.15.6`, `r2.14.3-1`,
    /// `2.15.0-12-gabcdef`): the first three numbers after an optional
    /// `v` or `r`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim().trim_start_matches(['v', 'r']);
        let core = text.split(['-', ' ', '+']).next()?;
        let mut parts = core.split('.').map(str::parse::<u32>);
        let major = parts.next()?.ok()?;
        let minor = parts.next()?.ok()?;
        let patch = parts.next().and_then(Result::ok).unwrap_or(0);
        Some(Self {
            major,
            minor,
            patch,
        })
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// What `/v1/status/ApiListener` reports about the node's cluster and API
/// connections and its JSON-RPC queues (Icinga 2.15 reports no length for
/// the work queue, only its rate).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ListenerStatus {
    /// Open HTTP (API) connections (`api.http.clients`).
    pub http_clients: u32,
    /// Endpoints the node should be connected to (its own zone, the
    /// parent zone and the child zones; `num_endpoints`).
    pub endpoints: u32,
    /// Those it is connected to (`num_conn_endpoints`).
    pub connected_endpoints: u32,
    /// Messages waiting to be relayed to other zones
    /// (`json_rpc.relay_queue_items`).
    pub relay_queue: f64,
    /// Messages relayed per second over the last minute
    /// (`json_rpc.relay_queue_item_rate`).
    pub relay_rate: f64,
    /// Configuration sync messages waiting (`json_rpc.sync_queue_items`).
    pub sync_queue: f64,
    /// JSON-RPC messages processed per second over the last minute
    /// (`json_rpc.work_queue_item_rate`).
    pub work_queue_rate: f64,
}

/// Whether one of the node's features runs (its object exists: the
/// feature is enabled), and whether it is paused there: a feature with
/// high availability runs on one master of a zone at a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FeatureState {
    /// Not enabled on this node.
    Off,
    /// Enabled and running here.
    Running,
    /// Enabled, but paused here (another master of the zone runs it).
    Paused,
}

/// The features of the node icygui talks to that its cluster health
/// page shows (`CheckerComponent`, `NotificationComponent` and `IcingaDB`
/// objects); `None` where the API user may not read them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeFeatures {
    /// The checker (`checker` feature).
    pub checker: Option<FeatureState>,
    /// Notifications (`notification` feature).
    pub notification: Option<FeatureState>,
    /// The `IcingaDB` writer (`icingadb` feature; icygui never reads it).
    pub icingadb: Option<FeatureState>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pending_service_counts_as_unknown_like_in_icinga() {
        let index = ObjectCounts::service_index;
        assert_eq!(index(ServiceState::Ok), 0);
        assert_eq!(index(ServiceState::Warning), 1);
        assert_eq!(index(ServiceState::Critical), 2);
        assert_eq!(index(ServiceState::Unknown), 3);
        assert_eq!(index(ServiceState::Pending), 3);
    }

    #[test]
    fn versions_read_like_icinga_writes_them() {
        let v = |major, minor, patch| Version {
            major,
            minor,
            patch,
        };
        assert_eq!(Version::from_number(21506), Some(v(2, 15, 6)));
        assert_eq!(Version::from_number(0), None);
        assert_eq!(Version::parse("v2.15.6"), Some(v(2, 15, 6)));
        assert_eq!(Version::parse("r2.14.3-1"), Some(v(2, 14, 3)));
        assert_eq!(Version::parse("2.15.0-12-gabcdef"), Some(v(2, 15, 0)));
        assert_eq!(Version::parse("v2.16"), Some(v(2, 16, 0)));
        assert_eq!(Version::parse(""), None);
        assert!(v(2, 14, 2) < v(2, 14, 3));
        assert_eq!(v(2, 14, 3).to_string(), "2.14.3");
    }
}
