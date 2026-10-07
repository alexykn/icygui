//! The freshness watchdog (docs/performance.md): every host and service has
//! a deadline, Icinga's own `next_update`, and an object past it is
//! re-queried by name. If Icinga still reports it overdue, its check is
//! genuinely *late* (a satellite or agent stopped checking) and the
//! snapshot says so.
//!
//! **Deadline.** `next_update` isn't loaded (Icinga 2.11 lacks it); it is
//! computed with Icinga's formula (`Checkable::GetNextUpdate`):
//! `next_check + interval + 2 × latency` with active checks,
//! `end of the last result + 2 × interval + 2 × latency` without, where
//! `interval` is the retry interval for a soft problem with active checks
//! and the check interval otherwise, and `latency` is the last result's
//! `execution_end − schedule_start`. Every `CheckResult` event moves
//! `next_check` (the store estimates it like Icinga does), so manual checks
//! by anyone reset the deadline.
//!
//! - *Lean objects* have no result yet: their latency counts as 0 until a
//!   `CheckResult` event or a full fetch brings one. That makes their
//!   deadline earlier by twice the latency (usually well under a second,
//!   next to a whole check interval of slack), so at worst an object is
//!   re-queried a little early.
//! - *Never-checked passive objects* (no result, active checks off) have no
//!   deadline: nothing is expected of them (Icinga's formula would count
//!   from the program start; IDO's `next_update` leaves them out too).
//! - *Globally disabled checks* (`/v1/status`: host or service checks off)
//!   take the deadline from active objects of that type: Icinga won't
//!   check them, and re-querying can't change that. A status that turns
//!   them off or on recomputes every deadline at once.
//!
//! **Clock.** Deadlines are on Icinga's clock, not the laptop's, which may
//! be off by minutes. [`IcingaClock`] follows Icinga's clock from the
//! timestamps it sends (every event carries one; a load brings the latest
//! `last_check`) plus the monotonic time since. It only lags behind Icinga
//! (events arrive after they happen), which errs towards fewer re-queries.
//! When Icinga's clock is set back (or a reconnect reaches a node whose
//! clock is behind), an event more than [`RESYNC`] behind the estimate
//! resets it, and so does a run of [`DRIFT_WINDOW`] in which every event
//! was more than [`DRIFT`] behind (a smaller step back).
//!
//! **Load on Icinga.** Overdue objects are re-queried by name through the
//! fetch queue, at most [`ic_api::NAMES_PER_REQUEST`] per sweep and one
//! sweep per `Tuning::watchdog_interval` (5 s), so even a stalled checker
//! with every object overdue costs one request every few seconds. An object
//! is re-queried at most once per interval (at least a minute). One that
//! Icinga still reports overdue is late and waits twice as long before the
//! next re-query (up to an hour); a check result clears the flag at once
//! without any query.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use ic_model::{CheckInfo, CheckableState, InstanceStatus, ObjectKey, StateType, Timestamp};
use tokio::time::Instant;

use crate::store::Store;

/// The shortest wait between two re-queries of one object, in seconds.
const MIN_SPACING: f64 = 60.0;

/// The longest wait between two re-queries of a late object, in seconds.
const MAX_SPACING: f64 = 3_600.0;

/// An event older than Icinga's clock as estimated by more than this (in
/// seconds) means the estimate is wrong (Icinga's clock was set back): it
/// starts over from the event.
const RESYNC: f64 = 300.0;

/// Events lag the estimate by the time they take to arrive (well under a
/// second, a few seconds while the applier is busy). When every event of a
/// [`DRIFT_WINDOW`] (at least [`DRIFT_EVENTS`] of them) lags by more than
/// this (in seconds), Icinga's clock was set back by less than [`RESYNC`]:
/// the clock starts over from the freshest of them.
const DRIFT: f64 = 30.0;
const DRIFT_WINDOW: Duration = Duration::from_mins(1);
const DRIFT_EVENTS: u32 = 3;

/// Icinga's clock as seen through the timestamps it sends.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct IcingaClock {
    /// An Icinga time (Unix seconds) and the instant it was seen.
    reference: Option<(f64, Instant)>,
    /// The events behind the estimate since the window began.
    behind: Option<Behind>,
}

/// Events behind the estimate within a window: how many, and the freshest
/// (smallest lag, its time and when it arrived).
#[derive(Clone, Copy, Debug)]
struct Behind {
    since: Instant,
    count: u32,
    lag: f64,
    freshest: (f64, Instant),
}

impl IcingaClock {
    /// An event sent at `at` arrived at `now`. Moves the clock forward, or
    /// back if `at` is much older than the estimate or a window's events
    /// all were somewhat older.
    pub(super) fn observe(&mut self, at: Timestamp, now: Instant) {
        let seconds = at.as_unix_seconds();
        if seconds <= 0.0 {
            return;
        }
        let Some(mut estimate) = self.now(now) else {
            self.reset_to(seconds, now);
            return;
        };
        if let Some(window) = self.behind
            && now.saturating_duration_since(window.since) >= DRIFT_WINDOW
        {
            self.behind = None;
            if window.count >= DRIFT_EVENTS && window.lag > DRIFT {
                // Every event of a whole window lagged far: Icinga's clock
                // was set back. Start over from the freshest of them.
                tracing::debug!(lag = window.lag, "Icinga's clock went back; following it");
                self.reference = Some(window.freshest);
                estimate = self.now(now).unwrap_or(seconds);
            }
        }
        let lag = estimate - seconds;
        if !(0.0..RESYNC).contains(&lag) {
            self.reset_to(seconds, now);
            return;
        }
        let window = self.behind.get_or_insert(Behind {
            since: now,
            count: 0,
            lag: f64::INFINITY,
            freshest: (seconds, now),
        });
        window.count += 1;
        if lag < window.lag {
            window.lag = lag;
            window.freshest = (seconds, now);
        }
    }

    /// Starts over from Icinga's time `seconds`, seen at `now`.
    fn reset_to(&mut self, seconds: f64, now: Instant) {
        self.reference = Some((seconds, now));
        self.behind = None;
    }

    /// Icinga's clock was at least at `at` by `now` (a load's latest
    /// `last_check`): moves the clock forward only.
    pub(super) fn advance(&mut self, at: Timestamp, now: Instant) {
        let seconds = at.as_unix_seconds();
        if seconds > 0.0 && self.now(now).is_none_or(|estimate| seconds > estimate) {
            self.reset_to(seconds, now);
        }
    }

    /// Icinga's time at `now` (Unix seconds), once known.
    pub(super) fn now(&self, now: Instant) -> Option<f64> {
        self.reference
            .map(|(seconds, at)| seconds + now.saturating_duration_since(at).as_secs_f64())
    }

    /// The instant Icinga's clock reads `seconds` (`now` if that's past).
    fn instant_of(&self, seconds: f64, now: Instant) -> Option<Instant> {
        let ahead = seconds - self.now(now)?;
        Some(match Duration::try_from_secs_f64(ahead.min(86_400.0)) {
            Ok(ahead) => now + ahead,
            Err(_) => now,
        })
    }
}

/// Which active checks Icinga runs at all (`/v1/status`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Globals {
    host_checks: bool,
    service_checks: bool,
}

impl Default for Globals {
    fn default() -> Self {
        Self {
            host_checks: true,
            service_checks: true,
        }
    }
}

impl Globals {
    pub(super) fn of(status: Option<&InstanceStatus>) -> Self {
        status.map_or_else(Self::default, |status| Self {
            host_checks: status.host_checks_enabled,
            service_checks: status.service_checks_enabled,
        })
    }

    fn runs(self, state: CheckableState) -> bool {
        match state {
            CheckableState::Host(_) => self.host_checks,
            CheckableState::Service(_) => self.service_checks,
        }
    }
}

/// An object's deadline (`next_update`, Unix seconds on Icinga's clock)
/// and the interval it was computed with; `None` when nothing is expected
/// of it (see the module notes).
pub(super) fn next_update(
    state: CheckableState,
    check: &CheckInfo,
    globals: Globals,
) -> Option<(f64, f64)> {
    let active = check.features.active_checks;
    if active && !globals.runs(state) {
        return None;
    }
    let result_end = check
        .result
        .as_ref()
        .and_then(|result| result.execution_end.non_zero());
    let last_end = result_end.or(check.last_check.and_then(Timestamp::non_zero));
    if !active && last_end.is_none() {
        return None;
    }
    let checked = last_end.is_some();
    let soft_problem = checked && state.is_problem() && check.state_type == StateType::Soft;
    let interval = if active && soft_problem {
        check.retry_interval
    } else {
        check.check_interval
    };
    if !interval.is_finite() || interval <= 0.0 {
        return None;
    }
    let latency = check.result.as_ref().map_or(0.0, |result| {
        let start = result.schedule_start.as_unix_seconds();
        let end = result.execution_end.as_unix_seconds();
        if start > 0.0 && end > start {
            end - start
        } else {
            0.0
        }
    });
    let base = if active {
        check.next_check?.as_unix_seconds()
    } else {
        last_end?.as_unix_seconds() + interval
    };
    Some((base + interval + 2.0 * latency, interval))
}

/// What the watchdog knows about one object.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Watch {
    deadline: f64,
    interval: f64,
    /// No re-query before this (Icinga time).
    not_before: f64,
    /// The wait after the next re-query, in seconds.
    spacing: f64,
}

impl Watch {
    /// When to look at the object again.
    fn next_look(&self) -> f64 {
        self.deadline.max(self.not_before)
    }
}

/// The freshness watchdog: deadlines, re-queries and the late flags.
#[derive(Debug, Default)]
pub(super) struct Watchdog {
    clock: IcingaClock,
    watched: HashMap<ObjectKey, Watch>,
    /// Watched objects by when to look at them next.
    queue: BTreeSet<(Seconds, ObjectKey)>,
    /// Re-queried, the answer isn't in yet.
    awaiting: HashSet<ObjectKey>,
    late: Arc<BTreeMap<ObjectKey, Timestamp>>,
    /// The late flags changed since the last [`Watchdog::take_changed`].
    changed: bool,
    /// The global check switches the deadlines were computed with.
    globals: Globals,
}

/// Unix seconds with a total order, for the queue.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Seconds(f64);

impl Eq for Seconds {}

impl PartialOrd for Seconds {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Seconds {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&other.0)
    }
}

impl Watchdog {
    /// Icinga's clock.
    pub(super) fn clock(&mut self) -> &mut IcingaClock {
        &mut self.clock
    }

    /// Icinga's time now, once known.
    pub(super) fn icinga_now(&self) -> Option<f64> {
        self.clock.now(Instant::now())
    }

    /// The late objects, for the snapshot.
    pub(super) fn late(&self) -> Arc<BTreeMap<ObjectKey, Timestamp>> {
        Arc::clone(&self.late)
    }

    /// Whether the late flags changed since the last call.
    pub(super) fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }

    /// Whether the late flags changed since the last [`Self::take_changed`].
    pub(super) fn has_changes(&self) -> bool {
        self.changed
    }

    /// Recomputes the deadlines of `objects` from the store (all of them
    /// with `all`, or when Icinga's global check switches changed).
    pub(super) fn update<'a>(
        &mut self,
        store: &Store,
        objects: impl IntoIterator<Item = &'a ObjectKey>,
        all: bool,
    ) {
        let globals = Globals::of(store.status());
        let all = all || globals != self.globals;
        self.globals = globals;
        if all {
            let gone: Vec<ObjectKey> = self
                .watched
                .keys()
                .filter(|key| !store.contains(key))
                .cloned()
                .collect();
            for key in gone {
                self.set(key, None);
            }
            let keys: Vec<ObjectKey> = store
                .hosts()
                .values()
                .map(|host| host.key())
                .chain(
                    store
                        .services()
                        .values()
                        .map(|service| service.object_key()),
                )
                .collect();
            for key in keys {
                let deadline = deadline_of(store, &key, globals);
                self.set(key, deadline);
            }
        } else {
            for key in objects {
                let deadline = deadline_of(store, key, globals);
                self.set(key.clone(), deadline);
            }
        }
    }

    /// Sets `key`'s deadline (or stops watching it).
    fn set(&mut self, key: ObjectKey, deadline: Option<(f64, f64)>) {
        let old = self.watched.get(&key).copied();
        let Some((deadline, interval)) = deadline else {
            if let Some(old) = old {
                self.queue.remove(&(Seconds(old.next_look()), key.clone()));
                self.watched.remove(&key);
            }
            self.awaiting.remove(&key);
            self.unlate(&key);
            return;
        };
        if old.is_some_and(|old| {
            old.deadline.total_cmp(&deadline).is_eq() && old.interval.total_cmp(&interval).is_eq()
        }) {
            return;
        }
        let now = self.icinga_now();
        let fresh = now.is_none_or(|now| deadline > now);
        let watch = match old {
            Some(old) if !fresh => Watch {
                deadline,
                interval,
                ..old
            },
            _ => Watch {
                deadline,
                interval,
                not_before: 0.0,
                spacing: interval.max(MIN_SPACING),
            },
        };
        if let Some(old) = old {
            self.queue.remove(&(Seconds(old.next_look()), key.clone()));
        }
        self.queue.insert((Seconds(watch.next_look()), key.clone()));
        self.watched.insert(key.clone(), watch);
        if fresh {
            self.unlate(&key);
        } else if self.late.contains_key(&key) {
            // Still late, with the deadline Icinga reports now.
            Arc::make_mut(&mut self.late).insert(key, Timestamp::from_unix_seconds(deadline));
            self.changed = true;
        }
    }

    fn unlate(&mut self, key: &ObjectKey) {
        if self.late.contains_key(key) {
            Arc::make_mut(&mut self.late).remove(key);
            self.changed = true;
        }
    }

    /// When the next object is due for a look, on Icinga's clock.
    fn next_look(&self) -> Option<f64> {
        self.queue
            .iter()
            .find(|(_, key)| !self.awaiting.contains(key))
            .map(|(Seconds(at), _)| *at)
    }

    /// The instant of the next look (`now` if it's past), once Icinga's
    /// clock is known.
    pub(super) fn due(&self, now: Instant) -> Option<Instant> {
        self.clock.instant_of(self.next_look()?, now)
    }

    /// Takes up to `limit` overdue objects to re-query now; each waits its
    /// spacing before the next re-query.
    pub(super) fn sweep(&mut self, limit: usize) -> Vec<ObjectKey> {
        let Some(now) = self.icinga_now() else {
            return Vec::new();
        };
        let due: Vec<(Seconds, ObjectKey)> = self
            .queue
            .iter()
            .take_while(|(Seconds(at), _)| *at <= now)
            .filter(|(_, key)| !self.awaiting.contains(key))
            .take(limit)
            .cloned()
            .collect();
        let mut keys = Vec::with_capacity(due.len());
        for (at, key) in due {
            let Some(watch) = self.watched.get_mut(&key) else {
                continue;
            };
            self.queue.remove(&(at, key.clone()));
            watch.not_before = now + watch.spacing;
            self.queue.insert((Seconds(watch.next_look()), key.clone()));
            self.awaiting.insert(key.clone());
            keys.push(key);
        }
        keys
    }

    /// The re-query of `keys` came back (and the store holds the answer):
    /// objects still overdue are late, and wait twice as long before the
    /// next re-query.
    pub(super) fn answered(&mut self, store: &Store, keys: &[ObjectKey]) {
        let answered: Vec<ObjectKey> = keys
            .iter()
            .filter(|key| self.awaiting.remove(*key))
            .cloned()
            .collect();
        self.update(store, &answered, false);
        self.confirm(&answered);
    }

    /// The re-query of `keys` failed (or never went out): they may be
    /// re-queried after their spacing.
    pub(super) fn failed(&mut self, keys: &[ObjectKey]) {
        for key in keys {
            self.awaiting.remove(key);
        }
    }

    /// Forgets the re-queries in flight (the connection is gone).
    pub(super) fn reset_awaiting(&mut self) {
        self.awaiting.clear();
    }

    /// Check results came back after quiet mode, which had none: deadlines
    /// that passed meanwhile say nothing (no result moved them). Every
    /// object is looked at again one interval from now at the earliest
    /// (at least a minute), by when its next check result has come if
    /// Icinga checks it, so waking up costs no re-query storm.
    pub(super) fn rebase(&mut self) {
        let Some(now) = self.icinga_now() else {
            return;
        };
        let keys: Vec<ObjectKey> = self.watched.keys().cloned().collect();
        for key in keys {
            let Some(watch) = self.watched.get_mut(&key) else {
                continue;
            };
            let old = watch.next_look();
            watch.not_before = watch.not_before.max(now + watch.interval.max(MIN_SPACING));
            let new = watch.next_look();
            if old.total_cmp(&new).is_ne() {
                self.queue.remove(&(Seconds(old), key.clone()));
                self.queue.insert((Seconds(new), key));
            }
        }
    }

    /// A load just brought Icinga's own view of every object: whatever is
    /// overdue in it is late, without a re-query.
    pub(super) fn loaded(&mut self, store: &Store) {
        if let Some(latest) = store.latest_check() {
            self.clock.advance(latest, Instant::now());
        }
        self.update(store, [], true);
        let keys: Vec<ObjectKey> = self.watched.keys().cloned().collect();
        self.confirm(&keys);
    }

    /// Marks those of `keys` that are overdue late (with a doubled
    /// spacing), the others not late.
    fn confirm(&mut self, keys: &[ObjectKey]) {
        let Some(now) = self.icinga_now() else {
            return;
        };
        for key in keys {
            let Some(watch) = self.watched.get(key).copied() else {
                continue;
            };
            if watch.deadline > now {
                self.unlate(key);
                continue;
            }
            let deadline = Timestamp::from_unix_seconds(watch.deadline);
            if self.late.get(key) != Some(&deadline) {
                Arc::make_mut(&mut self.late).insert(key.clone(), deadline);
                self.changed = true;
            }
            let spacing = (watch.spacing * 2.0).min(MAX_SPACING.max(watch.spacing));
            let next = Watch {
                not_before: now + watch.spacing,
                spacing,
                ..watch
            };
            self.queue
                .remove(&(Seconds(watch.next_look()), key.clone()));
            self.queue.insert((Seconds(next.next_look()), key.clone()));
            self.watched.insert(key.clone(), next);
        }
    }

    /// Forgets everything (another server).
    pub(super) fn clear(&mut self) {
        let had_late = !self.late.is_empty();
        *self = Self::default();
        self.changed = had_late;
    }
}

fn deadline_of(store: &Store, key: &ObjectKey, globals: Globals) -> Option<(f64, f64)> {
    let (state, check) = store.checkable(key)?;
    next_update(state, check, globals)
}

#[cfg(test)]
mod tests {
    use ic_api::Detail;
    use ic_model::{CheckResult, Host, HostState, Service, ServiceState};

    use super::*;

    fn t(seconds: f64) -> Timestamp {
        Timestamp::from_unix_seconds(seconds)
    }

    fn active(next_check: f64, interval: f64) -> CheckInfo {
        CheckInfo {
            last_check: Some(t(next_check - interval)),
            next_check: Some(t(next_check)),
            check_interval: interval,
            retry_interval: interval / 5.0,
            ..CheckInfo::default()
        }
    }

    const OK: CheckableState = CheckableState::Service(ServiceState::Ok);
    const CRITICAL: CheckableState = CheckableState::Service(ServiceState::Critical);

    #[test]
    fn deadlines_follow_icingas_formula() {
        let globals = Globals::default();
        // Active, lean (no result): next_check + interval.
        let check = active(1_000.0, 300.0);
        assert_eq!(next_update(OK, &check, globals), Some((1_300.0, 300.0)));

        // With a result: plus twice its latency (end − schedule start).
        let mut check = active(1_000.0, 300.0);
        check.result = Some(CheckResult {
            schedule_start: t(690.0),
            execution_start: t(698.0),
            execution_end: t(700.0),
            ..CheckResult::default()
        });
        assert_eq!(next_update(OK, &check, globals), Some((1_320.0, 300.0)));

        // A soft problem uses the retry interval.
        check.state_type = StateType::Soft;
        assert_eq!(
            next_update(CRITICAL, &check, globals),
            Some((1_080.0, 60.0))
        );
        assert_eq!(
            next_update(OK, &check, globals),
            Some((1_320.0, 300.0)),
            "soft OK uses the check interval"
        );

        // Passive: the last result's end + 2 × interval (+ latency).
        check.features.active_checks = false;
        assert_eq!(
            next_update(CRITICAL, &check, globals),
            Some((700.0 + 600.0 + 20.0, 300.0))
        );
        // Lean passive: from last_check.
        let mut lean = active(1_000.0, 300.0);
        lean.features.active_checks = false;
        assert_eq!(
            next_update(OK, &lean, globals),
            Some((700.0 + 600.0, 300.0))
        );
        // Never checked and passive: nothing expected.
        lean.last_check = None;
        assert_eq!(next_update(OK, &lean, globals), None);
        // Never checked but active: next_check + interval.
        let mut pending = active(1_000.0, 300.0);
        pending.last_check = None;
        assert_eq!(
            next_update(
                CheckableState::Service(ServiceState::Pending),
                &pending,
                globals
            ),
            Some((1_300.0, 300.0))
        );

        // Globally disabled checks: nothing expected of active objects.
        let off = Globals {
            host_checks: true,
            service_checks: false,
        };
        assert_eq!(next_update(OK, &active(1_000.0, 300.0), off), None);
        assert!(
            next_update(
                CheckableState::Host(HostState::Up),
                &active(1_000.0, 300.0),
                off
            )
            .is_some()
        );
        let mut broken = active(1_000.0, 0.0);
        assert_eq!(next_update(OK, &broken, globals), None);
        broken.check_interval = f64::NAN;
        assert_eq!(next_update(OK, &broken, globals), None);
    }

    #[test]
    fn the_clock_follows_icinga() {
        let start = Instant::now();
        let mut clock = IcingaClock::default();
        assert_eq!(clock.now(start), None);
        clock.observe(t(1_000.0), start);
        let later = start + Duration::from_secs(10);
        assert_eq!(clock.now(later), Some(1_010.0));
        // Older events (a backlog) don't move it back...
        clock.observe(t(990.0), later);
        assert_eq!(clock.now(later), Some(1_010.0));
        // ...newer ones move it forward...
        clock.observe(t(1_100.0), later);
        assert_eq!(clock.now(later), Some(1_100.0));
        // ...and a much older one means Icinga's clock was set back.
        clock.observe(t(500.0), later);
        assert_eq!(clock.now(later), Some(500.0));
        // A load's latest check only moves it forward.
        clock.advance(t(400.0), later);
        assert_eq!(clock.now(later), Some(500.0));
        clock.advance(t(600.0), later);
        assert_eq!(clock.now(later), Some(600.0));
        clock.observe(t(0.0), later);
        assert_eq!(clock.now(later), Some(600.0));
        assert_eq!(
            clock.instant_of(650.0, later),
            Some(later + Duration::from_secs(50))
        );
        assert_eq!(clock.instant_of(500.0, later), Some(later));
    }

    #[test]
    fn the_clock_follows_icinga_back_by_less_than_the_resync() {
        let start = Instant::now();
        let at = |seconds: u64| start + Duration::from_secs(seconds);
        let mut clock = IcingaClock::default();
        clock.observe(t(10_000.0), start);
        // Icinga's clock steps back by two minutes; events keep coming every
        // ten seconds, all 120 s behind the estimate.
        for second in (10_u32..=60).step_by(10) {
            clock.observe(
                t(10_000.0 + f64::from(second) - 120.0),
                at(u64::from(second)),
            );
        }
        assert_eq!(clock.now(at(60)), Some(10_060.0), "a minute isn't over yet");
        clock.observe(t(10_070.0 - 120.0), at(70));
        let now = clock.now(at(70)).unwrap();
        assert!(
            (now - (10_070.0 - 120.0)).abs() < 11.0,
            "re-anchored to the freshest event: {now}"
        );
        // From then on events are current again.
        clock.observe(t(10_080.0 - 120.0), at(80));
        assert_eq!(clock.now(at(80)), Some(10_080.0 - 120.0));

        // Events a few seconds late (a busy applier) never move it back.
        let mut clock = IcingaClock::default();
        clock.observe(t(20_000.0), start);
        for second in 1_u32..=200 {
            clock.observe(t(20_000.0 + f64::from(second) - 5.0), at(u64::from(second)));
        }
        assert_eq!(clock.now(at(200)), Some(20_200.0));
    }

    #[test]
    fn checks_turned_off_globally_drop_the_deadlines_at_once() {
        let mut store = store_with(2_000.0, vec![svc("a", 500.0, 300.0)]);
        let status = |service_checks: bool| InstanceStatus {
            node_name: "master".to_owned(),
            host_checks_enabled: true,
            service_checks_enabled: service_checks,
            ..InstanceStatus::default()
        };
        store.set_status(status(true));
        let mut watchdog = Watchdog::default();
        watchdog.update(&store, [], true);
        let a = ObjectKey::service("h", "a");
        assert!(watchdog.watched.contains_key(&a));
        // A status poll turns service checks off: nothing changed about the
        // objects, yet every service's deadline goes.
        store.set_status(status(false));
        watchdog.update(&store, [], false);
        assert!(!watchdog.watched.contains_key(&a));
        assert!(watchdog.watched.contains_key(&ObjectKey::host("h")));
        watchdog.clock().observe(t(1_000.0), Instant::now());
        assert!(watchdog.sweep(10).is_empty(), "nothing is re-queried");
        // And back on.
        store.set_status(status(true));
        watchdog.update(&store, [], false);
        assert!(watchdog.watched.contains_key(&a));
    }

    fn store_with(host_next_check: f64, services: Vec<Service>) -> Store {
        let mut store = Store::default();
        let mut host = Host::new("h");
        host.state = HostState::Up;
        host.check = active(host_next_check, 60.0);
        store.replace_hosts(vec![host], 0);
        store.replace_services(services, Detail::Lean, 0);
        store
    }

    fn svc(name: &str, next_check: f64, interval: f64) -> Service {
        let mut service = Service::new("h", name);
        service.state = ServiceState::Ok;
        service.check = active(next_check, interval);
        service
    }

    #[test]
    fn overdue_objects_are_requeried_then_late_with_growing_waits() {
        let store = store_with(
            2_000.0,
            vec![
                svc("fresh", 1_500.0, 300.0),
                svc("overdue", 500.0, 300.0),
                svc("also-overdue", 600.0, 120.0),
            ],
        );
        let mut watchdog = Watchdog::default();
        watchdog.update(&store, [], true);
        assert_eq!(watchdog.sweep(10), Vec::<ObjectKey>::new(), "no clock yet");
        let now = Instant::now();
        watchdog.clock().observe(t(1_000.0), now);
        // overdue: 500 + 300 = 800 ≤ 1 000; also-overdue: 720.
        let swept = watchdog.sweep(1);
        assert_eq!(
            swept,
            [ObjectKey::service("h", "also-overdue")],
            "oldest first, limited"
        );
        let swept = watchdog.sweep(10);
        assert_eq!(swept, [ObjectKey::service("h", "overdue")]);
        assert!(watchdog.sweep(10).is_empty(), "each once");

        // Icinga answers with the same, still overdue data: late.
        watchdog.answered(&store, &[ObjectKey::service("h", "overdue")]);
        assert!(watchdog.take_changed());
        assert_eq!(
            *watchdog.late(),
            BTreeMap::from([(ObjectKey::service("h", "overdue"), t(800.0))])
        );
        // A failed re-query isn't late, and may be retried after its spacing.
        watchdog.failed(&[ObjectKey::service("h", "also-overdue")]);
        assert!(
            !watchdog
                .late()
                .contains_key(&ObjectKey::service("h", "also-overdue"))
        );

        // Spacing: also-overdue (interval 120) again after 120 s; overdue
        // (late) after 300 s, then 600 s.
        let at = |seconds: f64| now + Duration::from_secs_f64(seconds - 1_000.0);
        let due = watchdog.due(now).unwrap();
        let expected = at(1_120.0);
        assert!(
            due.max(expected) - due.min(expected) < Duration::from_millis(5),
            "{due:?} vs {expected:?}"
        );
        // Icinga's clock jumps ahead (the watchdog reads the real monotonic
        // clock, so the events arrive "now"; half a second of slack for the
        // time the test took).
        watchdog.clock().observe(t(1_120.5), Instant::now());
        assert_eq!(
            watchdog.sweep(10),
            [ObjectKey::service("h", "also-overdue")]
        );
        watchdog.clock().observe(t(1_300.5), Instant::now());
        assert_eq!(watchdog.sweep(10), [ObjectKey::service("h", "overdue")]);
        watchdog.answered(&store, &[ObjectKey::service("h", "overdue")]);
        watchdog.clock().observe(t(1_899.0), Instant::now());
        assert!(
            !watchdog
                .sweep(10)
                .contains(&ObjectKey::service("h", "overdue"))
        );
        watchdog.clock().observe(t(1_901.0), Instant::now());
        assert!(
            watchdog
                .sweep(10)
                .contains(&ObjectKey::service("h", "overdue"))
        );
    }

    #[test]
    fn a_check_result_clears_the_flag_and_loads_confirm_directly() {
        // The latest last_check (990) is behind the events' clock (1 000).
        let mut store = store_with(
            1_050.0,
            vec![svc("a", 500.0, 300.0), svc("b", 1_100.0, 300.0)],
        );
        let mut watchdog = Watchdog::default();
        watchdog.clock().observe(t(1_000.0), Instant::now());
        watchdog.loaded(&store);
        let a = ObjectKey::service("h", "a");
        assert_eq!(
            watchdog.late().keys().cloned().collect::<Vec<_>>(),
            std::slice::from_ref(&a),
            "a load is Icinga's answer: no re-query needed"
        );
        assert!(watchdog.sweep(10).is_empty(), "a waits its spacing");

        // A fresh check result: next_check moved into the future.
        store.apply_fetched(
            Vec::new(),
            vec![svc("a", 1_200.0, 300.0)],
            Detail::Lean,
            &[],
            1,
        );
        watchdog.update(&store, [&a], false);
        assert!(watchdog.late().is_empty());
        assert!(watchdog.take_changed());

        // Deleted: forgotten.
        store.apply_fetched(
            Vec::new(),
            Vec::new(),
            Detail::Lean,
            std::slice::from_ref(&a),
            2,
        );
        watchdog.update(&store, [&a], false);
        assert!(!watchdog.watched.contains_key(&a));
        watchdog.clear();
        assert!(watchdog.watched.is_empty());
    }
}
