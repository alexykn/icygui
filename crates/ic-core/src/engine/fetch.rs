//! Re-queries by name: objects that config changes touched
//! (`ObjectCreated`/`Modified`/`Deleted`), objects events mention that the
//! store doesn't know, and action targets. Deduplicated, collected for a
//! moment, one round at a time, at most [`MAX_ROUND`] names per round in
//! batches of [`ic_api::NAMES_PER_REQUEST`].

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use ic_api::{ApiError, Client, Detail, Fetched};
use ic_model::{Dependency, Endpoint, HostGroup, ObjectKey, ServiceGroup};
use tokio::sync::mpsc::UnboundedSender;
use tokio::time::Instant;

use super::Internal;

/// Names per round; the rest waits for the next round.
pub(super) const MAX_ROUND: usize = 1_000;

/// The small lists a config change can make stale.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "one independent flag per list"
)]
pub(super) struct Lists {
    pub(super) host_groups: bool,
    pub(super) service_groups: bool,
    pub(super) dependencies: bool,
    pub(super) endpoints: bool,
}

impl Lists {
    fn any(self) -> bool {
        self.host_groups || self.service_groups || self.dependencies || self.endpoints
    }

    fn or(self, other: Self) -> Self {
        Self {
            host_groups: self.host_groups || other.host_groups,
            service_groups: self.service_groups || other.service_groups,
            dependencies: self.dependencies || other.dependencies,
            endpoints: self.endpoints || other.endpoints,
        }
    }
}

/// What to fetch, deduplicated, and what Icinga recently said doesn't
/// exist.
#[derive(Debug)]
pub(super) struct FetchQueue {
    pending: BTreeSet<ObjectKey>,
    lists: Lists,
    /// When the oldest pending entry was marked.
    since: Option<Instant>,
    /// Publish as soon as the answer is in (action targets).
    urgent: bool,
    in_flight: bool,
    /// Objects Icinga answered as unknown (deleted, or hidden by a
    /// filtered permission), and when: events about them don't cause
    /// re-queries until `missing_ttl` passed or they are created again.
    missing: HashMap<ObjectKey, Instant>,
    missing_ttl: Duration,
    /// Unknown objects seen while a load ran: re-queried after it.
    deferred: BTreeSet<ObjectKey>,
}

/// One round of re-queries.
#[derive(Debug, Default)]
pub(super) struct Round {
    pub(super) keys: Vec<ObjectKey>,
    pub(super) lists: Lists,
    pub(super) urgent: bool,
}

/// The answers of one round.
#[derive(Debug)]
pub(crate) struct Answers {
    /// Per detail level: `started` and the answer.
    pub(super) objects: Vec<(Detail, u64, Result<Fetched, ApiError>)>,
    pub(super) host_groups: Option<Result<Vec<HostGroup>, ApiError>>,
    pub(super) service_groups: Option<Result<Vec<ServiceGroup>, ApiError>>,
    pub(super) dependencies: Option<Result<Vec<Dependency>, ApiError>>,
    pub(super) endpoints: Option<Result<Vec<Endpoint>, ApiError>>,
    pub(super) urgent: bool,
}

impl FetchQueue {
    pub(super) fn new(missing_ttl: Duration) -> Self {
        Self {
            pending: BTreeSet::new(),
            lists: Lists::default(),
            since: None,
            urgent: false,
            in_flight: false,
            missing: HashMap::new(),
            missing_ttl,
            deferred: BTreeSet::new(),
        }
    }

    fn touch(&mut self, now: Instant) {
        self.since.get_or_insert(now);
    }

    /// Whether Icinga said recently that `key` doesn't exist.
    fn recently_missing(&self, key: &ObjectKey, now: Instant) -> bool {
        self.missing
            .get(key)
            .is_some_and(|at| now.duration_since(*at) < self.missing_ttl)
    }

    /// Re-queries `key` (an unknown object, a modified or deleted one),
    /// unless Icinga said recently that it doesn't exist.
    pub(super) fn mark(&mut self, key: ObjectKey, now: Instant) {
        if self.recently_missing(&key, now) {
            return;
        }
        self.pending.insert(key);
        self.touch(now);
    }

    /// Re-queries a created object, even if it was missing before.
    pub(super) fn mark_created(&mut self, key: ObjectKey, now: Instant) {
        self.missing.remove(&key);
        self.pending.insert(key);
        self.touch(now);
    }

    /// Re-queries action targets and publishes right after.
    pub(super) fn mark_urgent(&mut self, keys: impl IntoIterator<Item = ObjectKey>, now: Instant) {
        let before = self.pending.len();
        self.pending.extend(keys);
        if self.pending.len() > before {
            self.urgent = true;
            self.touch(now);
        }
    }

    /// Reloads small lists.
    pub(super) fn mark_lists(&mut self, lists: Lists, now: Instant) {
        if lists.any() {
            self.lists = self.lists.or(lists);
            self.touch(now);
        }
    }

    /// An unknown object while a load runs: decided after the load.
    pub(super) fn defer(&mut self, key: ObjectKey) {
        self.deferred.insert(key);
    }

    /// After a load: re-queries the objects deferred during it (events
    /// about them were dropped, so even those the load brought may be
    /// behind).
    pub(super) fn release_deferred(&mut self, now: Instant) {
        for key in std::mem::take(&mut self.deferred) {
            self.mark(key, now);
        }
    }

    /// When the next round may start: `delay` after the oldest mark, never
    /// while a round runs.
    pub(super) fn due(&self, delay: Duration) -> Option<Instant> {
        if self.in_flight || (self.pending.is_empty() && !self.lists.any()) {
            return None;
        }
        self.since.map(|since| since + delay)
    }

    /// Takes the next round (at most [`MAX_ROUND`] names).
    pub(super) fn take(&mut self, now: Instant) -> Round {
        let mut keys = Vec::new();
        while keys.len() < MAX_ROUND {
            match self.pending.pop_first() {
                Some(key) => keys.push(key),
                None => break,
            }
        }
        let round = Round {
            keys,
            lists: std::mem::take(&mut self.lists),
            urgent: std::mem::take(&mut self.urgent),
        };
        self.in_flight = true;
        self.since = (!self.pending.is_empty()).then_some(now);
        round
    }

    /// A round finished; `missing` didn't exist.
    pub(super) fn finished(&mut self, missing: &[ObjectKey], now: Instant) {
        self.in_flight = false;
        self.missing
            .retain(|_, at| now.duration_since(*at) < self.missing_ttl);
        for key in missing {
            self.missing.insert(key.clone(), now);
        }
    }

    /// Forgets pending work (the connection is gone; the reload after the
    /// reconnect brings everything). The missing names stay.
    pub(super) fn reset(&mut self) {
        self.pending.clear();
        self.lists = Lists::default();
        self.since = None;
        self.urgent = false;
        self.in_flight = false;
        self.deferred.clear();
    }
}

/// What a fetch task needs.
pub(super) struct FetchTask {
    pub(super) client: Client,
    pub(super) seq: Arc<AtomicU64>,
    pub(super) tx: UnboundedSender<Internal>,
    pub(super) session: u64,
    /// Hosts and services loaded in full before.
    pub(super) full: Vec<ObjectKey>,
    /// Services known only lean (or not at all).
    pub(super) lean: Vec<ObjectKey>,
    pub(super) lists: Lists,
    pub(super) urgent: bool,
}

impl FetchTask {
    /// Runs the round and reports its answers.
    pub(super) async fn run(self) {
        let mut objects = Vec::new();
        for (detail, keys) in [(Detail::Full, &self.full), (Detail::Lean, &self.lean)] {
            if keys.is_empty() {
                continue;
            }
            let started = self.seq.load(Ordering::SeqCst);
            objects.push((detail, started, self.client.objects(keys, detail).await));
        }
        let lists = self.lists;
        let answers = Answers {
            objects,
            host_groups: if lists.host_groups {
                Some(self.client.host_groups().await)
            } else {
                None
            },
            service_groups: if lists.service_groups {
                Some(self.client.service_groups().await)
            } else {
                None
            },
            dependencies: if lists.dependencies {
                Some(self.client.dependencies().await)
            } else {
                None
            },
            endpoints: if lists.endpoints {
                Some(self.client.endpoints().await)
            } else {
                None
            },
            urgent: self.urgent,
        };
        let _ = self.tx.send(Internal::Fetched {
            session: self.session,
            answers: Box::new(answers),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedupes_and_respects_missing() {
        let now = Instant::now();
        let mut queue = FetchQueue::new(Duration::from_mins(10));
        let a = ObjectKey::service("h", "a");
        queue.mark(a.clone(), now);
        queue.mark(a.clone(), now);
        assert_eq!(
            queue.due(Duration::from_millis(200)),
            Some(now + Duration::from_millis(200))
        );
        let round = queue.take(now);
        assert_eq!(round.keys, std::slice::from_ref(&a));
        assert!(!round.urgent);
        assert_eq!(queue.due(Duration::ZERO), None, "one round at a time");
        queue.finished(std::slice::from_ref(&a), now);
        assert_eq!(queue.due(Duration::ZERO), None);

        // Missing: no re-query for events, until created again.
        queue.mark(a.clone(), now + Duration::from_secs(1));
        assert_eq!(queue.due(Duration::ZERO), None);
        queue.mark_created(a.clone(), now + Duration::from_secs(2));
        assert!(queue.due(Duration::ZERO).is_some());
        let round = queue.take(now);
        assert_eq!(round.keys, std::slice::from_ref(&a));
        queue.finished(&[], now);

        // After the TTL it may be re-queried again.
        queue.finished(std::slice::from_ref(&a), now);
        queue.mark(a.clone(), now + Duration::from_mins(11));
        assert!(queue.due(Duration::ZERO).is_some());
    }

    #[test]
    fn rounds_are_bounded_and_urgent_marks_publish() {
        let now = Instant::now();
        let mut queue = FetchQueue::new(Duration::from_mins(10));
        for index in 0..MAX_ROUND + 5 {
            queue.mark(ObjectKey::host(&format!("h{index:05}")), now);
        }
        queue.mark_urgent([ObjectKey::host("action-target")], now);
        let round = queue.take(now);
        assert_eq!(round.keys.len(), MAX_ROUND);
        assert!(round.urgent);
        queue.finished(&[], now);
        let round = queue.take(now);
        assert_eq!(round.keys.len(), 6);
        assert!(!round.urgent);
    }

    #[test]
    fn deferred_keys_wait_for_the_load() {
        let now = Instant::now();
        let mut queue = FetchQueue::new(Duration::from_mins(10));
        queue.defer(ObjectKey::host("new"));
        assert_eq!(queue.due(Duration::ZERO), None);
        queue.release_deferred(now);
        assert_eq!(queue.take(now).keys, [ObjectKey::host("new")]);
        queue.mark_lists(
            Lists {
                endpoints: true,
                ..Lists::default()
            },
            now,
        );
        queue.reset();
        assert_eq!(queue.due(Duration::ZERO), None);
    }
}
