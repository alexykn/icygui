//! Private wire structs for Icinga's JSON and their mapping into `ic-model`
//! types. Everything here is forgiving: odd values fall back to defaults
//! and objects that can't be identified are skipped, never panicking.

use ic_model::{
    AckKind, CheckInfo, CheckResult, CheckableState, Comment, CommentKind, Dependency, Downtime,
    Endpoint, Features, Host, HostGroup, HostName, HostState, InstanceStatus, Links, Notification,
    ObjectKey, Perfdata, Service, ServiceGroup, ServiceKey, ServiceState, StateAfter, StateType,
    Threshold, Timestamp, Vars, Zone, parse_perfdata, parse_perfdata_entry,
};
use serde::Deserialize;
use serde_json::Value;

use crate::detail::Detail;
use crate::lenient::{FromJson, L, number};

/// `{ "results": [...] }`, the envelope of every non-streaming response.
#[derive(Debug, Deserialize)]
pub(crate) struct Results<T> {
    pub(crate) results: Vec<T>,
}

/// One entry of an object query. Entries that failed (an attribute the
/// server doesn't know) have `code`/`status` and no `attrs`.
#[derive(Debug, Deserialize)]
#[serde(bound = "A: Deserialize<'de>")]
pub(crate) struct QueryResult<A> {
    #[serde(default)]
    pub(crate) name: L<String>,
    #[serde(default)]
    pub(crate) attrs: Option<A>,
    #[serde(default)]
    pub(crate) code: L<Option<f64>>,
    #[serde(default)]
    pub(crate) status: L<String>,
}

/// A serialized `CheckResult` (`last_check_result`, events' `check_result`).
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct WireCheckResult {
    output: L<String>,
    performance_data: L<Option<Value>>,
    exit_status: L<Option<f64>>,
    state: L<Option<f64>>,
    schedule_start: L<f64>,
    execution_start: L<f64>,
    execution_end: L<f64>,
    check_source: L<String>,
    active: L<Option<bool>>,
    vars_after: L<Option<Value>>,
}

impl FromJson for Option<WireCheckResult> {
    fn from_json(value: Value) -> Self {
        value
            .is_object()
            .then(|| serde_json::from_value(value).ok())
            .flatten()
    }
}

impl WireCheckResult {
    /// `vars_after.reachable`: whether the checkable was reachable when
    /// this result was processed.
    pub(crate) fn reachable_after(&self) -> Option<bool> {
        let reachable = self.vars_after.0.as_ref()?.get("reachable")?.clone();
        Option::<bool>::from_json(reachable)
    }

    /// `vars_after.state_type`.
    pub(crate) fn state_type_after(&self) -> Option<StateType> {
        let code = number(self.vars_after.0.as_ref()?.get("state_type")?)?;
        state_type(Some(code))
    }

    /// `vars_after` as the state of `object` after Icinga processed this
    /// result: `None` without a state or a state type. For hosts the state
    /// is service-style (`Host::CalculateState`: 0 and 1 are up, 2 and 3
    /// down), and a down host that isn't reachable is unreachable.
    pub(crate) fn state_after(&self, object: &ObjectKey) -> Option<StateAfter> {
        let vars = self.vars_after.0.as_ref()?;
        let code = number(vars.get("state")?)?;
        let state_type = state_type(Some(number(vars.get("state_type")?)?))?;
        let reachable = vars
            .get("reachable")
            .cloned()
            .and_then(Option::<bool>::from_json)
            .unwrap_or(true);
        let attempt = vars.get("attempt").and_then(number).map_or(1, clamp_u32);
        let state = match object {
            ObjectKey::Host { .. } => CheckableState::Host(match clamp_u8(code) {
                0 | 1 => HostState::Up,
                _ if reachable => HostState::Down,
                _ => HostState::Unreachable,
            }),
            ObjectKey::Service { .. } => CheckableState::Service(service_state(Some(code))),
        };
        Some(StateAfter {
            state,
            state_type,
            attempt,
            reachable,
        })
    }

    /// Maps into the domain type. The result's `state` wins over
    /// `exit_status`, which Icinga leaves at 0 for passive results.
    pub(crate) fn into_model(self) -> CheckResult {
        let (output, long_output) = CheckResult::split_output(&self.output.0);
        let exit_status = self.state.0.or(self.exit_status.0).map_or(0, clamp_i32);
        CheckResult {
            output,
            long_output,
            perfdata: self
                .performance_data
                .0
                .as_ref()
                .map(perfdata_from_wire)
                .unwrap_or_default(),
            exit_status,
            schedule_start: Timestamp::from_unix_seconds(self.schedule_start.0),
            execution_start: Timestamp::from_unix_seconds(self.execution_start.0),
            execution_end: Timestamp::from_unix_seconds(self.execution_end.0),
            check_source: self.check_source.0,
            active: self.active.0.unwrap_or(true),
        }
    }
}

/// `performance_data`: an array of strings (`label=value;warn;crit`) or
/// `PerfdataValue` dictionaries; a single string is parsed whole.
pub(crate) fn perfdata_from_wire(value: &Value) -> Vec<Perfdata> {
    match value {
        Value::Array(items) => items
            .iter()
            .filter_map(|item| match item {
                Value::String(text) => parse_perfdata_entry(text),
                Value::Object(map) => perfdata_value(map),
                _ => None,
            })
            .collect(),
        Value::String(text) => parse_perfdata(text),
        _ => Vec::new(),
    }
}

/// A `PerfdataValue` dictionary. Icinga normalises units when it parses
/// perfdata into these (`ms` → `seconds`, `%` → `percent`, `c` →
/// `counter = true`); map the common ones back to plugin notation.
fn perfdata_value(map: &serde_json::Map<String, Value>) -> Option<Perfdata> {
    let label = map.get("label").and_then(Value::as_str)?.to_owned();
    if label.is_empty() {
        return None;
    }
    let unit = map.get("unit").and_then(Value::as_str).unwrap_or_default();
    let counter = map
        .get("counter")
        .and_then(|value| Option::<bool>::from_json(value.clone()))
        .unwrap_or(false);
    let unit = match unit {
        "seconds" => "s".to_owned(),
        "percent" => "%".to_owned(),
        "bytes" => "B".to_owned(),
        "" if counter => "c".to_owned(),
        other => other.to_owned(),
    };
    let threshold = |key: &str| match map.get(key)? {
        Value::String(text) => Threshold::parse(text),
        other => number(other).and_then(|n| Threshold::parse(&n.to_string())),
    };
    Some(Perfdata {
        label,
        value: map.get("value").and_then(number),
        unit,
        warn: threshold("warn"),
        crit: threshold("crit"),
        min: map.get("min").and_then(number),
        max: map.get("max").and_then(number),
    })
}

/// Attributes of a host or a service, as requested by
/// [`Detail::host_attrs`] and [`Detail::service_attrs`].
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct CheckableAttrs {
    name: L<String>,
    display_name: L<String>,
    host_name: L<String>,
    address: L<String>,
    address6: L<String>,
    state: L<Option<f64>>,
    state_type: L<Option<f64>>,
    last_state_change: L<f64>,
    last_hard_state_change: L<f64>,
    last_check: L<Option<f64>>,
    next_check: L<f64>,
    check_attempt: L<Option<f64>>,
    max_check_attempts: L<Option<f64>>,
    last_check_result: L<Option<WireCheckResult>>,
    acknowledgement: L<f64>,
    acknowledgement_expiry: L<f64>,
    downtime_depth: L<f64>,
    flapping: L<bool>,
    flapping_current: L<f64>,
    last_reachable: L<Option<bool>>,
    check_command: L<String>,
    check_interval: L<Option<f64>>,
    retry_interval: L<Option<f64>>,
    command_endpoint: L<String>,
    zone: L<String>,
    enable_active_checks: L<Option<bool>>,
    enable_passive_checks: L<Option<bool>>,
    enable_notifications: L<Option<bool>>,
    enable_event_handler: L<Option<bool>>,
    enable_flapping: L<Option<bool>>,
    enable_perfdata: L<Option<bool>>,
    groups: L<Vec<String>>,
    vars: L<Vars>,
    notes: L<String>,
    notes_url: L<String>,
    action_url: L<String>,
    icon_image: L<String>,
}

impl CheckableAttrs {
    fn check_info(&mut self) -> CheckInfo {
        let defaults = CheckInfo::default();
        let defaults_features = Features::default();
        CheckInfo {
            state_type: state_type(self.state_type.0).unwrap_or(StateType::Hard),
            last_state_change: Timestamp::from_unix_seconds(self.last_state_change.0),
            last_hard_state_change: Timestamp::from_unix_seconds(self.last_hard_state_change.0),
            last_check: self
                .last_check
                .0
                .and_then(|seconds| Timestamp::from_unix_seconds(seconds).non_zero()),
            next_check: Timestamp::from_unix_seconds(self.next_check.0).non_zero(),
            attempt: self.check_attempt.0.map_or(defaults.attempt, clamp_u32),
            max_attempts: self
                .max_check_attempts
                .0
                .map_or(defaults.max_attempts, clamp_u32),
            result: self
                .last_check_result
                .0
                .take()
                .map(WireCheckResult::into_model),
            acknowledgement: ack_kind(self.acknowledgement.0),
            acknowledgement_expiry: Timestamp::from_unix_seconds(self.acknowledgement_expiry.0)
                .non_zero(),
            downtime_depth: clamp_u32(self.downtime_depth.0),
            flapping: self.flapping.0,
            flapping_current: self.flapping_current.0,
            reachable: self.last_reachable.0.unwrap_or(true),
            check_command: std::mem::take(&mut self.check_command.0),
            check_interval: self.check_interval.0.unwrap_or(defaults.check_interval),
            retry_interval: self.retry_interval.0.unwrap_or(defaults.retry_interval),
            command_endpoint: non_empty(std::mem::take(&mut self.command_endpoint.0)),
            zone: non_empty(std::mem::take(&mut self.zone.0)),
            features: Features {
                active_checks: self
                    .enable_active_checks
                    .0
                    .unwrap_or(defaults_features.active_checks),
                passive_checks: self
                    .enable_passive_checks
                    .0
                    .unwrap_or(defaults_features.passive_checks),
                notifications: self
                    .enable_notifications
                    .0
                    .unwrap_or(defaults_features.notifications),
                event_handler: self
                    .enable_event_handler
                    .0
                    .unwrap_or(defaults_features.event_handler),
                flap_detection: self
                    .enable_flapping
                    .0
                    .unwrap_or(defaults_features.flap_detection),
                perfdata: self.enable_perfdata.0.unwrap_or(defaults_features.perfdata),
            },
        }
    }

    fn links(&mut self) -> Links {
        Links {
            notes: std::mem::take(&mut self.notes.0),
            notes_url: std::mem::take(&mut self.notes_url.0),
            action_url: std::mem::take(&mut self.action_url.0),
            icon_image: std::mem::take(&mut self.icon_image.0),
        }
    }

    /// Whether the object has never been checked. A full object is
    /// pending without a check result. A lean one has none to look at, so
    /// it's pending when Icinga's `last_check` (the last result's
    /// `schedule_end`, -1 without one) is negative.
    fn is_pending(&self, detail: Detail) -> bool {
        match detail {
            Detail::Full => self.last_check_result.0.is_none(),
            Detail::Lean => self.last_check.0.is_some_and(|seconds| seconds < 0.0),
        }
    }

    /// Maps a host. `full_name` is the result's `name`; `None` if the host
    /// can't be identified. `detail` is what was asked for.
    pub(crate) fn into_host(mut self, full_name: &str, detail: Detail) -> Option<Host> {
        let name = first_non_empty(full_name, &self.name.0)?.to_owned();
        let pending = self.is_pending(detail);
        let check = self.check_info();
        let state = if pending {
            HostState::Pending
        } else {
            host_state(self.state.0, check.reachable)
        };
        let display_name = first_non_empty(&self.display_name.0, &name)
            .unwrap_or_default()
            .to_owned();
        let links = self.links();
        Some(Host {
            name: HostName::new(&name),
            display_name,
            address: self.address.0,
            address6: self.address6.0,
            state,
            check,
            groups: self.groups.0,
            vars: self.vars.0,
            links,
        })
    }

    /// Maps a service. `full_name` is the result's `host!service` name; the
    /// attributes `host_name` and `name` win when present. `detail` is what
    /// was asked for.
    pub(crate) fn into_service(mut self, full_name: &str, detail: Detail) -> Option<Service> {
        let key = if !self.host_name.0.is_empty() && !self.name.0.is_empty() {
            ServiceKey::new(&self.host_name.0, &self.name.0)
        } else {
            ServiceKey::parse(full_name)?
        };
        let pending = self.is_pending(detail);
        let check = self.check_info();
        let state = if pending {
            ServiceState::Pending
        } else {
            service_state(self.state.0)
        };
        let display_name = first_non_empty(&self.display_name.0, &key.name)
            .unwrap_or_default()
            .to_owned();
        let links = self.links();
        Some(Service {
            key,
            display_name,
            state,
            check,
            groups: self.groups.0,
            vars: self.vars.0,
            links,
        })
    }
}

/// Attributes of a `Comment` (query results and event payloads).
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct CommentAttrs {
    #[serde(rename = "__name")]
    full_name: L<String>,
    name: L<String>,
    host_name: L<String>,
    service_name: L<String>,
    author: L<String>,
    text: L<String>,
    entry_type: L<f64>,
    entry_time: L<f64>,
    expire_time: L<f64>,
    persistent: L<bool>,
}

/// Attributes requested for comments.
pub(crate) const COMMENT_ATTRS: &[&str] = &[
    "host_name",
    "service_name",
    "author",
    "text",
    "entry_type",
    "entry_time",
    "expire_time",
    "persistent",
];

impl CommentAttrs {
    /// Maps a comment. `full_name` is the query result's name (empty for
    /// event payloads, which carry `__name`).
    pub(crate) fn into_model(self, full_name: &str) -> Option<Comment> {
        let object = object_key(&self.host_name.0, &self.service_name.0)?;
        let name = full_object_name(full_name, &self.full_name.0, &object, &self.name.0)?;
        Some(Comment {
            name,
            object,
            author: self.author.0,
            text: self.text.0,
            kind: CommentKind::from_code(clamp_u8(self.entry_type.0)),
            entry_time: Timestamp::from_unix_seconds(self.entry_time.0),
            expire_time: Timestamp::from_unix_seconds(self.expire_time.0).non_zero(),
            persistent: self.persistent.0,
        })
    }
}

/// Attributes of a `Downtime` (query results and event payloads).
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct DowntimeAttrs {
    #[serde(rename = "__name")]
    full_name: L<String>,
    name: L<String>,
    host_name: L<String>,
    service_name: L<String>,
    author: L<String>,
    comment: L<String>,
    start_time: L<f64>,
    end_time: L<f64>,
    fixed: L<Option<bool>>,
    duration: L<f64>,
    entry_time: L<f64>,
    trigger_time: L<f64>,
    triggered_by: L<String>,
    parent: L<String>,
    config_owner: L<String>,
    scheduled_by: L<String>,
    is_in_effect: L<Option<bool>>,
}

/// Attributes requested for downtimes. (`is_in_effect` is not an Icinga 2
/// attribute; requesting it would fail the query. It's honoured when a
/// payload carries it.)
pub(crate) const DOWNTIME_ATTRS: &[&str] = &[
    "host_name",
    "service_name",
    "author",
    "comment",
    "start_time",
    "end_time",
    "fixed",
    "duration",
    "entry_time",
    "trigger_time",
    "triggered_by",
    "parent",
    "config_owner",
    "scheduled_by",
];

impl DowntimeAttrs {
    /// Maps a downtime; `now` decides whether it's in effect (Icinga's
    /// `Downtime::IsInEffect`) unless the payload says so itself.
    pub(crate) fn into_model(self, full_name: &str, now: Timestamp) -> Option<Downtime> {
        self.map(full_name, |window, flag| {
            flag.unwrap_or_else(|| {
                downtime_in_effect(
                    window.fixed,
                    window.start,
                    window.end,
                    window.trigger_time,
                    window.duration,
                    now,
                )
            })
        })
    }

    /// Maps the payload of a `DowntimeRemoved` event at `removed_at`:
    /// `in_effect` says whether the downtime was in effect until it ended,
    /// see [`downtime_in_effect_until`].
    pub(crate) fn into_removed_model(
        self,
        full_name: &str,
        removed_at: Timestamp,
    ) -> Option<Downtime> {
        self.map(full_name, |window, flag| {
            flag == Some(true)
                || downtime_in_effect_until(
                    window.fixed,
                    window.start,
                    window.end,
                    window.trigger_time,
                    window.duration,
                    removed_at,
                )
        })
    }

    /// Maps a downtime; `in_effect` decides from its window and the
    /// payload's own `is_in_effect`, if any.
    fn map(
        self,
        full_name: &str,
        in_effect: impl FnOnce(&Window, Option<bool>) -> bool,
    ) -> Option<Downtime> {
        let object = object_key(&self.host_name.0, &self.service_name.0)?;
        let name = full_object_name(full_name, &self.full_name.0, &object, &self.name.0)?;
        let window = Window {
            // Icinga's `fixed` defaults to true in actions and to false in
            // config objects; downtimes always carry it, assume fixed if not.
            fixed: self.fixed.0.unwrap_or(true),
            start: Timestamp::from_unix_seconds(self.start_time.0),
            end: Timestamp::from_unix_seconds(self.end_time.0),
            trigger_time: Timestamp::from_unix_seconds(self.trigger_time.0).non_zero(),
            duration: self.duration.0,
        };
        let in_effect = in_effect(&window, self.is_in_effect.0);
        let Window {
            fixed,
            start: start_time,
            end: end_time,
            trigger_time,
            ..
        } = window;
        Some(Downtime {
            name,
            object,
            author: self.author.0,
            comment: self.comment.0,
            start_time,
            end_time,
            fixed,
            duration: self.duration.0,
            entry_time: Timestamp::from_unix_seconds(self.entry_time.0),
            trigger_time,
            triggered_by: non_empty(self.triggered_by.0),
            parent: non_empty(self.parent.0),
            in_effect,
            config_owned: !self.config_owner.0.is_empty() || !self.scheduled_by.0.is_empty(),
        })
    }
}

/// The times that decide whether a downtime is in effect.
struct Window {
    fixed: bool,
    start: Timestamp,
    end: Timestamp,
    trigger_time: Option<Timestamp>,
    duration: f64,
}

/// Icinga's `Downtime::IsInEffect`: fixed downtimes during `[start, end)`,
/// flexible ones from their trigger time for `duration` seconds.
pub(crate) fn downtime_in_effect(
    fixed: bool,
    start: Timestamp,
    end: Timestamp,
    trigger_time: Option<Timestamp>,
    duration: f64,
    now: Timestamp,
) -> bool {
    let now = now.as_unix_seconds();
    if fixed {
        return now >= start.as_unix_seconds() && now < end.as_unix_seconds();
    }
    trigger_time.is_some_and(|trigger| now < trigger.as_unix_seconds() + duration)
}

/// How long before the end of its effect a downtime that ran out is
/// looked at (see [`downtime_in_effect_until`]).
const LAST_INSTANT: f64 = 0.001;

/// Whether a downtime removed at `removed_at` was in effect until then, so
/// that its removal ends a downtime period.
///
/// Icinga removes a downtime that ran out from a timer shortly *after* the
/// end of its effect (`end_time` for fixed downtimes, `trigger_time +
/// duration` for triggered flexible ones; `downtime.cpp`), and doesn't set
/// `remove_time` then. At that moment `IsInEffect` is already false, so
/// asking it at the removal would say that a downtime never ends while in
/// effect. Instead it's asked at the removal or, when that comes after the
/// end of the effect, just before that end. A downtime cancelled inside
/// its window was in effect; one cancelled before it started, or a
/// flexible one that never triggered, was not.
pub(crate) fn downtime_in_effect_until(
    fixed: bool,
    start: Timestamp,
    end: Timestamp,
    trigger_time: Option<Timestamp>,
    duration: f64,
    removed_at: Timestamp,
) -> bool {
    let effect_end = if fixed {
        Some(end.as_unix_seconds())
    } else {
        trigger_time.map(|trigger| trigger.as_unix_seconds() + duration)
    };
    let at = match effect_end {
        Some(effect_end) if removed_at.as_unix_seconds() >= effect_end => {
            Timestamp::from_unix_seconds(effect_end - LAST_INSTANT)
        }
        _ => removed_at,
    };
    downtime_in_effect(fixed, start, end, trigger_time, duration, at)
}

/// Attributes of host and service groups.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct GroupAttrs {
    name: L<String>,
    display_name: L<String>,
}

/// Attributes requested for groups.
pub(crate) const GROUP_ATTRS: &[&str] = &["name", "display_name"];

impl GroupAttrs {
    fn names(self, full_name: &str) -> Option<(String, String)> {
        let name = first_non_empty(full_name, &self.name.0)?.to_owned();
        let display_name = first_non_empty(&self.display_name.0, &name)
            .unwrap_or_default()
            .to_owned();
        Some((name, display_name))
    }

    pub(crate) fn into_host_group(self, full_name: &str) -> Option<HostGroup> {
        self.names(full_name)
            .map(|(name, display_name)| HostGroup { name, display_name })
    }

    pub(crate) fn into_service_group(self, full_name: &str) -> Option<ServiceGroup> {
        self.names(full_name)
            .map(|(name, display_name)| ServiceGroup { name, display_name })
    }
}

/// Attributes of a `Dependency`.
#[derive(Debug, Default, Deserialize)]
#[expect(
    clippy::struct_field_names,
    reason = "Icinga's attribute names, mapped one to one"
)]
#[serde(default)]
pub(crate) struct DependencyAttrs {
    child_host_name: L<String>,
    child_service_name: L<String>,
    parent_host_name: L<String>,
    parent_service_name: L<String>,
}

/// Attributes requested for dependencies.
pub(crate) const DEPENDENCY_ATTRS: &[&str] = &[
    "child_host_name",
    "child_service_name",
    "parent_host_name",
    "parent_service_name",
];

impl DependencyAttrs {
    pub(crate) fn into_model(self, full_name: &str) -> Option<Dependency> {
        Some(Dependency {
            name: full_name.to_owned(),
            child: object_key(&self.child_host_name.0, &self.child_service_name.0)?,
            parent: object_key(&self.parent_host_name.0, &self.parent_service_name.0)?,
        })
    }
}

/// Attributes of an `Endpoint`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct EndpointAttrs {
    name: L<String>,
    zone: L<String>,
    connected: L<bool>,
}

/// Attributes requested for endpoints.
pub(crate) const ENDPOINT_ATTRS: &[&str] = &["name", "zone", "connected"];

/// Attributes requested for an endpoint's connection state alone.
pub(crate) const ENDPOINT_STATE_ATTRS: &[&str] = &["connected"];

impl EndpointAttrs {
    /// Maps an endpoint; `zone_of` finds the zone listing it (an endpoint's
    /// own `zone` attribute is where it was *defined*, usually empty).
    ///
    /// `local` is the name of the endpoint the client talks to (the node
    /// name). Icinga reports its own endpoint as not connected, since it
    /// has no connection to itself; it is the one we are connected to, so
    /// it counts as connected (Icinga's own cluster status treats it the
    /// same way).
    pub(crate) fn into_model(
        self,
        full_name: &str,
        zone_of: impl Fn(&str) -> Option<String>,
        local: Option<&str>,
    ) -> Option<Endpoint> {
        let name = first_non_empty(full_name, &self.name.0)?.to_owned();
        let zone = zone_of(&name).unwrap_or(self.zone.0);
        let connected = self.connected.0 || local == Some(name.as_str());
        Some(Endpoint {
            name,
            zone,
            connected,
        })
    }
}

impl EndpointAttrs {
    /// An endpoint's name (the query result's) and `connected`, as Icinga
    /// says it.
    pub(crate) fn into_state(self, full_name: &str) -> Option<(String, bool)> {
        let name = first_non_empty(full_name, &self.name.0)?.to_owned();
        Some((name, self.connected.0))
    }
}

/// Attributes of a `Notification` (`lib/icinga/notification.ti`): only
/// the object it belongs to and what it last did.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct NotificationAttrs {
    host_name: L<String>,
    service_name: L<String>,
    last_notification: L<f64>,
    notified_problem_users: L<Vec<String>>,
}

/// Attributes requested for notifications: about 150 bytes per object on
/// the wire, so even one notification object per service of a large
/// installation stays a fraction of the lean service list.
pub(crate) const NOTIFICATION_ATTRS: &[&str] = &[
    "host_name",
    "service_name",
    "last_notification",
    "notified_problem_users",
];

impl NotificationAttrs {
    /// Maps a notification; `full_name` is the query result's name
    /// (`host!service!name` or `host!name`).
    pub(crate) fn into_model(self, full_name: &str) -> Option<Notification> {
        if full_name.is_empty() {
            return None;
        }
        Some(Notification {
            name: full_name.to_owned(),
            object: object_key(&self.host_name.0, &self.service_name.0)?,
            last_notification: Timestamp::from_unix_seconds(self.last_notification.0).non_zero(),
            notified_problem_users: self
                .notified_problem_users
                .0
                .into_iter()
                .filter(|user| !user.is_empty())
                .collect(),
        })
    }
}

/// Attributes of a `Zone` (`lib/remote/zone.ti`).
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct ZoneAttrs {
    pub(crate) endpoints: L<Vec<String>>,
    parent: L<String>,
    global: L<bool>,
}

/// Attributes requested for the zone tree ([`crate::Client::zones`]).
pub(crate) const ZONE_TREE_ATTRS: &[&str] = &["endpoints", "global", "parent"];

impl ZoneAttrs {
    /// Maps a zone; `full_name` is the query result's name. Blank entries
    /// in `endpoints` are dropped, a blank parent is none.
    pub(crate) fn into_model(self, full_name: &str) -> Option<Zone> {
        if full_name.is_empty() {
            return None;
        }
        Some(Zone {
            name: full_name.to_owned(),
            parent: Some(self.parent.0).filter(|parent| !parent.is_empty()),
            endpoints: self
                .endpoints
                .0
                .into_iter()
                .filter(|endpoint| !endpoint.is_empty())
                .collect(),
            global: self.global.0,
        })
    }
}

/// `GET /v1` (`Accept: application/json`).
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct InfoResult {
    pub(crate) user: L<String>,
    pub(crate) permissions: L<Vec<String>>,
    pub(crate) version: L<String>,
}

/// One entry of `/v1/status/<name>`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct StatusResult {
    pub(crate) name: L<String>,
    pub(crate) status: L<Option<Value>>,
}

/// The node name from `/v1/status/IcingaApplication`'s `status` object.
pub(crate) fn node_name(application: &Value) -> Option<String> {
    application
        .pointer("/icingaapplication/app/node_name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
}

/// Maps `/v1/status/IcingaApplication` and `/v1/status/CIB` (their
/// `status` objects). `checks_per_minute` counts active host and service
/// checks in the last minute.
pub(crate) fn instance_status(application: &Value, cib: &Value) -> InstanceStatus {
    let app = application
        .pointer("/icingaapplication/app")
        .unwrap_or(&Value::Null);
    let text = |key: &str| {
        app.get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    // Icinga's global switches default to on.
    let flag = |key: &str| {
        app.get(key)
            .and_then(|value| Option::<bool>::from_json(value.clone()))
            .unwrap_or(true)
    };
    let stat = |key: &str| cib.get(key).and_then(number).unwrap_or(0.0);
    InstanceStatus {
        node_name: text("node_name"),
        version: text("version"),
        program_start: Timestamp::from_unix_seconds(
            app.get("program_start").and_then(number).unwrap_or(0.0),
        ),
        notifications_enabled: flag("enable_notifications"),
        host_checks_enabled: flag("enable_host_checks"),
        service_checks_enabled: flag("enable_service_checks"),
        event_handlers_enabled: flag("enable_event_handlers"),
        flap_detection_enabled: flag("enable_flapping"),
        perfdata_enabled: flag("enable_perfdata"),
        checks_per_minute: stat("active_host_checks_1min") + stat("active_service_checks_1min"),
        avg_latency: stat("avg_latency"),
        avg_execution_time: stat("avg_execution_time"),
    }
}

/// One entry of an action response.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct ActionResultWire {
    pub(crate) code: L<f64>,
    pub(crate) status: L<String>,
    pub(crate) name: L<String>,
}

/// The host or service named by `host_name` and `service_name` (empty =
/// the host itself).
pub(crate) fn object_key(host: &str, service: &str) -> Option<ObjectKey> {
    match (host.is_empty(), service.is_empty()) {
        (true, _) => None,
        (false, true) => Some(ObjectKey::host(host)),
        (false, false) => Some(ObjectKey::service(host, service)),
    }
}

/// The full name of a comment or downtime: the query result's name, the
/// payload's `__name`, or composed like Icinga's name composer
/// (`host!short` / `host!service!short`).
fn full_object_name(
    result_name: &str,
    dunder_name: &str,
    object: &ObjectKey,
    short_name: &str,
) -> Option<String> {
    if let Some(name) = first_non_empty(result_name, dunder_name) {
        return Some(name.to_owned());
    }
    (!short_name.is_empty()).then(|| format!("{}!{short_name}", object.full_name()))
}

/// Host state from Icinga's numeric state and reachability. Icinga hosts
/// only know 0 and 1; anything else is treated as down.
pub(crate) fn host_state(code: Option<f64>, reachable: bool) -> HostState {
    match code.map(clamp_u8) {
        Some(0) => HostState::Up,
        _ if reachable => HostState::Down,
        _ => HostState::Unreachable,
    }
}

/// Service state from Icinga's numeric state; out-of-range is unknown.
pub(crate) fn service_state(code: Option<f64>) -> ServiceState {
    code.map(clamp_u8)
        .and_then(ServiceState::from_code)
        .unwrap_or(ServiceState::Unknown)
}

pub(crate) fn state_type(code: Option<f64>) -> Option<StateType> {
    code.map(clamp_u8).and_then(StateType::from_code)
}

pub(crate) fn ack_kind(code: f64) -> AckKind {
    AckKind::from_code(clamp_u8(code))
}

fn non_empty(text: String) -> Option<String> {
    (!text.is_empty()).then_some(text)
}

fn first_non_empty<'a>(first: &'a str, second: &'a str) -> Option<&'a str> {
    [first, second].into_iter().find(|text| !text.is_empty())
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to the target range first"
)]
pub(crate) fn clamp_u8(value: f64) -> u8 {
    value.round().clamp(0.0, f64::from(u8::MAX)) as u8
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to the target range first"
)]
pub(crate) fn clamp_u32(value: f64) -> u32 {
    value.round().clamp(0.0, f64::from(u32::MAX)) as u32
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "clamped to the target range first"
)]
pub(crate) fn clamp_i32(value: f64) -> i32 {
    value
        .round()
        .clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
}

#[cfg(test)]
#[path = "wire_tests.rs"]
mod tests;
