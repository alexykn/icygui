//! `POST /v1/actions/<name>` (`ActionsHandler`, `apiactions.cpp`).

use std::collections::BTreeSet;

use serde_json::{Map, Value as Json};

use super::params::{Params, to_icinga_string};
use super::response::{Body, json, json_error};
use super::targets::{TargetError, filter_targets};
use crate::auth::Principal;
use crate::config::NumberFormat;
use crate::json::{int, num};
use crate::model::logic::{DowntimeSpec, RemovalReason};
use crate::model::{CheckInput, DepType, ObjKind, ObjRef, PendingExecution, ProcessOutcome, World};

/// The actions Icinga registers and the object types they accept (sorted
/// by type name, as Icinga's `std::set`).
const ACTIONS: &[(&str, &[ObjKind])] = &[
    ("process-check-result", &[ObjKind::Host, ObjKind::Service]),
    ("reschedule-check", &[ObjKind::Host, ObjKind::Service]),
    (
        "send-custom-notification",
        &[ObjKind::Host, ObjKind::Service],
    ),
    ("delay-notification", &[ObjKind::Host, ObjKind::Service]),
    ("acknowledge-problem", &[ObjKind::Host, ObjKind::Service]),
    ("remove-acknowledgement", &[ObjKind::Host, ObjKind::Service]),
    ("add-comment", &[ObjKind::Host, ObjKind::Service]),
    (
        "remove-comment",
        &[ObjKind::Comment, ObjKind::Host, ObjKind::Service],
    ),
    ("schedule-downtime", &[ObjKind::Host, ObjKind::Service]),
    (
        "remove-downtime",
        &[ObjKind::Downtime, ObjKind::Host, ObjKind::Service],
    ),
    ("execute-command", &[ObjKind::Host, ObjKind::Service]),
];

/// A per-object result: `{"code": ..., "status": ...}` plus extras.
struct ActionResult {
    code: u16,
    status: String,
    extra: Map<String, Json>,
}

impl ActionResult {
    fn new(code: u16, status: impl Into<String>) -> Self {
        Self {
            code,
            status: status.into(),
            extra: Map::new(),
        }
    }

    fn with(mut self, key: &str, value: Json) -> Self {
        self.extra.insert(key.to_owned(), value);
        self
    }

    fn into_json(self) -> Json {
        let mut map = self.extra;
        map.insert("code".into(), int(self.code));
        map.insert("status".into(), Json::String(self.status));
        Json::Object(map)
    }
}

/// `DiagnosticInformation(ex, false)` of an exception with a message.
fn diagnostic(message: &str) -> String {
    format!("Error: {message}\n")
}

/// An exception inside an action: Icinga reports it as a 500 result.
fn failed(message: &str) -> ActionResult {
    ActionResult::new(
        500,
        format!("Action execution failed: '{}'.", diagnostic(message)),
    )
}

/// Handles an action request.
pub(crate) fn handle(
    world: &mut World,
    user: &Principal,
    params: &Params,
    action: &str,
    format: NumberFormat,
    enforce_filter_permission: bool,
) -> hyper::Response<Body> {
    let Some((_, types)) = ACTIONS.iter().find(|(name, _)| *name == action) else {
        tracing::warn!(action, "unknown or unsupported action");
        return json_error(
            404,
            &format!("Action '{action}' does not exist."),
            format,
            Some(params),
            None,
        );
    };
    let targets = match filter_targets(
        world,
        types,
        &[],
        &format!("actions/{action}"),
        params,
        user,
        enforce_filter_permission,
    ) {
        Ok(targets) => targets,
        Err(TargetError::Forbidden(message)) => {
            return json_error(403, &message, format, Some(params), None);
        }
        Err(TargetError::NotFound(diagnostic)) => {
            return json_error(
                404,
                "No objects found.",
                format,
                Some(params),
                Some(&diagnostic),
            );
        }
        Err(TargetError::Unsupported(message)) => {
            return json_error(400, &message, format, Some(params), None);
        }
    };
    if targets.is_empty() {
        return json_error(404, "No objects found.", format, Some(params), None);
    }
    let verbose = params.verbose();
    let mut results = Vec::with_capacity(targets.len());
    for target in &targets {
        let result = match invoke(world, user, params, action, target) {
            Ok(result) => result,
            Err(message) => {
                let mut result = failed(&message);
                if verbose {
                    result.extra.insert(
                        "diagnostic_information".into(),
                        Json::String(diagnostic(&message)),
                    );
                }
                result
            }
        };
        results.push(result);
    }
    let ok: BTreeSet<u16> = results
        .iter()
        .map(|r| r.code)
        .filter(|c| (200..=299).contains(c))
        .collect();
    let not_ok: BTreeSet<u16> = results
        .iter()
        .map(|r| r.code)
        .filter(|c| !(200..=299).contains(c))
        .collect();
    let status = match (ok.len(), not_ok.len()) {
        (1, 0) => ok.first().copied().unwrap_or(200),
        (_, 1) => not_ok.first().copied().unwrap_or(500),
        (n, 0) if n >= 2 => 200,
        _ => 500,
    };
    let mut body = Map::new();
    body.insert(
        "results".into(),
        Json::Array(results.into_iter().map(ActionResult::into_json).collect()),
    );
    json(status, Json::Object(body), format, params.pretty())
}

#[expect(
    clippy::too_many_lines,
    reason = "mirrors the action dispatch of Icinga's ActionsHandler"
)]
fn invoke(
    world: &mut World,
    user: &Principal,
    params: &Params,
    action: &str,
    target: &ObjRef,
) -> Result<ActionResult, String> {
    match action {
        "process-check-result" => process_check_result(world, params, &target.name),
        "reschedule-check" => reschedule_check(world, params, &target.name),
        "send-custom-notification" => {
            if !params.contains("author") {
                return Ok(ActionResult::new(400, "Parameter 'author' is required."));
            }
            if !params.contains("comment") {
                return Ok(ActionResult::new(400, "Parameter 'comment' is required."));
            }
            if params.last_bool("force")
                && let Some(checkable) = world.checkable_mut(&target.name)
            {
                checkable.force_next_notification = true;
            }
            Ok(ActionResult::new(
                200,
                format!(
                    "Successfully sent custom notification for object '{}'.",
                    target.name
                ),
            ))
        }
        "delay-notification" => {
            if !params.contains("timestamp") {
                return Ok(ActionResult::new(
                    400,
                    "A timestamp is required to delay notifications",
                ));
            }
            // Icinga only converts the timestamp per notification object;
            // the mock has none, so nothing is converted or stored.
            Ok(ActionResult::new(
                200,
                format!(
                    "Successfully delayed notifications for object '{}'.",
                    target.name
                ),
            ))
        }
        "acknowledge-problem" => acknowledge(world, params, &target.name),
        "remove-acknowledgement" => {
            let removed_by = params.last_string("author");
            world.clear_acknowledgement(&target.name, &removed_by);
            world.remove_ack_comments(&target.name, &removed_by, f64::INFINITY);
            Ok(ActionResult::new(
                200,
                format!(
                    "Successfully removed acknowledgement for object '{}'.",
                    target.name
                ),
            ))
        }
        "add-comment" => {
            if !params.contains("author") || !params.contains("comment") {
                return Ok(ActionResult::new(
                    400,
                    "Comments require author and comment.",
                ));
            }
            let expiry = if params.contains("expiry") {
                params.last_f64("expiry")?
            } else {
                0.0
            };
            let Some((name, legacy_id)) = world.add_comment(
                &target.name,
                1,
                &params.last_string("author"),
                &params.last_string("comment"),
                false,
                expiry,
                false,
            ) else {
                return Ok(ActionResult::new(
                    404,
                    "Cannot add comment for non-existent object",
                ));
            };
            Ok(ActionResult::new(
                200,
                format!(
                    "Successfully added comment '{name}' for object '{}'.",
                    target.name
                ),
            )
            .with("name", Json::String(name))
            .with(
                "legacy_id",
                int(i64::try_from(legacy_id).unwrap_or(i64::MAX)),
            ))
        }
        "remove-comment" => {
            let author = params.last_string("author");
            if target.kind == ObjKind::Comment {
                world.remove_comment(&target.name, &author);
                Ok(ActionResult::new(
                    200,
                    format!("Successfully removed comment '{}'.", target.name),
                ))
            } else {
                for name in world.comments_of(&target.name) {
                    world.remove_comment(&name, &author);
                }
                Ok(ActionResult::new(
                    200,
                    format!(
                        "Successfully removed all comments for object '{}'.",
                        target.name
                    ),
                ))
            }
        }
        "schedule-downtime" => schedule_downtime(world, params, &target.name),
        "remove-downtime" => Ok(remove_downtime(world, params, target)),
        "execute-command" => execute_command(world, user, params, &target.name),
        other => Err(format!("unknown action '{other}'")),
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "mirrors Icinga's ApiActions::ProcessCheckResult step by step"
)]
fn process_check_result(
    world: &mut World,
    params: &Params,
    object: &str,
) -> Result<ActionResult, String> {
    let Some(checkable) = world.checkable(object) else {
        return Ok(ActionResult::new(
            404,
            "Cannot process passive check result for non-existent object.",
        ));
    };
    if !checkable.enable_passive_checks {
        return Ok(ActionResult::new(
            403,
            format!("Passive checks are disabled for object '{object}'."),
        ));
    }
    if !world.is_reachable(object, DepType::CheckExecution) {
        return Ok(ActionResult::new(
            200,
            format!("Ignoring passive check result for unreachable object '{object}'."),
        ));
    }
    if !params.contains("exit_status") {
        return Ok(ActionResult::new(
            400,
            "Parameter 'exit_status' is required.",
        ));
    }
    #[expect(
        clippy::cast_possible_truncation,
        reason = "Icinga converts the double to int the same way"
    )]
    let exit_status = params.last_f64("exit_status")? as i64;
    let is_service = checkable.is_service();
    let state = if is_service {
        match exit_status {
            0 => 0,
            1 => 1,
            2 => 2,
            _ => 3,
        }
    } else {
        match exit_status {
            0 => 0,
            1 => 2,
            _ => {
                return Ok(ActionResult::new(
                    400,
                    format!("Invalid 'exit_status' for Host {object}."),
                ));
            }
        }
    };
    if !params.contains("plugin_output") {
        return Ok(ActionResult::new(
            400,
            "Parameter 'plugin_output' is required",
        ));
    }
    let performance_data = match params.get("performance_data") {
        None | Some(Json::Null) => None,
        Some(Json::String(raw)) => Some(split_perfdata(raw)),
        Some(Json::Array(items)) => Some(items.iter().map(to_icinga_string).collect()),
        Some(other) => Some(vec![to_icinga_string(other)]),
    };
    let input = CheckInput {
        state,
        // Icinga doesn't copy exit_status into passive results.
        exit_status: 0,
        output: params.last_string("plugin_output"),
        performance_data,
        active: false,
        check_source: params.last_string("check_source"),
        command: params.get("check_command").cloned().unwrap_or(Json::Null),
        ttl: if params.contains("ttl") {
            params.last_f64("ttl")?
        } else {
            0.0
        },
        execution_start: if params.contains("execution_start") {
            params.last_f64("execution_start")?
        } else {
            0.0
        },
        execution_end: if params.contains("execution_end") {
            params.last_f64("execution_end")?
        } else {
            0.0
        },
        schedule_start: 0.0,
        schedule_end: 0.0,
    };
    Ok(match world.process_check_result(object, input) {
        Some(ProcessOutcome::Processed) => ActionResult::new(
            200,
            format!("Successfully processed check result for object '{object}'."),
        ),
        Some(ProcessOutcome::NewerCheckResultPresent) => ActionResult::new(
            409,
            format!(
                "Newer check result already present. Check result for '{object}' was discarded."
            ),
        ),
        None => ActionResult::new(
            503,
            format!(
                "Could not process check result for object '{object}' because the object is inactive."
            ),
        ),
    })
}

/// `PluginUtility::SplitPerfdata`: whitespace-separated, `'` quotes labels.
fn split_perfdata(raw: &str) -> Vec<String> {
    let mut entries = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    for c in raw.chars() {
        match c {
            '\'' => {
                in_quotes = !in_quotes;
                current.push(c);
            }
            c if c.is_whitespace() && !in_quotes => {
                if !current.is_empty() {
                    entries.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        entries.push(current);
    }
    entries
}

fn reschedule_check(
    world: &mut World,
    params: &Params,
    object: &str,
) -> Result<ActionResult, String> {
    let force = params.last_bool("force");
    let next_check = if params.contains("next_check") {
        params.last_f64("next_check")?
    } else {
        world.now()
    };
    let (host_checks, service_checks) = (
        world.app.enable_host_checks,
        world.app.enable_service_checks,
    );
    let Some(checkable) = world.checkable_mut(object) else {
        return Ok(ActionResult::new(
            404,
            "Cannot reschedule check for non-existent object.",
        ));
    };
    if force {
        checkable.force_next_check = true;
    }
    checkable.next_check = next_check;
    let globally = if checkable.is_service() {
        service_checks
    } else {
        host_checks
    };
    if force || (checkable.enable_active_checks && globally) {
        world.schedule_check(object, next_check);
    }
    Ok(ActionResult::new(
        200,
        format!("Successfully rescheduled check for object '{object}'."),
    ))
}

fn acknowledge(world: &mut World, params: &Params, object: &str) -> Result<ActionResult, String> {
    if !params.contains("author") || !params.contains("comment") {
        return Ok(ActionResult::new(
            400,
            "Acknowledgements require author and comment.",
        ));
    }
    let sticky = params.contains("sticky") && params.last_bool("sticky");
    let notify = params.contains("notify") && params.last_bool("notify");
    let persistent = params.contains("persistent") && params.last_bool("persistent");
    let expiry = if params.contains("expiry") {
        let expiry = params.last_f64("expiry")?;
        if expiry <= world.now() {
            return Ok(ActionResult::new(
                409,
                format!(
                    "Acknowledgement 'expiry' timestamp must be in the future for object {object}"
                ),
            ));
        }
        expiry
    } else {
        0.0
    };
    let Some(checkable) = world.checkable(object) else {
        return Ok(ActionResult::new(
            404,
            "Cannot acknowledge problem for non-existent object.",
        ));
    };
    let kind = if checkable.is_service() {
        if checkable.state_raw == 0 {
            return Ok(ActionResult::new(409, format!("Service {object} is OK.")));
        }
        "Service"
    } else {
        if checkable.state() == 0 {
            return Ok(ActionResult::new(409, format!("Host {object} is UP.")));
        }
        "Host"
    };
    // `IsAcknowledged()` clears an expired acknowledgement on the way.
    world.expire_acknowledgement(object);
    if world
        .checkable(object)
        .is_some_and(|c| c.acknowledgement != 0)
    {
        return Ok(ActionResult::new(
            409,
            format!("{kind} {object} is already acknowledged."),
        ));
    }
    world.acknowledge(
        object,
        &params.last_string("author"),
        &params.last_string("comment"),
        sticky,
        notify,
        persistent,
        expiry,
    );
    Ok(ActionResult::new(
        200,
        format!("Successfully acknowledged problem for object '{object}'."),
    ))
}

fn child_options(params: &Params) -> Result<u8, ()> {
    if !params.contains("child_options") {
        return Ok(0);
    }
    match params.last("child_options") {
        Some(Json::String(text)) => match text.as_str() {
            "DowntimeNoChildren" => Ok(0),
            "DowntimeTriggeredChildren" => Ok(1),
            "DowntimeNonTriggeredChildren" => Ok(2),
            _ => Err(()),
        },
        // Icinga converts numbers with `int(value)`, truncating fractions.
        Some(Json::Number(number)) => {
            let value = number.as_f64().map_or(-1.0, f64::trunc);
            [0.0_f64, 1.0, 2.0]
                .iter()
                .zip(0_u8..)
                .find(|(option, _)| (value - **option).abs() < f64::EPSILON)
                .map(|(_, option)| option)
                .ok_or(())
        }
        _ => Err(()),
    }
}

fn downtime_ref(name: String, legacy_id: u64) -> Map<String, Json> {
    let mut map = Map::new();
    map.insert("name".into(), Json::String(name));
    map.insert(
        "legacy_id".into(),
        int(i64::try_from(legacy_id).unwrap_or(i64::MAX)),
    );
    map
}

#[expect(
    clippy::too_many_lines,
    reason = "follows ApiActions::ScheduleDowntime step by step"
)]
fn schedule_downtime(
    world: &mut World,
    params: &Params,
    object: &str,
) -> Result<ActionResult, String> {
    if !params.contains("start_time")
        || !params.contains("end_time")
        || !params.contains("author")
        || !params.contains("comment")
    {
        return Ok(ActionResult::new(
            400,
            "Options 'start_time', 'end_time', 'author' and 'comment' are required",
        ));
    }
    let fixed = if params.contains("fixed") {
        params.last_bool("fixed")
    } else {
        true
    };
    if !fixed && !params.contains("duration") {
        return Ok(ActionResult::new(
            400,
            "Option 'duration' is required for flexible downtime",
        ));
    }
    let duration = if params.contains("duration") {
        params.last_f64("duration")?
    } else {
        0.0
    };
    let trigger_name = params.last_string("trigger_name");
    let trigger = if trigger_name.is_empty() {
        None
    } else if world.downtimes.contains_key(&trigger_name) {
        Some(trigger_name)
    } else {
        return Ok(ActionResult::new(
            404,
            "Won't schedule downtime with non-existent trigger downtime.",
        ));
    };
    let author = params.last_string("author");
    let comment = params.last_string("comment");
    let start_time = params.last_f64("start_time")?;
    let end_time = params.last_f64("end_time")?;
    let Ok(child_options) = child_options(params) else {
        return Ok(ActionResult::new(
            400,
            "Option 'child_options' provided an invalid value.",
        ));
    };
    if start_time <= 0.0 || end_time <= 0.0 {
        return Err("Could not create downtime.".to_owned());
    }
    let spec = |trigger: Option<String>, parent: String| DowntimeSpec {
        author: author.clone(),
        comment: comment.clone(),
        start_time,
        end_time,
        fixed,
        duration,
        trigger,
        scheduled_by: String::new(),
        parent,
        config_owner: String::new(),
    };
    let Some((name, legacy_id)) = world.add_downtime(object, spec(trigger.clone(), String::new()))
    else {
        return Ok(ActionResult::new(
            404,
            "Can't schedule downtime for non-existent object.",
        ));
    };
    let mut result = ActionResult::new(
        200,
        format!("Successfully scheduled downtime '{name}' for object '{object}'."),
    );
    result.extra = downtime_ref(name.clone(), legacy_id);
    let all_services = params.contains("all_services") && params.last_bool("all_services");
    let is_host = world.checkable(object).is_some_and(|c| !c.is_service());
    if all_services && is_host {
        let services: Vec<String> = world
            .services_of(object)
            .map(super::super::model::types::Checkable::full_name)
            .collect();
        let mut refs = Vec::new();
        for service in services {
            if let Some((service_name, id)) =
                world.add_downtime(&service, spec(trigger.clone(), name.clone()))
            {
                refs.push(Json::Object(downtime_ref(service_name, id)));
            }
        }
        result
            .extra
            .insert("service_downtimes".into(), Json::Array(refs));
    }
    if child_options != 0 {
        let child_trigger = if child_options == 1 {
            Some(name.clone())
        } else {
            trigger.clone()
        };
        let all_children = world.all_children_of(object);
        let mut children = Vec::new();
        for child in &all_children {
            let child_is_service = child.contains('!');
            if all_services && child_is_service {
                let host = child.split('!').next().unwrap_or_default();
                if all_children.contains(host) {
                    // Scheduled below together with its host.
                    continue;
                }
            }
            let Some((child_name, id)) =
                world.add_downtime(child, spec(child_trigger.clone(), name.clone()))
            else {
                continue;
            };
            let mut child_ref = downtime_ref(child_name.clone(), id);
            if all_services && !child_is_service {
                let services: Vec<String> = world
                    .services_of(child)
                    .map(super::super::model::types::Checkable::full_name)
                    .collect();
                let mut refs = Vec::new();
                for service in services {
                    if let Some((service_name, id)) = world
                        .add_downtime(&service, spec(child_trigger.clone(), child_name.clone()))
                    {
                        refs.push(Json::Object(downtime_ref(service_name, id)));
                    }
                }
                child_ref.insert("service_downtimes".into(), Json::Array(refs));
            }
            children.push(Json::Object(child_ref));
        }
        result
            .extra
            .insert("child_downtimes".into(), Json::Array(children));
    }
    Ok(result)
}

fn remove_downtime(world: &mut World, params: &Params, target: &ObjRef) -> ActionResult {
    let author = params.last_string("author");
    if target.kind == ObjKind::Downtime {
        let children = world.downtime_children(&target.name).len();
        return match world.remove_downtime(&target.name, true, RemovalReason::User, &author) {
            Ok(_) => ActionResult::new(
                200,
                format!(
                    "Successfully removed downtime '{}' and {children} child downtimes.",
                    target.name
                ),
            ),
            Err(message) => ActionResult::new(400, message),
        };
    }
    let mut children = 0;
    for name in world.downtimes_of(&target.name) {
        children += world.downtime_children(&name).len();
        if let Err(message) = world.remove_downtime(&name, true, RemovalReason::User, &author) {
            return ActionResult::new(400, message);
        }
    }
    ActionResult::new(
        200,
        format!(
            "Successfully removed all downtimes for object '{}' and {children} child downtimes.",
            target.name
        ),
    )
}

/// Resolves `$macro$` references like `MacroProcessor`: the `macros`
/// override first, then the service, then the host.
fn resolve_macros(
    world: &World,
    text: &str,
    object: &str,
    overrides: Option<&Map<String, Json>>,
) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find('$') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('$') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let name = &after[..end];
        out.push_str(&resolve_macro(world, name, object, overrides));
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// `MacroProcessor::ResolveMacro` for the `override`, `service` and `host`
/// resolvers: `host.<attr>` and `service.<attr>` address one object
/// (dotted paths walk into dictionaries such as `vars`); a bare name is
/// looked up as a custom variable, then as an attribute, on the service and
/// then on the host. The first object that has it wins, even when empty.
/// Unknown macros resolve to the empty string.
fn resolve_macro(
    world: &World,
    name: &str,
    object: &str,
    overrides: Option<&Map<String, Json>>,
) -> String {
    if let Some(value) = overrides.and_then(|o| o.get(name)) {
        return to_icinga_string(value);
    }
    let Some(checkable) = world.checkable(object) else {
        return String::new();
    };
    let host = ObjRef {
        kind: ObjKind::Host,
        name: checkable.host_name.clone(),
    };
    let service = checkable.is_service().then(|| ObjRef {
        kind: ObjKind::Service,
        name: object.to_owned(),
    });
    if let Some(path) = name.strip_prefix("host.") {
        return macro_value(world, &host, path).map_or_else(String::new, |v| to_icinga_string(&v));
    }
    if let Some(path) = name.strip_prefix("service.") {
        return service
            .and_then(|service| macro_value(world, &service, path))
            .map_or_else(String::new, |v| to_icinga_string(&v));
    }
    for target in service.iter().chain(std::iter::once(&host)) {
        let custom = world
            .attr(target, "vars")
            .ok()
            .flatten()
            .and_then(|vars| vars.get(name).cloned());
        if let Some(value) = custom.or_else(|| macro_value(world, target, name)) {
            return to_icinga_string(&value);
        }
    }
    String::new()
}

/// An attribute path (`address`, `vars.os`) of an object, if it exists.
fn macro_value(world: &World, target: &ObjRef, path: &str) -> Option<Json> {
    let mut parts = path.split('.');
    let mut value = world.attr(target, parts.next()?).ok().flatten()?;
    for part in parts {
        value = match value {
            Json::Object(mut map) => map.remove(part)?,
            _ => return None,
        };
    }
    Some(value)
}

/// `Zone::CanAccessObject`: the object's zone is the zone or a child of it.
fn zone_can_access(world: &World, zone: &str, object_zone: &str) -> bool {
    let local_zone = world
        .endpoints
        .get(&world.app.node_name)
        .map(|e| e.member_of.clone())
        .unwrap_or_default();
    let mut current = if object_zone.is_empty() {
        local_zone
    } else {
        object_zone.to_owned()
    };
    if world.zones.get(&current).is_some_and(|z| z.global) {
        return true;
    }
    for _ in 0..32 {
        if current == zone {
            return true;
        }
        match world.zones.get(&current).map(|z| z.parent.clone()) {
            Some(parent) if !parent.is_empty() => current = parent,
            _ => return false,
        }
    }
    false
}

#[expect(
    clippy::too_many_lines,
    reason = "mirrors Icinga's ApiActions::ExecuteCommand step by step"
)]
fn execute_command(
    world: &mut World,
    user: &Principal,
    params: &Params,
    object: &str,
) -> Result<ActionResult, String> {
    let command_type = if params.contains("command_type") {
        params.last_string("command_type")
    } else {
        "EventCommand".to_owned()
    };
    if !matches!(
        command_type.as_str(),
        "EventCommand" | "CheckCommand" | "NotificationCommand"
    ) {
        return Ok(ActionResult::new(
            400,
            format!("Invalid command_type '{command_type}'."),
        ));
    }
    if world.checkable(object).is_none() {
        return Ok(ActionResult::new(
            404,
            "Can't start a command execution for a non-existent object.",
        ));
    }
    if !params.contains("ttl") {
        return Ok(ActionResult::new(400, "Parameter ttl is required."));
    }
    let ttl = params.last_f64("ttl")?;
    if ttl <= 0.0 {
        return Ok(ActionResult::new(
            400,
            "Parameter ttl must be greater than 0.",
        ));
    }
    let endpoint = if params.contains("endpoint") {
        params.last_string("endpoint")
    } else {
        "$command_endpoint$".to_owned()
    };
    let overrides = if params.contains("macros") {
        match params.last("macros") {
            Some(Json::Object(map)) => Some(map.clone()),
            _ => {
                return Ok(ActionResult::new(
                    400,
                    "Parameter macros must be a dictionary.",
                ));
            }
        }
    } else {
        None
    };
    let resolved_endpoint = resolve_macros(world, &endpoint, object, overrides.as_ref());
    let endpoint_zone = match world.endpoints.get(&resolved_endpoint) {
        Some(e) if user.has_permission("objects/query/Endpoint") => e.member_of.clone(),
        _ => {
            return Ok(ActionResult::new(
                404,
                format!("Can't find a valid endpoint for '{resolved_endpoint}'."),
            ));
        }
    };
    let Some(checkable) = world.checkable(object) else {
        return Ok(ActionResult::new(
            404,
            "Can't start a command execution for a non-existent object.",
        ));
    };
    if checkable.command_endpoint != resolved_endpoint
        && !zone_can_access(world, &endpoint_zone, &checkable.meta.zone)
    {
        return Ok(ActionResult::new(
            409,
            format!("Zone '{endpoint_zone}' cannot access checkable '{object}'."),
        ));
    }
    let command = if params.contains("command") {
        params.last_string("command")
    } else {
        match command_type.as_str() {
            "CheckCommand" => "$check_command$",
            "EventCommand" => "$event_command$",
            _ => "$notification_command$",
        }
        .to_owned()
    };
    let resolved_command = resolve_macros(world, &command, object, overrides.as_ref());
    let exists = match command_type.as_str() {
        "CheckCommand" => world.check_commands.contains_key(&resolved_command),
        "EventCommand" => world.event_commands.contains_key(&resolved_command),
        _ => false,
    };
    if !exists || !user.has_permission(&format!("objects/query/{command_type}")) {
        return Ok(ActionResult::new(
            404,
            format!("Can't find a valid {command_type} for '{resolved_command}'."),
        ));
    }
    let id = world.names.uuid();
    let now = world.now();
    let mut execution = Map::new();
    execution.insert("pending".into(), Json::Bool(true));
    execution.insert("deadline".into(), num(now + ttl));
    execution.insert("endpoint".into(), Json::String(resolved_endpoint));
    if let Some(checkable) = world.checkable_mut(object) {
        checkable
            .executions
            .get_or_insert_with(Map::new)
            .insert(id.clone(), Json::Object(execution));
    }
    world.pending_executions.push(PendingExecution {
        due: now + 0.5,
        object: object.to_owned(),
        id: id.clone(),
        command: resolved_command,
        command_type,
    });
    Ok(ActionResult::new(202, "Accepted")
        .with("checkable", Json::String(object.to_owned()))
        .with("execution", Json::String(id)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_perfdata_strings() {
        assert_eq!(
            split_perfdata("rta=0.5ms;3000;5000 'disk /var'=97%;80;90  pl=0%"),
            vec!["rta=0.5ms;3000;5000", "'disk /var'=97%;80;90", "pl=0%"]
        );
        assert!(split_perfdata("   ").is_empty());
    }
}
