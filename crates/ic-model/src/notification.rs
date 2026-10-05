//! Icinga's own notifications: who Icinga notified about a host or service,
//! and when, from its `Notification` objects (configured in Icinga, usually
//! with `apply Notification … to Service`).
//!
//! These are read-only facts for the panes ("notified m.keller, oncall ·
//! 14m ago"). The client's own desktop notifications are a different thing:
//! `ic-rules` decides those from the event stream.

use serde::{Deserialize, Serialize};

use crate::name::ObjectKey;
use crate::time::Timestamp;

/// One of Icinga's `Notification` objects, with the runtime state the client
/// shows (`lib/icinga/notification.ti`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Notification {
    /// Full object name: `host!service!name` for a service, `host!name` for
    /// a host.
    pub name: String,
    /// The host or service it notifies about (`host_name`, `service_name`).
    pub object: ObjectKey,
    /// When Icinga last sent this notification, of any type (problem,
    /// recovery, acknowledgement, …), even if no user's filters let it
    /// through (`last_notification`); `None` if never.
    pub last_notification: Option<Timestamp>,
    /// The users notified about the object's current problem
    /// (`notified_problem_users`), in the order Icinga added them. Icinga
    /// clears the list when it sends the recovery.
    pub notified_problem_users: Vec<String>,
}

/// Who Icinga notified about one host or service, and when: all of the
/// object's [`Notification`] objects combined, for the panes' "notified"
/// row.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Notified {
    /// The latest `last_notification` of the object's notifications; `None`
    /// if Icinga never notified about it.
    pub last_notification: Option<Timestamp>,
    /// The users any of them notified about the current problem, sorted,
    /// each once. Empty when nobody was notified, or after the recovery.
    pub users: Vec<String>,
}

impl Notified {
    /// Combines the notifications of one object.
    #[must_use]
    pub fn of<'a>(notifications: impl IntoIterator<Item = &'a Notification>) -> Self {
        let mut last_notification: Option<Timestamp> = None;
        let mut users: Vec<String> = Vec::new();
        for notification in notifications {
            if let Some(at) = notification.last_notification
                && last_notification.is_none_or(|last| at > last)
            {
                last_notification = Some(at);
            }
            users.extend(notification.notified_problem_users.iter().cloned());
        }
        users.sort_unstable();
        users.dedup();
        Self {
            last_notification,
            users,
        }
    }

    /// Whether Icinga never notified about the object ("not notified").
    #[must_use]
    pub fn is_never(&self) -> bool {
        self.last_notification.is_none() && self.users.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notification(name: &str, last: f64, users: &[&str]) -> Notification {
        Notification {
            name: name.to_owned(),
            object: ObjectKey::service("db-prod-03", "postgres-replication"),
            last_notification: Timestamp::from_unix_seconds(last).non_zero(),
            notified_problem_users: users.iter().map(|user| (*user).to_owned()).collect(),
        }
    }

    #[test]
    fn combines_the_latest_time_and_every_user_once() {
        let mail = notification(
            "db-prod-03!postgres-replication!mail",
            100.0,
            &["oncall", "m.keller"],
        );
        let sms = notification("db-prod-03!postgres-replication!sms", 160.5, &["oncall"]);
        let never = notification("db-prod-03!postgres-replication!chat", 0.0, &[]);
        let notified = Notified::of([&mail, &sms, &never]);
        assert_eq!(
            notified.last_notification,
            Some(Timestamp::from_unix_seconds(160.5))
        );
        assert_eq!(notified.users, ["m.keller", "oncall"]);
        assert!(!notified.is_never());
    }

    #[test]
    fn nothing_sent_is_never() {
        let notified = Notified::of([&notification("h!s!mail", 0.0, &[])]);
        assert_eq!(notified, Notified::default());
        assert!(notified.is_never());
        assert!(Notified::of([]).is_never());
        // After a recovery the users are gone, the time stays.
        let recovered = Notified::of([&notification("h!s!mail", 50.0, &[])]);
        assert!(!recovered.is_never());
        assert!(recovered.users.is_empty());
    }
}
