//! Keeping in sync beyond the stream: the freshness watchdog (overdue
//! objects re-queried by name, still-overdue ones late), hydration on
//! demand, the jittered reload after a reconnect, and the periodic lean
//! reconcile.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers fail the test loudly"
)]

use std::collections::BTreeSet;
use std::time::Duration;

use crate::support::{ENV_ID, FakeSecrets, PASSWORD, environment, mock, start, start_for, tuning};
use ic_core::{Command, ConnectionState, CoreEvent, Tuning};
use ic_mock::{MockConfig, MockControl, scenarios};
use ic_model::{ObjectKey, ServiceKey, ServiceState};
use serde_json::Value;

/// An object query by name: its path, the names and whether it asked for
/// the full details (`last_check_result`).
#[derive(Debug)]
struct NameQuery {
    path: String,
    names: Vec<String>,
    full: bool,
}

fn name_queries(control: &MockControl) -> Vec<NameQuery> {
    control
        .requests()
        .iter()
        .filter_map(|request| {
            let body = request.body.as_ref()?;
            let names = ["hosts", "services"]
                .into_iter()
                .find_map(|kind| body.get(kind)?.as_array())?;
            let full = body
                .get("attrs")
                .and_then(Value::as_array)
                .is_some_and(|attrs| attrs.iter().any(|a| a == "last_check_result"));
            Some(NameQuery {
                path: request.path.clone(),
                names: names
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect(),
                full,
            })
        })
        .collect()
}

/// How often `name` was queried by name.
fn queried(control: &MockControl, name: &str) -> usize {
    name_queries(control)
        .iter()
        .filter(|query| query.names.iter().any(|n| n == name))
        .count()
}

/// Full reloads of the service list (no name list).
fn service_loads(control: &MockControl) -> Vec<bool> {
    control
        .requests()
        .iter()
        .filter(|request| request.path == "/v1/objects/services")
        .filter_map(|request| {
            let body = request.body.as_ref()?;
            if body.get("services").is_some() {
                return None;
            }
            Some(
                body.get("attrs")
                    .and_then(Value::as_array)
                    .is_some_and(|attrs| attrs.iter().any(|a| a == "last_check_result")),
            )
        })
        .collect()
}

/// Polls `condition` for up to ten seconds.
async fn wait_until(mut condition: impl FnMut() -> bool) -> bool {
    for _ in 0..1_000 {
        if condition() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    false
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn overdue_checks_are_requeried_and_late_ones_flagged() {
    // The lab: lab-01 and its four services are checked actively (every
    // 1 or 5 minutes); lab-02 and its service are passive and were never
    // checked. The simulator is off, so nothing checks unless told to.
    let server = mock(MockConfig::with_scenario(scenarios::lab())).await;
    let control = server.control();
    let mut engine = start_for(&server);
    engine.connected().await;
    let snapshot = engine
        .snapshot(|snapshot| snapshot.services.len() == 5)
        .await;
    assert!(snapshot.late.is_empty(), "everything is fresh");
    control.clear_requests();

    // Twenty minutes pass on Icinga without a check; the next event tells
    // the client what time it is.
    control.advance_clock(Duration::from_mins(20));
    let ssh = ObjectKey::service("lab-01", "ssh");
    control
        .process_check_result(&ssh, 0, "SSH OK - fresh", &[])
        .unwrap();
    let overdue: BTreeSet<ObjectKey> = [
        ObjectKey::host("lab-01"),
        ObjectKey::service("lab-01", "ping4"),
        ObjectKey::service("lab-01", "disk /"),
        ObjectKey::service("lab-01", "load"),
    ]
    .into();
    let snapshot = engine
        .snapshot(|snapshot| snapshot.late.keys().cloned().collect::<BTreeSet<_>>() == overdue)
        .await;
    assert!(!snapshot.is_late(&ssh), "just checked");
    assert!(
        !snapshot.is_late(&ObjectKey::host("lab-02")),
        "never checked and passive: nothing expected"
    );
    // The deadline it missed is Icinga's next_update.
    let truth = control.service("lab-01", "load").unwrap();
    let deadline = snapshot.late[&ObjectKey::service("lab-01", "load")];
    let expected = truth.check.next_check.unwrap().as_unix_seconds() + truth.check.check_interval;
    assert!(
        (deadline.as_unix_seconds() - expected).abs() < 1.0,
        "{deadline:?} vs {expected}"
    );

    // Each was asked about by name, once, in requests of at most 200.
    let queries = name_queries(&control);
    assert!(queries.iter().all(|query| query.names.len() <= 200));
    for object in &overdue {
        assert_eq!(queried(&control, &object.full_name()), 1, "{object}");
    }
    assert_eq!(queried(&control, "lab-01!ssh"), 0);
    let host_query = queries
        .iter()
        .find(|query| query.names == ["lab-01"])
        .unwrap();
    assert!(host_query.full, "hosts are queried in full");
    assert_eq!(host_query.path, "/v1/objects/hosts");
    let lean = queries
        .iter()
        .find(|query| query.names.contains(&"lab-01!load".to_owned()))
        .unwrap();
    assert!(!lean.full, "services known lean stay lean");

    // Not again within the interval, however often the watchdog looks.
    let count = control.requests().len();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        name_queries(&control).len(),
        queries.len(),
        "{:?}",
        &control.requests()[count..]
    );

    // A check result clears the flag at once.
    let load = ObjectKey::service("lab-01", "load");
    control
        .process_check_result(&load, 0, "OK - load average 0.10", &[])
        .unwrap();
    engine
        .snapshot(|snapshot| !snapshot.is_late(&load) && snapshot.late.len() == 3)
        .await;

    // Much later: the late ones are asked about again.
    control.advance_clock(Duration::from_hours(1));
    control
        .process_check_result(&ssh, 0, "SSH OK - fresh", &[])
        .unwrap();
    assert!(
        wait_until(|| queried(&control, "lab-01!ping4") == 2).await,
        "re-queried after its spacing"
    );
    assert!(
        wait_until(|| queried(&control, "lab-01!load") == 2).await,
        "overdue again an hour after its last check"
    );
    assert_eq!(queried(&control, "lab-01!ssh"), 0, "just checked again");
    let snapshot = engine
        .snapshot(|snapshot| {
            snapshot.late.len() == 4 && snapshot.is_late(&ObjectKey::service("lab-01", "ping4"))
        })
        .await;
    assert!(snapshot.is_late(&load), "an hour without a check");
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[expect(clippy::too_many_lines, reason = "one end-to-end scenario")]
async fn hydration_fetches_lean_services_in_full_once() {
    let server = mock(MockConfig::with_scenario(scenarios::prod_cluster())).await;
    let control = server.control();
    let mut engine = start_for(&server);
    engine.connected().await;
    let snapshot = engine
        .snapshot(|snapshot| snapshot.services.len() == control.services().len())
        .await;

    // Three OK services, known lean: no output, no links yet.
    let lean: Vec<ServiceKey> = control
        .services()
        .iter()
        .filter(|service| service.state == ServiceState::Ok)
        .filter(|service| !service.links.notes_url.is_empty() || service.vars.contains_key("role"))
        .take(3)
        .map(|service| service.key.clone())
        .collect();
    assert_eq!(lean.len(), 3);
    for key in &lean {
        assert!(snapshot.services[key].check.result.is_none(), "{key}");
    }
    control.clear_requests();
    let mut keys: Vec<ObjectKey> = lean.iter().cloned().map(ObjectKey::from).collect();
    keys.push(keys[0].clone());
    keys.push(ObjectKey::host("db-prod-03"));
    keys.push(ObjectKey::service("nowhere", "nothing"));
    engine.send(Command::Hydrate(keys.clone()));
    let snapshot = engine
        .snapshot(|snapshot| {
            lean.iter()
                .all(|key| snapshot.services[key].check.result.is_some())
        })
        .await;
    for key in &lean {
        let truth = control.service(key.host.as_str(), &key.name).unwrap();
        let stored = &snapshot.services[key];
        assert_eq!(stored.check.output(), truth.check.output());
        assert_eq!(stored.links, truth.links);
    }
    let queries = name_queries(&control);
    assert_eq!(queries.len(), 1, "{queries:?}");
    assert!(queries[0].full);
    let mut names = queries[0].names.clone();
    names.sort();
    let mut expected: Vec<String> = lean.iter().map(ServiceKey::full_name).collect();
    expected.sort();
    assert_eq!(
        names, expected,
        "deduplicated; hosts and unknown names left out"
    );

    // Asked again: already full, nothing to do.
    engine.send(Command::Hydrate(keys));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(name_queries(&control).len(), 1);

    // A lean reload doesn't take the details away again.
    let revision = engine.latest().unwrap().revision;
    let loads = service_loads(&control).len();
    engine.send(Command::Refresh);
    assert!(wait_until(|| service_loads(&control).len() > loads).await);
    assert_eq!(service_loads(&control), [false], "reloads are lean");
    let snapshot = engine
        .snapshot(|snapshot| snapshot.revision > revision + 1)
        .await;
    for key in &lean {
        let truth = control.service(key.host.as_str(), &key.name).unwrap();
        assert_eq!(snapshot.services[key].check.output(), truth.check.output());
        assert_eq!(snapshot.services[key].links, truth.links);
    }
    engine.shutdown();

    // Many at once: requests of at most 200 names.
    let server = mock(MockConfig::with_scenario(scenarios::large_with_hosts(
        40, 3,
    )))
    .await;
    let control = server.control();
    let mut engine = start_for(&server);
    engine.connected().await;
    engine
        .snapshot(|snapshot| snapshot.services.len() == 600)
        .await;
    control.clear_requests();
    let wanted: Vec<ObjectKey> = control
        .services()
        .iter()
        .filter(|service| !service.is_problem())
        .take(450)
        .map(ic_model::Service::object_key)
        .collect();
    assert_eq!(wanted.len(), 450);
    engine.send(Command::Hydrate(wanted.clone()));
    let snapshot = engine
        .snapshot(|snapshot| {
            wanted.iter().all(|key| {
                snapshot.services[key.as_service().unwrap()]
                    .check
                    .result
                    .is_some()
            })
        })
        .await;
    drop(snapshot);
    let queries = name_queries(&control);
    assert!(
        queries
            .iter()
            .all(|query| query.full && query.names.len() <= 200)
    );
    let asked: BTreeSet<String> = queries.iter().flat_map(|q| q.names.clone()).collect();
    assert_eq!(asked.len(), 450);
    assert_eq!(
        queries.iter().map(|q| q.names.len()).sum::<usize>(),
        450,
        "each once"
    );
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reconnect_goes_live_at_once_and_reconciles_what_it_missed() {
    let server = mock(MockConfig::with_scenario(scenarios::prod_cluster())).await;
    let control = server.control();
    let mut engine = start_for(&server);
    engine.connected().await;
    let replication = ServiceKey::new("db-prod-03", "postgres-replication");
    let snapshot = engine
        .snapshot(|snapshot| {
            snapshot
                .services
                .get(&replication)
                .is_some_and(|service| service.check.result.is_some())
        })
        .await;
    let before = &snapshot.services[&replication];
    assert!(before.is_problem(), "loaded in full as a problem");

    // Behind the client's back while the stream is down: the problem
    // recovers.
    let seen = engine.seen.len();
    control.drop_event_streams();
    control
        .set_service_state(
            "db-prod-03",
            "postgres-replication",
            ServiceState::Ok,
            "OK - replication lag 0s",
            true,
        )
        .unwrap();
    let snapshot = engine
        .snapshot(|snapshot| {
            snapshot
                .services
                .get(&replication)
                .is_some_and(|service| service.check.output() == "OK - replication lag 0s")
        })
        .await;
    assert_eq!(snapshot.services[&replication].state, ServiceState::Ok);

    // Live at once (no Loading), then the reload found the recovery, and
    // the stale critical output was replaced by fetching it in full.
    let states: Vec<ConnectionState> = engine.seen[seen..]
        .iter()
        .filter_map(|event| match event {
            CoreEvent::Connection(state) => Some(state.clone()),
            _ => None,
        })
        .collect();
    assert!(
        states
            .iter()
            .all(|state| !matches!(state, ConnectionState::Loading { .. })),
        "{states:?}"
    );
    assert!(matches!(
        states.last(),
        Some(ConnectionState::Connected { .. })
    ));
    assert_eq!(service_loads(&control).len(), 2, "one reload");
    assert!(
        name_queries(&control)
            .iter()
            .any(|query| query.full && query.names == ["db-prod-03!postgres-replication"]),
        "the recovered service's output was fetched"
    );
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn periodic_reconciles_are_lean_and_keep_the_stream() {
    let server = mock(MockConfig::with_scenario(scenarios::staging())).await;
    let control = server.control();
    let mut engine = start(
        environment(&server),
        FakeSecrets::with(ENV_ID, PASSWORD),
        Tuning {
            reconcile_interval: Some(Duration::from_millis(150)),
            ..tuning()
        },
    );
    engine.connected().await;
    // The first load fetched every problem's details.
    let problems = engine
        .latest()
        .unwrap()
        .services
        .values()
        .filter(|service| service.is_problem())
        .count();
    assert!(problems > 0);
    control.clear_requests();
    assert!(
        wait_until(|| service_loads(&control).len() >= 3).await,
        "reconciled periodically"
    );
    assert!(
        service_loads(&control).iter().all(|full| !full),
        "never a full-attribute reload"
    );
    let details: Vec<NameQuery> = name_queries(&control)
        .into_iter()
        .filter(|query| query.full)
        .collect();
    assert!(
        details.is_empty(),
        "problems held in full with a current result aren't fetched again: {details:?}"
    );
    assert!(
        !control
            .requests()
            .iter()
            .any(|request| request.path == "/v1/events"),
        "the stream stays"
    );
    assert_eq!(control.event_streams(), 1);
    let after_connected: Vec<ConnectionState> = engine
        .states()
        .into_iter()
        .skip_while(|state| !matches!(state, ConnectionState::Connected { .. }))
        .skip(1)
        .collect();
    assert!(after_connected.is_empty(), "{after_connected:?}");
    engine.shutdown();
}
