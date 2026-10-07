//! The local event log end to end: what the engine writes (state changes,
//! acknowledgements, user comments, downtimes; no plain check results),
//! history per object, notifications with their read flag, persistence
//! across engines, pruning at start, hourly and on a new retention, and
//! deleting an environment's log.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers fail the test loudly"
)]

use std::time::Duration;

use ic_core::{Command, LogEntry, LogKind, delete_event_log, event_log_path};
use ic_mock::{MockConfig, MockControl, scenarios};
use ic_model::{CheckableState, ObjectKey, ServiceState, StateType, Timestamp};

use crate::support::{ENV_ID, Engine, Launch, mock};

async fn settled(engine: &mut Engine) {
    engine.connected().await;
    engine
        .snapshot(|snapshot| !snapshot.icinga_notifications.is_empty())
        .await;
}

/// Makes `service` on `host` critical and waits for its notification:
/// everything the engine applied before is logged by then (the log works
/// in order).
async fn fence(engine: &mut Engine, control: &MockControl, host: &str, service: &str) {
    control
        .set_service_state(
            host,
            service,
            ServiceState::Critical,
            "CRITICAL - fence",
            true,
        )
        .unwrap();
    let title = format!("CRITICAL · {service} on {host}");
    engine
        .wait_for(|event| match event {
            ic_core::CoreEvent::Notification(record) if record.intent.title == title => Some(()),
            _ => None,
        })
        .await;
}

fn kinds(entries: &[LogEntry]) -> Vec<LogKind> {
    entries.iter().rev().map(|entry| entry.kind).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn changes_are_logged_and_history_answers_per_object() {
    let server = mock(MockConfig::with_scenario(scenarios::staging())).await;
    let control = server.control();
    let mut engine = Launch::new(&server).start();
    settled(&mut engine).await;
    let http = ObjectKey::service("stg-api-01", "http");
    let critical = CheckableState::Service(ServiceState::Critical);

    control
        .set_service_state(
            "stg-api-01",
            "http",
            ServiceState::Critical,
            "CRITICAL - 503",
            true,
        )
        .unwrap();
    // A plain check result in the same state isn't logged.
    control
        .process_check_result(&http, 2, "CRITICAL - 503", &[])
        .unwrap();
    control
        .acknowledge(&http, "m.keller", "deploy in progress", false)
        .unwrap();
    control
        .add_comment(&http, "a.ivanova", "vendor ticket 4711")
        .unwrap();
    let now = control.now();
    let downtime = control
        .schedule_downtime(
            &http,
            "j.berg",
            "patching",
            now,
            now.plus(Duration::from_hours(1)),
            None,
        )
        .unwrap();
    control.run_timers();
    control.remove_downtime(&downtime).unwrap();
    control
        .set_service_state("stg-api-01", "http", ServiceState::Ok, "HTTP OK", true)
        .unwrap();
    fence(&mut engine, &control, "stg-web-01", "http").await;

    let history = engine.history(Some(http.clone())).await;
    assert!(
        history.windows(2).all(|pair| pair[0].at >= pair[1].at),
        "newest first"
    );
    assert_eq!(
        kinds(&history),
        [
            LogKind::State {
                state: critical,
                state_type: StateType::Soft
            },
            LogKind::State {
                state: critical,
                state_type: StateType::Hard
            },
            LogKind::AcknowledgementSet,
            LogKind::CommentAdded,
            LogKind::DowntimeStarted,
            LogKind::DowntimeEnded,
            // The recovery ends the acknowledgement.
            LogKind::State {
                state: CheckableState::Service(ServiceState::Ok),
                state_type: StateType::Hard
            },
            LogKind::AcknowledgementCleared,
        ],
        "{history:#?}"
    );
    let oldest = history.last().unwrap();
    assert_eq!(oldest.object, http);
    assert_eq!(oldest.text, "CRITICAL - 503");
    let ack = history
        .iter()
        .find(|entry| entry.kind == LogKind::AcknowledgementSet)
        .unwrap();
    assert_eq!(ack.author.as_deref(), Some("m.keller"));
    assert_eq!(ack.text, "deploy in progress");
    let comment = history
        .iter()
        .find(|entry| entry.kind == LogKind::CommentAdded)
        .unwrap();
    assert_eq!(comment.author.as_deref(), Some("a.ivanova"));
    let started = history
        .iter()
        .find(|entry| entry.kind == LogKind::DowntimeStarted)
        .unwrap();
    assert_eq!(started.author.as_deref(), Some("j.berg"));
    assert_eq!(started.text, "patching");

    // A host's history has its services'; everything has the fence too.
    let host = engine.history(Some(ObjectKey::host("stg-api-01"))).await;
    assert_eq!(host.len(), history.len());
    let all = engine.history(None).await;
    assert!(all.len() > history.len());
    assert_eq!(all[0].object, ObjectKey::service("stg-web-01", "http"));

    let path = event_log_path(engine.data_dir(), ENV_ID);
    assert!(path.exists(), "{}", path.display());
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_log_outlives_the_engine_until_pruned_or_deleted() {
    let server = mock(MockConfig::with_scenario(scenarios::staging())).await;
    let control = server.control();
    let dir = tempfile::tempdir().unwrap();
    let launch = |now: f64, retention: u32| {
        let mut launch = Launch::new(&server);
        launch.data_dir = Some(dir.path().to_owned());
        launch.now = now;
        launch.general.event_log_retention_hours = retention;
        launch
    };
    let now = Timestamp::now().as_unix_seconds();

    let mut engine = launch(now, 48).start();
    settled(&mut engine).await;
    fence(&mut engine, &control, "stg-web-01", "http").await;
    engine.shutdown();

    // A new engine on the same log: history and notifications are there.
    let mut engine = launch(now, 48).start();
    assert_eq!(engine.history(None).await.len(), 2, "soft and hard");
    let stored = engine.stored_notifications().await;
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].intent.title, "CRITICAL · http on stg-web-01");
    // A shorter retention prunes at once.
    settled(&mut engine).await;
    let general = ic_config::General {
        event_log_retention_hours: 1,
        ..ic_config::General::default()
    };
    engine.clock.advance(Duration::from_hours(2));
    engine.send(Command::UpdateGeneral(general));
    assert!(engine.history(None).await.is_empty());
    assert!(engine.stored_notifications().await.is_empty());
    fence(&mut engine, &control, "stg-web-02", "http").await;
    engine.shutdown();

    // Started three hours later with a retention of one hour: pruned
    // before anything can read it.
    let mut engine = launch(now + 2.0 * 3_600.0 + 3.0 * 3_600.0, 1).start();
    assert!(engine.history(None).await.is_empty());
    assert!(engine.stored_notifications().await.is_empty());
    engine.shutdown();

    // Deleting the environment deletes its log.
    let path = event_log_path(dir.path(), ENV_ID);
    assert!(path.exists());
    delete_event_log(dir.path(), ENV_ID).unwrap();
    assert!(!path.exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_log_is_pruned_every_interval() {
    let server = mock(MockConfig::with_scenario(scenarios::staging())).await;
    let control = server.control();
    let mut launch = Launch::new(&server);
    launch.tuning.prune_interval = Duration::from_millis(50);
    launch.general.event_log_retention_hours = 1;
    let mut engine = launch.start();
    settled(&mut engine).await;
    fence(&mut engine, &control, "stg-web-01", "http").await;
    assert_eq!(engine.history(None).await.len(), 2);
    engine.clock.advance(Duration::from_mins(61));
    let mut empty = false;
    for _ in 0..200 {
        if engine.history(None).await.is_empty() {
            empty = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(empty, "pruned within a few intervals");
    assert!(engine.stored_notifications().await.is_empty());
    engine.shutdown();
}
