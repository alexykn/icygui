//! No synchronised clients (PLAN.md §4.2, docs/performance.md): after a
//! master restart every client reloads at once, and timers counted from
//! that moment would stay lined up for as long as the clients run, so
//! fifty of them would ask for the same heavy answer in the same second,
//! interval after interval. Every engine's periodic heavy requests (the
//! reconcile, the status polls, and with them the health page's reads for
//! the trouble alerts and the cluster nodes' states) are spread instead:
//!
//! - **a random phase** for the first of a series (the first reconcile
//!   after a load that wasn't one, a session's first status poll):
//!   anywhere in [`PHASE`] of the interval, a whole interval wide, so
//!   clients that loaded together are spread over a whole interval at
//!   once, and the average cadence stays the interval;
//! - **±[`SPREAD`]** on every interval after that, so they keep drifting
//!   apart rather than back together.
//!
//! The reconcile's windows never start below its documented minimum (the
//! engine's `sync` module lifts them above it), and a reconcile planned
//! again keeps the point of its window it was drawn at.
//!
//! The heartbeat polls (only for an API user who may not filter the event
//! stream, while quiet) take a random phase too, but every later one comes
//! at most an interval after the last ([`Jitter::within`]: 80 to 100 % of
//! it): a polled beat's deadline counts on a poll seeing it within an
//! interval of its check, so a longer gap would make it late.
//!
//! What only ever comes with another request (the health page's reads
//! for the trouble alerts and the cluster nodes' states, which come with a
//! status poll) is not spread a second time: it is due [`carried`] after
//! the last, the spread poll's lower bound, so every poll whose turn it is
//! carries it, and its pace is the poll's (spread already), not one or two
//! polls at random.
//!
//! The randomness is the engine's own ([`Jitter`]), from
//! `Tuning::seed`: random in production, fixed in tests, so a test run
//! repeats exactly. The reconnect backoff, the reload jitter and a
//! background start's delay draw from it too.

use std::time::Duration;

/// How far every periodic interval is spread: ±20 %.
pub(crate) const SPREAD: f64 = 0.2;

/// Where the first of a series falls, as fractions of its interval: from
/// half an interval to one and a half (a whole interval wide, the
/// interval on average).
pub(crate) const PHASE: (f64, f64) = (0.5, 1.5);

/// When a request that only comes with a spread periodic one (a status
/// poll) is due again, `interval` after it was last sent: at the carrier's
/// earliest, `interval × (1 − SPREAD)`, so the carrier poll whose turn it
/// is always finds it due (spread again on top, half the polls would come
/// a moment too early and leave it for the next one: 5 minutes would
/// become 4 to 12).
pub(crate) fn carried(interval: Duration) -> Duration {
    interval.mul_f64(1.0 - SPREAD)
}

/// An engine's source of randomness for its timers.
#[derive(Debug)]
pub(crate) struct Jitter {
    rng: fastrand::Rng,
}

impl Jitter {
    /// Seeded with `seed` (tests), or randomly (`None`: every engine its
    /// own, as in production).
    pub(crate) fn new(seed: Option<u64>) -> Self {
        Self {
            rng: seed.map_or_else(fastrand::Rng::new, fastrand::Rng::with_seed),
        }
    }

    /// A number in `[0, 1)`.
    pub(crate) fn unit(&mut self) -> f64 {
        self.rng.f64()
    }

    /// `interval` ±[`SPREAD`]: the next of a series.
    pub(crate) fn spread(&mut self, interval: Duration) -> Duration {
        interval.mul_f64(1.0 - SPREAD + 2.0 * SPREAD * self.unit())
    }

    /// The next of a series that must never come later than `interval`
    /// after the last (the heartbeat polls: a beat's deadline counts on
    /// being seen within an interval of its check): from `1 − SPREAD` of
    /// it to all of it, so clients still drift apart.
    pub(crate) fn within(&mut self, interval: Duration) -> Duration {
        interval.mul_f64(1.0 - SPREAD * self.unit())
    }

    /// The first of a series: a random phase in [`PHASE`] of `interval`.
    pub(crate) fn phase(&mut self, interval: Duration) -> Duration {
        let (from, to) = PHASE;
        interval.mul_f64(from + (to - from) * self.unit())
    }

    /// Up to `most`, uniformly (a reload's jitter, a background start's
    /// delay).
    pub(crate) fn up_to(&mut self, most: Duration) -> Duration {
        most.mul_f64(self.unit())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A series that must never be later than its interval (the heartbeat
    /// polls) still spreads: every gap within 80–100 % of it, fifty clients
    /// that started together end up apart.
    #[test]
    fn a_series_within_its_interval_never_comes_late_and_still_drifts() {
        let interval = Duration::from_secs(30);
        let mut jitter = Jitter::new(Some(9));
        for _ in 0..10_000 {
            let gap = jitter.within(interval);
            assert!(gap <= interval, "{gap:?}");
            assert!(gap >= interval.mul_f64(1.0 - SPREAD), "{gap:?}");
        }
        // Fifty clients polling from the same moment: after ten polls each
        // fifth of the interval holds some of them.
        let mut buckets = [0_u32; 5];
        for client in 0..50 {
            let mut jitter = Jitter::new(Some(client));
            let mut at = jitter.phase(interval);
            for _ in 0..10 {
                at += jitter.within(interval);
            }
            let fraction = (at.as_secs_f64() / interval.as_secs_f64()).fract();
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "0..5"
            )]
            let bucket = (fraction * 5.0) as usize;
            buckets[bucket.min(4)] += 1;
        }
        assert!(buckets.iter().all(|count| *count > 0), "{buckets:?}");
    }

    #[test]
    fn the_same_seed_gives_the_same_timers() {
        let interval = Duration::from_mins(5);
        let draw = |seed| {
            let mut jitter = Jitter::new(Some(seed));
            (0..50)
                .map(|_| (jitter.phase(interval), jitter.spread(interval)))
                .collect::<Vec<_>>()
        };
        assert_eq!(draw(7), draw(7));
        assert_ne!(draw(7), draw(8));
    }

    #[test]
    fn intervals_are_spread_twenty_percent_either_way() {
        let interval = Duration::from_mins(5);
        let mut jitter = Jitter::new(Some(1));
        let spread: Vec<f64> = (0..10_000)
            .map(|_| jitter.spread(interval).as_secs_f64() / interval.as_secs_f64())
            .collect();
        assert!(
            spread.iter().all(|&x| (0.8..1.2).contains(&x)),
            "within ±20 %"
        );
        // They use the whole range, not a corner of it.
        assert!(spread.iter().any(|&x| x < 0.82) && spread.iter().any(|&x| x > 1.18));
        let mean = spread.iter().sum::<f64>() / 10_000.0;
        assert!((mean - 1.0).abs() < 0.01, "{mean}");
    }

    /// Fifty clients that loaded in the same second: their first
    /// reconciles spread over a whole interval (no two minutes of the
    /// five without one), not over ±10 % of it as before.
    #[test]
    fn a_random_phase_spreads_clients_that_started_together_over_an_interval() {
        let interval = Duration::from_mins(5);
        let firsts: Vec<f64> = (0..50_u64)
            .map(|client| Jitter::new(Some(client)).phase(interval).as_secs_f64())
            .collect();
        assert!(firsts.iter().all(|&at| (150.0..450.0).contains(&at)));
        let mut minutes = [0_usize; 5];
        for at in &firsts {
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "0..5 by the assertion above"
            )]
            let minute = ((at - 150.0) / 60.0) as usize;
            minutes[minute] += 1;
        }
        assert!(minutes.iter().all(|&count| count > 0), "{minutes:?}");
        assert!(minutes.iter().all(|&count| count < 25), "{minutes:?}");
    }

    /// A read that comes with a spread poll every `interval` comes with
    /// every one of them (the poll's pace, spread once), not one or two
    /// polls at random: the gaps stay within the poll's ±20 %.
    #[test]
    fn a_read_carried_by_a_spread_poll_keeps_the_polls_pace() {
        let interval = Duration::from_mins(5);
        let mut jitter = Jitter::new(Some(3));
        let (mut now, mut last_read, mut due) = (0.0_f64, 0.0_f64, 0.0_f64);
        let mut gaps = Vec::new();
        for _ in 0..10_000 {
            now += jitter.spread(interval).as_secs_f64();
            if now >= due {
                gaps.push(now - last_read);
                last_read = now;
                due = now + carried(interval).as_secs_f64();
            }
        }
        assert_eq!(gaps.len(), 10_000, "every poll carries it");
        let mean = gaps.iter().sum::<f64>() / 10_000.0;
        assert!((mean - 300.0).abs() < 3.0, "{mean}");
        assert!(gaps.iter().all(|&gap| (240.0..360.0).contains(&gap)));
    }

    /// Clients whose timers lined up drift apart with every interval,
    /// rather than staying together.
    #[test]
    fn lined_up_clients_drift_apart() {
        let interval = Duration::from_mins(5);
        let mut clients: Vec<(Jitter, f64)> = (0..50_u64)
            .map(|client| (Jitter::new(Some(1_000 + client)), 0.0))
            .collect();
        for _ in 0..10 {
            for (jitter, at) in &mut clients {
                *at += jitter.spread(interval).as_secs_f64();
            }
        }
        let (min, max) = clients
            .iter()
            .fold((f64::MAX, f64::MIN), |(min, max), (_, at)| {
                (min.min(*at), max.max(*at))
            });
        // Ten intervals on, they are spread over more than a whole one.
        assert!(max - min > interval.as_secs_f64(), "{}", max - min);
    }
}
