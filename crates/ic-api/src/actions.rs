//! Request bodies for `/v1/actions/*`.
//!
//! Objects are always targeted by name (`hosts` / `services` arrays), never
//! by `filter`, which needs the `filter-expression` permission from Icinga
//! 2.17 on. An empty name list must never be sent: Icinga then falls back
//! to *every* object of the type.

use std::collections::HashSet;

use ic_model::{Action, ActionTarget, ChildOptions, DowntimeMode, ObjectKey};
use serde_json::{Map, Value, json};

use crate::client::NAMES_PER_REQUEST;
use crate::error::ApiError;

/// Names per `schedule-downtime` request with `all_services` or child
/// options. Icinga creates a downtime for each of the host's services and
/// each child too (every one a configuration object written to disk) and
/// answers only when all are done, so a request of 200 hosts could take
/// minutes; this many keep each answer quick.
pub(crate) const DOWNTIME_TREES_PER_REQUEST: usize = 20;

/// How many names one request of `action` carries.
pub(crate) fn names_per_request(action: &Action) -> usize {
    match action {
        Action::ScheduleDowntime {
            all_services,
            child_options,
            ..
        } if *all_services || *child_options != ChildOptions::None => DOWNTIME_TREES_PER_REQUEST,
        _ => NAMES_PER_REQUEST,
    }
}

/// What one action request targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TargetKind {
    /// `"type": "Host", "hosts": [...]`.
    Host,
    /// `"type": "Service", "services": ["host!service", ...]`.
    Service,
    /// `"type": "Downtime", "downtimes": [names]`.
    Downtime,
    /// `"type": "Comment", "comments": [names]`.
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

impl Batch {
    /// Splits the batch into requests of at most `size` names, in order.
    pub(crate) fn chunks(self, size: usize) -> Vec<Self> {
        self.names
            .chunks(size.max(1))
            .map(|names| Self {
                endpoint: self.endpoint,
                kind: self.kind,
                names: names.to_vec(),
            })
            .collect()
    }
}

/// Splits an action on a target into requests: hosts and services go
/// separately; downtime targets remove those downtimes and comment targets
/// those comments, by name (`"downtimes": [...]`, `"comments": [...]`,
/// which Icinga resolves like `hosts`). Duplicate names are sent once;
/// empty batches are dropped (an empty name list would make Icinga fall
/// back to every object of the type).
///
/// [`ActionTarget::Downtimes`] and [`ActionTarget::Comments`] can only be
/// removed, so they are only valid with [`Action::RemoveAllDowntimes`]
/// (`ic_model::Action` has no "remove comment" variant; that action stands
/// for removing the targeted downtimes or comments). Anything else is a
/// caller bug and fails with [`ApiError::InvalidSettings`] instead of
/// silently deleting something.
pub(crate) fn plan(action: &Action, target: &ActionTarget) -> Result<Vec<Batch>, ApiError> {
    match target {
        ActionTarget::Objects(keys) => {
            let (hosts, services) = names_by_kind(keys);
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
        ActionTarget::Downtimes(names) => {
            only_removal(action, "downtime")?;
            Ok(named(TargetKind::Downtime, "remove-downtime", names))
        }
        ActionTarget::Comments(names) => {
            only_removal(action, "comment")?;
            Ok(named(TargetKind::Comment, "remove-comment", names))
        }
    }
}

fn only_removal(action: &Action, what: &str) -> Result<(), ApiError> {
    if matches!(action, Action::RemoveAllDowntimes) {
        return Ok(());
    }
    Err(ApiError::InvalidSettings(format!(
        "\"{}\" can't target a {what}; only removing it can (Action::RemoveAllDowntimes)",
        action.label()
    )))
}

/// The full names of `keys`, hosts and services apart, each once, in the
/// order they first appear. (Linear: a bulk action on a whole dashboard
/// can carry tens of thousands of keys.)
pub(crate) fn names_by_kind(keys: &[ObjectKey]) -> (Vec<String>, Vec<String>) {
    #[derive(Default)]
    struct Unique {
        names: Vec<String>,
        seen: HashSet<String>,
    }
    impl Unique {
        fn push(&mut self, name: String) {
            if !self.seen.contains(&name) {
                self.seen.insert(name.clone());
                self.names.push(name);
            }
        }
    }
    let mut hosts = Unique::default();
    let mut services = Unique::default();
    for key in keys {
        match key {
            ObjectKey::Host { name } => hosts.push(name.to_string()),
            ObjectKey::Service { key } => services.push(key.full_name()),
        }
    }
    (hosts.names, services.names)
}

/// One batch of `names` (each once, blank ones left out), or none.
fn named(kind: TargetKind, endpoint: &'static str, names: &[String]) -> Vec<Batch> {
    let mut seen = HashSet::new();
    let names: Vec<String> = names
        .iter()
        .filter(|name| !name.is_empty() && seen.insert(name.as_str()))
        .cloned()
        .collect();
    if names.is_empty() {
        return Vec::new();
    }
    vec![Batch {
        endpoint,
        kind,
        names,
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
            body.insert("downtimes".to_owned(), json!(names));
        }
        TargetKind::Comment => {
            body.insert("comments".to_owned(), json!(names));
        }
    }
    match kind {
        TargetKind::Host | TargetKind::Service => parameters(action, kind, author, &mut body),
        TargetKind::Downtime | TargetKind::Comment => {
            body.insert("author".to_owned(), json!(author));
        }
    }
    Value::Object(body)
}

/// The `exit_status` Icinga's API takes for a host: 0 (UP) or 1 (DOWN);
/// it answers anything else with a per-object 400 (`apiactions.cpp`).
/// [`Action::ProcessCheckResult`] carries a plugin exit status (0–3), so
/// map it the way Icinga maps a host check plugin's exit status
/// (`Host::CalculateState`): OK and WARNING are UP, CRITICAL, UNKNOWN and
/// anything higher are DOWN.
pub(crate) fn host_exit_status(plugin_exit_status: u8) -> u8 {
    u8::from(plugin_exit_status >= 2)
}

/// The action's own parameters, named as in `12-icinga2-api.md` and
/// `apiactions.cpp`.
fn parameters(action: &Action, kind: TargetKind, author: &str, body: &mut Map<String, Value>) {
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
            let exit_status = match kind {
                TargetKind::Host => host_exit_status(*exit_status),
                TargetKind::Service | TargetKind::Downtime | TargetKind::Comment => *exit_status,
            };
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
    fn host_check_results_map_plugin_exit_statuses_to_up_and_down() {
        let host_body = |exit_status: u8| {
            let action = Action::ProcessCheckResult {
                exit_status,
                output: "PING".to_owned(),
                perfdata: Vec::new(),
                ttl: None,
            };
            body(&action, TargetKind::Host, &["k8s-node-11".to_owned()], "me")["exit_status"]
                .clone()
        };
        // Icinga's API takes only 0 (UP) and 1 (DOWN) for hosts; a plugin's
        // OK and WARNING mean UP, CRITICAL and UNKNOWN mean DOWN.
        assert_eq!(host_body(0), json!(0));
        assert_eq!(host_body(1), json!(0), "WARNING is UP for a host");
        assert_eq!(host_body(2), json!(1));
        assert_eq!(host_body(3), json!(1));
        assert_eq!(host_body(7), json!(1), "out of range is UNKNOWN: DOWN");
        // Services keep the plugin's exit status.
        for exit_status in 0..=3 {
            let action = Action::ProcessCheckResult {
                exit_status,
                output: String::new(),
                perfdata: Vec::new(),
                ttl: None,
            };
            assert_eq!(service_body(&action)["exit_status"], json!(exit_status));
        }
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
    fn downtime_and_comment_targets_go_by_name_lists() {
        let downtime = body(
            &Action::RemoveAllDowntimes,
            TargetKind::Downtime,
            &[
                "k8s-node-11!e96da238".to_owned(),
                "k8s-node-11!disk /!0c1f".to_owned(),
            ],
            "j.berg",
        );
        assert_eq!(
            downtime,
            json!({
                "type": "Downtime",
                "downtimes": ["k8s-node-11!e96da238", "k8s-node-11!disk /!0c1f"],
                "author": "j.berg"
            })
        );
        let comment = body(
            &Action::RemoveAllDowntimes,
            TargetKind::Comment,
            &["db-prod-03!dc3b4066".to_owned()],
            "j.berg",
        );
        assert_eq!(
            comment,
            json!({ "type": "Comment", "comments": ["db-prod-03!dc3b4066"], "author": "j.berg" })
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
        for target in [
            ActionTarget::Downtimes(Vec::new()),
            ActionTarget::Downtimes(vec![String::new()]),
            ActionTarget::Comments(Vec::new()),
            ActionTarget::Comments(vec![String::new(), String::new()]),
        ] {
            assert!(
                plan(&Action::RemoveAllDowntimes, &target)
                    .unwrap()
                    .is_empty(),
                "{target:?}"
            );
        }
    }

    #[test]
    fn downtime_targets_only_remove() {
        let target =
            ActionTarget::Downtimes(vec!["h!x".to_owned(), "h!y".to_owned(), "h!x".to_owned()]);
        let batches = plan(&Action::RemoveAllDowntimes, &target).unwrap();
        assert_eq!(batches.len(), 1, "one request for every name");
        assert_eq!(batches[0].endpoint, "remove-downtime");
        assert_eq!(batches[0].kind, TargetKind::Downtime);
        assert_eq!(batches[0].names, ["h!x", "h!y"], "each name once");
        assert!(matches!(
            plan(&Action::CheckNow { force: true }, &target),
            Err(ApiError::InvalidSettings(_))
        ));
    }

    #[test]
    fn comment_targets_only_remove_the_comment() {
        let target = ActionTarget::Comments(vec!["h!c".to_owned()]);
        let batches = plan(&Action::RemoveAllDowntimes, &target).unwrap();
        assert_eq!(
            batches,
            vec![Batch {
                endpoint: "remove-comment",
                kind: TargetKind::Comment,
                names: vec!["h!c".to_owned()],
            }]
        );
        // Any other action on a comment is a caller bug, never a silent
        // removal.
        for action in [
            Action::RemoveAcknowledgement,
            Action::CheckNow { force: true },
            Action::AddComment {
                text: "reply".to_owned(),
                expiry: None,
            },
        ] {
            assert!(
                matches!(plan(&action, &target), Err(ApiError::InvalidSettings(message)) if message.contains("comment")),
                "{action:?}"
            );
        }
    }

    #[test]
    fn names_are_unique_per_kind_in_first_seen_order() {
        let keys = vec![
            ObjectKey::service("b", "x"),
            ObjectKey::host("b"),
            ObjectKey::host("a"),
            ObjectKey::service("a", "y"),
            ObjectKey::host("b"),
            ObjectKey::service("b", "x"),
            ObjectKey::host("c"),
        ];
        let (hosts, services) = names_by_kind(&keys);
        assert_eq!(hosts, ["b", "a", "c"]);
        assert_eq!(services, ["b!x", "a!y"]);

        let many: Vec<ObjectKey> = (0..40_000)
            .map(|i| ObjectKey::service(&format!("h{}", i % 2_000), &format!("s{i}")))
            .chain((0..40_000).map(|i| ObjectKey::host(&format!("h{}", i % 2_000))))
            .collect();
        let (hosts, services) = names_by_kind(&many);
        assert_eq!(hosts.len(), 2_000);
        assert_eq!(services.len(), 40_000);
    }

    #[test]
    fn downtimes_for_whole_trees_go_out_in_smaller_requests() {
        let downtime = |all_services, child_options| Action::ScheduleDowntime {
            comment: "rack".to_owned(),
            start: Timestamp::from_unix_seconds(100.0),
            end: Timestamp::from_unix_seconds(200.0),
            mode: DowntimeMode::Fixed,
            all_services,
            child_options,
            trigger_name: None,
        };
        assert_eq!(
            names_per_request(&downtime(false, ChildOptions::None)),
            NAMES_PER_REQUEST
        );
        for (all_services, child_options) in [
            (true, ChildOptions::None),
            (false, ChildOptions::Triggered),
            (false, ChildOptions::NonTriggered),
        ] {
            assert_eq!(
                names_per_request(&downtime(all_services, child_options)),
                DOWNTIME_TREES_PER_REQUEST
            );
        }
        assert_eq!(
            names_per_request(&Action::CheckNow { force: true }),
            NAMES_PER_REQUEST
        );
    }

    #[test]
    fn batches_split_into_ordered_chunks() {
        let batch = Batch {
            endpoint: "reschedule-check",
            kind: TargetKind::Host,
            names: (0..5).map(|i| format!("h{i}")).collect(),
        };
        let chunks = batch.chunks(2);
        let sizes: Vec<usize> = chunks.iter().map(|chunk| chunk.names.len()).collect();
        assert_eq!(sizes, [2, 2, 1]);
        assert_eq!(chunks[2].names, ["h4"]);
        assert!(chunks.iter().all(|chunk| chunk.kind == TargetKind::Host));
    }
}
