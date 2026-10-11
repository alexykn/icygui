//! Adaptive pacing (PLAN.md §4.2 1b, docs/performance.md *Adaptive
//! pacing*): clients that see the same busy master spread their heavy
//! loads out by themselves, without talking to each other.
//!
//! - **The signal** is the master's own answer time, the one thing that
//!   matters to everyone else: a tiny by-name probe (one object, one
//!   attribute) before a paced load, and the status poll's timing for
//!   free. Icinga 2.15 reports no request queue; its `api_num_http_clients`
//!   counts every client's open event stream too, so it is used as a hint
//!   of the team's size ([`team_spread`]), not as a load signal.
//! - **The baseline** ([`Baselines`]) is the fastest answer seen, per URL
//!   and per [`Signal`], kept across runs in the event log. A slower answer
//!   pulls it up by [`DRIFT`] of the difference, so a lasting change of the
//!   path (a VPN) is learnt over tens of answers while a busy spell of a
//!   few minutes hardly moves it; a gate that waited its whole cap takes
//!   the fastest probe it saw as the new baseline ([`Baselines::settle`]).
//! - **Slow** ([`slow`]): more than `Tuning::pace_factor_percent` of the
//!   baseline *and* more than `Tuning::pace_floor` (3× and 150 ms); with no
//!   baseline yet, more than `Tuning::pace_unknown` (1 s).
//! - **Before a paced load** ([`Gate`]): probe (after a random delay); if
//!   slow, wait a random, growing time (from half to all of
//!   `Tuning::pace_wait_initial`, doubling) and probe again; the total
//!   wait is capped (`Tuning::pace_wait_max`), so every load runs.
//! - **During a paced load** ([`Aimd`]): by-name batches whose own answer
//!   times pace them, like TCP: one more request in flight per window of
//!   fast answers (additive increase, up to `Tuning::batch_window_max`),
//!   half as many when one is slow (multiplicative decrease); at one
//!   request in flight a slow answer adds a pause before the next instead
//!   (the pause doubles, plus the answer's time, so the rate halves), which
//!   a fast answer halves again.
//!
//! Only the bulk loads are paced: the background start's first load, the
//! reload after a restart or a long gap, the periodic safety pass and a
//! reload a count mismatch asks for. What the user does (opening an
//! object, actions, `Refresh`, the window's own first load) never waits;
//! the window's first load does run in paced batches when it has a key
//! list, the problems first, but without a gate.

use std::collections::BTreeMap;
use std::time::Duration;

use tokio::time::Instant;

/// What an answer time was measured on: each has its own baseline (a probe
/// is a fraction of a status poll, which is a fraction of a batch).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Signal {
    /// The tiny by-name probe before a paced load.
    Probe,
    /// The status poll (`/v1/status/IcingaApplication` and `CIB`).
    Status,
    /// One by-name batch of a paced load (200 services, lean).
    Batch,
}

impl Signal {
    /// The name kept in the event log.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Probe => "probe",
            Self::Status => "status",
            Self::Batch => "batch",
        }
    }

    /// The signal named `name` in the event log.
    pub(crate) fn from_name(name: &str) -> Option<Self> {
        match name {
            "probe" => Some(Self::Probe),
            "status" => Some(Self::Status),
            "batch" => Some(Self::Batch),
            _ => None,
        }
    }
}

/// How far an answer slower than the baseline pulls it up: a fiftieth of
/// the difference.
pub(crate) const DRIFT: f64 = 0.02;

/// When an answer counts as slow (from `Tuning`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Rules {
    /// Slower than this share of the baseline (percent: 300 is 3×) …
    pub(crate) factor_percent: u32,
    /// … and slower than this.
    pub(crate) floor: Duration,
    /// With no baseline yet: slower than this.
    pub(crate) unknown: Duration,
}

/// Whether an answer that took `took` is slow against `baseline`.
pub(crate) fn slow(rules: Rules, baseline: Option<Duration>, took: Duration) -> bool {
    match baseline {
        None => took > rules.unknown,
        Some(baseline) => {
            let limit = baseline.mul_f64(f64::from(rules.factor_percent) / 100.0);
            took > limit && took > rules.floor
        }
    }
}

/// The fastest answers seen, per URL and signal (see the module notes).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Baselines {
    map: BTreeMap<(String, Signal), Duration>,
    /// Changed since the event log last got them.
    dirty: bool,
}

impl Baselines {
    /// The baseline of `signal` at `url`.
    pub(crate) fn get(&self, url: &str, signal: Signal) -> Option<Duration> {
        self.map.get(&(url.to_owned(), signal)).copied()
    }

    /// Notes an answer of `signal` at `url` that took `took`: the new
    /// fastest, or a [`DRIFT`] of the way up to it.
    pub(crate) fn observe(&mut self, url: &str, signal: Signal, took: Duration) {
        let entry = self.map.entry((url.to_owned(), signal)).or_insert(took);
        let next = if took <= *entry {
            took
        } else {
            *entry + took.saturating_sub(*entry).mul_f64(DRIFT)
        };
        if next != *entry {
            *entry = next;
            self.dirty = true;
        } else if took == next {
            // A first observation inserted it.
            self.dirty = true;
        }
    }

    /// A gate waited its whole cap and its fastest probe still took
    /// `fastest`: that is the master's normal from now on (a slower path,
    /// or a master that stays this busy).
    pub(crate) fn settle(&mut self, url: &str, signal: Signal, fastest: Duration) {
        let entry = self.map.entry((url.to_owned(), signal)).or_insert(fastest);
        if *entry < fastest {
            *entry = fastest;
        }
        self.dirty = true;
    }

    /// Takes them from the event log (an older run's), keeping what this
    /// run measured already.
    pub(crate) fn remember(
        &mut self,
        entries: impl IntoIterator<Item = (String, Signal, Duration)>,
    ) {
        for (url, signal, took) in entries {
            self.map.entry((url, signal)).or_insert(took);
        }
    }

    /// Every baseline, if any changed since the last call (for the event
    /// log).
    pub(crate) fn take_dirty(&mut self) -> Option<Vec<(String, Signal, Duration)>> {
        if !std::mem::take(&mut self.dirty) {
            return None;
        }
        Some(
            self.map
                .iter()
                .map(|((url, signal), took)| (url.clone(), *signal, *took))
                .collect(),
        )
    }
}

/// What a [`Gate`] says after a probe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// Load now.
    Go,
    /// Probe again after this.
    Wait(Duration),
}

/// The wait before a paced load (see the module notes): created when the
/// load is due, it probes, and either lets the load run or waits a random,
/// growing time and probes again, never longer than its cap in all.
#[derive(Clone, Debug)]
pub(crate) struct Gate {
    /// Waited so far.
    waited: Duration,
    /// Waits so far.
    attempt: u32,
    /// The fastest probe it saw (the new baseline if it waits its cap).
    pub(crate) fastest: Option<Duration>,
    /// When it probes next.
    pub(crate) probe_at: Instant,
    /// A probe is out.
    pub(crate) in_flight: bool,
    /// The status poll that asked for the load was slow already: the first
    /// verdict is a wait without a probe of its own.
    pub(crate) busy_from_status: bool,
    /// Probes sent.
    pub(crate) probes: u32,
}

impl Gate {
    /// A gate whose first probe goes out at `probe_at`.
    pub(crate) fn new(probe_at: Instant) -> Self {
        Self {
            waited: Duration::ZERO,
            attempt: 0,
            fastest: None,
            probe_at,
            in_flight: false,
            busy_from_status: false,
            probes: 0,
        }
    }

    /// How long it has waited in all.
    pub(crate) fn waited(&self) -> Duration {
        self.waited
    }

    /// Whether it waited its whole cap.
    pub(crate) fn capped(&self, cap: Duration) -> bool {
        self.waited >= cap
    }

    /// After a probe (or the status poll) found the master `slow`: go, or
    /// wait from half to all of `initial × 2^waits` (`unit` in `[0, 1)`
    /// picks where), never beyond `cap` in all.
    pub(crate) fn decide(
        &mut self,
        slow: bool,
        unit: f64,
        initial: Duration,
        cap: Duration,
    ) -> Verdict {
        let remaining = cap.saturating_sub(self.waited);
        if !slow || remaining.is_zero() {
            return Verdict::Go;
        }
        let window = initial.saturating_mul(1_u32 << self.attempt.min(16));
        // The last wait takes what is left of the cap.
        let wait = if window >= remaining {
            remaining
        } else {
            window.mul_f64(0.5 + 0.5 * unit.clamp(0.0, 1.0))
        };
        self.attempt += 1;
        self.waited += wait;
        Verdict::Wait(wait)
    }

    /// Notes a probe's time.
    pub(crate) fn probed(&mut self, took: Duration) {
        self.probes += 1;
        self.fastest = Some(self.fastest.map_or(took, |fastest| fastest.min(took)));
    }
}

/// The concurrency of a paced load's batches (see the module notes).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Aimd {
    /// Requests in flight allowed (fractional: one more per window of fast
    /// answers).
    window: f64,
    max: f64,
    /// The pause before the next request while one is in flight at most.
    gap: Duration,
}

/// The longest pause [`Aimd`] puts between two batches.
pub(crate) const MAX_GAP: Duration = Duration::from_secs(10);

/// A pause shorter than this is no pause.
const MIN_GAP: Duration = Duration::from_millis(10);

impl Aimd {
    /// Starting with `start` requests in flight, at most `max`.
    pub(crate) fn new(start: usize, max: usize) -> Self {
        let max = max.max(1);
        #[expect(
            clippy::cast_precision_loss,
            reason = "a handful of requests in flight"
        )]
        Self {
            window: start.clamp(1, max) as f64,
            max: max as f64,
            gap: Duration::ZERO,
        }
    }

    /// How many requests may be in flight now.
    pub(crate) fn allowed(&self) -> usize {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "1..=max by construction"
        )]
        let allowed = self.window.floor() as usize;
        allowed.max(1)
    }

    /// The pause before the next request.
    pub(crate) fn gap(&self) -> Duration {
        self.gap
    }

    /// An answer came back after `took`; `slow` by [`slow`] (or it failed).
    pub(crate) fn answered(&mut self, slow: bool, took: Duration) {
        if slow {
            if self.window > 1.0 {
                self.window = (self.window / 2.0).max(1.0);
            } else {
                self.gap = (self.gap.saturating_mul(2) + took).min(MAX_GAP);
            }
        } else if !self.gap.is_zero() {
            self.gap /= 2;
            if self.gap < MIN_GAP {
                self.gap = Duration::ZERO;
            }
        } else {
            self.window = (self.window + 1.0 / self.window).min(self.max);
        }
    }
}

/// How widely a team's simultaneous heavy requests should spread, from
/// Icinga's count of open API connections (`api_num_http_clients`: every
/// client's event stream, so about the team's size): `per_client` for
/// each, between `low` and `high`.
pub(crate) fn team_spread(
    clients: Option<u32>,
    per_client: Duration,
    low: Duration,
    high: Duration,
) -> Duration {
    per_client
        .saturating_mul(clients.unwrap_or(0))
        .clamp(low, high.max(low))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules() -> Rules {
        Rules {
            factor_percent: 300,
            floor: Duration::from_millis(150),
            unknown: Duration::from_secs(1),
        }
    }

    fn ms(millis: u64) -> Duration {
        Duration::from_millis(millis)
    }

    #[test]
    fn slow_is_three_times_the_baseline_and_over_the_floor() {
        let rules = rules();
        // A LAN: 5 ms usual; 20 ms is 4× but far under the floor.
        assert!(!slow(rules, Some(ms(5)), ms(20)));
        assert!(!slow(rules, Some(ms(5)), ms(150)));
        assert!(slow(rules, Some(ms(5)), ms(151)));
        // A slow path: 100 ms usual; 250 ms is under 3×.
        assert!(!slow(rules, Some(ms(100)), ms(250)));
        assert!(slow(rules, Some(ms(100)), ms(301)));
        // No baseline yet: only a second or more.
        assert!(!slow(rules, None, ms(900)));
        assert!(slow(rules, None, ms(1_001)));
    }

    #[test]
    fn a_baseline_is_the_fastest_seen_and_drifts_up_slowly() {
        let mut baselines = Baselines::default();
        let url = "https://m1:5665/";
        assert_eq!(baselines.get(url, Signal::Probe), None);
        baselines.observe(url, Signal::Probe, ms(40));
        baselines.observe(url, Signal::Probe, ms(8));
        assert_eq!(baselines.get(url, Signal::Probe), Some(ms(8)));
        // Each signal and URL has its own.
        assert_eq!(baselines.get(url, Signal::Batch), None);
        assert_eq!(baselines.get("https://m2:5665/", Signal::Probe), None);
        // A busy spell of 20 slow answers (5 s) moves it little …
        for _ in 0..20 {
            baselines.observe(url, Signal::Probe, ms(5_000));
        }
        let after_spell = baselines.get(url, Signal::Probe).unwrap();
        assert!(after_spell < ms(5_000).mul_f64(0.35), "{after_spell:?}");
        // … a path four times slower for good is learnt.
        let mut path = Baselines::default();
        path.observe(url, Signal::Probe, ms(2));
        let mut n = 0;
        while slow(rules(), path.get(url, Signal::Probe), ms(200)) {
            path.observe(url, Signal::Probe, ms(200));
            n += 1;
            assert!(n < 100, "never learnt");
        }
        assert!((15..40).contains(&n), "{n} answers");
        // A gate that waited its cap settles it at once.
        let mut capped = Baselines::default();
        capped.observe(url, Signal::Probe, ms(2));
        capped.settle(url, Signal::Probe, ms(300));
        assert_eq!(capped.get(url, Signal::Probe), Some(ms(300)));
        assert!(!slow(rules(), capped.get(url, Signal::Probe), ms(320)));
    }

    #[test]
    fn baselines_from_an_earlier_run_fill_in_but_never_replace() {
        let mut baselines = Baselines::default();
        baselines.observe("a", Signal::Probe, ms(9));
        assert!(baselines.take_dirty().is_some());
        assert!(baselines.take_dirty().is_none(), "nothing new");
        baselines.remember([
            ("a".to_owned(), Signal::Probe, ms(3)),
            ("a".to_owned(), Signal::Status, ms(30)),
        ]);
        assert_eq!(baselines.get("a", Signal::Probe), Some(ms(9)));
        assert_eq!(baselines.get("a", Signal::Status), Some(ms(30)));
        assert_eq!(Signal::from_name(Signal::Batch.name()), Some(Signal::Batch));
    }

    /// A gate waits random, growing times while the master is slow, and
    /// never longer than its cap in all; a fast answer lets the load run
    /// at once. Seeded, so fifty gates that saw the same busy master don't
    /// wait alike.
    #[test]
    fn a_gate_waits_growing_random_times_up_to_its_cap() {
        let initial = Duration::from_secs(5);
        let cap = Duration::from_mins(3);
        let mut gate = Gate::new(Instant::now());
        assert_eq!(gate.decide(false, 0.3, initial, cap), Verdict::Go);
        let mut waits = Vec::new();
        while let Verdict::Wait(wait) = gate.decide(true, 0.5, initial, cap) {
            waits.push(wait);
            assert!(waits.len() < 20);
        }
        // 3.75, 7.5, 15, 30, 60 s, then what is left of the 3 minutes.
        assert_eq!(waits[0], Duration::from_millis(3_750));
        assert_eq!(waits[1], Duration::from_millis(7_500));
        assert!(waits.windows(2).take(4).all(|pair| pair[1] > pair[0]));
        assert_eq!(gate.waited(), cap);
        assert!(gate.capped(cap));
        // Fifty gates (seeded jitter) spread their first waits.
        let mut jitter = crate::jitter::Jitter::new(Some(11));
        let firsts: Vec<Duration> = (0..50)
            .map(|_| {
                let mut gate = Gate::new(Instant::now());
                match gate.decide(true, jitter.unit(), initial, cap) {
                    Verdict::Wait(wait) => wait,
                    Verdict::Go => Duration::ZERO,
                }
            })
            .collect();
        let min = firsts.iter().min().unwrap();
        let max = firsts.iter().max().unwrap();
        assert!(*min >= initial / 2 && *max < initial, "{min:?} … {max:?}");
        assert!(max.saturating_sub(*min) > initial / 3, "spread");
    }

    /// The batches' concurrency: one more per window of fast answers, half
    /// as many on a slow one, a pause at one in flight; fast answers take
    /// the pause back off before the window grows again.
    #[test]
    fn aimd_grows_slowly_and_halves_quickly() {
        let mut aimd = Aimd::new(1, 8);
        assert_eq!(aimd.allowed(), 1);
        // Fast answers: 1 → 2 after one, 2 → 3 after three more (a
        // window of answers per step) …
        aimd.answered(false, ms(30));
        assert_eq!(aimd.allowed(), 2);
        aimd.answered(false, ms(30));
        aimd.answered(false, ms(30));
        assert_eq!(aimd.allowed(), 2);
        aimd.answered(false, ms(30));
        assert_eq!(aimd.allowed(), 3);
        for _ in 0..100 {
            aimd.answered(false, ms(30));
        }
        assert_eq!(aimd.allowed(), 8, "at most the maximum");
        // Slow: 8 → 4 → 2 → 1.
        aimd.answered(true, ms(900));
        assert_eq!(aimd.allowed(), 4);
        aimd.answered(true, ms(900));
        aimd.answered(true, ms(900));
        assert_eq!(aimd.allowed(), 1);
        assert_eq!(aimd.gap(), Duration::ZERO);
        // At one in flight, a slow answer pauses: 0.9, then 2 × 0.9 + 0.9.
        aimd.answered(true, ms(900));
        assert_eq!(aimd.gap(), ms(900));
        aimd.answered(true, ms(900));
        assert_eq!(aimd.gap(), ms(2_700));
        for _ in 0..10 {
            aimd.answered(true, ms(5_000));
        }
        assert_eq!(aimd.gap(), MAX_GAP);
        // Fast again: the pause halves away first, then the window grows.
        let mut fast = 0;
        while !aimd.gap().is_zero() {
            aimd.answered(false, ms(30));
            fast += 1;
        }
        assert_eq!(fast, 10, "10 s → under 10 ms in ten halvings");
        assert_eq!(aimd.allowed(), 1);
        aimd.answered(false, ms(30));
        assert_eq!(aimd.allowed(), 2);
        // A start above the maximum is clamped.
        assert_eq!(Aimd::new(20, 4).allowed(), 4);
        assert_eq!(Aimd::new(0, 4).allowed(), 1);
    }

    /// Fifty clients that each pace their own batches by their own answers
    /// share a master that can answer two requests at a time without
    /// slowing down: each settles at a window or a pause that keeps the
    /// master's total near what it can take, instead of all of them at
    /// their maximum. (A deterministic model: the master's answer time
    /// grows with what is in flight overall.)
    #[test]
    fn fifty_paced_clients_share_a_master_instead_of_swamping_it() {
        let rules = rules();
        let baseline = ms(40);
        let mut clients: Vec<Aimd> = (0..50).map(|_| Aimd::new(1, 8)).collect();
        let mut took = baseline;
        let mut answers = Vec::new();
        for _round in 0..400 {
            // What is in flight on average: a client's window, for the
            // share of the time it isn't pausing.
            #[expect(clippy::cast_precision_loss, reason = "small counts")]
            let total: f64 = clients
                .iter()
                .map(|client| {
                    let busy = took.as_secs_f64() / (took + client.gap()).as_secs_f64();
                    client.allowed() as f64 * busy
                })
                .sum();
            // Two at a time at 40 ms; beyond that, each waits its turn.
            took = baseline.mul_f64((total / 2.0).max(1.0));
            for client in &mut clients {
                client.answered(slow(rules, Some(baseline), took), took);
            }
            answers.push(took);
        }
        // Without pacing: 50 × 8 = 400 in flight, answers of 8 s.
        let settled = &answers[200..];
        #[expect(clippy::cast_precision_loss, reason = "a few hundred rounds")]
        let mean = settled.iter().map(Duration::as_secs_f64).sum::<f64>() / settled.len() as f64;
        let worst = settled.iter().max().unwrap();
        assert!(mean < 0.5, "mean answer {mean:.3} s");
        assert!(
            *worst < Duration::from_millis(1_500),
            "worst answer {worst:?}"
        );
    }

    #[test]
    fn the_team_spread_follows_the_count_of_api_clients() {
        let per = ms(300);
        let (low, high) = (Duration::from_secs(2), Duration::from_secs(20));
        assert_eq!(team_spread(None, per, low, high), low);
        assert_eq!(team_spread(Some(3), per, low, high), low);
        assert_eq!(
            team_spread(Some(40), per, low, high),
            Duration::from_secs(12)
        );
        assert_eq!(team_spread(Some(500), per, low, high), high);
    }
}
