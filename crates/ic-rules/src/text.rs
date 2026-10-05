//! What notifications say: labels, titles, bodies, tones and ids.

use ic_model::{CheckableState, HostState, ObjectKey, ServiceState, Timestamp};

use crate::intent::Tone;
use crate::settings::EventFilter;

/// Longest body, in characters. Plugin output can be huge; OS notifications
/// show a few lines at most, and the notification centre a single line.
const MAX_BODY_CHARS: usize = 400;

/// What a notification is about. Decides the title's label, the tone, and
/// the wording of storm summaries.
///
/// The declaration order is the order of storm-summary breakdowns, worst
/// first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Label {
    /// Service critical.
    Critical,
    /// Host down.
    Down,
    /// Service unknown.
    Unknown,
    /// Host unreachable.
    Unreachable,
    /// Service warning.
    Warning,
    /// Back to OK / UP.
    Recovered,
    /// Acknowledgement set.
    Acknowledged,
    /// Acknowledgement removed or expired.
    AckCleared,
    /// Downtime in effect.
    Downtime,
    /// Downtime ended or removed.
    DowntimeEnded,
    /// Flapping started.
    Flapping,
    /// Flapping stopped.
    FlappingStopped,
}

impl Label {
    /// Every label, in breakdown order.
    pub(crate) const ALL: [Self; 12] = [
        Self::Critical,
        Self::Down,
        Self::Unknown,
        Self::Unreachable,
        Self::Warning,
        Self::Recovered,
        Self::Acknowledged,
        Self::AckCleared,
        Self::Downtime,
        Self::DowntimeEnded,
        Self::Flapping,
        Self::FlappingStopped,
    ];

    /// The label of a problem state; `None` for OK, UP and pending.
    pub(crate) fn for_problem(state: CheckableState) -> Option<Self> {
        match state {
            CheckableState::Service(ServiceState::Critical) => Some(Self::Critical),
            CheckableState::Service(ServiceState::Warning) => Some(Self::Warning),
            CheckableState::Service(ServiceState::Unknown) => Some(Self::Unknown),
            CheckableState::Host(HostState::Down) => Some(Self::Down),
            CheckableState::Host(HostState::Unreachable) => Some(Self::Unreachable),
            CheckableState::Service(ServiceState::Ok | ServiceState::Pending)
            | CheckableState::Host(HostState::Up | HostState::Pending) => None,
        }
    }

    /// Position in [`Label::ALL`].
    pub(crate) fn index(self) -> usize {
        match self {
            Self::Critical => 0,
            Self::Down => 1,
            Self::Unknown => 2,
            Self::Unreachable => 3,
            Self::Warning => 4,
            Self::Recovered => 5,
            Self::Acknowledged => 6,
            Self::AckCleared => 7,
            Self::Downtime => 8,
            Self::DowntimeEnded => 9,
            Self::Flapping => 10,
            Self::FlappingStopped => 11,
        }
    }

    /// The title's label: `CRITICAL`, `RECOVERED`, `ACKNOWLEDGED`, …
    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::Critical => "CRITICAL",
            Self::Down => "DOWN",
            Self::Unknown => "UNKNOWN",
            Self::Unreachable => "UNREACHABLE",
            Self::Warning => "WARNING",
            Self::Recovered => "RECOVERED",
            Self::Acknowledged => "ACKNOWLEDGED",
            Self::AckCleared => "ACK CLEARED",
            Self::Downtime => "DOWNTIME",
            Self::DowntimeEnded => "DOWNTIME ENDED",
            Self::Flapping => "FLAPPING",
            Self::FlappingStopped => "FLAPPING STOPPED",
        }
    }

    /// The word used in storm-summary breakdowns (`9 critical · 2 down`).
    pub(crate) fn noun(self) -> &'static str {
        match self {
            Self::Critical => "critical",
            Self::Down => "down",
            Self::Unknown => "unknown",
            Self::Unreachable => "unreachable",
            Self::Warning => "warning",
            Self::Recovered => "recovered",
            Self::Acknowledged => "acknowledged",
            Self::AckCleared => "ack cleared",
            Self::Downtime => "downtime",
            Self::DowntimeEnded => "downtime ended",
            Self::Flapping => "flapping",
            Self::FlappingStopped => "flapping stopped",
        }
    }

    /// Look and sound of the notification.
    pub(crate) fn tone(self) -> Tone {
        match self {
            Self::Critical | Self::Down => Tone::Critical,
            Self::Warning => Tone::Warning,
            Self::Unknown | Self::Unreachable => Tone::Unknown,
            Self::Recovered => Tone::Recovery,
            Self::Acknowledged
            | Self::AckCleared
            | Self::Downtime
            | Self::DowntimeEnded
            | Self::Flapping
            | Self::FlappingStopped => Tone::Info,
        }
    }

    /// Whether this is a problem state (what storm summaries call "new
    /// problems").
    pub(crate) fn is_problem(self) -> bool {
        matches!(
            self,
            Self::Critical | Self::Down | Self::Unknown | Self::Unreachable | Self::Warning
        )
    }

    /// Critical and down: what quiet hours with `allow_critical` keep
    /// audible.
    pub(crate) fn is_critical(self) -> bool {
        matches!(self, Self::Critical | Self::Down)
    }

    /// Whether `events` enables notifications of this kind. Only the event
    /// labels (acknowledgement, downtime, flapping) can be enabled here.
    pub(crate) fn event_enabled(self, events: EventFilter) -> bool {
        match self {
            Self::Acknowledged | Self::AckCleared => events.acknowledgements,
            Self::Downtime | Self::DowntimeEnded => events.downtimes,
            Self::Flapping | Self::FlappingStopped => events.flapping,
            Self::Critical
            | Self::Down
            | Self::Unknown
            | Self::Unreachable
            | Self::Warning
            | Self::Recovered => false,
        }
    }

    /// The kind part of an event intent's id (`host!service:ack:…`).
    pub(crate) fn event_slug(self) -> &'static str {
        match self {
            Self::Acknowledged => "ack",
            Self::AckCleared => "ack-cleared",
            Self::Downtime => "downtime-start",
            Self::DowntimeEnded => "downtime-end",
            Self::Flapping => "flapping-start",
            Self::FlappingStopped => "flapping-end",
            Self::Critical
            | Self::Down
            | Self::Unknown
            | Self::Unreachable
            | Self::Warning
            | Self::Recovered => "state",
        }
    }
}

/// The state part of a state intent's id (`host!service:critical:…`).
pub(crate) fn state_slug(state: CheckableState) -> &'static str {
    match state {
        CheckableState::Service(ServiceState::Ok) => "ok",
        CheckableState::Service(ServiceState::Warning) => "warning",
        CheckableState::Service(ServiceState::Critical) => "critical",
        CheckableState::Service(ServiceState::Unknown) => "unknown",
        CheckableState::Host(HostState::Up) => "up",
        CheckableState::Host(HostState::Down) => "down",
        CheckableState::Host(HostState::Unreachable) => "unreachable",
        CheckableState::Service(ServiceState::Pending)
        | CheckableState::Host(HostState::Pending) => "pending",
    }
}

/// Id of a state notification: `"{object}:{state}:{since}"`.
pub(crate) fn state_id(object: &ObjectKey, state: CheckableState, since: Timestamp) -> String {
    format!("{object}:{}:{}", state_slug(state), format_timestamp(since))
}

/// Id of an acknowledgement, downtime or flapping notification:
/// `"{object}:{kind}:{at}"`.
pub(crate) fn event_id(object: &ObjectKey, label: Label, at: Timestamp) -> String {
    format!("{object}:{}:{}", label.event_slug(), format_timestamp(at))
}

/// Id of a storm summary: `"storm:{window start}"`.
pub(crate) fn storm_id(started: Timestamp) -> String {
    format!("storm:{}", format_timestamp(started))
}

/// Unix seconds with millisecond precision: precise enough to tell state
/// changes apart, coarse enough to absorb float noise between the event
/// stream and object queries.
fn format_timestamp(timestamp: Timestamp) -> String {
    format!("{:.3}", timestamp.as_unix_seconds())
}

/// `"{LABEL} · {service} on {host}"` for services, `"{LABEL} · {host}"` for
/// hosts, using display names and falling back to object names when a
/// display name is empty.
pub(crate) fn title(
    label: Label,
    object: &ObjectKey,
    host_display: &str,
    service_display: Option<&str>,
) -> String {
    let host = non_empty(host_display).unwrap_or_else(|| object.host_name().as_str());
    match object.as_service() {
        Some(key) => {
            let service = service_display.and_then(non_empty).unwrap_or(&key.name);
            format!("{} · {service} on {host}", label.title())
        }
        None => format!("{} · {host}", label.title()),
    }
}

/// `"{group} / {dashboard}"`, or whichever of the two isn't empty.
pub(crate) fn subtitle(group: &str, dashboard: &str) -> String {
    match (non_empty(group), non_empty(dashboard)) {
        (Some(group), Some(dashboard)) => format!("{group} / {dashboard}"),
        (Some(name), None) | (None, Some(name)) => name.to_owned(),
        (None, None) => String::new(),
    }
}

/// The body for plugin output: its first non-blank line, clipped.
pub(crate) fn output_body(output: &str) -> String {
    let line = output
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    clip(line)
}

/// The body for an acknowledgement or downtime: `"{author}: {comment}"`,
/// clipped.
pub(crate) fn comment_body(author: &str, comment: &str) -> String {
    let (author, comment) = (author.trim(), comment.trim());
    let text = match (author.is_empty(), comment.is_empty()) {
        (false, false) => format!("{author}: {comment}"),
        (true, _) => comment.to_owned(),
        (false, true) => author.to_owned(),
    };
    clip(&text)
}

/// Cuts `text` to [`MAX_BODY_CHARS`] characters, ending with `…` if cut.
fn clip(text: &str) -> String {
    if text.char_indices().nth(MAX_BODY_CHARS).is_none() {
        return text.to_owned();
    }
    // Byte offset of the last character that still fits next to the `…`.
    let cut = text
        .char_indices()
        .nth(MAX_BODY_CHARS - 1)
        .map_or(text.len(), |(offset, _)| offset);
    format!("{}…", text.get(..cut).unwrap_or(text))
}

fn non_empty(text: &str) -> Option<&str> {
    let text = text.trim();
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_use_display_names_and_fall_back_to_object_names() {
        let service = ObjectKey::service("db-prod-03", "postgres-replication");
        assert_eq!(
            title(
                Label::Critical,
                &service,
                "db-prod-03",
                Some("postgres-replication")
            ),
            "CRITICAL · postgres-replication on db-prod-03"
        );
        assert_eq!(
            title(Label::Warning, &service, "DB 3", Some("Replication")),
            "WARNING · Replication on DB 3"
        );
        assert_eq!(
            title(Label::Recovered, &service, " ", None),
            "RECOVERED · postgres-replication on db-prod-03"
        );
        assert_eq!(
            title(Label::Unknown, &service, "", Some("")),
            "UNKNOWN · postgres-replication on db-prod-03"
        );

        let host = ObjectKey::host("k8s-node-07");
        assert_eq!(
            title(Label::Down, &host, "k8s-node-07", None),
            "DOWN · k8s-node-07"
        );
        assert_eq!(
            title(Label::Down, &host, "", Some("ignored")),
            "DOWN · k8s-node-07"
        );
    }

    #[test]
    fn subtitles_join_group_and_dashboard() {
        assert_eq!(
            subtitle("databases", "production"),
            "databases / production"
        );
        assert_eq!(subtitle("", "production"), "production");
        assert_eq!(subtitle("databases", " "), "databases");
        assert_eq!(subtitle("", ""), "");
    }

    #[test]
    fn output_bodies_take_the_first_non_blank_line() {
        assert_eq!(
            output_body("CRITICAL - lag 412s\nprimary db-01\n"),
            "CRITICAL - lag 412s"
        );
        assert_eq!(output_body("\r\n  \nsecond line\r\nthird"), "second line");
        assert_eq!(output_body(""), "");
    }

    #[test]
    fn comment_bodies_name_the_author() {
        assert_eq!(
            comment_body("m.keller", "looking into it"),
            "m.keller: looking into it"
        );
        assert_eq!(comment_body("", "maintenance"), "maintenance");
        assert_eq!(comment_body("m.keller", " "), "m.keller");
        assert_eq!(comment_body("", ""), "");
    }

    #[test]
    fn long_bodies_are_clipped_on_a_char_boundary() {
        let exact = "é".repeat(MAX_BODY_CHARS);
        assert_eq!(output_body(&exact), exact, "exactly the limit stays whole");

        let long = "é".repeat(MAX_BODY_CHARS + 10);
        let body = output_body(&long);
        assert_eq!(body.chars().count(), MAX_BODY_CHARS);
        assert!(body.ends_with('…'));
        assert!(body.starts_with("éé"));

        let one_over = "x".repeat(MAX_BODY_CHARS + 1);
        let body = output_body(&one_over);
        assert_eq!(body.chars().count(), MAX_BODY_CHARS);
        assert!(body.ends_with('…'));
    }

    #[test]
    fn ids_are_stable_and_millisecond_precise() {
        let object = ObjectKey::service("h", "s");
        let since = Timestamp::from_unix_seconds(1_700_000_000.123_456);
        assert_eq!(
            state_id(
                &object,
                CheckableState::Service(ServiceState::Critical),
                since
            ),
            "h!s:critical:1700000000.123"
        );
        assert_eq!(
            state_id(
                &ObjectKey::host("h"),
                CheckableState::Host(HostState::Up),
                since
            ),
            "h:up:1700000000.123"
        );
        assert_eq!(
            event_id(&object, Label::Acknowledged, since),
            "h!s:ack:1700000000.123"
        );
        assert_eq!(storm_id(Timestamp::from_unix_seconds(5.0)), "storm:5.000");
    }

    #[test]
    fn labels_cover_every_state() {
        assert_eq!(
            Label::for_problem(CheckableState::Service(ServiceState::Critical)),
            Some(Label::Critical)
        );
        assert_eq!(
            Label::for_problem(CheckableState::Host(HostState::Unreachable)),
            Some(Label::Unreachable)
        );
        assert_eq!(
            Label::for_problem(CheckableState::Service(ServiceState::Ok)),
            None
        );
        assert_eq!(
            Label::for_problem(CheckableState::Host(HostState::Pending)),
            None
        );
        for (position, label) in Label::ALL.into_iter().enumerate() {
            assert_eq!(label.index(), position);
        }
    }

    #[test]
    fn tones_follow_the_state() {
        assert_eq!(Label::Critical.tone(), Tone::Critical);
        assert_eq!(Label::Down.tone(), Tone::Critical);
        assert_eq!(Label::Warning.tone(), Tone::Warning);
        assert_eq!(Label::Unknown.tone(), Tone::Unknown);
        assert_eq!(Label::Unreachable.tone(), Tone::Unknown);
        assert_eq!(Label::Recovered.tone(), Tone::Recovery);
        assert_eq!(Label::Acknowledged.tone(), Tone::Info);
        assert_eq!(Label::FlappingStopped.tone(), Tone::Info);
    }
}
