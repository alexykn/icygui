//! The read-only state the UI renders. The runtime publishes a new
//! [`Snapshot`] after each batch of changes; the UI never mutates it.
//!
//! Collections are `Arc`-shared so publishing a snapshot is cheap and the UI
//! can hold on to an old one while the runtime builds the next.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use ic_model::{
    CheckInfo, CheckableState, Comment, Dependency, Downtime, Endpoint, Host, HostGroup, HostName,
    InstanceStatus, Notification, Notified, ObjectKey, Service, ServiceGroup, ServiceKey,
    Timestamp, Zone,
};
use ic_rules::DashboardRef;

use crate::command::LogEntry;
use crate::topology::{ClusterNode, ConnectedNode, cluster_nodes};

/// Everything known about an environment at one point in time.
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
    /// The cluster's zones (each with its endpoints and parent; empty
    /// without `objects/query/Zone`): [`Snapshot::cluster_nodes`] reads
    /// them.
    pub zones: Arc<Vec<Zone>>,
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
    /// The event stream carries no check results (quiet mode, PERF-09):
    /// outputs, last-check times and late markers may be stale, and a
    /// stream can be silent for many minutes, so the age of the last event
    /// says nothing about the connection's health.
    pub quiet: bool,
    /// Hosts and services whose full details are being fetched: the object
    /// the user opened ([`crate::Command::Focus`]), rows hydrating, a
    /// notified object's prefetch, problems refreshed after waking up. A
    /// pane shows an "updating" hint when its object stays here for a
    /// moment (about 300 ms).
    pub updating: Arc<BTreeSet<ObjectKey>>,
    /// The event log's latest entries, newest first (at most a thousand),
    /// from the local log the engine already keeps: what event streams
    /// show. The cluster section's *events*
    /// shows them for the whole environment ([`crate::stream_events`]).
    /// The same `Arc` while no event is added.
    pub events: Arc<Vec<LogEntry>>,
}

impl Snapshot {
    /// The cluster's masters and satellites with their state as the node
    /// the objects come from sees it ([`crate::topology::cluster_nodes`]):
    /// the switcher's node list.
    #[must_use]
    pub fn cluster_nodes(&self) -> Vec<ClusterNode> {
        cluster_nodes(&self.zones, &self.endpoints, self.node.as_deref())
    }

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

    /// Whether a host's or service's full details are being fetched (see
    /// [`Snapshot::updating`]).
    #[must_use]
    pub fn is_updating(&self, object: &ObjectKey) -> bool {
        self.updating.contains(object)
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

/// A dashboard evaluated by the runtime: every view's result, and the
/// dashboard's counts for the sidebar.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DashboardResult {
    /// Counts over the objects of the views that count problems
    /// ([`ic_config::View::counts_problems`]: every view but an event
    /// stream), each object once even when several views show it: the
    /// sidebar's count (`unhandled`) and dot (`worst_unhandled`), and the
    /// tray's.
    pub summary: Summary,
    /// One result per view, in the dashboard's order.
    pub views: Vec<ViewResult>,
}

impl DashboardResult {
    /// The result of the view with this id ([`ic_config::View::id`]).
    /// Match views by id, not position: after an edit, the next result may
    /// still be on its way.
    #[must_use]
    pub fn view(&self, id: &str) -> Option<&ViewResult> {
        self.views.iter().find(|view| view.id == id)
    }

    /// The first view's result: all there is of a single-view dashboard
    /// (every dashboard from rc1).
    #[must_use]
    pub fn first(&self) -> Option<&ViewResult> {
        self.views.first()
    }
}

/// One view's result.
///
/// The counts, for a list of service problems:
/// - [`ViewResult::summary`] counts every object the filter matches (the
///   editor's *8 matches, 2 handled*);
/// - [`ViewResult::counts`] counts the unhandled ones the view is about
///   (after *problems only*): the view header's and the summary bar's
///   per-state counts, which *show* and *hide* never change;
/// - [`ViewResult::shown`] counts the rows;
/// - [`ViewResult::hidden`] and [`ViewResult::handled`] drive the `N
///   hidden · show` / `N handled · hide` button.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ViewResult {
    /// The view's id ([`ic_config::View::id`]).
    pub id: String,
    /// Counts over every object the view's filter matches (a grid: its
    /// hosts, with their services when it colours by the worst of both;
    /// an event stream: none).
    pub summary: Summary,
    /// The per-state counts the view's header shows: the objects it is
    /// about (a list: after *problems only*; a grid: one per host, in the
    /// state of its square) that don't count as handled. Showing or hiding
    /// handled problems never changes them.
    pub counts: Summary,
    /// Counts over the rows of a list (after *problems only* and the
    /// handled switches); other displays: the same as `counts`.
    pub shown: Summary,
    /// How many objects the view's handled switches hide (lists only):
    /// `N hidden · show`. 0 when it hides none.
    pub hidden: u32,
    /// How many of the objects the view is about count as handled
    /// (acknowledged, in downtime, or a service of a host that is down;
    /// the hollow marks), hidden or not: `N handled · hide` when they show.
    pub handled: u32,
    /// What the view shows.
    pub body: ViewBody,
    /// An event stream: how many hosts its filter matches, whose events it
    /// shows (the editor's `valid · 9 hosts and their services`); 0 for the
    /// other displays.
    pub hosts: u32,
    /// An event stream: how many services its filter matches; 0 for the
    /// other displays.
    pub services: u32,
    /// The filter didn't parse or evaluate: the body is empty, the error
    /// names the object.
    pub error: Option<String>,
}

impl ViewResult {
    /// A list's rows (empty for the other displays).
    #[must_use]
    pub fn rows(&self) -> &[DashboardRow] {
        match &self.body {
            ViewBody::List(rows) => rows,
            _ => &[],
        }
    }

    /// A list's rows as the shared `Arc`, which stays the same while the
    /// rows don't change (`None` for the other displays).
    #[must_use]
    pub fn list_rows(&self) -> Option<&Arc<Vec<DashboardRow>>> {
        match &self.body {
            ViewBody::List(rows) => Some(rows),
            _ => None,
        }
    }

    /// Whether the view has nothing to show (its header says *nothing to
    /// show*). A handling or downtimes view's body is what its filter
    /// matches; whether any of it is being handled is the app's to say.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        match &self.body {
            ViewBody::List(rows) => rows.is_empty(),
            ViewBody::Grid(grid) => grid.groups.is_empty(),
            ViewBody::Tiles(tiles) => tiles.is_empty(),
            ViewBody::Stream(events) => events.is_empty(),
            ViewBody::Members(members) => members.is_empty(),
        }
    }

    /// A handling or downtimes view's members: the hosts and services its
    /// filter matches (`None` for the other displays).
    #[must_use]
    pub fn members(&self) -> Option<&Arc<BTreeSet<ObjectKey>>> {
        match &self.body {
            ViewBody::Members(members) => Some(members),
            _ => None,
        }
    }
}

/// What a view shows, by its display. Each part is an `Arc` that stays the
/// same while it doesn't change, so the UI can skip re-rendering it.
#[derive(Clone, Debug, PartialEq)]
pub enum ViewBody {
    /// A list's or grouped list's rows (filtered, sorted, grouped).
    List(Arc<Vec<DashboardRow>>),
    /// A host-group grid.
    Grid(Arc<Grid>),
    /// Summary tiles, in the view's order.
    Tiles(Arc<Vec<Tile>>),
    /// An event stream's events, newest first (at most
    /// [`STREAM_EVENTS`]).
    Stream(Arc<Vec<LogEntry>>),
    /// A handling or downtimes view (topic 14): the hosts and services its
    /// filter matches. The app builds the threads and the timeline from
    /// the snapshot's acknowledgements, downtimes and comments of these
    /// objects; nothing more is fetched.
    Members(Arc<BTreeSet<ObjectKey>>),
}

impl Default for ViewBody {
    fn default() -> Self {
        Self::List(Arc::default())
    }
}

/// The most events an event stream view holds; it scrolls past its
/// `lines`.
pub const STREAM_EVENTS: usize = 200;

/// A host-group grid (topic 05).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Grid {
    /// The groups, in the view's order (worst first, or by name).
    pub groups: Vec<GridGroup>,
    /// How many hosts the grid shows (each once).
    pub hosts: u32,
}

/// One group of a host-group grid: a host group, or a custom variable's
/// value.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GridGroup {
    /// The host group's name, or the variable's value: what a click
    /// filters the page by.
    pub name: String,
    /// The host group's display name, or the value.
    pub label: String,
    /// One per host, by host name.
    pub cells: Vec<GridCell>,
    /// The cells that don't count as handled, by their state: the group
    /// header's `● 1 ● 3` and its dot (`worst_unhandled`, the reddest
    /// count).
    pub counts: Summary,
}

/// One host of a grid.
#[derive(Clone, Debug, PartialEq)]
pub struct GridCell {
    /// The host.
    pub host: HostName,
    /// What the square shows: the worst unhandled problem of the host and
    /// (colouring by the worst of both) its services; else the worst
    /// handled one; else the host's own state (an OK host is dim green).
    pub state: CheckableState,
    /// Drawn hollow: the state comes from a handled problem, or the host
    /// has nothing wrong but is in downtime (an OK host in downtime is a
    /// hollow green square).
    pub handled: bool,
    /// Unhandled problems of the host and the services that colour it: the
    /// tooltip's problem count.
    pub problems: u32,
    /// The worst service that is not OK, when a service gives the cell its
    /// state: the tooltip's and a labelled cell's service.
    pub worst_service: Option<Arc<str>>,
}

/// One tile of a summary tiles view: a host group or a custom variable's
/// value, with its counts.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tile {
    /// The host group's name, or the variable's value: what a click
    /// filters the page by.
    pub name: String,
    /// The host group's display name, or the value.
    pub label: String,
    /// How many hosts the tile's objects are on (`3 hosts`).
    pub hosts: u32,
    /// Counts over every one of the tile's objects.
    pub summary: Summary,
    /// The tile's objects that aren't handled problems, by their state
    /// (OK and pending included): its stacked bar and numbers, which add up
    /// to the view header's unhandled counts, and its dot
    /// (`worst_unhandled`, the reddest count).
    pub counts: Summary,
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

/// Counts over objects: a dashboard's for the sidebar
/// ([`DashboardResult::summary`]), a view's ([`ViewResult::summary`],
/// [`ViewResult::counts`], [`ViewResult::shown`]: `12 critical · 29
/// warning · 24 unknown`), a grid group's or a tile's.
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
