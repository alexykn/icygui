//! Engines whose environment isn't on screen (`Command::SetActive(false)`;
//! the app runs one engine per environment): fewer snapshots while only
//! check results arrive, notifications as prompt as ever, what changed at
//! once when the environment comes back on screen. Several engines side by
//! side, each against its own mock and with its own event log in one data
//! directory.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers fail the test loudly"
)]

use std::time::{Duration, Instant};

use futures::StreamExt as _;
use ic_core::{Command, CoreEvent};
use ic_mock::{MockConfig, MockControl, scenarios};
use ic_model::{ObjectKey, ServiceState};

use crate::support::{Engine, FakeSecrets, Launch, PASSWORD, mock};

/// The background interval the tests use: far above anything an active
/// engine or a notification takes.
const BACKGROUND: Duration = Duration::from_secs(3);

/// Collects the engine's events for `duration`.
async fn drain_for(engine: &mut Engine, duration: Duration) {
    let until = tokio::time::Instant::now() + duration;
    while let Ok(Some(event)) = tokio::time::timeout_at(until, engine.events.next()).await {
        engine.seen.push(event);
    }
}

/// How many snapshots the engine published since event `from`.
fn snapshots_since(engine: &Engine, from: usize) -> usize {
    engine.seen[from..]
        .iter()
        .filter(|event| matches!(event, CoreEvent::Snapshot(_)))
        .count()
}

/// An OK service whose checks change nothing: (host, service).
fn quiet_service(engine: &Engine) -> ObjectKey {
    let snapshot = engine.latest().unwrap();
    snapshot
        .services
        .values()
        .find(|service| {
            service.state == ServiceState::Ok
                && snapshot
                    .hosts
                    .get(&service.key.host)
                    .is_some_and(|host| host.state == ic_model::HostState::Up)
        })
        .map(|service| ObjectKey::Service {
            key: service.key.clone(),
        })
        .expect("an OK service on an UP host")
}

/// Sends OK check results for `object` every 50 ms for `duration` while
/// collecting the engine's events.
async fn check_results_for(
    engine: &mut Engine,
    control: &MockControl,
    object: &ObjectKey,
    duration: Duration,
) {
    let started = Instant::now();
    while started.elapsed() < duration {
        control
            .process_check_result(object, 0, "OK - nothing new", &[])
            .unwrap();
        drain_for(engine, Duration::from_millis(50)).await;
    }
}

fn launch(server: &ic_mock::MockServer) -> Launch {
    let mut launch = Launch::new(server);
    launch.tuning.background_publish_interval = BACKGROUND;
    launch
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_inactive_engine_publishes_less_but_notifies_at_once() {
    let server = mock(MockConfig::with_scenario(scenarios::staging())).await;
    let control = server.control();
    let mut engine = launch(&server).start();
    engine.connected().await;
    let object = quiet_service(&engine);

    // On screen: check results come out at the normal pace (20 ms here).
    let from = engine.seen.len();
    check_results_for(&mut engine, &control, &object, Duration::from_secs(1)).await;
    let active = snapshots_since(&engine, from);
    assert!(active >= 5, "an active engine publishes often: {active}");

    // Off screen: the same changes go out at most every 3 s.
    engine.send(Command::SetActive(false));
    drain_for(&mut engine, Duration::from_millis(100)).await;
    let from = engine.seen.len();
    check_results_for(&mut engine, &control, &object, Duration::from_secs(2)).await;
    let inactive = snapshots_since(&engine, from);
    assert!(
        inactive <= 1,
        "an inactive engine publishes at most every {BACKGROUND:?}: {inactive}"
    );

    // A problem notifies as promptly as ever: its rule input doesn't wait
    // for the background interval.
    let ObjectKey::Service { key } = &object else {
        unreachable!()
    };
    let changed = Instant::now();
    control
        .set_service_state(
            key.host.as_str(),
            &key.name,
            ServiceState::Critical,
            "CRITICAL - broken",
            true,
        )
        .unwrap();
    let record = engine.notification().await;
    let took = changed.elapsed();
    assert!(record.intent.title.starts_with("CRITICAL"), "{record:?}");
    assert!(!record.intent.silent);
    assert!(
        took < BACKGROUND,
        "an inactive environment notifies at once: {took:?}"
    );
    assert_eq!(engine.shown().len(), 1, "it reached the desktop");

    // Back on screen: what changed goes out at once.
    drain_for(&mut engine, Duration::from_millis(100)).await;
    control
        .process_check_result(&object, 2, "CRITICAL - still broken", &[])
        .unwrap();
    drain_for(&mut engine, Duration::from_millis(200)).await;
    let from = engine.seen.len();
    let shown = Instant::now();
    engine.send(Command::SetActive(true));
    engine
        .snapshot(|snapshot| {
            snapshot
                .services
                .get(key)
                .and_then(|service| service.check.result.as_ref())
                .is_some_and(|result| result.output.contains("still broken"))
        })
        .await;
    assert!(
        shown.elapsed() < BACKGROUND,
        "the snapshot came at once: {:?}",
        shown.elapsed()
    );
    assert!(snapshots_since(&engine, from) >= 1);
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn engines_side_by_side_notify_each_into_their_own_log() {
    let production = mock(MockConfig::with_scenario(scenarios::staging())).await;
    let staging = mock(MockConfig::with_scenario(scenarios::staging())).await;
    let data_dir = tempfile::tempdir().unwrap();

    let mut first = launch(&production);
    first.data_dir = Some(data_dir.path().to_owned());
    let mut second = launch(&staging);
    second.environment.id = "staging-environment".to_owned();
    second.environment.name = "staging".to_owned();
    second.secrets = FakeSecrets::with("staging-environment", PASSWORD);
    second.data_dir = Some(data_dir.path().to_owned());
    let mut first = first.start();
    let mut second = second.start();
    first.connected().await;
    second.connected().await;
    // The app shows the first; the second runs in the background.
    second.send(Command::SetActive(false));

    for (engine, server) in [(&mut first, &production), (&mut second, &staging)] {
        let object = quiet_service(engine);
        let ObjectKey::Service { key } = &object else {
            unreachable!()
        };
        server
            .control()
            .set_service_state(
                key.host.as_str(),
                &key.name,
                ServiceState::Critical,
                "CRITICAL - broken",
                true,
            )
            .unwrap();
        let record = engine.notification().await;
        assert_eq!(record.intent.object.as_ref(), Some(&object));
    }
    // Each engine logged its own notification, in its own file.
    let first_log = first.stored_notifications().await;
    let second_log = second.stored_notifications().await;
    assert_eq!(first_log.len(), 1, "{first_log:?}");
    assert_eq!(second_log.len(), 1, "{second_log:?}");
    assert_ne!(first_log[0].intent.id, second_log[0].intent.id);
    let first_path = ic_core::event_log_path(data_dir.path(), crate::support::ENV_ID);
    let second_path = ic_core::event_log_path(data_dir.path(), "staging-environment");
    assert!(first_path.exists() && second_path.exists());

    // Deleting the second environment: its engine stops, then its log
    // goes; the first keeps running and keeps its log.
    second.shutdown();
    ic_core::delete_event_log(data_dir.path(), "staging-environment").unwrap();
    assert!(!second_path.exists());
    assert!(first_path.exists());
    assert_eq!(first.stored_notifications().await.len(), 1);
    // One stream per environment, no more.
    assert_eq!(production.control().event_streams(), 1);
    first.shutdown();
}
