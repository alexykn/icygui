//! The mock's runtime state ("world"): every object with the attributes
//! Icinga keeps, plus the event bus, timers and simulator state.
//!
//! All changes go through methods on [`World`], which emit the same events,
//! in the same order, as Icinga does.

pub(crate) mod attrs;
mod checker;
pub(crate) mod load;
pub(crate) mod logic;
mod snapshot;
pub(crate) mod stats;
pub(crate) mod types;

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

use ic_model::ObjectKey;
use serde_json::{Map, Value as Json};

use crate::events::{EventBus, EventType};
use crate::rng::Rng;
use crate::sim::SimState;

pub(crate) use attrs::{ObjKind, ObjRef};
pub(crate) use checker::CheckQueue;
pub(crate) use load::version_number;
pub(crate) use logic::{CheckInput, DepType, ProcessOutcome};
pub(crate) use stats::CheckStats;
pub(crate) use types::{
    Checkable, CommandData, CommentData, DependencyData, DowntimeData, EndpointData, FeatureData,
    GroupData, NotificationData, ObjMeta, UserData, ZoneData,
};

/// What `/v1/status/IcingaApplication` reports.
#[derive(Clone, Debug, PartialEq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "mirrors IcingaApplication's global switches"
)]
pub(crate) struct AppInfo {
    pub(crate) node_name: String,
    /// The zone of the node's endpoint (Icinga's `ZoneName`).
    pub(crate) zone_name: String,
    pub(crate) version: String,
    pub(crate) program_start: f64,
    pub(crate) pid: u32,
    pub(crate) environment: String,
    pub(crate) enable_notifications: bool,
    pub(crate) enable_event_handlers: bool,
    pub(crate) enable_flapping: bool,
    pub(crate) enable_host_checks: bool,
    pub(crate) enable_service_checks: bool,
    pub(crate) enable_perfdata: bool,
}

/// An `execute-command` waiting for its result.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PendingExecution {
    pub(crate) due: f64,
    pub(crate) object: String,
    pub(crate) id: String,
    pub(crate) command: String,
    pub(crate) command_type: String,
}

/// Everything the mock serves.
#[derive(Debug)]
pub(crate) struct World {
    /// Seconds added to the wall clock (`advance_clock`).
    pub(crate) clock_offset: f64,
    /// When the scenario was loaded: its times are shifted to it.
    pub(crate) loaded_at: f64,
    pub(crate) app: AppInfo,
    pub(crate) hosts: BTreeMap<String, Checkable>,
    /// Services by host name, then short name.
    pub(crate) services: BTreeMap<String, BTreeMap<String, Checkable>>,
    pub(crate) host_groups: BTreeMap<String, GroupData>,
    pub(crate) service_groups: BTreeMap<String, GroupData>,
    pub(crate) comments: BTreeMap<String, CommentData>,
    pub(crate) downtimes: BTreeMap<String, DowntimeData>,
    pub(crate) dependencies: BTreeMap<String, DependencyData>,
    pub(crate) endpoints: BTreeMap<String, EndpointData>,
    pub(crate) zones: BTreeMap<String, ZoneData>,
    pub(crate) users: BTreeMap<String, UserData>,
    /// Icinga's own notifications, by full name.
    pub(crate) notifications: BTreeMap<String, NotificationData>,
    pub(crate) check_commands: BTreeMap<String, CommandData>,
    pub(crate) event_commands: BTreeMap<String, CommandData>,
    /// The enabled features' objects (`checker`, `notification`,
    /// `icingadb`), by kind.
    pub(crate) features: BTreeMap<ObjKind, FeatureData>,
    /// Comment names by object full name.
    comments_by_object: BTreeMap<String, BTreeSet<String>>,
    /// Downtime names by object full name.
    downtimes_by_object: BTreeMap<String, BTreeSet<String>>,
    /// Notification names by object full name.
    notifications_by_object: BTreeMap<String, BTreeSet<String>>,
    /// Dependency names by child / parent full name.
    deps_by_child: BTreeMap<String, Vec<String>>,
    deps_by_parent: BTreeMap<String, Vec<String>>,
    next_comment_id: u64,
    next_downtime_id: u64,
    /// UUIDs for runtime objects (seeded, so names are reproducible).
    pub(crate) names: Rng,
    pub(crate) stats: CheckStats,
    pub(crate) bus: EventBus,
    /// `reschedule-check`s waiting for their time: object → when.
    pub(crate) scheduled_checks: BTreeMap<String, f64>,
    /// Checks due now, run at the checker's rate.
    pub(crate) checks: CheckQueue,
    pub(crate) pending_executions: Vec<PendingExecution>,
    pub(crate) pinned: HashSet<String>,
    pub(crate) sim: SimState,
    /// Seconds between `reschedule-check` and the resulting check.
    pub(crate) reschedule_delay: f64,
    /// Objects checked in real time (`Scenario::realtime`), and when each
    /// is due next.
    pub(crate) realtime: BTreeMap<String, f64>,
    /// No check runs (`MockControl::stop_checks`): the checker hangs.
    pub(crate) checks_stopped: bool,
    /// Endpoints whose checker hangs while they stay connected
    /// (`MockControl::stop_checks_on`): they run no check of their zone's
    /// and answer no check pinned to them.
    pub(crate) checkers_stopped: BTreeSet<String>,
}

/// Wall-clock seconds since the epoch.
pub(crate) fn wall_clock() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

impl World {
    /// The node's globals for filters (`NodeName`, `ZoneName`).
    pub(crate) fn filter_node(&self) -> crate::filter::Node {
        crate::filter::Node {
            name: self.app.node_name.clone(),
            zone: self.app.zone_name.clone(),
        }
    }

    /// The mock's current time: the wall clock plus `advance_clock` offsets.
    pub(crate) fn now(&self) -> f64 {
        wall_clock() + self.clock_offset
    }

    // --- lookups --------------------------------------------------------

    /// A host or service by full name (`host` or `host!service`).
    pub(crate) fn checkable(&self, full_name: &str) -> Option<&Checkable> {
        match full_name.split_once('!') {
            Some((host, service)) => self.services.get(host)?.get(service),
            None => self.hosts.get(full_name),
        }
    }

    pub(crate) fn checkable_mut(&mut self, full_name: &str) -> Option<&mut Checkable> {
        match full_name.split_once('!') {
            Some((host, service)) => self.services.get_mut(host)?.get_mut(service),
            None => self.hosts.get_mut(full_name),
        }
    }

    pub(crate) fn checkable_by_key(&self, key: &ObjectKey) -> Option<&Checkable> {
        match key {
            ObjectKey::Host { name } => self.hosts.get(name.as_str()),
            ObjectKey::Service { key } => self.services.get(key.host.as_str())?.get(&*key.name),
        }
    }

    /// Every service, ordered by host then name.
    pub(crate) fn all_services(&self) -> impl Iterator<Item = &Checkable> {
        self.services.values().flat_map(BTreeMap::values)
    }

    /// Every host and service.
    pub(crate) fn all_checkables(&self) -> impl Iterator<Item = &Checkable> {
        self.hosts.values().chain(self.all_services())
    }

    /// Services of a host.
    pub(crate) fn services_of(&self, host: &str) -> impl Iterator<Item = &Checkable> {
        self.services
            .get(host)
            .into_iter()
            .flat_map(BTreeMap::values)
    }

    /// Comments of an object, by name.
    pub(crate) fn comments_of(&self, object: &str) -> Vec<String> {
        self.comments_by_object
            .get(object)
            .map(|names| names.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Downtimes of an object, by name.
    pub(crate) fn downtimes_of(&self, object: &str) -> Vec<String> {
        self.downtimes_by_object
            .get(object)
            .map(|names| names.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Notifications of an object (a host's own, not its services'), by
    /// name.
    pub(crate) fn notifications_of(&self, object: &str) -> Vec<String> {
        self.notifications_by_object
            .get(object)
            .map(|names| names.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Adds a notification and indexes it by its object.
    pub(crate) fn insert_notification(&mut self, notification: NotificationData) {
        self.notifications_by_object
            .entry(notification.object())
            .or_default()
            .insert(notification.name.clone());
        self.notifications
            .insert(notification.name.clone(), notification);
    }

    /// Dependencies whose child is `object`.
    pub(crate) fn dependencies_of_child(&self, object: &str) -> Vec<&DependencyData> {
        self.deps_by_child
            .get(object)
            .into_iter()
            .flatten()
            .filter_map(|name| self.dependencies.get(name))
            .collect()
    }

    /// Objects that depend on `object` directly (`GetChildren`).
    pub(crate) fn children_of(&self, object: &str) -> BTreeSet<String> {
        self.deps_by_parent
            .get(object)
            .into_iter()
            .flatten()
            .filter_map(|name| self.dependencies.get(name))
            .map(|dep| dep.child.full_name())
            .collect()
    }

    /// Objects that depend on `object` directly or indirectly
    /// (`GetAllChildren`, at most 32 levels).
    pub(crate) fn all_children_of(&self, object: &str) -> BTreeSet<String> {
        let mut all = BTreeSet::new();
        let mut frontier = self.children_of(object);
        for _ in 0..=32 {
            let mut next = BTreeSet::new();
            for child in &frontier {
                if all.insert(child.clone()) {
                    next.extend(self.children_of(child));
                }
            }
            if next.is_empty() {
                break;
            }
            frontier = next;
        }
        all
    }

    /// Downtimes whose `parent` is `name` (`Downtime::GetChildren`).
    pub(crate) fn downtime_children(&self, name: &str) -> Vec<String> {
        self.downtimes
            .values()
            .filter(|d| d.parent == name)
            .map(|d| d.name.clone())
            .collect()
    }

    // --- index maintenance --------------------------------------------

    pub(crate) fn index_comment(&mut self, comment: &CommentData) {
        self.comments_by_object
            .entry(comment.object().full_name())
            .or_default()
            .insert(comment.name.clone());
    }

    pub(crate) fn unindex_comment(&mut self, comment: &CommentData) {
        let object = comment.object().full_name();
        if let Some(names) = self.comments_by_object.get_mut(&object) {
            names.remove(&comment.name);
            if names.is_empty() {
                self.comments_by_object.remove(&object);
            }
        }
    }

    pub(crate) fn index_downtime(&mut self, downtime: &DowntimeData) {
        self.downtimes_by_object
            .entry(downtime.object().full_name())
            .or_default()
            .insert(downtime.name.clone());
    }

    pub(crate) fn unindex_downtime(&mut self, downtime: &DowntimeData) {
        let object = downtime.object().full_name();
        if let Some(names) = self.downtimes_by_object.get_mut(&object) {
            names.remove(&downtime.name);
            if names.is_empty() {
                self.downtimes_by_object.remove(&object);
            }
        }
    }

    pub(crate) fn index_dependency(&mut self, dependency: &DependencyData) {
        self.deps_by_child
            .entry(dependency.child.full_name())
            .or_default()
            .push(dependency.name.clone());
        self.deps_by_parent
            .entry(dependency.parent.full_name())
            .or_default()
            .push(dependency.name.clone());
    }

    pub(crate) fn next_comment_legacy_id(&mut self) -> u64 {
        let id = self.next_comment_id;
        self.next_comment_id += 1;
        id
    }

    pub(crate) fn next_downtime_legacy_id(&mut self) -> u64 {
        let id = self.next_downtime_id;
        self.next_downtime_id += 1;
        id
    }

    // --- events ----------------------------------------------------------

    /// Publishes an event built by `build` if anyone listens for `ty`.
    pub(crate) fn emit(&mut self, ty: EventType, build: impl FnOnce(&Self) -> Map<String, Json>) {
        if !self.bus.wants(ty) {
            return;
        }
        let mut event = build(self);
        event.insert("type".into(), Json::String(ty.name().to_owned()));
        let now = self.now();
        event.insert("timestamp".into(), crate::json::num(now));
        self.bus.publish(ty, Json::Object(event), now);
    }

    /// `ObjectCreated` / `ObjectDeleted` / `ObjectModified`.
    pub(crate) fn emit_object_change(&mut self, ty: EventType, object_type: &str, name: &str) {
        self.emit(ty, |_| {
            let mut event = Map::new();
            event.insert("object_type".into(), Json::String(object_type.to_owned()));
            event.insert("object_name".into(), Json::String(name.to_owned()));
            event
        });
    }

    /// The `host` / `service` keys of checkable events.
    pub(crate) fn event_object_fields(event: &mut Map<String, Json>, checkable: &Checkable) {
        event.insert("host".into(), Json::String(checkable.host_name.clone()));
        if let Some(service) = &checkable.service_name {
            event.insert("service".into(), Json::String(service.clone()));
        }
    }
}
