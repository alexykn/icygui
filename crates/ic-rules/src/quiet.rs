//! Quiet hours: whether a local time falls inside the configured window.

use crate::intent::LocalTime;
use crate::settings::QuietHours;

const MINUTES_PER_DAY: u16 = 24 * 60;

/// Whether `local` is inside the quiet-hours window.
///
/// - `start < end`: a window within one day, `[start, end)`, on the
///   selected days.
/// - `start > end`: the window crosses midnight. It belongs to the day it
///   starts on, so Friday 22:00–07:00 covers Friday from 22:00 and Saturday
///   until 07:00, but not Friday morning.
/// - `start == end`: a full 24 hours from `start` on the selected days
///   (00:00–00:00 = the whole day).
///
/// `end = 1440` means midnight at the end of the day. A start at or after
/// 1440, or an end after it, is a corrupt config: the window is ignored
/// rather than guessed at, because an alerting tool should rather notify
/// than stay silent. The caller's local time is clamped (weekday modulo 7,
/// minute to 23:59).
pub(crate) fn is_quiet(quiet: &QuietHours, local: LocalTime) -> bool {
    let (start, end) = (quiet.start_minute, quiet.end_minute);
    if !quiet.enabled || start >= MINUTES_PER_DAY || end > MINUTES_PER_DAY {
        return false;
    }
    let today = usize::from(local.weekday % 7);
    let yesterday = (today + 6) % 7;
    let selected = |day: usize| quiet.days.get(day).copied().unwrap_or(false);
    let minute = local.minute_of_day.min(MINUTES_PER_DAY - 1);
    if start < end {
        selected(today) && (start..end).contains(&minute)
    } else {
        (selected(today) && minute >= start) || (selected(yesterday) && minute < end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MONDAY: u8 = 0;
    const FRIDAY: u8 = 4;
    const SATURDAY: u8 = 5;
    const SUNDAY: u8 = 6;

    fn at(weekday: u8, hour: u16, minute: u16) -> LocalTime {
        LocalTime {
            weekday,
            minute_of_day: hour * 60 + minute,
        }
    }

    fn window(start: u16, end: u16, days: [bool; 7]) -> QuietHours {
        QuietHours {
            enabled: true,
            start_minute: start,
            end_minute: end,
            days,
            allow_critical: false,
        }
    }

    const ONLY_FRIDAY: [bool; 7] = [false, false, false, false, true, false, false];

    #[test]
    fn disabled_is_never_quiet() {
        let quiet = QuietHours {
            enabled: false,
            ..window(0, 0, [true; 7])
        };
        assert!(!is_quiet(&quiet, at(MONDAY, 3, 0)));
    }

    #[test]
    fn same_day_windows_are_half_open() {
        let quiet = window(12 * 60, 13 * 60, ONLY_FRIDAY);
        assert!(!is_quiet(&quiet, at(FRIDAY, 11, 59)));
        assert!(is_quiet(&quiet, at(FRIDAY, 12, 0)));
        assert!(is_quiet(&quiet, at(FRIDAY, 12, 59)));
        assert!(!is_quiet(&quiet, at(FRIDAY, 13, 0)));
        assert!(
            !is_quiet(&quiet, at(SATURDAY, 12, 30)),
            "not a selected day"
        );
    }

    #[test]
    fn midnight_crossing_windows_belong_to_their_start_day() {
        let quiet = window(22 * 60, 7 * 60, ONLY_FRIDAY);
        assert!(!is_quiet(&quiet, at(FRIDAY, 21, 59)));
        assert!(is_quiet(&quiet, at(FRIDAY, 22, 0)));
        assert!(is_quiet(&quiet, at(FRIDAY, 23, 59)));
        assert!(
            is_quiet(&quiet, at(SATURDAY, 0, 0)),
            "Friday's window goes on"
        );
        assert!(is_quiet(&quiet, at(SATURDAY, 6, 59)));
        assert!(!is_quiet(&quiet, at(SATURDAY, 7, 0)));
        assert!(
            !is_quiet(&quiet, at(SATURDAY, 22, 30)),
            "Saturday isn't selected"
        );
        assert!(
            !is_quiet(&quiet, at(FRIDAY, 3, 0)),
            "Friday morning belongs to Thursday's window"
        );
    }

    #[test]
    fn sunday_windows_wrap_to_monday() {
        let mut days = [false; 7];
        days[usize::from(SUNDAY)] = true;
        let quiet = window(23 * 60, 60, days);
        assert!(is_quiet(&quiet, at(SUNDAY, 23, 30)));
        assert!(is_quiet(&quiet, at(MONDAY, 0, 30)));
        assert!(!is_quiet(&quiet, at(MONDAY, 1, 0)));
        assert!(!is_quiet(&quiet, at(MONDAY, 23, 30)));
    }

    #[test]
    fn equal_start_and_end_mean_a_full_day() {
        let weekend = [false, false, false, false, false, true, true];
        let quiet = window(0, 0, weekend);
        assert!(is_quiet(&quiet, at(SATURDAY, 0, 0)));
        assert!(is_quiet(&quiet, at(SUNDAY, 23, 59)));
        assert!(!is_quiet(&quiet, at(MONDAY, 0, 0)));
        assert!(!is_quiet(&quiet, at(FRIDAY, 23, 59)));

        let from_nine = window(9 * 60, 9 * 60, ONLY_FRIDAY);
        assert!(!is_quiet(&from_nine, at(FRIDAY, 8, 59)));
        assert!(is_quiet(&from_nine, at(FRIDAY, 9, 0)));
        assert!(is_quiet(&from_nine, at(SATURDAY, 8, 59)));
        assert!(!is_quiet(&from_nine, at(SATURDAY, 9, 0)));
    }

    #[test]
    fn window_until_end_of_day() {
        let quiet = window(20 * 60, MINUTES_PER_DAY, ONLY_FRIDAY);
        assert!(is_quiet(&quiet, at(FRIDAY, 23, 59)));
        assert!(!is_quiet(&quiet, at(SATURDAY, 0, 0)));
    }

    #[test]
    fn out_of_range_local_times_are_clamped() {
        let quiet = window(22 * 60, 7 * 60, [true; 7]);
        assert!(is_quiet(&quiet, at(FRIDAY + 7, 23, 0)), "weekday modulo 7");
        assert!(
            is_quiet(
                &quiet,
                LocalTime {
                    weekday: FRIDAY,
                    minute_of_day: 5_000
                }
            ),
            "minute clamped to 23:59"
        );
    }

    #[test]
    fn corrupt_windows_fail_open() {
        for (start, end) in [
            (5_000, 6_000),
            (MINUTES_PER_DAY, MINUTES_PER_DAY),
            (MINUTES_PER_DAY, 0),
            (22 * 60, MINUTES_PER_DAY + 1),
            (0, u16::MAX),
        ] {
            let quiet = window(start, end, [true; 7]);
            for hour in 0..24 {
                assert!(
                    !is_quiet(&quiet, at(FRIDAY, hour, 30)),
                    "{start}–{end} at {hour}:30"
                );
            }
        }
    }
}
