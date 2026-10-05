//! Object attributes as the API returns them (`ObjectQueryHandler`,
//! `*.ti`), joins, and the filter scope over the mock's objects.

use std::sync::Arc;

use serde_json::{Map, Value as Json};

use super::World;
use super::types::{Checkable, CommandData, GroupData};
use crate::filter::{self, EvalError, ObjectRef, Scope};
use crate::json::{int, num};

/// The object types the mock serves.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum ObjKind {
    Host,
    Service,
    HostGroup,
    ServiceGroup,
    Comment,
    Downtime,
    Dependency,
    Endpoint,
    Zone,
    User,
    CheckCommand,
    EventCommand,
}

/// A reference to one object.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ObjRef {
    pub(crate) kind: ObjKind,
    pub(crate) name: String,
}

/// Why an attribute could not be resolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ResolveError {
    /// The type has no such field (`Invalid field specified: x`).
    UnknownField(String),
    /// The field exists but users can't see it (`no_user_view`).
    Hidden(String),
}

const CHECKABLE_COMMON: &[&str] = &[
    "__name",
    "acknowledgement",
    "acknowledgement_expiry",
    "acknowledgement_last_change",
    "action_url",
    "active",
    "check_attempt",
    "check_command",
    "check_interval",
    "check_period",
    "check_timeout",
    "command_endpoint",
    "display_name",
    "downtime_depth",
    "enable_active_checks",
    "enable_event_handler",
    "enable_flapping",
    "enable_notifications",
    "enable_passive_checks",
    "enable_perfdata",
    "event_command",
    "executions",
    "flapping",
    "flapping_current",
    "flapping_ignore_states",
    "flapping_last_change",
    "flapping_threshold",
    "flapping_threshold_high",
    "flapping_threshold_low",
    "force_next_check",
    "force_next_notification",
    "groups",
    "ha_mode",
    "handled",
    "icon_image",
    "icon_image_alt",
    "last_check",
    "last_check_result",
    "last_hard_state",
    "last_hard_state_change",
    "last_reachable",
    "last_state",
    "last_state_change",
    "last_state_type",
    "last_state_unreachable",
    "max_check_attempts",
    "name",
    "next_check",
    "next_update",
    "notes",
    "notes_url",
    "original_attributes",
    "package",
    "paused",
    "previous_state_change",
    "problem",
    "retry_interval",
    "severity",
    "source_location",
    "state",
    "state_type",
    "templates",
    "type",
    "vars",
    "version",
    "volatile",
    "zone",
];
const HOST_ONLY: &[&str] = &["address", "address6", "last_state_down", "last_state_up"];
const SERVICE_ONLY: &[&str] = &[
    "host_name",
    "last_state_critical",
    "last_state_ok",
    "last_state_unknown",
    "last_state_warning",
];
/// Fields that exist but are `no_user_view`.
const CHECKABLE_HIDDEN: &[&str] = &[
    "last_check_started",
    "state_raw",
    "last_state_raw",
    "last_hard_state_raw",
    "last_hard_states_raw",
    "last_soft_states_raw",
    "flapping_ignore_states_filter_real",
    "flapping_last_state",
    "flapping_buffer",
    "flapping_index",
    "suppressed_notifications",
    "state_before_suppression",
    "pending_executions",
    "start_called",
    "stop_called",
    "pause_called",
    "resume_called",
    "extensions",
    "state_loaded",
    "icingadb_identifier",
];
const CONFIG_OBJECT: &[&str] = &[
    "__name",
    "active",
    "ha_mode",
    "name",
    "original_attributes",
    "package",
    "paused",
    "source_location",
    "templates",
    "type",
    "version",
    "zone",
];
const COMMENT: &[&str] = &[
    "author",
    "entry_time",
    "entry_type",
    "expire_time",
    "host_name",
    "legacy_id",
    "persistent",
    "service_name",
    "text",
];
const COMMENT_HIDDEN: &[&str] = &["sticky", "removed_by", "remove_time"];
const DOWNTIME: &[&str] = &[
    "author",
    "authoritative_zone",
    "comment",
    "config_owner",
    "config_owner_hash",
    "duration",
    "end_time",
    "entry_time",
    "fixed",
    "host_name",
    "legacy_id",
    "parent",
    "remove_time",
    "scheduled_by",
    "service_name",
    "start_time",
    "trigger_time",
    "triggered_by",
    "triggers",
    "was_cancelled",
];
const GROUP: &[&str] = &[
    "action_url",
    "display_name",
    "groups",
    "notes",
    "notes_url",
    "vars",
];
const DEPENDENCY: &[&str] = &[
    "child_host_name",
    "child_service_name",
    "disable_checks",
    "disable_notifications",
    "ignore_soft_states",
    "parent_host_name",
    "parent_service_name",
    "period",
    "redundancy_group",
    "states",
    "vars",
];
const ENDPOINT: &[&str] = &[
    "bytes_received_per_second",
    "bytes_sent_per_second",
    "capabilities",
    "connected",
    "connecting",
    "host",
    "icinga_version",
    "last_message_received",
    "last_message_sent",
    "local_log_position",
    "log_duration",
    "messages_received_per_second",
    "messages_sent_per_second",
    "port",
    "remote_log_position",
    "syncing",
];
const ZONE: &[&str] = &["all_parents", "endpoints", "global", "parent"];
const USER: &[&str] = &[
    "display_name",
    "email",
    "enable_notifications",
    "groups",
    "last_notification",
    "pager",
    "period",
    "states",
    "types",
    "vars",
];
const COMMAND: &[&str] = &["arguments", "command", "env", "execute", "timeout", "vars"];

impl ObjKind {
    pub(crate) const ALL: [Self; 12] = [
        Self::Host,
        Self::Service,
        Self::HostGroup,
        Self::ServiceGroup,
        Self::Comment,
        Self::Downtime,
        Self::Dependency,
        Self::Endpoint,
        Self::Zone,
        Self::User,
        Self::CheckCommand,
        Self::EventCommand,
    ];

    pub(crate) fn type_name(self) -> &'static str {
        match self {
            Self::Host => "Host",
            Self::Service => "Service",
            Self::HostGroup => "HostGroup",
            Self::ServiceGroup => "ServiceGroup",
            Self::Comment => "Comment",
            Self::Downtime => "Downtime",
            Self::Dependency => "Dependency",
            Self::Endpoint => "Endpoint",
            Self::Zone => "Zone",
            Self::User => "User",
            Self::CheckCommand => "CheckCommand",
            Self::EventCommand => "EventCommand",
        }
    }

    pub(crate) fn plural(self) -> &'static str {
        match self {
            Self::Host => "hosts",
            Self::Service => "services",
            Self::HostGroup => "hostgroups",
            Self::ServiceGroup => "servicegroups",
            Self::Comment => "comments",
            Self::Downtime => "downtimes",
            Self::Dependency => "dependencies",
            Self::Endpoint => "endpoints",
            Self::Zone => "zones",
            Self::User => "users",
            Self::CheckCommand => "checkcommands",
            Self::EventCommand => "eventcommands",
        }
    }

    pub(crate) fn from_type_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.type_name() == name)
    }

    /// The variable name of the object in filters (`host`, `service`, ...).
    pub(crate) fn variable(self) -> String {
        self.type_name().to_lowercase()
    }

    /// Every attribute name, sorted.
    pub(crate) fn attr_names(self) -> Vec<&'static str> {
        let mut names: Vec<&'static str> = match self {
            Self::Host => CHECKABLE_COMMON.iter().chain(HOST_ONLY).copied().collect(),
            Self::Service => CHECKABLE_COMMON
                .iter()
                .chain(SERVICE_ONLY)
                .copied()
                .collect(),
            Self::Comment => CONFIG_OBJECT.iter().chain(COMMENT).copied().collect(),
            Self::Downtime => CONFIG_OBJECT.iter().chain(DOWNTIME).copied().collect(),
            Self::HostGroup | Self::ServiceGroup => {
                CONFIG_OBJECT.iter().chain(GROUP).copied().collect()
            }
            Self::Dependency => CONFIG_OBJECT.iter().chain(DEPENDENCY).copied().collect(),
            Self::Endpoint => CONFIG_OBJECT.iter().chain(ENDPOINT).copied().collect(),
            Self::Zone => CONFIG_OBJECT.iter().chain(ZONE).copied().collect(),
            Self::User => CONFIG_OBJECT.iter().chain(USER).copied().collect(),
            Self::CheckCommand | Self::EventCommand => {
                CONFIG_OBJECT.iter().chain(COMMAND).copied().collect()
            }
        };
        names.sort_unstable();
        names.dedup();
        names
    }

    fn hidden(self) -> &'static [&'static str] {
        match self {
            Self::Host | Self::Service => CHECKABLE_HIDDEN,
            Self::Comment => COMMENT_HIDDEN,
            Self::Downtime => &["removed_by"],
            _ => &[],
        }
    }

    /// Navigation fields: join name and target type.
    pub(crate) fn navigation(self) -> &'static [(&'static str, ObjKind)] {
        match self {
            Self::Host => &[
                ("check_command", Self::CheckCommand),
                ("event_command", Self::EventCommand),
                ("command_endpoint", Self::Endpoint),
            ],
            Self::Service => &[
                ("host", Self::Host),
                ("check_command", Self::CheckCommand),
                ("event_command", Self::EventCommand),
                ("command_endpoint", Self::Endpoint),
            ],
            Self::Comment | Self::Downtime => &[("host", Self::Host), ("service", Self::Service)],
            Self::Dependency => &[
                ("child_host", Self::Host),
                ("child_service", Self::Service),
                ("parent_host", Self::Host),
                ("parent_service", Self::Service),
            ],
            Self::Zone => &[("parent", Self::Zone)],
            _ => &[],
        }
    }

    /// Navigation names Icinga knows for which the mock has no target
    /// objects (time periods): valid joins that never produce output.
    pub(crate) fn empty_navigation(self) -> &'static [&'static str] {
        match self {
            Self::Host | Self::Service => &["check_period"],
            Self::Dependency | Self::User => &["period"],
            _ => &[],
        }
    }
}

/// Plural type names Icinga knows but the mock has no objects of: queries
/// succeed with no results.
pub(crate) const EMPTY_TYPES: &[(&str, &str)] = &[
    ("apilisteners", "ApiListener"),
    ("apiusers", "ApiUser"),
    ("checkercomponents", "CheckerComponent"),
    ("checkresultreaders", "CheckResultReader"),
    ("compatloggers", "CompatLogger"),
    ("elasticsearchwriters", "ElasticsearchWriter"),
    ("externalcommandlisteners", "ExternalCommandListener"),
    ("fileloggers", "FileLogger"),
    ("gelfwriters", "GelfWriter"),
    ("graphitewriters", "GraphiteWriter"),
    ("icingaapplications", "IcingaApplication"),
    ("icingadbs", "IcingaDB"),
    ("idomysqlconnections", "IdoMysqlConnection"),
    ("idopgsqlconnections", "IdoPgsqlConnection"),
    ("influxdbwriters", "InfluxdbWriter"),
    ("influxdb2writers", "Influxdb2Writer"),
    ("journaldloggers", "JournaldLogger"),
    ("livestatuslisteners", "LivestatusListener"),
    ("notifications", "Notification"),
    ("notificationcommands", "NotificationCommand"),
    ("notificationcomponents", "NotificationComponent"),
    ("opentsdbwriters", "OpenTsdbWriter"),
    ("otlpmetricswriters", "OTLPMetricsWriter"),
    ("perfdatawriters", "PerfdataWriter"),
    ("scheduleddowntimes", "ScheduledDowntime"),
    ("sysloggers", "SyslogLogger"),
    ("timeperiods", "TimePeriod"),
    ("usergroups", "UserGroup"),
    ("windowseventlogloggers", "WindowsEventLogLogger"),
];

fn string(value: &str) -> Json {
    Json::String(value.to_owned())
}

fn strings(values: &[String]) -> Json {
    Json::Array(values.iter().cloned().map(Json::String).collect())
}

fn vars(vars: Option<&Map<String, Json>>) -> Json {
    vars.map_or(Json::Null, |map| Json::Object(map.clone()))
}

fn common(
    meta: &super::ObjMeta,
    name: &str,
    full_name: &str,
    short_name: &str,
    type_name: &str,
) -> Option<Json> {
    Some(match name {
        "__name" => string(full_name),
        "name" => string(short_name),
        "type" => string(type_name),
        "active" => Json::Bool(true),
        "paused" => Json::Bool(false),
        "ha_mode" => int(0),
        "original_attributes" => Json::Null,
        "templates" => strings(&meta.templates),
        "package" => string(&meta.package),
        "zone" => string(&meta.zone),
        "source_location" => meta.source.to_json(),
        "version" => num(meta.version),
        _ => return None,
    })
}

impl World {
    /// Whether an object exists.
    pub(crate) fn exists(&self, kind: ObjKind, name: &str) -> bool {
        match kind {
            ObjKind::Host | ObjKind::Service => self
                .checkable(name)
                .is_some_and(|c| c.is_service() == (kind == ObjKind::Service)),
            ObjKind::HostGroup => self.host_groups.contains_key(name),
            ObjKind::ServiceGroup => self.service_groups.contains_key(name),
            ObjKind::Comment => self.comments.contains_key(name),
            ObjKind::Downtime => self.downtimes.contains_key(name),
            ObjKind::Dependency => self.dependencies.contains_key(name),
            ObjKind::Endpoint => self.endpoints.contains_key(name),
            ObjKind::Zone => self.zones.contains_key(name),
            ObjKind::User => self.users.contains_key(name),
            ObjKind::CheckCommand => self.check_commands.contains_key(name),
            ObjKind::EventCommand => self.event_commands.contains_key(name),
        }
    }

    /// Every object of a type, by full name in a stable order.
    pub(crate) fn object_names(&self, kind: ObjKind) -> Vec<String> {
        match kind {
            ObjKind::Host => self.hosts.keys().cloned().collect(),
            ObjKind::Service => self.all_services().map(Checkable::full_name).collect(),
            ObjKind::HostGroup => self.host_groups.keys().cloned().collect(),
            ObjKind::ServiceGroup => self.service_groups.keys().cloned().collect(),
            ObjKind::Comment => self.comments.keys().cloned().collect(),
            ObjKind::Downtime => self.downtimes.keys().cloned().collect(),
            ObjKind::Dependency => self.dependencies.keys().cloned().collect(),
            ObjKind::Endpoint => self.endpoints.keys().cloned().collect(),
            ObjKind::Zone => self.zones.keys().cloned().collect(),
            ObjKind::User => self.users.keys().cloned().collect(),
            ObjKind::CheckCommand => self.check_commands.keys().cloned().collect(),
            ObjKind::EventCommand => self.event_commands.keys().cloned().collect(),
        }
    }

    /// The object a navigation field points at, if it exists.
    pub(crate) fn navigate(&self, kind: ObjKind, name: &str, join: &str) -> Option<ObjRef> {
        let (_, target) = kind.navigation().iter().find(|(n, _)| *n == join)?;
        let target_name = match kind {
            ObjKind::Host | ObjKind::Service => {
                let checkable = self.checkable(name)?;
                match join {
                    "host" => checkable.host_name.clone(),
                    "check_command" => checkable.check_command.clone(),
                    "event_command" => checkable.event_command.clone(),
                    "command_endpoint" => checkable.command_endpoint.clone(),
                    _ => return None,
                }
            }
            ObjKind::Comment => {
                let comment = self.comments.get(name)?;
                match join {
                    "host" => comment.host_name.clone(),
                    _ => format!("{}!{}", comment.host_name, comment.service_name.as_ref()?),
                }
            }
            ObjKind::Downtime => {
                let downtime = self.downtimes.get(name)?;
                match join {
                    "host" => downtime.host_name.clone(),
                    _ => format!("{}!{}", downtime.host_name, downtime.service_name.as_ref()?),
                }
            }
            ObjKind::Dependency => {
                let dependency = self.dependencies.get(name)?;
                let key = if join.starts_with("child") {
                    &dependency.child
                } else {
                    &dependency.parent
                };
                match (join.ends_with("host"), key) {
                    (true, key) => key.host_name().to_string(),
                    (false, ic_model::ObjectKey::Service { key }) => key.full_name(),
                    (false, ic_model::ObjectKey::Host { .. }) => return None,
                }
            }
            ObjKind::Zone => self.zones.get(name)?.parent.clone(),
            _ => return None,
        };
        (!target_name.is_empty() && self.exists(*target, &target_name)).then_some(ObjRef {
            kind: *target,
            name: target_name,
        })
    }

    /// The attributes of an object: all of them, or the selected names.
    ///
    /// # Errors
    /// The first selected name the type doesn't have (or hides).
    pub(crate) fn object_attrs(
        &self,
        object: &ObjRef,
        selection: Option<&[String]>,
    ) -> Result<Map<String, Json>, String> {
        let mut map = Map::new();
        match selection {
            None => {
                for name in object.kind.attr_names() {
                    if let Ok(Some(value)) = self.attr(object, name) {
                        map.insert(name.to_owned(), value);
                    }
                }
            }
            Some(names) => {
                for name in names {
                    match self.attr(object, name) {
                        Ok(Some(value)) => {
                            map.insert(name.clone(), value);
                        }
                        // Hidden fields exist; Icinga silently skips them.
                        Err(ResolveError::Hidden(_)) => {}
                        Ok(None) | Err(ResolveError::UnknownField(_)) => {
                            return Err(format!("Invalid field specified: {name}"));
                        }
                    }
                }
            }
        }
        Ok(map)
    }

    /// One attribute. `Ok(None)` if the object vanished.
    ///
    /// # Errors
    /// Unknown or hidden field names.
    #[expect(
        clippy::too_many_lines,
        reason = "one arm per object type and attribute reads best"
    )]
    pub(crate) fn attr(&self, object: &ObjRef, name: &str) -> Result<Option<Json>, ResolveError> {
        if object.kind.hidden().contains(&name) {
            return Err(ResolveError::Hidden(name.to_owned()));
        }
        let value = match object.kind {
            ObjKind::Host | ObjKind::Service => self
                .checkable(&object.name)
                .map(|c| self.checkable_attr(c, name)),
            ObjKind::Comment => self.comments.get(&object.name).map(|c| {
                common(&c.meta, name, &c.name, &c.short_name, "Comment").or_else(|| {
                    let mut map = Map::new();
                    c.insert_own_attrs(&mut map);
                    map.remove(name)
                })
            }),
            ObjKind::Downtime => self.downtimes.get(&object.name).map(|d| {
                common(&d.meta, name, &d.name, &d.short_name, "Downtime").or_else(|| {
                    if name == "was_cancelled" {
                        return Some(Json::Bool(d.remove_time > 0.0));
                    }
                    let mut map = Map::new();
                    d.insert_own_attrs(&mut map);
                    map.remove(name)
                })
            }),
            ObjKind::HostGroup => self
                .host_groups
                .get(&object.name)
                .map(|g| group_attr(g, name, "HostGroup")),
            ObjKind::ServiceGroup => self
                .service_groups
                .get(&object.name)
                .map(|g| group_attr(g, name, "ServiceGroup")),
            ObjKind::Dependency => self.dependencies.get(&object.name).map(|d| {
                common(&d.meta, name, &d.name, &d.short_name, "Dependency").or_else(|| {
                    Some(match name {
                        "child_host_name" => string(d.child.host_name().as_str()),
                        "child_service_name" => {
                            string(d.child.as_service().map_or("", |k| &*k.name))
                        }
                        "parent_host_name" => string(d.parent.host_name().as_str()),
                        "parent_service_name" => {
                            string(d.parent.as_service().map_or("", |k| &*k.name))
                        }
                        "disable_checks" => Json::Bool(d.disable_checks),
                        "disable_notifications" => Json::Bool(d.disable_notifications),
                        "ignore_soft_states" => Json::Bool(d.ignore_soft_states),
                        "period" => string(&d.period),
                        "redundancy_group" => string(&d.redundancy_group),
                        "states" => d.states.as_deref().map_or(Json::Null, strings),
                        "vars" => Json::Null,
                        _ => return None,
                    })
                })
            }),
            ObjKind::Endpoint => self.endpoints.get(&object.name).map(|e| {
                common(&e.meta, name, &e.name, &e.name, "Endpoint").or_else(|| {
                    let now = self.now();
                    Some(match name {
                        "host" => string(&e.host),
                        "port" => string(&e.port),
                        "log_duration" => num(86_400.0),
                        "connected" => Json::Bool(e.connected),
                        "connecting" | "syncing" => Json::Bool(false),
                        "icinga_version" => int(i64::try_from(e.icinga_version).unwrap_or(0)),
                        "capabilities" => int(if e.connected { 3 } else { 0 }),
                        "local_log_position" | "remote_log_position" => {
                            num(if e.connected { now - 1.5 } else { 0.0 })
                        }
                        "last_message_sent" | "last_message_received" => {
                            num(if e.connected { now - 0.4 } else { 0.0 })
                        }
                        "messages_sent_per_second" | "messages_received_per_second" => {
                            num(if e.connected { 41.25 } else { 0.0 })
                        }
                        "bytes_sent_per_second" | "bytes_received_per_second" => {
                            num(if e.connected { 18_432.5 } else { 0.0 })
                        }
                        _ => return None,
                    })
                })
            }),
            ObjKind::Zone => self.zones.get(&object.name).map(|z| {
                common(&z.meta, name, &z.name, &z.name, "Zone").or_else(|| {
                    Some(match name {
                        "parent" => string(&z.parent),
                        "endpoints" => strings(&z.endpoints),
                        "global" => Json::Bool(z.global),
                        "all_parents" => strings(&self.zone_parents(&z.name)),
                        _ => return None,
                    })
                })
            }),
            ObjKind::User => self.users.get(&object.name).map(|u| {
                common(&u.meta, name, &u.name, &u.name, "User").or_else(|| {
                    Some(match name {
                        "display_name" => string(&u.display_name),
                        "email" => string(&u.email),
                        "pager" | "period" => string(""),
                        "groups" => strings(&u.groups),
                        "enable_notifications" => Json::Bool(true),
                        "last_notification" => num(0.0),
                        "states" | "types" | "vars" => Json::Null,
                        _ => return None,
                    })
                })
            }),
            ObjKind::CheckCommand => self
                .check_commands
                .get(&object.name)
                .map(|c| command_attr(c, name, "CheckCommand", "Internal#PluginCheck")),
            ObjKind::EventCommand => self
                .event_commands
                .get(&object.name)
                .map(|c| command_attr(c, name, "EventCommand", "Internal#PluginEvent")),
        };
        match value {
            None => Ok(None),
            Some(Some(value)) => Ok(Some(value)),
            Some(None) => Err(ResolveError::UnknownField(name.to_owned())),
        }
    }

    /// Zone names from the parent up (`all_parents`).
    fn zone_parents(&self, zone: &str) -> Vec<String> {
        let mut parents = Vec::new();
        let mut current = self.zones.get(zone).map(|z| z.parent.clone());
        while let Some(parent) = current.filter(|p| !p.is_empty()) {
            if parents.contains(&parent) || parents.len() > 32 {
                break;
            }
            current = self.zones.get(&parent).map(|z| z.parent.clone());
            parents.push(parent);
        }
        parents
    }

    fn checkable_attr(&self, c: &Checkable, name: &str) -> Option<Json> {
        let full = c.full_name();
        if let Some(value) = common(&c.meta, name, &full, c.short_name(), c.type_name()) {
            return Some(value);
        }
        Some(match name {
            "display_name" => string(if c.display_name.is_empty() {
                c.short_name()
            } else {
                &c.display_name
            }),
            "groups" => strings(&c.groups),
            "vars" => vars(c.vars.as_ref()),
            "check_command" => string(&c.check_command),
            "max_check_attempts" => int(c.max_check_attempts),
            "check_period" => string(&c.check_period),
            "check_timeout" => c.check_timeout.map_or(Json::Null, num),
            "check_interval" => num(c.check_interval),
            "retry_interval" => num(c.retry_interval),
            "event_command" => string(&c.event_command),
            "volatile" => Json::Bool(c.volatile),
            "enable_active_checks" => Json::Bool(c.enable_active_checks),
            "enable_passive_checks" => Json::Bool(c.enable_passive_checks),
            "enable_event_handler" => Json::Bool(c.enable_event_handler),
            "enable_notifications" => Json::Bool(c.enable_notifications),
            "enable_flapping" => Json::Bool(c.enable_flapping),
            "enable_perfdata" => Json::Bool(c.enable_perfdata),
            "flapping_ignore_states" => Json::Null,
            "flapping_threshold" => num(0.0),
            "flapping_threshold_low" => num(c.flapping_threshold_low),
            "flapping_threshold_high" => num(c.flapping_threshold_high),
            "notes" => string(&c.notes),
            "notes_url" => string(&c.notes_url),
            "action_url" => string(&c.action_url),
            "icon_image" => string(&c.icon_image),
            "icon_image_alt" => string(&c.icon_image_alt),
            "next_check" => num(c.next_check),
            "check_attempt" => int(c.check_attempt),
            "state_type" => int(c.state_type),
            "last_state_type" => int(c.last_state_type),
            "last_reachable" => Json::Bool(c.last_reachable),
            "last_check_result" => {
                c.cr.as_ref()
                    .map_or(Json::Null, super::types::CheckResultData::to_json)
            }
            "last_state_change" => num(c.last_state_change),
            "last_hard_state_change" => num(c.last_hard_state_change),
            "last_state_unreachable" => num(c.last_state_unreachable),
            "previous_state_change" => num(c.previous_state_change),
            "severity" => int(self.severity(c)),
            "problem" => Json::Bool(c.problem()),
            "handled" => Json::Bool(self.handled(c)),
            "next_update" => num(self.next_update(c)),
            "force_next_check" => Json::Bool(c.force_next_check),
            "acknowledgement" => int(c.acknowledgement),
            "acknowledgement_expiry" => num(c.acknowledgement_expiry),
            "acknowledgement_last_change" => num(c.acknowledgement_last_change),
            "force_next_notification" => Json::Bool(c.force_next_notification),
            "last_check" => num(c.last_check()),
            "downtime_depth" => int(self.downtime_depth(&full)),
            "flapping_current" => num(c.flapping_current),
            "flapping_last_change" => num(c.flapping_last_change),
            "flapping" => Json::Bool(c.flapping),
            "command_endpoint" => string(&c.command_endpoint),
            "executions" => c
                .executions
                .as_ref()
                .map_or(Json::Null, |e| Json::Object(e.clone())),
            "state" => int(c.state()),
            "last_state" => int(if c.is_service() {
                c.last_state_raw
            } else {
                super::types::host_state(c.last_state_raw)
            }),
            "last_hard_state" => int(if c.is_service() {
                c.last_hard_state_raw
            } else {
                super::types::host_state(c.last_hard_state_raw)
            }),
            // Host-only attributes.
            "address" if !c.is_service() => string(&c.address),
            "address6" if !c.is_service() => string(&c.address6),
            "last_state_up" if !c.is_service() => num(c.last_state_at[0]),
            "last_state_down" if !c.is_service() => num(c.last_state_at[1]),
            // Service-only attributes.
            "host_name" if c.is_service() => string(&c.host_name),
            "last_state_ok" if c.is_service() => num(c.last_state_at[0]),
            "last_state_warning" if c.is_service() => num(c.last_state_at[1]),
            "last_state_critical" if c.is_service() => num(c.last_state_at[2]),
            "last_state_unknown" if c.is_service() => num(c.last_state_at[3]),
            _ => return None,
        })
    }
}

fn group_attr(group: &GroupData, name: &str, type_name: &str) -> Option<Json> {
    common(&group.meta, name, &group.name, &group.name, type_name).or_else(|| {
        Some(match name {
            "display_name" => string(&group.display_name),
            "groups" => strings(&group.groups),
            "notes" => string(&group.notes),
            "notes_url" => string(&group.notes_url),
            "action_url" => string(&group.action_url),
            "vars" => vars(group.vars.as_ref()),
            _ => return None,
        })
    })
}

fn command_attr(
    command: &CommandData,
    name: &str,
    type_name: &str,
    function: &str,
) -> Option<Json> {
    common(&command.meta, name, &command.name, &command.name, type_name).or_else(|| {
        Some(match name {
            "command" => strings(&command.command),
            "arguments" => command
                .arguments
                .as_ref()
                .map_or(Json::Null, |a| Json::Object(a.clone())),
            "env" => Json::Null,
            "timeout" => num(command.timeout),
            "vars" => vars(command.vars.as_ref()),
            "execute" => {
                let mut map = Map::new();
                map.insert(
                    "arguments".into(),
                    strings(&[
                        "checkable".to_owned(),
                        "cr".to_owned(),
                        "resolvedMacros".to_owned(),
                        "useResolvedMacros".to_owned(),
                    ]),
                );
                map.insert("deprecated".into(), Json::Bool(false));
                map.insert("name".into(), string(function));
                map.insert("side_effect_free".into(), Json::Bool(false));
                map.insert("type".into(), string("Function"));
                Json::Object(map)
            }
            _ => return None,
        })
    })
}

/// The filter scope for one object: the object as `obj` and as its type's
/// variable (`host`, `service`, ...), its joined objects, and field access
/// on every object of the world.
pub(crate) struct ObjectScope<'w> {
    pub(crate) world: &'w World,
    pub(crate) object: ObjRef,
}

fn object_value(object: &ObjRef) -> filter::Value {
    filter::Value::Object(ObjectRef {
        type_name: object.kind.type_name(),
        name: Arc::from(object.name.as_str()),
    })
}

impl Scope for ObjectScope<'_> {
    fn variable(&self, name: &str) -> Option<filter::Value> {
        if name == "obj" || name == self.object.kind.variable() {
            return Some(object_value(&self.object));
        }
        if self
            .object
            .kind
            .navigation()
            .iter()
            .any(|(n, _)| *n == name)
            || self.object.kind.empty_navigation().contains(&name)
        {
            return Some(
                self.world
                    .navigate(self.object.kind, &self.object.name, name)
                    .map_or(filter::Value::Null, |target| object_value(&target)),
            );
        }
        None
    }

    fn field(&self, object: &ObjectRef, name: &str) -> Result<Option<filter::Value>, EvalError> {
        let Some(kind) = ObjKind::from_type_name(object.type_name) else {
            return Ok(None);
        };
        let reference = ObjRef {
            kind,
            name: object.name.to_string(),
        };
        // `service.host` is a navigation field that's also a real field.
        if kind == ObjKind::Service && name == "host" {
            return Ok(self
                .world
                .navigate(kind, &reference.name, "host")
                .map(|host| object_value(&host)));
        }
        match self.world.attr(&reference, name) {
            Ok(Some(value)) => Ok(Some(filter::Value::from_json(&value))),
            Ok(None) => Ok(Some(filter::Value::Null)),
            Err(ResolveError::UnknownField(_)) => Ok(None),
            Err(ResolveError::Hidden(field)) => Err(EvalError(format!(
                "Accessing the field '{field}' for type '{}' is not allowed in sandbox mode.",
                object.type_name
            ))),
        }
    }
}
