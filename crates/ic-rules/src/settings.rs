//! Notification settings: what the user configures per environment, group,
//! dashboard and object. `ic-config` persists these types; the engine reads
//! them.

use serde::{Deserialize, Serialize};

use ic_model::{ObjectKey, Timestamp};

/// Which states notify.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "one independent checkbox per state in the settings UI"
)]
pub struct StateFilter {
    /// Service critical.
    pub critical: bool,
    /// Service warning.
    pub warning: bool,
    /// Service unknown.
    pub unknown: bool,
    /// Host down.
    pub down: bool,
    /// Host unreachable.
    pub unreachable: bool,
    /// Back to OK / UP after a problem that notified.
    pub recovery: bool,
}

impl Default for StateFilter {
    fn default() -> Self {
        Self {
            critical: true,
            warning: false,
            unknown: true,
            down: true,
            unreachable: false,
            recovery: true,
        }
    }
}

/// Which non-state events notify.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct EventFilter {
    /// Acknowledgement set or cleared.
    pub acknowledgements: bool,
    /// Downtime started or ended.
    pub downtimes: bool,
    /// Flapping started or stopped.
    pub flapping: bool,
}

/// A notification rule: the conditions under which a change notifies.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct Rule {
    /// States that notify.
    pub states: StateFilter,
    /// Only hard states (default). Soft states are retries in progress.
    pub hard_only: bool,
    /// Skip problems that are acknowledged, in downtime, or whose host is
    /// down (default).
    pub skip_handled: bool,
    /// Other events that notify.
    pub events: EventFilter,
    /// Only notify once a problem has lasted this long; cancelled if it
    /// recovers or gets handled first. `0` = immediately.
    pub min_duration_secs: u32,
    /// Play the platform's notification sound.
    pub sound: bool,
}

impl Default for Rule {
    fn default() -> Self {
        Self {
            states: StateFilter::default(),
            hard_only: true,
            skip_handled: true,
            events: EventFilter::default(),
            min_duration_secs: 0,
            sound: true,
        }
    }
}

/// A group's or dashboard's notification setting, relative to its parent
/// (dashboard → group → environment).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "mode", content = "rule", rename_all = "snake_case")]
pub enum ScopeSetting {
    /// Same as the parent.
    #[default]
    Inherit,
    /// Never notify from this scope.
    Off,
    /// Notify with the parent's rule, even if the parent is off.
    On,
    /// Notify with this rule.
    Custom(Rule),
}

/// Per-object override from the object's pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectMode {
    /// Always notify for this object (environment rule), even if none of
    /// its dashboards notify.
    Watch,
    /// Never notify for this object.
    Mute,
}

/// A watch or mute on one host or service.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ObjectOverride {
    /// The host or service.
    pub object: ObjectKey,
    /// Watch or mute.
    pub mode: ObjectMode,
    /// When the override ends; `None` = until removed.
    pub until: Option<Timestamp>,
}

/// Quiet hours: notifications are recorded but not shown as OS
/// notifications. Times are local, minutes after midnight; a window may
/// cross midnight (`start > end`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct QuietHours {
    /// Whether quiet hours apply.
    pub enabled: bool,
    /// Start, minutes after local midnight.
    pub start_minute: u16,
    /// End, minutes after local midnight.
    pub end_minute: u16,
    /// Days the window starts on, Monday first.
    pub days: [bool; 7],
    /// Still show critical and down notifications.
    pub allow_critical: bool,
}

impl Default for QuietHours {
    fn default() -> Self {
        Self {
            enabled: false,
            start_minute: 22 * 60,
            end_minute: 7 * 60,
            days: [true; 7],
            allow_critical: true,
        }
    }
}

/// Storm control: many notifications in a short window collapse into one
/// summary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct StormControl {
    /// Window length in seconds.
    pub window_secs: u32,
    /// More notifications than this inside the window become a summary.
    pub threshold: u32,
}

impl Default for StormControl {
    fn default() -> Self {
        Self {
            window_secs: 10,
            threshold: 5,
        }
    }
}

/// Everything about notifications for one environment.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NotificationSettings {
    /// Master switch for the environment.
    pub enabled: bool,
    /// The environment-level rule groups and dashboards inherit.
    pub default_rule: Rule,
    /// Quiet hours.
    pub quiet_hours: QuietHours,
    /// Storm control.
    pub storm: StormControl,
    /// Watched and muted objects.
    pub objects: Vec<ObjectOverride>,
}

impl Default for NotificationSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            default_rule: Rule::default(),
            quiet_hours: QuietHours::default(),
            storm: StormControl::default(),
            objects: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_settings_serialize_readably() {
        let json = serde_json::to_string(&ScopeSetting::Off).unwrap();
        assert_eq!(json, r#"{"mode":"off"}"#);
        let custom = ScopeSetting::Custom(Rule::default());
        let back: ScopeSetting =
            serde_json::from_str(&serde_json::to_string(&custom).unwrap()).unwrap();
        assert_eq!(back, custom);
    }

    #[test]
    fn missing_fields_take_defaults() {
        let rule: Rule = serde_json::from_str(r#"{"hard_only":false}"#).unwrap();
        assert!(!rule.hard_only);
        assert!(rule.skip_handled);
        assert!(rule.states.critical);
    }
}
