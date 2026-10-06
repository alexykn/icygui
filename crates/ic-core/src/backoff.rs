//! Reconnect delays: exponential with jitter, so clients that lost Icinga
//! at the same moment (a restart) don't come back at the same moment.

use std::time::Duration;

/// Counts consecutive failures and spaces the next attempt.
#[derive(Clone, Debug)]
pub(crate) struct Backoff {
    initial: Duration,
    max: Duration,
    failures: u32,
}

impl Backoff {
    /// Delays from `initial`, doubling up to `max`.
    pub(crate) fn new(initial: Duration, max: Duration) -> Self {
        Self {
            initial,
            max: max.max(initial),
            failures: 0,
        }
    }

    /// The number of the next attempt (1 after a reset).
    pub(crate) fn attempt(&self) -> u32 {
        self.failures.saturating_add(1)
    }

    /// Records a failure and returns how long to wait before the next
    /// attempt: `initial × 2^(failures − 1)`, at most `max`, of which a
    /// random half is jitter.
    pub(crate) fn fail(&mut self) -> Duration {
        self.failures = self.failures.saturating_add(1);
        jitter(self.delay(), fastrand::f64())
    }

    /// Like [`Backoff::fail`], with the delay before jitter at most `max`.
    pub(crate) fn fail_up_to(&mut self, max: Duration) -> Duration {
        self.failures = self.failures.saturating_add(1);
        jitter(self.delay().min(max), fastrand::f64())
    }

    /// Back to the first delay (after a healthy connection or a user's
    /// retry).
    pub(crate) fn reset(&mut self) {
        self.failures = 0;
    }

    /// The delay before jitter for the current failure count.
    fn delay(&self) -> Duration {
        let doublings = self.failures.saturating_sub(1).min(31);
        self.initial
            .checked_mul(1_u32 << doublings)
            .map_or(self.max, |delay| delay.min(self.max))
    }
}

/// `delay` with "equal jitter": half fixed, half random (`random` in
/// `[0, 1)`), so the result is in `[delay / 2, delay)`.
fn jitter(delay: Duration, random: f64) -> Duration {
    let half = delay / 2;
    half + half.mul_f64(random.clamp(0.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doubles_from_one_second_to_a_minute() {
        let mut backoff = Backoff::new(Duration::from_secs(1), Duration::from_mins(1));
        assert_eq!(backoff.attempt(), 1);
        let mut delays = Vec::new();
        for _ in 0..9 {
            backoff.fail();
            delays.push(backoff.delay().as_secs());
        }
        assert_eq!(delays, [1, 2, 4, 8, 16, 32, 60, 60, 60]);
        assert_eq!(backoff.attempt(), 10);
        backoff.reset();
        assert_eq!(backoff.attempt(), 1);
        backoff.fail();
        assert_eq!(backoff.delay(), Duration::from_secs(1));
    }

    #[test]
    fn jitter_keeps_half() {
        let delay = Duration::from_secs(8);
        assert_eq!(jitter(delay, 0.0), Duration::from_secs(4));
        assert_eq!(jitter(delay, 0.5), Duration::from_secs(6));
        assert!(jitter(delay, 0.999_999) < delay);
        let mut backoff = Backoff::new(Duration::from_secs(1), Duration::from_mins(1));
        for _ in 0..100 {
            let wait = backoff.fail();
            assert!(
                wait >= backoff.delay() / 2 && wait < backoff.delay().max(Duration::from_nanos(1))
            );
        }
    }

    #[test]
    fn a_lower_cap_applies_per_failure() {
        let mut backoff = Backoff::new(Duration::from_secs(30), Duration::from_mins(15));
        let mut waits = Vec::new();
        for _ in 0..7 {
            waits.push(backoff.fail_up_to(Duration::from_mins(5)));
        }
        let bounds = [30, 60, 120, 240, 300, 300, 300];
        for (wait, bound) in waits.iter().zip(bounds) {
            let bound = Duration::from_secs(bound);
            assert!(*wait >= bound / 2 && *wait < bound, "{waits:?}");
        }
    }

    #[test]
    fn many_failures_never_overflow() {
        let mut backoff = Backoff::new(Duration::from_secs(1), Duration::from_mins(1));
        for _ in 0..1_000 {
            backoff.fail();
        }
        assert_eq!(backoff.delay(), Duration::from_mins(1));
    }
}
