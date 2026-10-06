//! Built-in demo data: the design's `prod-cluster` sample objects as an
//! `ic_config::Config` and an `ic_core::snapshot::Snapshot`.
//!
//! It stands in for the live core until the runtime exists. Dashboards are
//! evaluated here ([`evaluate`]) with the rules the core will use, so
//! counts, dots, rows and summaries agree. [`DemoOptions::generated_rows`]
//! adds a generated `load` environment for testing large lists.

mod evaluate;
mod generated;
mod objects;

use std::collections::BTreeMap;
use std::sync::Arc;

use ic_config::{
    AuthConfig, CONFIG_VERSION, Config, Dashboard, DashboardGroup, Environment, General, GroupBy,
    ObjectKind, Sort, View,
};
use ic_core::snapshot::{DashboardResult, Snapshot};
use ic_model::Timestamp;
use ic_rules::{DashboardRef, ScopeSetting};

use evaluate::DemoFilter;

/// The demo environment's id (stable, so tests and screenshots can refer to it).
pub(crate) const ENVIRONMENT_ID: &str = "demo-prod-cluster";
/// The endpoint the demo pretends to be connected to.
pub(crate) const ENDPOINT: &str = "master-01";

/// What to add to the design's sample data.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DemoOptions {
    /// Generated services for load testing, listed by the `lab / load-test`
    /// dashboard (which is then selected at start). 0 adds nothing.
    pub(crate) generated_rows: usize,
}

/// The demo's configuration, snapshot and initial selection.
#[derive(Clone, Debug)]
pub(crate) struct Demo {
    /// One environment, `prod-cluster`, with three dashboard groups.
    pub(crate) config: Config,
    /// Objects and evaluated dashboards.
    pub(crate) snapshot: Snapshot,
    /// The dashboard selected at start (`overview / production`).
    pub(crate) selected: DashboardRef,
    /// Re-evaluates dashboards when their views change.
    pub(crate) evaluator: Evaluator,
}

/// Evaluates the demo's dashboards; stands in for the core's dashboard
/// evaluation.
#[derive(Clone, Debug, Default)]
pub(crate) struct Evaluator {
    filters: BTreeMap<DashboardRef, DemoFilter>,
}

impl Evaluator {
    /// Evaluates `reference` with `view` over `snapshot`; `None` for
    /// dashboards the demo doesn't know.
    pub(crate) fn evaluate(
        &self,
        snapshot: &Snapshot,
        reference: &DashboardRef,
        view: &View,
    ) -> Option<DashboardResult> {
        let filter = self.filters.get(reference)?;
        Some(evaluate::evaluate(snapshot, view, *filter))
    }

    /// Evaluates every dashboard of `config`'s active environment.
    pub(crate) fn evaluate_all(
        &self,
        snapshot: &Snapshot,
        config: &Config,
    ) -> BTreeMap<DashboardRef, DashboardResult> {
        let active = config.active_environment.as_deref();
        config
            .environments
            .iter()
            .filter(|environment| Some(environment.id.as_str()) == active)
            .flat_map(|environment| environment.groups.iter())
            .flat_map(|group| {
                group.dashboards.iter().map(move |dashboard| {
                    (
                        DashboardRef {
                            group_id: group.id.clone(),
                            dashboard_id: dashboard.id.clone(),
                        },
                        &dashboard.view,
                    )
                })
            })
            .filter_map(|(reference, view)| {
                let result = self.evaluate(snapshot, &reference, view)?;
                Some((reference, result))
            })
            .collect()
    }
}

/// Builds the demo as of `now`; times in state are relative to it.
#[cfg(test)]
pub(crate) fn build(now: Timestamp) -> Demo {
    build_with(now, DemoOptions::default())
}

/// Builds the demo as of `now` with `options`.
pub(crate) fn build_with(now: Timestamp, options: DemoOptions) -> Demo {
    let mut hosts = objects::hosts(now);
    let mut services = objects::services(now, &hosts);
    if options.generated_rows > 0 {
        let (generated_hosts, generated_services) =
            generated::generate(now, options.generated_rows);
        hosts.extend(generated_hosts);
        services.extend(generated_services);
    }
    let mut snapshot = Snapshot {
        revision: 1,
        taken_at: now,
        hosts: Arc::new(
            hosts
                .into_iter()
                .map(|host| (host.name.clone(), Arc::new(host)))
                .collect(),
        ),
        services: Arc::new(
            services
                .into_iter()
                .map(|service| (service.key.clone(), Arc::new(service)))
                .collect(),
        ),
        comments: Arc::new(objects::comments(now)),
        downtimes: Arc::new(objects::downtimes(now)),
        host_groups: Arc::new(objects::host_groups()),
        service_groups: Arc::new(objects::service_groups()),
        dependencies: Arc::new(objects::dependencies()),
        endpoints: Arc::new(objects::endpoints()),
        status: Some(Arc::new(objects::status(now))),
        dashboards: Arc::default(),
        ..Snapshot::default()
    };

    let mut groups = Vec::new();
    let mut filters = BTreeMap::new();
    for (group_name, dashboards) in DASHBOARDS {
        let group_id = format!("demo-{group_name}");
        let mut group = DashboardGroup {
            id: group_id.clone(),
            name: (*group_name).to_owned(),
            collapsed: false,
            notifications: ScopeSetting::Inherit,
            dashboards: Vec::new(),
        };
        let generated = (*group_name == "lab" && options.generated_rows > 0)
            .then_some(&LOAD_TEST)
            .into_iter();
        for spec in dashboards.iter().chain(generated) {
            let dashboard = Dashboard {
                id: format!("{group_id}-{}", spec.name),
                name: spec.name.to_owned(),
                view: spec.view(),
                notifications: ScopeSetting::Inherit,
            };
            filters.insert(
                DashboardRef {
                    group_id: group_id.clone(),
                    dashboard_id: dashboard.id.clone(),
                },
                spec.filter,
            );
            group.dashboards.push(dashboard);
        }
        groups.push(group);
    }

    let environment = Environment {
        id: ENVIRONMENT_ID.to_owned(),
        name: "prod-cluster".to_owned(),
        url: "https://master-01.example.com:5665".to_owned(),
        auth: AuthConfig::Basic {
            username: "icygui".to_owned(),
        },
        groups,
        ..Environment::default()
    };
    let config = Config {
        version: CONFIG_VERSION,
        general: General::default(),
        active_environment: Some(ENVIRONMENT_ID.to_owned()),
        environments: vec![environment],
    };
    let evaluator = Evaluator { filters };
    snapshot.dashboards = Arc::new(evaluator.evaluate_all(&snapshot, &config));
    let selected = if options.generated_rows > 0 {
        DashboardRef {
            group_id: "demo-lab".to_owned(),
            dashboard_id: format!("demo-lab-{}", LOAD_TEST.name),
        }
    } else {
        DashboardRef {
            group_id: "demo-overview".to_owned(),
            dashboard_id: "demo-overview-production".to_owned(),
        }
    };
    Demo {
        config,
        snapshot,
        selected,
        evaluator,
    }
}

struct DashboardSpec {
    name: &'static str,
    kind: ObjectKind,
    filter: DemoFilter,
    problems_only: bool,
    hide_handled: bool,
    group_by: GroupBy,
}

impl DashboardSpec {
    fn view(&self) -> View {
        View {
            object_kind: self.kind,
            filter: self.filter.expression(),
            problems_only: self.problems_only,
            hide_handled: self.hide_handled,
            sort: Sort::default(),
            group_by: self.group_by,
        }
    }
}

const fn spec(
    name: &'static str,
    kind: ObjectKind,
    filter: DemoFilter,
    problems_only: bool,
    hide_handled: bool,
) -> DashboardSpec {
    DashboardSpec {
        name,
        kind,
        filter,
        problems_only,
        hide_handled,
        group_by: GroupBy::None,
    }
}

/// Groups (folders) and their dashboards, in sidebar order. The names are the
/// design's; per PLAN.md D1 they're folders inside one environment.
const DASHBOARDS: &[(&str, &[DashboardSpec])] = &[
    (
        "overview",
        &[
            spec(
                "overview",
                ObjectKind::Services,
                DemoFilter::All,
                true,
                true,
            ),
            spec(
                "production",
                ObjectKind::Services,
                DemoFilter::Env("prod"),
                true,
                false,
            ),
            DashboardSpec {
                group_by: GroupBy::Host,
                ..spec(
                    "databases",
                    ObjectKind::Services,
                    DemoFilter::Roles(&["postgres", "redis"]),
                    true,
                    false,
                )
            },
        ],
    ),
    (
        "platform",
        &[
            spec(
                "network",
                ObjectKind::Hosts,
                DemoFilter::Roles(&["switch", "edge", "lb", "vpn"]),
                true,
                false,
            ),
            spec(
                "kubernetes",
                ObjectKind::Services,
                DemoFilter::Roles(&["k8s"]),
                true,
                true,
            ),
            spec(
                "certificates",
                ObjectKind::Services,
                DemoFilter::Certificates,
                true,
                false,
            ),
            spec(
                "deploy-checks",
                ObjectKind::Services,
                DemoFilter::DeployChecks,
                false,
                true,
            ),
        ],
    ),
    (
        "lab",
        &[spec(
            "sandbox",
            ObjectKind::Services,
            DemoFilter::Env("lab"),
            true,
            true,
        )],
    ),
];

/// Lists every generated service (`DemoOptions::generated_rows`).
const LOAD_TEST: DashboardSpec = spec(
    "load-test",
    ObjectKind::Services,
    DemoFilter::Env(generated::ENV),
    false,
    false,
);

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use ic_config::{SortKey, View};
    use ic_core::snapshot::{DashboardRow, Summary};
    use ic_model::{CheckableState, HostName, HostState, ObjectKey, ServiceKey, ServiceState};

    use super::*;

    fn demo() -> Demo {
        build(Timestamp::from_unix_seconds(1_790_000_000.))
    }

    fn reference(group: &str, dashboard: &str) -> DashboardRef {
        DashboardRef {
            group_id: format!("demo-{group}"),
            dashboard_id: format!("demo-{group}-{dashboard}"),
        }
    }

    fn result<'a>(demo: &'a Demo, group: &str, dashboard: &str) -> &'a DashboardResult {
        demo.snapshot
            .dashboards
            .get(&reference(group, dashboard))
            .unwrap()
    }

    fn row_names(result: &DashboardResult) -> Vec<String> {
        result
            .rows
            .iter()
            .map(|row| match row {
                DashboardRow::Object(key) => key.full_name(),
                DashboardRow::Group { label, .. } => format!("[{label}]"),
            })
            .collect()
    }

    #[test]
    fn one_environment_with_the_designs_folders() {
        let demo = demo();
        let config = &demo.config;
        assert_eq!(config.environments.len(), 1);
        assert_eq!(config.active_environment.as_deref(), Some(ENVIRONMENT_ID));
        let environment = &config.environments[0];
        assert_eq!(environment.name, "prod-cluster");
        let names: Vec<_> = environment.groups.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, ["overview", "platform", "lab"]);
        let dashboards: Vec<_> = environment
            .groups
            .iter()
            .flat_map(|group| group.dashboards.iter().map(|d| d.name.as_str()))
            .collect();
        assert_eq!(
            dashboards,
            [
                "overview",
                "production",
                "databases",
                "network",
                "kubernetes",
                "certificates",
                "deploy-checks",
                "sandbox"
            ]
        );
    }

    #[test]
    fn ids_are_unique_and_every_dashboard_has_a_result() {
        let demo = demo();
        let environment = &demo.config.environments[0];
        let mut ids = HashSet::new();
        for group in &environment.groups {
            assert!(ids.insert(group.id.clone()));
            for dashboard in &group.dashboards {
                assert!(ids.insert(dashboard.id.clone()));
                let reference = DashboardRef {
                    group_id: group.id.clone(),
                    dashboard_id: dashboard.id.clone(),
                };
                assert!(demo.snapshot.dashboards.contains_key(&reference));
            }
        }
        assert!(demo.snapshot.dashboards.contains_key(&demo.selected));
    }

    #[test]
    fn rows_reference_objects_in_the_snapshot() {
        let demo = demo();
        for result in demo.snapshot.dashboards.values() {
            for row in result.rows.iter() {
                if let DashboardRow::Object(key) = row {
                    let exists = match key {
                        ObjectKey::Host { name } => demo.snapshot.hosts.contains_key(name),
                        ObjectKey::Service { key } => demo.snapshot.services.contains_key(key),
                    };
                    assert!(exists, "{key} is missing");
                }
            }
        }
    }

    #[test]
    fn dashboards_cover_every_dot_colour() {
        let demo = demo();
        let worst = |group, dashboard| result(&demo, group, dashboard).summary.worst_unhandled;
        let critical = Some(CheckableState::Service(ServiceState::Critical));
        assert_eq!(worst("overview", "overview"), critical);
        assert_eq!(worst("overview", "production"), critical);
        assert_eq!(worst("overview", "databases"), critical);
        assert_eq!(
            worst("platform", "network"),
            Some(CheckableState::Host(HostState::Unreachable))
        );
        assert_eq!(
            worst("platform", "certificates"),
            Some(CheckableState::Service(ServiceState::Warning))
        );
        assert_eq!(worst("platform", "deploy-checks"), None);
        assert_eq!(worst("lab", "sandbox"), None);
        assert!(result(&demo, "platform", "deploy-checks").summary.ok > 0);
        assert_eq!(result(&demo, "lab", "sandbox").summary, Summary::default());
    }

    #[test]
    fn network_counts_the_unreachable_hosts_behind_the_switch() {
        let demo = demo();
        let summary = result(&demo, "platform", "network").summary;
        assert_eq!(summary.unreachable, 5);
        assert_eq!(
            summary.unhandled, 5,
            "the switch is acked, edge-fra-04 in downtime"
        );
        assert_eq!(summary.handled, 2);
        assert_eq!(summary.down, 2);
    }

    #[test]
    fn production_shows_the_designs_rows_including_handled_ones() {
        let demo = demo();
        let production = result(&demo, "overview", "production");
        let rows = row_names(production);
        for expected in [
            "db-prod-03!postgres-replication",
            "mq-prod-01!rabbitmq-queue",
            "k8s-node-07!disk /var",
            "web-edge-02!http-tls",
            "lb-prod-02!haproxy-backend",
            "api-gw-01!http-latency",
            "db-prod-01!load",
            "cache-02!redis-memory",
            "backup-01!borg-last-run",
            "k8s-node-04!kubelet",
            "k8s-node-02!ntp-offset",
            "vpn-gw-01!cert-expiry",
        ] {
            assert!(rows.iter().any(|row| row == expected), "{expected} missing");
        }
        // Unhandled criticals first (newest first among equals), handled last.
        assert_eq!(rows[0], "mq-prod-01!rabbitmq-queue");
        assert_eq!(rows[1], "db-prod-03!postgres-replication");
        assert_eq!(
            rows.last().map(String::as_str),
            Some("cache-02!redis-memory")
        );
        let summary = production.summary;
        assert_eq!(
            summary.unhandled + summary.handled,
            u32::try_from(rows.len()).unwrap()
        );
    }

    #[test]
    fn hide_handled_drops_handled_rows_but_not_their_counts() {
        let demo = demo();
        let overview = result(&demo, "overview", "overview");
        assert_eq!(
            u32::try_from(overview.rows.len()).unwrap(),
            overview.summary.unhandled
        );
        assert!(overview.summary.handled > 0);
        assert!(!row_names(overview).contains(&"web-edge-02!http-tls".to_owned()));
    }

    #[test]
    fn databases_are_grouped_by_host_worst_first() {
        let demo = demo();
        let rows = &result(&demo, "overview", "databases").rows;
        let headers: Vec<_> = rows
            .iter()
            .filter_map(|row| match row {
                DashboardRow::Group { label, count } => Some((label.as_str(), *count)),
                DashboardRow::Object(_) => None,
            })
            .collect();
        assert_eq!(
            headers,
            [("db-prod-03", 2), ("db-prod-01", 1), ("cache-02", 1)]
        );
    }

    #[test]
    fn db_prod_03_has_the_host_panes_23_services() {
        let demo = demo();
        let host = HostName::new("db-prod-03");
        assert_eq!(demo.snapshot.services_of(&host).count(), 23);
        let problems = demo
            .snapshot
            .services_of(&host)
            .filter(|service| service.is_problem())
            .count();
        assert_eq!(problems, 2);
    }

    #[test]
    fn there_are_plenty_of_ok_services() {
        let demo = demo();
        let ok = demo
            .snapshot
            .services
            .values()
            .filter(|service| service.state == ServiceState::Ok)
            .count();
        assert!(ok >= 150, "{ok}");
    }

    #[test]
    fn the_replication_check_has_the_designs_details() {
        let demo = demo();
        let key = ServiceKey::new("db-prod-03", "postgres-replication");
        let service = &demo.snapshot.services[&key];
        let result = service.check.result.as_ref().unwrap();
        assert_eq!(result.output, "CRITICAL - standby lag 412s (> 300s)");
        assert_eq!(result.long_output.lines().count(), 3);
        assert_eq!(result.perfdata.len(), 3);
        assert_eq!(
            service.check.command_endpoint.as_deref(),
            Some("sat-ams-01")
        );
        assert_eq!((service.check.attempt, service.check.max_attempts), (3, 3));
        assert!(!service.links.notes_url.is_empty());
        let comments = &demo.snapshot.comments[&service.object_key()];
        assert_eq!(comments[0].author, "j.berg");
    }

    #[test]
    fn the_host_pane_shows_the_designs_ages_and_output() {
        let now = Timestamp::from_unix_seconds(1_790_000_000.);
        let demo = build(now);
        let host = &demo.snapshot.hosts[&HostName::new("db-prod-03")];
        assert_eq!(host.check.output(), "PING OK rta 0.42ms");
        let age = |name: &str| {
            let service = &demo.snapshot.services[&ServiceKey::new("db-prod-03", name)];
            ic_model::format_compact(service.check.last_state_change.elapsed_until(now))
        };
        assert_eq!(age("disk /"), "6d");
        assert_eq!(age("load"), "2d");
        assert_eq!(age("memory"), "9d");
        assert_eq!(age("ntp-offset"), "41d");
    }

    #[test]
    fn re_evaluating_follows_the_view() {
        let demo = demo();
        let production = reference("overview", "production");
        let mut view = demo.config.environments[0].groups[0].dashboards[1]
            .view
            .clone();
        view.sort = Sort {
            key: SortKey::Host,
            descending: false,
        };
        view.hide_handled = true;
        let result = demo
            .evaluator
            .evaluate(&demo.snapshot, &production, &view)
            .unwrap();
        let rows = row_names(&result);
        assert_eq!(rows[0], "api-gw-01!http-latency", "hosts in name order");
        assert!(
            !rows.contains(&"web-edge-02!http-tls".to_owned()),
            "handled hidden"
        );
        assert!(
            demo.evaluator
                .evaluate(&demo.snapshot, &reference("nope", "nope"), &View::default())
                .is_none()
        );
    }

    #[test]
    fn grouping_by_host_group_uses_display_names() {
        let demo = demo();
        let mut view = demo.config.environments[0].groups[0].dashboards[2]
            .view
            .clone();
        view.group_by = GroupBy::HostGroup;
        let result = demo
            .evaluator
            .evaluate(&demo.snapshot, &reference("overview", "databases"), &view)
            .unwrap();
        let headers: Vec<String> = row_names(&result)
            .into_iter()
            .filter(|row| row.starts_with('['))
            .collect();
        assert_eq!(headers, ["[Linux servers]", "[Production databases]"]);
    }

    #[test]
    fn generated_rows_get_their_own_dashboard() {
        let generated = build_with(
            Timestamp::from_unix_seconds(1_790_000_000.),
            DemoOptions {
                generated_rows: 500,
            },
        );
        assert_eq!(generated.selected, reference("lab", "load-test"));
        let load = result(&generated, "lab", "load-test");
        assert_eq!(load.rows.len(), 500);
        // The design's dashboards don't change.
        assert_eq!(
            result(&generated, "overview", "production").rows.len(),
            result(&demo(), "overview", "production").rows.len()
        );
    }
}
