//! The object store of the active environment: the single source of truth
//! the snapshots are cut from, changed only by the engine.
//!
//! Collections are `Arc`-shared with the published snapshots and copied on
//! write: the first change after a publish copies the map (pointers, not
//! objects), and a changed object is copied only if a snapshot still holds
//! it.
//!
//! **Ordering of fetches and events.** The event stream and the queries
//! (initial load, reloads, re-queries) race: a query's answer may be older
//! than an event applied before the answer arrived, and an event read
//! before a query was sent is already reflected in its answer. Every event
//! line therefore carries a sequence number counted by the stream reader
//! when it reads the line, and every query records the reader's count when
//! it is sent (`started`). Then:
//! - an event with `seq <= started` of the query that last wrote its
//!   object is already reflected there and is skipped ([`Applied::Stale`]);
//! - a fetched object that an event with `seq > started` already changed
//!   keeps the fields events maintain (state, check result, acknowledgement,
//!   downtime depth, flapping, ...) and takes everything else (config,
//!   vars, groups, links) from the answer;
//! - comment and downtime lists keep the changes of events newer than the
//!   list's query;
//! - answers race each other too (re-queries run while a reload's tiers
//!   take seconds): an answer sent before the one that last wrote an object
//!   (`started` below the object's) changes nothing, and an object a newer
//!   answer found gone, or an `ObjectDeleted` event newer than the answer
//!   announced gone, stays gone (a bounded tombstone per removal), so an
//!   older list neither brings back a deleted object, nor drops one created
//!   since, nor reverts its config.
//!
//! So the store converges to Icinga's state whatever order answers and
//! events arrive in, without pausing the stream during queries.
//!
//! **Downtime depths.** `downtime_depth` is a count (Icinga's
//! `GetDowntimeDepth`: the downtimes in effect), not a value an event sets,
//! so applying a downtime event twice, or missing one, would leave it
//! wrong for good where no check result brings Icinga's count again (quiet
//! mode: a problem hidden as in downtime while Icinga says it isn't). Its
//! sources:
//! - a check event (`CheckResult`, `StateChange`) carries Icinga's count at
//!   the event's time, in the stream's order: the downtime events after it
//!   change it by exactly what they change in Icinga's count, judged per
//!   downtime by name (`Store::count_downtime`) and by Icinga's own rule
//!   (`Downtime::IsInEffect`: a fixed downtime counts from its start,
//!   before Icinga's start timer reports it up to 5 s later, and stops at
//!   its end, before the removal);
//! - a query answer carries Icinga's count at a moment between the request
//!   and the answer, so a downtime event read after the request may or may
//!   not be in it already. Such an event changes the depth for the moment
//!   (the depth is *provisional*) until an answer sent after the line or a
//!   check event brings Icinga's count with no downtime event after it.
//!   A provisional problem is asked for again by name
//!   (`Store::take_unsettled`, the engine's `reask` part: spread by the
//!   requests they take, retried with backoff when a request fails, handed
//!   out again after a new session); an OK object isn't, since its count
//!   can't hide a problem and the state change that makes it one brings
//!   Icinga's count (or, where it can't, hands it out then).
//!
//! A check event built in the moment a downtime is added (Icinga registers
//! the downtime with its object before it sends `DowntimeAdded`) may count
//! it: a count from a check event at or after the downtime's entry is
//! treated like an answer's for that event.
//!
//! An answer takes the depth unless a newer line wrote it, and is its
//! source from then on (even one that agrees with a count from a check
//! event: a line it already has may still wait in a stream switch's
//! buffer). Every load and reconcile is such an answer, so after one every
//! count is an answer's until the object's next check event. When the
//! store may have missed lines (a new session, a stream switch without a
//! line on both streams), depths counted from check events count as
//! answers' again (`Store::stream_gap`).
//!
//! **Acknowledgements** are values every event sets (set, cleared), so an
//! answer that overtook their lines converges once they are in. But check
//! events carry the acknowledgement too, as it was when Icinga built them:
//! in the overlap of a stream switch the new stream's check result can be
//! older than an acknowledgement line the old stream already brought (a
//! quiet stream has no check results), and a check event older than the
//! latest acknowledgement line (by Icinga's time, `Seqs::ack_at`) keeps the
//! stored acknowledgement, like an older one keeps the depth
//! (`Seqs::downtime_at`). Both times are kept also for lines an answer
//! already had, and forgotten at a new session (one stream from then on,
//! perhaps another master's clock). Comments have only their own events.
//!
//! **Views of the cluster (ENV-12).** While the engine is connected to a
//! node in a child zone (a partial view), objects its answers leave out
//! may only be outside its zone: they leave the store, but their last view
//! is kept ([`Store::set_hiding`]) instead of being reported gone, so the
//! rule engine doesn't forget them. A later answer that brings one back
//! (from a master) reports what changed meanwhile as [`Discovered`], and a
//! complete load from a node that sees everything reports those it didn't
//! bring as gone ([`Store::release_hidden`]). Objects that come with a
//! fuller view without having been seen before are listed for the rule
//! engine to learn ([`Store::track_appeared`]).

mod apply;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use ic_api::{Detail, EndpointState};
use ic_model::{
    CheckInfo, CheckableState, Comment, Dependency, Downtime, Endpoint, Host, HostGroup, HostName,
    InstanceStatus, Notification, ObjectCounts, ObjectKey, Service, ServiceGroup, ServiceKey,
    Timestamp, Zone,
};
use ic_rules::DashboardRef;

use crate::health::ClusterHealth;
use crate::snapshot::{DashboardResult, Snapshot};
use crate::summary::Tally;

/// At most this many removals are remembered (deletions are rare; beyond
/// it the older half is forgotten).
const MAX_TOMBSTONES: usize = 100_000;

pub(crate) use apply::{Applied, ObjectView};

/// Sequence numbers of the last query answer and the last events that
/// wrote an object, and where its downtime depth comes from.
#[derive(Clone, Copy, Debug, Default)]
struct Seqs {
    /// `started` of the last query whose answer was applied.
    fetched: u64,
    /// `seq` of the last event applied.
    evented: u64,
    /// `seq` of the last event applied that wrote the check state (a check
    /// result, a state change, flapping; not an acknowledgement or a
    /// downtime): above `fetched`, events wrote the check state last and
    /// no answer can have overtaken them (`Store::overtaken`).
    checked: u64,
    /// `seq` of the last acknowledgement event applied.
    handled: u64,
    /// `seq` of the last line that wrote the downtime depth: a check event
    /// that brought Icinga's count, or a downtime event that changed it (or
    /// could have: one in effect).
    depth: u64,
    /// The Icinga time of the check event the depth was last taken from;
    /// `None` while it comes from a query answer (see the module notes on
    /// downtime depths).
    depth_line: Option<Timestamp>,
    /// The Icinga time of the latest downtime event that changed the
    /// count: a check event older than that (the overlap of a stream
    /// switch) doesn't bring its count back.
    downtime_at: Option<Timestamp>,
    /// The Icinga time of the latest acknowledgement event applied: a
    /// check event older than that (the overlap of a stream switch)
    /// doesn't bring its acknowledgement back.
    ack_at: Option<Timestamp>,
}

/// Which groups of the fields events maintain an event newer than an
/// answer wrote: those keep the store's value, the rest come from the
/// answer (which is newer than whatever wrote them).
#[derive(Clone, Copy, Debug, Default)]
struct Newer {
    /// The check state (state, type, times, result, attempt, flapping,
    /// reachability): a check event.
    check: bool,
    /// The acknowledgement: an acknowledgement event or a check event.
    acknowledgement: bool,
    /// The downtime depth: a check event that brought it, or a downtime
    /// event that changed it.
    depth: bool,
}

/// What changed since the last snapshot, for publishing, the incremental
/// dashboard evaluation and the freshness watchdog.
#[derive(Clone, Debug, Default)]
pub(crate) struct Changes {
    /// Something changed: publish.
    pub(crate) any: bool,
    /// The object set changed wholesale (a reload): re-evaluate everything.
    pub(crate) all: bool,
    /// Hosts and services that changed, appeared or disappeared.
    pub(crate) objects: BTreeSet<ObjectKey>,
    /// The host or service group lists changed (group-by labels).
    pub(crate) groups: bool,
}

/// A change only a query answer revealed: no event explained it, so the
/// stream missed it (a reconnect gap, a dropped event) or it was never
/// sent (an object deleted while the client was away). `after` is `None`
/// when the object is gone. The rule engine (stage 3) judges these like
/// events, so missed problems still notify.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Discovered {
    /// The host or service.
    pub(crate) object: ObjectKey,
    /// What the store knew before the answer.
    pub(crate) before: ObjectView,
    /// What the answer says (`None`: Icinga no longer knows the object).
    pub(crate) after: Option<ObjectView>,
}

/// A comment or downtime as an event left it.
#[derive(Clone, Debug)]
enum Annotation {
    /// Added or changed (`true`), or removed.
    Comment(Comment, bool),
    /// Added or changed (`true`), or removed.
    Downtime(Downtime, bool),
}

/// The tier-1 lists apart from hosts.
#[derive(Clone, Debug, Default)]
pub(crate) struct Overview {
    /// `/v1/status`, if allowed.
    pub(crate) status: Option<InstanceStatus>,
    /// Host groups.
    pub(crate) host_groups: Vec<HostGroup>,
    /// Service groups.
    pub(crate) service_groups: Vec<ServiceGroup>,
    /// Dependencies.
    pub(crate) dependencies: Vec<Dependency>,
    /// Endpoints.
    pub(crate) endpoints: Vec<Endpoint>,
    /// Zones (empty without permission).
    pub(crate) zones: Vec<Zone>,
    /// Comments.
    pub(crate) comments: Vec<Comment>,
    /// Downtimes.
    pub(crate) downtimes: Vec<Downtime>,
}

/// The objects of one environment.
#[derive(Debug, Default)]
pub(crate) struct Store {
    hosts: Arc<BTreeMap<HostName, Arc<Host>>>,
    services: Arc<BTreeMap<ServiceKey, Arc<Service>>>,
    comments: Arc<BTreeMap<ObjectKey, Vec<Comment>>>,
    downtimes: Arc<BTreeMap<ObjectKey, Vec<Downtime>>>,
    host_groups: Arc<Vec<HostGroup>>,
    service_groups: Arc<Vec<ServiceGroup>>,
    dependencies: Arc<Vec<Dependency>>,
    endpoints: Arc<Vec<Endpoint>>,
    zones: Arc<Vec<Zone>>,
    status: Option<Arc<InstanceStatus>>,
    /// The cluster health page's data (`Snapshot::health`).
    health: Arc<ClusterHealth>,
    /// Icinga's own `Notification` objects, by host or service, each list
    /// by name.
    icinga_notifications: Arc<BTreeMap<ObjectKey, Arc<[Notification]>>>,
    /// `started` of the complete notification list in the store (0: none).
    notifications_listed: u64,
    /// `started` of the by-name query that last wrote (or found gone) a
    /// notification since that list, so an older answer never replaces a
    /// newer one. Only names re-queried since the list are here.
    notifications_fetched: HashMap<String, u64>,
    last_event_at: Option<Timestamp>,
    /// The node the objects come from (`Snapshot::node`).
    node: Option<Arc<crate::topology::ConnectedNode>>,
    /// Services whose links were loaded ([`Detail::Full`]).
    full: HashSet<ServiceKey>,
    seqs: HashMap<ObjectKey, Seqs>,
    /// Hosts and services found gone by a query answer (its `started`) or
    /// announced gone by an `ObjectDeleted` event (its `seq`): an answer
    /// sent before doesn't bring them back.
    removed: HashMap<ObjectKey, u64>,
    /// `started` of the comment and downtime lists in the store.
    annotations_fetched: u64,
    /// While a comment/downtime list query runs: what events did to each
    /// comment or downtime since, by name.
    annotation_log: Option<HashMap<String, (u64, Annotation)>>,
    changes: Changes,
    /// Changes found only by query answers, since the last
    /// [`Store::take_discovered`].
    discovered: Vec<Discovered>,
    /// Hosts and services a node with a partial view left out, with their
    /// last view (see the module notes).
    hidden: HashMap<ObjectKey, ObjectView>,
    /// The connected node has a partial view: objects its answers leave
    /// out are hidden, not gone.
    hiding: bool,
    /// While recording: objects answers brought that the store neither
    /// held nor hid (see [`Store::track_appeared`]).
    appeared: Option<Vec<ObjectKey>>,
    /// The heartbeat objects (`Snapshot::excluded`): left out of lists,
    /// counts, rules and notifications.
    excluded: Arc<BTreeSet<ServiceKey>>,
    /// Per host or service: the downtimes events changed since its depth
    /// was last taken from Icinga, by full name, and whether the stored
    /// depth counts each (see the module notes on downtime depths).
    counted: HashMap<ObjectKey, HashMap<String, bool>>,
    /// Hosts and services whose depth is provisional: a downtime event
    /// changed it while it came from a query answer (or while a check
    /// event's count can't tell whether it has the downtime). Until an
    /// answer sent after the line or a check event brings Icinga's count.
    unsettled: BTreeSet<ObjectKey>,
    /// Those of them handed out to be asked for again by name (queued, in
    /// flight, or waiting for a retry): not handed out twice.
    asked: BTreeSet<ObjectKey>,
    /// `unsettled` may hold a problem not yet handed out (a downtime event,
    /// a state change of a provisional object, a new session): the next
    /// [`Store::take_unsettled`] looks.
    unsettled_dirty: bool,
}

impl Store {
    /// Whether a host or service is known.
    pub(crate) fn contains(&self, key: &ObjectKey) -> bool {
        match key {
            ObjectKey::Host { name } => self.hosts.contains_key(name),
            ObjectKey::Service { key } => self.services.contains_key(key),
        }
    }

    /// Whether a service was loaded in full (links included).
    pub(crate) fn is_full(&self, key: &ServiceKey) -> bool {
        self.full.contains(key)
    }

    /// Whether a service's stored check result is older than its last
    /// check (a lean answer moved the state on but carries no result).
    pub(crate) fn result_is_stale(&self, key: &ServiceKey) -> bool {
        self.services.get(key).is_some_and(|service| {
            let check = &service.check;
            match (&check.result, check.last_check) {
                (Some(result), Some(last_check)) => {
                    result.execution_end.as_unix_seconds() + 1.0 < last_check.as_unix_seconds()
                }
                _ => false,
            }
        })
    }

    /// The check details of a host or service.
    pub(crate) fn check(&self, key: &ObjectKey) -> Option<&CheckInfo> {
        match key {
            ObjectKey::Host { name } => self.hosts.get(name).map(|host| &host.check),
            ObjectKey::Service { key } => self.services.get(key).map(|service| &service.check),
        }
    }

    /// The state and check details of a host or service.
    pub(crate) fn checkable(&self, key: &ObjectKey) -> Option<(CheckableState, &CheckInfo)> {
        match key {
            ObjectKey::Host { name } => self
                .hosts
                .get(name)
                .map(|host| (CheckableState::Host(host.state), &host.check)),
            ObjectKey::Service { key } => self
                .services
                .get(key)
                .map(|service| (CheckableState::Service(service.state), &service.check)),
        }
    }

    /// Every host, by name (shared with the snapshots).
    pub(crate) fn hosts(&self) -> &Arc<BTreeMap<HostName, Arc<Host>>> {
        &self.hosts
    }

    /// Every service, by key (shared with the snapshots).
    pub(crate) fn services(&self) -> &Arc<BTreeMap<ServiceKey, Arc<Service>>> {
        &self.services
    }

    /// The host groups.
    pub(crate) fn host_groups(&self) -> &Arc<Vec<HostGroup>> {
        &self.host_groups
    }

    /// The service groups.
    pub(crate) fn service_groups(&self) -> &Arc<Vec<ServiceGroup>> {
        &self.service_groups
    }

    /// How many hosts and services there are.
    pub(crate) fn object_count(&self) -> usize {
        self.hosts.len() + self.services.len()
    }

    /// The services by state (ok, warning, critical, unknown) as Icinga's
    /// `/v1/status/CIB` counts them: by the raw state, which is unknown
    /// while a service is pending ([`ObjectCounts::service_index`]), so a
    /// pending service's first check result moves both alike.
    pub(crate) fn service_states(&self) -> [u32; 4] {
        let mut counts = [0_u32; 4];
        for service in self.services.values() {
            let bucket = ObjectCounts::service_index(service.state);
            if let Some(count) = counts.get_mut(bucket) {
                *count = count.saturating_add(1);
            }
        }
        counts
    }

    /// The latest `last_check` of any host or service: a lower bound of
    /// Icinga's clock right after a load (with thousands of objects checked
    /// every few minutes, within a second or so of it).
    pub(crate) fn latest_check(&self) -> Option<Timestamp> {
        self.hosts
            .values()
            .map(|host| &host.check)
            .chain(self.services.values().map(|service| &service.check))
            .filter_map(|check| check.last_check)
            .max_by(|a, b| a.as_unix_seconds().total_cmp(&b.as_unix_seconds()))
    }

    /// The downtime with this full name.
    pub(crate) fn downtime(&self, name: &str) -> Option<&Downtime> {
        self.downtimes
            .values()
            .flatten()
            .find(|downtime| downtime.name == name)
    }

    /// The comment with this full name.
    pub(crate) fn comment(&self, name: &str) -> Option<&Comment> {
        self.comments
            .values()
            .flatten()
            .find(|comment| comment.name == name)
    }

    /// Whether a host is down or unreachable (its services' problems are
    /// handled then).
    pub(crate) fn host_problem(&self, host: &HostName) -> bool {
        self.hosts.get(host).is_some_and(|host| host.is_problem())
    }

    /// The display names of a host or service: the host's, and the
    /// service's for a service (names when unknown).
    pub(crate) fn display_names(&self, key: &ObjectKey) -> (String, Option<String>) {
        let host_name = key.host_name();
        let host = self.hosts.get(host_name).map_or_else(
            || host_name.as_str().to_owned(),
            |host| host.display_name.clone(),
        );
        let service = key.as_service().map(|service_key| {
            self.services.get(service_key).map_or_else(
                || service_key.name.to_string(),
                |service| service.display_name.clone(),
            )
        });
        (host, service)
    }

    /// The first line of a host's or service's latest check output, if
    /// it is loaded and belongs to the current state (a lean answer can
    /// move a service's state on while its stored result is older).
    pub(crate) fn current_output(&self, key: &ObjectKey) -> Option<&str> {
        if key
            .as_service()
            .is_some_and(|service| self.result_is_stale(service))
        {
            return None;
        }
        self.check(key)?
            .result
            .as_ref()
            .map(|result| result.output.as_str())
    }

    /// The services of a host that are in a problem state, with their
    /// views.
    pub(crate) fn problem_services_of(&self, host: &HostName) -> Vec<(ObjectKey, ObjectView)> {
        self.services
            .range(
                ServiceKey {
                    host: host.clone(),
                    name: Arc::from(""),
                }..,
            )
            .take_while(|(key, _)| &key.host == host)
            .filter(|(_, service)| service.is_problem())
            .map(|(key, service)| {
                (
                    ObjectKey::Service { key: key.clone() },
                    ObjectView::of(CheckableState::Service(service.state), &service.check),
                )
            })
            .collect()
    }

    /// The problems whose reachability a state change of `parent` may
    /// change: its services (Icinga's implicit dependency on the host) and
    /// the children of its `Dependency` objects, if they are problems.
    pub(crate) fn dependent_problems(&self, parent: &ObjectKey) -> Vec<ObjectKey> {
        let mut found: Vec<ObjectKey> = match parent {
            ObjectKey::Host { name } => self
                .problem_services_of(name)
                .into_iter()
                .map(|(key, _)| key)
                .collect(),
            ObjectKey::Service { .. } => Vec::new(),
        };
        for dependency in self.dependencies.iter() {
            if &dependency.parent == parent
                && self
                    .checkable(&dependency.child)
                    .is_some_and(|(state, _)| state.is_problem())
                && !found.contains(&dependency.child)
            {
                found.push(dependency.child.clone());
            }
        }
        found
    }

    /// An object's comments.
    pub(crate) fn comments_of(&self, object: &ObjectKey) -> &[Comment] {
        self.comments.get(object).map_or(&[], Vec::as_slice)
    }

    /// An object's downtimes.
    pub(crate) fn downtimes_of(&self, object: &ObjectKey) -> &[Downtime] {
        self.downtimes.get(object).map_or(&[], Vec::as_slice)
    }

    /// One of an object's downtimes, by full name.
    pub(crate) fn downtime_of(&self, object: &ObjectKey, name: &str) -> Option<&Downtime> {
        self.downtimes
            .get(object)?
            .iter()
            .find(|downtime| downtime.name == name)
    }

    /// The full names of the `Notification` objects of a host or service.
    pub(crate) fn notification_names(&self, key: &ObjectKey) -> Vec<String> {
        self.icinga_notifications
            .get(key)
            .map(|list| list.iter().map(|n| n.name.clone()).collect())
            .unwrap_or_default()
    }

    /// `started` of the complete notification list in the store (0: none
    /// yet): it reflects every `Notification` event read before that.
    pub(crate) fn notifications_listed(&self) -> u64 {
        self.notifications_listed
    }

    /// The `Notification` objects of every host and service.
    #[cfg(test)]
    pub(crate) fn icinga_notifications(&self) -> &BTreeMap<ObjectKey, Arc<[Notification]>> {
        &self.icinga_notifications
    }

    /// The instance status, once known.
    pub(crate) fn status(&self) -> Option<&InstanceStatus> {
        self.status.as_deref()
    }

    /// Takes what changed since the last call.
    pub(crate) fn take_changes(&mut self) -> Changes {
        std::mem::take(&mut self.changes)
    }

    /// Whether anything changed since the last [`Store::take_changes`].
    pub(crate) fn has_changes(&self) -> bool {
        self.changes.any
    }

    /// What changed since the last [`Store::take_changes`], without taking
    /// it.
    pub(crate) fn changes(&self) -> &Changes {
        &self.changes
    }

    /// Takes the changes only query answers revealed since the last call.
    pub(crate) fn take_discovered(&mut self) -> Vec<Discovered> {
        std::mem::take(&mut self.discovered)
    }

    /// Takes the hosts and services to ask for again by name: problems
    /// whose depth is provisional (a downtime event changed it while it
    /// came from a query answer, which may already count it; see the
    /// module notes on downtime depths), not handed out yet. An object that
    /// isn't a problem waits: its next state change brings Icinga's count
    /// (or, where it can't, makes it handed out then).
    pub(crate) fn take_unsettled(&mut self) -> Vec<ObjectKey> {
        if !std::mem::take(&mut self.unsettled_dirty) {
            return Vec::new();
        }
        let due: Vec<ObjectKey> = self
            .unsettled
            .iter()
            .filter(|key| !self.asked.contains(*key) && self.is_problem(key))
            .cloned()
            .collect();
        self.asked.extend(due.iter().cloned());
        due
    }

    /// Whether a host or service is a problem now.
    fn is_problem(&self, key: &ObjectKey) -> bool {
        match key {
            ObjectKey::Host { name } => self.hosts.get(name).is_some_and(|host| host.is_problem()),
            ObjectKey::Service { key } => self
                .services
                .get(key)
                .is_some_and(|service| service.is_problem()),
        }
    }

    /// The request that asked again for those of `keys` handed out by
    /// [`Store::take_unsettled`] failed: returns those still provisional,
    /// to be asked again later ([`Store::keep_asking`]).
    pub(crate) fn reask_failed(&self, keys: &[ObjectKey]) -> Vec<ObjectKey> {
        if self.asked.is_empty() {
            return Vec::new();
        }
        keys.iter()
            .filter(|key| self.asked.contains(*key) && self.unsettled.contains(*key))
            .cloned()
            .collect()
    }

    /// Whether to ask (again) for `key`, handed out earlier: its depth is
    /// still provisional and it is a problem. Otherwise it is no longer
    /// handed out (a later state change hands it out again if needed).
    pub(crate) fn keep_asking(&mut self, key: &ObjectKey) -> bool {
        if self.unsettled.contains(key) && self.is_problem(key) {
            return true;
        }
        self.asked.remove(key);
        false
    }

    /// The depth of `key` is Icinga's again: nothing to ask.
    fn settle(&mut self, key: &ObjectKey) {
        if !self.unsettled.is_empty() {
            self.unsettled.remove(key);
        }
        if !self.asked.is_empty() {
            self.asked.remove(key);
        }
    }

    /// `key`'s depth turned provisional (a downtime event Icinga's count
    /// may already have): it is asked for again, also if a request for it
    /// is under way (sent before the line, its answer can't settle it).
    fn unsettle(&mut self, key: &ObjectKey) {
        self.unsettled.insert(key.clone());
        self.asked.remove(key);
        self.unsettled_dirty = true;
    }

    /// The check state of `key` changed without its count settling (an
    /// answer whose depth a newer line kept, a check event older than a
    /// downtime line): if its depth is provisional, it may be a problem now.
    fn recheck_unsettled(&mut self, key: &ObjectKey) {
        if self.unsettled.contains(key) {
            self.unsettled_dirty = true;
        }
    }

    /// The event stream was replaced and the store may not have seen every
    /// line in between (a new session; a switch that ended without a line
    /// on both streams): depths counted from check events count as
    /// answers' again, so the next downtime event asks Icinga.
    pub(crate) fn stream_gap(&mut self) {
        for seqs in self.seqs.values_mut() {
            seqs.depth_line = None;
        }
        self.counted.clear();
    }

    /// A new session's stream replaces the last one, whose unread lines
    /// are gone ([`Store::stream_gap`]). Its lines come after every line
    /// applied so far, so a check event on it is never older than an
    /// acknowledgement or downtime line already applied: the times that
    /// guard against the overlap of a switch are forgotten (they may come
    /// from another master's clock).
    pub(crate) fn new_session(&mut self) {
        self.stream_gap();
        for seqs in self.seqs.values_mut() {
            seqs.ack_at = None;
            seqs.downtime_at = None;
        }
        // The last session's re-query queue is gone: whatever was handed
        // out and is still provisional is handed out again.
        self.asked.clear();
        self.unsettled_dirty = !self.unsettled.is_empty();
    }

    /// Forgets everything (the environment now points at another server).
    pub(crate) fn clear(&mut self) {
        *self = Self {
            changes: Changes {
                any: true,
                all: true,
                objects: BTreeSet::new(),
                groups: true,
            },
            ..Self::default()
        };
    }

    /// Records when the latest event arrived.
    pub(crate) fn set_last_event_at(&mut self, at: Timestamp) {
        self.last_event_at = Some(at);
        self.changes.any = true;
    }

    /// Records the node the objects come from. `announce`: the next
    /// snapshot goes out for it (otherwise it rides along with the next
    /// change: the first load's tiers).
    pub(crate) fn set_node(&mut self, node: Arc<crate::topology::ConnectedNode>, announce: bool) {
        if self.node.as_deref() != Some(&*node) {
            self.node = Some(node);
            self.changes.any |= announce;
        }
    }

    /// The node the objects come from, if any.
    pub(crate) fn node(&self) -> Option<&Arc<crate::topology::ConnectedNode>> {
        self.node.as_ref()
    }

    /// Whether the connected node has a partial view: objects answers
    /// leave out are then hidden (their last view kept), not gone.
    pub(crate) fn set_hiding(&mut self, hiding: bool) {
        self.hiding = hiding;
    }

    /// The hosts and services hidden so far.
    pub(crate) fn hidden_count(&self) -> usize {
        self.hidden.len()
    }

    /// A complete load from a node that sees the whole cluster didn't bring
    /// the hidden objects back: they are gone (reported as [`Discovered`]
    /// with no `after`, like any removal).
    pub(crate) fn release_hidden(&mut self) {
        let hidden = std::mem::take(&mut self.hidden);
        let mut gone: Vec<(ObjectKey, ObjectView)> = hidden.into_iter().collect();
        gone.sort_by(|(a, _), (b, _)| a.cmp(b));
        for (object, before) in gone {
            self.discovered.push(Discovered {
                object,
                before,
                after: None,
            });
        }
    }

    /// Starts (`true`) or stops recording the objects answers bring that
    /// the store neither held nor hid: what a fuller view adds to a store
    /// filled from a partial (or unverified) one. Stopping drops what was
    /// recorded; [`Store::take_appeared`] takes it.
    pub(crate) fn track_appeared(&mut self, track: bool) {
        self.appeared = track.then(Vec::new);
    }

    /// Takes the objects recorded since [`Store::track_appeared`] (still
    /// recording).
    pub(crate) fn take_appeared(&mut self) -> Vec<ObjectKey> {
        self.appeared
            .as_mut()
            .map(std::mem::take)
            .unwrap_or_default()
    }

    /// An object an answer put into the store that it didn't hold: a
    /// hidden one comes back with what changed meanwhile, another one is
    /// recorded while that is asked for.
    fn arrived(&mut self, key: &ObjectKey, after: ObjectView) {
        if let Some(before) = self.hidden.remove(key) {
            if before != after {
                self.discovered.push(Discovered {
                    object: key.clone(),
                    before,
                    after: Some(after),
                });
            }
        } else if let Some(appeared) = &mut self.appeared {
            appeared.push(key.clone());
        }
    }

    /// Stores a new instance status; returns the previous one.
    pub(crate) fn set_status(&mut self, status: InstanceStatus) -> Option<Arc<InstanceStatus>> {
        let changed = self.status.as_deref() != Some(&status);
        if changed {
            self.changes.any = true;
            self.status.replace(Arc::new(status))
        } else {
            self.status.clone()
        }
    }

    /// Cuts a snapshot (cheap: the collections are shared).
    pub(crate) fn snapshot(
        &self,
        revision: u64,
        taken_at: Timestamp,
        dashboards: Arc<BTreeMap<DashboardRef, DashboardResult>>,
        late: Arc<BTreeMap<ObjectKey, Timestamp>>,
    ) -> Snapshot {
        Snapshot {
            revision,
            taken_at,
            hosts: Arc::clone(&self.hosts),
            services: Arc::clone(&self.services),
            comments: Arc::clone(&self.comments),
            downtimes: Arc::clone(&self.downtimes),
            host_groups: Arc::clone(&self.host_groups),
            service_groups: Arc::clone(&self.service_groups),
            dependencies: Arc::clone(&self.dependencies),
            endpoints: Arc::clone(&self.endpoints),
            zones: Arc::clone(&self.zones),
            status: self.status.clone(),
            health: Arc::clone(&self.health),
            icinga_notifications: Arc::clone(&self.icinga_notifications),
            dashboards,
            last_event_at: self.last_event_at,
            overall: self.overall(),
            late,
            node: self.node.clone(),
            // Set by the engine.
            quiet: false,
            updating: Arc::default(),
            events: Arc::default(),
            excluded: Arc::clone(&self.excluded),
            heartbeats: Arc::default(),
            trouble: Arc::default(),
        }
    }

    /// The summary over every host and service.
    fn overall(&self) -> crate::snapshot::Summary {
        let mut tally = Tally::default();
        for host in self.hosts.values() {
            tally.add_host(host);
        }
        // Services are sorted by host: each host is looked up once.
        let mut current: Option<(&HostName, Option<&Host>)> = None;
        for service in self.services.values() {
            if self.excluded.contains(&service.key) {
                continue;
            }
            let host = match current {
                Some((name, host)) if *name == service.key.host => host,
                _ => {
                    let host = self.hosts.get(&service.key.host).map(Arc::as_ref);
                    current = Some((&service.key.host, host));
                    host
                }
            };
            tally.add_service(service, host);
        }
        tally.finish()
    }

    // --- query answers -------------------------------------------------------

    /// Starts recording comment and downtime events, for a comment and
    /// downtime list query about to be sent ([`Store::apply_overview`]
    /// stops it).
    pub(crate) fn begin_annotation_query(&mut self) {
        self.annotation_log.get_or_insert_with(HashMap::new);
    }

    /// Stops recording comment and downtime events (the list query
    /// failed or its session ended).
    pub(crate) fn end_annotation_query(&mut self) {
        self.annotation_log = None;
    }

    /// Applies the tier-1 lists queried at `started`: status, groups,
    /// dependencies, endpoints, comments and downtimes.
    pub(crate) fn apply_overview(&mut self, overview: Overview, started: u64) {
        if let Some(status) = overview.status {
            self.set_status(status);
        }
        self.set_host_groups(overview.host_groups);
        self.set_service_groups(overview.service_groups);
        set_list(
            &mut self.dependencies,
            overview.dependencies,
            &mut self.changes,
        );
        set_list(&mut self.endpoints, overview.endpoints, &mut self.changes);
        set_list(&mut self.zones, overview.zones, &mut self.changes);

        let mut comments: BTreeMap<ObjectKey, Vec<Comment>> = BTreeMap::new();
        for comment in overview.comments {
            comments
                .entry(comment.object.clone())
                .or_default()
                .push(comment);
        }
        let mut downtimes: BTreeMap<ObjectKey, Vec<Downtime>> = BTreeMap::new();
        for downtime in overview.downtimes {
            downtimes
                .entry(downtime.object.clone())
                .or_default()
                .push(downtime);
        }
        // Changes events made after the query was sent.
        let log = self.annotation_log.take().unwrap_or_default();
        let mut newer: Vec<(u64, Annotation)> = log
            .into_values()
            .filter(|(seq, _)| *seq > started)
            .collect();
        newer.sort_by_key(|(seq, _)| *seq);
        for (_, annotation) in newer {
            match annotation {
                Annotation::Comment(comment, true) => upsert_comment(&mut comments, comment),
                Annotation::Comment(comment, false) => {
                    remove_named(&mut comments, &comment.object, &comment.name, |c| &c.name);
                }
                Annotation::Downtime(downtime, true) => upsert_downtime(&mut downtimes, downtime),
                Annotation::Downtime(downtime, false) => {
                    remove_named(&mut downtimes, &downtime.object, &downtime.name, |d| {
                        &d.name
                    });
                }
            }
        }
        for list in comments.values_mut() {
            sort_comments(list);
        }
        for list in downtimes.values_mut() {
            sort_downtimes(list);
        }
        if *self.comments != comments {
            self.changes.any = true;
            self.changes
                .objects
                .extend(changed_keys(&self.comments, &comments));
            self.comments = Arc::new(comments);
        }
        if *self.downtimes != downtimes {
            self.changes.any = true;
            self.changes
                .objects
                .extend(changed_keys(&self.downtimes, &downtimes));
            self.downtimes = Arc::new(downtimes);
        }
        self.annotations_fetched = self.annotations_fetched.max(started);
    }

    /// Applies every host, queried in full at `started`: hosts missing from
    /// the answer are gone (unless an event newer than the query showed
    /// them), with their services.
    pub(crate) fn replace_hosts(&mut self, hosts: Vec<Host>, started: u64) {
        let fetched: HashSet<HostName> = hosts.iter().map(|host| host.name.clone()).collect();
        let gone: Vec<ObjectKey> = self
            .hosts
            .keys()
            .filter(|name| !fetched.contains(*name))
            .map(|name| ObjectKey::Host { name: name.clone() })
            .collect();
        self.remove_objects(&gone, started);
        for host in hosts {
            self.put_host(host, started);
        }
        self.changes.all = true;
        self.changes.any = true;
    }

    /// Applies every service, queried with `detail` at `started`: services
    /// missing from the answer are gone (unless an event newer than the
    /// query showed them).
    pub(crate) fn replace_services(
        &mut self,
        services: Vec<Service>,
        detail: Detail,
        started: u64,
    ) {
        let fetched: HashSet<ServiceKey> =
            services.iter().map(|service| service.key.clone()).collect();
        let gone: Vec<ObjectKey> = self
            .services
            .keys()
            .filter(|key| !fetched.contains(*key))
            .map(|key| ObjectKey::Service { key: key.clone() })
            .collect();
        self.remove_objects(&gone, started);
        for service in services {
            self.put_service(service, detail, started);
        }
        self.changes.all = true;
        self.changes.any = true;
    }

    /// Every service's name, listed at `started` (a paced load's list):
    /// services it doesn't name are gone (unless an event newer than the
    /// list showed them).
    pub(crate) fn retain_services(&mut self, listed: &HashSet<ServiceKey>, started: u64) {
        let gone: Vec<ObjectKey> = self
            .services
            .keys()
            .filter(|key| !listed.contains(*key))
            .map(|key| ObjectKey::Service { key: key.clone() })
            .collect();
        if !gone.is_empty() {
            self.remove_objects(&gone, started);
            self.changes.any = true;
        }
    }

    /// Applies hosts and services queried by name at `started`; `missing`
    /// are gone (unless an event newer than the query showed them).
    /// Returns the missing objects that were removed.
    pub(crate) fn apply_fetched(
        &mut self,
        hosts: Vec<Host>,
        services: Vec<Service>,
        detail: Detail,
        missing: &[ObjectKey],
        started: u64,
    ) -> Vec<ObjectKey> {
        for host in hosts {
            self.put_host(host, started);
        }
        for service in services {
            self.put_service(service, detail, started);
        }
        self.remove_objects(missing, started)
    }

    /// Applies the complete list of `Notification` objects, queried at
    /// `started`: notifications missing from it are gone, unless a by-name
    /// answer newer than the list wrote them. A list older than the one in
    /// the store is ignored.
    pub(crate) fn replace_notifications(&mut self, notifications: Vec<Notification>, started: u64) {
        if started < self.notifications_listed {
            return;
        }
        let newer = |name: &str| {
            self.notifications_fetched
                .get(name)
                .is_some_and(|fetched| *fetched > started)
        };
        let mut lists: BTreeMap<ObjectKey, Vec<Notification>> = BTreeMap::new();
        for notification in notifications {
            if !newer(&notification.name) {
                lists
                    .entry(notification.object.clone())
                    .or_default()
                    .push(notification);
            }
        }
        // What by-name answers newer than the list brought stays.
        for (object, list) in self.icinga_notifications.iter() {
            for notification in list.iter().filter(|n| newer(&n.name)) {
                lists
                    .entry(object.clone())
                    .or_default()
                    .push(notification.clone());
            }
        }
        let lists: BTreeMap<ObjectKey, Arc<[Notification]>> = lists
            .into_iter()
            .map(|(object, mut list)| {
                sort_notifications(&mut list);
                // Unchanged lists keep their allocation.
                let list = match self.icinga_notifications.get(&object) {
                    Some(stored) if **stored == *list => Arc::clone(stored),
                    _ => Arc::from(list),
                };
                (object, list)
            })
            .collect();
        if *self.icinga_notifications != lists {
            self.icinga_notifications = Arc::new(lists);
            self.changes.any = true;
        }
        self.notifications_listed = started;
        self.notifications_fetched
            .retain(|_, fetched| *fetched > started);
    }

    /// Applies `Notification` objects queried by name at `started`;
    /// `missing` (names Icinga doesn't know) are gone. Names a newer answer
    /// (a list or another by-name query) already wrote are left alone.
    pub(crate) fn apply_fetched_notifications(
        &mut self,
        found: Vec<Notification>,
        missing: &[String],
        started: u64,
    ) {
        for notification in found {
            if self.may_write_notification(&notification.name, started) {
                self.notifications_fetched
                    .insert(notification.name.clone(), started);
                self.put_notification(notification);
            }
        }
        for name in missing {
            if self.may_write_notification(name, started) {
                self.notifications_fetched.insert(name.clone(), started);
                if let Some(object) = notification_object(name) {
                    self.remove_notification(&object, name);
                }
            }
        }
    }

    /// Whether a by-name answer sent at `started` is at least as new as
    /// whatever last wrote the notification `name`.
    fn may_write_notification(&self, name: &str, started: u64) -> bool {
        started >= self.notifications_listed
            && self
                .notifications_fetched
                .get(name)
                .is_none_or(|fetched| started >= *fetched)
    }

    fn put_notification(&mut self, notification: Notification) {
        let current = self
            .icinga_notifications
            .get(&notification.object)
            .and_then(|list| list.iter().find(|n| n.name == notification.name));
        if current == Some(&notification) {
            return;
        }
        let map = Arc::make_mut(&mut self.icinga_notifications);
        let mut list = map
            .get(&notification.object)
            .map(|list| list.to_vec())
            .unwrap_or_default();
        list.retain(|n| n.name != notification.name);
        let object = notification.object.clone();
        list.push(notification);
        sort_notifications(&mut list);
        map.insert(object, Arc::from(list));
        self.changes.any = true;
    }

    fn remove_notification(&mut self, object: &ObjectKey, name: &str) {
        let Some(stored) = self.icinga_notifications.get(object) else {
            return;
        };
        if !stored.iter().any(|n| n.name == name) {
            return;
        }
        let list: Vec<Notification> = stored.iter().filter(|n| n.name != name).cloned().collect();
        let map = Arc::make_mut(&mut self.icinga_notifications);
        if list.is_empty() {
            map.remove(object);
        } else {
            map.insert(object.clone(), Arc::from(list));
        }
        self.changes.any = true;
    }

    /// Replaces the host groups.
    pub(crate) fn set_host_groups(&mut self, groups: Vec<HostGroup>) {
        if set_list(&mut self.host_groups, groups, &mut self.changes) {
            self.changes.groups = true;
        }
    }

    /// Replaces the service groups.
    pub(crate) fn set_service_groups(&mut self, groups: Vec<ServiceGroup>) {
        if set_list(&mut self.service_groups, groups, &mut self.changes) {
            self.changes.groups = true;
        }
    }

    /// Replaces the dependencies.
    pub(crate) fn set_dependencies(&mut self, dependencies: Vec<Dependency>) {
        set_list(&mut self.dependencies, dependencies, &mut self.changes);
    }

    /// Replaces the endpoints and the zones.
    pub(crate) fn set_cluster(&mut self, endpoints: Vec<Endpoint>, zones: Vec<Zone>) {
        set_list(&mut self.endpoints, endpoints, &mut self.changes);
        set_list(&mut self.zones, zones, &mut self.changes);
    }

    /// Sets the endpoints' `connected` as Icinga reported it (by name;
    /// `local`, the node the engine talks to, stays connected: Icinga
    /// reports its own endpoint as not connected), and keeps their numbers
    /// for the cluster health page.
    pub(crate) fn set_endpoint_states(&mut self, states: &[EndpointState], local: &str) {
        let stats_changed = states
            .iter()
            .any(|state| self.health.endpoints.get(&state.name) != Some(&state.stats));
        if stats_changed {
            let health = Arc::make_mut(&mut self.health);
            for state in states {
                health.endpoints.insert(state.name.clone(), state.stats);
            }
            self.changes.any = true;
        }
        let changed = self.endpoints.iter().any(|endpoint| {
            endpoint.name != local
                && states.iter().any(|state| {
                    state.name == endpoint.name && state.connected != endpoint.connected
                })
        });
        if !changed {
            return;
        }
        for endpoint in Arc::make_mut(&mut self.endpoints) {
            if endpoint.name == local {
                continue;
            }
            if let Some(state) = states.iter().find(|state| state.name == endpoint.name) {
                endpoint.connected = state.connected;
            }
        }
        self.changes.any = true;
    }

    /// The heartbeat objects, left out of lists, counts, rules and
    /// notifications.
    pub(crate) fn excluded(&self) -> &Arc<BTreeSet<ServiceKey>> {
        &self.excluded
    }

    /// Whether `key` is a heartbeat object.
    pub(crate) fn is_excluded(&self, key: &ObjectKey) -> bool {
        key.as_service()
            .is_some_and(|service| self.excluded.contains(service))
    }

    /// Sets the heartbeat objects; those that join or leave the set are
    /// marked changed, so the dashboards take them out (or back in).
    pub(crate) fn set_excluded(&mut self, excluded: BTreeSet<ServiceKey>) {
        if *self.excluded == excluded {
            return;
        }
        for key in excluded.symmetric_difference(&self.excluded) {
            self.changes.objects.insert(ObjectKey::from(key.clone()));
        }
        self.excluded = Arc::new(excluded);
        self.changes.any = true;
    }

    /// The cluster health page's data.
    pub(crate) fn health(&self) -> &ClusterHealth {
        &self.health
    }

    /// Changes the cluster health page's data (marks the store changed).
    pub(crate) fn update_health(&mut self, change: impl FnOnce(&mut ClusterHealth)) {
        let mut health = (*self.health).clone();
        change(&mut health);
        if health != *self.health {
            self.health = Arc::new(health);
            self.changes.any = true;
        }
    }

    /// The endpoints and zones (for the node list's states).
    pub(crate) fn cluster(&self) -> (&[Endpoint], &[Zone]) {
        (&self.endpoints, &self.zones)
    }

    /// Whether the answer that last wrote `key` already reflects stream
    /// line `seq` (it was sent after the line was read), so applying the
    /// line changes nothing ([`Applied::Stale`]).
    pub(crate) fn reflects(&self, key: &ObjectKey, seq: u64) -> bool {
        self.seqs.get(key).is_some_and(|seqs| seq <= seqs.fetched)
    }

    /// Whether what the store holds of `key` already includes a state
    /// change whose check result ended at `end`: Icinga sets
    /// `last_state_change` (and, for a hard change, `last_hard_state_change`)
    /// to that result's `execution_end`, so either being that late or later
    /// means the answer was composed after the change, even when its line
    /// came after the request went out.
    pub(crate) fn shows_change(&self, key: &ObjectKey, end: Timestamp) -> bool {
        let end = end.as_unix_seconds();
        self.check(key).is_some_and(|check| {
            check.last_state_change.as_unix_seconds() >= end
                || check.last_hard_state_change.as_unix_seconds() >= end
        })
    }

    /// The answer that last wrote `key` reflects stream line `seq` and
    /// those before it ([`Store::shows_change`]): applying them would take
    /// the object back to an older state, so they become
    /// [`Applied::Stale`] like lines read before the request went out.
    /// (Only for the lines that waited during the first load, all of one
    /// stream; a line an answer overtook later is judged by its own
    /// check's time instead, `Store::overtaken`.)
    pub(crate) fn note_reflected(&mut self, key: &ObjectKey, seq: u64) {
        self.mark_fetched(key.clone(), seq);
    }

    fn evented_after(&self, key: &ObjectKey, started: u64) -> bool {
        self.seqs
            .get(key)
            .is_some_and(|seqs| seqs.evented > started)
    }

    /// Which groups of `key`'s event fields lines newer than an answer
    /// sent at `started` wrote.
    fn newer(&self, key: &ObjectKey, started: u64) -> Newer {
        let seqs = self.seqs.get(key).copied().unwrap_or_default();
        Newer {
            check: seqs.checked > started,
            acknowledgement: seqs.checked.max(seqs.handled) > started,
            depth: seqs.depth > started,
        }
    }

    /// An answer sent at `started` was applied to `key`: its depth is the
    /// store's now, and the answer its source (see the module notes on
    /// downtime depths), unless a newer line wrote it (`kept`).
    fn depth_answered(&mut self, key: &ObjectKey, kept: bool) {
        if kept {
            // Its state may have changed: a provisional depth of a new
            // problem is asked for.
            self.recheck_unsettled(key);
            return;
        }
        self.seqs.entry(key.clone()).or_default().depth_line = None;
        self.counted.remove(key);
        self.settle(key);
    }

    /// Whether an answer sent at `started` is older than what the store
    /// knows of `key`: a newer answer wrote it, or a newer answer (or
    /// deletion) found it gone. Such an answer changes nothing about it.
    fn superseded(&self, key: &ObjectKey, started: u64) -> bool {
        self.seqs
            .get(key)
            .is_some_and(|seqs| seqs.fetched > started)
            || self
                .removed
                .get(key)
                .is_some_and(|removed| *removed > started)
    }

    /// Remembers that `key` was gone as of `seq`.
    fn tombstone(&mut self, key: ObjectKey, seq: u64) {
        if self.removed.len() >= MAX_TOMBSTONES && !self.removed.contains_key(&key) {
            let mut seqs: Vec<u64> = self.removed.values().copied().collect();
            let middle = seqs.len() / 2;
            let (_, cutoff, _) = seqs.select_nth_unstable(middle);
            let cutoff = *cutoff;
            self.removed.retain(|_, removed| *removed > cutoff);
        }
        let removed = self.removed.entry(key).or_default();
        *removed = (*removed).max(seq);
    }

    /// An `ObjectDeleted` event (read as line `seq`) for a host or service:
    /// answers sent before it can't bring the object (back) into the
    /// store. Whether it is gone is still asked (it may have been created
    /// again).
    pub(crate) fn note_deleted(&mut self, key: ObjectKey, seq: u64) {
        self.tombstone(key, seq);
    }

    fn mark_fetched(&mut self, key: ObjectKey, started: u64) {
        let seqs = self.seqs.entry(key).or_default();
        seqs.fetched = seqs.fetched.max(started);
    }

    /// An answer's `flapping` is Icinga's raw flag, which it recomputes
    /// with every check result even with flap detection off (`ic-api` keeps
    /// it only where the object's switch is on): with the global switch
    /// off (`/v1/status`), nothing is flapping as Icinga means it
    /// (`Checkable::IsFlapping`), and no `Flapping` event will say so.
    fn flapping_as_icinga_means_it(&self, check: &mut CheckInfo) {
        if self
            .status
            .as_ref()
            .is_some_and(|status| !status.flap_detection_enabled)
        {
            check.flapping = false;
        }
    }

    fn put_host(&mut self, mut host: Host, started: u64) {
        self.flapping_as_icinga_means_it(&mut host.check);
        let key = host.key();
        if self.superseded(&key, started) {
            return;
        }
        self.removed.remove(&key);
        let newer = self.newer(&key, started);
        if let Some(stored) = self.hosts.get(&host.name).cloned() {
            keep_newer_fields(&mut host.check, &stored.check, newer);
            if newer.check {
                host.state = stored.state;
            } else {
                let before = ObjectView::of(CheckableState::Host(stored.state), &stored.check);
                let after = ObjectView::of(CheckableState::Host(host.state), &host.check);
                if before != after {
                    self.discovered.push(Discovered {
                        object: key.clone(),
                        before,
                        after: Some(after),
                    });
                }
            }
            self.depth_answered(&key, newer.depth);
            if *stored == host {
                self.mark_fetched(key, started);
                return;
            }
        } else {
            self.depth_answered(&key, newer.depth);
            self.arrived(
                &key,
                ObjectView::of(CheckableState::Host(host.state), &host.check),
            );
        }
        Arc::make_mut(&mut self.hosts).insert(host.name.clone(), Arc::new(host));
        self.mark_fetched(key.clone(), started);
        self.changes.any = true;
        self.changes.objects.insert(key);
    }

    fn put_service(&mut self, mut service: Service, detail: Detail, started: u64) {
        self.flapping_as_icinga_means_it(&mut service.check);
        let key = service.object_key();
        if self.superseded(&key, started) {
            // Even a full answer: its result and links are older than the
            // object in the store (hydration asks again for what the UI
            // shows; a reconcile fetches problems not held in full).
            return;
        }
        self.removed.remove(&key);
        let newer = self.newer(&key, started);
        if let Some(stored) = self.services.get(&service.key).cloned() {
            keep_newer_fields(&mut service.check, &stored.check, newer);
            if newer.check {
                service.state = stored.state;
            } else {
                let before = ObjectView::of(CheckableState::Service(stored.state), &stored.check);
                let after = ObjectView::of(CheckableState::Service(service.state), &service.check);
                if before != after {
                    self.discovered.push(Discovered {
                        object: key.clone(),
                        before,
                        after: Some(after),
                    });
                }
            }
            self.depth_answered(&key, newer.depth);
            if detail == Detail::Lean {
                // A lean answer has no check result and no links: keep what
                // a full fetch or an event brought.
                if service.check.result.is_none() {
                    service.check.result.clone_from(&stored.check.result);
                }
                if self.full.contains(&service.key) {
                    service.links.clone_from(&stored.links);
                }
            }
            if *stored == service {
                if detail == Detail::Full {
                    self.full.insert(service.key.clone());
                }
                self.mark_fetched(key, started);
                return;
            }
        } else {
            self.depth_answered(&key, newer.depth);
            self.arrived(
                &key,
                ObjectView::of(CheckableState::Service(service.state), &service.check),
            );
        }
        if detail == Detail::Full {
            self.full.insert(service.key.clone());
        }
        Arc::make_mut(&mut self.services).insert(service.key.clone(), Arc::new(service));
        self.mark_fetched(key.clone(), started);
        self.changes.any = true;
        self.changes.objects.insert(key);
    }

    /// Removes objects Icinga no longer knows, unless an event or answer
    /// newer than `started` showed them. A removed host takes its services
    /// along. Returns what was removed.
    fn remove_objects(&mut self, keys: &[ObjectKey], started: u64) -> Vec<ObjectKey> {
        let mut removed = Vec::new();
        for key in keys {
            if self.evented_after(key, started) || self.superseded(key, started) {
                continue;
            }
            if !self.contains(key) {
                // Gone already: an older answer mustn't bring it back.
                self.tombstone(key.clone(), started);
                continue;
            }
            match key {
                ObjectKey::Host { name } => {
                    let services: Vec<ObjectKey> = self
                        .services
                        .range(
                            ServiceKey {
                                host: name.clone(),
                                name: Arc::from(""),
                            }..,
                        )
                        .take_while(|(service, _)| &service.host == name)
                        .map(|(service, _)| ObjectKey::Service {
                            key: service.clone(),
                        })
                        .collect();
                    for service in services {
                        self.drop_object(&service, started);
                        removed.push(service);
                    }
                }
                ObjectKey::Service { .. } => {}
            }
            self.drop_object(key, started);
            removed.push(key.clone());
        }
        removed
    }

    /// Removes an object an answer sent at `started` found gone (or, from
    /// a node with a partial view, left out: hidden).
    fn drop_object(&mut self, key: &ObjectKey, started: u64) {
        if let Some((state, check)) = self.checkable(key) {
            let before = ObjectView::of(state, check);
            if self.hiding {
                self.hidden.insert(key.clone(), before);
            } else {
                self.discovered.push(Discovered {
                    object: key.clone(),
                    before,
                    after: None,
                });
            }
        }
        match key {
            ObjectKey::Host { name } => {
                Arc::make_mut(&mut self.hosts).remove(name);
            }
            ObjectKey::Service { key } => {
                Arc::make_mut(&mut self.services).remove(key);
                self.full.remove(key);
            }
        }
        if self.comments.contains_key(key) {
            Arc::make_mut(&mut self.comments).remove(key);
        }
        if self.downtimes.contains_key(key) {
            Arc::make_mut(&mut self.downtimes).remove(key);
        }
        // Icinga deletes an object's notifications with it.
        if let Some(list) = self.icinga_notifications.get(key) {
            for notification in list.iter() {
                self.notifications_fetched.remove(&notification.name);
            }
            Arc::make_mut(&mut self.icinga_notifications).remove(key);
        }
        self.seqs.remove(key);
        self.counted.remove(key);
        self.settle(key);
        self.tombstone(key.clone(), started);
        self.changes.any = true;
        self.changes.objects.insert(key.clone());
    }
}

/// Keeps the groups of the fields events maintain that events newer than
/// the answer wrote (`newer`) from `stored`; the others come from the
/// answer.
fn keep_newer_fields(check: &mut CheckInfo, stored: &CheckInfo, newer: Newer) {
    if newer.check {
        check.state_type = stored.state_type;
        check.last_state_change = stored.last_state_change;
        check.last_hard_state_change = stored.last_hard_state_change;
        check.last_check = stored.last_check;
        check.next_check = stored.next_check;
        check.attempt = stored.attempt;
        check.result.clone_from(&stored.result);
        check.flapping = stored.flapping;
        check.flapping_current = stored.flapping_current;
        check.reachable = stored.reachable;
    }
    if newer.acknowledgement {
        check.acknowledgement = stored.acknowledgement;
        check.acknowledgement_expiry = stored.acknowledgement_expiry;
    }
    if newer.depth {
        check.downtime_depth = stored.downtime_depth;
    }
}

/// Replaces `list` if `new` differs; returns whether it did.
fn set_list<T: PartialEq>(list: &mut Arc<Vec<T>>, new: Vec<T>, changes: &mut Changes) -> bool {
    if **list == new {
        return false;
    }
    *list = Arc::new(new);
    changes.any = true;
    true
}

/// Keys whose lists differ between two maps.
fn changed_keys<'a, T: PartialEq>(
    old: &'a BTreeMap<ObjectKey, Vec<T>>,
    new: &'a BTreeMap<ObjectKey, Vec<T>>,
) -> impl Iterator<Item = ObjectKey> + 'a {
    old.keys()
        .chain(new.keys())
        .filter(|key| old.get(*key) != new.get(*key))
        .cloned()
}

fn upsert_comment(map: &mut BTreeMap<ObjectKey, Vec<Comment>>, comment: Comment) {
    let list = map.entry(comment.object.clone()).or_default();
    match list.iter_mut().find(|stored| stored.name == comment.name) {
        Some(stored) => *stored = comment,
        None => list.push(comment),
    }
}

fn upsert_downtime(map: &mut BTreeMap<ObjectKey, Vec<Downtime>>, downtime: Downtime) {
    let list = map.entry(downtime.object.clone()).or_default();
    match list.iter_mut().find(|stored| stored.name == downtime.name) {
        Some(stored) => *stored = downtime,
        None => list.push(downtime),
    }
}

fn cmp_time(a: Timestamp, b: Timestamp) -> std::cmp::Ordering {
    a.as_unix_seconds().total_cmp(&b.as_unix_seconds())
}

/// Comments oldest first (`Snapshot::comments`).
fn sort_comments(list: &mut [Comment]) {
    list.sort_by(|a, b| cmp_time(a.entry_time, b.entry_time).then_with(|| a.name.cmp(&b.name)));
}

/// Downtimes by start time (`Snapshot::downtimes`).
fn sort_downtimes(list: &mut [Downtime]) {
    list.sort_by(|a, b| cmp_time(a.start_time, b.start_time).then_with(|| a.name.cmp(&b.name)));
}

/// Notifications by name (`Snapshot::icinga_notifications`).
fn sort_notifications(list: &mut Vec<Notification>) {
    list.sort_by(|a, b| a.name.cmp(&b.name));
    list.dedup_by(|a, b| a.name == b.name);
}

/// The host or service a `Notification` belongs to, from its full name
/// (`host!service!name` or `host!name`; Icinga allows no `!` in names).
pub(crate) fn notification_object(name: &str) -> Option<ObjectKey> {
    let (object, short) = name.rsplit_once('!')?;
    if object.is_empty() || short.is_empty() {
        return None;
    }
    match object.split_once('!') {
        Some(_) => ServiceKey::parse(object).map(ObjectKey::from),
        None => Some(ObjectKey::host(object)),
    }
}

/// Removes the item named `name` from `object`'s list (and the list, once
/// empty).
fn remove_named<T>(
    map: &mut BTreeMap<ObjectKey, Vec<T>>,
    object: &ObjectKey,
    name: &str,
    name_of: impl Fn(&T) -> &String,
) -> bool {
    let Some(list) = map.get_mut(object) else {
        return false;
    };
    let before = list.len();
    list.retain(|item| name_of(item) != name);
    let removed = list.len() != before;
    if list.is_empty() {
        map.remove(object);
    }
    removed
}

#[cfg(test)]
mod tests;
