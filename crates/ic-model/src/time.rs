//! Timestamps as Icinga reports them, and compact durations for display.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// A Unix timestamp in seconds with sub-second precision (Icinga's wire
/// format). Icinga uses `0` for "never", see [`Timestamp::non_zero`].
#[derive(Clone, Copy, Debug, Default, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Timestamp(f64);

impl Timestamp {
    /// The Unix epoch, Icinga's "never".
    pub const EPOCH: Self = Self(0.0);

    /// From Unix seconds.
    #[must_use]
    pub fn from_unix_seconds(seconds: f64) -> Self {
        Self(if seconds.is_finite() { seconds } else { 0.0 })
    }

    /// The current wall-clock time.
    #[must_use]
    pub fn now() -> Self {
        let since_epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        Self(since_epoch.as_secs_f64())
    }

    /// Unix seconds.
    #[must_use]
    pub fn as_unix_seconds(self) -> f64 {
        self.0
    }

    /// `None` for Icinga's "never" (zero or negative), otherwise `Some(self)`.
    #[must_use]
    pub fn non_zero(self) -> Option<Self> {
        (self.0 > 0.0).then_some(self)
    }

    /// Time elapsed from `self` until `now`; zero if `now` is earlier.
    #[must_use]
    pub fn elapsed_until(self, now: Self) -> Duration {
        Duration::try_from_secs_f64(now.0 - self.0).unwrap_or_default()
    }

    /// Time from `now` until `self`; zero if `self` is in the past.
    #[must_use]
    pub fn remaining_from(self, now: Self) -> Duration {
        Duration::try_from_secs_f64(self.0 - now.0).unwrap_or_default()
    }

    /// `self` moved forward by `duration`.
    #[must_use]
    pub fn plus(self, duration: Duration) -> Self {
        Self(self.0 + duration.as_secs_f64())
    }
}

/// Formats a duration the way the design shows time-in-state: one unit,
/// rounded down (`12s`, `14m`, `2h`, `41d`).
#[must_use]
pub fn format_compact(duration: Duration) -> String {
    let seconds = duration.as_secs();
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3_600 => format!("{}m", seconds / 60),
        3_600..86_400 => format!("{}h", seconds / 3_600),
        _ => format!("{}d", seconds / 86_400),
    }
}

/// Formats a duration with up to two units (`2h 4m`, `3d 1h`, `45s`), for
/// places with more room such as tooltips and the detail pane.
#[must_use]
pub fn format_two_units(duration: Duration) -> String {
    let seconds = duration.as_secs();
    let (days, hours, minutes, secs) = (
        seconds / 86_400,
        seconds % 86_400 / 3_600,
        seconds % 3_600 / 60,
        seconds % 60,
    );
    match (days, hours, minutes) {
        (0, 0, 0) => format!("{secs}s"),
        (0, 0, m) if secs == 0 => format!("{m}m"),
        (0, 0, m) => format!("{m}m {secs}s"),
        (0, h, 0) => format!("{h}h"),
        (0, h, m) => format!("{h}h {m}m"),
        (d, 0, _) => format!("{d}d"),
        (d, h, _) => format!("{d}d {h}h"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_durations_use_one_unit() {
        assert_eq!(format_compact(Duration::from_secs(12)), "12s");
        assert_eq!(
            format_compact(Duration::from_mins(14) + Duration::from_secs(59)),
            "14m"
        );
        assert_eq!(
            format_compact(Duration::from_hours(2) + Duration::from_mins(2)),
            "2h"
        );
        assert_eq!(format_compact(Duration::from_hours(41 * 24)), "41d");
    }

    #[test]
    fn two_unit_durations() {
        assert_eq!(format_two_units(Duration::from_secs(45)), "45s");
        assert_eq!(format_two_units(Duration::from_mins(2)), "2m");
        assert_eq!(format_two_units(Duration::from_secs(125)), "2m 5s");
        assert_eq!(
            format_two_units(Duration::from_hours(2) + Duration::from_mins(4)),
            "2h 4m"
        );
        assert_eq!(format_two_units(Duration::from_hours(3 * 24 + 1)), "3d 1h");
        assert_eq!(
            format_two_units(Duration::from_hours(3 * 24) + Duration::from_secs(59)),
            "3d"
        );
    }

    #[test]
    fn elapsed_and_remaining_never_go_negative() {
        let earlier = Timestamp::from_unix_seconds(100.0);
        let later = Timestamp::from_unix_seconds(160.5);
        assert_eq!(earlier.elapsed_until(later), Duration::from_millis(60_500));
        assert_eq!(later.elapsed_until(earlier), Duration::ZERO);
        assert_eq!(later.remaining_from(earlier), Duration::from_millis(60_500));
        assert_eq!(earlier.remaining_from(later), Duration::ZERO);
    }

    #[test]
    fn zero_means_never() {
        assert_eq!(Timestamp::EPOCH.non_zero(), None);
        assert!(Timestamp::from_unix_seconds(1.0).non_zero().is_some());
        assert_eq!(Timestamp::from_unix_seconds(f64::NAN), Timestamp::EPOCH);
    }
}
