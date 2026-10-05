//! Events from Icinga's `/v1/events` stream, in domain terms.
//!
//! Events carry only what Icinga sends. A `CheckResult` event carries the
//! object's state after processing ([`StateAfter`], from the result's
//! `vars_after`), its downtime depth and acknowledgement, so `ic-core` can
//! update the object without re-querying it; it estimates the next check
//! itself.

use serde::{Deserialize, Serialize};

use crate::name::ObjectKey;
use crate::object::{AckKind, CheckResult, Comment, Downtime};
use crate::state::{HostState, ServiceState, StateType};
use crate::time::Timestamp;

/// The state of a host or a service.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CheckableState {
    /// A host state.
    Host(HostState),
    /// A service state.
    Service(ServiceState),
}

impl CheckableState {
    /// Whether this state is a problem.
    #[must_use]
    pub fn is_problem(self) -> bool {
        match self {
            Self::Host(state) => state.is_problem(),
            Self::Service(state) => state.is_problem(),
        }
    }

    /// The short label (`CRIT`, `DOWN`, …).
    #[must_use]
    pub fn short_label(self) -> &'static str {
        match self {
            Self::Host(state) => state.short_label(),
            Self::Service(state) => state.short_label(),
        }
    }
}

/// An object's state right after Icinga processed a check result: the
/// result's `vars_after` (`state`, `state_type`, `attempt`, `reachable`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StateAfter {
    /// The new state. For hosts, `vars_after.state` is a service-style state
    /// (0 and 1 are up, 2 and 3 down); a down host that isn't reachable is
    /// [`HostState::Unreachable`].
    pub state: CheckableState,
    /// Soft or hard.
    pub state_type: StateType,
    /// The check attempt (`check_attempt`).
    pub attempt: u32,
    /// Whether every dependency allowed the check (`last_reachable`).
    pub reachable: bool,
}

/// The kinds of events the client subscribes to (the API's `types`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EventKind {
    /// `CheckResult`.
    CheckResult,
    /// `StateChange`.
    StateChange,
    /// `AcknowledgementSet`.
    AcknowledgementSet,
    /// `AcknowledgementCleared`.
    AcknowledgementCleared,
    /// `CommentAdded`.
    CommentAdded,
    /// `CommentRemoved`.
    CommentRemoved,
    /// `DowntimeAdded`.
    DowntimeAdded,
    /// `DowntimeRemoved`.
    DowntimeRemoved,
    /// `DowntimeStarted`.
    DowntimeStarted,
    /// `DowntimeTriggered`.
    DowntimeTriggered,
    /// `Flapping`.
    Flapping,
    /// `ObjectCreated`.
    ObjectCreated,
    /// `ObjectModified`.
    ObjectModified,
    /// `ObjectDeleted`.
    ObjectDeleted,
}

impl EventKind {
    /// Every kind, in the order the client subscribes to them.
    pub const ALL: [Self; 14] = [
        Self::CheckResult,
        Self::StateChange,
        Self::AcknowledgementSet,
        Self::AcknowledgementCleared,
        Self::CommentAdded,
        Self::CommentRemoved,
        Self::DowntimeAdded,
        Self::DowntimeRemoved,
        Self::DowntimeStarted,
        Self::DowntimeTriggered,
        Self::Flapping,
        Self::ObjectCreated,
        Self::ObjectModified,
        Self::ObjectDeleted,
    ];

    /// The API's name for the type.
    #[must_use]
    pub fn api_name(self) -> &'static str {
        match self {
            Self::CheckResult => "CheckResult",
            Self::StateChange => "StateChange",
            Self::AcknowledgementSet => "AcknowledgementSet",
            Self::AcknowledgementCleared => "AcknowledgementCleared",
            Self::CommentAdded => "CommentAdded",
            Self::CommentRemoved => "CommentRemoved",
            Self::DowntimeAdded => "DowntimeAdded",
            Self::DowntimeRemoved => "DowntimeRemoved",
            Self::DowntimeStarted => "DowntimeStarted",
            Self::DowntimeTriggered => "DowntimeTriggered",
            Self::Flapping => "Flapping",
            Self::ObjectCreated => "ObjectCreated",
            Self::ObjectModified => "ObjectModified",
            Self::ObjectDeleted => "ObjectDeleted",
        }
    }

    /// Parses the API's name for the type.
    #[must_use]
    pub fn from_api_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.api_name() == name)
    }
}

/// One event from the stream.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Event {
    /// A check result was processed.
    CheckResult {
        /// The checked object.
        object: ObjectKey,
        /// The result.
        result: CheckResult,
        /// Downtime depth after processing (Icinga 2.11+).
        downtime_depth: Option<u32>,
        /// Acknowledgement after processing (Icinga 2.11+).
        acknowledgement: Option<AckKind>,
        /// The object's state after processing (`check_result.vars_after`),
        /// when Icinga sent it.
        after: Option<StateAfter>,
        /// When it happened.
        at: Timestamp,
    },
    /// The state changed (soft or hard).
    StateChange {
        /// The object.
        object: ObjectKey,
        /// The new state. For hosts, down vs unreachable is only known after
        /// re-querying `last_reachable`; the event reports down.
        state: CheckableState,
        /// Soft or hard.
        state_type: StateType,
        /// The check result that caused the change.
        result: CheckResult,
        /// Downtime depth after processing (Icinga 2.11+).
        downtime_depth: Option<u32>,
        /// Acknowledgement after processing (Icinga 2.11+).
        acknowledgement: Option<AckKind>,
        /// When it happened.
        at: Timestamp,
    },
    /// A problem was acknowledged.
    AcknowledgementSet {
        /// The object.
        object: ObjectKey,
        /// Who acknowledged.
        author: String,
        /// The acknowledgement comment.
        comment: String,
        /// Normal or sticky.
        kind: AckKind,
        /// When it expires, if it does.
        expiry: Option<Timestamp>,
        /// When it happened.
        at: Timestamp,
    },
    /// An acknowledgement was removed or expired.
    AcknowledgementCleared {
        /// The object.
        object: ObjectKey,
        /// When it happened.
        at: Timestamp,
    },
    /// A comment was added.
    CommentAdded {
        /// The comment.
        comment: Comment,
        /// When it happened.
        at: Timestamp,
    },
    /// A comment was removed.
    CommentRemoved {
        /// The comment.
        comment: Comment,
        /// When it happened.
        at: Timestamp,
    },
    /// A downtime was scheduled.
    DowntimeAdded {
        /// The downtime.
        downtime: Downtime,
        /// When it happened.
        at: Timestamp,
    },
    /// A downtime was removed or ended.
    DowntimeRemoved {
        /// The downtime.
        downtime: Downtime,
        /// When it happened.
        at: Timestamp,
    },
    /// A downtime window started.
    DowntimeStarted {
        /// The downtime.
        downtime: Downtime,
        /// When it happened.
        at: Timestamp,
    },
    /// A downtime took effect (fixed: at start; flexible: on the first problem).
    DowntimeTriggered {
        /// The downtime.
        downtime: Downtime,
        /// When it happened.
        at: Timestamp,
    },
    /// Flapping started or stopped.
    Flapping {
        /// The object.
        object: ObjectKey,
        /// Whether it's flapping now.
        flapping: bool,
        /// Current flapping value in percent.
        current: f64,
        /// When it happened.
        at: Timestamp,
    },
    /// A config object was created, modified or deleted.
    ObjectLifecycle {
        /// Created, modified or deleted.
        change: ObjectChange,
        /// The API object type (`Host`, `Service`, `Downtime`, …).
        object_type: String,
        /// The object's full name.
        name: String,
        /// When it happened.
        at: Timestamp,
    },
}

/// What happened to a config object.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ObjectChange {
    /// `ObjectCreated`.
    Created,
    /// `ObjectModified`.
    Modified,
    /// `ObjectDeleted`.
    Deleted,
}

impl Event {
    /// The event's timestamp.
    #[must_use]
    pub fn at(&self) -> Timestamp {
        match self {
            Self::CheckResult { at, .. }
            | Self::StateChange { at, .. }
            | Self::AcknowledgementSet { at, .. }
            | Self::AcknowledgementCleared { at, .. }
            | Self::CommentAdded { at, .. }
            | Self::CommentRemoved { at, .. }
            | Self::DowntimeAdded { at, .. }
            | Self::DowntimeRemoved { at, .. }
            | Self::DowntimeStarted { at, .. }
            | Self::DowntimeTriggered { at, .. }
            | Self::Flapping { at, .. }
            | Self::ObjectLifecycle { at, .. } => *at,
        }
    }

    /// The host or service the event is about, if any.
    #[must_use]
    pub fn object(&self) -> Option<&ObjectKey> {
        match self {
            Self::CheckResult { object, .. }
            | Self::StateChange { object, .. }
            | Self::AcknowledgementSet { object, .. }
            | Self::AcknowledgementCleared { object, .. }
            | Self::Flapping { object, .. } => Some(object),
            Self::CommentAdded { comment, .. } | Self::CommentRemoved { comment, .. } => {
                Some(&comment.object)
            }
            Self::DowntimeAdded { downtime, .. }
            | Self::DowntimeRemoved { downtime, .. }
            | Self::DowntimeStarted { downtime, .. }
            | Self::DowntimeTriggered { downtime, .. } => Some(&downtime.object),
            Self::ObjectLifecycle { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_names_round_trip() {
        for kind in EventKind::ALL {
            assert_eq!(EventKind::from_api_name(kind.api_name()), Some(kind));
        }
        assert_eq!(EventKind::from_api_name("Nope"), None);
    }

    #[test]
    fn events_know_their_object() {
        let event = Event::AcknowledgementCleared {
            object: ObjectKey::service("h", "s"),
            at: Timestamp::from_unix_seconds(5.0),
        };
        assert_eq!(event.object(), Some(&ObjectKey::service("h", "s")));
        assert_eq!(event.at(), Timestamp::from_unix_seconds(5.0));
    }
}
