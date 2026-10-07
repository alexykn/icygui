//! What the API user may do (ENV-09): actions it may not run are disabled
//! with an explanation, and lists it may not read say so instead of
//! looking empty. Icinga 2.17's least-privilege users lack
//! `filter-expression`, which the client never needs.

use ic_config::ObjectKind;
use ic_core::ApiInfo;

use crate::actions::ObjectAction;
use crate::lists::ListKind;

/// The permission an action needs (`GET /v1` lists the user's).
pub(crate) fn action_permission(action: &ObjectAction) -> &'static str {
    match action {
        ObjectAction::Acknowledge => "actions/acknowledge-problem",
        ObjectAction::RemoveAcknowledgement => "actions/remove-acknowledgement",
        ObjectAction::ScheduleDowntime => "actions/schedule-downtime",
        ObjectAction::RemoveDowntimes
        | ObjectAction::RemoveDowntime(_)
        | ObjectAction::RemoveNamedDowntimes(_) => "actions/remove-downtime",
        ObjectAction::CheckNow => "actions/reschedule-check",
        ObjectAction::AddComment => "actions/add-comment",
        ObjectAction::RemoveComments(_) => "actions/remove-comment",
        ObjectAction::SubmitCheckResult => "actions/process-check-result",
        ObjectAction::RunCommand => "actions/execute-command",
    }
}

/// What an action does, for "may not …".
fn action_verb(action: &ObjectAction) -> &'static str {
    match action {
        ObjectAction::Acknowledge => "acknowledge problems",
        ObjectAction::RemoveAcknowledgement => "remove acknowledgements",
        ObjectAction::ScheduleDowntime => "schedule downtimes",
        ObjectAction::RemoveDowntimes
        | ObjectAction::RemoveDowntime(_)
        | ObjectAction::RemoveNamedDowntimes(_) => "remove downtimes",
        ObjectAction::CheckNow => "reschedule checks",
        ObjectAction::AddComment => "add comments",
        ObjectAction::RemoveComments(_) => "remove comments",
        ObjectAction::SubmitCheckResult => "submit check results",
        ObjectAction::RunCommand => "run commands",
    }
}

/// Why the user may not run `action`, or `None` if it may (or nothing is
/// known yet: Icinga then answers 403 and the action reports it).
pub(crate) fn action_denial(info: Option<&ApiInfo>, action: &ObjectAction) -> Option<String> {
    let info = info?;
    let permission = action_permission(action);
    (!info.allows(permission)).then(|| {
        format!(
            "The API user {} may not {} (needs {permission})",
            info.user,
            action_verb(action)
        )
    })
}

/// Why the user may not read objects of `kind`, or `None` if it may.
pub(crate) fn query_denial(info: Option<&ApiInfo>, kind: ObjectKind) -> Option<String> {
    let info = info?;
    let (permission, what) = match kind {
        ObjectKind::Services => ("objects/query/Service", "services"),
        ObjectKind::Hosts => ("objects/query/Host", "hosts"),
    };
    (!info.allows(permission)).then(|| {
        format!(
            "The API user {} may not read {what} (needs {permission}).",
            info.user
        )
    })
}

/// Why the user may not read what list `kind` shows (downtimes, comments),
/// or `None` if it may. The acknowledged list reads hosts and services,
/// which the dashboards already need.
pub(crate) fn list_denial(info: Option<&ApiInfo>, kind: ListKind) -> Option<String> {
    let info = info?;
    let permission = match kind {
        ListKind::Downtimes => "objects/query/Downtime",
        ListKind::Comments => "objects/query/Comment",
        ListKind::Acknowledged => return None,
    };
    (!info.allows(permission)).then(|| {
        format!(
            "The API user {} may not read {} (needs {permission}).",
            info.user,
            kind.title()
        )
    })
}

/// Why the acknowledged list can't say who acknowledged a problem and why
/// (that is the acknowledgement's comment), or `None` if it can: without
/// `objects/query/Comment` there are no comments, so *only mine* can't tell
/// whose an acknowledgement is either.
pub(crate) fn ack_detail_denial(info: Option<&ApiInfo>) -> Option<String> {
    let info = info?;
    (!info.allows("objects/query/Comment")).then(|| {
        format!(
            "The API user {} may not read comments (needs objects/query/Comment): who \
             acknowledged and why aren't known, so only mine can't tell whose they are.",
            info.user
        )
    })
}

/// Whether the panes can say who Icinga notified (PANE-06): `Some(false)`
/// when the user may not read Icinga's `Notification` objects.
pub(crate) fn can_read_notifications(info: Option<&ApiInfo>) -> Option<bool> {
    info.map(|info| info.allows("objects/query/Notification"))
}

/// Whether `permission` is one the user guide's `ApiUser` leaves out on
/// purpose: run command's `actions/execute-command`, which with free-form
/// macros runs any command on the agents. Lacking it isn't a problem to
/// flag (*run command* still says what it needs).
pub(crate) fn is_opt_in(permission: &str) -> bool {
    permission == action_permission(&ObjectAction::RunCommand)
}

/// How many of the client's permissions (`ic_core::REQUIRED_PERMISSIONS`)
/// the user lacks, the opt-in ones not counted.
pub(crate) fn missing_needed(missing: &[String]) -> usize {
    missing
        .iter()
        .filter(|permission| !is_opt_in(permission))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(permissions: &[&str]) -> ApiInfo {
        ApiInfo {
            user: "viewer".to_owned(),
            permissions: permissions.iter().map(|p| (*p).to_owned()).collect(),
            version: "v2.15.6".to_owned(),
        }
    }

    #[test]
    fn every_action_names_its_permission() {
        let actions = [
            ObjectAction::Acknowledge,
            ObjectAction::RemoveAcknowledgement,
            ObjectAction::ScheduleDowntime,
            ObjectAction::CheckNow,
            ObjectAction::AddComment,
            ObjectAction::RemoveComments(vec!["c".to_owned()]),
            ObjectAction::RemoveDowntime("d".to_owned()),
            ObjectAction::RemoveNamedDowntimes(vec!["d".to_owned()]),
            ObjectAction::RemoveDowntimes,
            ObjectAction::SubmitCheckResult,
            ObjectAction::RunCommand,
        ];
        for action in &actions {
            let permission = action_permission(action);
            assert!(permission.starts_with("actions/"), "{permission}");
            assert!(
                ic_core::REQUIRED_PERMISSIONS.contains(&permission),
                "{permission} is one the client asks for"
            );
        }
    }

    /// The user guide's `ApiUser` (everything but run command) lacks
    /// nothing the client needs.
    #[test]
    fn only_run_commands_permission_is_opt_in() {
        let everything_but_run_command: Vec<&str> = ic_core::REQUIRED_PERMISSIONS
            .iter()
            .copied()
            .filter(|permission| *permission != "actions/execute-command")
            .collect();
        let guide = user(&everything_but_run_command);
        let missing = ic_core::missing_permissions(&guide);
        assert_eq!(missing, ["actions/execute-command"]);
        assert_eq!(missing_needed(&missing), 0);
        assert!(action_denial(Some(&guide), &ObjectAction::RunCommand).is_some());

        let viewer = user(&["objects/query/*", "status/query", "events/*"]);
        let missing = ic_core::missing_permissions(&viewer);
        assert_eq!(missing_needed(&missing), missing.len() - 1);
        assert!(!is_opt_in("actions/acknowledge-problem"));
    }

    #[test]
    fn denials_explain_the_missing_permission() {
        let viewer = user(&["objects/query/*", "status/query", "events/*"]);
        assert_eq!(
            action_denial(Some(&viewer), &ObjectAction::Acknowledge).as_deref(),
            Some(
                "The API user viewer may not acknowledge problems \
                 (needs actions/acknowledge-problem)"
            )
        );
        let operator = user(&["actions/*", "objects/query/*"]);
        assert_eq!(
            action_denial(Some(&operator), &ObjectAction::CheckNow),
            None
        );
        let checker = user(&["actions/reschedule-check (filtered)"]);
        assert_eq!(
            action_denial(Some(&checker), &ObjectAction::CheckNow),
            None,
            "filtered permissions count as allowed"
        );
        assert!(action_denial(Some(&checker), &ObjectAction::AddComment).is_some());
        assert_eq!(
            action_denial(None, &ObjectAction::Acknowledge),
            None,
            "unknown before the first connect"
        );
    }

    #[test]
    fn query_denials_name_the_object_type() {
        let hosts_only = user(&["objects/query/Host"]);
        assert_eq!(query_denial(Some(&hosts_only), ObjectKind::Hosts), None);
        assert_eq!(
            query_denial(Some(&hosts_only), ObjectKind::Services).as_deref(),
            Some("The API user viewer may not read services (needs objects/query/Service).")
        );
        assert_eq!(query_denial(None, ObjectKind::Services), None);
        assert_eq!(can_read_notifications(Some(&hosts_only)), Some(false));
        assert_eq!(can_read_notifications(Some(&user(&["*"]))), Some(true));
        assert_eq!(can_read_notifications(None), None);
    }
}
