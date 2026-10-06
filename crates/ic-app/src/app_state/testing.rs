//! A recording core link for tests of the outbound half.

use std::cell::RefCell;
use std::rc::Rc;

use ic_core::Command;
use ic_model::ObjectKey;

use super::CoreLink;

/// Records the commands sent to the core.
#[derive(Debug, Default, Clone)]
pub(crate) struct Recorder {
    sent: Rc<RefCell<Vec<String>>>,
    pub(crate) stopped: Rc<RefCell<bool>>,
}

impl Recorder {
    /// The commands sent so far, by name.
    pub(crate) fn sent(&self) -> Vec<String> {
        self.sent.borrow().clone()
    }

    /// Forgets them.
    pub(crate) fn clear(&self) {
        self.sent.borrow_mut().clear();
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
            other => format!("{other:?}"),
        };
        self.sent.borrow_mut().push(name);
    }

    fn shutdown(self: Box<Self>) {
        *self.stopped.borrow_mut() = true;
    }
}
