//! Re-queries by name: objects that config changes touched
//! (`ObjectCreated`/`Modified`/`Deleted`), objects events mention that the
//! store doesn't know, action targets, overdue objects (the freshness
//! watchdog), hydration (`Command::Hydrate`: full details of lean
//! objects) and Icinga's `Notification` objects of an object Icinga just
//! notified about. Deduplicated (also against the round in flight),
//! collected for a moment, one round at a time, at most [`MAX_ROUND`]
//! names per round (and as many notification names) in batches of
//! [`ic_api::NAMES_PER_REQUEST`].
//!
//! **Unknown objects** (events about objects the store doesn't know) are
//! looked up separately, at most [`MAX_UNKNOWN_ROUND`] per round and
//! without isolating unknown names ([`Client::objects_unsplit`]): an API
//! user with filtered `objects/query/*` permissions receives the events of
//! every object (`events/*` can't be filtered) but may query only some, and
//! isolating each hidden name would cost about two requests per name. A
//! batch Icinga can't answer as a whole counts as missing.
//!
//! **Priorities** (PERF-09): full fetches (hydration of the rows on
//! screen, a notified object's prefetch) go first in every round, then
//! re-queries; problems refreshed after quiet mode (the *background*
//! lane) only fill a round without full fetches, one request's worth at a
//! time, so rows on screen never wait behind them. The object the user is
//! opening doesn't queue at all (`Command::Focus`). Every request also
//! takes a token from the client's request budget
//! ([`ic_api::RequestBudget`]).
//!
//! **Missing names** (deleted, hidden, or in such a batch) aren't asked for
//! again for events about them within `missing_ttl` (10 minutes), unless
//! `ObjectCreated` names them; each time they come back missing the wait
//! doubles, up to [`MAX_MISSING_TTL`]. Kinds the API user may not query at
//! all (no `objects/query/Host` or `Service`) are never asked for.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use ic_api::{ApiError, Client, Detail, Fetched, FetchedNotifications};
use ic_model::{Dependency, HostGroup, ObjectKey, ServiceGroup};
use tokio::sync::mpsc::UnboundedSender;
use tokio::time::Instant;

use super::Internal;

/// Names per round; the rest waits for the next round.
pub(super) const MAX_ROUND: usize = 1_000;

/// Unknown objects looked up per round (one request): action targets,
/// config changes and hydration never wait behind many of them.
pub(super) const MAX_UNKNOWN_ROUND: usize = ic_api::NAMES_PER_REQUEST;

/// The longest wait before a name that keeps coming back missing is asked
/// for again.
pub(super) const MAX_MISSING_TTL: Duration = Duration::from_hours(4);

/// At most this many objects wait for a full fetch (hydration); more are
/// dropped with a warning (the UI asks again for what it still shows).
pub(super) const MAX_PENDING_FULL: usize = 5_000;

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
    /// Re-queried with the detail the store has (full if loaded in full).
    pending: BTreeSet<ObjectKey>,
    /// Fetched in full whatever the store has (hydration).
    full: BTreeSet<ObjectKey>,
    /// Fetched in full when nothing else is waiting: problems whose check
    /// result may have changed while quiet mode left out check results.
    background: BTreeSet<ObjectKey>,
    /// Objects the store doesn't know, looked up without isolating unknown
    /// names (see the module notes).
    unknown: BTreeSet<ObjectKey>,
    /// The names of the round in flight.
    flying: HashSet<ObjectKey>,
    /// Those of them fetched in full (hydration, prefetch, background).
    flying_full: HashSet<ObjectKey>,
    lists: Lists,
    /// When the oldest pending entry was marked.
    since: Option<Instant>,
    /// Publish as soon as the answer is in (action targets).
    urgent: bool,
    in_flight: bool,
    /// Objects Icinga answered as unknown (deleted, or hidden by a
    /// filtered permission): events about them don't cause re-queries
    /// until their wait passed or they are created again.
    missing: HashMap<ObjectKey, Missing>,
    missing_ttl: Duration,
    /// Unknown objects seen while a load ran: re-queried after it.
    deferred: BTreeSet<ObjectKey>,
    /// `Notification` objects to re-read, by full name.
    notifications: BTreeSet<String>,
    /// Kinds the API user may not query: never asked for.
    refused: Refused,
}

/// A name Icinga answered as unknown.
#[derive(Clone, Copy, Debug)]
struct Missing {
    /// Not asked for again (for events) before this.
    until: Instant,
    /// The current wait; it doubles each time the name comes back missing.
    ttl: Duration,
}

/// The kinds the API user may not query by name.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Refused {
    hosts: bool,
    services: bool,
}

impl Refused {
    fn covers(self, key: &ObjectKey) -> bool {
        match key {
            ObjectKey::Host { .. } => self.hosts,
            ObjectKey::Service { .. } => self.services,
        }
    }
}

/// One round of re-queries.
#[derive(Debug, Default)]
pub(super) struct Round {
    /// Re-queried with the detail the store has.
    pub(super) keys: Vec<ObjectKey>,
    /// Fetched in full.
    pub(super) full: Vec<ObjectKey>,
    /// Unknown to the store: looked up in whole batches.
    pub(super) unknown: Vec<ObjectKey>,
    pub(super) lists: Lists,
    pub(super) urgent: bool,
    /// `Notification` objects to re-read.
    pub(super) notifications: Vec<String>,
}

/// One query of a round (one kind, hosts or services): the detail, the
/// names, the reader's line count when it was sent, and the answer.
/// `whole`: a lookup of unknown objects without isolating unknown names, so
/// `missing` only says they weren't found (see the module notes).
#[derive(Debug)]
pub(crate) struct Answer {
    pub(super) detail: Detail,
    pub(super) keys: Vec<ObjectKey>,
    pub(super) started: u64,
    pub(super) whole: bool,
    pub(super) result: Result<Fetched, ApiError>,
}

/// The answers of one round.
#[derive(Debug)]
pub(crate) struct Answers {
    /// Per detail level.
    pub(super) objects: Vec<Answer>,
    pub(super) host_groups: Option<Result<Vec<HostGroup>, ApiError>>,
    pub(super) service_groups: Option<Result<Vec<ServiceGroup>, ApiError>>,
    pub(super) dependencies: Option<Result<Vec<Dependency>, ApiError>>,
    pub(super) endpoints: Option<Result<ic_api::Cluster, ApiError>>,
    /// The `Notification` objects re-read: the reader's line count when
    /// the query was sent, and the answer.
    pub(super) notifications: Option<(u64, Result<FetchedNotifications, ApiError>)>,
    pub(super) urgent: bool,
}

impl FetchQueue {
    pub(super) fn new(missing_ttl: Duration) -> Self {
        Self {
            pending: BTreeSet::new(),
            full: BTreeSet::new(),
            background: BTreeSet::new(),
            unknown: BTreeSet::new(),
            flying: HashSet::new(),
            flying_full: HashSet::new(),
            lists: Lists::default(),
            since: None,
            urgent: false,
            in_flight: false,
            missing: HashMap::new(),
            missing_ttl,
            deferred: BTreeSet::new(),
            notifications: BTreeSet::new(),
            refused: Refused::default(),
        }
    }

    fn touch(&mut self, now: Instant) {
        self.since.get_or_insert(now);
    }

    /// Whether Icinga said recently that `key` doesn't exist.
    fn recently_missing(&self, key: &ObjectKey, now: Instant) -> bool {
        self.missing
            .get(key)
            .is_some_and(|missing| now < missing.until)
    }

    /// The API user may not query hosts (or services) by name: they are
    /// never asked for, and those queued are dropped.
    pub(super) fn refuse(&mut self, hosts: bool, services: bool) {
        self.refused.hosts |= hosts;
        self.refused.services |= services;
        let refused = self.refused;
        for set in [
            &mut self.pending,
            &mut self.full,
            &mut self.background,
            &mut self.unknown,
            &mut self.deferred,
        ] {
            set.retain(|key| !refused.covers(key));
        }
    }

    /// Re-queries `key` (a modified or deleted object, an overdue one, an
    /// object a load may have left behind), unless Icinga said recently
    /// that it doesn't exist or the user may not query its kind. Returns
    /// whether it will be re-queried (a key already queued counts).
    pub(super) fn mark(&mut self, key: ObjectKey, now: Instant) -> bool {
        if self.refused.covers(&key) || self.recently_missing(&key, now) {
            return false;
        }
        if !self.full.contains(&key) && !self.background.contains(&key) {
            self.unknown.remove(&key);
            self.pending.insert(key);
        }
        self.touch(now);
        true
    }

    /// Looks up `key`, which an event mentions but the store doesn't know
    /// (in a whole batch, see the module notes), unless Icinga said
    /// recently that it doesn't exist or the user may not query its kind.
    pub(super) fn mark_unknown(&mut self, key: ObjectKey, now: Instant) -> bool {
        if self.refused.covers(&key) || self.recently_missing(&key, now) {
            return false;
        }
        if !self.full.contains(&key) && !self.pending.contains(&key) {
            self.unknown.insert(key);
        }
        self.touch(now);
        true
    }

    /// Fetches `keys` in full (hydration), skipping those already queued
    /// or in flight, and publishes right after the answer. Returns how many
    /// were added.
    pub(super) fn mark_full(
        &mut self,
        keys: impl IntoIterator<Item = ObjectKey>,
        now: Instant,
    ) -> usize {
        let mut added = 0;
        for key in keys {
            if self.full.contains(&key) || self.flying.contains(&key) || self.refused.covers(&key) {
                continue;
            }
            if self.full.len() >= MAX_PENDING_FULL {
                tracing::warn!(
                    limit = MAX_PENDING_FULL,
                    "too many objects to hydrate; dropping the rest"
                );
                break;
            }
            self.pending.remove(&key);
            self.unknown.remove(&key);
            self.background.remove(&key);
            self.missing.remove(&key);
            self.full.insert(key);
            added += 1;
        }
        if added > 0 {
            self.urgent = true;
            self.touch(now);
        }
        added
    }

    /// Fetches `keys` in full in the background lane (see the module
    /// notes), skipping those queued or in flight. Returns how many were
    /// added.
    pub(super) fn mark_background(
        &mut self,
        keys: impl IntoIterator<Item = ObjectKey>,
        now: Instant,
    ) -> usize {
        let mut added = 0;
        for key in keys {
            if self.full.contains(&key)
                || self.flying.contains(&key)
                || self.refused.covers(&key)
                || self.background.len() >= MAX_PENDING_FULL
            {
                continue;
            }
            if self.background.insert(key) {
                added += 1;
            }
        }
        if added > 0 {
            self.touch(now);
        }
        added
    }

    /// Forgets `key` wherever it waits (the object the user opens is
    /// fetched on its own, at once).
    pub(super) fn forget(&mut self, key: &ObjectKey) {
        self.full.remove(key);
        self.background.remove(key);
        self.pending.remove(key);
    }

    /// The objects waiting for or in a full fetch.
    pub(super) fn updating(&self) -> impl Iterator<Item = &ObjectKey> {
        self.full
            .iter()
            .chain(&self.background)
            .chain(&self.flying_full)
    }

    /// Whether `key` waits for or is in a full fetch.
    pub(super) fn fetches_full(&self, key: &ObjectKey) -> bool {
        self.full.contains(key) || self.background.contains(key) || self.flying_full.contains(key)
    }

    /// Re-queries a created object, even if it was missing before.
    pub(super) fn mark_created(&mut self, key: ObjectKey, now: Instant) {
        if self.refused.covers(&key) {
            return;
        }
        self.missing.remove(&key);
        self.unknown.remove(&key);
        if !self.full.contains(&key) {
            self.pending.insert(key);
        }
        self.touch(now);
    }

    /// Re-queries action targets and publishes right after.
    pub(super) fn mark_urgent(&mut self, keys: impl IntoIterator<Item = ObjectKey>, now: Instant) {
        let before = self.pending.len();
        let refused = self.refused;
        self.pending.extend(
            keys.into_iter()
                .filter(|key| !self.full.contains(key) && !refused.covers(key)),
        );
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

    /// Re-reads `Notification` objects by full name. Returns how many were
    /// added.
    pub(super) fn mark_notifications(
        &mut self,
        names: impl IntoIterator<Item = String>,
        now: Instant,
    ) -> usize {
        let before = self.notifications.len();
        self.notifications.extend(names);
        let added = self.notifications.len() - before;
        if added > 0 {
            self.touch(now);
        }
        added
    }

    /// An unknown object while a load runs: decided after the load.
    pub(super) fn defer(&mut self, key: ObjectKey) {
        if !self.refused.covers(&key) {
            self.deferred.insert(key);
        }
    }

    /// After a load: re-queries the objects deferred during it (events
    /// about them were dropped, so even those the load brought may be
    /// behind); those the store still doesn't know (`known` says) are
    /// looked up like other unknown objects.
    pub(super) fn release_deferred(&mut self, now: Instant, known: impl Fn(&ObjectKey) -> bool) {
        for key in std::mem::take(&mut self.deferred) {
            if known(&key) {
                self.mark(key, now);
            } else {
                self.mark_unknown(key, now);
            }
        }
    }

    /// When the next round may start: `delay` after the oldest mark, never
    /// while a round runs.
    pub(super) fn due(&self, delay: Duration) -> Option<Instant> {
        if self.in_flight
            || (self.pending.is_empty()
                && self.full.is_empty()
                && self.background.is_empty()
                && self.unknown.is_empty()
                && self.notifications.is_empty()
                && !self.lists.any())
        {
            return None;
        }
        self.since.map(|since| since + delay)
    }

    /// Takes the next round (at most [`MAX_ROUND`] names, full fetches
    /// first, plus at most [`MAX_UNKNOWN_ROUND`] unknown objects; the
    /// background lane only when no full fetch waits, one request's worth).
    pub(super) fn take(&mut self, now: Instant) -> Round {
        let mut full = Vec::new();
        while full.len() < MAX_ROUND {
            match self.full.pop_first() {
                Some(key) => full.push(key),
                None => break,
            }
        }
        let mut keys = Vec::new();
        while keys.len() + full.len() < MAX_ROUND {
            match self.pending.pop_first() {
                Some(key) => keys.push(key),
                None => break,
            }
        }
        if full.is_empty() {
            while full.len() < ic_api::NAMES_PER_REQUEST.min(MAX_ROUND - keys.len()) {
                match self.background.pop_first() {
                    Some(key) => full.push(key),
                    None => break,
                }
            }
        }
        let mut unknown = Vec::new();
        while unknown.len() < MAX_UNKNOWN_ROUND {
            match self.unknown.pop_first() {
                Some(key) => unknown.push(key),
                None => break,
            }
        }
        let mut notifications = Vec::new();
        while notifications.len() < MAX_ROUND {
            match self.notifications.pop_first() {
                Some(name) => notifications.push(name),
                None => break,
            }
        }
        self.flying = keys.iter().chain(&full).chain(&unknown).cloned().collect();
        self.flying_full = full.iter().cloned().collect();
        let round = Round {
            keys,
            full,
            unknown,
            lists: std::mem::take(&mut self.lists),
            urgent: std::mem::take(&mut self.urgent),
            notifications,
        };
        self.in_flight = true;
        self.since = (!self.pending.is_empty()
            || !self.full.is_empty()
            || !self.background.is_empty()
            || !self.unknown.is_empty()
            || !self.notifications.is_empty())
        .then_some(now);
        round
    }

    /// A round finished; `missing` weren't found. Each waits before it is
    /// asked for again for events: `missing_ttl` the first time, twice as
    /// long each time it comes back missing (up to [`MAX_MISSING_TTL`]).
    /// A name not asked for during a whole further wait starts over.
    pub(super) fn finished(&mut self, missing: &[ObjectKey], now: Instant) {
        self.in_flight = false;
        self.flying.clear();
        self.flying_full.clear();
        self.missing
            .retain(|_, missing| now < missing.until + missing.ttl);
        for key in missing {
            let ttl = self.missing.get(key).map_or(self.missing_ttl, |previous| {
                (previous.ttl * 2).min(MAX_MISSING_TTL.max(self.missing_ttl))
            });
            self.missing.insert(
                key.clone(),
                Missing {
                    until: now + ttl,
                    ttl,
                },
            );
        }
    }

    /// Forgets pending work (the connection is gone; the reload after the
    /// reconnect brings everything). The missing names stay.
    pub(super) fn reset(&mut self) {
        self.pending.clear();
        self.full.clear();
        self.background.clear();
        self.unknown.clear();
        self.flying.clear();
        self.flying_full.clear();
        // Set again from the next session's permissions.
        self.refused = Refused::default();
        self.lists = Lists::default();
        self.since = None;
        self.urgent = false;
        self.in_flight = false;
        self.deferred.clear();
        self.notifications.clear();
    }
}

/// What a fetch task needs.
pub(super) struct FetchTask {
    pub(super) client: Client,
    pub(super) seq: Arc<AtomicU64>,
    pub(super) tx: UnboundedSender<Internal>,
    pub(super) session: u64,
    /// Hosts, services loaded in full before, and hydration.
    pub(super) full: Vec<ObjectKey>,
    /// Services known only lean.
    pub(super) lean: Vec<ObjectKey>,
    /// Objects the store doesn't know (hosts in full, services lean),
    /// looked up in whole batches.
    pub(super) unknown: Vec<ObjectKey>,
    pub(super) lists: Lists,
    pub(super) urgent: bool,
    /// `Notification` objects to re-read.
    pub(super) notifications: Vec<String>,
}

impl FetchTask {
    /// Runs the round and reports its answers.
    pub(super) async fn run(self) {
        let mut objects = Vec::new();
        let (unknown_hosts, unknown_services): (Vec<ObjectKey>, Vec<ObjectKey>) = self
            .unknown
            .into_iter()
            .partition(|key| matches!(key, ObjectKey::Host { .. }));
        let queries = [
            (Detail::Full, self.full, false),
            (Detail::Lean, self.lean, false),
            (Detail::Full, unknown_hosts, true),
            (Detail::Lean, unknown_services, true),
        ];
        for (detail, keys, whole) in queries {
            // One query per kind: a refusal (403) then says which kind.
            let (hosts, services): (Vec<ObjectKey>, Vec<ObjectKey>) = keys
                .into_iter()
                .partition(|key| matches!(key, ObjectKey::Host { .. }));
            for keys in [hosts, services] {
                if keys.is_empty() {
                    continue;
                }
                let started = self.seq.load(Ordering::SeqCst);
                let result = if whole {
                    self.client.objects_unsplit(&keys, detail).await
                } else {
                    self.client.objects(&keys, detail).await
                };
                objects.push(Answer {
                    detail,
                    keys,
                    started,
                    whole,
                    result,
                });
            }
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
                Some(self.client.cluster().await)
            } else {
                None
            },
            notifications: if self.notifications.is_empty() {
                None
            } else {
                let started = self.seq.load(Ordering::SeqCst);
                Some((
                    started,
                    self.client.notifications_named(&self.notifications).await,
                ))
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
        assert!(queue.full.is_empty());
        assert_eq!(queue.due(Duration::ZERO), None);
        queue.release_deferred(now, |_| true);
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

    #[test]
    fn the_background_lane_waits_for_full_fetches() {
        let now = Instant::now();
        let mut queue = FetchQueue::new(Duration::from_mins(10));
        let problems: Vec<ObjectKey> = (0..450)
            .map(|index| ObjectKey::service("h", &format!("p{index:03}")))
            .collect();
        assert_eq!(queue.mark_background(problems.clone(), now), 450);
        let row = ObjectKey::service("h", "row");
        queue.mark_full([row.clone()], now);
        assert_eq!(queue.updating().count(), 451);
        let round = queue.take(now);
        assert_eq!(round.full, [row], "the row on screen alone");
        queue.finished(&[], now);
        let round = queue.take(now);
        assert_eq!(
            round.full.len(),
            ic_api::NAMES_PER_REQUEST,
            "then one request's worth"
        );
        assert!(round.full.iter().all(|key| problems.contains(key)));
        assert_eq!(queue.updating().count(), 450, "in flight counts");
        // A row queued meanwhile goes before the rest.
        queue.mark_full([ObjectKey::service("h", "row2")], now);
        queue.finished(&[], now);
        assert_eq!(queue.take(now).full, [ObjectKey::service("h", "row2")]);
        queue.finished(&[], now);
        // Focus takes an object out of every lane.
        let last = problems[449].clone();
        queue.forget(&last);
        assert_eq!(queue.take(now).full.len(), 200);
        queue.finished(&[], now);
        let round = queue.take(now);
        assert_eq!(round.full.len(), 49);
        assert!(!round.full.contains(&last));
        queue.finished(&[], now);
        assert_eq!(queue.due(Duration::ZERO), None);
        assert_eq!(queue.updating().count(), 0);
    }

    #[test]
    fn full_fetches_are_deduplicated_and_go_first() {
        let now = Instant::now();
        let mut queue = FetchQueue::new(Duration::from_mins(10));
        let a = ObjectKey::service("h", "a");
        let b = ObjectKey::service("h", "b");
        assert!(queue.mark(a.clone(), now));
        assert_eq!(queue.mark_full([a.clone(), b.clone(), a.clone()], now), 2);
        assert!(queue.mark(b.clone(), now), "already queued in full");
        let round = queue.take(now);
        assert_eq!(round.full, [a.clone(), b.clone()]);
        assert!(round.keys.is_empty(), "a moved to the full fetch");
        assert!(round.urgent, "hydration publishes right away");
        assert_eq!(queue.mark_full([a.clone()], now), 0, "in flight");
        queue.finished(&[], now);
        assert_eq!(queue.mark_full([a.clone()], now), 1);

        // Missing names aren't marked for events, but hydration asks anyway.
        let mut queue = FetchQueue::new(Duration::from_mins(10));
        let round = queue.take(now);
        assert!(round.full.is_empty() && round.keys.is_empty());
        queue.finished(std::slice::from_ref(&b), now);
        assert!(!queue.mark(b.clone(), now));
        assert_eq!(queue.mark_full([b.clone()], now), 1);

        // Bounded.
        let mut queue = FetchQueue::new(Duration::from_mins(10));
        let many =
            (0..MAX_PENDING_FULL + 10).map(|index| ObjectKey::service("h", &index.to_string()));
        assert_eq!(queue.mark_full(many, now), MAX_PENDING_FULL);
        assert_eq!(queue.take(now).full.len(), MAX_ROUND);
    }
}
