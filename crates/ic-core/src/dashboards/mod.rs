//! Dashboard evaluation: every dashboard's filter over the store, kept up
//! to date incrementally.
//!
//! Each dashboard ([`Board`]) keeps its compiled [`Filter`] (recompiled
//! when the filter or the object kind changes) and its *members*: the
//! objects the filter matches, with the facts its rows and summary need
//! (state, severity, handling, when the state began). Membership ignores
//! `problems_only` and `hide_handled`, so recoveries still match (the rule
//! engine's memberships come from here). The visible members are kept in
//! sort order, so a changed object costs a removal and an insertion, and a
//! dashboard whose members' facts didn't change keeps its rows (the same
//! `Arc`, so the UI can skip re-rendering).
//!
//! [`Dashboards::update`] evaluates only the objects that changed since the
//! last snapshot (and a host's services when the host changed, since a
//! service filter can read `host.*`); a reload, a new or changed dashboard
//! and, every [`TIME_REFRESH`], a filter calling `get_time()` are
//! evaluated in full. The engine runs it on a blocking thread.
//!
//! Rows: `problems_only`, then `hide_handled` (Icinga's handled: a problem
//! that is acknowledged, in downtime, or a service whose host has a
//! problem), sorted by the view's key with the ties going to severity
//! (descending), when the state began ([`state_since`]: `last_state_change`,
//! else `last_hard_state_change`; newest first), host name and service
//! name. `GroupBy` puts a header before each group's rows; groups are
//! ordered by their worst severity (descending), then label, and objects
//! without a group come last under "ungrouped". The summary counts every
//! member, before `problems_only` and `hide_handled`; `shown` counts the
//! rows.
//!
//! In quiet mode (PERF-09, [`Scope::Quiet`]) only the dashboards that
//! take part in notification decisions are evaluated, and only their
//! memberships (and sort order) are kept current: their rows and
//! summaries, which nobody sees while quiet, are rebuilt when quiet mode
//! ends; the dashboards left out are evaluated in full then.
//!
//! A filter that doesn't parse, or fails to evaluate for some object (a
//! type error such as `"a" < 1`, an unknown function), sets
//! `DashboardResult.error` and leaves the rows and summary empty, like an
//! Icinga API query with such a filter fails as a whole; the error names
//! the object. The members that matched still count as memberships.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ic_config::{GroupBy, ObjectKind, SortKey, View};
use ic_filter::{Filter, HostScope, ServiceScope};
use ic_model::{
    CheckableState, Host, HostGroup, HostName, ObjectKey, Service, ServiceGroup, ServiceKey,
    Timestamp,
};
use ic_rules::DashboardRef;

use crate::snapshot::{DashboardResult, DashboardRow, Summary, state_since};
use crate::store::Changes;
use crate::summary::Tally;

/// How often dashboards whose filter calls `get_time()` are evaluated in
/// full: their result changes with time, not only with the objects.
pub(crate) const TIME_REFRESH: Duration = Duration::from_secs(30);

/// The label of the group of objects without a group.
pub(crate) const UNGROUPED: &str = "ungrouped";

/// How many evaluations run between two looks at the cancel flag.
const CANCEL_EVERY: usize = 1_024;

/// What an evaluation reads: the objects and groups of one snapshot.
#[derive(Clone, Debug, Default)]
pub(crate) struct Data {
    /// Hosts by name.
    pub(crate) hosts: Arc<BTreeMap<HostName, Arc<Host>>>,
    /// Services by key.
    pub(crate) services: Arc<BTreeMap<ServiceKey, Arc<Service>>>,
    /// Host groups (labels for `GroupBy::HostGroup`).
    pub(crate) host_groups: Arc<Vec<HostGroup>>,
    /// Service groups (labels for `GroupBy::ServiceGroup`).
    pub(crate) service_groups: Arc<Vec<ServiceGroup>>,
    /// What `get_time()` returns.
    pub(crate) now: Timestamp,
}

/// What [`Dashboards::update`] brings up to date.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum Scope {
    /// Every dashboard, rows and summaries included.
    #[default]
    All,
    /// Quiet mode: only the memberships (and sort order) of these
    /// dashboards (`None`: of every one); rows and summaries wait.
    Quiet(Option<BTreeSet<DashboardRef>>),
}

/// Every dashboard of the environment.
#[derive(Debug, Default)]
pub(crate) struct Dashboards {
    boards: Vec<Board>,
    results: Arc<BTreeMap<DashboardRef, DashboardResult>>,
    scope: Scope,
}

impl Dashboards {
    /// Takes the dashboards of `environment`: new ones and ones whose
    /// filter or object kind changed are evaluated in full by the next
    /// [`Dashboards::update`]; changed display settings (problems only,
    /// hide handled, sort, group by) only rebuild the rows; removed ones
    /// are dropped.
    pub(crate) fn configure(&mut self, environment: &ic_config::Environment) {
        let mut old: HashMap<DashboardRef, Board> = self
            .boards
            .drain(..)
            .map(|board| (board.reference.clone(), board))
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
                let board = match old.remove(&reference) {
                    Some(mut board) => {
                        board.reconfigure(&dashboard.view);
                        board
                    }
                    None => Board::new(reference, &dashboard.view),
                };
                self.boards.push(board);
            }
        }
    }

    /// Whether some dashboard's filter calls `get_time()`.
    pub(crate) fn time_dependent(&self) -> bool {
        self.boards
            .iter()
            .any(|board| board.time_dependent && self.evaluates(&board.reference))
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

    /// The latest results.
    #[cfg(test)]
    pub(crate) fn results(&self) -> &Arc<BTreeMap<DashboardRef, DashboardResult>> {
        &self.results
    }

    /// The dashboards whose filter matches `object` (ignoring
    /// `problems_only` and `hide_handled`), as of the last update: the rule
    /// inputs' memberships.
    pub(crate) fn memberships(&self, object: &ObjectKey) -> Vec<DashboardRef> {
        self.boards
            .iter()
            .filter(|board| board.members.contains_key(object))
            .map(|board| board.reference.clone())
            .collect()
    }

    /// Brings every dashboard up to date with `data`: `changes` says what
    /// changed since the last update (everything, with `changes.all`);
    /// `refresh_time` re-evaluates filters that call `get_time()`. Stops
    /// early, leaving the results stale, once `cancel` is set (shutdown).
    /// Returns the results; they are the same `Arc` when nothing changed.
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
        for board in &mut self.boards {
            if cancel.load(Ordering::Relaxed) {
                return Arc::clone(&self.results);
            }
            if only.is_some_and(|only| !only.contains(&board.reference)) {
                // Stale until evaluated again, then in full.
                board.needs_full = true;
                continue;
            }
            if changes.all || board.needs_full || (refresh_time && board.time_dependent) {
                board.evaluate_all(data, cancel);
            } else if !changes.objects.is_empty() {
                let dirty = dirty.get_or_insert_with(|| Dirty::new(data, &changes.objects));
                board.evaluate_some(data, dirty, cancel);
            }
            if changes.groups && board.view.group_by != GroupBy::None {
                board.rows_dirty = true;
            }
            if !members_only {
                any_changed |= board.finish(data);
            }
        }
        let stale = self.results.len() != self.boards.len()
            || self
                .boards
                .iter()
                .any(|board| !self.results.contains_key(&board.reference));
        if any_changed || stale {
            self.results = Arc::new(
                self.boards
                    .iter()
                    .map(|board| (board.reference.clone(), board.result.clone()))
                    .collect(),
            );
        }
        Arc::clone(&self.results)
    }
}

/// Evaluates a view that isn't saved (the dashboard editor's preview) in
/// full over `data`.
///
/// # Errors
///
/// The filter doesn't parse, or fails for some object: the message says
/// where.
pub(crate) fn preview(view: &View, data: &Data) -> Result<DashboardResult, String> {
    let mut board = Board::new(
        DashboardRef {
            group_id: String::new(),
            dashboard_id: String::new(),
        },
        view,
    );
    if let Err(message) = &board.filter {
        return Err(message.clone());
    }
    board.evaluate_all(data, &AtomicBool::new(false));
    board.finish(data);
    match board.result.error {
        Some(message) => Err(message),
        None => Ok(board.result),
    }
}

/// The objects to re-evaluate, worked out once per update for every
/// dashboard.
#[derive(Debug)]
struct Dirty {
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

/// An `f64` with a total order (timestamps never are NaN, but sorting
/// must not depend on it).
#[derive(Clone, Copy, Debug)]
struct OrdF64(f64);

impl PartialEq for OrdF64 {
    fn eq(&self, other: &Self) -> bool {
        self.0.total_cmp(&other.0).is_eq()
    }
}

impl Eq for OrdF64 {}

impl PartialOrd for OrdF64 {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OrdF64 {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&other.0)
    }
}

/// What a dashboard needs to know about a member.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Facts {
    state: CheckableState,
    severity: u32,
    /// When the state began ([`state_since`]), Unix seconds.
    since: f64,
    problem: bool,
    /// Icinga's handled (a problem acknowledged, in downtime, or a service
    /// whose host has a problem).
    handled: bool,
    /// A hash of what `group_by` files the object under (0 without
    /// grouping), so a changed group membership rebuilds the rows.
    groups: u64,
}

/// The primary sort key, already turned around for descending sorts.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Primary {
    Number(OrdF64),
    Host(HostName),
    HostDescending(Reverse<HostName>),
    Name(Arc<str>),
    NameDescending(Reverse<Arc<str>>),
}

/// A visible row's position: the sort key, then the tie-breaks.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct RowKey {
    primary: Primary,
    severity: Reverse<u32>,
    since: Reverse<OrdF64>,
    host: HostName,
    /// The service's name; `None` for hosts.
    service: Option<Arc<str>>,
}

impl RowKey {
    fn new(sort: ic_config::Sort, object: &ObjectKey, facts: &Facts) -> Self {
        let (host, service) = match object {
            ObjectKey::Host { name } => (name.clone(), None),
            ObjectKey::Service { key } => (key.host.clone(), Some(Arc::clone(&key.name))),
        };
        let number =
            |value: f64| Primary::Number(OrdF64(if sort.descending { -value } else { value }));
        let primary = match sort.key {
            SortKey::Severity => number(f64::from(facts.severity)),
            SortKey::LastStateChange => number(facts.since),
            SortKey::Host => host_primary(&host, sort.descending),
            SortKey::Service => match &service {
                Some(name) if sort.descending => Primary::NameDescending(Reverse(Arc::clone(name))),
                Some(name) => Primary::Name(Arc::clone(name)),
                // Host views sort "service" by the host name.
                None => host_primary(&host, sort.descending),
            },
        };
        Self {
            primary,
            severity: Reverse(facts.severity),
            since: Reverse(OrdF64(facts.since)),
            host,
            service,
        }
    }

    fn object(&self) -> ObjectKey {
        match &self.service {
            None => ObjectKey::Host {
                name: self.host.clone(),
            },
            Some(name) => ObjectKey::Service {
                key: ServiceKey {
                    host: self.host.clone(),
                    name: Arc::clone(name),
                },
            },
        }
    }
}

fn host_primary(host: &HostName, descending: bool) -> Primary {
    if descending {
        Primary::HostDescending(Reverse(host.clone()))
    } else {
        Primary::Host(host.clone())
    }
}

/// One dashboard.
#[derive(Debug)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent flags: what changed and what to rebuild"
)]
struct Board {
    reference: DashboardRef,
    view: View,
    /// The compiled filter, or why it doesn't parse.
    filter: Result<Filter, String>,
    /// The filter calls `get_time()`.
    time_dependent: bool,
    /// Matching objects.
    members: HashMap<ObjectKey, Facts>,
    /// Objects the filter failed for, with the error.
    errors: BTreeMap<ObjectKey, String>,
    /// Visible members in display order.
    order: BTreeSet<RowKey>,
    /// Evaluate every object at the next update.
    needs_full: bool,
    /// Recompute visibility and order from the members (display settings
    /// changed).
    restyle: bool,
    rows_dirty: bool,
    summary_dirty: bool,
    result: DashboardResult,
}

impl Board {
    fn new(reference: DashboardRef, view: &View) -> Self {
        let filter = compile(&view.filter);
        Self {
            reference,
            time_dependent: is_time_dependent(&view.filter),
            view: view.clone(),
            filter,
            members: HashMap::new(),
            errors: BTreeMap::new(),
            order: BTreeSet::new(),
            needs_full: true,
            restyle: false,
            rows_dirty: true,
            summary_dirty: true,
            result: DashboardResult::default(),
        }
    }

    fn reconfigure(&mut self, view: &View) {
        if *view == self.view {
            return;
        }
        if view.filter != self.view.filter || view.object_kind != self.view.object_kind {
            *self = Self::new(self.reference.clone(), view);
            return;
        }
        let regroup = view.group_by != self.view.group_by;
        self.view = view.clone();
        if regroup {
            // Members' group hashes depend on the grouping.
            self.needs_full = true;
        } else {
            self.restyle = true;
        }
    }

    fn visible(&self, facts: &Facts) -> bool {
        (!self.view.problems_only || facts.problem) && (!self.view.hide_handled || !facts.handled)
    }

    /// Forgets every member and evaluates every object of the view's kind.
    fn evaluate_all(&mut self, data: &Data, cancel: &AtomicBool) {
        self.members.clear();
        self.errors.clear();
        self.order.clear();
        self.needs_full = false;
        self.restyle = false;
        self.rows_dirty = true;
        self.summary_dirty = true;
        if self.filter.is_err() {
            return;
        }
        match self.view.object_kind {
            ObjectKind::Hosts => {
                for (index, host) in data.hosts.values().enumerate() {
                    if index % CANCEL_EVERY == 0 && cancel.load(Ordering::Relaxed) {
                        return;
                    }
                    self.evaluate_host(data, host.key(), Some(host));
                }
            }
            ObjectKind::Services => {
                for (index, service) in data.services.values().enumerate() {
                    if index % CANCEL_EVERY == 0 && cancel.load(Ordering::Relaxed) {
                        return;
                    }
                    let host = data.hosts.get(&service.key.host).map(Arc::as_ref);
                    self.evaluate_service(data, service.object_key(), Some(service), host);
                }
            }
        }
    }

    /// Re-evaluates the changed objects of the view's kind.
    fn evaluate_some(&mut self, data: &Data, dirty: &Dirty, cancel: &AtomicBool) {
        if self.filter.is_err() {
            return;
        }
        match self.view.object_kind {
            ObjectKind::Hosts => {
                for (index, name) in dirty.hosts.iter().enumerate() {
                    if index % CANCEL_EVERY == 0 && cancel.load(Ordering::Relaxed) {
                        return;
                    }
                    let object = ObjectKey::Host { name: name.clone() };
                    self.evaluate_host(data, object, data.hosts.get(name));
                }
            }
            ObjectKind::Services => {
                for (index, key) in dirty.services.iter().enumerate() {
                    if index % CANCEL_EVERY == 0 && cancel.load(Ordering::Relaxed) {
                        return;
                    }
                    let service = data.services.get(key);
                    let host = data.hosts.get(&key.host).map(Arc::as_ref);
                    self.evaluate_service(data, ObjectKey::from(key.clone()), service, host);
                }
            }
        }
    }

    fn evaluate_host(&mut self, data: &Data, object: ObjectKey, host: Option<&Arc<Host>>) {
        let Some(host) = host else {
            self.forget(&object);
            return;
        };
        let outcome = self.test(&HostScope { host }, data.now);
        let facts = Facts {
            state: CheckableState::Host(host.state),
            severity: host.severity(),
            since: state_since(&host.check).as_unix_seconds(),
            problem: host.is_problem(),
            handled: host.is_handled(),
            groups: self.groups_hash(Some(host), None),
        };
        self.apply(object, outcome, facts);
    }

    fn evaluate_service(
        &mut self,
        data: &Data,
        object: ObjectKey,
        service: Option<&Arc<Service>>,
        host: Option<&Host>,
    ) {
        let Some(service) = service else {
            self.forget(&object);
            return;
        };
        let outcome = self.test(&ServiceScope { service, host }, data.now);
        let host_problem = host.is_some_and(Host::is_problem);
        let facts = Facts {
            state: CheckableState::Service(service.state),
            severity: service.severity(),
            since: state_since(&service.check).as_unix_seconds(),
            problem: service.is_problem(),
            handled: service.is_handled(host_problem),
            groups: self.groups_hash(host, Some(service)),
        };
        self.apply(object, outcome, facts);
    }

    /// Whether the filter matches in `scope`: `Ok(true/false)`, or the
    /// evaluation error.
    fn test(&self, scope: &dyn ic_filter::Scope, now: Timestamp) -> Result<bool, String> {
        match &self.filter {
            Ok(filter) if filter.is_empty() => Ok(true),
            Ok(filter) => filter
                .evaluate_at(scope, now)
                .map(|value| value.is_truthy())
                .map_err(|error| error.message),
            Err(message) => Err(message.clone()),
        }
    }

    fn apply(&mut self, object: ObjectKey, outcome: Result<bool, String>, facts: Facts) {
        match outcome {
            Ok(matched) => {
                if self.errors.remove(&object).is_some() {
                    self.rows_dirty = true;
                }
                self.set(&object, matched.then_some(facts));
            }
            Err(message) => {
                self.set(&object, None);
                if self.errors.insert(object, message).is_none() {
                    self.rows_dirty = true;
                }
            }
        }
    }

    /// An object that is gone: no member, no error.
    fn forget(&mut self, object: &ObjectKey) {
        if self.errors.remove(object).is_some() {
            self.rows_dirty = true;
        }
        self.set(object, None);
    }

    /// Makes `object` a member with `facts`, or no member.
    fn set(&mut self, object: &ObjectKey, facts: Option<Facts>) {
        let old = match facts {
            Some(facts) => self.members.insert(object.clone(), facts),
            None => self.members.remove(object),
        };
        if old == facts {
            return;
        }
        self.summary_dirty = true;
        let sort = self.view.sort;
        let old_row = old
            .filter(|old| self.visible(old))
            .map(|old| RowKey::new(sort, object, &old));
        let new_row = facts
            .filter(|facts| self.visible(facts))
            .map(|facts| RowKey::new(sort, object, &facts));
        let grouped = self.view.group_by != GroupBy::None
            && (old_row.is_some() || new_row.is_some())
            && old.map(|old| old.groups) != facts.map(|facts| facts.groups);
        if old_row != new_row || grouped {
            if let Some(row) = old_row {
                self.order.remove(&row);
            }
            if let Some(row) = new_row {
                self.order.insert(row);
            }
            self.rows_dirty = true;
        }
    }

    /// A hash of the groups `group_by` files an object under.
    fn groups_hash(&self, host: Option<&Host>, service: Option<&Service>) -> u64 {
        let mut hasher = DefaultHasher::new();
        match self.view.group_by {
            GroupBy::None => return 0,
            GroupBy::Host => host.map(|host| &host.display_name).hash(&mut hasher),
            GroupBy::HostGroup => host.map(|host| &host.groups).hash(&mut hasher),
            GroupBy::ServiceGroup => service.map(|service| &service.groups).hash(&mut hasher),
        }
        hasher.finish()
    }

    /// Rebuilds what changed; returns whether the result changed.
    fn finish(&mut self, data: &Data) -> bool {
        if self.restyle {
            self.restyle = false;
            let sort = self.view.sort;
            self.order = self
                .members
                .iter()
                .filter(|(_, facts)| self.visible(facts))
                .map(|(object, facts)| RowKey::new(sort, object, facts))
                .collect();
            self.rows_dirty = true;
            // What is shown changed, so do its counts.
            self.summary_dirty = true;
        }
        if !self.rows_dirty && !self.summary_dirty {
            return false;
        }
        let result = if let Err(message) = &self.filter {
            DashboardResult {
                error: Some(message.clone()),
                ..DashboardResult::default()
            }
        } else if let Some((object, message)) = self.errors.first_key_value() {
            DashboardResult {
                error: Some(evaluation_error(message, object, self.errors.len())),
                ..DashboardResult::default()
            }
        } else {
            let rows = if self.rows_dirty || self.result.error.is_some() {
                Arc::new(self.rows(data))
            } else {
                Arc::clone(&self.result.rows)
            };
            let (summary, shown) = if self.summary_dirty || self.result.error.is_some() {
                self.summaries()
            } else {
                (self.result.summary, self.result.shown)
            };
            DashboardResult {
                rows,
                summary,
                shown,
                error: None,
            }
        };
        self.rows_dirty = false;
        self.summary_dirty = false;
        if result == self.result {
            return false;
        }
        self.result = result;
        true
    }

    /// The counts over every member, and over the members the rows show.
    fn summaries(&self) -> (Summary, Summary) {
        let mut all = Tally::default();
        let mut shown = Tally::default();
        for facts in self.members.values() {
            all.add(facts.state, facts.handled, facts.severity);
            if self.visible(facts) {
                shown.add(facts.state, facts.handled, facts.severity);
            }
        }
        (all.finish(), shown.finish())
    }

    fn rows(&self, data: &Data) -> Vec<DashboardRow> {
        if self.view.group_by == GroupBy::None {
            return self
                .order
                .iter()
                .map(|row| DashboardRow::Object(row.object()))
                .collect();
        }
        let labels = Labels::new(data, self.view.group_by);
        let mut groups: BTreeMap<String, Group> = BTreeMap::new();
        let mut ungrouped = Vec::new();
        for row in &self.order {
            let object = row.object();
            let mut filed = false;
            for (id, label) in labels.of(data, &object) {
                let group = groups.entry(id).or_insert_with(|| Group {
                    label,
                    worst: 0,
                    rows: Vec::new(),
                });
                group.worst = group.worst.max(row.severity.0);
                group.rows.push(object.clone());
                filed = true;
            }
            if !filed {
                ungrouped.push(object);
            }
        }
        let mut groups: Vec<(String, Group)> = groups.into_iter().collect();
        groups.sort_by(|(a_id, a), (b_id, b)| {
            b.worst
                .cmp(&a.worst)
                .then_with(|| a.label.cmp(&b.label))
                .then_with(|| a_id.cmp(b_id))
        });
        let mut rows = Vec::with_capacity(self.order.len() + groups.len() + 1);
        for (_, group) in groups {
            rows.push(DashboardRow::Group {
                label: group.label,
                count: group.rows.len(),
            });
            rows.extend(group.rows.into_iter().map(DashboardRow::Object));
        }
        if !ungrouped.is_empty() {
            rows.push(DashboardRow::Group {
                label: UNGROUPED.to_owned(),
                count: ungrouped.len(),
            });
            rows.extend(ungrouped.into_iter().map(DashboardRow::Object));
        }
        rows
    }
}

/// One group-by group while rows are built.
struct Group {
    label: String,
    worst: u32,
    rows: Vec<ObjectKey>,
}

/// Group ids and labels for `group_by`.
struct Labels<'a> {
    group_by: GroupBy,
    /// Group name → display name.
    display: HashMap<&'a str, &'a str>,
}

impl<'a> Labels<'a> {
    fn new(data: &'a Data, group_by: GroupBy) -> Self {
        let display = match group_by {
            GroupBy::HostGroup => data
                .host_groups
                .iter()
                .map(|group| (group.name.as_str(), group.display_name.as_str()))
                .collect(),
            GroupBy::ServiceGroup => data
                .service_groups
                .iter()
                .map(|group| (group.name.as_str(), group.display_name.as_str()))
                .collect(),
            GroupBy::None | GroupBy::Host => HashMap::new(),
        };
        Self { group_by, display }
    }

    fn label(&self, name: &str) -> String {
        self.display
            .get(name)
            .filter(|label| !label.is_empty())
            .copied()
            .unwrap_or(name)
            .to_owned()
    }

    /// The (id, label) of every group `object` is filed under.
    fn of(&self, data: &Data, object: &ObjectKey) -> Vec<(String, String)> {
        let host_name = object.host_name();
        match self.group_by {
            GroupBy::None => Vec::new(),
            GroupBy::Host => {
                let label = data
                    .hosts
                    .get(host_name)
                    .map(|host| host.display_name.as_str())
                    .filter(|label| !label.is_empty())
                    .unwrap_or(host_name.as_str());
                vec![(host_name.as_str().to_owned(), label.to_owned())]
            }
            GroupBy::HostGroup => data
                .hosts
                .get(host_name)
                .map(|host| group_labels(&host.groups, self))
                .unwrap_or_default(),
            GroupBy::ServiceGroup => object
                .as_service()
                .and_then(|key| data.services.get(key))
                .map(|service| group_labels(&service.groups, self))
                .unwrap_or_default(),
        }
    }
}

fn group_labels(groups: &[String], labels: &Labels<'_>) -> Vec<(String, String)> {
    let mut seen = BTreeSet::new();
    groups
        .iter()
        .filter(|name| seen.insert(name.as_str()))
        .map(|name| (name.clone(), labels.label(name)))
        .collect()
}

/// Parses a dashboard filter; the error names the line and column.
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

fn evaluation_error(message: &str, object: &ObjectKey, count: usize) -> String {
    match count {
        0 | 1 => format!("{message} (for {object})"),
        2 => format!("{message} (for {object} and 1 other object)"),
        _ => format!("{message} (for {object} and {} other objects)", count - 1),
    }
}

#[cfg(test)]
mod tests;
