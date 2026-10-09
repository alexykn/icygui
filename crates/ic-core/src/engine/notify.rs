//! Notifications and the event log's entries: what every applied change
//! means for the rule engine (`ic_rules::RuleEngine`) and the local log.
//!
//! - Each change the store applied (an event, or a query answer that found
//!   something no event announced) becomes a [`RuleInput`] and, for the
//!   kinds the log keeps, a [`LogEntry`] right away, while the store still
//!   shows the object as the change left it. The initial load produces
//!   none: it fills the store and seeds the rule engine with its problems
//!   and flapping objects ([`Notify::seed`]), so the rule engine knows
//!   what was already wrong without notifying it; only a state that
//!   changed after the event stream subscribed (its `StateChange` line
//!   waited while the load ran, or while a background start waited) is
//!   judged as a change. Problems an earlier run notified (their ids are
//!   in the event log) count as notified, so their recoveries and
//!   acknowledgements follow.
//! - `handled` is computed from the store after the change: a problem
//!   that is acknowledged, in downtime, unreachable through a dependency,
//!   or (for services) on a host with a problem; Icinga suppresses its
//!   notifications in all these cases.
//! - When `handled` changes without an event of the object's own (the
//!   services of a host that went down or came back), the object's state
//!   is repeated with the new `handled`; after `handled` turned false, the
//!   object's next check result repeats it once more, so the rule engine
//!   knows the state is current rather than left over from the outage.
//! - Inputs wait for their memberships: the dashboards that match the
//!   object, known once the dashboards were evaluated for the snapshot
//!   that includes the change. Then the rule engine judges them, in order.
//! - The rule engine ticks every second (delays, storm summaries, mutes
//!   and pauses ending). Every intent is logged first, then emitted as
//!   `CoreEvent::Notification`; the audible ones also go to the
//!   `Notifier`.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::mem;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use ic_api::ApiError;
use ic_model::{
    CheckableState, CommentKind, Event, HostName, HostState, Notification, ObjectKey, ServiceState,
    StateType, Timestamp,
};
use ic_rules::{
    Change, DashboardRef, DashboardScope, GroupScope, LocalTime, NotificationIntent, RuleEngine,
    RuleInput, RuleSet, state_intent_id,
};

use super::{AppliedEvent, Engine, Internal};
use crate::command::{CoreEvent, LogEntry, LogKind, NotificationRecord};
use crate::connect::Failure;
use crate::dashboards::Dashboards;
use crate::store::{Discovered, ObjectView, Store};

/// At most this many objects wait for the check result that confirms
/// their state after their handling ended; beyond it the set starts over
/// (the rule engine then waits its settle time instead).
const MAX_CONFIRM: usize = 50_000;
/// At most this many downtimes are remembered as started (Icinga reports
/// a fixed downtime as started and triggered: logged once).
const MAX_STARTED: usize = 10_000;

/// The rule engine and what feeds it.
#[derive(Debug)]
pub(super) struct Notify {
    rules: RuleEngine,
    /// Inputs waiting for their memberships (changes since the last
    /// snapshot was cut).
    pending: Vec<RuleInput>,
    /// Inputs of the snapshot whose dashboards are being evaluated.
    evaluating: Vec<RuleInput>,
    /// What the session's first load found (problems and flapping
    /// objects, each with whether it flaps), waiting for memberships like
    /// inputs; judged before them.
    seeds: Vec<(RuleInput, bool)>,
    /// Seeds of the snapshot whose dashboards are being evaluated.
    seeds_evaluating: Vec<(RuleInput, bool)>,
    /// Seeded problems by their state's notification id, until the event
    /// log said which of them an earlier run notified.
    seeded: HashMap<String, (ObjectKey, CheckableState, Timestamp)>,
    /// Ids of seeded problems the event log hasn't been asked about yet.
    lookups: Vec<String>,
    /// Objects whose `handled` turned false: their next check result
    /// repeats their state.
    confirm: HashSet<ObjectKey>,
    /// Downtimes logged as started.
    started: HashSet<String>,
    /// What the load in flight found that no event announced: judged when
    /// the load is complete, so the problems' output is loaded by then, or
    /// before the next input about the same object.
    discovered: Deferred,
    /// The pause last announced (`CoreEvent::NotificationsPaused`).
    announced_pause: Option<Timestamp>,
}

/// A load's findings waiting to be judged, in order, each with Icinga's
/// time when it was found. The rule engine needs each object's inputs in
/// order: an object whose state an event changes while its finding waits
/// has the finding judged first ([`Deferred::take_object`]).
#[derive(Debug, Default)]
struct Deferred {
    found: Vec<Option<(Discovered, Timestamp)>>,
    /// Indexes into `found`, per object.
    by_object: HashMap<ObjectKey, Vec<usize>>,
}

impl Deferred {
    fn push(&mut self, change: Discovered, at: Timestamp) {
        self.by_object
            .entry(change.object.clone())
            .or_default()
            .push(self.found.len());
        self.found.push(Some((change, at)));
    }

    /// Takes `object`'s findings, in order.
    fn take_object(&mut self, object: &ObjectKey) -> Vec<(Discovered, Timestamp)> {
        let Some(indexes) = self.by_object.remove(object) else {
            return Vec::new();
        };
        indexes
            .into_iter()
            .filter_map(|index| self.found.get_mut(index).and_then(Option::take))
            .collect()
    }

    /// Takes every finding, in order.
    fn take_all(&mut self) -> Vec<(Discovered, Timestamp)> {
        self.by_object.clear();
        mem::take(&mut self.found).into_iter().flatten().collect()
    }
}

impl Notify {
    /// The dashboards whose memberships can change a notification
    /// decision, for quiet mode (which evaluates only these): every one
    /// (`None`) while the environment's own rule is on, since an object
    /// that matches no dashboard notifies by it and one that matches a
    /// dashboard that is off doesn't; else those whose effective rule is
    /// on (with the environment off, a dashboard that is off decides
    /// nothing whether it matches or not).
    pub(super) fn decisive_dashboards(&self) -> Option<BTreeSet<DashboardRef>> {
        let rules = self.rules.rules();
        if rules.settings.enabled {
            return None;
        }
        Some(
            rules
                .groups
                .iter()
                .flat_map(|group| {
                    group.dashboards.iter().map(|dashboard| DashboardRef {
                        group_id: group.id.clone(),
                        dashboard_id: dashboard.id.clone(),
                    })
                })
                .filter(|reference| {
                    rules
                        .dashboard_rule(reference)
                        .is_some_and(|rule| rule.enabled)
                })
                .collect(),
        )
    }

    /// Notifications for `environment`'s rules, nothing seen yet.
    pub(super) fn new(environment: &ic_config::Environment) -> Self {
        Self {
            rules: RuleEngine::new(rule_set(environment)),
            pending: Vec::new(),
            evaluating: Vec::new(),
            seeds: Vec::new(),
            seeds_evaluating: Vec::new(),
            seeded: HashMap::new(),
            lookups: Vec::new(),
            confirm: HashSet::new(),
            started: HashSet::new(),
            discovered: Deferred::default(),
            announced_pause: None,
        }
    }

    /// The environment's rules changed: the rule engine judges with the new
    /// ones from now on (and re-judges waiting notifications).
    pub(super) fn set_rules(&mut self, environment: &ic_config::Environment) {
        let rules = rule_set(environment);
        if *self.rules.rules() != rules {
            self.rules.set_rules(rules);
        }
    }

    /// The environment now points at another server: nothing the rule
    /// engine remembers applies any more. A pause stays.
    pub(super) fn reset(&mut self, environment: &ic_config::Environment) {
        let paused = self.rules.paused_until();
        *self = Self {
            announced_pause: self.announced_pause,
            ..Self::new(environment)
        };
        self.rules.pause_until(paused);
    }

    /// Pauses notifications until `until` (`None` resumes); returns the
    /// pause in effect, to announce.
    pub(super) fn pause(&mut self, until: Option<Timestamp>) -> Option<Timestamp> {
        self.rules.pause_until(until);
        self.announced_pause = self.rules.paused_until();
        self.announced_pause
    }

    /// Whether notifications are paused at `now` (only a pause holds the
    /// trouble alerts back).
    pub(super) fn paused(&self, now: Timestamp) -> bool {
        self.rules
            .paused_until()
            .is_some_and(|until| until.as_unix_seconds() > now.as_unix_seconds())
    }

    /// Whether inputs (or seeds) wait for a snapshot.
    pub(super) fn has_pending(&self) -> bool {
        !self.pending.is_empty() || !self.seeds.is_empty()
    }

    /// A snapshot is cut for evaluation: the inputs so far belong to it.
    pub(super) fn begin_evaluation(&mut self) {
        let pending = mem::take(&mut self.pending);
        self.evaluating.extend(pending);
        let seeds = mem::take(&mut self.seeds);
        self.seeds_evaluating.extend(seeds);
    }

    /// The evaluation failed: its inputs wait for the next one.
    pub(super) fn evaluation_failed(&mut self) {
        let mut inputs = mem::take(&mut self.evaluating);
        inputs.append(&mut self.pending);
        self.pending = inputs;
        let mut seeds = mem::take(&mut self.seeds_evaluating);
        seeds.append(&mut self.seeds);
        self.seeds = seeds;
    }

    /// Judges the inputs of the evaluated snapshot (`evaluated`) or, with
    /// no evaluation in between, every pending input, with their
    /// memberships from `dashboards`, after the seeds among them. Returns
    /// the intents.
    pub(super) fn judge(
        &mut self,
        dashboards: &Dashboards,
        evaluated: bool,
        now: Timestamp,
        local: LocalTime,
    ) -> Vec<NotificationIntent> {
        let (seeds, inputs) = if evaluated {
            (
                mem::take(&mut self.seeds_evaluating),
                mem::take(&mut self.evaluating),
            )
        } else {
            let mut seeds = mem::take(&mut self.seeds_evaluating);
            seeds.append(&mut self.seeds);
            let mut inputs = mem::take(&mut self.evaluating);
            inputs.append(&mut self.pending);
            (seeds, inputs)
        };
        for (mut input, flapping) in seeds {
            input.memberships = dashboards.memberships(&input.object);
            if let Change::State { current, since, .. } = &input.change
                && current.is_problem()
            {
                let since = since.non_zero().unwrap_or(input.at);
                let id = state_intent_id(&input.object, *current, since);
                self.lookups.push(id.clone());
                self.seeded
                    .insert(id, (input.object.clone(), *current, since));
            }
            self.rules.seed(input, flapping, now);
        }
        let mut intents = Vec::new();
        for mut input in inputs {
            input.memberships = dashboards.memberships(&input.object);
            intents.extend(self.rules.on_input(input, now, local));
        }
        intents
    }

    /// The notification ids of problems seeded since the last call: the
    /// event log is asked which of them an earlier run notified.
    pub(super) fn take_lookups(&mut self) -> Vec<String> {
        mem::take(&mut self.lookups)
    }

    /// The event log answered about the seeded problems `asked`: it has
    /// `known`, which an earlier run notified, so their recoveries and
    /// acknowledgements follow (if the objects are still in those states).
    pub(super) fn restore(&mut self, asked: &[String], known: &[String], now: Timestamp) {
        for id in known {
            if let Some((object, state, since)) = self.seeded.remove(id) {
                self.rules.restore_notified(&object, state, since, now);
            }
        }
        for id in asked {
            self.seeded.remove(id);
        }
        if self.seeded.is_empty() {
            self.seeded = HashMap::new();
        }
    }

    /// The session's first load is complete: the rule engine learns what
    /// is wrong (and what flaps) without notifying it, so a problem left
    /// over from an outage still waits for a fresh check when its host
    /// comes back, and a flapping object stays quiet until it stops. The
    /// seeds wait for their memberships like inputs. `at`: Icinga's time
    /// as far as it is known.
    ///
    /// `began`: objects whose state changed after the event stream
    /// subscribed, which the load's answers already show (a `StateChange`
    /// line waited while the load ran, or while a background start waited
    /// to begin it). Their state is judged and logged like any state
    /// change (from an unknown previous state), so a problem that began
    /// meanwhile notifies; a flapping one is seeded all the same.
    pub(super) fn seed(
        &mut self,
        store: &Store,
        began: &HashSet<ObjectKey>,
        at: Timestamp,
        log: &mut Vec<LogEntry>,
    ) {
        let hosts = store.hosts().iter().map(|(name, host)| {
            (
                ObjectKey::Host { name: name.clone() },
                ObjectView::of(CheckableState::Host(host.state), &host.check),
            )
        });
        let services = store.services().iter().map(|(key, service)| {
            (
                ObjectKey::Service { key: key.clone() },
                ObjectView::of(CheckableState::Service(service.state), &service.check),
            )
        });
        let mut changed = Vec::new();
        let known = hosts.chain(services).filter_map(|(object, view)| {
            if !view.flapping && began.contains(&object) {
                changed.push((object, view));
                None
            } else {
                Some((object, view))
            }
        });
        self.seed_views(store, known, at);
        for (object, view) in changed {
            let output = store.current_output(&object).unwrap_or_default().to_owned();
            let host_problem = host_problem_of(store, &object);
            self.push(
                store,
                &object,
                Change::State {
                    previous: None,
                    current: view.state,
                    state_type: view.state_type,
                    since: view.since,
                    output: output.clone(),
                },
                handled(&view, host_problem),
                at,
            );
            log.push(entry_of(
                &object,
                LogKind::State {
                    state: view.state,
                    state_type: view.state_type,
                },
                view.since.non_zero().unwrap_or(at),
                first_line(&output),
                None,
            ));
        }
    }

    /// Objects a fuller view brought that the store had never held (a
    /// master's load after a satellite's, ENV-12): the rule engine learns
    /// them like the first load's, and those an earlier run notified count
    /// as notified ([`Notify::restore`]), so their recoveries follow.
    pub(super) fn seed_objects(&mut self, store: &Store, objects: &[ObjectKey], at: Timestamp) {
        let views = objects
            .iter()
            .filter_map(|object| Some((object.clone(), store.view_of(object)?)));
        self.seed_views(store, views, at);
    }

    /// Seeds the problems and flapping objects among `views`.
    fn seed_views(
        &mut self,
        store: &Store,
        views: impl Iterator<Item = (ObjectKey, ObjectView)>,
        at: Timestamp,
    ) {
        let found: Vec<(ObjectKey, ObjectView)> = views
            .filter(|(object, view)| {
                (view.state.is_problem() || view.flapping) && !store.is_excluded(object)
            })
            .collect();
        tracing::debug!(count = found.len(), "seeding the rule engine");
        for (object, view) in found {
            let output = store.current_output(&object).unwrap_or_default().to_owned();
            let (host_display, service_display) = store.display_names(&object);
            let input = RuleInput {
                handled: handled(&view, host_problem_of(store, &object)),
                object,
                host_display,
                service_display,
                change: Change::State {
                    previous: None,
                    current: view.state,
                    state_type: view.state_type,
                    since: view.since,
                    output,
                },
                memberships: Vec::new(),
                at,
            };
            self.seeds.push((input, view.flapping));
        }
    }

    /// The one-second tick: due delayed notifications and storm
    /// summaries. Also says whether the pause ended (to announce).
    pub(super) fn tick(
        &mut self,
        now: Timestamp,
        local: LocalTime,
    ) -> (Vec<NotificationIntent>, bool) {
        let intents = self.rules.tick(now, local);
        let ended = self.announced_pause.is_some() && self.rules.paused_until().is_none();
        if ended {
            self.announced_pause = None;
        }
        (intents, ended)
    }

    // --- inputs ------------------------------------------------------------------------

    /// An event the store just applied (the store shows its result):
    /// queues its rule inputs and appends its log entries to `log`.
    #[expect(clippy::too_many_lines, reason = "one arm per event type")]
    pub(super) fn applied(&mut self, store: &Store, entry: &AppliedEvent, log: &mut Vec<LogEntry>) {
        let Some(object) = entry.event.object() else {
            return;
        };
        if store.is_excluded(object) {
            // A heartbeat: no rule inputs, no history.
            return;
        }
        // What a load found about the object came before this event.
        self.flush_deferred(store, object, log);
        let (Some(before), Some(after)) = (entry.before, entry.after) else {
            return;
        };
        let at = entry.event.at();
        let host_problem = host_problem_of(store, object);
        let handled_before = handled(&before, host_problem);
        let handled_after = handled(&after, host_problem);
        // Whether the event has an input of its own, which carries the new
        // `handled`; and whether the state is judged as it is now (a state
        // change, or a confirmed one), needing no further confirmation.
        let mut own_input = false;
        let mut confirmed = false;
        match &entry.event {
            Event::CheckResult { result, .. } | Event::StateChange { result, .. } => {
                let check = matches!(entry.event, Event::CheckResult { .. });
                if state_changed(&before, &after) {
                    self.state_change(store, object, before.state, &after, &result.output, at, log);
                    own_input = true;
                    confirmed = true;
                } else if check && handled_before && !handled_after {
                    // The check shows the handling over (reachable again,
                    // or an acknowledgement or downtime gone without its
                    // event): it ended by the previous check at the latest,
                    // and this check, made since, confirms the state.
                    match entry.previous_check.filter(|previous| *previous < at) {
                        Some(previous) => {
                            self.repeat(store, object, &after, false, previous);
                            self.repeat(store, object, &after, false, at);
                            confirmed = true;
                        }
                        None => self.repeat(store, object, &after, false, at),
                    }
                    own_input = true;
                } else if check && !handled_after && self.confirm.remove(object) {
                    // The first check after the handling ended: the state
                    // is current.
                    self.repeat(store, object, &after, false, at);
                    own_input = true;
                    confirmed = true;
                }
            }
            Event::AcknowledgementSet {
                author, comment, ..
            } => {
                self.push(
                    store,
                    object,
                    Change::AcknowledgementSet {
                        author: author.clone(),
                        comment: comment.clone(),
                    },
                    handled_after,
                    at,
                );
                log.push(entry_of(
                    object,
                    LogKind::AcknowledgementSet,
                    at,
                    comment,
                    Some(author),
                ));
                own_input = true;
            }
            Event::AcknowledgementCleared { .. } => {
                if before.acknowledged {
                    self.push(
                        store,
                        object,
                        Change::AcknowledgementCleared,
                        handled_after,
                        at,
                    );
                    log.push(entry_of(
                        object,
                        LogKind::AcknowledgementCleared,
                        at,
                        "",
                        None,
                    ));
                    own_input = true;
                }
            }
            Event::CommentAdded { comment, .. } | Event::CommentRemoved { comment, .. } => {
                if comment.kind == CommentKind::User {
                    let kind = if matches!(entry.event, Event::CommentAdded { .. }) {
                        LogKind::CommentAdded
                    } else {
                        LogKind::CommentRemoved
                    };
                    log.push(entry_of(
                        object,
                        kind,
                        at,
                        &comment.text,
                        Some(&comment.author),
                    ));
                }
            }
            Event::DowntimeStarted { downtime, .. } | Event::DowntimeTriggered { downtime, .. } => {
                // Icinga reports a fixed downtime as started and triggered;
                // the rule engine counts starts within five seconds as one.
                self.push(
                    store,
                    object,
                    Change::DowntimeStarted {
                        author: downtime.author.clone(),
                        comment: downtime.comment.clone(),
                    },
                    handled_after,
                    at,
                );
                own_input = true;
                if self.started.len() >= MAX_STARTED {
                    self.started.clear();
                }
                if self.started.insert(downtime.name.clone()) {
                    log.push(entry_of(
                        object,
                        LogKind::DowntimeStarted,
                        at,
                        &downtime.comment,
                        Some(&downtime.author),
                    ));
                }
            }
            Event::DowntimeRemoved { downtime, .. } => {
                let started = self.started.remove(&downtime.name);
                // Removed while (or after) it was in effect: it ended. One
                // removed before it began ends nothing.
                if started
                    || entry.downtime_was_in_effect
                    || downtime.in_effect
                    || downtime.trigger_time.is_some()
                {
                    self.push(store, object, Change::DowntimeEnded, handled_after, at);
                    log.push(entry_of(
                        object,
                        LogKind::DowntimeEnded,
                        at,
                        &downtime.comment,
                        Some(&downtime.author),
                    ));
                    own_input = true;
                }
            }
            Event::Flapping {
                flapping, current, ..
            } => {
                if before.flapping != after.flapping {
                    self.flapping(
                        store,
                        object,
                        *flapping,
                        handled_after,
                        at,
                        log,
                        Some(*current),
                    );
                    own_input = true;
                }
            }
            Event::DowntimeAdded { .. }
            | Event::ObjectLifecycle { .. }
            | Event::Notification { .. } => {}
        }
        self.handling(
            store,
            object,
            &after,
            handled_before,
            handled_after,
            own_input,
            at,
        );
        if confirmed {
            self.confirm.remove(object);
        }
        if let ObjectKey::Host { name } = object {
            self.host_changed(store, name, &before, &after, at, log);
        }
    }

    /// Changes query answers found that no event announced. During a load
    /// (`defer`) they wait until it is complete (its last tier loads the
    /// problems' output); otherwise they are judged now, after what a load
    /// found about the same object before. `at`: Icinga's time as far as
    /// it is known.
    pub(super) fn discovered(
        &mut self,
        store: &Store,
        found: Vec<Discovered>,
        defer: bool,
        at: Timestamp,
        log: &mut Vec<LogEntry>,
    ) {
        for change in found {
            if defer {
                self.discovered.push(change, at);
            } else {
                self.flush_deferred(store, &change.object, log);
                self.discovered_one(store, change, at, log);
            }
        }
    }

    /// The load is over (complete, failed or cut off): judges what it
    /// found.
    pub(super) fn load_finished(&mut self, store: &Store, log: &mut Vec<LogEntry>) {
        for (change, at) in self.discovered.take_all() {
            self.discovered_one(store, change, at, log);
        }
    }

    /// Judges what the load in flight found about `object` now: an input
    /// about it is about to follow, and the rule engine needs them in
    /// order (a finding judged after a newer event would carry a stale
    /// `handled`, or a state the object already left).
    fn flush_deferred(&mut self, store: &Store, object: &ObjectKey, log: &mut Vec<LogEntry>) {
        for (change, at) in self.discovered.take_object(object) {
            self.discovered_one(store, change, at, log);
        }
    }

    fn discovered_one(
        &mut self,
        store: &Store,
        change: Discovered,
        at: Timestamp,
        log: &mut Vec<LogEntry>,
    ) {
        let Discovered {
            object,
            before,
            after,
        } = change;
        if store.is_excluded(&object) {
            return;
        }
        let Some(after) = after else {
            // Gone: the rule engine forgets it (a change to pending never
            // notifies).
            self.confirm.remove(&object);
            let pending = match before.state {
                CheckableState::Host(_) => CheckableState::Host(HostState::Pending),
                CheckableState::Service(_) => CheckableState::Service(ServiceState::Pending),
            };
            self.push(
                store,
                &object,
                Change::State {
                    previous: Some(before.state),
                    current: pending,
                    state_type: StateType::Hard,
                    since: at,
                    output: String::new(),
                },
                false,
                at,
            );
            return;
        };
        let host_problem = host_problem_of(store, &object);
        let handled_before = handled(&before, host_problem);
        let handled_after = handled(&after, host_problem);
        let when = after.since.non_zero().unwrap_or(at);
        let mut own_input = false;
        let began_again = began_again(&before, &after);
        let state_input = began_again || state_changed(&before, &after);
        if state_input {
            // The stored output, unless an event moved the object on since
            // the answer (a finding judged early).
            let current = store
                .view_of(&object)
                .is_some_and(|view| view.state == after.state);
            let output = if current {
                store.current_output(&object).unwrap_or_default().to_owned()
            } else {
                String::new()
            };
            // A problem that began again ended unseen in between: the rule
            // engine judges a new problem (the missed recovery itself
            // isn't announced).
            let previous = if began_again {
                recovered(after.state)
            } else {
                before.state
            };
            self.state_change(store, &object, previous, &after, &output, when, log);
            own_input = true;
        }
        if before.flapping != after.flapping {
            self.flapping(store, &object, after.flapping, handled_after, at, log, None);
            own_input = true;
        }
        self.handling(
            store,
            &object,
            &after,
            handled_before,
            handled_after,
            own_input,
            at,
        );
        if state_input {
            self.confirm.remove(&object);
        }
        if let ObjectKey::Host { name } = &object {
            self.host_changed(store, name, &before, &after, at, log);
        }
    }

    /// A state change from `previous` (or a soft state turning hard).
    #[expect(clippy::too_many_arguments, reason = "the parts of one change")]
    fn state_change(
        &mut self,
        store: &Store,
        object: &ObjectKey,
        previous: CheckableState,
        after: &ObjectView,
        output: &str,
        at: Timestamp,
        log: &mut Vec<LogEntry>,
    ) {
        self.confirm.remove(object);
        let host_problem = host_problem_of(store, object);
        self.push(
            store,
            object,
            Change::State {
                previous: Some(previous),
                current: after.state,
                state_type: after.state_type,
                since: after.since,
                output: output.to_owned(),
            },
            handled(after, host_problem),
            at,
        );
        // A new state began at `since`; a soft state turning hard did so
        // with this check.
        let when = if previous == after.state {
            at
        } else {
            after.since.non_zero().unwrap_or(at)
        };
        log.push(entry_of(
            object,
            LogKind::State {
                state: after.state,
                state_type: after.state_type,
            },
            when,
            first_line(output),
            None,
        ));
    }

    /// Flapping started or stopped.
    #[expect(clippy::too_many_arguments, reason = "the parts of one change")]
    fn flapping(
        &mut self,
        store: &Store,
        object: &ObjectKey,
        flapping: bool,
        handled: bool,
        at: Timestamp,
        log: &mut Vec<LogEntry>,
        current: Option<f64>,
    ) {
        let (change, kind) = if flapping {
            (Change::FlappingStarted, LogKind::FlappingStarted)
        } else {
            (Change::FlappingStopped, LogKind::FlappingStopped)
        };
        self.push(store, object, change, handled, at);
        let text = current
            .map(|percent| format!("{percent:.1} % state change"))
            .unwrap_or_default();
        log.push(entry_of(object, kind, at, &text, None));
    }

    /// `handled` changed with this change: without an input of its own,
    /// the state is repeated with the new value; once it is false, the
    /// next check result repeats it again.
    #[expect(clippy::too_many_arguments, reason = "the parts of one change")]
    fn handling(
        &mut self,
        store: &Store,
        object: &ObjectKey,
        after: &ObjectView,
        handled_before: bool,
        handled_after: bool,
        own_input: bool,
        at: Timestamp,
    ) {
        if handled_before == handled_after {
            return;
        }
        if !own_input {
            self.repeat(store, object, after, handled_after, at);
        }
        if handled_after {
            self.confirm.remove(object);
        } else {
            self.await_confirmation(object);
        }
    }

    /// A host went down or came back (or a query found that): its problem
    /// services' `handled` changed without an event of their own (after
    /// what a load found about them).
    fn host_changed(
        &mut self,
        store: &Store,
        host: &HostName,
        before: &ObjectView,
        after: &ObjectView,
        at: Timestamp,
        log: &mut Vec<LogEntry>,
    ) {
        let was_problem = before.state.is_problem();
        let is_problem = after.state.is_problem();
        if was_problem == is_problem {
            return;
        }
        for (service, view) in store.problem_services_of(host) {
            self.flush_deferred(store, &service, log);
            let handled_before = handled(&view, was_problem);
            let handled_after = handled(&view, is_problem);
            self.handling(
                store,
                &service,
                &view,
                handled_before,
                handled_after,
                false,
                at,
            );
        }
    }

    fn await_confirmation(&mut self, object: &ObjectKey) {
        if self.confirm.len() >= MAX_CONFIRM {
            tracing::debug!("too many objects await a confirming check; starting over");
            self.confirm.clear();
        }
        self.confirm.insert(object.clone());
    }

    /// The object's current state again (same state and `since`), with
    /// `handled`.
    fn repeat(
        &mut self,
        store: &Store,
        object: &ObjectKey,
        view: &ObjectView,
        handled: bool,
        at: Timestamp,
    ) {
        let output = store.current_output(object).unwrap_or_default().to_owned();
        self.push(
            store,
            object,
            Change::State {
                previous: Some(view.state),
                current: view.state,
                state_type: view.state_type,
                since: view.since,
                output,
            },
            handled,
            at,
        );
    }

    /// Queues an input; memberships follow with the snapshot.
    fn push(
        &mut self,
        store: &Store,
        object: &ObjectKey,
        change: Change,
        handled: bool,
        at: Timestamp,
    ) {
        // A heartbeat is no problem of anyone's: its trouble alerts say
        // what it means.
        if store.is_excluded(object) {
            return;
        }
        let (host_display, service_display) = store.display_names(object);
        self.pending.push(RuleInput {
            object: object.clone(),
            host_display,
            service_display,
            change,
            handled,
            memberships: Vec::new(),
            at,
        });
    }
}

impl Engine {
    /// The rule engine's tick (every `Tuning::rule_tick`, one second):
    /// delayed notifications that are due, storm summaries, the end of a
    /// pause.
    pub(super) fn tick(&mut self, now: tokio::time::Instant) {
        self.tick_at = now + self.tuning.rule_tick;
        self.alive(now);
        let (intents, pause_ended) = self
            .notify
            .tick(self.ports.clock.now(), self.ports.clock.local());
        if pause_ended {
            tracing::info!("notifications resumed");
            self.emit(CoreEvent::NotificationsPaused(None));
        }
        self.deliver(intents);
    }

    /// Prunes the event log to the retention (every
    /// `Tuning::prune_interval`, one hour, and at start).
    pub(super) fn prune(&mut self, now: tokio::time::Instant) {
        self.prune_at = now + self.tuning.prune_interval;
        let hours = self
            .spec
            .general
            .event_log_retention_hours
            .max(ic_config::MIN_EVENT_LOG_RETENTION_HOURS);
        let before = Timestamp::from_unix_seconds(
            self.ports.clock.now().as_unix_seconds() - f64::from(hours) * 3_600.0,
        );
        self.event_log.prune(before);
        self.prune_recent_events(before);
    }

    /// Judges the queued rule inputs with the dashboards' memberships:
    /// those of the evaluated snapshot (`evaluated`), or all of them.
    pub(super) fn judge(&mut self, evaluated: bool) {
        let Some(dashboards) = self.dashboards.as_deref() else {
            return;
        };
        let intents = self.notify.judge(
            dashboards,
            evaluated,
            self.ports.clock.now(),
            self.ports.clock.local(),
        );
        self.deliver(intents);
        let asked = self.notify.take_lookups();
        if !asked.is_empty() {
            let tx = self.internal_tx.clone();
            self.event_log.known(
                asked.clone(),
                Box::new(move |known| {
                    let _ = tx.send(Internal::NotifiedBefore { asked, known });
                }),
            );
        }
    }

    /// The event log answered which seeded problems an earlier run
    /// notified.
    pub(super) fn on_notified_before(&mut self, asked: &[String], known: &[String]) {
        if !known.is_empty() {
            tracing::debug!(
                count = known.len(),
                "problems open at the start were notified before"
            );
        }
        self.notify.restore(asked, known, self.ports.clock.now());
    }

    /// Logs intents; they are emitted once logged
    /// ([`Engine::on_logged`]).
    pub(super) fn deliver(&mut self, intents: Vec<NotificationIntent>) {
        if intents.is_empty() {
            return;
        }
        let tx = self.internal_tx.clone();
        self.event_log.log_notifications(
            intents,
            Box::new(move |logged| {
                let _ = tx.send(Internal::Logged(logged));
            }),
        );
    }

    /// Logged: every intent goes to the UI, the audible ones also to the
    /// OS.
    pub(super) fn on_logged(&mut self, intents: Vec<NotificationIntent>) {
        for intent in intents {
            tracing::debug!(id = %intent.id, title = %intent.title, silent = intent.silent, "notification");
            if !intent.silent {
                self.ports.notifier.notify(&intent);
                // Its pane should be complete when it is clicked.
                if let Some(object) = &intent.object {
                    self.prefetch(object);
                }
            }
            self.emit(CoreEvent::Notification(NotificationRecord {
                intent,
                read: false,
            }));
        }
    }

    // --- Icinga's own `Notification` objects --------------------------------------

    /// Loads every `Notification` object (in the background, after a
    /// load's problem lists), unless the user may not or it runs.
    pub(super) fn load_notifications(&mut self) {
        let Some(conn) = &mut self.conn else {
            return;
        };
        if !conn.notifications_allowed || conn.notifications_in_flight {
            return;
        }
        conn.notifications_in_flight = true;
        let client = conn.client.clone();
        let seq = Arc::clone(&self.seq);
        let tx = self.internal_tx.clone();
        let session = self.session;
        self.tasks.spawn(async move {
            let started = seq.load(Ordering::SeqCst);
            let result = client.notifications().await;
            let _ = tx.send(Internal::IcingaNotifications {
                session,
                started,
                result,
            });
        });
    }

    pub(super) fn on_icinga_notifications(
        &mut self,
        started: u64,
        result: Result<Vec<Notification>, ApiError>,
    ) {
        let Some(conn) = &mut self.conn else {
            return;
        };
        conn.notifications_in_flight = false;
        let waiting = mem::take(&mut conn.notification_events_waiting);
        match result {
            Ok(list) => {
                tracing::debug!(count = list.len(), "loaded Icinga's notifications");
                self.notifications_current = conn.notification_events;
                self.store.replace_notifications(list, started);
                // Events the list doesn't reflect yet.
                let now = tokio::time::Instant::now();
                for (seq, object) in waiting {
                    self.on_icinga_notification(seq, &object, now);
                }
                self.publish_changes();
            }
            Err(ApiError::Unauthorized) => {
                self.fail(Failure::Auth(ApiError::Unauthorized.to_string()));
            }
            Err(error) => self.notifications_refused(&error),
        }
    }

    /// A query for `Notification` objects failed: without permission
    /// (Icinga said so, though `GET /v1` allowed it), the engine stops
    /// asking for this session; after other errors the store's list may be
    /// behind, so the next load (a periodic reconcile too) reloads it.
    pub(super) fn notifications_refused(&mut self, error: &ApiError) {
        match error {
            ApiError::Forbidden(message) | ApiError::NotFound(message) => {
                tracing::warn!(%message, "the API user may not read Icinga's notifications; leaving them out");
                if let Some(conn) = &mut self.conn {
                    conn.notifications_allowed = false;
                }
            }
            error => {
                tracing::warn!(%error, "couldn't load Icinga's notifications; the next reload brings them");
                self.notifications_current = false;
            }
        }
    }

    /// Icinga sent one of its notifications about `object` (event `seq`):
    /// its `Notification` objects are re-read by name, unless the list in
    /// the store already reflects the event.
    pub(super) fn on_icinga_notification(
        &mut self,
        seq: u64,
        object: &ObjectKey,
        now: tokio::time::Instant,
    ) {
        let Some(conn) = &mut self.conn else {
            return;
        };
        if !conn.notifications_allowed {
            return;
        }
        if conn.notifications_in_flight {
            conn.notification_events_waiting.push((seq, object.clone()));
            return;
        }
        if seq <= self.store.notifications_listed() {
            return;
        }
        let names = self.store.notification_names(object);
        if names.is_empty() {
            tracing::debug!(%object, "Icinga notified about an object without known notifications");
            return;
        }
        self.fetch.mark_notifications(names, now);
    }
}

/// The rule set of an environment: its name and settings, and the group →
/// dashboard tree with each level's setting, in sidebar order.
pub(super) fn rule_set(environment: &ic_config::Environment) -> RuleSet {
    RuleSet {
        environment_name: environment.name.clone(),
        settings: environment.notifications.clone(),
        groups: environment
            .groups
            .iter()
            .map(|group| GroupScope {
                id: group.id.clone(),
                name: group.name.clone(),
                setting: group.notifications.clone(),
                dashboards: group
                    .dashboards
                    .iter()
                    .map(|dashboard| DashboardScope {
                        id: dashboard.id.clone(),
                        name: dashboard.name.clone(),
                        setting: dashboard.notifications.clone(),
                    })
                    .collect(),
            })
            .collect(),
    }
}

/// Whether a host's or service's problem is handled for the rule engine:
/// acknowledged, in downtime, unreachable through a dependency, or (for a
/// service) on a host with a problem. Only problems are handled.
pub(super) fn handled(view: &ObjectView, host_problem: bool) -> bool {
    view.state.is_problem()
        && (view.acknowledged || view.downtime_depth > 0 || !view.reachable || host_problem)
}

/// For a service, whether its host has a problem; `false` for hosts.
fn host_problem_of(store: &Store, object: &ObjectKey) -> bool {
    match object {
        ObjectKey::Host { .. } => false,
        ObjectKey::Service { key } => store.host_problem(&key.host),
    }
}

/// Whether the state, or its type, changed.
fn state_changed(before: &ObjectView, after: &ObjectView) -> bool {
    before.state != after.state || before.state_type != after.state_type
}

/// Whether a query found an object in the same problem state as before,
/// but begun later (`last_state_change` moved on by more than a
/// millisecond): it left the state and came back while no event told,
/// most likely through a recovery (a laptop asleep overnight while a
/// paged problem recovered and failed again). It is a new problem.
fn began_again(before: &ObjectView, after: &ObjectView) -> bool {
    after.state.is_problem()
        && before.state == after.state
        && before.since.non_zero().is_some()
        && after.since.as_unix_seconds() - before.since.as_unix_seconds() > 0.001
}

/// The state a problem of `state`'s kind recovers to: OK or UP.
fn recovered(state: CheckableState) -> CheckableState {
    match state {
        CheckableState::Host(_) => CheckableState::Host(HostState::Up),
        CheckableState::Service(_) => CheckableState::Service(ServiceState::Ok),
    }
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or_default()
}

fn entry_of(
    object: &ObjectKey,
    kind: LogKind,
    at: Timestamp,
    text: &str,
    author: Option<&String>,
) -> LogEntry {
    LogEntry {
        at,
        object: object.clone(),
        kind,
        text: text.to_owned(),
        author: author.filter(|author| !author.is_empty()).cloned(),
    }
}

#[cfg(test)]
#[path = "notify_tests.rs"]
mod tests;
