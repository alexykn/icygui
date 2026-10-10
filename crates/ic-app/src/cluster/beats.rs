//! The health page's pinned parts (PLAN.md §4.2 B3, E; mock-ups 16a–16a3,
//! 16r–16u), worked out from a snapshot: the heartbeat row under the
//! health line, each zone's and endpoint's beat in the table's
//! *heartbeat* column, and the alert block (the worst alert in full, the
//! others one line each). Every age and every verdict on time is worked
//! out here, against the UI's clock, from the times the engine reports: a
//! beat past its deadline is late (yellow) even when the engine, stuck or
//! offline, last said it was on time (no false green).

use ic_core::heartbeat::{BeatState, Heartbeat, HeartbeatSetup, Heartbeats};
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

/// A beat's tone at `now`.
pub(crate) fn tone_of(beat: &Heartbeat, now: Timestamp) -> BeatTone {
    if overdue(beat, now) {
        return BeatTone::Warning;
    }
    match beat.state {
        BeatState::OnTime => BeatTone::Ok,
        BeatState::Late | BeatState::Checking | BeatState::Disappeared | BeatState::NotFound => {
            BeatTone::Warning
        }
        BeatState::Dead(_) => BeatTone::Critical,
        BeatState::Waiting => BeatTone::Off,
    }
}

/// A tone without live data: on time turns grey (not proven now).
fn stale(tone: BeatTone, live: Liveness) -> BeatTone {
    match tone {
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

/// A beat's cell in the table.
pub(crate) fn cell(beat: &Heartbeat, live: Liveness, now: Timestamp) -> BeatCell {
    let text = match beat.state {
        BeatState::Disappeared => "disappeared".to_owned(),
        BeatState::NotFound => "not found".to_owned(),
        _ => age_of(beat, now),
    };
    BeatCell {
        tone: stale(tone_of(beat, now), live),
        text,
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

/// The heartbeat row while what the page shows may not be current: an
/// on-time or waiting row says *no live data* in grey (nothing proves the
/// beats now); a late, dead or gone one keeps its words.
pub(crate) fn row_when(beats: &Heartbeats, live: Liveness, now: Timestamp) -> BeatRow {
    let row = row(beats, now);
    if live.is_live() || !row.watched || !matches!(row.tone, BeatTone::Ok | BeatTone::Off) {
        return row;
    }
    BeatRow {
        tone: BeatTone::Off,
        label: "heartbeats",
        subject: String::new(),
        status: "no live data".to_owned(),
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
        .filter(|beat| beat.state.is_dead())
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
}
