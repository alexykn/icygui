//! Text formats shared by the list, the panes and the sidebar: times in
//! state, relative and clock times, check intervals, attempts and state
//! words.

use std::fmt::Display;
use std::time::Duration;

use chrono::{DateTime, Local, TimeZone};
use ic_model::{
    CheckInfo, CheckableState, HostState, ServiceState, StateType, Timestamp, format_compact,
    format_two_units,
};

/// Time in state, as list rows show it under the circle (`14m`). Empty when
/// the state never changed (pending objects).
pub(crate) fn since(at: Timestamp, now: Timestamp) -> String {
    at.non_zero()
        .map(|at| format_compact(at.elapsed_until(now)))
        .unwrap_or_default()
}

/// How long the object has been in its state ([`since`] of
/// [`ic_core::snapshot::state_since`]: Icinga's `last_state_change`, or the
/// last hard change when Icinga reports 0 for an object that has had its
/// state since the first check).
pub(crate) fn time_in_state(check: &CheckInfo, now: Timestamp) -> String {
    since(ic_core::snapshot::state_since(check), now)
}

/// How long ago `at` was: `12s ago`, or `never`.
pub(crate) fn ago(at: Option<Timestamp>, now: Timestamp) -> String {
    match at.and_then(Timestamp::non_zero) {
        Some(at) => format!("{} ago", format_compact(at.elapsed_until(now))),
        None => "never".to_owned(),
    }
}

/// How long until `at`: `in 48s`; `due` once it has passed; `not scheduled`
/// without a time.
pub(crate) fn until(at: Option<Timestamp>, now: Timestamp) -> String {
    match at.and_then(Timestamp::non_zero) {
        Some(at) if at > now => format!("in {}", format_compact(at.remaining_from(now))),
        Some(_) => "due".to_owned(),
        None => "not scheduled".to_owned(),
    }
}

/// A check interval: plain seconds below two minutes (`60s`, as Icinga
/// configurations usually write it), otherwise up to two units (`5m`,
/// `1h 30m`).
pub(crate) fn interval(seconds: f64) -> String {
    let duration = Duration::try_from_secs_f64(seconds).unwrap_or_default();
    if duration.as_secs() < 120 {
        format!("{}s", duration.as_secs())
    } else {
        format_two_units(duration)
    }
}

/// Seconds with a precision that suits their size: `0.004s`, `1.82s`, `12s`.
pub(crate) fn seconds(value: f64) -> String {
    let value = value.max(0.);
    if value >= 10. {
        format!("{value:.0}s")
    } else if value >= 0.1 {
        format!("{value:.2}s")
    } else {
        format!("{value:.3}s")
    }
}

/// The state type and attempt: `hard 3/3`, `soft 2/3`.
pub(crate) fn attempt(check: &CheckInfo) -> String {
    let kind = match check.state_type {
        StateType::Hard => "hard",
        StateType::Soft => "soft",
    };
    format!("{kind} {}/{}", check.attempt, check.max_attempts)
}

/// The state as a word: `critical`, `up`, `unreachable`.
pub(crate) fn state_word(state: CheckableState) -> &'static str {
    match state {
        CheckableState::Service(state) => match state {
            ServiceState::Ok => "ok",
            ServiceState::Warning => "warning",
            ServiceState::Critical => "critical",
            ServiceState::Unknown => "unknown",
            ServiceState::Pending => "pending",
        },
        CheckableState::Host(state) => match state {
            HostState::Up => "up",
            HostState::Down => "down",
            HostState::Unreachable => "unreachable",
            HostState::Pending => "pending",
        },
    }
}

/// A local clock time: `13:58` on the same day as `now`, `Oct 3 13:58` on
/// other days.
pub(crate) fn clock(at: Timestamp, now: Timestamp) -> String {
    clock_in(at, now, &Local)
}

/// [`clock`] in time zone `zone`.
pub(crate) fn clock_in<Tz>(at: Timestamp, now: Timestamp, zone: &Tz) -> String
where
    Tz: TimeZone,
    Tz::Offset: Display,
{
    let (Some(at), Some(now)) = (date_time(at, zone), date_time(now, zone)) else {
        return "—".to_owned();
    };
    if at.date_naive() == now.date_naive() {
        at.format("%H:%M").to_string()
    } else {
        at.format("%b %-d %H:%M").to_string()
    }
}

/// A local calendar date: `2027-03-14`.
pub(crate) fn date(at: Timestamp) -> String {
    date_time(at, &Local).map_or_else(|| "—".to_owned(), |at| at.format("%Y-%m-%d").to_string())
}

/// When a certificate stops being valid, as the trust dialog shows it:
/// `2027-03-14 · in 159d`, or `2025-01-02 · expired 3d ago`; and whether
/// it has expired.
pub(crate) fn expiry(not_after: Timestamp, now: Timestamp) -> (String, bool) {
    if not_after > now {
        (
            format!(
                "{} · in {}",
                date(not_after),
                format_compact(not_after.remaining_from(now))
            ),
            false,
        )
    } else {
        (
            format!(
                "{} · expired {} ago",
                date(not_after),
                format_compact(not_after.elapsed_until(now))
            ),
            true,
        )
    }
}

fn date_time<Tz: TimeZone>(at: Timestamp, zone: &Tz) -> Option<DateTime<Tz>> {
    let seconds = at.as_unix_seconds().floor();
    // Icinga's timestamps are well inside i64's range; anything else is
    // garbage and shows as unknown.
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
mod expiry_tests {
    use super::*;

    #[test]
    fn expiry_says_how_long_is_left_or_since_when_it_expired() {
        let now = Timestamp::from_unix_seconds(1_790_000_000.);
        let (valid, expired) = expiry(
            Timestamp::from_unix_seconds(1_790_000_000. + 86_400. * 3.),
            now,
        );
        assert!(valid.ends_with(" · in 3d"), "{valid}");
        assert!(!expired);
        let (gone, expired) = expiry(Timestamp::from_unix_seconds(1_790_000_000. - 7_200.), now);
        assert!(gone.ends_with(" · expired 2h ago"), "{gone}");
        assert!(expired);
        assert_eq!(date(now).len(), 10);
    }
}

#[cfg(test)]
mod tests {
    use chrono::FixedOffset;

    use super::*;

    fn at(seconds: f64) -> Timestamp {
        Timestamp::from_unix_seconds(seconds)
    }

    const NOW: f64 = 1_790_000_000.;

    #[test]
    fn times_in_state_use_one_unit() {
        assert_eq!(since(at(NOW - 14. * 60.), at(NOW)), "14m");
        assert_eq!(since(at(NOW - 3. * 3600. - 5.), at(NOW)), "3h");
        assert_eq!(
            since(Timestamp::EPOCH, at(NOW)),
            "",
            "pending: never changed"
        );
        assert_eq!(
            since(at(NOW + 5.), at(NOW)),
            "0s",
            "clock skew stays positive"
        );
    }

    #[test]
    fn relative_times() {
        assert_eq!(ago(Some(at(NOW - 12.)), at(NOW)), "12s ago");
        assert_eq!(ago(None, at(NOW)), "never");
        assert_eq!(ago(Some(Timestamp::EPOCH), at(NOW)), "never");
        assert_eq!(until(Some(at(NOW + 48.)), at(NOW)), "in 48s");
        assert_eq!(until(Some(at(NOW - 1.)), at(NOW)), "due");
        assert_eq!(until(None, at(NOW)), "not scheduled");
    }

    #[test]
    fn intervals_read_like_icinga_configs() {
        assert_eq!(interval(60.), "60s");
        assert_eq!(interval(15.), "15s");
        assert_eq!(interval(300.), "5m");
        assert_eq!(interval(5400.), "1h 30m");
        assert_eq!(interval(f64::NAN), "0s");
        assert_eq!(interval(-3.), "0s");
    }

    #[test]
    fn seconds_keep_meaningful_digits() {
        assert_eq!(seconds(0.004), "0.004s");
        assert_eq!(seconds(1.8234), "1.82s");
        assert_eq!(seconds(12.4), "12s");
        assert_eq!(seconds(-1.), "0.000s");
    }

    #[test]
    fn attempts_name_the_state_type() {
        let mut check = CheckInfo {
            attempt: 3,
            max_attempts: 3,
            ..CheckInfo::default()
        };
        assert_eq!(attempt(&check), "hard 3/3");
        check.state_type = StateType::Soft;
        check.attempt = 2;
        assert_eq!(attempt(&check), "soft 2/3");
    }

    #[test]
    fn state_words() {
        assert_eq!(
            state_word(CheckableState::Service(ServiceState::Critical)),
            "critical"
        );
        assert_eq!(state_word(CheckableState::Host(HostState::Up)), "up");
        assert_eq!(
            state_word(CheckableState::Host(HostState::Unreachable)),
            "unreachable"
        );
    }

    #[test]
    fn clock_times_show_the_date_only_for_other_days() {
        let zone = FixedOffset::east_opt(2 * 3600).unwrap();
        // 2026-09-21 14:13:20 UTC = 16:13 at +02:00.
        let now = at(NOW);
        assert_eq!(clock_in(at(NOW - 34. * 60.), now, &zone), "15:39");
        assert_eq!(clock_in(at(NOW - 3. * 86_400.), now, &zone), "Sep 18 16:13");
        assert_eq!(
            clock_in(at(f64::MAX), now, &zone),
            "—",
            "garbage is unknown"
        );
    }
}
