//! What icygui says about downtimes (topic 01): the banner fixed under a
//! pane's header, the pane's other downtimes, the host line's marker for a
//! host downtime that leaves the service alone, the list rows' tags, and
//! what a removal takes along.
//!
//! Pure: everything comes from the snapshot the engine keeps current
//! (downtimes are part of the initial queries and the event stream), so
//! nothing here asks Icinga for anything.
//!
//! Words, as the approved mock-ups draw them:
//!
//! - in effect: `In downtime 1h 48m left`, `fixed · 13:00 → 16:00 today ·
//!   started 1h 12m ago`; a service in its host's downtime (`all_services`)
//!   `In downtime with its host`, `host k8s-node-07 · fixed · 13:30 → 15:30
//!   today · host and all its services`;
//! - not in effect yet (grey, the problem is still unhandled): `Flexible
//!   downtime not started`, `lasts 2h from the first problem · window 14:00
//!   → 18:00 today`; `Downtime scheduled starts in 7h 48m`;
//! - list tags: `downtime 1h 48m`, `host downtime 1h 18m`, `downtime,
//!   flexible 1h 12m`, `downtime at 22:00`, `downtime, flexible, not
//!   started`.

use std::collections::HashSet;
use std::fmt::Display;
use std::time::Duration;

use chrono::{Datelike as _, Local, TimeZone, Timelike as _};
use ic_core::snapshot::Snapshot;
use ic_model::{
    CheckInfo, CheckableState, Downtime, DowntimePhase, ObjectKey, Timestamp, format_two_units,
};

use crate::format;

/// How deep a removal follows children of children (child hosts of child
/// hosts); Icinga's own chains are short.
const CHILD_DEPTH: usize = 8;

/// An object's downtimes in the snapshot, by start time.
pub(crate) fn of<'a>(snapshot: &'a Snapshot, object: &ObjectKey) -> &'a [Downtime] {
    snapshot.downtimes.get(object).map_or(&[], Vec::as_slice)
}

/// The downtime with this full name, looked up on `object` first.
pub(crate) fn find<'a>(
    snapshot: &'a Snapshot,
    object: Option<&ObjectKey>,
    name: &str,
) -> Option<&'a Downtime> {
    object
        .and_then(|object| of(snapshot, object).iter().find(|d| d.name == name))
        .or_else(|| {
            snapshot
                .downtimes
                .values()
                .flatten()
                .find(|downtime| downtime.name == name)
        })
}

/// Whether two downtimes were scheduled together (`all_services` on an
/// Icinga that doesn't link them with `parent`).
fn twins(a: &Downtime, b: &Downtime) -> bool {
    a.author == b.author
        && a.comment == b.comment
        && a.start_time == b.start_time
        && a.end_time == b.end_time
        && a.fixed == b.fixed
}

/// The host's downtime a service's `downtime` belongs to: its `parent`
/// (scheduled with `all_services`), or on an Icinga that doesn't say, the
/// host's downtime scheduled with it (same author, comment and window).
pub(crate) fn host_parent<'a>(snapshot: &'a Snapshot, downtime: &Downtime) -> Option<&'a Downtime> {
    let ObjectKey::Service { key } = &downtime.object else {
        return None;
    };
    let host = of(
        snapshot,
        &ObjectKey::Host {
            name: key.host.clone(),
        },
    );
    match &downtime.parent {
        Some(parent) => host.iter().find(|candidate| candidate.name == *parent),
        None => host.iter().find(|candidate| twins(candidate, downtime)),
    }
}

/// The downtimes that go with `downtime` when it is removed: its children
/// (`all_services`, child hosts; Icinga removes them with it), their
/// children, and for a host's downtime on an Icinga that doesn't link them,
/// its services' twins. Services in name order, then other hosts.
pub(crate) fn children<'a>(snapshot: &'a Snapshot, downtime: &Downtime) -> Vec<&'a Downtime> {
    let mut found: Vec<&Downtime> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    let mut parents = vec![downtime.name.as_str()];
    for _ in 0..CHILD_DEPTH {
        let next: Vec<&Downtime> = snapshot
            .downtimes
            .values()
            .flatten()
            .filter(|candidate| {
                candidate
                    .parent
                    .as_deref()
                    .is_some_and(|parent| parents.contains(&parent))
            })
            .filter(|candidate| seen.insert(candidate.name.as_str()))
            .collect();
        if next.is_empty() {
            break;
        }
        parents = next.iter().map(|child| child.name.as_str()).collect();
        found.extend(next);
    }
    if let ObjectKey::Host { name } = &downtime.object {
        for service in snapshot.services_of(name) {
            for candidate in of(snapshot, &service.object_key()) {
                if candidate.parent.is_none()
                    && twins(candidate, downtime)
                    && seen.insert(candidate.name.as_str())
                {
                    found.push(candidate);
                }
            }
        }
    }
    found.sort_by(|a, b| {
        let host = |d: &Downtime| matches!(d.object, ObjectKey::Host { .. });
        host(a)
            .cmp(&host(b))
            .then_with(|| a.object.full_name().cmp(&b.object.full_name()))
    });
    found
}

/// The downtime a pane's banner shows: the one in effect (the one ending
/// last when several are: the object stays handled until then), else the
/// next to begin (a flexible one waiting for a problem first, then the
/// earliest start). Ended ones never show.
pub(crate) fn primary(downtimes: &[Downtime], now: Timestamp) -> Option<&Downtime> {
    let end = |downtime: &Downtime| {
        downtime
            .effective_end()
            .unwrap_or(downtime.end_time)
            .as_unix_seconds()
    };
    let in_effect = downtimes
        .iter()
        .filter(|downtime| downtime.phase(now) == DowntimePhase::InEffect)
        .max_by(|a, b| end(a).total_cmp(&end(b)));
    in_effect
        .or_else(|| {
            downtimes
                .iter()
                .find(|downtime| downtime.phase(now) == DowntimePhase::Waiting)
        })
        .or_else(|| {
            downtimes
                .iter()
                .filter(|downtime| downtime.phase(now) == DowntimePhase::Upcoming)
                .min_by(|a, b| {
                    a.start_time
                        .as_unix_seconds()
                        .total_cmp(&b.start_time.as_unix_seconds())
                })
        })
}

/// A part of a banner's facts line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Fact {
    /// Plain text.
    Text(String),
    /// `host <link>`: the host whose downtime it is, a link to its pane.
    Host(String),
}

/// What a pane's downtime banner says (variant A of topic 01).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Banner {
    /// The downtime it shows, by full name (what *remove downtime* starts
    /// from).
    pub(crate) name: String,
    /// The object the downtime is set on.
    pub(crate) object: ObjectKey,
    /// In effect (the accent: the object is handled), or not yet (grey:
    /// a problem is still unhandled).
    pub(crate) in_effect: bool,
    /// From the config (`ScheduledDowntime`): a lock instead of the
    /// calendar, and it can't be removed.
    pub(crate) config: bool,
    /// The `ScheduledDowntime`'s name, if Icinga said.
    pub(crate) schedule: Option<String>,
    /// `In downtime`, `In downtime with its host`, `Flexible downtime`,
    /// `Downtime scheduled`.
    pub(crate) title: &'static str,
    /// `1h 48m left`, `not started`, `starts in 7h 48m`.
    pub(crate) status: String,
    /// The facts: `fixed`, the window, coverage or when it started.
    pub(crate) facts: Vec<Fact>,
    /// Who set it.
    pub(crate) author: String,
    /// When (`12:41`), or `config`.
    pub(crate) entered: String,
    /// Why.
    pub(crate) comment: String,
    /// The object's other downtimes, when it has more:
    /// `+ 2 more below`, `tonight 22:00, flexible`, `Sat 06:00, from config`.
    pub(crate) more: Vec<String>,
    /// How much of it has passed (0 until it takes effect).
    pub(crate) progress: f32,
}

/// The banner of `object`'s pane, if it has a downtime in effect or still
/// to come.
pub(crate) fn banner(snapshot: &Snapshot, object: &ObjectKey, now: Timestamp) -> Option<Banner> {
    banner_in(snapshot, object, now, &Local)
}

/// [`banner`] in time zone `zone`.
pub(crate) fn banner_in<Tz>(
    snapshot: &Snapshot,
    object: &ObjectKey,
    now: Timestamp,
    zone: &Tz,
) -> Option<Banner>
where
    Tz: TimeZone,
    Tz::Offset: Display,
{
    let own = of(snapshot, object);
    let shown = primary(own, now)?;
    let phase = shown.phase(now);
    let in_effect = phase == DowntimePhase::InEffect;
    let parent = host_parent(snapshot, shown);
    let title = match phase {
        DowntimePhase::InEffect if parent.is_some() => "In downtime with its host",
        DowntimePhase::InEffect => "In downtime",
        DowntimePhase::Waiting => "Flexible downtime",
        DowntimePhase::Upcoming | DowntimePhase::Over if !shown.fixed => "Flexible downtime",
        DowntimePhase::Upcoming | DowntimePhase::Over => "Downtime scheduled",
    };
    let status = status_of(shown, phase, now);
    let mut facts = Vec::new();
    if let Some(parent) = parent {
        facts.push(Fact::Host(parent.object.host_name().to_string()));
    }
    facts.extend(
        kind_and_window(shown, phase, now, zone)
            .into_iter()
            .map(Fact::Text),
    );
    match (&shown.object, parent) {
        (_, Some(parent)) => facts.push(Fact::Text(coverage(snapshot, parent, true))),
        (ObjectKey::Host { .. }, None) => {
            let covered = coverage(snapshot, shown, false);
            if !covered.is_empty() {
                facts.push(Fact::Text(covered));
            }
        }
        (ObjectKey::Service { .. }, None) => {
            if in_effect
                && shown.fixed
                && let Some(start) = shown.effective_start()
            {
                facts.push(Fact::Text(format!(
                    "started {} ago",
                    left(start.elapsed_until(now))
                )));
            }
        }
    }
    if matches!(phase, DowntimePhase::Upcoming | DowntimePhase::Over) && shown.fixed {
        facts.push(Fact::Text(if is_problem(snapshot, object) {
            "not in effect yet: the problem is unhandled".to_owned()
        } else {
            "not in effect yet".to_owned()
        }));
    }
    if shown.config_owned {
        facts.push(Fact::Text(from_config(shown)));
    }
    let more = more_of(own, shown, now, zone);
    Some(Banner {
        name: shown.name.clone(),
        object: shown.object.clone(),
        in_effect,
        config: shown.config_owned,
        schedule: shown.schedule.clone(),
        title,
        status,
        facts,
        author: shown.author.clone(),
        entered: entered(shown, now, zone),
        comment: shown.comment.clone(),
        more,
        progress: if in_effect { shown.progress(now) } else { 0.0 },
    })
}

/// The banner's status: `1h 48m left`, `not started`, `starts in 7h 48m`,
/// `window opens in 7h 48m`.
fn status_of(shown: &Downtime, phase: DowntimePhase, now: Timestamp) -> String {
    match phase {
        DowntimePhase::InEffect => format!(
            "{} left",
            left(
                shown
                    .effective_end()
                    .unwrap_or(shown.end_time)
                    .remaining_from(now)
            )
        ),
        DowntimePhase::Waiting => "not started".to_owned(),
        DowntimePhase::Upcoming | DowntimePhase::Over => {
            let ahead = shown.start_time.remaining_from(now);
            match (shown.fixed, ahead.as_secs() < 60) {
                (true, true) => "starts now".to_owned(),
                (true, false) => format!("starts in {}", left(ahead)),
                (false, true) => "window opens now".to_owned(),
                (false, false) => format!("window opens in {}", left(ahead)),
            }
        }
    }
}

/// The banner's last line when the object has more downtimes: `+ 2 more
/// below`, then the next two in short.
fn more_of<Tz>(own: &[Downtime], shown: &Downtime, now: Timestamp, zone: &Tz) -> Vec<String>
where
    Tz: TimeZone,
    Tz::Offset: Display,
{
    let others: Vec<&Downtime> = own
        .iter()
        .filter(|downtime| downtime.name != shown.name)
        .filter(|downtime| downtime.phase(now) != DowntimePhase::Over)
        .collect();
    if others.is_empty() {
        return Vec::new();
    }
    std::iter::once(format!("+ {} more below", others.len()))
        .chain(
            others
                .iter()
                .take(2)
                .map(|downtime| short(downtime, now, zone)),
        )
        .collect()
}

/// The marker on a service's host line when its host is in a downtime
/// that doesn't cover the service (scheduled without `all_services`): as
/// in Icinga Web, the service isn't in downtime, but its pane says the
/// host is (`host in downtime`), and until when (`until 15:00`, empty if
/// unknown).
pub(crate) fn host_marker(
    snapshot: &Snapshot,
    service: &ic_model::Service,
    now: Timestamp,
) -> Option<String> {
    host_marker_in(snapshot, service, now, &Local)
}

/// [`host_marker`] in time zone `zone`.
pub(crate) fn host_marker_in<Tz>(
    snapshot: &Snapshot,
    service: &ic_model::Service,
    now: Timestamp,
    zone: &Tz,
) -> Option<String>
where
    Tz: TimeZone,
    Tz::Offset: Display,
{
    let host = snapshot.host_of(&service.key)?;
    if !host.check.in_downtime() || service.check.in_downtime() {
        return None;
    }
    let until = of(snapshot, &host.key())
        .iter()
        .filter(|downtime| downtime.phase(now) == DowntimePhase::InEffect)
        .filter_map(Downtime::effective_end)
        .max_by(|a, b| a.as_unix_seconds().total_cmp(&b.as_unix_seconds()));
    Some(until.map_or_else(String::new, |until| {
        format!("until {}", format::clock_in(until, now, zone))
    }))
}

/// A list row's tag about a downtime: in effect (`downtime 1h 48m`, `host
/// downtime 1h 18m`, `downtime, flexible 1h 12m`), else one still to come
/// (`downtime at 22:00`, `downtime, flexible, not started`). `check` is the
/// object's (whose `downtime_depth` says it's in downtime).
pub(crate) fn tag(
    snapshot: &Snapshot,
    object: &ObjectKey,
    check: &CheckInfo,
    now: Timestamp,
) -> Option<String> {
    tag_in(snapshot, object, check, now, &Local)
}

/// [`tag`] in time zone `zone`.
pub(crate) fn tag_in<Tz>(
    snapshot: &Snapshot,
    object: &ObjectKey,
    check: &CheckInfo,
    now: Timestamp,
    zone: &Tz,
) -> Option<String>
where
    Tz: TimeZone,
    Tz::Offset: Display,
{
    let own = of(snapshot, object);
    if check.in_downtime() {
        let Some(downtime) =
            primary(own, now).filter(|downtime| downtime.phase(now) == DowntimePhase::InEffect)
        else {
            return Some("downtime".to_owned());
        };
        let remaining = left(
            downtime
                .effective_end()
                .unwrap_or(downtime.end_time)
                .remaining_from(now),
        );
        return Some(if host_parent(snapshot, downtime).is_some() {
            format!("host downtime {remaining}")
        } else if downtime.fixed {
            format!("downtime {remaining}")
        } else {
            format!("downtime, flexible {remaining}")
        });
    }
    let coming = primary(own, now)?;
    match coming.phase(now) {
        DowntimePhase::Waiting => Some("downtime, flexible, not started".to_owned()),
        // One pattern whatever the day: `downtime at 22:00`, `downtime at
        // tomorrow 08:00`, `downtime at Sat 02:21`.
        DowntimePhase::Upcoming => {
            Some(format!("downtime at {}", at(coming.start_time, now, zone)))
        }
        DowntimePhase::InEffect | DowntimePhase::Over => None,
    }
}

/// How long is left (or has passed): `48m`, `1h 08m`, `2d 4h`; minutes
/// rounded up, so a downtime in effect never shows `0m`.
pub(crate) fn left(duration: Duration) -> String {
    let minutes = duration.as_secs().div_ceil(60);
    let (days, hours, minutes) = (minutes / 1_440, minutes % 1_440 / 60, minutes % 60);
    match (days, hours) {
        (0, 0) => format!("{minutes}m"),
        (0, hours) => format!("{hours}h {minutes:02}m"),
        (days, hours) => format!("{days}d {hours}h"),
    }
}

/// A flexible downtime's length: `2h`, `1h 30m`.
fn duration(seconds: f64) -> String {
    format_two_units(Duration::try_from_secs_f64(seconds.max(0.0)).unwrap_or_default())
}

/// The window, read from `now`: `13:00 → 16:00 today`, `tonight 22:00 →
/// 06:00`, `08:00 → 10:00 tomorrow`, `Sat 10 Oct 06:00 → 10:00`, `Fri 9 Oct
/// 22:00 → Sat 10 Oct 06:00`.
pub(crate) fn window_in<Tz>(start: Timestamp, end: Timestamp, now: Timestamp, zone: &Tz) -> String
where
    Tz: TimeZone,
    Tz::Offset: Display,
{
    let (Some(from), Some(to), Some(today)) = (
        format::date_time(start, zone),
        format::date_time(end, zone),
        format::date_time(now, zone),
    ) else {
        return String::new();
    };
    let clock = |at: &chrono::DateTime<Tz>| at.format("%H:%M").to_string();
    let offset = |at: &chrono::DateTime<Tz>| {
        at.date_naive()
            .signed_duration_since(today.date_naive())
            .num_days()
    };
    let word = |days: i64| match days {
        0 => Some("today"),
        1 => Some("tomorrow"),
        -1 => Some("yesterday"),
        _ => None,
    };
    let date = |at: &chrono::DateTime<Tz>| {
        if at.year() == today.year() {
            at.format("%a %-d %b").to_string()
        } else {
            at.format("%a %-d %b %Y").to_string()
        }
    };
    let (start_day, end_day) = (offset(&from), offset(&to));
    if start_day == end_day {
        return match word(start_day) {
            Some(word) => format!("{} → {} {word}", clock(&from), clock(&to)),
            None => format!("{} {} → {}", date(&from), clock(&from), clock(&to)),
        };
    }
    if start_day == 0 && end_day == 1 && from.hour() >= 17 && to.hour() < 13 {
        return format!("tonight {} → {}", clock(&from), clock(&to));
    }
    let day =
        |at: &chrono::DateTime<Tz>, days: i64| word(days).map_or_else(|| date(at), str::to_owned);
    format!(
        "{} {} → {} {}",
        day(&from, start_day),
        clock(&from),
        day(&to, end_day),
        clock(&to)
    )
}

/// When something begins, short: `22:00` today, `tomorrow 08:00`, `Sat
/// 06:00` within a week, `10 Oct 06:00` later.
fn at<Tz>(when: Timestamp, now: Timestamp, zone: &Tz) -> String
where
    Tz: TimeZone,
    Tz::Offset: Display,
{
    let (Some(when), Some(today)) = (format::date_time(when, zone), format::date_time(now, zone))
    else {
        return String::new();
    };
    match when
        .date_naive()
        .signed_duration_since(today.date_naive())
        .num_days()
    {
        0 => when.format("%H:%M").to_string(),
        1 => when.format("tomorrow %H:%M").to_string(),
        2..=6 => when.format("%a %H:%M").to_string(),
        _ => when.format("%-d %b %H:%M").to_string(),
    }
}

/// Another downtime in a few words, for the banner's last line: `tonight
/// 22:00, flexible`, `Sat 06:00, from config`, `until 15:00`.
fn short<Tz>(downtime: &Downtime, now: Timestamp, zone: &Tz) -> String
where
    Tz: TimeZone,
    Tz::Offset: Display,
{
    let when = if downtime.phase(now) == DowntimePhase::InEffect {
        format!(
            "until {}",
            format::clock_in(
                downtime.effective_end().unwrap_or(downtime.end_time),
                now,
                zone
            )
        )
    } else {
        let start = format::date_time(downtime.start_time, zone);
        let today = format::date_time(now, zone);
        match (start, today) {
            (Some(start), Some(today))
                if start.date_naive() == today.date_naive() && start.hour() >= 17 =>
            {
                format!("tonight {}", start.format("%H:%M"))
            }
            _ => at(downtime.start_time, now, zone),
        }
    };
    if downtime.config_owned {
        format!("{when}, from config")
    } else if downtime.fixed {
        when
    } else {
        format!("{when}, flexible")
    }
}

/// `fixed` and the window, or a flexible downtime's length and window:
/// `flexible, 2h from 14:31` once a problem started it, `lasts 2h from
/// the first problem` before.
fn kind_and_window<Tz>(
    downtime: &Downtime,
    phase: DowntimePhase,
    now: Timestamp,
    zone: &Tz,
) -> Vec<String>
where
    Tz: TimeZone,
    Tz::Offset: Display,
{
    let window = window_in(downtime.start_time, downtime.end_time, now, zone);
    if downtime.fixed {
        return vec!["fixed".to_owned(), window];
    }
    let length = duration(downtime.duration);
    let kind = match (phase, downtime.trigger_time) {
        (DowntimePhase::InEffect, Some(trigger)) => {
            format!(
                "flexible, {length} from {}",
                format::clock_in(trigger, now, zone)
            )
        }
        _ => format!("lasts {length} from the first problem"),
    };
    vec![kind, format!("window {window}")]
}

/// What a host's downtime covers: `host and its 18 services`, `host and 5
/// of its 18 services`, `the host only, not its services`; for a service
/// in it (`for_service`), `host and all its services`.
fn coverage(snapshot: &Snapshot, host_downtime: &Downtime, for_service: bool) -> String {
    let ObjectKey::Host { name } = &host_downtime.object else {
        return String::new();
    };
    let total = snapshot.services_of(name).count();
    let covered = children(snapshot, host_downtime)
        .iter()
        .filter(|child| child.object.host_name() == name && child.object.as_service().is_some())
        .count();
    match (covered, total) {
        (_, 0) => String::new(),
        (0, _) => "the host only, not its services".to_owned(),
        (covered, total) if covered >= total && for_service => {
            "host and all its services".to_owned()
        }
        (covered, total) if covered >= total => format!(
            "host and its {total} {}",
            if total == 1 { "service" } else { "services" }
        ),
        (covered, total) => format!("host and {covered} of its {total} services"),
    }
}

/// `from config: weekly-patching`.
fn from_config(downtime: &Downtime) -> String {
    match &downtime.schedule {
        Some(schedule) => format!("from config: {schedule}"),
        None => "from config".to_owned(),
    }
}

/// When it was set (`12:41`), or `config` for one from the config.
fn entered<Tz>(downtime: &Downtime, now: Timestamp, zone: &Tz) -> String
where
    Tz: TimeZone,
    Tz::Offset: Display,
{
    if downtime.config_owned {
        "config".to_owned()
    } else {
        format::clock_in(downtime.entry_time, now, zone)
    }
}

/// Whether `object` is in a problem state.
fn is_problem(snapshot: &Snapshot, object: &ObjectKey) -> bool {
    state_of(snapshot, object).is_some_and(CheckableState::is_problem)
}

/// `object`'s state, if the snapshot has it.
pub(crate) fn state_of(snapshot: &Snapshot, object: &ObjectKey) -> Option<CheckableState> {
    match object {
        ObjectKey::Host { name } => snapshot
            .hosts
            .get(name)
            .map(|host| CheckableState::Host(host.state)),
        ObjectKey::Service { key } => snapshot
            .services
            .get(key)
            .map(|service| CheckableState::Service(service.state)),
    }
}

/// A downtime a removal lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Removed {
    /// Its full name.
    pub(crate) name: String,
    /// The host or service it is set on.
    pub(crate) object: ObjectKey,
}

/// What a removal sends to Icinga.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Send {
    /// `remove-downtime` for one downtime by name; Icinga removes its
    /// children with it. `objects` are the hosts and services it changes.
    One {
        /// The downtime's full name.
        name: String,
        /// The objects whose downtimes go.
        objects: Vec<ObjectKey>,
    },
}

/// One way to remove: what goes, what is sent, what stays.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Scope {
    /// The choice's label (`this service only`, `the host and its 23
    /// services`); empty when there is no choice.
    pub(crate) label: String,
    /// Every downtime that goes, as the dialog lists them.
    pub(crate) removed: Vec<Removed>,
    /// What is sent.
    pub(crate) send: Vec<Send>,
    /// Downtimes from the config, which Icinga refuses to remove (they
    /// come back with the config).
    pub(crate) skipped: Vec<Removed>,
}

/// A removal of downtimes, before anything is sent: the dialog lists every
/// downtime it removes and the button counts them. A service's downtime
/// that belongs to its host's offers two scopes, this service only or the
/// host's whole downtime.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Removal {
    /// The objects asked about (the dialog's title).
    pub(crate) objects: Vec<ObjectKey>,
    /// The host whose downtime the second scope removes, if there are two.
    pub(crate) host: Option<String>,
    /// One scope, or two to choose from.
    pub(crate) scopes: Vec<Scope>,
    /// The scope chosen.
    pub(crate) chosen: usize,
}

impl Removal {
    /// Removing the downtime `name` of `object` (a pane's *remove
    /// downtime*, an other downtime's `×`); `None` if it's gone.
    pub(crate) fn downtime(snapshot: &Snapshot, object: &ObjectKey, name: &str) -> Option<Self> {
        let downtime = find(snapshot, Some(object), name)?;
        let own = scope_of(snapshot, downtime);
        let parent = host_parent(snapshot, downtime).filter(|parent| !parent.config_owned);
        let Some(parent) = parent.filter(|_| !downtime.config_owned) else {
            return Some(Self {
                objects: vec![object.clone()],
                host: None,
                scopes: vec![Scope {
                    label: String::new(),
                    ..own
                }],
                chosen: 0,
            });
        };
        let mut whole = scope_of(snapshot, parent);
        let services = whole
            .removed
            .iter()
            .filter(|removed| removed.object.as_service().is_some())
            .count();
        whole.label = format!(
            "the host and its {services} {}",
            if services == 1 { "service" } else { "services" }
        );
        Some(Self {
            objects: vec![object.clone()],
            host: Some(parent.object.host_name().to_string()),
            scopes: vec![
                Scope {
                    label: "this service only".to_owned(),
                    ..own
                },
                whole,
            ],
            // As drawn: the host's whole downtime, which is what was
            // scheduled; *this service only* is one click away.
            chosen: 1,
        })
    }

    /// Removing every downtime of `objects` (the pane's `···`, the
    /// selection bar, the palette). Only the downtimes listed go, by name:
    /// one scheduled by someone else after the dialog opened stays (it was
    /// never shown). A child goes with its parent, so it is sent only when
    /// its parent isn't.
    pub(crate) fn all_of(snapshot: &Snapshot, objects: &[ObjectKey]) -> Self {
        let mut removed: Vec<Removed> = Vec::new();
        let mut skipped: Vec<Removed> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut send: Vec<Send> = Vec::new();
        let mut names_sent: HashSet<String> = HashSet::new();
        for object in objects {
            for downtime in of(snapshot, object) {
                if downtime.config_owned {
                    if seen.insert(downtime.name.clone()) {
                        skipped.push(removed_of(downtime));
                    }
                    continue;
                }
                let scope = scope_of(snapshot, downtime);
                for item in scope.removed {
                    if seen.insert(item.name.clone()) {
                        removed.push(item);
                    }
                }
                for item in scope.send {
                    let Send::One { name, .. } = &item;
                    if names_sent.insert(name.clone()) {
                        send.push(item);
                    }
                }
            }
        }
        // Icinga removes a downtime's children with it: a child whose
        // parent goes too isn't sent again (it would come back as gone).
        send.retain(|Send::One { name, .. }| {
            find(snapshot, None, name)
                .and_then(|downtime| downtime.parent.as_ref())
                .is_none_or(|parent| !names_sent.contains(parent))
        });
        Self {
            objects: objects.to_vec(),
            host: None,
            scopes: vec![Scope {
                label: String::new(),
                removed,
                send,
                skipped,
            }],
            chosen: 0,
        }
    }

    /// The scope chosen.
    pub(crate) fn scope(&self) -> &Scope {
        self.scopes
            .get(self.chosen)
            .or_else(|| self.scopes.first())
            .unwrap_or(&EMPTY_SCOPE)
    }

    /// What the removed downtimes share, for the list's heading: `fixed ·
    /// 13:30 → 15:30 · m.keller`; `None` when they differ.
    pub(crate) fn shared(&self, snapshot: &Snapshot, now: Timestamp) -> Option<String> {
        let mut downtimes = self
            .scope()
            .removed
            .iter()
            .filter_map(|removed| find(snapshot, Some(&removed.object), &removed.name));
        let first = downtimes.next()?;
        if !downtimes.all(|other| twins(other, first)) {
            return None;
        }
        let kind = if first.fixed {
            "fixed".to_owned()
        } else {
            format!("flexible, {}", duration(first.duration))
        };
        Some(
            [kind, clock_window(first, now), first.author.clone()]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" · "),
        )
    }

    /// What follows: which problems notify again once their downtimes are
    /// gone (`disk /var and kubelet are critical: they notify again once
    /// their downtimes are gone.`).
    pub(crate) fn consequence(&self, snapshot: &Snapshot, now: Timestamp) -> String {
        let scope = self.scope();
        let removed: HashSet<&str> = scope
            .removed
            .iter()
            .map(|removed| removed.name.as_str())
            .collect();
        let mut objects: Vec<&ObjectKey> = Vec::new();
        for item in &scope.removed {
            if !objects.contains(&&item.object) {
                objects.push(&item.object);
            }
        }
        let one_host = objects
            .iter()
            .map(|object| object.host_name())
            .collect::<HashSet<_>>()
            .len()
            == 1;
        let mut problems: Vec<(String, CheckableState)> = Vec::new();
        for object in objects {
            let Some(state) = state_of(snapshot, object).filter(|state| state.is_problem()) else {
                continue;
            };
            let (acknowledged, host_problem) = match object {
                ObjectKey::Host { name } => (
                    snapshot
                        .hosts
                        .get(name)
                        .is_some_and(|host| host.check.acknowledgement.is_acknowledged()),
                    false,
                ),
                ObjectKey::Service { key } => (
                    snapshot
                        .services
                        .get(key)
                        .is_some_and(|service| service.check.acknowledgement.is_acknowledged()),
                    snapshot.host_of(key).is_some_and(|host| host.is_problem()),
                ),
            };
            let stays = of(snapshot, object).iter().any(|downtime| {
                !removed.contains(downtime.name.as_str())
                    && downtime.phase(now) == DowntimePhase::InEffect
            });
            if acknowledged || host_problem || stays {
                continue;
            }
            let name = match object {
                ObjectKey::Service { key } if one_host => key.name.to_string(),
                ObjectKey::Service { key } => format!("{} on {}", key.name, key.host),
                ObjectKey::Host { name } => name.to_string(),
            };
            problems.push((name, state));
        }
        consequence_text(&problems)
    }
}

static EMPTY_SCOPE: Scope = Scope {
    label: String::new(),
    removed: Vec::new(),
    send: Vec::new(),
    skipped: Vec::new(),
};

/// Removing `downtime` by name: it and its children go; children Icinga
/// doesn't know as its children (twins) are sent by name too.
fn scope_of(snapshot: &Snapshot, downtime: &Downtime) -> Scope {
    if downtime.config_owned {
        return Scope {
            label: String::new(),
            removed: Vec::new(),
            send: Vec::new(),
            skipped: vec![removed_of(downtime)],
        };
    }
    let children = children(snapshot, downtime);
    let mut removed = vec![removed_of(downtime)];
    removed.extend(children.iter().map(|child| removed_of(child)));
    let mut objects: Vec<ObjectKey> = Vec::new();
    for item in &removed {
        if !objects.contains(&item.object) {
            objects.push(item.object.clone());
        }
    }
    let mut send = vec![Send::One {
        name: downtime.name.clone(),
        objects,
    }];
    send.extend(
        children
            .iter()
            .filter(|child| child.parent.is_none())
            .map(|child| Send::One {
                name: child.name.clone(),
                objects: vec![child.object.clone()],
            }),
    );
    Scope {
        label: String::new(),
        removed,
        send,
        skipped: Vec::new(),
    }
}

fn removed_of(downtime: &Downtime) -> Removed {
    Removed {
        name: downtime.name.clone(),
        object: downtime.object.clone(),
    }
}

/// `13:30 → 15:30` (dates where they aren't today).
pub(crate) fn clock_window(downtime: &Downtime, now: Timestamp) -> String {
    format!(
        "{} → {}",
        format::clock(downtime.start_time, now),
        format::clock(downtime.end_time, now)
    )
}

/// The sentence for [`Removal::consequence`].
fn consequence_text(problems: &[(String, CheckableState)]) -> String {
    let word = |state: CheckableState| format::state_word(state);
    match problems {
        [] => "None of them is a problem now: nothing notifies because of this.".to_owned(),
        [(name, state)] => format!(
            "{name} is {}: it notifies again once its downtime is gone.",
            word(*state)
        ),
        many if many.len() > 4 => {
            let names: Vec<&str> = many.iter().take(3).map(|(name, _)| name.as_str()).collect();
            format!(
                "{} and {} more problems notify again once their downtimes are gone.",
                names.join(", "),
                many.len() - 3
            )
        }
        many if many.iter().all(|(_, state)| *state == many[0].1) => {
            let names: Vec<&str> = many.iter().map(|(name, _)| name.as_str()).collect();
            let (last, rest) = names.split_last().unwrap_or((&"", &[]));
            format!(
                "{} and {last} are {}: they notify again once their downtimes are gone.",
                rest.join(", "),
                word(many[0].1)
            )
        }
        many => {
            let parts: Vec<String> = many
                .iter()
                .map(|(name, state)| format!("{name} is {}", word(*state)))
                .collect();
            format!(
                "{}: they notify again once their downtimes are gone.",
                parts.join(", ")
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use chrono::Utc;
    use ic_model::{AckKind, Host, HostState, Service, ServiceState};

    use super::*;

    /// The mock-ups' wall clock: Wednesday 7 October 2026, 14:12 (UTC).
    fn now() -> Timestamp {
        at(14, 12)
    }

    fn at(hour: u32, minute: u32) -> Timestamp {
        day_at(7, hour, minute)
    }

    #[expect(clippy::cast_precision_loss, reason = "test timestamps")]
    fn day_at(day: u32, hour: u32, minute: u32) -> Timestamp {
        let when = Utc
            .with_ymd_and_hms(2026, 10, day, hour, minute, 0)
            .single()
            .unwrap();
        Timestamp::from_unix_seconds(when.timestamp() as f64)
    }

    fn downtime(object: ObjectKey, name: &str, start: Timestamp, end: Timestamp) -> Downtime {
        Downtime {
            name: format!("{}!{name}", object.full_name()),
            object,
            author: "m.keller".to_owned(),
            comment: "why".to_owned(),
            start_time: start,
            end_time: end,
            fixed: true,
            duration: 0.0,
            entry_time: at(13, 24),
            trigger_time: None,
            triggered_by: None,
            parent: None,
            in_effect: start <= now() && now() < end,
            config_owned: false,
            schedule: None,
        }
    }

    fn host(name: &str, state: HostState, depth: u32) -> Host {
        let mut host = Host::new(name);
        host.state = state;
        host.check.downtime_depth = depth;
        host
    }

    fn service(host: &str, name: &str, state: ServiceState, depth: u32) -> Service {
        let mut service = Service::new(host, name);
        service.state = state;
        service.check.downtime_depth = depth;
        service
    }

    fn snapshot(hosts: Vec<Host>, services: Vec<Service>, downtimes: Vec<Downtime>) -> Snapshot {
        let mut by_object: BTreeMap<ObjectKey, Vec<Downtime>> = BTreeMap::new();
        for downtime in downtimes {
            by_object
                .entry(downtime.object.clone())
                .or_default()
                .push(downtime);
        }
        Snapshot {
            hosts: Arc::new(
                hosts
                    .into_iter()
                    .map(|host| (host.name.clone(), Arc::new(host)))
                    .collect(),
            ),
            services: Arc::new(
                services
                    .into_iter()
                    .map(|service| (service.key.clone(), Arc::new(service)))
                    .collect(),
            ),
            downtimes: Arc::new(by_object),
            ..Snapshot::default()
        }
    }

    fn texts(banner: &Banner) -> Vec<String> {
        banner
            .facts
            .iter()
            .map(|fact| match fact {
                Fact::Text(text) => text.clone(),
                Fact::Host(host) => format!("host {host}"),
            })
            .collect()
    }

    /// k8s-node-07 in downtime with its three services (1c), one of them
    /// critical.
    fn host_with_services(linked: bool) -> Snapshot {
        let window = (at(13, 30), at(15, 30));
        let host_downtime = downtime(ObjectKey::host("k8s-node-07"), "h", window.0, window.1);
        let mut downtimes = vec![host_downtime.clone()];
        for name in ["disk /var", "kubelet", "memory"] {
            let mut child = downtime(
                ObjectKey::service("k8s-node-07", name),
                name,
                window.0,
                window.1,
            );
            child.parent = linked.then(|| host_downtime.name.clone());
            downtimes.push(child);
        }
        snapshot(
            vec![host("k8s-node-07", HostState::Up, 1)],
            vec![
                service("k8s-node-07", "disk /var", ServiceState::Critical, 1),
                service("k8s-node-07", "kubelet", ServiceState::Critical, 1),
                service("k8s-node-07", "memory", ServiceState::Ok, 1),
            ],
            downtimes,
        )
    }

    #[test]
    fn a_fixed_downtime_in_effect_says_how_long_is_left() {
        let object = ObjectKey::service("db-prod-03", "postgres-replication");
        let mut drill = downtime(object.clone(), "d", at(13, 0), at(16, 0));
        drill.author = "j.berg".to_owned();
        drill.entry_time = at(12, 41);
        let snapshot = snapshot(
            vec![host("db-prod-03", HostState::Up, 0)],
            vec![service(
                "db-prod-03",
                "postgres-replication",
                ServiceState::Critical,
                1,
            )],
            vec![drill],
        );
        let banner = banner_in(&snapshot, &object, now(), &Utc).unwrap();
        assert!(banner.in_effect);
        assert_eq!(banner.title, "In downtime");
        assert_eq!(banner.status, "1h 48m left");
        assert_eq!(
            texts(&banner),
            ["fixed", "13:00 → 16:00 today", "started 1h 12m ago"]
        );
        assert_eq!(
            (banner.author.as_str(), banner.entered.as_str()),
            ("j.berg", "12:41")
        );
        assert!((banner.progress - 0.4).abs() < 0.01);
        assert!(banner.more.is_empty());
    }

    #[test]
    fn a_service_in_its_hosts_downtime_names_the_host() {
        for linked in [true, false] {
            let snapshot = host_with_services(linked);
            let object = ObjectKey::service("k8s-node-07", "disk /var");
            let banner = banner_in(&snapshot, &object, now(), &Utc).unwrap();
            assert_eq!(banner.title, "In downtime with its host", "linked {linked}");
            assert_eq!(banner.status, "1h 18m left");
            assert_eq!(
                texts(&banner),
                [
                    "host k8s-node-07",
                    "fixed",
                    "13:30 → 15:30 today",
                    "host and all its services"
                ]
            );
            let tag = tag_in(
                &snapshot,
                &object,
                &snapshot.services[object.as_service().unwrap()].check,
                now(),
                &Utc,
            );
            assert_eq!(tag.as_deref(), Some("host downtime 1h 18m"));
        }
        // The host's own pane says what it covers.
        let snapshot = host_with_services(true);
        let banner = banner_in(&snapshot, &ObjectKey::host("k8s-node-07"), now(), &Utc).unwrap();
        assert_eq!(banner.title, "In downtime");
        assert_eq!(texts(&banner)[2], "host and its 3 services");
    }

    #[test]
    fn downtimes_not_in_effect_yet_are_grey() {
        let locks = ObjectKey::service("db-prod-05", "pg-locks");
        let mut flexible = downtime(locks.clone(), "f", at(14, 0), at(18, 0));
        flexible.fixed = false;
        flexible.duration = 7_200.0;
        flexible.in_effect = false;
        let haproxy = ObjectKey::service("lb-prod-02", "haproxy-backend");
        let later = downtime(haproxy.clone(), "l", at(22, 0), at(23, 0));
        let snapshot = snapshot(
            vec![
                host("db-prod-05", HostState::Up, 0),
                host("lb-prod-02", HostState::Up, 0),
            ],
            vec![
                service("db-prod-05", "pg-locks", ServiceState::Ok, 0),
                service("lb-prod-02", "haproxy-backend", ServiceState::Warning, 0),
            ],
            vec![flexible, later],
        );
        let banner = banner_in(&snapshot, &locks, now(), &Utc).unwrap();
        assert!(!banner.in_effect);
        assert_eq!(
            (banner.title, banner.status.as_str()),
            ("Flexible downtime", "not started")
        );
        assert_eq!(
            texts(&banner),
            [
                "lasts 2h from the first problem",
                "window 14:00 → 18:00 today"
            ]
        );
        assert!(banner.progress.abs() < f32::EPSILON);
        let check = |key: &ObjectKey| snapshot.services[key.as_service().unwrap()].check.clone();
        assert_eq!(
            tag_in(&snapshot, &locks, &check(&locks), now(), &Utc).as_deref(),
            Some("downtime, flexible, not started")
        );

        let banner = banner_in(&snapshot, &haproxy, now(), &Utc).unwrap();
        assert_eq!(
            (banner.title, banner.status.as_str()),
            ("Downtime scheduled", "starts in 7h 48m")
        );
        assert_eq!(
            texts(&banner),
            [
                "fixed",
                "22:00 → 23:00 today",
                "not in effect yet: the problem is unhandled"
            ]
        );
        assert_eq!(
            tag_in(&snapshot, &haproxy, &check(&haproxy), now(), &Utc).as_deref(),
            Some("downtime at 22:00")
        );
        // Another day reads the same way.
        let saturday = downtime(haproxy.clone(), "s", day_at(10, 2, 21), day_at(10, 6, 0));
        let checked = check(&haproxy);
        let later = Snapshot {
            downtimes: Arc::new(BTreeMap::from([(haproxy.clone(), vec![saturday])])),
            ..snapshot.clone()
        };
        let tag = tag_in(&later, &haproxy, &checked, now(), &Utc).unwrap();
        assert!(tag.starts_with("downtime at "), "{tag}");
        assert!(tag.ends_with(" 02:21"), "{tag}");
    }

    #[test]
    fn a_flexible_downtime_a_problem_started_runs_for_its_duration() {
        let bloat = ObjectKey::service("db-prod-05", "pg-bloat");
        let mut flexible = downtime(bloat.clone(), "f", at(13, 0), at(17, 0));
        flexible.fixed = false;
        flexible.duration = 7_200.0;
        flexible.trigger_time = Some(at(13, 24));
        flexible.in_effect = true;
        let snapshot = snapshot(
            vec![host("db-prod-05", HostState::Up, 0)],
            vec![service("db-prod-05", "pg-bloat", ServiceState::Warning, 1)],
            vec![flexible],
        );
        let banner = banner_in(&snapshot, &bloat, now(), &Utc).unwrap();
        assert_eq!(
            (banner.title, banner.status.as_str()),
            ("In downtime", "1h 12m left")
        );
        assert_eq!(
            texts(&banner),
            ["flexible, 2h from 13:24", "window 13:00 → 17:00 today"]
        );
        assert_eq!(
            tag_in(
                &snapshot,
                &bloat,
                &snapshot.services[bloat.as_service().unwrap()].check,
                now(),
                &Utc
            )
            .as_deref(),
            Some("downtime, flexible 1h 12m")
        );
    }

    #[test]
    fn a_host_with_three_downtimes_shows_the_one_in_effect() {
        let switch = ObjectKey::host("sw-core-ams-02");
        let swap = downtime(switch.clone(), "a", at(14, 0), at(15, 0));
        let mut upgrade = downtime(switch.clone(), "b", at(22, 0), day_at(8, 6, 0));
        upgrade.fixed = false;
        upgrade.duration = 3_600.0;
        let mut patching = downtime(switch.clone(), "c", day_at(10, 6, 0), day_at(10, 10, 0));
        patching.config_owned = true;
        patching.schedule = Some("weekly-patching".to_owned());
        patching.author = "icingaadmin".to_owned();
        let snapshot = snapshot(
            vec![host("sw-core-ams-02", HostState::Up, 1)],
            Vec::new(),
            vec![swap, upgrade, patching],
        );
        let banner = banner_in(&snapshot, &switch, now(), &Utc).unwrap();
        assert_eq!(banner.status, "48m left");
        assert_eq!(
            banner.more,
            [
                "+ 2 more below",
                "tonight 22:00, flexible",
                "Sat 06:00, from config"
            ]
        );
    }

    #[test]
    fn a_host_only_downtime_leaves_its_services_alone() {
        let mut bmc = downtime(ObjectKey::host("k8s-node-02"), "b", at(14, 0), at(15, 0));
        bmc.comment = "BMC firmware update".to_owned();
        let snapshot = snapshot(
            vec![host("k8s-node-02", HostState::Up, 1)],
            vec![service(
                "k8s-node-02",
                "ntp-offset",
                ServiceState::Warning,
                0,
            )],
            vec![bmc],
        );
        let ntp = &snapshot.services[&ic_model::ServiceKey::new("k8s-node-02", "ntp-offset")];
        assert_eq!(
            host_marker_in(&snapshot, ntp, now(), &Utc).as_deref(),
            Some("until 15:00")
        );
        assert!(
            banner_in(&snapshot, &ntp.object_key(), now(), &Utc).is_none(),
            "the service isn't in downtime"
        );
        let host = banner_in(&snapshot, &ObjectKey::host("k8s-node-02"), now(), &Utc).unwrap();
        assert_eq!(texts(&host)[2], "the host only, not its services");
        // In downtime itself: the banner says it, not the marker.
        let covered = host_with_services(true);
        let disk = &covered.services[&ic_model::ServiceKey::new("k8s-node-07", "disk /var")];
        assert_eq!(host_marker_in(&covered, disk, now(), &Utc), None);
    }

    #[test]
    fn windows_read_from_now() {
        let window = |start, end| window_in(start, end, now(), &Utc);
        assert_eq!(window(at(13, 0), at(16, 0)), "13:00 → 16:00 today");
        assert_eq!(window(at(22, 0), day_at(8, 6, 0)), "tonight 22:00 → 06:00");
        assert_eq!(
            window(day_at(8, 8, 0), day_at(8, 10, 0)),
            "08:00 → 10:00 tomorrow"
        );
        assert_eq!(
            window(day_at(10, 6, 0), day_at(10, 10, 0)),
            "Sat 10 Oct 06:00 → 10:00"
        );
        assert_eq!(
            window(at(9, 0), day_at(9, 9, 0)),
            "today 09:00 → Fri 9 Oct 09:00"
        );
    }

    #[test]
    fn time_left_rounds_up_to_minutes() {
        assert_eq!(left(Duration::from_mins(48)), "48m");
        assert_eq!(left(Duration::from_secs(48 * 60 - 20)), "48m");
        assert_eq!(left(Duration::from_secs(20)), "1m");
        assert_eq!(left(Duration::from_mins(68)), "1h 08m");
        assert_eq!(left(Duration::from_hours(52)), "2d 4h");
    }

    #[test]
    fn removing_a_service_downtime_of_its_host_offers_the_whole_host() {
        for linked in [true, false] {
            let snapshot = host_with_services(linked);
            let disk = ObjectKey::service("k8s-node-07", "disk /var");
            let name = format!("{}!disk /var", disk.full_name());
            let removal = Removal::downtime(&snapshot, &disk, &name).unwrap();
            assert_eq!(removal.scopes.len(), 2);
            assert_eq!(removal.host.as_deref(), Some("k8s-node-07"));
            assert_eq!(removal.chosen, 1, "the host's whole downtime, as drawn");
            assert_eq!(removal.scopes[0].label, "this service only");
            assert_eq!(removal.scopes[0].removed.len(), 1);
            assert_eq!(removal.scopes[1].label, "the host and its 3 services");
            let whole = removal.scope();
            assert_eq!(whole.removed.len(), 4);
            assert_eq!(whole.removed[0].object, ObjectKey::host("k8s-node-07"));
            // Icinga removes linked children with their parent; twins go by
            // name.
            assert_eq!(
                whole.send.len(),
                if linked { 1 } else { 4 },
                "linked {linked}"
            );
            assert_eq!(
                removal.shared(&snapshot, now()).as_deref(),
                Some(format!(
                    "fixed · {} · m.keller",
                    clock_window(&find(&snapshot, None, &name).unwrap().clone(), now())
                ))
                .as_deref()
            );
            assert_eq!(
                removal.consequence(&snapshot, now()),
                "disk /var and kubelet are critical: they notify again once their downtimes \
                 are gone."
            );
        }
    }

    #[test]
    fn removing_every_downtime_of_a_host_and_its_services_sends_the_parent_once() {
        let snapshot = host_with_services(true);
        let mut objects = vec![ObjectKey::host("k8s-node-07")];
        objects.extend(
            ["disk /var", "kubelet", "memory"]
                .into_iter()
                .map(|service| ObjectKey::service("k8s-node-07", service)),
        );
        let removal = Removal::all_of(&snapshot, &objects);
        let scope = removal.scope();
        assert_eq!(scope.removed.len(), 4, "every downtime is listed once");
        let names: Vec<&str> = scope
            .send
            .iter()
            .map(|Send::One { name, .. }| name.as_str())
            .collect();
        assert_eq!(names, ["k8s-node-07!h"], "the children go with it");
        // Only the services: each child goes by its own name.
        let removal = Removal::all_of(&snapshot, &objects[1..]);
        assert_eq!(removal.scope().send.len(), 3);
    }

    #[test]
    fn removing_every_downtime_skips_the_configs() {
        let switch = ObjectKey::host("sw-core-ams-02");
        let swap = downtime(switch.clone(), "a", at(14, 0), at(15, 0));
        let mut patching = downtime(switch.clone(), "c", day_at(10, 6, 0), day_at(10, 10, 0));
        patching.config_owned = true;
        let other = ObjectKey::host("db-prod-05");
        let plain = downtime(other.clone(), "x", at(14, 0), at(15, 0));
        let snapshot = snapshot(
            vec![
                host("sw-core-ams-02", HostState::Down, 1),
                host("db-prod-05", HostState::Up, 1),
            ],
            Vec::new(),
            vec![swap.clone(), patching, plain.clone()],
        );
        let removal = Removal::all_of(&snapshot, &[switch.clone(), other.clone()]);
        let scope = removal.scope();
        assert_eq!(scope.removed.len(), 2);
        assert_eq!(scope.skipped.len(), 1, "the config's");
        assert_eq!(
            scope.send,
            [
                Send::One {
                    name: swap.name.clone(),
                    objects: vec![switch]
                },
                Send::One {
                    name: plain.name.clone(),
                    objects: vec![other]
                },
            ],
            "only the listed downtimes go, by name"
        );
        assert_eq!(
            removal.consequence(&snapshot, now()),
            "sw-core-ams-02 is down: it notifies again once its downtime is gone."
        );
    }

    #[test]
    fn the_consequence_names_the_problems_that_notify_again() {
        let critical = CheckableState::Service(ServiceState::Critical);
        let warning = CheckableState::Service(ServiceState::Warning);
        let name = |text: &str| text.to_owned();
        assert!(consequence_text(&[]).starts_with("None of them is a problem"));
        assert_eq!(
            consequence_text(&[(name("load"), warning), (name("disk /"), critical)]),
            "load is warning, disk / is critical: they notify again once their downtimes are \
             gone."
        );
        let many: Vec<(String, CheckableState)> = (0..6)
            .map(|index| (format!("s{index}"), critical))
            .collect();
        assert_eq!(
            consequence_text(&many),
            "s0, s1, s2 and 3 more problems notify again once their downtimes are gone."
        );
    }

    #[test]
    fn acknowledged_problems_dont_notify_again() {
        let mut snapshot = host_with_services(true);
        let services = Arc::make_mut(&mut snapshot.services);
        for service in services.values_mut() {
            Arc::make_mut(service).check.acknowledgement = AckKind::Normal;
        }
        let removal =
            Removal::downtime(&snapshot, &ObjectKey::host("k8s-node-07"), "k8s-node-07!h").unwrap();
        assert_eq!(
            removal.scopes.len(),
            1,
            "the host's own downtime: one scope"
        );
        assert_eq!(removal.scope().removed.len(), 4);
        assert!(
            removal
                .consequence(&snapshot, now())
                .starts_with("None of them is a problem"),
            "{}",
            removal.consequence(&snapshot, now())
        );
    }
}
