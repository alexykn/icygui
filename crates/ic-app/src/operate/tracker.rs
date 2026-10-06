//! What became of the actions sent (ACT-07): the objects' optimistic
//! markers (`ack pending…` in their rows and panes while Icinga works on
//! it and until a snapshot shows the change), the last failure per object
//! (shown under the pane's buttons), and the toasts that report each
//! action: in progress, done, done except for some objects (each named
//! with Icinga's reason), or failed.
//!
//! Pure: times come in as `Instant`s, snapshots as references, so it's
//! tested without a window or a core.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::time::{Duration, Instant};

use ic_core::ActionOutcome;
use ic_core::snapshot::Snapshot;
use ic_model::{ActionTarget, ObjectKey, Timestamp};

use super::ActionSpec;
use super::forms::describe_objects;
use crate::actions::ObjectAction;
use crate::app_state::parse_object;

/// A successful action's marker stays this long at most when no snapshot
/// shows its change (an event Icinga never sent, a change undone at once).
const SETTLE_TIMEOUT: Duration = Duration::from_secs(15);
/// A check takes up to its plugin's timeout to bring its result.
const CHECK_SETTLE_TIMEOUT: Duration = Duration::from_mins(1);
/// A marker whose action never got an answer (the engine stopped) goes
/// after this long.
const UNANSWERED_TIMEOUT: Duration = Duration::from_mins(5);
/// How long toasts that need no attention stay.
const SUCCESS_TOAST: Duration = Duration::from_secs(5);
/// How long information and refusals stay.
const INFO_TOAST: Duration = Duration::from_secs(8);
/// The most toasts shown at once; older ones that need no answer go
/// first.
const MAX_TOASTS: usize = 4;
/// The most failures a toast lists by name.
const MAX_FAILURE_LINES: usize = 4;
/// Longer reasons from Icinga are cut.
const MAX_REASON_CHARS: usize = 200;

/// How a toast looks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToastTone {
    /// The action is on its way.
    Pending,
    /// It worked for every object.
    Success,
    /// It failed for some objects.
    Partial,
    /// It failed.
    Failed,
    /// Something to know (nothing to do, a refusal).
    Info,
}

/// One toast.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Toast {
    /// Its id (for dismissing).
    pub(crate) id: u64,
    /// The action it reports, if any.
    pub(crate) action: Option<u64>,
    /// Its look.
    pub(crate) tone: ToastTone,
    /// What happened.
    pub(crate) title: String,
    /// The failures by object (`disk on db-01: Service db-01!disk is
    /// OK.`), all of them, or a detail.
    pub(crate) lines: Vec<String>,
    /// When it goes by itself (`None`: once dismissed).
    pub(crate) expires: Option<Instant>,
}

impl Toast {
    /// The lines a toast shows (the first few failures).
    pub(crate) fn shown_lines(&self) -> &[String] {
        &self.lines[..self.lines.len().min(MAX_FAILURE_LINES)]
    }

    /// The lines it doesn't show.
    pub(crate) fn hidden(&self) -> usize {
        self.lines.len().saturating_sub(MAX_FAILURE_LINES)
    }
}

/// The last failed action on an object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Failure {
    /// What failed.
    pub(crate) action: ObjectAction,
    /// Icinga's reason.
    pub(crate) reason: String,
}

/// When a successful action's marker can go: once a snapshot shows its
/// change.
#[derive(Clone, Debug, PartialEq)]
enum Settle {
    Acknowledged,
    NotAcknowledged,
    /// A check result newer than the one before.
    CheckedAfter(Option<Timestamp>),
    /// More downtimes than before.
    DowntimesAbove(usize),
    NoDowntimes,
    DowntimeGone(String),
    /// More comments than before.
    CommentsAbove(usize),
    CommentGone(String),
    /// Nothing to see: the answer is enough.
    Answered,
}

/// An object's optimistic marker.
#[derive(Clone, Debug, PartialEq)]
struct Mark {
    action: u64,
    kind: ObjectAction,
    label: &'static str,
    settle: Settle,
    started: Instant,
    finished: Option<Instant>,
    timeout: Duration,
}

/// An action in flight.
#[derive(Clone, Debug, PartialEq)]
struct Running {
    kind: ObjectAction,
    objects: Vec<ObjectKey>,
    /// It targets a downtime or comment by name: Icinga's failures name
    /// that, not the object.
    by_name: bool,
    toast: u64,
}

/// The actions in flight, their markers, failures and toasts.
#[derive(Clone, Debug, Default)]
pub(crate) struct Tracker {
    running: BTreeMap<u64, Running>,
    marks: HashMap<ObjectKey, Mark>,
    failures: HashMap<ObjectKey, Failure>,
    toasts: VecDeque<Toast>,
    next_toast: u64,
}

/// The words for an action in toasts: in progress, done, to do.
struct Verbs {
    progressive: &'static str,
    past: &'static str,
    infinitive: &'static str,
}

fn verbs(kind: &ObjectAction) -> Verbs {
    let (progressive, past, infinitive) = match kind {
        ObjectAction::Acknowledge => ("Acknowledging", "Acknowledged", "acknowledge"),
        ObjectAction::RemoveAcknowledgement => (
            "Removing the acknowledgement of",
            "Removed the acknowledgement of",
            "remove the acknowledgement of",
        ),
        ObjectAction::ScheduleDowntime => (
            "Scheduling a downtime for",
            "Scheduled a downtime for",
            "schedule a downtime for",
        ),
        ObjectAction::RemoveDowntimes => (
            "Removing the downtimes of",
            "Removed the downtimes of",
            "remove the downtimes of",
        ),
        ObjectAction::RemoveDowntime(_) => (
            "Removing a downtime of",
            "Removed a downtime of",
            "remove a downtime of",
        ),
        ObjectAction::CheckNow => (
            "Rescheduling the check of",
            "Rescheduled the check of",
            "reschedule the check of",
        ),
        ObjectAction::AddComment => (
            "Adding a comment to",
            "Added a comment to",
            "add a comment to",
        ),
        ObjectAction::RemoveComment(_) => (
            "Removing a comment of",
            "Removed a comment of",
            "remove a comment of",
        ),
        ObjectAction::SubmitCheckResult => (
            "Submitting a check result for",
            "Submitted a check result for",
            "submit a check result for",
        ),
        ObjectAction::RunCommand => (
            "Starting the command on",
            "Started the command on",
            "start the command on",
        ),
    };
    Verbs {
        progressive,
        past,
        infinitive,
    }
}

/// The marker rows and panes show while `kind` is in flight.
pub(crate) fn mark_label(kind: &ObjectAction) -> &'static str {
    match kind {
        ObjectAction::Acknowledge => "ack pending…",
        ObjectAction::RemoveAcknowledgement => "removing ack…",
        ObjectAction::ScheduleDowntime => "downtime pending…",
        ObjectAction::RemoveDowntimes | ObjectAction::RemoveDowntime(_) => "removing downtime…",
        ObjectAction::CheckNow => "checking…",
        ObjectAction::AddComment => "comment pending…",
        ObjectAction::RemoveComment(_) => "removing comment…",
        ObjectAction::SubmitCheckResult => "result pending…",
        ObjectAction::RunCommand => "command running…",
    }
}

fn last_check(snapshot: &Snapshot, object: &ObjectKey) -> Option<Timestamp> {
    match object {
        ObjectKey::Host { name } => snapshot.hosts.get(name)?.check.last_check,
        ObjectKey::Service { key } => snapshot.services.get(key)?.check.last_check,
    }
}

fn acknowledged(snapshot: &Snapshot, object: &ObjectKey) -> Option<bool> {
    Some(
        match object {
            ObjectKey::Host { name } => snapshot.hosts.get(name)?.check.acknowledgement,
            ObjectKey::Service { key } => snapshot.services.get(key)?.check.acknowledgement,
        }
        .is_acknowledged(),
    )
}

fn downtimes(snapshot: &Snapshot, object: &ObjectKey) -> usize {
    snapshot.downtimes.get(object).map_or(0, Vec::len)
}

fn comments(snapshot: &Snapshot, object: &ObjectKey) -> usize {
    snapshot.comments.get(object).map_or(0, Vec::len)
}

impl Settle {
    fn for_action(kind: &ObjectAction, snapshot: &Snapshot, object: &ObjectKey) -> Self {
        match kind {
            ObjectAction::Acknowledge => Self::Acknowledged,
            ObjectAction::RemoveAcknowledgement => Self::NotAcknowledged,
            ObjectAction::ScheduleDowntime => Self::DowntimesAbove(downtimes(snapshot, object)),
            ObjectAction::RemoveDowntimes => Self::NoDowntimes,
            ObjectAction::RemoveDowntime(name) => Self::DowntimeGone(name.clone()),
            ObjectAction::CheckNow | ObjectAction::SubmitCheckResult => {
                Self::CheckedAfter(last_check(snapshot, object))
            }
            ObjectAction::AddComment => Self::CommentsAbove(comments(snapshot, object)),
            ObjectAction::RemoveComment(name) => Self::CommentGone(name.clone()),
            ObjectAction::RunCommand => Self::Answered,
        }
    }

    /// Whether `snapshot` shows the change (an object gone from it shows
    /// everything it ever will).
    fn shown(&self, snapshot: &Snapshot, object: &ObjectKey) -> bool {
        match self {
            Self::Acknowledged => acknowledged(snapshot, object).is_none_or(|ack| ack),
            Self::NotAcknowledged => acknowledged(snapshot, object).is_none_or(|ack| !ack),
            Self::CheckedAfter(before) => match (last_check(snapshot, object), before) {
                (Some(now), Some(before)) => now > *before,
                (Some(_), None) => true,
                (None, _) => acknowledged(snapshot, object).is_none(),
            },
            Self::DowntimesAbove(before) => downtimes(snapshot, object) > *before,
            Self::NoDowntimes => downtimes(snapshot, object) == 0,
            Self::DowntimeGone(name) => snapshot
                .downtimes
                .get(object)
                .is_none_or(|list| list.iter().all(|downtime| &downtime.name != name)),
            Self::CommentsAbove(before) => comments(snapshot, object) > *before,
            Self::CommentGone(name) => snapshot
                .comments
                .get(object)
                .is_none_or(|list| list.iter().all(|comment| &comment.name != name)),
            Self::Answered => true,
        }
    }
}

/// `disk on db-01: Service db-01!disk is OK.`
fn failure_line(object: Option<&ObjectKey>, name: &str, reason: &str) -> String {
    let who = object.map_or_else(
        || name.to_owned(),
        |object| describe_objects(std::slice::from_ref(object)),
    );
    let reason = cut(reason.trim());
    if reason.is_empty() {
        who
    } else {
        format!("{who}: {reason}")
    }
}

fn cut(text: &str) -> String {
    if text.chars().count() <= MAX_REASON_CHARS {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(MAX_REASON_CHARS - 1).collect();
    cut.push('…');
    cut
}

impl Tracker {
    /// Records that action `id` (`spec`) was sent at `now`: markers on its
    /// objects, a toast saying it's on its way. Earlier failures of those
    /// objects are forgotten.
    pub(crate) fn start(&mut self, id: u64, spec: &ActionSpec, snapshot: &Snapshot, now: Instant) {
        let timeout = if matches!(
            spec.kind,
            ObjectAction::CheckNow | ObjectAction::SubmitCheckResult
        ) {
            CHECK_SETTLE_TIMEOUT
        } else {
            SETTLE_TIMEOUT
        };
        for object in &spec.objects {
            self.failures.remove(object);
            self.marks.insert(
                object.clone(),
                Mark {
                    action: id,
                    kind: spec.kind.clone(),
                    label: mark_label(&spec.kind),
                    settle: Settle::for_action(&spec.kind, snapshot, object),
                    started: now,
                    finished: None,
                    timeout,
                },
            );
        }
        let words = verbs(&spec.kind);
        let toast = self.push(
            Some(id),
            ToastTone::Pending,
            format!("{} {}…", words.progressive, describe_objects(&spec.objects)),
            Vec::new(),
            None,
        );
        self.running.insert(
            id,
            Running {
                kind: spec.kind.clone(),
                objects: spec.objects.clone(),
                by_name: !matches!(spec.target, ActionTarget::Objects(_)),
                toast,
            },
        );
    }

    /// Takes the core's answer to action `id`: failed objects lose their
    /// marker and remember why; the others keep it until a snapshot shows
    /// the change (or a while passes). The toast says how it went.
    pub(crate) fn finish(
        &mut self,
        id: u64,
        outcome: &ActionOutcome,
        snapshot: &Snapshot,
        now: Instant,
    ) {
        let Some(running) = self.running.remove(&id) else {
            tracing::debug!(id, "an answer to an action this window didn't send");
            return;
        };
        let words = verbs(&running.kind);
        let what = describe_objects(&running.objects);
        // Which objects failed, and why.
        let mut failed: Vec<(Option<ObjectKey>, String, String)> = Vec::new();
        if let Some(error) = &outcome.error {
            for object in &running.objects {
                failed.push((Some(object.clone()), object.full_name(), error.clone()));
            }
        } else {
            for (name, reason) in &outcome.failed {
                let object = if running.by_name {
                    running.objects.first().cloned()
                } else {
                    parse_object(name).filter(|object| running.objects.contains(object))
                };
                failed.push((object, name.clone(), reason.clone()));
            }
        }
        for (object, _, reason) in &failed {
            if let Some(object) = object {
                if self.marks.get(object).is_some_and(|mark| mark.action == id) {
                    self.marks.remove(object);
                }
                self.failures.insert(
                    object.clone(),
                    Failure {
                        action: running.kind.clone(),
                        reason: reason.clone(),
                    },
                );
            }
        }
        for mark in self.marks.values_mut().filter(|mark| mark.action == id) {
            mark.finished = Some(now);
        }
        self.settle(snapshot, now);

        let (tone, title, lines) = if let Some(error) = &outcome.error {
            (
                ToastTone::Failed,
                format!("Couldn't {} {what}", words.infinitive),
                vec![cut(error)],
            )
        } else if failed.is_empty() {
            (
                ToastTone::Success,
                format!("{} {what}", words.past),
                Vec::new(),
            )
        } else {
            let lines = failed
                .iter()
                .map(|(object, name, reason)| failure_line(object.as_ref(), name, reason))
                .collect();
            if outcome.ok == 0 {
                (
                    ToastTone::Failed,
                    format!("Couldn't {} {what}", words.infinitive),
                    lines,
                )
            } else {
                (
                    ToastTone::Partial,
                    format!("{} {} of {what}", words.past, outcome.ok),
                    lines,
                )
            }
        };
        let expires = (tone == ToastTone::Success).then(|| now + SUCCESS_TOAST);
        match self
            .toasts
            .iter_mut()
            .find(|toast| toast.id == running.toast)
        {
            Some(toast) => {
                toast.tone = tone;
                toast.title = title;
                toast.lines = lines;
                toast.expires = expires;
            }
            None => {
                self.push(Some(id), tone, title, lines, expires);
            }
        }
    }

    /// Removes the markers a snapshot shows the change of (or that waited
    /// long enough). Returns whether any went.
    pub(crate) fn settle(&mut self, snapshot: &Snapshot, now: Instant) -> bool {
        let before = self.marks.len();
        self.marks.retain(|object, mark| match mark.finished {
            Some(finished) => {
                !mark.settle.shown(snapshot, object)
                    && now.saturating_duration_since(finished) < mark.timeout
            }
            None => now.saturating_duration_since(mark.started) < UNANSWERED_TIMEOUT,
        });
        self.marks.len() != before
    }

    /// The marker to show for `object` (`ack pending…`), if any.
    pub(crate) fn label(&self, object: &ObjectKey) -> Option<&'static str> {
        self.marks.get(object).map(|mark| mark.label)
    }

    /// The action in flight or settling on `object`, with its marker.
    pub(crate) fn pending(&self, object: &ObjectKey) -> Option<(&ObjectAction, &'static str)> {
        self.marks.get(object).map(|mark| (&mark.kind, mark.label))
    }

    /// The last failure of an action on `object`.
    pub(crate) fn failure(&self, object: &ObjectKey) -> Option<&Failure> {
        self.failures.get(object)
    }

    /// Forgets `object`'s failure. Returns whether there was one.
    pub(crate) fn dismiss_failure(&mut self, object: &ObjectKey) -> bool {
        self.failures.remove(object).is_some()
    }

    /// The toasts, oldest first.
    pub(crate) fn toasts(&self) -> impl Iterator<Item = &Toast> {
        self.toasts.iter()
    }

    /// Whether any action is in flight.
    #[cfg(test)]
    pub(crate) fn has_running(&self) -> bool {
        !self.running.is_empty()
    }

    /// Shows a toast that needs no answer (nothing to do, a refusal).
    pub(crate) fn inform(&mut self, title: String, detail: Option<String>, now: Instant) -> u64 {
        self.push(
            None,
            ToastTone::Info,
            title,
            detail.into_iter().collect(),
            Some(now + INFO_TOAST),
        )
    }

    /// Hides toast `id`. Returns whether it was shown.
    pub(crate) fn dismiss_toast(&mut self, id: u64) -> bool {
        let before = self.toasts.len();
        self.toasts.retain(|toast| toast.id != id);
        self.toasts.len() != before
    }

    /// Removes the toasts whose time is up. Returns whether any went.
    pub(crate) fn expire(&mut self, now: Instant) -> bool {
        let before = self.toasts.len();
        self.toasts
            .retain(|toast| toast.expires.is_none_or(|expires| expires > now));
        self.toasts.len() != before
    }

    /// Forgets everything (another environment).
    pub(crate) fn clear(&mut self) {
        *self = Self {
            next_toast: self.next_toast,
            ..Self::default()
        };
    }

    fn push(
        &mut self,
        action: Option<u64>,
        tone: ToastTone,
        title: String,
        lines: Vec<String>,
        expires: Option<Instant>,
    ) -> u64 {
        self.next_toast += 1;
        let id = self.next_toast;
        self.toasts.push_back(Toast {
            id,
            action,
            tone,
            title,
            lines,
            expires,
        });
        while self.toasts.len() > MAX_TOASTS {
            // The oldest that doesn't wait for an answer or a reader goes;
            // else the oldest.
            let index = self
                .toasts
                .iter()
                .position(|toast| matches!(toast.tone, ToastTone::Success | ToastTone::Info))
                .unwrap_or(0);
            self.toasts.remove(index);
        }
        id
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ic_model::{AckKind, Action, Comment, CommentKind, Service, ServiceState};

    use super::*;

    fn disk() -> ObjectKey {
        ObjectKey::service("db-01", "disk")
    }

    fn load() -> ObjectKey {
        ObjectKey::service("db-01", "load")
    }

    fn snapshot(acknowledged: bool, last_check: f64) -> Snapshot {
        let services = [("disk", acknowledged), ("load", false)]
            .into_iter()
            .map(|(name, acknowledged)| {
                let mut service = Service::new("db-01", name);
                service.state = ServiceState::Critical;
                service.check.last_check = Some(Timestamp::from_unix_seconds(last_check));
                if acknowledged {
                    service.check.acknowledgement = AckKind::Normal;
                }
                (service.key.clone(), Arc::new(service))
            })
            .collect();
        Snapshot {
            services: Arc::new(services),
            ..Snapshot::default()
        }
    }

    fn spec(kind: ObjectAction, action: Action, objects: Vec<ObjectKey>) -> ActionSpec {
        ActionSpec {
            kind,
            action,
            target: ActionTarget::Objects(objects.clone()),
            objects,
        }
    }

    fn ack(objects: Vec<ObjectKey>) -> ActionSpec {
        spec(
            ObjectAction::Acknowledge,
            Action::Acknowledge {
                comment: "c".to_owned(),
                sticky: false,
                persistent: false,
                expiry: None,
            },
            objects,
        )
    }

    fn ok(count: usize) -> ActionOutcome {
        ActionOutcome {
            ok: count,
            ..ActionOutcome::default()
        }
    }

    #[test]
    fn markers_last_until_a_snapshot_shows_the_change() {
        let mut tracker = Tracker::default();
        let t0 = Instant::now();
        let before = snapshot(false, 100.);
        tracker.start(1, &ack(vec![disk()]), &before, t0);
        assert_eq!(tracker.label(&disk()), Some("ack pending…"));
        assert_eq!(tracker.label(&load()), None);
        let toast = tracker.toasts().next().unwrap().clone();
        assert_eq!(toast.tone, ToastTone::Pending);
        assert_eq!(toast.title, "Acknowledging disk on db-01…");

        // Answered, but the snapshot doesn't show it yet: still pending.
        tracker.finish(1, &ok(1), &before, t0 + Duration::from_millis(300));
        assert_eq!(tracker.label(&disk()), Some("ack pending…"));
        let toast = tracker.toasts().next().unwrap().clone();
        assert_eq!(toast.tone, ToastTone::Success);
        assert_eq!(toast.title, "Acknowledged disk on db-01");
        assert!(toast.expires.is_some());

        // The acknowledgement arrives.
        assert!(tracker.settle(&snapshot(true, 100.), t0 + Duration::from_secs(1)));
        assert_eq!(tracker.label(&disk()), None);
        assert!(!tracker.has_running());
    }

    #[test]
    fn markers_without_a_visible_change_time_out() {
        let mut tracker = Tracker::default();
        let t0 = Instant::now();
        let snapshot = snapshot(false, 100.);
        tracker.start(1, &ack(vec![disk()]), &snapshot, t0);
        tracker.finish(1, &ok(1), &snapshot, t0);
        assert!(!tracker.settle(&snapshot, t0 + Duration::from_secs(14)));
        assert!(tracker.settle(&snapshot, t0 + Duration::from_secs(15)));
        // An action that's never answered goes after five minutes.
        tracker.start(2, &ack(vec![load()]), &snapshot, t0);
        assert!(!tracker.settle(&snapshot, t0 + Duration::from_mins(4)));
        assert!(tracker.settle(&snapshot, t0 + Duration::from_mins(5)));
    }

    #[test]
    fn checks_settle_with_a_newer_result() {
        let mut tracker = Tracker::default();
        let t0 = Instant::now();
        let check = spec(
            ObjectAction::CheckNow,
            Action::CheckNow { force: true },
            vec![disk(), load()],
        );
        tracker.start(7, &check, &snapshot(false, 100.), t0);
        assert_eq!(tracker.label(&load()), Some("checking…"));
        tracker.finish(7, &ok(2), &snapshot(false, 100.), t0);
        let toast = tracker.toasts().next().unwrap();
        assert_eq!(toast.title, "Rescheduled the check of 2 services");
        assert!(!tracker.settle(&snapshot(false, 100.), t0 + Duration::from_secs(30)));
        assert!(tracker.settle(&snapshot(false, 101.), t0 + Duration::from_secs(31)));
        assert_eq!(tracker.label(&disk()), None);
    }

    #[test]
    fn partial_failures_name_each_object_and_stay() {
        let mut tracker = Tracker::default();
        let t0 = Instant::now();
        let snapshot = snapshot(false, 100.);
        tracker.start(3, &ack(vec![disk(), load()]), &snapshot, t0);
        let outcome = ActionOutcome {
            ok: 1,
            failed: vec![(
                "db-01!load".to_owned(),
                "Service db-01!load is already acknowledged.".to_owned(),
            )],
            error: None,
        };
        tracker.finish(3, &outcome, &snapshot, t0);
        let toast = tracker.toasts().next().unwrap().clone();
        assert_eq!(toast.tone, ToastTone::Partial);
        assert_eq!(toast.title, "Acknowledged 1 of 2 services");
        assert_eq!(
            toast.lines,
            ["load on db-01: Service db-01!load is already acknowledged."]
        );
        assert_eq!(toast.expires, None, "failures stay until dismissed");
        assert_eq!(
            tracker.label(&load()),
            None,
            "the failed object's marker goes"
        );
        assert_eq!(tracker.label(&disk()), Some("ack pending…"));
        let failure = tracker.failure(&load()).unwrap();
        assert_eq!(failure.action, ObjectAction::Acknowledge);
        assert!(failure.reason.contains("already acknowledged"));
        assert!(tracker.dismiss_failure(&load()));
        assert!(tracker.failure(&load()).is_none());
        assert!(!tracker.expire(t0 + Duration::from_hours(1)));
        assert!(tracker.dismiss_toast(toast.id));
        assert_eq!(tracker.toasts().count(), 0);
    }

    #[test]
    fn a_failed_request_fails_every_object() {
        let mut tracker = Tracker::default();
        let t0 = Instant::now();
        let snapshot = snapshot(false, 100.);
        tracker.start(4, &ack(vec![disk(), load()]), &snapshot, t0);
        let outcome = ActionOutcome {
            error: Some("forbidden: Missing permission: actions/acknowledge-problem".to_owned()),
            ..ActionOutcome::default()
        };
        tracker.finish(4, &outcome, &snapshot, t0);
        let toast = tracker.toasts().next().unwrap();
        assert_eq!(toast.tone, ToastTone::Failed);
        assert_eq!(toast.title, "Couldn't acknowledge 2 services");
        assert!(toast.lines[0].contains("Missing permission"));
        assert!(tracker.failure(&disk()).is_some());
        assert!(tracker.failure(&load()).is_some());
        assert_eq!(tracker.label(&disk()), None);
        // A new action on the object forgets the old failure.
        tracker.start(5, &ack(vec![disk()]), &snapshot, t0);
        assert!(tracker.failure(&disk()).is_none());
    }

    #[test]
    fn removals_by_name_report_on_their_object() {
        let mut tracker = Tracker::default();
        let t0 = Instant::now();
        let mut snapshot = snapshot(false, 100.);
        let mut comments = BTreeMap::new();
        comments.insert(
            disk(),
            vec![Comment {
                name: "db-01!disk!c1".to_owned(),
                object: disk(),
                author: "a".to_owned(),
                text: "t".to_owned(),
                kind: CommentKind::User,
                entry_time: Timestamp::from_unix_seconds(1.),
                expire_time: None,
                persistent: false,
            }],
        );
        snapshot.comments = Arc::new(comments);
        let removal = ActionSpec {
            kind: ObjectAction::RemoveComment("db-01!disk!c1".to_owned()),
            action: Action::RemoveAllDowntimes,
            target: ActionTarget::Comment("db-01!disk!c1".to_owned()),
            objects: vec![disk()],
        };
        tracker.start(6, &removal, &snapshot, t0);
        assert_eq!(tracker.label(&disk()), Some("removing comment…"));
        let outcome = ActionOutcome {
            ok: 0,
            failed: vec![(
                "db-01!disk!c1".to_owned(),
                "Cannot remove non-existent comment object.".to_owned(),
            )],
            error: None,
        };
        tracker.finish(6, &outcome, &snapshot, t0);
        assert!(tracker.failure(&disk()).is_some(), "mapped to its object");
        let toast = tracker.toasts().next().unwrap();
        assert_eq!(toast.title, "Couldn't remove a comment of disk on db-01");
        assert_eq!(
            toast.lines,
            ["disk on db-01: Cannot remove non-existent comment object."]
        );
    }

    #[test]
    fn many_failures_are_summed_up() {
        let mut tracker = Tracker::default();
        let t0 = Instant::now();
        let objects: Vec<ObjectKey> = (0..10)
            .map(|index| ObjectKey::service("h", &format!("s{index}")))
            .collect();
        tracker.start(8, &ack(objects.clone()), &Snapshot::default(), t0);
        let outcome = ActionOutcome {
            ok: 3,
            failed: objects[3..]
                .iter()
                .map(|key| (key.full_name(), "x".repeat(300)))
                .collect(),
            error: None,
        };
        tracker.finish(8, &outcome, &Snapshot::default(), t0);
        let toast = tracker.toasts().next().unwrap();
        assert_eq!(toast.lines.len(), 7, "all kept, for copying");
        assert_eq!(toast.shown_lines().len(), MAX_FAILURE_LINES);
        assert_eq!(toast.hidden(), 3);
        assert!(toast.lines[0].chars().count() < 230, "long reasons are cut");
    }

    #[test]
    fn toasts_expire_and_make_room() {
        let mut tracker = Tracker::default();
        let t0 = Instant::now();
        let first = tracker.inform("Nothing to acknowledge".to_owned(), None, t0);
        for index in 0..MAX_TOASTS {
            tracker.inform(format!("info {index}"), Some("detail".to_owned()), t0);
        }
        assert_eq!(tracker.toasts().count(), MAX_TOASTS);
        assert!(
            tracker.toasts().all(|toast| toast.id != first),
            "the oldest went"
        );
        assert!(!tracker.expire(t0 + Duration::from_secs(7)));
        assert!(tracker.expire(t0 + Duration::from_secs(8)));
        assert_eq!(tracker.toasts().count(), 0);
        tracker.start(9, &ack(vec![disk()]), &Snapshot::default(), t0);
        tracker.clear();
        assert_eq!(tracker.toasts().count(), 0);
        assert!(!tracker.has_running());
        assert_eq!(tracker.label(&disk()), None);
        // An answer to an action from before is ignored.
        tracker.finish(9, &ok(1), &Snapshot::default(), t0);
        assert_eq!(tracker.toasts().count(), 0);
    }

    #[test]
    fn a_newer_action_on_the_object_keeps_its_marker() {
        let mut tracker = Tracker::default();
        let t0 = Instant::now();
        let snapshot = snapshot(false, 100.);
        tracker.start(1, &ack(vec![disk()]), &snapshot, t0);
        let check = spec(
            ObjectAction::CheckNow,
            Action::CheckNow { force: true },
            vec![disk()],
        );
        tracker.start(2, &check, &snapshot, t0);
        let outcome = ActionOutcome {
            error: Some("not connected to Icinga".to_owned()),
            ..ActionOutcome::default()
        };
        tracker.finish(1, &outcome, &snapshot, t0);
        assert_eq!(tracker.label(&disk()), Some("checking…"));
        assert_eq!(
            tracker.pending(&disk()),
            Some((&ObjectAction::CheckNow, "checking…"))
        );
    }
}
