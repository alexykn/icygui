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
    /// Average check latency in seconds.
    pub avg_latency: f64,
    /// Average check execution time in seconds.
    pub avg_execution_time: f64,
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
}
