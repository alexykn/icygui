//! Request bodies for `/v1/actions/*`.
//!
//! Objects are always targeted by name (`hosts` / `services` arrays), never
//! by `filter`, which needs the `filter-expression` permission from Icinga
//! 2.17 on. An empty name list must never be sent: Icinga then falls back
//! to *every* object of the type.

use ic_model::{Action, ActionTarget, DowntimeMode, ObjectKey};
use serde_json::{Map, Value, json};

use crate::error::ApiError;

/// What one action request targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TargetKind {
    /// `"type": "Host", "hosts": [...]`.
    Host,
    /// `"type": "Service", "services": ["host!service", ...]`.
    Service,
    /// `"type": "Downtime", "downtime": name`.
    Downtime,
    /// `"type": "Comment", "comment": name`.
    Comment,
}

impl TargetKind {
    fn type_name(self) -> &'static str {
        match self {
            Self::Host => "Host",
            Self::Service => "Service",
            Self::Downtime => "Downtime",
            Self::Comment => "Comment",
        }
    }
}

/// One request to send: the endpoint, the target kind and the names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Batch {
    pub(crate) endpoint: &'static str,
    pub(crate) kind: TargetKind,
    pub(crate) names: Vec<String>,
}

/// Splits an action on a target into requests: hosts and services go
/// separately; a downtime target removes that downtime; a comment target
/// removes that comment. Duplicate names are sent once; empty batches are
/// dropped.
///
/// `ic_model::Action` has no "remove comment" variant, so a
/// [`ActionTarget::Comment`] (documented as "only for removing comments")
/// means `remove-comment` whatever the action. A [`ActionTarget::Downtime`]
/// is only valid with [`Action::RemoveAllDowntimes`].
pub(crate) fn plan(action: &Action, target: &ActionTarget) -> Result<Vec<Batch>, ApiError> {
    match target {
        ActionTarget::Objects(keys) => {
            let mut hosts: Vec<String> = Vec::new();
            let mut services: Vec<String> = Vec::new();
            for key in keys {
                let (list, name) = match key {
                    ObjectKey::Host { name } => (&mut hosts, name.to_string()),
                    ObjectKey::Service { key } => (&mut services, key.full_name()),
                };
                if !list.contains(&name) {
                    list.push(name);
                }
            }
            Ok([(TargetKind::Host, hosts), (TargetKind::Service, services)]
                .into_iter()
                .filter(|(_, names)| !names.is_empty())
                .map(|(kind, names)| Batch {
                    endpoint: action.api_name(),
                    kind,
                    names,
                })
                .collect())
        }
        ActionTarget::Downtime(name) => {
            if !matches!(action, Action::RemoveAllDowntimes) {
                return Err(ApiError::InvalidSettings(format!(
                    "\"{}\" can't target a downtime; only removing it can",
                    action.label()
                )));
            }
            Ok(single(TargetKind::Downtime, "remove-downtime", name))
        }
        ActionTarget::Comment(name) => Ok(single(TargetKind::Comment, "remove-comment", name)),
    }
}

fn single(kind: TargetKind, endpoint: &'static str, name: &str) -> Vec<Batch> {
    if name.is_empty() {
        return Vec::new();
    }
    vec![Batch {
        endpoint,
        kind,
        names: vec![name.to_owned()],
    }]
}

/// The JSON body for one batch (or a part of it, when retrying names).
pub(crate) fn body(action: &Action, kind: TargetKind, names: &[String], author: &str) -> Value {
    let mut body = Map::new();
    body.insert("type".to_owned(), json!(kind.type_name()));
    match kind {
        TargetKind::Host => {
            body.insert("hosts".to_owned(), json!(names));
        }
        TargetKind::Service => {
            body.insert("services".to_owned(), json!(names));
        }
        TargetKind::Downtime => {
            body.insert("downtime".to_owned(), json!(names.first()));
        }
        TargetKind::Comment => {
            body.insert("comment".to_owned(), json!(names.first()));
        }
    }
    match kind {
        TargetKind::Host | TargetKind::Service => parameters(action, author, &mut body),
        TargetKind::Downtime | TargetKind::Comment => {
            body.insert("author".to_owned(), json!(author));
        }
    }
    Value::Object(body)
}

/// The action's own parameters, named as in `12-icinga2-api.md` and
/// `apiactions.cpp`.
fn parameters(action: &Action, author: &str, body: &mut Map<String, Value>) {
    let mut set = |key: &str, value: Value| {
        body.insert(key.to_owned(), value);
    };
    match action {
        Action::CheckNow { force } => {
            set("force", json!(force));
            // No `next_check`: Icinga then uses its own current time, which
            // is "now" without the desktop's clock skew (a client clock a
            // few minutes ahead would otherwise delay the check).
        }
        Action::Acknowledge {
            comment,
            sticky,
            persistent,
            expiry,
        } => {
            set("author", json!(author));
            set("comment", json!(comment));
            set("sticky", json!(sticky));
            set("persistent", json!(persistent));
            // The client never asks Icinga to notify (configuration is
            // managed elsewhere; PLAN.md D6).
            set("notify", json!(false));
            if let Some(expiry) = expiry {
                set("expiry", json!(expiry.as_unix_seconds()));
            }
        }
        Action::RemoveAcknowledgement | Action::RemoveAllDowntimes => {
            set("author", json!(author));
        }
        Action::ScheduleDowntime {
            comment,
            start,
            end,
            mode,
            all_services,
            child_options,
            trigger_name,
        } => {
            set("author", json!(author));
            set("comment", json!(comment));
            set("start_time", json!(start.as_unix_seconds()));
            set("end_time", json!(end.as_unix_seconds()));
            let (fixed, duration) = match mode {
                DowntimeMode::Fixed => (true, 0.0),
                DowntimeMode::Flexible { duration } => (false, *duration),
            };
            set("fixed", json!(fixed));
            set("duration", json!(duration));
            set("all_services", json!(all_services));
            set("child_options", json!(child_options.api_value()));
            if let Some(trigger) = trigger_name.as_deref().filter(|name| !name.is_empty()) {
                set("trigger_name", json!(trigger));
            }
        }
        Action::AddComment { text, expiry } => {
            set("author", json!(author));
            set("comment", json!(text));
            if let Some(expiry) = expiry {
                set("expiry", json!(expiry.as_unix_seconds()));
            }
        }
        Action::ProcessCheckResult {
            exit_status,
            output,
            perfdata,
            ttl,
        } => {
            set("exit_status", json!(exit_status));
            set("plugin_output", json!(output));
            set("performance_data", json!(perfdata));
            if let Some(ttl) = ttl {
                set("ttl", json!(ttl));
            }
        }
        Action::ExecuteCommand {
            command_type,
            command,
            endpoint,
            macros,
            ttl,
        } => {
            set("command_type", json!(command_type.api_value()));
            if let Some(command) = command.as_deref().filter(|name| !name.is_empty()) {
                set("command", json!(command));
            }
            if let Some(endpoint) = endpoint.as_deref().filter(|name| !name.is_empty()) {
                set("endpoint", json!(endpoint));
            }
            set("macros", Value::Object(macros.clone()));
            set("ttl", json!(ttl));
        }
    }
}

#[cfg(test)]
mod tests {
    use ic_model::{ChildOptions, CommandType, Timestamp, Vars};

    use super::*;

    fn service_body(action: &Action) -> Value {
        body(
            action,
            TargetKind::Service,
            &["db-prod-03!load".to_owned()],
            "j.berg",
        )
    }

    #[test]
    fn check_now_forces_without_a_client_timestamp() {
        assert_eq!(
            service_body(&Action::CheckNow { force: true }),
            json!({
                "type": "Service",
                "services": ["db-prod-03!load"],
                "force": true,
            })
        );
    }

    #[test]
    fn acknowledge_never_notifies() {
        let action = Action::Acknowledge {
            comment: "looking into it".to_owned(),
            sticky: true,
            persistent: false,
            expiry: Some(Timestamp::from_unix_seconds(1_791_206_774.0)),
        };
        assert_eq!(
            body(
                &action,
                TargetKind::Host,
                &["k8s-node-11".to_owned()],
                "j.berg"
            ),
            json!({
                "type": "Host",
                "hosts": ["k8s-node-11"],
                "author": "j.berg",
                "comment": "looking into it",
                "sticky": true,
                "persistent": false,
                "notify": false,
                "expiry": 1_791_206_774.0,
            })
        );
        let without_expiry = Action::Acknowledge {
            comment: String::new(),
            sticky: false,
            persistent: true,
            expiry: None,
        };
        let value = service_body(&without_expiry);
        assert!(value.get("expiry").is_none());
        assert_eq!(value["notify"], json!(false));
        assert_eq!(
            value["comment"],
            json!(""),
            "comment is required, may be empty"
        );
    }

    #[test]
    fn remove_acknowledgement_sends_the_author() {
        assert_eq!(
            service_body(&Action::RemoveAcknowledgement),
            json!({ "type": "Service", "services": ["db-prod-03!load"], "author": "j.berg" })
        );
    }

    #[test]
    fn fixed_downtime() {
        let action = Action::ScheduleDowntime {
            comment: "rack maintenance".to_owned(),
            start: Timestamp::from_unix_seconds(1_791_203_174.0),
            end: Timestamp::from_unix_seconds(1_791_210_374.0),
            mode: DowntimeMode::Fixed,
            all_services: true,
            child_options: ChildOptions::Triggered,
            trigger_name: None,
        };
        assert_eq!(
            body(
                &action,
                TargetKind::Host,
                &["k8s-node-11".to_owned()],
                "a.ivanova"
            ),
            json!({
                "type": "Host",
                "hosts": ["k8s-node-11"],
                "author": "a.ivanova",
                "comment": "rack maintenance",
                "start_time": 1_791_203_174.0,
                "end_time": 1_791_210_374.0,
                "fixed": true,
                "duration": 0.0,
                "all_services": true,
                "child_options": "DowntimeTriggeredChildren",
            })
        );
    }

    #[test]
    fn flexible_downtime_with_trigger() {
        let action = Action::ScheduleDowntime {
            comment: "deploy".to_owned(),
            start: Timestamp::from_unix_seconds(100.0),
            end: Timestamp::from_unix_seconds(7300.0),
            mode: DowntimeMode::Flexible { duration: 1800.0 },
            all_services: false,
            child_options: ChildOptions::None,
            trigger_name: Some("k8s-node-11!e96da238".to_owned()),
        };
        let value = service_body(&action);
        assert_eq!(value["fixed"], json!(false));
        assert_eq!(value["duration"], json!(1800.0));
        assert_eq!(value["child_options"], json!("DowntimeNoChildren"));
        assert_eq!(value["trigger_name"], json!("k8s-node-11!e96da238"));
        assert_eq!(value["all_services"], json!(false));
    }

    #[test]
    fn remove_all_downtimes_of_objects() {
        assert_eq!(
            service_body(&Action::RemoveAllDowntimes),
            json!({ "type": "Service", "services": ["db-prod-03!load"], "author": "j.berg" })
        );
    }

    #[test]
    fn add_comment() {
        let action = Action::AddComment {
            text: "Failover drill at 15:00".to_owned(),
            expiry: None,
        };
        assert_eq!(
            body(
                &action,
                TargetKind::Host,
                &["db-prod-03".to_owned()],
                "j.berg"
            ),
            json!({
                "type": "Host",
                "hosts": ["db-prod-03"],
                "author": "j.berg",
                "comment": "Failover drill at 15:00",
            })
        );
        let expiring = Action::AddComment {
            text: "x".to_owned(),
            expiry: Some(Timestamp::from_unix_seconds(5.0)),
        };
        assert_eq!(service_body(&expiring)["expiry"], json!(5.0));
    }

    #[test]
    fn process_check_result() {
        let action = Action::ProcessCheckResult {
            exit_status: 2,
            output: "DISK CRITICAL - /var 97% used\nsecond line".to_owned(),
            perfdata: vec!["/var=97%;80;90;0;100".to_owned()],
            ttl: Some(300.0),
        };
        assert_eq!(
            service_body(&action),
            json!({
                "type": "Service",
                "services": ["db-prod-03!load"],
                "exit_status": 2,
                "plugin_output": "DISK CRITICAL - /var 97% used\nsecond line",
                "performance_data": ["/var=97%;80;90;0;100"],
                "ttl": 300.0,
            })
        );
        let no_ttl = Action::ProcessCheckResult {
            exit_status: 0,
            output: "OK".to_owned(),
            perfdata: Vec::new(),
            ttl: None,
        };
        assert!(service_body(&no_ttl).get("ttl").is_none());
    }

    #[test]
    fn execute_command() {
        let mut macros = Vars::new();
        macros.insert("ls_dir".to_owned(), json!("/tmp"));
        let action = Action::ExecuteCommand {
            command_type: CommandType::CheckCommand,
            command: Some("custom_command".to_owned()),
            endpoint: Some("icinga-master".to_owned()),
            macros,
            ttl: 15.0,
        };
        assert_eq!(
            service_body(&action),
            json!({
                "type": "Service",
                "services": ["db-prod-03!load"],
                "command_type": "CheckCommand",
                "command": "custom_command",
                "endpoint": "icinga-master",
                "macros": { "ls_dir": "/tmp" },
                "ttl": 15.0,
            })
        );
        let defaults = Action::ExecuteCommand {
            command_type: CommandType::EventCommand,
            command: None,
            endpoint: None,
            macros: Vars::new(),
            ttl: 60.0,
        };
        let value = service_body(&defaults);
        assert!(value.get("command").is_none());
        assert!(value.get("endpoint").is_none());
        assert_eq!(value["command_type"], json!("EventCommand"));
        assert_eq!(value["macros"], json!({}));
    }

    #[test]
    fn downtime_and_comment_targets() {
        let downtime = body(
            &Action::RemoveAllDowntimes,
            TargetKind::Downtime,
            &["k8s-node-11!e96da238".to_owned()],
            "j.berg",
        );
        assert_eq!(
            downtime,
            json!({ "type": "Downtime", "downtime": "k8s-node-11!e96da238", "author": "j.berg" })
        );
        let comment = body(
            &Action::RemoveAllDowntimes,
            TargetKind::Comment,
            &["db-prod-03!dc3b4066".to_owned()],
            "j.berg",
        );
        assert_eq!(
            comment,
            json!({ "type": "Comment", "comment": "db-prod-03!dc3b4066", "author": "j.berg" })
        );
    }

    #[test]
    fn plans_split_hosts_and_services_and_drop_duplicates() {
        let target = ActionTarget::Objects(vec![
            ObjectKey::service("db-prod-03", "load"),
            ObjectKey::host("k8s-node-11"),
            ObjectKey::service("db-prod-03", "load"),
            ObjectKey::service("web", "http!8080"),
        ]);
        let batches = plan(&Action::CheckNow { force: true }, &target).unwrap();
        assert_eq!(
            batches,
            vec![
                Batch {
                    endpoint: "reschedule-check",
                    kind: TargetKind::Host,
                    names: vec!["k8s-node-11".to_owned()],
                },
                Batch {
                    endpoint: "reschedule-check",
                    kind: TargetKind::Service,
                    names: vec!["db-prod-03!load".to_owned(), "web!http!8080".to_owned()],
                },
            ]
        );
    }

    #[test]
    fn never_plans_an_empty_name_list() {
        let empty = ActionTarget::Objects(Vec::new());
        assert!(
            plan(&Action::RemoveAcknowledgement, &empty)
                .unwrap()
                .is_empty()
        );
        let only_hosts = ActionTarget::Objects(vec![ObjectKey::host("a")]);
        let batches = plan(&Action::RemoveAcknowledgement, &only_hosts).unwrap();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].kind, TargetKind::Host);
        assert!(
            plan(
                &Action::RemoveAllDowntimes,
                &ActionTarget::Downtime(String::new())
            )
            .unwrap()
            .is_empty()
        );
    }

    #[test]
    fn downtime_targets_only_remove() {
        let target = ActionTarget::Downtime("h!x".to_owned());
        let batches = plan(&Action::RemoveAllDowntimes, &target).unwrap();
        assert_eq!(batches[0].endpoint, "remove-downtime");
        assert_eq!(batches[0].kind, TargetKind::Downtime);
        assert!(matches!(
            plan(&Action::CheckNow { force: true }, &target),
            Err(ApiError::InvalidSettings(_))
        ));
    }

    #[test]
    fn comment_targets_remove_the_comment() {
        let target = ActionTarget::Comment("h!c".to_owned());
        let batches = plan(&Action::RemoveAcknowledgement, &target).unwrap();
        assert_eq!(
            batches,
            vec![Batch {
                endpoint: "remove-comment",
                kind: TargetKind::Comment,
                names: vec!["h!c".to_owned()],
            }]
        );
    }
}
