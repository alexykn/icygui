//! The core's `Notifier` port: the core calls it on its own thread for
//! every notification that should show (not the silent ones); the intent
//! goes to the UI thread, which shows it on the desktop
//! (`super::desktop`).

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use ic_core::ports::Notifier;
use ic_rules::NotificationIntent;

/// Hands intents from the core's thread to the UI thread.
#[derive(Debug)]
pub(crate) struct GpuiNotifier {
    sender: UnboundedSender<NotificationIntent>,
}

impl GpuiNotifier {
    /// A notifier and the receiver the UI thread drains.
    pub(crate) fn new() -> (Self, UnboundedReceiver<NotificationIntent>) {
        let (sender, receiver) = unbounded();
        (Self { sender }, receiver)
    }
}

impl Notifier for GpuiNotifier {
    fn notify(&self, intent: &NotificationIntent) {
        if self.sender.unbounded_send(intent.clone()).is_err() {
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
            at: Timestamp::from_unix_seconds(1_790_000_000.),
        }
    }

    #[test]
    fn the_port_hands_intents_to_the_receiver() {
        let (notifier, mut receiver) = GpuiNotifier::new();
        notifier.notify(&intent("body"));
        let received_intent = receiver.try_recv().unwrap();
        assert_eq!(received_intent.body, "body");
        drop(receiver);
        // A notifier whose UI is gone drops intents quietly.
        notifier.notify(&intent("later"));
    }
}
