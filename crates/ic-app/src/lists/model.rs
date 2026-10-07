//! The two views of topic 14 and their choices: **handling** (who is
//! handling what: one thread per object with its acknowledgement, its
//! downtimes and its free-standing comments) and **downtimes** (in effect
//! and upcoming, as a timeline or a list). What they show is built in
//! [`super::threads`]; this module holds what the user chooses (the chip,
//! the sort, *only mine*, the timeline or the list), how those are kept
//! between runs, and the words for times shared by the views, the panes'
//! thread and the removal dialogs.
//!
//! Sorting follows the question (README, topic 14): handling opens on
//! *latest activity*; its *acknowledged* chip on *expires soonest* (with
//! the sections *expires within 2 hours*, *expires later*, *no expiry*);
//! *in downtime* on *ends soonest* and *upcoming* on *starts soonest*. The
//! downtimes list opens on *ends, then starts soonest* (in effect by when
//! they end, upcoming by when they start), the timeline *by time*. A chosen sort
//! stays until another chip or mode is picked; the header always shows the
//! sort in effect.

use std::fmt::Display;
use std::time::Duration;

use chrono::TimeZone;
use ic_core::snapshot::Snapshot;
use ic_model::{Comment, CommentKind, ObjectKey, Timestamp, format_two_units};
use ic_ui_kit::IconName;

use crate::format;

/// One of the two views.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum ListKind {
    /// Who is handling what: acknowledgements, downtimes and comments, a
    /// thread per object.
    Handling,
    /// Every downtime in effect or still to come, as a timeline or a list.
    Downtimes,
}

impl ListKind {
    /// The two, in the order the sidebar lists them.
    #[cfg(test)]
    pub(crate) const ALL: [Self; 2] = [Self::Handling, Self::Downtimes];

    /// The kind a dashboard view of `display` shows, if it is one of the
    /// two (topic 14, round 5: view kinds).
    pub(crate) fn of_display(display: ic_config::ViewDisplay) -> Option<Self> {
        match display {
            ic_config::ViewDisplay::Handling => Some(Self::Handling),
            ic_config::ViewDisplay::Downtimes => Some(Self::Downtimes),
            _ => None,
        }
    }

    /// The view's name: its header's title and the sidebar's label.
    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::Handling => "handling",
            Self::Downtimes => "downtimes",
        }
    }

    /// Its id in the UI state (`state.toml`).
    pub(crate) fn id(self) -> &'static str {
        self.title()
    }

    /// The view with this id. Stage 2's lists open their successor: the
    /// comment and acknowledged lists are chips of handling now.
    pub(crate) fn from_id(id: &str) -> Option<Self> {
        match id {
            "handling" | "comments" | "acknowledged" => Some(Self::Handling),
            "downtimes" => Some(Self::Downtimes),
            _ => None,
        }
    }

    /// The icon in the sidebar's mark slot and the palette.
    pub(crate) fn icon(self) -> IconName {
        match self {
            Self::Handling => IconName::Users,
            Self::Downtimes => IconName::CalendarClock,
        }
    }

    /// The chips of its summary bar, *all* first.
    pub(crate) fn chips(self) -> &'static [Chip] {
        match self {
            Self::Handling => &[
                Chip::All,
                Chip::Acknowledged,
                Chip::InEffect,
                Chip::Upcoming,
                Chip::Comments,
            ],
            Self::Downtimes => &[Chip::All, Chip::InEffect, Chip::Upcoming, Chip::FromConfig],
        }
    }

    /// The sorts its menu offers.
    pub(crate) fn sorts(self) -> &'static [SortChoice] {
        match self {
            Self::Handling => &[
                SortChoice::LatestActivity,
                SortChoice::Soonest,
                SortChoice::Object,
                SortChoice::Author,
            ],
            Self::Downtimes => &[
                SortChoice::Soonest,
                SortChoice::ByTime,
                SortChoice::Object,
                SortChoice::Author,
            ],
        }
    }

    /// The selection bar's main button.
    pub(crate) fn removal_label(self) -> &'static str {
        match self {
            Self::Handling => "remove selected",
            Self::Downtimes => "remove downtimes",
        }
    }
}

/// A chip of the summary bar: what the view shows. Its count is the
/// chip's; a click shows only that kind, *all* every kind.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) enum Chip {
    /// Everything.
    #[default]
    All,
    /// Acknowledged problems (handling).
    Acknowledged,
    /// Downtimes in effect (handling's *in downtime*, the downtimes
    /// view's *in effect*).
    InEffect,
    /// Downtimes not in effect yet.
    Upcoming,
    /// Free-standing comments (handling).
    Comments,
    /// Downtimes from the config (the downtimes view).
    FromConfig,
}

impl Chip {
    /// Its id in the UI state.
    pub(crate) fn id(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Acknowledged => "acknowledged",
            Self::InEffect => "in-effect",
            Self::Upcoming => "upcoming",
            Self::Comments => "comments",
            Self::FromConfig => "from-config",
        }
    }

    /// The chip `kind` offers with this id.
    pub(crate) fn from_id(kind: ListKind, id: &str) -> Option<Self> {
        kind.chips().iter().copied().find(|chip| chip.id() == id)
    }

    /// Its word in `kind`'s summary bar (after the count).
    pub(crate) fn label(self, kind: ListKind) -> &'static str {
        match (self, kind) {
            (Self::All, _) => "all",
            (Self::Acknowledged, _) => "acknowledged",
            (Self::InEffect, ListKind::Handling) => "in downtime",
            (Self::InEffect, ListKind::Downtimes) => "in effect",
            (Self::Upcoming, _) => "upcoming",
            (Self::Comments, _) => "comments",
            (Self::FromConfig, _) => "from config",
        }
    }

    /// What the header's subtitle says while it is picked (`comments`).
    pub(crate) fn scope(self, kind: ListKind) -> Option<&'static str> {
        (self != Self::All).then(|| self.label(kind))
    }
}

/// The timeline or the list (the downtimes view).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) enum Mode {
    /// Bars on a shared axis around now (the default).
    #[default]
    Timeline,
    /// Sections and groups.
    List,
}

impl Mode {
    /// Its id in the UI state.
    pub(crate) fn id(self) -> &'static str {
        match self {
            Self::Timeline => "timeline",
            Self::List => "list",
        }
    }

    /// The mode with this id.
    pub(crate) fn from_id(id: &str) -> Option<Self> {
        match id {
            "timeline" => Some(Self::Timeline),
            "list" => Some(Self::List),
            _ => None,
        }
    }
}

/// How a view is sorted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum SortChoice {
    /// The newest thread first, by its latest entry (handling).
    LatestActivity,
    /// What comes back or changes soonest first: an acknowledgement's
    /// expiry, a downtime's end, an upcoming downtime's start.
    Soonest,
    /// By when the downtimes start (the timeline's).
    ByTime,
    /// By object: hosts, then services, in name order.
    Object,
    /// By who set it.
    Author,
}

impl SortChoice {
    /// Its id in the UI state.
    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::LatestActivity => "latest-activity",
            Self::Soonest => "soonest",
            Self::ByTime => "by-time",
            Self::Object => "object",
            Self::Author => "author",
        }
    }

    /// The sort a saved id names, if `kind` offers it.
    pub(crate) fn from_key(kind: ListKind, key: &str) -> Option<Self> {
        kind.sorts().iter().copied().find(|sort| sort.key() == key)
    }

    /// The header's word for it, which says what *soonest* means with the
    /// chip picked: `expires soonest ↑`, `starts soonest ↑`.
    pub(crate) fn label(self, kind: ListKind, chip: Chip) -> &'static str {
        match self {
            Self::LatestActivity => "latest activity ↓",
            Self::Soonest => match (kind, chip) {
                (ListKind::Handling, Chip::Acknowledged) => "expires soonest ↑",
                (_, Chip::Upcoming) => "starts soonest ↑",
                (ListKind::Downtimes, Chip::All | Chip::FromConfig) => {
                    "ends, then starts soonest ↑"
                }
                _ => "ends soonest ↑",
            },
            Self::ByTime => "by time",
            Self::Object => "object ↑",
            Self::Author => "author ↑",
        }
    }

    /// The sort menu's item.
    pub(crate) fn menu_label(self, kind: ListKind) -> &'static str {
        match (self, kind) {
            (Self::LatestActivity, _) => "latest activity",
            (Self::Soonest, ListKind::Handling) => "expires or ends soonest",
            (Self::Soonest, ListKind::Downtimes) => "ends, then starts soonest",
            (Self::ByTime, _) => "by time",
            (Self::Object, _) => "object",
            (Self::Author, _) => "author",
        }
    }

    /// The sort menu item's element id.
    pub(crate) fn id(self) -> &'static str {
        match self {
            Self::LatestActivity => "list-sort-latest",
            Self::Soonest => "list-sort-soonest",
            Self::ByTime => "list-sort-time",
            Self::Object => "list-sort-object",
            Self::Author => "list-sort-author",
        }
    }
}

/// What a view shows, as the user set it: kept with the dashboard view
/// ([`ic_config::ThreadOptions`]), or for the cluster section's entries
/// with the environment's UI state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Options {
    /// The chip picked.
    pub(crate) chip: Chip,
    /// The sort chosen; `None`: the chip's (or the mode's) own.
    pub(crate) sort: Option<SortChoice>,
    /// Only what the environment's author set (*only mine*).
    pub(crate) only_mine: bool,
    /// The downtimes view's display.
    pub(crate) mode: Mode,
    /// Which downtimes the downtimes view shows (a dashboard view's
    /// *shows*; the cluster's shows all).
    pub(crate) shows: ic_config::DowntimeKinds,
}

impl Options {
    /// A dashboard view's options ([`ic_config::View::threads`]); what
    /// `kind` doesn't offer is its default.
    pub(crate) fn of_view(kind: ListKind, threads: ic_config::ThreadOptions) -> Self {
        use ic_config::{DowntimesMode, ThreadChip, ThreadSort};
        let chip = match threads.chip {
            ThreadChip::All => Chip::All,
            ThreadChip::Acknowledged => Chip::Acknowledged,
            ThreadChip::InEffect => Chip::InEffect,
            ThreadChip::Upcoming => Chip::Upcoming,
            ThreadChip::Comments => Chip::Comments,
            ThreadChip::FromConfig => Chip::FromConfig,
        };
        let sort = threads.sort.map(|sort| match sort {
            ThreadSort::LatestActivity => SortChoice::LatestActivity,
            ThreadSort::Soonest => SortChoice::Soonest,
            ThreadSort::ByTime => SortChoice::ByTime,
            ThreadSort::Object => SortChoice::Object,
            ThreadSort::Author => SortChoice::Author,
        });
        Self {
            chip: if kind.chips().contains(&chip) {
                chip
            } else {
                Chip::All
            },
            sort: sort.filter(|sort| kind.sorts().contains(sort)),
            only_mine: threads.only_mine,
            mode: match (kind, threads.mode) {
                (ListKind::Downtimes, DowntimesMode::List) => Mode::List,
                _ => Mode::Timeline,
            },
            shows: match kind {
                ListKind::Handling => ic_config::DowntimeKinds::default(),
                ListKind::Downtimes => threads.shows,
            },
        }
    }

    /// The options as a dashboard view keeps them.
    pub(crate) fn to_view(&self) -> ic_config::ThreadOptions {
        use ic_config::{DowntimesMode, ThreadChip, ThreadSort};
        ic_config::ThreadOptions {
            chip: match self.chip {
                Chip::All => ThreadChip::All,
                Chip::Acknowledged => ThreadChip::Acknowledged,
                Chip::InEffect => ThreadChip::InEffect,
                Chip::Upcoming => ThreadChip::Upcoming,
                Chip::Comments => ThreadChip::Comments,
                Chip::FromConfig => ThreadChip::FromConfig,
            },
            sort: self.sort.map(|sort| match sort {
                SortChoice::LatestActivity => ThreadSort::LatestActivity,
                SortChoice::Soonest => ThreadSort::Soonest,
                SortChoice::ByTime => ThreadSort::ByTime,
                SortChoice::Object => ThreadSort::Object,
                SortChoice::Author => ThreadSort::Author,
            }),
            mode: match self.mode {
                Mode::Timeline => DowntimesMode::Timeline,
                Mode::List => DowntimesMode::List,
            },
            shows: self.shows,
            only_mine: self.only_mine,
        }
    }

    /// A view's options as saved; what it doesn't offer is its default.
    pub(crate) fn saved(kind: ListKind, saved: &ic_config::ListOptionsState) -> Self {
        Self {
            chip: saved
                .chip
                .as_deref()
                .and_then(|id| Chip::from_id(kind, id))
                .unwrap_or_default(),
            sort: saved
                .sort
                .as_deref()
                .and_then(|key| SortChoice::from_key(kind, key)),
            only_mine: saved.only_mine,
            mode: match kind {
                ListKind::Handling => Mode::default(),
                ListKind::Downtimes => saved
                    .mode
                    .as_deref()
                    .and_then(Mode::from_id)
                    .unwrap_or_default(),
            },
            shows: ic_config::DowntimeKinds::default(),
        }
    }

    /// What is kept between runs (defaults kept as none).
    pub(crate) fn to_saved(&self, kind: ListKind) -> ic_config::ListOptionsState {
        ic_config::ListOptionsState {
            sort: self.sort.map(|sort| sort.key().to_owned()),
            only_mine: self.only_mine,
            chip: (self.chip != Chip::All).then(|| self.chip.id().to_owned()),
            mode: (kind == ListKind::Downtimes && self.mode != Mode::default())
                .then(|| self.mode.id().to_owned()),
            density: None,
        }
    }

    /// The sort in effect: the chosen one, else the chip's or the mode's.
    pub(crate) fn sort(&self, kind: ListKind) -> SortChoice {
        self.sort.unwrap_or(match (kind, self.chip, self.mode) {
            (ListKind::Handling, Chip::All | Chip::Comments, _) => SortChoice::LatestActivity,
            (ListKind::Handling, _, _) | (ListKind::Downtimes, _, Mode::List) => {
                SortChoice::Soonest
            }
            (ListKind::Downtimes, _, Mode::Timeline) => SortChoice::ByTime,
        })
    }

    /// Picks `chip`: its own sort comes with it.
    pub(crate) fn pick_chip(&mut self, chip: Chip) {
        self.chip = chip;
        self.sort = None;
    }

    /// Picks the timeline or the list: its own sort comes with it.
    pub(crate) fn pick_mode(&mut self, mode: Mode) {
        self.mode = mode;
        self.sort = None;
    }

    /// The next sort `kind` offers after the one in effect (`s`).
    pub(crate) fn next_sort(&self, kind: ListKind) -> SortChoice {
        let sorts = kind.sorts();
        let index = sorts
            .iter()
            .position(|sort| *sort == self.sort(kind))
            .unwrap_or(0);
        sorts[(index + 1) % sorts.len()]
    }
}

/// How many the sidebar and the palette count of the objects in `scope`:
/// handling the objects being handled, downtimes those in effect now (a
/// host's with its services counts once) of the kinds `shows` lets
/// through.
pub(crate) fn count(
    kind: ListKind,
    snapshot: &Snapshot,
    scope: super::threads::Scope<'_>,
    shows: ic_config::DowntimeKinds,
    now: Timestamp,
) -> usize {
    match kind {
        ListKind::Handling => super::threads::handled_objects(snapshot, scope, now),
        ListKind::Downtimes => super::threads::downtimes_in_effect(snapshot, scope, shows, now),
    }
}

/// Icinga's own comments, written with an acknowledgement, a downtime or
/// flapping: never entries of their own (the acknowledgement's or the
/// downtime's text is part of that entry).
pub(crate) fn is_automatic(kind: CommentKind) -> bool {
    kind != CommentKind::User
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

/// A text's first line (lists show one).
pub(crate) fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or_default().trim()
}

/// A flexible downtime's length: `2h`, `1h 30m`.
pub(crate) fn length(seconds: f64) -> String {
    format_two_units(Duration::try_from_secs_f64(seconds.max(0.0)).unwrap_or_default())
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

/// Whether `at` is on the same local day as `now`.
pub(crate) fn same_day<Tz>(at: Timestamp, now: Timestamp, zone: &Tz) -> bool
where
    Tz: TimeZone,
    Tz::Offset: Display,
{
    match (format::date_time(at, zone), format::date_time(now, zone)) {
        (Some(at), Some(today)) => at.date_naive() == today.date_naive(),
        _ => false,
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
    use chrono::Utc;

    use super::*;

    /// Wednesday 7 October 2026, 14:12 UTC: the mock-ups' clock.
    fn now() -> Timestamp {
        Timestamp::from_unix_seconds(1_791_382_320.)
    }

    fn at(minutes: f64) -> Timestamp {
        Timestamp::from_unix_seconds(now().as_unix_seconds() + minutes * 60.)
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
        assert!(same_day(at(30.), now(), &Utc));
        assert!(!same_day(at(60. * 10.), now(), &Utc));
        assert_eq!(length(5_400.), "1h 30m");
        assert_eq!(first_line("  one\ntwo"), "one");
    }

    #[test]
    fn kinds_have_ids_chips_and_sorts() {
        for kind in ListKind::ALL {
            assert_eq!(ListKind::from_id(kind.id()), Some(kind));
            assert_eq!(kind.chips()[0], Chip::All);
            for chip in kind.chips() {
                assert_eq!(Chip::from_id(kind, chip.id()), Some(*chip));
            }
            for sort in kind.sorts() {
                assert_eq!(SortChoice::from_key(kind, sort.key()), Some(*sort));
            }
        }
        // Stage 2's lists open their successor.
        assert_eq!(ListKind::from_id("comments"), Some(ListKind::Handling));
        assert_eq!(ListKind::from_id("acknowledged"), Some(ListKind::Handling));
        assert_eq!(ListKind::from_id("nonsense"), None);
        assert_eq!(Chip::from_id(ListKind::Downtimes, "comments"), None);
        assert_eq!(Chip::InEffect.label(ListKind::Handling), "in downtime");
        assert_eq!(Chip::InEffect.label(ListKind::Downtimes), "in effect");
    }

    #[test]
    fn the_sort_follows_the_question() {
        let mut handling = Options::default();
        assert_eq!(
            handling.sort(ListKind::Handling),
            SortChoice::LatestActivity
        );
        handling.pick_chip(Chip::Acknowledged);
        assert_eq!(handling.sort(ListKind::Handling), SortChoice::Soonest);
        assert_eq!(
            SortChoice::Soonest.label(ListKind::Handling, Chip::Acknowledged),
            "expires soonest ↑"
        );
        assert_eq!(
            SortChoice::Soonest.label(ListKind::Handling, Chip::Upcoming),
            "starts soonest ↑"
        );
        handling.sort = Some(SortChoice::Author);
        assert_eq!(
            handling.next_sort(ListKind::Handling),
            SortChoice::LatestActivity
        );
        // Another chip brings its own sort.
        handling.pick_chip(Chip::Comments);
        assert_eq!(
            handling.sort(ListKind::Handling),
            SortChoice::LatestActivity
        );

        let mut downtimes = Options::default();
        assert_eq!(downtimes.mode, Mode::Timeline);
        assert_eq!(downtimes.sort(ListKind::Downtimes), SortChoice::ByTime);
        downtimes.pick_mode(Mode::List);
        assert_eq!(downtimes.sort(ListKind::Downtimes), SortChoice::Soonest);
    }

    #[test]
    fn choices_are_kept_and_defaults_are_left_out() {
        let options = Options {
            chip: Chip::Upcoming,
            sort: Some(SortChoice::Object),
            only_mine: true,
            mode: Mode::List,
            shows: ic_config::DowntimeKinds::default(),
        };
        let saved = options.to_saved(ListKind::Downtimes);
        assert_eq!(saved.chip.as_deref(), Some("upcoming"));
        assert_eq!(saved.mode.as_deref(), Some("list"));
        assert_eq!(Options::saved(ListKind::Downtimes, &saved), options);
        assert_eq!(
            Options::default().to_saved(ListKind::Downtimes),
            ic_config::ListOptionsState::default()
        );
        // What a view doesn't offer falls back to its default.
        let foreign = ic_config::ListOptionsState {
            chip: Some("comments".to_owned()),
            sort: Some("ends-soonest".to_owned()),
            ..ic_config::ListOptionsState::default()
        };
        assert_eq!(
            Options::saved(ListKind::Downtimes, &foreign),
            Options::default()
        );
    }
}
