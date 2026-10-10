//! Dashboards end to end: evaluated with every snapshot from the live
//! store, re-evaluated as events change objects, reconfigured by
//! `UpdateEnvironment`, previewed by `PreviewDashboard`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers fail the test loudly"
)]

use std::collections::BTreeSet;

use crate::support::{ENV_ID, FakeSecrets, PASSWORD, WAIT, environment, mock, start, tuning};
use futures::channel::oneshot;
use ic_config::{
    Dashboard, DashboardGroup, Environment, GroupBy, HandledSetting, ObjectKind, Sort, View,
};
use ic_core::Command;
use ic_core::snapshot::{DashboardRow, Snapshot, ViewResult};
use ic_mock::{MockConfig, MockControl, MockServer, scenarios};
use ic_model::{ObjectKey, Service, ServiceState};
use ic_rules::DashboardRef;

fn view(kind: ObjectKind, filter: &str, problems_only: bool, hide_handled: bool) -> View {
    View {
        id: "v".to_owned(),
        object_kind: kind,
        filter: filter.to_owned(),
        problems_only,
        handled: if hide_handled {
            HandledSetting::SETTINGS
        } else {
            HandledSetting::SHOW
        },
        sort: Sort::default(),
        group_by: GroupBy::None,
        ..View::default()
    }
}

const DATABASES: &str = "\"databases\" in service.groups";

/// Dashboards in one group `g`: `db` (unhandled database problems), `all-db`
/// (every database service, grouped by host), `pg-hosts` (postgres hosts)
/// and `broken` (a filter that doesn't parse).
fn dashboards(server: &MockServer) -> Environment {
    let mut environment = environment(server);
    let mut all_db = view(ObjectKind::Services, DATABASES, false, false);
    all_db.set_grouping(GroupBy::Host);
    let boards = [
        ("db", view(ObjectKind::Services, DATABASES, true, true)),
        ("all-db", all_db),
        (
            "pg-hosts",
            view(
                ObjectKind::Hosts,
                "host.vars.role == \"postgres\"",
                false,
                false,
            ),
        ),
        (
            "broken",
            view(ObjectKind::Services, "service.state ==", true, true),
        ),
    ];
    environment.groups = vec![DashboardGroup {
        id: "g".to_owned(),
        name: "group".to_owned(),
        dashboards: boards
            .into_iter()
            .map(|(id, view)| Dashboard {
                id: id.to_owned(),
                name: id.to_owned(),
                views: vec![view],
                ..Dashboard::default()
            })
            .collect(),
        ..DashboardGroup::default()
    }];
    environment
}

fn reference(id: &str) -> DashboardRef {
    DashboardRef {
        group_id: "g".to_owned(),
        dashboard_id: id.to_owned(),
    }
}

/// The only view of dashboard `id`.
fn board<'a>(snapshot: &'a Snapshot, id: &str) -> &'a ViewResult {
    &snapshot.dashboards[&reference(id)].views[0]
}

fn objects(result: &ViewResult) -> Vec<ObjectKey> {
    result
        .rows()
        .iter()
        .filter_map(|row| match row {
            DashboardRow::Object(object) => Some(object.clone()),
            DashboardRow::Group { .. } => None,
        })
        .collect()
}

fn is_database(service: &Service) -> bool {
    service.groups.iter().any(|group| group == "databases")
}

/// The unhandled database problems according to the mock.
fn unhandled_database_problems(control: &MockControl) -> BTreeSet<ObjectKey> {
    control
        .services()
        .into_iter()
        .filter(|service| {
            let host_problem = control
                .host(service.key.host.as_str())
                .is_some_and(|host| host.is_problem());
            is_database(service) && service.is_problem() && !service.is_handled(host_problem)
        })
        .map(|service| service.object_key())
        .collect()
}

/// Whether the latest snapshot holds the whole initial load and its
/// dashboards.
fn complete(snapshot: &Snapshot, control: &MockControl) -> bool {
    snapshot.services.len() == control.services().len() && snapshot.dashboards.len() == 4
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[expect(clippy::too_many_lines, reason = "one end-to-end scenario")]
async fn dashboards_follow_the_live_store() {
    let server = mock(MockConfig::with_scenario(scenarios::prod_cluster())).await;
    let control = server.control();
    let mut engine = start(
        dashboards(&server),
        FakeSecrets::with(ENV_ID, PASSWORD),
        tuning(),
    );
    engine.connected().await;
    let snapshot = engine
        .snapshot(|snapshot| complete(snapshot, &control))
        .await;

    // Problems only, handled hidden, by severity.
    let db = board(&snapshot, "db");
    assert_eq!(db.error, None);
    let rows = objects(db);
    let expected = unhandled_database_problems(&control);
    assert!(!expected.is_empty(), "the scenario has database problems");
    assert_eq!(rows.iter().cloned().collect::<BTreeSet<_>>(), expected);
    assert_eq!(rows.len(), expected.len(), "each once");
    let severities: Vec<u32> = rows
        .iter()
        .map(|object| snapshot.services[object.as_service().unwrap()].severity())
        .collect();
    assert!(
        severities.windows(2).all(|pair| pair[0] >= pair[1]),
        "{severities:?}"
    );

    // The summary counts every member, OK and handled ones too.
    let members: Vec<Service> = control.services().into_iter().filter(is_database).collect();
    let summary = db.summary;
    assert_eq!(summary, board(&snapshot, "all-db").summary);
    let count = |state: ServiceState| {
        u32::try_from(members.iter().filter(|s| s.state == state).count()).unwrap()
    };
    assert_eq!(summary.ok, count(ServiceState::Ok));
    assert_eq!(summary.critical, count(ServiceState::Critical));
    assert_eq!(summary.warning, count(ServiceState::Warning));
    assert_eq!(
        summary.unhandled,
        u32::try_from(expected.len()).unwrap(),
        "unhandled = the rows"
    );

    // Grouped by host: a header per host, every member under it.
    let all_db = board(&snapshot, "all-db");
    let headers = all_db
        .rows()
        .iter()
        .filter(|row| matches!(row, DashboardRow::Group { .. }))
        .count();
    let hosts: BTreeSet<&str> = members.iter().map(|s| s.key.host.as_str()).collect();
    assert_eq!(headers, hosts.len());
    assert_eq!(objects(all_db).len(), members.len());

    // A host view.
    let pg_hosts = board(&snapshot, "pg-hosts");
    let postgres: BTreeSet<ObjectKey> = control
        .hosts()
        .iter()
        .filter(|host| host.vars.get("role").and_then(|role| role.as_str()) == Some("postgres"))
        .map(ic_model::Host::key)
        .collect();
    assert!(!postgres.is_empty());
    assert_eq!(
        objects(pg_hosts).into_iter().collect::<BTreeSet<_>>(),
        postgres
    );

    // A filter that doesn't parse says where.
    let broken = board(&snapshot, "broken");
    assert!(broken.rows().is_empty());
    assert!(
        broken.error.as_deref().unwrap().contains("line 1"),
        "{:?}",
        broken.error
    );

    // Live: an OK database service turns critical...
    let victim = members
        .iter()
        .find(|service| {
            service.state == ServiceState::Ok
                && !control
                    .host(service.key.host.as_str())
                    .unwrap()
                    .is_problem()
        })
        .unwrap()
        .clone();
    let key = victim.object_key();
    control
        .set_service_state(
            victim.key.host.as_str(),
            &victim.key.name,
            ServiceState::Critical,
            "CRITICAL - injected",
            true,
        )
        .unwrap();
    let snapshot = engine
        .snapshot(|snapshot| objects(board(snapshot, "db")).contains(&key))
        .await;
    assert_eq!(
        board(&snapshot, "db").summary.critical,
        summary.critical + 1
    );
    assert_eq!(
        objects(board(&snapshot, "db"))[0],
        key,
        "the newest critical leads"
    );

    // ...is acknowledged: handled, so hidden, but still a member.
    control.acknowledge(&key, "alice", "on it", false).unwrap();
    let snapshot = engine
        .snapshot(|snapshot| !objects(board(snapshot, "db")).contains(&key))
        .await;
    let db = board(&snapshot, "db");
    assert_eq!(db.summary.handled, summary.handled + 1);
    assert!(objects(board(&snapshot, "all-db")).contains(&key));
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[expect(clippy::too_many_lines, reason = "one end-to-end scenario")]
async fn previews_and_changed_dashboards() {
    let server = mock(MockConfig::with_scenario(scenarios::prod_cluster())).await;
    let control = server.control();
    let mut environment = dashboards(&server);
    let mut engine = start(
        environment.clone(),
        FakeSecrets::with(ENV_ID, PASSWORD),
        tuning(),
    );
    engine.connected().await;
    let snapshot = engine
        .snapshot(|snapshot| complete(snapshot, &control))
        .await;

    // The editor's preview of an unsaved view.
    let preview = |view: View| {
        let (reply, answer) = oneshot::channel();
        engine.send(Command::PreviewDashboard {
            views: vec![view],
            reply,
        });
        answer
    };
    let answer = preview(view(
        ObjectKind::Services,
        "service.name == \"postgres-replication\"",
        false,
        false,
    ));
    let result = tokio::time::timeout(WAIT, answer).await.unwrap().unwrap();
    let result = &result.views[0];
    let replication: BTreeSet<ObjectKey> = control
        .services()
        .iter()
        .filter(|service| &*service.key.name == "postgres-replication")
        .map(Service::object_key)
        .collect();
    assert_eq!(
        objects(result).into_iter().collect::<BTreeSet<_>>(),
        replication
    );
    // Errors come back in the view, with the position.
    let bad = preview(view(ObjectKind::Hosts, "host.name == (", false, false));
    let bad = tokio::time::timeout(WAIT, bad).await.unwrap().unwrap();
    let error = bad.views[0].error.as_deref().unwrap();
    assert!(error.contains("line 1"), "{error}");
    // A preview replaced while another runs is dropped; the newest is
    // answered.
    let first = preview(view(ObjectKind::Services, "", false, false));
    let second = preview(view(
        ObjectKind::Services,
        "service.state == 2",
        false,
        false,
    ));
    let third = preview(view(ObjectKind::Hosts, "", false, false));
    let first = tokio::time::timeout(WAIT, first).await.unwrap();
    let third = tokio::time::timeout(WAIT, third).await.unwrap().unwrap();
    assert_eq!(third.views[0].rows().len(), control.hosts().len());
    if let Ok(first) = first {
        assert_eq!(first.views[0].rows().len(), control.services().len());
    }
    let second = tokio::time::timeout(WAIT, second).await.unwrap();
    assert!(
        second.is_err() || second.unwrap().views[0].error.is_none(),
        "dropped (Canceled) or answered"
    );

    // Changed dashboards apply in place: re-evaluated, no reconnect.
    let streams = control
        .requests()
        .iter()
        .filter(|request| request.path == "/v1/events")
        .count();
    let revision = snapshot.revision;
    environment.groups[0].dashboards[0].views[0].filter =
        "\"replication\" in service.groups".to_owned();
    environment.groups[0].dashboards[0].views[0].problems_only = false;
    environment.groups[0]
        .dashboards
        .retain(|dashboard| dashboard.id != "broken");
    engine.send(Command::UpdateEnvironment(environment));
    let snapshot = engine
        .snapshot(|snapshot| snapshot.revision > revision && snapshot.dashboards.len() == 3)
        .await;
    let replication_group: BTreeSet<ObjectKey> = control
        .services()
        .iter()
        .filter(|service| service.groups.iter().any(|group| group == "replication"))
        .filter(|service| {
            let host_problem = control
                .host(service.key.host.as_str())
                .is_some_and(|host| host.is_problem());
            !service.is_handled(host_problem)
        })
        .map(Service::object_key)
        .collect();
    assert_eq!(
        objects(board(&snapshot, "db"))
            .into_iter()
            .collect::<BTreeSet<_>>(),
        replication_group
    );
    assert!(!snapshot.dashboards.contains_key(&reference("broken")));
    assert_eq!(
        control
            .requests()
            .iter()
            .filter(|request| request.path == "/v1/events")
            .count(),
        streams,
        "no reconnect"
    );
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[expect(clippy::too_many_lines, reason = "one end-to-end scenario")]
async fn multi_view_dashboards_and_event_streams_follow_the_store() {
    use ic_config::{HideHandled, ViewDisplay};
    use ic_core::snapshot::ViewBody;

    let server = mock(MockConfig::with_scenario(scenarios::prod_cluster())).await;
    let control = server.control();
    let mut environment = environment(&server);
    let grid = View {
        id: "grid".to_owned(),
        display: ViewDisplay::HostGroupGrid,
        object_kind: ObjectKind::Hosts,
        ..View::default()
    };
    let db = View {
        id: "db".to_owned(),
        ..view(ObjectKind::Services, DATABASES, true, true)
    };
    let stream = View {
        id: "events".to_owned(),
        display: ViewDisplay::EventStream,
        filter: DATABASES.to_owned(),
        ..View::default()
    };
    environment.groups = vec![DashboardGroup {
        id: "g".to_owned(),
        name: "group".to_owned(),
        dashboards: vec![Dashboard {
            id: "m".to_owned(),
            name: "databases".to_owned(),
            views: vec![grid, db, stream],
            ..Dashboard::default()
        }],
        ..DashboardGroup::default()
    }];
    let mut engine = start(environment, FakeSecrets::with(ENV_ID, PASSWORD), tuning());
    engine.connected().await;
    let snapshot = engine
        .snapshot(|snapshot| {
            snapshot.services.len() == control.services().len() && snapshot.dashboards.len() == 1
        })
        .await;
    let result = &snapshot.dashboards[&reference("m")];
    let ids: Vec<&str> = result.views.iter().map(|view| view.id.as_str()).collect();
    assert_eq!(ids, ["grid", "db", "events"]);
    let ViewBody::Grid(grid) = &result.view("grid").unwrap().body else {
        panic!("a grid");
    };
    let grouped = control
        .hosts()
        .iter()
        .filter(|host| !host.groups.is_empty())
        .count();
    assert_eq!(grid.hosts, u32::try_from(grouped).unwrap());
    // The sidebar counts the grid's hosts and services and the list's
    // services, each once: at least what the list counts.
    let db = result.view("db").unwrap();
    assert!(result.summary.unhandled >= db.counts.unhandled);
    assert_eq!(db.counts.unhandled, db.summary.unhandled);

    // A database service fails hard: the stream shows it, the grid's
    // square turns critical.
    let victim = control
        .services()
        .into_iter()
        .filter(is_database)
        .find(|service| {
            service.state == ServiceState::Ok
                && !control
                    .host(service.key.host.as_str())
                    .unwrap()
                    .is_problem()
        })
        .unwrap();
    let key = victim.object_key();
    control
        .set_service_state(
            victim.key.host.as_str(),
            &victim.key.name,
            ServiceState::Critical,
            "CRITICAL - injected",
            true,
        )
        .unwrap();
    let streamed = |snapshot: &Snapshot| {
        snapshot
            .dashboards
            .get(&reference("m"))
            .is_some_and(|result| {
                matches!(&result.view("events").unwrap().body, ViewBody::Stream(events)
                if events.iter().any(|entry| entry.object == key))
            })
    };
    let snapshot = engine.snapshot(streamed).await;
    let result = &snapshot.dashboards[&reference("m")];
    let ViewBody::Grid(grid) = &result.view("grid").unwrap().body else {
        panic!("a grid");
    };
    let host = victim.key.host.clone();
    assert!(
        grid.groups
            .iter()
            .flat_map(|group| &group.cells)
            .filter(|cell| cell.host == host)
            .all(|cell| cell.state.is_problem())
    );
    assert!(objects(result.view("db").unwrap()).contains(&key));

    // The settings show handled problems now: the list follows them.
    let handled = result.view("db").unwrap().hidden;
    let rows = result.view("db").unwrap().rows().len();
    engine.send(Command::SetHandledDefaults(HideHandled::NONE));
    let snapshot = engine
        .snapshot(|snapshot| {
            snapshot
                .dashboards
                .get(&reference("m"))
                .is_some_and(|result| result.view("db").unwrap().hidden == 0)
        })
        .await;
    let db = snapshot.dashboards[&reference("m")].view("db").unwrap();
    assert_eq!(db.rows().len(), rows + usize::try_from(handled).unwrap());
    engine.shutdown();
}
