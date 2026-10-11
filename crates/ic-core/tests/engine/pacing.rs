//! Adaptive pacing (PLAN.md §4.2 1b, docs/performance.md *Adaptive
//! pacing*) end to end against `ic-mock`: the services in by-name batches
//! (the keys of the last run kept in the event log, the problems first),
//! the gate before a paced load that waits while the master answers
//! slowly and lets the load through within its cap, what the user does
//! never waiting behind it, and the status poll's object counts as the
//! reconcile's safety net. Answer times come from the mock's latency
//! (far above or far below the thresholds), the timers' jitter is seeded:
//! no outcome depends on how fast the machine is.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers fail the test loudly"
)]

use std::time::Duration;

use ic_core::{Command, ConnectionState, Start};
use ic_mock::{MockConfig, MockControl, RecordedRequest, scenarios};
use ic_model::ObjectKey;

use crate::support::{Engine, Launch, mock, wait_until};

/// Services in the scenario: 60 hosts × 15.
const HOSTS: usize = 60;

fn launch(server: &ic_mock::MockServer) -> Launch {
    let mut launch = Launch::new(server);
    // Every size is paced; thresholds the mock's latency is far from.
    launch.tuning.pace_from = 0;
    launch.tuning.pace_floor = Duration::from_millis(100);
    launch.tuning.pace_unknown = Duration::from_millis(300);
    launch.tuning.pace_probe_jitter = Duration::from_millis(20);
    launch.tuning.pace_wait_initial = Duration::from_millis(200);
    launch.tuning.pace_wait_max = Duration::from_millis(1_500);
    launch.tuning.status_interval = Duration::from_millis(200);
    launch
}

/// The services asked for by name, lean (a paced load's batches).
fn batches(control: &MockControl) -> Vec<Vec<String>> {
    by_name(control, true)
}

/// The services asked for by name (`lean`: only lean requests, not the
/// problems' details or an opened object).
fn by_name(control: &MockControl, lean: bool) -> Vec<Vec<String>> {
    control
        .requests()
        .iter()
        .filter(|request| request.path == "/v1/objects/services")
        .filter(|request| {
            !lean
                || request.body.as_ref().is_some_and(|body| {
                    body.get("attrs")
                        .and_then(|attrs| attrs.as_array())
                        .is_some_and(|attrs| {
                            !attrs
                                .iter()
                                .any(|attr| attr.as_str() == Some("last_check_result"))
                        })
                })
        })
        .filter_map(|request| {
            let names = request.body.as_ref()?.get("services")?.as_array()?;
            Some(
                names
                    .iter()
                    .filter_map(|name| name.as_str().map(str::to_owned))
                    .collect(),
            )
        })
        .collect()
}

/// Whether `request` asks for every service with `attrs` exactly.
fn list_with(request: &RecordedRequest, attrs: &[&str]) -> bool {
    request.path == "/v1/objects/services"
        && request.body.as_ref().is_some_and(|body| {
            body.get("services").is_none()
                && body
                    .get("attrs")
                    .and_then(|attrs| attrs.as_array())
                    .is_some_and(|asked| {
                        asked.len() == attrs.len()
                            && asked
                                .iter()
                                .zip(attrs)
                                .all(|(asked, attr)| asked.as_str() == Some(attr))
                    })
        })
}

/// Lists of every service's name (a paced load without keys).
fn name_lists(control: &MockControl) -> usize {
    control
        .requests()
        .iter()
        .filter(|request| list_with(request, &["name"]))
        .count()
}

/// Lists of every service, lean (an unpaced load).
fn lean_lists(control: &MockControl) -> usize {
    control
        .requests()
        .iter()
        .filter(|request| {
            request.path == "/v1/objects/services"
                && request.body.as_ref().is_some_and(|body| {
                    body.get("services").is_none()
                        && body
                            .get("attrs")
                            .and_then(|attrs| attrs.as_array())
                            .is_some_and(|attrs| attrs.len() > 1)
                })
        })
        .count()
}

/// The gate's probes (the connected node's endpoint by name).
fn probes(control: &MockControl) -> usize {
    control
        .requests()
        .iter()
        .filter(|request| {
            request.path == "/v1/objects/endpoints"
                && request
                    .body
                    .as_ref()
                    .is_some_and(|body| body.get("endpoints").is_some())
                && request.body.as_ref().is_some_and(|body| {
                    body.get("attrs")
                        .and_then(|attrs| attrs.as_array())
                        .is_some_and(|attrs| attrs.len() == 1)
                })
        })
        .count()
}

async fn connected_with_all(engine: &mut Engine, control: &MockControl) {
    engine.connected().await;
    let services = control.services().len();
    engine
        .snapshot(|snapshot| snapshot.services.len() == services)
        .await;
}

/// The window's first load without a key list: every service's name, then
/// the services by name in batches of at most 200, never one answer of
/// every service; the next run asks for the keys it kept at once, the
/// problems in the first batch.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_paced_load_asks_for_the_services_in_batches_and_keeps_their_keys() {
    let server = mock(MockConfig::with_scenario(scenarios::large_with_hosts(
        HOSTS, 7,
    )))
    .await;
    let control = server.control();
    let dir = tempfile::tempdir().unwrap();
    let mut first = launch(&server);
    first.data_dir = Some(dir.path().to_owned());
    let mut engine = first.start();
    connected_with_all(&mut engine, &control).await;
    let services = control.services().len();
    assert!(services > 600, "{services} services: several batches");
    assert_eq!(name_lists(&control), 1, "the names, once");
    assert_eq!(lean_lists(&control), 0, "never every service in one answer");
    let asked = batches(&control);
    assert!(asked.iter().all(|batch| batch.len() <= 200));
    let total: usize = asked.iter().map(Vec::len).sum();
    assert_eq!(total, services, "each service asked for once");
    engine.shutdown();

    // The next run: the kept keys, by name at once, the problems first.
    control.clear_requests();
    let problems: Vec<String> = control
        .services()
        .iter()
        .filter(|service| service.state.is_problem())
        .map(|service| format!("{}!{}", service.key.host, service.key.name))
        .collect();
    assert!(!problems.is_empty());
    let mut second = launch(&server);
    second.data_dir = Some(dir.path().to_owned());
    let mut engine = second.start();
    connected_with_all(&mut engine, &control).await;
    assert_eq!(name_lists(&control), 0, "the keys were kept");
    assert_eq!(lean_lists(&control), 0);
    let asked = batches(&control);
    let leading: Vec<&String> = asked.iter().flatten().take(problems.len()).collect();
    assert!(
        problems.iter().all(|problem| leading.contains(&problem)),
        "the problems come first"
    );
    engine.shutdown();
}

/// A restart's reload waits at its gate while the master answers slowly,
/// probing again after random, growing waits, and still runs within its
/// cap; the object the user opens meanwhile is fetched at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restart_reload_waits_at_its_gate_while_the_master_is_slow() {
    let server = mock(MockConfig::with_scenario(scenarios::large_with_hosts(
        HOSTS, 7,
    )))
    .await;
    let control = server.control();
    let mut engine = launch(&server).start();
    connected_with_all(&mut engine, &control).await;
    // The status polls learn their baseline.
    assert!(
        wait_until(|| {
            control
                .requests()
                .iter()
                .filter(|request| request.path == "/v1/status/IcingaApplication")
                .count()
                >= 4
        })
        .await
    );
    control.clear_requests();
    // The master turns slow (every answer 400 ms), then restarts.
    control.set_latency(Duration::from_millis(400));
    control.set_program_start(control.now());
    // The gate probes, slow, again and again …
    assert!(wait_until(|| probes(&control) >= 2).await, "probed again");
    assert_eq!(batches(&control).len(), 0, "the reload waits");
    // … while the object the user opens is asked for at once.
    let object = control
        .services()
        .iter()
        .find(|service| !service.state.is_problem())
        .map(|service| ObjectKey::Service {
            key: service.key.clone(),
        })
        .unwrap();
    let ObjectKey::Service { key } = &object else {
        unreachable!()
    };
    let name = format!("{}!{}", key.host, key.name);
    engine.send(Command::Focus(object.clone()));
    assert!(
        wait_until(|| by_name(&control, false)
            .iter()
            .any(|batch| batch == std::slice::from_ref(&name)))
        .await,
        "the opened object isn't paced"
    );
    // The cap lets the reload through: its batches go out.
    assert!(
        wait_until(|| batches(&control)
            .iter()
            .filter(|batch| batch.len() > 1)
            .count()
            >= 3)
        .await,
        "the reload ran within its cap"
    );
    let reload_probes = probes(&control);
    assert!(
        (2..=12).contains(&reload_probes),
        "{reload_probes} probes for a cap of 1.5 s"
    );
    // It never touched the connection state.
    assert!(matches!(
        engine.states().last(),
        Some(ConnectionState::Connected { .. })
    ));
    engine.shutdown();
}

/// With the master answering as fast as ever, a restart's reload goes
/// after one probe.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fast_master_lets_a_paced_reload_through_after_one_probe() {
    let server = mock(MockConfig::with_scenario(scenarios::large_with_hosts(
        HOSTS, 7,
    )))
    .await;
    let control = server.control();
    let mut engine = launch(&server).start();
    connected_with_all(&mut engine, &control).await;
    control.clear_requests();
    control.set_program_start(control.now());
    assert!(
        wait_until(|| batches(&control).len() >= 3).await,
        "the reload's batches"
    );
    assert_eq!(probes(&control), 1, "one probe, fast");
    assert_eq!(lean_lists(&control), 0);
    engine.shutdown();
}

/// A background start's first load waits at its gate too; the window
/// shown (`StartNow`) ends the wait at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn opening_the_window_ends_a_background_start_waiting_at_its_gate() {
    let server = mock(MockConfig::with_scenario(scenarios::large_with_hosts(
        HOSTS, 7,
    )))
    .await;
    let control = server.control();
    control.set_latency(Duration::from_millis(400));
    let mut launch = launch(&server);
    launch.start = Start::Background;
    launch.tuning.start_delay_max = Duration::ZERO;
    // Long enough that only `StartNow` can end it within the test.
    launch.tuning.pace_wait_initial = Duration::from_mins(1);
    launch.tuning.pace_wait_max = Duration::from_mins(10);
    let mut engine = launch.start();
    assert!(
        wait_until(|| probes(&control) >= 1).await,
        "the gate probes"
    );
    // Slow, no baseline: over `pace_unknown`, it waits.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(batches(&control).len(), 0);
    control.set_latency(Duration::ZERO);
    engine.send(Command::StartNow);
    connected_with_all(&mut engine, &control).await;
    engine.shutdown();
}

/// The status poll's object counts against the store's: a service that
/// went without an event (the stream held back) is found once a second
/// poll confirms the difference, and only the services are reloaded.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_count_mismatch_reloads_what_differs() {
    let server = mock(MockConfig::with_scenario(scenarios::large_with_hosts(
        HOSTS, 7,
    )))
    .await;
    let control = server.control();
    let mut launch = launch(&server);
    // The production rule: the count check stands in for the timer.
    launch.tuning.reconcile_interval = None;
    let mut engine = launch.start();
    connected_with_all(&mut engine, &control).await;
    let services = control.services().len();
    let gone = control.services().last().unwrap().key.clone();
    // Its `ObjectDeleted` waits in the mock.
    control.hold_event_streams();
    control
        .remove_service(gone.host.as_str(), &gone.name)
        .unwrap();
    control.clear_requests();
    engine
        .snapshot(|snapshot| {
            snapshot.services.len() == services - 1 && !snapshot.services.contains_key(&gone)
        })
        .await;
    // One reload, its services in batches.
    assert!(!batches(&control).is_empty());
    let host_lists = control
        .requests()
        .iter()
        .filter(|request| {
            request.path == "/v1/objects/hosts"
                && request
                    .body
                    .as_ref()
                    .is_some_and(|body| body.get("hosts").is_none())
        })
        .count();
    assert_eq!(host_lists, 1, "one reload");
    control.release_event_streams();
    engine.shutdown();
}
