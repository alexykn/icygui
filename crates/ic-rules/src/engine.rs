//! The rule engine: turns [`RuleInput`]s into [`NotificationIntent`]s.

use std::collections::HashMap;
use std::time::Duration;

use ic_model::{CheckableState, HostState, ObjectKey, ServiceState, StateType, Timestamp};
use tracing::debug;

use crate::dedupe::Dedupe;
use crate::intent::{Change, DashboardRef, LocalTime, NotificationIntent, RuleInput, Tone};
use crate::quiet;
use crate::recent::Recent;
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
/// How many objects that left a problem state (OK, UP or pending) are
/// remembered, so a late or replayed older problem is recognized as such.
const SETTLED_CAPACITY: usize = 10_000;
/// How many flapping objects are remembered.
const FLAPPING_CAPACITY: usize = 10_000;
/// How many objects' latest downtime start is remembered.
const DOWNTIME_CAPACITY: usize = 10_000;

/// After an acknowledgement is removed or a downtime ends, a problem that
/// was skipped because of it waits at most this long for a fresh check
/// before it notifies. Icinga holds such notifications back while a check
/// is due within a minute, so a problem that recovers right away never
/// notifies.
const HANDLING_SETTLE: Duration = Duration::from_mins(1);
/// After a problem stops being handled without an event of its own (its
/// host or a parent recovered), it waits at most this long for a fresh
/// check: its state is likely left over from the outage. Icinga waits for
/// the object's next check after such a recovery.
const RECOVERY_SETTLE: Duration = Duration::from_mins(5);
/// Downtime starts reported for one object within this long count as one:
/// Icinga reports a fixed downtime as started and triggered at once.
const DOWNTIME_START_PAIR: Duration = Duration::from_secs(5);
/// A flapping object whose state hasn't changed for this long counts as no
/// longer flapping, in case the report that it stopped got lost.
const FLAPPING_EXPIRY: Duration = Duration::from_hours(1);
/// `since` values closer than this are the same instant, as in intent ids
/// (millisecond precision).
const SINCE_TOLERANCE_SECS: f64 = 0.001;

/// Memory bounds; tests shrink them.
#[derive(Clone, Copy, Debug)]
struct Limits {
    dedupe: usize,
    tracked: usize,
    settled: usize,
    flapping: usize,
    downtime_starts: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            dedupe: DEDUPE_CAPACITY,
            tracked: MAX_TRACKED,
            settled: SETTLED_CAPACITY,
            flapping: FLAPPING_CAPACITY,
            downtime_starts: DOWNTIME_CAPACITY,
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
///    A watch added while the object is in a problem that didn't notify
///    judges that problem again on the next tick, so it notifies (and its
///    recovery follows) as if it had been watched all along.
///
/// The follow-ups of a problem, its recovery and its acknowledgement, are
/// judged by the scopes that notified it instead (see below).
///
/// # Problems
///
/// A *problem* lasts from the object leaving OK / UP until it is back (or
/// pending); it may go through several problem states on the way.
///
/// - A problem state notifies if the rule's
///   [`StateFilter`](crate::StateFilter) selects it, it is hard or
///   `hard_only` is off, and it is unhandled or `skip_handled` is off. The
///   intent id is `"{object}:{state}:{since}"`, so a soft state turning hard
///   (same `since`) never notifies twice.
/// - A state already notified during the problem doesn't notify again,
///   unless another state notified in between. A check hovering around a
///   threshold (critical, warning, critical, …) doesn't page on every swing
///   back; Icinga 2.14+ skips such duplicates too.
/// - `min_duration_secs` holds the notification until the *problem* has
///   lasted that long, counted from when it began: a change to another
///   problem state doesn't restart the delay, and the state the object is
///   in when the delay runs out notifies. [`RuleEngine::tick`] releases it.
///   A recovery (or pending) cancels it, and so does handling (an
///   acknowledgement, a downtime, an unreachable object) for a rule that
///   skips handled problems. With several matching rules the earliest one
///   notifies. If the problem's `since` lies in the engine's future
///   (Icinga's clock runs ahead), the delay counts from when the engine
///   first saw it instead.
/// - A problem that didn't notify only because it was handled notifies once
///   its handling ends, the way Icinga sends suppressed notifications, but
///   only once its state is confirmed by a fresh check: a repeat of the
///   state that happened after the handling ended. Without one, it waits a
///   minute after an acknowledgement or downtime ended, and five minutes
///   after the handling ended without an event (its host or a parent
///   recovered, which leaves states over from the outage). A state change
///   in between is judged instead, so a problem that recovers on its next
///   check never notifies.
/// - While Icinga reports an object as flapping, its problems and
///   recoveries don't notify (as in Icinga). When it stops, its state then
///   is judged: a problem notifies (unless that state already did), and if
///   it is OK / UP, the recovery of a problem that notified does. An object
///   that hasn't changed state for an hour counts as no longer flapping.
/// - A recovery (back to OK / UP) notifies only if the problem notified,
///   including silent notifications (quiet hours, pause, storm). It is
///   judged by the scopes that notified the problem, as they are configured
///   now, and by a watch on the object: a dashboard turned off or a watch
///   removed since stays quiet, and a dashboard that didn't notify the
///   problem doesn't announce its end. `handled` doesn't matter: a problem
///   the user heard about ends even if its host is down. Recoveries are
///   never delayed.
/// - A change to pending forgets the object without notifying (it was
///   removed or re-created).
///
/// Inputs for one object must arrive in order. A state change older than
/// the state the engine knows (by `since`, to the millisecond) is ignored,
/// unless the known state lies in the future (Icinga's clock was ahead);
/// so is a problem older than the recovery that ended it, for the last
/// 10 000 objects that recovered. A `previous` state of OK, UP or pending
/// while the engine still tracks a problem means it missed the recovery:
/// a new problem begins.
///
/// # Other events
///
/// Acknowledgements, downtimes and flapping notify when the matching
/// scope's [`EventFilter`](crate::EventFilter) enables them. `hard_only`,
/// `skip_handled` and delays don't apply to them. Their id is
/// `"{object}:{kind}:{at}"`.
///
/// - Like Icinga, an acknowledgement (set or cleared) of a problem the
///   engine knows notifies only if the problem notified, and is judged by
///   the scopes that notified it. For an object whose problem began before
///   the engine saw it (after a restart), the scopes it is on decide.
/// - Downtime starts reported for one object within five seconds notify
///   once: Icinga reports a fixed downtime as both started and triggered.
///
/// # Output
///
/// - The same id is never emitted twice (across memberships, repeated
///   inputs and reconnect replays). The last 10 000 ids are remembered.
/// - Storm control: a notification that would be the `storm.threshold +
///   1`-th audible one within the `storm.window_secs` ending with it is
///   `silent` instead, for as long as notifications keep coming that fast.
///   What a storm silenced is summarized (`"14 new problems in
///   prod-cluster"`, `object = None`, tone `Info`, a breakdown in the body)
///   once a whole window passes without a silenced notification, and while
///   the storm lasts at most once a minute. Notifications that are silent
///   anyway (quiet hours, pause) don't count. `window_secs = 0` disables
///   storm control.
/// - Quiet hours (local time; see [`QuietHours`](crate::QuietHours)) make
///   notifications `silent`; with `allow_critical`, critical and down stay
///   audible (and so does a storm summary that silenced one of them).
/// - While paused, every notification is `silent`.
/// - Title `"{LABEL} · {service} on {host}"` or `"{LABEL} · {host}"` with
///   display names; body = the first line of the output, or `"{author}:
///   {comment}"`; tone by state; sound from the deciding rule; `at` = when
///   the change happened. Text is cleaned of control and bidirectional
///   formatting characters and cut to 400 characters.
#[derive(Debug)]
pub struct RuleEngine {
    scopes: Scopes,
    /// Bumped by `set_rules`; waiting notifications computed under an older
    /// generation are re-judged on the next tick.
    generation: u64,
    paused_until: Option<Timestamp>,
    /// Objects in a problem the engine has seen a change for.
    tracked: HashMap<ObjectKey, Tracked>,
    /// Objects that left a problem: the `since` of the state they settled
    /// in (OK, UP or pending).
    settled: Recent<ObjectKey, Timestamp>,
    /// Flapping objects, with when the engine last saw evidence of it (the
    /// start of the flapping, or a state change since).
    flapping: Recent<ObjectKey, Timestamp>,
    /// When each object's latest downtime start happened.
    downtime_starts: Recent<ObjectKey, Timestamp>,
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
            settled: Recent::new(limits.settled),
            flapping: Recent::new(limits.flapping),
            downtime_starts: Recent::new(limits.downtime_starts),
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
    /// the engine remembers (problems and who notified them, ids, the
    /// storm, pause) is kept. On the next [`RuleEngine::tick`], the new
    /// rules judge waiting notifications, problems held back by a mute the
    /// new rules no longer have, and problems of objects that gained a
    /// watch. Other problems that are already open don't notify because of
    /// the new rules: notifications are about changes.
    pub fn set_rules(&mut self, rules: RuleSet) {
        let old = &self.scopes.rules().settings.objects;
        let gained: Vec<ObjectKey> = rules
            .settings
            .objects
            .iter()
            .filter(|entry| entry.mode == ObjectMode::Watch && !old.contains(entry))
            .map(|entry| entry.object.clone())
            .collect();
        for object in gained {
            if let Some(tracked) = self.tracked.get_mut(&object) {
                tracked.rejudge = true;
            }
        }
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
    /// - `previous` = the state before the change, if known;
    /// - `handled` and `memberships` as they are *after* the change, with
    ///   memberships ignoring `problems_only` and `hide_handled`. `handled`
    ///   also covers an object that is unreachable (a failed host or parent
    ///   dependency), whose notifications Icinga suppresses as well;
    /// - for a host that isn't reachable, the state `Unreachable` rather
    ///   than `Down`: a down notification can't be taken back;
    /// - when an object's `handled` changes without an event of its own
    ///   (the services of a host that went down or came back, an object
    ///   whose parent recovered), a repeat of its current state (same
    ///   `current` and `since`) with the new `handled`;
    /// - after any change of `handled` to false, a repeat of the object's
    ///   state once its next check result confirms it, with `at` = that
    ///   check's time. Until then (or a settle time) the engine holds back
    ///   a problem whose handling ended, so a state left over from an
    ///   outage or a maintenance doesn't notify;
    /// - `FlappingStarted` and `FlappingStopped` whenever Icinga's flapping
    ///   flag changes, also when a reconcile finds it changed;
    /// - one `DowntimeStarted` per downtime: Icinga reports a fixed
    ///   downtime as both started and triggered (the engine counts starts
    ///   within five seconds as one).
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
            .and_then(|tracked| self.ready_at(&input.object, tracked, cutoff, now))
            .is_some()
        {
            self.evaluate(&input.object, moment, cutoff, &mut out);
        }
        if matches!(input.change, Change::State { .. }) {
            self.apply_state(input, moment, &mut out);
        } else {
            self.apply_event(input, moment, &mut out);
        }
        out
    }

    /// Advances time: ends an expired pause, summarizes a storm that ended
    /// or has gone on for a while, releases delayed notifications that are
    /// due, and judges problems whose mute ended, whose object stopped
    /// flapping, or that gained a watch. Call it about once a second.
    #[must_use = "the intents must be recorded, and the audible ones shown"]
    pub fn tick(&mut self, now: Timestamp, local: LocalTime) -> Vec<NotificationIntent> {
        let moment = Moment { now, local };
        let mut out = Vec::new();
        self.advance(moment, &mut out);
        let mut ready: Vec<(Timestamp, ObjectKey)> = self
            .tracked
            .iter()
            .filter_map(|(object, tracked)| {
                self.ready_at(object, tracked, now, now)
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
            self.evaluate(&object, moment, now, &mut out);
        }
        out
    }

    /// Whether a tracked problem must be judged again by `cutoff`, and the
    /// time that orders it among others: its delay ran out, the rules
    /// changed while it waited, its mute ended, its object stopped flapping
    /// (by `now`), or it gained a watch.
    fn ready_at(
        &self,
        object: &ObjectKey,
        tracked: &Tracked,
        cutoff: Timestamp,
        now: Timestamp,
    ) -> Option<Timestamp> {
        if tracked.status == Status::Notified {
            return None;
        }
        if tracked.rejudge {
            return Some(cutoff);
        }
        match tracked.status {
            Status::Waiting { due, generation } => {
                (due <= cutoff || generation != self.generation).then_some(due)
            }
            Status::Muted => (self.scopes.rules().object_mode(object, cutoff)
                != Some(ObjectMode::Mute))
            .then_some(cutoff),
            Status::Flapping => (!self.is_flapping(object, now)).then_some(cutoff),
            Status::Idle | Status::Suppressed | Status::Notified => None,
        }
    }

    /// Ends an expired pause and summarizes the storm if it ended or has
    /// gone on for a while.
    fn advance(&mut self, moment: Moment, out: &mut Vec<NotificationIntent>) {
        if self.paused_until.is_some_and(|until| until <= moment.now) {
            self.paused_until = None;
        }
        let settings = self.scopes.rules().settings.storm;
        let Some(summary) = self.storm.poll(moment.now, settings) else {
            return;
        };
        let environment = self.scopes.environment_name();
        // A storm starting again at the same instant (the clock went back)
        // must not reuse an id.
        let base = text::storm_id(summary.started);
        let mut id = base.clone();
        let mut attempt = 1_u32;
        while self.dedupe.contains(&id) {
            attempt = attempt.saturating_add(1);
            id = format!("{base}#{attempt}");
        }
        let intent = NotificationIntent {
            id,
            object: None,
            title: summary.title(environment),
            subtitle: environment.to_owned(),
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
            previous,
            current,
            state_type,
            since,
            output,
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
            // Only the cleaned first line is ever used; plugin output can be
            // huge.
            body: text::output_body(&output),
            handled,
            memberships,
        };
        match self.arrival(&observation, moment.now) {
            Arrival::Stale => {}
            Arrival::Repeat => self.apply_repeat(observation, moment, out),
            Arrival::New => self.apply_new_state(observation, previous, moment, out),
        }
    }

    /// How a state change relates to what the engine knows of the object.
    fn arrival(&self, observation: &Observation, now: Timestamp) -> Arrival {
        let object = &observation.subject.object;
        if let Some(tracked) = self.tracked.get(object) {
            let known = tracked.last.since;
            if is_before(observation.since, known) && known <= now {
                debug!(%object, "ignoring a state change older than the known state");
                return Arrival::Stale;
            }
            if tracked.last.state == observation.state && same_instant(observation.since, known) {
                return Arrival::Repeat;
            }
        } else if let Some(settled) = self.settled.get(object) {
            if is_before(observation.since, *settled) && *settled <= now {
                debug!(%object, "ignoring a problem older than the state that ended it");
                return Arrival::Stale;
            }
            if !observation.state.is_problem() && same_instant(observation.since, *settled) {
                // The OK / UP / pending state it settled in, again.
                return Arrival::Repeat;
            }
        }
        Arrival::New
    }

    /// Takes in the same state again: soft turned hard, the handling
    /// changed, or a check confirmed the state. For an object that isn't
    /// in a problem, there is nothing to do.
    fn apply_repeat(
        &mut self,
        observation: Observation,
        moment: Moment,
        out: &mut Vec<NotificationIntent>,
    ) {
        let object = observation.subject.object.clone();
        let Some(tracked) = self.tracked.get_mut(&object) else {
            return;
        };
        if !observation.handled
            && tracked
                .settle
                .is_some_and(|settle| is_after(observation.at, settle.after))
        {
            debug!(%object, "state confirmed by a check after the handling ended");
            tracked.settle = None;
        }
        if tracked.last.handled && !observation.handled && tracked.status != Status::Notified {
            // Handled no more without an event: the host or a parent
            // recovered, and the state may be left over from the outage.
            tracked.settle = Some(Settle {
                until: moment.now.plus(RECOVERY_SETTLE),
                after: observation.at,
            });
        }
        tracked.last.update(observation);
        self.evaluate(&object, moment, moment.now, out);
    }

    /// Takes in a new state: the problem goes on in another state, begins,
    /// or ends.
    fn apply_new_state(
        &mut self,
        observation: Observation,
        previous: Option<CheckableState>,
        moment: Moment,
        out: &mut Vec<NotificationIntent>,
    ) {
        let object = observation.subject.object.clone();
        self.note_state_change(&object, moment.now);
        let replaced = self.tracked.remove(&object);
        if replaced
            .as_ref()
            .is_some_and(|tracked| matches!(tracked.status, Status::Waiting { .. }))
        {
            debug!(%object, "waiting notification superseded by a new state");
        }
        let episode = replaced.and_then(|tracked| {
            // The caller saw the problem end: the engine missed a recovery.
            let missed_recovery = tracked.last.state.is_problem()
                && previous.is_some_and(|previous| !previous.is_problem());
            if missed_recovery {
                debug!(%object, "the previous problem ended unseen; a new one begins");
            }
            (!missed_recovery).then_some(tracked.episode)
        });

        if observation.state.is_problem() {
            self.settled.remove(&object);
            let episode =
                episode.unwrap_or_else(|| Episode::new(earliest(observation.since, moment.now)));
            self.track(Tracked::new(observation, episode));
            self.evaluate(&object, moment, moment.now, out);
        } else if is_up(observation.state) {
            self.settled.insert(object.clone(), observation.since);
            let Some(episode) = episode.filter(|episode| !episode.notified_by.is_empty()) else {
                return;
            };
            if self.is_flapping(&object, moment.now) {
                debug!(%object, "recovery held back: flapping");
                let mut tracked = Tracked::new(observation, episode);
                tracked.status = Status::Flapping;
                self.track(tracked);
            } else {
                self.recover(&observation, &episode, moment, out);
            }
        } else {
            // Pending: the object has no state any more (removed or
            // re-created); it is forgotten without a notification.
            self.settled.insert(object, observation.since);
        }
    }

    /// Notifies a recovery, if a scope that notified the problem (or a
    /// watch) wants it.
    fn recover(
        &mut self,
        observation: &Observation,
        episode: &Episode,
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
            .followup_candidates(&episode.notified_by, mode == Some(ObjectMode::Watch))
            .iter()
            .find(|candidate| candidate.enabled && candidate.rule.states.recovery)
            .map(|candidate| Draft {
                id: text::state_id(object, observation.state, observation.since),
                object: object.clone(),
                label: Label::Recovered,
                title: observation.subject.title(Label::Recovered),
                subtitle: candidate.subtitle.to_owned(),
                body: observation.body.clone(),
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
        match label {
            Label::Flapping => self.flapping.insert(object.clone(), moment.now),
            Label::FlappingStopped => {
                self.flapping.remove(&object);
            }
            _ => {}
        }
        let repeated_start = label == Label::Downtime
            && self
                .downtime_starts
                .get(&object)
                .is_some_and(|previous| within(at, *previous, DOWNTIME_START_PAIR));
        if label == Label::Downtime {
            self.downtime_starts.insert(object.clone(), at);
        }
        let subject = Subject {
            object,
            host_display,
            service_display,
        };
        if repeated_start {
            debug!(object = %subject.object, "downtime start already reported");
        } else {
            self.notify_event(&subject, label, body, &memberships, at, moment, out);
        }

        // The event may start or end the handling of a known problem, or
        // its flapping.
        let object = subject.object.clone();
        let Some(tracked) = self.tracked.get_mut(&object) else {
            return;
        };
        let ended = tracked.last.handled && !handled;
        tracked.last.subject = subject;
        tracked.last.handled = handled;
        tracked.last.memberships = memberships;
        if ended && tracked.status != Status::Notified {
            tracked.settle = Some(Settle {
                until: moment.now.plus(HANDLING_SETTLE),
                after: at,
            });
        }
        let rejudge = match label {
            Label::Flapping | Label::FlappingStopped => true,
            _ => matches!(tracked.status, Status::Waiting { .. } | Status::Suppressed),
        };
        if rejudge {
            self.evaluate(&object, moment, moment.now, out);
        }
    }

    /// Notifies an acknowledgement, downtime or flapping change, if a scope
    /// wants it.
    #[expect(
        clippy::too_many_arguments,
        reason = "the parts of one event, taken apart by the caller"
    )]
    fn notify_event(
        &mut self,
        subject: &Subject,
        label: Label,
        body: String,
        memberships: &[DashboardRef],
        at: Timestamp,
        moment: Moment,
        out: &mut Vec<NotificationIntent>,
    ) {
        let object = &subject.object;
        let mode = self.scopes.rules().object_mode(object, moment.now);
        if mode == Some(ObjectMode::Mute) {
            debug!(%object, "event not notified: muted");
            return;
        }
        let watched = mode == Some(ObjectMode::Watch);
        let candidates = match (label, self.tracked.get(object)) {
            // Like Icinga: an acknowledgement concerns the users who were
            // told about the problem.
            (Label::Acknowledged | Label::AckCleared, Some(tracked)) => {
                if tracked.episode.notified_by.is_empty() {
                    debug!(%object, "acknowledgement of a problem that didn't notify");
                    return;
                }
                self.scopes
                    .followup_candidates(&tracked.episode.notified_by, watched)
            }
            _ => self.scopes.candidates(memberships, watched),
        };
        let draft = candidates
            .iter()
            .find(|candidate| candidate.enabled && label.event_enabled(candidate.rule.events))
            .map(|candidate| Draft {
                id: text::event_id(object, label, at),
                object: object.clone(),
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

    /// Decides what the object's tracked state does now (notify, wait, hold
    /// back, or nothing) and records the outcome. Delays count as run out
    /// if due by `cutoff`.
    fn evaluate(
        &mut self,
        object: &ObjectKey,
        moment: Moment,
        cutoff: Timestamp,
        out: &mut Vec<NotificationIntent>,
    ) {
        let flapping = self.is_flapping(object, moment.now);
        let rules = self.scopes.rules();
        let muted = rules.object_mode(object, moment.now) == Some(ObjectMode::Mute);
        // Judged with the watch even while a mute wins over it, so a watched
        // problem is held back (not dropped) until the mute ends.
        let watched = rules.is_watched(object, moment.now);
        let generation = self.generation;
        let Some(tracked) = self.tracked.get_mut(object) else {
            return;
        };
        tracked.rejudge = false;
        if tracked.status == Status::Notified {
            return;
        }
        let verdict = if !tracked.last.state.is_problem() {
            // An OK / UP state kept while the object flapped.
            if flapping {
                Verdict::Hold(Status::Flapping)
            } else {
                Verdict::Recover
            }
        } else if tracked.episode.last_notified == Some(tracked.last.state) {
            debug!(%object, "state already notified during this problem");
            Verdict::Hold(Status::Notified)
        } else if flapping {
            Verdict::Hold(Status::Flapping)
        } else {
            match decide(&self.scopes, tracked, watched, cutoff) {
                Decision::Notify { .. } | Decision::Wait { .. } if muted => {
                    Verdict::Hold(Status::Muted)
                }
                Decision::Notify { draft, sources } => Verdict::Notify { draft, sources },
                Decision::Wait { due } => Verdict::Hold(Status::Waiting { due, generation }),
                Decision::Suppressed => Verdict::Hold(Status::Suppressed),
                Decision::Idle => Verdict::Hold(Status::Idle),
            }
        };
        match verdict {
            Verdict::Hold(status) => {
                if status != tracked.status {
                    debug!(%object, from = ?tracked.status, to = ?status, "problem status");
                }
                tracked.status = status;
            }
            Verdict::Recover => {
                if let Some(tracked) = self.tracked.remove(object) {
                    self.recover(&tracked.last, &tracked.episode, moment, out);
                }
            }
            Verdict::Notify { draft, sources } => {
                let emitted = self.emit(draft, moment, out);
                if let Some(tracked) = self.tracked.get_mut(object) {
                    // An id emitted before means an old state came back
                    // (a replay): it doesn't make the problem notified.
                    if emitted {
                        for source in sources {
                            if !tracked.episode.notified_by.contains(&source) {
                                tracked.episode.notified_by.push(source);
                            }
                        }
                        tracked.episode.last_notified = Some(tracked.last.state);
                    }
                    tracked.status = Status::Notified;
                }
            }
        }
    }

    /// Records `draft` unless it was emitted before, deciding whether it is
    /// silent: paused, quiet hours, or absorbed by a storm. Returns whether
    /// it was emitted.
    fn emit(&mut self, draft: Draft, moment: Moment, out: &mut Vec<NotificationIntent>) -> bool {
        if self.dedupe.contains(&draft.id) {
            debug!(id = %draft.id, "already notified");
            return false;
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
        true
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

    /// Whether `object` is flapping at `now`.
    fn is_flapping(&self, object: &ObjectKey, now: Timestamp) -> bool {
        self.flapping
            .get(object)
            .is_some_and(|evidence| now < evidence.plus(FLAPPING_EXPIRY))
    }

    /// A state change of a flapping object shows it still flaps; an
    /// expired flag is dropped.
    fn note_state_change(&mut self, object: &ObjectKey, now: Timestamp) {
        if self.is_flapping(object, now) {
            self.flapping.insert(object.clone(), now);
        } else {
            self.flapping.remove(object);
        }
    }

    /// Starts tracking an object's problem, keeping memory bounded.
    fn track(&mut self, tracked: Tracked) {
        let object = tracked.last.subject.object.clone();
        self.tracked.insert(object.clone(), tracked);
        if self.tracked.len() > self.limits.tracked {
            self.evict(&object);
        }
    }

    /// Forgets objects down to seven eighths of the limit (so this runs
    /// rarely), never `keep`. Problems that hold nothing (no notification
    /// pending, no recovery owed) go first; within each kind, those whose
    /// state changed longest ago.
    fn evict(&mut self, keep: &ObjectKey) {
        let target = self.limits.tracked - self.limits.tracked / 8;
        let excess = self.tracked.len().saturating_sub(target);
        let mut by_value: Vec<(bool, Timestamp, ObjectKey)> = self
            .tracked
            .iter()
            .filter(|(object, _)| *object != keep)
            .map(|(object, tracked)| (tracked.holds_something(), tracked.last.at, object.clone()))
            .collect();
        by_value.sort_by(|(holds_a, at_a, object_a), (holds_b, at_b, object_b)| {
            holds_a
                .cmp(holds_b)
                .then_with(|| at_a.as_unix_seconds().total_cmp(&at_b.as_unix_seconds()))
                .then_with(|| object_a.cmp(object_b))
        });
        for (_, _, object) in by_value.into_iter().take(excess) {
            self.tracked.remove(&object);
        }
        debug!(excess, "tracked too many problems; forgot some");
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
    /// The notification body: the output's first line, cleaned and cut.
    body: String,
    handled: bool,
    memberships: Vec<DashboardRef>,
}

impl Observation {
    /// Takes in a repeat of the same state. The known `since` stays (it
    /// names the state in ids); a blank body keeps the previous one.
    fn update(&mut self, newer: Self) {
        let body = if newer.body.is_empty() {
            std::mem::take(&mut self.body)
        } else {
            newer.body
        };
        *self = Self {
            since: self.since,
            body,
            ..newer
        };
    }
}

/// An object in a problem (or, while it flaps, back to OK / UP with the
/// problem's recovery still owed).
#[derive(Clone, Debug)]
struct Tracked {
    /// The latest state change and what later events told about it.
    last: Observation,
    /// The problem this state belongs to.
    episode: Episode,
    /// What the current state's notification is doing.
    status: Status,
    /// Set when the handling ended: the state awaits a fresh check.
    settle: Option<Settle>,
    /// Judge again on the next tick (the object gained a watch).
    rejudge: bool,
}

impl Tracked {
    fn new(last: Observation, episode: Episode) -> Self {
        Self {
            last,
            episode,
            status: Status::Idle,
            settle: None,
            rejudge: false,
        }
    }

    /// Whether forgetting it would lose something: a notification that may
    /// still come, or a recovery owed.
    fn holds_something(&self) -> bool {
        !self.episode.notified_by.is_empty()
            || self.rejudge
            || !matches!(self.status, Status::Idle | Status::Notified)
    }
}

/// A problem: from leaving OK / UP until back.
#[derive(Clone, Debug)]
struct Episode {
    /// When it began: the earliest of its first state's `since` and when
    /// the engine first saw it. A state can't begin after the engine hears
    /// of it, so a server clock that runs ahead can't hold delays back.
    start: Timestamp,
    /// Scopes whose rule notified one of its states; non-empty means its
    /// recovery and acknowledgement may notify.
    notified_by: Vec<Source>,
    /// The state notified last.
    last_notified: Option<CheckableState>,
}

impl Episode {
    fn new(start: Timestamp) -> Self {
        Self {
            start,
            notified_by: Vec::new(),
            last_notified: None,
        }
    }
}

/// A problem whose handling ended, waiting for a fresh check.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Settle {
    /// Without a confirmation it notifies no earlier than this (engine
    /// time).
    until: Timestamp,
    /// When the handling ended (the input's `at`); a repeat of the state
    /// that happened later confirms it.
    after: Timestamp,
}

/// The notification of an object's current state.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Status {
    /// Nothing to notify: no enabled rule matches.
    Idle,
    /// A rule would match if the problem weren't handled; judged again
    /// once the handling ends.
    Suppressed,
    /// A rule matches but the object is muted; notifies once the mute ends
    /// if the object is still in this state.
    Muted,
    /// The object flaps; judged again once it stops.
    Flapping,
    /// Waiting for a delay (`min_duration_secs`, or the settling after the
    /// handling ended).
    Waiting {
        /// When it notifies.
        due: Timestamp,
        /// The rules generation `due` was computed under.
        generation: u64,
    },
    /// Notified (or known to be notified already).
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

/// What a problem state's notification should do, by the rules.
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

/// How a state change relates to what the engine knows of the object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Arrival {
    /// Older than the known state: ignored.
    Stale,
    /// The known state again (same state and `since`).
    Repeat,
    /// A new state.
    New,
}

/// What [`RuleEngine::evaluate`] does with a tracked state.
#[derive(Debug)]
enum Verdict {
    /// Record this status, notify nothing.
    Hold(Status),
    /// Notify the problem.
    Notify { draft: Draft, sources: Vec<Source> },
    /// Notify the recovery and forget the object.
    Recover,
}

/// Judges a tracked problem state by the scopes of its object. Delays count
/// as run out if due by `cutoff`.
fn decide(scopes: &Scopes, tracked: &Tracked, watched: bool, cutoff: Timestamp) -> Decision {
    let last = &tracked.last;
    let Some(label) = Label::for_problem(last.state) else {
        return Decision::Idle;
    };
    let candidates = scopes.candidates(&last.memberships, watched);
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

    let due = |candidate: &Candidate<'_>| {
        let delay = Duration::from_secs(u64::from(candidate.rule.min_duration_secs));
        let delayed = tracked.episode.start.plus(delay);
        match tracked.settle {
            // Only rules that skip handled problems were held back by the
            // handling; for the others it never mattered.
            Some(settle) if candidate.rule.skip_handled => latest(delayed, settle.until),
            _ => delayed,
        }
    };
    if let Some(chosen) = matching.iter().find(|candidate| due(candidate) <= cutoff) {
        let draft = Draft {
            id: text::state_id(&last.subject.object, last.state, last.since),
            object: last.subject.object.clone(),
            label,
            title: last.subject.title(label),
            subtitle: chosen.subtitle.to_owned(),
            body: last.body.clone(),
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

/// Whether `a` and `b` are the same instant, to the millisecond.
fn same_instant(a: Timestamp, b: Timestamp) -> bool {
    (a.as_unix_seconds() - b.as_unix_seconds()).abs() < SINCE_TOLERANCE_SECS
}

/// Whether `a` lies before `b` by more than the tolerance.
fn is_before(a: Timestamp, b: Timestamp) -> bool {
    a.as_unix_seconds() < b.as_unix_seconds() - SINCE_TOLERANCE_SECS
}

/// Whether `a` lies after `b` by more than the tolerance.
fn is_after(a: Timestamp, b: Timestamp) -> bool {
    is_before(b, a)
}

/// Whether `a` and `b` lie less than `span` apart.
fn within(a: Timestamp, b: Timestamp, span: Duration) -> bool {
    (a.as_unix_seconds() - b.as_unix_seconds()).abs() < span.as_secs_f64()
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

    fn state_input(service: &str, state: ServiceState, since: f64) -> RuleInput {
        RuleInput {
            object: ObjectKey::service("h", service),
            host_display: "h".to_owned(),
            service_display: Some(service.to_owned()),
            change: Change::State {
                previous: None,
                current: CheckableState::Service(state),
                state_type: StateType::Hard,
                since: at(since),
                output: format!("{state:?}"),
            },
            handled: false,
            memberships: Vec::new(),
            at: at(since),
        }
    }

    fn critical(service: &str, since: f64) -> RuleInput {
        state_input(service, ServiceState::Critical, since)
    }

    fn limits(tracked: usize) -> Limits {
        Limits {
            dedupe: 100,
            tracked,
            ..Limits::default()
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
        let mut engine = engine(limits(8));
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
        let mut engine = engine(limits(2));
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
    fn eviction_forgets_problems_that_hold_nothing_first() {
        let mut rules = RuleSet::default();
        rules.settings.storm.window_secs = 0;
        rules.settings.default_rule.min_duration_secs = 300;
        let mut engine = RuleEngine::with_limits(rules, limits(4));
        // An old problem waiting for its delay.
        assert!(
            engine
                .on_input(critical("waiting", 0.0), at(0.0), LOCAL)
                .is_empty()
        );
        // Newer warnings no rule selects.
        for n in 0..8_u32 {
            let warning = state_input(&format!("w{n}"), ServiceState::Warning, f64::from(n + 1));
            assert!(engine.on_input(warning, at(10.0), LOCAL).is_empty());
        }
        assert!(engine.tracked.len() <= 4);
        let intents = engine.tick(at(300.0), LOCAL);
        assert_eq!(intents.len(), 1, "the waiting notification survived");
        assert_eq!(intents[0].object, Some(ObjectKey::service("h", "waiting")));
    }

    #[test]
    fn dedupe_memory_is_bounded() {
        let mut engine = engine(Limits {
            dedupe: 3,
            ..Limits::default()
        });
        for n in 0..10_u32 {
            let intents =
                engine.on_input(critical(&n.to_string(), f64::from(n + 1)), at(100.0), LOCAL);
            assert_eq!(intents.len(), 1);
        }
        assert_eq!(engine.dedupe.len(), 3);
    }

    #[test]
    fn other_memories_are_bounded() {
        let mut engine = engine(Limits {
            settled: 3,
            flapping: 3,
            downtime_starts: 3,
            ..Limits::default()
        });
        for n in 0..10_u32 {
            let service = n.to_string();
            let mut flapping = critical(&service, 0.0);
            flapping.change = Change::FlappingStarted;
            let mut downtime = critical(&service, 0.0);
            downtime.change = Change::DowntimeStarted {
                author: String::new(),
                comment: String::new(),
            };
            let ok = state_input(&service, ServiceState::Ok, f64::from(n));
            for input in [flapping, downtime, ok] {
                let _ = engine.on_input(input, at(100.0), LOCAL);
            }
        }
        assert_eq!(engine.settled.len(), 3);
        assert_eq!(engine.flapping.len(), 3);
        assert_eq!(engine.downtime_starts.len(), 3);
    }

    #[test]
    fn recovered_objects_are_no_longer_tracked() {
        let mut engine = engine(Limits::default());
        assert_eq!(engine.on_input(critical("a", 1.0), at(1.0), LOCAL).len(), 1);
        assert_eq!(engine.tracked.len(), 1);
        let ok = state_input("a", ServiceState::Ok, 2.0);
        let intents = engine.on_input(ok, at(2.0), LOCAL);
        assert_eq!(intents.len(), 1);
        assert!(engine.tracked.is_empty());
    }

    #[test]
    fn huge_plugin_output_is_not_kept() {
        let mut engine = engine(Limits::default());
        let mut input = critical("a", 1.0);
        if let Change::State { output, .. } = &mut input.change {
            *output = format!("{}\n{}", "x".repeat(2_000_000), "y".repeat(1_000_000));
        }
        let intents = engine.on_input(input, at(1.0), LOCAL);
        assert_eq!(intents[0].body.chars().count(), 400);
        let tracked = engine.tracked.values().next().unwrap();
        assert!(tracked.last.body.len() <= 4 * 400);
        assert!(format!("{engine:?}").len() < 20_000);
    }

    #[test]
    fn instants_compare_to_the_millisecond() {
        assert!(same_instant(at(100.0004), at(100.0001)));
        assert!(same_instant(at(100.0004), at(100.0006)));
        assert!(!same_instant(at(100.0), at(100.002)));
        assert!(!is_before(at(100.0001), at(100.0004)));
        assert!(is_before(at(99.998), at(100.0)));
        assert!(is_after(at(100.002), at(100.0)));
        assert!(!is_after(at(100.0005), at(100.0)));
        assert!(within(at(0.0), at(4.9), Duration::from_secs(5)));
        assert!(!within(at(0.0), at(5.0), Duration::from_secs(5)));
    }
}
