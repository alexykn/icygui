//! A notification as the notification centre lists it (NOTE-05): when,
//! its tone, title, first line, where it matched, and whether it was
//! silent or is unread. Pure, so it is tested without a window.

use ic_core::NotificationRecord;
use ic_model::{ObjectKey, Timestamp};
use ic_rules::Tone;

use crate::format;

/// One row of the notification centre.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CentreEntry {
    /// The intent's id (to mark it read).
    pub(crate) id: String,
    /// The object it is about (`None`: a storm summary).
    pub(crate) object: Option<ObjectKey>,
    /// `14:32` today, else the day (`Oct 3`).
    pub(crate) time: String,
    /// Its colour.
    pub(crate) tone: Tone,
    /// `CRITICAL · postgres-replication on db-prod-03`.
    pub(crate) title: String,
    /// The output's first line, the comment, or the summary.
    pub(crate) body: String,
    /// Where it matched, and `silent` when it showed no system
    /// notification (quiet hours, a pause, a storm).
    pub(crate) detail: String,
    /// Not seen yet.
    pub(crate) unread: bool,
}

/// The centre's rows for `records` (newest first, as given).
pub(crate) fn entries<'a>(
    records: impl Iterator<Item = &'a NotificationRecord>,
    now: Timestamp,
) -> Vec<CentreEntry> {
    records
        .map(|record| {
            let intent = &record.intent;
            let mut detail = intent.subtitle.trim().to_owned();
            if intent.silent {
                if !detail.is_empty() {
                    detail.push_str(" · ");
                }
                detail.push_str("silent");
            }
            CentreEntry {
                id: intent.id.clone(),
                object: intent.object.clone(),
                time: time_label(intent.at, now),
                tone: intent.tone,
                title: intent.title.clone(),
                body: intent
                    .body
                    .lines()
                    .map(str::trim)
                    .find(|line| !line.is_empty())
                    .unwrap_or_default()
                    .to_owned(),
                detail,
                unread: !record.read,
            }
        })
        .collect()
}

/// `14:32` for today, the day (`Oct 3`) for older ones.
pub(crate) fn time_label(at: Timestamp, now: Timestamp) -> String {
    let clock = format::clock(at, now);
    // Other days read `Oct 3 14:32`: the day is enough in a narrow column.
    match clock.rsplit_once(' ') {
        Some((day, _)) => day.to_owned(),
        None => clock,
    }
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

/// What the centre's tooltip and the tray say about silent entries.
pub(crate) const SILENT_HINT: &str = "Recorded without a system notification: quiet hours, a pause, \
     or one of many in a storm (the summary notified instead).";

#[cfg(test)]
mod tests {
    use ic_rules::NotificationIntent;

    use super::*;

    fn now() -> Timestamp {
        Timestamp::from_unix_seconds(1_790_000_000.)
    }

    fn record(body: &str, subtitle: &str, silent: bool, read: bool, at: f64) -> NotificationRecord {
        NotificationRecord {
            intent: NotificationIntent {
                id: "id".to_owned(),
                object: None,
                title: "14 new problems in prod-cluster".to_owned(),
                subtitle: subtitle.to_owned(),
                body: body.to_owned(),
                tone: Tone::Info,
                sound: false,
                silent,
                at: Timestamp::from_unix_seconds(at),
            },
            read,
        }
    }

    #[test]
    fn entries_show_the_first_line_where_and_whether_silent() {
        let records = [
            record(
                "\n  first line\nsecond",
                "databases / production",
                true,
                false,
                1_790_000_000.,
            ),
            record("", "", false, true, 1_789_000_000.),
        ];
        let rows = entries(records.iter(), now());
        assert_eq!(rows[0].body, "first line");
        assert_eq!(rows[0].detail, "databases / production · silent");
        assert!(rows[0].unread);
        assert_eq!(rows[1].detail, "");
        assert!(!rows[1].unread);
        assert!(
            !rows[1].time.contains(':'),
            "older days show the day: {}",
            rows[1].time
        );
        assert!(rows[0].time.contains(':'), "today shows the time");
    }

    #[test]
    fn the_heading_and_badge_count_unread() {
        assert_eq!(summary(0, 0), "");
        assert_eq!(summary(0, 3), "all read");
        assert_eq!(summary(2, 3), "2 unread");
        assert_eq!(badge(0), None);
        assert_eq!(badge(7).as_deref(), Some("7"));
        assert_eq!(badge(250).as_deref(), Some("99+"));
    }
}
