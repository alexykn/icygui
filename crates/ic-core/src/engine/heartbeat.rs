//! Watching the heartbeats ([`crate::heartbeat`], PLAN.md §4.2 B, B2, B3).
//!
//! - **Found** in the objects the store holds, after every load and every
//!   re-query round that brought services, and when the settings change:
//!   no request of its own. The beats' objects are left out of lists,
//!   counts, rules and notifications ([`Store::set_excluded`]); a quiet
//!   stream subscribes to their check results by name
//!   ([`connect::StreamWish`]).
//! - **On the stream:** each beat's result moves its deadline (its time
//!   budget, [`heartbeat::Budget`]); a result that isn't OK makes it dead
//!   at once.
//! - **Late:** past its deadline it is *late*; [`heartbeat::QUERY_AFTER`]
//!   later one REST query of every late beat (by name, through the request
//!   budget) decides: Icinga stopped running it (dead), its result isn't
//!   OK (dead), or it ran and the stream didn't bring it: a second look
//!   [`heartbeat::RECHECK_AFTER`] later, then a reconnect; only if the
//!   beat is lost again after that is it dead (*not delivered*: the
//!   environment is blind). A failing query is a broken connection
//!   (reconnect).
//! - **Polled:** while the stream can't carry the beats' results (quiet
//!   without Icinga's stream filter, or no check result events for this
//!   user), one request reads them once per interval (the shortest beat's)
//!   instead, and a newer `last_check` counts as a beat.
//! - **Grace:** after a reconnect, the computer waking up or an Icinga
//!   restart, every beat that isn't dead gets
//!   [`heartbeat::GRACE_INTERVALS`] intervals before it can be late.
//! - **Remembered** in the event log: a beat discovery no longer finds is
//!   *disappeared* until the user confirms its removal.
//!
//! [`Store::set_excluded`]: crate::store::Store::set_excluded
//! [`connect::StreamWish`]: crate::connect::StreamWish

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use ic_api::{ApiError, Detail};
use ic_model::{CheckResult, EventKind, ObjectKey, ServiceKey, ServiceState, Timestamp};
use tokio::time::Instant;

use super::{Engine, Internal, Phase};
use crate::connect::Failure;
use crate::event_log::RememberedBeat;
use crate::heartbeat::{
    self, BeatState, Budget, Death, Found, Heartbeat, HeartbeatSetup, Heartbeats, Proves, Verdict,
};

/// How long a beat that isn't delivered waits before the engine tries
/// another reconnect for it.
const UNDELIVERED_RETRY: Duration = Duration::from_mins(5);

/// One beat as the engine watches it.
#[derive(Clone, Debug)]
struct Watch {
    found: Found,
    state: BeatState,
    /// When `state` began (local clock).
    since: Timestamp,
    budget: Budget,
    allowance: Duration,
    /// When the last OK beat arrived (local clock).
    last_beat: Option<Timestamp>,
    /// The `execution_end` of the last result that came (Icinga's clock):
    /// what a REST query's `last_check` is compared with.
    last_end: Option<Timestamp>,
    /// When Icinga ran it last (Icinga's clock).
    last_check: Option<Timestamp>,
    /// Late after this.
    deadline: Option<Instant>,
    /// The REST query (or the second look) goes out then.
    query_at: Option<Instant>,
    /// A query found it ran though the stream didn't bring it: the second
    /// look is (or was) on its way.
    rechecked: bool,
    /// The engine reconnected for it since its last beat.
    reconnected: bool,
    /// Why it is dead.
    reason: Option<String>,
}

impl Watch {
    fn new(found: Found, now: Timestamp, allowance: Duration) -> Self {
        Self {
            found,
            state: BeatState::Waiting,
            since: now,
            budget: Budget::default(),
            allowance,
            last_beat: None,
            last_end: None,
            last_check: None,
            deadline: None,
            query_at: None,
            rechecked: false,
            reconnected: false,
            reason: None,
        }
    }

    /// Whether its deadline counts (found, not dead, no query out).
    fn running(&self) -> bool {
        matches!(self.state, BeatState::Waiting | BeatState::OnTime)
    }
}

/// The heartbeats of the environment, as the engine watches them.
#[derive(Debug, Default)]
pub(super) struct Beats {
    setup: HeartbeatSetup,
    /// Which beats the settings asked for at the last discovery (mode,
    /// variable, list): a beat that leaves because they changed is
    /// forgotten, not a finding.
    asked: Option<(ic_config::HeartbeatMode, String, Vec<ServiceKey>)>,
    watches: BTreeMap<ServiceKey, Watch>,
    /// Listed in the settings, but Icinga has no such service.
    not_found: Vec<ServiceKey>,
    /// What the event log remembers (`None` until it answered).
    remembered: Option<BTreeMap<ServiceKey, RememberedBeat>>,
    /// Remembered beats discovery no longer finds, and since when.
    gone: BTreeMap<ServiceKey, (Proves, Timestamp)>,
    /// A query (late beats, or the poll) is out.
    in_flight: bool,
    /// The next poll while the stream can't carry the beats.
    next_poll: Option<Instant>,
    /// What the snapshots carry.
    published: Arc<Heartbeats>,
    /// Something the snapshots show changed (`published` is rebuilt).
    pub(super) changed: bool,
    /// `published` was rebuilt since the last snapshot.
    pub(super) news: bool,
    /// Counts the reads of the remembered beats (another environment's
    /// log drops an older read's answer).
    generation: u64,
}

impl Beats {
    /// What the snapshots carry.
    pub(super) fn published(&self) -> &Arc<Heartbeats> {
        &self.published
    }

    /// Whether `key` is a beat icygui watches.
    pub(super) fn watches(&self, key: &ServiceKey) -> bool {
        self.watches.contains_key(key)
    }
}

impl Engine {
    /// The heartbeats the event log remembers (asked once, at the start
    /// and with another environment's log).
    pub(super) fn read_remembered_beats(&mut self) {
        self.beats.remembered = None;
        self.beats.generation += 1;
        let tx = self.internal_tx.clone();
        let generation = self.beats.generation;
        self.event_log.heartbeats(Box::new(move |beats| {
            let _ = tx.send(Internal::RememberedBeats { generation, beats });
        }));
    }

    /// The event log answered.
    pub(super) fn on_remembered_beats(&mut self, generation: u64, beats: Vec<RememberedBeat>) {
        if generation != self.beats.generation {
            return;
        }
        self.beats.remembered = Some(
            beats
                .into_iter()
                .map(|beat| (beat.key.clone(), beat))
                .collect(),
        );
        // Before the first load there is nothing to find them in.
        if self.loaded {
            self.discover_beats();
        }
    }

    /// Finds the heartbeats in the store's services (see the module
    /// notes) and watches them.
    #[expect(
        clippy::too_many_lines,
        reason = "found, gone, remembered and the stream filter in one pass"
    )]
    pub(super) fn discover_beats(&mut self) {
        let settings = self.spec.environment.trouble.heartbeats.clone();
        let setup = if settings.is_off() {
            HeartbeatSetup::Off
        } else {
            match settings.mode {
                ic_config::HeartbeatMode::Find => HeartbeatSetup::Find {
                    variable: settings.variable_name().to_owned(),
                },
                ic_config::HeartbeatMode::List => HeartbeatSetup::List,
            }
        };
        if setup != self.beats.setup {
            self.beats.setup = setup;
            self.beats.changed = true;
        }
        // The settings changed which beats there are: the ones they no
        // longer ask for are forgotten (no finding), remembered or not.
        let asked = (
            settings.mode,
            settings.variable_name().to_owned(),
            settings.listed(),
        );
        let reconfigured = self
            .beats
            .asked
            .as_ref()
            .is_some_and(|previous| *previous != asked);
        self.beats.asked = Some(asked);
        let local_zone = self.store.node().and_then(|node| node.zone.clone());
        let (found, missing) = heartbeat::discover(
            &settings,
            self.store.services(),
            local_zone.as_deref(),
            self.tuning.heartbeat.min_interval,
        );
        let min_allowance = self.tuning.heartbeat.min_allowance;
        let now = self.ports.clock.now();
        let instant = Instant::now();
        let live = self.phase == Phase::Live;
        let mut remember = Vec::new();
        let found_keys: BTreeSet<ServiceKey> = found.iter().map(|beat| beat.key.clone()).collect();
        // New and changed beats.
        for beat in found {
            let key = beat.key.clone();
            if let Some(watch) = self.beats.watches.get_mut(&key) {
                if watch.found != beat {
                    if watch.found.proves != beat.proves {
                        remember.push(RememberedBeat {
                            key: key.clone(),
                            proves: beat.proves.encode(),
                            missing_since: None,
                        });
                    }
                    watch.allowance = watch.budget.allowance(beat.interval, min_allowance);
                    watch.found = beat;
                    self.beats.changed = true;
                }
            } else {
                let allowance = Budget::default().allowance(beat.interval, min_allowance);
                let mut watch = Watch::new(beat, now, allowance);
                // What the load says about it.
                if let Some(service) = self.store.services().get(&key) {
                    watch.last_check = service.check.last_check;
                    watch.last_end = service.check.last_check;
                    if service.state == ServiceState::Ok {
                        // Its last beat before this session, by Icinga's
                        // clock (close enough for the age the page shows
                        // until the stream brings one).
                        watch.last_beat = service.check.last_check;
                    } else if service.check.last_check.is_some() {
                        watch.state = BeatState::Dead(Death::NotOk);
                        watch.reason = Some(output_of(service));
                    }
                }
                if live && watch.state == BeatState::Waiting {
                    watch.deadline = Some(instant + grace_of(watch.found.interval, allowance));
                }
                let known = self
                    .beats
                    .remembered
                    .as_ref()
                    .and_then(|remembered| remembered.get(&key));
                if known.is_none_or(|known| {
                    known.missing_since.is_some() || known.proves != watch.found.proves.encode()
                }) {
                    remember.push(RememberedBeat {
                        key: key.clone(),
                        proves: watch.found.proves.encode(),
                        missing_since: None,
                    });
                }
                tracing::debug!(object = %watch_object(&key), proves = %watch.found.proves.label(), "heartbeat found");
                self.beats.watches.insert(key.clone(), watch);
                self.beats.gone.remove(&key);
                self.beats.changed = true;
            }
        }
        // Beats no longer found: gone (a finding) when remembered and the
        // store holds a complete load, else forgotten.
        let lost: Vec<ServiceKey> = self
            .beats
            .watches
            .keys()
            .filter(|key| !found_keys.contains(*key))
            .cloned()
            .collect();
        if reconfigured {
            self.forget_unasked_beats(&found_keys);
        }
        for key in lost {
            if let Some(watch) = self.beats.watches.remove(&key) {
                self.beats.changed = true;
                if reconfigured {
                    tracing::debug!(object = %watch_object(&key), "no longer a heartbeat by the settings");
                    self.event_log.forget_heartbeat(key);
                } else if self.loaded && self.beats.setup != HeartbeatSetup::Off {
                    tracing::debug!(object = %watch_object(&key), "heartbeat no longer found");
                    self.beats
                        .gone
                        .insert(key.clone(), (watch.found.proves.clone(), now));
                    remember.push(RememberedBeat {
                        key,
                        proves: watch.found.proves.encode(),
                        missing_since: Some(now),
                    });
                }
            }
        }
        // Remembered from before and not found now (only once a complete
        // load is in, and while heartbeats are on).
        if self.loaded
            && self.beats.setup != HeartbeatSetup::Off
            && let Some(remembered) = &self.beats.remembered
        {
            for (key, beat) in remembered {
                if found_keys.contains(key) || self.beats.gone.contains_key(key) {
                    continue;
                }
                let Some(proves) = Proves::decode(&beat.proves) else {
                    continue;
                };
                let since = beat.missing_since.unwrap_or(now);
                if beat.missing_since.is_none() {
                    remember.push(RememberedBeat {
                        key: key.clone(),
                        proves: beat.proves.clone(),
                        missing_since: Some(since),
                    });
                }
                self.beats.gone.insert(key.clone(), (proves, since));
                self.beats.changed = true;
            }
        }
        // Back again: no longer gone.
        let back: Vec<ServiceKey> = self
            .beats
            .gone
            .keys()
            .filter(|key| found_keys.contains(*key))
            .cloned()
            .collect();
        for key in back {
            self.beats.gone.remove(&key);
            self.beats.changed = true;
        }
        if self.beats.not_found != missing {
            self.beats.not_found = missing;
            self.beats.changed = true;
        }
        if !remember.is_empty() {
            if let Some(remembered) = &mut self.beats.remembered {
                for beat in &remember {
                    remembered.insert(beat.key.clone(), beat.clone());
                }
            }
            self.event_log.remember_heartbeats(remember);
        }
        // Left out of everything else.
        self.store.set_excluded(found_keys.clone());
        // The quiet stream's filter names them.
        let filter = heartbeat::stream_filter(&found_keys);
        let changed = {
            let mut beats = self
                .wish
                .beats
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if *beats == filter {
                false
            } else {
                *beats = filter;
                true
            }
        };
        if changed {
            self.want_mode();
        }
        self.schedule_poll();
        if self.beats.changed {
            self.trouble_due_now();
        }
    }

    /// The settings no longer ask for the remembered and disappeared beats
    /// not in `found`: forgotten, their findings dropped.
    fn forget_unasked_beats(&mut self, found: &BTreeSet<ServiceKey>) {
        let gone: Vec<ServiceKey> = self
            .beats
            .gone
            .keys()
            .filter(|key| !found.contains(*key))
            .cloned()
            .collect();
        for key in &gone {
            self.beats.gone.remove(key);
            self.drop_gone_alert(&watch_object(key));
        }
        let remembered: Vec<ServiceKey> = self
            .beats
            .remembered
            .as_ref()
            .map(|remembered| {
                remembered
                    .keys()
                    .filter(|key| !found.contains(*key))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        if let Some(map) = &mut self.beats.remembered {
            for key in &remembered {
                map.remove(key);
            }
        }
        for key in remembered.into_iter().chain(gone) {
            self.event_log.forget_heartbeat(key);
        }
        self.beats.changed = true;
    }

    /// The user confirmed that a disappeared heartbeat is gone for good.
    pub(super) fn confirm_beat_removal(&mut self, key: ServiceKey) {
        tracing::info!(environment = %self.spec.environment.name, object = %watch_object(&key), "heartbeat removal confirmed");
        self.beats.gone.remove(&key);
        if let Some(remembered) = &mut self.beats.remembered {
            remembered.remove(&key);
        }
        self.drop_gone_alert(&watch_object(&key));
        self.event_log.forget_heartbeat(key);
        self.beats.changed = true;
        self.trouble_due_now();
        self.publish_changes();
    }

    /// Forgets the beats' watch (another server): found again after the
    /// next load.
    pub(super) fn reset_beats(&mut self) {
        self.beats.watches.clear();
        self.beats.gone.clear();
        self.beats.not_found.clear();
        self.beats.in_flight = false;
        self.beats.next_poll = None;
        self.beats.changed = true;
        self.store.set_excluded(BTreeSet::new());
    }

    /// The stream (or a poll) brought a result of beat `key`: `streamed`
    /// results count for its time budget.
    pub(super) fn beat_result(&mut self, key: &ServiceKey, result: &CheckResult, streamed: bool) {
        let now = self.ports.clock.now();
        let instant = Instant::now();
        let state = self.store.services().get(key).map(|service| service.state);
        let polled = self.beats_polled();
        let Some(watch) = self.beats.watches.get_mut(key) else {
            return;
        };
        let end = result.execution_end.non_zero();
        if end.is_some() && watch.last_end == end && watch.running() {
            // The same check again (its state change and its result).
            return;
        }
        if streamed {
            // Against the system's clock: Icinga's times are real ones.
            watch.budget.record(result, Timestamp::now());
            watch.allowance = watch
                .budget
                .allowance(watch.found.interval, self.tuning.heartbeat.min_allowance);
        }
        if end.is_some() {
            watch.last_end = end;
            watch.last_check = end;
        }
        watch.rechecked = false;
        watch.reconnected = false;
        watch.query_at = None;
        let ok = state.is_none_or(|state| state == ServiceState::Ok);
        let old = watch.state;
        if ok {
            watch.last_beat = Some(now);
            watch.reason = None;
            watch.deadline =
                Some(instant + deadline_after(watch.found.interval, watch.allowance, polled));
            watch.state = BeatState::OnTime;
        } else {
            watch.reason = Some(first_line(&result.output));
            watch.deadline = None;
            watch.state = BeatState::Dead(Death::NotOk);
        }
        if old != watch.state {
            watch.since = now;
            tracing::debug!(object = %watch_object(key), from = ?old, to = ?watch.state, "heartbeat");
            self.beats.changed = true;
            self.trouble_due_now();
        } else if ok {
            // The age the page shows moves with each beat.
            self.beats.changed = true;
        }
    }

    /// Whether `service` is (or may have become) a heartbeat: discovery
    /// looks again when an answer brings one.
    pub(super) fn beat_candidate(&self, service: &ic_model::Service) -> bool {
        if self.beats.watches(&service.key) {
            return true;
        }
        let settings = &self.spec.environment.trouble.heartbeats;
        match settings.mode {
            ic_config::HeartbeatMode::Find => {
                let variable = settings.variable_name();
                !variable.is_empty() && service.vars.contains_key(variable)
            }
            ic_config::HeartbeatMode::List => settings
                .list
                .iter()
                .any(|entry| ServiceKey::parse(entry.trim()).as_ref() == Some(&service.key)),
        }
    }

    /// How long every watched beat takes at most to be found stopped once
    /// Icinga stops: the longest interval with its allowance, the query
    /// and the second look.
    pub(super) fn beats_settle(&self) -> Duration {
        let timing = self.tuning.heartbeat;
        self.beats
            .watches
            .values()
            .map(|watch| watch.found.interval + watch.allowance)
            .max()
            .unwrap_or_default()
            + timing.query_after
            + timing.recheck_after
    }

    /// Whether the stream can't carry the beats' results: they are read
    /// once per interval instead.
    pub(super) fn beats_polled(&self) -> bool {
        self.conn.as_ref().is_some_and(|conn| {
            self.lines.is_none() || !conn.kinds.contains(&EventKind::CheckResult)
        })
    }

    /// Plans the next poll (or none, when the stream carries the beats).
    fn schedule_poll(&mut self) {
        let polled = self.beats_polled() && !self.beats.watches.is_empty();
        if polled != self.beats.published.polled {
            self.beats.changed = true;
        }
        if !polled {
            self.beats.next_poll = None;
            return;
        }
        if self.beats.next_poll.is_none() {
            self.beats.next_poll = Some(Instant::now() + self.poll_interval());
        }
    }

    /// The shortest beat's interval.
    fn poll_interval(&self) -> Duration {
        self.beats
            .watches
            .values()
            .map(|watch| watch.found.interval)
            .min()
            .unwrap_or(Duration::from_mins(1))
            .max(self.tuning.heartbeat.min_interval)
    }

    /// The stream's mode changed (a switch completed, or a connect): polls
    /// start or stop.
    pub(super) fn beats_stream_changed(&mut self) {
        self.beats.next_poll = None;
        self.schedule_poll();
        if self.beats.next_poll.is_some() {
            // A poll may see a beat up to an interval after it ran.
            let now = Instant::now();
            for watch in self.beats.watches.values_mut() {
                if watch.running() {
                    let polled = now + deadline_after(watch.found.interval, watch.allowance, true);
                    watch.deadline = Some(watch.deadline.map_or(polled, |at| at.max(polled)));
                }
            }
        }
    }

    /// Every beat that isn't dead gets its grace (after a reconnect, the
    /// computer waking up or an Icinga restart): late only
    /// [`heartbeat::GRACE_INTERVALS`] intervals from now.
    pub(super) fn beats_grace(&mut self, why: &str) {
        let instant = Instant::now();
        let mut any = false;
        for watch in self.beats.watches.values_mut() {
            match watch.state {
                BeatState::Dead(Death::NotDelivered) => {
                    // The reconnect may have healed the stream: the next
                    // beat says so; else one more query after the grace.
                    let grace = grace_of(watch.found.interval, watch.allowance);
                    watch.query_at = Some(instant + grace + self.tuning.heartbeat.query_after);
                }
                BeatState::Dead(_) => {}
                _ => {
                    let grace = instant + grace_of(watch.found.interval, watch.allowance);
                    watch.deadline = Some(watch.deadline.map_or(grace, |at| at.max(grace)));
                    if matches!(watch.state, BeatState::Late | BeatState::Checking) {
                        watch.state = BeatState::Waiting;
                        watch.query_at = None;
                        watch.rechecked = false;
                        self.beats.changed = true;
                    }
                    any = true;
                }
            }
        }
        if any {
            tracing::debug!(why, "heartbeats: grace");
        }
        self.beats.in_flight = false;
        self.beats_stream_changed();
    }

    /// When the beats next need a look: a deadline, a query, a poll. Only
    /// while live, and one query at a time.
    pub(super) fn beats_due(&self) -> Option<Instant> {
        if self.phase != Phase::Live {
            return None;
        }
        let deadlines = self.beats.watches.values().filter_map(|watch| {
            if watch.running() {
                watch.deadline
            } else {
                None
            }
        });
        let queries = (!self.beats.in_flight)
            .then(|| {
                self.beats
                    .watches
                    .values()
                    .filter_map(|watch| watch.query_at)
                    .chain(self.beats.next_poll)
                    .min()
            })
            .flatten();
        deadlines.chain(queries).min()
    }

    /// Late beats, their queries and the poll, when due.
    pub(super) fn run_beats(&mut self, now: Instant) {
        if self.phase != Phase::Live {
            return;
        }
        let wall = self.ports.clock.now();
        let mut changed = false;
        for (key, watch) in &mut self.beats.watches {
            if watch.running() && watch.deadline.is_some_and(|at| at <= now) {
                // Timing alone never alerts: one query decides.
                tracing::debug!(object = %watch_object(key), "heartbeat late");
                watch.state = BeatState::Late;
                watch.since = wall;
                watch.query_at =
                    Some(watch.deadline.unwrap_or(now) + self.tuning.heartbeat.query_after);
                changed = true;
            }
        }
        if changed {
            self.beats.changed = true;
            self.trouble_due_now();
        }
        if self.beats.in_flight {
            return;
        }
        let due: Vec<ServiceKey> = self
            .beats
            .watches
            .iter()
            .filter(|(_, watch)| watch.query_at.is_some_and(|at| at <= now))
            .map(|(key, _)| key.clone())
            .collect();
        let poll = self.beats.next_poll.is_some_and(|at| at <= now);
        if due.is_empty() && !poll {
            return;
        }
        let keys: Vec<ServiceKey> = if poll {
            self.beats.next_poll = Some(now + self.poll_interval());
            self.beats.watches.keys().cloned().collect()
        } else {
            due.clone()
        };
        for key in &due {
            if let Some(watch) = self.beats.watches.get_mut(key) {
                watch.query_at = None;
                if watch.state == BeatState::Late {
                    watch.state = BeatState::Checking;
                    self.beats.changed = true;
                }
            }
        }
        self.query_beats(keys, due);
    }

    /// Asks Icinga for `keys` (one request through the budget); `late`
    /// are the ones a deadline sent.
    fn query_beats(&mut self, keys: Vec<ServiceKey>, late: Vec<ServiceKey>) {
        let Some(conn) = &self.conn else {
            return;
        };
        self.beats.in_flight = true;
        let client = conn.client.clone();
        let tx = self.internal_tx.clone();
        let session = self.session;
        let started = self.seq.load(std::sync::atomic::Ordering::SeqCst);
        self.tasks.spawn(async move {
            let objects: Vec<ObjectKey> = keys.iter().cloned().map(ObjectKey::from).collect();
            let result = client.objects(&objects, Detail::Full).await;
            let _ = tx.send(Internal::Beats {
                session,
                started,
                late,
                result,
            });
        });
    }

    /// A query of the beats is back: `late` are the ones a deadline sent,
    /// the others came with a poll.
    #[expect(
        clippy::too_many_lines,
        reason = "each verdict of a beat's query with what it does, in one place"
    )]
    pub(super) fn on_beats(
        &mut self,
        started: u64,
        late: &[ServiceKey],
        result: Result<ic_api::Fetched, ApiError>,
    ) {
        self.beats.in_flight = false;
        let fetched = match result {
            Ok(fetched) => fetched,
            Err(ApiError::Unauthorized) => {
                self.fail(Failure::Auth(ApiError::Unauthorized.to_string()));
                return;
            }
            Err(ApiError::Forbidden(message)) => {
                tracing::warn!(environment = %self.spec.environment.name, %message, "the API user may not read the heartbeats; they can't be checked");
                return;
            }
            Err(error) => {
                // The connection broke: the usual way back.
                tracing::info!(environment = %self.spec.environment.name, %error, "a heartbeat query failed; reconnecting");
                self.fail(Failure::Transient(format!(
                    "a heartbeat query failed: {error}"
                )));
                return;
            }
        };
        let services = fetched.services.clone();
        self.store.apply_fetched(
            Vec::new(),
            fetched.services,
            Detail::Full,
            &fetched.missing,
            started,
        );
        if !fetched.missing.is_empty() {
            // Deleted meanwhile: discovery takes it out.
            self.discover_beats();
        }
        let icinga_now = self.watchdog.icinga_now();
        let polled = self.beats_polled();
        let wall = self.ports.clock.now();
        let instant = Instant::now();
        let mut reconnect = None;
        for service in &services {
            let key = service.key.clone();
            let Some(watch) = self.beats.watches.get(&key) else {
                continue;
            };
            let verdict = heartbeat::judge(
                service,
                watch.last_end,
                watch.found.interval,
                watch.allowance,
                icinga_now,
                self.tuning.heartbeat.query_after,
            );
            let asked_late = late.contains(&key);
            match verdict {
                Verdict::Fresh if polled || !asked_late => {
                    // A poll's newer `last_check` is a beat.
                    if let Some(result) = &service.check.result {
                        self.beat_result(&key, result, false);
                    } else if let Some(last_check) = service.check.last_check
                        && watch.last_end != Some(last_check)
                    {
                        let result = CheckResult {
                            execution_end: last_check,
                            ..CheckResult::default()
                        };
                        self.beat_result(&key, &result, false);
                    }
                }
                Verdict::NotOk(output) => {
                    let Some(watch) = self.beats.watches.get_mut(&key) else {
                        continue;
                    };
                    if watch.state != BeatState::Dead(Death::NotOk) {
                        watch.state = BeatState::Dead(Death::NotOk);
                        watch.since = wall;
                        self.beats.changed = true;
                    }
                    watch.reason = Some(first_line(&output));
                    watch.last_check = service.check.last_check;
                    watch.deadline = None;
                }
                Verdict::Stopped if asked_late => {
                    let Some(watch) = self.beats.watches.get_mut(&key) else {
                        continue;
                    };
                    tracing::debug!(object = %watch_object(&key), "heartbeat stopped: no check since the last beat");
                    if watch.state != BeatState::Dead(Death::Stopped) {
                        watch.state = BeatState::Dead(Death::Stopped);
                        watch.since = wall;
                        self.beats.changed = true;
                    }
                    watch.last_check = service.check.last_check;
                    watch.reason = Some(service.check.last_check.map_or_else(
                        || "Icinga never ran it".to_owned(),
                        |at| format!("no check since {}", crate::ports::clock_time(at)),
                    ));
                    watch.deadline = None;
                }
                Verdict::Stopped => {}
                Verdict::Fresh => {
                    // It ran, and the stream didn't bring it.
                    let Some(watch) = self.beats.watches.get_mut(&key) else {
                        continue;
                    };
                    watch.last_check = service.check.last_check;
                    if !watch.rechecked {
                        watch.rechecked = true;
                        watch.state = BeatState::Checking;
                        watch.query_at = Some(instant + self.tuning.heartbeat.recheck_after);
                    } else if !watch.reconnected
                        || watch.state == BeatState::Dead(Death::NotDelivered)
                    {
                        watch.reconnected = true;
                        watch.rechecked = false;
                        if watch.state == BeatState::Dead(Death::NotDelivered) {
                            watch.query_at = Some(instant + UNDELIVERED_RETRY);
                        }
                        reconnect = Some(key.clone());
                    } else {
                        tracing::debug!(object = %watch_object(&key), "heartbeat not delivered, even after a reconnect");
                        watch.state = BeatState::Dead(Death::NotDelivered);
                        watch.since = wall;
                        watch.reason =
                            Some("Icinga runs it, but its results don't arrive".to_owned());
                        watch.query_at = Some(instant + UNDELIVERED_RETRY);
                        watch.deadline = None;
                        self.beats.changed = true;
                    }
                }
            }
        }
        self.trouble_due_now();
        if let Some(key) = reconnect {
            tracing::info!(environment = %self.spec.environment.name, object = %watch_object(&key), "Icinga ran the heartbeat, but the event stream didn't bring it; reconnecting");
            self.fail(Failure::Transient(
                "the event stream lost a heartbeat's result; reconnecting".to_owned(),
            ));
        }
    }

    /// Rebuilds the heartbeats as the snapshots (and the trouble alerts)
    /// see them, if something changed.
    pub(super) fn refresh_beats(&mut self) {
        if !std::mem::take(&mut self.beats.changed) {
            return;
        }
        let instant = Instant::now();
        let now = self.ports.clock.now();
        let polled = self.beats_polled() && !self.beats.watches.is_empty();
        let mut beats: Vec<Heartbeat> = self
            .beats
            .watches
            .iter()
            .map(|(key, watch)| Heartbeat {
                key: key.clone(),
                proves: watch.found.proves.clone(),
                interval: watch.found.interval,
                state: watch.state,
                last_beat: watch.last_beat,
                last_check: watch.last_check,
                since: watch.since,
                deadline: watch
                    .deadline
                    .filter(|_| watch.running())
                    .map(|at| now.plus(at.saturating_duration_since(instant))),
                allowance: watch.allowance,
                reason: watch.reason.clone(),
            })
            .chain(
                self.beats
                    .gone
                    .iter()
                    .map(|(key, (proves, since))| Heartbeat {
                        key: key.clone(),
                        proves: proves.clone(),
                        interval: Duration::ZERO,
                        state: BeatState::Disappeared,
                        last_beat: None,
                        last_check: None,
                        since: *since,
                        deadline: None,
                        allowance: Duration::ZERO,
                        reason: None,
                    }),
            )
            .chain(self.beats.not_found.iter().map(|key| Heartbeat {
                key: key.clone(),
                proves: Proves::Zone(String::new()),
                interval: Duration::ZERO,
                state: BeatState::NotFound,
                last_beat: None,
                last_check: None,
                since: now,
                deadline: None,
                allowance: Duration::ZERO,
                reason: None,
            }))
            .collect();
        let ranks = self.zone_ranks();
        heartbeat::order(&mut beats, |zone| {
            zone.and_then(|zone| ranks.get(zone).copied())
                .unwrap_or(usize::MAX)
        });
        let published = Arc::new(Heartbeats {
            setup: self.beats.setup.clone(),
            beats,
            polled,
        });
        if published != self.beats.published {
            self.beats.published = published;
            self.beats.news = true;
        }
    }

    /// The zones in the cluster's order (top-level first, then depth
    /// first), as their position.
    fn zone_ranks(&self) -> BTreeMap<String, usize> {
        let (endpoints, zones) = self.store.cluster();
        let node = self.conn.as_ref().map(|conn| &*conn.node);
        let mut ranks = BTreeMap::new();
        for node in crate::topology::cluster_nodes(zones, endpoints, node) {
            let next = ranks.len();
            ranks.entry(node.zone).or_insert(next);
        }
        for zone in zones {
            let next = ranks.len();
            ranks.entry(zone.name.clone()).or_insert(next);
        }
        ranks
    }
}

/// How long a beat may take after its last one: its interval and
/// allowance (a poll may see it up to an interval later).
fn deadline_after(interval: Duration, allowance: Duration, polled: bool) -> Duration {
    if polled {
        interval * 2 + allowance
    } else {
        interval + allowance
    }
}

/// A beat's grace: [`heartbeat::GRACE_INTERVALS`] intervals, and at least
/// one interval with its allowance.
fn grace_of(interval: Duration, allowance: Duration) -> Duration {
    (interval * heartbeat::GRACE_INTERVALS).max(interval + allowance)
}

/// The first line of a plugin output.
fn first_line(output: &str) -> String {
    output.lines().next().unwrap_or_default().trim().to_owned()
}

/// A service's output, as far as the store has it.
fn output_of(service: &ic_model::Service) -> String {
    service
        .check
        .result
        .as_ref()
        .map(|result| first_line(&result.output))
        .unwrap_or_default()
}

/// `host!service`, for the log.
fn watch_object(key: &ServiceKey) -> String {
    format!("{}!{}", key.host, key.name)
}
