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
/// client leaves that attribute out and asks again (see [`crate::Client`]
/// and [`crate::Client::unknown_attributes`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Detail {
    /// Everything except the check result and the links: identity,
    /// states, scheduling, handled flags, the check configuration
    /// (`check_command`, `command_endpoint`, `zone`, intervals), every
    /// `enable_*` switch, `flapping_current`, groups and custom variables.
    /// So states, severity, the freshness watchdog and dashboard or rule
    /// filters on any of these are exact for lean objects. (No event
    /// carries the check configuration, the switches or
    /// `flapping_current`: a lean object without them would never learn
    /// them.)
    ///
    /// - [`CheckInfo::result`](ic_model::CheckInfo::result) is `None`. The
    ///   object is pending ([`ServiceState::Pending`](ic_model::ServiceState::Pending),
    ///   [`HostState::Pending`](ic_model::HostState::Pending)) only if Icinga's
    ///   `last_check` is negative (never checked); otherwise its state is
    ///   known and only its output isn't loaded. `CheckResult` events bring
    ///   the result.
    /// - [`Links`](ic_model::Links) (`notes`, `notes_url`, `action_url`,
    ///   `icon_image`) keep their defaults: free text, possibly long, that
    ///   only a pane shows.
    /// - Filters on `last_check_result` or the links see `null` and `""`
    ///   until a `CheckResult` event (the result) or a [`Detail::Full`]
    ///   fetch brings them. Don't overwrite a full object's result and
    ///   links with a lean one's.
    ///
    /// Attributes: see [`Detail::host_attrs`] and [`Detail::service_attrs`].
    Lean,
    /// Everything the client shows: [`Detail::Lean`] plus
    /// `last_check_result`, `notes`, `notes_url`, `action_url` and
    /// `icon_image`. Pending means no check result.
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
    "flapping_current",
    "check_command",
    "check_interval",
    "retry_interval",
    "command_endpoint",
    "zone",
    "enable_active_checks",
    "enable_passive_checks",
    "enable_notifications",
    "enable_event_handler",
    "enable_flapping",
    "enable_perfdata",
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
    "flapping_current",
    "check_command",
    "check_interval",
    "retry_interval",
    "command_endpoint",
    "zone",
    "enable_active_checks",
    "enable_passive_checks",
    "enable_notifications",
    "enable_event_handler",
    "enable_flapping",
    "enable_perfdata",
    "groups",
    "vars",
    // Full only.
    "last_check_result",
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
    "flapping_current",
    "check_command",
    "check_interval",
    "retry_interval",
    "command_endpoint",
    "zone",
    "enable_active_checks",
    "enable_passive_checks",
    "enable_notifications",
    "enable_event_handler",
    "enable_flapping",
    "enable_perfdata",
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
    "flapping_current",
    "check_command",
    "check_interval",
    "retry_interval",
    "command_endpoint",
    "zone",
    "enable_active_checks",
    "enable_passive_checks",
    "enable_notifications",
    "enable_event_handler",
    "enable_flapping",
    "enable_perfdata",
    "groups",
    "vars",
    // Full only.
    "last_check_result",
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
    fn lean_leaves_out_the_check_result_and_the_links() {
        for detail in [Detail::Lean, Detail::Full] {
            let full = detail == Detail::Full;
            for attr in FULL_ONLY {
                assert_eq!(detail.service_attrs().contains(attr), full, "{attr}");
                assert_eq!(detail.host_attrs().contains(attr), full, "{attr}");
            }
            // What decides pending in a lean object, and what tells active
            // from passive checks for the freshness watchdog.
            assert!(detail.service_attrs().contains(&"last_check"));
            assert!(detail.service_attrs().contains(&"enable_active_checks"));
        }
    }

    /// The stored attributes dashboard and rule filters can use
    /// (`docs/architecture.md`, ic-filter: "Object attributes"; `problem`,
    /// `handled`, `severity` and `__name` are derived from these).
    const FILTER_ATTRIBUTES: &[&str] = &[
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
        "flapping_current",
        "last_reachable",
        "check_command",
        "check_interval",
        "retry_interval",
        "command_endpoint",
        "zone",
        "enable_active_checks",
        "enable_passive_checks",
        "enable_notifications",
        "enable_event_handler",
        "enable_flapping",
        "enable_perfdata",
        "groups",
        "vars",
        "notes",
        "notes_url",
        "action_url",
        "icon_image",
        "last_check_result",
    ];

    /// Check events never carry the check configuration or the switches,
    /// so a lean object must have every filter attribute except those a
    /// full fetch brings: the check result and the links.
    #[test]
    fn lean_has_every_filter_attribute_but_the_result_and_the_links() {
        let missing_when_lean: BTreeSet<&str> = FILTER_ATTRIBUTES
            .iter()
            .copied()
            .filter(|attr| *attr != "name")
            .filter(|attr| !SERVICE_LEAN.contains(attr))
            .collect();
        assert_eq!(missing_when_lean, set(FULL_ONLY));
        let missing_when_lean: BTreeSet<&str> = FILTER_ATTRIBUTES
            .iter()
            .copied()
            .filter(|attr| !HOST_LEAN.contains(attr))
            .collect();
        // A host's name is the query result's `name`, not an attribute.
        let mut expected = set(FULL_ONLY);
        expected.insert("name");
        assert_eq!(missing_when_lean, expected);
        for attr in FILTER_ATTRIBUTES.iter().filter(|attr| **attr != "name") {
            assert!(SERVICE_FULL.contains(attr), "{attr}");
            assert!(HOST_FULL.contains(attr), "{attr}");
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
