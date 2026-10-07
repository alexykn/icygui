//! What the lists of every downtime, comment and acknowledged problem show
//! (topic 07), built from the snapshot the engine keeps current: downtimes
//! and comments are part of the initial load and the event stream, and an
//! object's acknowledgement is one of its attributes. Nothing here asks
//! Icinga for anything.
//!
//! [`build`] turns the snapshot into a [`Listing`]: the rows in order (the
//! downtime list's sections, *in effect* and *upcoming*, as header rows),
//! and the summary bar's counts. Rows refer to the snapshot's downtimes
//! and comments by position, so building one costs a pass and a sort over
//! them; the words of a row ([`row_text`]) are made only for the rows on
//! screen. Pure, so it's tested without a window.
//!
//! Words, as the approved mock-ups draw them:
//!
//! - downtimes: `fixed · 13:30 → 15:30 · m.keller: drained for the kernel
//!   6.8 rollout`, `flexible 2h · window 14:00 → 18:00 · dba-oncall: …`,
//!   `fixed · Sat 06:00 → 10:00 · from config: weekly-patching`; the tag's
//!   time: `1h 18m left`, `not started`, `in 7h 48m`, `by 22:00`, `from
//!   config`; a host's downtime with its services: `+ 23 services`;
//! - comments: `m.keller 12:02 · renewal ordered, …`, in the tag the kind
//!   (`acknowledgement`, `comment`) and the detail (`sticky`, `expires Fri
//!   12:00`);
//! - acknowledged: `dba-oncall 13:30, 42m ago · lock from the migration
//!   test`, in the tag `sticky` and `expires Thu 08:00` or `no expiry`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Display;
use std::sync::Arc;
use std::time::Duration;

use chrono::{Local, TimeZone};
use ic_config::ListTimes;
use ic_core::snapshot::Snapshot;
use ic_model::{
    AckKind, CheckInfo, CheckableState, Comment, CommentKind, Downtime, DowntimePhase, HostState,
    ObjectKey, ServiceState, Timestamp, format_two_units,
};
use ic_ui_kit::{IconName, ObjectMark};

use crate::actions::ObjectAction;
use crate::dashboard::selection::SelectableRow;
use crate::{downtimes, format};

/// One of the three lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum ListKind {
    /// Every downtime, in effect and upcoming.
    Downtimes,
    /// Every comment (downtime and flapping comments on request).
    Comments,
    /// Every acknowledged problem.
    Acknowledged,
}

impl ListKind {
    /// The three, in the order the sidebar lists them.
    pub(crate) const ALL: [Self; 3] = [Self::Downtimes, Self::Comments, Self::Acknowledged];

    /// The list's name: its header's title and the sidebar's label.
    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::Downtimes => "downtimes",
            Self::Comments => "comments",
            Self::Acknowledged => "acknowledged",
        }
    }

    /// Its id in the UI state (`state.toml`).
    pub(crate) fn id(self) -> &'static str {
        self.title()
    }

    /// The list with this id.
    pub(crate) fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.id() == id)
    }

    /// The icon in the sidebar's mark slot and the palette.
    pub(crate) fn icon(self) -> IconName {
        match self {
            Self::Downtimes => IconName::CalendarClock,
            Self::Comments => IconName::MessageSquare,
            Self::Acknowledged => IconName::Check,
        }
    }

    /// What it covers, for the header's subtitle: `every object`.
    pub(crate) fn every(self) -> &'static str {
        match self {
            Self::Downtimes | Self::Comments => "every object",
            Self::Acknowledged => "every problem",
        }
    }

    /// The sorts the header offers, the default first.
    pub(crate) fn sorts(self) -> &'static [SortChoice] {
        match self {
            Self::Downtimes => &[
                SortChoice::EndsSoonest,
                SortChoice::StartsSoonest,
                SortChoice::Newest,
                SortChoice::Name,
            ],
            Self::Comments | Self::Acknowledged => {
                &[SortChoice::Newest, SortChoice::Oldest, SortChoice::Name]
            }
        }
    }

    /// The sort a list opens with: ends soonest, newest.
    pub(crate) fn default_sort(self) -> SortChoice {
        self.sorts()[0]
    }

    /// The selection bar's main button.
    pub(crate) fn removal_label(self) -> &'static str {
        match self {
            Self::Downtimes => "remove downtimes",
            Self::Comments => "remove comments",
            Self::Acknowledged => "remove acknowledgements",
        }
    }

    /// The action whose permission removing from the list needs.
    pub(crate) fn removal_action(self) -> ObjectAction {
        match self {
            Self::Downtimes => ObjectAction::RemoveNamedDowntimes(Vec::new()),
            Self::Comments => ObjectAction::RemoveComments(Vec::new()),
            Self::Acknowledged => ObjectAction::RemoveAcknowledgement,
        }
    }
}

/// How a list is sorted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum SortChoice {
    /// In effect by when they end, upcoming by when they start (the
    /// downtime list's default).
    EndsSoonest,
    /// By when they start (or started).
    StartsSoonest,
    /// The most recently set first.
    Newest,
    /// The longest set first.
    Oldest,
    /// By object: hosts and their services, in name order.
    Name,
}

impl SortChoice {
    /// The header's sort trigger: `ends soonest ↑`.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::EndsSoonest => "ends soonest ↑",
            Self::StartsSoonest => "starts soonest ↑",
            Self::Newest => "newest ↓",
            Self::Oldest => "oldest ↑",
            Self::Name => "name ↑",
        }
    }

    /// The sort menu's item.
    pub(crate) fn menu_label(self) -> &'static str {
        match self {
            Self::EndsSoonest => "ends soonest",
            Self::StartsSoonest => "starts soonest",
            Self::Newest => "newest",
            Self::Oldest => "oldest",
            Self::Name => "name",
        }
    }

    /// The sort's id in the UI state (`ends-soonest`).
    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::EndsSoonest => "ends-soonest",
            Self::StartsSoonest => "starts-soonest",
            Self::Newest => "newest",
            Self::Oldest => "oldest",
            Self::Name => "name",
        }
    }

    /// The sort a saved id names, if `kind` offers it.
    pub(crate) fn from_key(kind: ListKind, key: &str) -> Option<Self> {
        kind.sorts().iter().copied().find(|sort| sort.key() == key)
    }

    /// The sort menu item's element id.
    pub(crate) fn id(self) -> &'static str {
        match self {
            Self::EndsSoonest => "list-sort-ends",
            Self::StartsSoonest => "list-sort-starts",
            Self::Newest => "list-sort-newest",
            Self::Oldest => "list-sort-oldest",
            Self::Name => "list-sort-name",
        }
    }
}

/// What a list shows, as the user set it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Options {
    /// The order.
    pub(crate) sort: SortChoice,
    /// Only what the environment's author set (*only mine*).
    pub(crate) only_mine: bool,
    /// The comment list shows downtime and flapping comments too.
    pub(crate) system_comments: bool,
    /// The host downtimes shown with their children's rows under them
    /// (by name; folded is the default).
    pub(crate) unfolded: BTreeSet<String>,
}

impl Options {
    /// A list's options when it opens.
    pub(crate) fn new(kind: ListKind) -> Self {
        Self {
            sort: kind.default_sort(),
            only_mine: false,
            system_comments: false,
            unfolded: BTreeSet::new(),
        }
    }

    /// A list's options as saved (a sort it doesn't offer: its default).
    pub(crate) fn saved(kind: ListKind, saved: &ic_config::ListOptionsState) -> Self {
        let mut options = Self::new(kind);
        if let Some(sort) = saved
            .sort
            .as_deref()
            .and_then(|key| SortChoice::from_key(kind, key))
        {
            options.sort = sort;
        }
        options.only_mine = saved.only_mine;
        options.system_comments = saved.system_comments && kind == ListKind::Comments;
        options
    }

    /// What is kept between runs (not which hosts are unfolded); the
    /// default sort is kept as none.
    pub(crate) fn to_saved(&self, kind: ListKind) -> ic_config::ListOptionsState {
        ic_config::ListOptionsState {
            sort: (self.sort != kind.default_sort()).then(|| self.sort.key().to_owned()),
            only_mine: self.only_mine,
            system_comments: self.system_comments,
        }
    }

    /// The next sort `kind` offers, after the last the first (`s`).
    pub(crate) fn next_sort(&self, kind: ListKind) -> SortChoice {
        let sorts = kind.sorts();
        let index = sorts
            .iter()
            .position(|sort| *sort == self.sort)
            .unwrap_or(0);
        sorts[(index + 1) % sorts.len()]
    }
}

/// A section of the downtime list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Section {
    /// Downtimes in effect: their objects are handled.
    InEffect,
    /// Not in effect yet: scheduled for later, or flexible and not started.
    Upcoming,
}

impl Section {
    /// The header's title: `in effect · 4`.
    pub(crate) fn title(self, count: usize) -> String {
        match self {
            Self::InEffect => format!("in effect · {count}"),
            Self::Upcoming => format!("upcoming · {count}"),
        }
    }

    /// The header's second line: `handled now · ending soonest first`.
    pub(crate) fn detail(self, sort: SortChoice) -> String {
        let what = match self {
            Self::InEffect => "handled now",
            Self::Upcoming => "not in effect yet",
        };
        let order = match (self, sort) {
            (Self::InEffect, SortChoice::EndsSoonest) => "ending soonest first",
            (Self::Upcoming, SortChoice::EndsSoonest) | (_, SortChoice::StartsSoonest) => {
                "starting soonest first"
            }
            (_, SortChoice::Newest) => "newest first",
            (_, SortChoice::Oldest) => "oldest first",
            (_, SortChoice::Name) => "by name",
        };
        format!("{what} · {order}")
    }

    /// The header's icon.
    pub(crate) fn icon(self) -> IconName {
        match self {
            Self::InEffect => IconName::CalendarClock,
            Self::Upcoming => IconName::Clock,
        }
    }
}

/// What identifies a row: its downtime or comment by name, or the
/// acknowledged problem.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum RecordKey {
    /// A downtime, by its full name.
    Downtime(String),
    /// A comment, by its full name.
    Comment(String),
    /// An acknowledged host or service.
    Problem(ObjectKey),
}

impl RecordKey {
    /// The name or the object's full name, for element ids.
    pub(crate) fn name(&self) -> String {
        match self {
            Self::Downtime(name) | Self::Comment(name) => name.clone(),
            Self::Problem(object) => object.full_name(),
        }
    }
}

/// How many other downtimes a host's downtime stands for in its row:
/// those Icinga made with it (`all_services`, child hosts).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct Children {
    /// Of hosts (child options).
    pub(crate) hosts: usize,
    /// Of services (`all_services`).
    pub(crate) services: usize,
}

impl Children {
    /// How many in all.
    pub(crate) fn total(self) -> usize {
        self.hosts + self.services
    }

    /// `+ 18 services`, `+ 2 hosts and 18 services`; `None` without any.
    pub(crate) fn label(self) -> Option<String> {
        let plural = |count: usize, one: &str, many: &str| {
            format!("{count} {}", if count == 1 { one } else { many })
        };
        match (self.hosts, self.services) {
            (0, 0) => None,
            (0, services) => Some(format!("+ {}", plural(services, "service", "services"))),
            (hosts, 0) => Some(format!("+ {}", plural(hosts, "host", "hosts"))),
            (hosts, services) => Some(format!(
                "+ {} and {}",
                plural(hosts, "host", "hosts"),
                plural(services, "service", "services")
            )),
        }
    }
}

/// One row of a list.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Line {
    /// The downtime list's section header.
    Section {
        /// Which.
        section: Section,
        /// Its downtimes (rows of their own; children folded into a
        /// host's row count with it).
        count: usize,
    },
    /// A downtime.
    Downtime {
        /// Its name.
        key: RecordKey,
        /// The object it's set on.
        object: ObjectKey,
        /// Where it is in the snapshot's list of `object`'s downtimes.
        index: usize,
        /// The downtimes it stands for (a host's with its services).
        children: Children,
        /// It's one of those, listed under its host's (unfolded).
        child: bool,
    },
    /// A comment.
    Comment {
        /// Its name.
        key: RecordKey,
        /// The object it's on.
        object: ObjectKey,
        /// Where it is in the snapshot's list of `object`'s comments.
        index: usize,
    },
    /// An acknowledged problem.
    Problem {
        /// The object.
        key: RecordKey,
    },
}

impl Line {
    /// The host or service the row is about.
    pub(crate) fn object(&self) -> Option<&ObjectKey> {
        match self {
            Self::Downtime { object, .. }
            | Self::Comment { object, .. }
            | Self::Problem {
                key: RecordKey::Problem(object),
            } => Some(object),
            Self::Section { .. } | Self::Problem { .. } => None,
        }
    }
}

impl SelectableRow for Line {
    type Key = RecordKey;

    fn key(&self) -> Option<&RecordKey> {
        match self {
            Self::Section { .. } => None,
            Self::Downtime { key, .. } | Self::Comment { key, .. } | Self::Problem { key } => {
                Some(key)
            }
        }
    }
}

/// The summary bar's counts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ListSummary {
    /// Downtimes in effect, upcoming, and how many of the listed are from
    /// the config (`ScheduledDowntime`).
    pub(crate) in_effect: usize,
    /// Downtimes not in effect yet.
    pub(crate) upcoming: usize,
    /// Downtimes from the config.
    pub(crate) from_config: usize,
    /// Downtimes folded into their host's row.
    pub(crate) folded: usize,
    /// Comments by users.
    pub(crate) comments: usize,
    /// Comments of acknowledgements.
    pub(crate) acknowledgements: usize,
    /// Downtime and flapping comments (hidden unless asked for).
    pub(crate) system: usize,
    /// Acknowledged problems by state, worst first (only states with
    /// some).
    pub(crate) states: Vec<(CheckableState, usize)>,
    /// Sticky acknowledgements.
    pub(crate) sticky: usize,
    /// Acknowledgements that expire.
    pub(crate) expiring: usize,
    /// Left out by *only mine*.
    pub(crate) by_others: usize,
}

/// A list's rows and counts.
#[derive(Clone, Debug)]
pub(crate) struct Listing {
    /// The rows in order.
    pub(crate) rows: Arc<Vec<Line>>,
    /// The summary bar's counts.
    pub(crate) summary: ListSummary,
    /// The downtimes the rows refer to.
    pub(crate) downtimes: Arc<BTreeMap<ObjectKey, Vec<Downtime>>>,
    /// The comments the rows refer to.
    pub(crate) comments: Arc<BTreeMap<ObjectKey, Vec<Comment>>>,
}

impl Listing {
    /// The downtime of row `line`, if it is one.
    pub(crate) fn downtime(&self, line: &Line) -> Option<&Downtime> {
        match line {
            Line::Downtime { object, index, .. } => self.downtimes.get(object)?.get(*index),
            _ => None,
        }
    }

    /// The comment of row `line`, if it is one.
    pub(crate) fn comment(&self, line: &Line) -> Option<&Comment> {
        match line {
            Line::Comment { object, index, .. } => self.comments.get(object)?.get(*index),
            _ => None,
        }
    }

    /// How many rows with a key it has (not counting headers).
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.rows.iter().filter(|line| line.key().is_some()).count()
    }
}

/// The list `kind` of `snapshot`, as `options` ask; `author` is the
/// environment's author (*only mine*).
pub(crate) fn build(
    kind: ListKind,
    snapshot: &Snapshot,
    author: &str,
    options: &Options,
    now: Timestamp,
) -> Listing {
    let (rows, summary) = match kind {
        ListKind::Downtimes => downtime_rows(snapshot, author, options, now),
        ListKind::Comments => comment_rows(snapshot, author, options),
        ListKind::Acknowledged => problem_rows(snapshot, author, options),
    };
    Listing {
        rows: Arc::new(rows),
        summary,
        downtimes: Arc::clone(&snapshot.downtimes),
        comments: Arc::clone(&snapshot.comments),
    }
}

/// How many rows a list has with its default options, for the sidebar's
/// count: downtimes in effect or upcoming (a host's with its services
/// counts once), comments but downtime and flapping ones, acknowledged
/// problems.
pub(crate) fn count(kind: ListKind, snapshot: &Snapshot, now: Timestamp) -> usize {
    match kind {
        ListKind::Downtimes => {
            let names: std::collections::HashSet<&str> = snapshot
                .downtimes
                .values()
                .flatten()
                .map(|downtime| downtime.name.as_str())
                .collect();
            snapshot
                .downtimes
                .values()
                .flatten()
                .filter(|downtime| downtime.phase(now) != DowntimePhase::Over)
                .filter(|downtime| {
                    downtime
                        .parent
                        .as_deref()
                        .is_none_or(|parent| !names.contains(parent))
                })
                .count()
        }
        ListKind::Comments => snapshot
            .comments
            .values()
            .flatten()
            .filter(|comment| !is_system(comment.kind))
            .count(),
        ListKind::Acknowledged => {
            snapshot
                .hosts
                .values()
                .filter(|host| host.check.acknowledgement.is_acknowledged())
                .count()
                + snapshot
                    .services
                    .values()
                    .filter(|service| service.check.acknowledgement.is_acknowledged())
                    .count()
        }
    }
}

/// Downtime and flapping comments: Icinga's own, which the downtime list
/// and the objects' state already say.
pub(crate) fn is_system(kind: CommentKind) -> bool {
    matches!(kind, CommentKind::Downtime | CommentKind::Flapping)
}

/// Whether `who` is the environment's author (*only mine*).
fn is_mine(who: &str, author: &str) -> bool {
    !author.is_empty() && who.trim() == author
}

/// A downtime in the snapshot, by position.
#[derive(Clone, Copy)]
struct Entry<'a> {
    object: &'a ObjectKey,
    index: usize,
    downtime: &'a Downtime,
}

fn downtime_rows(
    snapshot: &Snapshot,
    author: &str,
    options: &Options,
    now: Timestamp,
) -> (Vec<Line>, ListSummary) {
    let all: Vec<Entry<'_>> = snapshot
        .downtimes
        .iter()
        .flat_map(|(object, list)| {
            list.iter().enumerate().map(move |(index, downtime)| Entry {
                object,
                index,
                downtime,
            })
        })
        .collect();
    let names: std::collections::HashSet<&str> = all
        .iter()
        .map(|entry| entry.downtime.name.as_str())
        .collect();
    // Children whose parent is listed fold into the parent's row.
    let mut children: HashMap<&str, Vec<Entry<'_>>> = HashMap::new();
    let mut top: Vec<Entry<'_>> = Vec::new();
    for entry in &all {
        match entry.downtime.parent.as_deref() {
            Some(parent) if names.contains(parent) => {
                children.entry(parent).or_default().push(*entry);
            }
            _ => top.push(*entry),
        }
    }
    let mut summary = ListSummary::default();
    top.retain(|entry| entry.downtime.phase(now) != DowntimePhase::Over);
    if options.only_mine {
        let before = top.len();
        top.retain(|entry| is_mine(&entry.downtime.author, author));
        summary.by_others = before - top.len();
    }
    let (mut in_effect, mut upcoming): (Vec<Entry<'_>>, Vec<Entry<'_>>) = top
        .into_iter()
        .partition(|entry| entry.downtime.phase(now) == DowntimePhase::InEffect);
    sort_downtimes(&mut in_effect, Section::InEffect, options.sort);
    sort_downtimes(&mut upcoming, Section::Upcoming, options.sort);
    summary.in_effect = in_effect.len();
    summary.upcoming = upcoming.len();
    let mut rows = Vec::new();
    for (section, entries) in [
        (Section::InEffect, &in_effect),
        (Section::Upcoming, &upcoming),
    ] {
        if entries.is_empty() {
            continue;
        }
        rows.push(Line::Section {
            section,
            count: entries.len(),
        });
        for entry in entries {
            let mut under = children
                .get(entry.downtime.name.as_str())
                .cloned()
                .unwrap_or_default();
            let counted = Children {
                hosts: under
                    .iter()
                    .filter(|child| matches!(child.object, ObjectKey::Host { .. }))
                    .count(),
                services: under
                    .iter()
                    .filter(|child| matches!(child.object, ObjectKey::Service { .. }))
                    .count(),
            };
            if entry.downtime.config_owned {
                summary.from_config += 1;
            }
            rows.push(downtime_line(entry, counted, false));
            if options.unfolded.contains(&entry.downtime.name) {
                under.sort_by(|a, b| object_order(a.object, b.object));
                rows.extend(
                    under
                        .iter()
                        .map(|child| downtime_line(child, Children::default(), true)),
                );
            } else {
                summary.folded += counted.total();
            }
        }
    }
    (rows, summary)
}

fn downtime_line(entry: &Entry<'_>, children: Children, child: bool) -> Line {
    Line::Downtime {
        key: RecordKey::Downtime(entry.downtime.name.clone()),
        object: entry.object.clone(),
        index: entry.index,
        children,
        child,
    }
}

/// Hosts before services; then by host, then by service name.
fn object_order(a: &ObjectKey, b: &ObjectKey) -> std::cmp::Ordering {
    a.host_name().cmp(b.host_name()).then_with(|| match (a, b) {
        (ObjectKey::Host { .. }, ObjectKey::Host { .. }) => std::cmp::Ordering::Equal,
        (ObjectKey::Host { .. }, ObjectKey::Service { .. }) => std::cmp::Ordering::Less,
        (ObjectKey::Service { .. }, ObjectKey::Host { .. }) => std::cmp::Ordering::Greater,
        (ObjectKey::Service { key: a }, ObjectKey::Service { key: b }) => a.name.cmp(&b.name),
    })
}

fn sort_downtimes(entries: &mut [Entry<'_>], section: Section, sort: SortChoice) {
    let seconds = |at: Timestamp| at.as_unix_seconds();
    // When it ends: a flexible one waiting runs at most until its
    // window's end.
    let ends = |downtime: &Downtime| seconds(downtime.effective_end().unwrap_or(downtime.end_time));
    let starts =
        |downtime: &Downtime| seconds(downtime.effective_start().unwrap_or(downtime.start_time));
    entries.sort_by(|a, b| {
        let (x, y) = (a.downtime, b.downtime);
        let order = match (sort, section) {
            (SortChoice::EndsSoonest, Section::InEffect) => ends(x).total_cmp(&ends(y)),
            (SortChoice::EndsSoonest, Section::Upcoming) => {
                seconds(x.start_time).total_cmp(&seconds(y.start_time))
            }
            (SortChoice::StartsSoonest, _) => starts(x).total_cmp(&starts(y)),
            (SortChoice::Newest, _) => seconds(y.entry_time).total_cmp(&seconds(x.entry_time)),
            (SortChoice::Oldest, _) => seconds(x.entry_time).total_cmp(&seconds(y.entry_time)),
            (SortChoice::Name, _) => object_order(a.object, b.object),
        };
        order
            .then_with(|| object_order(a.object, b.object))
            .then_with(|| x.name.cmp(&y.name))
    });
}

fn comment_rows(snapshot: &Snapshot, author: &str, options: &Options) -> (Vec<Line>, ListSummary) {
    let mut summary = ListSummary::default();
    let mut entries: Vec<(&ObjectKey, usize, &Comment)> = Vec::new();
    for (object, list) in snapshot.comments.iter() {
        for (index, comment) in list.iter().enumerate() {
            if is_system(comment.kind) {
                summary.system += 1;
                if !options.system_comments {
                    continue;
                }
            }
            if options.only_mine && !is_mine(&comment.author, author) {
                summary.by_others += 1;
                continue;
            }
            entries.push((object, index, comment));
        }
    }
    let seconds = |comment: &Comment| comment.entry_time.as_unix_seconds();
    entries.sort_by(|(a_object, _, a), (b_object, _, b)| {
        match options.sort {
            SortChoice::Oldest => seconds(a).total_cmp(&seconds(b)),
            SortChoice::Name => {
                object_order(a_object, b_object).then_with(|| seconds(b).total_cmp(&seconds(a)))
            }
            _ => seconds(b).total_cmp(&seconds(a)),
        }
        .then_with(|| a.name.cmp(&b.name))
    });
    for (_, _, comment) in &entries {
        if comment.kind == CommentKind::Acknowledgement {
            summary.acknowledgements += 1;
        } else {
            summary.comments += 1;
        }
    }
    let rows = entries
        .into_iter()
        .map(|(object, index, comment)| Line::Comment {
            key: RecordKey::Comment(comment.name.clone()),
            object: object.clone(),
            index,
        })
        .collect();
    (rows, summary)
}

/// The latest acknowledgement comment of `object`: who acknowledged it,
/// when and why.
pub(crate) fn ack_comment<'a>(snapshot: &'a Snapshot, object: &ObjectKey) -> Option<&'a Comment> {
    snapshot
        .comments
        .get(object)?
        .iter()
        .filter(|comment| comment.kind == CommentKind::Acknowledgement)
        .max_by(|a, b| {
            a.entry_time
                .as_unix_seconds()
                .total_cmp(&b.entry_time.as_unix_seconds())
        })
}

/// An acknowledged object's state and acknowledgement.
fn acknowledged(snapshot: &Snapshot) -> Vec<(ObjectKey, CheckableState, &CheckInfo)> {
    let hosts = snapshot
        .hosts
        .values()
        .filter(|host| host.check.acknowledgement.is_acknowledged())
        .map(|host| (host.key(), CheckableState::Host(host.state), &host.check));
    let services = snapshot
        .services
        .values()
        .filter(|service| service.check.acknowledgement.is_acknowledged())
        .map(|service| {
            (
                service.object_key(),
                CheckableState::Service(service.state),
                &service.check,
            )
        });
    hosts.chain(services).collect()
}

fn problem_rows(snapshot: &Snapshot, author: &str, options: &Options) -> (Vec<Line>, ListSummary) {
    let mut summary = ListSummary::default();
    let mut entries: Vec<(ObjectKey, Option<&Comment>)> = Vec::new();
    let mut states: BTreeMap<u8, (CheckableState, usize)> = BTreeMap::new();
    for (object, state, check) in acknowledged(snapshot) {
        let comment = ack_comment(snapshot, &object);
        if options.only_mine && !comment.is_some_and(|comment| is_mine(&comment.author, author)) {
            summary.by_others += 1;
            continue;
        }
        states.entry(state_rank(state)).or_insert((state, 0)).1 += 1;
        if check.acknowledgement == AckKind::Sticky {
            summary.sticky += 1;
        }
        if check.acknowledgement_expiry.is_some() {
            summary.expiring += 1;
        }
        entries.push((object, comment));
    }
    summary.states = states.into_values().collect();
    let seconds = |comment: Option<&Comment>| {
        comment.map_or(f64::NEG_INFINITY, |comment| {
            comment.entry_time.as_unix_seconds()
        })
    };
    entries.sort_by(|(a_object, a), (b_object, b)| {
        match options.sort {
            SortChoice::Oldest => seconds(*a).total_cmp(&seconds(*b)),
            SortChoice::Name => object_order(a_object, b_object),
            _ => seconds(*b).total_cmp(&seconds(*a)),
        }
        .then_with(|| object_order(a_object, b_object))
    });
    let rows = entries
        .into_iter()
        .map(|(object, _)| Line::Problem {
            key: RecordKey::Problem(object),
        })
        .collect();
    (rows, summary)
}

/// Worst first: critical, down, warning, unreachable, unknown, then the
/// rest.
fn state_rank(state: CheckableState) -> u8 {
    match state {
        CheckableState::Service(ServiceState::Critical) => 0,
        CheckableState::Host(HostState::Down) => 1,
        CheckableState::Service(ServiceState::Warning) => 2,
        CheckableState::Host(HostState::Unreachable) => 3,
        CheckableState::Service(ServiceState::Unknown) => 4,
        CheckableState::Service(ServiceState::Ok) | CheckableState::Host(HostState::Up) => 5,
        CheckableState::Service(ServiceState::Pending)
        | CheckableState::Host(HostState::Pending) => 6,
    }
}

/// What a comment row says about its kind, in the tag's first slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KindSlot {
    /// `✓ acknowledgement`.
    Acknowledgement,
    /// `comment`.
    Comment,
    /// `downtime`.
    Downtime,
    /// `flapping`.
    Flapping,
}

impl KindSlot {
    /// The word.
    pub(crate) fn word(self) -> &'static str {
        match self {
            Self::Acknowledgement => "acknowledgement",
            Self::Comment => "comment",
            Self::Downtime => "downtime",
            Self::Flapping => "flapping",
        }
    }

    /// Its icon.
    pub(crate) fn icon(self) -> IconName {
        match self {
            Self::Acknowledgement => IconName::Check,
            Self::Comment => IconName::MessageSquare,
            Self::Downtime => IconName::CalendarClock,
            Self::Flapping => IconName::ArrowLeftRight,
        }
    }

    /// The longest word, which the slot is sized for.
    pub(crate) const LONGEST: &'static str = "acknowledgement";
}

/// A row's tag, in its fixed slots.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Tag {
    /// A downtime: its progress line (`None`: a lock, from the config) and
    /// its time, in the accent while it's in effect.
    Downtime {
        /// How much has passed, 0 to 1.
        progress: Option<f32>,
        /// `1h 48m left`, `in 7h 48m`, `not started`, `from config`.
        text: String,
        /// In effect: the text in the accent.
        accent: bool,
    },
    /// A comment: its kind and a detail (`sticky`, `expires 16:00`).
    Comment {
        /// The kind.
        kind: KindSlot,
        /// The detail, or empty.
        detail: String,
    },
    /// An acknowledgement: sticky or not, and its expiry.
    Ack {
        /// Sticky.
        sticky: bool,
        /// `expires Thu 08:00`, `no expiry`.
        expiry: String,
    },
}

/// The words of a row.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RowText {
    /// The object's state, for the circle.
    pub(crate) state: Option<CheckableState>,
    /// A hollow circle: handled (a downtime row: this downtime in effect).
    pub(crate) hollow: bool,
    /// Under the circle: a service's time in state, `host` for hosts.
    pub(crate) caption: String,
    /// The service's or host's name.
    pub(crate) name: String,
    /// The host, for services (`on db-prod-03`).
    pub(crate) host: Option<String>,
    /// `+ 18 services`.
    pub(crate) more: Option<String>,
    /// The second line.
    pub(crate) line: String,
    /// The tag.
    pub(crate) tag: Tag,
    /// From the config: can't be removed.
    pub(crate) config: bool,
    /// An unfolded child of a host's downtime: its line says only that it
    /// goes with its host, and its tag is faint (the parent's row above
    /// carries the comment and the time left).
    pub(crate) child: bool,
}

/// The words of `line`, in the local time zone.
pub(crate) fn row_text(
    listing: &Listing,
    line: &Line,
    snapshot: &Snapshot,
    times: ListTimes,
    now: Timestamp,
) -> Option<RowText> {
    row_text_in(listing, line, snapshot, times, now, &Local)
}

/// [`row_text`] in time zone `zone`.
pub(crate) fn row_text_in<Tz>(
    listing: &Listing,
    line: &Line,
    snapshot: &Snapshot,
    times: ListTimes,
    now: Timestamp,
    zone: &Tz,
) -> Option<RowText>
where
    Tz: TimeZone,
    Tz::Offset: Display,
{
    let object = line.object()?;
    let (state, mark, caption, name, host) = object_facts(snapshot, object, times, now);
    match line {
        Line::Section { .. } => None,
        Line::Downtime {
            children, child, ..
        } => {
            let downtime = listing.downtime(line)?;
            let phase = downtime.phase(now);
            let mut tag = downtime_tag(downtime, phase, now, zone);
            let detail = if *child {
                if let Tag::Downtime { accent, .. } = &mut tag {
                    *accent = false;
                }
                format!(
                    "with its host · {}",
                    compact_window(downtime.start_time, downtime.end_time, now, zone)
                )
            } else {
                downtime_detail(snapshot, downtime, now, zone)
            };
            Some(RowText {
                state,
                // Hollow while this downtime is in effect, whatever the
                // state; one not in effect yet keeps the filled circle.
                hollow: phase == DowntimePhase::InEffect,
                caption,
                name,
                host,
                more: children.label(),
                line: detail,
                tag,
                config: downtime.config_owned,
                child: *child,
            })
        }
        Line::Comment { .. } => {
            let comment = listing.comment(line)?;
            Some(RowText {
                state,
                hollow: mark,
                caption,
                name,
                host,
                more: None,
                line: format!(
                    "{} {} · {}",
                    comment.author,
                    day_clock(comment.entry_time, now, zone),
                    first_line(&comment.text)
                ),
                tag: comment_tag(snapshot, comment, now, zone),
                config: false,
                child: false,
            })
        }
        Line::Problem { .. } => {
            let check = check_of(snapshot, object)?;
            let comment = ack_comment(snapshot, object);
            let line = comment.map_or_else(
                || "acknowledged; its comment hasn't arrived".to_owned(),
                |comment| {
                    format!(
                        "{} {}, {} ago · {}",
                        comment.author,
                        day_clock(comment.entry_time, now, zone),
                        ago(comment.entry_time.elapsed_until(now)),
                        first_line(&comment.text)
                    )
                },
            );
            Some(RowText {
                state,
                // Acknowledged means handled.
                hollow: true,
                caption,
                name,
                host,
                more: None,
                line,
                tag: Tag::Ack {
                    sticky: check.acknowledgement == AckKind::Sticky,
                    expiry: check.acknowledgement_expiry.map_or_else(
                        || "no expiry".to_owned(),
                        |at| format!("expires {}", short_when(at, now, zone)),
                    ),
                },
                config: false,
                child: false,
            })
        }
    }
}

/// An object's state, whether its mark is hollow, the caption under its
/// circle, its name and its host.
fn object_facts(
    snapshot: &Snapshot,
    object: &ObjectKey,
    times: ListTimes,
    now: Timestamp,
) -> (Option<CheckableState>, bool, String, String, Option<String>) {
    match object {
        ObjectKey::Host { name } => match snapshot.hosts.get(name) {
            Some(host) => {
                let mark = ObjectMark::host(host);
                (
                    Some(mark.state),
                    mark.hollow,
                    "host".to_owned(),
                    host.display_name.clone(),
                    None,
                )
            }
            None => (None, false, "host".to_owned(), name.to_string(), None),
        },
        ObjectKey::Service { key } => {
            let host = snapshot
                .host_of(key)
                .map_or_else(|| key.host.to_string(), |host| host.display_name.clone());
            match snapshot.services.get(key) {
                Some(service) => {
                    let mark =
                        ObjectMark::service(service, snapshot.host_of(key).map(AsRef::as_ref));
                    let caption = match times {
                        ListTimes::Relative => format::time_in_state(&service.check, now),
                        ListTimes::Clock => format::state_clock(&service.check, now),
                    };
                    (
                        Some(mark.state),
                        mark.hollow,
                        caption,
                        service.display_name.clone(),
                        Some(host),
                    )
                }
                None => (None, false, String::new(), key.name.to_string(), Some(host)),
            }
        }
    }
}

fn check_of<'a>(snapshot: &'a Snapshot, object: &ObjectKey) -> Option<&'a CheckInfo> {
    match object {
        ObjectKey::Host { name } => snapshot.hosts.get(name).map(|host| &host.check),
        ObjectKey::Service { key } => snapshot.services.get(key).map(|service| &service.check),
    }
}

/// A downtime's second line: `fixed · 13:30 → 15:30 · m.keller: drained
/// for the kernel 6.8 rollout`.
fn downtime_detail<Tz>(
    snapshot: &Snapshot,
    downtime: &Downtime,
    now: Timestamp,
    zone: &Tz,
) -> String
where
    Tz: TimeZone,
    Tz::Offset: Display,
{
    let window = compact_window(downtime.start_time, downtime.end_time, now, zone);
    let mut parts = Vec::new();
    if downtime.fixed {
        parts.push("fixed".to_owned());
        parts.push(window);
    } else {
        parts.push(format!("flexible {}", length(downtime.duration)));
        if let Some(trigger) = downtime.trigger_time {
            parts.push(format!("started {}", day_clock(trigger, now, zone)));
        }
        parts.push(format!("window {window}"));
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
    if downtime.config_owned {
        parts.push(match &downtime.schedule {
            Some(schedule) => format!("from config: {schedule}"),
            None => format!("from config: {}", first_line(&downtime.comment)),
        });
    } else {
        parts.push(format!(
            "{}: {}",
            downtime.author,
            first_line(&downtime.comment)
        ));
    }
    parts.join(" · ")
}

/// A downtime's tag.
fn downtime_tag<Tz>(downtime: &Downtime, phase: DowntimePhase, now: Timestamp, zone: &Tz) -> Tag
where
    Tz: TimeZone,
    Tz::Offset: Display,
{
    let progress = (!downtime.config_owned).then(|| {
        if phase == DowntimePhase::InEffect {
            downtime.progress(now)
        } else {
            0.0
        }
    });
    let (text, accent) = match phase {
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
            true,
        ),
        _ if downtime.config_owned => ("from config".to_owned(), false),
        DowntimePhase::Waiting => ("not started".to_owned(), false),
        DowntimePhase::Upcoming if !downtime.fixed => (
            format!("by {}", short_when(downtime.start_time, now, zone)),
            false,
        ),
        DowntimePhase::Upcoming => (
            format!(
                "in {}",
                downtimes::left(downtime.start_time.remaining_from(now))
            ),
            false,
        ),
        DowntimePhase::Over => ("over".to_owned(), false),
    };
    Tag::Downtime {
        progress,
        text,
        accent,
    }
}

/// A comment's tag: its kind, and sticky (an acknowledgement's) or when it
/// expires.
fn comment_tag<Tz>(snapshot: &Snapshot, comment: &Comment, now: Timestamp, zone: &Tz) -> Tag
where
    Tz: TimeZone,
    Tz::Offset: Display,
{
    let kind = match comment.kind {
        CommentKind::Acknowledgement => KindSlot::Acknowledgement,
        CommentKind::User => KindSlot::Comment,
        CommentKind::Downtime => KindSlot::Downtime,
        CommentKind::Flapping => KindSlot::Flapping,
    };
    let check = check_of(snapshot, &comment.object);
    let expiry = comment.expire_time.or_else(|| {
        (comment.kind == CommentKind::Acknowledgement)
            .then(|| check.and_then(|check| check.acknowledgement_expiry))
            .flatten()
    });
    let sticky = comment.kind == CommentKind::Acknowledgement
        && check.is_some_and(|check| check.acknowledgement == AckKind::Sticky);
    let detail = if sticky {
        "sticky".to_owned()
    } else {
        expiry.map_or_else(String::new, |at| {
            format!("expires {}", short_when(at, now, zone))
        })
    };
    Tag::Comment { kind, detail }
}

/// A comment's first line (the row shows one).
fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or_default().trim()
}

/// A flexible downtime's length: `2h`, `1h 30m`.
fn length(seconds: f64) -> String {
    format_two_units(Duration::try_from_secs_f64(seconds.max(0.0)).unwrap_or_default())
}

/// How long ago, for an acknowledgement: `42m`, `1h 42m`, `2d`.
pub(crate) fn ago(elapsed: Duration) -> String {
    let minutes = elapsed.as_secs() / 60;
    match (minutes / 1_440, minutes / 60, minutes % 60) {
        (0, 0, 0) => "less than a minute".to_owned(),
        (0, 0, minutes) => format!("{minutes}m"),
        (0, hours, 0) => format!("{hours}h"),
        (0, hours, minutes) => format!("{hours}h {minutes}m"),
        (days, _, _) => format!("{days}d"),
    }
}

/// A local time with its day when it isn't today: `13:58`, `Mon 09:14`
/// (within a week either way), `3 Oct 09:14`.
pub(crate) fn day_clock<Tz>(at: Timestamp, now: Timestamp, zone: &Tz) -> String
where
    Tz: TimeZone,
    Tz::Offset: Display,
{
    let (Some(at), Some(today)) = (format::date_time(at, zone), format::date_time(now, zone))
    else {
        return "—".to_owned();
    };
    match at
        .date_naive()
        .signed_duration_since(today.date_naive())
        .num_days()
    {
        0 => at.format("%H:%M").to_string(),
        -6..=6 => at.format("%a %H:%M").to_string(),
        _ => at.format("%-d %b %H:%M").to_string(),
    }
}

/// A time short enough for a tag's slot: `15:00`, `Thu 08:00` within a
/// week, `10 Oct` further.
pub(crate) fn short_when<Tz>(at: Timestamp, now: Timestamp, zone: &Tz) -> String
where
    Tz: TimeZone,
    Tz::Offset: Display,
{
    let (Some(at), Some(today)) = (format::date_time(at, zone), format::date_time(now, zone))
    else {
        return "—".to_owned();
    };
    match at
        .date_naive()
        .signed_duration_since(today.date_naive())
        .num_days()
    {
        0 => at.format("%H:%M").to_string(),
        -6..=6 => at.format("%a %H:%M").to_string(),
        _ => at.format("%-d %b").to_string(),
    }
}

/// A downtime's window in a row: `14:00 → 15:00`, `Thu 01:00 → 03:00`,
/// `22:00 → 06:00` (overnight), `Fri 22:00 → Sun 06:00`.
pub(crate) fn compact_window<Tz>(
    start: Timestamp,
    end: Timestamp,
    now: Timestamp,
    zone: &Tz,
) -> String
where
    Tz: TimeZone,
    Tz::Offset: Display,
{
    let from = day_clock(start, now, zone);
    let (Some(start_at), Some(end_at)) =
        (format::date_time(start, zone), format::date_time(end, zone))
    else {
        return from;
    };
    let days = end_at
        .date_naive()
        .signed_duration_since(start_at.date_naive())
        .num_days();
    let overnight = days == 1 && end.as_unix_seconds() - start.as_unix_seconds() < 86_400.0;
    let to = if days == 0 || overnight {
        end_at.format("%H:%M").to_string()
    } else {
        day_clock(end, now, zone)
    };
    format!("{from} → {to}")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::Utc;
    use ic_model::{Host, Service};

    use super::*;

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
            entry_time: at(start - 30.),
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
            text: format!("{name} text\nsecond line"),
            kind,
            entry_time: at(-ago),
            expire_time: None,
            persistent: false,
        }
    }

    struct World {
        snapshot: Snapshot,
    }

    impl World {
        fn new() -> Self {
            let mut hosts = BTreeMap::new();
            let mut services = BTreeMap::new();
            for name in [
                "k8s-node-07",
                "cache-02",
                "db-prod-05",
                "lb-prod-02",
                "sw-core-ams-02",
            ] {
                let mut host = Host::new(name);
                host.state = HostState::Up;
                hosts.insert(host.name.clone(), Arc::new(host));
            }
            for (host, name, state) in [
                ("k8s-node-07", "disk /var", ServiceState::Critical),
                ("k8s-node-07", "kubelet", ServiceState::Critical),
                ("k8s-node-07", "load", ServiceState::Ok),
                ("cache-02", "redis-memory", ServiceState::Warning),
                ("db-prod-05", "pg-locks", ServiceState::Ok),
                ("lb-prod-02", "haproxy-backend", ServiceState::Warning),
            ] {
                let mut service = Service::new(host, name);
                service.state = state;
                service.check.last_state_change = at(-180.);
                services.insert(service.key.clone(), Arc::new(service));
            }
            Self {
                snapshot: Snapshot {
                    hosts: Arc::new(hosts),
                    services: Arc::new(services),
                    ..Snapshot::default()
                },
            }
        }

        fn downtimes(mut self, list: Vec<Downtime>) -> Self {
            let mut map: BTreeMap<ObjectKey, Vec<Downtime>> = BTreeMap::new();
            for downtime in list {
                map.entry(downtime.object.clone())
                    .or_default()
                    .push(downtime);
            }
            // The objects in a downtime in effect are in downtime.
            let mut services = (*self.snapshot.services).clone();
            let mut hosts = (*self.snapshot.hosts).clone();
            for list in map.values() {
                for downtime in list.iter().filter(|downtime| downtime.in_effect) {
                    match &downtime.object {
                        ObjectKey::Service { key } => {
                            if let Some(service) = services.get_mut(key) {
                                Arc::make_mut(service).check.downtime_depth += 1;
                            }
                        }
                        ObjectKey::Host { name } => {
                            if let Some(host) = hosts.get_mut(name) {
                                Arc::make_mut(host).check.downtime_depth += 1;
                            }
                        }
                    }
                }
            }
            self.snapshot.services = Arc::new(services);
            self.snapshot.hosts = Arc::new(hosts);
            self.snapshot.downtimes = Arc::new(map);
            self
        }

        fn comments(mut self, list: Vec<Comment>) -> Self {
            let mut map: BTreeMap<ObjectKey, Vec<Comment>> = BTreeMap::new();
            for comment in list {
                map.entry(comment.object.clone()).or_default().push(comment);
            }
            self.snapshot.comments = Arc::new(map);
            self
        }

        fn acknowledge(mut self, object: &ObjectKey, kind: AckKind, expiry: Option<f64>) -> Self {
            let mut services = (*self.snapshot.services).clone();
            if let ObjectKey::Service { key } = object {
                let service = Arc::make_mut(services.get_mut(key).unwrap());
                service.check.acknowledgement = kind;
                service.check.acknowledgement_expiry = expiry.map(at);
            }
            self.snapshot.services = Arc::new(services);
            self
        }
    }

    fn texts(listing: &Listing, snapshot: &Snapshot) -> Vec<RowText> {
        listing
            .rows
            .iter()
            .filter_map(|line| {
                row_text_in(listing, line, snapshot, ListTimes::Relative, now(), &Utc)
            })
            .collect()
    }

    fn the_mockups_downtimes() -> World {
        let node = ObjectKey::host("k8s-node-07");
        let mut host = downtime(&node, "drain", "m.keller", -42., 78.);
        host.comment = "drained for the kernel 6.8 rollout".to_owned();
        let mut children: Vec<Downtime> = ["disk /var", "kubelet", "load"]
            .iter()
            .map(|service| {
                let mut child = downtime(
                    &ObjectKey::service("k8s-node-07", service),
                    "drain",
                    "m.keller",
                    -42.,
                    78.,
                );
                child.parent = Some(host.name.clone());
                child
            })
            .collect();
        let redis = downtime(
            &ObjectKey::service("cache-02", "redis-memory"),
            "memory",
            "j.berg",
            -117.,
            123.,
        );
        let mut locks = downtime(
            &ObjectKey::service("db-prod-05", "pg-locks"),
            "vacuum",
            "dba-oncall",
            -12.,
            228.,
        );
        locks.fixed = false;
        locks.duration = 7_200.;
        locks.in_effect = false;
        let haproxy = downtime(
            &ObjectKey::service("lb-prod-02", "haproxy-backend"),
            "rollout",
            "m.keller",
            468.,
            528.,
        );
        let mut weekly = downtime(
            &ObjectKey::host("sw-core-ams-02"),
            "weekly",
            "icingaadmin",
            3_828.,
            4_068.,
        );
        weekly.config_owned = true;
        weekly.schedule = Some("weekly-patching".to_owned());
        let over = downtime(&ObjectKey::host("lb-prod-02"), "over", "j.berg", -120., -1.);
        let mut list = vec![host, redis, locks, haproxy, weekly, over];
        list.append(&mut children);
        World::new().downtimes(list)
    }

    #[test]
    fn downtimes_come_in_two_sections_with_children_folded() {
        let world = the_mockups_downtimes();
        let options = Options::new(ListKind::Downtimes);
        let listing = build(
            ListKind::Downtimes,
            &world.snapshot,
            "j.berg",
            &options,
            now(),
        );
        let shape: Vec<String> = listing
            .rows
            .iter()
            .map(|line| match line {
                Line::Section { section, count } => section.title(*count),
                other => other.object().unwrap().full_name(),
            })
            .collect();
        assert_eq!(
            shape,
            [
                "in effect · 2",
                "k8s-node-07",
                "cache-02!redis-memory",
                "upcoming · 3",
                "db-prod-05!pg-locks",
                "lb-prod-02!haproxy-backend",
                "sw-core-ams-02",
            ],
            "ending soonest, then starting soonest; the ended one is left out"
        );
        assert_eq!(listing.summary.in_effect, 2);
        assert_eq!(listing.summary.upcoming, 3);
        assert_eq!(listing.summary.from_config, 1);
        assert_eq!(listing.summary.folded, 3);
        assert_eq!(count(ListKind::Downtimes, &world.snapshot, now()), 5);
        let Line::Downtime { children, .. } = &listing.rows[1] else {
            panic!("a downtime row");
        };
        assert_eq!(children.label().as_deref(), Some("+ 3 services"));
    }

    #[test]
    fn downtime_rows_read_as_drawn() {
        let world = the_mockups_downtimes();
        let options = Options::new(ListKind::Downtimes);
        let listing = build(
            ListKind::Downtimes,
            &world.snapshot,
            "j.berg",
            &options,
            now(),
        );
        let rows = texts(&listing, &world.snapshot);
        let node = &rows[0];
        assert_eq!(node.caption, "host");
        assert!(node.hollow, "in effect: hollow, an OK host too");
        assert_eq!(node.more.as_deref(), Some("+ 3 services"));
        assert_eq!(
            node.line,
            "fixed · 13:30 → 15:30 · m.keller: drained for the kernel 6.8 rollout"
        );
        assert_eq!(
            node.tag,
            Tag::Downtime {
                progress: Some(0.35),
                text: "1h 18m left".to_owned(),
                accent: true
            }
        );
        let redis = &rows[1];
        assert_eq!(
            (redis.name.as_str(), redis.host.as_deref()),
            ("redis-memory", Some("cache-02"))
        );
        assert_eq!(redis.caption, "3h");
        let locks = &rows[2];
        assert!(!locks.hollow, "not in effect yet: filled");
        assert_eq!(
            locks.line,
            "flexible 2h · window 14:00 → 18:00 · dba-oncall: vacuum work"
        );
        assert!(
            matches!(&locks.tag, Tag::Downtime { text, accent: false, progress: Some(_) } if text == "not started")
        );
        let haproxy = &rows[3];
        assert!(matches!(&haproxy.tag, Tag::Downtime { text, .. } if text == "in 7h 48m"));
        assert_eq!(
            haproxy.line,
            "fixed · 22:00 → 23:00 · m.keller: rollout work"
        );
        let weekly = &rows[4];
        assert!(weekly.config);
        assert_eq!(
            weekly.line,
            "fixed · Sat 06:00 → 10:00 · from config: weekly-patching"
        );
        assert_eq!(
            weekly.tag,
            Tag::Downtime {
                progress: None,
                text: "from config".to_owned(),
                accent: false
            }
        );
    }

    #[test]
    fn a_host_downtime_unfolds_its_services_under_it() {
        let world = the_mockups_downtimes();
        let mut options = Options::new(ListKind::Downtimes);
        options.unfolded.insert("k8s-node-07!drain".to_owned());
        let listing = build(ListKind::Downtimes, &world.snapshot, "", &options, now());
        let children: Vec<String> = listing
            .rows
            .iter()
            .filter(|line| matches!(line, Line::Downtime { child: true, .. }))
            .map(|line| line.object().unwrap().full_name())
            .collect();
        assert_eq!(
            children,
            [
                "k8s-node-07!disk /var",
                "k8s-node-07!kubelet",
                "k8s-node-07!load"
            ]
        );
        assert_eq!(
            listing.rows[2].object().unwrap().full_name(),
            "k8s-node-07!disk /var"
        );
        assert_eq!(listing.summary.folded, 0);
        assert_eq!(
            listing.summary.in_effect, 2,
            "sections count their own rows"
        );
        // A child says it goes with its host; the comment and the time left
        // are on the host's row above it.
        let child = row_text_in(
            &listing,
            &listing.rows[2],
            &world.snapshot,
            ListTimes::Relative,
            now(),
            &Utc,
        )
        .unwrap();
        assert!(child.child);
        assert!(child.line.starts_with("with its host · "), "{}", child.line);
        assert!(matches!(child.tag, Tag::Downtime { accent: false, .. }));
        let parent = row_text_in(
            &listing,
            &listing.rows[1],
            &world.snapshot,
            ListTimes::Relative,
            now(),
            &Utc,
        )
        .unwrap();
        assert!(!parent.child && !parent.line.starts_with("with its host"));
    }

    #[test]
    fn only_mine_keeps_the_authors_and_counts_the_rest() {
        let world = the_mockups_downtimes();
        let mut options = Options::new(ListKind::Downtimes);
        options.only_mine = true;
        let listing = build(
            ListKind::Downtimes,
            &world.snapshot,
            "j.berg",
            &options,
            now(),
        );
        let objects: Vec<String> = listing
            .rows
            .iter()
            .filter_map(Line::object)
            .map(ObjectKey::full_name)
            .collect();
        assert_eq!(objects, ["cache-02!redis-memory"]);
        assert_eq!(listing.summary.by_others, 4);
        // Without an author, nothing is anyone's.
        let none = build(ListKind::Downtimes, &world.snapshot, "", &options, now());
        assert!(none.rows.is_empty());
    }

    #[test]
    fn other_sorts_keep_the_sections() {
        let world = the_mockups_downtimes();
        let mut options = Options::new(ListKind::Downtimes);
        options.sort = SortChoice::Name;
        let listing = build(ListKind::Downtimes, &world.snapshot, "", &options, now());
        let upcoming: Vec<String> = listing.rows[4..]
            .iter()
            .filter_map(Line::object)
            .map(ObjectKey::full_name)
            .collect();
        assert_eq!(
            upcoming,
            [
                "db-prod-05!pg-locks",
                "lb-prod-02!haproxy-backend",
                "sw-core-ams-02"
            ]
        );
        assert_eq!(
            Section::Upcoming.detail(SortChoice::Name),
            "not in effect yet · by name"
        );
        assert_eq!(
            Section::InEffect.detail(SortChoice::EndsSoonest),
            "handled now · ending soonest first"
        );
        assert_eq!(
            Section::Upcoming.detail(SortChoice::EndsSoonest),
            "not in effect yet · starting soonest first"
        );
    }

    #[test]
    fn comments_list_kinds_and_leave_out_downtime_and_flapping_ones() {
        let tls = ObjectKey::service("cache-02", "redis-memory");
        let pg = ObjectKey::service("db-prod-05", "pg-locks");
        let world = World::new()
            .acknowledge(&tls, AckKind::Sticky, Some(60. * 22.))
            .comments(vec![
                comment(&tls, "ack", "m.keller", CommentKind::Acknowledgement, 130.),
                {
                    let mut user = comment(&pg, "note", "j.berg", CommentKind::User, 31.);
                    user.expire_time = Some(at(108.));
                    user
                },
                comment(&pg, "dt", "m.keller", CommentKind::Downtime, 10.),
                comment(&pg, "flap", "icinga", CommentKind::Flapping, 5.),
            ]);
        let options = Options::new(ListKind::Comments);
        let listing = build(
            ListKind::Comments,
            &world.snapshot,
            "j.berg",
            &options,
            now(),
        );
        assert_eq!(listing.len(), 2);
        assert_eq!(listing.summary.comments, 1);
        assert_eq!(listing.summary.acknowledgements, 1);
        assert_eq!(listing.summary.system, 2);
        assert_eq!(count(ListKind::Comments, &world.snapshot, now()), 2);
        let rows = texts(&listing, &world.snapshot);
        assert_eq!(
            rows[0].line, "j.berg 13:41 · note text",
            "newest first, first line"
        );
        assert_eq!(
            rows[0].tag,
            Tag::Comment {
                kind: KindSlot::Comment,
                detail: "expires 16:00".to_owned()
            }
        );
        assert_eq!(
            rows[1].tag,
            Tag::Comment {
                kind: KindSlot::Acknowledgement,
                detail: "sticky".to_owned()
            }
        );
        assert!(rows[1].hollow, "the object's mark: acknowledged");
        // Asked for, downtime and flapping comments show too.
        let mut options = options;
        options.system_comments = true;
        let all = build(
            ListKind::Comments,
            &world.snapshot,
            "j.berg",
            &options,
            now(),
        );
        assert_eq!(all.len(), 4);
        let kinds: Vec<KindSlot> = texts(&all, &world.snapshot)
            .into_iter()
            .map(|text| match text.tag {
                Tag::Comment { kind, .. } => kind,
                _ => panic!("comment tag"),
            })
            .collect();
        assert_eq!(
            kinds,
            [
                KindSlot::Flapping,
                KindSlot::Downtime,
                KindSlot::Comment,
                KindSlot::Acknowledgement
            ]
        );
    }

    #[test]
    fn acknowledged_problems_say_who_when_and_until_when() {
        let redis = ObjectKey::service("cache-02", "redis-memory");
        let haproxy = ObjectKey::service("lb-prod-02", "haproxy-backend");
        let disk = ObjectKey::service("k8s-node-07", "disk /var");
        let world = World::new()
            .acknowledge(&redis, AckKind::Sticky, Some(60. * 18.))
            .acknowledge(&haproxy, AckKind::Normal, None)
            .acknowledge(&disk, AckKind::Normal, Some(48.))
            .comments(vec![
                comment(&redis, "a", "dba-oncall", CommentKind::Acknowledgement, 42.),
                comment(
                    &haproxy,
                    "b",
                    "m.keller",
                    CommentKind::Acknowledgement,
                    102.,
                ),
                comment(&disk, "c", "j.berg", CommentKind::Acknowledgement, 1_500.),
            ]);
        let options = Options::new(ListKind::Acknowledged);
        let listing = build(
            ListKind::Acknowledged,
            &world.snapshot,
            "j.berg",
            &options,
            now(),
        );
        let rows = texts(&listing, &world.snapshot);
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|row| row.hollow), "acknowledged: hollow");
        assert_eq!(rows[0].line, "dba-oncall 13:30, 42m ago · a text");
        assert_eq!(
            rows[0].tag,
            Tag::Ack {
                sticky: true,
                expiry: "expires Thu 08:12".to_owned()
            }
        );
        assert_eq!(rows[1].line, "m.keller 12:30, 1h 42m ago · b text");
        assert_eq!(
            rows[1].tag,
            Tag::Ack {
                sticky: false,
                expiry: "no expiry".to_owned()
            }
        );
        assert_eq!(rows[2].line, "j.berg Tue 13:12, 1d ago · c text");
        assert_eq!(listing.summary.sticky, 1);
        assert_eq!(listing.summary.expiring, 2);
        assert_eq!(
            listing.summary.states,
            [
                (CheckableState::Service(ServiceState::Critical), 1),
                (CheckableState::Service(ServiceState::Warning), 2)
            ]
        );
        let mut mine = options;
        mine.only_mine = true;
        let listing = build(
            ListKind::Acknowledged,
            &world.snapshot,
            "j.berg",
            &mine,
            now(),
        );
        assert_eq!(listing.len(), 1);
        assert_eq!(listing.summary.by_others, 2);
    }

    #[test]
    fn times_read_short() {
        assert_eq!(day_clock(at(-60. * 26.), now(), &Utc), "Tue 12:12");
        assert_eq!(day_clock(at(60. * 24. * 9.), now(), &Utc), "16 Oct 14:12");
        assert_eq!(short_when(at(60. * 24. * 9.), now(), &Utc), "16 Oct");
        assert_eq!(
            compact_window(at(468.), at(948.), now(), &Utc),
            "22:00 → 06:00"
        );
        assert_eq!(
            compact_window(at(60. * 24.), at(60. * 50.), now(), &Utc),
            "Thu 14:12 → Fri 16:12"
        );
        assert_eq!(ago(Duration::from_secs(30)), "less than a minute");
        assert_eq!(ago(Duration::from_hours(1)), "1h");
        assert_eq!(ago(Duration::from_hours(72)), "3d");
        assert_eq!(
            Children {
                hosts: 2,
                services: 1
            }
            .label()
            .as_deref(),
            Some("+ 2 hosts and 1 service")
        );
        assert_eq!(Children::default().label(), None);
    }

    #[test]
    fn kinds_have_ids_titles_and_sorts() {
        for kind in ListKind::ALL {
            assert_eq!(ListKind::from_id(kind.id()), Some(kind));
            assert_eq!(kind.sorts()[0], kind.default_sort());
        }
        assert_eq!(ListKind::from_id("nonsense"), None);
        assert_eq!(SortChoice::EndsSoonest.label(), "ends soonest ↑");
        assert_eq!(SortChoice::Newest.label(), "newest ↓");
        assert_eq!(ListKind::Acknowledged.every(), "every problem");
    }

    #[test]
    fn a_large_installation_builds_quickly() {
        // 2 000 hosts with a downtime each and 15 children (30 000 rows
        // folded into 2 000).
        let mut list = Vec::new();
        for host in 0..2_000 {
            let object = ObjectKey::host(&format!("h{host:04}"));
            let parent = downtime(&object, "d", "a", -10., 50. + f64::from(host));
            for service in 0..15 {
                let mut child = downtime(
                    &ObjectKey::service(&format!("h{host:04}"), &format!("s{service}")),
                    "d",
                    "a",
                    -10.,
                    50. + f64::from(host),
                );
                child.parent = Some(parent.name.clone());
                list.push(child);
            }
            list.push(parent);
        }
        let world = World::new().downtimes(list);
        let started = std::time::Instant::now();
        let listing = build(
            ListKind::Downtimes,
            &world.snapshot,
            "",
            &Options::new(ListKind::Downtimes),
            now(),
        );
        assert_eq!(listing.len(), 2_000);
        assert_eq!(listing.summary.folded, 30_000);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
    }
}
