//! Storm control: many notifications within a short window collapse into
//! one summary.
//!
//! A window opens with the first audible notification and lasts
//! `window_secs`. The first `threshold` notifications inside it are shown;
//! further ones are recorded silently. When the window closes, a summary
//! covers the whole window if anything was silenced. The next notification
//! opens a fresh window, so storms are rate-limited to `threshold + 1` shown
//! notifications per window.

use std::time::Duration;

use ic_model::Timestamp;

use crate::settings::StormControl;
use crate::text::Label;

/// Storm control's decision for one notification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Admission {
    /// Show it.
    Audible,
    /// Record it silently; the window's summary covers it.
    Absorbed,
}

/// A closed window that silenced at least one notification.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StormSummary {
    /// When the window opened.
    pub(crate) started: Timestamp,
    /// Every notification counted in the window, shown or silenced.
    pub(crate) total: u32,
    /// How many of each kind, in [`Label::ALL`] order.
    counts: [u32; Label::ALL.len()],
    /// A silenced notification was critical or down.
    pub(crate) critical: bool,
    /// A silenced notification wanted a sound.
    pub(crate) sound: bool,
}

impl StormSummary {
    /// `"14 new problems in prod-cluster"`, or `"14 notifications in …"`
    /// when the window also held recoveries or other events.
    pub(crate) fn title(&self, environment: &str) -> String {
        let total = self.total;
        let only_problems = Label::ALL
            .into_iter()
            .all(|label| label.is_problem() || self.count(label) == 0);
        let what = match (only_problems, total == 1) {
            (true, true) => "new problem",
            (true, false) => "new problems",
            (false, true) => "notification",
            (false, false) => "notifications",
        };
        if environment.trim().is_empty() {
            format!("{total} {what}")
        } else {
            format!("{total} {what} in {environment}")
        }
    }

    /// The breakdown, worst first: `"12 critical · 2 down · 1 recovered"`.
    pub(crate) fn body(&self) -> String {
        Label::ALL
            .into_iter()
            .filter(|label| self.count(*label) > 0)
            .map(|label| format!("{} {}", self.count(label), label.noun()))
            .collect::<Vec<_>>()
            .join(" · ")
    }

    fn count(&self, label: Label) -> u32 {
        self.counts.get(label.index()).copied().unwrap_or(0)
    }
}

/// The storm window, if one is open.
#[derive(Clone, Debug, Default)]
pub(crate) struct Storm {
    window: Option<Window>,
}

#[derive(Clone, Debug)]
struct Window {
    start: Timestamp,
    end: Timestamp,
    counted: u32,
    absorbed: u32,
    counts: [u32; Label::ALL.len()],
    absorbed_critical: bool,
    absorbed_sound: bool,
}

impl Storm {
    /// Counts a notification that would be shown at `now`, opening a
    /// window if none is open. Callers close finished windows first (see
    /// [`Storm::close_if_over`]). A zero-length window disables storm
    /// control.
    pub(crate) fn admit(
        &mut self,
        now: Timestamp,
        settings: StormControl,
        label: Label,
        sound: bool,
    ) -> Admission {
        if settings.window_secs == 0 {
            return Admission::Audible;
        }
        let window = self.window.get_or_insert_with(|| Window {
            start: now,
            end: now.plus(Duration::from_secs(u64::from(settings.window_secs))),
            counted: 0,
            absorbed: 0,
            counts: [0; Label::ALL.len()],
            absorbed_critical: false,
            absorbed_sound: false,
        });
        window.counted = window.counted.saturating_add(1);
        if let Some(count) = window.counts.get_mut(label.index()) {
            *count = count.saturating_add(1);
        }
        if window.counted > settings.threshold {
            window.absorbed = window.absorbed.saturating_add(1);
            window.absorbed_critical |= label.is_critical();
            window.absorbed_sound |= sound;
            Admission::Absorbed
        } else {
            Admission::Audible
        }
    }

    /// Closes the window if it ended at or before `now`, or if `now` lies
    /// before its start (the clock went back). Returns its summary if it
    /// silenced anything.
    pub(crate) fn close_if_over(&mut self, now: Timestamp) -> Option<StormSummary> {
        let window = self.window.as_ref()?;
        if now >= window.start && now < window.end {
            return None;
        }
        let window = self.window.take()?;
        (window.absorbed > 0).then_some(StormSummary {
            started: window.start,
            total: window.counted,
            counts: window.counts,
            critical: window.absorbed_critical,
            sound: window.absorbed_sound,
        })
    }

    /// Whether a window is open.
    #[cfg(test)]
    pub(crate) fn is_open(&self) -> bool {
        self.window.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: f64) -> Timestamp {
        Timestamp::from_unix_seconds(seconds)
    }

    const SETTINGS: StormControl = StormControl {
        window_secs: 10,
        threshold: 2,
    };

    #[test]
    fn the_first_threshold_notifications_are_shown() {
        let mut storm = Storm::default();
        assert_eq!(
            storm.admit(at(0.0), SETTINGS, Label::Critical, true),
            Admission::Audible
        );
        assert_eq!(
            storm.admit(at(1.0), SETTINGS, Label::Critical, true),
            Admission::Audible
        );
        assert_eq!(
            storm.admit(at(2.0), SETTINGS, Label::Down, true),
            Admission::Absorbed
        );
        assert_eq!(
            storm.admit(at(9.9), SETTINGS, Label::Warning, false),
            Admission::Absorbed
        );

        assert_eq!(storm.close_if_over(at(9.99)), None, "still open");
        let summary = storm.close_if_over(at(10.0)).unwrap();
        assert_eq!(summary.started, at(0.0));
        assert_eq!(summary.total, 4);
        assert!(summary.critical);
        assert!(summary.sound);
        assert_eq!(summary.title("prod"), "4 new problems in prod");
        assert_eq!(summary.body(), "2 critical · 1 down · 1 warning");
        assert!(!storm.is_open());

        assert_eq!(
            storm.admit(at(11.0), SETTINGS, Label::Critical, true),
            Admission::Audible,
            "a new window starts fresh"
        );
    }

    #[test]
    fn quiet_windows_close_without_a_summary() {
        let mut storm = Storm::default();
        storm.admit(at(0.0), SETTINGS, Label::Critical, true);
        storm.admit(at(1.0), SETTINGS, Label::Critical, true);
        assert_eq!(storm.close_if_over(at(10.0)), None);
        assert!(!storm.is_open());
    }

    #[test]
    fn summaries_describe_mixed_windows() {
        let mut storm = Storm::default();
        let settings = StormControl {
            window_secs: 5,
            threshold: 0,
        };
        assert_eq!(
            storm.admit(at(0.0), settings, Label::Recovered, false),
            Admission::Absorbed
        );
        let summary = storm.close_if_over(at(5.0)).unwrap();
        assert_eq!(summary.title("prod"), "1 notification in prod");
        assert_eq!(summary.body(), "1 recovered");
        assert!(!summary.critical);
        assert!(!summary.sound);

        storm.admit(at(10.0), settings, Label::Critical, false);
        storm.admit(at(10.0), settings, Label::Acknowledged, false);
        let summary = storm.close_if_over(at(20.0)).unwrap();
        assert_eq!(summary.title(""), "2 notifications");
        assert_eq!(summary.body(), "1 critical · 1 acknowledged");

        storm.admit(at(30.0), settings, Label::Unknown, false);
        assert_eq!(
            storm.close_if_over(at(35.0)).unwrap().title("prod"),
            "1 new problem in prod"
        );
    }

    #[test]
    fn a_zero_window_disables_storm_control() {
        let mut storm = Storm::default();
        let settings = StormControl {
            window_secs: 0,
            threshold: 0,
        };
        for second in 0..100 {
            assert_eq!(
                storm.admit(at(f64::from(second)), settings, Label::Critical, true),
                Admission::Audible
            );
        }
        assert!(!storm.is_open());
    }

    #[test]
    fn a_clock_jump_back_closes_the_window() {
        let mut storm = Storm::default();
        for _ in 0..3 {
            storm.admit(at(1_000.0), SETTINGS, Label::Critical, true);
        }
        let summary = storm.close_if_over(at(10.0)).unwrap();
        assert_eq!(summary.total, 3);
        assert!(!storm.is_open());
    }
}
