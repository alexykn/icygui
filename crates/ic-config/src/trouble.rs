//! An environment's trouble alerts (PLAN.md §4.2 A, B, B3, E; mock-ups
//! 16b, 16b2, 16b3): the alerts that say icygui can't see, or Icinga
//! isn't working, rather than that a host or service has a problem.
//!
//! They always notify at the OS level, whatever the environment's rules,
//! mutes, storm limits or quiet hours say; only an explicit pause holds
//! them back. What can be chosen is how insistent they are
//! ([`TroublePolicy`]) and which heartbeats prove that Icinga runs its
//! checks and its results reach icygui ([`HeartbeatSettings`]).

use ic_model::ServiceKey;
use serde::{Deserialize, Serialize};

/// The custom variable that marks a heartbeat service by default
/// (`vars.icygui_heartbeat = true`).
pub const DEFAULT_HEARTBEAT_VARIABLE: &str = "icygui_heartbeat";

/// The shortest heartbeat interval an override may set, in seconds:
/// shorter beats would only make Icinga and icygui busier without telling
/// sooner (the time budget allows at least 5 s per beat).
pub const MIN_HEARTBEAT_INTERVAL_SECS: u32 = 10;

/// An environment's trouble alerts.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Trouble {
    /// How the desktop notification of a trouble alert behaves.
    pub policy: TroublePolicy,
    /// Which heartbeats icygui watches.
    pub heartbeats: HeartbeatSettings,
}

impl Trouble {
    /// Whether nothing differs from the defaults (the settings file leaves
    /// the table out then).
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// How a trouble alert notifies.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TroublePolicy {
    /// A desktop notification when it is raised and one when it clears.
    #[default]
    Notify,
    /// The same at critical urgency, and it stays on screen until the
    /// trouble is over.
    Persistent,
}

impl TroublePolicy {
    /// Both, in the order of the settings' select.
    pub const ALL: [Self; 2] = [Self::Notify, Self::Persistent];

    /// The word the settings show.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Notify => "notify",
            Self::Persistent => "persistent",
        }
    }
}

/// Which services are heartbeats: always-OK checks whose results prove
/// that Icinga runs checks (in a zone, or on one endpoint) and that the
/// results reach icygui.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HeartbeatSettings {
    /// Found by a custom variable, or listed by name.
    pub mode: HeartbeatMode,
    /// The custom variable that marks them in [`HeartbeatMode::Find`]
    /// (without `vars.`): every service whose variable is set and not
    /// `false`, `0` or empty is one.
    pub variable: String,
    /// The heartbeats in [`HeartbeatMode::List`], as `host!service`.
    pub list: Vec<String>,
    /// Watch every heartbeat at this interval, in seconds, instead of its
    /// object's `check_interval` (for a check whose interval icygui can't
    /// read; at least [`MIN_HEARTBEAT_INTERVAL_SECS`]). Only in the
    /// settings file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interval_secs: Option<u32>,
}

impl Default for HeartbeatSettings {
    fn default() -> Self {
        Self {
            mode: HeartbeatMode::Find,
            variable: DEFAULT_HEARTBEAT_VARIABLE.to_owned(),
            list: Vec::new(),
            interval_secs: None,
        }
    }
}

impl HeartbeatSettings {
    /// The custom variable's name without a `vars.` or `service.vars.`
    /// prefix, trimmed; empty when none is set.
    #[must_use]
    pub fn variable_name(&self) -> &str {
        let name = self.variable.trim();
        name.strip_prefix("service.vars.")
            .or_else(|| name.strip_prefix("vars."))
            .unwrap_or(name)
            .trim()
    }

    /// The listed heartbeats that parse as `host!service`, each once, in
    /// order.
    #[must_use]
    pub fn listed(&self) -> Vec<ServiceKey> {
        let mut keys: Vec<ServiceKey> = Vec::new();
        for entry in &self.list {
            if let Some(key) = ServiceKey::parse(entry.trim())
                && !keys.contains(&key)
            {
                keys.push(key);
            }
        }
        keys
    }

    /// Whether heartbeats are off: no variable to find them by, or an
    /// empty list.
    #[must_use]
    pub fn is_off(&self) -> bool {
        match self.mode {
            HeartbeatMode::Find => self.variable_name().is_empty(),
            HeartbeatMode::List => self.listed().is_empty(),
        }
    }
}

/// How heartbeats are found.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeartbeatMode {
    /// Every service with the custom variable
    /// ([`HeartbeatSettings::variable`]).
    #[default]
    Find,
    /// The services listed by name ([`HeartbeatSettings::list`]).
    List,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_find_heartbeats_by_icygui_heartbeat() {
        let trouble = Trouble::default();
        assert!(trouble.is_default());
        assert_eq!(trouble.policy, TroublePolicy::Notify);
        assert_eq!(trouble.heartbeats.mode, HeartbeatMode::Find);
        assert_eq!(trouble.heartbeats.variable_name(), "icygui_heartbeat");
        assert!(!trouble.heartbeats.is_off());
    }

    #[test]
    fn the_variable_loses_its_prefix() {
        for (written, name) in [
            ("icygui_heartbeat", "icygui_heartbeat"),
            (" vars.beat ", "beat"),
            ("service.vars.beat", "beat"),
            ("  ", ""),
        ] {
            let settings = HeartbeatSettings {
                variable: written.to_owned(),
                ..HeartbeatSettings::default()
            };
            assert_eq!(settings.variable_name(), name, "{written:?}");
            assert_eq!(settings.is_off(), name.is_empty());
        }
    }

    #[test]
    fn listed_heartbeats_parse_and_repeat_once() {
        let settings = HeartbeatSettings {
            mode: HeartbeatMode::List,
            list: vec![
                "icygui-hb-master-01!beat".to_owned(),
                "not a service".to_owned(),
                " icygui-hb-master-01!beat ".to_owned(),
                "icygui-hb-ams!beat".to_owned(),
            ],
            ..HeartbeatSettings::default()
        };
        assert_eq!(
            settings.listed(),
            [
                ServiceKey::new("icygui-hb-master-01", "beat"),
                ServiceKey::new("icygui-hb-ams", "beat")
            ]
        );
        assert!(!settings.is_off());
        let empty = HeartbeatSettings {
            mode: HeartbeatMode::List,
            ..HeartbeatSettings::default()
        };
        assert!(empty.is_off());
    }

    #[test]
    fn trouble_round_trips_in_toml() {
        let trouble = Trouble {
            policy: TroublePolicy::Persistent,
            heartbeats: HeartbeatSettings {
                mode: HeartbeatMode::List,
                variable: "beat".to_owned(),
                list: vec!["h!s".to_owned()],
                interval_secs: Some(30),
            },
        };
        let text = toml::to_string(&trouble).unwrap();
        assert!(text.contains("policy = \"persistent\""), "{text}");
        assert!(text.contains("mode = \"list\""), "{text}");
        assert_eq!(toml::from_str::<Trouble>(&text).unwrap(), trouble);
        // An empty table is the defaults.
        assert_eq!(toml::from_str::<Trouble>("").unwrap(), Trouble::default());
    }
}
