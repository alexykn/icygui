//! Event stream views (topic 04): the latest events of the objects a view
//! matches, from the local event log's recent entries.

use ic_config::StreamOptions;
use ic_model::{CheckableState, HostState, ServiceState, StateType};

use super::board::Board;
use crate::command::{LogEntry, LogKind};
use crate::snapshot::STREAM_EVENTS;

/// The events (newest first) of `board`'s members that its options let
/// through, at most [`STREAM_EVENTS`].
pub(super) fn select(board: &Board, events: &[LogEntry]) -> Vec<LogEntry> {
    let options = board.view.stream;
    events
        .iter()
        .filter(|entry| shows(options, entry.kind) && board.members.contains_key(&entry.object))
        .take(STREAM_EVENTS)
        .cloned()
        .collect()
}

/// The events (newest first) an event stream with `options` and no filter
/// shows, at most [`STREAM_EVENTS`]: the cluster section's *events*, the
/// whole environment's (topic 14). It reads the snapshot's `events` and
/// needs no evaluation.
#[must_use]
pub fn stream_events(options: StreamOptions, events: &[LogEntry]) -> Vec<LogEntry> {
    events
        .iter()
        .filter(|entry| shows(options, entry.kind))
        .take(STREAM_EVENTS)
        .cloned()
        .collect()
}

/// Whether an event stream with `options` shows events of `kind`.
pub(super) fn shows(options: StreamOptions, kind: LogKind) -> bool {
    let events = options.events;
    match kind {
        LogKind::State { state, state_type } => {
            events.state_changes
                && (!options.hard_states_only || state_type == StateType::Hard)
                && (options.recoveries || !is_recovery(state))
        }
        LogKind::AcknowledgementSet | LogKind::AcknowledgementCleared => events.acknowledgements,
        LogKind::CommentAdded | LogKind::CommentRemoved => events.comments,
        LogKind::DowntimeStarted | LogKind::DowntimeEnded => events.downtimes,
        LogKind::FlappingStarted | LogKind::FlappingStopped => events.flapping,
    }
}

/// A state change to OK or UP.
fn is_recovery(state: CheckableState) -> bool {
    matches!(
        state,
        CheckableState::Host(HostState::Up) | CheckableState::Service(ServiceState::Ok)
    )
}

#[cfg(test)]
mod tests {
    use ic_config::StreamEvents;

    use super::*;

    fn state(state: CheckableState, state_type: StateType) -> LogKind {
        LogKind::State { state, state_type }
    }

    #[test]
    fn options_pick_the_kinds_of_events() {
        let critical = CheckableState::Service(ServiceState::Critical);
        let ok = CheckableState::Service(ServiceState::Ok);
        let up = CheckableState::Host(HostState::Up);
        let defaults = StreamOptions::default();
        assert!(shows(defaults, state(critical, StateType::Hard)));
        assert!(
            !shows(defaults, state(critical, StateType::Soft)),
            "hard only"
        );
        assert!(
            !shows(defaults, state(ok, StateType::Hard)),
            "no recoveries"
        );
        assert!(!shows(defaults, state(up, StateType::Hard)));
        assert!(shows(defaults, LogKind::AcknowledgementSet));
        assert!(shows(defaults, LogKind::DowntimeEnded));
        assert!(shows(defaults, LogKind::CommentAdded));
        assert!(!shows(defaults, LogKind::FlappingStarted));

        let everything = StreamOptions {
            events: StreamEvents {
                flapping: true,
                ..StreamEvents::default()
            },
            hard_states_only: false,
            recoveries: true,
            lines: 8,
        };
        for kind in [
            state(critical, StateType::Soft),
            state(ok, StateType::Hard),
            LogKind::FlappingStopped,
        ] {
            assert!(shows(everything, kind), "{kind:?}");
        }
        let nothing = StreamOptions {
            events: StreamEvents {
                state_changes: false,
                acknowledgements: false,
                downtimes: false,
                comments: false,
                flapping: false,
            },
            ..everything
        };
        for kind in [
            state(critical, StateType::Hard),
            LogKind::AcknowledgementCleared,
            LogKind::CommentRemoved,
            LogKind::DowntimeStarted,
            LogKind::FlappingStarted,
        ] {
            assert!(!shows(nothing, kind), "{kind:?}");
        }
    }
}
