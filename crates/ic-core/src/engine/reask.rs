//! Asking Icinga again by name for problems whose downtime depth is
//! provisional (the store's notes on downtime depths;
//! `Store::take_unsettled`).
//!
//! - **Spread:** every client reads the same downtime events at the same
//!   moment (a schedule starting downtimes on thousands of objects reaches
//!   fifty clients in the same second), so asking at once would line them
//!   up (no synchronised clients, PLAN.md §4.2). A batch waits a random
//!   delay of up to `Tuning::reask_spread` per request it takes
//!   ([`ic_api::NAMES_PER_REQUEST`] names), at most [`MAX_SPREAD`]; what
//!   the store hands out meanwhile joins it. One problem waits up to 2 s,
//!   a thousand up to 10 s.
//! - **Retry:** a request that fails leaves the objects provisional, so
//!   they are asked again after `Tuning::reask_retry`, doubling up to
//!   [`MAX_RETRY`] (with jitter) while requests keep failing; an answered
//!   round starts the delay over.
//!
//! What goes out is asked of the store first (`Store::keep_asking`): an
//! object whose depth settled meanwhile, or that is no longer a problem,
//! isn't asked for.

use std::collections::BTreeSet;
use std::time::Duration;

use ic_model::ObjectKey;
use tokio::time::Instant;

use crate::backoff::Backoff;

/// The longest a batch is spread over.
pub(super) const MAX_SPREAD: Duration = Duration::from_secs(30);

/// The longest wait before a failed request is tried again.
pub(super) const MAX_RETRY: Duration = Duration::from_mins(5);

/// The re-asks waiting for their moment, and those whose request failed.
#[derive(Debug)]
pub(super) struct Reasks {
    /// The spread per request.
    spread: Duration,
    /// Handed out by the store, waiting.
    waiting: BTreeSet<ObjectKey>,
    /// When the first of them was handed out, and where in its spread
    /// the batch goes out (`[0, 1)`).
    since: Option<(Instant, f64)>,
    /// Whose request failed.
    failed: BTreeSet<ObjectKey>,
    /// When they are asked again.
    retry_at: Option<Instant>,
    backoff: Backoff,
}

impl Reasks {
    pub(super) fn new(spread: Duration, retry: Duration) -> Self {
        Self {
            spread,
            waiting: BTreeSet::new(),
            since: None,
            failed: BTreeSet::new(),
            retry_at: None,
            backoff: Backoff::new(retry, MAX_RETRY),
        }
    }

    /// Adds what the store handed out; `unit` draws the batch's point in
    /// its spread (`[0, 1)`, the engine's jitter) when it is the first.
    pub(super) fn add(&mut self, keys: Vec<ObjectKey>, now: Instant, unit: impl FnOnce() -> f64) {
        if keys.is_empty() {
            return;
        }
        if self.since.is_none() {
            self.since = Some((now, unit()));
        }
        self.waiting.extend(keys);
    }

    /// When the waiting batch goes out.
    fn batch_due(&self) -> Option<Instant> {
        let (since, unit) = self.since?;
        let requests = self.waiting.len().div_ceil(ic_api::NAMES_PER_REQUEST);
        let spread = self
            .spread
            .saturating_mul(u32::try_from(requests.max(1)).unwrap_or(u32::MAX))
            .min(MAX_SPREAD);
        Some(since + spread.mul_f64(unit))
    }

    /// When something is due.
    pub(super) fn due(&self) -> Option<Instant> {
        [self.batch_due(), self.retry_at]
            .into_iter()
            .flatten()
            .min()
    }

    /// Takes what is due at `now`.
    pub(super) fn take_due(&mut self, now: Instant) -> Vec<ObjectKey> {
        let mut due = Vec::new();
        if self.batch_due().is_some_and(|at| at <= now) {
            self.since = None;
            due.extend(std::mem::take(&mut self.waiting));
        }
        if self.retry_at.is_some_and(|at| at <= now) {
            self.retry_at = None;
            due.extend(std::mem::take(&mut self.failed));
        }
        due
    }

    /// The request that asked for `keys` (those still provisional) failed;
    /// `random` in `[0, 1)` for the backoff's jitter.
    pub(super) fn failed(&mut self, keys: Vec<ObjectKey>, now: Instant, random: f64) {
        if keys.is_empty() {
            return;
        }
        self.failed.extend(keys);
        if self.retry_at.is_none() {
            self.retry_at = Some(now + self.backoff.fail(random));
        }
    }

    /// A round of by-name requests was answered: the next failure waits
    /// the first delay again.
    pub(super) fn answered(&mut self) {
        self.backoff.reset();
    }

    /// The session ended: its requests are gone (the store hands out again
    /// what is still provisional, `Store::new_session`).
    pub(super) fn reset(&mut self) {
        self.waiting.clear();
        self.since = None;
        self.failed.clear();
        self.retry_at = None;
        self.backoff.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(count: usize) -> Vec<ObjectKey> {
        (0..count)
            .map(|index| ObjectKey::service("h", &format!("s{index}")))
            .collect()
    }

    #[test]
    fn a_batch_is_spread_by_the_requests_it_takes() {
        let now = Instant::now();
        let mut reasks = Reasks::new(Duration::from_secs(2), Duration::from_secs(5));
        assert_eq!(reasks.due(), None);
        reasks.add(keys(1), now, || 0.5);
        assert_eq!(
            reasks.due(),
            Some(now + Duration::from_secs(1)),
            "half of 2 s"
        );
        // What comes meanwhile joins it, and moves it within the spread.
        reasks.add(keys(1_000), now + Duration::from_millis(100), || 0.9);
        assert_eq!(
            reasks.due(),
            Some(now + Duration::from_secs(5)),
            "half of 5 requests' 10 s, drawn once"
        );
        assert!(reasks.take_due(now + Duration::from_secs(4)).is_empty());
        assert_eq!(reasks.take_due(now + Duration::from_secs(5)).len(), 1_000);
        assert_eq!(reasks.due(), None);
        // A huge batch: at most 30 s.
        reasks.add(keys(100_000), now, || 0.999);
        assert!(reasks.due().unwrap() <= now + MAX_SPREAD);
    }

    #[test]
    fn failed_requests_are_retried_with_backoff() {
        let now = Instant::now();
        let mut reasks = Reasks::new(Duration::from_secs(2), Duration::from_secs(4));
        reasks.failed(keys(2), now, 0.0);
        // Half of the delay is jitter: 2 to 4 s.
        let first = reasks.due().unwrap();
        assert_eq!(first, now + Duration::from_secs(2));
        assert_eq!(reasks.take_due(first).len(), 2);
        reasks.failed(keys(2), first, 0.0);
        assert_eq!(
            reasks.due(),
            Some(first + Duration::from_secs(4)),
            "doubled"
        );
        reasks.answered();
        reasks.take_due(first + Duration::from_secs(4));
        reasks.failed(keys(1), first, 0.0);
        assert_eq!(reasks.due(), Some(first + Duration::from_secs(2)), "over");
        reasks.reset();
        assert_eq!(reasks.due(), None);
    }
}
