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
//!   list's query.
//!
//! So the store converges to Icinga's state whatever order answers and
//! events arrive in, without pausing the stream during queries.

mod apply;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use ic_api::Detail;
use ic_model::{
    CheckInfo, Comment, Dependency, Downtime, Endpoint, Host, HostGroup, HostName, InstanceStatus,
    ObjectKey, Service, ServiceGroup, ServiceKey, Timestamp,
};
use ic_rules::DashboardRef;

use crate::snapshot::{DashboardResult, Snapshot};
use crate::summary::Tally;

pub(crate) use apply::{Applied, ObjectView};

/// Sequence numbers of the last query answer and the last event that
/// wrote an object.
#[derive(Clone, Copy, Debug, Default)]
struct Seqs {
    /// `started` of the last query whose answer was applied.
    fetched: u64,
    /// `seq` of the last event applied.
    evented: u64,
}

/// What changed since the last snapshot, for publishing and (stage 2) for
/// incremental dashboard evaluation.
#[derive(Clone, Debug, Default)]
pub(crate) struct Changes {
    /// Something changed: publish.
    pub(crate) any: bool,
    /// The object set changed wholesale (a reload): re-evaluate everything.
    pub(crate) all: bool,
    /// Hosts and services that changed, appeared or disappeared.
    pub(crate) objects: BTreeSet<ObjectKey>,
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
    status: Option<Arc<InstanceStatus>>,
    last_event_at: Option<Timestamp>,
    /// Services whose links were loaded ([`Detail::Full`]).
    full: HashSet<ServiceKey>,
    seqs: HashMap<ObjectKey, Seqs>,
    /// `started` of the comment and downtime lists in the store.
    annotations_fetched: u64,
    /// While a comment/downtime list query runs: what events did to each
    /// comment or downtime since, by name.
    annotation_log: Option<HashMap<String, (u64, Annotation)>>,
    changes: Changes,
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

    /// The check details of a host or service.
    pub(crate) fn check(&self, key: &ObjectKey) -> Option<&CheckInfo> {
        match key {
            ObjectKey::Host { name } => self.hosts.get(name).map(|host| &host.check),
            ObjectKey::Service { key } => self.services.get(key).map(|service| &service.check),
        }
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

    /// Forgets everything (the environment now points at another server).
    pub(crate) fn clear(&mut self) {
        *self = Self {
            changes: Changes {
                any: true,
                all: true,
                objects: BTreeSet::new(),
            },
            ..Self::default()
        };
    }

    /// Records when the latest event arrived.
    pub(crate) fn set_last_event_at(&mut self, at: Timestamp) {
        self.last_event_at = Some(at);
        self.changes.any = true;
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
            status: self.status.clone(),
            dashboards,
            last_event_at: self.last_event_at,
            overall: self.overall(),
        }
    }

    /// The summary over every host and service.
    fn overall(&self) -> crate::snapshot::Summary {
        let mut tally = Tally::default();
        for host in self.hosts.values() {
            tally.add_host(host);
        }
        for service in self.services.values() {
            tally.add_service(service, self.hosts.get(&service.key.host).map(Arc::as_ref));
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
        set_list(
            &mut self.host_groups,
            overview.host_groups,
            &mut self.changes,
        );
        set_list(
            &mut self.service_groups,
            overview.service_groups,
            &mut self.changes,
        );
        set_list(
            &mut self.dependencies,
            overview.dependencies,
            &mut self.changes,
        );
        set_list(&mut self.endpoints, overview.endpoints, &mut self.changes);

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

    /// Replaces the host groups.
    pub(crate) fn set_host_groups(&mut self, groups: Vec<HostGroup>) {
        set_list(&mut self.host_groups, groups, &mut self.changes);
    }

    /// Replaces the service groups.
    pub(crate) fn set_service_groups(&mut self, groups: Vec<ServiceGroup>) {
        set_list(&mut self.service_groups, groups, &mut self.changes);
    }

    /// Replaces the dependencies.
    pub(crate) fn set_dependencies(&mut self, dependencies: Vec<Dependency>) {
        set_list(&mut self.dependencies, dependencies, &mut self.changes);
    }

    /// Replaces the endpoints.
    pub(crate) fn set_endpoints(&mut self, endpoints: Vec<Endpoint>) {
        set_list(&mut self.endpoints, endpoints, &mut self.changes);
    }

    fn evented_after(&self, key: &ObjectKey, started: u64) -> bool {
        self.seqs
            .get(key)
            .is_some_and(|seqs| seqs.evented > started)
    }

    fn mark_fetched(&mut self, key: ObjectKey, started: u64) {
        let seqs = self.seqs.entry(key).or_default();
        seqs.fetched = seqs.fetched.max(started);
    }

    fn put_host(&mut self, mut host: Host, started: u64) {
        let key = host.key();
        if let Some(stored) = self.hosts.get(&host.name) {
            if self.evented_after(&key, started) {
                keep_event_fields(&mut host.check, &stored.check);
                host.state = stored.state;
            }
            if **stored == host {
                self.mark_fetched(key, started);
                return;
            }
        }
        Arc::make_mut(&mut self.hosts).insert(host.name.clone(), Arc::new(host));
        self.mark_fetched(key.clone(), started);
        self.changes.any = true;
        self.changes.objects.insert(key);
    }

    fn put_service(&mut self, mut service: Service, detail: Detail, started: u64) {
        let key = service.object_key();
        if let Some(stored) = self.services.get(&service.key) {
            if self.evented_after(&key, started) {
                keep_event_fields(&mut service.check, &stored.check);
                service.state = stored.state;
            }
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
            if **stored == service {
                if detail == Detail::Full {
                    self.full.insert(service.key.clone());
                }
                self.mark_fetched(key, started);
                return;
            }
        }
        if detail == Detail::Full {
            self.full.insert(service.key.clone());
        }
        Arc::make_mut(&mut self.services).insert(service.key.clone(), Arc::new(service));
        self.mark_fetched(key.clone(), started);
        self.changes.any = true;
        self.changes.objects.insert(key);
    }

    /// Removes objects Icinga no longer knows, unless an event newer than
    /// `started` showed them. A removed host takes its services along.
    /// Returns what was removed.
    fn remove_objects(&mut self, keys: &[ObjectKey], started: u64) -> Vec<ObjectKey> {
        let mut removed = Vec::new();
        for key in keys {
            if self.evented_after(key, started) || !self.contains(key) {
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
                        self.drop_object(&service);
                        removed.push(service);
                    }
                }
                ObjectKey::Service { .. } => {}
            }
            self.drop_object(key);
            removed.push(key.clone());
        }
        removed
    }

    fn drop_object(&mut self, key: &ObjectKey) {
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
        self.seqs.remove(key);
        self.changes.any = true;
        self.changes.objects.insert(key.clone());
    }
}

/// Keeps the fields events maintain from `stored` (newer than the answer
/// they are merged into).
fn keep_event_fields(check: &mut CheckInfo, stored: &CheckInfo) {
    check.state_type = stored.state_type;
    check.last_state_change = stored.last_state_change;
    check.last_hard_state_change = stored.last_hard_state_change;
    check.last_check = stored.last_check;
    check.next_check = stored.next_check;
    check.attempt = stored.attempt;
    check.result.clone_from(&stored.result);
    check.acknowledgement = stored.acknowledgement;
    check.acknowledgement_expiry = stored.acknowledgement_expiry;
    check.downtime_depth = stored.downtime_depth;
    check.flapping = stored.flapping;
    check.flapping_current = stored.flapping_current;
    check.reachable = stored.reachable;
}

fn set_list<T: PartialEq>(list: &mut Arc<Vec<T>>, new: Vec<T>, changes: &mut Changes) {
    if **list != new {
        *list = Arc::new(new);
        changes.any = true;
    }
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
