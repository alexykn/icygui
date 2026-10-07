//! Operator actions (ACT-01..07): the dialogs that ask for what an action
//! needs, the checks before it goes out, and what became of it.
//!
//! The flow, from a key, a button or the palette to Icinga and back:
//!
//! 1. A view asks `AppState::request` for an [`ObjectAction`] on some
//!    objects. Actions the API user may not run are refused there with
//!    the missing permission named (ENV-09).
//! 2. The workspace picks the request up and opens its dialog
//!    ([`dialog::ActionDialog`]: acknowledge, downtime, comment, passive
//!    result, run command), asks first (bulk removals, many checks), or
//!    sends it at once (a check, one removal). [`forms::eligible`] drops
//!    objects Icinga would refuse (acknowledging an OK service) and the
//!    dialog says which.
//! 3. A form becomes an [`ActionSpec`]; `AppState::submit` sends it to the
//!    core as one `Command::Action` (the core splits host and service
//!    targets and batches names) and the [`tracker::Tracker`] marks the
//!    objects (`ack pending…`) and shows a toast.
//! 4. The core's `ActionFinished` updates the toast (done, done for some
//!    with each failure and Icinga's reason, or failed) and the markers;
//!    the core re-queries the changed objects, so their new state shows
//!    within a second.
//!
//! Only runtime operations exist here (D6, ACT-08): no attribute changes,
//! no configuration, no notifications sent by Icinga.

pub(crate) mod dialog;
pub(crate) mod expression;
pub(crate) mod forms;
pub(crate) mod toasts;
pub(crate) mod tracker;
pub(crate) mod when;

use ic_model::{Action, ActionTarget, ObjectKey};

use crate::actions::ObjectAction;

/// Above this many objects, checking them now asks first: a mass re-check
/// is a burst of work for the satellites.
pub(crate) const CHECK_CONFIRM_ABOVE: usize = 20;

/// An action ready to send.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ActionSpec {
    /// What it is, for permissions, markers and messages.
    pub(crate) kind: ObjectAction,
    /// What goes to Icinga.
    pub(crate) action: Action,
    /// Where.
    pub(crate) target: ActionTarget,
    /// The hosts and services it changes (the comment's or downtime's
    /// object for removals by name).
    pub(crate) objects: Vec<ObjectKey>,
}

impl ActionSpec {
    /// `action` for `objects`.
    pub(crate) fn for_objects(kind: ObjectAction, action: Action, objects: Vec<ObjectKey>) -> Self {
        Self {
            kind,
            action,
            target: ActionTarget::Objects(objects.clone()),
            objects,
        }
    }

    /// The actions that need no dialog: a forced check, removing
    /// acknowledgements or downtimes, one comment or downtime by name.
    /// `None` for the actions with a dialog. `object` is the comment's or
    /// downtime's object for removals by name.
    pub(crate) fn immediate(kind: &ObjectAction, objects: Vec<ObjectKey>) -> Option<Self> {
        let action = match kind {
            ObjectAction::CheckNow => Action::CheckNow { force: true },
            ObjectAction::RemoveAcknowledgement => Action::RemoveAcknowledgement,
            ObjectAction::RemoveDowntimes => Action::RemoveAllDowntimes,
            ObjectAction::RemoveComment(name) => {
                return Some(Self {
                    kind: kind.clone(),
                    action: Action::RemoveAllDowntimes,
                    target: ActionTarget::Comment(name.clone()),
                    objects,
                });
            }
            ObjectAction::RemoveDowntime(name) => {
                return Some(Self {
                    kind: kind.clone(),
                    action: Action::RemoveAllDowntimes,
                    target: ActionTarget::Downtime(name.clone()),
                    objects,
                });
            }
            ObjectAction::Acknowledge
            | ObjectAction::ScheduleDowntime
            | ObjectAction::AddComment
            | ObjectAction::SubmitCheckResult
            | ObjectAction::RunCommand => return None,
        };
        Some(Self::for_objects(kind.clone(), action, objects))
    }
}

/// The actions (`/v1/actions/<name>`) the client may send (ACT-08): runtime
/// operations only (PLAN.md D6); `execute-command` behind a confirmation.
#[cfg(test)]
pub(crate) const ALLOWED_ENDPOINTS: [&str; 9] = [
    "reschedule-check",
    "acknowledge-problem",
    "remove-acknowledgement",
    "schedule-downtime",
    "remove-downtime",
    "add-comment",
    "remove-comment",
    "process-check-result",
    "execute-command",
];

/// The Icinga actions (`/v1/actions/<name>`) the client can send: every
/// kind of [`ObjectAction`], built the way the dialogs and immediate
/// actions build them (ACT-08 checks this set).
#[cfg(test)]
pub(crate) fn allowed_action_names() -> Vec<&'static str> {
    let now = ic_model::Timestamp::now();
    let object = vec![ObjectKey::host("h")];
    let every = [
        ObjectAction::Acknowledge,
        ObjectAction::RemoveAcknowledgement,
        ObjectAction::ScheduleDowntime,
        ObjectAction::RemoveDowntimes,
        ObjectAction::CheckNow,
        ObjectAction::AddComment,
        ObjectAction::RemoveComment("h!c".to_owned()),
        ObjectAction::RemoveDowntime("h!d".to_owned()),
        ObjectAction::SubmitCheckResult,
        ObjectAction::RunCommand,
    ];
    every
        .iter()
        .map(|kind| {
            // A new kind must be listed above: this match fails to compile.
            let spec = match kind {
                ObjectAction::Acknowledge => ActionSpec::for_objects_opt(
                    kind.clone(),
                    forms::AckForm {
                        comment: "c".to_owned(),
                        ..forms::AckForm::default()
                    }
                    .action(now)
                    .ok(),
                    &object,
                ),
                ObjectAction::ScheduleDowntime => ActionSpec::for_objects_opt(
                    kind.clone(),
                    forms::DowntimeForm {
                        comment: "c".to_owned(),
                        ..forms::DowntimeForm::default()
                    }
                    .action(now)
                    .ok(),
                    &object,
                ),
                ObjectAction::AddComment => ActionSpec::for_objects_opt(
                    kind.clone(),
                    forms::CommentForm {
                        text: "c".to_owned(),
                        ..forms::CommentForm::default()
                    }
                    .action(now)
                    .ok(),
                    &object,
                ),
                ObjectAction::SubmitCheckResult => ActionSpec::for_objects_opt(
                    kind.clone(),
                    forms::ResultForm {
                        output: "OK".to_owned(),
                        ..forms::ResultForm::default()
                    }
                    .action()
                    .ok(),
                    &object,
                ),
                ObjectAction::RunCommand => ActionSpec::for_objects_opt(
                    kind.clone(),
                    forms::CommandForm::default().action(&[]).ok(),
                    &object,
                ),
                ObjectAction::RemoveAcknowledgement
                | ObjectAction::RemoveDowntimes
                | ObjectAction::CheckNow
                | ObjectAction::RemoveComment(_)
                | ObjectAction::RemoveDowntime(_) => ActionSpec::immediate(kind, object.clone()),
            };
            let spec = spec.unwrap_or_else(|| panic!("{kind:?} builds an action"));
            // `ic-api` sends a removal by comment name to `remove-comment`.
            match spec.target {
                ActionTarget::Comment(_) => "remove-comment",
                ActionTarget::Objects(_) | ActionTarget::Downtime(_) => spec.action.api_name(),
            }
        })
        .collect()
}

#[cfg(test)]
impl ActionSpec {
    fn for_objects_opt(
        kind: ObjectAction,
        action: Option<Action>,
        objects: &[ObjectKey],
    ) -> Option<Self> {
        action.map(|action| Self::for_objects(kind, action, objects.to_vec()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_action_goes_to_a_runtime_endpoint() {
        let names = allowed_action_names();
        assert_eq!(names.len(), 10, "one per kind of action");
        for name in &names {
            assert!(ALLOWED_ENDPOINTS.contains(name), "{name}");
        }
        // And every allowed endpoint is one the client really uses.
        for endpoint in ALLOWED_ENDPOINTS {
            assert!(names.contains(&endpoint), "{endpoint} is unused");
        }
    }

    #[test]
    fn immediate_actions_need_no_dialog() {
        let objects = vec![ObjectKey::host("db-01")];
        let check = ActionSpec::immediate(&ObjectAction::CheckNow, objects.clone()).unwrap();
        assert_eq!(check.action, Action::CheckNow { force: true });
        assert_eq!(check.target, ActionTarget::Objects(objects.clone()));
        let removal = ActionSpec::immediate(
            &ObjectAction::RemoveComment("db-01!c".to_owned()),
            objects.clone(),
        )
        .unwrap();
        assert_eq!(removal.target, ActionTarget::Comment("db-01!c".to_owned()));
        assert_eq!(removal.objects, objects);
        let downtime = ActionSpec::immediate(
            &ObjectAction::RemoveDowntime("db-01!d".to_owned()),
            objects.clone(),
        )
        .unwrap();
        assert_eq!(
            downtime.target,
            ActionTarget::Downtime("db-01!d".to_owned())
        );
        assert_eq!(downtime.action, Action::RemoveAllDowntimes);
        assert!(ActionSpec::immediate(&ObjectAction::Acknowledge, objects).is_none());
    }
}
