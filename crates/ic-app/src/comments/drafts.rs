//! The comments sent from a handling view, until Icinga's event stream
//! shows them (topic 17): each is a *draft* with the text as typed, the
//! object it is for and how far it got. While it is on its way it shows
//! dimmed in the thread with `sending…`; once a snapshot holds the new
//! comment it goes (the real entry takes its place, so the object never
//! shows the comment twice). Refused, it keeps the text with Icinga's
//! reason until the operator retries or discards it: a comment is never
//! lost.
//!
//! Pure: times come in as arguments, snapshots as references, so it's
//! tested without a window or a core.

use std::time::{Duration, Instant};

use ic_core::ActionOutcome;
use ic_core::snapshot::Snapshot;
use ic_model::{ObjectKey, Timestamp};

/// An accepted comment whose event never came (a stream that missed it)
/// stops showing as pending after this long: Icinga has it, and the next
/// reconcile brings it.
const ACCEPTED_TIMEOUT: Duration = Duration::from_mins(1);

/// How far a draft got.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    /// On its way: action `action` was sent (`None`: being sent again).
    Sending {
        /// The action carrying it.
        action: Option<u64>,
    },
    /// Icinga said yes; waiting for its event to show it.
    Accepted {
        /// When the answer came.
        since: Instant,
    },
    /// Not sent: why (Icinga's reason, or the app's).
    Refused(String),
}

/// A comment sent from a handling view.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Draft {
    /// Its id (never reused).
    pub(crate) id: u64,
    /// The environment it went to, by id.
    pub(crate) environment: String,
    /// The host or service it is on.
    pub(crate) object: ObjectKey,
    /// Who wrote it (the environment's author).
    pub(crate) author: String,
    /// What was typed (trimmed).
    pub(crate) text: String,
    /// When it was sent (last).
    pub(crate) at: Timestamp,
    /// How far it got.
    pub(crate) phase: Phase,
    /// The object's comments when it was sent: a new one with its text
    /// confirms it.
    before: Vec<String>,
}

impl Draft {
    /// Whether it was refused (its reason).
    pub(crate) fn refusal(&self) -> Option<&str> {
        match &self.phase {
            Phase::Refused(reason) => Some(reason),
            Phase::Sending { .. } | Phase::Accepted { .. } => None,
        }
    }

    /// Whether `snapshot` holds it: a comment of its object that wasn't
    /// there when it was sent, with its text.
    pub(crate) fn shown_in(&self, snapshot: &Snapshot) -> bool {
        snapshot.comments.get(&self.object).is_some_and(|comments| {
            comments.iter().any(|comment| {
                comment.text.trim() == self.text && !self.before.contains(&comment.name)
            })
        })
    }
}

/// The comments of `object` in `snapshot`, by name.
fn comment_names(snapshot: &Snapshot, object: &ObjectKey) -> Vec<String> {
    snapshot
        .comments
        .get(object)
        .map(|comments| {
            comments
                .iter()
                .map(|comment| comment.name.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// Every draft, oldest first.
#[derive(Clone, Debug, Default)]
pub(crate) struct Drafts {
    drafts: Vec<Draft>,
    last_id: u64,
}

/// What a new draft is.
pub(crate) struct NewDraft<'a> {
    pub(crate) environment: &'a str,
    pub(crate) object: &'a ObjectKey,
    pub(crate) author: &'a str,
    pub(crate) text: &'a str,
    pub(crate) at: Timestamp,
}

impl Drafts {
    /// Records a comment about to be sent (`Sending`, no action yet).
    /// Returns its id.
    pub(crate) fn add(&mut self, new: &NewDraft<'_>, snapshot: &Snapshot) -> u64 {
        self.last_id += 1;
        self.drafts.push(Draft {
            id: self.last_id,
            environment: new.environment.to_owned(),
            object: new.object.clone(),
            author: new.author.to_owned(),
            text: new.text.trim().to_owned(),
            at: new.at,
            phase: Phase::Sending { action: None },
            before: comment_names(snapshot, new.object),
        });
        self.last_id
    }

    /// Draft `id`.
    pub(crate) fn get(&self, id: u64) -> Option<&Draft> {
        self.drafts.iter().find(|draft| draft.id == id)
    }

    /// The drafts sent to `environment`, oldest first.
    pub(crate) fn of<'a>(&'a self, environment: &'a str) -> impl Iterator<Item = &'a Draft> {
        self.drafts
            .iter()
            .filter(move |draft| draft.environment == environment)
    }

    /// Draft `id` went out as action `action`.
    pub(crate) fn sent(&mut self, id: u64, action: u64) {
        if let Some(draft) = self.drafts.iter_mut().find(|draft| draft.id == id) {
            draft.phase = Phase::Sending {
                action: Some(action),
            };
        }
    }

    /// Draft `id` couldn't be sent, or Icinga refused it.
    pub(crate) fn refuse(&mut self, id: u64, reason: String) {
        if let Some(draft) = self.drafts.iter_mut().find(|draft| draft.id == id) {
            draft.phase = Phase::Refused(reason);
        }
    }

    /// Draft `id` goes again (*retry*): sending, as of `at`, against the
    /// comments `snapshot` has now. Returns its text and object.
    pub(crate) fn resend(
        &mut self,
        id: u64,
        snapshot: &Snapshot,
        at: Timestamp,
    ) -> Option<(ObjectKey, String)> {
        let draft = self.drafts.iter_mut().find(|draft| draft.id == id)?;
        draft.phase = Phase::Sending { action: None };
        draft.at = at;
        draft.before = comment_names(snapshot, &draft.object);
        Some((draft.object.clone(), draft.text.clone()))
    }

    /// Forgets draft `id` (*discard*). Returns whether it was there.
    pub(crate) fn remove(&mut self, id: u64) -> bool {
        let before = self.drafts.len();
        self.drafts.retain(|draft| draft.id != id);
        self.drafts.len() != before
    }

    /// Takes the core's answer to action `action`: a refusal keeps the
    /// draft with the reason; a yes waits for the event. Returns whether a
    /// draft was waiting for it.
    pub(crate) fn finish(&mut self, action: u64, outcome: &ActionOutcome, now: Instant) -> bool {
        let Some(draft) = self.drafts.iter_mut().find(|draft| {
            draft.phase
                == Phase::Sending {
                    action: Some(action),
                }
        }) else {
            return false;
        };
        draft.phase = if let Some(error) = &outcome.error {
            Phase::Refused(error.clone())
        } else if let Some((_, reason)) = outcome.failed.first() {
            Phase::Refused(reason.clone())
        } else if outcome.ok == 0 {
            Phase::Refused("Icinga added nothing".to_owned())
        } else {
            Phase::Accepted { since: now }
        };
        true
    }

    /// Drops the drafts of `environment` that `snapshot` shows (the real
    /// comment is there now) and the accepted ones whose event is long
    /// overdue. Returns whether any went.
    pub(crate) fn settle(&mut self, environment: &str, snapshot: &Snapshot, now: Instant) -> bool {
        let before = self.drafts.len();
        self.drafts.retain(|draft| {
            if draft.environment != environment {
                return true;
            }
            match &draft.phase {
                Phase::Refused(_) => true,
                Phase::Sending { .. } => !draft.shown_in(snapshot),
                Phase::Accepted { since } => {
                    !draft.shown_in(snapshot)
                        && now.saturating_duration_since(*since) < ACCEPTED_TIMEOUT
                }
            }
        });
        self.drafts.len() != before
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use ic_model::{Comment, CommentKind, HostName};

    use super::*;

    fn host() -> ObjectKey {
        ObjectKey::Host {
            name: HostName::from("k8s-node-11"),
        }
    }

    fn comment(name: &str, text: &str) -> Comment {
        Comment {
            name: name.to_owned(),
            object: host(),
            author: "n.weber".to_owned(),
            text: text.to_owned(),
            kind: CommentKind::User,
            entry_time: Timestamp::from_unix_seconds(100.),
            expire_time: None,
            persistent: false,
        }
    }

    fn snapshot(comments: Vec<Comment>) -> Snapshot {
        let mut map = BTreeMap::new();
        map.insert(host(), comments);
        Snapshot {
            comments: Arc::new(map),
            ..Snapshot::default()
        }
    }

    fn new_draft<'a>(object: &'a ObjectKey, text: &'a str) -> NewDraft<'a> {
        NewDraft {
            environment: "prod",
            object,
            author: "n.weber",
            text,
            at: Timestamp::from_unix_seconds(200.),
        }
    }

    fn ok() -> ActionOutcome {
        ActionOutcome {
            ok: 1,
            failed: Vec::new(),
            error: None,
        }
    }

    #[test]
    fn a_draft_waits_for_its_comment_to_show() {
        let object = host();
        // The same text was written before: only a new comment confirms.
        let old = snapshot(vec![comment("k8s-node-11!a", "Hardware swap at 03:00")]);
        let mut drafts = Drafts::default();
        let id = drafts.add(&new_draft(&object, "  Hardware swap at 03:00 "), &old);
        assert_eq!(drafts.get(id).unwrap().text, "Hardware swap at 03:00");
        drafts.sent(id, 7);
        let now = Instant::now();
        assert!(drafts.finish(7, &ok(), now));
        assert!(matches!(
            drafts.get(id).unwrap().phase,
            Phase::Accepted { .. }
        ));
        assert!(!drafts.settle("prod", &old, now), "the old one isn't it");
        assert!(!drafts.finish(7, &ok(), now), "answered once");

        let confirmed = snapshot(vec![
            comment("k8s-node-11!a", "Hardware swap at 03:00"),
            comment("k8s-node-11!b", "Hardware swap at 03:00"),
        ]);
        assert!(
            !drafts.settle("staging", &confirmed, now),
            "another environment"
        );
        assert!(drafts.settle("prod", &confirmed, now));
        assert!(drafts.get(id).is_none());
    }

    #[test]
    fn the_event_may_come_before_the_answer() {
        let object = host();
        let mut drafts = Drafts::default();
        let id = drafts.add(&new_draft(&object, "on it"), &snapshot(Vec::new()));
        drafts.sent(id, 3);
        let shown = snapshot(vec![comment("k8s-node-11!x", "on it")]);
        assert!(drafts.settle("prod", &shown, Instant::now()));
        assert!(!drafts.finish(3, &ok(), Instant::now()));
    }

    #[test]
    fn refusals_keep_the_text_until_retried_or_discarded() {
        let object = host();
        let empty = snapshot(Vec::new());
        let mut drafts = Drafts::default();
        let id = drafts.add(&new_draft(&object, "swap"), &empty);
        drafts.sent(id, 1);
        let refused = ActionOutcome {
            ok: 0,
            failed: Vec::new(),
            error: Some("403, no permission for comments".to_owned()),
        };
        drafts.finish(1, &refused, Instant::now());
        let draft = drafts.get(id).unwrap();
        assert_eq!(draft.refusal(), Some("403, no permission for comments"));
        // Long after, still there.
        assert!(!drafts.settle("prod", &empty, Instant::now() + Duration::from_hours(1)));

        let (again, text) = drafts
            .resend(id, &empty, Timestamp::from_unix_seconds(300.))
            .unwrap();
        assert_eq!((again, text.as_str()), (object.clone(), "swap"));
        assert_eq!(
            drafts.get(id).unwrap().phase,
            Phase::Sending { action: None }
        );
        drafts.sent(id, 2);
        let partial = ActionOutcome {
            ok: 0,
            failed: vec![("k8s-node-11".to_owned(), "No objects found.".to_owned())],
            error: None,
        };
        drafts.finish(2, &partial, Instant::now());
        assert_eq!(drafts.get(id).unwrap().refusal(), Some("No objects found."));
        assert!(drafts.remove(id));
        assert!(!drafts.remove(id));
        assert_eq!(drafts.of("prod").count(), 0);
    }

    #[test]
    fn an_accepted_comment_without_its_event_stops_waiting() {
        let object = host();
        let empty = snapshot(Vec::new());
        let mut drafts = Drafts::default();
        let id = drafts.add(&new_draft(&object, "late"), &empty);
        drafts.sent(id, 9);
        let now = Instant::now();
        drafts.finish(9, &ok(), now);
        assert!(!drafts.settle("prod", &empty, now + Duration::from_secs(30)));
        assert!(drafts.settle("prod", &empty, now + ACCEPTED_TIMEOUT));
    }
}
