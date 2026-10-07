//! How long notifications pause (NOTE-04) and how long an object stays
//! muted (NOTE-02): the choices the palette, the notification centre, the
//! settings, the panes and the tray offer, and how their ends read.
//!
//! "Until 08:00" means the next 08:00 local time, as in the tray's menu: a
//! pause chosen at 05:30 after a night incident ends at 08:00 that
//! morning, not 26 hours later. Pure, so it is tested without a window.

use chrono::{DateTime, Days, Local, NaiveTime, TimeZone};
use ic_model::Timestamp;

/// The local hour at which a pause or mute "until morning" ends.
const MORNING_HOUR: u32 = 8;

/// How long to pause notifications for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum PauseChoice {
    /// 30 minutes.
    HalfHour,
    /// One hour.
    Hour,
    /// Until the next 08:00 local time.
    UntilMorning,
}

impl PauseChoice {
    /// Every choice, shortest first.
    pub(crate) const ALL: [Self; 3] = [Self::HalfHour, Self::Hour, Self::UntilMorning];

    /// When a pause chosen at `now` ends.
    pub(crate) fn until(self, now: Timestamp) -> Timestamp {
        match self {
            Self::HalfHour => later(now, 30 * 60),
            Self::Hour => later(now, 60 * 60),
            Self::UntilMorning => next_morning(now),
        }
    }

    /// What the choice reads as after "pause": `for 30 minutes`, `for 1
    /// hour`, `until 08:00` (before 08:00) or `until tomorrow 08:00`.
    pub(crate) fn label(self, now: Timestamp) -> String {
        match self {
            Self::HalfHour => "for 30 minutes".to_owned(),
            Self::Hour => "for 1 hour".to_owned(),
            Self::UntilMorning => format!("until {}", when(next_morning(now), now)),
        }
    }

    /// A short label for buttons: `30m`, `1h`, `08:00`.
    pub(crate) fn short_label(self) -> &'static str {
        match self {
            Self::HalfHour => "30m",
            Self::Hour => "1h",
            Self::UntilMorning => "until 08:00",
        }
    }
}

/// How long to mute an object for (a watch has no end).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum MuteChoice {
    /// One hour.
    Hour,
    /// Four hours.
    FourHours,
    /// Until the next 08:00 local time.
    UntilMorning,
    /// Until unmuted.
    Indefinitely,
}

impl MuteChoice {
    /// Every choice, shortest first.
    pub(crate) const ALL: [Self; 4] = [
        Self::Hour,
        Self::FourHours,
        Self::UntilMorning,
        Self::Indefinitely,
    ];

    /// When a mute chosen at `now` ends (`None`: when unmuted).
    pub(crate) fn until(self, now: Timestamp) -> Option<Timestamp> {
        match self {
            Self::Hour => Some(later(now, 60 * 60)),
            Self::FourHours => Some(later(now, 4 * 60 * 60)),
            Self::UntilMorning => Some(next_morning(now)),
            Self::Indefinitely => None,
        }
    }

    /// What the choice reads as after "mute": `for 1 hour`, `for 4
    /// hours`, `until tomorrow 08:00`, `until unmuted`.
    pub(crate) fn label(self, now: Timestamp) -> String {
        match self {
            Self::Hour => "for 1 hour".to_owned(),
            Self::FourHours => "for 4 hours".to_owned(),
            Self::UntilMorning => format!("until {}", when(next_morning(now), now)),
            Self::Indefinitely => "until unmuted".to_owned(),
        }
    }
}

/// The next 08:00 local time after `now` (later today before 08:00, else
/// tomorrow); a day from now if the calendar can't say (a time zone gap).
pub(crate) fn next_morning(now: Timestamp) -> Timestamp {
    next_morning_in(now, &Local)
}

/// [`next_morning`] in time zone `zone`.
fn next_morning_in<Tz: TimeZone>(now: Timestamp, zone: &Tz) -> Timestamp {
    let fallback = later(now, 86_400);
    let Some(local) = local_time(now, zone) else {
        return fallback;
    };
    let Some(morning) = NaiveTime::from_hms_opt(MORNING_HOUR, 0, 0) else {
        return fallback;
    };
    let at = |day: chrono::NaiveDate| zone.from_local_datetime(&day.and_time(morning)).earliest();
    let today = local.date_naive();
    let end = at(today)
        .filter(|end| end.timestamp() > local.timestamp())
        .or_else(|| today.checked_add_days(Days::new(1)).and_then(at));
    end.map_or(fallback, |end| {
        #[expect(
            clippy::cast_precision_loss,
            reason = "Unix seconds fit an f64 exactly"
        )]
        let seconds = end.timestamp() as f64;
        Timestamp::from_unix_seconds(seconds)
    })
}

/// When something ends, as a person says it: `18:30` today, `tomorrow
/// 08:00`, else the date and time (`Oct 9 08:00`).
pub(crate) fn when(at: Timestamp, now: Timestamp) -> String {
    when_in(at, now, &Local)
}

/// [`when`] in time zone `zone`.
fn when_in<Tz>(at: Timestamp, now: Timestamp, zone: &Tz) -> String
where
    Tz: TimeZone,
    Tz::Offset: std::fmt::Display,
{
    let (Some(local_at), Some(local_now)) = (local_time(at, zone), local_time(now, zone)) else {
        return "—".to_owned();
    };
    let tomorrow = local_now.date_naive().checked_add_days(Days::new(1));
    if local_at.date_naive() == local_now.date_naive() {
        local_at.format("%H:%M").to_string()
    } else if Some(local_at.date_naive()) == tomorrow {
        local_at.format("tomorrow %H:%M").to_string()
    } else {
        local_at.format("%b %-d %H:%M").to_string()
    }
}

/// `now` plus `seconds`.
fn later(now: Timestamp, seconds: u32) -> Timestamp {
    Timestamp::from_unix_seconds(now.as_unix_seconds() + f64::from(seconds))
}

/// `at` in time zone `zone`, to the second.
fn local_time<Tz: TimeZone>(at: Timestamp, zone: &Tz) -> Option<DateTime<Tz>> {
    let seconds = at.as_unix_seconds().floor();
    if !(-1e15..1e15).contains(&seconds) {
        return None;
    }
    #[expect(
        clippy::cast_possible_truncation,
        reason = "checked to be well inside i64's range above"
    )]
    let whole = seconds as i64;
    Some(DateTime::from_timestamp(whole, 0)?.with_timezone(zone))
}

#[cfg(test)]
mod tests {
    use chrono::{FixedOffset, Utc};

    use super::*;

    /// 2026-10-06 `hour`:`minute` UTC.
    fn at(hour: u32, minute: u32) -> Timestamp {
        let at = Utc
            .with_ymd_and_hms(2026, 10, 6, hour, minute, 0)
            .single()
            .unwrap();
        #[expect(clippy::cast_precision_loss, reason = "test times")]
        Timestamp::from_unix_seconds(at.timestamp() as f64)
    }

    #[test]
    fn mornings_are_the_next_eight_o_clock() {
        // Before 08:00 it's this morning; after, tomorrow's.
        assert_eq!(next_morning_in(at(5, 30), &Utc), at(8, 0));
        let tomorrow = Timestamp::from_unix_seconds(at(8, 0).as_unix_seconds() + 86_400.);
        assert_eq!(next_morning_in(at(18, 0), &Utc), tomorrow);
        assert_eq!(
            next_morning_in(at(8, 0), &Utc),
            tomorrow,
            "at 08:00 sharp the next one is tomorrow's"
        );
        // In another zone: 08:00 at UTC+2 is 06:00 UTC.
        let zone = FixedOffset::east_opt(2 * 3600).unwrap();
        assert_eq!(next_morning_in(at(5, 0), &zone), at(6, 0));
    }

    #[test]
    fn pauses_and_mutes_end_when_they_say() {
        let now = at(12, 0);
        assert_eq!(PauseChoice::HalfHour.until(now), at(12, 30));
        assert_eq!(PauseChoice::Hour.until(now), at(13, 0));
        assert!(PauseChoice::UntilMorning.until(now) > now);
        assert_eq!(MuteChoice::Hour.until(now), Some(at(13, 0)));
        assert_eq!(MuteChoice::FourHours.until(now), Some(at(16, 0)));
        assert_eq!(MuteChoice::Indefinitely.until(now), None);
        assert_eq!(PauseChoice::HalfHour.label(now), "for 30 minutes");
        assert_eq!(MuteChoice::Indefinitely.label(now), "until unmuted");
        assert!(PauseChoice::UntilMorning.label(now).starts_with("until "));
    }

    #[test]
    fn ends_read_as_today_tomorrow_or_a_date() {
        let now = at(12, 0);
        assert_eq!(when_in(at(18, 30), now, &Utc), "18:30");
        assert_eq!(
            when_in(next_morning_in(now, &Utc), now, &Utc),
            "tomorrow 08:00"
        );
        let later = Timestamp::from_unix_seconds(at(8, 0).as_unix_seconds() + 3. * 86_400.);
        assert_eq!(when_in(later, now, &Utc), "Oct 9 08:00");
    }
}
