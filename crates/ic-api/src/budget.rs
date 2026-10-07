//! A request budget for by-name queries: a token bucket shared by every
//! clone of a [`Client`](crate::Client) it is attached to
//! ([`Client::with_budget`](crate::Client::with_budget)).
//!
//! Each by-name request (one batch of at most
//! [`NAMES_PER_REQUEST`](crate::NAMES_PER_REQUEST) names, including the
//! halves of a batch split to find an unknown name) takes a token. Tokens
//! come back at one per `interval` up to `burst`, so a client sends at
//! most `burst` requests at once and one per `interval` after that. A
//! request that finds no token waits its turn (first come, first served:
//! each waiter reserves the next free slot), so the budget paces a long
//! list of names without ever refusing one.
//!
//! The object the user is opening goes first
//! ([`Client::priority`](crate::Client::priority)): its request takes a
//! token without waiting, borrowing one if none is left, which the next
//! requests then wait off. Small installations never run out; large ones
//! spread their by-name requests out by themselves.

use std::sync::Mutex;
use std::time::Duration;

use tokio::time::Instant;

/// A token bucket for by-name requests (see the module notes).
#[derive(Debug)]
pub struct RequestBudget {
    /// One token comes back per interval; zero: no limit.
    interval: Duration,
    /// At most this many tokens are saved up.
    burst: f64,
    state: Mutex<State>,
}

#[derive(Debug)]
struct State {
    /// Tokens available; below zero while requests wait their turn (or
    /// a priority request borrowed one).
    tokens: f64,
    /// When `tokens` was last brought up to date.
    at: Instant,
    /// Requests counted so far.
    taken: u64,
    /// Requests that had to wait.
    waited: u64,
}

impl RequestBudget {
    /// A budget of one request per `interval` with bursts of `burst` (at
    /// least 1); it starts full. A zero `interval` never makes a request
    /// wait (it still counts them).
    #[must_use]
    pub fn new(interval: Duration, burst: u32) -> Self {
        let burst = f64::from(burst.max(1));
        Self {
            interval,
            burst,
            state: Mutex::new(State {
                tokens: burst,
                at: Instant::now(),
                taken: 0,
                waited: 0,
            }),
        }
    }

    /// Takes a token, and how long to wait for it (zero: one was there).
    fn reserve(&self, now: Instant) -> Duration {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.taken += 1;
        if self.interval.is_zero() {
            return Duration::ZERO;
        }
        let refill =
            now.saturating_duration_since(state.at).as_secs_f64() / self.interval.as_secs_f64();
        state.tokens = (state.tokens + refill).min(self.burst);
        state.at = now;
        state.tokens -= 1.0;
        if state.tokens >= 0.0 {
            Duration::ZERO
        } else {
            state.waited += 1;
            self.interval.mul_f64(-state.tokens)
        }
    }

    /// Waits for a token (first come, first served). A waiter that is
    /// dropped keeps its slot used: the budget errs towards fewer
    /// requests.
    pub async fn acquire(&self) {
        let wait = self.reserve(Instant::now());
        if !wait.is_zero() {
            tracing::trace!(?wait, "request budget: waiting for a token");
            tokio::time::sleep(wait).await;
        }
    }

    /// Takes a token without waiting, borrowing one when none is left (the
    /// object the user is opening goes before everything else).
    pub fn take_now(&self) {
        let _ = self.reserve(Instant::now());
    }

    /// How many requests the budget counted so far, and how many of them
    /// had to wait for a token.
    #[must_use]
    pub fn counts(&self) -> (u64, u64) {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (state.taken, state.waited)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn bursts_then_paces() {
        let budget = RequestBudget::new(Duration::from_millis(200), 10);
        let started = Instant::now();
        for _ in 0..10 {
            budget.acquire().await;
        }
        assert_eq!(started.elapsed(), Duration::ZERO, "a burst of 10 at once");
        for _ in 0..5 {
            budget.acquire().await;
        }
        assert_eq!(
            started.elapsed(),
            Duration::from_secs(1),
            "then one per 200 ms"
        );
        assert_eq!(budget.counts(), (15, 5));
        // Resting refills it, up to the burst.
        tokio::time::sleep(Duration::from_mins(1)).await;
        let rested = Instant::now();
        for _ in 0..10 {
            budget.acquire().await;
        }
        assert_eq!(rested.elapsed(), Duration::ZERO);
        budget.acquire().await;
        assert_eq!(rested.elapsed(), Duration::from_millis(200));
    }

    #[tokio::test(start_paused = true)]
    async fn waiters_are_served_in_order() {
        let budget = std::sync::Arc::new(RequestBudget::new(Duration::from_millis(100), 1));
        budget.acquire().await;
        let started = Instant::now();
        let mut tasks = Vec::new();
        for _ in 0..3 {
            let budget = std::sync::Arc::clone(&budget);
            tasks.push(tokio::spawn(async move {
                budget.acquire().await;
                Instant::now()
            }));
        }
        let mut done = Vec::new();
        for task in tasks {
            done.push(task.await.unwrap().duration_since(started));
        }
        done.sort();
        assert_eq!(
            done,
            [100, 200, 300].map(Duration::from_millis),
            "each waiter took the next free slot"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn priority_never_waits_but_counts() {
        let budget = RequestBudget::new(Duration::from_millis(200), 2);
        budget.acquire().await;
        budget.acquire().await;
        let started = Instant::now();
        budget.take_now();
        assert_eq!(started.elapsed(), Duration::ZERO, "it borrowed a token");
        budget.acquire().await;
        assert_eq!(
            started.elapsed(),
            Duration::from_millis(400),
            "the next request waits off the borrowed token too"
        );
        assert_eq!(budget.counts().0, 4);
    }

    #[tokio::test(start_paused = true)]
    async fn a_zero_interval_never_waits() {
        let budget = RequestBudget::new(Duration::ZERO, 1);
        let started = Instant::now();
        for _ in 0..100 {
            budget.acquire().await;
        }
        assert_eq!(started.elapsed(), Duration::ZERO);
        assert_eq!(budget.counts(), (100, 0));
    }
}
