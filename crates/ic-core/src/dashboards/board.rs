//! One view of a dashboard, evaluated incrementally: its compiled filter,
//! its members with the facts its body and counts need, and its result.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ic_config::{
    GridColour, GroupBy, HideHandled, ObjectKind, SortKey, StateChip, View, ViewDisplay,
};
use ic_filter::{Filter, HostScope, ServiceScope};
use ic_model::{
    CheckableState, Host, HostName, HostState, ObjectKey, Service, ServiceKey, ServiceState,
    Timestamp,
};

use super::groups::{self, Picker};
use super::{
    CANCEL_EVERY, Counted, Data, Dirty, UNGROUPED, compile, is_time_dependent, service_range,
};
use crate::command::LogEntry;
use crate::snapshot::{DashboardRow, Summary, ViewBody, ViewResult, state_since};
use crate::summary::Tally;

/// Why an object counts as handled (the hollow marks), as bits of
/// [`Facts::reasons`]: each is one of the settings' *hide* switches.
pub(super) const ACKNOWLEDGED: u8 = 1;
/// A downtime is in effect, whatever the state.
pub(super) const IN_DOWNTIME: u8 = 2;
/// A service with a problem whose host is down or unreachable.
pub(super) const HOST_DOWN: u8 = 4;

/// The reasons the switches in `hidden` hide.
fn mask(hidden: HideHandled) -> u8 {
    (if hidden.acknowledged { ACKNOWLEDGED } else { 0 })
        | (if hidden.in_downtime { IN_DOWNTIME } else { 0 })
        | (if hidden.host_down { HOST_DOWN } else { 0 })
}

/// What a view needs to know about a member.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Facts {
    pub(super) state: CheckableState,
    pub(super) severity: u32,
    /// When the state began ([`state_since`]), Unix seconds.
    pub(super) since: f64,
    pub(super) problem: bool,
    /// Counts as handled (the hollow marks): Icinga's handled (a problem
    /// acknowledged, in downtime, or a service whose host has a problem),
    /// or a downtime in effect whatever the state. The same as
    /// `reasons != 0`.
    pub(super) handled: bool,
    /// Why it counts as handled ([`ACKNOWLEDGED`], [`IN_DOWNTIME`],
    /// [`HOST_DOWN`]).
    pub(super) reasons: u8,
    /// A hash of the groups the view files the object under (0 without
    /// grouping), so a changed group membership rebuilds the body.
    pub(super) groups: u64,
}

/// What a dashboard's counts need of a service on a grid that its host's
/// square counts.
fn counted_service(service: &Service, host_problem: bool) -> Counted {
    Counted {
        state: CheckableState::Service(service.state),
        handled: service.counts_as_handled(host_problem),
        severity: service.severity(),
    }
}

impl Facts {
    fn counted(&self) -> Counted {
        Counted {
            state: self.state,
            handled: self.handled,
            severity: self.severity,
        }
    }

    fn of_host(host: &Host, groups: u64) -> Self {
        let problem = host.is_problem();
        let reasons = handled_reasons(&host.check, problem, false);
        Self {
            state: CheckableState::Host(host.state),
            severity: host.severity(),
            since: state_since(&host.check).as_unix_seconds(),
            problem,
            handled: reasons != 0,
            reasons,
            groups,
        }
    }

    fn of_service(service: &Service, host_problem: bool, groups: u64) -> Self {
        let problem = service.is_problem();
        let reasons = handled_reasons(&service.check, problem, problem && host_problem);
        Self {
            state: CheckableState::Service(service.state),
            severity: service.severity(),
            since: state_since(&service.check).as_unix_seconds(),
            problem,
            handled: reasons != 0,
            reasons,
            groups,
        }
    }
}

/// The reasons an object counts as handled ([`Host::counts_as_handled`],
/// [`Service::counts_as_handled`], split into their parts).
fn handled_reasons(check: &ic_model::CheckInfo, problem: bool, host_down: bool) -> u8 {
    let mut reasons = 0;
    if problem && check.acknowledgement.is_acknowledged() {
        reasons |= ACKNOWLEDGED;
    }
    if check.in_downtime() {
        reasons |= IN_DOWNTIME;
    }
    if host_down {
        reasons |= HOST_DOWN;
    }
    reasons
}

/// An `f64` with a total order (timestamps never are NaN, but sorting
/// must not depend on it).
#[derive(Clone, Copy, Debug)]
pub(super) struct OrdF64(pub(super) f64);

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

/// What decides a view's members: a change of any of it evaluates the
/// view again from scratch.
#[derive(Clone, Debug, PartialEq, Eq)]
struct MemberKey {
    filter: String,
    hosts: bool,
    services: bool,
    /// The groups a grid or tiles view shows (its members are the objects
    /// in them), and whether a grid's squares read the services.
    groups: Option<(ic_config::ViewGroups, GridColour)>,
}

impl MemberKey {
    fn of(view: &View) -> Self {
        let (hosts, services) = targets(view);
        let groups = matches!(
            view.display,
            ViewDisplay::HostGroupGrid | ViewDisplay::SummaryTiles
        )
        .then(|| {
            let colour = if view.display == ViewDisplay::HostGroupGrid {
                view.grid.colour
            } else {
                GridColour::HostOnly
            };
            (view.groups.clone(), colour)
        });
        Self {
            filter: view.filter.clone(),
            hosts,
            services,
            groups,
        }
    }
}

/// Which objects a view evaluates its filter on: (hosts, services). A
/// grid shows hosts; an event stream, handling and downtimes follow hosts
/// and services; the other displays list their object kind.
fn targets(view: &View) -> (bool, bool) {
    match view.display {
        ViewDisplay::HostGroupGrid => (true, false),
        ViewDisplay::EventStream | ViewDisplay::Handling | ViewDisplay::Downtimes => (true, true),
        ViewDisplay::List | ViewDisplay::GroupedList | ViewDisplay::SummaryTiles => {
            match view.object_kind {
                ObjectKind::Hosts => (true, false),
                ObjectKind::Services => (false, true),
            }
        }
    }
}

/// One view.
#[derive(Debug)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent flags: what changed and what to rebuild"
)]
pub(super) struct Board {
    pub(super) view: View,
    /// The handled problems the view hides (lists; none for the rest).
    hidden: HideHandled,
    /// The compiled filter, or why it doesn't parse.
    filter: Result<Filter, String>,
    /// The filter calls `get_time()`.
    pub(super) time_dependent: bool,
    /// The groups a grid or tiles view shows.
    picker: Picker,
    /// Matching objects.
    pub(super) members: HashMap<ObjectKey, Facts>,
    /// Objects the filter failed for, with the error.
    errors: BTreeMap<ObjectKey, String>,
    /// A list's visible members in display order.
    order: BTreeSet<RowKey>,
    /// Evaluate every object at the next update.
    pub(super) needs_full: bool,
    /// Recompute a list's visibility and order from the members (display
    /// settings changed).
    restyle: bool,
    /// Rebuild the body.
    pub(super) rows_dirty: bool,
    /// Rebuild the counts.
    summary_dirty: bool,
    pub(super) result: ViewResult,
    /// `result` holds a body and counts built by [`Board::finish`] (not
    /// yet for a board quiet mode only kept the memberships of).
    pub(super) finished: bool,
    /// The recent events an event stream's body was selected from.
    seen_events: Option<Arc<Vec<LogEntry>>>,
}

impl Board {
    pub(super) fn new(view: &View, defaults: HideHandled) -> Self {
        let filter = compile(&view.filter);
        Self {
            time_dependent: is_time_dependent(&view.filter),
            hidden: view.hidden_handled(defaults),
            picker: Picker::new(view),
            view: view.clone(),
            filter,
            members: HashMap::new(),
            errors: BTreeMap::new(),
            order: BTreeSet::new(),
            needs_full: true,
            restyle: false,
            rows_dirty: true,
            summary_dirty: true,
            result: ViewResult {
                id: view.id.clone(),
                ..ViewResult::default()
            },
            finished: false,
            seen_events: None,
        }
    }

    /// Takes the view's new settings: a new filter, object kind or set of
    /// groups evaluates it again; a new grouping of a list's rows evaluates
    /// it again too (the members' group hashes depend on it); anything else
    /// (sort, problems only, the handled switches, the display's options)
    /// only rebuilds its body and counts.
    pub(super) fn reconfigure(&mut self, view: &View, defaults: HideHandled) {
        let hidden = view.hidden_handled(defaults);
        if view.evaluated() == self.view.evaluated() && hidden == self.hidden {
            // Only what the app draws changed (the row density, the
            // handling and downtimes views' options): same result.
            self.view = view.clone();
            return;
        }
        if MemberKey::of(view) != MemberKey::of(&self.view) {
            *self = Self::new(view, defaults);
            return;
        }
        let regroup = view.list_grouping() != self.view.list_grouping();
        self.view = view.clone();
        self.hidden = hidden;
        self.result.id.clone_from(&view.id);
        self.seen_events = None;
        if regroup {
            self.needs_full = true;
        } else {
            self.restyle = true;
        }
    }

    pub(super) fn is_list(&self) -> bool {
        self.view.is_list()
    }

    fn colours_by_services(&self) -> bool {
        self.view.display == ViewDisplay::HostGroupGrid
            && self.view.grid.colour == GridColour::WorstOfHostAndServices
    }

    /// Whether the body shows group labels, which a change of the host or
    /// service groups' display names changes.
    pub(super) fn uses_group_labels(&self) -> bool {
        match self.view.display {
            ViewDisplay::List
            | ViewDisplay::EventStream
            | ViewDisplay::Handling
            | ViewDisplay::Downtimes => false,
            ViewDisplay::GroupedList => self.view.list_grouping() != GroupBy::Host,
            ViewDisplay::HostGroupGrid | ViewDisplay::SummaryTiles => true,
        }
    }

    /// Whether the view's members count toward its dashboard: the sidebar,
    /// the notifications ([`View::counts_problems`]).
    pub(super) fn counts(&self) -> bool {
        self.view.counts_problems()
    }

    /// Whether `object` is one of the objects the view counts: a member,
    /// or a service of a host on a grid coloured by its services too.
    pub(super) fn contains(&self, object: &ObjectKey) -> bool {
        self.members.contains_key(object)
            || (self.colours_by_services()
                && object.as_service().is_some_and(|key| {
                    self.members.contains_key(&ObjectKey::Host {
                        name: key.host.clone(),
                    })
                }))
    }

    /// Whether the view's members are usable for counting: its filter
    /// parses and evaluated for every object.
    pub(super) fn is_sound(&self) -> bool {
        self.filter.is_ok() && self.errors.is_empty()
    }

    /// Calls `count` with every object the view counts, once, with what a
    /// dashboard's counts need. A grid coloured by its services counts
    /// them too.
    pub(super) fn for_each_counted(&self, data: &Data, mut count: impl FnMut(&ObjectKey, Counted)) {
        for (object, facts) in &self.members {
            count(object, facts.counted());
        }
        if !self.colours_by_services() {
            return;
        }
        for object in self.members.keys() {
            let ObjectKey::Host { name } = object else {
                continue;
            };
            let host_problem = data.hosts.get(name).is_some_and(|host| host.is_problem());
            for (key, service) in data
                .services
                .range(service_range(name))
                .take_while(|(key, _)| &key.host == name)
            {
                count(
                    &ObjectKey::from(key.clone()),
                    counted_service(service, host_problem),
                );
            }
        }
    }

    /// What the dashboard's counts need of `object`, if the view counts it.
    pub(super) fn counted(&self, data: &Data, object: &ObjectKey) -> Option<Counted> {
        if let Some(facts) = self.members.get(object) {
            return Some(facts.counted());
        }
        let key = object.as_service().filter(|_| self.colours_by_services())?;
        let host = ObjectKey::Host {
            name: key.host.clone(),
        };
        if !self.members.contains_key(&host) {
            return None;
        }
        let service = data.services.get(key)?;
        let host_problem = data
            .hosts
            .get(&key.host)
            .is_some_and(|host| host.is_problem());
        Some(counted_service(service, host_problem))
    }

    fn visible(&self, facts: &Facts) -> bool {
        (!self.view.problems_only || facts.problem)
            && facts.reasons & mask(self.hidden) == 0
            && self.in_chip(facts)
    }

    /// Whether `facts` is in the state the list's state chip picked (any
    /// state without one).
    fn in_chip(&self, facts: &Facts) -> bool {
        self.view
            .state
            .is_none_or(|chip| chip_matches(chip, facts.state))
    }

    /// Forgets every member and evaluates every object the view looks at.
    pub(super) fn evaluate_all(&mut self, data: &Data, cancel: &AtomicBool) {
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
        let (hosts, services) = targets(&self.view);
        if hosts {
            for (index, host) in data.hosts.values().enumerate() {
                if index % CANCEL_EVERY == 0 && cancel.load(Ordering::Relaxed) {
                    return;
                }
                self.evaluate_host(data, host.key(), Some(host));
            }
        }
        if services {
            for (index, service) in data.services.values().enumerate() {
                if index % CANCEL_EVERY == 0 && cancel.load(Ordering::Relaxed) {
                    return;
                }
                let host = data.hosts.get(&service.key.host).map(Arc::as_ref);
                self.evaluate_service(data, service.object_key(), Some(service), host);
            }
        }
    }

    /// Re-evaluates the changed objects the view looks at.
    pub(super) fn evaluate_some(&mut self, data: &Data, dirty: &Dirty, cancel: &AtomicBool) {
        if self.filter.is_err() {
            return;
        }
        let (hosts, services) = targets(&self.view);
        if hosts {
            for (index, name) in dirty.hosts.iter().enumerate() {
                if index % CANCEL_EVERY == 0 && cancel.load(Ordering::Relaxed) {
                    return;
                }
                let object = ObjectKey::Host { name: name.clone() };
                self.evaluate_host(data, object, data.hosts.get(name));
            }
        }
        if services {
            for (index, key) in dirty.services.iter().enumerate() {
                if index % CANCEL_EVERY == 0 && cancel.load(Ordering::Relaxed) {
                    return;
                }
                let service = data.services.get(key);
                let host = data.hosts.get(&key.host).map(Arc::as_ref);
                self.evaluate_service(data, ObjectKey::from(key.clone()), service, host);
            }
        }
        // A grid coloured by its services: a changed service of a host on
        // it changes its square and counts.
        if self.colours_by_services()
            && dirty.services.iter().any(|key| {
                self.members.contains_key(&ObjectKey::Host {
                    name: key.host.clone(),
                })
            })
        {
            self.rows_dirty = true;
            self.summary_dirty = true;
        }
    }

    fn evaluate_host(&mut self, data: &Data, object: ObjectKey, host: Option<&Arc<Host>>) {
        let Some(host) = host else {
            self.forget(&object);
            return;
        };
        let picked = self.picker.includes(host);
        let outcome = self
            .test(&HostScope { host }, data.now)
            .map(|matched| matched && picked);
        let facts = Facts::of_host(host, self.groups_hash(Some(host), None));
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
        let picked = host.is_none_or(|host| self.picker.includes(host));
        let outcome = self
            .test(&ServiceScope { service, host }, data.now)
            .map(|matched| matched && picked);
        let host_problem = host.is_some_and(Host::is_problem);
        let facts = Facts::of_service(service, host_problem, self.groups_hash(host, Some(service)));
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
        match self.view.display {
            ViewDisplay::List | ViewDisplay::GroupedList => self.reorder(object, old, facts),
            // An event stream shows the members' events, handling and
            // downtimes their records: only joining and leaving changes
            // them.
            ViewDisplay::EventStream | ViewDisplay::Handling | ViewDisplay::Downtimes => {
                if old.is_some() != facts.is_some() {
                    self.rows_dirty = true;
                }
            }
            ViewDisplay::HostGroupGrid | ViewDisplay::SummaryTiles => self.rows_dirty = true,
        }
    }

    /// Moves a list's row for `object` from where `old` put it to where
    /// `new` puts it.
    fn reorder(&mut self, object: &ObjectKey, old: Option<Facts>, new: Option<Facts>) {
        let sort = self.view.sort;
        let old_row = old
            .filter(|old| self.visible(old))
            .map(|old| RowKey::new(sort, object, &old));
        let new_row = new
            .filter(|facts| self.visible(facts))
            .map(|facts| RowKey::new(sort, object, &facts));
        let grouped = self.view.list_grouping() != GroupBy::None
            && (old_row.is_some() || new_row.is_some())
            && old.map(|old| old.groups) != new.map(|facts| facts.groups);
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

    /// A hash of the groups the view files an object under.
    fn groups_hash(&self, host: Option<&Host>, service: Option<&Service>) -> u64 {
        let mut hasher = DefaultHasher::new();
        match self.view.display {
            ViewDisplay::List | ViewDisplay::GroupedList => match self.view.list_grouping() {
                GroupBy::None => return 0,
                GroupBy::Host => host.map(|host| &host.display_name).hash(&mut hasher),
                GroupBy::HostGroup => host.map(|host| &host.groups).hash(&mut hasher),
                GroupBy::ServiceGroup => service.map(|service| &service.groups).hash(&mut hasher),
            },
            ViewDisplay::HostGroupGrid | ViewDisplay::SummaryTiles => {
                host.map(|host| self.picker.groups_of(host))
                    .hash(&mut hasher);
            }
            ViewDisplay::EventStream | ViewDisplay::Handling | ViewDisplay::Downtimes => return 0,
        }
        hasher.finish()
    }

    /// Rebuilds what changed; returns whether the result changed.
    pub(super) fn finish(&mut self, data: &Data) -> bool {
        if self.restyle {
            self.restyle = false;
            if self.is_list() {
                let sort = self.view.sort;
                self.order = self
                    .members
                    .iter()
                    .filter(|(_, facts)| self.visible(facts))
                    .map(|(object, facts)| RowKey::new(sort, object, facts))
                    .collect();
            }
            self.rows_dirty = true;
            // What is shown changed, so do its counts.
            self.summary_dirty = true;
        }
        if self.view.display == ViewDisplay::EventStream
            && !self
                .seen_events
                .as_ref()
                .is_some_and(|seen| Arc::ptr_eq(seen, &data.events))
        {
            self.rows_dirty = true;
        }
        if !self.rows_dirty && !self.summary_dirty && self.finished {
            return false;
        }
        let failed = self.result.error.is_some();
        let result = if let Err(message) = &self.filter {
            self.empty_result(Some(message.clone()))
        } else if let Some((object, message)) = self.errors.first_key_value() {
            let error = evaluation_error(message, object, self.errors.len());
            self.empty_result(Some(error))
        } else {
            match self.view.display {
                ViewDisplay::List | ViewDisplay::GroupedList => self.list_result(data, failed),
                ViewDisplay::HostGroupGrid => {
                    let built = groups::grid(self, &self.picker, data);
                    ViewResult {
                        id: self.view.id.clone(),
                        summary: built.summary,
                        counts: built.counts,
                        shown: built.counts,
                        hidden: 0,
                        handled: built.handled,
                        body: ViewBody::Grid(self.keep_grid(built.grid)),
                        hosts: 0,
                        services: 0,
                        error: None,
                    }
                }
                ViewDisplay::SummaryTiles => {
                    let built = groups::tiles(self, &self.picker, data);
                    ViewResult {
                        id: self.view.id.clone(),
                        summary: built.summary,
                        counts: built.counts,
                        shown: built.counts,
                        hidden: 0,
                        handled: built.handled,
                        body: ViewBody::Tiles(self.keep_tiles(built.tiles)),
                        hosts: 0,
                        services: 0,
                        error: None,
                    }
                }
                ViewDisplay::EventStream => {
                    self.seen_events = Some(Arc::clone(&data.events));
                    let events = super::stream::select(self, &data.events);
                    let (hosts, services) = self.member_counts();
                    ViewResult {
                        id: self.view.id.clone(),
                        body: ViewBody::Stream(self.keep_events(events)),
                        hosts,
                        services,
                        ..ViewResult::default()
                    }
                }
                ViewDisplay::Handling | ViewDisplay::Downtimes => {
                    let (hosts, services) = self.member_counts();
                    ViewResult {
                        id: self.view.id.clone(),
                        body: ViewBody::Members(self.keep_members()),
                        hosts,
                        services,
                        ..ViewResult::default()
                    }
                }
            }
        };
        self.rows_dirty = false;
        self.summary_dirty = false;
        self.finished = true;
        if result == self.result {
            return false;
        }
        self.result = result;
        true
    }

    /// The result of a view whose filter failed: no body, no counts.
    fn empty_result(&self, error: Option<String>) -> ViewResult {
        let body = match self.view.display {
            ViewDisplay::List | ViewDisplay::GroupedList => ViewBody::List(Arc::default()),
            ViewDisplay::HostGroupGrid => ViewBody::Grid(Arc::default()),
            ViewDisplay::SummaryTiles => ViewBody::Tiles(Arc::default()),
            ViewDisplay::EventStream => ViewBody::Stream(Arc::default()),
            ViewDisplay::Handling | ViewDisplay::Downtimes => ViewBody::Members(Arc::default()),
        };
        ViewResult {
            id: self.view.id.clone(),
            body,
            error,
            ..ViewResult::default()
        }
    }

    /// A list's rows and counts, keeping the rows (the same `Arc`) when
    /// only the counts changed.
    fn list_result(&self, data: &Data, failed: bool) -> ViewResult {
        let rows = match &self.result.body {
            ViewBody::List(rows) if !self.rows_dirty && !failed => Arc::clone(rows),
            _ => Arc::new(self.rows(data)),
        };
        let counts = if self.summary_dirty || failed {
            self.list_counts()
        } else {
            ListCounts {
                summary: self.result.summary,
                counts: self.result.counts,
                shown: self.result.shown,
                hidden: self.result.hidden,
                handled: self.result.handled,
            }
        };
        ViewResult {
            id: self.view.id.clone(),
            summary: counts.summary,
            counts: counts.counts,
            shown: counts.shown,
            hidden: counts.hidden,
            handled: counts.handled,
            body: ViewBody::List(rows),
            hosts: 0,
            services: 0,
            error: None,
        }
    }

    /// The same grid as before (its `Arc`) when nothing in it changed.
    fn keep_grid(&self, grid: crate::snapshot::Grid) -> Arc<crate::snapshot::Grid> {
        match &self.result.body {
            ViewBody::Grid(old) if **old == grid => Arc::clone(old),
            _ => Arc::new(grid),
        }
    }

    fn keep_tiles(&self, tiles: Vec<crate::snapshot::Tile>) -> Arc<Vec<crate::snapshot::Tile>> {
        match &self.result.body {
            ViewBody::Tiles(old) if **old == tiles => Arc::clone(old),
            _ => Arc::new(tiles),
        }
    }

    /// How many hosts and services the view's filter matches.
    fn member_counts(&self) -> (u32, u32) {
        let hosts = self
            .members
            .keys()
            .filter(|object| matches!(object, ObjectKey::Host { .. }))
            .count();
        let hosts = u32::try_from(hosts).unwrap_or(u32::MAX);
        let services = u32::try_from(self.members.len())
            .unwrap_or(u32::MAX)
            .saturating_sub(hosts);
        (hosts, services)
    }

    /// The members as a set: the same `Arc` while nobody joined or left.
    fn keep_members(&self) -> Arc<BTreeSet<ObjectKey>> {
        if let ViewBody::Members(old) = &self.result.body
            && old.len() == self.members.len()
            && self.members.keys().all(|object| old.contains(object))
        {
            return Arc::clone(old);
        }
        Arc::new(self.members.keys().cloned().collect())
    }

    fn keep_events(&self, events: Vec<LogEntry>) -> Arc<Vec<LogEntry>> {
        match &self.result.body {
            ViewBody::Stream(old) if **old == events => Arc::clone(old),
            _ => Arc::new(events),
        }
    }

    /// A list's counts: over every member, over the unhandled ones it is
    /// about (after *problems only*), over its rows, and how many members
    /// count as handled and are hidden.
    fn list_counts(&self) -> ListCounts {
        let mut all = Tally::default();
        let mut counts = Tally::default();
        let mut shown = Tally::default();
        let mut hidden = 0_u32;
        let mut handled = 0_u32;
        let mask = mask(self.hidden);
        for facts in self.members.values() {
            all.add(facts.state, facts.handled, facts.severity);
            if self.view.problems_only && !facts.problem {
                continue;
            }
            // The header's numbers count every state, the chip or not.
            if !facts.handled {
                counts.add(facts.state, false, facts.severity);
            }
            // What shows and hides is what the state chip lets through.
            if !self.in_chip(facts) {
                continue;
            }
            if facts.handled {
                handled += 1;
                if facts.reasons & mask != 0 {
                    hidden += 1;
                    continue;
                }
            }
            shown.add(facts.state, facts.handled, facts.severity);
        }
        ListCounts {
            summary: all.finish(),
            counts: counts.finish(),
            shown: shown.finish(),
            hidden,
            handled,
        }
    }

    fn rows(&self, data: &Data) -> Vec<DashboardRow> {
        let grouping = self.view.list_grouping();
        if grouping == GroupBy::None {
            return self
                .order
                .iter()
                .map(|row| DashboardRow::Object(row.object()))
                .collect();
        }
        let labels = Labels::new(data, grouping);
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

/// A list's counts ([`Board::list_counts`]).
struct ListCounts {
    summary: Summary,
    counts: Summary,
    shown: Summary,
    hidden: u32,
    handled: u32,
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

/// Whether an object in `state` is in the state chip `chip` picked.
fn chip_matches(chip: StateChip, state: CheckableState) -> bool {
    matches!(
        (chip, state),
        (
            StateChip::Critical,
            CheckableState::Service(ServiceState::Critical)
        ) | (
            StateChip::Warning,
            CheckableState::Service(ServiceState::Warning)
        ) | (
            StateChip::Unknown,
            CheckableState::Service(ServiceState::Unknown)
        ) | (StateChip::Down, CheckableState::Host(HostState::Down))
            | (
                StateChip::Unreachable,
                CheckableState::Host(HostState::Unreachable)
            )
    )
}

fn evaluation_error(message: &str, object: &ObjectKey, count: usize) -> String {
    match count {
        0 | 1 => format!("{message} (for {object})"),
        2 => format!("{message} (for {object} and 1 other object)"),
        _ => format!("{message} (for {object} and {} other objects)", count - 1),
    }
}
