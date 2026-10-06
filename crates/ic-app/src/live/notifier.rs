//! The core's `Notifier` port, through GPUI's system notifications (XDG
//! notifications on Linux, `UNUserNotificationCenter` from the app bundle
//! on macOS; docs/spikes.md).
//!
//! The core calls the port on its own thread; the intent is handed to the
//! UI thread, which posts it. Clicking a notification opens its object.

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use gpui::{SharedString, SystemNotification, SystemNotificationAction};
use ic_core::ports::Notifier;
use ic_rules::NotificationIntent;

/// The action id of the notification's "Open" button.
pub(crate) const OPEN_ACTION: &str = "open";

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

/// The system notification for `intent`: its title, the body with the
/// matching dashboard under it, and an "Open" button. The tag is the
/// intent's id, so a click can be traced back to its object.
pub(crate) fn system_notification(intent: &NotificationIntent) -> SystemNotification {
    let body = match (intent.body.trim(), intent.subtitle.trim()) {
        ("", subtitle) => subtitle.to_owned(),
        (body, "") => body.to_owned(),
        (body, subtitle) => format!("{body}\n{subtitle}"),
    };
    SystemNotification {
        tag: SharedString::from(intent.id.clone()),
        title: SharedString::from(intent.title.clone()),
        body: SharedString::from(body),
        actions: if intent.object.is_some() {
            vec![SystemNotificationAction {
                id: OPEN_ACTION.into(),
                label: "Open".into(),
            }]
        } else {
            Vec::new()
        },
    }
}

#[cfg(test)]
mod tests {
    use ic_model::ObjectKey;
    use ic_rules::Tone;

    use super::*;

    fn intent(object: Option<ObjectKey>, body: &str, subtitle: &str) -> NotificationIntent {
        NotificationIntent {
            id: "db-prod-03!postgres-replication:critical:1790000000".to_owned(),
            object,
            title: "CRITICAL · postgres-replication on db-prod-03".to_owned(),
            subtitle: subtitle.to_owned(),
            body: body.to_owned(),
            tone: Tone::Critical,
            sound: true,
            silent: false,
            at: ic_model::Timestamp::from_unix_seconds(1_790_000_000.),
        }
    }

    #[test]
    fn notifications_carry_title_body_and_dashboard() {
        let notification = system_notification(&intent(
            Some(ObjectKey::service("db-prod-03", "postgres-replication")),
            "CRITICAL - standby lag 412s (> 300s)",
            "overview / databases",
        ));
        assert_eq!(
            notification.title,
            "CRITICAL · postgres-replication on db-prod-03"
        );
        assert_eq!(
            notification.body,
            "CRITICAL - standby lag 412s (> 300s)\noverview / databases"
        );
        assert_eq!(
            notification.tag,
            "db-prod-03!postgres-replication:critical:1790000000"
        );
        assert_eq!(notification.actions.len(), 1);
        assert_eq!(notification.actions[0].id, OPEN_ACTION);
    }

    #[test]
    fn summaries_have_no_open_button() {
        let summary = system_notification(&intent(None, "", "prod-cluster"));
        assert!(summary.actions.is_empty());
        assert_eq!(summary.body, "prod-cluster");
    }

    #[test]
    fn the_port_hands_intents_to_the_receiver() {
        let (notifier, mut receiver) = GpuiNotifier::new();
        notifier.notify(&intent(None, "body", ""));
        let received_intent = receiver.try_recv().unwrap();
        assert_eq!(received_intent.body, "body");
        drop(receiver);
        // A notifier whose UI is gone drops intents quietly.
        notifier.notify(&intent(None, "later", ""));
    }
}
