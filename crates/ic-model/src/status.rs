//! Instance status from `/v1/status` (read-only; the client never changes
//! Icinga's global switches).

use serde::{Deserialize, Serialize};

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
}
