//! What the sidebar shows, computed from the environment, the evaluated
//! dashboards, the selection and the search query. Pure, so it's tested
//! without a window.

use std::collections::BTreeMap;

use ic_config::{Dashboard, DashboardGroup, EffectiveMark, Environment, ViewDisplay};
use ic_core::snapshot::{DashboardResult, Snapshot, Summary};
use ic_model::{CheckableState, ObjectKey, ServiceState, Timestamp};
use ic_rules::DashboardRef;
use ic_ui_kit::{IconName, ObjectMark};

use crate::cluster::{ClusterEntry, ClusterState};
use crate::lists::ListKind;

/// A dashboard's or an object's state dot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Dot {
    /// The worst unhandled problem's state.
    State(CheckableState),
    /// Objects match and nothing is unhandled (green).
    Ok,
    /// Nothing matches, or everything is still pending (grey).
    Empty,
    /// An object that counts as handled: a hollow ring in its state's
    /// colour (hollow = handled; an OK service in downtime is a hollow
    /// green ring).
    Handled(CheckableState),
}

impl Dot {
    /// The dot for an object's mark ([`ObjectMark`]): hollow when it counts
    /// as handled, else as [`Dot::for_object`].
    pub(crate) fn for_mark(mark: ObjectMark) -> Self {
        if mark.hollow {
            Self::Handled(mark.state)
        } else {
            Self::for_object(Some(mark.state))
        }
    }

    /// How bad the dot is, for picking the worst of several: an unhandled
    /// problem first, then the redder state
    /// ([`CheckableState::severity_rank`], the order every dot follows).
    pub(crate) fn rank(self) -> (bool, u8) {
        match self {
            Self::State(state) => (state.is_problem(), state.severity_rank()),
            Self::Handled(state) => (false, state.severity_rank()),
            Self::Ok | Self::Empty => (false, 0),
        }
    }

    /// Whether it's drawn as a ring.
    pub(crate) fn is_hollow(self) -> bool {
        matches!(self, Self::Handled(_))
    }

    /// The dot for one object: its state if it has a problem, green when OK,
    /// grey while pending or when it's gone.
    pub(crate) fn for_object(state: Option<CheckableState>) -> Self {
        match state {
            Some(state) if state.is_problem() => Self::State(state),
            Some(
                CheckableState::Service(ServiceState::Ok)
                | CheckableState::Host(ic_model::HostState::Up),
            ) => Self::Ok,
            Some(_) | None => Self::Empty,
        }
    }

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

/// What a row shows in its fixed mark slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mark {
    /// A state dot.
    Dot(Dot),
    /// An icon (a dashboard's chosen mark, a view kind's icon, a cluster
    /// entry's): monochrome and neutral, so colour keeps meaning state.
    Icon(IconName),
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
    /// The mark: the worst state's dot, or an icon (the sidebar mark).
    pub(crate) mark: Mark,
    /// Unhandled problems, if any; a dashboard without a problem view:
    /// its first view's count (objects being handled, downtimes in
    /// effect).
    pub(crate) count: Option<u32>,
    /// Its notifications are off (a bell-off glyph).
    pub(crate) muted: bool,
}

/// A row of the cluster section.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ClusterRow {
    /// Which entry.
    pub(crate) entry: ClusterEntry,
    /// Its mark: the entry's icon, or health's state dot.
    pub(crate) mark: Mark,
    /// Its count (handling: objects being handled; downtimes: in effect
    /// now); events and health have none.
    pub(crate) count: Option<usize>,
    /// Whether it's shown.
    pub(crate) active: bool,
}

/// The cluster section's rows (topic 14, round 5): handling and downtimes
/// count the whole environment; health has the cluster's state dot.
pub(crate) fn cluster_rows(
    snapshot: &Snapshot,
    active: Option<ClusterEntry>,
    state: ClusterState,
    now: Timestamp,
) -> Vec<ClusterRow> {
    ClusterEntry::ALL
        .iter()
        .map(|entry| {
            let count = entry.list().map(|kind| {
                crate::lists::model::count(
                    kind,
                    snapshot,
                    None,
                    ic_config::DowntimeKinds::default(),
                    now,
                )
            });
            let mark = match entry.icon() {
                Some(icon) => Mark::Icon(icon),
                None => Mark::Dot(match state {
                    ClusterState::Ok => Dot::Ok,
                    ClusterState::Critical => {
                        Dot::State(CheckableState::Service(ServiceState::Critical))
                    }
                    ClusterState::Unknown => Dot::Empty,
                }),
            };
            ClusterRow {
                entry: *entry,
                mark,
                count: count.filter(|count| *count > 0),
                active: active == Some(*entry),
            }
        })
        .collect()
}

/// A view display's icon, as the view header's mark slot and the sidebar
/// show it.
pub(crate) fn display_icon(display: ViewDisplay) -> IconName {
    crate::dashboard::header::display_icon(display)
}

/// A dashboard's mark and count (topic 14, round 5): with a problem view,
/// the worst unhandled state's dot (or the icon chosen) and the problem
/// count; without one, the first view's kind icon (or the icon chosen) and
/// that view's count: objects being handled, downtimes in effect, none for
/// events.
pub(crate) fn mark_and_count(
    dashboard: &Dashboard,
    result: Option<&DashboardResult>,
    snapshot: &Snapshot,
    now: Timestamp,
) -> (Mark, Option<u32>) {
    let first = dashboard.views.first();
    let kind_icon = || {
        Mark::Icon(display_icon(
            first.map_or(ViewDisplay::List, |view| view.display),
        ))
    };
    let mark = match dashboard.effective_mark() {
        EffectiveMark::State => Mark::Dot(Dot::from_summary(result.map(|result| &result.summary))),
        EffectiveMark::Icon(name) => {
            IconName::from_lucide_name(name).map_or_else(kind_icon, Mark::Icon)
        }
        EffectiveMark::KindIcon => kind_icon(),
    };
    let count = if dashboard.has_problem_view() {
        result
            .map(|result| result.summary.unhandled)
            .filter(|unhandled| *unhandled > 0)
    } else {
        first.and_then(|view| {
            let kind = ListKind::of_display(view.display)?;
            let members = result?.view(&view.id)?.members()?;
            let count =
                crate::lists::model::count(kind, snapshot, Some(members), view.threads.shows, now);
            u32::try_from(count).ok().filter(|count| *count > 0)
        })
    };
    (mark, count)
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

/// An object open as a tab, in the sidebar's "open" section.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct OpenTab {
    /// The object.
    pub(crate) key: ObjectKey,
    /// Service or host name.
    pub(crate) name: String,
    /// The host, for services.
    pub(crate) host: Option<String>,
    /// The object's state dot.
    pub(crate) dot: Dot,
    /// Whether it's the tab shown.
    pub(crate) active: bool,
}

/// The "open" section's rows.
pub(crate) fn open_tabs(
    tabs: &[ObjectKey],
    active: Option<&ObjectKey>,
    snapshot: &Snapshot,
) -> Vec<OpenTab> {
    tabs.iter()
        .map(|key| {
            let (name, host, mark) = match key {
                ObjectKey::Host { name } => {
                    let host = snapshot.hosts.get(name);
                    (
                        host.map_or_else(|| name.to_string(), |host| host.display_name.clone()),
                        None,
                        host.map(|host| ObjectMark::host(host)),
                    )
                }
                ObjectKey::Service { key: service_key } => {
                    let service = snapshot.services.get(service_key);
                    (
                        service.map_or_else(
                            || service_key.name.to_string(),
                            |service| service.display_name.clone(),
                        ),
                        Some(service_key.host.to_string()),
                        service.map(|service| {
                            ObjectMark::service(
                                service,
                                snapshot.host_of(service_key).map(AsRef::as_ref),
                            )
                        }),
                    )
                }
            };
            OpenTab {
                key: key.clone(),
                name,
                host,
                dot: mark.map_or(Dot::Empty, Dot::for_mark),
                active: active == Some(key),
            }
        })
        .collect()
}

/// The groups to show for `query` (case-insensitive; empty shows all).
///
/// A search matches dashboard names, and group names (showing the whole
/// group); groups without matches are left out and the rest are shown
/// expanded, even if collapsed.
pub(crate) fn groups<'a>(
    environment: &'a Environment,
    snapshot: &Snapshot,
    selected: Option<&DashboardRef>,
    query: &str,
    now: Timestamp,
) -> Vec<SidebarGroup<'a>> {
    let results: &BTreeMap<DashboardRef, DashboardResult> = &snapshot.dashboards;
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
                    let (mark, count) =
                        mark_and_count(dashboard, results.get(&reference), snapshot, now);
                    SidebarItem {
                        selected: selected == Some(&reference),
                        reference,
                        name: &dashboard.name,
                        mark,
                        count,
                        muted: dashboard.notifications == ic_rules::ScopeSetting::Off,
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
    use ic_model::{HostState, ServiceState};

    use super::*;
    use crate::fixture;

    /// The palette's stacks pick their dot by this: an unhandled problem
    /// before a handled one, then the redder state, the order of every
    /// other dot (a down host before an unknown service, although Icinga's
    /// severity weighs the unknown higher).
    #[test]
    fn dots_rank_unhandled_first_then_by_the_redder_state() {
        let down = Dot::State(CheckableState::Host(HostState::Down));
        let unknown = Dot::State(CheckableState::Service(ServiceState::Unknown));
        let critical = CheckableState::Service(ServiceState::Critical);
        assert!(down.rank() > unknown.rank());
        assert!(Dot::State(critical).rank() > down.rank());
        assert!(
            Dot::State(CheckableState::Service(ServiceState::Warning)).rank()
                > Dot::Handled(critical).rank()
        );
        assert!(Dot::Handled(critical).rank() > Dot::Ok.rank());
    }

    fn now() -> Timestamp {
        Timestamp::from_unix_seconds(1_790_000_000.)
    }

    fn setup() -> fixture::Fixture {
        fixture::build(now())
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
        let groups = groups(environment, &demo.snapshot, Some(&demo.selected), "", now());
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
        let groups = groups(environment, &demo.snapshot, None, "", now());
        let item = |group: usize, index: usize| &groups[group].items[index];
        assert_eq!(
            item(1, 0).mark,
            Mark::Dot(Dot::State(CheckableState::Host(HostState::Unreachable)))
        );
        assert_eq!(item(1, 0).count, Some(5));
        assert_eq!(
            item(1, 3).mark,
            Mark::Dot(Dot::Ok),
            "deploy-checks is all OK"
        );
        assert_eq!(item(1, 3).count, None);
        assert_eq!(
            item(2, 0).mark,
            Mark::Dot(Dot::Empty),
            "sandbox matches nothing"
        );
        assert_eq!(item(2, 0).count, None);
        assert!(groups.iter().all(|group| !group.active), "nothing selected");
    }

    #[test]
    fn collapsed_groups_hide_their_dashboards() {
        let mut demo = setup();
        demo.config.environments[0].groups[1].collapsed = true;
        let environment = &demo.config.environments[0];
        let groups = groups(environment, &demo.snapshot, None, "", now());
        assert!(!groups[1].expanded);
        assert!(groups[1].items.is_empty());
        assert!(groups[0].expanded);
    }

    #[test]
    fn search_filters_dashboards_case_insensitively() {
        let demo = setup();
        let environment = &demo.config.environments[0];
        let groups = groups(environment, &demo.snapshot, None, "  NETW ", now());
        assert_eq!(names(&groups), [("platform", vec!["network"])]);
    }

    #[test]
    fn search_matches_group_names_and_expands_collapsed_groups() {
        let mut demo = setup();
        demo.config.environments[0].groups[2].collapsed = true;
        let environment = &demo.config.environments[0];
        let groups = groups(environment, &demo.snapshot, None, "lab", now());
        assert_eq!(names(&groups), [("lab", vec!["sandbox"])]);
        assert!(groups[0].expanded);
    }

    #[test]
    fn open_tabs_show_state_and_host() {
        let demo = setup();
        let replication = ObjectKey::service("db-prod-03", "postgres-replication");
        let tabs = [
            replication.clone(),
            ObjectKey::host("db-prod-03"),
            ObjectKey::host("gone"),
        ];
        let rows = open_tabs(&tabs, Some(&replication), &demo.snapshot);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].name, "postgres-replication");
        assert_eq!(rows[0].host.as_deref(), Some("db-prod-03"));
        assert_eq!(
            rows[0].dot,
            Dot::State(CheckableState::Service(ServiceState::Critical))
        );
        assert!(rows[0].active);
        assert_eq!(rows[1].dot, Dot::Ok);
        assert_eq!(rows[1].host, None);
        assert!(!rows[1].active);
        assert_eq!(rows[2].dot, Dot::Empty, "gone objects are grey");
        assert_eq!(rows[2].name, "gone");
    }

    #[test]
    fn search_without_matches_shows_nothing() {
        let demo = setup();
        let environment = &demo.config.environments[0];
        assert!(groups(environment, &demo.snapshot, None, "zzz", now()).is_empty());
    }
}
