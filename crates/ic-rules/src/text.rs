//! What notifications say: labels, titles, bodies, tones and ids.
//!
//! Everything that ends up in a notification's text goes through
//! [`clean`]: plugin output is the least trusted input (passive results,
//! remote agents), and names come from config files that can be shared.

use std::iter::Peekable;
use std::str::Chars;

use ic_model::{CheckableState, HostState, ObjectKey, ServiceState, Timestamp};

use crate::intent::Tone;
use crate::settings::EventFilter;

/// Longest text, in characters, of any part of a notification. Plugin
/// output can be huge; OS notifications show a few lines at most, and the
/// notification centre a single line.
const MAX_TEXT_CHARS: usize = 400;

const ESCAPE: char = '\u{1b}';

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
    let host = non_empty(host_display).unwrap_or_else(|| clean(object.host_name().as_str()));
    match object.as_service() {
        Some(key) => {
            let service = service_display
                .and_then(non_empty)
                .unwrap_or_else(|| clean(&key.name));
            format!("{} · {service} on {host}", label.title())
        }
        None => format!("{} · {host}", label.title()),
    }
}

/// `"{group} / {dashboard}"`, or whichever of the two isn't empty.
pub(crate) fn subtitle(group: &str, dashboard: &str) -> String {
    match (non_empty(group), non_empty(dashboard)) {
        (Some(group), Some(dashboard)) => format!("{group} / {dashboard}"),
        (Some(name), None) | (None, Some(name)) => name,
        (None, None) => String::new(),
    }
}

/// The body for plugin output: its first non-blank line, [cleaned](clean).
/// Any line break counts, including a lone `\r` and Unicode's line and
/// paragraph separators.
pub(crate) fn output_body(output: &str) -> String {
    output
        .split(is_line_break)
        .map(clean)
        .find(|line| !line.is_empty())
        .unwrap_or_default()
}

/// The body for an acknowledgement or downtime: `"{author}: {comment}"`,
/// [cleaned](clean) (a multi-line comment becomes one line).
pub(crate) fn comment_body(author: &str, comment: &str) -> String {
    let (author, comment) = (clean(author), clean(comment));
    let text = match (author.is_empty(), comment.is_empty()) {
        (false, false) => format!("{author}: {comment}"),
        (true, _) => comment,
        (false, true) => author,
    };
    // Each part fits, but both together may not.
    clean(&text)
}

/// Makes untrusted text fit for a notification:
///
/// - control characters (C0, DEL and C1, line breaks included) become
///   spaces, except that ANSI escape sequences (`ESC [ … m`) disappear;
///   D-Bus rejects a NUL, and escapes are noise outside a terminal;
/// - bidirectional formatting characters disappear, so output can't
///   reorder what the rest of the line appears to say;
/// - surrounding whitespace is trimmed;
/// - the result is cut to [`MAX_TEXT_CHARS`] characters, ending with `…`
///   when cut.
///
/// It reads only as far as it needs, so huge input costs little.
pub(crate) fn clean(text: &str) -> String {
    let mut out = String::new();
    let mut kept = 0_usize;
    let mut cut = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        let c = match c {
            ESCAPE => {
                skip_escape_sequence(&mut chars);
                continue;
            }
            c if is_bidi_control(c) => continue,
            c if c.is_control() => ' ',
            c => c,
        };
        if kept == 0 && c.is_whitespace() {
            continue;
        }
        if kept == MAX_TEXT_CHARS {
            if c.is_whitespace() {
                // Only more text after it makes this a cut.
                continue;
            }
            cut = true;
            break;
        }
        out.push(c);
        kept += 1;
    }
    if cut {
        // Make room for the ellipsis.
        out.pop();
    }
    out.truncate(out.trim_end().len());
    if cut {
        out.push('…');
    }
    out
}

/// After an `ESC`: drops a CSI sequence (`[`, parameter and intermediate
/// bytes, one final byte). Any other escape loses only its `ESC`.
fn skip_escape_sequence(chars: &mut Peekable<Chars<'_>>) {
    if chars.next_if_eq(&'[').is_none() {
        return;
    }
    while chars
        .next_if(|c| ('\u{20}'..='\u{3f}').contains(c))
        .is_some()
    {}
    chars.next_if(|c| ('\u{40}'..='\u{7e}').contains(c));
}

/// Unicode's `Bidi_Control` characters: marks, embeddings, overrides and
/// isolates.
fn is_bidi_control(c: char) -> bool {
    matches!(
        c,
        '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
    )
}

/// Unicode's mandatory line breaks: LF, CR, VT, FF, NEL and the line and
/// paragraph separators.
fn is_line_break(c: char) -> bool {
    matches!(
        c,
        '\n' | '\r' | '\u{0b}' | '\u{0c}' | '\u{85}' | '\u{2028}' | '\u{2029}'
    )
}

/// `text` [cleaned](clean), or `None` if nothing is left.
fn non_empty(text: &str) -> Option<String> {
    let text = clean(text);
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
        assert_eq!(output_body("first\rsecond"), "first", "a lone CR breaks");
        assert_eq!(output_body("\u{2028}first\u{2029}second"), "first");
        assert_eq!(output_body(" \t\u{0}\n\u{1b}[0m\nthird"), "third");
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
        assert_eq!(
            comment_body("m.keller", "on it\nticket INC-1"),
            "m.keller: on it ticket INC-1"
        );
        let long = comment_body("m.keller", &"x".repeat(MAX_TEXT_CHARS));
        assert_eq!(long.chars().count(), MAX_TEXT_CHARS);
        assert!(long.starts_with("m.keller: x") && long.ends_with('…'));
    }

    #[test]
    fn long_bodies_are_clipped_on_a_char_boundary() {
        let exact = "é".repeat(MAX_TEXT_CHARS);
        assert_eq!(output_body(&exact), exact, "exactly the limit stays whole");
        let trailing = format!("{exact}   \n");
        assert_eq!(output_body(&trailing), exact, "trailing blanks don't cut");

        let long = "é".repeat(MAX_TEXT_CHARS + 10);
        let body = output_body(&long);
        assert_eq!(body.chars().count(), MAX_TEXT_CHARS);
        assert!(body.ends_with('…'));
        assert!(body.starts_with("éé"));

        let one_over = "x".repeat(MAX_TEXT_CHARS + 1);
        let body = output_body(&one_over);
        assert_eq!(body.chars().count(), MAX_TEXT_CHARS);
        assert!(body.ends_with('…'));

        let huge = format!("{}\nsecond", "y".repeat(2_000_000));
        assert_eq!(output_body(&huge).chars().count(), MAX_TEXT_CHARS);
    }

    #[test]
    fn control_and_bidi_characters_never_reach_a_notification() {
        assert_eq!(
            clean("CRIT\0ICAL \u{1b}[31mred\u{1b}[0m \u{202e}KO\ttail\u{7f}\u{9b}end"),
            "CRIT ICAL red KO tail  end"
        );
        assert_eq!(clean("a\u{1b}[1;31;40mb"), "ab", "CSI with parameters");
        assert_eq!(
            clean("a\u{1b}]0;x\u{7}b"),
            "a]0;x b",
            "other escapes lose ESC"
        );
        assert_eq!(clean("trailing escape\u{1b}"), "trailing escape");
        assert_eq!(clean("\u{1b}["), "");
        for bidi in [
            '\u{061c}', '\u{200e}', '\u{200f}', '\u{202a}', '\u{202b}', '\u{202c}', '\u{202d}',
            '\u{202e}', '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}',
        ] {
            assert_eq!(clean(&format!("a{bidi}b")), "ab", "{bidi:?}");
        }
        assert_eq!(clean("  שלום  "), "שלום", "right-to-left text itself stays");

        let body = output_body("CRIT\0ICAL \u{1b}[31mred\u{1b}[0m \u{202e}KO\rtail");
        assert_eq!(body, "CRIT ICAL red KO");
        assert!(!body.chars().any(char::is_control));
    }

    #[test]
    fn titles_and_subtitles_are_cleaned() {
        let service = ObjectKey::service("db\u{0}03", "pg\u{202e}lag");
        assert_eq!(
            title(Label::Critical, &service, "", None),
            "CRITICAL · pglag on db 03"
        );
        assert_eq!(
            title(Label::Critical, &service, "DB\n3", Some("\u{1b}[1mlag")),
            "CRITICAL · lag on DB 3"
        );
        assert_eq!(
            subtitle("data\u{0}bases", "\u{2066}prod"),
            "data bases / prod"
        );
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
