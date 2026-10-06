//! Quiet mode (PERF-09) end to end against `ic-mock`: the stream without
//! check results, notifications as prompt as ever, switching back and
//! forth without losing or repeating an event, the quiet schedules, the
//! object the user opens ahead of every queue and the request budget, the
//! prefetch on notification, the refresh after waking up, a quiet stream
//! that stalls, background starts, and the dashboards quiet mode leaves
//! alone.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers fail the test loudly"
)]

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use ic_core::{Command, ConnectionState, CoreEvent, LoadPhase, LogKind, Start, Tuning};
use ic_mock::{MockConfig, MockControl, scenarios};
use ic_model::{ObjectKey, ServiceState, Timestamp};
use ic_rules::{DashboardRef, ScopeSetting};
use serde_json::Value;

use crate::support::{Engine, Launch, WAIT, mock, tuning, wait_until};

/// Waits until the engine is live and Icinga's notifications are loaded.
async fn settled(engine: &mut Engine) {
    engine.connected().await;
    engine
        .snapshot(|snapshot| !snapshot.icinga_notifications.is_empty())
        .await;
}

/// Whether the mock has exactly one event stream, with check results
/// unless `quiet`.
fn one_stream(control: &MockControl, quiet: bool) -> bool {
    let streams = control.event_stream_stats();
    streams.len() == 1 && streams[0].types.contains(&"CheckResult") != quiet
}

/// Waits for [`one_stream`].
async fn stream_mode(control: &MockControl, quiet: bool) {
    assert!(
        wait_until(|| one_stream(control, quiet)).await,
        "one {} stream: {:?}",
        if quiet { "quiet" } else { "live" },
        control.event_stream_stats()
    );
}

/// The by-name queries of `kind` (`hosts`, `services`) the mock received:
/// when, and the names.
fn by_name(control: &MockControl, kind: &str) -> Vec<(Timestamp, Vec<String>)> {
    control
        .requests()
        .iter()
        .filter(|request| request.path == format!("/v1/objects/{kind}"))
        .filter_map(|request| {
            let names = request.body.as_ref()?.get(kind)?.as_array()?;
            Some((
                request.at,
                names
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect(),
            ))
        })
        .collect()
}

/// How many requests went to `path`.
fn requests_to(control: &MockControl, path: &str) -> usize {
    control
        .requests()
        .iter()
        .filter(|request| request.path == path)
        .count()
}

/// How many list queries of `kind` (not by name: a load or reconcile) the
/// mock received.
fn lists(control: &MockControl, kind: &str) -> usize {
    control
        .requests()
        .iter()
        .filter(|request| request.path == format!("/v1/objects/{kind}"))
        .filter(|request| {
            request
                .body
                .as_ref()
                .is_none_or(|body| body.get(kind).is_none())
        })
        .count()
}

fn set(control: &MockControl, host: &str, service: &str, state: ServiceState, output: &str) {
    control
        .set_service_state(host, service, state, output, true)
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quiet_mode_drops_check_results_but_never_delays_a_notification() {
    let server = mock(MockConfig::with_scenario(scenarios::staging())).await;
    let control = server.control();
    let mut engine = Launch::new(&server).start();
    settled(&mut engine).await;
    stream_mode(&control, false).await;
    assert!(!engine.latest().unwrap().quiet);

    engine.send(Command::SetQuiet(true));
    stream_mode(&control, true).await;
    engine.snapshot(|snapshot| snapshot.quiet).await;
    let quiet = control.event_stream_stats()[0].clone();
    assert!(quiet.types.contains(&"StateChange"));
    assert!(quiet.types.contains(&"Notification"));
    assert!(quiet.types.contains(&"AcknowledgementSet"));
    assert_eq!(quiet.types.len(), 14, "everything but check results");

    // A check result without a state change doesn't come.
    let web = ObjectKey::service("stg-web-01", "http");
    control
        .process_check_result(&web, 0, "HTTP OK - quiet", &[])
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(control.event_stream_stats()[0].lines, quiet.lines);

    // A state change comes at once, with its check result, and notifies.
    let started = Instant::now();
    set(
        &control,
        "stg-api-01",
        "http",
        ServiceState::Critical,
        "CRITICAL - http is broken",
    );
    let record = engine.notification().await;
    assert_eq!(record.intent.title, "CRITICAL · http on stg-api-01");
    assert!(started.elapsed() < Duration::from_secs(2));
    let api = ObjectKey::service("stg-api-01", "http");
    let snapshot = engine
        .snapshot(|snapshot| {
            snapshot.services[api.as_service().unwrap()].state == ServiceState::Critical
        })
        .await;
    let result = snapshot.services[api.as_service().unwrap()]
        .check
        .result
        .clone()
        .unwrap();
    assert_eq!(result.output, "CRITICAL - http is broken");

    // Live again: check results come again.
    engine.send(Command::SetQuiet(false));
    stream_mode(&control, false).await;
    engine.snapshot(|snapshot| !snapshot.quiet).await;
    control
        .process_check_result(&web, 0, "HTTP OK - live", &[])
        .unwrap();
    engine
        .snapshot(|snapshot| {
            snapshot.services[web.as_service().unwrap()]
                .check
                .result
                .as_ref()
                .is_some_and(|result| result.output == "HTTP OK - live")
        })
        .await;
    engine.shutdown();
}

/// The services of hosts `host-0000`… (the `large` scenario) that are ok.
fn ok_services(control: &MockControl, count: usize, skip: usize) -> Vec<(String, String)> {
    control
        .services()
        .iter()
        .filter(|service| service.state == ServiceState::Ok)
        .skip(skip)
        .take(count)
        .map(|service| (service.key.host.to_string(), service.key.name.to_string()))
        .collect()
}

/// The state entries of the event log for `object`.
async fn state_entries(engine: &Engine, object: &ObjectKey) -> usize {
    engine
        .history(Some(object.clone()))
        .await
        .iter()
        .filter(|entry| matches!(entry.kind, LogKind::State { .. }))
        .count()
}

/// Switches the mode while events arrive on both streams (the burst lands
/// in the overlap) and checks nothing was lost or applied twice: every
/// state change is in the store and logged once, and a removed downtime
/// counts once (a repeated `DowntimeRemoved` would take the depth below
/// Icinga's).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[expect(clippy::too_many_lines, reason = "one scenario, both directions")]
async fn switching_modes_loses_and_repeats_no_event() {
    let server = mock(MockConfig {
        event_buffer: 10_000,
        ..MockConfig::with_scenario(scenarios::large_with_hosts(20, 3))
    })
    .await;
    let control = server.control();
    let mut launch = Launch::new(&server);
    launch.tuning = Tuning {
        stream_handover: Duration::from_secs(2),
        ..tuning()
    };
    let mut engine = launch.start();
    settled(&mut engine).await;
    stream_mode(&control, false).await;

    // Three downtimes in effect on one service.
    let held = ok_services(&control, 1, 0)[0].clone();
    let held_key = ObjectKey::service(&held.0, &held.1);
    let now = control.now().as_unix_seconds();
    let downtimes: Vec<String> = (0..3)
        .map(|index| {
            control
                .schedule_downtime(
                    &held_key,
                    "carol",
                    &format!("maintenance {index}"),
                    Timestamp::from_unix_seconds(now - 60.0),
                    Timestamp::from_unix_seconds(now + 3_600.0),
                    None,
                )
                .unwrap()
        })
        .collect();
    engine
        .snapshot(|snapshot| {
            snapshot.services[held_key.as_service().unwrap()]
                .check
                .downtime_depth
                == 3
        })
        .await;

    let mut changed: Vec<(ObjectKey, ServiceState)> = Vec::new();
    let fresh = ok_services(&control, 100, 1);
    for (round, quiet) in [true, false, true, false].into_iter().enumerate() {
        // The burst begins as soon as the second stream is subscribed, so
        // its events go to both.
        engine.send(Command::SetQuiet(quiet));
        assert!(
            control
                .wait_for_event_streams(2, Duration::from_secs(10))
                .await,
            "round {round}: the new stream opens next to the old"
        );
        for (host, service) in &fresh[round * 25..(round + 1) * 25] {
            set(
                &control,
                host,
                service,
                ServiceState::Critical,
                "CRITICAL - burst",
            );
            changed.push((ObjectKey::service(host, service), ServiceState::Critical));
        }
        if round < 2 {
            control.remove_downtime(&downtimes[round]).unwrap();
        }
        // Earlier rounds' problems recover.
        for (key, state) in changed
            .iter_mut()
            .filter(|(_, state)| *state == ServiceState::Critical)
            .take(10)
        {
            let service = key.as_service().unwrap();
            set(
                &control,
                &service.host.to_string(),
                &service.name,
                ServiceState::Ok,
                "OK - back",
            );
            *state = ServiceState::Ok;
        }
        stream_mode(&control, quiet).await;
        let expected = changed.clone();
        let snapshot = engine
            .snapshot(|snapshot| {
                expected.iter().all(|(key, state)| {
                    snapshot.services[key.as_service().unwrap()].state == *state
                })
            })
            .await;
        assert_eq!(snapshot.quiet, quiet);
    }
    let snapshot = engine
        .snapshot(|snapshot| {
            snapshot.services[held_key.as_service().unwrap()]
                .check
                .downtime_depth
                == 1
        })
        .await;
    assert_eq!(snapshot.downtimes[&held_key].len(), 1);
    // Every state change once in the log: a problem twice (the mock goes
    // soft, then hard), a recovered one three times.
    for (key, state) in &changed {
        let expected = if *state == ServiceState::Critical {
            2
        } else {
            3
        };
        let entries = state_entries(&engine, key).await;
        if entries != expected {
            let history = engine.history(Some(key.clone())).await;
            panic!("{key}: {entries} state entries, expected {expected}: {history:#?}");
        }
    }
    // Still the same after a while (nothing applied late).
    tokio::time::sleep(Duration::from_millis(300)).await;
    let latest = engine.latest().unwrap();
    assert_eq!(
        latest.services[held_key.as_service().unwrap()]
            .check
            .downtime_depth,
        1
    );
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quiet_mode_polls_and_reconciles_less_and_hydrates_nothing() {
    let server = mock(MockConfig::with_scenario(scenarios::large_with_hosts(
        20, 5,
    )))
    .await;
    let control = server.control();
    let mut launch = Launch::new(&server);
    launch.tuning = Tuning {
        status_interval: Duration::from_millis(100),
        quiet_status_interval: Duration::from_millis(1_500),
        reconcile_interval: Some(Duration::from_millis(400)),
        quiet_reconcile_interval: Duration::from_secs(10),
        ..tuning()
    };
    let mut engine = launch.start();
    settled(&mut engine).await;

    engine.send(Command::SetQuiet(true));
    stream_mode(&control, true).await;
    // Let the last live poll and reconcile finish (slow on a busy
    // machine: the tests run side by side).
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    control.clear_requests();
    // Rows on screen: not hydrated while quiet.
    let snapshot = engine.latest().unwrap();
    let lean: Vec<ObjectKey> = snapshot
        .services
        .values()
        .filter(|service| service.check.result.is_none())
        .take(50)
        .map(|service| service.object_key())
        .collect();
    assert_eq!(lean.len(), 50);
    engine.send(Command::Hydrate(lean.clone()));
    tokio::time::sleep(Duration::from_secs(3)).await;
    let polls = requests_to(&control, "/v1/status/CIB");
    let reconciles = lists(&control, "services");
    eprintln!("quiet, 3 s: {polls} status polls, {reconciles} reconciles");
    assert!(polls <= 3, "every 1.5 s at most: {polls}");
    assert_eq!(
        reconciles,
        0,
        "at least every 10 s: {:#?}",
        control
            .requests()
            .iter()
            .map(|request| (request.at, request.path.clone()))
            .collect::<Vec<_>>()
    );
    assert!(by_name(&control, "services").is_empty(), "no hydration");

    // Live: the normal pace, and the rows hydrate.
    engine.send(Command::SetQuiet(false));
    stream_mode(&control, false).await;
    control.clear_requests();
    engine.send(Command::Hydrate(lean.clone()));
    tokio::time::sleep(Duration::from_secs(2)).await;
    let polls = requests_to(&control, "/v1/status/CIB");
    let reconciles = lists(&control, "services");
    eprintln!("live, 2 s: {polls} status polls, {reconciles} reconciles");
    assert!(polls >= 6, "every 100 ms: {polls}");
    assert!(reconciles >= 2, "every 400 ms: {reconciles}");
    let hydrated: BTreeSet<String> = by_name(&control, "services")
        .into_iter()
        .flat_map(|(_, names)| names)
        .collect();
    assert!(lean.iter().all(|key| hydrated.contains(&key.full_name())));
    engine.shutdown();
}

/// The object the user opens is fetched at once, ahead of a long
/// hydration queue and the request budget; it shows as updating
/// meanwhile.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_opened_object_goes_first() {
    let server = mock(MockConfig::with_scenario(scenarios::large_with_hosts(
        100, 7,
    )))
    .await;
    let control = server.control();
    let mut launch = Launch::new(&server);
    launch.tuning = Tuning {
        request_interval: Duration::from_millis(400),
        request_burst: 1,
        ..tuning()
    };
    let mut engine = launch.start();
    settled(&mut engine).await;
    let snapshot = engine.latest().unwrap();
    let lean: Vec<ObjectKey> = snapshot
        .services
        .values()
        .filter(|service| service.check.result.is_none())
        .map(|service| service.object_key())
        .collect();
    assert!(lean.len() > 1_200);
    let opened = lean[lean.len() - 1].clone();
    // A screen full of rows (five requests, 400 ms apart), then the
    // object opened.
    engine.send(Command::Hydrate(lean[..1_000].to_vec()));
    let started = Instant::now();
    engine.send(Command::Focus(opened.clone()));
    let service = opened.as_service().unwrap().clone();
    let snapshot = engine
        .snapshot(|snapshot| snapshot.services[&service].check.result.is_some())
        .await;
    let filled = started.elapsed();
    eprintln!("the opened object was filled in after {filled:?}");
    assert!(filled < Duration::from_secs(1), "{filled:?}");
    assert!(!snapshot.is_updating(&opened));
    let hydrated = lean[..1_000]
        .iter()
        .filter(|key| {
            snapshot.services[key.as_service().unwrap()]
                .check
                .result
                .is_some()
        })
        .count();
    assert!(hydrated < 1_000, "before the rows on screen");
    // It was its own request.
    assert!(
        by_name(&control, "services")
            .iter()
            .any(|(_, names)| names == &[opened.full_name()])
    );
    // A slow answer: the object shows as updating meanwhile.
    control.set_latency(Duration::from_millis(500));
    let slow = lean[lean.len() - 2].clone();
    engine.send(Command::Focus(slow.clone()));
    engine
        .snapshot(|snapshot| snapshot.is_updating(&slow))
        .await;
    engine
        .snapshot(|snapshot| {
            !snapshot.is_updating(&slow)
                && snapshot.services[slow.as_service().unwrap()]
                    .check
                    .result
                    .is_some()
        })
        .await;
    control.set_latency(Duration::ZERO);
    // An object held in full and current needs no request.
    control.clear_requests();
    engine.send(Command::Focus(opened.clone()));
    let host = ObjectKey::host(&service.host.to_string());
    engine.send(Command::Focus(host));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        by_name(&control, "services")
            .iter()
            .all(|(_, names)| !names.contains(&opened.full_name())),
        "live since its result: current"
    );
    assert!(by_name(&control, "hosts").is_empty());
    engine.shutdown();
}

/// Every by-name request takes a token: after the burst, one per interval.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn by_name_requests_are_paced_by_the_budget() {
    let server = mock(MockConfig::with_scenario(scenarios::large_with_hosts(
        100, 9,
    )))
    .await;
    let control = server.control();
    let mut launch = Launch::new(&server);
    launch.tuning = Tuning {
        request_interval: Duration::from_millis(200),
        request_burst: 2,
        ..tuning()
    };
    let mut engine = launch.start();
    settled(&mut engine).await;
    // Let the load's tier 3 refill the budget.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let lean: Vec<ObjectKey> = engine
        .latest()
        .unwrap()
        .services
        .values()
        .filter(|service| service.check.result.is_none())
        .take(1_400)
        .map(|service| service.object_key())
        .collect();
    assert_eq!(lean.len(), 1_400);
    control.clear_requests();
    engine.send(Command::Hydrate(lean.clone()));
    engine
        .snapshot(|snapshot| {
            lean.iter().all(|key| {
                snapshot.services[key.as_service().unwrap()]
                    .check
                    .result
                    .is_some()
            })
        })
        .await;
    let requests = by_name(&control, "services");
    assert_eq!(requests.len(), 7, "200 names each");
    let span = requests.last().unwrap().0.as_unix_seconds() - requests[0].0.as_unix_seconds();
    eprintln!("7 by-name requests over {span:.2} s");
    // Two at once, then 200 ms each.
    assert!(span >= 0.9, "{span}");
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_background_start_waits_until_the_user_comes() {
    let server = mock(MockConfig::with_scenario(scenarios::staging())).await;
    let control = server.control();
    let mut launch = Launch::new(&server);
    launch.start = Start::Background;
    launch.tuning = Tuning {
        // Hours: the random wait is longer than this test.
        start_delay_per_thousand: Duration::from_hours(10_000),
        start_delay_max: Duration::from_hours(10),
        ..tuning()
    };
    let mut engine = launch.start();
    engine
        .wait_state(|state| {
            matches!(
                state,
                ConnectionState::Loading {
                    phase: LoadPhase::Hosts,
                    done: 0,
                    ..
                }
            )
        })
        .await;
    // It sized the installation (the status), then waits.
    assert!(wait_until(|| requests_to(&control, "/v1/status/CIB") >= 1).await);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(lists(&control, "hosts"), 0, "no big query yet");
    assert!(
        engine
            .latest()
            .is_none_or(|snapshot| snapshot.hosts.is_empty())
    );
    // The window was shown.
    let shown = Instant::now();
    engine.send(Command::StartNow);
    engine.connected().await;
    assert!(shown.elapsed() < Duration::from_secs(5));
    assert!(!engine.latest().unwrap().hosts.is_empty());
    engine.shutdown();

    // A small installation waits under a second: 3 s per 1 000 services.
    let mut launch = Launch::new(&server);
    launch.start = Start::Background;
    let started = Instant::now();
    let mut engine = launch.start();
    engine.connected().await;
    let services = control.services().len();
    eprintln!(
        "background start with {services} services: connected after {:?}",
        started.elapsed()
    );
    assert!(started.elapsed() < Duration::from_secs(3));
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_shown_notification_prefetches_its_object() {
    let server = mock(MockConfig::with_scenario(scenarios::large_with_hosts(
        20, 11,
    )))
    .await;
    let control = server.control();
    let mut engine = Launch::new(&server).start();
    settled(&mut engine).await;
    engine.send(Command::SetQuiet(true));
    stream_mode(&control, true).await;
    control.clear_requests();

    // One problem: shown, prefetched (lean before: no links, a result
    // only from the state change).
    let (host, service) = ok_services(&control, 1, 0)[0].clone();
    let key = ObjectKey::service(&host, &service);
    set(
        &control,
        &host,
        &service,
        ServiceState::Critical,
        "CRITICAL - down",
    );
    let record = engine.notification().await;
    assert!(!record.intent.silent);
    assert!(
        wait_until(|| by_name(&control, "services")
            .iter()
            .any(|(_, names)| names.contains(&key.full_name())))
        .await,
        "the notified object is fetched in full"
    );

    // A storm: what storm control silences prefetches nothing.
    control.clear_requests();
    let many = ok_services(&control, 30, 0);
    for (host, service) in &many {
        set(
            &control,
            host,
            service,
            ServiceState::Critical,
            "CRITICAL - storm",
        );
    }
    let deadline = tokio::time::Instant::now() + WAIT;
    while engine.notifications().len() < 31 && tokio::time::Instant::now() < deadline {
        let _ = tokio::time::timeout(Duration::from_millis(200), engine.next()).await;
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    let shown = engine
        .notifications()
        .iter()
        .skip(1)
        .filter(|record| !record.intent.silent && record.intent.object.is_some())
        .count();
    let prefetched: BTreeSet<String> = by_name(&control, "services")
        .into_iter()
        .flat_map(|(_, names)| names)
        .collect();
    eprintln!(
        "storm of 30: {shown} shown, {} prefetched",
        prefetched.len()
    );
    assert!(shown < 30, "storm control silenced most");
    assert!(prefetched.len() <= shown, "{} > {shown}", prefetched.len());
    engine.shutdown();
}

/// Waking up fetches the problems whose result a check may have replaced
/// while no check results came (here a quiet reconcile saw a newer check).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn waking_up_refreshes_problems_whose_results_went_stale() {
    let server = mock(MockConfig::with_scenario(scenarios::staging())).await;
    let control = server.control();
    let mut launch = Launch::new(&server);
    launch.tuning = Tuning {
        reconcile_interval: Some(Duration::from_millis(200)),
        quiet_reconcile_interval: Duration::from_millis(300),
        ..tuning()
    };
    let mut engine = launch.start();
    settled(&mut engine).await;
    let snapshot = engine.latest().unwrap();
    let (problem, code) = snapshot
        .services
        .values()
        .find(|service| service.is_problem() && service.check.result.is_some())
        .map(|service| {
            let code = match service.state {
                ServiceState::Warning => 1,
                ServiceState::Critical => 2,
                _ => 3,
            };
            (service.object_key(), code)
        })
        .expect("a problem");
    engine.send(Command::SetQuiet(true));
    stream_mode(&control, true).await;

    // Checked again, same state, new output: nothing comes while quiet.
    control
        .process_check_result(&problem, code, "CRITICAL - newer output", &[])
        .unwrap();
    let truth = control
        .service(
            &problem.as_service().unwrap().host.to_string(),
            &problem.as_service().unwrap().name,
        )
        .unwrap();
    let last_check = truth.check.last_check.unwrap().as_unix_seconds();
    // A quiet reconcile brings the newer last check, not the output.
    let service = problem.as_service().unwrap().clone();
    let snapshot = engine
        .snapshot(|snapshot| {
            snapshot.services[&service]
                .check
                .last_check
                .is_some_and(|at| (at.as_unix_seconds() - last_check).abs() < 0.01)
        })
        .await;
    assert_ne!(
        snapshot.services[&service]
            .check
            .result
            .as_ref()
            .unwrap()
            .output,
        "CRITICAL - newer output",
        "stale while quiet"
    );
    control.clear_requests();
    engine.send(Command::SetQuiet(false));
    engine
        .snapshot(|snapshot| {
            snapshot.services[&service]
                .check
                .result
                .as_ref()
                .is_some_and(|result| result.output == "CRITICAL - newer output")
        })
        .await;
    assert!(
        by_name(&control, "services")
            .iter()
            .any(|(_, names)| names.contains(&problem.full_name()))
    );
    engine.shutdown();
}

/// A quiet stream is silent most of the time; one that stalls is found
/// by Icinga's state counts moving while nothing arrives, and reopened;
/// the reload after it finds the change.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_quiet_stream_that_stalls_is_reopened() {
    let server = mock(MockConfig::with_scenario(scenarios::staging())).await;
    let control = server.control();
    let mut launch = Launch::new(&server);
    launch.tuning = Tuning {
        status_interval: Duration::from_millis(100),
        quiet_status_interval: Duration::from_millis(200),
        stall_after: Duration::from_millis(300),
        ..tuning()
    };
    let mut engine = launch.start();
    settled(&mut engine).await;
    engine.send(Command::SetQuiet(true));
    stream_mode(&control, true).await;
    // Two quiet polls with nothing changed: no reconnect.
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(
        !engine.seen.iter().any(|event| matches!(
            event,
            CoreEvent::Connection(ConnectionState::Reconnecting { .. })
        )),
        "a silent quiet stream is fine"
    );

    assert_eq!(control.stall_event_streams(), 1);
    set(
        &control,
        "stg-api-01",
        "http",
        ServiceState::Critical,
        "CRITICAL - unseen",
    );
    let state = engine
        .wait_state(|state| matches!(state, ConnectionState::Reconnecting { .. }))
        .await;
    let ConnectionState::Reconnecting { error, .. } = state else {
        unreachable!()
    };
    assert!(error.contains("stalled"), "{error}");
    engine.connected().await;
    stream_mode(&control, true).await;
    // What the stall hid comes with the reload, and notifies.
    let record = engine.notification().await;
    assert_eq!(record.intent.title, "CRITICAL · http on stg-api-01");
    engine.shutdown();
}

/// With the environment's notifications off and one dashboard on, quiet
/// mode keeps that dashboard's memberships current (its notification
/// names it), and leaves rows and summaries until waking up.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quiet_mode_evaluates_only_what_notifications_need() {
    let server = mock(MockConfig::with_scenario(scenarios::staging())).await;
    let control = server.control();
    let mut launch = Launch::new(&server);
    launch.environment.notifications.enabled = false;
    let group = &mut launch.environment.groups[0];
    group.dashboards[0].notifications = ScopeSetting::On;
    let reference = |index: usize| DashboardRef {
        group_id: launch.environment.groups[0].id.clone(),
        dashboard_id: launch.environment.groups[0].dashboards[index].id.clone(),
    };
    let (problems, all) = (reference(0), reference(2));
    let mut engine = launch.start();
    settled(&mut engine).await;
    let before = engine.latest().unwrap();
    let all_critical = before.dashboards[&all].summary.critical;
    let problems_critical = before.dashboards[&problems].summary.critical;

    engine.send(Command::SetQuiet(true));
    stream_mode(&control, true).await;
    set(
        &control,
        "stg-web-01",
        "http",
        ServiceState::Critical,
        "CRITICAL - quiet",
    );
    // Notified through the dashboard that is on.
    let record = engine.notification().await;
    assert_eq!(record.intent.title, "CRITICAL · http on stg-web-01");
    assert_eq!(record.intent.subtitle, "overview / problems");
    let key = ic_model::ServiceKey::new("stg-web-01", "http");
    let snapshot = engine
        .snapshot(|snapshot| snapshot.services[&key].state == ServiceState::Critical)
        .await;
    assert_eq!(
        snapshot.dashboards[&all].summary.critical, all_critical,
        "left alone while quiet"
    );
    assert_eq!(
        snapshot.dashboards[&problems].summary.critical,
        problems_critical
    );

    engine.send(Command::SetQuiet(false));
    engine
        .snapshot(|snapshot| {
            snapshot.dashboards[&all].summary.critical == all_critical + 1
                && snapshot.dashboards[&problems].summary.critical == problems_critical + 1
        })
        .await;
    engine.shutdown();
}
