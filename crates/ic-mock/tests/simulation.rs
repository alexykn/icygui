//! The simulator (determinism, stepping, events) and the mock clock.

mod common;

use std::time::Duration;

use common::{EventStream, start};
use ic_mock::{MockConfig, SimulationConfig, StormConfig};
use ic_model::{ObjectKey, ServiceState};
use serde_json::json;

/// What the simulator decides, without wall-clock timestamps.
fn fingerprint(server: &ic_mock::MockServer) -> Vec<(String, ServiceState, String, u32, bool)> {
    server
        .control()
        .services()
        .into_iter()
        .map(|s| {
            (
                s.key.to_string(),
                s.state,
                s.check.result.map(|r| r.output).unwrap_or_default(),
                s.check.attempt,
                s.check.flapping,
            )
        })
        .collect()
}

fn config(seed: u64) -> MockConfig {
    MockConfig {
        simulation: SimulationConfig {
            seed,
            problems_per_hour: 120.0,
            ..SimulationConfig::default()
        },
        ..MockConfig::with_scenario(ic_mock::scenarios::prod_cluster())
    }
}

#[tokio::test]
async fn same_seed_same_story() {
    let (a, _) = start(config(42)).await;
    let (b, _) = start(config(42)).await;
    let (c, _) = start(config(43)).await;
    let before = fingerprint(&a);
    for server in [&a, &b, &c] {
        assert!(!server.control().simulation_running());
        server.control().step_simulation(600);
        assert_eq!(server.control().simulation_tick(), 600);
    }
    let story = fingerprint(&a);
    assert_ne!(story, before, "ten simulated minutes change something");
    assert_eq!(story, fingerprint(&b));
    assert_ne!(story, fingerprint(&c), "another seed tells another story");
}

#[tokio::test]
async fn stepping_emits_check_results() {
    let (server, client) = start(config(7)).await;
    let mut stream = EventStream::open(
        &client,
        &server,
        &json!({"types": ["CheckResult", "StateChange"], "queue": "sim"}),
    )
    .await;
    server.control().step_simulation(120);
    let event = stream.next().await;
    assert!(event["type"] == "CheckResult" || event["type"] == "StateChange");
    assert!(event["check_result"]["output"].is_string());
}

#[tokio::test]
async fn pinned_design_problems_survive_the_simulator() {
    let (server, _) = start(config(9)).await;
    server.control().step_simulation(3600);
    let service = server
        .control()
        .service("db-prod-03", "postgres-replication")
        .unwrap();
    assert_eq!(service.state, ServiceState::Critical);
}

#[tokio::test]
async fn storms_raise_many_problems_at_once() {
    let mut storm = config(5);
    storm.simulation.storm = Some(StormConfig {
        every_ticks: 100,
        size: 30,
        duration_ticks: 50,
    });
    storm.simulation.problems_per_hour = 0.0;
    let (server, _) = start(storm).await;
    let problems = |server: &ic_mock::MockServer| {
        server
            .control()
            .services()
            .iter()
            .filter(|s| s.state != ServiceState::Ok)
            .count()
    };
    let before = problems(&server);
    server.control().step_simulation(110);
    assert!(
        problems(&server) >= before + 20,
        "{before} -> {}",
        problems(&server)
    );
}

#[tokio::test]
async fn real_time_simulation_runs_and_pauses() {
    let mut fast = config(3);
    fast.simulation.enabled = true;
    fast.simulation.speed = 50.0;
    let (server, _) = start(fast).await;
    let control = server.control();
    assert!(control.simulation_running());
    let mut ticked = false;
    for _ in 0..50 {
        if control.simulation_tick() > 2 {
            ticked = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(ticked);
    control.pause_simulation();
    // Let an in-flight tick finish.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let tick = control.simulation_tick();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(control.simulation_tick(), tick);
}

#[tokio::test]
async fn advancing_the_clock_expires_downtimes_and_acks() {
    let (server, _) = start(MockConfig::default()).await;
    let control = server.control();
    let host = ObjectKey::host("lab-01");
    let now = control.now();
    let end = ic_model::Timestamp::from_unix_seconds(now.as_unix_seconds() + 600.0);
    let start_at = ic_model::Timestamp::from_unix_seconds(now.as_unix_seconds() + 60.0);
    let name = control
        .schedule_downtime(&host, "t", "later", start_at, end, None)
        .unwrap();
    assert!(
        !control
            .downtimes()
            .iter()
            .any(|d| d.name == name && d.in_effect)
    );
    control.advance_clock(Duration::from_mins(2));
    assert!(
        control
            .downtimes()
            .iter()
            .any(|d| d.name == name && d.in_effect)
    );
    assert_eq!(control.host("lab-01").unwrap().check.downtime_depth, 1);
    control.advance_clock(Duration::from_mins(10));
    assert!(
        control.downtimes().iter().all(|d| d.name != name),
        "expired"
    );
    assert_eq!(control.host("lab-01").unwrap().check.downtime_depth, 0);
}

#[tokio::test]
async fn control_rejects_unknown_objects() {
    let (server, _) = start(MockConfig::default()).await;
    let control = server.control();
    assert!(
        control
            .set_service_state("lab-01", "nope", ServiceState::Critical, "x", true)
            .is_err()
    );
    assert!(
        control
            .acknowledge(&ObjectKey::host("lab-01"), "a", "c", false)
            .is_err(),
        "UP hosts can't be acknowledged"
    );
    assert!(control.remove_downtime("nope").is_err());
}

/// The first simulated check of an object comes at its `next_check`, as
/// Icinga's checker would run it: a soft problem retried a minute after
/// the start is checked then, not minutes later (clients would mark it
/// late).
#[tokio::test]
async fn first_checks_come_when_next_check_says() {
    let (server, _) = start(config(7)).await;
    let control = server.control();
    let kubelet = || control.service("k8s-node-04", "kubelet").unwrap();
    let before = kubelet();
    let next_check = before.check.next_check.expect("scheduled");
    let last_check = before.check.last_check.expect("checked before");
    let due_in = next_check.as_unix_seconds() - ic_model::Timestamp::now().as_unix_seconds();
    assert!(
        (1.0..=120.0).contains(&due_in),
        "a soft problem's retry: {due_in}s"
    );
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a positive number of seconds below two minutes"
    )]
    let ticks = due_in.ceil() as u64 + 2;
    control.step_simulation(ticks);
    let after = kubelet();
    assert!(
        after.check.last_check.expect("checked") > last_check,
        "checked within {ticks} ticks"
    );
}
