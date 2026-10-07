//! The settings dialog's data: what each tab edits, how the text fields
//! read and are checked, and the notification rules of every scope
//! (environment, group, dashboard) with their inheritance. Pure, so it is
//! tested without a window.

use std::fmt::Write as _;

use ic_config::{MIN_EVENT_LOG_RETENTION_HOURS, MIN_RECONCILE_INTERVAL_SECS};
use ic_rules::{Rule, ScopeSetting};

use crate::app_state::NotificationPlan;

/// The dialog's tabs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) enum SettingsTab {
    /// Background, launch at login, the event log, reconciles.
    #[default]
    General,
    /// The active environment's notification rules (NOTE-02..06).
    Notifications,
}

impl SettingsTab {
    /// Both tabs, in order.
    pub(crate) const ALL: [Self; 2] = [Self::General, Self::Notifications];

    /// The tab's label.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Notifications => "notifications",
        }
    }
}

/// A notification scope: the environment's default rule, a group or a
/// dashboard.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum ScopeKey {
    /// The environment.
    Environment,
    /// A group, by id.
    Group(String),
    /// A dashboard: group id, dashboard id.
    Dashboard(String, String),
}

/// A text field of the dialog.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum FieldId {
    /// Event log retention, hours.
    Retention,
    /// Reconcile interval, seconds (when not adaptive).
    Reconcile,
    /// A rule's minimum duration.
    MinDuration(ScopeKey),
    /// Quiet hours' start.
    QuietStart,
    /// Quiet hours' end.
    QuietEnd,
    /// Storm control's threshold.
    StormThreshold,
    /// Storm control's window.
    StormWindow,
}

/// The longest event log retention offered: a year.
const MAX_RETENTION_HOURS: u32 = 24 * 365;
/// The longest minimum duration of a problem: a day.
const MAX_MIN_DURATION_SECS: u32 = 86_400;

/// Parses the event log retention (hours).
///
/// # Errors
///
/// A message for the field.
pub(crate) fn parse_retention(text: &str) -> Result<u32, String> {
    let hours = text.trim().trim_end_matches('h').trim();
    let hours: u32 = hours
        .parse()
        .map_err(|_| "Enter a number of hours, like 48.".to_owned())?;
    if hours < MIN_EVENT_LOG_RETENTION_HOURS {
        return Err(format!("At least {MIN_EVENT_LOG_RETENTION_HOURS} hour."));
    }
    if hours > MAX_RETENTION_HOURS {
        return Err("At most a year (8760 hours).".to_owned());
    }
    Ok(hours)
}

/// Parses a fixed reconcile interval (seconds, or a duration like `10m`).
///
/// # Errors
///
/// A message for the field.
pub(crate) fn parse_reconcile(text: &str) -> Result<u32, String> {
    let seconds =
        parse_seconds(text).map_err(|_| "Enter seconds, or a duration like 10m.".to_owned())?;
    if seconds < MIN_RECONCILE_INTERVAL_SECS {
        return Err(format!(
            "At least {MIN_RECONCILE_INTERVAL_SECS} seconds: a lean reload of a large Icinga \
             costs the master real memory."
        ));
    }
    Ok(seconds)
}

/// Parses a rule's minimum duration: `0` or empty (at once), or a
/// duration (`5m`, `90s`, `1h`; a plain number is seconds).
///
/// # Errors
///
/// A message for the field.
pub(crate) fn parse_min_duration(text: &str) -> Result<u32, String> {
    let text = text.trim();
    if text.is_empty() || text == "0" {
        return Ok(0);
    }
    let seconds = parse_seconds(text)
        .map_err(|_| "Enter a duration like 5m, or 0 for at once.".to_owned())?;
    if seconds > MAX_MIN_DURATION_SECS {
        return Err("At most a day.".to_owned());
    }
    Ok(seconds)
}

/// A minimum duration as the field shows it: `0`, `90s`, `5m`, `1h30m`.
pub(crate) fn format_min_duration(seconds: u32) -> String {
    if seconds == 0 {
        return "0".to_owned();
    }
    let (hours, minutes, rest) = (seconds / 3600, seconds % 3600 / 60, seconds % 60);
    let mut text = String::new();
    if hours > 0 {
        let _ = write!(text, "{hours}h");
    }
    if minutes > 0 {
        let _ = write!(text, "{minutes}m");
    }
    if rest > 0 {
        let _ = write!(text, "{rest}s");
    }
    text
}

/// Parses a local clock time (`22:00`, `7:30`, `07.30`, `7`) into minutes
/// after midnight.
///
/// # Errors
///
/// A message for the field.
pub(crate) fn parse_clock(text: &str) -> Result<u16, String> {
    let text = text.trim();
    let invalid = || "Enter a time like 22:00.".to_owned();
    let (hours, minutes) = match text.split_once([':', '.']) {
        Some((hours, minutes)) => (hours.trim(), minutes.trim()),
        None => (text, "0"),
    };
    let hours: u16 = hours.parse().map_err(|_| invalid())?;
    let minutes: u16 = minutes.parse().map_err(|_| invalid())?;
    if hours > 23 || minutes > 59 {
        return Err(invalid());
    }
    Ok(hours * 60 + minutes)
}

/// Minutes after midnight as `HH:MM`.
pub(crate) fn format_clock(minute: u16) -> String {
    format!("{:02}:{:02}", minute / 60 % 24, minute % 60)
}

/// Parses storm control's threshold: how many notifications in a window
/// are still shown one by one.
///
/// # Errors
///
/// A message for the field.
pub(crate) fn parse_threshold(text: &str) -> Result<u32, String> {
    let count: u32 = text
        .trim()
        .parse()
        .map_err(|_| "Enter a number, like 5.".to_owned())?;
    if count == 0 || count > 1000 {
        return Err("Between 1 and 1000.".to_owned());
    }
    Ok(count)
}

/// Parses storm control's window (seconds, or a duration like `1m`).
///
/// # Errors
///
/// A message for the field.
pub(crate) fn parse_window(text: &str) -> Result<u32, String> {
    let seconds = parse_seconds(text).map_err(|_| "Enter seconds, like 10.".to_owned())?;
    if seconds == 0 || seconds > 3600 {
        return Err("Between 1 second and 1 hour.".to_owned());
    }
    Ok(seconds)
}

/// Seconds from `120` or a duration (`2m`, `1h30m`).
fn parse_seconds(text: &str) -> Result<u32, String> {
    let text = text.trim();
    if let Ok(seconds) = text.parse::<u32>() {
        return Ok(seconds);
    }
    let duration = crate::operate::when::parse_duration(text)?;
    u32::try_from(duration.as_secs()).map_err(|_| "too long".to_owned())
}

/// The three ways a group or dashboard can relate to its parent, plus a
/// rule of its own, as the dialog's segmented control offers them.
pub(crate) const SCOPE_CHOICES: [&str; 4] = ["inherit", "on", "off", "custom"];

/// The index of `setting` in [`SCOPE_CHOICES`].
pub(crate) fn scope_choice(setting: &ScopeSetting) -> usize {
    match setting {
        ScopeSetting::Inherit => 0,
        ScopeSetting::On => 1,
        ScopeSetting::Off => 2,
        ScopeSetting::Custom(_) => 3,
    }
}

impl NotificationPlan {
    /// The setting of a group or dashboard (`None` for the environment,
    /// or one that is gone).
    pub(crate) fn setting(&self, key: &ScopeKey) -> Option<&ScopeSetting> {
        match key {
            ScopeKey::Environment => None,
            ScopeKey::Group(id) => self
                .groups
                .iter()
                .find(|group| group.id == *id)
                .map(|group| &group.setting),
            ScopeKey::Dashboard(group_id, dashboard_id) => self
                .groups
                .iter()
                .find(|group| group.id == *group_id)?
                .dashboards
                .iter()
                .find(|(id, _, _)| id == dashboard_id)
                .map(|(_, _, setting)| setting),
        }
    }

    /// The rule a scope's own settings edit: the environment's default
    /// rule, or a group's or dashboard's custom rule.
    pub(crate) fn rule(&self, key: &ScopeKey) -> Option<&Rule> {
        match key {
            ScopeKey::Environment => Some(&self.settings.default_rule),
            _ => match self.setting(key)? {
                ScopeSetting::Custom(rule) => Some(rule),
                _ => None,
            },
        }
    }

    /// [`NotificationPlan::rule`], to change it.
    pub(crate) fn rule_mut(&mut self, key: &ScopeKey) -> Option<&mut Rule> {
        match key {
            ScopeKey::Environment => Some(&mut self.settings.default_rule),
            _ => match self.setting_mut(key)? {
                ScopeSetting::Custom(rule) => Some(rule),
                _ => None,
            },
        }
    }

    fn setting_mut(&mut self, key: &ScopeKey) -> Option<&mut ScopeSetting> {
        match key {
            ScopeKey::Environment => None,
            ScopeKey::Group(id) => self
                .groups
                .iter_mut()
                .find(|group| group.id == *id)
                .map(|group| &mut group.setting),
            ScopeKey::Dashboard(group_id, dashboard_id) => self
                .groups
                .iter_mut()
                .find(|group| group.id == *group_id)?
                .dashboards
                .iter_mut()
                .find(|(id, _, _)| id == dashboard_id)
                .map(|(_, _, setting)| setting),
        }
    }

    /// The rule a scope notifies with when on: its custom rule, else its
    /// parent's (dashboard → group → environment).
    pub(crate) fn effective_rule(&self, key: &ScopeKey) -> Rule {
        if let Some(rule) = self.rule(key) {
            return rule.clone();
        }
        match key {
            ScopeKey::Environment | ScopeKey::Group(_) => self.settings.default_rule.clone(),
            ScopeKey::Dashboard(group_id, _) => {
                self.effective_rule(&ScopeKey::Group(group_id.clone()))
            }
        }
    }

    /// Sets a group's or dashboard's setting from its choice index (see
    /// [`SCOPE_CHOICES`]). A new custom rule starts as the rule the scope
    /// notified with before. Returns whether it changed.
    pub(crate) fn choose(&mut self, key: &ScopeKey, choice: usize) -> bool {
        let start = self.effective_rule(key);
        let Some(setting) = self.setting_mut(key) else {
            return false;
        };
        let new = match choice {
            0 => ScopeSetting::Inherit,
            1 => ScopeSetting::On,
            2 => ScopeSetting::Off,
            _ => match setting {
                ScopeSetting::Custom(_) => return false,
                _ => ScopeSetting::Custom(start),
            },
        };
        if *setting == new {
            return false;
        }
        *setting = new;
        true
    }

    /// Every scope with a rule of its own (the environment first), for
    /// their minimum-duration fields.
    pub(crate) fn rule_scopes(&self) -> Vec<ScopeKey> {
        let mut keys = vec![ScopeKey::Environment];
        for group in &self.groups {
            let key = ScopeKey::Group(group.id.clone());
            if matches!(group.setting, ScopeSetting::Custom(_)) {
                keys.push(key);
            }
            for (id, _, setting) in &group.dashboards {
                if matches!(setting, ScopeSetting::Custom(_)) {
                    keys.push(ScopeKey::Dashboard(group.id.clone(), id.clone()));
                }
            }
        }
        keys
    }
}

/// One on/off condition of a rule (NOTE-03), as the dialog's chips and
/// switches edit it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum RuleFlag {
    /// Service critical.
    Critical,
    /// Service warning.
    Warning,
    /// Service unknown.
    Unknown,
    /// Host down.
    Down,
    /// Host unreachable.
    Unreachable,
    /// Back to OK/UP after a notified problem.
    Recovery,
    /// Acknowledgements set or cleared.
    Acknowledgements,
    /// Downtimes started or ended.
    Downtimes,
    /// Flapping started or stopped.
    Flapping,
    /// Hard states only.
    HardOnly,
    /// Skip handled problems.
    SkipHandled,
    /// Play a sound.
    Sound,
}

impl RuleFlag {
    /// The states, in the order the dialog shows them.
    pub(crate) const STATES: [Self; 6] = [
        Self::Critical,
        Self::Warning,
        Self::Unknown,
        Self::Down,
        Self::Unreachable,
        Self::Recovery,
    ];
    /// The other events.
    pub(crate) const EVENTS: [Self; 3] = [Self::Acknowledgements, Self::Downtimes, Self::Flapping];

    /// The chip's or switch's label.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Critical => "critical",
            Self::Warning => "warning",
            Self::Unknown => "unknown",
            Self::Down => "down",
            Self::Unreachable => "unreachable",
            Self::Recovery => "recovery",
            Self::Acknowledgements => "acknowledgements",
            Self::Downtimes => "downtimes",
            Self::Flapping => "flapping",
            Self::HardOnly => "hard states only: soft states are retries in progress",
            Self::SkipHandled => "skip handled problems: acknowledged, in downtime, host down",
            Self::Sound => "play a sound",
        }
    }

    /// Whether `rule` has it on.
    pub(crate) fn get(self, rule: &Rule) -> bool {
        match self {
            Self::Critical => rule.states.critical,
            Self::Warning => rule.states.warning,
            Self::Unknown => rule.states.unknown,
            Self::Down => rule.states.down,
            Self::Unreachable => rule.states.unreachable,
            Self::Recovery => rule.states.recovery,
            Self::Acknowledgements => rule.events.acknowledgements,
            Self::Downtimes => rule.events.downtimes,
            Self::Flapping => rule.events.flapping,
            Self::HardOnly => rule.hard_only,
            Self::SkipHandled => rule.skip_handled,
            Self::Sound => rule.sound,
        }
    }

    /// Turns it on or off in `rule`.
    pub(crate) fn set(self, rule: &mut Rule, on: bool) {
        let field = match self {
            Self::Critical => &mut rule.states.critical,
            Self::Warning => &mut rule.states.warning,
            Self::Unknown => &mut rule.states.unknown,
            Self::Down => &mut rule.states.down,
            Self::Unreachable => &mut rule.states.unreachable,
            Self::Recovery => &mut rule.states.recovery,
            Self::Acknowledgements => &mut rule.events.acknowledgements,
            Self::Downtimes => &mut rule.events.downtimes,
            Self::Flapping => &mut rule.events.flapping,
            Self::HardOnly => &mut rule.hard_only,
            Self::SkipHandled => &mut rule.skip_handled,
            Self::Sound => &mut rule.sound,
        };
        *field = on;
    }
}

/// What a scope's setting means, for the line under it: `notifies with
/// the environment's rule`, `muted`, …
pub(crate) fn scope_meaning(setting: &ScopeSetting, parent: &str) -> String {
    match setting {
        ScopeSetting::Inherit => format!("inherits {parent}"),
        ScopeSetting::On => format!("notifies with {parent}'s rule, even when that is off"),
        ScopeSetting::Off => "never notifies (bell off in the sidebar)".to_owned(),
        ScopeSetting::Custom(_) => "notifies with its own rule".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use ic_rules::NotificationSettings;

    use super::*;
    use crate::app_state::GroupPlan;

    fn plan() -> NotificationPlan {
        NotificationPlan {
            environment_id: "prod".to_owned(),
            settings: NotificationSettings::default(),
            groups: vec![GroupPlan {
                id: "g".to_owned(),
                name: "databases".to_owned(),
                setting: ScopeSetting::Inherit,
                dashboards: vec![(
                    "d".to_owned(),
                    "production".to_owned(),
                    ScopeSetting::Inherit,
                )],
            }],
        }
    }

    #[test]
    fn fields_read_numbers_durations_and_times() {
        assert_eq!(parse_retention("48"), Ok(48));
        assert_eq!(parse_retention(" 72h "), Ok(72));
        assert!(parse_retention("0").is_err());
        assert!(parse_retention("lots").is_err());
        assert!(parse_retention("9000").is_err());
        assert_eq!(parse_reconcile("600"), Ok(600));
        assert_eq!(parse_reconcile("10m"), Ok(600));
        assert!(parse_reconcile("30").unwrap_err().contains("At least 60"));
        assert_eq!(parse_min_duration(""), Ok(0));
        assert_eq!(parse_min_duration("0"), Ok(0));
        assert_eq!(parse_min_duration("5m"), Ok(300));
        assert_eq!(parse_min_duration("90"), Ok(90));
        assert!(parse_min_duration("2d").is_err());
        assert!(parse_min_duration("soon").is_err());
        for seconds in [0, 90, 300, 3600, 5400, 3725] {
            assert_eq!(
                parse_min_duration(&format_min_duration(seconds)),
                Ok(seconds)
            );
        }
        assert_eq!(format_min_duration(5400), "1h30m");
        assert_eq!(parse_clock("22:00"), Ok(1320));
        assert_eq!(parse_clock("7"), Ok(420));
        assert_eq!(parse_clock("07.30"), Ok(450));
        assert!(parse_clock("24:00").is_err());
        assert!(parse_clock("noon").is_err());
        assert_eq!(format_clock(450), "07:30");
        assert_eq!(parse_threshold("5"), Ok(5));
        assert!(parse_threshold("0").is_err());
        assert_eq!(parse_window("1m"), Ok(60));
        assert!(parse_window("0").is_err());
    }

    #[test]
    fn scopes_inherit_and_custom_rules_start_from_their_parent() {
        let mut plan = plan();
        let group = ScopeKey::Group("g".to_owned());
        let dashboard = ScopeKey::Dashboard("g".to_owned(), "d".to_owned());
        plan.settings.default_rule.min_duration_secs = 120;
        assert_eq!(plan.effective_rule(&dashboard).min_duration_secs, 120);
        assert!(plan.rule(&group).is_none());

        // A custom group rule starts as the environment's; the dashboard
        // inherits it.
        assert!(plan.choose(&group, 3));
        assert!(!plan.choose(&group, 3), "already custom: the rule stays");
        plan.rule_mut(&group).unwrap().hard_only = false;
        assert!(!plan.effective_rule(&dashboard).hard_only);
        assert!(plan.effective_rule(&ScopeKey::Environment).hard_only);
        assert_eq!(plan.rule_scopes(), [ScopeKey::Environment, group.clone()]);

        assert!(plan.choose(&dashboard, 2));
        assert_eq!(plan.setting(&dashboard), Some(&ScopeSetting::Off));
        assert_eq!(scope_choice(plan.setting(&dashboard).unwrap()), 2);
        assert!(plan.choose(&group, 0));
        assert!(
            plan.effective_rule(&dashboard).hard_only,
            "back to the environment's"
        );
        assert!(!plan.choose(&ScopeKey::Group("gone".to_owned()), 1));
        assert!(scope_meaning(&ScopeSetting::Off, "databases").contains("never"));
    }

    #[test]
    fn every_rule_flag_reads_and_writes_its_own_field() {
        let all = RuleFlag::STATES.into_iter().chain(RuleFlag::EVENTS).chain([
            RuleFlag::HardOnly,
            RuleFlag::SkipHandled,
            RuleFlag::Sound,
        ]);
        for flag in all {
            let mut rule = Rule::default();
            let before = flag.get(&rule);
            flag.set(&mut rule, !before);
            assert_eq!(flag.get(&rule), !before, "{flag:?}");
            let mut expected = Rule::default();
            flag.set(&mut expected, !before);
            assert_ne!(rule, Rule::default(), "{flag:?} changed the rule");
            // Nothing else changed.
            flag.set(&mut rule, before);
            assert_eq!(rule, Rule::default(), "{flag:?}");
            assert!(!flag.label().is_empty());
        }
    }
}
