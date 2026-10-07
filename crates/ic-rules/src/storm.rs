//! Storm control: bursts of notifications collapse into summaries.
//!
//! Storm control watches the notifications that would be shown, over a
//! trailing window of `window_secs`: a notification that would be the
//! `threshold + 1`-th inside the window ending with it is recorded silently
//! instead. Each notification is judged by its own window, so a storm lasts
//! exactly as long as notifications keep coming faster than that: a
//! sustained flood stays silent however long it goes on, and once it calms
//! down, notifications are shown again. In any window, at most `threshold`
//! notifications are shown.
//!
//! What a storm silenced is summarized: when the storm is over (a whole
//! window without a silenced notification), and while it lasts at most once
//! per [`PROGRESS_EVERY`] (or per window, if that is longer). Each summary
//! covers everything counted since the previous one.

use std::collections::VecDeque;
use std::time::Duration;

use ic_model::Timestamp;

use crate::settings::StormControl;
use crate::text::Label;

/// While a storm lasts, summaries come at most this often (or once per
/// window, if the window is longer).
pub(crate) const PROGRESS_EVERY: Duration = Duration::from_mins(1);

/// The most notifications remembered to judge the rate. A larger threshold
/// counts as this; at that point storm control is all but off anyway.
const MAX_THRESHOLD: usize = 10_000;

/// Storm control's decision for one notification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Admission {
    /// Show it.
    Audible,
    /// Record it silently; a summary covers it.
    Absorbed,
}

/// What a storm silenced and counted since its previous summary.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StormSummary {
    /// When the summarized stretch began.
    pub(crate) started: Timestamp,
    /// Every notification counted in it, shown or silenced.
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
    /// when the stretch also held recoveries or other events.
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

/// Recent notifications and the storm, if one is on.
#[derive(Clone, Debug, Default)]
pub(crate) struct Storm {
    /// The notifications counted within the last window, oldest first; at
    /// most [`MAX_THRESHOLD`] of them.
    recent: VecDeque<(Timestamp, Label)>,
    /// The storm going on, if any.
    period: Option<Period>,
}

/// A storm's stretch since its previous summary.
#[derive(Clone, Debug)]
struct Period {
    started: Timestamp,
    /// The storm ends a window after this.
    last_absorbed: Timestamp,
    total: u32,
    counts: [u32; Label::ALL.len()],
    absorbed: u32,
    absorbed_critical: bool,
    absorbed_sound: bool,
}

impl Period {
    /// A storm starting now; the notifications already counted within the
    /// window belong to it.
    fn open<'a>(
        now: Timestamp,
        recent: impl ExactSizeIterator<Item = &'a (Timestamp, Label)>,
    ) -> Self {
        let mut period = Self::empty(now, now);
        for (index, (at, label)) in recent.enumerate() {
            if index == 0 {
                period.started = *at;
            }
            period.count(*label);
        }
        period
    }

    fn empty(started: Timestamp, last_absorbed: Timestamp) -> Self {
        Self {
            started,
            last_absorbed,
            total: 0,
            counts: [0; Label::ALL.len()],
            absorbed: 0,
            absorbed_critical: false,
            absorbed_sound: false,
        }
    }

    fn count(&mut self, label: Label) {
        self.total = self.total.saturating_add(1);
        if let Some(count) = self.counts.get_mut(label.index()) {
            *count = count.saturating_add(1);
        }
    }

    fn absorb(&mut self, now: Timestamp, label: Label, sound: bool) {
        self.count(label);
        self.absorbed = self.absorbed.saturating_add(1);
        self.absorbed_critical |= label.is_critical();
        self.absorbed_sound |= sound;
        if now > self.last_absorbed {
            self.last_absorbed = now;
        }
    }

    /// The summary, if anything was silenced.
    fn summary(&self) -> Option<StormSummary> {
        (self.absorbed > 0).then_some(StormSummary {
            started: self.started,
            total: self.total,
            counts: self.counts,
            critical: self.absorbed_critical,
            sound: self.absorbed_sound,
        })
    }
}

impl Storm {
    /// Counts a notification that would be shown at `now` and decides
    /// whether it is. Callers [poll](Storm::poll) first, so a finished storm
    /// is summarized before a new one starts. A zero-length window disables
    /// storm control.
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
        let window = window(settings);
        self.forget_before(now, window);
        let limit = usize::try_from(settings.threshold)
            .map_or(MAX_THRESHOLD, |threshold| threshold.min(MAX_THRESHOLD));
        let admission = if self.recent.len() >= limit {
            let recent = &self.recent;
            self.period
                .get_or_insert_with(|| Period::open(now, recent.iter()))
                .absorb(now, label, sound);
            Admission::Absorbed
        } else {
            if let Some(period) = &mut self.period {
                period.count(label);
            }
            Admission::Audible
        };
        self.recent.push_back((now, label));
        while self.recent.len() > MAX_THRESHOLD {
            self.recent.pop_front();
        }
        admission
    }

    /// Ends the storm if it is over (a whole window without a silenced
    /// notification, storm control turned off, or the clock went back), or
    /// takes stock if it has gone on for a while. Returns the summary of
    /// what was silenced since the previous one, if anything was.
    pub(crate) fn poll(&mut self, now: Timestamp, settings: StormControl) -> Option<StormSummary> {
        let period = self.period.as_mut()?;
        let window = window(settings);
        let over = settings.window_secs == 0
            || now < period.started
            || now >= period.last_absorbed.plus(window);
        if over {
            let summary = period.summary();
            self.period = None;
            return summary;
        }
        if now >= period.started.plus(window.max(PROGRESS_EVERY)) {
            let summary = period.summary();
            *period = Period::empty(now, period.last_absorbed);
            return summary;
        }
        None
    }

    /// Drops the notifications that fell out of the window; all of them if
    /// the clock went back.
    fn forget_before(&mut self, now: Timestamp, window: Duration) {
        if self.recent.back().is_some_and(|(at, _)| *at > now) {
            self.recent.clear();
        }
        while self
            .recent
            .front()
            .is_some_and(|(at, _)| at.plus(window) <= now)
        {
            self.recent.pop_front();
        }
    }

    /// When the storm's current stretch began (`None`: no storm is on).
    /// The summary that covers what it silences now carries this time in
    /// its id.
    pub(crate) fn started(&self) -> Option<Timestamp> {
        self.period.as_ref().map(|period| period.started)
    }

    /// Whether a storm is on.
    #[cfg(test)]
    pub(crate) fn is_on(&self) -> bool {
        self.period.is_some()
    }
}

fn window(settings: StormControl) -> Duration {
    Duration::from_secs(u64::from(settings.window_secs))
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
            storm.admit(at(9.5), SETTINGS, Label::Warning, false),
            Admission::Absorbed
        );

        assert_eq!(storm.poll(at(19.25), SETTINGS), None, "still on");
        let summary = storm.poll(at(19.5), SETTINGS).unwrap();
        assert_eq!(summary.started, at(0.0));
        assert_eq!(summary.total, 4);
        assert!(summary.critical);
        assert!(summary.sound);
        assert_eq!(summary.title("prod"), "4 new problems in prod");
        assert_eq!(summary.body(), "2 critical · 1 down · 1 warning");
        assert!(!storm.is_on());

        assert_eq!(
            storm.admit(at(20.0), SETTINGS, Label::Critical, true),
            Admission::Audible,
            "calm again"
        );
    }

    #[test]
    fn quiet_windows_close_without_a_summary() {
        let mut storm = Storm::default();
        storm.admit(at(0.0), SETTINGS, Label::Critical, true);
        storm.admit(at(1.0), SETTINGS, Label::Critical, true);
        assert!(!storm.is_on());
        assert_eq!(storm.poll(at(10.0), SETTINGS), None);
    }

    #[test]
    fn each_notification_is_judged_by_its_own_window() {
        let mut storm = Storm::default();
        // Two per window is fine, however long it goes on.
        for second in 0..100 {
            assert_eq!(
                storm.admit(at(f64::from(second) * 5.0), SETTINGS, Label::Critical, true),
                Admission::Audible
            );
        }
        // A third within ten seconds of two others is not.
        assert_eq!(
            storm.admit(at(496.0), SETTINGS, Label::Critical, true),
            Admission::Absorbed
        );
    }

    #[test]
    fn a_sustained_flood_stays_silent_and_is_summarized_every_minute() {
        let mut storm = Storm::default();
        let mut shown = 0;
        let mut summaries = Vec::new();
        for tenth in 0..1_200 {
            let now = at(f64::from(tenth) / 10.0);
            summaries.extend(storm.poll(now, SETTINGS));
            for _ in 0..5 {
                if storm.admit(now, SETTINGS, Label::Critical, true) == Admission::Audible {
                    shown += 1;
                }
            }
        }
        summaries.extend(storm.poll(at(200.0), SETTINGS));
        assert_eq!(shown, 2, "only the first threshold notifications");
        assert_eq!(summaries.len(), 2, "one a minute plus the final one");
        assert_eq!(summaries[0].started, at(0.0));
        assert_eq!(summaries[1].started, at(60.0));
        let total: u32 = summaries.iter().map(|summary| summary.total).sum();
        assert_eq!(total, 6_000, "every notification is counted once");
        assert!(!storm.is_on());
    }

    #[test]
    fn a_trickle_after_a_flood_is_shown_again() {
        let mut storm = Storm::default();
        for _ in 0..20 {
            storm.admit(at(0.0), SETTINGS, Label::Critical, true);
        }
        assert_eq!(
            storm.admit(at(5.0), SETTINGS, Label::Critical, true),
            Admission::Absorbed
        );
        // Ten seconds later the flood is out of the window.
        assert_eq!(
            storm.admit(at(15.0), SETTINGS, Label::Critical, true),
            Admission::Audible,
            "the storm is still being summarized, but the rate is fine"
        );
        let summary = storm.poll(at(15.0), SETTINGS).unwrap();
        assert_eq!(
            summary.total, 22,
            "the shown one during the storm counts too"
        );
    }

    #[test]
    fn summaries_describe_mixed_stretches() {
        let mut storm = Storm::default();
        let settings = StormControl {
            window_secs: 5,
            threshold: 0,
        };
        assert_eq!(
            storm.admit(at(0.0), settings, Label::Recovered, false),
            Admission::Absorbed
        );
        let summary = storm.poll(at(5.0), settings).unwrap();
        assert_eq!(summary.title("prod"), "1 notification in prod");
        assert_eq!(summary.body(), "1 recovered");
        assert!(!summary.critical);
        assert!(!summary.sound);

        storm.admit(at(10.0), settings, Label::Critical, false);
        storm.admit(at(10.0), settings, Label::Acknowledged, false);
        let summary = storm.poll(at(20.0), settings).unwrap();
        assert_eq!(summary.title(""), "2 notifications");
        assert_eq!(summary.body(), "1 critical · 1 acknowledged");

        storm.admit(at(30.0), settings, Label::Unknown, false);
        assert_eq!(
            storm.poll(at(35.0), settings).unwrap().title("prod"),
            "1 new problem in prod"
        );
    }

    #[test]
    fn a_zero_window_disables_storm_control_and_ends_a_storm() {
        let mut storm = Storm::default();
        for _ in 0..3 {
            storm.admit(at(0.0), SETTINGS, Label::Critical, true);
        }
        assert!(storm.is_on());
        let off = StormControl {
            window_secs: 0,
            threshold: 0,
        };
        assert_eq!(storm.poll(at(1.0), off).unwrap().total, 3);
        for second in 0..100 {
            assert_eq!(
                storm.admit(at(f64::from(second)), off, Label::Critical, true),
                Admission::Audible
            );
        }
        assert!(!storm.is_on());
    }

    #[test]
    fn a_changed_threshold_applies_at_once() {
        let mut storm = Storm::default();
        for _ in 0..3 {
            storm.admit(at(0.0), SETTINGS, Label::Critical, true);
        }
        let higher = StormControl {
            window_secs: 10,
            threshold: 5,
        };
        for _ in 0..2 {
            assert_eq!(
                storm.admit(at(1.0), higher, Label::Critical, true),
                Admission::Audible
            );
        }
        assert_eq!(
            storm.admit(at(1.0), higher, Label::Critical, true),
            Admission::Absorbed,
            "the sixth in the window"
        );
        let lower = StormControl {
            window_secs: 10,
            threshold: 1,
        };
        assert_eq!(storm.poll(at(1.0), lower), None);
        assert_eq!(
            storm.admit(at(2.0), lower, Label::Critical, true),
            Admission::Absorbed
        );
    }

    #[test]
    fn a_clock_jump_back_ends_the_storm() {
        let mut storm = Storm::default();
        for _ in 0..3 {
            storm.admit(at(1_000.0), SETTINGS, Label::Critical, true);
        }
        let summary = storm.poll(at(10.0), SETTINGS).unwrap();
        assert_eq!(summary.total, 3);
        assert!(!storm.is_on());
        assert_eq!(
            storm.admit(at(10.0), SETTINGS, Label::Critical, true),
            Admission::Audible,
            "what was counted in the future is forgotten"
        );
    }

    #[test]
    fn huge_thresholds_are_capped() {
        let mut storm = Storm::default();
        let settings = StormControl {
            window_secs: 10,
            threshold: u32::MAX,
        };
        for _ in 0..MAX_THRESHOLD {
            assert_eq!(
                storm.admit(at(0.0), settings, Label::Critical, false),
                Admission::Audible
            );
        }
        assert_eq!(
            storm.admit(at(0.0), settings, Label::Critical, false),
            Admission::Absorbed
        );
        assert!(storm.recent.len() <= MAX_THRESHOLD);
    }
}
