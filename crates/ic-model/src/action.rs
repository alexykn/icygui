//! Operator actions (`/v1/actions/*`).
//!
//! Only runtime operations: the client never changes configuration or
//! object attributes. Acknowledgements never ask Icinga to send a
//! notification (`notify` is always false).

use serde::{Deserialize, Serialize};

use crate::name::ObjectKey;
use crate::object::Vars;
use crate::time::Timestamp;

/// Fixed or flexible downtime.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum DowntimeMode {
    /// In effect for the whole window.
    Fixed,
    /// Starts on the first problem inside the window and lasts `duration`
    /// seconds.
    Flexible {
        /// Length in seconds once triggered.
        duration: f64,
    },
}

/// Whether child hosts get downtimes too (`child_options`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ChildOptions {
    /// `DowntimeNoChildren`.
    #[default]
    None,
    /// `DowntimeTriggeredChildren`: children get downtimes triggered by this one.
    Triggered,
    /// `DowntimeNonTriggeredChildren`: children get independent downtimes.
    NonTriggered,
}

impl ChildOptions {
    /// The API value.
    #[must_use]
    pub fn api_value(self) -> &'static str {
        match self {
            Self::None => "DowntimeNoChildren",
            Self::Triggered => "DowntimeTriggeredChildren",
            Self::NonTriggered => "DowntimeNonTriggeredChildren",
        }
    }
}

/// The command type for `execute-command`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CommandType {
    /// The object's check command.
    CheckCommand,
    /// The object's event command (Icinga's default).
    #[default]
    EventCommand,
}

impl CommandType {
    /// The API value.
    #[must_use]
    pub fn api_value(self) -> &'static str {
        match self {
            Self::CheckCommand => "CheckCommand",
            Self::EventCommand => "EventCommand",
        }
    }
}

/// An action on one or more hosts or services. The author is added by the
/// environment runtime from its configuration.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Action {
    /// `reschedule-check` with `next_check` = now.
    CheckNow {
        /// Run even outside the check period or with active checks disabled.
        force: bool,
    },
    /// `acknowledge-problem` (always with `notify: false`).
    Acknowledge {
        /// Comment text.
        comment: String,
        /// Keep it until the object fully recovers.
        sticky: bool,
        /// Keep the comment after the acknowledgement ends.
        persistent: bool,
        /// Remove it at this time.
        expiry: Option<Timestamp>,
    },
    /// `remove-acknowledgement`.
    RemoveAcknowledgement,
    /// `schedule-downtime`.
    ScheduleDowntime {
        /// Comment text.
        comment: String,
        /// Window start.
        start: Timestamp,
        /// Window end.
        end: Timestamp,
        /// Fixed or flexible.
        mode: DowntimeMode,
        /// For hosts: also schedule downtimes for all their services.
        all_services: bool,
        /// For hosts: downtimes for child hosts.
        child_options: ChildOptions,
        /// Name of a downtime that triggers this one.
        trigger_name: Option<String>,
    },
    /// `remove-downtime` for all downtimes of the targets.
    RemoveAllDowntimes,
    /// `add-comment`.
    AddComment {
        /// Comment text.
        text: String,
        /// Remove it at this time.
        expiry: Option<Timestamp>,
    },
    /// `process-check-result`: submit a passive result.
    ProcessCheckResult {
        /// Plugin exit status (0–3).
        exit_status: u8,
        /// Plugin output.
        output: String,
        /// Performance data entries.
        perfdata: Vec<String>,
        /// Expect a new result within this many seconds.
        ttl: Option<f64>,
    },
    /// `execute-command` (Icinga 2.13+).
    ExecuteCommand {
        /// Check or event command.
        command_type: CommandType,
        /// The command to run; the object's own command if `None`.
        command: Option<String>,
        /// Where to run it; `$command_endpoint$` if `None`.
        endpoint: Option<String>,
        /// Macro overrides.
        macros: Vars,
        /// Seconds the execution result is kept.
        ttl: f64,
    },
}

impl Action {
    /// The `/v1/actions/<name>` endpoint.
    #[must_use]
    pub fn api_name(&self) -> &'static str {
        match self {
            Self::CheckNow { .. } => "reschedule-check",
            Self::Acknowledge { .. } => "acknowledge-problem",
            Self::RemoveAcknowledgement => "remove-acknowledgement",
            Self::ScheduleDowntime { .. } => "schedule-downtime",
            Self::RemoveAllDowntimes => "remove-downtime",
            Self::AddComment { .. } => "add-comment",
            Self::ProcessCheckResult { .. } => "process-check-result",
            Self::ExecuteCommand { .. } => "execute-command",
        }
    }

    /// A short verb for messages ("acknowledge", "check now", …).
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::CheckNow { .. } => "check now",
            Self::Acknowledge { .. } => "acknowledge",
            Self::RemoveAcknowledgement => "remove acknowledgement",
            Self::ScheduleDowntime { .. } => "schedule downtime",
            Self::RemoveAllDowntimes => "remove downtimes",
            Self::AddComment { .. } => "add comment",
            Self::ProcessCheckResult { .. } => "submit check result",
            Self::ExecuteCommand { .. } => "run command",
        }
    }
}

/// What an action applies to.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ActionTarget {
    /// These hosts and services.
    Objects(Vec<ObjectKey>),
    /// One downtime, by its full name (only for removing downtimes).
    Downtime(String),
    /// One comment, by its full name (only for removing comments).
    Comment(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ACT-08 (PLAN.md D6): every action is a runtime operation. A new
    /// variant must be added to `every` (the match below won't compile
    /// otherwise) and its endpoint judged against the allowed list.
    #[test]
    fn only_runtime_operations_exist() {
        const ALLOWED: [&str; 8] = [
            "reschedule-check",
            "acknowledge-problem",
            "remove-acknowledgement",
            "schedule-downtime",
            "remove-downtime",
            "add-comment",
            "process-check-result",
            "execute-command",
        ];
        let every = [
            Action::CheckNow { force: true },
            Action::Acknowledge {
                comment: String::new(),
                sticky: false,
                persistent: false,
                expiry: None,
            },
            Action::RemoveAcknowledgement,
            Action::ScheduleDowntime {
                comment: String::new(),
                start: Timestamp::from_unix_seconds(0.),
                end: Timestamp::from_unix_seconds(1.),
                mode: DowntimeMode::Fixed,
                all_services: false,
                child_options: ChildOptions::None,
                trigger_name: None,
            },
            Action::RemoveAllDowntimes,
            Action::AddComment {
                text: String::new(),
                expiry: None,
            },
            Action::ProcessCheckResult {
                exit_status: 0,
                output: String::new(),
                perfdata: Vec::new(),
                ttl: None,
            },
            Action::ExecuteCommand {
                command_type: CommandType::EventCommand,
                command: None,
                endpoint: None,
                macros: Vars::new(),
                ttl: 1.,
            },
        ];
        for action in &every {
            match action {
                Action::CheckNow { .. }
                | Action::Acknowledge { .. }
                | Action::RemoveAcknowledgement
                | Action::ScheduleDowntime { .. }
                | Action::RemoveAllDowntimes
                | Action::AddComment { .. }
                | Action::ProcessCheckResult { .. }
                | Action::ExecuteCommand { .. } => {}
            }
            assert!(
                ALLOWED.contains(&action.api_name()),
                "{} is not a runtime operation",
                action.api_name()
            );
            assert!(!action.label().is_empty());
        }
    }

    #[test]
    fn api_names() {
        assert_eq!(
            Action::CheckNow { force: true }.api_name(),
            "reschedule-check"
        );
        assert_eq!(Action::RemoveAllDowntimes.api_name(), "remove-downtime");
        assert_eq!(
            ChildOptions::Triggered.api_value(),
            "DowntimeTriggeredChildren"
        );
    }
}
