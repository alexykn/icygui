//! `Command::Action`: runs an action with the environment's author and
//! reports per-object failures; succeeded targets are re-queried so their
//! new state shows within a second (events usually bring it first).

use ic_api::{ActionResult, ApiError, Client};
use ic_model::{Action, ActionTarget, ObjectKey};
use tokio::time::Instant;

use super::{Engine, Internal};
use crate::command::{ActionOutcome, CoreEvent};

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
        let dirty = self.action_objects(&target);
        let runs = self.plan(action, target);
        let tx = self.internal_tx.clone();
        // Not a session task: an action whose request went out finishes
        // and reports even if the stream drops meanwhile.
        tokio::spawn(async move {
            let outcome = execute(&client, runs, &author).await;
            let _ = tx.send(Internal::ActionDone { id, dirty, outcome });
        });
    }

    pub(super) fn on_action_done(
        &mut self,
        id: u64,
        dirty: Vec<ObjectKey>,
        outcome: ActionOutcome,
    ) {
        tracing::info!(
            id,
            ok = outcome.ok,
            failed = outcome.failed.len(),
            error = outcome.error.as_deref().unwrap_or(""),
            "action finished"
        );
        if outcome.ok > 0 {
            self.fetch.mark_urgent(dirty, Instant::now());
        }
        self.emit(CoreEvent::ActionFinished { id, outcome });
        self.publish_changes();
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

/// Runs the planned requests and sums up their results.
async fn execute(
    client: &Client,
    runs: Vec<(Action, ActionTarget)>,
    author: &str,
) -> ActionOutcome {
    let mut outcome = ActionOutcome::default();
    let mut answered = false;
    for (action, target) in runs {
        match client.run_action(&action, &target, author).await {
            Ok(results) => {
                answered = true;
                count(&mut outcome, results);
            }
            Err(error) if !answered => {
                outcome.error = Some(describe(&error));
                return outcome;
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
    }
    outcome
}

fn count(outcome: &mut ActionOutcome, results: Vec<ActionResult>) {
    for result in results {
        if result.is_success() {
            outcome.ok += 1;
        } else {
            outcome
                .failed
                .push((result.target.unwrap_or_default(), result.status));
        }
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
