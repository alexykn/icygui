//! Times and durations as the action dialogs take them: typed by an
//! engineer in a hurry, so several forms are accepted and the dialog shows
//! what each one means before anything is sent.
//!
//! - Durations: `30m`, `2h`, `1h30m`, `1h 30m`, `1d`, `1w`, `45s`. A bare
//!   number is refused (minutes or hours?): the message asks for a unit.
//! - Times: `now`; a duration from a base (`2h`, `+2h`, `in 2h`: from now
//!   for a start or an expiry, from the start for an end); a clock time
//!   (`14:30`, the next one after the base); `today 18:00`, `tomorrow
//!   08:00`, `tomorrow` (08:00); a date with or without a time
//!   (`2026-10-06 14:30`, `2026-10-06T14:30:00`, `06.10.2026 14:30`).
//!
//! Everything is local time. Pure and generic over the time zone, so the
//! tests pin one.

use std::time::Duration;

#[cfg(test)]
use chrono::Timelike as _;
use chrono::{DateTime, Datelike as _, Days, Local, NaiveDate, NaiveDateTime, NaiveTime, TimeZone};
use ic_model::{Timestamp, format_two_units};

/// The hour "tomorrow" means without a time: the start of a working day.
const MORNING_HOUR: u32 = 8;

/// A quick choice next to a time field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Preset {
    /// The chip's label.
    pub(crate) label: &'static str,
    /// What choosing it types into the field.
    pub(crate) text: &'static str,
}

/// Quick choices for a downtime's end (from its start).
pub(crate) const END_PRESETS: [Preset; 8] = [
    Preset {
        label: "30m",
        text: "+30m",
    },
    Preset {
        label: "1h",
        text: "+1h",
    },
    Preset {
        label: "2h",
        text: "+2h",
    },
    Preset {
        label: "4h",
        text: "+4h",
    },
    Preset {
        label: "8h",
        text: "+8h",
    },
    Preset {
        label: "1d",
        text: "+1d",
    },
    Preset {
        label: "1w",
        text: "+1w",
    },
    Preset {
        label: "08:00 tomorrow",
        text: "tomorrow 08:00",
    },
];

/// Quick choices for when an acknowledgement or comment expires (from now).
pub(crate) const EXPIRY_PRESETS: [Preset; 4] = [
    Preset {
        label: "1h",
        text: "+1h",
    },
    Preset {
        label: "4h",
        text: "+4h",
    },
    Preset {
        label: "1d",
        text: "+1d",
    },
    Preset {
        label: "08:00 tomorrow",
        text: "tomorrow 08:00",
    },
];

/// Parses a duration (`30m`, `1h30m`, `1h 30m`, `2d`, `1w`, `45s`).
///
/// # Errors
///
/// A message for the field: empty, a number without a unit, an unknown
/// unit, or zero.
pub(crate) fn parse_duration(text: &str) -> Result<Duration, String> {
    let compact: String = text.trim().chars().filter(|c| !c.is_whitespace()).collect();
    let compact = compact.to_lowercase();
    if compact.is_empty() {
        return Err("Enter a duration, like 30m, 2h or 1d.".to_owned());
    }
    let mut total: u64 = 0;
    let mut number = String::new();
    let mut units = 0;
    let mut chars = compact.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_ascii_digit() {
            number.push(c);
            continue;
        }
        if number.is_empty() {
            return Err(format!(
                "“{}” isn't a duration (try 30m, 2h, 1d).",
                text.trim()
            ));
        }
        // `ms` would be the only two-letter unit; nobody schedules
        // milliseconds, so letters after the first are an error.
        let unit = match c {
            's' => 1,
            'm' => 60,
            'h' => 3_600,
            'd' => 86_400,
            'w' => 604_800,
            _ => {
                return Err(format!(
                    "Unknown unit “{c}”: use s, m, h, d or w (30m, 2h, 1d)."
                ));
            }
        };
        if chars.peek().is_some_and(char::is_ascii_alphabetic) {
            return Err(format!(
                "“{}” isn't a duration (try 30m, 2h, 1d).",
                text.trim()
            ));
        }
        let value: u64 = number
            .parse()
            .map_err(|_| "That duration is too long.".to_owned())?;
        total = value
            .checked_mul(unit)
            .and_then(|seconds| total.checked_add(seconds))
            .ok_or_else(|| "That duration is too long.".to_owned())?;
        number.clear();
        units += 1;
    }
    if !number.is_empty() {
        return Err(if units == 0 {
            format!(
                "Add a unit: {number}m, {number}h or {number}d?",
                number = number.trim_start_matches('0').max("0")
            )
        } else {
            "Every number needs a unit (1h30m).".to_owned()
        });
    }
    // Ten years: anything longer is a typo.
    if total > 10 * 365 * 86_400 {
        return Err("That duration is too long.".to_owned());
    }
    if total == 0 {
        return Err("The duration must be longer than zero.".to_owned());
    }
    Ok(Duration::from_secs(total))
}

/// Parses a time in the local time zone (see the module docs).
///
/// # Errors
///
/// A message for the field.
pub(crate) fn parse_time(text: &str, base: Timestamp, now: Timestamp) -> Result<Timestamp, String> {
    parse_time_in(text, base, now, &Local)
}

/// [`parse_time`] in `zone`. `base` is what relative times and clock
/// times count from; `now` is what `now`, `today` and `tomorrow` mean.
///
/// # Errors
///
/// A message for the field.
pub(crate) fn parse_time_in<Tz: TimeZone>(
    text: &str,
    base: Timestamp,
    now: Timestamp,
    zone: &Tz,
) -> Result<Timestamp, String> {
    let trimmed = text.trim();
    let lower = trimmed.to_lowercase();
    if lower.is_empty() {
        return Err("Enter a time, like now, 14:30, +2h or 2026-10-06 14:30.".to_owned());
    }
    if lower == "now" {
        return Ok(now);
    }
    let relative = lower
        .strip_prefix('+')
        .or_else(|| lower.strip_prefix("in "))
        .map(str::trim);
    if let Some(duration) = relative {
        return parse_duration(duration).map(|duration| add(base, duration));
    }
    if lower.starts_with(|c: char| c.is_ascii_digit())
        && lower.ends_with(|c: char| c.is_ascii_alphabetic())
        && let Ok(duration) = parse_duration(&lower)
    {
        return Ok(add(base, duration));
    }
    let now_local = local(now, zone)?;
    if let Some(rest) = lower.strip_prefix("tomorrow") {
        let day = now_local
            .date_naive()
            .checked_add_days(Days::new(1))
            .ok_or_else(|| "That date is out of range.".to_owned())?;
        let time = match rest.trim() {
            "" => morning(),
            clock => parse_clock(clock)?,
        };
        return at(zone, day.and_time(time));
    }
    if let Some(rest) = lower.strip_prefix("today") {
        let time = match rest.trim() {
            "" => return Err("Add a time: today 18:00.".to_owned()),
            clock => parse_clock(clock)?,
        };
        return at(zone, now_local.date_naive().and_time(time));
    }
    if let Ok(time) = parse_clock(&lower) {
        // The next such time after the base: `08:00` typed in the
        // afternoon means tomorrow morning.
        let base_local = local(base, zone)?;
        let today = at(zone, base_local.date_naive().and_time(time))?;
        if today > base {
            return Ok(today);
        }
        let tomorrow = base_local
            .date_naive()
            .checked_add_days(Days::new(1))
            .ok_or_else(|| "That date is out of range.".to_owned())?;
        return at(zone, tomorrow.and_time(time));
    }
    if let Some(date_time) = parse_date_time(trimmed) {
        return at(zone, date_time);
    }
    Err(format!(
        "“{trimmed}” isn't a time: try now, 14:30, +2h, tomorrow 08:00 or 2026-10-06 14:30."
    ))
}

/// What a parsed time means, for the field's status: `Tue 6 Oct 15:30 ·
/// in 1h` (`today`/`tomorrow` instead of the date when they apply).
pub(crate) fn describe(at: Timestamp, now: Timestamp) -> String {
    describe_in(at, now, &Local)
}

/// [`describe`] in `zone`.
pub(crate) fn describe_in<Tz: TimeZone>(at: Timestamp, now: Timestamp, zone: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let (Ok(when), Ok(today)) = (local(at, zone), local(now, zone)) else {
        return String::new();
    };
    let day_offset = when
        .date_naive()
        .signed_duration_since(today.date_naive())
        .num_days();
    let day = match day_offset {
        0 => "today".to_owned(),
        1 => "tomorrow".to_owned(),
        -1 => "yesterday".to_owned(),
        _ if when.year() == today.year() => when.format("%a %-d %b").to_string(),
        _ => when.format("%a %-d %b %Y").to_string(),
    };
    let clock = when.format("%H:%M").to_string();
    let distance = if at > now {
        let ahead = at.remaining_from(now);
        if ahead.as_secs() < 60 {
            "now".to_owned()
        } else {
            format!("in {}", format_two_units(round_to_minutes(ahead)))
        }
    } else {
        let behind = at.elapsed_until(now);
        if behind.as_secs() < 60 {
            "now".to_owned()
        } else {
            format!("{} ago", format_two_units(round_to_minutes(behind)))
        }
    };
    format!("{day} {clock} · {distance}")
}

fn round_to_minutes(duration: Duration) -> Duration {
    Duration::from_secs((duration.as_secs() + 30) / 60 * 60)
}

fn add(base: Timestamp, duration: Duration) -> Timestamp {
    Timestamp::from_unix_seconds(base.as_unix_seconds() + duration.as_secs_f64())
}

fn morning() -> NaiveTime {
    NaiveTime::from_hms_opt(MORNING_HOUR, 0, 0).unwrap_or(NaiveTime::MIN)
}

/// `at` in `zone`.
fn local<Tz: TimeZone>(at: Timestamp, zone: &Tz) -> Result<DateTime<Tz>, String> {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "times typed into a dialog are far inside i64's range"
    )]
    let seconds = at.as_unix_seconds().floor() as i64;
    zone.timestamp_opt(seconds, 0)
        .single()
        .ok_or_else(|| "That time is out of range.".to_owned())
}

/// The moment a local date and time names in `zone`. A time a clock
/// change skips doesn't exist; one it repeats means its first occurrence.
fn at<Tz: TimeZone>(zone: &Tz, date_time: NaiveDateTime) -> Result<Timestamp, String> {
    let resolved = zone
        .from_local_datetime(&date_time)
        .earliest()
        .ok_or_else(|| {
            format!(
                "{} doesn't exist here (the clocks change then).",
                date_time.format("%Y-%m-%d %H:%M")
            )
        })?;
    #[expect(
        clippy::cast_precision_loss,
        reason = "Unix seconds fit an f64 exactly"
    )]
    let seconds = resolved.timestamp() as f64;
    Ok(Timestamp::from_unix_seconds(seconds))
}

/// `14:30`, `9:05`, `14:30:15`, `1430` is not accepted (ambiguous with a
/// year).
fn parse_clock(text: &str) -> Result<NaiveTime, String> {
    let text = text.trim();
    let parts: Vec<&str> = text.split(':').collect();
    let number = |part: &str, max: u32| -> Option<u32> {
        (!part.is_empty() && part.len() <= 2 && part.chars().all(|c| c.is_ascii_digit()))
            .then(|| part.parse().ok())
            .flatten()
            .filter(|value| *value <= max)
    };
    let time = match parts.as_slice() {
        [hours, minutes] => number(hours, 23)
            .zip(number(minutes, 59))
            .and_then(|(h, m)| NaiveTime::from_hms_opt(h, m, 0)),
        [hours, minutes, seconds] => {
            match (number(hours, 23), number(minutes, 59), number(seconds, 59)) {
                (Some(h), Some(m), Some(s)) => NaiveTime::from_hms_opt(h, m, s),
                _ => None,
            }
        }
        _ => None,
    };
    time.ok_or_else(|| format!("“{text}” isn't a time of day (14:30)."))
}

/// `2026-10-06 14:30[:ss]`, `2026-10-06T14:30[:ss]`, `2026-10-06`,
/// `06.10.2026 14:30`, `06.10.2026`.
fn parse_date_time(text: &str) -> Option<NaiveDateTime> {
    let text = text.trim();
    for format in [
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M",
        "%d.%m.%Y %H:%M:%S",
        "%d.%m.%Y %H:%M",
    ] {
        if let Ok(parsed) = NaiveDateTime::parse_from_str(text, format) {
            return Some(parsed);
        }
    }
    for format in ["%Y-%m-%d", "%d.%m.%Y"] {
        if let Ok(date) = NaiveDate::parse_from_str(text, format) {
            return Some(date.and_time(NaiveTime::MIN));
        }
    }
    None
}

/// Whether a time is `minute`-precise (no seconds), for tests and the
/// field's default.
#[cfg(test)]
fn minute_of<Tz: TimeZone>(at: Timestamp, zone: &Tz) -> (u32, u32) {
    local(at, zone).map_or((0, 0), |time| (time.hour(), time.minute()))
}

#[cfg(test)]
mod tests {
    use chrono::{FixedOffset, Utc};

    use super::*;

    /// Tuesday, 6 October 2026, 14:20:00 UTC.
    fn now() -> Timestamp {
        Timestamp::from_unix_seconds(1_791_296_400.)
    }

    fn utc(text: &str) -> Result<Timestamp, String> {
        parse_time_in(text, now(), now(), &Utc)
    }

    fn hours(value: f64) -> Timestamp {
        Timestamp::from_unix_seconds(now().as_unix_seconds() + value * 3_600.)
    }

    #[test]
    fn durations_need_units_and_combine_them() {
        assert_eq!(parse_duration("30m"), Ok(Duration::from_mins(30)));
        assert_eq!(parse_duration(" 2H "), Ok(Duration::from_hours(2)));
        assert_eq!(parse_duration("1h30m"), Ok(Duration::from_mins(90)));
        assert_eq!(parse_duration("1h 30m"), Ok(Duration::from_mins(90)));
        assert_eq!(parse_duration("1d"), Ok(Duration::from_hours(24)));
        assert_eq!(parse_duration("1w"), Ok(Duration::from_hours(168)));
        assert_eq!(parse_duration("45s"), Ok(Duration::from_secs(45)));
        assert_eq!(
            parse_duration("90").unwrap_err(),
            "Add a unit: 90m, 90h or 90d?"
        );
        assert!(parse_duration("1h30").unwrap_err().contains("needs a unit"));
        assert!(parse_duration("2y").unwrap_err().contains("Unknown unit"));
        assert!(parse_duration("2ms").is_err());
        assert!(parse_duration("h").is_err());
        assert!(parse_duration("").unwrap_err().contains("Enter a duration"));
        assert!(
            parse_duration("0m")
                .unwrap_err()
                .contains("longer than zero")
        );
        assert!(parse_duration("99999999999999999999h").is_err());
        assert!(parse_duration("5000w").unwrap_err().contains("too long"));
    }

    #[test]
    fn relative_times_count_from_the_base() {
        assert_eq!(utc("now"), Ok(now()));
        assert_eq!(utc("+2h"), Ok(hours(2.)));
        assert_eq!(utc("in 30m"), Ok(hours(0.5)));
        assert_eq!(utc("2h"), Ok(hours(2.)));
        assert_eq!(utc("1d"), Ok(hours(24.)));
        // An end counts from its start, `now` is still now.
        let start = hours(1.);
        assert_eq!(parse_time_in("+1h", start, now(), &Utc), Ok(hours(2.)));
        assert_eq!(parse_time_in("now", start, now(), &Utc), Ok(now()));
        assert!(utc("+").is_err());
        assert!(utc("+90").unwrap_err().contains("Add a unit"));
    }

    #[test]
    fn clock_times_mean_the_next_one() {
        // 14:20 now: 16:00 is today, 08:00 tomorrow.
        assert_eq!(utc("16:00"), Ok(hours(1. + 40. / 60.)));
        assert_eq!(utc("08:00"), Ok(hours(17. + 40. / 60.)));
        assert_eq!(utc("14:20"), Ok(hours(24.)), "not this very minute");
        assert_eq!(utc("9:05:30").map(|at| minute_of(at, &Utc)), Ok((9, 5)));
        assert!(utc("24:00").is_err());
        assert!(utc("12:60").is_err());
        assert!(utc("1430").is_err(), "four digits could be a year");
    }

    #[test]
    fn today_and_tomorrow() {
        assert_eq!(utc("tomorrow 08:00"), Ok(hours(17. + 40. / 60.)));
        assert_eq!(utc("Tomorrow"), Ok(hours(17. + 40. / 60.)), "08:00");
        assert_eq!(utc("today 18:00"), Ok(hours(3. + 40. / 60.)));
        assert_eq!(
            utc("today 08:00"),
            Ok(hours(-(6. + 20. / 60.))),
            "today means today, even when it has passed"
        );
        assert!(utc("today").unwrap_err().contains("Add a time"));
        assert!(utc("tomorrow 25:00").is_err());
    }

    #[test]
    fn dates_in_iso_and_german_order() {
        let expected = Ok(hours(26. + 10. / 60.));
        assert_eq!(utc("2026-10-07 16:30"), expected);
        assert_eq!(utc("2026-10-07T16:30"), expected);
        assert_eq!(utc("2026-10-07T16:30:00"), expected);
        assert_eq!(utc("07.10.2026 16:30"), expected);
        assert_eq!(utc("2026-10-07").map(|at| minute_of(at, &Utc)), Ok((0, 0)));
        assert_eq!(utc("07.10.2026").map(|at| minute_of(at, &Utc)), Ok((0, 0)));
        let error = utc("next friday").unwrap_err();
        assert!(error.contains("isn't a time"), "{error}");
        assert!(utc("").unwrap_err().contains("Enter a time"));
        assert!(utc("2026-13-01 10:00").is_err());
    }

    #[test]
    fn times_are_local() {
        // UTC+2: 16:00 local is 14:00 UTC, which has passed at 14:20 UTC.
        let berlin = FixedOffset::east_opt(2 * 3_600).unwrap();
        let at = parse_time_in("16:00", now(), now(), &berlin).unwrap();
        assert_eq!(at, hours(24. - 20. / 60.));
        assert_eq!(
            describe_in(now(), now(), &berlin),
            "today 16:20 · now",
            "shown in local time too"
        );
    }

    #[test]
    fn descriptions_say_when_and_how_far() {
        assert_eq!(describe_in(hours(1.), now(), &Utc), "today 15:20 · in 1h");
        assert_eq!(
            describe_in(hours(17. + 40. / 60.), now(), &Utc),
            "tomorrow 08:00 · in 17h 40m"
        );
        assert_eq!(describe_in(now(), now(), &Utc), "today 14:20 · now");
        assert_eq!(describe_in(hours(-2.), now(), &Utc), "today 12:20 · 2h ago");
        assert_eq!(
            describe_in(hours(-15.), now(), &Utc),
            "yesterday 23:20 · 15h ago"
        );
        assert_eq!(
            describe_in(hours(24. * 3.), now(), &Utc),
            "Fri 9 Oct 14:20 · in 3d"
        );
        assert_eq!(
            describe_in(hours(24. * 400.), now(), &Utc),
            "Wed 10 Nov 2027 14:20 · in 400d"
        );
    }

    #[test]
    fn presets_parse() {
        for preset in END_PRESETS.iter().chain(&EXPIRY_PRESETS) {
            assert!(utc(preset.text).is_ok(), "{}", preset.text);
        }
    }
}
