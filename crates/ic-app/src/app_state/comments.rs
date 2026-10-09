//! The comments sent from handling views (topic 17): sent through the
//! action path every comment takes (permissions, the tracker's markers and
//! toasts), and kept as drafts until the event stream shows them, or with
//! Icinga's reason when refused.

use std::time::Instant;

use ic_model::{Action, ObjectKey, Timestamp};

use super::AppState;
use crate::actions::ObjectAction;
use crate::comments::drafts::{Draft, NewDraft};
use crate::operate::ActionSpec;

impl AppState {
    /// Sends `text` as a comment on `object` by the environment's author,
    /// as a draft the handling views show until the snapshot holds it (or
    /// with the reason it couldn't be sent). Returns the draft's id;
    /// `None` without an environment or text.
    pub(crate) fn send_comment(&mut self, object: &ObjectKey, text: &str) -> Option<u64> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        let environment = self.active_environment_id()?.to_owned();
        let author = self.author().to_owned();
        let id = self.drafts.add(
            &NewDraft {
                environment: &environment,
                object,
                author: &author,
                text,
                at: Timestamp::now(),
            },
            &self.engine.snapshot,
        );
        self.submit_draft(id, object.clone(), text.to_owned());
        Some(id)
    }

    /// Sends refused draft `id` again (*retry*).
    pub(crate) fn retry_comment(&mut self, id: u64) {
        let snapshot = std::sync::Arc::clone(&self.engine.snapshot);
        if let Some((object, text)) = self.drafts.resend(id, &snapshot, Timestamp::now()) {
            self.submit_draft(id, object, text);
        }
    }

    /// Forgets refused draft `id` (*discard*). Returns whether it was
    /// there.
    pub(crate) fn discard_comment(&mut self, id: u64) -> bool {
        self.drafts.remove(id)
    }

    /// The drafts of the environment on screen that its snapshot doesn't
    /// show yet, oldest first.
    pub(crate) fn comment_drafts(&self) -> Vec<&Draft> {
        let Some(environment) = self.active_environment_id() else {
            return Vec::new();
        };
        self.drafts
            .of(environment)
            .filter(|draft| draft.refusal().is_some() || !draft.shown_in(&self.engine.snapshot))
            .collect()
    }

    /// Draft `id`.
    pub(crate) fn comment_draft(&self, id: u64) -> Option<&Draft> {
        self.drafts.get(id)
    }

    /// Drops the drafts the snapshot on screen shows. Returns whether any
    /// went.
    pub(super) fn settle_drafts(&mut self, now: Instant) -> bool {
        let Some(environment) = self.active_environment_id().map(str::to_owned) else {
            return false;
        };
        self.drafts
            .settle(&environment, &self.engine.snapshot, now)
    }

    /// Hands draft `id` to the action path; a refusal there (no
    /// permission, not connected) keeps it with the reason.
    fn submit_draft(&mut self, id: u64, object: ObjectKey, text: String) {
        let spec = ActionSpec::for_objects(
            ObjectAction::AddComment,
            Action::AddComment { text, expiry: None },
            vec![object],
        );
        match self.submit(spec) {
            Ok(action) => self.drafts.sent(id, action),
            Err(reason) => self.drafts.refuse(id, reason),
        }
    }
}
