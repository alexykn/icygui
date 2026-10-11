//! The words of an entry (topic 14, `threads.js` `tEntry`), shared by the
//! handling and downtimes views and the object's pane: the kind's icon and
//! word, the author, the time and the details, the text, and the two
//! fixed tag slots (A: `sticky` or the progress line; B: the expiry, the
//! time left or when it starts). Also the timeline's bars and its axis.
//!
//! As drawn: `in downtime  j.berg  12:41  fixed 13:00 → 16:00` / `Failover
//! drill on db-prod-01 (CHG-4473)…` / slot B `1h 48m left` in the accent;
//! `acknowledged  dba-oncall  13:12` / slot B `expires 15:00, in 48m` in
//! the warning colour within 2 hours; `downtime, upcoming  m.keller  13:57
//! flexible 1h · window 22:00 → 06:00 · host only` / `by 22:00`;
//! `downtime, from config  weekly-patching  fixed · Sat 06:00 → 10:00` /
//! `Sat 06:00`; a comment `m.keller  14:09` with no kind word.

use std::fmt::Display;

use chrono::{Local, TimeZone, Timelike as _};
use ic_core::snapshot::Snapshot;
use ic_model::{AckKind, Downtime, DowntimePhase, ObjectKey, Timestamp};
use ic_ui_kit::IconName;

use super::model::{
    ack_comment, compact_window, day_clock, first_line, length, same_day, short_when,
};
use super::threads::{Entry, EntryKind, Record, SOON};
use crate::downtimes;

/// The tone of a slot's words.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Tone {
    /// Faint.
    #[default]
    Faint,
    /// The accent: a downtime in effect.
    Accent,
    /// The warning colour: an acknowledgement expiring within 2 hours.
    Warning,
}

/// Slot A of an entry's tag.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) enum SlotA {
    /// Nothing.
    #[default]
    Empty,
    /// `sticky`.
    Sticky,
    /// The progress line of a downtime in effect, 0 to 1.
    Progress(f32),
}

/// The words of an entry.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct EntryText {
    /// The icon in the mark slot.
    pub(crate) icon: IconName,
    /// The kind's word (none for a comment).
    pub(crate) kind: Option<&'static str>,
    /// The icon and the kind's word in the accent.
    pub(crate) accent: bool,
    /// Who set it (a config downtime: its schedule).
    pub(crate) author: String,
    /// When it was set (none for a config downtime).
    pub(crate) at: String,
    /// The details: `fixed 13:00 → 16:00 · host and 18 services`.
    pub(crate) meta: String,
    /// Its text (the comment typed with it).
    pub(crate) text: String,
    /// Slot A.
    pub(crate) slot_a: SlotA,
    /// Slot B and its tone.
    pub(crate) slot_b: String,
    pub(crate) tone: Tone,
}

/// The words of `entry`, in the local time zone.
pub(crate) fn entry_text(snapshot: &Snapshot, entry: &Entry, now: Timestamp) -> Option<EntryText> {
    entry_text_in(snapshot, entry, now, &Local)
}

/// The words of a comment sent from a handling view that the snapshot
/// doesn't show yet (topic 17): as a comment's, its time when it was sent.
pub(crate) fn draft_text(draft: &crate::comments::drafts::Draft, now: Timestamp) -> EntryText {
    EntryText {
        icon: IconName::MessageSquare,
        kind: None,
        accent: false,
        author: draft.author.clone(),
        at: day_clock(draft.at, now, &Local),
        meta: String::new(),
        text: first_line(&draft.text).to_owned(),
        slot_a: SlotA::Empty,
        slot_b: String::new(),
        tone: Tone::Faint,
    }
}

/// The record of a downtime entry.
pub(crate) fn downtime_of<'a>(snapshot: &'a Snapshot, entry: &Entry) -> Option<&'a Downtime> {
    match entry.record {
        Record::Downtime(index) => downtimes::of(snapshot, &entry.object).get(index),
        _ => None,
    }
}

/// [`entry_text`] in time zone `zone`.
pub(crate) fn entry_text_in<Tz>(
    snapshot: &Snapshot,
    entry: &Entry,
    now: Timestamp,
    zone: &Tz,
) -> Option<EntryText>
where
    Tz: TimeZone,
    Tz::Offset: Display,
{
    match entry.record {
        Record::Ack => {
            let check = check_of(snapshot, &entry.object)?;
            let comment = ack_comment(snapshot, &entry.object);
            let (slot_b, tone) = expiry(check.acknowledgement_expiry, now, zone);
            Some(EntryText {
                icon: IconName::Check,
                kind: Some("acknowledged"),
                accent: true,
                author: comment
                    .map(|comment| comment.author.clone())
                    .unwrap_or_default(),
                at: comment
                    .map(|comment| day_clock(comment.entry_time, now, zone))
                    .unwrap_or_default(),
                meta: String::new(),
                text: comment.map_or_else(
                    || "its comment hasn't arrived".to_owned(),
                    |comment| first_line(&comment.text).to_owned(),
                ),
                slot_a: if check.acknowledgement == AckKind::Sticky {
                    SlotA::Sticky
                } else {
                    SlotA::Empty
                },
                slot_b: if check.acknowledgement_expiry.is_none() {
                    "no expiry".to_owned()
                } else {
                    slot_b
                },
                tone,
            })
        }
        Record::Comment(index) => {
            let comment = snapshot.comments.get(&entry.object)?.get(index)?;
            let (slot_b, tone) = expiry(comment.expire_time, now, zone);
            Some(EntryText {
                icon: IconName::MessageSquare,
                kind: None,
                accent: false,
                author: comment.author.clone(),
                at: day_clock(comment.entry_time, now, zone),
                meta: String::new(),
                text: first_line(&comment.text).to_owned(),
                slot_a: SlotA::Empty,
                slot_b,
                tone,
            })
        }
        Record::Downtime(_) => {
            let downtime = downtime_of(snapshot, entry)?;
            let in_effect = entry.kind == EntryKind::InEffect;
            let (slot_b, tone) = downtime_time(downtime, now, zone);
            Some(EntryText {
                icon: if entry.config {
                    IconName::Lock
                } else {
                    IconName::CalendarClock
                },
                kind: Some(match (in_effect, entry.config) {
                    (true, _) => "in downtime",
                    (false, true) => "downtime, from config",
                    (false, false) => "downtime, upcoming",
                }),
                accent: in_effect,
                author: if entry.config {
                    downtime
                        .schedule
                        .clone()
                        .unwrap_or_else(|| downtime.author.clone())
                } else {
                    downtime.author.clone()
                },
                at: if entry.config {
                    String::new()
                } else {
                    day_clock(downtime.entry_time, now, zone)
                },
                meta: downtime_meta(snapshot, entry, downtime, now, zone),
                text: first_line(&downtime.comment).to_owned(),
                slot_a: if in_effect {
                    SlotA::Progress(downtime.progress(now))
                } else {
                    SlotA::Empty
                },
                slot_b,
                tone,
            })
        }
    }
}

fn check_of<'a>(snapshot: &'a Snapshot, object: &ObjectKey) -> Option<&'a ic_model::CheckInfo> {
    match object {
        ObjectKey::Host { name } => snapshot.hosts.get(name).map(|host| &host.check),
        ObjectKey::Service { key } => snapshot.services.get(key).map(|service| &service.check),
    }
}

/// An expiry in slot B: `expires 15:00, in 48m` today (the warning colour
/// within 2 hours), `expires Thu 08:00` later; nothing without one.
fn expiry<Tz>(at: Option<Timestamp>, now: Timestamp, zone: &Tz) -> (String, Tone)
where
    Tz: TimeZone,
    Tz::Offset: Display,
{
    let Some(at) = at else {
        return (String::new(), Tone::Faint);
    };
    let left = at.as_unix_seconds() - now.as_unix_seconds();
    let tone = if left <= SOON {
        Tone::Warning
    } else {
        Tone::Faint
    };
    if same_day(at, now, zone) {
        (
            format!(
                "expires {}, in {}",
                short_when(at, now, zone),
                downtimes::left(at.remaining_from(now))
            ),
            tone,
        )
    } else {
        (format!("expires {}", short_when(at, now, zone)), tone)
    }
}

/// A downtime's time in slot B (and the timeline's right column): `1h 48m
/// left` in the accent while in effect, `not started` (flexible, waiting),
/// `by 22:00` (flexible, its window still to open), `in 7h 48m` (fixed),
/// `Sat 06:00` (from the config).
pub(crate) fn downtime_time<Tz>(downtime: &Downtime, now: Timestamp, zone: &Tz) -> (String, Tone)
where
    Tz: TimeZone,
    Tz::Offset: Display,
{
    match downtime.phase(now) {
        DowntimePhase::InEffect => (
            format!(
                "{} left",
                downtimes::left(
                    downtime
                        .effective_end()
                        .unwrap_or(downtime.end_time)
                        .remaining_from(now)
                )
            ),
            Tone::Accent,
        ),
        _ if downtime.config_owned => (short_when(downtime.start_time, now, zone), Tone::Faint),
        DowntimePhase::Waiting => ("not started".to_owned(), Tone::Faint),
        DowntimePhase::Upcoming if !downtime.fixed => (
            format!("by {}", short_when(downtime.start_time, now, zone)),
            Tone::Faint,
        ),
        DowntimePhase::Upcoming => (
            format!(
                "in {}",
                downtimes::left(downtime.start_time.remaining_from(now))
            ),
            Tone::Faint,
        ),
        DowntimePhase::Over => ("over".to_owned(), Tone::Faint),
    }
}

/// A downtime's details: `fixed 13:00 → 16:00`, `fixed · Thu 01:00 →
/// 03:00 · host and 6 services`, `flexible 1h · window 22:00 → 06:00 ·
/// host only`, `flexible 2h · started 13:40`, `fixed 13:30 → 17:00 · its
/// own`.
fn downtime_meta<Tz>(
    snapshot: &Snapshot,
    entry: &Entry,
    downtime: &Downtime,
    now: Timestamp,
    zone: &Tz,
) -> String
where
    Tz: TimeZone,
    Tz::Offset: Display,
{
    let window = compact_window(downtime.start_time, downtime.end_time, now, zone);
    // A window today reads `fixed 13:00 → 16:00`; one on another day
    // `fixed · Thu 01:00 → 03:00`.
    let today = same_day(downtime.start_time, now, zone);
    let mut parts = Vec::new();
    if downtime.fixed {
        if today {
            parts.push(format!("fixed {window}"));
        } else {
            parts.push("fixed".to_owned());
            parts.push(window);
        }
    } else {
        parts.push(format!("flexible {}", length(downtime.duration)));
        match downtime.trigger_time {
            Some(trigger) => parts.push(format!("started {}", day_clock(trigger, now, zone))),
            None => parts.push(format!("window {window}")),
        }
    }
    match (&entry.object, entry.services) {
        (ObjectKey::Host { .. }, None) => parts.push("host only".to_owned()),
        (ObjectKey::Host { .. }, Some(0)) | (ObjectKey::Service { .. }, _) => {}
        (ObjectKey::Host { .. }, Some(1)) => parts.push("host and 1 service".to_owned()),
        (ObjectKey::Host { .. }, Some(count)) => parts.push(format!("host and {count} services")),
    }
    if entry.own {
        parts.push("its own".to_owned());
    } else if matches!(entry.object, ObjectKey::Service { .. })
        && downtimes::host_parent(snapshot, downtime).is_some()
    {
        parts.push("with its host".to_owned());
    }
    if let Some(by) = &downtime.triggered_by {
        let what = downtimes::find(snapshot, None, by).map_or_else(
            || by.clone(),
            |trigger| {
                crate::operate::forms::describe_objects(std::slice::from_ref(&trigger.object))
            },
        );
        parts.push(format!("triggered by {what}"));
    }
    parts.join(" · ")
}

/// The timeline's span: twelve hours from the even hour two hours before
/// now (`noon to midnight · now 14:12`), the hours every two hours.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Axis {
    /// Where it starts and ends (seconds).
    pub(crate) start: f64,
    pub(crate) end: f64,
    /// `noon to midnight · now 14:12`.
    pub(crate) label: String,
    /// The hours marked, as fractions of the span, with their labels.
    pub(crate) ticks: Vec<(f32, String)>,
    /// Where now is, as a fraction of the span.
    pub(crate) now: f32,
}

/// The span of the axis.
const AXIS_HOURS: i64 = 12;

/// The axis around `now`, in the local time zone.
pub(crate) fn axis(now: Timestamp) -> Axis {
    axis_in(now, &Local)
}

/// [`axis`] in time zone `zone`.
pub(crate) fn axis_in<Tz>(now: Timestamp, zone: &Tz) -> Axis
where
    Tz: TimeZone,
    Tz::Offset: Display,
{
    let Some(local) = crate::format::date_time(now, zone) else {
        return Axis {
            start: now.as_unix_seconds(),
            end: now.as_unix_seconds() + 12. * 3_600.,
            label: String::new(),
            ticks: Vec::new(),
            now: 0.,
        };
    };
    // The even hour at or before two hours ago.
    let back = local.clone() - chrono::Duration::hours(2);
    let hour = i64::from(back.hour()) / 2 * 2;
    let into = i64::from(back.hour()) - hour;
    let start_local = back.clone()
        - chrono::Duration::hours(into)
        - chrono::Duration::minutes(i64::from(back.minute()))
        - chrono::Duration::seconds(i64::from(back.second()));
    #[expect(
        clippy::cast_precision_loss,
        reason = "seconds since 1970 fit an f64 exactly"
    )]
    let start = start_local.timestamp() as f64;
    #[expect(clippy::cast_precision_loss, reason = "a few hours in seconds")]
    let end = start + (AXIS_HOURS * 3_600) as f64;
    let name = |hour: i64| match hour % 24 {
        0 => "midnight".to_owned(),
        12 => "noon".to_owned(),
        other => format!("{other:02}:00"),
    };
    let label = format!(
        "{} to {} · now {}",
        name(hour),
        name(hour + AXIS_HOURS),
        local.format("%H:%M")
    );
    let span = end - start;
    let ticks = (0..=AXIS_HOURS / 2)
        .map(|step| {
            #[expect(clippy::cast_precision_loss, reason = "a handful of steps")]
            let fraction = (step * 2 * 3_600) as f64 / span;
            #[expect(clippy::cast_possible_truncation, reason = "a fraction for drawing")]
            (fraction as f32, format!("{:02}:00", (hour + step * 2) % 24))
        })
        .collect();
    #[expect(clippy::cast_possible_truncation, reason = "a fraction for drawing")]
    let now_at = ((now.as_unix_seconds() - start) / span) as f32;
    Axis {
        start,
        end,
        label,
        ticks,
        now: now_at,
    }
}

/// A downtime on the axis.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Bar {
    /// In effect: a track with the elapsed part in the accent.
    InEffect {
        /// Where it starts and ends on the axis (fractions, clamped).
        from: f32,
        to: f32,
        /// Where the elapsed part ends: now, on the axis (a fraction of
        /// the axis, not of the downtime, so it meets the now line however
        /// much of the downtime lies off the axis).
        elapsed: f32,
        /// It began before the axis: when, above the bar's left end
        /// (`← since Tue 22:00`).
        since: Option<String>,
    },
    /// Still to come: a faint track.
    Upcoming {
        /// Where it starts and ends on the axis.
        from: f32,
        to: f32,
    },
    /// Flexible, not started: a dashed line over its window, its length
    /// above it (`flexible 2h`).
    Flexible {
        /// Where its window opens and closes on the axis.
        from: f32,
        to: f32,
        /// `flexible 2h`.
        label: String,
    },
    /// Past the axis's end: when it starts, at the right edge (`→ Thu
    /// 01:00`), with the lock for a config one.
    Later {
        /// `→ Sat 06:00`.
        text: String,
        /// From the config.
        lock: bool,
    },
}

/// `downtime` on `axis`, in the local time zone.
pub(crate) fn bar(downtime: &Downtime, axis: &Axis, now: Timestamp) -> Bar {
    bar_in(downtime, axis, now, &Local)
}

/// [`bar`] in time zone `zone`.
pub(crate) fn bar_in<Tz>(downtime: &Downtime, axis: &Axis, now: Timestamp, zone: &Tz) -> Bar
where
    Tz: TimeZone,
    Tz::Offset: Display,
{
    let span = axis.end - axis.start;
    #[expect(clippy::cast_possible_truncation, reason = "a fraction for drawing")]
    let at = |seconds: f64| ((seconds - axis.start) / span).clamp(0., 1.) as f32;
    let phase = downtime.phase(now);
    let start = downtime
        .effective_start()
        .unwrap_or(downtime.start_time)
        .as_unix_seconds();
    let end = downtime
        .effective_end()
        .unwrap_or(downtime.end_time)
        .as_unix_seconds();
    if phase != DowntimePhase::InEffect && downtime.start_time.as_unix_seconds() >= axis.end {
        return Bar::Later {
            text: format!("→ {}", short_when(downtime.start_time, now, zone)),
            lock: downtime.config_owned,
        };
    }
    match phase {
        DowntimePhase::InEffect => Bar::InEffect {
            from: at(start),
            to: at(end),
            elapsed: at(now.as_unix_seconds()).clamp(at(start), at(end)),
            since: (start < axis.start).then(|| {
                format!(
                    "← since {}",
                    short_when(Timestamp::from_unix_seconds(start), now, zone)
                )
            }),
        },
        _ if !downtime.fixed && downtime.trigger_time.is_none() => Bar::Flexible {
            from: at(downtime.start_time.as_unix_seconds()),
            to: at(downtime.end_time.as_unix_seconds()),
            label: format!("flexible {}", length(downtime.duration)),
        },
        _ => Bar::Upcoming {
            from: at(start),
            to: at(end),
        },
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;

    /// Wednesday 7 October 2026, 14:12 UTC: the mock-ups' clock.
    fn now() -> Timestamp {
        Timestamp::from_unix_seconds(1_791_382_320.)
    }

    /// A fixed downtime in effect, from `start` to `end` minutes from now.
    fn in_effect(start: f64, end: f64) -> Downtime {
        let at =
            |minutes: f64| Timestamp::from_unix_seconds(now().as_unix_seconds() + minutes * 60.);
        Downtime {
            name: "db-01!work".to_owned(),
            object: ObjectKey::host("db-01"),
            author: "ana".to_owned(),
            comment: "work".to_owned(),
            start_time: at(start),
            end_time: at(end),
            fixed: true,
            duration: 0.,
            entry_time: at(start),
            trigger_time: None,
            triggered_by: None,
            parent: None,
            in_effect: true,
            config_owned: false,
            schedule: None,
        }
    }

    /// The elapsed part ends at the now line, whatever lies off the axis.
    #[test]
    fn the_elapsed_part_ends_at_the_now_line() {
        let axis = axis_in(now(), &Utc);
        let elapsed = |downtime: &Downtime| match bar_in(downtime, &axis, now(), &Utc) {
            Bar::InEffect {
                from,
                elapsed,
                since,
                ..
            } => (from, elapsed, since),
            other => panic!("{other:?}"),
        };
        // 08:12 to 20:12: began before the axis (noon).
        let (from, end, since) = elapsed(&in_effect(-360., 360.));
        assert!(from.abs() < 0.001);
        assert!((end - axis.now).abs() < 0.01, "{end} vs {}", axis.now);
        assert_eq!(since.as_deref(), Some("← since 08:12"));
        // 14:00 today to 14:00 tomorrow: ends after the axis.
        let (_, end, since) = elapsed(&in_effect(-12., 24. * 60. - 12.));
        assert!((end - axis.now).abs() < 0.01, "{end} vs {}", axis.now);
        assert_eq!(since, None);
        // 13:12 to 15:12: inside the axis.
        let (_, end, _) = elapsed(&in_effect(-60., 60.));
        assert!((end - axis.now).abs() < 0.01, "{end} vs {}", axis.now);
    }

    #[test]
    fn the_axis_runs_noon_to_midnight_at_twelve_past_two() {
        let axis = axis_in(now(), &Utc);
        assert_eq!(axis.label, "noon to midnight · now 14:12");
        assert_eq!(axis.ticks.len(), 7);
        assert_eq!(axis.ticks[0], (0., "12:00".to_owned()));
        assert_eq!(axis.ticks[6].1, "00:00");
        assert!((axis.now - (2.2 / 12.)).abs() < 0.001, "{}", axis.now);
        // At ten past one in the morning: from 22:00.
        let late = Timestamp::from_unix_seconds(now().as_unix_seconds() + 11. * 3_600. - 2. * 60.);
        assert_eq!(axis_in(late, &Utc).label, "22:00 to 10:00 · now 01:10");
    }
}
