//! What the handling and downtimes views show (topic 14, `threads.js`),
//! built from the snapshot the engine keeps current: acknowledgements are
//! attributes of the hosts and services, downtimes and comments come with
//! the initial load and the event stream. Nothing here asks Icinga for
//! anything.
//!
//! **One grouping pattern everywhere**: each object is a group, a slim
//! band with its entries under it, oldest first like a chat. An entry is
//! an acknowledgement (its comment is its text), a downtime (in effect or
//! still to come; its comment is its text) or a free-standing comment;
//! Icinga's automatic comments are never entries of their own. A host's
//! downtime with all its services is one entry whose identical service
//! downtimes fold under it (`18 services, same downtime`, closed by
//! default, paged by count when open); a service with an entry of its own
//! is its own group instead, so the same object never shows twice.
//!
//! [`build`] turns the snapshot into a [`Listing`]: the lines in order
//! (section labels, the timeline's axis, bands, entries, folds and their
//! services, paging rows) and the chips' counts. Lines refer to the
//! snapshot's downtimes and comments by position, so a build is a pass
//! and a sort over them; the words of a line are made only for the lines
//! on screen. Pure, so it's tested without a window.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use ic_core::snapshot::Snapshot;
use ic_model::{AckKind, CheckInfo, Downtime, DowntimePhase, ObjectKey, Timestamp};

use ic_config::DowntimeKinds;

use super::model::{Chip, ListKind, Mode, Options, SortChoice, ack_comment, is_automatic};
use crate::dashboard::selection::SelectableRow;
use crate::paging;

/// How many entries of a thread, or services of a fold, show before
/// `+ N more` (the paging rule of every host-with-services view).
pub(crate) const PREVIEW: usize = paging::HOST_SERVICES_PREVIEW;

/// An acknowledgement expiring within this many seconds is in the
/// *expires within 2 hours* section, its expiry in the warning colour.
pub(crate) const SOON: f64 = 2. * 3_600.;

/// What a view covers (topic 14, round 5): the whole environment (the
/// cluster section's entries), or the hosts and services a dashboard
/// view's filter matches, as the core evaluated them.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Scope<'a> {
    /// Every object.
    All,
    /// These objects.
    Members(&'a BTreeSet<ObjectKey>),
}

impl Scope<'_> {
    /// Whether the view covers `object`.
    pub(crate) fn contains(self, object: &ObjectKey) -> bool {
        match self {
            Self::All => true,
            Self::Members(members) => members.contains(object),
        }
    }
}

/// What an entry is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum EntryKind {
    /// The object's acknowledgement.
    Ack,
    /// A downtime in effect.
    InEffect,
    /// A downtime not in effect yet (scheduled, or flexible and waiting).
    Upcoming,
    /// A free-standing comment.
    Comment,
}

/// What identifies an entry: the acknowledged object, or the downtime or
/// comment by name.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum EntryKey {
    /// An object's acknowledgement.
    Ack(ObjectKey),
    /// A downtime, by its full name.
    Downtime(String),
    /// A comment, by its full name.
    Comment(String),
}

/// Where an entry's record is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Record {
    /// The object's acknowledgement (its comment: [`ack_comment`]).
    Ack,
    /// Position in the snapshot's list of the object's downtimes.
    Downtime(usize),
    /// Position in the snapshot's list of the object's comments.
    Comment(usize),
}

/// An entry of a thread.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Entry {
    /// Its identity (selection, removal).
    pub(crate) key: EntryKey,
    /// The host or service it is about.
    pub(crate) object: ObjectKey,
    /// Where its record is.
    pub(crate) record: Record,
    /// What it is.
    pub(crate) kind: EntryKind,
    /// A downtime from the config (a lock, never removable).
    pub(crate) config: bool,
    /// A host's downtime: how many services it covers (its children with
    /// `all_services`); `None` for a host's downtime that leaves its
    /// services alone while the host has some (`host only`).
    pub(crate) services: Option<usize>,
    /// A service's own downtime while its host's downtime covers it too
    /// (`its own`).
    pub(crate) own: bool,
    /// The services folded under it, worst first, then by name (the ones
    /// without an entry of their own).
    pub(crate) folded: Arc<Vec<ObjectKey>>,
}

/// A light section label.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Section {
    /// Acknowledgements expiring within 2 hours.
    ExpiresSoon,
    /// Acknowledgements expiring later.
    ExpiresLater,
    /// Acknowledgements without an expiry.
    NoExpiry,
    /// Objects with a downtime in effect.
    InEffect,
    /// Objects whose downtimes are all still to come.
    Upcoming,
}

impl Section {
    /// Its title: `in effect · 5`.
    pub(crate) fn title(self, count: usize) -> String {
        let what = match self {
            Self::ExpiresSoon => "expires within 2 hours",
            Self::ExpiresLater => "expires later",
            Self::NoExpiry => "no expiry",
            Self::InEffect => "in effect",
            Self::Upcoming => "upcoming",
        };
        format!("{what} · {count}")
    }

    /// The faint words after it: what it means, or its order.
    pub(crate) fn detail(self, sort: SortChoice) -> &'static str {
        match (self, sort) {
            (Self::ExpiresSoon, _) => "then it is a problem again and notifies",
            (Self::ExpiresLater, _) => "soonest first",
            (Self::NoExpiry, _) => "until the problem recovers",
            (Self::InEffect, SortChoice::Soonest) => "ending soonest first",
            (Self::Upcoming, SortChoice::Soonest) | (_, SortChoice::ByTime) => {
                "starting soonest first"
            }
            (_, SortChoice::Object) => "by name",
            (_, SortChoice::Author) => "by author",
            (_, SortChoice::LatestActivity) => "latest activity first",
        }
    }

    /// Its icon.
    pub(crate) fn icon(self) -> ic_ui_kit::IconName {
        use ic_ui_kit::IconName;
        match self {
            Self::ExpiresSoon | Self::Upcoming => IconName::Clock,
            Self::ExpiresLater | Self::InEffect => IconName::CalendarClock,
            Self::NoExpiry => IconName::Check,
        }
    }
}

/// What a host's band says its downtime covers: `host and 18 services in
/// downtime`, `host and 6 services, Thu 01:00`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Covers {
    /// How many services.
    pub(crate) services: usize,
    /// In effect now.
    pub(crate) in_effect: bool,
    /// When it starts (or started).
    pub(crate) start: Timestamp,
}

/// What a paging row pages.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum MoreKey {
    /// The entries of an object's thread.
    Thread(ObjectKey),
    /// The services folded under a host's downtime, by its name.
    Fold(String),
}

/// What identifies a line for the cursor.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum ItemKey {
    /// An object's band.
    Band(ObjectKey),
    /// An entry.
    Entry(EntryKey),
    /// The fold under a host's downtime, by its name.
    Fold(String),
    /// A service in an open fold.
    Service(String, ObjectKey),
    /// A paging row.
    More(MoreKey),
}

/// One line of a view.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Line {
    /// A light section label.
    Section {
        /// Which.
        section: Section,
        /// How many objects it holds.
        count: usize,
    },
    /// The timeline's axis.
    Axis,
    /// An object's band.
    Band {
        /// The object.
        object: ObjectKey,
        /// What its thread holds (`in downtime · 3 comments`, `3
        /// downtimes`), whatever the chip.
        slot: String,
        /// Folded to its band.
        collapsed: bool,
        /// A host's downtime with its services, if one is in the thread.
        covers: Option<Covers>,
    },
    /// An entry.
    Entry {
        /// It.
        entry: Arc<Entry>,
        /// A later entry of its thread (the reply rule).
        reply: bool,
        /// The object's only entry, band and entry in one row (the
        /// downtimes view).
        single: bool,
    },
    /// The services a host's downtime covers, folded under it.
    Fold {
        /// The host's downtime.
        downtime: String,
        /// Where it is: its host and its position in the host's list.
        parent: (ObjectKey, usize),
        /// How many services are folded.
        count: usize,
        /// Open: the services show under it.
        open: bool,
    },
    /// A service of an open fold.
    Service {
        /// The host's downtime it belongs to.
        downtime: String,
        /// Where that downtime is.
        parent: (ObjectKey, usize),
        /// The service.
        object: ObjectKey,
    },
    /// `+ N more` (or `− show fewer` once all show).
    More {
        /// What it pages.
        key: MoreKey,
        /// How many wait behind it (0: all show).
        hidden: usize,
    },
    /// A comment sent from the view, until the event stream shows it, or
    /// refused (topic 17): its draft, by id.
    Draft {
        /// The object it is on.
        object: ObjectKey,
        /// The draft's id ([`crate::comments::drafts::Draft`]).
        id: u64,
        /// Refused: its reason shows under it.
        refused: bool,
    },
    /// The comment field, open as the thread's next entry (topic 17).
    Composer {
        /// The object the comment is for.
        object: ObjectKey,
    },
}

impl Line {
    /// The host or service the line is about.
    pub(crate) fn object(&self) -> Option<&ObjectKey> {
        match self {
            Self::Band { object, .. }
            | Self::Service { object, .. }
            | Self::More {
                key: MoreKey::Thread(object),
                ..
            }
            | Self::Draft { object, .. }
            | Self::Composer { object } => Some(object),
            Self::Entry { entry, .. } => Some(&entry.object),
            Self::Fold { parent, .. } => Some(&parent.0),
            Self::Section { .. } | Self::Axis | Self::More { .. } => None,
        }
    }

    /// The entry on this line, if it is one.
    pub(crate) fn entry(&self) -> Option<&Entry> {
        match self {
            Self::Entry { entry, .. } => Some(entry),
            _ => None,
        }
    }
}

/// A line and its key, as the selection walks them.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Keyed {
    /// The line.
    pub(crate) line: Line,
    /// Its key; `None` for sections and the axis.
    pub(crate) key: Option<ItemKey>,
}

impl SelectableRow for Keyed {
    type Key = ItemKey;

    fn key(&self) -> Option<&ItemKey> {
        self.key.as_ref()
    }

    fn markable(&self) -> bool {
        matches!(self.key, Some(ItemKey::Entry(_)))
    }
}

/// The chips' counts and what the header and summary say.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Summary {
    /// Each chip's count (*all* has none): acknowledgements, downtimes in
    /// effect, upcoming, comments, from the config.
    pub(crate) chips: Vec<(Chip, usize)>,
    /// The objects shown.
    pub(crate) objects: usize,
    /// The downtimes shown (a host's with its services counts once).
    pub(crate) downtimes: usize,
    /// Of the acknowledgements shown, the sticky ones.
    pub(crate) sticky: usize,
    /// Services folded into their host's downtime.
    pub(crate) folded: usize,
    /// Left out by *only mine*.
    pub(crate) by_others: usize,
}

impl Summary {
    /// The count of `chip`.
    pub(crate) fn count(&self, chip: Chip) -> Option<usize> {
        self.chips
            .iter()
            .find(|(candidate, _)| *candidate == chip)
            .map(|(_, count)| *count)
    }
}

/// What the user folded and paged in a view (kept while it is open).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Folds {
    /// Objects folded to their band.
    pub(crate) collapsed: HashSet<ObjectKey>,
    /// Host downtimes whose services show (closed by default).
    pub(crate) open: HashSet<String>,
    /// Open folds showing all their services.
    pub(crate) all_services: HashSet<String>,
    /// Threads showing all their entries.
    pub(crate) all_entries: HashSet<ObjectKey>,
}

/// A view's lines and counts.
#[derive(Clone, Debug)]
pub(crate) struct Listing {
    /// The lines in order, with their keys.
    pub(crate) lines: Arc<Vec<Keyed>>,
    /// The counts.
    pub(crate) summary: Summary,
    /// The downtimes the lines refer to.
    pub(crate) downtimes: Arc<BTreeMap<ObjectKey, Vec<Downtime>>>,
}

impl Listing {
    /// The downtime at `parent` (a fold's).
    pub(crate) fn downtime_at(&self, parent: &(ObjectKey, usize)) -> Option<&Downtime> {
        self.downtimes.get(&parent.0)?.get(parent.1)
    }

    /// The line `index`.
    pub(crate) fn line(&self, index: usize) -> Option<&Line> {
        self.lines.get(index).map(|keyed| &keyed.line)
    }

    /// Whether line `index` is its thread's last entry (in a list: where
    /// *+ comment* shows, topic 17). Folds and paging rows after it don't
    /// count; a single row (the downtimes list) is no thread.
    pub(crate) fn is_last_entry(&self, index: usize) -> bool {
        let Some(Line::Entry { single: false, .. }) = self.line(index) else {
            return false;
        };
        // The thread runs to the next band or section.
        self.lines[index + 1..]
            .iter()
            .map(|keyed| &keyed.line)
            .take_while(|line| {
                !matches!(line, Line::Band { .. } | Line::Section { .. } | Line::Axis)
            })
            .all(|line| !matches!(line, Line::Entry { .. }))
    }
}

/// What topic 17 adds to a handling view's threads: the comments sent from
/// it that the snapshot doesn't show yet (or that were refused), and the
/// open comment field.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CommentLines {
    /// The drafts: object, id, refused; oldest first.
    pub(crate) drafts: Vec<(ObjectKey, u64, bool)>,
    /// The object whose thread has the field open.
    pub(crate) composer: Option<ObjectKey>,
}

impl CommentLines {
    /// Nothing to add.
    pub(crate) fn is_empty(&self) -> bool {
        self.drafts.is_empty() && self.composer.is_none()
    }
}

/// Puts `comments` into `listing`'s threads: after each thread's last
/// entry and what belongs to it (an open fold), before its paging row, its
/// drafts in the order they were sent, then the open field. A thread
/// folded to its band, or not in the listing, gets nothing. Keyless lines:
/// the cursor never stops on them.
pub(crate) fn place_comments(listing: &mut Listing, comments: &CommentLines) {
    if comments.is_empty() {
        return;
    }
    let lines = &listing.lines;
    // Where each object's thread takes its additions: after its last line
    // but a paging row.
    let mut after: HashMap<&ObjectKey, usize> = HashMap::new();
    let mut current: Option<&ObjectKey> = None;
    for (index, keyed) in lines.iter().enumerate() {
        match &keyed.line {
            Line::Band {
                object, collapsed, ..
            } => current = (!*collapsed).then_some(object),
            Line::Entry {
                entry,
                single: false,
                ..
            } if current == Some(&entry.object) => {
                after.insert(&entry.object, index);
            }
            Line::Fold { parent, .. } | Line::Service { parent, .. }
                if current == Some(&parent.0) =>
            {
                after.insert(&parent.0, index);
            }
            Line::Section { .. } | Line::Axis => current = None,
            _ => {}
        }
    }
    let mut extra: BTreeMap<usize, Vec<Keyed>> = BTreeMap::new();
    for (object, id, refused) in &comments.drafts {
        if let Some(index) = after.get(object) {
            extra.entry(*index).or_default().push(Keyed {
                line: Line::Draft {
                    object: object.clone(),
                    id: *id,
                    refused: *refused,
                },
                key: None,
            });
        }
    }
    if let Some(object) = &comments.composer
        && let Some(index) = after.get(object)
    {
        extra.entry(*index).or_default().push(Keyed {
            line: Line::Composer {
                object: object.clone(),
            },
            key: None,
        });
    }
    if extra.is_empty() {
        return;
    }
    let count = extra.values().map(Vec::len).sum::<usize>();
    let mut placed = Vec::with_capacity(lines.len() + count);
    for (index, keyed) in lines.iter().enumerate() {
        placed.push(keyed.clone());
        if let Some(added) = extra.remove(&index) {
            placed.extend(added);
        }
    }
    listing.lines = Arc::new(placed);
}

/// A downtime still running or to come (not over).
fn live(downtime: &Downtime, now: Timestamp) -> bool {
    downtime.phase(now) != DowntimePhase::Over
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

/// Which service downtimes fold under a host's downtime: the children a
/// host's downtime has with `all_services` (by `parent`, or on an Icinga
/// that doesn't link them, the identical ones scheduled with it). Only
/// downtimes still running or to come count.
#[derive(Debug, Default)]
struct Tree<'a> {
    /// A folded service downtime → its host's downtime.
    parent_of: HashMap<&'a str, &'a Downtime>,
    /// A host's downtime → its service downtimes.
    children: HashMap<&'a str, Vec<&'a Downtime>>,
}

impl<'a> Tree<'a> {
    fn of(snapshot: &'a Snapshot, now: Timestamp) -> Self {
        let mut by_name: HashMap<&str, &Downtime> = HashMap::new();
        let mut of_host: HashMap<&ic_model::HostName, Vec<&Downtime>> = HashMap::new();
        for downtime in snapshot.downtimes.values().flatten() {
            if !live(downtime, now) {
                continue;
            }
            by_name.insert(downtime.name.as_str(), downtime);
            if let ObjectKey::Host { name } = &downtime.object {
                of_host.entry(name).or_default().push(downtime);
            }
        }
        let mut tree = Tree::default();
        for downtime in snapshot.downtimes.values().flatten() {
            let ObjectKey::Service { key } = &downtime.object else {
                continue;
            };
            if !live(downtime, now) {
                continue;
            }
            let parent = match downtime.parent.as_deref() {
                Some(parent) => by_name.get(parent).copied().filter(|parent| {
                    matches!(&parent.object, ObjectKey::Host { name } if *name == key.host)
                }),
                None => of_host.get(&key.host).and_then(|candidates| {
                    candidates
                        .iter()
                        .copied()
                        .find(|candidate| twins(candidate, downtime))
                }),
            };
            if let Some(parent) = parent {
                tree.parent_of.insert(downtime.name.as_str(), parent);
                tree.children
                    .entry(parent.name.as_str())
                    .or_default()
                    .push(downtime);
            }
        }
        tree
    }

    /// Whether `downtime` folds under its host's (which the view shows:
    /// a service in scope whose host isn't keeps its downtime as an entry
    /// of its own).
    fn folds(&self, downtime: &Downtime, scope: Scope<'_>) -> bool {
        self.parent_of
            .get(downtime.name.as_str())
            .is_some_and(|parent| scope.contains(&parent.object))
    }
}

/// An entry before it is placed, with what sorting and filtering need.
#[derive(Clone, Debug)]
struct Candidate<'a> {
    key: EntryKey,
    object: ObjectKey,
    record: Record,
    kind: EntryKind,
    config: bool,
    author: &'a str,
    /// When it was set (seconds).
    at: f64,
    /// When it expires or ends (in effect), or starts (upcoming).
    due: Option<f64>,
    /// When it starts (or started), and ends.
    start: f64,
    end: f64,
    /// A sticky acknowledgement.
    sticky: bool,
}

impl Candidate<'_> {
    fn is_downtime(&self) -> bool {
        matches!(self.kind, EntryKind::InEffect | EntryKind::Upcoming)
    }
}

/// Every entry of `kind`'s view of the objects in `scope`, Icinga's
/// automatic comments, folded service downtimes and the downtimes `shows`
/// leaves out (the downtimes view's *shows*) left out.
fn candidates<'a>(
    kind: ListKind,
    snapshot: &'a Snapshot,
    tree: &Tree<'_>,
    scope: Scope<'_>,
    shows: DowntimeKinds,
    now: Timestamp,
) -> Vec<Candidate<'a>> {
    let mut found = Vec::new();
    if kind == ListKind::Handling {
        for (object, check) in acknowledged(snapshot) {
            if !scope.contains(&object) {
                continue;
            }
            let comment = ack_comment(snapshot, &object);
            found.push(Candidate {
                key: EntryKey::Ack(object.clone()),
                object,
                record: Record::Ack,
                kind: EntryKind::Ack,
                config: false,
                author: comment.map_or("", |comment| comment.author.as_str()),
                at: comment.map_or(f64::NEG_INFINITY, |comment| {
                    comment.entry_time.as_unix_seconds()
                }),
                due: check.acknowledgement_expiry.map(Timestamp::as_unix_seconds),
                start: f64::NEG_INFINITY,
                end: f64::INFINITY,
                sticky: check.acknowledgement == AckKind::Sticky,
            });
        }
        for (object, list) in snapshot.comments.iter() {
            if !scope.contains(object) {
                continue;
            }
            for (index, comment) in list.iter().enumerate() {
                if is_automatic(comment.kind) {
                    continue;
                }
                found.push(Candidate {
                    key: EntryKey::Comment(comment.name.clone()),
                    object: object.clone(),
                    record: Record::Comment(index),
                    kind: EntryKind::Comment,
                    config: false,
                    author: comment.author.as_str(),
                    at: comment.entry_time.as_unix_seconds(),
                    due: comment.expire_time.map(Timestamp::as_unix_seconds),
                    start: f64::NEG_INFINITY,
                    end: f64::INFINITY,
                    sticky: false,
                });
            }
        }
    }
    for (object, list) in snapshot.downtimes.iter() {
        if !scope.contains(object) {
            continue;
        }
        for (index, downtime) in list.iter().enumerate() {
            if !live(downtime, now) || tree.folds(downtime, scope) {
                continue;
            }
            let in_effect = downtime.phase(now) == DowntimePhase::InEffect;
            // The downtimes view's *shows*: a kind left out isn't there,
            // its chip counts none.
            if kind == ListKind::Downtimes
                && !(if downtime.config_owned {
                    shows.from_config
                } else if in_effect {
                    shows.in_effect
                } else {
                    shows.upcoming
                })
            {
                continue;
            }
            let start = downtime
                .effective_start()
                .unwrap_or(downtime.start_time)
                .as_unix_seconds();
            let end = downtime
                .effective_end()
                .unwrap_or(downtime.end_time)
                .as_unix_seconds();
            found.push(Candidate {
                key: EntryKey::Downtime(downtime.name.clone()),
                object: object.clone(),
                record: Record::Downtime(index),
                kind: if in_effect {
                    EntryKind::InEffect
                } else {
                    EntryKind::Upcoming
                },
                config: downtime.config_owned,
                author: downtime.author.as_str(),
                at: downtime.entry_time.as_unix_seconds(),
                due: Some(if in_effect {
                    end
                } else {
                    downtime.start_time.as_unix_seconds()
                }),
                start,
                end,
                sticky: false,
            });
        }
    }
    found
}

/// The acknowledged hosts and services.
fn acknowledged(snapshot: &Snapshot) -> Vec<(ObjectKey, &CheckInfo)> {
    let mut found = Vec::new();
    for host in snapshot.hosts.values() {
        if host.check.acknowledgement.is_acknowledged() {
            found.push((host.key(), &host.check));
        }
    }
    for service in snapshot.services.values() {
        if service.check.acknowledgement.is_acknowledged() {
            found.push((service.object_key(), &service.check));
        }
    }
    found
}

/// Whether `candidate` is in `chip`, in `kind`'s view.
fn in_chip(kind: ListKind, chip: Chip, candidate: &Candidate<'_>) -> bool {
    match chip {
        Chip::All => true,
        Chip::Acknowledged => candidate.kind == EntryKind::Ack,
        Chip::InEffect => candidate.kind == EntryKind::InEffect,
        Chip::Upcoming => {
            candidate.kind == EntryKind::Upcoming
                && (kind == ListKind::Handling || !candidate.config)
        }
        Chip::Comments => candidate.kind == EntryKind::Comment,
        Chip::FromConfig => candidate.is_downtime() && candidate.config,
    }
}

/// Whether `author` is the environment's author (*only mine*).
fn is_mine(who: &str, author: &str) -> bool {
    !author.is_empty() && who.trim() == author
}

/// Hosts before services; then by host, then by service name.
pub(crate) fn object_order(a: &ObjectKey, b: &ObjectKey) -> Ordering {
    a.host_name().cmp(b.host_name()).then_with(|| match (a, b) {
        (ObjectKey::Host { .. }, ObjectKey::Host { .. }) => Ordering::Equal,
        (ObjectKey::Host { .. }, ObjectKey::Service { .. }) => Ordering::Less,
        (ObjectKey::Service { .. }, ObjectKey::Host { .. }) => Ordering::Greater,
        (ObjectKey::Service { key: a }, ObjectKey::Service { key: b }) => a.name.cmp(&b.name),
    })
}

/// What a thread holds, for its band: `in downtime · 3 comments`.
#[derive(Clone, Copy, Debug, Default)]
struct Holds {
    ack: bool,
    in_effect: usize,
    upcoming: usize,
    comments: usize,
}

impl Holds {
    fn add(&mut self, candidate: &Candidate<'_>) {
        match candidate.kind {
            EntryKind::Ack => self.ack = true,
            EntryKind::InEffect => self.in_effect += 1,
            EntryKind::Upcoming => self.upcoming += 1,
            EntryKind::Comment => self.comments += 1,
        }
    }

    /// `acknowledged · 1 comment`, `in downtime · 2 upcoming`, `upcoming`.
    fn slot(self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if self.ack {
            parts.push("acknowledged".to_owned());
        }
        if self.in_effect > 0 {
            parts.push("in downtime".to_owned());
        }
        match self.upcoming {
            0 => {}
            1 if parts.is_empty() => parts.push("upcoming".to_owned()),
            count => parts.push(format!("{count} upcoming")),
        }
        match self.comments {
            0 => {}
            1 => parts.push("1 comment".to_owned()),
            count => parts.push(format!("{count} comments")),
        }
        parts.join(" · ")
    }
}

/// A thread: an object and its entries, as shown.
struct Thread<'a> {
    object: ObjectKey,
    entries: Vec<Candidate<'a>>,
}

impl Thread<'_> {
    /// When someone last did something on it. A config downtime is no
    /// one's doing (Icinga makes it again at every reload): it counts only
    /// when the thread holds nothing else, and then as old as can be.
    fn latest(&self) -> f64 {
        self.entries
            .iter()
            .filter(|entry| !entry.config)
            .map(|entry| entry.at)
            .fold(f64::NEG_INFINITY, f64::max)
    }

    fn soonest(&self) -> f64 {
        self.entries
            .iter()
            .filter_map(|entry| entry.due)
            .fold(f64::INFINITY, f64::min)
    }

    fn first_start(&self) -> f64 {
        self.entries
            .iter()
            .filter(|entry| entry.is_downtime())
            .map(|entry| entry.start)
            .fold(f64::INFINITY, f64::min)
    }

    fn in_effect(&self) -> bool {
        self.entries
            .iter()
            .any(|entry| entry.kind == EntryKind::InEffect)
    }

    /// When its downtimes in effect end soonest, else when the others
    /// start soonest (the downtime list's order).
    fn ends_then_starts(&self) -> f64 {
        if self.in_effect() {
            self.entries
                .iter()
                .filter(|entry| entry.kind == EntryKind::InEffect)
                .map(|entry| entry.end)
                .fold(f64::INFINITY, f64::min)
        } else {
            self.entries
                .iter()
                .filter_map(|entry| entry.is_downtime().then_some(entry.start))
                .fold(f64::INFINITY, f64::min)
        }
    }

    /// The author of its latest entry.
    fn author(&self) -> &str {
        self.entries
            .iter()
            .max_by(|a, b| a.at.total_cmp(&b.at))
            .map_or("", |entry| entry.author)
    }

    /// Its acknowledgement's expiry (the acknowledged chip's sections).
    fn ack_expiry(&self) -> Option<f64> {
        self.entries
            .iter()
            .find(|entry| entry.kind == EntryKind::Ack)
            .and_then(|entry| entry.due)
    }
}

/// The objects being handled: those with an acknowledgement, a downtime
/// still running or to come of their own, or a free-standing comment (the
/// handling view's count in the sidebar and the palette).
pub(crate) fn handled_objects(snapshot: &Snapshot, scope: Scope<'_>, now: Timestamp) -> usize {
    let tree = Tree::of(snapshot, now);
    let objects: HashSet<ObjectKey> = candidates(
        ListKind::Handling,
        snapshot,
        &tree,
        scope,
        DowntimeKinds::default(),
        now,
    )
    .into_iter()
    .map(|candidate| candidate.object)
    .collect();
    objects.len()
}

/// The downtimes in effect now of the objects in `scope` (a host's with
/// its services counts once) that `shows` lets through: the downtimes
/// view's count.
pub(crate) fn downtimes_in_effect(
    snapshot: &Snapshot,
    scope: Scope<'_>,
    shows: DowntimeKinds,
    now: Timestamp,
) -> usize {
    let tree = Tree::of(snapshot, now);
    snapshot
        .downtimes
        .iter()
        .filter(|(object, _)| scope.contains(object))
        .flat_map(|(_, list)| list)
        .filter(|downtime| downtime.phase(now) == DowntimePhase::InEffect)
        .filter(|downtime| {
            if downtime.config_owned {
                shows.from_config
            } else {
                shows.in_effect
            }
        })
        .filter(|downtime| !tree.folds(downtime, scope))
        .count()
}

/// The view `kind` of `snapshot`, as `options` ask, folded and paged as
/// `folds` say; `author` is the environment's author (*only mine*).
#[expect(
    clippy::too_many_lines,
    reason = "filtering, counting, grouping and ordering of one view, in the order they apply"
)]
pub(crate) fn build(
    kind: ListKind,
    snapshot: &Snapshot,
    scope: Scope<'_>,
    author: &str,
    options: &Options,
    folds: &Folds,
    now: Timestamp,
) -> Listing {
    let tree = Tree::of(snapshot, now);
    let all = candidates(kind, snapshot, &tree, scope, options.shows, now);
    let mut summary = Summary::default();

    // What each thread holds whatever the chip (the band's slot).
    let mut holds: HashMap<ObjectKey, Holds> = HashMap::new();
    for candidate in &all {
        holds
            .entry(candidate.object.clone())
            .or_default()
            .add(candidate);
    }

    // *only mine*, then the chips' counts, then the chip.
    let mut mine = Vec::with_capacity(all.len());
    for candidate in all {
        if options.only_mine && !is_mine(candidate.author, author) {
            summary.by_others += 1;
        } else {
            mine.push(candidate);
        }
    }
    summary.chips = kind
        .chips()
        .iter()
        .filter(|chip| **chip != Chip::All)
        .map(|chip| {
            (
                *chip,
                mine.iter()
                    .filter(|candidate| in_chip(kind, *chip, candidate))
                    .count(),
            )
        })
        .collect();
    let mut by_object: BTreeMap<ObjectKey, Vec<Candidate<'_>>> = BTreeMap::new();
    for candidate in mine {
        if in_chip(kind, options.chip, &candidate) {
            by_object
                .entry(candidate.object.clone())
                .or_default()
                .push(candidate);
        }
    }
    let shown: HashSet<ObjectKey> = by_object.keys().cloned().collect();
    let mut threads: Vec<Thread<'_>> = by_object
        .into_iter()
        .map(|(object, entries)| Thread { object, entries })
        .collect();
    summary.objects = threads.len();
    for thread in &threads {
        for entry in &thread.entries {
            if entry.is_downtime() {
                summary.downtimes += 1;
            }
            if entry.sticky {
                summary.sticky += 1;
            }
        }
    }

    // Order: threads, then the entries inside each.
    let sort = options.sort(kind);
    let mode = match kind {
        ListKind::Handling => Mode::List,
        ListKind::Downtimes => options.mode,
    };
    let by = |a: f64, b: f64| a.total_cmp(&b);
    threads.sort_by(|a, b| {
        let order = match sort {
            SortChoice::LatestActivity => by(b.latest(), a.latest()),
            SortChoice::Soonest => match kind {
                ListKind::Handling => by(a.soonest(), b.soonest()),
                ListKind::Downtimes => b
                    .in_effect()
                    .cmp(&a.in_effect())
                    .then_with(|| by(a.ends_then_starts(), b.ends_then_starts())),
            },
            SortChoice::ByTime => by(a.first_start(), b.first_start()),
            SortChoice::Object => Ordering::Equal,
            SortChoice::Author => a
                .author()
                .cmp(b.author())
                .then_with(|| by(b.latest(), a.latest())),
        };
        order.then_with(|| object_order(&a.object, &b.object))
    });
    for thread in &mut threads {
        thread.entries.sort_by(|a, b| {
            let order = if mode == Mode::Timeline {
                by(a.start, b.start)
            } else {
                // Oldest first, like a chat; a config downtime is a
                // standing schedule, after the conversation.
                a.config.cmp(&b.config).then_with(|| by(a.at, b.at))
            };
            order.then_with(|| a.key.cmp(&b.key))
        });
    }

    // Sections: the acknowledged chip by expiry; the downtimes list by
    // the most current downtime of each object.
    let section_of = |thread: &Thread<'_>| -> Option<Section> {
        match (kind, mode) {
            (ListKind::Handling, _)
                if options.chip == Chip::Acknowledged && sort == SortChoice::Soonest =>
            {
                Some(match thread.ack_expiry() {
                    Some(at) if at - now.as_unix_seconds() <= SOON => Section::ExpiresSoon,
                    Some(_) => Section::ExpiresLater,
                    None => Section::NoExpiry,
                })
            }
            (ListKind::Downtimes, Mode::List) => Some(if thread.in_effect() {
                Section::InEffect
            } else {
                Section::Upcoming
            }),
            _ => None,
        }
    };
    let section_rank = |section: Option<Section>| match section {
        Some(Section::ExpiresSoon | Section::InEffect) | None => 0,
        Some(Section::ExpiresLater | Section::Upcoming) => 1,
        Some(Section::NoExpiry) => 2,
    };
    threads.sort_by_key(|thread| section_rank(section_of(thread)));

    // Lines.
    let mut lines: Vec<Keyed> = Vec::new();
    let push = |lines: &mut Vec<Keyed>, line: Line, key: Option<ItemKey>| {
        lines.push(Keyed { line, key });
    };
    if kind == ListKind::Downtimes && mode == Mode::Timeline && !threads.is_empty() {
        push(&mut lines, Line::Axis, None);
    }
    let mut section_counts: HashMap<Section, usize> = HashMap::new();
    for thread in &threads {
        if let Some(section) = section_of(thread) {
            *section_counts.entry(section).or_default() += 1;
        }
    }
    let mut last_section: Option<Section> = None;
    for thread in &threads {
        let section = section_of(thread);
        if let Some(section) = section
            && last_section != Some(section)
        {
            push(
                &mut lines,
                Line::Section {
                    section,
                    count: section_counts.get(&section).copied().unwrap_or(0),
                },
                None,
            );
            last_section = Some(section);
        }
        let object = thread.object.clone();
        let entries: Vec<Arc<Entry>> = thread
            .entries
            .iter()
            .map(|candidate| {
                let entry = place(candidate, snapshot, &tree, &shown, scope);
                summary.folded += entry.folded.len();
                Arc::new(entry)
            })
            .collect();
        let has_fold = entries.iter().any(|entry| !entry.folded.is_empty());
        let single = kind == ListKind::Downtimes && entries.len() == 1 && !has_fold;
        if single {
            let entry = Arc::clone(&entries[0]);
            let key = ItemKey::Entry(entry.key.clone());
            push(
                &mut lines,
                Line::Entry {
                    entry,
                    reply: false,
                    single: true,
                },
                Some(key),
            );
            continue;
        }
        let collapsed = folds.collapsed.contains(&object);
        let slot = match kind {
            ListKind::Handling => holds
                .get(&thread.object)
                .copied()
                .unwrap_or_default()
                .slot(),
            ListKind::Downtimes => match entries.len() {
                1 => "1 downtime".to_owned(),
                count => format!("{count} downtimes"),
            },
        };
        let covers = covers(&entries, &thread.entries);
        push(
            &mut lines,
            Line::Band {
                object: object.clone(),
                slot,
                collapsed,
                covers,
            },
            Some(ItemKey::Band(object.clone())),
        );
        if collapsed {
            continue;
        }
        let all_entries = folds.all_entries.contains(&object);
        let shown_count = if all_entries {
            entries.len()
        } else {
            entries.len().min(PREVIEW)
        };
        for (index, entry) in entries.iter().take(shown_count).enumerate() {
            push(
                &mut lines,
                Line::Entry {
                    entry: Arc::clone(entry),
                    reply: index > 0,
                    single: false,
                },
                Some(ItemKey::Entry(entry.key.clone())),
            );
            if entry.folded.is_empty() {
                continue;
            }
            let EntryKey::Downtime(name) = &entry.key else {
                continue;
            };
            let Record::Downtime(position) = entry.record else {
                continue;
            };
            let parent = (entry.object.clone(), position);
            let open = folds.open.contains(name);
            push(
                &mut lines,
                Line::Fold {
                    downtime: name.clone(),
                    parent: parent.clone(),
                    count: entry.folded.len(),
                    open,
                },
                Some(ItemKey::Fold(name.clone())),
            );
            if !open {
                continue;
            }
            let problems = entry
                .folded
                .iter()
                .filter(|service| is_problem(snapshot, service))
                .count();
            let all_services = folds.all_services.contains(name);
            let services = paging::shown_count(entry.folded.len(), problems, all_services);
            for service in entry.folded.iter().take(services) {
                push(
                    &mut lines,
                    Line::Service {
                        downtime: name.clone(),
                        parent: parent.clone(),
                        object: service.clone(),
                    },
                    Some(ItemKey::Service(name.clone(), service.clone())),
                );
            }
            if paging::pages(entry.folded.len(), problems) {
                let key = MoreKey::Fold(name.clone());
                push(
                    &mut lines,
                    Line::More {
                        key: key.clone(),
                        hidden: entry.folded.len() - services,
                    },
                    Some(ItemKey::More(key)),
                );
            }
        }
        if entries.len() > PREVIEW {
            let key = MoreKey::Thread(object.clone());
            push(
                &mut lines,
                Line::More {
                    key: key.clone(),
                    hidden: entries.len() - shown_count,
                },
                Some(ItemKey::More(key)),
            );
        }
    }
    Listing {
        lines: Arc::new(lines),
        summary,
        downtimes: Arc::clone(&snapshot.downtimes),
    }
}

/// The host's downtime with services among `entries` (the one in effect,
/// else the first to start), for the band.
fn covers(entries: &[Arc<Entry>], candidates: &[Candidate<'_>]) -> Option<Covers> {
    entries
        .iter()
        .zip(candidates)
        .filter_map(|(entry, candidate)| {
            let services = entry.services.filter(|count| *count > 0)?;
            Some(Covers {
                services,
                in_effect: entry.kind == EntryKind::InEffect,
                start: Timestamp::from_unix_seconds(candidate.start),
            })
        })
        .min_by(|a, b| {
            b.in_effect.cmp(&a.in_effect).then_with(|| {
                a.start
                    .as_unix_seconds()
                    .total_cmp(&b.start.as_unix_seconds())
            })
        })
}

/// Whether `object` is a problem now (a fold's services: problems first).
fn is_problem(snapshot: &Snapshot, object: &ObjectKey) -> bool {
    crate::downtimes::state_of(snapshot, object).is_some_and(ic_model::CheckableState::is_problem)
}

/// Turns a candidate into an entry: a host's downtime learns what it
/// covers and which services fold under it.
fn place(
    candidate: &Candidate<'_>,
    snapshot: &Snapshot,
    tree: &Tree<'_>,
    shown: &HashSet<ObjectKey>,
    scope: Scope<'_>,
) -> Entry {
    let mut services = None;
    let mut own = false;
    let mut folded: Vec<ObjectKey> = Vec::new();
    if let (EntryKey::Downtime(name), true) = (&candidate.key, candidate.is_downtime()) {
        match &candidate.object {
            ObjectKey::Host { name: host } => {
                let children = tree.children.get(name.as_str());
                let count = children.map_or(0, Vec::len);
                services =
                    (count > 0 || snapshot.services_of(host).next().is_none()).then_some(count);
                if let Some(children) = children {
                    folded = children
                        .iter()
                        .map(|child| child.object.clone())
                        .filter(|object| !shown.contains(object))
                        .collect::<HashSet<_>>()
                        .into_iter()
                        .collect();
                    folded.sort_by(|a, b| {
                        let severity = |object: &ObjectKey| match object {
                            ObjectKey::Service { key } => snapshot
                                .services
                                .get(key)
                                .map_or(0, |service| service.severity()),
                            ObjectKey::Host { .. } => 0,
                        };
                        let name = |object: &ObjectKey| match object {
                            ObjectKey::Service { key } => snapshot.services.get(key).map_or_else(
                                || key.name.to_string(),
                                |service| service.display_name.clone(),
                            ),
                            ObjectKey::Host { name } => name.to_string(),
                        };
                        paging::service_order((severity(a), &name(a)), (severity(b), &name(b)))
                    });
                }
            }
            ObjectKey::Service { .. } => {
                own = snapshot
                    .downtimes
                    .get(&candidate.object)
                    .is_some_and(|list| list.iter().any(|other| tree.folds(other, scope)));
            }
        }
    }
    Entry {
        key: candidate.key.clone(),
        object: candidate.object.clone(),
        record: candidate.record,
        kind: candidate.kind,
        config: candidate.config,
        services,
        own,
        folded: Arc::new(folded),
    }
}

/// One object's thread, oldest first (a downtime from the config after
/// the conversation), for its pane: its acknowledgement, every downtime
/// still running or to come (one it has with its host's included, which
/// the pane's banner shows) and its free-standing comments.
pub(crate) fn thread_of(snapshot: &Snapshot, object: &ObjectKey, now: Timestamp) -> Vec<Entry> {
    let mut found: Vec<(Entry, f64)> = Vec::new();
    let check = match object {
        ObjectKey::Host { name } => snapshot.hosts.get(name).map(|host| &host.check),
        ObjectKey::Service { key } => snapshot.services.get(key).map(|service| &service.check),
    };
    if check.is_some_and(|check| check.acknowledgement.is_acknowledged()) {
        let at = ack_comment(snapshot, object).map_or(f64::NEG_INFINITY, |comment| {
            comment.entry_time.as_unix_seconds()
        });
        found.push((
            bare(
                EntryKey::Ack(object.clone()),
                object,
                Record::Ack,
                EntryKind::Ack,
            ),
            at,
        ));
    }
    for (index, comment) in snapshot
        .comments
        .get(object)
        .into_iter()
        .flatten()
        .enumerate()
    {
        if is_automatic(comment.kind) {
            continue;
        }
        found.push((
            bare(
                EntryKey::Comment(comment.name.clone()),
                object,
                Record::Comment(index),
                EntryKind::Comment,
            ),
            comment.entry_time.as_unix_seconds(),
        ));
    }
    let own_list = crate::downtimes::of(snapshot, object);
    for (index, downtime) in own_list.iter().enumerate() {
        if !live(downtime, now) {
            continue;
        }
        let kind = if downtime.phase(now) == DowntimePhase::InEffect {
            EntryKind::InEffect
        } else {
            EntryKind::Upcoming
        };
        let mut entry = bare(
            EntryKey::Downtime(downtime.name.clone()),
            object,
            Record::Downtime(index),
            kind,
        );
        entry.config = downtime.config_owned;
        match object {
            ObjectKey::Host { name } => {
                let services = snapshot
                    .services_of(name)
                    .filter(|service| {
                        crate::downtimes::of(snapshot, &service.object_key())
                            .iter()
                            .any(|child| {
                                live(child, now)
                                    && crate::downtimes::host_parent(snapshot, child)
                                        .is_some_and(|parent| parent.name == downtime.name)
                            })
                    })
                    .count();
                let has_services = snapshot.services_of(name).next().is_some();
                entry.services = (services > 0 || !has_services).then_some(services);
            }
            ObjectKey::Service { .. } => {
                let child = crate::downtimes::host_parent(snapshot, downtime).is_some();
                entry.own = !child
                    && own_list.iter().any(|other| {
                        live(other, now) && crate::downtimes::host_parent(snapshot, other).is_some()
                    });
            }
        }
        found.push((entry, downtime.entry_time.as_unix_seconds()));
    }
    found.sort_by(|(a, a_at), (b, b_at)| {
        a.config
            .cmp(&b.config)
            .then_with(|| a_at.total_cmp(b_at))
            .then_with(|| a.key.cmp(&b.key))
    });
    found.into_iter().map(|(entry, _)| entry).collect()
}

/// An entry with nothing folded under it.
fn bare(key: EntryKey, object: &ObjectKey, record: Record, kind: EntryKind) -> Entry {
    Entry {
        key,
        object: object.clone(),
        record,
        kind,
        config: false,
        services: None,
        own: false,
        folded: Arc::new(Vec::new()),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use chrono::Utc;
    use ic_model::{CommentKind, Host, HostState, Service, ServiceState};

    use super::*;
    use crate::lists::words::{SlotA, Tone, entry_text_in};

    /// Wednesday 7 October 2026, 14:12 UTC: the mock-ups' clock.
    fn now() -> Timestamp {
        Timestamp::from_unix_seconds(1_791_382_320.)
    }

    fn at(minutes: f64) -> Timestamp {
        Timestamp::from_unix_seconds(now().as_unix_seconds() + minutes * 60.)
    }

    fn downtime(object: &ObjectKey, name: &str, author: &str, start: f64, end: f64) -> Downtime {
        Downtime {
            name: format!("{}!{name}", object.full_name()),
            object: object.clone(),
            author: author.to_owned(),
            comment: format!("{name} work"),
            start_time: at(start),
            end_time: at(end),
            fixed: true,
            duration: 0.0,
            // Set half an hour before it starts, or half an hour ago.
            entry_time: at(start.min(0.) - 30.),
            trigger_time: None,
            triggered_by: None,
            parent: None,
            in_effect: start <= 0. && end > 0.,
            config_owned: false,
            schedule: None,
        }
    }

    fn comment(
        object: &ObjectKey,
        name: &str,
        author: &str,
        kind: CommentKind,
        ago: f64,
    ) -> Comment {
        Comment {
            name: format!("{}!{name}", object.full_name()),
            object: object.clone(),
            author: author.to_owned(),
            text: format!("{name} text"),
            kind,
            entry_time: at(-ago),
            expire_time: None,
            persistent: false,
        }
    }

    use ic_model::Comment;

    /// The mock-ups' world: k8s-node-07 drained with its services (kubelet
    /// has a longer downtime of its own), postgres-replication in downtime
    /// with a conversation, pg-autovacuum acknowledged until 15:00 with a
    /// comment, rabbitmq-queue with a comment, sw-core-ams-02 with three
    /// downtimes, haproxy-backend's tonight.
    struct World {
        snapshot: Snapshot,
    }

    impl World {
        #[expect(
            clippy::too_many_lines,
            reason = "the mock-ups' world in one place, as drawn"
        )]
        fn new() -> Self {
            let mut hosts = BTreeMap::new();
            for name in [
                "k8s-node-07",
                "db-prod-03",
                "db-prod-01",
                "mq-prod-01",
                "sw-core-ams-02",
                "lb-prod-02",
            ] {
                let mut host = Host::new(name);
                host.state = HostState::Up;
                hosts.insert(host.name.clone(), Arc::new(host));
            }
            let mut services = BTreeMap::new();
            for (host, name, state) in [
                ("k8s-node-07", "disk /var", ServiceState::Critical),
                ("k8s-node-07", "kubelet", ServiceState::Critical),
                ("k8s-node-07", "load", ServiceState::Ok),
                ("k8s-node-07", "memory", ServiceState::Ok),
                ("db-prod-03", "postgres-replication", ServiceState::Critical),
                ("db-prod-01", "pg-autovacuum", ServiceState::Critical),
                ("mq-prod-01", "rabbitmq-queue", ServiceState::Critical),
                ("sw-core-ams-02", "uplink", ServiceState::Critical),
                ("lb-prod-02", "haproxy-backend", ServiceState::Warning),
            ] {
                let mut service = Service::new(host, name);
                service.state = state;
                services.insert(service.key.clone(), Arc::new(service));
            }
            let node = ObjectKey::host("k8s-node-07");
            let drain = downtime(&node, "drain", "m.keller", -42., 78.);
            let mut downtimes = vec![drain.clone()];
            for service in ["disk /var", "kubelet", "load", "memory"] {
                let mut child = downtime(
                    &ObjectKey::service("k8s-node-07", service),
                    "drain",
                    "m.keller",
                    -42.,
                    78.,
                );
                child.parent = Some(drain.name.clone());
                downtimes.push(child);
            }
            downtimes.push(downtime(
                &ObjectKey::service("k8s-node-07", "kubelet"),
                "reimage",
                "j.berg",
                -42.,
                168.,
            ));
            let replication = ObjectKey::service("db-prod-03", "postgres-replication");
            downtimes.push(downtime(&replication, "drill", "j.berg", -72., 108.));
            let switch = ObjectKey::host("sw-core-ams-02");
            let swap = downtime(&switch, "swap", "m.keller", -12., 48.);
            let mut swap_child = downtime(
                &ObjectKey::service("sw-core-ams-02", "uplink"),
                "swap",
                "m.keller",
                -12.,
                48.,
            );
            // An Icinga that doesn't link them: the twin folds all the same.
            swap_child.parent = None;
            downtimes.push(swap.clone());
            downtimes.push(swap_child);
            let mut upgrade = downtime(&switch, "upgrade", "m.keller", 468., 948.);
            upgrade.fixed = false;
            upgrade.duration = 3_600.;
            downtimes.push(upgrade);
            let mut weekly = downtime(&switch, "weekly", "icingaadmin", 3_828., 4_068.);
            weekly.config_owned = true;
            weekly.schedule = Some("weekly-patching".to_owned());
            weekly.entry_time = at(-10_000.);
            downtimes.push(weekly);
            downtimes.push(downtime(
                &ObjectKey::service("lb-prod-02", "haproxy-backend"),
                "rollout",
                "m.keller",
                468.,
                528.,
            ));
            // One that is over: never shown.
            downtimes.push(downtime(
                &ObjectKey::host("lb-prod-02"),
                "over",
                "j.berg",
                -120.,
                -1.,
            ));
            let autovacuum = ObjectKey::service("db-prod-01", "pg-autovacuum");
            let comments = vec![
                comment(&replication, "a", "j.berg", CommentKind::User, 31.),
                comment(&replication, "b", "dba-oncall", CommentKind::User, 14.),
                comment(&replication, "c", "j.berg", CommentKind::User, 7.),
                // Icinga's own: never entries.
                comment(&replication, "auto", "j.berg", CommentKind::Downtime, 72.),
                comment(
                    &autovacuum,
                    "ack",
                    "dba-oncall",
                    CommentKind::Acknowledgement,
                    60.,
                ),
                comment(&autovacuum, "still", "j.berg", CommentKind::User, 28.),
                comment(
                    &ObjectKey::service("mq-prod-01", "rabbitmq-queue"),
                    "rollback",
                    "m.keller",
                    CommentKind::User,
                    3.,
                ),
            ];
            let mut world = Self {
                snapshot: Snapshot {
                    hosts: Arc::new(hosts),
                    services: Arc::new(services),
                    ..Snapshot::default()
                },
            };
            world.set_downtimes(downtimes);
            world.set_comments(comments);
            world.acknowledge(&autovacuum, AckKind::Normal, Some(48.));
            world
        }

        fn set_downtimes(&mut self, list: Vec<Downtime>) {
            let mut map: BTreeMap<ObjectKey, Vec<Downtime>> = BTreeMap::new();
            for downtime in list {
                map.entry(downtime.object.clone())
                    .or_default()
                    .push(downtime);
            }
            self.snapshot.downtimes = Arc::new(map);
        }

        fn set_comments(&mut self, list: Vec<Comment>) {
            let mut map: BTreeMap<ObjectKey, Vec<Comment>> = BTreeMap::new();
            for comment in list {
                map.entry(comment.object.clone()).or_default().push(comment);
            }
            self.snapshot.comments = Arc::new(map);
        }

        fn acknowledge(&mut self, object: &ObjectKey, kind: AckKind, expiry: Option<f64>) {
            let mut services = (*self.snapshot.services).clone();
            if let ObjectKey::Service { key } = object {
                let service = Arc::make_mut(services.get_mut(key).unwrap());
                service.check.acknowledgement = kind;
                service.check.acknowledgement_expiry = expiry.map(at);
            }
            self.snapshot.services = Arc::new(services);
        }

        fn build(&self, kind: ListKind, options: &Options) -> Listing {
            build(
                kind,
                &self.snapshot,
                Scope::All,
                "m.keller",
                options,
                &Folds::default(),
                now(),
            )
        }
    }

    /// The lines as short words, for comparing.
    fn sketch(listing: &Listing) -> Vec<String> {
        listing
            .lines
            .iter()
            .map(|keyed| match &keyed.line {
                Line::Section { section, count } => format!("## {}", section.title(*count)),
                Line::Axis => "axis".to_owned(),
                Line::Band { object, slot, .. } => format!("[{}] {slot}", object.full_name()),
                Line::Entry {
                    entry,
                    reply,
                    single,
                } => format!(
                    "{}{}{:?} {}",
                    if *single { "1 " } else { "  " },
                    if *reply { "| " } else { "" },
                    entry.kind,
                    match &entry.key {
                        EntryKey::Ack(object) => object.full_name(),
                        EntryKey::Downtime(name) | EntryKey::Comment(name) => name.clone(),
                    }
                ),
                Line::Fold { count, open, .. } => {
                    format!("  fold {count} {}", if *open { "open" } else { "closed" })
                }
                Line::Service { object, .. } => format!("    {}", object.full_name()),
                Line::More { hidden, .. } => format!("  more {hidden}"),
                Line::Draft { id, refused, .. } => {
                    format!("  draft {id}{}", if *refused { " refused" } else { "" })
                }
                Line::Composer { object } => format!("  field {}", object.full_name()),
            })
            .collect()
    }

    #[test]
    fn comments_go_after_a_threads_last_line_and_never_into_a_folded_one() {
        let world = World::new();
        let postgres = ObjectKey::service("db-prod-03", "postgres-replication");
        let switch = ObjectKey::host("sw-core-ams-02");
        let elsewhere = ObjectKey::host("nowhere-01");
        let mut listing = world.build(ListKind::Handling, &Options::default());
        let before = sketch(&listing);
        // The last entry of a thread offers *+ comment*; earlier ones and
        // bands don't.
        let band = before
            .iter()
            .position(|line| line.starts_with("[db-prod-03!postgres-replication]"))
            .unwrap();
        assert!(!listing.is_last_entry(band));
        assert!(!listing.is_last_entry(band + 1));
        assert!(listing.is_last_entry(band + 4), "{before:#?}");
        place_comments(
            &mut listing,
            &CommentLines {
                drafts: vec![
                    (postgres.clone(), 1, false),
                    (switch.clone(), 2, true),
                    (elsewhere.clone(), 3, false),
                    (postgres.clone(), 4, true),
                ],
                composer: Some(postgres.clone()),
            },
        );
        let after = sketch(&listing);
        assert_eq!(
            after[band + 5..band + 8],
            [
                "  draft 1",
                "  draft 4 refused",
                "  field db-prod-03!postgres-replication"
            ],
            "after the thread's last entry, in the order sent, then the field"
        );
        // The switch's thread (a host's downtime with its services folded):
        // its draft comes at its end.
        let switch_band = after
            .iter()
            .position(|line| line.starts_with("[sw-core-ams-02]"))
            .unwrap();
        let draft = after
            .iter()
            .position(|line| line == "  draft 2 refused")
            .unwrap();
        assert!(draft > switch_band);
        assert!(
            after[switch_band + 1..draft]
                .iter()
                .all(|line| !line.starts_with('[')),
            "inside its own thread: {after:#?}"
        );
        assert!(
            after
                .get(draft + 1)
                .is_none_or(|line| line.starts_with('[') || line.starts_with("##")),
            "at the thread's end: {after:#?}"
        );
        // An object without a thread here gets nothing; the added lines
        // have no keys, so the cursor never stops on them.
        assert!(!after.iter().any(|line| line == "  draft 3"));
        assert_eq!(after.len(), before.len() + 4);
        assert!(
            listing
                .lines
                .iter()
                .filter(|keyed| matches!(keyed.line, Line::Draft { .. } | Line::Composer { .. }))
                .all(|keyed| keyed.key.is_none())
        );

        // A folded thread offers nothing and gets nothing.
        let folds = Folds {
            collapsed: std::iter::once(postgres.clone()).collect(),
            ..Folds::default()
        };
        let mut folded = build(
            ListKind::Handling,
            &world.snapshot,
            Scope::All,
            "m.keller",
            &Options::default(),
            &folds,
            now(),
        );
        let count = folded.lines.len();
        place_comments(
            &mut folded,
            &CommentLines {
                drafts: vec![(postgres.clone(), 1, false)],
                composer: Some(postgres),
            },
        );
        assert_eq!(folded.lines.len(), count);
    }

    #[test]
    fn handling_is_a_thread_per_object_oldest_first() {
        let world = World::new();
        let listing = world.build(ListKind::Handling, &Options::default());
        let lines = sketch(&listing);
        // Latest activity first: rabbitmq (3m ago), postgres (7m), …
        let bands: Vec<&String> = lines.iter().filter(|line| line.starts_with('[')).collect();
        assert_eq!(bands[0], "[mq-prod-01!rabbitmq-queue] 1 comment");
        assert!(
            bands.contains(
                &&"[db-prod-03!postgres-replication] in downtime · 3 comments".to_owned()
            ),
            "{lines:#?}"
        );
        assert!(bands.contains(&&"[db-prod-01!pg-autovacuum] acknowledged · 1 comment".to_owned()));
        assert!(bands.contains(&&"[sw-core-ams-02] in downtime · 2 upcoming".to_owned()));
        // The postgres thread: its downtime, then the comments as replies;
        // Icinga's own downtime comment is no entry.
        let at = lines
            .iter()
            .position(|line| line.starts_with("[db-prod-03!postgres-replication]"))
            .unwrap();
        assert_eq!(
            lines[at + 1..at + 5],
            [
                "  InEffect db-prod-03!postgres-replication!drill",
                "  | Comment db-prod-03!postgres-replication!a",
                "  | Comment db-prod-03!postgres-replication!b",
                "  | Comment db-prod-03!postgres-replication!c",
            ]
        );
        // The acknowledgement's comment is part of its entry, not one of
        // its own.
        let autovacuum = lines
            .iter()
            .position(|line| line.starts_with("[db-prod-01!pg-autovacuum]"))
            .unwrap();
        assert_eq!(
            lines[autovacuum + 1..autovacuum + 3],
            [
                "  Ack db-prod-01!pg-autovacuum",
                "  | Comment db-prod-01!pg-autovacuum!still",
            ]
        );
        // The switch: the swap with its uplink folded (a twin: no parent
        // link), the upgrade, then the config downtime last.
        let switch = lines
            .iter()
            .position(|line| line.starts_with("[sw-core-ams-02]"))
            .unwrap();
        assert_eq!(
            lines[switch + 1..switch + 5],
            [
                "  InEffect sw-core-ams-02!swap",
                "  fold 1 closed",
                "  | Upcoming sw-core-ams-02!upgrade",
                "  | Upcoming sw-core-ams-02!weekly",
            ]
        );
        // The same object never twice; the over downtime nowhere.
        let mut objects: Vec<&String> = bands.clone();
        objects.dedup();
        assert_eq!(objects.len(), bands.len());
        assert!(!lines.iter().any(|line| line.contains("!over")));
        assert_eq!(listing.summary.objects, bands.len());
        assert_eq!(
            handled_objects(&world.snapshot, Scope::All, now()),
            bands.len()
        );
    }

    #[test]
    fn a_hosts_identical_service_downtimes_fold_and_a_service_of_its_own_stays_apart() {
        let world = World::new();
        let listing = world.build(ListKind::Handling, &Options::default());
        let lines = sketch(&listing);
        let node = lines
            .iter()
            .position(|line| line == "[k8s-node-07] in downtime")
            .unwrap();
        // disk /var, load and memory fold; kubelet has its own downtime, so
        // it is its own group and not in the fold.
        assert_eq!(
            lines[node + 1..node + 3],
            ["  InEffect k8s-node-07!drain", "  fold 3 closed"]
        );
        assert!(lines.contains(&"[k8s-node-07!kubelet] in downtime".to_owned()));
        let kubelet = listing
            .lines
            .iter()
            .filter_map(|keyed| keyed.line.entry())
            .find(|entry| entry.key == EntryKey::Downtime("k8s-node-07!kubelet!reimage".to_owned()))
            .unwrap();
        assert!(kubelet.own);
        let drain = listing
            .lines
            .iter()
            .filter_map(|keyed| keyed.line.entry())
            .find(|entry| entry.key == EntryKey::Downtime("k8s-node-07!drain".to_owned()))
            .unwrap();
        assert_eq!(drain.services, Some(4));
        // Problems first in the fold.
        assert_eq!(
            drain
                .folded
                .iter()
                .map(ObjectKey::full_name)
                .collect::<Vec<_>>(),
            [
                "k8s-node-07!disk /var",
                "k8s-node-07!load",
                "k8s-node-07!memory"
            ]
        );
        // Opened, the services show under it.
        let mut folds = Folds::default();
        folds.open.insert("k8s-node-07!drain".to_owned());
        let open = build(
            ListKind::Handling,
            &world.snapshot,
            Scope::All,
            "",
            &Options::default(),
            &folds,
            now(),
        );
        let lines = sketch(&open);
        let node = lines
            .iter()
            .position(|line| line == "[k8s-node-07] in downtime")
            .unwrap();
        assert_eq!(
            lines[node + 2..node + 6],
            [
                "  fold 3 open",
                "    k8s-node-07!disk /var",
                "    k8s-node-07!load",
                "    k8s-node-07!memory",
            ]
        );
        // Folded, the band alone.
        folds.collapsed.insert(ObjectKey::host("k8s-node-07"));
        let collapsed = build(
            ListKind::Handling,
            &world.snapshot,
            Scope::All,
            "",
            &Options::default(),
            &folds,
            now(),
        );
        let lines = sketch(&collapsed);
        let node = lines
            .iter()
            .position(|line| line == "[k8s-node-07] in downtime")
            .unwrap();
        assert!(lines[node + 1].starts_with('['));
    }

    #[test]
    fn a_fold_pages_by_count() {
        let mut world = World::new();
        let host = ObjectKey::host("big");
        let mut hosts = (*world.snapshot.hosts).clone();
        hosts.insert(ic_model::HostName::new("big"), Arc::new(Host::new("big")));
        world.snapshot.hosts = Arc::new(hosts);
        let parent = downtime(&host, "all", "m.keller", -10., 50.);
        let mut list = vec![parent.clone()];
        let mut services = (*world.snapshot.services).clone();
        for index in 0..22 {
            let name = format!("s{index:02}");
            let service = Service::new("big", &name);
            services.insert(service.key.clone(), Arc::new(service));
            let mut child = downtime(
                &ObjectKey::service("big", &name),
                "all",
                "m.keller",
                -10.,
                50.,
            );
            child.parent = Some(parent.name.clone());
            list.push(child);
        }
        world.snapshot.services = Arc::new(services);
        world.set_downtimes(list);
        let mut folds = Folds::default();
        folds.open.insert(parent.name.clone());
        let listing = build(
            ListKind::Handling,
            &world.snapshot,
            Scope::All,
            "",
            &Options::default(),
            &folds,
            now(),
        );
        let lines = sketch(&listing);
        let fold = lines
            .iter()
            .position(|line| line == "  fold 22 open")
            .unwrap();
        assert_eq!(lines[fold + 8], "  more 15");
        folds.all_services.insert(parent.name.clone());
        let listing = build(
            ListKind::Handling,
            &world.snapshot,
            Scope::All,
            "",
            &Options::default(),
            &folds,
            now(),
        );
        let lines = sketch(&listing);
        let fold = lines
            .iter()
            .position(|line| line == "  fold 22 open")
            .unwrap();
        assert_eq!(lines[fold + 23], "  more 0");
        assert_eq!(listing.summary.folded, 22);
    }

    #[test]
    fn chips_filter_the_entries_and_the_bands_keep_saying_what_the_thread_holds() {
        let world = World::new();
        let mut options = Options::default();
        let all = world.build(ListKind::Handling, &options);
        assert_eq!(all.summary.count(Chip::Acknowledged), Some(1));
        assert_eq!(all.summary.count(Chip::InEffect), Some(4));
        assert_eq!(all.summary.count(Chip::Upcoming), Some(3));
        assert_eq!(all.summary.count(Chip::Comments), Some(5));
        options.pick_chip(Chip::Comments);
        let comments = world.build(ListKind::Handling, &options);
        let lines = sketch(&comments);
        assert!(
            lines
                .contains(&"[db-prod-03!postgres-replication] in downtime · 3 comments".to_owned())
        );
        assert!(
            lines
                .iter()
                .all(|line| !line.contains("InEffect") && !line.contains("Ack "))
        );
        // The first comment of a thread is no reply.
        assert!(lines.contains(&"  Comment db-prod-03!postgres-replication!a".to_owned()));
        assert_eq!(comments.summary.objects, 3);
        // The acknowledged chip: by expiry, in sections.
        options.pick_chip(Chip::Acknowledged);
        assert_eq!(options.sort(ListKind::Handling), SortChoice::Soonest);
        let acknowledged = world.build(ListKind::Handling, &options);
        assert_eq!(
            sketch(&acknowledged),
            [
                "## expires within 2 hours · 1",
                "[db-prod-01!pg-autovacuum] acknowledged · 1 comment",
                "  Ack db-prod-01!pg-autovacuum",
            ]
        );
    }

    #[test]
    fn acknowledgements_fall_in_expiry_sections() {
        let mut world = World::new();
        world.acknowledge(
            &ObjectKey::service("mq-prod-01", "rabbitmq-queue"),
            AckKind::Sticky,
            Some(600.),
        );
        world.acknowledge(
            &ObjectKey::service("lb-prod-02", "haproxy-backend"),
            AckKind::Normal,
            None,
        );
        let mut options = Options::default();
        options.pick_chip(Chip::Acknowledged);
        let listing = world.build(ListKind::Handling, &options);
        let sections: Vec<String> = sketch(&listing)
            .into_iter()
            .filter(|line| line.starts_with("##"))
            .collect();
        assert_eq!(
            sections,
            [
                "## expires within 2 hours · 1",
                "## expires later · 1",
                "## no expiry · 1",
            ]
        );
        assert_eq!(listing.summary.sticky, 1);
    }

    #[test]
    fn a_reload_is_no_ones_activity() {
        // Icinga makes config downtimes again at every reload: one made a
        // minute ago doesn't bring its object to the top.
        let mut world = World::new();
        let mut downtimes: Vec<Downtime> = world
            .snapshot
            .downtimes
            .values()
            .flatten()
            .cloned()
            .collect();
        for downtime in &mut downtimes {
            if downtime.config_owned {
                downtime.entry_time = at(-1.);
            }
        }
        world.set_downtimes(downtimes);
        let listing = world.build(ListKind::Handling, &Options::default());
        let lines = sketch(&listing);
        let first = lines.iter().find(|line| line.starts_with('[')).unwrap();
        assert_eq!(first, "[mq-prod-01!rabbitmq-queue] 1 comment", "{lines:#?}");
    }

    #[test]
    fn only_mine_keeps_the_authors_entries() {
        let world = World::new();
        let options = Options {
            only_mine: true,
            ..Options::default()
        };
        let listing = world.build(ListKind::Handling, &options);
        let lines = sketch(&listing);
        // m.keller's: the drain, the swap and the switch's upgrade, the
        // haproxy rollout, the rabbitmq comment; kubelet's own downtime is
        // j.berg's, so kubelet folds into its host's again.
        assert!(lines.contains(&"  fold 4 closed".to_owned()), "{lines:#?}");
        assert!(!lines.iter().any(|line| line.contains("postgres")));
        assert!(listing.summary.by_others > 0);
    }

    #[test]
    fn the_downtimes_list_has_sections_and_single_downtimes_take_one_row() {
        let world = World::new();
        let options = Options {
            mode: Mode::List,
            ..Options::default()
        };
        let listing = world.build(ListKind::Downtimes, &options);
        assert_eq!(
            sketch(&listing),
            [
                "## in effect · 4",
                "[sw-core-ams-02] 3 downtimes",
                "  InEffect sw-core-ams-02!swap",
                "  fold 1 closed",
                "  | Upcoming sw-core-ams-02!upgrade",
                "  | Upcoming sw-core-ams-02!weekly",
                "[k8s-node-07] 1 downtime",
                "  InEffect k8s-node-07!drain",
                "  fold 3 closed",
                "1 InEffect db-prod-03!postgres-replication!drill",
                "1 InEffect k8s-node-07!kubelet!reimage",
                "## upcoming · 1",
                "1 Upcoming lb-prod-02!haproxy-backend!rollout",
            ]
        );
        assert_eq!(listing.summary.count(Chip::InEffect), Some(4));
        assert_eq!(listing.summary.count(Chip::Upcoming), Some(2));
        assert_eq!(listing.summary.count(Chip::FromConfig), Some(1));
        assert_eq!(listing.summary.downtimes, 7);
        assert_eq!(
            downtimes_in_effect(&world.snapshot, Scope::All, DowntimeKinds::default(), now()),
            4
        );
    }

    #[test]
    fn the_timeline_orders_by_time_with_one_line_per_downtime() {
        let world = World::new();
        let listing = world.build(ListKind::Downtimes, &Options::default());
        assert_eq!(
            sketch(&listing),
            [
                "axis",
                "1 InEffect db-prod-03!postgres-replication!drill",
                "[k8s-node-07] 1 downtime",
                "  InEffect k8s-node-07!drain",
                "  fold 3 closed",
                "1 InEffect k8s-node-07!kubelet!reimage",
                "[sw-core-ams-02] 3 downtimes",
                "  InEffect sw-core-ams-02!swap",
                "  fold 1 closed",
                "  | Upcoming sw-core-ams-02!upgrade",
                "  | Upcoming sw-core-ams-02!weekly",
                "1 Upcoming lb-prod-02!haproxy-backend!rollout",
            ]
        );
        // The chips: from config alone.
        let options = Options {
            chip: Chip::FromConfig,
            ..Options::default()
        };
        let config = world.build(ListKind::Downtimes, &options);
        assert_eq!(
            sketch(&config),
            ["axis", "1 Upcoming sw-core-ams-02!weekly"]
        );
    }

    #[test]
    fn a_long_thread_pages_by_count() {
        let mut world = World::new();
        let queue = ObjectKey::service("mq-prod-01", "rabbitmq-queue");
        let comments: Vec<Comment> = (0..10)
            .map(|index| {
                comment(
                    &queue,
                    &format!("c{index}"),
                    "m.keller",
                    CommentKind::User,
                    f64::from(100 - index),
                )
            })
            .collect();
        world.set_comments(comments);
        let listing = world.build(ListKind::Handling, &Options::default());
        let lines = sketch(&listing);
        let band = lines
            .iter()
            .position(|line| line.starts_with("[mq-prod-01!rabbitmq-queue]"))
            .unwrap();
        assert_eq!(lines[band + 1], "  Comment mq-prod-01!rabbitmq-queue!c0");
        assert_eq!(lines[band + 8], "  more 3");
        let mut folds = Folds::default();
        folds.all_entries.insert(queue);
        let all = build(
            ListKind::Handling,
            &world.snapshot,
            Scope::All,
            "",
            &Options::default(),
            &folds,
            now(),
        );
        let lines = sketch(&all);
        let band = lines
            .iter()
            .position(|line| line.starts_with("[mq-prod-01!rabbitmq-queue]"))
            .unwrap();
        assert_eq!(lines[band + 11], "  more 0");
    }

    #[test]
    fn the_panes_thread_has_every_downtime_of_its_object() {
        let world = World::new();
        let kubelet = ObjectKey::service("k8s-node-07", "kubelet");
        let thread = thread_of(&world.snapshot, &kubelet, now());
        let keys: Vec<String> = thread
            .iter()
            .map(|entry| match &entry.key {
                EntryKey::Downtime(name) => name.clone(),
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(
            keys,
            ["k8s-node-07!kubelet!drain", "k8s-node-07!kubelet!reimage"]
        );
        assert!(!thread[0].own);
        assert!(thread[1].own);
        let words = |entry: &Entry| entry_text_in(&world.snapshot, entry, now(), &Utc).unwrap();
        assert_eq!(
            words(&thread[0]).meta,
            "fixed 13:30 → 15:30 · with its host"
        );
        assert_eq!(words(&thread[1]).meta, "fixed 13:30 → 17:00 · its own");
        let node = thread_of(&world.snapshot, &ObjectKey::host("k8s-node-07"), now());
        assert_eq!(
            words(&node[0]).meta,
            "fixed 13:30 → 15:30 · host and 4 services"
        );
    }

    #[test]
    fn entries_read_as_drawn() {
        let world = World::new();
        let words = |object: &ObjectKey| {
            thread_of(&world.snapshot, object, now())
                .iter()
                .map(|entry| entry_text_in(&world.snapshot, entry, now(), &Utc).unwrap())
                .collect::<Vec<_>>()
        };
        let autovacuum = words(&ObjectKey::service("db-prod-01", "pg-autovacuum"));
        assert_eq!(autovacuum[0].kind, Some("acknowledged"));
        assert_eq!(autovacuum[0].author, "dba-oncall");
        assert_eq!(autovacuum[0].at, "13:12");
        assert_eq!(autovacuum[0].slot_b, "expires 15:00, in 48m");
        assert_eq!(autovacuum[0].tone, Tone::Warning);
        assert_eq!(autovacuum[1].kind, None);
        let replication = words(&ObjectKey::service("db-prod-03", "postgres-replication"));
        assert_eq!(replication[0].kind, Some("in downtime"));
        assert_eq!(replication[0].slot_b, "1h 48m left");
        assert_eq!(replication[0].tone, Tone::Accent);
        assert!(
            matches!(replication[0].slot_a, SlotA::Progress(fraction) if (fraction - 0.4).abs() < 0.01)
        );
        let switch = words(&ObjectKey::host("sw-core-ams-02"));
        assert_eq!(switch[1].kind, Some("downtime, upcoming"));
        assert_eq!(
            switch[1].meta,
            "flexible 1h · window 22:00 → 06:00 · host only"
        );
        assert_eq!(switch[1].slot_b, "by 22:00");
        assert_eq!(switch[2].kind, Some("downtime, from config"));
        assert_eq!(switch[2].author, "weekly-patching");
        assert_eq!(switch[2].at, "");
        assert_eq!(switch[2].meta, "fixed · Sat 06:00 → 10:00 · host only");
        assert_eq!(switch[2].slot_b, "Sat 06:00");
        let haproxy = words(&ObjectKey::service("lb-prod-02", "haproxy-backend"));
        assert_eq!(haproxy[0].slot_b, "in 7h 48m");
    }

    #[test]
    fn a_large_installation_builds_quickly() {
        // 2 000 hosts each in a downtime with its 15 services, an
        // acknowledgement and a comment on every tenth host's first
        // service: 32 000 downtimes.
        let mut hosts = BTreeMap::new();
        let mut services = BTreeMap::new();
        let mut downtimes = Vec::new();
        let mut comments = Vec::new();
        for index in 0..2_000 {
            let name = format!("h{index:04}");
            hosts.insert(ic_model::HostName::new(&name), Arc::new(Host::new(&name)));
            let parent = downtime(
                &ObjectKey::host(&name),
                "d",
                "a",
                -10.,
                50. + f64::from(index),
            );
            for service in 0..15 {
                let mut entry = Service::new(&name, &format!("s{service}"));
                if index % 10 == 0 && service == 0 {
                    entry.check.acknowledgement = AckKind::Normal;
                    comments.push(comment(
                        &entry.object_key(),
                        "c",
                        "b",
                        CommentKind::User,
                        5.,
                    ));
                }
                let mut child =
                    downtime(&entry.object_key(), "d", "a", -10., 50. + f64::from(index));
                child.parent = Some(parent.name.clone());
                downtimes.push(child);
                services.insert(entry.key.clone(), Arc::new(entry));
            }
            downtimes.push(parent);
        }
        let mut world = World {
            snapshot: Snapshot {
                hosts: Arc::new(hosts),
                services: Arc::new(services),
                ..Snapshot::default()
            },
        };
        world.set_downtimes(downtimes);
        world.set_comments(comments);
        let started = std::time::Instant::now();
        let listing = world.build(ListKind::Handling, &Options::default());
        let downtimes = world.build(ListKind::Downtimes, &Options::default());
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(listing.summary.objects, 2_200);
        assert_eq!(downtimes.summary.downtimes, 2_000);
        // The downtimes view folds every service; handling leaves out the
        // 200 with a thread of their own (they show on their own).
        assert_eq!(downtimes.summary.folded, 30_000);
        assert_eq!(listing.summary.folded, 30_000 - 200);
    }
}
