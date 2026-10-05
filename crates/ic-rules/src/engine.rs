//! The rule engine: turns [`RuleInput`]s into [`NotificationIntent`]s.

use std::collections::HashMap;
use std::time::Duration;

use ic_model::{CheckableState, HostState, ObjectKey, ServiceState, StateType, Timestamp};
use tracing::debug;

use crate::dedupe::Dedupe;
use crate::intent::{Change, DashboardRef, LocalTime, NotificationIntent, RuleInput, Tone};
use crate::quiet;
use crate::scope::{Candidate, RuleSet, Scopes, Source};
use crate::settings::{ObjectMode, Rule};
use crate::storm::{Admission, Storm};
use crate::text::{self, Label};

/// How many emitted intent ids are remembered for deduplication.
const DEDUPE_CAPACITY: usize = 10_000;
/// How many objects in a problem state are tracked at most. Far above what
/// a 20 000-service environment produces even in a full outage; it only
/// bounds memory if removed objects never report back.
const MAX_TRACKED: usize = 50_000;
/// After a handled problem becomes unhandled again, its notification waits
/// at least this long. Icinga clears a normal acknowledgement *before* it
/// reports the state change that cleared it, so the wait lets that state
/// change cancel a notification for a state that is already gone.
const HANDLING_GRACE: Duration = Duration::from_secs(10);

/// Memory bounds; tests shrink them.
#[derive(Clone, Copy, Debug)]
struct Limits {
    dedupe: usize,
    tracked: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            dedupe: DEDUPE_CAPACITY,
            tracked: MAX_TRACKED,
        }
    }
}

/// The client-side notification rule engine for one environment.
///
/// Feed it every change of the environment's hosts and services with
/// [`RuleEngine::on_input`] and call [`RuleEngine::tick`] about once a
/// second; both return the notifications to record (all of them) and show
/// (the ones that aren't `silent`). The engine is pure: it never reads a
/// clock, and time comes in as arguments.
///
/// # Scopes
///
/// A change is judged by the rules of the scopes its object belongs to:
///
/// 1. The environment: `(settings.enabled, settings.default_rule)`.
/// 2. A group's [`ScopeSetting`](crate::ScopeSetting) applies to that:
///    `Inherit` keeps it, `Off` disables it, `On` enables it with the
///    parent's rule, `Custom(rule)` enables it with `rule`.
/// 3. A dashboard's setting applies the same way to its group's result.
/// 4. With memberships, the change notifies if *any* membership's effective
///    rule is enabled and matches; the subtitle names the first matching
///    `group / dashboard` in sidebar order. Memberships of dashboards that
///    no longer exist are ignored.
/// 5. Without (known) memberships, the environment pair applies and the
///    subtitle is the environment name.
/// 6. Object overrides come first: a `Mute` (until it expires) suppresses
///    everything for the object; a `Watch` adds the environment's default
///    rule, enabled even if every scope (and the environment) is off. A mute
///    wins over a watch. Like a snooze, a mute holds back the object's
///    current problem rather than forgetting it: if the object is still in
///    that state when the mute ends (expires or is removed), the problem
///    notifies then. Recoveries and other events during a mute are dropped.
///
/// # States
///
/// - A problem state (critical, warning, unknown, down, unreachable)
///   notifies if the rule's [`StateFilter`](crate::StateFilter) selects it,
///   it is hard or `hard_only` is off, and it is unhandled or `skip_handled`
///   is off. The intent id is `"{object}:{state}:{since}"`, so a soft state
///   turning hard (same `since`) never notifies twice.
/// - `min_duration_secs` holds the notification until `since +
///   min_duration`; [`RuleEngine::tick`] releases it if the object is still
///   in that state and the rule still matches. A recovery, another state
///   change, or a handling change (acknowledgement, downtime) cancels it.
///   With several matching rules the earliest one notifies. If `since` lies
///   in the engine's future (Icinga's clock runs ahead), the delay counts
///   from when the engine first saw the state instead.
/// - A recovery (back to OK / UP) notifies only if a problem of the object
///   was notified since it last left OK / UP, including silent
///   notifications (quiet hours, pause, storm). It is judged by the scopes
///   the object is on now *and* the scopes that notified the problem, so an
///   object whose dashboard filter depends on its state still gets its
///   recovery. Recoveries are never delayed.
/// - Like Icinga's own notifications, a problem that didn't notify only
///   because it was handled notifies once it is no longer handled
///   (acknowledgement removed or expired, downtime ended) and still in the
///   same state, after a short grace period that lets an immediately
///   following state change cancel it.
/// - A change to pending forgets the object without notifying (it was
///   removed or re-created).
///
/// Inputs for one object must arrive in order. A state change older than
/// the state the engine already knows (by `since`) is ignored, unless the
/// known state lies in the future (Icinga's clock was ahead).
///
/// # Other events
///
/// Acknowledgements, downtimes and flapping notify when the matching
/// scope's [`EventFilter`](crate::EventFilter) enables them. `hard_only`,
/// `skip_handled` and delays don't apply to them. Their id is
/// `"{object}:{kind}:{at}"`.
///
/// # Output
///
/// - The same id is never emitted twice (across memberships, repeated
///   inputs and reconnect replays). The last 10 000 ids are remembered.
/// - Storm control: when more than `storm.threshold` audible notifications
///   fall inside `storm.window_secs` (counted from the first one), further
///   ones in that window are `silent`. When the window closes, one summary
///   follows (`"14 new problems in prod-cluster"`, `object = None`, tone
///   `Info`) with a breakdown in the body. Notifications that are silent
///   anyway (quiet hours, pause) don't count. `window_secs = 0` disables
///   storm control.
/// - Quiet hours (local time; see [`QuietHours`](crate::QuietHours)) make
///   notifications `silent`; with `allow_critical`, critical and down stay
///   audible (and so does a storm summary that silenced one of them).
/// - While paused, every notification is `silent`.
/// - Title `"{LABEL} · {service} on {host}"` or `"{LABEL} · {host}"` with
///   display names; body = the first line of the output, or `"{author}:
///   {comment}"`; tone by state; sound from the deciding rule; `at` = when
///   the change happened.
#[derive(Debug)]
pub struct RuleEngine {
    scopes: Scopes,
    /// Bumped by `set_rules`; waiting notifications computed under an older
    /// generation are re-judged on the next tick.
    generation: u64,
    paused_until: Option<Timestamp>,
    /// Objects in a problem state the engine has seen a change for.
    tracked: HashMap<ObjectKey, Tracked>,
    dedupe: Dedupe,
    storm: Storm,
    limits: Limits,
}

impl RuleEngine {
    /// An engine for `rules`, with nothing seen yet.
    pub fn new(rules: RuleSet) -> Self {
        Self::with_limits(rules, Limits::default())
    }

    fn with_limits(rules: RuleSet, limits: Limits) -> Self {
        Self {
            scopes: Scopes::new(rules),
            generation: 0,
            paused_until: None,
            tracked: HashMap::new(),
            dedupe: Dedupe::new(limits.dedupe),
            storm: Storm::default(),
            limits,
        }
    }

    /// The rules in effect.
    pub fn rules(&self) -> &RuleSet {
        self.scopes.rules()
    }

    /// Replaces the rules (settings, groups, dashboards, overrides). What
    /// the engine remembers (notified problems, ids, the storm window,
    /// pause) is kept. Waiting notifications are re-judged by the new rules
    /// on the next [`RuleEngine::tick`], and so are problems held back by a
    /// mute the new rules no longer have.
    pub fn set_rules(&mut self, rules: RuleSet) {
        self.scopes = Scopes::new(rules);
        self.generation = self.generation.wrapping_add(1);
    }

    /// Pauses notifications until `until` (`None` resumes). While paused,
    /// intents are still produced, but `silent`.
    pub fn pause_until(&mut self, until: Option<Timestamp>) {
        self.paused_until = until;
    }

    /// When the pause ends; `None` if not paused. An expired pause reads as
    /// `None` after the next `tick` or `on_input`.
    pub fn paused_until(&self) -> Option<Timestamp> {
        self.paused_until
    }

    /// Judges one change. `now` is the current time and `local` the local
    /// wall-clock time, for quiet hours. Returns the intents in the order
    /// they happened: a storm summary that was due, a notification whose
    /// delay or mute ran out before this change, then this change's own.
    ///
    /// Callers provide:
    ///
    /// - one input per change, in order for each object;
    /// - `since` = `last_state_change`, which a soft state turning hard
    ///   doesn't change (so the hard state keeps the soft state's id);
    /// - `handled` and `memberships` as they are *after* the change, with
    ///   memberships ignoring `problems_only` and `hide_handled`;
    /// - when an object's `handled` changes without an event of its own
    ///   (the services of a host that went down or came back), a repeat of
    ///   its current state change (same `current` and `since`) with the new
    ///   `handled`: the engine re-judges a repeated state, so a problem
    ///   that was skipped as handled notifies once it no longer is.
    #[must_use = "the intents must be recorded, and the audible ones shown"]
    pub fn on_input(
        &mut self,
        input: RuleInput,
        now: Timestamp,
        local: LocalTime,
    ) -> Vec<NotificationIntent> {
        let moment = Moment { now, local };
        let mut out = Vec::new();
        self.advance(moment, &mut out);
        // A delay (or mute) that ran out before this change happened
        // notifies first: the problem did last that long.
        let cutoff = earliest(input.at, now);
        if self
            .tracked
            .get(&input.object)
            .and_then(|tracked| self.ready_at(&input.object, tracked, cutoff))
            .is_some()
        {
            self.evaluate(&input.object, moment, cutoff, false, &mut out);
        }
        if matches!(input.change, Change::State { .. }) {
            self.apply_state(input, moment, &mut out);
        } else {
            self.apply_event(input, moment, &mut out);
        }
        out
    }

    /// Advances time: ends an expired pause, closes a finished storm window
    /// (emitting its summary), releases delayed notifications that are due,
    /// and notifies problems whose mute ended while they were still open.
    /// Call it about once a second.
    #[must_use = "the intents must be recorded, and the audible ones shown"]
    pub fn tick(&mut self, now: Timestamp, local: LocalTime) -> Vec<NotificationIntent> {
        let moment = Moment { now, local };
        let mut out = Vec::new();
        self.advance(moment, &mut out);
        let mut ready: Vec<(Timestamp, ObjectKey)> = self
            .tracked
            .iter()
            .filter_map(|(object, tracked)| {
                self.ready_at(object, tracked, now)
                    .map(|ready| (ready, object.clone()))
            })
            .collect();
        ready.sort_by(|(due_a, object_a), (due_b, object_b)| {
            due_a
                .as_unix_seconds()
                .total_cmp(&due_b.as_unix_seconds())
                .then_with(|| object_a.cmp(object_b))
        });
        for (_, object) in ready {
            self.evaluate(&object, moment, now, false, &mut out);
        }
        out
    }

    /// Whether a tracked problem must be judged again by `cutoff`, and the
    /// time that orders it among others: its delay ran out, the rules
    /// changed while it waited, or its mute ended.
    fn ready_at(
        &self,
        object: &ObjectKey,
        tracked: &Tracked,
        cutoff: Timestamp,
    ) -> Option<Timestamp> {
        match tracked.status {
            Status::Waiting {
                due, generation, ..
            } => (due <= cutoff || generation != self.generation).then_some(due),
            Status::Muted => (self.scopes.rules().object_mode(object, cutoff)
                != Some(ObjectMode::Mute))
            .then_some(cutoff),
            Status::Idle | Status::Suppressed | Status::Notified => None,
        }
    }

    /// Ends an expired pause and closes a finished storm window.
    fn advance(&mut self, moment: Moment, out: &mut Vec<NotificationIntent>) {
        if self.paused_until.is_some_and(|until| until <= moment.now) {
            self.paused_until = None;
        }
        let Some(summary) = self.storm.close_if_over(moment.now) else {
            return;
        };
        let environment = &self.scopes.rules().environment_name;
        let intent = NotificationIntent {
            id: text::storm_id(summary.started),
            object: None,
            title: summary.title(environment),
            subtitle: environment.clone(),
            body: summary.body(),
            tone: Tone::Info,
            sound: summary.sound,
            silent: self.is_silenced(moment, summary.critical),
            at: moment.now,
        };
        debug!(id = %intent.id, title = %intent.title, "storm summary");
        self.dedupe.insert(&intent.id);
        out.push(intent);
    }

    /// Applies a state change.
    fn apply_state(&mut self, input: RuleInput, moment: Moment, out: &mut Vec<NotificationIntent>) {
        let RuleInput {
            object,
            host_display,
            service_display,
            change,
            handled,
            memberships,
            at,
        } = input;
        let Change::State {
            current,
            state_type,
            since,
            output,
            ..
        } = change
        else {
            return;
        };
        let observation = Observation {
            subject: Subject {
                object,
                host_display,
                service_display,
            },
            state: current,
            state_type,
            // "Never" would give every occurrence of the state one id.
            since: since.non_zero().unwrap_or(at),
            at,
            output,
            handled,
            memberships,
        };
        let object = observation.subject.object.clone();

        if let Some(tracked) = self.tracked.get_mut(&object) {
            let known = tracked.last.since;
            if observation.since < known && known <= moment.now {
                debug!(%object, "ignoring a state change older than the known state");
                return;
            }
            if tracked.last.state == observation.state && known == observation.since {
                // The same state again: soft turned hard, or a repeat.
                tracked.last.update(observation);
                self.evaluate(&object, moment, moment.now, false, out);
                return;
            }
        }

        let previous = self.tracked.remove(&object);
        if previous
            .as_ref()
            .is_some_and(|tracked| matches!(tracked.status, Status::Waiting { .. }))
        {
            debug!(%object, "delayed notification cancelled: the state changed");
        }
        let notified_by = previous
            .map(|tracked| tracked.notified_by)
            .unwrap_or_default();

        if Label::for_problem(observation.state).is_some() {
            // A new problem state; the problem episode (and whether it
            // notified) carries over from the previous problem state.
            self.track(Tracked {
                last: observation,
                first_seen: moment.now,
                status: Status::Idle,
                notified_by,
            });
            self.evaluate(&object, moment, moment.now, false, out);
        } else if is_up(observation.state) && !notified_by.is_empty() {
            self.recover(&observation, &notified_by, moment, out);
        }
        // Pending: the object has no state any more (removed or
        // re-created); it is forgotten without a notification.
    }

    /// Notifies a recovery, if a scope wants it.
    fn recover(
        &mut self,
        observation: &Observation,
        notified_by: &[Source],
        moment: Moment,
        out: &mut Vec<NotificationIntent>,
    ) {
        let object = &observation.subject.object;
        let mode = self.scopes.rules().object_mode(object, moment.now);
        if mode == Some(ObjectMode::Mute) {
            debug!(%object, "recovery not notified: muted");
            return;
        }
        let draft = self
            .scopes
            .candidates(
                &observation.memberships,
                notified_by,
                mode == Some(ObjectMode::Watch),
            )
            .iter()
            .find(|candidate| candidate.enabled && candidate.rule.states.recovery)
            .map(|candidate| Draft {
                id: text::state_id(object, observation.state, observation.since),
                object: object.clone(),
                label: Label::Recovered,
                title: observation.subject.title(Label::Recovered),
                subtitle: candidate.subtitle.to_owned(),
                body: text::output_body(&observation.output),
                sound: candidate.rule.sound,
                at: observation.at,
            });
        if let Some(draft) = draft {
            self.emit(draft, moment, out);
        }
    }

    /// Applies an acknowledgement, downtime or flapping change.
    fn apply_event(&mut self, input: RuleInput, moment: Moment, out: &mut Vec<NotificationIntent>) {
        let RuleInput {
            object,
            host_display,
            service_display,
            change,
            handled,
            memberships,
            at,
        } = input;
        let (label, body) = match change {
            Change::State { .. } => return,
            Change::AcknowledgementSet { author, comment } => {
                (Label::Acknowledged, text::comment_body(&author, &comment))
            }
            Change::AcknowledgementCleared => (Label::AckCleared, String::new()),
            Change::DowntimeStarted { author, comment } => {
                (Label::Downtime, text::comment_body(&author, &comment))
            }
            Change::DowntimeEnded => (Label::DowntimeEnded, String::new()),
            Change::FlappingStarted => (Label::Flapping, String::new()),
            Change::FlappingStopped => (Label::FlappingStopped, String::new()),
        };
        let subject = Subject {
            object,
            host_display,
            service_display,
        };

        let mode = self.scopes.rules().object_mode(&subject.object, moment.now);
        if mode == Some(ObjectMode::Mute) {
            debug!(object = %subject.object, "event not notified: muted");
        } else {
            let draft = self
                .scopes
                .candidates(&memberships, &[], mode == Some(ObjectMode::Watch))
                .iter()
                .find(|candidate| candidate.enabled && label.event_enabled(candidate.rule.events))
                .map(|candidate| Draft {
                    id: text::event_id(&subject.object, label, at),
                    object: subject.object.clone(),
                    label,
                    title: subject.title(label),
                    subtitle: candidate.subtitle.to_owned(),
                    body,
                    sound: candidate.rule.sound,
                    at,
                });
            if let Some(draft) = draft {
                self.emit(draft, moment, out);
            }
        }

        // The event may start or end the handling of a known problem.
        let object = subject.object.clone();
        let Some(tracked) = self.tracked.get_mut(&object) else {
            return;
        };
        tracked.last.subject = subject;
        tracked.last.handled = handled;
        tracked.last.memberships = memberships;
        let rearm = match tracked.status {
            Status::Waiting { .. } => false,
            Status::Suppressed if !handled => true,
            Status::Suppressed | Status::Muted | Status::Idle | Status::Notified => return,
        };
        self.evaluate(&object, moment, moment.now, rearm, out);
    }

    /// Decides what the object's problem state does now (notify, wait for
    /// its delay, or nothing) and records the outcome. Delays count as run
    /// out if due by `cutoff`. `rearm`: the problem just stopped being
    /// handled, so it waits at least the grace period.
    fn evaluate(
        &mut self,
        object: &ObjectKey,
        moment: Moment,
        cutoff: Timestamp,
        rearm: bool,
        out: &mut Vec<NotificationIntent>,
    ) {
        let rules = self.scopes.rules();
        let muted = rules.object_mode(object, moment.now) == Some(ObjectMode::Mute);
        // Judged with the watch even while a mute wins over it, so a watched
        // problem is held back (not dropped) until the mute ends.
        let watched = rules.is_watched(object, moment.now);
        let Some(tracked) = self.tracked.get(object) else {
            return;
        };
        if tracked.status == Status::Notified {
            return;
        }
        let was_waiting = matches!(tracked.status, Status::Waiting { .. });
        let mut not_before = match tracked.status {
            Status::Waiting { not_before, .. } => not_before,
            Status::Muted | Status::Idle | Status::Suppressed | Status::Notified => {
                Timestamp::EPOCH
            }
        };
        if rearm {
            not_before = latest(not_before, moment.now.plus(HANDLING_GRACE));
        }
        let decision = decide(&self.scopes, tracked, watched, cutoff, not_before);

        let status = match decision {
            Decision::Notify { .. } | Decision::Wait { .. } if muted => {
                debug!(%object, "problem held back: muted");
                Status::Muted
            }
            Decision::Notify { draft, sources } => {
                self.emit(draft, moment, out);
                if let Some(tracked) = self.tracked.get_mut(object) {
                    for source in sources {
                        if !tracked.notified_by.contains(&source) {
                            tracked.notified_by.push(source);
                        }
                    }
                }
                Status::Notified
            }
            Decision::Wait { due } => {
                if !was_waiting {
                    debug!(%object, due = due.as_unix_seconds(), "notification delayed");
                }
                Status::Waiting {
                    due,
                    not_before,
                    generation: self.generation,
                }
            }
            Decision::Suppressed => Status::Suppressed,
            Decision::Idle => Status::Idle,
        };
        if was_waiting && matches!(status, Status::Idle | Status::Suppressed) {
            debug!(%object, "delayed notification cancelled");
        }
        if let Some(tracked) = self.tracked.get_mut(object) {
            tracked.status = status;
        }
    }

    /// Records `draft` unless it was emitted before, deciding whether it is
    /// silent: paused, quiet hours, or absorbed by a storm.
    fn emit(&mut self, draft: Draft, moment: Moment, out: &mut Vec<NotificationIntent>) {
        if self.dedupe.contains(&draft.id) {
            debug!(id = %draft.id, "already notified");
            return;
        }
        let storm = self.scopes.rules().settings.storm;
        let silent = self.is_silenced(moment, draft.label.is_critical())
            || self
                .storm
                .admit(moment.now, storm, draft.label, draft.sound)
                == Admission::Absorbed;
        debug!(id = %draft.id, silent, "notification");
        self.dedupe.insert(&draft.id);
        out.push(NotificationIntent {
            id: draft.id,
            object: Some(draft.object),
            title: draft.title,
            subtitle: draft.subtitle,
            body: draft.body,
            tone: draft.label.tone(),
            sound: draft.sound,
            silent,
            at: draft.at,
        });
    }

    /// Whether notifications are silent at `moment` because of a pause or
    /// quiet hours. `critical`: critical or down, which quiet hours with
    /// `allow_critical` keep audible.
    fn is_silenced(&self, moment: Moment, critical: bool) -> bool {
        let paused = self.paused_until.is_some_and(|until| moment.now < until);
        let quiet_hours = &self.scopes.rules().settings.quiet_hours;
        paused
            || (quiet::is_quiet(quiet_hours, moment.local)
                && !(quiet_hours.allow_critical && critical))
    }

    /// Starts tracking an object's problem, keeping memory bounded.
    fn track(&mut self, tracked: Tracked) {
        let object = tracked.last.subject.object.clone();
        self.tracked.insert(object.clone(), tracked);
        if self.tracked.len() > self.limits.tracked {
            self.evict(&object);
        }
    }

    /// Forgets the objects whose state changed longest ago (never `keep`),
    /// down to seven eighths of the limit, so this runs rarely.
    fn evict(&mut self, keep: &ObjectKey) {
        let target = self.limits.tracked - self.limits.tracked / 8;
        let excess = self.tracked.len().saturating_sub(target);
        let mut by_age: Vec<(Timestamp, ObjectKey)> = self
            .tracked
            .iter()
            .filter(|(object, _)| *object != keep)
            .map(|(object, tracked)| (tracked.last.at, object.clone()))
            .collect();
        by_age.sort_by(|(at_a, object_a), (at_b, object_b)| {
            at_a.as_unix_seconds()
                .total_cmp(&at_b.as_unix_seconds())
                .then_with(|| object_a.cmp(object_b))
        });
        for (_, object) in by_age.into_iter().take(excess) {
            self.tracked.remove(&object);
        }
        debug!(excess, "tracked too many problems; forgot the oldest");
    }
}

/// Engine time plus local wall-clock facts.
#[derive(Clone, Copy, Debug)]
struct Moment {
    now: Timestamp,
    local: LocalTime,
}

/// Who a notification is about.
#[derive(Clone, Debug)]
struct Subject {
    object: ObjectKey,
    host_display: String,
    service_display: Option<String>,
}

impl Subject {
    fn title(&self, label: Label) -> String {
        text::title(
            label,
            &self.object,
            &self.host_display,
            self.service_display.as_deref(),
        )
    }
}

/// What the latest state change said about an object.
#[derive(Clone, Debug)]
struct Observation {
    subject: Subject,
    state: CheckableState,
    state_type: StateType,
    since: Timestamp,
    at: Timestamp,
    output: String,
    handled: bool,
    memberships: Vec<DashboardRef>,
}

impl Observation {
    /// Takes in a repeat of the same state (same `state` and `since`).
    fn update(&mut self, newer: Self) {
        let output = if newer.output.trim().is_empty() {
            std::mem::take(&mut self.output)
        } else {
            newer.output
        };
        *self = Self { output, ..newer };
    }
}

/// An object in a problem state.
#[derive(Clone, Debug)]
struct Tracked {
    /// The latest state change and what later events told about it.
    last: Observation,
    /// When the engine first saw the current state. A state can't begin
    /// after the engine hears of it, so delays count from the earlier of
    /// this and `since`, which keeps a server clock that runs ahead from
    /// holding notifications back.
    first_seen: Timestamp,
    /// What the current problem state's notification is doing.
    status: Status,
    /// Scopes that notified a problem since the object last left OK / UP;
    /// non-empty means its recovery may notify.
    notified_by: Vec<Source>,
}

/// The notification of an object's current problem state.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Status {
    /// Nothing to notify: no enabled rule matches.
    Idle,
    /// A rule would match if the problem weren't handled; notifies once it
    /// isn't.
    Suppressed,
    /// A rule matches but the object is muted; notifies once the mute ends
    /// if the object is still in this state.
    Muted,
    /// Waiting for a delay (`min_duration_secs`, or the grace after
    /// handling ended).
    Waiting {
        /// When it notifies.
        due: Timestamp,
        /// It never notifies before this.
        not_before: Timestamp,
        /// The rules generation `due` was computed under.
        generation: u64,
    },
    /// Notified (or found already notified).
    Notified,
}

/// An intent before pause, quiet hours and storm control had their say.
#[derive(Clone, Debug)]
struct Draft {
    id: String,
    object: ObjectKey,
    label: Label,
    title: String,
    subtitle: String,
    body: String,
    sound: bool,
    at: Timestamp,
}

/// What a problem state's notification should do.
#[derive(Debug)]
enum Decision {
    /// No enabled rule matches.
    Idle,
    /// Only handling keeps a rule from matching.
    Suppressed,
    /// A rule matches, but its delay runs until `due`.
    Wait { due: Timestamp },
    /// Notify now; `sources` are all scopes whose rule matched.
    Notify { draft: Draft, sources: Vec<Source> },
}

/// Judges a tracked problem state by the scopes of its object. Delays count
/// as run out if due by `cutoff`; none runs out before `not_before`.
fn decide(
    scopes: &Scopes,
    tracked: &Tracked,
    watched: bool,
    cutoff: Timestamp,
    not_before: Timestamp,
) -> Decision {
    let last = &tracked.last;
    let Some(label) = Label::for_problem(last.state) else {
        return Decision::Idle;
    };
    let candidates = scopes.candidates(&last.memberships, &[], watched);
    let matching: Vec<&Candidate<'_>> = candidates
        .iter()
        .filter(|candidate| {
            candidate.enabled && matches_problem(candidate.rule, last, last.handled)
        })
        .collect();
    if matching.is_empty() {
        let suppressed = last.handled
            && candidates
                .iter()
                .any(|candidate| candidate.enabled && matches_problem(candidate.rule, last, false));
        return if suppressed {
            Decision::Suppressed
        } else {
            Decision::Idle
        };
    }

    let start = earliest(last.since, tracked.first_seen);
    let due = |candidate: &Candidate<'_>| {
        let delay = Duration::from_secs(u64::from(candidate.rule.min_duration_secs));
        latest(start.plus(delay), not_before)
    };
    if let Some(chosen) = matching.iter().find(|candidate| due(candidate) <= cutoff) {
        let draft = Draft {
            id: text::state_id(&last.subject.object, last.state, last.since),
            object: last.subject.object.clone(),
            label,
            title: last.subject.title(label),
            subtitle: chosen.subtitle.to_owned(),
            body: text::output_body(&last.output),
            sound: chosen.rule.sound,
            at: last.at,
        };
        let sources = matching
            .iter()
            .map(|candidate| candidate.source.to_source())
            .collect();
        return Decision::Notify { draft, sources };
    }
    match matching
        .iter()
        .map(|candidate| due(candidate))
        .reduce(earliest)
    {
        Some(due) => Decision::Wait { due },
        None => Decision::Idle,
    }
}

/// Whether `rule` notifies the observed problem state, taking `handled` as
/// given.
fn matches_problem(rule: &Rule, observation: &Observation, handled: bool) -> bool {
    selects(rule, observation.state)
        && (!rule.hard_only || observation.state_type == StateType::Hard)
        && !(rule.skip_handled && handled)
}

/// Whether the rule's state filter selects a problem state.
fn selects(rule: &Rule, state: CheckableState) -> bool {
    let states = rule.states;
    match state {
        CheckableState::Service(ServiceState::Critical) => states.critical,
        CheckableState::Service(ServiceState::Warning) => states.warning,
        CheckableState::Service(ServiceState::Unknown) => states.unknown,
        CheckableState::Host(HostState::Down) => states.down,
        CheckableState::Host(HostState::Unreachable) => states.unreachable,
        CheckableState::Service(ServiceState::Ok | ServiceState::Pending)
        | CheckableState::Host(HostState::Up | HostState::Pending) => false,
    }
}

/// OK or UP: the states a recovery goes to. Icinga's OK states are always
/// hard, so recoveries ignore `hard_only`.
fn is_up(state: CheckableState) -> bool {
    matches!(
        state,
        CheckableState::Service(ServiceState::Ok) | CheckableState::Host(HostState::Up)
    )
}

fn latest(a: Timestamp, b: Timestamp) -> Timestamp {
    if b > a { b } else { a }
}

fn earliest(a: Timestamp, b: Timestamp) -> Timestamp {
    if b < a { b } else { a }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCAL: LocalTime = LocalTime {
        weekday: 0,
        minute_of_day: 12 * 60,
    };

    fn at(seconds: f64) -> Timestamp {
        Timestamp::from_unix_seconds(seconds)
    }

    fn critical(service: &str, since: f64) -> RuleInput {
        RuleInput {
            object: ObjectKey::service("h", service),
            host_display: "h".to_owned(),
            service_display: Some(service.to_owned()),
            change: Change::State {
                previous: Some(CheckableState::Service(ServiceState::Ok)),
                current: CheckableState::Service(ServiceState::Critical),
                state_type: StateType::Hard,
                since: at(since),
                output: "CRITICAL".to_owned(),
            },
            handled: false,
            memberships: Vec::new(),
            at: at(since),
        }
    }

    fn engine(limits: Limits) -> RuleEngine {
        let mut rules = RuleSet::default();
        rules.settings.storm.window_secs = 0;
        RuleEngine::with_limits(rules, limits)
    }

    #[test]
    fn the_engine_can_live_on_another_thread() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<RuleEngine>();
    }

    #[test]
    fn tracking_stays_bounded_and_keeps_the_newest() {
        let mut engine = engine(Limits {
            dedupe: 100,
            tracked: 8,
        });
        for n in 0..20_u32 {
            let intents =
                engine.on_input(critical(&n.to_string(), f64::from(n + 1)), at(100.0), LOCAL);
            assert_eq!(intents.len(), 1);
        }
        assert!(engine.tracked.len() <= 8);
        assert!(engine.tracked.contains_key(&ObjectKey::service("h", "19")));
        assert!(!engine.tracked.contains_key(&ObjectKey::service("h", "0")));
    }

    #[test]
    fn eviction_never_drops_the_object_just_seen() {
        let mut engine = engine(Limits {
            dedupe: 100,
            tracked: 2,
        });
        assert_eq!(
            engine.on_input(critical("a", 50.0), at(100.0), LOCAL).len(),
            1
        );
        assert_eq!(
            engine.on_input(critical("b", 60.0), at(100.0), LOCAL).len(),
            1
        );
        // Older than everything tracked, yet it must be tracked and notify.
        let intents = engine.on_input(critical("c", 10.0), at(100.0), LOCAL);
        assert_eq!(intents.len(), 1);
        assert!(engine.tracked.contains_key(&ObjectKey::service("h", "c")));
        assert!(engine.tracked.len() <= 2);
    }

    #[test]
    fn dedupe_memory_is_bounded() {
        let mut engine = engine(Limits {
            dedupe: 3,
            tracked: 100,
        });
        for n in 0..10_u32 {
            let intents =
                engine.on_input(critical(&n.to_string(), f64::from(n + 1)), at(100.0), LOCAL);
            assert_eq!(intents.len(), 1);
        }
        assert_eq!(engine.dedupe.len(), 3);
    }

    #[test]
    fn recovered_objects_are_no_longer_tracked() {
        let mut engine = engine(Limits::default());
        assert_eq!(engine.on_input(critical("a", 1.0), at(1.0), LOCAL).len(), 1);
        assert_eq!(engine.tracked.len(), 1);
        let mut ok = critical("a", 2.0);
        ok.change = Change::State {
            previous: Some(CheckableState::Service(ServiceState::Critical)),
            current: CheckableState::Service(ServiceState::Ok),
            state_type: StateType::Hard,
            since: at(2.0),
            output: "OK".to_owned(),
        };
        let intents = engine.on_input(ok, at(2.0), LOCAL);
        assert_eq!(intents.len(), 1);
        assert!(engine.tracked.is_empty());
    }
}
