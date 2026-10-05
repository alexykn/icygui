//! Loading a [`Scenario`] into the runtime state.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use ic_model::{
    AckKind, CheckInfo, CommentKind, Host, HostState, ObjectKey, Service, ServiceState, StateType,
};
use serde_json::{Map, Value as Json};

use super::logic::plugin_command;
use super::types::{
    CheckResultData, Checkable, CommandData, CommentData, DependencyData, DowntimeData,
    EndpointData, GroupData, ObjMeta, SourceLocation, UserData, VarsState, ZoneData,
};
use super::{AppInfo, CheckStats, World};
use crate::config::NumberFormat;
use crate::error::MockError;
use crate::events::EventBus;
use crate::rng::Rng;
use crate::scenario::{Scenario, format_perfdata};
use crate::sim::SimState;

/// Settings that shape the world beyond the scenario.
#[derive(Clone, Debug)]
pub(crate) struct LoadOptions {
    pub(crate) number_format: NumberFormat,
    pub(crate) event_buffer: usize,
    pub(crate) seed: u64,
    pub(crate) reschedule_delay: f64,
    /// Checks per second the checker runs from its queue.
    pub(crate) check_rate: f64,
    /// The time the scenario's `time_base` is moved to.
    pub(crate) now: f64,
}

fn invalid(message: String) -> MockError {
    MockError::InvalidConfig(message)
}

/// Moves scenario timestamps (non-zero ones) by `delta`.
#[derive(Clone, Copy)]
struct Shift(f64);

impl Shift {
    fn at(self, t: ic_model::Timestamp) -> f64 {
        let t = t.as_unix_seconds();
        if t > 0.0 { t + self.0 } else { 0.0 }
    }

    fn opt(self, t: Option<ic_model::Timestamp>) -> f64 {
        t.map_or(0.0, |t| self.at(t))
    }
}

/// `r2.14.3-1` → 21403 (`icinga_version`).
fn version_number(version: &str) -> u64 {
    let digits: Vec<u64> = version
        .trim_start_matches(['r', 'v'])
        .split(['.', '-'])
        .take(3)
        .map(|part| part.parse().unwrap_or(0))
        .collect();
    match digits.as_slice() {
        [major, minor, patch] => major * 10_000 + minor * 100 + patch,
        _ => 0,
    }
}

impl World {
    /// Builds the runtime state from a scenario.
    ///
    /// # Errors
    /// Inconsistent scenarios: services without hosts, comments or
    /// downtimes on unknown objects, duplicate names, malformed names.
    #[expect(
        clippy::too_many_lines,
        reason = "one pass per object type keeps the loading order visible"
    )]
    pub(crate) fn load(scenario: &Scenario, options: &LoadOptions) -> Result<Self, MockError> {
        let now = options.now;
        let shift = Shift(now - scenario.time_base.as_unix_seconds());
        let status = &scenario.status;
        let program_start = if status.program_start.as_unix_seconds() > 0.0 {
            shift.at(status.program_start)
        } else {
            now - 2.0 * 86_400.0
        };
        let local_zone = scenario
            .endpoints
            .iter()
            .find(|e| e.name == status.node_name)
            .map_or_else(|| "master".to_owned(), |e| e.zone.clone());
        let app = AppInfo {
            node_name: status.node_name.clone(),
            zone_name: local_zone.clone(),
            version: status.version.clone(),
            program_start,
            pid: 1_729,
            environment: String::new(),
            enable_notifications: status.notifications_enabled,
            enable_event_handlers: status.event_handlers_enabled,
            enable_flapping: status.flap_detection_enabled,
            enable_host_checks: status.host_checks_enabled,
            enable_service_checks: status.service_checks_enabled,
            enable_perfdata: status.perfdata_enabled,
        };
        let mut world = Self {
            clock_offset: 0.0,
            app,
            hosts: BTreeMap::new(),
            services: BTreeMap::new(),
            host_groups: BTreeMap::new(),
            service_groups: BTreeMap::new(),
            comments: BTreeMap::new(),
            downtimes: BTreeMap::new(),
            dependencies: BTreeMap::new(),
            endpoints: BTreeMap::new(),
            zones: BTreeMap::new(),
            users: BTreeMap::new(),
            check_commands: BTreeMap::new(),
            event_commands: BTreeMap::new(),
            comments_by_object: BTreeMap::new(),
            downtimes_by_object: BTreeMap::new(),
            deps_by_child: BTreeMap::new(),
            deps_by_parent: BTreeMap::new(),
            next_comment_id: 1,
            next_downtime_id: 1,
            names: Rng::derive(options.seed, 0x006e_616d_6573),
            stats: CheckStats::default(),
            bus: EventBus::new(options.number_format, options.event_buffer),
            scheduled_checks: BTreeMap::new(),
            checks: super::CheckQueue::new(options.check_rate),
            pending_executions: Vec::new(),
            pinned: scenario.pinned.iter().map(ObjectKey::full_name).collect(),
            sim: SimState::default(),
            reschedule_delay: options.reschedule_delay,
        };

        let mut line = 1;
        for host in &scenario.hosts {
            let name = host.name.as_str();
            if name.is_empty() || name.contains('!') {
                return Err(invalid(format!("invalid host name '{name}'")));
            }
            let checkable = load_host(host, shift, &world.app, &local_zone, line);
            line += 8;
            if world.hosts.insert(name.to_owned(), checkable).is_some() {
                return Err(invalid(format!("duplicate host '{name}'")));
            }
        }
        line = 1;
        for service in &scenario.services {
            let host = service.key.host.as_str();
            let Some(zone) = world.hosts.get(host).map(|h| h.meta.zone.clone()) else {
                return Err(invalid(format!(
                    "service '{}' references unknown host '{host}'",
                    service.key
                )));
            };
            if service.key.name.is_empty() {
                return Err(invalid(format!("service on '{host}' without a name")));
            }
            let checkable = load_service(service, shift, &world.app, &zone, line);
            line += 6;
            let services = world.services.entry(host.to_owned()).or_default();
            if services
                .insert(service.key.name.to_string(), checkable)
                .is_some()
            {
                return Err(invalid(format!("duplicate service '{}'", service.key)));
            }
        }

        // Groups (explicit first, then the ones objects reference).
        for group in &scenario.host_groups {
            world.host_groups.insert(
                group.name.clone(),
                group_data(&group.name, &group.display_name, "hostgroups"),
            );
        }
        for group in &scenario.service_groups {
            world.service_groups.insert(
                group.name.clone(),
                group_data(&group.name, &group.display_name, "servicegroups"),
            );
        }
        let referenced: BTreeSet<String> = world
            .hosts
            .values()
            .flat_map(|h| h.groups.iter().cloned())
            .collect();
        for name in referenced {
            world
                .host_groups
                .entry(name.clone())
                .or_insert_with(|| group_data(&name, &name, "hostgroups"));
        }
        let referenced: BTreeSet<String> = world
            .all_services()
            .flat_map(|s| s.groups.iter().cloned())
            .collect();
        for name in referenced {
            world
                .service_groups
                .entry(name.clone())
                .or_insert_with(|| group_data(&name, &name, "servicegroups"));
        }

        // Zones and endpoints.
        for zone in &scenario.zones {
            world.zones.insert(
                zone.name.clone(),
                ZoneData {
                    name: zone.name.clone(),
                    parent: zone.parent.clone().unwrap_or_default(),
                    endpoints: Vec::new(),
                    global: zone.global,
                    meta: ObjMeta::config(
                        vec![zone.name.clone()],
                        "",
                        SourceLocation::file("/etc/icinga2/zones.conf", 1, 3),
                    ),
                },
            );
        }
        let icinga_version = version_number(&scenario.status.version);
        for endpoint in &scenario.endpoints {
            let zone = world
                .zones
                .entry(endpoint.zone.clone())
                .or_insert_with(|| ZoneData {
                    name: endpoint.zone.clone(),
                    parent: String::new(),
                    endpoints: Vec::new(),
                    global: false,
                    meta: ObjMeta::config(
                        vec![endpoint.zone.clone()],
                        "",
                        SourceLocation::file("/etc/icinga2/zones.conf", 1, 3),
                    ),
                });
            zone.endpoints.push(endpoint.name.clone());
            let local = endpoint.name == world.app.node_name;
            world.endpoints.insert(
                endpoint.name.clone(),
                EndpointData {
                    name: endpoint.name.clone(),
                    member_of: endpoint.zone.clone(),
                    host: if local {
                        String::new()
                    } else {
                        endpoint.name.clone()
                    },
                    port: "5665".to_owned(),
                    connected: endpoint.connected,
                    icinga_version: if endpoint.connected || local {
                        icinga_version
                    } else {
                        0
                    },
                    meta: ObjMeta::config(
                        vec![endpoint.name.clone()],
                        "",
                        SourceLocation::file("/etc/icinga2/zones.conf", 1, 3),
                    ),
                },
            );
        }

        for user in &scenario.users {
            world.users.insert(
                user.name.clone(),
                UserData {
                    name: user.name.clone(),
                    display_name: user.display_name.clone(),
                    email: user.email.clone(),
                    groups: user.groups.clone(),
                    meta: ObjMeta::config(
                        vec![user.name.clone(), "generic-user".to_owned()],
                        &local_zone,
                        SourceLocation::file("/etc/icinga2/conf.d/users.conf", 1, 6),
                    ),
                },
            );
        }

        // Commands referenced by checkables.
        let check_commands: BTreeSet<String> = world
            .all_checkables()
            .map(|c| c.check_command.clone())
            .filter(|c| !c.is_empty())
            .collect();
        for name in check_commands {
            world
                .check_commands
                .insert(name.clone(), command_data(&name, true));
        }
        let event_commands: BTreeSet<String> = world
            .all_checkables()
            .map(|c| c.event_command.clone())
            .filter(|c| !c.is_empty())
            .chain(["restart-service".to_owned()])
            .collect();
        for name in event_commands {
            world
                .event_commands
                .insert(name.clone(), command_data(&name, false));
        }

        // Dependencies.
        for dependency in &scenario.dependencies {
            for key in [&dependency.child, &dependency.parent] {
                if world.checkable_by_key(key).is_none() {
                    return Err(invalid(format!(
                        "dependency '{}' references unknown object '{key}'",
                        dependency.name
                    )));
                }
            }
            let child = dependency.child.full_name();
            let short = dependency
                .name
                .strip_prefix(&format!("{child}!"))
                .unwrap_or(&dependency.name)
                .to_owned();
            let name = format!("{child}!{short}");
            let zone = world
                .checkable(&child)
                .map(|c| c.meta.zone.clone())
                .unwrap_or_default();
            let data = DependencyData {
                name: name.clone(),
                short_name: short.clone(),
                child: dependency.child.clone(),
                parent: dependency.parent.clone(),
                redundancy_group: String::new(),
                disable_checks: false,
                disable_notifications: true,
                ignore_soft_states: true,
                period: String::new(),
                states: None,
                meta: ObjMeta::config(
                    vec![short, "generic-dependency".to_owned()],
                    &zone,
                    SourceLocation::file("/etc/icinga2/conf.d/dependencies.conf", 1, 6),
                ),
            };
            world.index_dependency(&data);
            if world.dependencies.insert(name.clone(), data).is_some() {
                return Err(invalid(format!("duplicate dependency '{name}'")));
            }
        }

        // Comments.
        for comment in &scenario.comments {
            let object = comment.object.full_name();
            let Some(checkable) = world.checkable(&object) else {
                return Err(invalid(format!(
                    "comment '{}' references unknown object '{object}'",
                    comment.name
                )));
            };
            let zone = checkable.meta.zone.clone();
            let Some(short) = comment.name.strip_prefix(&format!("{object}!")) else {
                return Err(invalid(format!(
                    "comment name '{}' must start with '{object}!'",
                    comment.name
                )));
            };
            let entry_time = shift.at(comment.entry_time);
            let legacy_id = world.next_comment_legacy_id();
            let data = CommentData {
                name: comment.name.clone(),
                short_name: short.to_owned(),
                host_name: comment.object.host_name().to_string(),
                service_name: comment.object.as_service().map(|k| k.name.to_string()),
                author: comment.author.clone(),
                text: comment.text.clone(),
                entry_time,
                entry_type: match comment.kind {
                    CommentKind::User => 1,
                    CommentKind::Downtime => 2,
                    CommentKind::Flapping => 3,
                    CommentKind::Acknowledgement => 4,
                },
                expire_time: shift.opt(comment.expire_time),
                persistent: comment.persistent,
                sticky: false,
                legacy_id,
                meta: super::logic::runtime_meta(
                    "Comment",
                    short,
                    &zone,
                    &comment.name,
                    entry_time,
                ),
            };
            world.index_comment(&data);
            if world.comments.insert(comment.name.clone(), data).is_some() {
                return Err(invalid(format!("duplicate comment '{}'", comment.name)));
            }
        }
        // Acknowledgement comments mirror the sticky flag.
        let sticky: HashSet<String> = world
            .all_checkables()
            .filter(|c| c.acknowledgement == 2)
            .map(Checkable::full_name)
            .collect();
        for comment in world.comments.values_mut() {
            if comment.entry_type == 4 && sticky.contains(&comment.object().full_name()) {
                comment.sticky = true;
            }
        }

        // Downtimes.
        for downtime in &scenario.downtimes {
            let object = downtime.object.full_name();
            let Some(checkable) = world.checkable(&object) else {
                return Err(invalid(format!(
                    "downtime '{}' references unknown object '{object}'",
                    downtime.name
                )));
            };
            let zone = checkable.meta.zone.clone();
            let Some(short) = downtime.name.strip_prefix(&format!("{object}!")) else {
                return Err(invalid(format!(
                    "downtime name '{}' must start with '{object}!'",
                    downtime.name
                )));
            };
            let entry_time = shift.at(downtime.entry_time);
            let legacy_id = world.next_downtime_legacy_id();
            let config_owner = if downtime.config_owned {
                format!("{object}!maintenance-window")
            } else {
                String::new()
            };
            let data = DowntimeData {
                name: downtime.name.clone(),
                short_name: short.to_owned(),
                host_name: downtime.object.host_name().to_string(),
                service_name: downtime.object.as_service().map(|k| k.name.to_string()),
                author: downtime.author.clone(),
                comment: downtime.comment.clone(),
                start_time: shift.at(downtime.start_time),
                end_time: shift.at(downtime.end_time),
                entry_time,
                trigger_time: shift.opt(downtime.trigger_time),
                fixed: downtime.fixed,
                duration: downtime.duration,
                triggered_by: downtime.triggered_by.clone().unwrap_or_default(),
                scheduled_by: if downtime.config_owned {
                    config_owner.clone()
                } else {
                    String::new()
                },
                parent: downtime.parent.clone().unwrap_or_default(),
                triggers: Vec::new(),
                legacy_id,
                remove_time: 0.0,
                authoritative_zone: if downtime.config_owned {
                    zone.clone()
                } else {
                    String::new()
                },
                config_owner_hash: if downtime.config_owned {
                    format!("{:016x}", legacy_id.wrapping_mul(0x9E37_79B9_7F4A_7C15))
                } else {
                    String::new()
                },
                config_owner,
                meta: super::logic::runtime_meta(
                    "Downtime",
                    short,
                    &zone,
                    &downtime.name,
                    entry_time,
                ),
            };
            world.index_downtime(&data);
            if world
                .downtimes
                .insert(downtime.name.clone(), data)
                .is_some()
            {
                return Err(invalid(format!("duplicate downtime '{}'", downtime.name)));
            }
        }
        let triggers: Vec<(String, String)> = world
            .downtimes
            .values()
            .filter(|d| !d.triggered_by.is_empty())
            .map(|d| (d.triggered_by.clone(), d.name.clone()))
            .collect();
        for (parent, child) in triggers {
            if let Some(parent) = world.downtimes.get_mut(&parent) {
                parent.triggers.push(child);
            }
        }
        world.resolve_command_macros();
        Ok(world)
    }

    /// Check results carry the executed (macro-resolved) command line.
    fn resolve_command_macros(&mut self) {
        let addresses: BTreeMap<String, String> = self
            .hosts
            .iter()
            .map(|(name, host)| (name.clone(), host.address.clone()))
            .collect();
        let hosts = self.hosts.values_mut();
        let services = self.services.values_mut().flat_map(BTreeMap::values_mut);
        for checkable in hosts.chain(services) {
            let address = addresses
                .get(&checkable.host_name)
                .map_or("", String::as_str);
            if let Some(cr) = checkable.cr.as_mut() {
                resolve_address(&mut cr.command, address);
            }
        }
    }
}

/// Replaces `$address$` in a command line (array of arguments).
pub(crate) fn resolve_address(command: &mut Json, address: &str) {
    if let Json::Array(arguments) = command {
        for argument in arguments {
            if let Json::String(text) = argument
                && text.contains("$address$")
            {
                *text = text.replace("$address$", address);
            }
        }
    }
}

fn group_data(name: &str, display_name: &str, directory: &str) -> GroupData {
    GroupData {
        name: name.to_owned(),
        display_name: if display_name.is_empty() {
            name.to_owned()
        } else {
            display_name.to_owned()
        },
        groups: Vec::new(),
        notes: String::new(),
        notes_url: String::new(),
        action_url: String::new(),
        vars: None,
        meta: ObjMeta::config(
            vec![name.to_owned()],
            "",
            SourceLocation::file(&format!("/etc/icinga2/conf.d/{directory}.conf"), 1, 3),
        ),
    }
}

/// A `CheckCommand` (`check = true`) or `EventCommand` as the ITL defines
/// it: internal ones (`dummy`, `icinga`, ...) have no command line.
fn command_data(name: &str, check: bool) -> CommandData {
    let internal = check
        .then(|| super::logic::internal_check_function(name))
        .flatten();
    if let Some(function) = internal {
        let vars = match name {
            "dummy" => Some(json_vars(&[
                ("dummy_state", Json::from(0)),
                ("dummy_text", Json::from("Check was successful.")),
            ])),
            "passive" => Some(json_vars(&[
                ("dummy_state", Json::from(3)),
                (
                    "dummy_text",
                    Json::from("No Passive Check Result Received."),
                ),
            ])),
            "icinga" => Some(json_vars(&[("icinga_min_version", Json::from(""))])),
            _ => None,
        };
        // `passive` imports `dummy`; the others their own template.
        let base = if name == "passive" { "dummy" } else { name };
        return CommandData {
            name: name.to_owned(),
            command: None,
            arguments: None,
            timeout: 60.0,
            vars,
            execute: function,
            meta: ObjMeta::config(
                vec![
                    name.to_owned(),
                    "plugin-check-command".to_owned(),
                    format!("{base}-check-command"),
                ],
                "",
                SourceLocation::file("/usr/share/icinga2/include/command-icinga.conf", 17, 0),
            ),
        };
    }
    let command = match plugin_command(name) {
        Json::Array(items) => items
            .into_iter()
            .filter_map(|item| item.as_str().map(str::to_owned))
            .take(1)
            .collect(),
        _ => vec![format!("/usr/lib/nagios/plugins/{name}")],
    };
    let mut host_argument = Map::new();
    host_argument.insert("description".into(), Json::from("host name or address"));
    host_argument.insert("value".into(), Json::from("$address$"));
    let mut arguments = Map::new();
    arguments.insert("-H".into(), Json::Object(host_argument));
    CommandData {
        name: name.to_owned(),
        command: Some(command),
        arguments: Some(arguments),
        timeout: 60.0,
        vars: None,
        execute: if check {
            "Internal#PluginCheck"
        } else {
            "Internal#PluginEvent"
        },
        meta: ObjMeta::config(
            vec![
                name.to_owned(),
                if check {
                    "plugin-check-command"
                } else {
                    "plugin-event-command"
                }
                .to_owned(),
            ],
            "",
            SourceLocation::file("/usr/share/icinga2/include/command-plugins.conf", 40, 30),
        ),
    }
}

fn json_vars(pairs: &[(&str, Json)]) -> Map<String, Json> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), value.clone()))
        .collect()
}

/// The parts of a checkable common to hosts and services.
struct CheckableSeed<'a> {
    host_name: String,
    service_name: Option<String>,
    display_name: String,
    groups: Vec<String>,
    vars: Option<Map<String, Json>>,
    links: &'a ic_model::Links,
    check: &'a CheckInfo,
    /// Raw state; `None` while pending.
    state_raw: Option<u8>,
    exit_status: i64,
    templates: Vec<String>,
    source: SourceLocation,
}

fn load_host(host: &Host, shift: Shift, app: &AppInfo, local_zone: &str, line: u32) -> Checkable {
    let state_raw = match host.state {
        HostState::Up => Some(0),
        HostState::Down | HostState::Unreachable => Some(2),
        HostState::Pending => None,
    };
    let mut checkable = build_checkable(
        CheckableSeed {
            host_name: host.name.to_string(),
            service_name: None,
            display_name: host.display_name.clone(),
            groups: host.groups.clone(),
            vars: (!host.vars.is_empty()).then(|| host.vars.clone()),
            links: &host.links,
            check: &host.check,
            state_raw,
            exit_status: host
                .check
                .result
                .as_ref()
                .map_or(i64::from(state_raw.unwrap_or(0)), |r| {
                    i64::from(r.exit_status)
                }),
            templates: vec![host.name.to_string(), "generic-host".to_owned()],
            source: SourceLocation::file("/etc/icinga2/conf.d/hosts.conf", line, 7),
        },
        shift,
        app,
        local_zone,
    );
    checkable.address.clone_from(&host.address);
    checkable.address6.clone_from(&host.address6);
    if host.state == HostState::Unreachable {
        checkable.last_reachable = false;
    }
    checkable
}

fn load_service(
    service: &Service,
    shift: Shift,
    app: &AppInfo,
    zone: &str,
    line: u32,
) -> Checkable {
    let state_raw = match service.state {
        ServiceState::Ok => Some(0),
        ServiceState::Warning => Some(1),
        ServiceState::Critical => Some(2),
        ServiceState::Unknown => Some(3),
        ServiceState::Pending => None,
    };
    build_checkable(
        CheckableSeed {
            host_name: service.key.host.to_string(),
            service_name: Some(service.key.name.to_string()),
            display_name: service.display_name.clone(),
            groups: service.groups.clone(),
            vars: (!service.vars.is_empty()).then(|| service.vars.clone()),
            links: &service.links,
            check: &service.check,
            state_raw,
            exit_status: service
                .check
                .result
                .as_ref()
                .map_or(i64::from(state_raw.unwrap_or(0)), |r| {
                    i64::from(r.exit_status)
                }),
            templates: vec![service.key.name.to_string(), "generic-service".to_owned()],
            source: SourceLocation::file("/etc/icinga2/conf.d/services.conf", line, 5),
        },
        shift,
        app,
        zone,
    )
}

#[expect(
    clippy::too_many_lines,
    reason = "derives every Icinga runtime attribute from the scenario"
)]
fn build_checkable(seed: CheckableSeed<'_>, shift: Shift, app: &AppInfo, zone: &str) -> Checkable {
    let check = seed.check;
    let is_service = seed.service_name.is_some();
    let max_attempts = check.max_attempts.max(1);
    let pending = seed.state_raw.is_none();
    let state_raw = seed.state_raw.unwrap_or(3);
    let state_ok = if is_service {
        state_raw == 0
    } else {
        state_raw <= 1
    };
    let state_type: u8 = u8::from(!(pending || check.state_type == StateType::Soft));
    let attempt = if state_type == 1 || state_ok {
        1
    } else {
        check
            .attempt
            .clamp(1, max_attempts.saturating_sub(1).max(1))
    };
    let reachable = check.reachable;
    let last_state_change = if check.last_state_change.as_unix_seconds() > 0.0 {
        shift.at(check.last_state_change)
    } else if pending {
        // Without a check result Icinga never changed the state: 0, like
        // `last_hard_state_change` and `previous_state_change`.
        0.0
    } else {
        app.program_start
    };
    let last_hard_state_change = if check.last_hard_state_change.as_unix_seconds() > 0.0 {
        shift.at(check.last_hard_state_change)
    } else {
        last_state_change
    };
    let interval = check.check_interval.max(1.0);
    let cr = if pending {
        None
    } else {
        let result = check.result.clone().unwrap_or_default();
        let execution_end = match result.execution_end.non_zero() {
            Some(t) => shift.at(t),
            None => shift.opt(check.last_check).max(last_state_change),
        };
        let execution_end = if execution_end > 0.0 {
            execution_end
        } else {
            last_state_change
        };
        let execution_start = match result.execution_start.non_zero() {
            Some(t) => shift.at(t),
            None => execution_end - super::logic::execution_time(&check.check_command),
        };
        let schedule_start = match result.schedule_start.non_zero() {
            Some(t) => shift.at(t),
            None => execution_start - 0.001,
        };
        let mut output = result.output.clone();
        if !result.long_output.is_empty() {
            output.push('\n');
            output.push_str(&result.long_output);
        }
        let vars = VarsState {
            attempt,
            reachable,
            state: state_raw,
            state_type,
        };
        Some(CheckResultData {
            schedule_start,
            schedule_end: execution_end,
            execution_start,
            execution_end,
            command: if result.active || check.result.is_none() {
                plugin_command(&check.check_command)
            } else {
                Json::Null
            },
            exit_status: seed.exit_status,
            state: state_raw,
            previous_hard_state: if state_type == 1 && !state_ok { 0 } else { 99 },
            output,
            performance_data: Some(result.perfdata.iter().map(format_perfdata).collect()),
            active: result.active || check.result.is_none(),
            check_source: if result.check_source.is_empty() {
                check
                    .command_endpoint
                    .clone()
                    .unwrap_or_else(|| app.node_name.clone())
            } else {
                result.check_source.clone()
            },
            scheduling_source: app.node_name.clone(),
            ttl: 0.0,
            vars_before: Some(vars),
            vars_after: Some(vars),
        })
    };
    let last_check = cr.as_ref().map_or(0.0, |cr| cr.execution_end);
    let next_check = match check.next_check.and_then(ic_model::Timestamp::non_zero) {
        Some(t) => shift.at(t),
        None if last_check > 0.0 => {
            last_check
                + if state_type == 0 && !pending {
                    check.retry_interval
                } else {
                    interval
                }
        }
        None => app.program_start + interval,
    };
    let mut last_state_at = [0.0; 4];
    if let Some(cr) = &cr {
        if is_service {
            last_state_at[usize::from(state_raw.min(3))] = cr.execution_end;
            if !state_ok {
                last_state_at[0] = last_state_change;
            }
        } else if state_ok {
            last_state_at[0] = cr.execution_end;
        } else {
            last_state_at[1] = cr.execution_end;
            last_state_at[0] = last_state_change;
        }
    }
    let hard_raw = if state_type == 1 { state_raw } else { 0 };
    Checkable {
        host_name: seed.host_name,
        service_name: seed.service_name,
        display_name: seed.display_name,
        address: String::new(),
        address6: String::new(),
        groups: seed.groups,
        vars: seed.vars,
        meta: ObjMeta::config(
            seed.templates,
            check.zone.as_deref().unwrap_or(zone),
            seed.source,
        ),
        check_command: if check.check_command.is_empty() {
            if is_service { "dummy" } else { "hostalive" }.to_owned()
        } else {
            check.check_command.clone()
        },
        max_check_attempts: max_attempts,
        check_period: String::new(),
        check_timeout: None,
        check_interval: interval,
        retry_interval: check.retry_interval.max(1.0),
        event_command: String::new(),
        volatile: false,
        enable_active_checks: check.features.active_checks,
        enable_passive_checks: check.features.passive_checks,
        enable_event_handler: check.features.event_handler,
        enable_notifications: check.features.notifications,
        enable_flapping: check.features.flap_detection,
        enable_perfdata: check.features.perfdata,
        flapping_threshold_low: 25.0,
        flapping_threshold_high: 30.0,
        notes: seed.links.notes.clone(),
        notes_url: seed.links.notes_url.clone(),
        action_url: seed.links.action_url.clone(),
        icon_image: seed.links.icon_image.clone(),
        icon_image_alt: String::new(),
        command_endpoint: check.command_endpoint.clone().unwrap_or_default(),
        state_raw,
        last_state_raw: state_raw,
        last_hard_state_raw: if pending { 3 } else { hard_raw },
        last_hard_states_raw: if pending {
            9_999
        } else if state_type == 1 && !state_ok {
            u16::from(state_raw) * 100
        } else {
            99 + u16::from(hard_raw) * 100
        },
        state_type,
        last_state_type: state_type,
        check_attempt: attempt,
        last_reachable: reachable,
        cr,
        next_check,
        last_state_change,
        last_hard_state_change,
        previous_state_change: last_state_change,
        last_state_unreachable: if reachable { 0.0 } else { last_check },
        last_state_at,
        force_next_check: false,
        force_next_notification: false,
        acknowledgement: match check.acknowledgement {
            AckKind::None => 0,
            AckKind::Normal => 1,
            AckKind::Sticky => 2,
        },
        acknowledgement_expiry: shift.opt(check.acknowledgement_expiry),
        acknowledgement_last_change: if check.acknowledgement == AckKind::None {
            0.0
        } else {
            last_state_change
        },
        flapping: check.flapping,
        flapping_current: check.flapping_current,
        flapping_last_change: if check.flapping {
            last_state_change
        } else {
            0.0
        },
        flapping_buffer: 0,
        flapping_index: 0,
        flapping_last_state: state_raw,
        executions: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_versions() {
        assert_eq!(version_number("r2.14.3-1"), 21_403);
        assert_eq!(version_number("v2.15.0"), 21_500);
        assert_eq!(version_number("garbage"), 0);
    }
}
