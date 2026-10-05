//! What the sidebar shows, computed from the environment, the evaluated
//! dashboards, the selection and the search query. Pure, so it's tested
//! without a window.

use std::collections::BTreeMap;

use ic_config::{DashboardGroup, Environment};
use ic_core::snapshot::{DashboardResult, Summary};
use ic_model::CheckableState;
use ic_rules::DashboardRef;

/// A dashboard's state dot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Dot {
    /// The worst unhandled problem's state.
    State(CheckableState),
    /// Objects match and nothing is unhandled (green).
    Ok,
    /// Nothing matches, or everything is still pending (grey).
    Empty,
}

impl Dot {
    /// The dot for a dashboard's summary (`None` before it's evaluated).
    pub(crate) fn from_summary(summary: Option<&Summary>) -> Self {
        let Some(summary) = summary else {
            return Self::Empty;
        };
        if let Some(state) = summary.worst_unhandled {
            return Self::State(state);
        }
        let checked = summary.ok
            + summary.critical
            + summary.warning
            + summary.unknown
            + summary.down
            + summary.unreachable;
        if checked > 0 { Self::Ok } else { Self::Empty }
    }
}

/// One dashboard row.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SidebarItem<'a> {
    /// Identifies the dashboard.
    pub(crate) reference: DashboardRef,
    /// Display name.
    pub(crate) name: &'a str,
    /// Whether it's the selected dashboard.
    pub(crate) selected: bool,
    /// The state dot.
    pub(crate) dot: Dot,
    /// Unhandled problems, if any.
    pub(crate) count: Option<u32>,
}

/// One group row and the dashboard rows shown under it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SidebarGroup<'a> {
    /// The group.
    pub(crate) group: &'a DashboardGroup,
    /// Whether it holds the selected dashboard (highlighted, icons shown).
    pub(crate) active: bool,
    /// Whether its dashboards are listed (expanded, or a search is active).
    pub(crate) expanded: bool,
    /// The dashboards to list; empty when collapsed.
    pub(crate) items: Vec<SidebarItem<'a>>,
}

/// The groups to show for `query` (case-insensitive; empty shows all).
///
/// A search matches dashboard names, and group names (showing the whole
/// group); groups without matches are left out and the rest are shown
/// expanded, even if collapsed.
pub(crate) fn groups<'a>(
    environment: &'a Environment,
    results: &BTreeMap<DashboardRef, DashboardResult>,
    selected: Option<&DashboardRef>,
    query: &str,
) -> Vec<SidebarGroup<'a>> {
    let query = query.trim().to_lowercase();
    let searching = !query.is_empty();
    environment
        .groups
        .iter()
        .filter_map(|group| {
            let group_matches = searching && group.name.to_lowercase().contains(&query);
            let items: Vec<SidebarItem<'a>> = group
                .dashboards
                .iter()
                .filter(|dashboard| {
                    !searching || group_matches || dashboard.name.to_lowercase().contains(&query)
                })
                .map(|dashboard| {
                    let reference = DashboardRef {
                        group_id: group.id.clone(),
                        dashboard_id: dashboard.id.clone(),
                    };
                    let summary = results.get(&reference).map(|result| &result.summary);
                    SidebarItem {
                        selected: selected == Some(&reference),
                        reference,
                        name: &dashboard.name,
                        dot: Dot::from_summary(summary),
                        count: summary
                            .map(|summary| summary.unhandled)
                            .filter(|unhandled| *unhandled > 0),
                    }
                })
                .collect();
            if searching && items.is_empty() {
                return None;
            }
            let active = selected.is_some_and(|selected| selected.group_id == group.id);
            let expanded = searching || !group.collapsed;
            Some(SidebarGroup {
                group,
                active,
                expanded,
                items: if expanded { items } else { Vec::new() },
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use ic_model::{HostState, ServiceState, Timestamp};

    use super::*;
    use crate::demo;

    fn setup() -> demo::Demo {
        demo::build(Timestamp::from_unix_seconds(1_790_000_000.))
    }

    fn names<'a>(groups: &[SidebarGroup<'a>]) -> Vec<(&'a str, Vec<&'a str>)> {
        groups
            .iter()
            .map(|group| {
                (
                    group.group.name.as_str(),
                    group.items.iter().map(|item| item.name).collect(),
                )
            })
            .collect()
    }

    #[test]
    fn dots_follow_the_worst_unhandled_state() {
        let critical = CheckableState::Service(ServiceState::Critical);
        let summary = Summary {
            critical: 2,
            unhandled: 2,
            worst_unhandled: Some(critical),
            ..Summary::default()
        };
        assert_eq!(Dot::from_summary(Some(&summary)), Dot::State(critical));
        let quiet = Summary {
            ok: 4,
            ..Summary::default()
        };
        assert_eq!(Dot::from_summary(Some(&quiet)), Dot::Ok);
        let handled = Summary {
            down: 1,
            handled: 1,
            ..Summary::default()
        };
        assert_eq!(
            Dot::from_summary(Some(&handled)),
            Dot::Ok,
            "nothing needs attention"
        );
        let pending = Summary {
            pending: 3,
            ..Summary::default()
        };
        assert_eq!(Dot::from_summary(Some(&pending)), Dot::Empty);
        assert_eq!(Dot::from_summary(Some(&Summary::default())), Dot::Empty);
        assert_eq!(Dot::from_summary(None), Dot::Empty);
    }

    #[test]
    fn lists_every_group_and_marks_the_selection() {
        let demo = setup();
        let environment = &demo.config.environments[0];
        let groups = groups(
            environment,
            &demo.snapshot.dashboards,
            Some(&demo.selected),
            "",
        );
        assert_eq!(
            names(&groups),
            [
                ("overview", vec!["overview", "production", "databases"]),
                (
                    "platform",
                    vec!["network", "kubernetes", "certificates", "deploy-checks"]
                ),
                ("lab", vec!["sandbox"]),
            ]
        );
        assert!(groups[0].active);
        assert!(!groups[1].active);
        let selected: Vec<_> = groups
            .iter()
            .flat_map(|group| &group.items)
            .filter(|item| item.selected)
            .map(|item| item.name)
            .collect();
        assert_eq!(selected, ["production"]);
    }

    #[test]
    fn items_carry_dots_and_counts() {
        let demo = setup();
        let environment = &demo.config.environments[0];
        let groups = groups(environment, &demo.snapshot.dashboards, None, "");
        let item = |group: usize, index: usize| &groups[group].items[index];
        assert_eq!(
            item(1, 0).dot,
            Dot::State(CheckableState::Host(HostState::Unreachable))
        );
        assert_eq!(item(1, 0).count, Some(5));
        assert_eq!(item(1, 3).dot, Dot::Ok, "deploy-checks is all OK");
        assert_eq!(item(1, 3).count, None);
        assert_eq!(item(2, 0).dot, Dot::Empty, "sandbox matches nothing");
        assert_eq!(item(2, 0).count, None);
        assert!(groups.iter().all(|group| !group.active), "nothing selected");
    }

    #[test]
    fn collapsed_groups_hide_their_dashboards() {
        let mut demo = setup();
        demo.config.environments[0].groups[1].collapsed = true;
        let environment = &demo.config.environments[0];
        let groups = groups(environment, &demo.snapshot.dashboards, None, "");
        assert!(!groups[1].expanded);
        assert!(groups[1].items.is_empty());
        assert!(groups[0].expanded);
    }

    #[test]
    fn search_filters_dashboards_case_insensitively() {
        let demo = setup();
        let environment = &demo.config.environments[0];
        let groups = groups(environment, &demo.snapshot.dashboards, None, "  NETW ");
        assert_eq!(names(&groups), [("platform", vec!["network"])]);
    }

    #[test]
    fn search_matches_group_names_and_expands_collapsed_groups() {
        let mut demo = setup();
        demo.config.environments[0].groups[2].collapsed = true;
        let environment = &demo.config.environments[0];
        let groups = groups(environment, &demo.snapshot.dashboards, None, "lab");
        assert_eq!(names(&groups), [("lab", vec!["sandbox"])]);
        assert!(groups[0].expanded);
    }

    #[test]
    fn search_without_matches_shows_nothing() {
        let demo = setup();
        let environment = &demo.config.environments[0];
        assert!(groups(environment, &demo.snapshot.dashboards, None, "zzz").is_empty());
    }
}
