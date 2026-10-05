//! The checker's run queue: forced re-checks (`reschedule-check`) and
//! bursts wait here and run at a limited rate, the way Icinga's
//! `CheckerComponent` works through due checks as fast as its concurrency
//! allows. With 2 000 hosts and 30 000 services a forced re-check of
//! everything produced about 5 000 results per second (docs/performance.md).
//!
//! The server's real-time driver calls [`World::checker_step`] every few
//! milliseconds; tests can run queued checks right away with
//! [`World::run_queued_checks`].

use std::collections::{HashSet, VecDeque};

use super::World;
use super::types::Checkable;

/// Checks waiting to run, and how fast they may run.
#[derive(Debug)]
pub(crate) struct CheckQueue {
    queue: VecDeque<String>,
    /// The objects in `queue` (each waits at most once).
    queued: HashSet<String>,
    /// Checks per second.
    rate: f64,
    /// Checks that may run now (a token bucket).
    credit: f64,
}

impl CheckQueue {
    /// An empty queue that runs `rate` checks per second.
    pub(crate) fn new(rate: f64) -> Self {
        Self {
            queue: VecDeque::new(),
            queued: HashSet::new(),
            rate,
            credit: 0.0,
        }
    }

    /// Checks waiting.
    pub(crate) fn len(&self) -> usize {
        self.queue.len()
    }

    /// Whether no check is waiting.
    pub(crate) fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// Changes the rate (checks per second).
    pub(crate) fn set_rate(&mut self, rate: f64) {
        self.rate = rate;
    }

    /// Queues a check of `object` unless one is waiting already. Returns
    /// whether it was queued.
    pub(crate) fn push(&mut self, object: &str) -> bool {
        if self.queued.contains(object) {
            return false;
        }
        self.queued.insert(object.to_owned());
        self.queue.push_back(object.to_owned());
        true
    }

    fn pop(&mut self) -> Option<String> {
        let object = self.queue.pop_front()?;
        self.queued.remove(&object);
        Some(object)
    }
}

impl World {
    /// A burst: queues a check of every host and service (like a forced
    /// `reschedule-check` of everything, or an Icinga restart). Returns how
    /// many checks were queued; objects already waiting count once.
    pub(crate) fn queue_all_checks(&mut self) -> usize {
        let names: Vec<String> = self.all_checkables().map(Checkable::full_name).collect();
        names.iter().filter(|name| self.checks.push(name)).count()
    }

    /// Runs up to `max` queued checks now, whatever the rate. Returns how
    /// many ran.
    pub(crate) fn run_queued_checks(&mut self, max: usize) -> usize {
        let mut ran = 0;
        while ran < max {
            let Some(object) = self.checks.pop() else {
                break;
            };
            self.run_check(&object);
            ran += 1;
        }
        ran
    }

    /// One step of the real-time checker, `elapsed` seconds after the last
    /// one: runs as many queued checks as the rate allows. An idle checker
    /// saves no credit, so a new burst starts at the configured pace, and
    /// a stalled one catches up with at most one second's worth at once.
    pub(crate) fn checker_step(&mut self, elapsed: f64) -> usize {
        if self.checks.is_empty() {
            self.checks.credit = 0.0;
            return 0;
        }
        let rate = self.checks.rate;
        let credit = (self.checks.credit + elapsed.max(0.0) * rate).min(rate.max(1.0));
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a non-negative credit of at most one second's worth of checks"
        )]
        let allowed = credit.floor() as usize;
        let ran = self.run_queued_checks(allowed);
        #[expect(
            clippy::cast_precision_loss,
            reason = "at most one second's worth of checks"
        )]
        let spent = ran as f64;
        self.checks.credit = credit - spent;
        ran
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_object_waits_once_in_order() {
        let mut queue = CheckQueue::new(10.0);
        assert!(queue.push("a"));
        assert!(queue.push("b"));
        assert!(!queue.push("a"), "already waiting");
        assert_eq!(queue.len(), 2);
        assert_eq!(queue.pop().as_deref(), Some("a"));
        assert!(queue.push("a"), "queued again once it ran");
        assert_eq!(queue.pop().as_deref(), Some("b"));
        assert_eq!(queue.pop().as_deref(), Some("a"));
        assert_eq!(queue.pop(), None);
    }
}
