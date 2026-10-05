//! Variable resolution: the [`Scope`] trait and the scopes for hosts,
//! services and API `filter_vars`.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use ic_model::{
    AckKind, CheckInfo, CheckResult, Host, HostState, Links, Service, ServiceState, StateType, Vars,
};

use crate::eval::{Field, lookup_field};
use crate::value::Value;

/// Resolves variables for a filter.
///
/// `path` is a variable name followed by the member names the filter spells
/// out as constants: `host.vars.role` and `host.vars["role"]` are
/// `["host", "vars", "role"]`, `host.groups[0]` is `["host", "groups", "0"]`.
///
/// The evaluator asks for the whole path first and then for shorter
/// prefixes, and indexes whatever is left itself. So an implementation
/// returns:
///
/// - `Some(value)` only when `value` is the value of the **entire** path. A
///   missing key below a variable it provides is `Some(Value::Null)`.
/// - `None` when it doesn't provide the variable `path[0]`, or can't (or
///   doesn't want to) follow the path to its end, for example through a
///   string or a list. The evaluator then retries with a shorter prefix and
///   applies Icinga's rules to the rest, including its errors.
///
/// Never return the value of a shorter prefix for a longer path: the
/// evaluator would take it as the value of the whole path.
///
/// Resolving deep paths directly is an optimisation: `host.vars.role` is
/// then read straight from the host's custom variables instead of
/// converting all of them first. The simplest correct scope resolves only
/// single-element paths:
///
/// ```
/// use ic_filter::{Filter, Scope, Value};
///
/// struct Site;
///
/// impl Scope for Site {
///     fn lookup(&self, path: &[&str]) -> Option<Value> {
///         match path {
///             ["site"] => Some(Value::from("berlin")),
///             _ => None,
///         }
///     }
/// }
///
/// let filter = Filter::parse(r#"site == "berlin" && site.len() == 6"#).unwrap();
/// assert!(filter.matches(&Site));
/// ```
pub trait Scope {
    /// Resolves `path`. See the trait documentation.
    fn lookup(&self, path: &[&str]) -> Option<Value>;
}

impl<T: Scope + ?Sized> Scope for &T {
    fn lookup(&self, path: &[&str]) -> Option<Value> {
        (**self).lookup(path)
    }
}

/// Exposes a host as `host` (and `obj`, as Icinga does), with Icinga's
/// attribute names and types.
///
/// Attributes: `name`, `display_name`, `__name`, `type`, `address`,
/// `address6`, `state` (0 up, 1 down or unreachable, and 1 while pending,
/// as in Icinga), `state_type`, `last_state_change`,
/// `last_hard_state_change`, `last_check` (-1 if never), `next_check`,
/// `check_attempt`, `max_check_attempts`, `acknowledgement` (0, 1 normal,
/// 2 sticky), `acknowledgement_expiry`, `downtime_depth`, `flapping`,
/// `flapping_current`, `last_reachable`, `problem` (false while pending),
/// `handled`, `severity`, `check_command`, `check_interval`,
/// `retry_interval`, `command_endpoint`, `zone`, `enable_active_checks`,
/// `enable_passive_checks`, `enable_notifications`, `enable_event_handler`,
/// `enable_flapping`, `enable_perfdata`, `groups`, `vars` (`null` when the
/// host has no custom variables, as in Icinga), `notes`, `notes_url`,
/// `action_url`, `icon_image` and `last_check_result` (`null` without a
/// check result, otherwise a dictionary with `output`, `exit_status`,
/// `state`, `execution_start`, `execution_end`, `schedule_start`,
/// `check_source`, `active`, `type`). Unknown attributes are `null`.
///
/// The model doesn't keep a check result's own state, so for hosts
/// `last_check_result.state` is derived: up hosts give 1 for exit status 1
/// and 0 otherwise; down and unreachable hosts give 3 for exit statuses
/// outside 0–2 (Icinga maps them to UNKNOWN) and for active results with
/// exit status 0 or 1 (Icinga sets UNKNOWN without an exit status, for
/// example when a command endpoint is not connected), and 2 otherwise.
///
/// The scope reads what the [`Host`] holds. Attributes the caller didn't
/// load (an object fetched without its check result, say) read as the
/// model's defaults: `null` for `last_check_result`, `""`, `0` or the
/// default switches for the others. Filters on such attributes need fully
/// loaded objects.
#[derive(Clone, Copy, Debug)]
pub struct HostScope<'a> {
    /// The host.
    pub host: &'a Host,
}

impl Scope for HostScope<'_> {
    fn lookup(&self, path: &[&str]) -> Option<Value> {
        match path.split_first() {
            Some((&("host" | "obj"), rest)) => host_path(self.host, rest),
            _ => None,
        }
    }
}

/// Exposes a service as `service` (and `obj`) and its host as `host`, with
/// Icinga's attribute names and types.
///
/// Services have the attributes listed for [`HostScope`] except `address`
/// and `address6`, plus `host_name` and `host` (the host's attributes).
/// `name` is the short name and `__name` is `host!service`. `state` is 0 ok,
/// 1 warning, 2 critical, 3 unknown, and 3 while pending, as in Icinga
/// (`problem` is false while pending). `last_check_result.state` is the
/// service's state.
///
/// `handled` follows Icinga's `Service::GetHandled()`: a problem that is
/// acknowledged or in downtime, or *any* service whose host has a problem,
/// including an OK one. That deliberately differs from
/// [`ic_model::Service::is_handled`], which also requires the service
/// itself to be a problem.
///
/// Without a host, `host.name` and `host.__name` still resolve (from the
/// service's key) and other host attributes are `null`.
#[derive(Clone, Copy, Debug)]
pub struct ServiceScope<'a> {
    /// The service.
    pub service: &'a Service,
    /// The service's host, if known.
    pub host: Option<&'a Host>,
}

impl Scope for ServiceScope<'_> {
    fn lookup(&self, path: &[&str]) -> Option<Value> {
        match path.split_first() {
            Some((&("service" | "obj"), rest)) => self.service_path(rest),
            Some((&"host", rest)) => self.host_path(rest),
            _ => None,
        }
    }
}

impl ServiceScope<'_> {
    fn service_path(&self, rest: &[&str]) -> Option<Value> {
        let Some((attribute, deeper)) = rest.split_first() else {
            return Some(self.service_dict());
        };
        if *attribute == "host" {
            return self.host_path(deeper);
        }
        let service = self.service;
        let check = &service.check;
        match *attribute {
            "vars" => return json_path(&service.vars, deeper),
            "groups" => return strings_path(&service.groups, deeper),
            "last_check_result" => {
                return check_result_path(check, &CheckState::Service(service.state), deeper);
            }
            _ => {}
        }
        let value = self.service_attribute(attribute).unwrap_or_default();
        scalar_path(value, deeper)
    }

    fn host_path(&self, rest: &[&str]) -> Option<Value> {
        if let Some(host) = self.host {
            return host_path(host, rest);
        }
        let name = self.service.key.host.as_str();
        let value = match rest.split_first() {
            None => Value::from(BTreeMap::from([
                ("__name".to_owned(), Value::str(name)),
                ("name".to_owned(), Value::str(name)),
            ])),
            Some((&("name" | "__name"), _)) => Value::str(name),
            Some(_) => Value::Null,
        };
        scalar_path(value, rest.get(1..).unwrap_or_default())
    }

    fn handled(&self) -> bool {
        let service = self.service;
        (service.is_problem() && service.check.is_acknowledged_or_in_downtime())
            || self.host.is_some_and(Host::is_problem)
    }

    fn service_attribute(&self, name: &str) -> Option<Value> {
        let service = self.service;
        Some(match name {
            "name" => Value::String(Arc::clone(&service.key.name)),
            "display_name" => Value::str(&service.display_name),
            "__name" => Value::from(service.key.full_name()),
            "host_name" => Value::str(service.key.host.as_str()),
            "type" => Value::str("Service"),
            "state" => Value::Number(service_state_code(service.state)),
            "problem" => Value::Bool(service.is_problem()),
            "handled" => Value::Bool(self.handled()),
            "severity" => Value::from(service.severity()),
            "vars" => vars_value(&service.vars),
            "groups" => strings_value(&service.groups),
            "last_check_result" => {
                check_result_value(&service.check, &CheckState::Service(service.state))
            }
            other => {
                return check_attribute(&service.check, other)
                    .or_else(|| links_attribute(&service.links, other));
            }
        })
    }

    fn service_dict(&self) -> Value {
        let mut entries = BTreeMap::new();
        for name in SERVICE_ATTRIBUTES
            .iter()
            .chain(CHECK_ATTRIBUTES.iter())
            .chain(LINK_ATTRIBUTES.iter())
        {
            if let Some(value) = self.service_attribute(name) {
                entries.insert((*name).to_owned(), value);
            }
        }
        let host = self.host_path(&[]).unwrap_or_default();
        entries.insert("host".to_owned(), host);
        Value::from(entries)
    }
}

/// `VarsScope` exposes API `filter_vars` (or any named values) as variables.
#[derive(Clone, Copy, Debug)]
pub struct VarsScope<'a> {
    /// The variables by name.
    pub vars: &'a BTreeMap<String, Value>,
}

impl Scope for VarsScope<'_> {
    fn lookup(&self, path: &[&str]) -> Option<Value> {
        let (name, rest) = path.split_first()?;
        value_path(self.vars.get(*name)?.clone(), rest)
    }
}

/// Combines scopes: the first scope that provides a variable resolves it.
///
/// The variable (`path[0]`) decides, not the whole path: once a scope
/// provides `host`, it alone answers for `host.…`, even where it can't
/// follow the path (through a string, say). Later scopes never answer for
/// a variable an earlier one provides.
///
/// For API filters, put the object's scope before the [`VarsScope`] with the
/// `filter_vars`: in Icinga, `host`, `service` and `obj` win over filter
/// variables of the same name.
#[derive(Clone, Copy)]
pub struct Chain<'a> {
    /// The scopes, in priority order.
    pub scopes: &'a [&'a dyn Scope],
}

impl Scope for Chain<'_> {
    fn lookup(&self, path: &[&str]) -> Option<Value> {
        for scope in self.scopes {
            if let Some(value) = scope.lookup(path) {
                return Some(value);
            }
            // The scope can't follow the path; if it provides the variable,
            // the evaluator has to retry with a shorter path rather than let
            // a later scope answer. (Only asked on this rare path, since
            // resolving just the variable may build a large value.)
            if path.len() > 1 && scope.lookup(&path[..1]).is_some() {
                return None;
            }
        }
        None
    }
}

impl fmt::Debug for Chain<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Chain")
            .field("scopes", &self.scopes.len())
            .finish()
    }
}

/// Follows `rest` into a value; `None` where Icinga would fail (members of
/// strings, numbers and booleans, non-numeric array indexes).
fn value_path(mut value: Value, rest: &[&str]) -> Option<Value> {
    for name in rest {
        value = match lookup_field(&value, name) {
            Field::Found(found) => found,
            Field::Missing => return Some(Value::Null),
            Field::Invalid => return None,
        };
    }
    Some(value)
}

/// A scalar attribute: members of it are left to the evaluator (which
/// reports them), except for `null`.
fn scalar_path(value: Value, rest: &[&str]) -> Option<Value> {
    if rest.is_empty() || value.is_null() {
        Some(value)
    } else {
        None
    }
}

/// Follows `rest` into custom variables without converting what isn't
/// needed.
fn json_path(vars: &Vars, rest: &[&str]) -> Option<Value> {
    let Some((first, deeper)) = rest.split_first() else {
        return Some(vars_value(vars));
    };
    let Some(mut current) = vars.get(*first) else {
        return Some(Value::Null);
    };
    for name in deeper {
        current = match current {
            serde_json::Value::Object(entries) => match entries.get(*name) {
                Some(next) => next,
                None => return Some(Value::Null),
            },
            serde_json::Value::Array(items) => {
                let index = crate::ops::parse_integer(name)?;
                match usize::try_from(index)
                    .ok()
                    .and_then(|index| items.get(index))
                {
                    Some(next) => next,
                    None => return Some(Value::Null),
                }
            }
            serde_json::Value::Null => return Some(Value::Null),
            _ => return None,
        };
    }
    Some(Value::from_json(current))
}

/// Custom variables as a dictionary. Objects without any are `null`, as in
/// Icinga, where `vars` is unset until a variable is assigned.
fn vars_value(vars: &Vars) -> Value {
    if vars.is_empty() {
        return Value::Null;
    }
    Value::from(
        vars.iter()
            .map(|(key, value)| (key.clone(), Value::from_json(value)))
            .collect::<BTreeMap<_, _>>(),
    )
}

fn strings_value(items: &[String]) -> Value {
    items.iter().map(|item| Value::str(item)).collect()
}

/// `groups` and `groups[i]`.
fn strings_path(items: &[String], rest: &[&str]) -> Option<Value> {
    match rest.split_first() {
        None => Some(strings_value(items)),
        Some((index, deeper)) => {
            let index = crate::ops::parse_integer(index)?;
            let item = usize::try_from(index)
                .ok()
                .and_then(|index| items.get(index))
                .map_or(Value::Null, |item| Value::str(item));
            scalar_path(item, deeper)
        }
    }
}

const HOST_ATTRIBUTES: [&str; 13] = [
    "name",
    "display_name",
    "__name",
    "type",
    "address",
    "address6",
    "state",
    "problem",
    "handled",
    "severity",
    "vars",
    "groups",
    "last_check_result",
];

const SERVICE_ATTRIBUTES: [&str; 12] = [
    "name",
    "display_name",
    "__name",
    "host_name",
    "type",
    "state",
    "problem",
    "handled",
    "severity",
    "vars",
    "groups",
    "last_check_result",
];

const CHECK_ATTRIBUTES: [&str; 24] = [
    "state_type",
    "last_state_change",
    "last_hard_state_change",
    "last_check",
    "next_check",
    "check_attempt",
    "max_check_attempts",
    "acknowledgement",
    "acknowledgement_expiry",
    "downtime_depth",
    "flapping",
    "flapping_current",
    "last_reachable",
    "check_command",
    "check_interval",
    "retry_interval",
    "command_endpoint",
    "zone",
    "enable_active_checks",
    "enable_passive_checks",
    "enable_notifications",
    "enable_event_handler",
    "enable_flapping",
    "enable_perfdata",
];

const LINK_ATTRIBUTES: [&str; 4] = ["notes", "notes_url", "action_url", "icon_image"];

fn host_path(host: &Host, rest: &[&str]) -> Option<Value> {
    let Some((attribute, deeper)) = rest.split_first() else {
        return Some(host_dict(host));
    };
    match *attribute {
        "vars" => json_path(&host.vars, deeper),
        "groups" => strings_path(&host.groups, deeper),
        "last_check_result" => {
            check_result_path(&host.check, &CheckState::Host(host.state), deeper)
        }
        other => scalar_path(host_attribute(host, other).unwrap_or_default(), deeper),
    }
}

fn host_attribute(host: &Host, name: &str) -> Option<Value> {
    Some(match name {
        "name" | "__name" => Value::str(host.name.as_str()),
        "display_name" => Value::str(&host.display_name),
        "type" => Value::str("Host"),
        "address" => Value::str(&host.address),
        "address6" => Value::str(&host.address6),
        "state" => Value::Number(host_state_code(host.state)),
        "problem" => Value::Bool(host.is_problem()),
        "handled" => Value::Bool(host.is_handled()),
        "severity" => Value::from(host.severity()),
        "vars" => vars_value(&host.vars),
        "groups" => strings_value(&host.groups),
        "last_check_result" => check_result_value(&host.check, &CheckState::Host(host.state)),
        other => {
            return check_attribute(&host.check, other)
                .or_else(|| links_attribute(&host.links, other));
        }
    })
}

fn host_dict(host: &Host) -> Value {
    let entries = HOST_ATTRIBUTES
        .iter()
        .chain(CHECK_ATTRIBUTES.iter())
        .chain(LINK_ATTRIBUTES.iter())
        .filter_map(|name| host_attribute(host, name).map(|value| ((*name).to_owned(), value)))
        .collect::<BTreeMap<_, _>>();
    Value::from(entries)
}

/// Icinga's host `state`. Pending hosts are 1: Icinga's raw state starts
/// as UNKNOWN, which `Host::CalculateState` turns into DOWN.
fn host_state_code(state: HostState) -> f64 {
    match state {
        HostState::Up => 0.0,
        HostState::Down | HostState::Unreachable | HostState::Pending => 1.0,
    }
}

/// Icinga's service `state`. Pending services are 3: Icinga's raw state
/// starts as UNKNOWN (`checkable.ti`), and the API reports it until the
/// first check result.
fn service_state_code(state: ServiceState) -> f64 {
    match state {
        ServiceState::Ok => 0.0,
        ServiceState::Warning => 1.0,
        ServiceState::Critical => 2.0,
        ServiceState::Unknown | ServiceState::Pending => 3.0,
    }
}

fn timestamp(value: Option<ic_model::Timestamp>, never: f64) -> Value {
    Value::Number(value.map_or(never, ic_model::Timestamp::as_unix_seconds))
}

/// Attributes shared by hosts and services (Icinga's `Checkable`).
fn check_attribute(check: &CheckInfo, name: &str) -> Option<Value> {
    let features = &check.features;
    Some(match name {
        "state_type" => Value::Number(match check.state_type {
            StateType::Soft => 0.0,
            StateType::Hard => 1.0,
        }),
        "last_state_change" => Value::Number(check.last_state_change.as_unix_seconds()),
        "last_hard_state_change" => Value::Number(check.last_hard_state_change.as_unix_seconds()),
        // Icinga reports -1 for objects that were never checked.
        "last_check" => timestamp(check.last_check, -1.0),
        "next_check" => timestamp(check.next_check, 0.0),
        "check_attempt" => Value::from(check.attempt),
        "max_check_attempts" => Value::from(check.max_attempts),
        "acknowledgement" => Value::Number(match check.acknowledgement {
            AckKind::None => 0.0,
            AckKind::Normal => 1.0,
            AckKind::Sticky => 2.0,
        }),
        "acknowledgement_expiry" => timestamp(check.acknowledgement_expiry, 0.0),
        "downtime_depth" => Value::from(check.downtime_depth),
        "flapping" => Value::Bool(check.flapping),
        "flapping_current" => Value::Number(check.flapping_current),
        "last_reachable" => Value::Bool(check.reachable),
        "check_command" => Value::str(&check.check_command),
        "check_interval" => Value::Number(check.check_interval),
        "retry_interval" => Value::Number(check.retry_interval),
        "command_endpoint" => Value::str(check.command_endpoint.as_deref().unwrap_or_default()),
        "zone" => Value::str(check.zone.as_deref().unwrap_or_default()),
        "enable_active_checks" => Value::Bool(features.active_checks),
        "enable_passive_checks" => Value::Bool(features.passive_checks),
        "enable_notifications" => Value::Bool(features.notifications),
        "enable_event_handler" => Value::Bool(features.event_handler),
        "enable_flapping" => Value::Bool(features.flap_detection),
        "enable_perfdata" => Value::Bool(features.perfdata),
        _ => return None,
    })
}

fn links_attribute(links: &Links, name: &str) -> Option<Value> {
    Some(Value::str(match name {
        "notes" => &links.notes,
        "notes_url" => &links.notes_url,
        "action_url" => &links.action_url,
        "icon_image" => &links.icon_image,
        _ => return None,
    }))
}

/// The object state a check result belongs to.
enum CheckState {
    Host(HostState),
    Service(ServiceState),
}

const CHECK_RESULT_ATTRIBUTES: [&str; 9] = [
    "output",
    "exit_status",
    "state",
    "execution_start",
    "execution_end",
    "schedule_start",
    "check_source",
    "active",
    "type",
];

fn check_result_path(check: &CheckInfo, state: &CheckState, rest: &[&str]) -> Option<Value> {
    let Some(result) = &check.result else {
        // Pending: `last_check_result` is null, and so is anything below it.
        return Some(Value::Null);
    };
    match rest.split_first() {
        None => Some(check_result_value(check, state)),
        Some((name, deeper)) => scalar_path(
            check_result_attribute(result, state, name).unwrap_or_default(),
            deeper,
        ),
    }
}

fn check_result_value(check: &CheckInfo, state: &CheckState) -> Value {
    let Some(result) = &check.result else {
        return Value::Null;
    };
    let entries = CHECK_RESULT_ATTRIBUTES
        .iter()
        .filter_map(|name| {
            check_result_attribute(result, state, name).map(|value| ((*name).to_owned(), value))
        })
        .collect::<BTreeMap<_, _>>();
    Value::from(entries)
}

fn check_result_attribute(result: &CheckResult, state: &CheckState, name: &str) -> Option<Value> {
    Some(match name {
        // Icinga keeps the plugin output whole; the model splits it.
        "output" => {
            if result.long_output.is_empty() {
                Value::str(&result.output)
            } else {
                Value::from(format!("{}\n{}", result.output, result.long_output))
            }
        }
        "exit_status" => Value::from(result.exit_status),
        "state" => Value::Number(check_result_state(result, state)),
        "execution_start" => Value::Number(result.execution_start.as_unix_seconds()),
        "execution_end" => Value::Number(result.execution_end.as_unix_seconds()),
        "schedule_start" => Value::Number(result.schedule_start.as_unix_seconds()),
        "check_source" => Value::str(&result.check_source),
        "active" => Value::Bool(result.active),
        "type" => Value::str("CheckResult"),
        _ => return None,
    })
}

/// The check result's own state, a service state code even for hosts
/// (0 ok, 1 warning, 2 critical, 3 unknown). The model doesn't keep it, so
/// for hosts it's derived from the exit status, consistent with the host
/// state (up is ok or warning, down is critical or unknown):
///
/// - Icinga maps exit statuses outside 0–3 to UNKNOWN.
/// - Passive results keep exit status 0 whatever their state.
/// - Icinga sets UNKNOWN without touching the exit status when it can't run
///   a check, for example on a disconnected command endpoint; such a result
///   is active with exit status 0, which can't otherwise belong to a down
///   host.
fn check_result_state(result: &CheckResult, state: &CheckState) -> f64 {
    match state {
        CheckState::Service(state) => service_state_code(*state),
        CheckState::Host(HostState::Up | HostState::Pending) => {
            if result.exit_status == 1 {
                1.0
            } else {
                0.0
            }
        }
        CheckState::Host(HostState::Down | HostState::Unreachable) => match result.exit_status {
            0 | 1 if result.active => 3.0,
            0..=2 => 2.0,
            _ => 3.0,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ic_model::Timestamp;
    use serde_json::json;

    fn host() -> Host {
        let mut host = Host::new("db-prod-03");
        host.display_name = "DB prod 03".to_owned();
        host.address = "10.0.4.3".to_owned();
        host.address6 = "fd00::3".to_owned();
        host.state = HostState::Up;
        host.groups = vec!["databases".to_owned(), "linux".to_owned()];
        host.vars = json!({
            "role": "postgres",
            "disks": { "/var": { "warn": "80%" } },
            "ports": [5432, 9187],
            "nothing": null,
        })
        .as_object()
        .cloned()
        .unwrap_or_default();
        host.check.result = Some(CheckResult {
            output: "PING OK".to_owned(),
            long_output: "rta 0.1ms".to_owned(),
            exit_status: 0,
            check_source: "master-01".to_owned(),
            active: true,
            ..CheckResult::default()
        });
        host.check.last_check = Some(Timestamp::from_unix_seconds(1_700_000_000.0));
        host
    }

    fn lookup(scope: &dyn Scope, path: &[&str]) -> Option<Value> {
        scope.lookup(path)
    }

    #[test]
    fn host_attributes() {
        let host = host();
        let scope = HostScope { host: &host };
        let cases = [
            (vec!["host", "name"], Value::from("db-prod-03")),
            (vec!["obj", "name"], Value::from("db-prod-03")),
            (vec!["host", "__name"], Value::from("db-prod-03")),
            (vec!["host", "display_name"], Value::from("DB prod 03")),
            (vec!["host", "type"], Value::from("Host")),
            (vec!["host", "address"], Value::from("10.0.4.3")),
            (vec!["host", "address6"], Value::from("fd00::3")),
            (vec!["host", "state"], Value::Number(0.0)),
            (vec!["host", "state_type"], Value::Number(1.0)),
            (vec!["host", "last_check"], Value::Number(1_700_000_000.0)),
            (vec!["host", "next_check"], Value::Number(0.0)),
            (vec!["host", "problem"], Value::Bool(false)),
            (vec!["host", "handled"], Value::Bool(false)),
            (vec!["host", "severity"], Value::Number(0.0)),
            (vec!["host", "check_interval"], Value::Number(300.0)),
            (vec!["host", "zone"], Value::from("")),
            (vec!["host", "enable_flapping"], Value::Bool(false)),
            (
                vec!["host", "groups"],
                Value::from(vec![Value::from("databases"), Value::from("linux")]),
            ),
            (vec!["host", "groups", "1"], Value::from("linux")),
            (vec!["host", "groups", "5"], Value::Null),
            (vec!["host", "vars", "role"], Value::from("postgres")),
            (
                vec!["host", "vars", "disks", "/var", "warn"],
                Value::from("80%"),
            ),
            (vec!["host", "vars", "disks", "/tmp", "warn"], Value::Null),
            (vec!["host", "vars", "ports", "0"], Value::Number(5432.0)),
            (vec!["host", "vars", "ports", "9"], Value::Null),
            (vec!["host", "vars", "nothing", "x"], Value::Null),
            (vec!["host", "vars", "missing"], Value::Null),
            (vec!["host", "nonexistent"], Value::Null),
            (vec!["host", "nonexistent", "deeper"], Value::Null),
            (
                vec!["host", "last_check_result", "output"],
                Value::from("PING OK\nrta 0.1ms"),
            ),
            (
                vec!["host", "last_check_result", "state"],
                Value::Number(0.0),
            ),
            (
                vec!["host", "last_check_result", "check_source"],
                Value::from("master-01"),
            ),
            (vec!["host", "last_check_result", "unknown"], Value::Null),
        ];
        for (path, expected) in cases {
            assert_eq!(lookup(&scope, &path), Some(expected), "{path:?}");
        }
        // Paths through scalars are left to the evaluator.
        assert_eq!(lookup(&scope, &["host", "name", "x"]), None);
        assert_eq!(lookup(&scope, &["host", "vars", "role", "x"]), None);
        assert_eq!(lookup(&scope, &["host", "vars", "ports", "x"]), None);
        assert_eq!(lookup(&scope, &["host", "groups", "x"]), None);
        assert_eq!(lookup(&scope, &["host", "groups", "0", "x"]), None);
        assert_eq!(lookup(&scope, &["service", "name"]), None);
        assert_eq!(lookup(&scope, &["anything"]), None);

        let Some(Value::Dict(all)) = lookup(&scope, &["host"]) else {
            panic!("host is a dictionary");
        };
        for name in HOST_ATTRIBUTES
            .iter()
            .chain(&CHECK_ATTRIBUTES)
            .chain(&LINK_ATTRIBUTES)
        {
            assert!(all.contains_key(*name), "{name}");
        }
        assert_eq!(all["vars"].as_dict().map(BTreeMap::len), Some(4));
        let Some(Value::Dict(result)) = lookup(&scope, &["host", "last_check_result"]) else {
            panic!("check result is a dictionary");
        };
        assert_eq!(result.len(), CHECK_RESULT_ATTRIBUTES.len());
    }

    #[test]
    fn host_states_follow_icinga() {
        let mut host = host();
        let state = |host: &Host, name: &str| HostScope { host }.lookup(&["host", name]);
        host.state = HostState::Down;
        host.check.result = Some(CheckResult {
            exit_status: 2,
            ..CheckResult::default()
        });
        assert_eq!(state(&host, "state"), Some(Value::Number(1.0)));
        assert_eq!(state(&host, "problem"), Some(Value::Bool(true)));
        assert_eq!(state(&host, "handled"), Some(Value::Bool(false)));
        assert_eq!(state(&host, "severity"), Some(Value::Number(2080.0)));
        assert_eq!(
            HostScope { host: &host }.lookup(&["host", "last_check_result", "state"]),
            Some(Value::Number(2.0))
        );
        host.state = HostState::Unreachable;
        host.check.reachable = false;
        assert_eq!(state(&host, "state"), Some(Value::Number(1.0)));
        assert_eq!(state(&host, "last_reachable"), Some(Value::Bool(false)));
        host.check.acknowledgement = AckKind::Sticky;
        assert_eq!(state(&host, "acknowledgement"), Some(Value::Number(2.0)));
        assert_eq!(state(&host, "handled"), Some(Value::Bool(true)));

        // Icinga reports pending hosts as DOWN (state 1) but not as problems.
        let pending = Host::new("new");
        assert_eq!(state(&pending, "state"), Some(Value::Number(1.0)));
        assert_eq!(state(&pending, "problem"), Some(Value::Bool(false)));
        assert_eq!(state(&pending, "handled"), Some(Value::Bool(false)));
        assert_eq!(state(&pending, "last_check_result"), Some(Value::Null));
        assert_eq!(
            HostScope { host: &pending }.lookup(&["host", "last_check_result", "output"]),
            Some(Value::Null)
        );
        assert_eq!(state(&pending, "last_check"), Some(Value::Number(-1.0)));
        assert_eq!(state(&pending, "severity"), Some(Value::Number(16.0)));
    }

    fn service(host: &str) -> Service {
        let mut service = Service::new(host, "postgres-replication");
        service.display_name = "Postgres replication".to_owned();
        service.state = ServiceState::Critical;
        service.check.state_type = StateType::Soft;
        service.check.attempt = 2;
        service.check.max_attempts = 3;
        service.check.downtime_depth = 0;
        service.check.check_command = "pg_replication".to_owned();
        service.check.zone = Some("prod".to_owned());
        service.check.result = Some(CheckResult {
            output: "CRITICAL - lag 412s".to_owned(),
            exit_status: 2,
            ..CheckResult::default()
        });
        service.groups = vec!["postgres".to_owned()];
        service.vars = json!({ "team": "dba" })
            .as_object()
            .cloned()
            .unwrap_or_default();
        service.links.notes_url = "https://wiki/pg".to_owned();
        service
    }

    #[test]
    fn service_attributes() {
        let host = host();
        let service = service("db-prod-03");
        let scope = ServiceScope {
            service: &service,
            host: Some(&host),
        };
        let cases = [
            (vec!["service", "name"], Value::from("postgres-replication")),
            (vec!["obj", "name"], Value::from("postgres-replication")),
            (
                vec!["service", "__name"],
                Value::from("db-prod-03!postgres-replication"),
            ),
            (vec!["service", "host_name"], Value::from("db-prod-03")),
            (
                vec!["service", "display_name"],
                Value::from("Postgres replication"),
            ),
            (vec!["service", "type"], Value::from("Service")),
            (vec!["service", "state"], Value::Number(2.0)),
            (vec!["service", "state_type"], Value::Number(0.0)),
            (vec!["service", "check_attempt"], Value::Number(2.0)),
            (vec!["service", "max_check_attempts"], Value::Number(3.0)),
            (vec!["service", "problem"], Value::Bool(true)),
            (vec!["service", "handled"], Value::Bool(false)),
            (vec!["service", "severity"], Value::Number(2176.0)),
            (
                vec!["service", "check_command"],
                Value::from("pg_replication"),
            ),
            (vec!["service", "zone"], Value::from("prod")),
            (vec!["service", "notes_url"], Value::from("https://wiki/pg")),
            (vec!["service", "groups", "0"], Value::from("postgres")),
            (vec!["service", "vars", "team"], Value::from("dba")),
            (
                vec!["service", "last_check_result", "output"],
                Value::from("CRITICAL - lag 412s"),
            ),
            (
                vec!["service", "last_check_result", "state"],
                Value::Number(2.0),
            ),
            (
                vec!["service", "last_check_result", "exit_status"],
                Value::Number(2.0),
            ),
            (vec!["service", "host", "name"], Value::from("db-prod-03")),
            (
                vec!["service", "host", "vars", "role"],
                Value::from("postgres"),
            ),
            (vec!["host", "name"], Value::from("db-prod-03")),
            (vec!["host", "address"], Value::from("10.0.4.3")),
            (vec!["host", "vars", "role"], Value::from("postgres")),
        ];
        for (path, expected) in cases {
            assert_eq!(scope.lookup(&path), Some(expected), "{path:?}");
        }
        assert_eq!(scope.lookup(&["service", "address"]), Some(Value::Null));
        assert_eq!(scope.lookup(&["names"]), None);

        let Some(Value::Dict(all)) = scope.lookup(&["service"]) else {
            panic!("service is a dictionary");
        };
        for name in SERVICE_ATTRIBUTES
            .iter()
            .chain(&CHECK_ATTRIBUTES)
            .chain(&LINK_ATTRIBUTES)
        {
            assert!(all.contains_key(*name), "{name}");
        }
        assert_eq!(
            all["host"].as_dict().and_then(|host| host.get("name")),
            Some(&Value::from("db-prod-03"))
        );
    }

    #[test]
    fn handled_follows_icinga() {
        let mut host = host();
        let mut service = service("db-prod-03");
        let handled = |service: &Service, host: Option<&Host>| {
            ServiceScope { service, host }.lookup(&["service", "handled"])
        };
        assert_eq!(handled(&service, Some(&host)), Some(Value::Bool(false)));
        service.check.acknowledgement = AckKind::Normal;
        assert_eq!(handled(&service, Some(&host)), Some(Value::Bool(true)));
        service.check.acknowledgement = AckKind::None;
        service.check.downtime_depth = 1;
        assert_eq!(handled(&service, Some(&host)), Some(Value::Bool(true)));
        service.check.downtime_depth = 0;
        host.state = HostState::Down;
        assert_eq!(
            handled(&service, Some(&host)),
            Some(Value::Bool(true)),
            "host problem"
        );
        service.state = ServiceState::Ok;
        assert_eq!(
            handled(&service, Some(&host)),
            Some(Value::Bool(true)),
            "Icinga counts every service of a problem host as handled"
        );
        assert_eq!(handled(&service, None), Some(Value::Bool(false)));
    }

    #[test]
    fn service_without_host() {
        let service = service("web-01");
        let scope = ServiceScope {
            service: &service,
            host: None,
        };
        assert_eq!(scope.lookup(&["host", "name"]), Some(Value::from("web-01")));
        assert_eq!(
            scope.lookup(&["host", "__name"]),
            Some(Value::from("web-01"))
        );
        assert_eq!(scope.lookup(&["host", "address"]), Some(Value::Null));
        assert_eq!(scope.lookup(&["host", "vars", "role"]), Some(Value::Null));
        assert_eq!(scope.lookup(&["host", "name", "x"]), None);
        assert_eq!(
            scope.lookup(&["service", "host", "name"]),
            Some(Value::from("web-01"))
        );
        let Some(Value::Dict(host)) = scope.lookup(&["host"]) else {
            panic!("host is a dictionary");
        };
        assert_eq!(host.len(), 2);
    }

    #[test]
    fn pending_services_are_unknown_but_no_problem() {
        // As recorded from Icinga 2.15.6 (`db-prod-03!postgres-replication`):
        // state 3, last_check -1, last_check_result null, problem false,
        // handled false, severity 16.
        let service = Service::new("h", "s");
        let scope = ServiceScope {
            service: &service,
            host: None,
        };
        for (attribute, expected) in [
            ("state", Value::Number(3.0)),
            ("last_check", Value::Number(-1.0)),
            ("last_check_result", Value::Null),
            ("problem", Value::Bool(false)),
            ("handled", Value::Bool(false)),
            ("severity", Value::Number(16.0)),
        ] {
            assert_eq!(
                scope.lookup(&["service", attribute]),
                Some(expected),
                "{attribute}"
            );
        }
    }

    #[test]
    fn objects_without_custom_variables_have_null_vars() {
        // As recorded from Icinga 2.15.6 (`"vars": null`).
        let host = Host::new("bare");
        let scope = HostScope { host: &host };
        assert_eq!(scope.lookup(&["host", "vars"]), Some(Value::Null));
        assert_eq!(scope.lookup(&["host", "vars", "role"]), Some(Value::Null));
        let Some(Value::Dict(all)) = scope.lookup(&["host"]) else {
            panic!("host is a dictionary");
        };
        assert_eq!(all.get("vars"), Some(&Value::Null));
        let service = Service::new("bare", "s");
        let service_scope = ServiceScope {
            service: &service,
            host: Some(&host),
        };
        for source in [
            "service.vars == null",
            "host.vars == null",
            "\"x\" !in service.vars",
            "!(\"x\" in host.vars)",
            "typeof(service.vars) == Object",
            "len(service.vars) == 0",
        ] {
            let filter = crate::Filter::parse(source).unwrap();
            assert_eq!(
                filter.evaluate(&service_scope),
                Ok(Value::Bool(true)),
                "{source}"
            );
        }
    }

    #[test]
    fn host_check_result_state_is_derived_like_icinga() {
        let mut host = host();
        let result_state =
            |host: &Host| HostScope { host }.lookup(&["host", "last_check_result", "state"]);
        let cases = [
            (HostState::Up, 0, true, 0.0),
            (HostState::Up, 1, true, 1.0),
            (HostState::Up, 0, false, 0.0),
            (HostState::Down, 2, true, 2.0),
            (HostState::Down, 3, true, 3.0),
            (HostState::Down, 4, true, 3.0),
            (HostState::Down, 127, true, 3.0),
            (HostState::Down, -1, true, 3.0),
            // Passive DOWN results keep exit status 0.
            (HostState::Down, 0, false, 2.0),
            // A disconnected command endpoint: UNKNOWN, exit status 0.
            (HostState::Down, 0, true, 3.0),
            (HostState::Unreachable, 2, true, 2.0),
        ];
        for (state, exit_status, active, expected) in cases {
            host.state = state;
            host.check.result = Some(CheckResult {
                exit_status,
                active,
                ..CheckResult::default()
            });
            assert_eq!(
                result_state(&host),
                Some(Value::Number(expected)),
                "{state:?} exit {exit_status} active {active}"
            );
        }
    }

    #[test]
    fn chain_lets_the_scope_providing_a_variable_answer_alone() {
        let host = host();
        let host_scope = HostScope { host: &host };
        let shadow = BTreeMap::from([(
            "host".to_owned(),
            Value::from(BTreeMap::from([(
                "name".to_owned(),
                Value::from(BTreeMap::from([("x".to_owned(), Value::Number(1.0))])),
            )])),
        )]);
        let shadow_scope = VarsScope { vars: &shadow };
        let chain = Chain {
            scopes: &[&host_scope, &shadow_scope],
        };
        assert_eq!(chain.lookup(&["host", "name", "x"]), None);
        assert_eq!(
            chain.lookup(&["host", "name"]),
            Some(Value::from("db-prod-03"))
        );
        let filter = crate::Filter::parse("host.name.x").unwrap();
        assert_eq!(
            filter.evaluate(&chain).unwrap_err().message,
            "invalid field access (for value of type 'String'): 'x' (in `host.name.x`)",
            "the host answers, as with HostScope alone"
        );
        // A variable the first scope doesn't provide still falls through.
        let vars = BTreeMap::from([("cfg".to_owned(), Value::from(vec![Value::Number(7.0)]))]);
        let vars_scope = VarsScope { vars: &vars };
        let chain = Chain {
            scopes: &[&host_scope, &vars_scope],
        };
        assert_eq!(chain.lookup(&["cfg", "0"]), Some(Value::Number(7.0)));
        assert_eq!(chain.lookup(&["cfg", "x"]), None);
    }

    #[test]
    fn vars_scope_and_chain() {
        let vars = BTreeMap::from([
            ("names".to_owned(), Value::from(vec![Value::from("a!b")])),
            (
                "cfg".to_owned(),
                Value::from(BTreeMap::from([("depth".to_owned(), Value::Number(2.0))])),
            ),
        ]);
        let scope = VarsScope { vars: &vars };
        assert_eq!(scope.lookup(&["names", "0"]), Some(Value::from("a!b")));
        assert_eq!(scope.lookup(&["names", "1"]), Some(Value::Null));
        assert_eq!(scope.lookup(&["names", "x"]), None);
        assert_eq!(scope.lookup(&["cfg", "depth"]), Some(Value::Number(2.0)));
        assert_eq!(scope.lookup(&["cfg", "depth", "x"]), None);
        assert_eq!(scope.lookup(&["cfg", "missing", "x"]), Some(Value::Null));
        assert_eq!(scope.lookup(&["other"]), None);

        let host = host();
        let host_scope = HostScope { host: &host };
        let shadow = BTreeMap::from([("host".to_owned(), Value::from("shadowed"))]);
        let shadow_scope = VarsScope { vars: &shadow };
        let chain = Chain {
            scopes: &[&host_scope, &scope, &shadow_scope],
        };
        assert_eq!(
            chain.lookup(&["host", "name"]),
            Some(Value::from("db-prod-03"))
        );
        assert_eq!(chain.lookup(&["names", "0"]), Some(Value::from("a!b")));
        assert_eq!(chain.lookup(&["nothing"]), None);
        assert_eq!(format!("{chain:?}"), "Chain { scopes: 3 }");
        let reference: &dyn Scope = &&scope;
        assert_eq!(
            reference.lookup(&["cfg", "depth"]),
            Some(Value::Number(2.0))
        );
    }
}
