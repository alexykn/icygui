//! How much of a host or service to load ([`Detail`]) and the result of a
//! targeted re-query ([`Fetched`]).
//!
//! At production scale (30 000 services) `last_check_result` is about two
//! thirds of a service's bytes, so the initial load asks for services
//! without it ([`Detail::Lean`]) and fetches the full objects only where
//! they're needed (problems, rows on screen, an opened pane). See
//! `docs/performance.md`.

use ic_model::{Host, ObjectKey, Service};

/// Which attributes a host or service query asks for.
///
/// Every attribute in both lists exists in Icinga 2.15 (checked against a
/// real 2.15.6 by the contract tests). Icinga 2.15 and older reject a whole
/// query that names an attribute they don't know (`400 Invalid field
/// specified: …`); newer versions answer it per object. Either way the
/// client leaves that attribute out and asks again (see [`crate::Client`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Detail {
    /// State and scheduling, without the check result and the check
    /// configuration: everything states, handled flags, severity, the
    /// dashboards' filters and the freshness watchdog need.
    ///
    /// - [`CheckInfo::result`](ic_model::CheckInfo::result) is `None`. The
    ///   object is pending ([`ServiceState::Pending`](ic_model::ServiceState::Pending),
    ///   [`HostState::Pending`](ic_model::HostState::Pending)) only if Icinga's
    ///   `last_check` is negative (never checked); otherwise its state is
    ///   known and only its output isn't loaded.
    /// - Of the feature switches only `active_checks` is loaded (active and
    ///   passive checks go stale differently); the others keep their
    ///   defaults, as do `check_command`, `command_endpoint`, `zone`,
    ///   `flapping_current` and the links. Don't overwrite those of a
    ///   [`Detail::Full`] object with a lean one's.
    ///
    /// Attributes: see [`Detail::host_attrs`] and [`Detail::service_attrs`].
    Lean,
    /// Everything the client shows: [`Detail::Lean`] plus
    /// `last_check_result`, `check_command`, `command_endpoint`, `zone`, the
    /// other `enable_*` switches, `flapping_current`, `notes`, `notes_url`,
    /// `action_url` and `icon_image`. Pending means no check result.
    Full,
}

impl Detail {
    /// The attributes a host query with this detail asks for.
    #[must_use]
    pub fn host_attrs(self) -> &'static [&'static str] {
        match self {
            Self::Lean => HOST_LEAN,
            Self::Full => HOST_FULL,
        }
    }

    /// The attributes a service query with this detail asks for.
    #[must_use]
    pub fn service_attrs(self) -> &'static [&'static str] {
        match self {
            Self::Lean => SERVICE_LEAN,
            Self::Full => SERVICE_FULL,
        }
    }
}

/// The result of [`crate::Client::objects`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Fetched {
    /// The hosts that were found.
    pub hosts: Vec<Host>,
    /// The services that were found.
    pub services: Vec<Service>,
    /// The requested objects Icinga doesn't know (any more): deleted, or
    /// hidden from this API user by a filtered `objects/query/*`
    /// permission (Icinga answers both alike). In request order, each once.
    pub missing: Vec<ObjectKey>,
}

/// [`Detail::Lean`] for hosts. Hosts are identified by the result's
/// `name`.
const HOST_LEAN: &[&str] = &[
    "display_name",
    "address",
    "address6",
    "state",
    "state_type",
    "last_state_change",
    "last_hard_state_change",
    "last_check",
    "next_check",
    "check_attempt",
    "max_check_attempts",
    "acknowledgement",
    "acknowledgement_expiry",
    "downtime_depth",
    "flapping",
    "last_reachable",
    "check_interval",
    "retry_interval",
    "enable_active_checks",
    "groups",
    "vars",
];

/// [`Detail::Full`] for hosts.
const HOST_FULL: &[&str] = &[
    "display_name",
    "address",
    "address6",
    "state",
    "state_type",
    "last_state_change",
    "last_hard_state_change",
    "last_check",
    "next_check",
    "check_attempt",
    "max_check_attempts",
    "acknowledgement",
    "acknowledgement_expiry",
    "downtime_depth",
    "flapping",
    "last_reachable",
    "check_interval",
    "retry_interval",
    "enable_active_checks",
    "groups",
    "vars",
    // Full only.
    "last_check_result",
    "check_command",
    "command_endpoint",
    "zone",
    "enable_passive_checks",
    "enable_notifications",
    "enable_event_handler",
    "enable_flapping",
    "enable_perfdata",
    "flapping_current",
    "notes",
    "notes_url",
    "action_url",
    "icon_image",
];

/// [`Detail::Lean`] for services. `host_name` and `name` identify a
/// service even when its name contains `!`.
const SERVICE_LEAN: &[&str] = &[
    "host_name",
    "name",
    "display_name",
    "state",
    "state_type",
    "last_state_change",
    "last_hard_state_change",
    "last_check",
    "next_check",
    "check_attempt",
    "max_check_attempts",
    "acknowledgement",
    "acknowledgement_expiry",
    "downtime_depth",
    "flapping",
    "last_reachable",
    "check_interval",
    "retry_interval",
    "enable_active_checks",
    "groups",
    "vars",
];

/// [`Detail::Full`] for services.
const SERVICE_FULL: &[&str] = &[
    "host_name",
    "name",
    "display_name",
    "state",
    "state_type",
    "last_state_change",
    "last_hard_state_change",
    "last_check",
    "next_check",
    "check_attempt",
    "max_check_attempts",
    "acknowledgement",
    "acknowledgement_expiry",
    "downtime_depth",
    "flapping",
    "last_reachable",
    "check_interval",
    "retry_interval",
    "enable_active_checks",
    "groups",
    "vars",
    // Full only.
    "last_check_result",
    "check_command",
    "command_endpoint",
    "zone",
    "enable_passive_checks",
    "enable_notifications",
    "enable_event_handler",
    "enable_flapping",
    "enable_perfdata",
    "flapping_current",
    "notes",
    "notes_url",
    "action_url",
    "icon_image",
];

/// The attributes only [`Detail::Full`] asks for, the same for hosts and
/// services.
#[cfg(test)]
const FULL_ONLY: &[&str] = &[
    "last_check_result",
    "check_command",
    "command_endpoint",
    "zone",
    "enable_passive_checks",
    "enable_notifications",
    "enable_event_handler",
    "enable_flapping",
    "enable_perfdata",
    "flapping_current",
    "notes",
    "notes_url",
    "action_url",
    "icon_image",
];

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn set(attrs: &[&'static str]) -> BTreeSet<&'static str> {
        attrs.iter().copied().collect()
    }

    #[test]
    fn full_is_lean_plus_the_full_only_attributes() {
        for (lean, full) in [(HOST_LEAN, HOST_FULL), (SERVICE_LEAN, SERVICE_FULL)] {
            assert_eq!(set(lean).len(), lean.len(), "no duplicates");
            assert_eq!(set(full).len(), full.len(), "no duplicates");
            let extra: BTreeSet<_> = set(full).difference(&set(lean)).copied().collect();
            assert_eq!(extra, set(FULL_ONLY));
            assert!(set(lean).is_subset(&set(full)));
        }
    }

    #[test]
    fn lean_leaves_out_the_check_result() {
        for detail in [Detail::Lean, Detail::Full] {
            let full = detail == Detail::Full;
            assert_eq!(detail.service_attrs().contains(&"last_check_result"), full);
            assert_eq!(detail.host_attrs().contains(&"last_check_result"), full);
            // What decides pending in a lean object, and what tells active
            // from passive checks for the freshness watchdog.
            assert!(detail.service_attrs().contains(&"last_check"));
            assert!(detail.service_attrs().contains(&"enable_active_checks"));
        }
    }

    #[test]
    fn hosts_and_services_differ_only_in_their_own_attributes() {
        for detail in [Detail::Lean, Detail::Full] {
            let hosts = set(detail.host_attrs());
            let services = set(detail.service_attrs());
            let host_only: Vec<_> = hosts.difference(&services).copied().collect();
            let service_only: Vec<_> = services.difference(&hosts).copied().collect();
            assert_eq!(host_only, ["address", "address6"]);
            assert_eq!(service_only, ["host_name", "name"]);
        }
    }
}
