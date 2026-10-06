//! The read-only state the UI renders. The runtime publishes a new
//! [`Snapshot`] after each batch of changes; the UI never mutates it.
//!
//! Collections are `Arc`-shared so publishing a snapshot is cheap and the UI
//! can hold on to an old one while the runtime builds the next.

use std::collections::BTreeMap;
use std::sync::Arc;

use ic_model::{
    CheckInfo, CheckableState, Comment, Dependency, Downtime, Endpoint, Host, HostGroup, HostName,
    InstanceStatus, Notification, Notified, ObjectKey, Service, ServiceGroup, ServiceKey,
    Timestamp,
};
use ic_rules::DashboardRef;

use crate::topology::ConnectedNode;

/// Everything known about the active environment at one point in time.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    /// Increases with every published snapshot.
    pub revision: u64,
    /// When this snapshot was built.
    pub taken_at: Timestamp,
    /// Hosts by name.
    pub hosts: Arc<BTreeMap<HostName, Arc<Host>>>,
    /// Services by key.
    pub services: Arc<BTreeMap<ServiceKey, Arc<Service>>>,
    /// Comments by object, oldest first.
    pub comments: Arc<BTreeMap<ObjectKey, Vec<Comment>>>,
    /// Downtimes by object, by start time.
    pub downtimes: Arc<BTreeMap<ObjectKey, Vec<Downtime>>>,
    /// Host groups.
    pub host_groups: Arc<Vec<HostGroup>>,
    /// Service groups.
    pub service_groups: Arc<Vec<ServiceGroup>>,
    /// Dependencies (parents and children of hosts and services).
    pub dependencies: Arc<Vec<Dependency>>,
    /// Cluster endpoints.
    pub endpoints: Arc<Vec<Endpoint>>,
    /// Instance status; `None` until first fetched.
    pub status: Option<Arc<InstanceStatus>>,
    /// Icinga's own `Notification` objects (who Icinga notified, and when)
    /// by host or service, each list by name. They load in the background
    /// once the problem lists are complete and follow Icinga's
    /// `Notification` events; empty without the `objects/query/Notification`
    /// permission. [`Snapshot::notified`] combines them for the panes'
    /// "notified" row. (The client's own desktop notifications are
    /// something else: `CoreEvent::Notification`.)
    pub icinga_notifications: Arc<BTreeMap<ObjectKey, Arc<[Notification]>>>,
    /// Evaluated dashboards.
    pub dashboards: Arc<BTreeMap<DashboardRef, DashboardResult>>,
    /// When the latest event-stream message arrived (local clock); `None`
    /// before the first. The footer shows its age (`master-01 · 2s`).
    pub last_event_at: Option<Timestamp>,
    /// Counts over every host and service, for the tray icon and its
    /// tooltip.
    pub overall: Summary,
    /// Hosts and services whose check is late: Icinga still reported them
    /// overdue when the freshness watchdog asked (or when a reload brought
    /// them), with the deadline they missed (Icinga's `next_update`, on
    /// Icinga's clock). The UI marks them ("late 12m"). An object leaves
    /// the map as soon as a check result moves its deadline.
    pub late: Arc<BTreeMap<ObjectKey, Timestamp>>,
    /// The node the objects come from (ENV-12): the connected node from
    /// the start of the first load into an empty store, once the engine
    /// went live on objects loaded from a node with the same view (the
    /// other master of an HA zone), or once a load from it completed.
    /// After a switch to a node with another view (a satellite while the
    /// masters are down, or back) it stays the old node until the reload
    /// from the new one is complete, so a partial view never looks
    /// complete. `None` before the first connect.
    pub node: Option<Arc<ConnectedNode>>,
}

impl Snapshot {
    /// The host a service runs on.
    #[must_use]
    pub fn host_of(&self, service: &ServiceKey) -> Option<&Arc<Host>> {
        self.hosts.get(&service.host)
    }

    /// Services of a host, in name order.
    pub fn services_of<'a>(&'a self, host: &'a HostName) -> impl Iterator<Item = &'a Arc<Service>> {
        self.services
            .range(
                ServiceKey {
                    host: host.clone(),
                    name: Arc::from(""),
                }..,
            )
            .take_while(move |(key, _)| &key.host == host)
            .map(|(_, service)| service)
    }

    /// Who Icinga notified about a host or service, and when (PANE-06):
    /// its `Notification` objects combined (the latest `last_notification`,
    /// every user in `notified_problem_users`). [`Notified::is_never`]
    /// means "not notified".
    #[must_use]
    pub fn notified(&self, object: &ObjectKey) -> Notified {
        self.icinga_notifications
            .get(object)
            .map(|list| Notified::of(list.iter()))
            .unwrap_or_default()
    }

    /// Whether a host's or service's check is late (see [`Snapshot::late`]).
    #[must_use]
    pub fn is_late(&self, object: &ObjectKey) -> bool {
        self.late.contains_key(object)
    }
}

/// When an object's current state began, for "time in state" and the
/// *last state change* sort: Icinga's `last_state_change`, or, when that
/// is 0, `last_hard_state_change`. Icinga 2 reports `last_state_change = 0`
/// for objects that have been in their state since their first check
/// (a host that came up down), while `last_hard_state_change` has the
/// time. [`Timestamp::EPOCH`] when neither is known (pending objects).
#[must_use]
pub fn state_since(check: &CheckInfo) -> Timestamp {
    check
        .last_state_change
        .non_zero()
        .or_else(|| check.last_hard_state_change.non_zero())
        .unwrap_or(Timestamp::EPOCH)
}

/// A dashboard's rows and counts, evaluated by the runtime.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DashboardResult {
    /// Rows in display order (filtered, sorted, grouped).
    pub rows: Arc<Vec<DashboardRow>>,
    /// Counts over every matching object (before `problems_only` and
    /// `hide_handled`): the sidebar's count and dot.
    pub summary: Summary,
    /// Counts over the objects `rows` lists (after `problems_only` and
    /// `hide_handled`): the summary bar, which reads as the list's size.
    pub shown: Summary,
    /// The filter didn't parse or evaluate; `rows` is empty.
    pub error: Option<String>,
}

/// One row of a dashboard list.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum DashboardRow {
    /// A group header (`group_by`), with the number of rows under it.
    Group {
        /// Group label (host name, group display name, or "ungrouped").
        label: String,
        /// Rows in the group.
        count: usize,
    },
    /// A host or service.
    Object(ObjectKey),
}

/// Counts over a dashboard's objects: all matching ones
/// ([`DashboardResult::summary`], the sidebar) or the listed ones
/// ([`DashboardResult::shown`], the summary bar: `12 critical · 29 warning
/// · 24 unknown`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Summary {
    /// Critical services.
    pub critical: u32,
    /// Warning services.
    pub warning: u32,
    /// Unknown services.
    pub unknown: u32,
    /// Down hosts.
    pub down: u32,
    /// Unreachable hosts.
    pub unreachable: u32,
    /// OK services and up hosts.
    pub ok: u32,
    /// Pending objects.
    pub pending: u32,
    /// Problems that are handled.
    pub handled: u32,
    /// Problems that are not handled: the sidebar count.
    pub unhandled: u32,
    /// The worst unhandled state: the sidebar dot. `None` if nothing is
    /// unhandled.
    pub worst_unhandled: Option<CheckableState>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_state_began_at_the_last_state_change_or_the_last_hard_one() {
        let at = Timestamp::from_unix_seconds;
        let mut check = CheckInfo {
            last_state_change: at(200.0),
            last_hard_state_change: at(100.0),
            last_check: Some(at(300.0)),
            ..CheckInfo::default()
        };
        assert_eq!(state_since(&check), at(200.0));
        // What Icinga 2.15 reports for a host that came up down.
        check.last_state_change = Timestamp::EPOCH;
        assert_eq!(state_since(&check), at(100.0));
        // Never changed: unknown, not the last check.
        check.last_hard_state_change = Timestamp::EPOCH;
        assert_eq!(state_since(&check), Timestamp::EPOCH);
    }
}
