//! A recording core link for tests of the outbound half.

use std::cell::RefCell;
use std::rc::Rc;

use ic_core::Command;
use ic_model::{Action, ActionTarget, ObjectKey};

use super::CoreLink;

/// Records the commands sent to the core.
#[derive(Debug, Default, Clone)]
pub(crate) struct Recorder {
    sent: Rc<RefCell<Vec<String>>>,
    actions: Rc<RefCell<Vec<(u64, ActionTarget, Action)>>>,
    pub(crate) stopped: Rc<RefCell<bool>>,
}

impl Recorder {
    /// The commands sent so far, by name.
    pub(crate) fn sent(&self) -> Vec<String> {
        self.sent.borrow().clone()
    }

    /// The actions sent so far: id, target, action.
    pub(crate) fn actions(&self) -> Vec<(u64, ActionTarget, Action)> {
        self.actions.borrow().clone()
    }

    /// Forgets them.
    pub(crate) fn clear(&self) {
        self.sent.borrow_mut().clear();
        self.actions.borrow_mut().clear();
    }
}

impl CoreLink for Recorder {
    fn send(&self, command: Command) {
        let name = match &command {
            Command::Refresh => "Refresh".to_owned(),
            Command::UpdateEnvironment(environment) => {
                format!("UpdateEnvironment({})", environment.name)
            }
            Command::Hydrate(keys) => format!(
                "Hydrate({})",
                keys.iter()
                    .map(ObjectKey::full_name)
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            Command::Action { id, target, action } => {
                self.actions
                    .borrow_mut()
                    .push((*id, target.clone(), action.clone()));
                format!("Action({id}, {})", action.api_name())
            }
            other => format!("{other:?}"),
        };
        self.sent.borrow_mut().push(name);
    }

    fn shutdown_in_background(self: Box<Self>) -> futures::channel::oneshot::Receiver<()> {
        *self.stopped.borrow_mut() = true;
        let (done, stopped) = futures::channel::oneshot::channel();
        let _ = done.send(());
        stopped
    }
}
