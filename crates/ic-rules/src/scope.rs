//! Rule scopes: the environment, its groups and their dashboards, how a
//! setting at each level applies to its parent's result, and which rules a
//! change is judged by.

use std::collections::HashMap;

use ic_model::{ObjectKey, Timestamp};

use crate::intent::DashboardRef;
use crate::settings::{NotificationSettings, ObjectMode, ObjectOverride, Rule, ScopeSetting};
use crate::text;

/// The notification rules of one environment, in the shape the engine
/// needs: the environment's settings plus the group → dashboard tree with
/// each level's [`ScopeSetting`]. `ic-core` builds it from the config.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RuleSet {
    /// The environment's display name: the subtitle of notifications that
    /// matched no dashboard, and the place storm summaries name.
    pub environment_name: String,
    /// Environment-wide settings: master switch, default rule, quiet hours,
    /// storm control, watched and muted objects.
    pub settings: NotificationSettings,
    /// Dashboard groups in sidebar order. When several dashboards match,
    /// the first one in this order names the notification's subtitle.
    pub groups: Vec<GroupScope>,
}

/// A sidebar group and its notification setting.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GroupScope {
    /// The group's id (`DashboardRef::group_id`).
    pub id: String,
    /// Display name, for subtitles (`databases / production`).
    pub name: String,
    /// Setting relative to the environment.
    pub setting: ScopeSetting,
    /// The group's dashboards, in sidebar order.
    pub dashboards: Vec<DashboardScope>,
}

/// A dashboard and its notification setting.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DashboardScope {
    /// The dashboard's id (`DashboardRef::dashboard_id`).
    pub id: String,
    /// Display name, for subtitles.
    pub name: String,
    /// Setting relative to its group.
    pub setting: ScopeSetting,
}

/// A scope's resolved setting: whether it notifies, and with which rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EffectiveRule<'a> {
    /// Whether the scope notifies at all.
    pub enabled: bool,
    /// The rule its notifications follow. Kept when the scope is disabled,
    /// so a child scope that is `On` notifies with it.
    pub rule: &'a Rule,
}

impl<'a> EffectiveRule<'a> {
    /// Applies a child scope's setting to this (its parent's) result:
    /// `Inherit` keeps it, `Off` disables it, `On` enables it with the
    /// parent's rule, and `Custom` enables it with its own rule.
    #[must_use]
    pub fn apply(self, setting: &'a ScopeSetting) -> Self {
        match setting {
            ScopeSetting::Inherit => self,
            ScopeSetting::Off => Self {
                enabled: false,
                ..self
            },
            ScopeSetting::On => Self {
                enabled: true,
                ..self
            },
            ScopeSetting::Custom(rule) => Self {
                enabled: true,
                rule,
            },
        }
    }
}

impl RuleSet {
    /// The environment level: the master switch and the default rule.
    pub fn environment_rule(&self) -> EffectiveRule<'_> {
        EffectiveRule {
            enabled: self.settings.enabled,
            rule: &self.settings.default_rule,
        }
    }

    /// A group's effective rule (environment → group), or `None` for an
    /// unknown group id.
    pub fn group_rule(&self, group_id: &str) -> Option<EffectiveRule<'_>> {
        let group = self.groups.iter().find(|group| group.id == group_id)?;
        Some(self.environment_rule().apply(&group.setting))
    }

    /// A dashboard's effective rule (environment → group → dashboard), or
    /// `None` if the group or the dashboard doesn't exist.
    pub fn dashboard_rule(&self, dashboard: &DashboardRef) -> Option<EffectiveRule<'_>> {
        let group = self
            .groups
            .iter()
            .find(|group| group.id == dashboard.group_id)?;
        let scope = group
            .dashboards
            .iter()
            .find(|scope| scope.id == dashboard.dashboard_id)?;
        Some(
            self.environment_rule()
                .apply(&group.setting)
                .apply(&scope.setting),
        )
    }

    /// The watch or mute in effect for `object` at `now`. Overrides whose
    /// `until` has passed are ignored; a mute wins over a watch.
    pub fn object_mode(&self, object: &ObjectKey, now: Timestamp) -> Option<ObjectMode> {
        let mut mode = None;
        for entry in self.active_overrides(object, now) {
            match entry.mode {
                ObjectMode::Mute => return Some(ObjectMode::Mute),
                ObjectMode::Watch => mode = Some(ObjectMode::Watch),
            }
        }
        mode
    }

    /// Whether a watch on `object` is in effect at `now`, even if a mute
    /// currently wins over it.
    pub(crate) fn is_watched(&self, object: &ObjectKey, now: Timestamp) -> bool {
        self.active_overrides(object, now)
            .any(|entry| entry.mode == ObjectMode::Watch)
    }

    fn active_overrides<'a>(
        &'a self,
        object: &'a ObjectKey,
        now: Timestamp,
    ) -> impl Iterator<Item = &'a ObjectOverride> {
        self.settings.objects.iter().filter(move |entry| {
            entry.object == *object && entry.until.is_none_or(|until| now < until)
        })
    }
}

/// Where a rule a change is judged by comes from. Stable across rule
/// changes, so the engine can remember which scopes notified a problem and
/// judge its recovery by the same scopes.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Source {
    /// A dashboard the object appears on.
    Dashboard(DashboardRef),
    /// The environment default (the object is on no dashboard).
    Environment,
    /// The object is watched.
    Watch,
}

/// A borrowed [`Source`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SourceRef<'a> {
    /// A dashboard the object appears on.
    Dashboard(&'a DashboardRef),
    /// The environment default.
    Environment,
    /// The object is watched.
    Watch,
}

impl SourceRef<'_> {
    /// The owned form, for remembering.
    pub(crate) fn to_source(self) -> Source {
        match self {
            Self::Dashboard(reference) => Source::Dashboard(reference.clone()),
            Self::Environment => Source::Environment,
            Self::Watch => Source::Watch,
        }
    }
}

/// One rule a change is judged by.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Candidate<'a> {
    /// Where the rule comes from.
    pub(crate) source: SourceRef<'a>,
    /// Whether that scope notifies at all.
    pub(crate) enabled: bool,
    /// The rule.
    pub(crate) rule: &'a Rule,
    /// The notification's subtitle when this candidate decides.
    pub(crate) subtitle: &'a str,
}

/// A dashboard with its effective rule resolved.
#[derive(Clone, Debug)]
struct ResolvedDashboard {
    reference: DashboardRef,
    enabled: bool,
    rule: Rule,
    subtitle: String,
}

/// A [`RuleSet`] plus what the engine looks up on every input: each
/// dashboard's effective rule and its position in sidebar order.
#[derive(Clone, Debug)]
pub(crate) struct Scopes {
    rules: RuleSet,
    dashboards: Vec<ResolvedDashboard>,
    positions: HashMap<DashboardRef, usize>,
}

impl Scopes {
    /// Resolves every dashboard's effective rule.
    pub(crate) fn new(rules: RuleSet) -> Self {
        let mut dashboards = Vec::new();
        let mut positions = HashMap::new();
        let environment = rules.environment_rule();
        for group in &rules.groups {
            let group_rule = environment.apply(&group.setting);
            for scope in &group.dashboards {
                let effective = group_rule.apply(&scope.setting);
                let reference = DashboardRef {
                    group_id: group.id.clone(),
                    dashboard_id: scope.id.clone(),
                };
                // Ids should be unique; if not, the first one wins.
                positions
                    .entry(reference.clone())
                    .or_insert(dashboards.len());
                dashboards.push(ResolvedDashboard {
                    reference,
                    enabled: effective.enabled,
                    rule: effective.rule.clone(),
                    subtitle: text::subtitle(&group.name, &scope.name),
                });
            }
        }
        Self {
            rules,
            dashboards,
            positions,
        }
    }

    /// The rule set.
    pub(crate) fn rules(&self) -> &RuleSet {
        &self.rules
    }

    /// The rules a change is judged by, in subtitle order: the dashboards
    /// (sidebar order), then the environment, then the watch.
    ///
    /// - `memberships`: the dashboards the object appears on now. Unknown
    ///   ones (deleted since) are skipped; if none is known, the
    ///   environment pair applies.
    /// - `remembered`: scopes that notified the object's problem, so they
    ///   also judge its recovery even if the object left them.
    /// - `watched`: the object is watched, which adds the environment's
    ///   default rule, enabled even if every scope is off.
    pub(crate) fn candidates(
        &self,
        memberships: &[DashboardRef],
        remembered: &[Source],
        watched: bool,
    ) -> Vec<Candidate<'_>> {
        let mut positions: Vec<usize> = memberships
            .iter()
            .filter_map(|reference| self.positions.get(reference).copied())
            .collect();
        let on_dashboards = !positions.is_empty();
        positions.extend(remembered.iter().filter_map(|source| match source {
            Source::Dashboard(reference) => self.positions.get(reference).copied(),
            Source::Environment | Source::Watch => None,
        }));
        positions.sort_unstable();
        positions.dedup();

        let mut candidates: Vec<Candidate<'_>> = positions
            .into_iter()
            .filter_map(|position| self.dashboards.get(position))
            .map(|dashboard| Candidate {
                source: SourceRef::Dashboard(&dashboard.reference),
                enabled: dashboard.enabled,
                rule: &dashboard.rule,
                subtitle: &dashboard.subtitle,
            })
            .collect();
        let environment = self.rules.environment_rule();
        if !on_dashboards || remembered.contains(&Source::Environment) {
            candidates.push(Candidate {
                source: SourceRef::Environment,
                enabled: environment.enabled,
                rule: environment.rule,
                subtitle: &self.rules.environment_name,
            });
        }
        if watched || remembered.contains(&Source::Watch) {
            candidates.push(Candidate {
                source: SourceRef::Watch,
                enabled: true,
                rule: environment.rule,
                subtitle: &self.rules.environment_name,
            });
        }
        candidates
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::StateFilter;

    fn custom(warning: bool) -> Rule {
        Rule {
            states: StateFilter {
                warning,
                ..StateFilter::default()
            },
            ..Rule::default()
        }
    }

    fn dashboard(id: &str, setting: ScopeSetting) -> DashboardScope {
        DashboardScope {
            id: id.to_owned(),
            name: id.to_owned(),
            setting,
        }
    }

    fn group(id: &str, setting: ScopeSetting, dashboards: Vec<DashboardScope>) -> GroupScope {
        GroupScope {
            id: id.to_owned(),
            name: id.to_owned(),
            setting,
            dashboards,
        }
    }

    fn reference(group: &str, dashboard: &str) -> DashboardRef {
        DashboardRef {
            group_id: group.to_owned(),
            dashboard_id: dashboard.to_owned(),
        }
    }

    fn rules(enabled: bool, groups: Vec<GroupScope>) -> RuleSet {
        RuleSet {
            environment_name: "prod".to_owned(),
            settings: NotificationSettings {
                enabled,
                ..NotificationSettings::default()
            },
            groups,
        }
    }

    #[test]
    fn settings_apply_down_the_tree() {
        let rules = rules(
            true,
            vec![
                group(
                    "inherit",
                    ScopeSetting::Inherit,
                    vec![
                        dashboard("inherit", ScopeSetting::Inherit),
                        dashboard("off", ScopeSetting::Off),
                        dashboard("custom", ScopeSetting::Custom(custom(true))),
                    ],
                ),
                group(
                    "off",
                    ScopeSetting::Off,
                    vec![
                        dashboard("inherit", ScopeSetting::Inherit),
                        dashboard("on", ScopeSetting::On),
                        dashboard("custom", ScopeSetting::Custom(custom(true))),
                    ],
                ),
                group(
                    "custom",
                    ScopeSetting::Custom(custom(true)),
                    vec![
                        dashboard("inherit", ScopeSetting::Inherit),
                        dashboard("off", ScopeSetting::Off),
                        dashboard("on", ScopeSetting::On),
                    ],
                ),
            ],
        );
        let default = &rules.settings.default_rule;
        let warnings = custom(true);
        let check = |group: &str, dashboard: &str, enabled: bool, rule: &Rule| {
            let effective = rules.dashboard_rule(&reference(group, dashboard)).unwrap();
            assert_eq!(effective.enabled, enabled, "{group}/{dashboard}");
            assert_eq!(effective.rule, rule, "{group}/{dashboard}");
        };
        check("inherit", "inherit", true, default);
        check("inherit", "off", false, default);
        check("inherit", "custom", true, &warnings);
        check("off", "inherit", false, default);
        check("off", "on", true, default);
        check("off", "custom", true, &warnings);
        check("custom", "inherit", true, &warnings);
        check("custom", "off", false, &warnings);
        check("custom", "on", true, &warnings);

        assert!(!rules.group_rule("off").unwrap().enabled);
        assert_eq!(rules.group_rule("custom").unwrap().rule, &warnings);
        assert!(rules.group_rule("nope").is_none());
        assert!(rules.dashboard_rule(&reference("off", "nope")).is_none());
        assert!(rules.dashboard_rule(&reference("nope", "on")).is_none());
    }

    #[test]
    fn on_overrides_a_disabled_environment() {
        let rules = rules(
            false,
            vec![group(
                "g",
                ScopeSetting::Inherit,
                vec![
                    dashboard("inherit", ScopeSetting::Inherit),
                    dashboard("on", ScopeSetting::On),
                ],
            )],
        );
        assert!(!rules.environment_rule().enabled);
        assert!(
            !rules
                .dashboard_rule(&reference("g", "inherit"))
                .unwrap()
                .enabled
        );
        assert!(rules.dashboard_rule(&reference("g", "on")).unwrap().enabled);
    }

    #[test]
    fn object_modes_expire_and_mute_wins() {
        let object = ObjectKey::service("h", "s");
        let mut rules = rules(true, Vec::new());
        let at = Timestamp::from_unix_seconds;
        rules.settings.objects = vec![
            ObjectOverride {
                object: object.clone(),
                mode: ObjectMode::Watch,
                until: None,
            },
            ObjectOverride {
                object: object.clone(),
                mode: ObjectMode::Mute,
                until: Some(at(100.0)),
            },
            ObjectOverride {
                object: ObjectKey::host("h"),
                mode: ObjectMode::Mute,
                until: None,
            },
        ];
        assert_eq!(rules.object_mode(&object, at(50.0)), Some(ObjectMode::Mute));
        assert_eq!(
            rules.object_mode(&object, at(100.0)),
            Some(ObjectMode::Watch)
        );
        assert_eq!(
            rules.object_mode(&ObjectKey::host("h"), at(1e9)),
            Some(ObjectMode::Mute)
        );
        assert_eq!(rules.object_mode(&ObjectKey::host("other"), at(0.0)), None);
    }

    #[test]
    fn candidates_follow_sidebar_order_and_fall_back_to_the_environment() {
        let scopes = Scopes::new(rules(
            true,
            vec![
                group(
                    "a",
                    ScopeSetting::Inherit,
                    vec![
                        dashboard("1", ScopeSetting::Off),
                        dashboard("2", ScopeSetting::On),
                    ],
                ),
                group(
                    "b",
                    ScopeSetting::Inherit,
                    vec![dashboard("1", ScopeSetting::Inherit)],
                ),
            ],
        ));

        let subtitles = |memberships: &[DashboardRef], remembered: &[Source], watched: bool| {
            scopes
                .candidates(memberships, remembered, watched)
                .iter()
                .map(|candidate| (candidate.subtitle.to_owned(), candidate.enabled))
                .collect::<Vec<_>>()
        };

        assert_eq!(
            subtitles(
                &[
                    reference("b", "1"),
                    reference("a", "1"),
                    reference("a", "1")
                ],
                &[],
                false
            ),
            [("a / 1".to_owned(), false), ("b / 1".to_owned(), true)],
            "sidebar order, duplicates removed, no environment fallback"
        );
        assert_eq!(
            subtitles(&[], &[], false),
            [("prod".to_owned(), true)],
            "no memberships: the environment pair"
        );
        assert_eq!(
            subtitles(&[reference("gone", "x")], &[], true),
            [("prod".to_owned(), true), ("prod".to_owned(), true)],
            "unknown memberships count as none; watch comes last"
        );
        assert_eq!(
            subtitles(
                &[reference("b", "1")],
                &[
                    Source::Dashboard(reference("a", "2")),
                    Source::Environment,
                    Source::Watch
                ],
                false
            ),
            [
                ("a / 2".to_owned(), true),
                ("b / 1".to_owned(), true),
                ("prod".to_owned(), true),
                ("prod".to_owned(), true),
            ],
            "remembered scopes join in"
        );
    }

    #[test]
    fn watch_is_enabled_even_when_everything_is_off() {
        let scopes = Scopes::new(rules(
            false,
            vec![group(
                "a",
                ScopeSetting::Off,
                vec![dashboard("1", ScopeSetting::Inherit)],
            )],
        ));
        let candidates = scopes.candidates(&[reference("a", "1")], &[], true);
        assert_eq!(candidates.len(), 2);
        assert!(!candidates[0].enabled);
        assert!(candidates[1].enabled);
        assert_eq!(candidates[1].source, SourceRef::Watch);
        assert_eq!(candidates[1].rule, &Rule::default());
    }
}
