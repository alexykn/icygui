//! Runtime objects with the attributes Icinga keeps (`*.ti` files).

use ic_model::ObjectKey;
use serde_json::{Map, Value as Json};

use crate::json::{int, num};

/// Where an object was defined (`source_location`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SourceLocation {
    pub(crate) path: String,
    pub(crate) first_line: u32,
    pub(crate) first_column: u32,
    pub(crate) last_line: u32,
    pub(crate) last_column: u32,
}

impl SourceLocation {
    pub(crate) fn file(path: &str, line: u32, lines: u32) -> Self {
        Self {
            path: path.to_owned(),
            first_line: line,
            first_column: 1,
            last_line: line + lines,
            last_column: 1,
        }
    }

    pub(crate) fn to_json(&self) -> Json {
        let mut map = Map::new();
        map.insert("first_column".into(), int(self.first_column));
        map.insert("first_line".into(), int(self.first_line));
        map.insert("last_column".into(), int(self.last_column));
        map.insert("last_line".into(), int(self.last_line));
        map.insert("path".into(), Json::String(self.path.clone()));
        Json::Object(map)
    }
}

/// `ConfigObject` attributes every object has.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ObjMeta {
    pub(crate) templates: Vec<String>,
    /// `_etc` for configuration files, `_api` for runtime objects.
    pub(crate) package: String,
    pub(crate) zone: String,
    pub(crate) source: SourceLocation,
    /// Creation/modification time for runtime objects, `0` for config files.
    pub(crate) version: f64,
}

impl ObjMeta {
    pub(crate) fn config(templates: Vec<String>, zone: &str, source: SourceLocation) -> Self {
        Self {
            templates,
            package: "_etc".to_owned(),
            zone: zone.to_owned(),
            source,
            version: 0.0,
        }
    }

    /// The `ConfigObject` attributes `Serialize(obj, FAConfig | FAState)`
    /// writes (as in event payloads).
    pub(crate) fn insert_serialized_attrs(&self, map: &mut Map<String, Json>) {
        map.insert(
            "templates".into(),
            Json::Array(self.templates.iter().cloned().map(Json::String).collect()),
        );
        map.insert("package".into(), Json::String(self.package.clone()));
        map.insert("zone".into(), Json::String(self.zone.clone()));
        map.insert("source_location".into(), self.source.to_json());
        map.insert("version".into(), num(self.version));
    }
}

/// `vars_before` / `vars_after` of a check result.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct VarsState {
    pub(crate) attempt: u32,
    pub(crate) reachable: bool,
    pub(crate) state: u8,
    pub(crate) state_type: u8,
}

impl VarsState {
    pub(crate) fn to_json(self) -> Json {
        let mut map = Map::new();
        map.insert("attempt".into(), int(self.attempt));
        map.insert("reachable".into(), Json::Bool(self.reachable));
        map.insert("state".into(), int(self.state));
        map.insert("state_type".into(), int(self.state_type));
        Json::Object(map)
    }
}

/// A check result (`checkresult.ti`).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CheckResultData {
    pub(crate) schedule_start: f64,
    pub(crate) schedule_end: f64,
    pub(crate) execution_start: f64,
    pub(crate) execution_end: f64,
    /// The command line (array) or null for passive results.
    pub(crate) command: Json,
    pub(crate) exit_status: i64,
    /// Raw state: 0 OK, 1 WARNING, 2 CRITICAL, 3 UNKNOWN (hosts: 0 or 2).
    pub(crate) state: u8,
    pub(crate) previous_hard_state: u8,
    /// Output including long output, without performance data.
    pub(crate) output: String,
    /// `None` is written as `null` (passive results without perfdata).
    pub(crate) performance_data: Option<Vec<String>>,
    pub(crate) active: bool,
    pub(crate) check_source: String,
    pub(crate) scheduling_source: String,
    pub(crate) ttl: f64,
    pub(crate) vars_before: Option<VarsState>,
    pub(crate) vars_after: Option<VarsState>,
}

impl CheckResultData {
    /// The fields of Icinga's `CheckResult` type, which filters can access
    /// (`to_json` writes them and `type`, which is not a field).
    pub(crate) const FIELDS: [&'static str; 16] = [
        "active",
        "check_source",
        "command",
        "execution_end",
        "execution_start",
        "exit_status",
        "output",
        "performance_data",
        "previous_hard_state",
        "schedule_end",
        "schedule_start",
        "scheduling_source",
        "state",
        "ttl",
        "vars_after",
        "vars_before",
    ];

    /// `Serialize(cr)`: every state attribute plus `type`.
    pub(crate) fn to_json(&self) -> Json {
        let mut map = Map::new();
        map.insert("active".into(), Json::Bool(self.active));
        map.insert(
            "check_source".into(),
            Json::String(self.check_source.clone()),
        );
        map.insert("command".into(), self.command.clone());
        map.insert("execution_end".into(), num(self.execution_end));
        map.insert("execution_start".into(), num(self.execution_start));
        map.insert("exit_status".into(), int(self.exit_status));
        map.insert("output".into(), Json::String(self.output.clone()));
        map.insert(
            "performance_data".into(),
            self.performance_data
                .as_ref()
                .map_or(Json::Null, |entries| {
                    Json::Array(entries.iter().cloned().map(Json::String).collect())
                }),
        );
        map.insert("previous_hard_state".into(), int(self.previous_hard_state));
        map.insert("schedule_end".into(), num(self.schedule_end));
        map.insert("schedule_start".into(), num(self.schedule_start));
        map.insert(
            "scheduling_source".into(),
            Json::String(self.scheduling_source.clone()),
        );
        map.insert("state".into(), int(self.state));
        map.insert("ttl".into(), num(self.ttl));
        map.insert("type".into(), Json::String("CheckResult".into()));
        map.insert(
            "vars_after".into(),
            self.vars_after.map_or(Json::Null, VarsState::to_json),
        );
        map.insert(
            "vars_before".into(),
            self.vars_before.map_or(Json::Null, VarsState::to_json),
        );
        Json::Object(map)
    }
}

/// A host or a service with Icinga's runtime attributes.
#[derive(Clone, Debug, PartialEq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "mirrors Icinga's checkable attributes one to one"
)]
pub(crate) struct Checkable {
    pub(crate) host_name: String,
    /// The service's short name; `None` for hosts.
    pub(crate) service_name: Option<String>,
    pub(crate) display_name: String,
    pub(crate) address: String,
    pub(crate) address6: String,
    pub(crate) groups: Vec<String>,
    pub(crate) vars: Option<Map<String, Json>>,
    pub(crate) meta: ObjMeta,
    // Configuration.
    pub(crate) check_command: String,
    pub(crate) max_check_attempts: u32,
    pub(crate) check_period: String,
    pub(crate) check_timeout: Option<f64>,
    pub(crate) check_interval: f64,
    pub(crate) retry_interval: f64,
    pub(crate) event_command: String,
    pub(crate) volatile: bool,
    pub(crate) enable_active_checks: bool,
    pub(crate) enable_passive_checks: bool,
    pub(crate) enable_event_handler: bool,
    pub(crate) enable_notifications: bool,
    pub(crate) enable_flapping: bool,
    pub(crate) enable_perfdata: bool,
    pub(crate) flapping_threshold_low: f64,
    pub(crate) flapping_threshold_high: f64,
    pub(crate) notes: String,
    pub(crate) notes_url: String,
    pub(crate) action_url: String,
    pub(crate) icon_image: String,
    pub(crate) icon_image_alt: String,
    pub(crate) command_endpoint: String,
    // State.
    pub(crate) state_raw: u8,
    pub(crate) last_state_raw: u8,
    pub(crate) last_hard_state_raw: u8,
    /// `previous + current * 100` of the hard states (99 = never).
    pub(crate) last_hard_states_raw: u16,
    pub(crate) state_type: u8,
    pub(crate) last_state_type: u8,
    pub(crate) check_attempt: u32,
    pub(crate) last_reachable: bool,
    pub(crate) cr: Option<CheckResultData>,
    pub(crate) next_check: f64,
    pub(crate) last_state_change: f64,
    pub(crate) last_hard_state_change: f64,
    pub(crate) previous_state_change: f64,
    pub(crate) last_state_unreachable: f64,
    /// Services: last OK, WARNING, CRITICAL, UNKNOWN. Hosts: last UP, DOWN.
    pub(crate) last_state_at: [f64; 4],
    pub(crate) force_next_check: bool,
    pub(crate) force_next_notification: bool,
    pub(crate) acknowledgement: u8,
    pub(crate) acknowledgement_expiry: f64,
    pub(crate) acknowledgement_last_change: f64,
    pub(crate) flapping: bool,
    pub(crate) flapping_current: f64,
    pub(crate) flapping_last_change: f64,
    /// Ring buffer of "state changed" bits over the last 20 results.
    pub(crate) flapping_buffer: u32,
    pub(crate) flapping_index: u32,
    pub(crate) flapping_last_state: u8,
    pub(crate) executions: Option<Map<String, Json>>,
}

impl Checkable {
    pub(crate) fn is_service(&self) -> bool {
        self.service_name.is_some()
    }

    pub(crate) fn full_name(&self) -> String {
        match &self.service_name {
            Some(service) => format!("{}!{service}", self.host_name),
            None => self.host_name.clone(),
        }
    }

    pub(crate) fn short_name(&self) -> &str {
        self.service_name.as_deref().unwrap_or(&self.host_name)
    }

    pub(crate) fn type_name(&self) -> &'static str {
        if self.is_service() { "Service" } else { "Host" }
    }

    /// `Host::CalculateState` for hosts, the raw state for services.
    pub(crate) fn state(&self) -> u8 {
        if self.is_service() {
            self.state_raw
        } else {
            host_state(self.state_raw)
        }
    }

    /// `IsStateOK`: OK for services, OK or WARNING (= UP) for hosts.
    pub(crate) fn is_state_ok(&self, raw: u8) -> bool {
        if self.is_service() {
            raw == 0
        } else {
            host_state(raw) == 0
        }
    }

    pub(crate) fn has_been_checked(&self) -> bool {
        self.cr.is_some()
    }

    /// `GetProblem`: a check result whose state is not OK.
    pub(crate) fn problem(&self) -> bool {
        self.cr
            .as_ref()
            .is_some_and(|cr| !self.is_state_ok(cr.state))
    }

    /// `GetLastCheck`: the check result's `schedule_end`, `-1` if never.
    pub(crate) fn last_check(&self) -> f64 {
        self.cr.as_ref().map_or(-1.0, |cr| cr.schedule_end)
    }
}

/// `Host::CalculateState`: OK and WARNING are UP (0), everything else DOWN.
pub(crate) fn host_state(raw: u8) -> u8 {
    u8::from(raw > 1)
}

/// A comment (`comment.ti`).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CommentData {
    pub(crate) name: String,
    pub(crate) short_name: String,
    pub(crate) host_name: String,
    pub(crate) service_name: Option<String>,
    pub(crate) author: String,
    pub(crate) text: String,
    pub(crate) entry_time: f64,
    /// 1 user, 2 downtime, 3 flapping, 4 acknowledgement.
    pub(crate) entry_type: u8,
    pub(crate) expire_time: f64,
    pub(crate) persistent: bool,
    pub(crate) sticky: bool,
    pub(crate) legacy_id: u64,
    pub(crate) meta: ObjMeta,
}

impl CommentData {
    pub(crate) fn object(&self) -> ObjectKey {
        object_key(&self.host_name, self.service_name.as_deref())
    }

    /// `Serialize(comment, FAConfig | FAState)` as in `CommentAdded`.
    pub(crate) fn to_event_json(&self) -> Json {
        let mut map = Map::new();
        self.meta.insert_serialized_attrs(&mut map);
        map.insert("__name".into(), Json::String(self.name.clone()));
        map.insert("name".into(), Json::String(self.short_name.clone()));
        map.insert("type".into(), Json::String("Comment".into()));
        self.insert_own_attrs(&mut map);
        map.insert("sticky".into(), Json::Bool(self.sticky));
        Json::Object(map)
    }

    pub(crate) fn insert_own_attrs(&self, map: &mut Map<String, Json>) {
        map.insert("host_name".into(), Json::String(self.host_name.clone()));
        map.insert(
            "service_name".into(),
            Json::String(self.service_name.clone().unwrap_or_default()),
        );
        map.insert("author".into(), Json::String(self.author.clone()));
        map.insert("text".into(), Json::String(self.text.clone()));
        map.insert("entry_time".into(), num(self.entry_time));
        map.insert("entry_type".into(), int(self.entry_type));
        map.insert("expire_time".into(), num(self.expire_time));
        map.insert("persistent".into(), Json::Bool(self.persistent));
        map.insert(
            "legacy_id".into(),
            int(i64::try_from(self.legacy_id).unwrap_or(i64::MAX)),
        );
    }
}

/// A downtime (`downtime.ti`).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DowntimeData {
    pub(crate) name: String,
    pub(crate) short_name: String,
    pub(crate) host_name: String,
    pub(crate) service_name: Option<String>,
    pub(crate) author: String,
    pub(crate) comment: String,
    pub(crate) start_time: f64,
    pub(crate) end_time: f64,
    pub(crate) entry_time: f64,
    pub(crate) trigger_time: f64,
    pub(crate) fixed: bool,
    pub(crate) duration: f64,
    pub(crate) triggered_by: String,
    pub(crate) scheduled_by: String,
    pub(crate) parent: String,
    pub(crate) triggers: Vec<String>,
    pub(crate) legacy_id: u64,
    pub(crate) remove_time: f64,
    pub(crate) config_owner: String,
    pub(crate) config_owner_hash: String,
    pub(crate) authoritative_zone: String,
    pub(crate) meta: ObjMeta,
}

impl DowntimeData {
    pub(crate) fn object(&self) -> ObjectKey {
        object_key(&self.host_name, self.service_name.as_deref())
    }

    /// `Downtime::IsInEffect`.
    pub(crate) fn is_in_effect(&self, now: f64) -> bool {
        if self.fixed {
            now >= self.start_time && now < self.end_time
        } else if self.trigger_time == 0.0 {
            false
        } else {
            now < self.trigger_time + self.duration
        }
    }

    /// `Downtime::IsTriggered`.
    pub(crate) fn is_triggered(&self, now: f64) -> bool {
        self.trigger_time > 0.0 && self.trigger_time <= now
    }

    /// `Downtime::IsExpired`.
    pub(crate) fn is_expired(&self, now: f64) -> bool {
        if self.fixed {
            self.end_time < now
        } else if self.is_triggered(now) && !self.is_in_effect(now) {
            true
        } else {
            !self.is_triggered(now) && self.end_time < now
        }
    }

    /// `Downtime::CanBeTriggered`.
    pub(crate) fn can_be_triggered(&self, now: f64) -> bool {
        if self.is_in_effect(now) && self.is_triggered(now) {
            return false;
        }
        if self.is_expired(now) {
            return false;
        }
        !(now < self.start_time || now > self.end_time)
    }

    /// `Serialize(downtime, FAConfig | FAState)` as in downtime events.
    pub(crate) fn to_event_json(&self) -> Json {
        let mut map = Map::new();
        self.meta.insert_serialized_attrs(&mut map);
        map.insert("__name".into(), Json::String(self.name.clone()));
        map.insert("name".into(), Json::String(self.short_name.clone()));
        map.insert("type".into(), Json::String("Downtime".into()));
        self.insert_own_attrs(&mut map);
        Json::Object(map)
    }

    pub(crate) fn insert_own_attrs(&self, map: &mut Map<String, Json>) {
        map.insert("host_name".into(), Json::String(self.host_name.clone()));
        map.insert(
            "service_name".into(),
            Json::String(self.service_name.clone().unwrap_or_default()),
        );
        map.insert("author".into(), Json::String(self.author.clone()));
        map.insert("comment".into(), Json::String(self.comment.clone()));
        map.insert("start_time".into(), num(self.start_time));
        map.insert("end_time".into(), num(self.end_time));
        map.insert("entry_time".into(), num(self.entry_time));
        map.insert("trigger_time".into(), num(self.trigger_time));
        map.insert("fixed".into(), Json::Bool(self.fixed));
        map.insert("duration".into(), num(self.duration));
        map.insert(
            "triggered_by".into(),
            Json::String(self.triggered_by.clone()),
        );
        map.insert(
            "scheduled_by".into(),
            Json::String(self.scheduled_by.clone()),
        );
        map.insert("parent".into(), Json::String(self.parent.clone()));
        map.insert(
            "triggers".into(),
            Json::Array(self.triggers.iter().cloned().map(Json::String).collect()),
        );
        map.insert(
            "legacy_id".into(),
            int(i64::try_from(self.legacy_id).unwrap_or(i64::MAX)),
        );
        map.insert("remove_time".into(), num(self.remove_time));
        map.insert(
            "config_owner".into(),
            Json::String(self.config_owner.clone()),
        );
        map.insert(
            "config_owner_hash".into(),
            Json::String(self.config_owner_hash.clone()),
        );
        map.insert(
            "authoritative_zone".into(),
            Json::String(self.authoritative_zone.clone()),
        );
    }
}

/// A dependency (`dependency.ti`).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DependencyData {
    pub(crate) name: String,
    pub(crate) short_name: String,
    pub(crate) child: ObjectKey,
    pub(crate) parent: ObjectKey,
    pub(crate) redundancy_group: String,
    pub(crate) disable_checks: bool,
    pub(crate) disable_notifications: bool,
    pub(crate) ignore_soft_states: bool,
    pub(crate) period: String,
    /// `None` uses the defaults: `[Up]` for host parents, `[OK, Warning]`
    /// for service parents.
    pub(crate) states: Option<Vec<String>>,
    pub(crate) meta: ObjMeta,
}

impl DependencyData {
    /// The state filter as a bit mask (OK 1, Warning 2, Critical 4,
    /// Unknown 8, Up 16, Down 32).
    pub(crate) fn state_filter(&self) -> u32 {
        match &self.states {
            Some(states) => states
                .iter()
                .map(|s| match s.as_str() {
                    "OK" => 1,
                    "Warning" => 2,
                    "Critical" => 4,
                    "Unknown" => 8,
                    "Up" => 16,
                    "Down" => 32,
                    _ => 0,
                })
                .fold(0, |acc, bit| acc | bit),
            None => match self.parent {
                ObjectKey::Host { .. } => 16,
                ObjectKey::Service { .. } => 1 | 2,
            },
        }
    }
}

/// A host or service group.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct GroupData {
    pub(crate) name: String,
    pub(crate) display_name: String,
    /// Parent groups; Icinga writes `null` when there are none.
    pub(crate) groups: Vec<String>,
    pub(crate) notes: String,
    pub(crate) notes_url: String,
    pub(crate) action_url: String,
    pub(crate) vars: Option<Map<String, Json>>,
    pub(crate) meta: ObjMeta,
}

/// A cluster endpoint.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct EndpointData {
    pub(crate) name: String,
    /// The zone that lists it (not the endpoint's own `zone` attribute).
    pub(crate) member_of: String,
    pub(crate) host: String,
    pub(crate) port: String,
    pub(crate) connected: bool,
    pub(crate) icinga_version: u64,
    pub(crate) meta: ObjMeta,
}

/// A cluster zone.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ZoneData {
    pub(crate) name: String,
    pub(crate) parent: String,
    pub(crate) endpoints: Vec<String>,
    pub(crate) global: bool,
    pub(crate) meta: ObjMeta,
}

/// An Icinga `User` object.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct UserData {
    pub(crate) name: String,
    pub(crate) display_name: String,
    pub(crate) email: String,
    pub(crate) groups: Vec<String>,
    pub(crate) meta: ObjMeta,
}

/// A `CheckCommand` or `EventCommand`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CommandData {
    pub(crate) name: String,
    /// The plugin command line; `None` for commands Icinga runs internally
    /// (`dummy`, `icinga`, ...).
    pub(crate) command: Option<Vec<String>>,
    pub(crate) arguments: Option<Map<String, Json>>,
    pub(crate) timeout: f64,
    pub(crate) vars: Option<Map<String, Json>>,
    /// The `execute` function (`Internal#PluginCheck`, `Internal#DummyCheck`, ...).
    pub(crate) execute: &'static str,
    pub(crate) meta: ObjMeta,
}

pub(crate) fn object_key(host: &str, service: Option<&str>) -> ObjectKey {
    match service {
        Some(service) => ObjectKey::service(host, service),
        None => ObjectKey::host(host),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_result_fields_are_the_serialized_keys_but_type() {
        let cr = CheckResultData {
            schedule_start: 1.0,
            schedule_end: 2.0,
            execution_start: 1.0,
            execution_end: 2.0,
            command: Json::Null,
            exit_status: 0,
            state: 0,
            previous_hard_state: 0,
            output: "OK".into(),
            performance_data: None,
            active: true,
            check_source: "master".into(),
            scheduling_source: "master".into(),
            ttl: 0.0,
            vars_before: None,
            vars_after: None,
        };
        let Json::Object(map) = cr.to_json() else {
            unreachable!("a dictionary")
        };
        let keys: Vec<&str> = map
            .keys()
            .map(String::as_str)
            .filter(|key| *key != "type")
            .collect();
        assert_eq!(keys, CheckResultData::FIELDS);
    }
}
