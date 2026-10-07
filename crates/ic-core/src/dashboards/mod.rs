//! Dashboard evaluation: every view of every dashboard over the store,
//! kept up to date incrementally.
//!
//! A dashboard is a list of views (v1, topic 04), each with its own
//! display, filter and options ([`ic_config::View`]). Each view
//! ([`board::Board`]) keeps its compiled [`Filter`](ic_filter::Filter)
//! (recompiled when what decides its members changes: the filter, the
//! object kind, a grid's or tiles' groups) and its *members*: the objects
//! the filter matches, with the facts its body and counts need (state,
//! severity, why it counts as handled, when the state began). Membership
//! ignores *problems only* and the handled switches, so recoveries still
//! match (the rule engine's memberships come from here). A list's visible
//! members are kept in sort order, so a changed object costs a removal and
//! an insertion, and a view whose members' facts didn't change keeps its
//! body (the same `Arc`, so the UI can skip re-rendering).
//!
//! [`Dashboards::update`] evaluates only the objects that changed since the
//! last snapshot (and a host's services when the host changed, since a
//! service filter can read `host.*`); a reload, a new or changed view and,
//! every [`TIME_REFRESH`], a filter calling `get_time()` are evaluated in
//! full. The engine runs it on a blocking thread.
//!
//! The displays:
//!
//! - *List* and *grouped list*: rows after *problems only*, then the
//!   handled switches ([`ic_config::HideHandled`]: acknowledged, in
//!   downtime, services of hosts that are down; the view's own or the
//!   settings' defaults), sorted by the view's key with the ties going to
//!   severity (descending), when the state began ([`state_since`]:
//!   `last_state_change`, else `last_hard_state_change`; newest first),
//!   host name and service name. A grouped list puts a header before each
//!   group's rows; groups are ordered by their worst severity
//!   (descending), then label, and objects without a group come last under
//!   "ungrouped".
//! - *Host-group grid*: the member hosts (those its filter matches in the
//!   groups it shows), a square per host and group, coloured by the worst
//!   unhandled problem of the host and (by default) its services, else the
//!   worst handled one (hollow), else the host's state.
//! - *Summary tiles*: a tile per group with the counts of the members in
//!   it.
//! - *Event stream*: the recent events of the hosts and services its
//!   filter matches, newest first, from the event log's latest entries
//!   ([`Data::events`]).
//!
//! The counts: a view's `summary` counts every member; its `counts` the
//! unhandled objects it is about (the header's numbers, which showing or
//! hiding handled problems never changes); a list's `shown` its rows. A
//! dashboard's `summary` (the sidebar's count and dot) counts the members
//! of every view but event streams, each object once; its memberships are
//! the same objects.
//!
//! In quiet mode (PERF-09, [`Scope::Quiet`]) only the dashboards that take
//! part in notification decisions are evaluated, and only their
//! memberships (and sort order) are kept current: their bodies and counts,
//! which nobody sees while quiet, are rebuilt when quiet mode ends; the
//! dashboards left out are evaluated in full then. A dashboard that has no
//! result yet (quiet since the start, or new or refiltered while quiet)
//! has no result until then, rather than an empty one: it is being
//! evaluated, not empty.
//!
//! A filter that doesn't parse, or fails to evaluate for some object (a
//! type error such as `"a" < 1`, an unknown function), sets the view's
//! `error` and leaves its body and counts empty, like an Icinga API query
//! with such a filter fails as a whole; the error names the object. The
//! members that matched still count as memberships.

mod board;
mod groups;
mod stream;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ic_config::{HideHandled, View};
use ic_filter::Filter;
use ic_model::{
    Host, HostGroup, HostName, ObjectKey, Service, ServiceGroup, ServiceKey, Timestamp,
};
use ic_rules::DashboardRef;

use self::board::Board;
use crate::command::LogEntry;
use crate::snapshot::{DashboardResult, Snapshot, Summary};
use crate::store::Changes;
use crate::summary::Tally;

/// How often dashboards whose filter calls `get_time()` are evaluated in
/// full: their result changes with time, not only with the objects.
pub(crate) const TIME_REFRESH: Duration = Duration::from_secs(30);

/// The label of the group of objects without a group.
pub(crate) const UNGROUPED: &str = "ungrouped";

/// How many of the event log's latest entries the engine keeps for event
/// stream views.
pub(crate) const RECENT_EVENTS: usize = 1_000;

/// How many evaluations run between two looks at the cancel flag.
const CANCEL_EVERY: usize = 1_024;

/// What an evaluation reads: the objects and groups of one snapshot, and
/// the latest events.
#[derive(Clone, Debug, Default)]
pub(crate) struct Data {
    /// Hosts by name.
    pub(crate) hosts: Arc<BTreeMap<HostName, Arc<Host>>>,
    /// Services by key.
    pub(crate) services: Arc<BTreeMap<ServiceKey, Arc<Service>>>,
    /// Host groups (labels for grouping by host group).
    pub(crate) host_groups: Arc<Vec<HostGroup>>,
    /// Service groups (labels for grouping by service group).
    pub(crate) service_groups: Arc<Vec<ServiceGroup>>,
    /// What `get_time()` returns.
    pub(crate) now: Timestamp,
    /// The event log's latest entries, newest first (at most
    /// [`RECENT_EVENTS`]): what event stream views show. A new `Arc` when
    /// they change.
    pub(crate) events: Arc<Vec<LogEntry>>,
}

impl Data {
    /// The objects and groups of `snapshot`, with `events`.
    pub(crate) fn of_snapshot(snapshot: &Snapshot, events: Arc<Vec<LogEntry>>) -> Self {
        Self {
            hosts: Arc::clone(&snapshot.hosts),
            services: Arc::clone(&snapshot.services),
            host_groups: Arc::clone(&snapshot.host_groups),
            service_groups: Arc::clone(&snapshot.service_groups),
            now: snapshot.taken_at,
            events,
        }
    }
}

/// What [`Dashboards::update`] brings up to date.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum Scope {
    /// Every dashboard, bodies and counts included.
    #[default]
    All,
    /// Quiet mode: only the memberships (and sort order) of these
    /// dashboards (`None`: of every one); bodies and counts wait.
    Quiet(Option<BTreeSet<DashboardRef>>),
}

/// Every dashboard of the environment.
#[derive(Debug, Default)]
pub(crate) struct Dashboards {
    all: Vec<Dashboard>,
    results: Arc<BTreeMap<DashboardRef, DashboardResult>>,
    scope: Scope,
    /// The settings' handled defaults, which views follow unless they set
    /// their own.
    defaults: HideHandled,
}

/// One dashboard: its views and its result.
#[derive(Debug)]
struct Dashboard {
    reference: DashboardRef,
    boards: Vec<Board>,
    result: DashboardResult,
    /// The views changed (added, removed, reordered): rebuild the result
    /// even if no view's own result changed.
    reshaped: bool,
    /// A dashboard with several views that count: every object they hold,
    /// each once, with what its counts need, kept up to date with the
    /// changed objects (the sidebar's counts).
    union: HashMap<ObjectKey, Counted>,
    /// Which views `union` was built from (their ids, while their filters
    /// worked); `None`: build it again.
    union_of: Option<Vec<String>>,
}

/// What a dashboard's counts need of an object.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Counted {
    pub(super) state: ic_model::CheckableState,
    pub(super) handled: bool,
    pub(super) severity: u32,
}

impl Dashboard {
    fn new(reference: DashboardRef, views: &[View], defaults: HideHandled) -> Self {
        Self {
            reference,
            boards: views
                .iter()
                .map(|view| Board::new(view, defaults))
                .collect(),
            result: DashboardResult::default(),
            reshaped: true,
            union: HashMap::new(),
            union_of: None,
        }
    }

    /// Takes the dashboard's views: views are matched by id, so reordered
    /// ones keep their state; new ones are evaluated in full.
    fn reconfigure(&mut self, views: &[View], defaults: HideHandled) {
        let same_views = self.boards.len() == views.len()
            && self
                .boards
                .iter()
                .zip(views)
                .all(|(board, view)| board.view.id == view.id);
        let mut old: HashMap<String, Board> = self
            .boards
            .drain(..)
            .map(|board| (board.view.id.clone(), board))
            .collect();
        let mut seen = HashSet::new();
        for view in views {
            let board = match old
                .remove(&view.id)
                .filter(|_| seen.insert(view.id.clone()))
            {
                Some(mut board) => {
                    board.reconfigure(view, defaults);
                    board
                }
                None => Board::new(view, defaults),
            };
            self.boards.push(board);
        }
        self.reshaped |= !same_views;
    }

    /// Whether every view has a body and counts.
    fn finished(&self) -> bool {
        self.boards.iter().all(|board| board.finished)
    }

    /// Whether some view that counts holds `object`.
    fn contains(&self, object: &ObjectKey) -> bool {
        self.boards
            .iter()
            .any(|board| board.counts() && board.contains(object))
    }

    /// Rebuilds the result from the views' results and the union.
    fn rebuild(&mut self) {
        self.reshaped = false;
        self.result = DashboardResult {
            summary: self.summary(),
            views: self
                .boards
                .iter()
                .map(|board| board.result.clone())
                .collect(),
        };
    }

    /// The views whose objects count: they count problems and their
    /// filters work (a view whose filter fails counts nothing, as its own
    /// counts don't).
    fn counting(&self) -> impl Iterator<Item = &Board> {
        self.boards
            .iter()
            .filter(|board| board.counts() && board.is_sound())
    }

    /// Brings the union of the counting views' objects up to date: in
    /// full when the counting views changed (or `dirty` is `None`: a view
    /// was evaluated in full), else for the objects in `dirty`. Only a
    /// dashboard with several counting views keeps one. Returns whether
    /// it changed.
    fn update_union(&mut self, data: &Data, dirty: Option<&Dirty>) -> bool {
        let ids: Vec<String> = self.counting().map(|board| board.view.id.clone()).collect();
        if ids.len() < 2 {
            let had = !self.union.is_empty();
            self.union = HashMap::new();
            self.union_of = None;
            return had;
        }
        let rebuild = self.union_of.as_ref() != Some(&ids) || dirty.is_none();
        if rebuild {
            let mut union = HashMap::with_capacity(self.union.len());
            for board in self.counting() {
                board.for_each_counted(data, |object, counted| {
                    if !union.contains_key(object) {
                        union.insert(object.clone(), counted);
                    }
                });
            }
            self.union = union;
            self.union_of = Some(ids);
            return true;
        }
        let Some(dirty) = dirty else {
            return false;
        };
        let mut changed = false;
        let objects = dirty
            .hosts
            .iter()
            .map(|name| ObjectKey::Host { name: name.clone() })
            .chain(dirty.services.iter().cloned().map(ObjectKey::from));
        for object in objects {
            let counted = self
                .counting()
                .find_map(|board| board.counted(data, &object));
            let before = match counted {
                Some(counted) => self.union.insert(object, counted),
                None => self.union.remove(&object),
            };
            changed |= before != counted;
        }
        changed
    }

    /// The counts of every object the counting views hold, each once.
    fn summary(&self) -> Summary {
        let mut counting = self.counting();
        match (counting.next(), counting.next()) {
            (None, _) => Summary::default(),
            (Some(board), None) => board.result.summary,
            (Some(_), Some(_)) => {
                let mut tally = Tally::default();
                for counted in self.union.values() {
                    tally.add(counted.state, counted.handled, counted.severity);
                }
                tally.finish()
            }
        }
    }
}

impl Dashboards {
    /// Takes the dashboards of `environment` and the settings' handled
    /// defaults: new views and ones whose members depend on something
    /// that changed are evaluated in full by the next
    /// [`Dashboards::update`]; changed display settings (problems only,
    /// the handled switches, sort, a display's options) only rebuild their
    /// bodies and counts; removed ones are dropped.
    pub(crate) fn configure(
        &mut self,
        environment: &ic_config::Environment,
        defaults: HideHandled,
    ) {
        self.defaults = defaults;
        let mut old: HashMap<DashboardRef, Dashboard> = self
            .all
            .drain(..)
            .map(|dashboard| (dashboard.reference.clone(), dashboard))
            .collect();
        let mut seen = BTreeSet::new();
        for group in &environment.groups {
            for dashboard in &group.dashboards {
                let reference = DashboardRef {
                    group_id: group.id.clone(),
                    dashboard_id: dashboard.id.clone(),
                };
                if !seen.insert(reference.clone()) {
                    tracing::warn!(
                        group = %group.name,
                        dashboard = %dashboard.name,
                        "two dashboards share an id; showing the first"
                    );
                    continue;
                }
                let state = match old.remove(&reference) {
                    Some(mut state) => {
                        state.reconfigure(&dashboard.views, defaults);
                        state
                    }
                    None => Dashboard::new(reference, &dashboard.views, defaults),
                };
                self.all.push(state);
            }
        }
    }

    /// Whether some view's filter calls `get_time()`.
    pub(crate) fn time_dependent(&self) -> bool {
        self.all.iter().any(|dashboard| {
            self.evaluates(&dashboard.reference)
                && dashboard.boards.iter().any(|board| board.time_dependent)
        })
    }

    /// Whether some view is an event stream (new events then need an
    /// evaluation).
    pub(crate) fn has_streams(&self) -> bool {
        self.all.iter().any(|dashboard| {
            dashboard
                .boards
                .iter()
                .any(|board| board.view.display == ic_config::ViewDisplay::EventStream)
        })
    }

    /// What the next updates bring up to date (see [`Scope`]). A dashboard
    /// left out keeps its last result and is evaluated in full when it is
    /// evaluated again.
    pub(crate) fn set_scope(&mut self, scope: Scope) {
        self.scope = scope;
    }

    fn evaluates(&self, reference: &DashboardRef) -> bool {
        match &self.scope {
            Scope::All | Scope::Quiet(None) => true,
            Scope::Quiet(Some(only)) => only.contains(reference),
        }
    }

    /// The latest results: every dashboard's, but for one quiet mode
    /// hasn't built a result for yet.
    #[cfg(test)]
    pub(crate) fn results(&self) -> &Arc<BTreeMap<DashboardRef, DashboardResult>> {
        &self.results
    }

    /// The dashboards one of whose views that count problems holds
    /// `object` (ignoring *problems only* and the handled switches), as of
    /// the last update: the rule inputs' memberships.
    pub(crate) fn memberships(&self, object: &ObjectKey) -> Vec<DashboardRef> {
        self.all
            .iter()
            .filter(|dashboard| dashboard.contains(object))
            .map(|dashboard| dashboard.reference.clone())
            .collect()
    }

    /// Brings every dashboard up to date with `data`: `changes` says what
    /// changed since the last update (everything, with `changes.all`);
    /// `refresh_time` re-evaluates filters that call `get_time()`. Stops
    /// early, leaving the results stale, once `cancel` is set (shutdown).
    /// Returns the results; they are the same `Arc` when nothing changed.
    /// A dashboard whose result was never built (quiet mode) is left out.
    pub(crate) fn update(
        &mut self,
        data: &Data,
        changes: &Changes,
        refresh_time: bool,
        cancel: &AtomicBool,
    ) -> Arc<BTreeMap<DashboardRef, DashboardResult>> {
        let mut dirty: Option<Dirty> = None;
        let mut any_changed = false;
        let (only, members_only) = match &self.scope {
            Scope::All => (None, false),
            Scope::Quiet(only) => (only.as_ref(), true),
        };
        for dashboard in &mut self.all {
            let skipped = only.is_some_and(|only| !only.contains(&dashboard.reference));
            let mut view_changed = false;
            let mut evaluated_all = false;
            for board in &mut dashboard.boards {
                if cancel.load(Ordering::Relaxed) {
                    return Arc::clone(&self.results);
                }
                // Quiet mode keeps only what notifications decide on.
                if skipped || (members_only && !board.counts()) {
                    // Stale until evaluated again, then in full.
                    board.needs_full = true;
                    continue;
                }
                if changes.all || board.needs_full || (refresh_time && board.time_dependent) {
                    board.evaluate_all(data, cancel);
                    evaluated_all = true;
                } else if !changes.objects.is_empty() {
                    let dirty = dirty.get_or_insert_with(|| Dirty::new(data, &changes.objects));
                    board.evaluate_some(data, dirty, cancel);
                }
                if changes.groups && board.uses_group_labels() {
                    board.rows_dirty = true;
                }
                if !members_only {
                    view_changed |= board.finish(data);
                }
            }
            if members_only || skipped {
                continue;
            }
            let union_changed = if evaluated_all {
                dashboard.update_union(data, None)
            } else if let Some(dirty) = &dirty {
                dashboard.update_union(data, Some(dirty))
            } else {
                // Nothing changed but, maybe, which views count.
                dashboard.update_union(data, Some(&Dirty::default()))
            };
            if (view_changed || union_changed || dashboard.reshaped) && dashboard.finished() {
                dashboard.rebuild();
                any_changed = true;
            }
        }
        // A dashboard quiet mode never finished has no result yet: an
        // empty one would read as "nothing matches" until quiet mode ends.
        let finished = || self.all.iter().filter(|dashboard| dashboard.finished());
        let stale = self.results.len() != finished().count()
            || finished().any(|dashboard| !self.results.contains_key(&dashboard.reference));
        if any_changed || stale {
            self.results = Arc::new(
                finished()
                    .map(|dashboard| (dashboard.reference.clone(), dashboard.result.clone()))
                    .collect(),
            );
        }
        Arc::clone(&self.results)
    }
}

/// Evaluates views that aren't saved (the dashboard editor's preview) in
/// full over `data`, as one dashboard. A view whose filter fails has its
/// `error` set.
pub(crate) fn preview(views: &[View], data: &Data, defaults: HideHandled) -> DashboardResult {
    let mut dashboard = Dashboard::new(
        DashboardRef {
            group_id: String::new(),
            dashboard_id: String::new(),
        },
        views,
        defaults,
    );
    let cancel = AtomicBool::new(false);
    for board in &mut dashboard.boards {
        board.evaluate_all(data, &cancel);
        board.finish(data);
    }
    dashboard.update_union(data, None);
    dashboard.rebuild();
    dashboard.result
}

/// Evaluates a dashboard's `views` over `snapshot` in full, exactly as the
/// engine does: for views without an engine (tests, fixtures). `events`
/// are the event log's latest entries, newest first, for event stream
/// views; `defaults` the settings' handled defaults.
#[must_use]
pub fn evaluate_dashboard(
    views: &[View],
    snapshot: &Snapshot,
    events: &[LogEntry],
    defaults: HideHandled,
) -> DashboardResult {
    let data = Data::of_snapshot(snapshot, Arc::new(events.to_vec()));
    preview(views, &data, defaults)
}

/// The objects to re-evaluate, worked out once per update for every view.
#[derive(Debug, Default)]
pub(crate) struct Dirty {
    hosts: Vec<HostName>,
    /// Changed services, and every service of a changed host (its filter
    /// may read `host.*`, and its handling depends on the host's state).
    services: BTreeSet<ServiceKey>,
}

impl Dirty {
    fn new(data: &Data, objects: &BTreeSet<ObjectKey>) -> Self {
        let mut hosts = Vec::new();
        let mut services = BTreeSet::new();
        for object in objects {
            match object {
                ObjectKey::Host { name } => {
                    hosts.push(name.clone());
                    services.extend(
                        data.services
                            .range(service_range(name))
                            .take_while(|(key, _)| &key.host == name)
                            .map(|(key, _)| key.clone()),
                    );
                }
                ObjectKey::Service { key } => {
                    services.insert(key.clone());
                }
            }
        }
        Self { hosts, services }
    }
}

/// The keys of `host`'s services start here.
fn service_range(host: &HostName) -> std::ops::RangeFrom<ServiceKey> {
    ServiceKey {
        host: host.clone(),
        name: Arc::from(""),
    }..
}

/// Parses a view's filter; the error names the line and column.
fn compile(source: &str) -> Result<Filter, String> {
    Filter::parse(source).map_err(|error| {
        let (line, column) = error.line_column(source);
        format!("{} (line {line}, column {column})", error.message)
    })
}

/// Whether a filter's result depends on the time (it calls `get_time()`).
fn is_time_dependent(source: &str) -> bool {
    source.contains("get_time")
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod view_tests;
