//! The state's half of operator actions (`crate::operate`): requests from
//! the views, actions sent to the core, their answers, the markers and
//! toasts.

use std::time::Instant;

use ic_core::{ActionOutcome, Command};
use ic_model::ObjectKey;

use super::AppState;
use crate::actions::{ActionRequest, ObjectAction};
use crate::operate::ActionSpec;
use crate::operate::tracker::{Failure, Toast};

/// What `submit` says without a connection: nothing was sent, and the
/// dialog stays open so nothing typed is lost.
pub(crate) const NOT_CONNECTED: &str =
    "Not connected to Icinga, so nothing was sent. Try again once the connection is back.";

impl AppState {
    /// Records an action the user asked for, unless the API user may not
    /// run it (ENV-09): then the reason comes back and shows as a toast.
    /// The workspace picks the request up ([`AppState::take_request`]).
    ///
    /// # Errors
    ///
    /// Why the API user may not run the action.
    pub(crate) fn request(&mut self, request: ActionRequest) -> Result<(), String> {
        let targets: Vec<String> = request.targets.iter().map(ObjectKey::full_name).collect();
        if let Some(denial) = self.action_denial(&request.action) {
            tracing::info!(action = request.action.label(), ?targets, %denial, "action refused");
            self.last_denial = Some(denial.clone());
            self.tracker.inform(
                format!("Can't {}", request.action.label()),
                Some(denial.clone()),
                Instant::now(),
            );
            return Err(denial);
        }
        if request.targets.is_empty() {
            return Ok(());
        }
        tracing::info!(
            action = request.action.label(),
            ?targets,
            "action requested"
        );
        self.last_request = Some(request.clone());
        self.requested = Some((request, None));
        Ok(())
    }

    /// Records an action asked for in environment `environment`, which
    /// need not be the active one: a desktop notification's *Acknowledge*
    /// goes to its own environment's engine without switching (A1). Its
    /// API user's permissions decide, as in [`AppState::request`].
    ///
    /// # Errors
    ///
    /// Why the API user may not run the action, or that the environment is
    /// gone.
    pub(crate) fn request_in(
        &mut self,
        environment: &str,
        request: ActionRequest,
    ) -> Result<(), String> {
        if self.is_active(environment) {
            return self.request(request);
        }
        let Some(name) = self
            .config
            .environment(environment)
            .map(|found| found.name.clone())
        else {
            return Err("That environment was removed.".to_owned());
        };
        if let Some(denial) = self.action_denial_in(environment, &request.action) {
            tracing::info!(action = request.action.label(), %denial, "action refused");
            self.last_denial = Some(denial.clone());
            self.tracker.inform(
                format!("Can't {} in {name}", request.action.label()),
                Some(denial.clone()),
                Instant::now(),
            );
            return Err(denial);
        }
        if request.targets.is_empty() {
            return Ok(());
        }
        tracing::info!(action = request.action.label(), environment = %name, "action requested in another environment");
        self.last_request = Some(request.clone());
        self.requested = Some((request, Some(environment.to_owned())));
        Ok(())
    }

    /// The action asked for and not yet picked up, with its environment
    /// (`None`: the active one).
    pub(crate) fn take_request(&mut self) -> Option<(ActionRequest, Option<String>)> {
        self.requested.take()
    }

    /// Drops a waiting request meant for the environment on screen (it
    /// changed: the request was for the previous one); one bound to its
    /// own environment stays. Returns the dropped one.
    pub(crate) fn drop_unbound_request(&mut self) -> Option<ActionRequest> {
        if matches!(self.requested, Some((_, None))) {
            return self.requested.take().map(|(request, _)| request);
        }
        None
    }

    /// Whether a request waits to be picked up.
    pub(crate) fn has_request(&self) -> bool {
        self.requested.is_some()
    }

    /// The last action the user asked for.
    #[cfg(test)]
    pub(crate) fn last_request(&self) -> Option<&ActionRequest> {
        self.last_request.as_ref()
    }

    /// Why the last action asked for was refused.
    #[cfg(test)]
    pub(crate) fn last_denial(&self) -> Option<&str> {
        self.last_denial.as_deref()
    }

    /// Sends `spec` to the core and starts tracking it. Returns its id.
    ///
    /// # Errors
    ///
    /// Why nothing was sent: the API user may not run it, or there is no
    /// connection to Icinga (the caller keeps its dialog open).
    pub(crate) fn submit(&mut self, spec: ActionSpec) -> Result<u64, String> {
        if let Some(denial) = self.action_denial(&spec.kind) {
            return Err(denial);
        }
        if spec.objects.is_empty() {
            return Err("Nothing to do: none of the objects qualifies.".to_owned());
        }
        if self.engine.core.is_none() || !self.engine.connection.is_connected() {
            return Err(NOT_CONNECTED.to_owned());
        }
        self.last_action_id += 1;
        let id = self.last_action_id;
        tracing::info!(
            id,
            action = spec.action.api_name(),
            objects = spec.objects.len(),
            "sending action"
        );
        self.tracker
            .start(id, &spec, &self.engine.snapshot, Instant::now());
        self.send(Command::Action {
            id,
            target: spec.target,
            action: spec.action,
        });
        Ok(id)
    }

    /// Sends `spec` to environment `environment`'s engine (the active one,
    /// or another: an acknowledgement from its desktop notification). An
    /// action in another environment gets a toast, no markers: its rows
    /// aren't on screen. Returns its id.
    ///
    /// # Errors
    ///
    /// Why nothing was sent: the environment is gone, the API user may not
    /// run it, or there is no connection to that Icinga.
    pub(crate) fn submit_in(&mut self, environment: &str, spec: ActionSpec) -> Result<u64, String> {
        if self.is_active(environment) {
            return self.submit(spec);
        }
        let Some(name) = self
            .config
            .environment(environment)
            .map(|found| found.name.clone())
        else {
            return Err("That environment was removed; nothing was sent.".to_owned());
        };
        if let Some(denial) = self.action_denial_in(environment, &spec.kind) {
            return Err(denial);
        }
        if spec.objects.is_empty() {
            return Err("Nothing to do: none of the objects qualifies.".to_owned());
        }
        let connected = self
            .slot(environment)
            .is_some_and(|slot| slot.core.is_some() && slot.connection.is_connected());
        if !connected {
            return Err(NOT_CONNECTED.to_owned());
        }
        self.last_action_id += 1;
        let id = self.last_action_id;
        tracing::info!(
            id,
            action = spec.action.api_name(),
            objects = spec.objects.len(),
            environment = %name,
            "sending action to another environment"
        );
        self.tracker.start_elsewhere(id, &spec, &name);
        if let Some(slot) = self.slot(environment) {
            slot.send(Command::Action {
                id,
                target: spec.target,
                action: spec.action,
            });
        }
        Ok(id)
    }

    /// The core answered action `id`.
    pub(super) fn action_finished(&mut self, id: u64, outcome: &ActionOutcome) {
        tracing::info!(
            id,
            ok = outcome.ok,
            failed = outcome.failed.len(),
            error = outcome.error.as_deref().unwrap_or(""),
            "action finished"
        );
        self.tracker
            .finish(id, outcome, &self.engine.snapshot, Instant::now());
    }

    /// The marker for `object` while an action on it is in flight or
    /// settling (`ack pending`).
    pub(crate) fn pending_label(&self, object: &ObjectKey) -> Option<&'static str> {
        self.tracker.label(object)
    }

    /// The action in flight or settling on `object`, with its marker.
    pub(crate) fn pending_action(
        &self,
        object: &ObjectKey,
    ) -> Option<(&ObjectAction, &'static str)> {
        self.tracker.pending(object)
    }

    /// The last failed action on `object`, until dismissed or another
    /// action on it.
    pub(crate) fn action_failure(&self, object: &ObjectKey) -> Option<&Failure> {
        self.tracker.failure(object)
    }

    /// Forgets `object`'s failed action. Returns whether there was one.
    pub(crate) fn dismiss_action_failure(&mut self, object: &ObjectKey) -> bool {
        self.tracker.dismiss_failure(object)
    }

    /// The toasts, oldest first.
    pub(crate) fn toasts(&self) -> impl Iterator<Item = &Toast> {
        self.tracker.toasts()
    }

    /// Hides a toast. Returns whether it was shown.
    pub(crate) fn dismiss_toast(&mut self, id: u64) -> bool {
        self.tracker.dismiss_toast(id)
    }

    /// Shows a toast that needs no answer.
    pub(crate) fn inform(&mut self, title: impl Into<String>, detail: Option<String>) {
        self.tracker.inform(title.into(), detail, Instant::now());
    }

    /// Lets toasts and markers whose time is up go. Returns whether
    /// anything changed.
    pub(crate) fn tick_actions(&mut self, now: Instant) -> bool {
        let expired = self.tracker.expire(now);
        let settled = self.tracker.settle(&self.engine.snapshot, now);
        expired || settled
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use ic_core::ApiInfo;
    use ic_model::{Action, ActionTarget, Timestamp};

    use super::*;
    use crate::app_state::testing::Recorder;
    use crate::operate::tracker::ToastTone;

    fn connected() -> (AppState, Recorder) {
        let mut state = AppState::fixture(Timestamp::from_unix_seconds(1_790_000_000.));
        let recorder = Recorder::default();
        state.set_core(Box::new(recorder.clone()));
        (state, recorder)
    }

    fn check(objects: Vec<ObjectKey>) -> ActionSpec {
        ActionSpec::for_objects(
            ObjectAction::CheckNow,
            Action::CheckNow { force: true },
            objects,
        )
    }

    fn replication() -> ObjectKey {
        ObjectKey::service("db-prod-03", "postgres-replication")
    }

    #[test]
    fn requests_wait_for_the_workspace() {
        let (mut state, _) = connected();
        let request = ActionRequest {
            action: ObjectAction::Acknowledge,
            targets: vec![replication()],
            review: false,
        };
        assert!(state.request(request.clone()).is_ok());
        assert!(state.has_request());
        assert_eq!(state.take_request(), Some((request.clone(), None)));
        assert_eq!(state.take_request(), None);
        assert_eq!(state.last_request(), Some(&request));
        // Nothing to act on: nothing to pick up.
        assert!(
            state
                .request(ActionRequest {
                    action: ObjectAction::CheckNow,
                    targets: Vec::new(),
                    review: false,
                })
                .is_ok()
        );
        assert!(!state.has_request());
    }

    #[test]
    fn refused_requests_explain_themselves_in_a_toast() {
        let (mut state, _) = connected();
        state.set_permissions(Some(ApiInfo {
            user: "viewer".to_owned(),
            permissions: vec!["objects/query/*".to_owned()],
            version: "v2.15.6".to_owned(),
        }));
        let error = state
            .request(ActionRequest {
                action: ObjectAction::RunCommand,
                targets: vec![replication()],
                review: false,
            })
            .unwrap_err();
        assert!(error.contains("actions/execute-command"), "{error}");
        assert!(!state.has_request());
        let toast = state.toasts().next().unwrap();
        assert_eq!(toast.tone, ToastTone::Info);
        assert_eq!(toast.title, "Can't run command");
        assert_eq!(toast.lines, [error]);
        // Submitting directly is refused too.
        assert!(state.submit(check(vec![replication()])).is_err());
    }

    #[test]
    fn submitting_sends_one_command_and_tracks_it() {
        let (mut state, recorder) = connected();
        let id = state.submit(check(vec![replication()])).unwrap();
        assert_eq!(id, 1);
        assert_eq!(recorder.sent(), ["Action(1, reschedule-check)"]);
        assert_eq!(
            recorder.actions(),
            [(
                1,
                ActionTarget::Objects(vec![replication()]),
                Action::CheckNow { force: true }
            )]
        );
        assert_eq!(state.pending_label(&replication()), Some("checking"));
        assert_eq!(state.toasts().count(), 1);

        // The answer: a per-object failure.
        state.apply(ic_core::CoreEvent::ActionFinished {
            id,
            outcome: ActionOutcome {
                ok: 0,
                failed: vec![(replication().full_name(), "No objects found.".to_owned())],
                error: None,
            },
        });
        assert_eq!(state.pending_label(&replication()), None);
        assert_eq!(
            state.action_failure(&replication()).unwrap().reason,
            "No objects found."
        );
        assert!(state.dismiss_action_failure(&replication()));
        let toast = state.toasts().next().unwrap().clone();
        assert_eq!(toast.tone, ToastTone::Failed);
        assert!(state.dismiss_toast(toast.id));
        // Ids keep counting.
        assert_eq!(state.submit(check(vec![replication()])), Ok(2));
    }

    #[test]
    fn nothing_is_sent_without_a_connection() {
        let mut state = AppState::fixture(Timestamp::from_unix_seconds(1_790_000_000.));
        assert_eq!(
            state.submit(check(vec![replication()])),
            Err(NOT_CONNECTED.to_owned()),
            "no core"
        );
        let (mut state, recorder) = connected();
        state.set_connection_lost();
        assert_eq!(
            state.submit(check(vec![replication()])),
            Err(NOT_CONNECTED.to_owned())
        );
        assert!(recorder.sent().is_empty());
        assert!(
            state
                .submit(ActionSpec {
                    kind: ObjectAction::CheckNow,
                    action: Action::CheckNow { force: true },
                    target: ActionTarget::Objects(Vec::new()),
                    objects: Vec::new(),
                })
                .is_err()
        );
    }

    #[test]
    fn switching_environments_forgets_the_actions() {
        let (mut state, _) = connected();
        state.submit(check(vec![replication()])).unwrap();
        state.reset_connection();
        assert_eq!(state.pending_label(&replication()), None);
        assert_eq!(state.toasts().count(), 0);
    }

    #[test]
    fn ticks_expire_toasts() {
        let (mut state, _) = connected();
        state.inform("Nothing to acknowledge", None);
        assert!(!state.tick_actions(Instant::now()));
        assert!(state.tick_actions(Instant::now() + Duration::from_secs(9)));
        assert_eq!(state.toasts().count(), 0);
    }
}
