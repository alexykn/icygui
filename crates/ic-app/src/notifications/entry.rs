//! The notification centre's list as it shows (NOTE-05, A2), pure so it is
//! tested without a window: the notifications of one environment or of
//! every one (the scope), newest first under time sections (`now`, `last
//! hour`, `earlier today`, `yesterday`, `older`); each with its time, tone,
//! title, first line, where it matched (`overview`, never `overview /
//! overview`; the environment in front only in *all*), and why it was
//! silent (`silent · storm`, `silent · quiet hours`, `silent · paused`). A
//! storm's notifications collapse into its summary, which expands to list
//! them. A label filters the list to the notifications of that place.

use std::collections::HashMap;
use std::fmt::Display;

use chrono::{DateTime, Local, NaiveDate, TimeZone};
use ic_core::NotificationRecord;
use ic_model::{ObjectKey, Timestamp};
use ic_rules::{Silence, Tone};

/// Which notifications the centre lists.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Scope {
    /// Every environment's.
    All,
    /// One environment's, by id.
    Environment(String),
}

/// One environment's notifications, newest first.
#[derive(Clone, Debug)]
pub(crate) struct Source<'a> {
    /// The environment's id.
    pub(crate) id: &'a str,
    /// Its name.
    pub(crate) name: &'a str,
    /// Its notifications.
    pub(crate) records: Vec<&'a NotificationRecord>,
}

/// Where notifications matched, in one environment: what a click on an
/// entry's label filters the list to.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Place {
    /// The environment's id.
    pub(crate) environment: String,
    /// The label without the environment (`overview`, `databases /
    /// production`); empty for the environment itself (its default rule,
    /// a storm summary).
    pub(crate) label: String,
}

/// An entry's label: the text shown and the place it filters to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Label {
    /// `overview`, `staging · overview` (in *all*), `staging`.
    pub(crate) text: String,
    /// What a click filters to.
    pub(crate) place: Place,
}

/// One notification as the centre shows it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CentreEntry {
    /// The environment it came from (its id).
    pub(crate) environment: String,
    /// The intent's id (to mark it read).
    pub(crate) id: String,
    /// The object it is about (`None`: a storm summary).
    pub(crate) object: Option<ObjectKey>,
    /// When it happened.
    pub(crate) at: Timestamp,
    /// `14:32`, or the day (`Oct 3`) in the *older* section.
    pub(crate) time: String,
    /// Its colour.
    pub(crate) tone: Tone,
    /// `CRITICAL · postgres-replication on db-prod-03`.
    pub(crate) title: String,
    /// The output's first line, the comment, or the summary.
    pub(crate) body: String,
    /// Where it matched (`None`: the scope already says it).
    pub(crate) label: Option<Label>,
    /// Why no system notification showed: `silent · storm`, `silent ·
    /// quiet hours`, `silent · paused`, or `silent` (an older record).
    pub(crate) silence: Option<String>,
    /// Not seen yet.
    pub(crate) unread: bool,
}

impl CentreEntry {
    /// Whether it was recorded without a system notification.
    pub(crate) fn is_silent(&self) -> bool {
        self.silence.is_some()
    }
}

/// A storm: its summary, and the notifications it held back.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StormGroup {
    /// Unique in the centre (environment and summary id): what expands it.
    pub(crate) key: String,
    /// The summary (`24 new problems in prod-cluster`), or a stand-in
    /// while the storm still goes on (no summary yet).
    pub(crate) header: CentreEntry,
    /// Whether `header` is a stand-in (not a notification of its own).
    pub(crate) ongoing: bool,
    /// What the storm held back, newest first.
    pub(crate) members: Vec<CentreEntry>,
}

impl StormGroup {
    /// Whether the summary or any notification it held back is unread.
    pub(crate) fn unread(&self) -> bool {
        (!self.ongoing && self.header.unread) || self.members.iter().any(|entry| entry.unread)
    }
}

/// A row of the centre.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum CentreItem {
    /// One notification.
    Entry(CentreEntry),
    /// A storm, collapsed into one row that expands.
    Storm(StormGroup),
}

impl CentreItem {
    /// When it happened: a storm by its summary, or its newest
    /// notification while it goes on.
    fn at(&self) -> Timestamp {
        match self {
            Self::Entry(entry) => entry.at,
            Self::Storm(group) => group.header.at,
        }
    }

    /// The notifications in it (a storm's summary, unless a stand-in, and
    /// what it held back).
    pub(crate) fn entries(&self) -> Vec<&CentreEntry> {
        match self {
            Self::Entry(entry) => vec![entry],
            Self::Storm(group) => (!group.ongoing)
                .then_some(&group.header)
                .into_iter()
                .chain(&group.members)
                .collect(),
        }
    }
}

/// The centre's time sections.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum When {
    /// The last five minutes.
    Now,
    /// The last hour.
    LastHour,
    /// Earlier today (local time).
    EarlierToday,
    /// Yesterday (local time).
    Yesterday,
    /// Before yesterday.
    Older,
}

impl When {
    /// The section's title.
    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::Now => "now",
            Self::LastHour => "last hour",
            Self::EarlierToday => "earlier today",
            Self::Yesterday => "yesterday",
            Self::Older => "older",
        }
    }

    /// The section of something that happened at `at`, seen at `now`.
    fn of<Tz: TimeZone>(at: Timestamp, now: Timestamp, zone: &Tz) -> Self {
        let age = now.as_unix_seconds() - at.as_unix_seconds();
        if age < 5. * 60. {
            return Self::Now;
        }
        if age < 60. * 60. {
            return Self::LastHour;
        }
        let (Some(day), Some(today)) = (local_date(at, zone), local_date(now, zone)) else {
            return Self::Older;
        };
        if day == today {
            Self::EarlierToday
        } else if today.pred_opt() == Some(day) {
            Self::Yesterday
        } else {
            Self::Older
        }
    }
}

/// One time section of the centre, newest first.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CentreSection {
    /// Its time.
    pub(crate) when: When,
    /// Its rows, newest first.
    pub(crate) items: Vec<CentreItem>,
}

/// The centre's list for a scope (and a label filter).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct CentreView {
    /// The sections, newest first; empty ones are left out.
    pub(crate) sections: Vec<CentreSection>,
    /// Unread notifications in the scope (what the heading counts).
    pub(crate) unread: usize,
    /// Notifications in the scope.
    pub(crate) total: usize,
}

impl CentreView {
    /// Every notification listed (a storm's too, collapsed or not).
    pub(crate) fn entries(&self) -> impl Iterator<Item = &CentreEntry> {
        self.sections
            .iter()
            .flat_map(|section| &section.items)
            .flat_map(CentreItem::entries)
    }

    /// Whether any notification listed is unread.
    pub(crate) fn has_unread(&self) -> bool {
        self.entries().any(|entry| entry.unread)
    }
}

/// The centre's list of `sources` (every environment in scope), filtered
/// to `filter`'s place, at `now` in local time. `all`: the scope is every
/// environment (labels then name the environment).
pub(crate) fn centre_view(
    sources: &[Source<'_>],
    all: bool,
    filter: Option<&Place>,
    now: Timestamp,
) -> CentreView {
    centre_view_in(sources, all, filter, now, &Local)
}

/// [`centre_view`] in time zone `zone`.
pub(crate) fn centre_view_in<Tz: TimeZone>(
    sources: &[Source<'_>],
    all: bool,
    filter: Option<&Place>,
    now: Timestamp,
    zone: &Tz,
) -> CentreView
where
    Tz::Offset: Display,
{
    let mut view = CentreView::default();
    let mut items = Vec::new();
    for source in sources {
        view.total += source.records.len();
        view.unread += source.records.iter().filter(|record| !record.read).count();
        items.extend(items_of(source, all, now, zone));
    }
    if let Some(place) = filter {
        items = items
            .into_iter()
            .filter_map(|item| keep(item, place))
            .collect();
    }
    // Newest first across environments (stable: an environment's own
    // order holds for equal times).
    items.sort_by(|a, b| {
        b.at()
            .as_unix_seconds()
            .total_cmp(&a.at().as_unix_seconds())
    });
    for item in items {
        let when = When::of(item.at(), now, zone);
        match view.sections.last_mut() {
            Some(section) if section.when == when => section.items.push(item),
            _ => view.sections.push(CentreSection {
                when,
                items: vec![item],
            }),
        }
    }
    view
}

/// `item` as far as it shows notifications of `place`: a storm whose
/// summary is there keeps everything it held back, another keeps what
/// was there; `None` when nothing is.
fn keep(item: CentreItem, place: &Place) -> Option<CentreItem> {
    let at = |entry: &CentreEntry| entry.label.as_ref().map(|label| &label.place) == Some(place);
    match item {
        CentreItem::Entry(entry) => at(&entry).then_some(CentreItem::Entry(entry)),
        CentreItem::Storm(group) if !group.ongoing && at(&group.header) => {
            Some(CentreItem::Storm(group))
        }
        CentreItem::Storm(mut group) => {
            group.members.retain(at);
            (!group.members.is_empty()).then_some(CentreItem::Storm(group))
        }
    }
}

/// One environment's rows: its storms collapsed, the rest one by one.
fn items_of<Tz: TimeZone>(
    source: &Source<'_>,
    all: bool,
    now: Timestamp,
    zone: &Tz,
) -> Vec<CentreItem>
where
    Tz::Offset: Display,
{
    let entry = |record: &NotificationRecord| entry_of(source, record, all, now, zone);
    // What each storm held back, by its summary's id.
    let mut held: HashMap<&str, Vec<CentreEntry>> = HashMap::new();
    for record in &source.records {
        if let Some(Silence::Storm { summary }) = &record.intent.silenced {
            held.entry(summary.as_str())
                .or_default()
                .push(entry(record));
        }
    }
    let mut items = Vec::new();
    for record in &source.records {
        let intent = &record.intent;
        if matches!(intent.silenced, Some(Silence::Storm { .. })) {
            continue;
        }
        match held.remove(intent.id.as_str()) {
            Some(members) => items.push(CentreItem::Storm(StormGroup {
                key: format!("{}\u{1f}{}", source.id, intent.id),
                header: entry(record),
                ongoing: false,
                members,
            })),
            None => items.push(CentreItem::Entry(entry(record))),
        }
    }
    // Storms still going on (no summary yet), and storms whose summary
    // is no longer among the recent notifications.
    let mut ongoing: Vec<_> = held.into_iter().collect();
    ongoing.sort_by(|a, b| a.0.cmp(b.0));
    for (summary, members) in ongoing {
        let Some(newest) = members.first() else {
            continue;
        };
        let header = CentreEntry {
            environment: source.id.to_owned(),
            id: summary.to_owned(),
            object: None,
            at: newest.at,
            time: newest.time.clone(),
            tone: Tone::Info,
            title: format!("{} held back by a storm", count_of(members.len())),
            body: "The summary follows when it calms down.".to_owned(),
            label: label_of(source, "", all),
            silence: None,
            unread: false,
        };
        items.push(CentreItem::Storm(StormGroup {
            key: format!("{}\u{1f}{summary}", source.id),
            header,
            ongoing: true,
            members,
        }));
    }
    items
}

/// `3 notifications`, `1 notification`.
fn count_of(count: usize) -> String {
    if count == 1 {
        "1 notification".to_owned()
    } else {
        format!("{count} notifications")
    }
}

/// `record` of `source` as a row.
fn entry_of<Tz: TimeZone>(
    source: &Source<'_>,
    record: &NotificationRecord,
    all: bool,
    now: Timestamp,
    zone: &Tz,
) -> CentreEntry
where
    Tz::Offset: Display,
{
    let intent = &record.intent;
    let silence = intent.silent.then(|| match &intent.silenced {
        Some(reason) => format!("silent · {}", reason.label()),
        None => "silent".to_owned(),
    });
    CentreEntry {
        environment: source.id.to_owned(),
        id: intent.id.clone(),
        object: intent.object.clone(),
        at: intent.at,
        time: time_label_in(intent.at, now, zone),
        tone: intent.tone,
        title: intent.title.clone(),
        body: intent
            .body
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or_default()
            .to_owned(),
        label: label_of(source, &intent.subtitle, all),
        silence,
        unread: !record.read,
    }
}

/// Where a notification of environment `environment` matched, from its
/// subtitle, without repeating anything: equal parts in a row once
/// (`overview / overview` reads `overview`), and nothing when it names
/// only the environment (a storm's summary). The centre's labels and the
/// desktop notifications both read it.
pub(crate) fn place_of(subtitle: &str, environment: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in subtitle.split(" / ").map(str::trim) {
        if !part.is_empty() && parts.last() != Some(&part) {
            parts.push(part);
        }
    }
    let place = parts.join(" / ");
    if place == environment.trim() {
        String::new()
    } else {
        place
    }
}

/// Where a notification of `source` matched ([`place_of`]): `None` when
/// the scope already says it all.
fn label_of(source: &Source<'_>, subtitle: &str, all: bool) -> Option<Label> {
    let label = place_of(subtitle, source.name);
    let text = match (all, label.is_empty()) {
        (true, true) => source.name.to_owned(),
        (true, false) => format!("{} · {label}", source.name),
        (false, true) => return None,
        (false, false) => label.clone(),
    };
    Some(Label {
        text,
        place: Place {
            environment: source.id.to_owned(),
            label,
        },
    })
}

/// `14:32`, or the day (`Oct 3`) before yesterday (the section names
/// yesterday).
fn time_label_in<Tz: TimeZone>(at: Timestamp, now: Timestamp, zone: &Tz) -> String
where
    Tz::Offset: Display,
{
    let Some(time) = date_time(at, zone) else {
        return "—".to_owned();
    };
    if When::of(at, now, zone) == When::Older {
        time.format("%b %-d").to_string()
    } else {
        time.format("%H:%M").to_string()
    }
}

fn date_time<Tz: TimeZone>(at: Timestamp, zone: &Tz) -> Option<DateTime<Tz>> {
    let millis = (at.as_unix_seconds() * 1000.).round();
    if !millis.is_finite() {
        return None;
    }
    #[expect(
        clippy::cast_possible_truncation,
        reason = "finite and rounded; out-of-range times are rejected by chrono"
    )]
    let millis = millis as i64;
    zone.timestamp_millis_opt(millis).single()
}

fn local_date<Tz: TimeZone>(at: Timestamp, zone: &Tz) -> Option<NaiveDate> {
    date_time(at, zone).map(|time| time.date_naive())
}

/// The centre's heading after "Notifications": `3 unread`, `all read`,
/// or nothing while there are none.
pub(crate) fn summary(unread: usize, total: usize) -> String {
    match (unread, total) {
        (_, 0) => String::new(),
        (0, _) => "all read".to_owned(),
        (unread, _) => format!("{unread} unread"),
    }
}

/// The footer clock's badge: the count, at most `99+`.
pub(crate) fn badge(unread: usize) -> Option<String> {
    match unread {
        0 => None,
        1..=99 => Some(unread.to_string()),
        _ => Some("99+".to_owned()),
    }
}

/// What the centre says about an entry that showed no system
/// notification, by its reason.
pub(crate) fn silent_hint(silence: &str) -> &'static str {
    if silence.ends_with("storm") {
        "Recorded without a system notification: one of many in a storm. The storm's \
         summary notified instead."
    } else if silence.ends_with("quiet hours") {
        "Recorded without a system notification: it came during quiet hours."
    } else if silence.ends_with("paused") {
        "Recorded without a system notification: notifications were paused."
    } else {
        "Recorded without a system notification: quiet hours, a pause, or one of many in \
         a storm (the summary notified instead)."
    }
}

/// The centre's footer: where the history is kept, and for how long.
pub(crate) fn kept_text(retention_hours: u32) -> String {
    let span = if retention_hours.is_multiple_of(24) && retention_hours >= 48 {
        format!("{} days", retention_hours / 24)
    } else if retention_hours == 1 {
        "1 hour".to_owned()
    } else {
        format!("{retention_hours} hours")
    };
    format!("kept {span} on this computer")
}

#[cfg(test)]
mod tests {
    use chrono::FixedOffset;
    use ic_rules::NotificationIntent;

    use super::*;

    /// 2026-10-06 12:00 UTC.
    const NOON: f64 = 1_791_288_000.;

    fn zone() -> FixedOffset {
        FixedOffset::east_opt(0).unwrap()
    }

    fn now() -> Timestamp {
        Timestamp::from_unix_seconds(NOON)
    }

    fn record(id: &str, ago: f64, subtitle: &str, silenced: Option<Silence>) -> NotificationRecord {
        NotificationRecord {
            intent: NotificationIntent {
                id: id.to_owned(),
                object: (!id.starts_with("storm:")).then(|| ObjectKey::service("db-01", id)),
                title: format!("CRITICAL · {id} on db-01"),
                subtitle: subtitle.to_owned(),
                body: "\n  first line\nsecond".to_owned(),
                tone: Tone::Critical,
                sound: false,
                silent: silenced.is_some(),
                silenced,
                at: Timestamp::from_unix_seconds(NOON - ago),
            },
            read: false,
        }
    }

    fn source<'a>(id: &'a str, name: &'a str, records: &'a [NotificationRecord]) -> Source<'a> {
        Source {
            id,
            name,
            records: records.iter().collect(),
        }
    }

    fn titles(view: &CentreView) -> Vec<(&'static str, Vec<String>)> {
        view.sections
            .iter()
            .map(|section| {
                (
                    section.when.title(),
                    section
                        .items
                        .iter()
                        .map(|item| match item {
                            CentreItem::Entry(entry) => entry.id.clone(),
                            CentreItem::Storm(group) => format!(
                                "{}[{}]",
                                group.header.id,
                                group
                                    .members
                                    .iter()
                                    .map(|member| member.id.as_str())
                                    .collect::<Vec<_>>()
                                    .join(",")
                            ),
                        })
                        .collect(),
                )
            })
            .collect()
    }

    #[test]
    fn entries_fall_into_time_sections_newest_first() {
        let records = [
            record("a", 30., "overview / overview", None),
            record("b", 20. * 60., "overview / overview", None),
            record("c", 3. * 3600., "overview / overview", None),
            record("d", 20. * 3600., "overview / overview", None),
            record("e", 3. * 86_400., "overview / overview", None),
        ];
        let view = centre_view_in(
            &[source("prod", "prod-cluster", &records)],
            false,
            None,
            now(),
            &zone(),
        );
        assert_eq!(
            titles(&view),
            [
                ("now", vec!["a".to_owned()]),
                ("last hour", vec!["b".to_owned()]),
                ("earlier today", vec!["c".to_owned()]),
                ("yesterday", vec!["d".to_owned()]),
                ("older", vec!["e".to_owned()]),
            ]
        );
        let entries: Vec<_> = view.entries().collect();
        assert_eq!(entries[0].time, "11:59");
        assert_eq!(entries[3].time, "16:00", "yesterday's time");
        assert_eq!(entries[4].time, "Oct 3", "older ones show the day");
        assert_eq!(entries[0].body, "first line");
        assert_eq!((view.unread, view.total), (5, 5));
    }

    #[test]
    fn labels_say_where_without_repeating_and_name_the_environment_in_all() {
        let prod = [
            record("a", 10., "overview / overview", None),
            record("b", 20., "databases / production", None),
            record("c", 30., "prod-cluster", None),
        ];
        let staging = [record("d", 15., "overview / overview", None)];
        let one = centre_view_in(
            &[source("prod", "prod-cluster", &prod)],
            false,
            None,
            now(),
            &zone(),
        );
        let labels: Vec<_> = one
            .entries()
            .map(|entry| entry.label.as_ref().map(|label| label.text.as_str()))
            .collect();
        assert_eq!(
            labels,
            [Some("overview"), Some("databases / production"), None]
        );

        let all = centre_view_in(
            &[
                source("prod", "prod-cluster", &prod),
                source("staging", "staging", &staging),
            ],
            true,
            None,
            now(),
            &zone(),
        );
        let labels: Vec<_> = all
            .entries()
            .map(|entry| entry.label.clone().unwrap().text)
            .collect();
        assert_eq!(
            labels,
            [
                "prod-cluster · overview",
                "staging · overview",
                "prod-cluster · databases / production",
                "prod-cluster",
            ],
            "merged newest first"
        );
        assert_eq!((all.unread, all.total), (4, 4));
    }

    #[test]
    fn a_label_filters_to_its_place() {
        let prod = [
            record("a", 10., "overview / overview", None),
            record("b", 20., "databases / production", None),
        ];
        let staging = [record("c", 15., "overview / overview", None)];
        let sources = [
            source("prod", "prod-cluster", &prod),
            source("staging", "staging", &staging),
        ];
        let place = Place {
            environment: "prod".to_owned(),
            label: "overview".to_owned(),
        };
        let view = centre_view_in(&sources, true, Some(&place), now(), &zone());
        let ids: Vec<_> = view.entries().map(|entry| entry.id.as_str()).collect();
        assert_eq!(ids, ["a"], "staging's overview is another place");
        assert_eq!(view.unread, 3, "the heading counts the scope");
    }

    #[test]
    fn storms_collapse_into_their_summary() {
        let storm = || Silence::Storm {
            summary: "storm:1".to_owned(),
        };
        let records = [
            record("storm:1", 10., "prod-cluster", None),
            record("x", 20., "overview / overview", Some(storm())),
            record("y", 25., "overview / overview", Some(storm())),
            record("z", 30., "overview / overview", Some(Silence::QuietHours)),
            // A storm going on: no summary yet.
            record(
                "w",
                5.,
                "network / network",
                Some(Silence::Storm {
                    summary: "storm:2".to_owned(),
                }),
            ),
        ];
        let view = centre_view_in(
            &[source("prod", "prod-cluster", &records)],
            false,
            None,
            now(),
            &zone(),
        );
        assert_eq!(
            titles(&view),
            [(
                "now",
                vec![
                    "storm:2[w]".to_owned(),
                    "storm:1[x,y]".to_owned(),
                    "z".to_owned()
                ]
            )]
        );
        let CentreItem::Storm(ongoing) = &view.sections[0].items[0] else {
            panic!("a storm");
        };
        assert!(ongoing.ongoing);
        assert_eq!(ongoing.header.title, "1 notification held back by a storm");
        assert!(ongoing.unread());
        let CentreItem::Storm(over) = &view.sections[0].items[1] else {
            panic!("a storm");
        };
        assert!(!over.ongoing);
        assert_eq!(over.members[0].silence.as_deref(), Some("silent · storm"));
        assert_eq!(view.entries().count(), 5, "the stand-in isn't one");
        let CentreItem::Entry(quiet) = &view.sections[0].items[2] else {
            panic!("an entry");
        };
        assert_eq!(quiet.silence.as_deref(), Some("silent · quiet hours"));
        assert!(quiet.is_silent());

        // Filtering by the members' place keeps the storm with them only.
        let place = Place {
            environment: "prod".to_owned(),
            label: "overview".to_owned(),
        };
        let filtered = centre_view_in(
            &[source("prod", "prod-cluster", &records)],
            true,
            Some(&place),
            now(),
            &zone(),
        );
        assert_eq!(
            titles(&filtered),
            [("now", vec!["storm:1[x,y]".to_owned(), "z".to_owned()])]
        );
    }

    #[test]
    fn older_records_are_silent_without_a_reason() {
        let mut old = record("a", 10., "", None);
        old.intent.silent = true;
        old.read = true;
        let view = centre_view_in(
            &[source("prod", "prod-cluster", std::slice::from_ref(&old))],
            false,
            None,
            now(),
            &zone(),
        );
        let entry = view.entries().next().unwrap();
        assert_eq!(entry.silence.as_deref(), Some("silent"));
        assert!(!entry.unread);
        assert!(!view.has_unread());
        assert!(silent_hint("silent").contains("quiet hours, a pause"));
        assert!(silent_hint("silent · storm").contains("storm"));
    }

    #[test]
    fn the_heading_badge_and_footer_say_how_much_and_how_long() {
        assert_eq!(summary(0, 0), "");
        assert_eq!(summary(0, 3), "all read");
        assert_eq!(summary(2, 3), "2 unread");
        assert_eq!(badge(0), None);
        assert_eq!(badge(7).as_deref(), Some("7"));
        assert_eq!(badge(250).as_deref(), Some("99+"));
        assert_eq!(kept_text(48), "kept 2 days on this computer");
        assert_eq!(kept_text(36), "kept 36 hours on this computer");
        assert_eq!(kept_text(1), "kept 1 hour on this computer");
    }
}
