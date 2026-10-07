//! Production scale (docs/performance.md): the `large` scenario's payloads,
//! its steady check rate, and bursts (mass re-checks) at Icinga's pace.
//!
//! The default suite runs the scenario at a tenth of its size or less; the
//! full-size measurements are `#[ignore]`d (`cargo test -p ic-mock --test
//! scale -- --ignored`, best with `--release`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_precision_loss,
    reason = "test code fails loudly and computes rough averages"
)]

mod common;

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use common::{EventStream, json, request, results, start};
use ic_mock::{MockConfig, MockError, MockServer, SimulationConfig, scenarios};
use reqwest::{Client, Method, StatusCode};
use serde_json::{Value, json};

/// The attributes of the client's lean service load (`Detail::Lean`).
const LEAN: [&str; 21] = [
    "__name",
    "host_name",
    "display_name",
    "state",
    "state_type",
    "last_state_change",
    "last_hard_state_change",
    "last_check",
    "next_check",
    "next_update",
    "check_attempt",
    "max_check_attempts",
    "acknowledgement",
    "acknowledgement_expiry",
    "downtime_depth",
    "flapping",
    "last_reachable",
    "check_interval",
    "retry_interval",
    "groups",
    "vars",
];

/// A query's body size in bytes and its parsed results.
async fn query(client: &Client, server: &MockServer, plural: &str, body: &Value) -> (usize, Value) {
    let response = request(
        client,
        server,
        Method::POST,
        &format!("/v1/objects/{plural}"),
    )
    .header("X-HTTP-Method-Override", "GET")
    .json(body)
    .send()
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.bytes().await.unwrap();
    (bytes.len(), serde_json::from_slice(&bytes).unwrap())
}

/// The full name an event is about.
fn event_object(event: &Value) -> String {
    let host = event["host"].as_str().unwrap();
    match event["service"].as_str() {
        Some(service) => format!("{host}!{service}"),
        None => host.to_owned(),
    }
}

/// Average bytes per object of the measured setup (docs/performance.md,
/// 2 005 hosts and 30 006 services on a real Icinga 2.15.6).
const MEASURED_HOST: f64 = 5.8e6 / 2_005.0;
const MEASURED_SERVICE_ALL: f64 = 73.8e6 / 30_006.0;
const MEASURED_SERVICE_LEAN: f64 = 20.1e6 / 30_006.0;

fn assert_near(what: &str, actual: f64, measured: f64) {
    let ratio = actual / measured;
    assert!(
        (0.8..=1.25).contains(&ratio),
        "{what}: {actual:.0} bytes per object, measured {measured:.0}"
    );
}

#[tokio::test]
async fn large_payloads_have_the_measured_sizes() {
    let config = MockConfig::with_scenario(scenarios::large_with_hosts(60, 7));
    let (server, client) = start(config).await;

    let (bytes, hosts) = query(&client, &server, "hosts", &json!({})).await;
    assert_eq!(results(&hosts).len(), 60);
    assert_near("hosts, all attributes", bytes as f64 / 60.0, MEASURED_HOST);

    let (bytes, services) = query(&client, &server, "services", &json!({})).await;
    assert_eq!(results(&services).len(), 900);
    assert_near(
        "services, all attributes",
        bytes as f64 / 900.0,
        MEASURED_SERVICE_ALL,
    );

    // A lean query carries exactly the selected attributes: no check
    // results, so it is a fraction of the size.
    let (lean_bytes, lean) = query(&client, &server, "services", &json!({ "attrs": LEAN })).await;
    let expected: BTreeSet<&str> = LEAN.into_iter().collect();
    for entry in results(&lean) {
        let keys: BTreeSet<&str> = entry["attrs"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, expected);
    }
    assert_near(
        "services, lean",
        lean_bytes as f64 / 900.0,
        MEASURED_SERVICE_LEAN,
    );
    assert!(lean_bytes * 3 < bytes, "lean {lean_bytes}, all {bytes}");

    // Realistic content: outputs with perfdata, custom variables, about
    // 5 % problems.
    let services = results(&services);
    let with_perfdata = services
        .iter()
        .filter(|s| {
            s["attrs"]["last_check_result"]["performance_data"]
                .as_array()
                .is_some_and(|p| !p.is_empty())
        })
        .count();
    assert!(with_perfdata * 10 > services.len() * 7, "{with_perfdata}");
    assert!(services.iter().all(|s| s["attrs"]["vars"].is_object()));
    assert!(
        services
            .iter()
            .all(|s| s["attrs"]["check_interval"] == json!(300)
                && s["attrs"]["retry_interval"] == json!(60))
    );
    let problems = services
        .iter()
        .filter(|s| s["attrs"]["state"] != json!(0))
        .count();
    assert!((15..=80).contains(&problems), "{problems} of 900");
}

#[tokio::test]
async fn large_checks_every_object_once_per_five_minutes() {
    // A seed without failed hosts: their services would change state and
    // be re-checked at the retry interval.
    let scenario = (1..100)
        .map(|seed| scenarios::large_with_hosts(40, seed))
        .find(|scenario| scenario.summary().hosts_down == 0)
        .unwrap();
    let config = MockConfig {
        simulation: SimulationConfig {
            problems_per_hour: 0.0,
            flapping: false,
            outages: false,
            downtimes: false,
            ..SimulationConfig::default()
        },
        ..MockConfig::with_scenario(scenario)
    };
    let (server, client) = start(config).await;
    let mut stream = EventStream::open(
        &client,
        &server,
        &json!({"types": ["CheckResult"], "queue": "steady"}),
    )
    .await;
    // Five simulated minutes (one tick is one second): 40 hosts and 600
    // services, each checked once. At full size that is 32 000 checks per
    // 5 minutes, the ~107 results per second measured on the real Icinga.
    server.control().step_simulation(300);
    let mut checked = BTreeSet::new();
    let mut events = 0;
    while let Ok(Some(line)) =
        tokio::time::timeout(Duration::from_millis(500), stream.next_line()).await
    {
        let event: Value = serde_json::from_str(&line).unwrap();
        checked.insert(event_object(&event));
        events += 1;
    }
    assert_eq!(checked.len(), 640, "every object was checked");
    assert_eq!(events, 640, "exactly once");
}

#[tokio::test]
async fn bursts_recheck_every_object_at_the_check_rate() {
    let config = MockConfig {
        check_rate: 400.0,
        ..MockConfig::with_scenario(scenarios::large_with_hosts(10, 1))
    };
    let (server, client) = start(config).await;
    let control = server.control();
    let before = control.services();
    let mut stream = EventStream::open(
        &client,
        &server,
        &json!({"types": ["CheckResult", "StateChange"], "queue": "burst"}),
    )
    .await;
    let started = Instant::now();
    assert_eq!(control.burst(), 160, "10 hosts and 150 services");
    assert_eq!(
        control.burst(),
        0,
        "objects already waiting are checked once"
    );
    let mut seen = BTreeSet::new();
    while seen.len() < 160 {
        let event = stream.next().await;
        assert_eq!(event["type"], "CheckResult", "a re-check changes no state");
        assert!(event["check_result"]["vars_after"].is_object());
        assert!(seen.insert(event_object(&event)), "one result per object");
    }
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_millis(300),
        "160 checks at 400/s take about 0.4 s, not {elapsed:?}"
    );
    assert_eq!(control.queued_checks(), 0);
    // Same states, newer results.
    let after = control.services();
    for (old, new) in before.iter().zip(&after) {
        assert_eq!(old.state, new.state, "{}", old.key);
        assert!(
            new.check.last_check > old.check.last_check,
            "{}: {:?} -> {:?}",
            old.key,
            old.check.last_check,
            new.check.last_check
        );
    }
}

#[tokio::test]
async fn bursts_can_be_run_without_waiting() {
    let (server, client) =
        start(MockConfig::with_scenario(scenarios::large_with_hosts(5, 2))).await;
    let control = server.control();
    // Slow enough that nothing runs on its own during the test.
    control.set_check_rate(0.01).unwrap();
    let mut stream = EventStream::open(
        &client,
        &server,
        &json!({"types": ["CheckResult"], "queue": "q"}),
    )
    .await;
    assert_eq!(control.burst(), 80);
    assert_eq!(control.queued_checks(), 80);
    assert_eq!(control.run_queued_checks(30), 30);
    assert_eq!(control.queued_checks(), 50);
    control.run_timers();
    assert_eq!(control.queued_checks(), 0, "the timers run what is due");
    assert!(
        control
            .wait_for_queued_checks(Duration::from_millis(10))
            .await
    );
    let mut seen = BTreeSet::new();
    for _ in 0..80 {
        seen.insert(event_object(&stream.next().await));
    }
    assert_eq!(seen.len(), 80);

    for rate in [0.0, -5.0, f64::NAN, f64::INFINITY] {
        assert!(matches!(
            control.set_check_rate(rate),
            Err(MockError::InvalidConfig(_))
        ));
    }
    let invalid = MockConfig {
        check_rate: 0.0,
        ..MockConfig::default()
    };
    assert!(matches!(
        MockServer::start(invalid).await,
        Err(MockError::InvalidConfig(_))
    ));
}

/// A forced `reschedule-check` of everything (how the measurements made
/// their burst) goes through the same checker: results arrive at the
/// check rate instead of all at once.
#[tokio::test]
async fn mass_reschedule_checks_are_paced_like_bursts() {
    let config = MockConfig {
        check_rate: 500.0,
        reschedule_delay: Duration::ZERO,
        ..MockConfig::with_scenario(scenarios::large_with_hosts(10, 4))
    };
    let (server, client) = start(config).await;
    let mut stream = EventStream::open(
        &client,
        &server,
        &json!({"types": ["CheckResult"], "queue": "q"}),
    )
    .await;
    let started = Instant::now();
    let (status, body) = json(
        request(
            &client,
            &server,
            Method::POST,
            "/v1/actions/reschedule-check",
        )
        .json(&json!({"type": "Service", "force": true}))
        .send()
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(results(&body).len(), 150);
    let mut seen = BTreeSet::new();
    while seen.len() < 150 {
        seen.insert(event_object(&stream.next().await));
    }
    assert!(
        started.elapsed() >= Duration::from_millis(200),
        "{:?}",
        started.elapsed()
    );
}

/// The full-size scenario against the measured numbers (slow in debug
/// builds; run with `--ignored`, best with `--release`).
#[tokio::test]
#[ignore = "full production scale: 2 000 hosts, 30 000 services"]
async fn full_size_large_matches_the_measurements() {
    let (server, client) = start(MockConfig::with_scenario(scenarios::large(1))).await;
    let (bytes, hosts) = query(&client, &server, "hosts", &json!({})).await;
    assert_eq!(results(&hosts).len(), 2_000);
    assert_near("hosts", bytes as f64 / 2_000.0, MEASURED_HOST);
    let (bytes, services) = query(&client, &server, "services", &json!({})).await;
    assert_eq!(results(&services).len(), 30_000);
    assert_near(
        "services, all",
        bytes as f64 / 30_000.0,
        MEASURED_SERVICE_ALL,
    );
    let (bytes, _) = query(&client, &server, "services", &json!({ "attrs": LEAN })).await;
    assert_near(
        "services, lean",
        bytes as f64 / 30_000.0,
        MEASURED_SERVICE_LEAN,
    );

    // A burst: 32 000 results at about 5 000 per second.
    let mut stream = EventStream::open(
        &client,
        &server,
        &json!({"types": ["CheckResult", "StateChange"], "queue": "burst"}),
    )
    .await;
    let started = Instant::now();
    assert_eq!(server.control().burst(), 32_000);
    let mut bytes = 0;
    for _ in 0..32_000 {
        bytes += stream.next_line().await.unwrap().len() + 1;
    }
    let elapsed = started.elapsed();
    assert!(
        (Duration::from_secs(5)..Duration::from_secs(20)).contains(&elapsed),
        "{elapsed:?}"
    );
    let average = bytes as f64 / 32_000.0;
    assert!(
        (500.0..1_000.0).contains(&average),
        "{average} bytes per event"
    );
}

/// Filters that use objects as values (`host != null`, `service.host ==
/// host`) build each object's value once per request, not once per use:
/// a few seconds at most for 30 000 services, not minutes with the
/// server's state locked.
#[tokio::test]
#[ignore = "full production scale: 2 000 hosts, 30 000 services"]
async fn full_size_filters_with_objects_as_values() {
    let (server, client) = start(MockConfig::with_scenario(scenarios::large(1))).await;
    for filter in [
        "host != null && service.state == 2",
        "service.host == host",
        "len([host, host, host, host, host, host, host, host, host, host]) == 10",
    ] {
        let started = Instant::now();
        let (_, body) = query(
            &client,
            &server,
            "services",
            &json!({ "filter": filter, "attrs": ["name"] }),
        )
        .await;
        let elapsed = started.elapsed();
        assert!(!results(&body).is_empty(), "{filter}");
        assert!(elapsed < Duration::from_secs(20), "{filter}: {elapsed:?}");
    }
}
