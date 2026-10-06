//! Treating a struggling or unusual Icinga gently: a failing reload waits
//! a whole interval, `Refresh` presses coalesce, events about objects the
//! user can't query cost one request per batch (and less and less often),
//! a kind the user may not query is never asked for, and a stream that
//! stalls without closing is noticed and reopened.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers fail the test loudly"
)]

use std::time::Duration;

use crate::support::{Launch, PASSWORD, USER, mock, wait_until};
use ic_core::{Command, ConnectionState, Tuning};
use ic_mock::{MockConfig, MockControl, MockUser, scenarios};
use ic_model::{HostState, ServiceKey, ServiceState};
use serde_json::{Value, json};

/// The arrival times (mock clock, seconds) of whole host list loads
/// (tier 1), failed or not.
fn host_lists(control: &MockControl) -> Vec<f64> {
    control
        .requests()
        .iter()
        .filter(|request| {
            request.path == "/v1/objects/hosts"
                && request
                    .body
                    .as_ref()
                    .is_some_and(|body| body.get("hosts").is_none())
        })
        .map(|request| request.at.as_unix_seconds())
        .collect()
}

/// The by-name queries of `path` so far, with their names.
fn by_name(control: &MockControl, path: &str, kind: &str) -> Vec<Vec<String>> {
    control
        .requests()
        .iter()
        .filter(|request| request.path == path)
        .filter_map(|request| {
            let names = request.body.as_ref()?.get(kind)?.as_array()?;
            Some(
                names
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect(),
            )
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failing_reload_waits_a_whole_interval() {
    let server = mock(MockConfig::with_scenario(scenarios::lab())).await;
    let control = server.control();
    let interval = Duration::from_millis(250);
    let mut launch = Launch::new(&server);
    launch.tuning = Tuning {
        reconcile_interval: Some(interval),
        ..launch.tuning
    };
    let mut engine = launch.start();
    engine.connected().await;
    engine
        .snapshot(|snapshot| !snapshot.icinga_notifications.is_empty())
        .await;

    // The master (or a proxy in front of it) answers 503 from now on; the
    // event stream stays open. Each attempt's tier 1 asks for the status
    // first (the status poll runs every 30 s, not within this test).
    control.clear_requests();
    control.fail_next(1_000_000, 503);
    let attempts = || -> Vec<f64> {
        control
            .requests()
            .iter()
            .filter(|request| request.path == "/v1/status/IcingaApplication")
            .map(|request| request.at.as_unix_seconds())
            .collect()
    };
    assert!(
        wait_until(|| attempts().len() >= 4).await,
        "{:?}",
        attempts()
    );
    control.fail_next(0, 503);
    let loads = attempts();
    for pair in loads.windows(2) {
        let gap = pair[1] - pair[0];
        assert!(
            gap >= 0.9 * interval.as_secs_f64() - 0.02,
            "a failed reload is retried only after an interval: {loads:?}"
        );
    }
    let after_connected: Vec<ConnectionState> = engine
        .states()
        .into_iter()
        .skip_while(|state| !matches!(state, ConnectionState::Connected { .. }))
        .skip(1)
        .collect();
    assert!(
        after_connected.is_empty(),
        "the stream decides about the connection: {after_connected:?}"
    );
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refresh_presses_coalesce() {
    let server = mock(MockConfig::with_scenario(scenarios::lab())).await;
    let control = server.control();
    let mut engine = Launch::new(&server).start();
    engine.connected().await;
    engine
        .snapshot(|snapshot| !snapshot.icinga_notifications.is_empty())
        .await;
    control.clear_requests();
    let notification_lists = || {
        control
            .requests()
            .iter()
            .filter(|request| {
                request.path == "/v1/objects/notifications"
                    && request
                        .body
                        .as_ref()
                        .is_none_or(|body| body.get("notifications").is_none())
            })
            .count()
    };

    // The first press reloads at once (pressed again while it runs: once).
    engine.send(Command::Refresh);
    engine.send(Command::Refresh);
    assert!(wait_until(|| notification_lists() == 1).await, "reloaded");
    assert_eq!(host_lists(&control).len(), 1);

    // More presses right after: one reload, later.
    for _ in 0..5 {
        engine.send(Command::Refresh);
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(host_lists(&control).len(), 1, "coalesced, not repeated");
    engine.shutdown();
}

/// A `CheckResult` event for a service the client can't see.
fn hidden_check(name: &str) -> Value {
    json!({
        "type": "CheckResult",
        "timestamp": 1.0,
        "host": "hidden-host",
        "service": name,
        "check_result": {
            "state": 2,
            "output": "CRITICAL",
            "vars_after": {"state": 2, "state_type": 1, "attempt": 1, "reachable": true},
        },
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn events_about_hidden_objects_cost_one_request_per_batch() {
    // An API user whose `objects/query/Service` is filtered still receives
    // every service's events; Icinga answers names it hides like unknown
    // ones ("No objects found.").
    let server = mock(MockConfig::with_scenario(scenarios::lab())).await;
    let control = server.control();
    let ttl = Duration::from_millis(400);
    let mut launch = Launch::new(&server);
    launch.tuning = Tuning {
        missing_ttl: ttl,
        ..launch.tuning
    };
    let mut engine = launch.start();
    engine.connected().await;
    control.clear_requests();
    let names: Vec<String> = (0..400).map(|index| format!("s{index:03}")).collect();
    let emit_all = || {
        for name in &names {
            control.emit_raw(hidden_check(name));
        }
    };
    let service_queries = || by_name(&control, "/v1/objects/services", "services");
    let asked = || service_queries().iter().map(Vec::len).sum::<usize>();

    emit_all();
    assert!(wait_until(|| asked() >= 400).await, "{}", asked());
    // A barrier: a known object's change is re-queried after them.
    control.touch_object("Service", "lab-01!ssh").unwrap();
    assert!(
        wait_until(|| service_queries()
            .iter()
            .any(|names| names == &["lab-01!ssh"]))
        .await
    );
    let hidden: Vec<Vec<String>> = service_queries()
        .into_iter()
        .filter(|names| names != &["lab-01!ssh"])
        .collect();
    assert!(
        hidden.len() <= 4,
        "whole batches, not isolated names: {} requests",
        hidden.len()
    );
    assert!(hidden.iter().all(|names| names.len() <= 200));

    // Missing now: more events cost nothing.
    control.clear_requests();
    emit_all();
    control.touch_object("Service", "lab-01!ssh").unwrap();
    assert!(wait_until(|| !service_queries().is_empty()).await);
    assert_eq!(service_queries(), [vec!["lab-01!ssh".to_owned()]]);

    // After the wait they are asked once more, and then wait twice as long.
    tokio::time::sleep(ttl + Duration::from_millis(50)).await;
    control.clear_requests();
    emit_all();
    assert!(wait_until(|| asked() >= 400).await, "{}", asked());
    tokio::time::sleep(ttl + Duration::from_millis(50)).await;
    control.clear_requests();
    emit_all();
    control.touch_object("Service", "lab-01!ssh").unwrap();
    assert!(wait_until(|| !service_queries().is_empty()).await);
    assert_eq!(
        service_queries(),
        [vec!["lab-01!ssh".to_owned()]],
        "the second wait is longer"
    );
    let snapshot = engine.latest().unwrap();
    assert!(
        !snapshot
            .services
            .contains_key(&ServiceKey::new("hidden-host", "s000"))
    );
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_kind_the_user_may_not_query_is_never_asked_for() {
    // Services and events, but no `objects/query/Host`.
    let mut config = MockConfig::with_scenario(scenarios::lab());
    config.users = vec![MockUser::new(
        USER,
        PASSWORD,
        &["objects/query/Service", "status/query", "events/*"],
    )];
    let server = mock(config).await;
    let control = server.control();
    let mut engine = Launch::new(&server).start();
    engine.connected().await;
    let snapshot = engine.latest().unwrap();
    assert!(snapshot.hosts.is_empty());
    assert!(!snapshot.services.is_empty());
    control.clear_requests();

    // Host check results: every host is unknown to the store.
    for _ in 0..3 {
        control
            .set_host_state("lab-01", HostState::Up, "UP", false)
            .unwrap();
    }
    control
        .set_service_state("lab-01", "ssh", ServiceState::Critical, "CRITICAL", true)
        .unwrap();
    engine
        .snapshot(|snapshot| {
            snapshot
                .services
                .get(&ServiceKey::new("lab-01", "ssh"))
                .is_some_and(|service| service.state == ServiceState::Critical)
        })
        .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let hosts: Vec<u16> = control
        .requests()
        .iter()
        .filter(|request| request.path == "/v1/objects/hosts")
        .map(|request| request.status)
        .collect();
    assert!(hosts.is_empty(), "never asked: {hosts:?}");
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stream_that_stalls_without_closing_is_reopened() {
    let server = mock(MockConfig::with_scenario(scenarios::lab())).await;
    let control = server.control();
    let mut launch = Launch::new(&server);
    launch.tuning = Tuning {
        status_interval: Duration::from_millis(100),
        stall_after: Duration::from_millis(400),
        ..launch.tuning
    };
    let mut engine = launch.start();
    engine.connected().await;
    let streams = || {
        control
            .requests()
            .iter()
            .filter(|request| request.path == "/v1/events")
            .count()
    };
    assert_eq!(streams(), 1);

    // A proxy stops relaying the stream; Icinga keeps checking.
    assert_eq!(control.stall_event_streams(), 1);
    control.burst();
    control.run_queued_checks(usize::MAX);
    let state = engine
        .wait_state(|state| matches!(state, ConnectionState::Reconnecting { .. }))
        .await;
    let ConnectionState::Reconnecting { error, .. } = state else {
        unreachable!();
    };
    assert!(error.contains("stalled"), "{error}");
    engine.connected().await;
    assert!(streams() >= 2, "a new stream");

    // The new stream works.
    control
        .set_service_state("lab-01", "ssh", ServiceState::Critical, "CRITICAL", true)
        .unwrap();
    engine
        .snapshot(|snapshot| {
            snapshot
                .services
                .get(&ServiceKey::new("lab-01", "ssh"))
                .is_some_and(|service| service.state == ServiceState::Critical)
        })
        .await;
    engine.shutdown();
}
