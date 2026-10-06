//! `Command::Action`: runs an action with the environment's author and
//! reports per-object failures. What it did shows through the events
//! Icinga sends about it (a check result, an acknowledgement, a comment, a
//! downtime); only when the stream doesn't carry those event types (or for
//! `execute-command`) are the succeeded targets re-queried, so their new
//! state shows within a second. A forced check's re-query in particular
//! would only bring the state from before the check, at the moment Icinga
//! runs it.
//!
//! An action Icinga didn't answer (its outcome is unknown: Icinga may have
//! applied it, or may still be applying it) is reported as failed with
//! that explanation, never as a plain failure, and its objects are
//! re-queried. Adding a comment, scheduling a downtime and running a
//! command aren't safe to repeat (a second request adds a duplicate), so
//! for [`DOUBT_WINDOW`] the same action on those objects is held back:
//! with the same text once Icinga's answer shows it applied the first one
//! (the comment or downtime is there), and with any text while nothing
//! shows yet.

use std::collections::HashMap;
use std::time::Duration;

use ic_api::{ActionResult, ApiError, Client};
use ic_model::{Action, ActionTarget, EventKind, ObjectKey, Timestamp};
use tokio::time::Instant;

use super::{Engine, Internal};
use crate::command::{ActionOutcome, CoreEvent};
use crate::store::Store;

/// How long an unknown outcome holds back the same action on its objects
/// (10 minutes). The client already waited the action timeout (5
/// minutes) for Icinga's answer; whatever Icinga still applies after that
/// shows within this time (the event stream carries new comments and
/// downtimes).
pub(super) const DOUBT_WINDOW: Duration = Duration::from_mins(10);

/// Icinga's clock and this machine's may differ by this much (seconds)
/// when an object created by an unanswered action is recognised by its
/// `entry_time`.
const CLOCK_SKEW: f64 = 300.0;

/// What an action creates that a repeat would duplicate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Repeat {
    /// `add-comment` with this text.
    Comment(String),
    /// `schedule-downtime` with this comment.
    Downtime(String),
    /// `execute-command`.
    Command,
}

impl Repeat {
    /// `None` for actions that are safe to repeat: Icinga refuses a second
    /// acknowledgement ("already acknowledged"), removals and checks are
    /// idempotent, a passive result repeats a state.
    fn of(action: &Action) -> Option<Self> {
        match action {
            Action::AddComment { text, .. } => Some(Self::Comment(text.clone())),
            Action::ScheduleDowntime { comment, .. } => Some(Self::Downtime(comment.clone())),
            Action::ExecuteCommand { .. } => Some(Self::Command),
            Action::CheckNow { .. }
            | Action::Acknowledge { .. }
            | Action::RemoveAcknowledgement
            | Action::RemoveAllDowntimes
            | Action::ProcessCheckResult { .. } => None,
        }
    }

    fn same_kind(&self, other: &Self) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
    }

    fn what(&self) -> &'static str {
        match self {
            Self::Comment(_) => "add a comment",
            Self::Downtime(_) => "schedule a downtime",
            Self::Command => "run a command",
        }
    }
}

/// An action on an object whose outcome is unknown.
#[derive(Clone, Debug)]
struct Doubt {
    repeat: Repeat,
    author: String,
    /// When the request went out (wall clock, to compare with Icinga's
    /// `entry_time`).
    sent: Timestamp,
    /// Until when the same action is held back.
    until: Instant,
}

/// The objects whose last comment, downtime or command got no answer.
#[derive(Debug, Default)]
pub(super) struct Doubts {
    by_object: HashMap<ObjectKey, Vec<Doubt>>,
}

impl Doubts {
    /// Remembers that `repeat` by `author`, sent at `sent`, got no answer
    /// for `object`.
    fn add(
        &mut self,
        object: ObjectKey,
        repeat: Repeat,
        author: &str,
        sent: Timestamp,
        now: Instant,
    ) {
        let doubts = self.by_object.entry(object).or_default();
        doubts.retain(|doubt| doubt.until > now && doubt.repeat != repeat);
        doubts.push(Doubt {
            repeat,
            author: author.to_owned(),
            sent,
            until: now + DOUBT_WINDOW,
        });
    }

    /// Splits `keys` into those `repeat` by `author` may go to and those
    /// held back, each with the reason.
    fn hold(
        &mut self,
        keys: Vec<ObjectKey>,
        repeat: &Repeat,
        author: &str,
        store: &Store,
        now: Instant,
    ) -> (Vec<ObjectKey>, Vec<(String, String)>) {
        self.by_object.retain(|_, doubts| {
            doubts.retain(|doubt| doubt.until > now);
            !doubts.is_empty()
        });
        if self.by_object.is_empty() {
            return (keys, Vec::new());
        }
        let mut allowed = Vec::with_capacity(keys.len());
        let mut held = Vec::new();
        for key in keys {
            let doubt = self.by_object.get(&key).and_then(|doubts| {
                doubts
                    .iter()
                    .find(|doubt| doubt.repeat.same_kind(repeat) && doubt.author == author)
            });
            let reason = doubt.and_then(|doubt| reason_to_hold(doubt, repeat, &key, store, now));
            match reason {
                Some(reason) => held.push((key.full_name(), reason)),
                None => allowed.push(key),
            }
        }
        (allowed, held)
    }
}

/// Why `repeat` on `key` is held back because of `doubt`, if it is:
/// Icinga applied the earlier request after all (and this one would add
/// the same again), or nothing shows yet whether it will.
fn reason_to_hold(
    doubt: &Doubt,
    repeat: &Repeat,
    key: &ObjectKey,
    store: &Store,
    now: Instant,
) -> Option<String> {
    let since = doubt.sent.as_unix_seconds() - CLOCK_SKEW;
    let applied = match &doubt.repeat {
        Repeat::Comment(text) => store
            .comments_of(key)
            .iter()
            .find(|comment| {
                comment.author == doubt.author
                    && comment.text == *text
                    && comment.entry_time.as_unix_seconds() >= since
            })
            .map(|comment| comment.name.clone()),
        Repeat::Downtime(text) => store
            .downtimes_of(key)
            .iter()
            .find(|downtime| {
                downtime.author == doubt.author
                    && downtime.comment == *text
                    && downtime.entry_time.as_unix_seconds() >= since
            })
            .map(|downtime| downtime.name.clone()),
        Repeat::Command => None,
    };
    if let Some(name) = applied {
        // Settled: the earlier one is there. Only the very same again is
        // a duplicate.
        return (doubt.repeat == *repeat).then(|| {
            format!(
                "not sent again: Icinga applied the earlier request that got no answer ({name})"
            )
        });
    }
    let minutes = doubt
        .until
        .saturating_duration_since(now)
        .as_secs()
        .div_ceil(60);
    Some(format!(
        "held back: an earlier request to {} here got no answer, and Icinga may still \
         apply it; look at the object, and try again in {minutes} min if it isn't there",
        repeat.what()
    ))
}

/// What a finished action reports back to the engine.
#[derive(Debug)]
pub(crate) struct Finished {
    pub(super) id: u64,
    /// The hosts and services it changes (re-queried unless the stream
    /// reports what it did).
    pub(super) dirty: Vec<ObjectKey>,
    /// The event types that report what it did.
    pub(super) reported_by: &'static [EventKind],
    pub(super) outcome: ActionOutcome,
    /// The objects whose outcome is unknown.
    pub(super) unknown: Vec<ObjectKey>,
    /// What a repeat would duplicate, by whom, and when it was sent.
    pub(super) repeat: Option<(Repeat, String, Timestamp)>,
}

impl Engine {
    pub(super) fn run_action(&mut self, id: u64, target: ActionTarget, action: Action) {
        let Some(conn) = &self.conn else {
            self.emit(CoreEvent::ActionFinished {
                id,
                outcome: ActionOutcome {
                    error: Some("not connected to Icinga".to_owned()),
                    ..ActionOutcome::default()
                },
            });
            return;
        };
        let client = conn.client.clone();
        let author = self.spec.environment.author_name().to_owned();
        let repeat = Repeat::of(&action);
        // Hold back what an unanswered earlier request may have done.
        let (target, held) = match (target, &repeat) {
            (ActionTarget::Objects(keys), Some(repeat)) => {
                let (keys, held) =
                    self.doubts
                        .hold(keys, repeat, &author, &self.store, Instant::now());
                (ActionTarget::Objects(keys), held)
            }
            (target, _) => (target, Vec::new()),
        };
        if !held.is_empty() {
            tracing::info!(
                id,
                held = held.len(),
                action = action.api_name(),
                "holding back a repeat of an action that got no answer"
            );
        }
        if matches!(&target, ActionTarget::Objects(keys) if keys.is_empty()) {
            self.emit(CoreEvent::ActionFinished {
                id,
                outcome: ActionOutcome {
                    failed: held,
                    ..ActionOutcome::default()
                },
            });
            return;
        }
        let dirty = self.action_objects(&target);
        let reported_by = reported_by(&action, &target);
        let runs = self.plan(action, target);
        let tx = self.internal_tx.clone();
        let sent = Timestamp::now();
        // Not a session task: an action whose request went out finishes
        // and reports even if the stream drops meanwhile.
        tokio::spawn(async move {
            let (mut outcome, unknown) = execute(&client, runs, &author).await;
            outcome.failed.extend(held);
            let finished = Finished {
                id,
                dirty,
                reported_by,
                outcome,
                unknown,
                repeat: repeat.map(|repeat| (repeat, author, sent)),
            };
            let _ = tx.send(Internal::ActionDone(Box::new(finished)));
        });
    }

    pub(super) fn on_action_done(&mut self, finished: Finished) {
        let Finished {
            id,
            dirty,
            reported_by,
            outcome,
            unknown,
            repeat,
        } = finished;
        tracing::info!(
            id,
            ok = outcome.ok,
            failed = outcome.failed.len(),
            unknown = unknown.len(),
            error = outcome.error.as_deref().unwrap_or(""),
            "action finished"
        );
        let now = Instant::now();
        if let Some((repeat, author, sent)) = repeat {
            for object in &unknown {
                self.doubts
                    .add(object.clone(), repeat.clone(), &author, sent, now);
            }
        }
        // Show what Icinga did: the events about it do, else the changed
        // objects are re-queried; those it may have changed are.
        if outcome.ok > 0 && !self.streams(reported_by) {
            self.fetch.mark_urgent(dirty, now);
        }
        if !unknown.is_empty() {
            self.fetch.mark_urgent(unknown, now);
        }
        self.emit(CoreEvent::ActionFinished { id, outcome });
        self.publish_changes();
    }

    /// Whether the event stream carries every one of `kinds` (none:
    /// `false`).
    fn streams(&self, kinds: &[EventKind]) -> bool {
        let Some(conn) = self.conn.as_ref().filter(|_| self.lines.is_some()) else {
            return false;
        };
        !kinds.is_empty()
            && kinds
                .iter()
                .all(|kind| conn.info.allows(&format!("events/{}", kind.api_name())))
    }

    /// The hosts and services an action changes: its objects, or the
    /// object of the downtime or comment it removes.
    fn action_objects(&self, target: &ActionTarget) -> Vec<ObjectKey> {
        match target {
            ActionTarget::Objects(keys) => keys.clone(),
            ActionTarget::Downtime(name) => self
                .store
                .downtime(name)
                .map(|downtime| downtime.object.clone())
                .into_iter()
                .collect(),
            ActionTarget::Comment(name) => self
                .store
                .comment(name)
                .map(|comment| comment.object.clone())
                .into_iter()
                .collect(),
        }
    }

    /// Splits `execute-command` without an endpoint: Icinga's default
    /// (`$command_endpoint$`) only works for objects that have one; the
    /// others get the instance's node name (otherwise Icinga answers "Can't
    /// find a valid endpoint" per object).
    fn plan(&self, action: Action, target: ActionTarget) -> Vec<(Action, ActionTarget)> {
        let (Action::ExecuteCommand { endpoint: None, .. }, ActionTarget::Objects(keys)) =
            (&action, &target)
        else {
            return vec![(action, target)];
        };
        let (remote, local): (Vec<ObjectKey>, Vec<ObjectKey>) =
            keys.iter().cloned().partition(|key| {
                self.store
                    .check(key)
                    .is_some_and(|check| check.command_endpoint.is_some())
            });
        let node = self
            .store
            .status()
            .map(|status| status.node_name.clone())
            .filter(|name| !name.is_empty());
        let mut runs = Vec::new();
        if !remote.is_empty() {
            runs.push((action.clone(), ActionTarget::Objects(remote)));
        }
        if !local.is_empty() {
            let mut action = action;
            if let Action::ExecuteCommand { endpoint, .. } = &mut action {
                endpoint.clone_from(&node);
            }
            runs.push((action, ActionTarget::Objects(local)));
        }
        runs
    }
}

/// The event types through which Icinga reports what `action` on `target`
/// did; with all of them on the stream its targets aren't re-queried.
/// None for `execute-command`, whose effect depends on the command.
fn reported_by(action: &Action, target: &ActionTarget) -> &'static [EventKind] {
    match (action, target) {
        (_, ActionTarget::Downtime(_)) => &[EventKind::DowntimeRemoved],
        (_, ActionTarget::Comment(_)) => &[EventKind::CommentRemoved],
        (Action::CheckNow { .. } | Action::ProcessCheckResult { .. }, _) => {
            &[EventKind::CheckResult]
        }
        (Action::Acknowledge { .. }, _) => &[EventKind::AcknowledgementSet],
        (Action::RemoveAcknowledgement, _) => &[EventKind::AcknowledgementCleared],
        (Action::ScheduleDowntime { .. }, _) => &[
            EventKind::DowntimeAdded,
            EventKind::DowntimeStarted,
            EventKind::DowntimeTriggered,
        ],
        (Action::RemoveAllDowntimes, _) => &[EventKind::DowntimeRemoved],
        (Action::AddComment { .. }, _) => &[EventKind::CommentAdded],
        (Action::ExecuteCommand { .. }, _) => &[],
    }
}

/// Runs the planned requests and sums up their results, and returns the
/// objects whose outcome is unknown. After a request got no answer nothing
/// more is sent (a master that stopped answering gets no more work).
async fn execute(
    client: &Client,
    runs: Vec<(Action, ActionTarget)>,
    author: &str,
) -> (ActionOutcome, Vec<ObjectKey>) {
    let mut outcome = ActionOutcome::default();
    let mut unknown = Vec::new();
    let mut answered = false;
    let mut runs = runs.into_iter();
    while let Some((action, target)) = runs.next() {
        match client.run_action(&action, &target, author).await {
            Ok(results) => {
                answered = true;
                let keys = keys_by_name(&target);
                count(&mut outcome, &mut unknown, &keys, results);
            }
            Err(error) if !answered => {
                outcome.error = Some(describe(&error));
                return (outcome, unknown);
            }
            Err(error) => {
                let message = describe(&error);
                outcome.failed.extend(
                    target_names(&target)
                        .into_iter()
                        .map(|name| (name, message.clone())),
                );
            }
        }
        if !unknown.is_empty() {
            for (_, target) in runs.by_ref() {
                outcome
                    .failed
                    .extend(target_names(&target).into_iter().map(|name| {
                        (
                            name,
                            "not sent: an earlier request got no answer".to_owned(),
                        )
                    }));
            }
        }
    }
    (outcome, unknown)
}

/// Adds results to `outcome`. Unknown outcomes count as failures that say
/// so (Icinga may have applied them); their objects go to `unknown`.
fn count(
    outcome: &mut ActionOutcome,
    unknown: &mut Vec<ObjectKey>,
    keys: &HashMap<String, ObjectKey>,
    results: Vec<ActionResult>,
) {
    for result in results {
        if result.is_success() {
            outcome.ok += 1;
            continue;
        }
        let name = result.target.unwrap_or_default();
        if result.unknown {
            if let Some(key) = keys.get(&name) {
                unknown.push(key.clone());
            }
            outcome.failed.push((
                name,
                format!("{}; look at it before trying again", result.status),
            ));
        } else {
            outcome.failed.push((name, result.status));
        }
    }
}

/// The objects of a target by full name, to recognise them in results.
fn keys_by_name(target: &ActionTarget) -> HashMap<String, ObjectKey> {
    match target {
        ActionTarget::Objects(keys) => keys
            .iter()
            .map(|key| (key.full_name(), key.clone()))
            .collect(),
        ActionTarget::Downtime(_) | ActionTarget::Comment(_) => HashMap::new(),
    }
}

fn describe(error: &ApiError) -> String {
    error.to_string()
}

fn target_names(target: &ActionTarget) -> Vec<String> {
    match target {
        ActionTarget::Objects(keys) => keys.iter().map(ObjectKey::full_name).collect(),
        ActionTarget::Downtime(name) | ActionTarget::Comment(name) => vec![name.clone()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn comment(text: &str) -> Repeat {
        Repeat::Comment(text.to_owned())
    }

    #[test]
    fn only_actions_that_duplicate_are_held_back() {
        assert_eq!(
            Repeat::of(&Action::AddComment {
                text: "x".to_owned(),
                expiry: None
            }),
            Some(comment("x"))
        );
        assert_eq!(Repeat::of(&Action::CheckNow { force: true }), None);
        assert_eq!(Repeat::of(&Action::RemoveAllDowntimes), None);
        assert_eq!(
            Repeat::of(&Action::Acknowledge {
                comment: "x".to_owned(),
                sticky: false,
                persistent: false,
                expiry: None,
            }),
            None,
            "Icinga refuses a second acknowledgement itself"
        );
    }

    #[test]
    fn doubts_hold_back_the_same_kind_by_the_same_author_until_they_expire() {
        let store = Store::default();
        let now = Instant::now();
        let host = ObjectKey::host("h");
        let other = ObjectKey::host("other");
        let mut doubts = Doubts::default();
        doubts.add(host.clone(), comment("a"), "me", Timestamp::now(), now);

        let (allowed, held) = doubts.hold(
            vec![host.clone(), other.clone()],
            &comment("b"),
            "me",
            &store,
            now,
        );
        assert_eq!(allowed, std::slice::from_ref(&other));
        assert_eq!(held.len(), 1);
        assert!(held[0].1.contains("10 min"), "{}", held[0].1);
        // Another kind of action, or another author: not held.
        let downtime = Repeat::Downtime("a".to_owned());
        assert_eq!(
            doubts
                .hold(vec![host.clone()], &downtime, "me", &store, now)
                .1,
            []
        );
        assert_eq!(
            doubts
                .hold(vec![host.clone()], &comment("a"), "you", &store, now)
                .1,
            []
        );
        // After the window everything goes out again.
        let later = now + DOUBT_WINDOW + Duration::from_secs(1);
        let (allowed, held) = doubts.hold(vec![host.clone()], &comment("a"), "me", &store, later);
        assert_eq!(allowed, [host]);
        assert!(held.is_empty());
        assert!(doubts.by_object.is_empty(), "expired doubts are dropped");
    }
}
