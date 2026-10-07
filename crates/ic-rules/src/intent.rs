//! The engine's input (what changed, and where the object appears) and
//! output (notifications to show or record).

use serde::{Deserialize, Serialize};

use ic_model::{CheckableState, ObjectKey, StateType, Timestamp};

/// Identifies a dashboard inside its group, by the ids `ic-config` assigns.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DashboardRef {
    /// The group's id.
    pub group_id: String,
    /// The dashboard's id.
    pub dashboard_id: String,
}

/// What changed on an object.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Change {
    /// The state changed, or a soft state became hard.
    State {
        /// The state before, if known.
        previous: Option<CheckableState>,
        /// The state now.
        current: CheckableState,
        /// Soft or hard.
        state_type: StateType,
        /// When the current state began (`last_state_change`).
        since: Timestamp,
        /// First line of the plugin output.
        output: String,
    },
    /// A problem was acknowledged.
    AcknowledgementSet {
        /// Who acknowledged.
        author: String,
        /// The comment.
        comment: String,
    },
    /// An acknowledgement was removed or expired.
    AcknowledgementCleared,
    /// A downtime took effect.
    DowntimeStarted {
        /// Who scheduled it.
        author: String,
        /// The comment.
        comment: String,
    },
    /// A downtime ended or was removed.
    DowntimeEnded,
    /// The object started flapping.
    FlappingStarted,
    /// The object stopped flapping.
    FlappingStopped,
}

/// One change for the engine to judge.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RuleInput {
    /// The host or service.
    pub object: ObjectKey,
    /// The host's display name.
    pub host_display: String,
    /// The service's display name, for services.
    pub service_display: Option<String>,
    /// What changed.
    pub change: Change,
    /// Whether the object's problem is handled after the change.
    pub handled: bool,
    /// The dashboards whose filter matches the object after the change.
    pub memberships: Vec<DashboardRef>,
    /// When the change happened.
    pub at: Timestamp,
}

/// Local wall-clock facts the engine needs for quiet hours. The caller
/// computes them from the system time zone, keeping the engine pure.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LocalTime {
    /// Day of the week, 0 = Monday.
    pub weekday: u8,
    /// Minutes after local midnight.
    pub minute_of_day: u16,
}

/// How a notification should look.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Tone {
    /// Critical or down.
    Critical,
    /// Warning.
    Warning,
    /// Unknown or unreachable.
    Unknown,
    /// Recovered.
    Recovery,
    /// Acknowledgement, downtime, flapping, summaries.
    Info,
}

/// A notification the engine decided on.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NotificationIntent {
    /// Stable id; the same change never produces two intents with the same id.
    pub id: String,
    /// The object, or `None` for storm summaries.
    pub object: Option<ObjectKey>,
    /// `CRITICAL · postgres-replication on db-prod-03`.
    pub title: String,
    /// Where it matched: `databases / production`, or the environment name.
    pub subtitle: String,
    /// First line of the output, the ack comment, or the summary text.
    pub body: String,
    /// Look and sound.
    pub tone: Tone,
    /// Play a sound.
    pub sound: bool,
    /// Record it in the notification centre but don't show an OS
    /// notification (quiet hours, paused, or absorbed by a storm summary).
    pub silent: bool,
    /// Why it is silent: set by the engine exactly when `silent` is. A
    /// record logged before the reason was kept has `silent` without one
    /// (`None`: the reason is unknown).
    #[serde(default)]
    pub silenced: Option<Silence>,
    /// When the change happened.
    pub at: Timestamp,
}

/// Why a notification was recorded without an OS notification.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Silence {
    /// Notifications were paused (every environment, or this one).
    Paused,
    /// Quiet hours.
    QuietHours,
    /// One of many in a storm: the storm summary with this id covers it
    /// (the summary comes when the storm calms down, or once a minute
    /// while it lasts).
    Storm {
        /// The [`NotificationIntent::id`] of the storm summary that covers
        /// it (`storm:<when the stretch began>`).
        summary: String,
    },
}

impl Silence {
    /// The reason in a word or two: `paused`, `quiet hours`, `storm`.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Paused => "paused",
            Self::QuietHours => "quiet hours",
            Self::Storm { .. } => "storm",
        }
    }
}

/// The id of the notification of `object` entering the problem or
/// recovery `state` at `since` (`last_state_change`):
/// `"{object}:{state}:{since}"`, with `since` in Unix seconds to the
/// millisecond. [`NotificationIntent::id`] of every state notification has
/// this form; an event log of earlier runs can be asked for it (see
/// [`RuleEngine::restore_notified`](crate::RuleEngine::restore_notified)).
#[must_use]
pub fn state_intent_id(object: &ObjectKey, state: CheckableState, since: Timestamp) -> String {
    crate::text::state_id(object, state, since)
}
