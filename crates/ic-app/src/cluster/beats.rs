//! The health page's pinned parts (PLAN.md §4.2 B3, E; mock-ups 16a–16a3,
//! 16r–16u), worked out from a snapshot: the heartbeat row under the
//! health line, each zone's and endpoint's beat in the table's
//! *heartbeat* column, and the alert block (the worst alert in full, the
//! others one line each). Every age and every verdict on time is worked
//! out here, against the UI's clock, from the times the engine reports: a
//! beat past its deadline is late (yellow) even when the engine last said
//! it was on time (no false green).
//!
//! **icygui's own blindness is never the cluster's fault** (PLAN.md §4.2,
//! no false green rule 5): without live data ([`Liveness::live_data`]:
//! the connection lost, the stream or the engine silent) no beat is judged
//! against the clock, since none could have come. The row says *no live
//! data*, the column keeps each beat's last known age (as of the page's
//! *as of*, faint, grey), and only what was proven while live keeps its
//! words: a dead beat (red), one discovery no longer finds. A beat Icinga
//! ran but the stream didn't bring (`Death::NotDelivered`) is *not
//! delivered* (yellow: connected, but nothing arrives), never *dead*. Once
//! live again each beat is judged by the deadline of its grace (the
//! engine publishes deadlines only while live).

use ic_core::heartbeat::{BeatState, Death, Heartbeat, HeartbeatSetup, Heartbeats};
use ic_core::snapshot::Snapshot;
use ic_core::trouble::{Alert, AlertAction, AlertTone};
use ic_model::{Timestamp, format_compact, format_two_units};

use super::health::{Liveness, Tone};

/// How a beat's dot looks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum BeatTone {
    /// On time (green).
    Ok,
    /// Late, or gone (yellow).
    Warning,
    /// Dead (red).
    Critical,
    /// Off, none found, or waiting for its first beat (grey).
    #[default]
    Off,
}

impl BeatTone {
    /// Its dot's colour.
    pub(crate) fn fill(self, theme: &ic_ui_kit::Theme) -> gpui::Hsla {
        match self {
            Self::Ok => theme.states.fill.ok,
            Self::Warning => theme.states.fill.warning,
            Self::Critical => theme.states.fill.critical,
            Self::Off => theme.states.fill.pending,
        }
    }

    /// The text colour of what the row says.
    pub(crate) fn text(self) -> Tone {
        match self {
            Self::Critical => Tone::Critical,
            Self::Warning => Tone::Warning,
            Self::Ok | Self::Off => Tone::Normal,
        }
    }
}

/// The heartbeat row (16a2): `● heartbeats 6 of 6 · on time`,
/// `● heartbeat ams · 1 interval late · 48s`,
/// `● heartbeat fra · dead · last 02:11`, `● heartbeats 5 of 6 · fra
/// disappeared`, `● heartbeats · off`, `● heartbeats · none found`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct BeatRow {
    pub(crate) tone: BeatTone,
    /// `heartbeats`, or `heartbeat` when one beat is named.
    pub(crate) label: &'static str,
    /// `6 of 6`, `ams`, or nothing.
    pub(crate) subject: String,
    /// `on time`, `1 interval late`, `dead · last 02:11`, `off`.
    pub(crate) status: String,
    /// How long since a late beat's last one (`48s`).
    pub(crate) age: Option<String>,
    /// Whether the policy word shows (not while off or none is found).
    pub(crate) watched: bool,
}

/// A beat in the table's *heartbeat* column: a dot and its age (`● 8s`,
/// `● 6m 12s`, `● disappeared`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BeatCell {
    pub(crate) tone: BeatTone,
    pub(crate) text: String,
    /// Without live data: the last known age (drawn faint).
    pub(crate) last_known: bool,
}

/// An alert of the block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AlertLine {
    pub(crate) tone: Tone,
    pub(crate) title: String,
    pub(crate) detail: String,
    /// The link (the worst alert's only): its words and what it does.
    pub(crate) link: Option<(String, AlertAction)>,
    /// `since 02:14`.
    pub(crate) since: String,
}

/// Whether a beat the engine last called on time (or waiting) is past its
/// deadline by the UI's clock: late, whatever the engine says.
pub(crate) fn overdue(beat: &Heartbeat, now: Timestamp) -> bool {
    matches!(beat.state, BeatState::OnTime | BeatState::Waiting)
        && beat.deadline.is_some_and(|deadline| deadline < now)
}

/// Whether a beat is late at `now`: the engine says so, or its deadline
/// passed.
fn late(beat: &Heartbeat, now: Timestamp) -> bool {
    matches!(beat.state, BeatState::Late | BeatState::Checking) || overdue(beat, now)
}

/// Whether a beat is dead by the cluster's doing: Icinga stopped running
/// it, or its result isn't OK. One Icinga ran but the stream didn't bring
/// is icygui's blindness ([`undelivered`]).
pub(crate) fn dead_in_cluster(beat: &Heartbeat) -> bool {
    matches!(beat.state, BeatState::Dead(Death::NotOk | Death::Stopped))
}

/// Whether Icinga ran a beat but the stream didn't bring it, even after a
/// reconnect: icygui can't see it (connected, but nothing arrives).
fn undelivered(beat: &Heartbeat) -> bool {
    beat.state == BeatState::Dead(Death::NotDelivered)
}

/// A beat's tone at `now`, with live data.
pub(crate) fn tone_of(beat: &Heartbeat, now: Timestamp) -> BeatTone {
    if overdue(beat, now) {
        return BeatTone::Warning;
    }
    match beat.state {
        BeatState::OnTime => BeatTone::Ok,
        BeatState::Late
        | BeatState::Checking
        | BeatState::Disappeared
        | BeatState::NotFound
        | BeatState::Dead(Death::NotDelivered) => BeatTone::Warning,
        BeatState::Dead(Death::NotOk | Death::Stopped) => BeatTone::Critical,
        BeatState::Waiting => BeatTone::Off,
    }
}

/// A beat's tone without live data: only what was proven while live (a
/// dead beat, one that disappeared) keeps its colour; nothing about time
/// can be judged, so the rest is grey.
fn last_known_tone(beat: &Heartbeat) -> BeatTone {
    match beat.state {
        BeatState::Dead(Death::NotOk | Death::Stopped) => BeatTone::Critical,
        BeatState::Disappeared | BeatState::NotFound => BeatTone::Warning,
        _ => BeatTone::Off,
    }
}

/// A beat's tone as the page shows it: judged with live data, its green
/// grey while what the page shows isn't current (the endpoints' numbers
/// old); the last known one without live data.
pub(crate) fn shown_tone(beat: &Heartbeat, live: Liveness, now: Timestamp) -> BeatTone {
    if !live.live_data() {
        return last_known_tone(beat);
    }
    match tone_of(beat, now) {
        BeatTone::Ok if !live.is_live() => BeatTone::Off,
        tone => tone,
    }
}

/// How long ago a beat's last one came (`8s`, `6m 12s`), or `—`.
fn age_of(beat: &Heartbeat, now: Timestamp) -> String {
    beat.last_beat.map_or_else(
        || "—".to_owned(),
        |at| format_two_units(at.elapsed_until(now)),
    )
}

/// A beat's age as the page shows it: against the UI's clock with live
/// data; without, the last known one, as of when live data last came (the
/// stream's last line, or the endpoints' states if later: it doesn't
/// count up while nothing can arrive).
pub(crate) fn shown_age(beat: &Heartbeat, live: Liveness, now: Timestamp) -> String {
    if live.live_data() {
        age_of(beat, now)
    } else {
        let as_of = live
            .last_live()
            .filter(|at| at.as_unix_seconds() < now.as_unix_seconds())
            .unwrap_or(now);
        age_of(beat, as_of)
    }
}

/// A beat's cell in the table.
pub(crate) fn cell(beat: &Heartbeat, live: Liveness, now: Timestamp) -> BeatCell {
    let last_known = !live.live_data();
    let text = match beat.state {
        BeatState::Disappeared => "disappeared".to_owned(),
        BeatState::NotFound => "not found".to_owned(),
        BeatState::Dead(Death::NotDelivered) if !last_known => "not delivered".to_owned(),
        _ => shown_age(beat, live, now),
    };
    BeatCell {
        tone: shown_tone(beat, live, now),
        text,
        last_known,
    }
}

/// The beat that proves endpoint `name`, as its cell.
pub(crate) fn endpoint_cell(
    beats: &Heartbeats,
    name: &str,
    live: Liveness,
    now: Timestamp,
) -> Option<BeatCell> {
    beats.of_endpoint(name).map(|beat| cell(beat, live, now))
}

/// The beat that proves zone `name`, as its cell.
pub(crate) fn zone_cell(
    beats: &Heartbeats,
    name: &str,
    live: Liveness,
    now: Timestamp,
) -> Option<BeatCell> {
    beats.of_zone(name).map(|beat| cell(beat, live, now))
}

/// The heartbeat row as the page shows it. With live data, judged at
/// `now` (its green grey while the endpoints' numbers are old). Without,
/// *no live data* in grey, never *late* (icygui's blindness is not the
/// cluster's fault, rule 5): only a dead beat (red) and what discovery no
/// longer finds keep their words, as proven while live.
pub(crate) fn row_when(beats: &Heartbeats, live: Liveness, now: Timestamp) -> BeatRow {
    let row = row(beats, now);
    if live.live_data() {
        return match row.tone {
            BeatTone::Ok if !live.is_live() => BeatRow {
                tone: BeatTone::Off,
                ..row
            },
            _ => row,
        };
    }
    // Off, none found, or a dead beat: the row's words hold.
    if !row.watched || row.tone == BeatTone::Critical {
        return row;
    }
    let findings: Vec<String> = beats
        .beats
        .iter()
        .filter_map(|beat| match beat.state {
            BeatState::Disappeared => Some(format!("{} disappeared", beat.proves.subject())),
            BeatState::NotFound => Some(format!("{} not found", beat.object())),
            _ => None,
        })
        .collect();
    BeatRow {
        tone: if findings.is_empty() {
            BeatTone::Off
        } else {
            BeatTone::Warning
        },
        label: "heartbeats",
        subject: String::new(),
        status: std::iter::once("no live data".to_owned())
            .chain(findings)
            .collect::<Vec<_>>()
            .join(" · "),
        age: None,
        watched: true,
    }
}

/// The last beat of `beats` as a clock time (`last 02:11`).
fn last_of<'a>(beats: impl Iterator<Item = &'a Heartbeat>, now: Timestamp) -> String {
    beats
        .filter_map(|beat| beat.last_beat)
        .max_by(|a, b| a.as_unix_seconds().total_cmp(&b.as_unix_seconds()))
        .map_or_else(String::new, |at| {
            format!(" · last {}", crate::format::list_clock(at, now))
        })
}

/// The heartbeat row for `beats` at `now`.
#[expect(
    clippy::too_many_lines,
    reason = "every state of the summary row, worst first, with its words"
)]
pub(crate) fn row(beats: &Heartbeats, now: Timestamp) -> BeatRow {
    if beats.setup == HeartbeatSetup::Off {
        return BeatRow {
            tone: BeatTone::Off,
            label: "heartbeats",
            status: "off".to_owned(),
            ..BeatRow::default()
        };
    }
    let watched: Vec<&Heartbeat> = beats.watched().collect();
    let gone: Vec<&Heartbeat> = beats
        .beats
        .iter()
        .filter(|beat| beat.state == BeatState::Disappeared)
        .collect();
    let missing: Vec<&Heartbeat> = beats
        .beats
        .iter()
        .filter(|beat| beat.state == BeatState::NotFound)
        .collect();
    if watched.is_empty() && gone.is_empty() && missing.is_empty() {
        return BeatRow {
            tone: BeatTone::Off,
            label: "heartbeats",
            status: "none found".to_owned(),
            ..BeatRow::default()
        };
    }
    let total = watched.len() + gone.len() + missing.len();
    let beating = watched
        .iter()
        .filter(|beat| {
            matches!(beat.state, BeatState::OnTime | BeatState::Waiting) && !overdue(beat, now)
        })
        .count();
    let count = format!("{beating} of {total}");
    let dead: Vec<&Heartbeat> = watched
        .iter()
        .copied()
        .filter(|beat| dead_in_cluster(beat))
        .collect();
    let undelivered: Vec<&Heartbeat> = watched
        .iter()
        .copied()
        .filter(|beat| undelivered(beat))
        .collect();
    let late: Vec<&Heartbeat> = watched
        .iter()
        .copied()
        .filter(|beat| late(beat, now))
        .collect();
    let named = |beats: &[&Heartbeat]| beats.len() == 1 && total > 1;
    if !dead.is_empty() {
        let last = last_of(dead.iter().copied(), now);
        return if named(&dead) {
            BeatRow {
                tone: BeatTone::Critical,
                label: "heartbeat",
                subject: dead[0].proves.subject().to_owned(),
                status: format!("dead{last}"),
                age: None,
                watched: true,
            }
        } else {
            BeatRow {
                tone: BeatTone::Critical,
                label: "heartbeats",
                subject: count,
                status: if dead.len() == total {
                    format!("dead{last}")
                } else {
                    format!("{} dead{last}", dead.len())
                },
                age: None,
                watched: true,
            }
        };
    }
    if !late.is_empty() {
        let beat = late[0];
        let missed = beat.last_beat.map_or(1, |at| {
            let interval = beat.interval.as_secs_f64().max(1.0);
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a small, positive count of intervals"
            )]
            let missed = (at.elapsed_until(now).as_secs_f64() / interval)
                .floor()
                .max(1.0) as u64;
            missed
        });
        let intervals = if missed == 1 {
            "1 interval late".to_owned()
        } else {
            format!("{missed} intervals late")
        };
        return if named(&late) {
            BeatRow {
                tone: BeatTone::Warning,
                label: "heartbeat",
                subject: beat.proves.subject().to_owned(),
                status: intervals,
                age: beat
                    .last_beat
                    .map(|at| format_compact(at.elapsed_until(now))),
                watched: true,
            }
        } else {
            BeatRow {
                tone: BeatTone::Warning,
                label: "heartbeats",
                subject: count,
                status: format!("{} late", late.len()),
                age: None,
                watched: true,
            }
        };
    }
    if !undelivered.is_empty() {
        // Icinga ran them; icygui doesn't see them: not the cluster's
        // fault (the banner says why).
        return if named(&undelivered) {
            BeatRow {
                tone: BeatTone::Warning,
                label: "heartbeat",
                subject: undelivered[0].proves.subject().to_owned(),
                status: "not delivered".to_owned(),
                age: None,
                watched: true,
            }
        } else {
            BeatRow {
                tone: BeatTone::Warning,
                label: "heartbeats",
                subject: count,
                status: if undelivered.len() == total {
                    "not delivered".to_owned()
                } else {
                    format!("{} not delivered", undelivered.len())
                },
                age: None,
                watched: true,
            }
        };
    }
    if !gone.is_empty() || !missing.is_empty() {
        let findings: Vec<String> = gone
            .iter()
            .map(|beat| format!("{} disappeared", beat.proves.subject()))
            .chain(
                missing
                    .iter()
                    .map(|beat| format!("{} not found", beat.object())),
            )
            .collect();
        return BeatRow {
            tone: BeatTone::Warning,
            label: "heartbeats",
            subject: count,
            status: findings.join(" · "),
            age: None,
            watched: true,
        };
    }
    let waiting = watched.iter().all(|beat| beat.state == BeatState::Waiting);
    BeatRow {
        tone: if waiting { BeatTone::Off } else { BeatTone::Ok },
        label: "heartbeats",
        subject: count,
        status: if waiting && watched.iter().any(|beat| beat.last_beat.is_some()) {
            // After a reconnect or a wake-up: their grace.
            "waiting for the next beats".to_owned()
        } else if waiting {
            "waiting for the first beats".to_owned()
        } else if beats.polled {
            "on time · read every interval".to_owned()
        } else {
            "on time".to_owned()
        },
        age: None,
        watched: true,
    }
}

/// The alert block's lines, worst first (the engine's order).
pub(crate) fn alert_lines(snapshot: &Snapshot, now: Timestamp) -> Vec<AlertLine> {
    snapshot
        .trouble
        .alerts
        .iter()
        .enumerate()
        .map(|(index, alert)| line(alert, index == 0, now))
        .collect()
}

fn line(alert: &Alert, worst: bool, now: Timestamp) -> AlertLine {
    let link = alert
        .action
        .clone()
        .filter(|_| worst)
        .map(|action| (link_words(&action), action));
    AlertLine {
        tone: match alert.tone {
            AlertTone::Critical => Tone::Critical,
            AlertTone::Warning => Tone::Warning,
        },
        title: alert.title.clone(),
        detail: alert.detail.clone(),
        link,
        since: format!("since {}", crate::format::list_clock(alert.since, now)),
    }
}

/// What an alert's link says.
fn link_words(action: &AlertAction) -> String {
    match action {
        AlertAction::LateChecks { .. } => "show the late checks".to_owned(),
        AlertAction::ShowNode(node) => format!("show {node}"),
        AlertAction::Settings => "settings".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use ic_core::heartbeat::{Death, Proves};
    use ic_model::ServiceKey;

    use super::*;

    const NOW: f64 = 1_800_000_000.0;

    fn beat(subject: &str, state: BeatState, ago: f64) -> Heartbeat {
        Heartbeat {
            key: ServiceKey::new(&format!("icygui-hb-{subject}"), "beat"),
            proves: Proves::Zone(subject.to_owned()),
            interval: Duration::from_secs(30),
            state,
            last_beat: Some(Timestamp::from_unix_seconds(NOW - ago)),
            last_check: None,
            since: Timestamp::from_unix_seconds(NOW - ago),
            deadline: None,
            allowance: Duration::from_secs(5),
            reason: None,
        }
    }

    fn beats(list: Vec<Heartbeat>) -> Heartbeats {
        Heartbeats {
            setup: HeartbeatSetup::Find {
                variable: "icygui_heartbeat".to_owned(),
            },
            beats: list,
            polled: false,
        }
    }

    fn now() -> Timestamp {
        Timestamp::from_unix_seconds(NOW)
    }

    #[test]
    fn the_row_says_what_the_beats_do_in_one_line() {
        let ok = beats(vec![
            beat("ams", BeatState::OnTime, 8.0),
            beat("fra", BeatState::OnTime, 3.0),
        ]);
        let row = row(&ok, now());
        assert_eq!((row.tone, row.label), (BeatTone::Ok, "heartbeats"));
        assert_eq!(
            (row.subject.as_str(), row.status.as_str()),
            ("2 of 2", "on time")
        );

        let late = beats(vec![
            beat("ams", BeatState::Late, 48.0),
            beat("fra", BeatState::OnTime, 3.0),
        ]);
        let row = super::row(&late, now());
        assert_eq!((row.tone, row.label), (BeatTone::Warning, "heartbeat"));
        assert_eq!(row.subject, "ams");
        assert_eq!(row.status, "1 interval late");
        assert_eq!(row.age.as_deref(), Some("48s"));

        let dead = beats(vec![
            beat("ams", BeatState::Dead(Death::Stopped), 400.0),
            beat("fra", BeatState::Dead(Death::Stopped), 380.0),
        ]);
        let row = super::row(&dead, now());
        assert_eq!(row.tone, BeatTone::Critical);
        assert_eq!(row.subject, "0 of 2");
        assert!(row.status.starts_with("dead · last "), "{}", row.status);

        let mut gone = beat("fra", BeatState::Disappeared, 0.0);
        gone.last_beat = None;
        let row = super::row(
            &beats(vec![beat("ams", BeatState::OnTime, 8.0), gone]),
            now(),
        );
        assert_eq!(row.tone, BeatTone::Warning);
        assert_eq!(
            (row.subject.as_str(), row.status.as_str()),
            ("1 of 2", "fra disappeared")
        );

        let off = Heartbeats::default();
        assert_eq!(super::row(&off, now()).status, "off");
        let none = beats(Vec::new());
        assert_eq!(super::row(&none, now()).status, "none found");
        assert!(!super::row(&none, now()).watched);
    }

    #[test]
    fn cells_age_against_the_ui_clock() {
        let list = beats(vec![beat("ams", BeatState::OnTime, 21.0)]);
        let cell = zone_cell(&list, "ams", Liveness::Live, now()).unwrap();
        assert_eq!((cell.tone, cell.text.as_str()), (BeatTone::Ok, "21s"));
        let dead = beat("ams", BeatState::Dead(Death::NotOk), 372.0);
        assert_eq!(super::cell(&dead, Liveness::Live, now()).text, "6m 12s");
        assert!(endpoint_cell(&list, "master-01", Liveness::Live, now()).is_none());
    }

    /// No false green: an on-time beat whose deadline passed is late by the
    /// UI's clock (a stuck engine says nothing new), and the row says so.
    #[test]
    fn a_beat_past_its_deadline_is_late_whatever_the_engine_last_said() {
        let mut stuck = beat("ams", BeatState::OnTime, 270.0);
        stuck.deadline = Some(Timestamp::from_unix_seconds(NOW - 235.0));
        assert!(overdue(&stuck, now()));
        let shown = cell(&stuck, Liveness::Live, now());
        assert_eq!(
            (shown.tone, shown.text.as_str()),
            (BeatTone::Warning, "4m 30s")
        );
        let one = row(&beats(vec![stuck.clone()]), now());
        assert_eq!(one.tone, BeatTone::Warning);
        assert_eq!(one.status, "1 late");
        let two = beats(vec![stuck, beat("fra", BeatState::OnTime, 3.0)]);
        let named = row(&two, now());
        assert_eq!((named.label, named.subject.as_str()), ("heartbeat", "ams"));
        assert_eq!(named.status, "9 intervals late");
        assert_eq!(named.age.as_deref(), Some("4m"));
    }

    /// Without live data nothing on time is green: the cells turn grey and
    /// the row says so; what is dead stays red.
    #[test]
    fn without_live_data_no_beat_is_green() {
        let stale = Liveness::Stale {
            as_of: None,
            heard: None,
            link: super::super::health::Link::Lost(Tone::Warning),
        };
        let list = beats(vec![
            beat("ams", BeatState::OnTime, 3.0),
            beat("fra", BeatState::OnTime, 8.0),
        ]);
        let row = row_when(&list, stale, now());
        assert_eq!(row.tone, BeatTone::Off);
        assert_eq!(row.status, "no live data");
        assert_eq!(
            zone_cell(&list, "ams", stale, now()).unwrap().tone,
            BeatTone::Off
        );
        let dead = beats(vec![beat("ams", BeatState::Dead(Death::Stopped), 400.0)]);
        assert_eq!(row_when(&dead, stale, now()).tone, BeatTone::Critical);
    }

    /// Rule 5: icygui's own blindness is never the cluster's fault. While
    /// the connection is lost (or the stream or engine is silent) no beat
    /// is late, whatever its deadline or the engine last said: the row says
    /// *no live data*, the cells keep their last known age (as of the
    /// page's *as of*, not counting up) in grey. With live data again, and
    /// while only the endpoints' numbers are old, they are judged.
    #[test]
    fn without_live_data_no_beat_is_late() {
        use super::super::health::Link;
        let mut overdue = beat("ams", BeatState::OnTime, 230.0);
        overdue.deadline = Some(Timestamp::from_unix_seconds(NOW - 195.0));
        let late = beat("fra", BeatState::Late, 400.0);
        let list = beats(vec![overdue.clone(), late.clone()]);
        let as_of = Some(Timestamp::from_unix_seconds(NOW - 200.0));
        for link in [
            Link::Lost(Tone::Critical),
            Link::Silent(Tone::Warning),
            Link::Silent(Tone::Critical),
        ] {
            let blind = Liveness::Stale {
                as_of,
                heard: None,
                link,
            };
            let row = row_when(&list, blind, now());
            assert_eq!(
                (row.tone, row.status.as_str(), row.age.as_ref()),
                (BeatTone::Off, "no live data", None),
                "{link:?}"
            );
            let ams = cell(&overdue, blind, now());
            assert_eq!(
                (ams.tone, ams.text.as_str(), ams.last_known),
                (BeatTone::Off, "30s", true),
                "the age as of the last known states: {link:?}"
            );
            let fra = cell(&late, blind, now());
            assert_eq!((fra.tone, fra.text.as_str()), (BeatTone::Off, "3m 20s"));
            // Later in the outage: the same.
            let later = Timestamp::from_unix_seconds(NOW + 600.0);
            assert_eq!(cell(&overdue, blind, later).text, "30s");
            assert_eq!(row_when(&list, blind, later).status, "no live data");
            assert_eq!(shown_tone(&late, blind, later), BeatTone::Off);
        }
        // The stream was heard after the endpoints' states came (beats
        // arrive between status polls): the ages count to its last line,
        // so a beat that came after the last poll doesn't read 0s.
        let heard_later = Liveness::Stale {
            as_of,
            heard: Some(Timestamp::from_unix_seconds(NOW - 100.0)),
            link: Link::Lost(Tone::Critical),
        };
        let fresh = beat("fra", BeatState::OnTime, 150.0);
        assert_eq!(cell(&fresh, heard_later, now()).text, "50s");
        assert_eq!(cell(&overdue, heard_later, now()).text, "2m 10s");
        // Proven while live: a dead beat stays red, a disappeared one keeps
        // its words (after *no live data*).
        let blind = Liveness::Stale {
            as_of,
            heard: None,
            link: Link::Lost(Tone::Critical),
        };
        let mut gone = beat("sat-fra-02", BeatState::Disappeared, 0.0);
        gone.last_beat = None;
        let row = row_when(&beats(vec![overdue.clone(), gone]), blind, now());
        assert_eq!(
            (row.tone, row.status.as_str()),
            (BeatTone::Warning, "no live data · sat-fra-02 disappeared")
        );
        let dead = beat("ams", BeatState::Dead(Death::NotOk), 400.0);
        assert_eq!(cell(&dead, blind, now()).tone, BeatTone::Critical);
        // Live data (or only the endpoints' numbers old): judged again.
        let row = row_when(&list, Liveness::Live, now());
        assert_eq!(
            (row.tone, row.status.as_str()),
            (BeatTone::Warning, "2 late")
        );
        let numbers_old = Liveness::Stale {
            as_of,
            heard: None,
            link: Link::Up,
        };
        assert_eq!(row_when(&list, numbers_old, now()).status, "2 late");
        assert_eq!(cell(&overdue, numbers_old, now()).tone, BeatTone::Warning);
        assert!(!cell(&overdue, numbers_old, now()).last_known);
        // An on-time row while the numbers are old: on time, but not green.
        let fine = beats(vec![beat("ams", BeatState::OnTime, 3.0)]);
        let row = row_when(&fine, numbers_old, now());
        assert_eq!((row.tone, row.status.as_str()), (BeatTone::Off, "on time"));
    }

    /// A beat Icinga ran that the stream didn't bring is icygui's
    /// blindness, not the cluster's fault: *not delivered*, yellow, never
    /// *dead* (and its zone isn't cut off for it).
    #[test]
    fn an_undelivered_beat_is_not_dead() {
        let undelivered = beat("ams", BeatState::Dead(Death::NotDelivered), 400.0);
        assert!(!dead_in_cluster(&undelivered));
        let list = beats(vec![
            undelivered.clone(),
            beat("fra", BeatState::OnTime, 3.0),
        ]);
        let row = row(&list, now());
        assert_eq!(
            (
                row.tone,
                row.label,
                row.subject.as_str(),
                row.status.as_str()
            ),
            (BeatTone::Warning, "heartbeat", "ams", "not delivered")
        );
        let shown = cell(&undelivered, Liveness::Live, now());
        assert_eq!(
            (shown.tone, shown.text.as_str()),
            (BeatTone::Warning, "not delivered")
        );
        let all = beats(vec![undelivered]);
        assert_eq!(super::row(&all, now()).status, "not delivered");
    }
}
