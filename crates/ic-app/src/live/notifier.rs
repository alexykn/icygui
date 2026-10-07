//! The core's `Notifier` port: the core calls it on its own thread for
//! every notification that should show (not the silent ones); the intent
//! goes to the UI thread, which shows it on the desktop
//! (`super::desktop`).
//!
//! Every engine gets its own notifier, which tags its intents with the
//! engine's environment: an engine that is being replaced can still raise
//! some, and they must not be taken for the next environment's (a click
//! on *Acknowledge* would go to the wrong Icinga).

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use ic_core::ports::Notifier;
use ic_rules::NotificationIntent;

/// An intent and the environment whose engine raised it.
#[derive(Clone, Debug)]
pub(crate) struct Raised {
    /// The environment's id.
    pub(crate) environment: String,
    /// What to show.
    pub(crate) intent: NotificationIntent,
}

/// Hands one engine's intents from the core's thread to the UI thread.
#[derive(Debug)]
pub(crate) struct GpuiNotifier {
    sender: UnboundedSender<Raised>,
    environment: String,
}

impl GpuiNotifier {
    /// The channel every engine's notifier sends to, and the receiver the
    /// UI thread drains.
    pub(crate) fn channel() -> (UnboundedSender<Raised>, UnboundedReceiver<Raised>) {
        unbounded()
    }

    /// A notifier for the engine of `environment` (its id), sending to
    /// `sender`.
    pub(crate) fn new(sender: UnboundedSender<Raised>, environment: impl Into<String>) -> Self {
        Self {
            sender,
            environment: environment.into(),
        }
    }
}

impl Notifier for GpuiNotifier {
    fn notify(&self, intent: &NotificationIntent) {
        let raised = Raised {
            environment: self.environment.clone(),
            intent: intent.clone(),
        };
        if self.sender.unbounded_send(raised).is_err() {
            tracing::debug!(id = %intent.id, "the UI is gone; notification dropped");
        }
    }
}

#[cfg(test)]
mod tests {
    use ic_model::Timestamp;
    use ic_rules::Tone;

    use super::*;

    fn intent(body: &str) -> NotificationIntent {
        NotificationIntent {
            id: "x".to_owned(),
            object: None,
            title: "14 new problems in prod-cluster".to_owned(),
            subtitle: String::new(),
            body: body.to_owned(),
            tone: Tone::Info,
            sound: true,
            silent: false,
            silenced: None,
            at: Timestamp::from_unix_seconds(1_790_000_000.),
        }
    }

    #[test]
    fn the_port_hands_intents_to_the_receiver_with_their_environment() {
        let (sender, mut receiver) = GpuiNotifier::channel();
        let production = GpuiNotifier::new(sender.clone(), "prod");
        let staging = GpuiNotifier::new(sender, "staging");
        production.notify(&intent("body"));
        staging.notify(&intent("other"));
        let raised = receiver.try_recv().unwrap();
        assert_eq!(raised.intent.body, "body");
        assert_eq!(raised.environment, "prod");
        let raised = receiver.try_recv().unwrap();
        assert_eq!(raised.intent.body, "other");
        assert_eq!(raised.environment, "staging");
        drop(receiver);
        // A notifier whose UI is gone drops intents quietly.
        production.notify(&intent("later"));
    }
}
