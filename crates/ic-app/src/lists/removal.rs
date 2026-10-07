//! Removing from the handling and downtimes views (topics 07 and 14) and
//! the pane's thread: the confirmation's content, before
//! anything is sent. Every removal confirms and lists every target (the
//! dialog's box scrolls): downtimes grouped by the downtime they belong to
//! (a host's downtime lists the host and each service that goes with it),
//! comments with author and time, acknowledgements with who set them,
//! sticky and expiry. It says what follows and what it skips with the
//! reason (acknowledgement comments: remove the acknowledgement; downtimes
//! from the config), and the button counts what goes.
//!
//! What is sent is one action for the lot: downtimes and comments by name
//! (Icinga takes the names in batches, never a request each),
//! acknowledgements by object. Pure, so it's tested without a window.

use std::collections::{HashMap, HashSet};
use std::fmt::Display;

use chrono::{Local, TimeZone};
use ic_core::snapshot::Snapshot;
use ic_model::{
    AckKind, Action, ActionTarget, CheckableState, Comment, CommentKind, Downtime, DowntimePhase,
    ObjectKey, Timestamp,
};

use super::model::{ack_comment, day_clock, short_when};
use crate::actions::ObjectAction;
use crate::downtimes;
use crate::operate::ActionSpec;
use crate::operate::forms::describe_objects;

/// How many names a sentence lists before `and N more`.
const NAMED: usize = 6;

/// One line of the confirmation's box.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TargetRow {
    /// What the targets under it share: `k8s-node-07 · host and 23
    /// services · fixed 13:30 → 15:30 · m.keller`.
    Heading(String),
    /// A host or service, and a faint detail (`host`, `j.berg 13:41`).
    Target {
        /// The object.
        object: ObjectKey,
        /// The detail, or empty.
        meta: String,
    },
}

/// What a removal removes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RemovalKind {
    /// Downtimes, by name.
    Downtimes,
    /// Free-standing comments, by name.
    Comments,
    /// Acknowledgements, by object.
    Acknowledgements,
    /// Several of these at once (handling's marks), each in its part of
    /// the box.
    Mixed,
}

/// A removal from a list, before anything is sent.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct BulkRemoval {
    /// What it removes.
    pub(crate) kind: RemovalKind,
    /// How many rows were selected.
    pub(crate) selected: usize,
    /// What the box lists, in order.
    pub(crate) rows: Vec<TargetRow>,
    /// How many downtimes, comments or acknowledgements go (the button).
    pub(crate) count: usize,
    /// What it leaves alone, and why.
    pub(crate) skipped: Vec<String>,
    /// What follows (the problems that notify again).
    pub(crate) consequence: Option<String>,
    /// A fainter note (children go with their parent, acknowledgement
    /// comments go too).
    pub(crate) note: Option<String>,
    /// What is sent.
    pub(crate) specs: Vec<ActionSpec>,
}

impl BulkRemoval {
    /// The dialog's title.
    pub(crate) fn title(&self) -> &'static str {
        match self.kind {
            RemovalKind::Downtimes => "Remove downtimes",
            RemovalKind::Comments => "Remove comments",
            RemovalKind::Acknowledgements => "Remove acknowledgements",
            RemovalKind::Mixed => "Remove",
        }
    }

    /// What the title names: `3 selected · 26 downtimes`, `3 selected`.
    pub(crate) fn what(&self) -> String {
        match self.kind {
            RemovalKind::Acknowledgements | RemovalKind::Mixed => {
                format!("{} selected", self.selected)
            }
            RemovalKind::Downtimes | RemovalKind::Comments => {
                format!("{} selected · {}", self.selected, self.counted())
            }
        }
    }

    /// `26 downtimes`, `1 comment`, `3 acknowledgements`, `5 records`.
    fn counted(&self) -> String {
        let (one, many) = self.nouns();
        format!(
            "{} {}",
            self.count,
            if self.count == 1 { one } else { many }
        )
    }

    fn nouns(&self) -> (&'static str, &'static str) {
        match self.kind {
            RemovalKind::Downtimes => ("downtime", "downtimes"),
            RemovalKind::Comments => ("comment", "comments"),
            RemovalKind::Acknowledgements => ("acknowledgement", "acknowledgements"),
            RemovalKind::Mixed => ("record", "records"),
        }
    }

    /// The label above the box: `26 downtimes on 1 host and 25 services`
    /// (downtimes only).
    pub(crate) fn label(&self) -> Option<String> {
        if self.kind != RemovalKind::Downtimes || self.count == 0 {
            return None;
        }
        let objects: HashSet<&ObjectKey> = self
            .rows
            .iter()
            .filter_map(|row| match row {
                TargetRow::Target { object, .. } => Some(object),
                TargetRow::Heading(_) => None,
            })
            .collect();
        let hosts = objects
            .iter()
            .filter(|object| matches!(object, ObjectKey::Host { .. }))
            .count();
        let services = objects.len() - hosts;
        let plural = |count: usize, one: &str, many: &str| {
            format!("{count} {}", if count == 1 { one } else { many })
        };
        let on = match (hosts, services) {
            (0, services) => plural(services, "service", "services"),
            (hosts, 0) => plural(hosts, "host", "hosts"),
            (hosts, services) => format!(
                "{} and {}",
                plural(hosts, "host", "hosts"),
                plural(services, "service", "services")
            ),
        };
        Some(format!("{} on {on}", self.counted()))
    }

    /// The danger button: `remove 26 downtimes`, `remove comment`,
    /// `remove all 5`.
    pub(crate) fn button(&self) -> String {
        let (one, many) = self.nouns();
        match (self.kind, self.count) {
            (RemovalKind::Mixed, count) => format!("remove all {count}"),
            (_, 1) => format!("remove {one}"),
            (_, count) => format!("remove {count} {many}"),
        }
    }

    /// Several removals as one confirmation (handling's marks of several
    /// kinds): each part under a heading naming what it removes, the
    /// words of each, and everything sent with the button. One part stays
    /// as it is.
    pub(crate) fn merge(parts: Vec<Self>) -> Option<Self> {
        let mut parts: Vec<Self> = parts.into_iter().filter(|part| part.selected > 0).collect();
        if parts.len() <= 1 {
            return parts.pop();
        }
        let mut merged = Self {
            kind: RemovalKind::Mixed,
            selected: 0,
            rows: Vec::new(),
            count: 0,
            skipped: Vec::new(),
            consequence: None,
            note: None,
            specs: Vec::new(),
        };
        let mut consequences = Vec::new();
        let mut notes = Vec::new();
        for part in parts {
            if part.count > 0 {
                merged.rows.push(TargetRow::Heading(part.counted()));
            }
            merged.selected += part.selected;
            merged.count += part.count;
            merged.rows.extend(part.rows);
            merged.skipped.extend(part.skipped);
            consequences.extend(part.consequence);
            notes.extend(part.note);
            merged.specs.extend(part.specs);
        }
        merged.consequence = (!consequences.is_empty()).then(|| consequences.join(" "));
        merged.note = (!notes.is_empty()).then(|| notes.join(" "));
        Some(merged)
    }

    /// The objects it changes.
    #[cfg(test)]
    pub(crate) fn objects(&self) -> Vec<ObjectKey> {
        self.specs
            .iter()
            .flat_map(|spec| spec.objects.iter().cloned())
            .collect()
    }
}

/// Removing the downtimes `names` (in list order), in the local time zone.
pub(crate) fn downtimes(snapshot: &Snapshot, names: &[String], now: Timestamp) -> BulkRemoval {
    downtimes_in(snapshot, names, now, &Local)
}

/// [`downtimes`] in time zone `zone`.
#[expect(
    clippy::too_many_lines,
    reason = "grouping, skipping and the words of one dialog"
)]
pub(crate) fn downtimes_in<Tz>(
    snapshot: &Snapshot,
    names: &[String],
    now: Timestamp,
    zone: &Tz,
) -> BulkRemoval
where
    Tz: TimeZone,
    Tz::Offset: Display,
{
    let by_name: HashMap<&str, &Downtime> = snapshot
        .downtimes
        .values()
        .flatten()
        .map(|downtime| (downtime.name.as_str(), downtime))
        .collect();
    let selected: HashSet<&str> = names.iter().map(String::as_str).collect();
    // A selected downtime whose parent (or its parent's) goes too is
    // listed with it.
    let covered = |downtime: &Downtime| {
        let mut parent = downtime.parent.as_deref();
        for _ in 0..8 {
            let Some(name) = parent else {
                return false;
            };
            if selected.contains(name) {
                return true;
            }
            parent = by_name.get(name).and_then(|up| up.parent.as_deref());
        }
        false
    };
    let mut rows = Vec::new();
    let mut sent: Vec<String> = Vec::new();
    let mut objects: Vec<ObjectKey> = Vec::new();
    let mut seen_objects: HashSet<ObjectKey> = HashSet::new();
    let mut removed: HashSet<&str> = HashSet::new();
    let mut config: Vec<&Downtime> = Vec::new();
    let mut gone = 0;
    let mut children_total = 0;
    let mut child_hosts = 0;
    let mut parents = 0;
    let mut count = 0;
    for name in names {
        let Some(downtime) = by_name.get(name.as_str()).copied() else {
            gone += 1;
            continue;
        };
        if covered(downtime) || removed.contains(downtime.name.as_str()) {
            continue;
        }
        if downtime.config_owned {
            config.push(downtime);
            continue;
        }
        let children = downtimes::children(snapshot, downtime);
        rows.push(TargetRow::Heading(heading(downtime, &children, now, zone)));
        let group = std::iter::once(downtime).chain(children.iter().copied());
        for member in group {
            if !removed.insert(member.name.as_str()) {
                continue;
            }
            count += 1;
            rows.push(TargetRow::Target {
                object: member.object.clone(),
                meta: match member.object {
                    ObjectKey::Host { .. } => "host".to_owned(),
                    ObjectKey::Service { .. } => String::new(),
                },
            });
            if seen_objects.insert(member.object.clone()) {
                objects.push(member.object.clone());
            }
        }
        sent.push(downtime.name.clone());
        // Children Icinga doesn't know as its children go by name too.
        sent.extend(
            children
                .iter()
                .filter(|child| child.parent.is_none())
                .map(|child| child.name.clone()),
        );
        if !children.is_empty() {
            parents += 1;
            children_total += children.len();
            child_hosts += children
                .iter()
                .filter(|child| matches!(child.object, ObjectKey::Host { .. }))
                .count();
        }
    }
    let mut skipped = Vec::new();
    if !config.is_empty() {
        let names: Vec<String> = config
            .iter()
            .map(|downtime| describe_objects(std::slice::from_ref(&downtime.object)))
            .collect();
        let schedules: Vec<&str> = config
            .iter()
            .filter_map(|downtime| downtime.schedule.as_deref())
            .collect();
        let from = if schedules.is_empty() {
            "the config".to_owned()
        } else {
            format!("the config ({})", dedup(&schedules).join(", "))
        };
        skipped.push(format!(
            "skipped: {}, from {from}: Icinga refuses to remove {}, and the config brings {} back.",
            listing(&names),
            if config.len() == 1 { "it" } else { "them" },
            if config.len() == 1 { "it" } else { "them" },
        ));
    }
    if gone > 0 {
        skipped.push(gone_line(gone, "downtime"));
    }
    let consequence = (count > 0).then(|| notify_again(snapshot, &objects, &removed, now));
    let note = (parents > 0).then(|| {
        let kind = match (child_hosts, children_total - child_hosts) {
            (0, _) => "service ",
            (_, 0) => "host ",
            _ => "",
        };
        let noun = if children_total == 1 {
            "downtime"
        } else {
            "downtimes"
        };
        if parents == 1 {
            format!(
                "The host downtime's {children_total} {kind}{noun} {} with it: Icinga removes \
                 the children of a downtime it removes.",
                if children_total == 1 { "goes" } else { "go" },
            )
        } else {
            format!(
                "The host downtimes' {children_total} {kind}{noun} go with them: Icinga \
                 removes the children of a downtime it removes."
            )
        }
    });
    let specs = if sent.is_empty() {
        Vec::new()
    } else {
        vec![ActionSpec {
            kind: ObjectAction::RemoveNamedDowntimes(sent.clone()),
            action: Action::RemoveAllDowntimes,
            target: ActionTarget::Downtimes(sent),
            objects,
        }]
    };
    BulkRemoval {
        kind: RemovalKind::Downtimes,
        selected: names.len(),
        rows,
        count,
        skipped,
        consequence,
        note,
        specs,
    }
}

/// What a downtime and the ones going with it share: `k8s-node-07 · host
/// and 23 services · fixed 13:30 → 15:30 · m.keller`, `fixed 13:00 →
/// 16:00 · j.berg`.
fn heading<Tz>(downtime: &Downtime, children: &[&Downtime], now: Timestamp, zone: &Tz) -> String
where
    Tz: TimeZone,
    Tz::Offset: Display,
{
    let window = format!(
        "{} → {}",
        day_clock(downtime.start_time, now, zone),
        day_clock(downtime.end_time, now, zone)
    );
    let kind = if downtime.fixed {
        format!("fixed {window}")
    } else {
        format!("flexible {window}")
    };
    let mut parts = Vec::new();
    if let ObjectKey::Host { name } = &downtime.object {
        parts.push(name.to_string());
        let hosts = children
            .iter()
            .filter(|child| matches!(child.object, ObjectKey::Host { .. }))
            .count();
        let services = children.len() - hosts;
        let plural = |count: usize, one: &str, many: &str| {
            format!("{count} {}", if count == 1 { one } else { many })
        };
        match (hosts, services) {
            (0, 0) => {}
            (0, services) => parts.push(format!(
                "host and {}",
                plural(services, "service", "services")
            )),
            (hosts, 0) => parts.push(format!("host and {}", plural(hosts, "host", "hosts"))),
            (hosts, services) => parts.push(format!(
                "host, {} and {}",
                plural(hosts, "host", "hosts"),
                plural(services, "service", "services")
            )),
        }
    }
    parts.push(kind);
    parts.push(downtime.author.clone());
    parts.join(" · ")
}

/// Which of `objects` notify again once the downtimes `removed` are gone:
/// problems that aren't acknowledged, aren't behind their host's problem,
/// and keep no other downtime in effect.
fn notify_again(
    snapshot: &Snapshot,
    objects: &[ObjectKey],
    removed: &HashSet<&str>,
    now: Timestamp,
) -> String {
    // Grouped by host, in the order they're listed.
    let mut by_host: Vec<(String, Vec<String>)> = Vec::new();
    for object in objects {
        let Some(state) = downtimes::state_of(snapshot, object).filter(|state| state.is_problem())
        else {
            continue;
        };
        if stays_handled(snapshot, object, state) {
            continue;
        }
        let stays = downtimes::of(snapshot, object).iter().any(|downtime| {
            !removed.contains(downtime.name.as_str())
                && downtime.phase(now) == DowntimePhase::InEffect
        });
        if stays {
            continue;
        }
        let host = object.host_name().to_string();
        let index = by_host
            .iter()
            .position(|(name, _)| *name == host)
            .unwrap_or_else(|| {
                by_host.push((host, Vec::new()));
                by_host.len() - 1
            });
        if let ObjectKey::Service { key } = object {
            by_host[index].1.push(key.name.to_string());
        }
    }
    if by_host.is_empty() {
        return "None of them is a problem now: nothing notifies because of this.".to_owned();
    }
    // `disk /var and kubelet on k8s-node-07, postgres-replication on
    // db-prod-03`; a host that is a problem itself by its name.
    let shown = by_host.len().min(NAMED);
    let mut parts: Vec<String> = by_host[..shown]
        .iter()
        .map(|(host, services)| {
            if services.is_empty() {
                host.clone()
            } else {
                format!("{} on {host}", and_list(services))
            }
        })
        .collect();
    if by_host.len() > shown {
        parts.push(format!("{} more hosts", by_host.len() - shown));
    }
    format!(
        "Their problems notify again once the downtimes are gone: {}.",
        parts.join(", ")
    )
}

/// Whether `object`'s problem stays handled whatever its downtimes:
/// acknowledged, or a service of a host with a problem.
fn stays_handled(snapshot: &Snapshot, object: &ObjectKey, _state: CheckableState) -> bool {
    match object {
        ObjectKey::Host { name } => snapshot
            .hosts
            .get(name)
            .is_some_and(|host| host.check.acknowledgement.is_acknowledged()),
        ObjectKey::Service { key } => {
            snapshot
                .services
                .get(key)
                .is_some_and(|service| service.check.acknowledgement.is_acknowledged())
                || snapshot.host_of(key).is_some_and(|host| host.is_problem())
        }
    }
}

/// Removing the comments `names` (in list order), in the local time zone.
pub(crate) fn comments(snapshot: &Snapshot, names: &[String], now: Timestamp) -> BulkRemoval {
    comments_in(snapshot, names, now, &Local)
}

/// [`comments`] in time zone `zone`.
pub(crate) fn comments_in<Tz>(
    snapshot: &Snapshot,
    names: &[String],
    now: Timestamp,
    zone: &Tz,
) -> BulkRemoval
where
    Tz: TimeZone,
    Tz::Offset: Display,
{
    let by_name: HashMap<&str, &Comment> = snapshot
        .comments
        .values()
        .flatten()
        .map(|comment| (comment.name.as_str(), comment))
        .collect();
    let mut rows = Vec::new();
    let mut sent = Vec::new();
    let mut objects = Vec::new();
    let mut seen = HashSet::new();
    let mut acknowledgements = Vec::new();
    let mut of_downtimes = Vec::new();
    let mut gone = 0;
    for name in names {
        let Some(comment) = by_name.get(name.as_str()).copied() else {
            gone += 1;
            continue;
        };
        let what = describe_objects(std::slice::from_ref(&comment.object));
        match comment.kind {
            CommentKind::Acknowledgement => {
                acknowledgements.push(what);
                continue;
            }
            CommentKind::Downtime => {
                of_downtimes.push(what);
                continue;
            }
            CommentKind::User | CommentKind::Flapping => {}
        }
        rows.push(TargetRow::Target {
            object: comment.object.clone(),
            meta: format!(
                "{} {}",
                comment.author,
                day_clock(comment.entry_time, now, zone)
            ),
        });
        sent.push(comment.name.clone());
        if seen.insert(comment.object.clone()) {
            objects.push(comment.object.clone());
        }
    }
    let mut skipped = Vec::new();
    match acknowledgements.as_slice() {
        [] => {}
        [one] => skipped.push(format!(
            "skipped: {one}, an acknowledgement. Remove the acknowledgement instead."
        )),
        many => skipped.push(format!(
            "skipped: {}, acknowledgements. Remove the acknowledgements instead.",
            listing(many)
        )),
    }
    match of_downtimes.as_slice() {
        [] => {}
        [one] => skipped.push(format!(
            "skipped: {one}, a downtime's comment. Remove the downtime instead."
        )),
        many => skipped.push(format!(
            "skipped: {}, downtimes' comments. Remove the downtimes instead.",
            listing(many)
        )),
    }
    if gone > 0 {
        skipped.push(gone_line(gone, "comment"));
    }
    let count = sent.len();
    let specs = if sent.is_empty() {
        Vec::new()
    } else {
        vec![ActionSpec {
            kind: ObjectAction::RemoveComments(sent.clone()),
            action: Action::RemoveAllDowntimes,
            target: ActionTarget::Comments(sent),
            objects,
        }]
    };
    BulkRemoval {
        kind: RemovalKind::Comments,
        selected: names.len(),
        rows,
        count,
        skipped,
        consequence: None,
        note: None,
        specs,
    }
}

/// Removing the acknowledgements of `objects` (in list order), in the
/// local time zone.
pub(crate) fn acknowledgements(
    snapshot: &Snapshot,
    objects: &[ObjectKey],
    now: Timestamp,
) -> BulkRemoval {
    acknowledgements_in(snapshot, objects, now, &Local)
}

/// [`acknowledgements`] in time zone `zone`.
#[expect(
    clippy::too_many_lines,
    reason = "the targets and the words of one dialog"
)]
pub(crate) fn acknowledgements_in<Tz>(
    snapshot: &Snapshot,
    objects: &[ObjectKey],
    now: Timestamp,
    zone: &Tz,
) -> BulkRemoval
where
    Tz: TimeZone,
    Tz::Offset: Display,
{
    let mut rows = Vec::new();
    let mut targets = Vec::new();
    let mut not_acknowledged = Vec::new();
    let mut seen = HashSet::new();
    let mut become_unhandled = 0;
    for object in objects {
        if !seen.insert(object.clone()) {
            continue;
        }
        let check = match object {
            ObjectKey::Host { name } => snapshot.hosts.get(name).map(|host| &host.check),
            ObjectKey::Service { key } => snapshot.services.get(key).map(|service| &service.check),
        };
        let Some(check) = check.filter(|check| check.acknowledgement.is_acknowledged()) else {
            not_acknowledged.push(describe_objects(std::slice::from_ref(object)));
            continue;
        };
        let who = ack_comment(snapshot, object).map_or_else(
            || "acknowledged".to_owned(),
            |comment| {
                format!(
                    "{} {}",
                    comment.author,
                    day_clock(comment.entry_time, now, zone)
                )
            },
        );
        let mut meta = vec![who];
        if check.acknowledgement == AckKind::Sticky {
            meta.push("sticky".to_owned());
        }
        meta.push(check.acknowledgement_expiry.map_or_else(
            || "no expiry".to_owned(),
            |at| format!("until {}", short_when(at, now, zone)),
        ));
        rows.push(TargetRow::Target {
            object: object.clone(),
            meta: meta.join(" · "),
        });
        let in_downtime = check.in_downtime();
        let behind_host = match object {
            ObjectKey::Service { key } => {
                snapshot.host_of(key).is_some_and(|host| host.is_problem())
            }
            ObjectKey::Host { .. } => false,
        };
        let problem = downtimes::state_of(snapshot, object).is_some_and(CheckableState::is_problem);
        if problem && !in_downtime && !behind_host {
            become_unhandled += 1;
        }
        targets.push(object.clone());
    }
    let count = targets.len();
    let consequence = (count > 0).then(|| match (count, become_unhandled) {
        (1, 1) => "It is still a problem: it becomes unhandled again, notifies by your rules, and \
                   counts in the dashboards again."
            .to_owned(),
        (count, unhandled) if unhandled == count => format!(
            "All {} are still problems: they become unhandled again, notify by your rules, and \
             count in the dashboards again.",
            number(count)
        ),
        (1, 0) => "It stays handled: a downtime or its host's problem still covers it.".to_owned(),
        (_, 0) => {
            "They stay handled: downtimes or their hosts' problems still cover them.".to_owned()
        }
        (count, unhandled) => format!(
            "{} of the {} become unhandled again, notify by your rules, and count in the \
             dashboards again; the others stay handled by a downtime or their host's problem.",
            capitalized(&number(unhandled)),
            number(count)
        ),
    });
    let note = (count > 0).then(|| {
        if count == 1 {
            "Its acknowledgement comment goes too, unless it was set as persistent.".to_owned()
        } else {
            "Their acknowledgement comments go too, unless they were set as persistent.".to_owned()
        }
    });
    let mut skipped = Vec::new();
    if !not_acknowledged.is_empty() {
        skipped.push(format!(
            "skipped: {}, not acknowledged any more.",
            listing(&not_acknowledged)
        ));
    }
    let specs = if targets.is_empty() {
        Vec::new()
    } else {
        vec![ActionSpec::for_objects(
            ObjectAction::RemoveAcknowledgement,
            Action::RemoveAcknowledgement,
            targets,
        )]
    };
    BulkRemoval {
        kind: RemovalKind::Acknowledgements,
        selected: objects.len(),
        rows,
        count,
        skipped,
        consequence,
        note,
        specs,
    }
}

/// `3 comments are gone already.`
fn gone_line(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("skipped: 1 {noun}, gone already.")
    } else {
        format!("skipped: {count} {noun}s, gone already.")
    }
}

/// `a, b and c`, at most [`NAMED`] names, then `and 4 more`.
fn listing(names: &[String]) -> String {
    if names.len() > NAMED {
        let shown = names[..NAMED - 1].join(", ");
        return format!("{shown} and {} more", names.len() - (NAMED - 1));
    }
    and_list(names)
}

/// `a`, `a and b`, `a, b and c`.
fn and_list(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// Each name once, in order.
fn dedup<'a>(names: &[&'a str]) -> Vec<&'a str> {
    let mut seen = HashSet::new();
    names
        .iter()
        .copied()
        .filter(|name| seen.insert(*name))
        .collect()
}

/// A count in words up to twelve: `three`.
fn number(count: usize) -> String {
    const WORDS: [&str; 13] = [
        "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
        "eleven", "twelve",
    ];
    WORDS
        .get(count)
        .map_or_else(|| count.to_string(), |word| (*word).to_owned())
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use chrono::Utc;
    use ic_model::{Host, HostState, Service, ServiceState};

    use super::*;

    fn now() -> Timestamp {
        Timestamp::from_unix_seconds(1_791_382_320.)
    }

    fn at(minutes: f64) -> Timestamp {
        Timestamp::from_unix_seconds(now().as_unix_seconds() + minutes * 60.)
    }

    fn downtime(object: &ObjectKey, name: &str, author: &str, start: f64, end: f64) -> Downtime {
        Downtime {
            name: name.to_owned(),
            object: object.clone(),
            author: author.to_owned(),
            comment: "c".to_owned(),
            start_time: at(start),
            end_time: at(end),
            fixed: true,
            duration: 0.0,
            entry_time: at(start - 30.),
            trigger_time: None,
            triggered_by: None,
            parent: None,
            in_effect: start <= 0. && end > 0.,
            config_owned: false,
            schedule: None,
        }
    }

    /// The mock-up's 7c: k8s-node-07 with its services, postgres-
    /// replication and redis-memory in downtime, a weekly one from the
    /// config.
    fn world() -> Snapshot {
        let mut hosts = BTreeMap::new();
        for name in ["k8s-node-07", "db-prod-03", "cache-02", "sw-core-ams-01"] {
            let mut host = Host::new(name);
            host.state = HostState::Up;
            hosts.insert(host.name.clone(), Arc::new(host));
        }
        let mut services = BTreeMap::new();
        for (host, name, state) in [
            ("k8s-node-07", "disk /", ServiceState::Ok),
            ("k8s-node-07", "disk /var", ServiceState::Critical),
            ("k8s-node-07", "kubelet", ServiceState::Critical),
            ("db-prod-03", "postgres-replication", ServiceState::Critical),
            ("cache-02", "redis-memory", ServiceState::Warning),
        ] {
            let mut service = Service::new(host, name);
            service.state = state;
            services.insert(service.key.clone(), Arc::new(service));
        }
        let node = ObjectKey::host("k8s-node-07");
        let mut list = vec![downtime(&node, "k8s-node-07!drain", "m.keller", -42., 78.)];
        for service in ["disk /", "disk /var", "kubelet"] {
            let mut child = downtime(
                &ObjectKey::service("k8s-node-07", service),
                &format!("k8s-node-07!{service}!drain"),
                "m.keller",
                -42.,
                78.,
            );
            child.parent = Some("k8s-node-07!drain".to_owned());
            list.push(child);
        }
        list.push(downtime(
            &ObjectKey::service("db-prod-03", "postgres-replication"),
            "db-prod-03!postgres-replication!drill",
            "j.berg",
            -72.,
            108.,
        ));
        list.push(downtime(
            &ObjectKey::service("cache-02", "redis-memory"),
            "cache-02!redis-memory!memory",
            "j.berg",
            -117.,
            123.,
        ));
        let mut weekly = downtime(
            &ObjectKey::host("sw-core-ams-01"),
            "sw-core-ams-01!weekly",
            "icingaadmin",
            3_828.,
            4_068.,
        );
        weekly.config_owned = true;
        weekly.schedule = Some("weekly-patching".to_owned());
        list.push(weekly);
        let mut downtimes: BTreeMap<ObjectKey, Vec<Downtime>> = BTreeMap::new();
        for downtime in list {
            downtimes
                .entry(downtime.object.clone())
                .or_default()
                .push(downtime);
        }
        Snapshot {
            hosts: Arc::new(hosts),
            services: Arc::new(services),
            downtimes: Arc::new(downtimes),
            ..Snapshot::default()
        }
    }

    #[test]
    fn downtimes_are_grouped_by_the_downtime_they_belong_to() {
        let snapshot = world();
        let names = vec![
            "k8s-node-07!drain".to_owned(),
            "k8s-node-07!kubelet!drain".to_owned(),
            "db-prod-03!postgres-replication!drill".to_owned(),
            "cache-02!redis-memory!memory".to_owned(),
            "sw-core-ams-01!weekly".to_owned(),
        ];
        let removal = downtimes_in(&snapshot, &names, now(), &Utc);
        assert_eq!(removal.title(), "Remove downtimes");
        assert_eq!(removal.count, 6, "the host's, its 3 services', 2 more");
        assert_eq!(removal.what(), "5 selected · 6 downtimes");
        assert_eq!(
            removal.label().as_deref(),
            Some("6 downtimes on 1 host and 5 services")
        );
        assert_eq!(removal.button(), "remove 6 downtimes");
        assert_eq!(
            removal.rows[0],
            TargetRow::Heading(
                "k8s-node-07 · host and 3 services · fixed 13:30 → 15:30 · m.keller".to_owned()
            )
        );
        assert_eq!(
            removal.rows[1],
            TargetRow::Target {
                object: ObjectKey::host("k8s-node-07"),
                meta: "host".to_owned()
            }
        );
        assert_eq!(
            removal.rows[5],
            TargetRow::Heading("fixed 13:00 → 16:00 · j.berg".to_owned())
        );
        assert_eq!(
            removal.consequence.as_deref(),
            Some(
                "Their problems notify again once the downtimes are gone: disk /var and kubelet \
                 on k8s-node-07, postgres-replication on db-prod-03, redis-memory on cache-02."
            )
        );
        assert_eq!(
            removal.note.as_deref(),
            Some(
                "The host downtime's 3 service downtimes go with it: Icinga removes the children \
                 of a downtime it removes."
            )
        );
        assert_eq!(removal.skipped.len(), 1);
        assert!(
            removal.skipped[0].contains("sw-core-ams-01, from the config (weekly-patching)"),
            "{}",
            removal.skipped[0]
        );
        // One request: the host's (its children go with it) and the others.
        assert_eq!(removal.specs.len(), 1);
        assert_eq!(
            removal.specs[0].target,
            ActionTarget::Downtimes(vec![
                "k8s-node-07!drain".to_owned(),
                "db-prod-03!postgres-replication!drill".to_owned(),
                "cache-02!redis-memory!memory".to_owned(),
            ])
        );
        assert_eq!(removal.objects().len(), 6);
    }

    #[test]
    fn only_config_downtimes_remove_nothing() {
        let removal = downtimes_in(
            &world(),
            &["sw-core-ams-01!weekly".to_owned(), "gone!x".to_owned()],
            now(),
            &Utc,
        );
        assert_eq!(removal.count, 0);
        assert!(removal.specs.is_empty());
        assert_eq!(removal.skipped.len(), 2);
        assert_eq!(removal.skipped[1], "skipped: 1 downtime, gone already.");
        assert_eq!(removal.consequence, None);
    }

    fn comment(object: &ObjectKey, name: &str, author: &str, kind: CommentKind) -> Comment {
        Comment {
            name: name.to_owned(),
            object: object.clone(),
            author: author.to_owned(),
            text: "t".to_owned(),
            kind,
            entry_time: at(-31.),
            expire_time: None,
            persistent: false,
        }
    }

    #[test]
    fn comments_skip_acknowledgements_and_downtimes() {
        let replication = ObjectKey::service("db-prod-03", "postgres-replication");
        let redis = ObjectKey::service("cache-02", "redis-memory");
        let mut snapshot = world();
        let mut map = BTreeMap::new();
        map.insert(
            replication.clone(),
            vec![
                comment(&replication, "c1", "j.berg", CommentKind::User),
                comment(&replication, "c2", "j.berg", CommentKind::Downtime),
            ],
        );
        map.insert(
            redis.clone(),
            vec![comment(
                &redis,
                "c3",
                "m.keller",
                CommentKind::Acknowledgement,
            )],
        );
        snapshot.comments = Arc::new(map);
        let names: Vec<String> = ["c1", "c2", "c3"].map(str::to_owned).to_vec();
        let removal = comments_in(&snapshot, &names, now(), &Utc);
        assert_eq!(removal.what(), "3 selected · 1 comment");
        assert_eq!(removal.button(), "remove comment");
        assert_eq!(
            removal.rows,
            [TargetRow::Target {
                object: replication,
                meta: "j.berg 13:41".to_owned()
            }]
        );
        assert_eq!(
            removal.skipped,
            [
                "skipped: redis-memory on cache-02, an acknowledgement. Remove the \
                 acknowledgement instead.",
                "skipped: postgres-replication on db-prod-03, a downtime's comment. Remove the \
                 downtime instead."
            ]
        );
        assert_eq!(
            removal.specs[0].target,
            ActionTarget::Comments(vec!["c1".to_owned()])
        );
    }

    #[test]
    fn acknowledgements_list_who_sticky_and_expiry() {
        let mut snapshot = world();
        let disk = ObjectKey::service("k8s-node-07", "disk /var");
        let redis = ObjectKey::service("cache-02", "redis-memory");
        let replication = ObjectKey::service("db-prod-03", "postgres-replication");
        let mut services = (*snapshot.services).clone();
        for (object, kind, expiry) in [
            (&redis, AckKind::Sticky, Some(at(60. * 18.))),
            (&replication, AckKind::Normal, None),
        ] {
            let service = Arc::make_mut(services.get_mut(object.as_service().unwrap()).unwrap());
            service.check.acknowledgement = kind;
            service.check.acknowledgement_expiry = expiry;
        }
        snapshot.services = Arc::new(services);
        let mut map = BTreeMap::new();
        map.insert(
            redis.clone(),
            vec![comment(
                &redis,
                "a",
                "dba-oncall",
                CommentKind::Acknowledgement,
            )],
        );
        snapshot.comments = Arc::new(map);
        // In the world, redis-memory and postgres-replication are in a
        // downtime in effect only by their downtimes; give them none here.
        snapshot.downtimes = Arc::new(BTreeMap::new());
        let removal = acknowledgements_in(
            &snapshot,
            &[redis.clone(), replication.clone(), disk.clone()],
            now(),
            &Utc,
        );
        assert_eq!(removal.title(), "Remove acknowledgements");
        assert_eq!(removal.what(), "3 selected");
        assert_eq!(removal.button(), "remove 2 acknowledgements");
        assert_eq!(
            removal.rows,
            [
                TargetRow::Target {
                    object: redis.clone(),
                    meta: "dba-oncall 13:41 · sticky · until Thu 08:12".to_owned()
                },
                TargetRow::Target {
                    object: replication.clone(),
                    meta: "acknowledged · no expiry".to_owned()
                },
            ]
        );
        assert_eq!(
            removal.consequence.as_deref(),
            Some(
                "All two are still problems: they become unhandled again, notify by your rules, \
                 and count in the dashboards again."
            )
        );
        assert_eq!(
            removal.skipped,
            ["skipped: disk /var on k8s-node-07, not acknowledged any more."]
        );
        assert_eq!(
            removal.specs[0].target,
            ActionTarget::Objects(vec![redis, replication])
        );
    }

    #[test]
    fn words_for_lists_and_numbers() {
        let names: Vec<String> = (1..=9).map(|i| format!("n{i}")).collect();
        assert_eq!(listing(&names[..3]), "n1, n2 and n3");
        assert_eq!(listing(&names), "n1, n2, n3, n4, n5 and 4 more");
        assert_eq!(number(3), "three");
        assert_eq!(number(30), "30");
        assert_eq!(capitalized("two"), "Two");
    }
}
