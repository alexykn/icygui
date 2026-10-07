//! The local event log as the history shows it (PANE-04): state changes,
//! acknowledgements, comments, downtimes and flapping of an object (a
//! host's include its services'), recorded while icygui runs, newest
//! first, like the turn-1 design's event stream. Pure, so it is tested
//! without a window.

use ic_core::{LogEntry, LogKind};
use ic_model::{CheckableState, ObjectKey, StateType, Timestamp};

use crate::format;

/// How many entries a host's history tab shows.
pub(crate) const HOST_HISTORY_LIMIT: usize = 200;
/// How many entries a service pane's history section shows.
pub(crate) const SERVICE_HISTORY_LIMIT: usize = 12;

/// The colour of a history line's dot and kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HistoryTone {
    /// The state's colour.
    State(CheckableState),
    /// The accent: acknowledgements.
    Accent,
    /// Downtimes (the unknown purple, as in the design's event stream).
    Downtime,
    /// Flapping (warning yellow).
    Flapping,
    /// Comments, ends of things (muted).
    Quiet,
}

/// One line of the history.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct HistoryLine {
    /// `14:32` today, `Oct 3 14:32` on other days.
    pub(crate) time: String,
    /// Its colour.
    pub(crate) tone: HistoryTone,
    /// `CRITICAL`, `OK`, `ACK`, `DOWNTIME`, …
    pub(crate) kind: &'static str,
    /// What it happened to: the service's name in a host's history, else
    /// empty (the pane says whose history it is).
    pub(crate) object: String,
    /// `hard · CRITICAL - replication lag 412s`, `m.keller: on it`.
    pub(crate) note: String,
}

/// The line for `entry` in the history of `viewer`.
pub(crate) fn line(entry: &LogEntry, viewer: &ObjectKey, now: Timestamp) -> HistoryLine {
    let (tone, kind) = match entry.kind {
        LogKind::State { state, .. } => (HistoryTone::State(state), state_kind(state)),
        LogKind::AcknowledgementSet => (HistoryTone::Accent, "ACK"),
        LogKind::AcknowledgementCleared => (HistoryTone::Quiet, "ACK REMOVED"),
        LogKind::CommentAdded => (HistoryTone::Quiet, "COMMENT"),
        LogKind::CommentRemoved => (HistoryTone::Quiet, "COMMENT REMOVED"),
        LogKind::DowntimeStarted => (HistoryTone::Downtime, "DOWNTIME"),
        LogKind::DowntimeEnded => (HistoryTone::Quiet, "DOWNTIME ENDED"),
        LogKind::FlappingStarted => (HistoryTone::Flapping, "FLAPPING"),
        LogKind::FlappingStopped => (HistoryTone::Quiet, "FLAPPING ENDED"),
    };
    let object = match (&entry.object, viewer) {
        (ObjectKey::Service { key }, ObjectKey::Host { .. }) => key.name.to_string(),
        (ObjectKey::Host { .. }, ObjectKey::Host { .. }) => "host".to_owned(),
        _ => String::new(),
    };
    let text = entry
        .text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let note = match (entry.kind, entry.author.as_deref()) {
        (LogKind::State { state_type, .. }, _) => {
            let kind = match state_type {
                StateType::Hard => "hard",
                StateType::Soft => "soft",
            };
            if text.is_empty() {
                kind.to_owned()
            } else {
                format!("{kind} · {text}")
            }
        }
        (_, Some(author)) if !author.trim().is_empty() && !text.is_empty() => {
            format!("{}: {text}", author.trim())
        }
        (_, Some(author)) if !author.trim().is_empty() => author.trim().to_owned(),
        _ => text.to_owned(),
    };
    HistoryLine {
        time: format::clock(entry.at, now),
        tone,
        kind,
        object,
        note,
    }
}

/// `CRITICAL`, `OK`, `DOWN`, …
fn state_kind(state: CheckableState) -> &'static str {
    use ic_model::{HostState, ServiceState};
    match state {
        CheckableState::Service(ServiceState::Ok) => "OK",
        CheckableState::Service(ServiceState::Warning) => "WARNING",
        CheckableState::Service(ServiceState::Critical) => "CRITICAL",
        CheckableState::Service(ServiceState::Unknown) => "UNKNOWN",
        CheckableState::Host(HostState::Up) => "UP",
        CheckableState::Host(HostState::Down) => "DOWN",
        CheckableState::Host(HostState::Unreachable) => "UNREACHABLE",
        CheckableState::Service(ServiceState::Pending)
        | CheckableState::Host(HostState::Pending) => "PENDING",
    }
}

/// `recorded locally since 14:02 · kept 48 h`: since the log's oldest
/// entry or this run's start, whichever is earlier (the log is kept for
/// the retention period; while icygui doesn't run, nothing is recorded).
pub(crate) fn since_text(
    oldest: Option<Timestamp>,
    started: Timestamp,
    retention_hours: u32,
    now: Timestamp,
) -> String {
    let since = oldest.map_or(
        started,
        |oldest| if oldest < started { oldest } else { started },
    );
    format!(
        "recorded locally since {} · kept {retention_hours} h",
        format::clock(since, now)
    )
}

#[cfg(test)]
mod tests {
    use ic_model::{HostState, ServiceState};

    use super::*;

    fn now() -> Timestamp {
        Timestamp::from_unix_seconds(1_790_000_000.)
    }

    fn entry(object: ObjectKey, kind: LogKind, text: &str, author: Option<&str>) -> LogEntry {
        LogEntry {
            at: now(),
            object,
            kind,
            text: text.to_owned(),
            author: author.map(str::to_owned),
        }
    }

    #[test]
    fn lines_name_the_kind_the_service_and_what_was_said() {
        let host = ObjectKey::host("db-prod-03");
        let service = ObjectKey::service("db-prod-03", "postgres-replication");
        let critical = entry(
            service.clone(),
            LogKind::State {
                state: CheckableState::Service(ServiceState::Critical),
                state_type: StateType::Hard,
            },
            "CRITICAL - lag 412s\nmore",
            None,
        );
        let in_host = line(&critical, &host, now());
        assert_eq!(in_host.kind, "CRITICAL");
        assert_eq!(in_host.object, "postgres-replication");
        assert_eq!(in_host.note, "hard · CRITICAL - lag 412s");
        assert_eq!(
            in_host.tone,
            HistoryTone::State(CheckableState::Service(ServiceState::Critical))
        );
        assert_eq!(line(&critical, &service, now()).object, "", "its own pane");

        let ack = entry(
            service.clone(),
            LogKind::AcknowledgementSet,
            "on it",
            Some("m.keller"),
        );
        let ack = line(&ack, &host, now());
        assert_eq!((ack.kind, ack.note.as_str()), ("ACK", "m.keller: on it"));
        assert_eq!(ack.tone, HistoryTone::Accent);

        let down = entry(
            host.clone(),
            LogKind::State {
                state: CheckableState::Host(HostState::Down),
                state_type: StateType::Soft,
            },
            "",
            None,
        );
        let down = line(&down, &host, now());
        assert_eq!(
            (down.kind, down.object.as_str(), down.note.as_str()),
            ("DOWN", "host", "soft")
        );
        let ended = line(
            &entry(host.clone(), LogKind::DowntimeEnded, "", None),
            &host,
            now(),
        );
        assert_eq!(
            (ended.kind, ended.tone),
            ("DOWNTIME ENDED", HistoryTone::Quiet)
        );
    }

    #[test]
    fn the_history_says_since_when_it_records() {
        let started = now();
        let earlier = Timestamp::from_unix_seconds(now().as_unix_seconds() - 600.);
        let text = since_text(Some(earlier), started, 48, now());
        assert!(text.starts_with("recorded locally since "), "{text}");
        assert!(text.ends_with(" · kept 48 h"), "{text}");
        assert_eq!(
            since_text(None, started, 48, now()),
            since_text(Some(now()), started, 48, now())
        );
    }
}
