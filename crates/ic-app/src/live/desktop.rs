//! Desktop notifications (NOTE-01): what a notification intent becomes on
//! screen, and who shows it.
//!
//! - title `CRITICAL · postgres-replication on db-prod-03`, the output's
//!   first line with the matching `group / dashboard` under it (without
//!   the output when the settings' *show plugin output* is off, for shared
//!   screens and the lock screen);
//! - urgency by tone (critical and down: critical, so most desktops keep
//!   them on screen; warnings and unknowns: normal; recoveries and
//!   information: low), and the rule's sound (a freedesktop sound name by
//!   tone, or silence);
//! - *Acknowledge* (problems the API user may acknowledge) and *Open*
//!   buttons; a click on the body opens the object too.
//!
//! On Linux they go straight to the desktop's notification server over
//! D-Bus ([`super::dbus`]: one connection, urgency and sound hints, the
//! desktop entry). On macOS they go to `UNUserNotificationCenter`
//! (`super::macos`, from the app bundle: the system's alert sound when
//! the rule wants one, no urgency). Clicks come back as [`Response`]s.

use gpui::App;
use ic_rules::{NotificationIntent, Tone};

/// The action id of the *Open* button.
pub(crate) const OPEN_ACTION: &str = "open";
/// The action id of the *Acknowledge* button.
pub(crate) const ACKNOWLEDGE_ACTION: &str = "acknowledge";

/// How insistent a notification is (the XDG urgency levels).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Urgency {
    /// Recoveries, acknowledgements, downtimes, summaries.
    Low,
    /// Warnings and unknowns.
    Normal,
    /// Critical services and down hosts: stays until dismissed on most
    /// desktops.
    Critical,
}

impl Urgency {
    /// The XDG `urgency` hint's byte (only the Linux D-Bus notifier sends it).
    #[cfg(any(target_os = "linux", test))]
    pub(crate) fn level(self) -> u8 {
        match self {
            Self::Low => 0,
            Self::Normal => 1,
            Self::Critical => 2,
        }
    }
}

/// A notification as posted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Posted {
    /// The intent's id: clicks carry it back.
    pub(crate) tag: String,
    /// `CRITICAL · postgres-replication on db-prod-03`.
    pub(crate) title: String,
    /// The first line, then where it matched.
    pub(crate) body: String,
    /// How insistent.
    pub(crate) urgency: Urgency,
    /// A freedesktop sound name (macOS plays its alert sound for any), or
    /// `None` for silence.
    pub(crate) sound: Option<&'static str>,
    /// Buttons: id and label.
    pub(crate) actions: Vec<(&'static str, &'static str)>,
}

/// A click on a notification: its tag and the button (`None`: the body).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Response {
    /// The notification's tag.
    pub(crate) tag: String,
    /// The button's action id, or `None` for the body.
    pub(crate) action: Option<String>,
}

/// What `intent` of environment `environment` looks like on screen: the
/// first line, then where it matched without repeating anything (like the
/// notification centre's labels). `acknowledge` offers the *Acknowledge*
/// button (a problem the API user may acknowledge). Without `output` (the
/// settings' *show plugin output* off) a state change says only where it
/// matched: its first line is the plugin's output. Acknowledgement and
/// downtime comments and storm summaries stay.
pub(crate) fn posted(
    intent: &NotificationIntent,
    environment: &str,
    acknowledge: bool,
    output: bool,
) -> Posted {
    let place = crate::notifications::entry::place_of(&intent.subtitle, environment);
    let state_change = intent.object.is_some() && intent.tone != Tone::Info;
    let first_line = if output || !state_change {
        intent.body.trim()
    } else {
        ""
    };
    let body = match (first_line, place.as_str()) {
        ("", place) => place.to_owned(),
        (body, "") => body.to_owned(),
        (body, place) => format!("{body}\n{place}"),
    };
    let problem = matches!(intent.tone, Tone::Critical | Tone::Warning | Tone::Unknown);
    let mut actions = Vec::new();
    if intent.object.is_some() {
        if problem && acknowledge {
            actions.push((ACKNOWLEDGE_ACTION, "Acknowledge"));
        }
        actions.push((OPEN_ACTION, "Open"));
    }
    Posted {
        tag: intent.id.clone(),
        title: intent.title.clone(),
        body,
        urgency: match intent.tone {
            Tone::Critical => Urgency::Critical,
            Tone::Warning | Tone::Unknown => Urgency::Normal,
            Tone::Recovery | Tone::Info => Urgency::Low,
        },
        sound: intent.sound.then_some(match intent.tone {
            Tone::Critical => "dialog-error",
            Tone::Warning | Tone::Unknown => "dialog-warning",
            Tone::Recovery => "complete",
            Tone::Info => "dialog-information",
        }),
        actions,
    }
}

/// `title` with the environment's name in front, for when there is more
/// than one environment (A1): `staging · CRITICAL · disk on stg-01`. A
/// storm's summary names its environment already (`14 new problems in
/// staging`) and stays as it is.
pub(crate) fn prefixed_title(title: &str, environment: &str) -> String {
    if environment.trim().is_empty() || title.ends_with(&format!(" in {environment}")) {
        return title.to_owned();
    }
    format!("{environment} · {title}")
}

/// Shows notifications on the desktop.
pub(crate) trait Desktop {
    /// Shows `posted`.
    fn show(&self, posted: Posted, cx: &mut App);
}

/// GPUI's system notifications (neither Linux nor macOS: no sound).
#[cfg(all(not(any(target_os = "linux", target_os = "macos")), not(test)))]
#[derive(Debug, Default)]
pub(crate) struct GpuiDesktop;

#[cfg(all(not(any(target_os = "linux", target_os = "macos")), not(test)))]
impl Desktop for GpuiDesktop {
    fn show(&self, posted: Posted, cx: &mut App) {
        cx.show_system_notification(system_notification(&posted));
    }
}

/// `posted` for GPUI.
#[cfg(any(test, not(any(target_os = "linux", target_os = "macos"))))]
pub(crate) fn system_notification(posted: &Posted) -> gpui::SystemNotification {
    gpui::SystemNotification {
        tag: gpui::SharedString::from(posted.tag.clone()),
        title: gpui::SharedString::from(posted.title.clone()),
        body: gpui::SharedString::from(posted.body.clone()),
        actions: posted
            .actions
            .iter()
            .map(|(id, label)| gpui::SystemNotificationAction {
                id: (*id).into(),
                label: (*label).into(),
            })
            .collect(),
    }
}

/// Records what would be shown (tests).
#[cfg(test)]
#[derive(Clone, Debug, Default)]
pub(crate) struct RecordingDesktop {
    pub(crate) shown: std::rc::Rc<std::cell::RefCell<Vec<Posted>>>,
}

#[cfg(test)]
impl Desktop for RecordingDesktop {
    fn show(&self, posted: Posted, _cx: &mut App) {
        self.shown.borrow_mut().push(posted);
    }
}

#[cfg(test)]
mod tests {
    use ic_model::{ObjectKey, Timestamp};

    use super::*;

    fn intent(tone: Tone, object: Option<ObjectKey>, sound: bool) -> NotificationIntent {
        NotificationIntent {
            id: "db-prod-03!postgres-replication:critical:1790000000".to_owned(),
            object,
            title: "CRITICAL · postgres-replication on db-prod-03".to_owned(),
            subtitle: "overview / databases".to_owned(),
            body: "CRITICAL - standby lag 412s (> 300s)".to_owned(),
            tone,
            sound,
            silent: false,
            silenced: None,
            at: Timestamp::from_unix_seconds(1_790_000_000.),
        }
    }

    fn replication() -> ObjectKey {
        ObjectKey::service("db-prod-03", "postgres-replication")
    }

    #[test]
    fn problems_offer_acknowledge_and_open_with_urgency_and_sound() {
        let posted = posted(
            &intent(Tone::Critical, Some(replication()), true),
            "prod-cluster",
            true,
            true,
        );
        assert_eq!(
            posted.title,
            "CRITICAL · postgres-replication on db-prod-03"
        );
        assert_eq!(
            posted.body,
            "CRITICAL - standby lag 412s (> 300s)\noverview / databases"
        );
        assert_eq!(
            posted.tag,
            "db-prod-03!postgres-replication:critical:1790000000"
        );
        assert_eq!(posted.urgency, Urgency::Critical);
        assert_eq!(posted.sound, Some("dialog-error"));
        assert_eq!(
            posted.actions,
            [(ACKNOWLEDGE_ACTION, "Acknowledge"), (OPEN_ACTION, "Open")]
        );
        let gpui = system_notification(&posted);
        assert_eq!(gpui.actions.len(), 2);
        assert_eq!(gpui.actions[1].id, OPEN_ACTION);
    }

    #[test]
    fn with_several_environments_the_title_names_its_environment() {
        assert_eq!(
            prefixed_title("CRITICAL · disk on stg-01", "staging"),
            "staging · CRITICAL · disk on stg-01"
        );
        // A storm's summary names it already.
        assert_eq!(
            prefixed_title("14 new problems in staging", "staging"),
            "14 new problems in staging"
        );
        assert_eq!(prefixed_title("CRITICAL · x", " "), "CRITICAL · x");
    }

    #[test]
    fn recoveries_and_summaries_are_quieter() {
        let recovery = posted(
            &intent(Tone::Recovery, Some(replication()), false),
            "prod-cluster",
            true,
            true,
        );
        assert_eq!(recovery.urgency, Urgency::Low);
        assert_eq!(recovery.sound, None, "the rule turned the sound off");
        assert_eq!(
            recovery.actions,
            [(OPEN_ACTION, "Open")],
            "nothing to acknowledge"
        );
        let warning = posted(
            &intent(Tone::Warning, Some(replication()), true),
            "prod-cluster",
            false,
            true,
        );
        assert_eq!(warning.urgency, Urgency::Normal);
        assert_eq!(
            warning.actions,
            [(OPEN_ACTION, "Open")],
            "the API user may not acknowledge"
        );
        let mut summary = intent(Tone::Info, None, true);
        summary.body = String::new();
        let summary = posted(&summary, "prod-cluster", true, true);
        assert!(summary.actions.is_empty(), "no object to open");
        assert_eq!(summary.body, "overview / databases");
        assert_eq!(summary.urgency.level(), 0);
    }

    #[test]
    fn the_place_is_said_once() {
        let mut repeated = intent(Tone::Critical, Some(replication()), true);
        repeated.subtitle = "overview / overview".to_owned();
        assert_eq!(
            posted(&repeated, "prod-cluster", true, true).body,
            "CRITICAL - standby lag 412s (> 300s)\noverview"
        );
        // A storm's summary names its environment in the title already.
        let mut summary = intent(Tone::Info, None, true);
        summary.title = "14 new problems in staging".to_owned();
        summary.subtitle = "staging".to_owned();
        summary.body = "14 critical".to_owned();
        assert_eq!(posted(&summary, "staging", true, true).body, "14 critical");
    }

    #[test]
    fn without_plugin_output_a_state_change_says_only_where() {
        let critical = intent(Tone::Critical, Some(replication()), true);
        let hidden = posted(&critical, "prod-cluster", true, false);
        assert_eq!(hidden.body, "overview / databases");
        assert_eq!(
            hidden.title, "CRITICAL · postgres-replication on db-prod-03",
            "the title still names the object and its state"
        );
        assert_eq!(hidden.actions.len(), 2);
        let recovery = intent(Tone::Recovery, Some(replication()), true);
        assert!(
            !posted(&recovery, "prod-cluster", true, false)
                .body
                .contains("standby lag")
        );
        // An acknowledgement's comment and a storm's summary aren't the
        // plugin's output.
        let mut acknowledged = intent(Tone::Info, Some(replication()), true);
        acknowledged.body = "m.keller: replica rebuilding".to_owned();
        assert!(
            posted(&acknowledged, "prod-cluster", true, false)
                .body
                .starts_with("m.keller: replica rebuilding")
        );
        let mut summary = intent(Tone::Info, None, true);
        summary.body = "14 critical".to_owned();
        summary.subtitle = "staging".to_owned();
        assert_eq!(posted(&summary, "staging", true, false).body, "14 critical");
    }
}
