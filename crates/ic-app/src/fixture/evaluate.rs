//! Dashboard evaluation for the demo, with the rules the core will use
//! (docs/architecture.md): rows apply `problems_only` then `hide_handled`,
//! sort by the view's key with severity, recency and names as tie-breaks,
//! group with headers ordered by their worst severity, and the summary
//! counts every match.
//!
//! Filters are small Rust predicates that mirror the dashboards' filter
//! expressions; the live core evaluates the expressions with `ic-filter`.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::sync::Arc;

use ic_config::{GroupBy, ObjectKind, Sort, SortKey, View};
use ic_core::snapshot::{DashboardResult, DashboardRow, Snapshot, Summary};
use ic_model::{
    CheckableState, Host, HostName, HostState, ObjectKey, Service, ServiceState, Timestamp,
};

/// The label of the group holding objects without groups.
pub(crate) const UNGROUPED: &str = "ungrouped";

/// Which objects a demo dashboard matches; mirrors its filter expression.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DemoFilter {
    /// The empty filter: everything.
    All,
    /// `host.vars.env == "…"`.
    Env(&'static str),
    /// `host.vars.role in [...]`.
    Roles(&'static [&'static str]),
    /// Certificate checks.
    Certificates,
    /// `service.vars.deploy_check == true`.
    DeployChecks,
}

impl DemoFilter {
    /// The filter expression the dashboard shows.
    pub(crate) fn expression(self) -> String {
        match self {
            Self::All => String::new(),
            Self::Env(env) => format!("host.vars.env == \"{env}\""),
            Self::Roles(roles) => {
                let quoted: Vec<_> = roles.iter().map(|role| format!("\"{role}\"")).collect();
                format!("host.vars.role in [{}]", quoted.join(", "))
            }
            Self::Certificates => {
                "match(\"*cert*\", service.name) || service.name == \"http-tls\"".to_owned()
            }
            Self::DeployChecks => "service.vars.deploy_check == true".to_owned(),
        }
    }

    fn matches(self, host: &Host, service: Option<&Service>) -> bool {
        let host_var = |name: &str| host.vars.get(name).and_then(|value| value.as_str());
        match self {
            Self::All => true,
            Self::Env(env) => host_var("env") == Some(env),
            Self::Roles(roles) => host_var("role").is_some_and(|role| roles.contains(&role)),
            Self::Certificates => service.is_some_and(|service| {
                service.key.name.contains("cert") || &*service.key.name == "http-tls"
            }),
            Self::DeployChecks => service.is_some_and(|service| {
                service
                    .vars
                    .get("deploy_check")
                    .and_then(serde_json::Value::as_bool)
                    == Some(true)
            }),
        }
    }
}

/// A matched host or service with what dashboards need to sort, group and
/// count it.
#[derive(Clone, Debug)]
pub(super) struct Match {
    pub(super) key: ObjectKey,
    pub(super) host_name: HostName,
    pub(super) service_name: Option<Arc<str>>,
    pub(super) state: CheckableState,
    pub(super) problem: bool,
    pub(super) handled: bool,
    pub(super) severity: u32,
    pub(super) last_state_change: Timestamp,
    pub(super) host_groups: Vec<String>,
    pub(super) service_groups: Vec<String>,
}

impl Match {
    fn host(host: &Host) -> Self {
        Self {
            key: host.key(),
            host_name: host.name.clone(),
            service_name: None,
            state: CheckableState::Host(host.state),
            problem: host.is_problem(),
            handled: host.is_handled(),
            severity: host.severity(),
            last_state_change: host.check.last_state_change,
            host_groups: host.groups.clone(),
            service_groups: Vec::new(),
        }
    }

    fn service(service: &Service, host: Option<&Host>) -> Self {
        let host_problem = host.is_some_and(Host::is_problem);
        Self {
            key: service.object_key(),
            host_name: service.key.host.clone(),
            service_name: Some(service.key.name.clone()),
            state: CheckableState::Service(service.state),
            problem: service.is_problem(),
            handled: service.is_handled(host_problem),
            severity: service.severity(),
            last_state_change: service.check.last_state_change,
            host_groups: host.map(|host| host.groups.clone()).unwrap_or_default(),
            service_groups: service.groups.clone(),
        }
    }
}

/// Evaluates `view` over `snapshot` with `filter`.
pub(crate) fn evaluate(snapshot: &Snapshot, view: &View, filter: DemoFilter) -> DashboardResult {
    evaluate_with(snapshot, view, |host, service| {
        filter.matches(host, service)
    })
}

/// Evaluates `view` over `snapshot` with its filter expression, parsed and
/// evaluated by `ic-filter` the way the core does: a filter that doesn't
/// parse is an error naming the line and column, like the core's.
///
/// # Errors
///
/// The filter doesn't parse.
pub(crate) fn evaluate_expression(
    snapshot: &Snapshot,
    view: &View,
) -> Result<DashboardResult, String> {
    let filter = ic_filter::Filter::parse(&view.filter).map_err(|error| {
        let (line, column) = error.line_column(&view.filter);
        format!("{} (line {line}, column {column})", error.message)
    })?;
    Ok(evaluate_with(
        snapshot,
        view,
        |host, service| match service {
            Some(service) => filter.matches(&ic_filter::ServiceScope {
                service,
                host: Some(host),
            }),
            None => filter.matches(&ic_filter::HostScope { host }),
        },
    ))
}

/// Evaluates `view` over `snapshot`, matching with `matches(host, service)`
/// (`None` for host views).
fn evaluate_with(
    snapshot: &Snapshot,
    view: &View,
    matches: impl Fn(&Host, Option<&Service>) -> bool,
) -> DashboardResult {
    let matches: Vec<Match> = match view.object_kind {
        ObjectKind::Hosts => snapshot
            .hosts
            .values()
            .filter(|host| matches(host, None))
            .map(|host| Match::host(host))
            .collect(),
        ObjectKind::Services => snapshot
            .services
            .values()
            .filter_map(|service| {
                let host = snapshot.host_of(&service.key).map(Arc::as_ref);
                let keep = host.is_some_and(|host| matches(host, Some(service)));
                keep.then(|| Match::service(service, host))
            })
            .collect(),
    };
    let summary = summarize(&matches);
    let mut visible: Vec<Match> = matches
        .into_iter()
        .filter(|object| !view.problems_only || object.problem)
        .filter(|object| !view.hide_handled || !object.handled)
        .collect();
    visible.sort_by(|a, b| compare(a, b, view.sort));
    let label = |name: &str, groups: &[(String, String)]| {
        groups
            .iter()
            .find(|(group, _)| group == name)
            .map_or_else(|| name.to_owned(), |(_, display)| display.clone())
    };
    let host_groups: Vec<(String, String)> = snapshot
        .host_groups
        .iter()
        .map(|group| (group.name.clone(), group.display_name.clone()))
        .collect();
    let service_groups: Vec<(String, String)> = snapshot
        .service_groups
        .iter()
        .map(|group| (group.name.clone(), group.display_name.clone()))
        .collect();
    let rows = group_rows(visible, view.group_by, |object| match view.group_by {
        GroupBy::None => Vec::new(),
        GroupBy::Host => vec![object.host_name.to_string()],
        GroupBy::HostGroup => object
            .host_groups
            .iter()
            .map(|group| label(group, &host_groups))
            .collect(),
        GroupBy::ServiceGroup => object
            .service_groups
            .iter()
            .map(|group| label(group, &service_groups))
            .collect(),
    });
    DashboardResult {
        rows: Arc::new(rows),
        summary,
        error: None,
    }
}

/// Every host's and service's counts, as the core's `Snapshot::overall`.
pub(super) fn overall(snapshot: &Snapshot) -> Summary {
    let matches: Vec<Match> = snapshot
        .hosts
        .values()
        .map(|host| Match::host(host))
        .chain(snapshot.services.values().map(|service| {
            Match::service(service, snapshot.host_of(&service.key).map(AsRef::as_ref))
        }))
        .collect();
    summarize(&matches)
}

pub(super) fn summarize(matches: &[Match]) -> Summary {
    let mut summary = Summary::default();
    let mut worst: Option<&Match> = None;
    for object in matches {
        let counter = match object.state {
            CheckableState::Service(ServiceState::Ok) | CheckableState::Host(HostState::Up) => {
                &mut summary.ok
            }
            CheckableState::Service(ServiceState::Warning) => &mut summary.warning,
            CheckableState::Service(ServiceState::Critical) => &mut summary.critical,
            CheckableState::Service(ServiceState::Unknown) => &mut summary.unknown,
            CheckableState::Host(HostState::Down) => &mut summary.down,
            CheckableState::Host(HostState::Unreachable) => &mut summary.unreachable,
            CheckableState::Service(ServiceState::Pending)
            | CheckableState::Host(HostState::Pending) => &mut summary.pending,
        };
        *counter += 1;
        if !object.problem {
            continue;
        }
        if object.handled {
            summary.handled += 1;
        } else {
            summary.unhandled += 1;
            if worst.is_none_or(|worst| object.severity > worst.severity) {
                worst = Some(object);
            }
        }
    }
    summary.worst_unhandled = worst.map(|object| object.state);
    summary
}

/// The view's sort key, then severity (desc), last state change (newest
/// first), host and service name.
pub(super) fn compare(a: &Match, b: &Match, sort: Sort) -> Ordering {
    let primary = match sort.key {
        SortKey::Severity => a.severity.cmp(&b.severity),
        SortKey::LastStateChange => cmp_time(a.last_state_change, b.last_state_change),
        SortKey::Host => a.host_name.cmp(&b.host_name),
        SortKey::Service => a
            .service_name
            .as_deref()
            .unwrap_or(a.host_name.as_str())
            .cmp(b.service_name.as_deref().unwrap_or(b.host_name.as_str())),
    };
    let primary = if sort.descending {
        primary.reverse()
    } else {
        primary
    };
    primary
        .then_with(|| b.severity.cmp(&a.severity))
        .then_with(|| cmp_time(b.last_state_change, a.last_state_change))
        .then_with(|| a.host_name.cmp(&b.host_name))
        .then_with(|| a.service_name.cmp(&b.service_name))
}

/// Inserts group headers: an object appears under each group `groups_of`
/// returns, in its sorted order. Groups are ordered by their worst severity
/// (desc), then label; objects without groups go under "ungrouped", last.
pub(super) fn group_rows(
    sorted: Vec<Match>,
    group_by: GroupBy,
    groups_of: impl Fn(&Match) -> Vec<String>,
) -> Vec<DashboardRow> {
    if group_by == GroupBy::None {
        return sorted
            .into_iter()
            .map(|object| DashboardRow::Object(object.key))
            .collect();
    }
    let mut groups: BTreeMap<String, (u32, Vec<ObjectKey>)> = BTreeMap::new();
    let mut ungrouped: Vec<ObjectKey> = Vec::new();
    for object in &sorted {
        let labels = groups_of(object);
        if labels.is_empty() {
            ungrouped.push(object.key.clone());
        }
        for label in labels {
            let (worst, keys) = groups.entry(label).or_insert((0, Vec::new()));
            *worst = (*worst).max(object.severity);
            keys.push(object.key.clone());
        }
    }
    let mut ordered: Vec<(String, u32, Vec<ObjectKey>)> = groups
        .into_iter()
        .map(|(label, (worst, keys))| (label, worst, keys))
        .collect();
    ordered.sort_by(|(a_label, a_worst, _), (b_label, b_worst, _)| {
        b_worst.cmp(a_worst).then_with(|| a_label.cmp(b_label))
    });
    if !ungrouped.is_empty() {
        ordered.push((UNGROUPED.to_owned(), 0, ungrouped));
    }
    ordered
        .into_iter()
        .flat_map(|(label, _, keys)| {
            let header = DashboardRow::Group {
                label,
                count: keys.len(),
            };
            std::iter::once(header).chain(keys.into_iter().map(DashboardRow::Object))
        })
        .collect()
}

fn cmp_time(a: Timestamp, b: Timestamp) -> Ordering {
    a.as_unix_seconds().total_cmp(&b.as_unix_seconds())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make(host: &str, severity: u32, age: f64, groups: &[&str]) -> Match {
        let now = 1000.;
        Match {
            key: ObjectKey::host(host),
            host_name: HostName::new(host),
            service_name: None,
            state: CheckableState::Host(HostState::Down),
            problem: true,
            handled: false,
            severity,
            last_state_change: Timestamp::from_unix_seconds(now - age),
            host_groups: groups.iter().map(|group| (*group).to_owned()).collect(),
            service_groups: Vec::new(),
        }
    }

    #[test]
    fn sorting_honours_the_key_and_direction() {
        let a = make("a", 10, 50., &[]);
        let b = make("b", 20, 10., &[]);
        let by_severity = Sort {
            key: SortKey::Severity,
            descending: true,
        };
        assert_eq!(compare(&a, &b, by_severity), Ordering::Greater);
        let by_host = Sort {
            key: SortKey::Host,
            descending: false,
        };
        assert_eq!(compare(&a, &b, by_host), Ordering::Less);
        let by_recency = Sort {
            key: SortKey::LastStateChange,
            descending: true,
        };
        assert_eq!(
            compare(&a, &b, by_recency),
            Ordering::Greater,
            "b changed later"
        );
        let same = make("a", 10, 50., &[]);
        assert_eq!(compare(&a, &same, by_severity), Ordering::Equal);
    }

    #[test]
    fn groups_are_ordered_by_worst_severity_with_ungrouped_last() {
        let sorted = vec![
            make("a", 30, 1., &["web"]),
            make("b", 20, 1., &["db", "web"]),
            make("c", 10, 1., &[]),
            make("d", 40, 1., &["db"]),
        ];
        let rows = group_rows(sorted, GroupBy::HostGroup, |object| {
            object.host_groups.clone()
        });
        let labels: Vec<String> = rows
            .iter()
            .map(|row| match row {
                DashboardRow::Group { label, count } => format!("[{label} {count}]"),
                DashboardRow::Object(key) => key.full_name(),
            })
            .collect();
        assert_eq!(
            labels,
            [
                "[db 2]",
                "b",
                "d",
                "[web 2]",
                "a",
                "b",
                "[ungrouped 1]",
                "c"
            ],
            "db's worst (40) beats web's (30); b appears in both"
        );
    }

    #[test]
    fn no_grouping_keeps_the_sorted_order() {
        let rows = group_rows(
            vec![make("b", 1, 1., &[]), make("a", 1, 1., &[])],
            GroupBy::None,
            |_| Vec::new(),
        );
        assert_eq!(
            rows,
            [
                DashboardRow::Object(ObjectKey::host("b")),
                DashboardRow::Object(ObjectKey::host("a"))
            ]
        );
    }

    #[test]
    fn filters_render_as_icinga_expressions() {
        assert_eq!(DemoFilter::All.expression(), "");
        assert_eq!(
            DemoFilter::Roles(&["switch", "edge"]).expression(),
            "host.vars.role in [\"switch\", \"edge\"]"
        );
        assert_eq!(
            DemoFilter::Env("prod").expression(),
            "host.vars.env == \"prod\""
        );
    }
}
