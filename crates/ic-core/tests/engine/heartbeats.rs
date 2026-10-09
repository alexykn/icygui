//! Heartbeats and trouble alerts (PLAN.md §4.2 A, B, B2, B3, E) end to end
//! against `ic-mock` with heartbeats in real time: found and left out of
//! everything else, carried by the quiet stream's filter (or read once per
//! interval when the API user may not filter), a checker that stops (one
//! alert: *Icinga runs no checks*), a stream that stalls (a reconnect, no
//! alert), an endpoint that goes away with its pinned beat (one line), no
//! live data (blind and live again), a heartbeat that disappears (until its
//! removal is confirmed), and a pause holding the alerts back.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers fail the test loudly"
)]

use std::path::PathBuf;
use std::time::Duration;

use ic_core::heartbeat::{BeatState, Death, Timing};
use ic_core::snapshot::Snapshot;
use ic_core::{Command, ConnectionState, Tuning};
use ic_mock::{MockConfig, MockServer, MockUser, scenarios};
use ic_model::{ServiceKey, Timestamp};
use ic_rules::Tone;

use crate::support::{Engine, Launch, mock, wait_until};

/// The beats every 0.3 s; prod-cluster has five (master, its two
/// endpoints, ams, fra).
const BEATS: usize = 5;

async fn server_with(users: Vec<MockUser>, heartbeats: bool) -> MockServer {
    let scenario = if heartbeats {
        scenarios::prod_cluster().with_heartbeats(0.3)
    } else {
        scenarios::prod_cluster()
    };
    mock(MockConfig {
        housekeeping_interval: Duration::from_millis(50),
        users,
        ..MockConfig::with_scenario(scenario)
    })
    .await
}

async fn server() -> MockServer {
    server_with(vec![MockUser::root()], true).await
}

/// Short budgets: beats watched at their 0.3 s, late 0.15 s after it, the
/// query 0.15 s after that; alerts after 0.3 s.
fn tuning() -> Tuning {
    Tuning {
        status_interval: Duration::from_millis(200),
        heartbeat: Timing {
            min_interval: Duration::from_millis(200),
            min_allowance: Duration::from_millis(150),
            query_after: Duration::from_millis(150),
            recheck_after: Duration::from_millis(100),
        },
        trouble_grace: Duration::from_millis(300),
        ..crate::support::tuning()
    }
}

fn launch(server: &MockServer) -> Launch {
    Launch {
        tuning: tuning(),
        ..Launch::new(server)
    }
}

fn on_time(snapshot: &Snapshot) -> usize {
    snapshot
        .heartbeats
        .beats
        .iter()
        .filter(|beat| beat.state == BeatState::OnTime)
        .count()
}

/// Waits until every beat came in time.
async fn beating(engine: &mut Engine) -> std::sync::Arc<Snapshot> {
    engine.snapshot(|snapshot| on_time(snapshot) == BEATS).await
}

/// Waits for a notification whose title is `title`; returns its tone.
async fn notified(engine: &mut Engine, title: &str) -> Tone {
    let title = title.to_owned();
    engine
        .wait_for(|event| match event {
            ic_core::CoreEvent::Notification(record) if record.intent.title == title => {
                Some(record.intent.tone)
            }
            _ => None,
        })
        .await
}

fn titles(engine: &Engine) -> Vec<String> {
    engine
        .notifications()
        .iter()
        .map(|record| record.intent.title.clone())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn heartbeats_are_found_watched_and_left_out_of_everything_else() {
    let server = server().await;
    let mut engine = launch(&server).start();
    engine.connected().await;
    let snapshot = beating(&mut engine).await;
    let beats = &snapshot.heartbeats;
    let objects: Vec<String> = beats
        .beats
        .iter()
        .map(ic_core::heartbeat::Heartbeat::object)
        .collect();
    assert_eq!(
        objects,
        [
            "icygui-hb-master!beat",
            "icygui-hb-master!beat-master-01",
            "icygui-hb-master!beat-master-02",
            "icygui-hb-ams!beat",
            "icygui-hb-fra!beat",
        ],
        "the top-level zone first, its own beat before its endpoints'"
    );
    assert_eq!(beats.beats[1].proves.label(), "master-01");
    assert_eq!(beats.beats[3].proves.label(), "zone ams");
    assert!(!beats.polled);
    // Left out of lists, counts and the watchdog; the store keeps them.
    for beat in &beats.beats {
        assert!(snapshot.excluded.contains(&beat.key));
        assert!(snapshot.services.contains_key(&beat.key));
        assert!(
            !snapshot
                .late
                .contains_key(&ic_model::ObjectKey::from(beat.key.clone()))
        );
    }
    assert!(snapshot.trouble.alerts.is_empty(), "{:?}", snapshot.trouble);
    assert!(snapshot.trouble.blind.is_none());

    // Quiet: the stream's filter names the beats, which keep coming.
    engine.send(Command::SetQuiet(true));
    let control = server.control();
    assert!(
        wait_until(|| control
            .event_stream_stats()
            .iter()
            .any(|stream| stream.filtered && stream.types.contains(&"CheckResult")))
        .await
    );
    // (Icinga's clock: the engine's own stands still in these tests.)
    let before = engine.latest().unwrap().heartbeats.beats[0].last_check;
    let snapshot = engine
        .snapshot(|snapshot| {
            snapshot.quiet
                && snapshot.heartbeats.beats[0].last_check > before
                && on_time(snapshot) == BEATS
        })
        .await;
    assert!(!snapshot.heartbeats.polled);
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_user_who_may_not_filter_reads_the_beats_once_per_interval_while_quiet() {
    let user = MockUser::new(
        "root",
        "icinga",
        &["objects/query/*", "status/query", "events/*"],
    );
    let server = server_with(vec![user], true).await;
    let mut engine = launch(&server).start();
    engine.connected().await;
    beating(&mut engine).await;
    engine.send(Command::SetQuiet(true));
    let snapshot = engine
        .snapshot(|snapshot| snapshot.quiet && snapshot.heartbeats.polled)
        .await;
    let before = snapshot.heartbeats.beats[0].last_check;
    // Polls bring the beats: still on time, a moment later.
    engine
        .snapshot(|snapshot| {
            snapshot.heartbeats.beats[0].last_check > before && on_time(snapshot) == BEATS
        })
        .await;
    let control = server.control();
    let polls = control
        .requests()
        .iter()
        .filter(|request| {
            request.path.starts_with("/v1/objects/services")
                && request
                    .body
                    .as_ref()
                    .is_some_and(|body| body.to_string().contains("icygui-hb-"))
        })
        .count();
    assert!(polls >= 1, "the beats are read by name");
    assert!(engine.notifications().is_empty(), "{:?}", titles(&engine));
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_checker_that_stops_is_one_alert_and_one_recovery() {
    let server = server().await;
    let mut engine = launch(&server).start();
    engine.connected().await;
    beating(&mut engine).await;
    let control = server.control();
    control.stop_checks();
    let tone = notified(&mut engine, "test: Icinga runs no checks").await;
    assert_eq!(tone, Tone::Critical);
    let snapshot = engine
        .snapshot(|snapshot| !snapshot.trouble.alerts.is_empty())
        .await;
    assert_eq!(
        snapshot.trouble.alerts.len(),
        1,
        "{:?}",
        snapshot.trouble.alerts
    );
    assert!(
        snapshot.trouble.alerts[0]
            .title
            .starts_with("Icinga runs no checks: none since "),
        "{}",
        snapshot.trouble.alerts[0].title
    );
    assert!(
        snapshot
            .heartbeats
            .beats
            .iter()
            .all(|beat| beat.state == BeatState::Dead(Death::Stopped))
    );
    // Checks again: the beats come back, the alert clears with one
    // notification.
    control.start_checks();
    let tone = notified(&mut engine, "test: Icinga runs checks again").await;
    assert_eq!(tone, Tone::Recovery);
    engine
        .snapshot(|snapshot| snapshot.trouble.alerts.is_empty() && on_time(snapshot) == BEATS)
        .await;
    let titles = titles(&engine);
    assert_eq!(
        titles,
        [
            "test: Icinga runs no checks",
            "test: Icinga runs checks again"
        ],
        "one cause, one line"
    );
    // Always to the desktop: no rule needed.
    assert_eq!(engine.shown(), titles);
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stream_that_stalls_reconnects_without_an_alert() {
    let server = server().await;
    let mut engine = Launch {
        tuning: Tuning {
            trouble_grace: Duration::from_secs(5),
            ..tuning()
        },
        ..Launch::new(&server)
    }
    .start();
    engine.connected().await;
    beating(&mut engine).await;
    server.control().stall_event_streams();
    // The beats run but don't arrive: a query finds them fresh, looks
    // again, and the engine reconnects.
    let state = engine
        .wait_state(|state| matches!(state, ConnectionState::Reconnecting { .. }))
        .await;
    let ConnectionState::Reconnecting { error, .. } = state else {
        unreachable!()
    };
    assert!(error.contains("heartbeat"), "{error}");
    engine.connected().await;
    beating(&mut engine).await;
    assert!(engine.notifications().is_empty(), "{:?}", titles(&engine));
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_endpoint_that_goes_away_with_its_beat_is_one_line() {
    let server = server().await;
    let mut engine = launch(&server).start();
    engine.connected().await;
    beating(&mut engine).await;
    server
        .control()
        .set_endpoint_connected("master-02", false)
        .unwrap();
    let snapshot = engine
        .snapshot(|snapshot| !snapshot.trouble.alerts.is_empty())
        .await;
    let alerts = &snapshot.trouble.alerts;
    assert_eq!(alerts.len(), 1, "{alerts:?}");
    assert_eq!(
        alerts[0].title,
        "heartbeat master-02 dead: Remote Icinga instance 'master-02' is not connected"
    );
    assert!(
        alerts[0].detail.contains("pinned to master-02"),
        "{}",
        alerts[0].detail
    );
    let pinned = snapshot
        .heartbeats
        .beats
        .iter()
        .find(|beat| beat.key == ServiceKey::new("icygui-hb-master", "beat-master-02"))
        .unwrap();
    assert_eq!(pinned.state, BeatState::Dead(Death::NotOk));
    notified(&mut engine, "test: master-02 disconnected").await;
    // Back: one recovery.
    server
        .control()
        .set_endpoint_connected("master-02", true)
        .unwrap();
    notified(&mut engine, "test: master-02 connected again").await;
    assert_eq!(
        titles(&engine),
        [
            "test: master-02 disconnected",
            "test: master-02 connected again"
        ]
    );
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_live_data_is_blind_after_its_grace_and_live_again_after() {
    let server = server().await;
    let mut engine = launch(&server).start();
    engine.connected().await;
    let control = server.control();
    // Every request fails for a while: the stream breaks, reconnects fail.
    control.fail_next(10_000, 503);
    control.drop_event_streams();
    let tone = notified(&mut engine, "no live data from test").await;
    assert_eq!(tone, Tone::Warning);
    let record = engine.notifications().last().cloned().unwrap();
    assert!(
        record.intent.body.starts_with("since "),
        "{}",
        record.intent.body
    );
    assert!(
        record.intent.body.ends_with(" · connection lost"),
        "{}",
        record.intent.body
    );
    let snapshot = engine
        .snapshot(|snapshot| snapshot.trouble.blind.is_some())
        .await;
    assert_eq!(
        snapshot.trouble.blind.as_ref().unwrap().reason,
        "connection lost"
    );
    control.fail_next(0, 503);
    notified(&mut engine, "test live again").await;
    engine
        .snapshot(|snapshot| snapshot.trouble.blind.is_none())
        .await;
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pause_holds_trouble_alerts_back() {
    let server = server().await;
    let mut engine = launch(&server).start();
    engine.connected().await;
    beating(&mut engine).await;
    let until = Timestamp::from_unix_seconds(Timestamp::now().as_unix_seconds() + 3_600.0);
    engine.send(Command::PauseNotifications(Some(until)));
    server.control().stop_checks();
    let record = engine
        .wait_for(|event| match event {
            ic_core::CoreEvent::Notification(record)
                if record.intent.title == "test: Icinga runs no checks" =>
            {
                Some(record.clone())
            }
            _ => None,
        })
        .await;
    assert!(record.intent.silent, "recorded, not shown");
    assert_eq!(record.intent.silenced, Some(ic_rules::Silence::Paused));
    assert!(engine.shown().is_empty(), "{:?}", engine.shown());
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_heartbeat_that_disappears_is_a_finding_until_its_removal_is_confirmed() {
    let dir = tempfile::tempdir().unwrap();
    let data_dir: PathBuf = dir.path().to_owned();
    // A first run sees the beats (and remembers them).
    {
        let server = server().await;
        let mut engine = Launch {
            data_dir: Some(data_dir.clone()),
            ..launch(&server)
        }
        .start();
        engine.connected().await;
        beating(&mut engine).await;
        engine.shutdown();
    }
    // The next run's Icinga has none.
    let server = server_with(vec![MockUser::root()], false).await;
    let mut engine = Launch {
        data_dir: Some(data_dir),
        ..launch(&server)
    }
    .start();
    engine.connected().await;
    let snapshot = engine
        .snapshot(|snapshot| {
            snapshot
                .heartbeats
                .beats
                .iter()
                .filter(|beat| beat.state == BeatState::Disappeared)
                .count()
                == BEATS
        })
        .await;
    assert!(snapshot.excluded.is_empty());
    let snapshot = engine
        .snapshot(|snapshot| snapshot.trouble.alerts.len() == BEATS)
        .await;
    assert!(
        snapshot
            .trouble
            .alerts
            .iter()
            .any(|alert| alert.title.starts_with("heartbeat fra disappeared since ")),
        "{:?}",
        snapshot.trouble.alerts
    );
    notified(&mut engine, "test: heartbeat fra disappeared").await;
    // Confirmed: forgotten, its finding ends without a notification.
    let before = engine.notifications().len();
    engine.send(Command::ConfirmHeartbeatRemoval(ServiceKey::new(
        "icygui-hb-fra",
        "beat",
    )));
    let snapshot = engine
        .snapshot(|snapshot| snapshot.heartbeats.beats.len() == BEATS - 1)
        .await;
    assert!(
        !snapshot
            .trouble
            .alerts
            .iter()
            .any(|alert| alert.title.contains("fra")),
        "{:?}",
        snapshot.trouble.alerts
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    let after: Vec<String> = engine.notifications()[before..]
        .iter()
        .map(|record| record.intent.title.clone())
        .collect();
    assert!(
        !after.iter().any(|title| title.contains("fra")),
        "{after:?}"
    );
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn beats_the_settings_no_longer_ask_for_are_forgotten_not_findings() {
    let server = server().await;
    let mut engine = launch(&server).start();
    engine.connected().await;
    beating(&mut engine).await;
    // Listed by name now: only master-01's beat.
    let mut environment = crate::support::environment(&server);
    environment.trouble.heartbeats.mode = ic_config::HeartbeatMode::List;
    environment.trouble.heartbeats.list = vec!["icygui-hb-master!beat-master-01".to_owned()];
    engine.send(Command::UpdateEnvironment(environment));
    let snapshot = engine
        .snapshot(|snapshot| snapshot.heartbeats.beats.len() == 1)
        .await;
    assert_eq!(
        snapshot.heartbeats.beats[0].key,
        ServiceKey::new("icygui-hb-master", "beat-master-01")
    );
    assert!(
        snapshot
            .heartbeats
            .beats
            .iter()
            .all(|beat| beat.state != BeatState::Disappeared)
    );
    // Nothing is raised for the beats left out, through the grace.
    tokio::time::sleep(Duration::from_millis(900)).await;
    let snapshot = engine.snapshot(|_| true).await;
    assert!(
        snapshot.trouble.alerts.is_empty(),
        "{:?}",
        snapshot.trouble.alerts
    );
    assert!(
        titles(&engine)
            .iter()
            .all(|title| !title.contains("disappeared")),
        "{:?}",
        titles(&engine)
    );
    assert_eq!(snapshot.excluded.len(), 1);
    engine.shutdown();
}
