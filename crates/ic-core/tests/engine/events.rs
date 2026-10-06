//! The live event stream: every event type reaches the snapshot without a
//! re-query, config changes and unknown objects are re-queried by name
//! (batched, deduplicated, remembered when missing), events during the
//! initial load are kept, and bursts are absorbed.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers fail the test loudly"
)]

use std::time::Duration;

use crate::support::{Launch, mock, start_for, wait_until};
use ic_core::snapshot::Snapshot;
use ic_core::{ConnectionState, LoadPhase};
use ic_mock::{MockConfig, MockControl, scenarios};
use ic_model::{
    AckKind, CommentKind, HostState, ObjectKey, ServiceKey, ServiceState, StateType, Timestamp,
};
use serde_json::{Value, json};

fn service<'a>(snapshot: &'a Snapshot, host: &str, name: &str) -> &'a ic_model::Service {
    &snapshot.services[&ServiceKey::new(host, name)]
}

/// The names of every object re-query (`hosts`/`services` lists) so far.
fn requeried(control: &MockControl) -> Vec<(String, Vec<String>)> {
    control
        .requests()
        .iter()
        .filter_map(|request| {
            let body = request.body.as_ref()?;
            ["hosts", "services"].into_iter().find_map(|kind| {
                let names = body.get(kind)?.as_array()?;
                Some((
                    request.path.clone(),
                    names
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect(),
                ))
            })
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[expect(clippy::too_many_lines, reason = "one end-to-end scenario")]
async fn every_event_type_reaches_the_snapshot() {
    let server = mock(MockConfig::with_scenario(scenarios::lab())).await;
    let control = server.control();
    // Without notifications: a shown notification prefetches its object
    // (one by-name request, `quiet.rs`), which isn't what this checks.
    let mut launch = Launch::new(&server);
    launch.environment.notifications.enabled = false;
    let mut engine = launch.start();
    engine.connected().await;
    let ssh = ObjectKey::service("lab-01", "ssh");
    control.clear_requests();

    // CheckResult and StateChange: soft, then hard.
    control
        .set_service_state(
            "lab-01",
            "ssh",
            ServiceState::Critical,
            "SSH CRITICAL - refused",
            false,
        )
        .unwrap();
    let snapshot = engine
        .snapshot(|snapshot| service(snapshot, "lab-01", "ssh").state == ServiceState::Critical)
        .await;
    let stored = service(&snapshot, "lab-01", "ssh");
    assert_eq!(stored.check.state_type, StateType::Soft);
    assert_eq!(stored.check.attempt, 1);
    assert_eq!(stored.check.output(), "SSH CRITICAL - refused");
    assert!(snapshot.last_event_at.is_some());
    control
        .set_service_state(
            "lab-01",
            "ssh",
            ServiceState::Critical,
            "SSH CRITICAL - refused",
            true,
        )
        .unwrap();
    let snapshot = engine
        .snapshot(|snapshot| service(snapshot, "lab-01", "ssh").check.state_type == StateType::Hard)
        .await;
    let stored = service(&snapshot, "lab-01", "ssh");
    let truth = control.service("lab-01", "ssh").unwrap();
    assert_eq!(stored.state, truth.state);
    assert_eq!(stored.check.attempt, truth.check.attempt);
    // The same instants, up to the JSON's float precision.
    let same =
        |a: Timestamp, b: Timestamp| (a.as_unix_seconds() - b.as_unix_seconds()).abs() < 1e-3;
    assert!(same(
        stored.check.last_state_change,
        truth.check.last_state_change
    ));
    assert!(same(
        stored.check.last_hard_state_change,
        truth.check.last_hard_state_change
    ));
    assert!(stored.check.next_check.is_some());

    // Acknowledgement set (with its comment) and cleared.
    control
        .acknowledge(&ssh, "alice", "looking into it", true)
        .unwrap();
    let snapshot = engine
        .snapshot(|snapshot| {
            service(snapshot, "lab-01", "ssh").check.acknowledgement == AckKind::Sticky
                && snapshot.comments.contains_key(&ssh)
        })
        .await;
    let comment = &snapshot.comments[&ssh][0];
    assert_eq!(comment.kind, CommentKind::Acknowledgement);
    assert_eq!(comment.author, "alice");
    assert_eq!(snapshot.overall.handled, 1);
    control.remove_acknowledgement(&ssh).unwrap();
    engine
        .snapshot(|snapshot| {
            service(snapshot, "lab-01", "ssh").check.acknowledgement == AckKind::None
                && !snapshot.comments.contains_key(&ssh)
        })
        .await;

    // A user comment.
    let name = control.add_comment(&ssh, "bob", "rebooted").unwrap();
    engine
        .snapshot(|snapshot| {
            snapshot.comments.get(&ssh).is_some_and(|comments| {
                comments
                    .iter()
                    .any(|c| c.name == name && c.text == "rebooted")
            })
        })
        .await;

    // A fixed downtime in its window: added, started, triggered; then
    // removed.
    let now = control.now();
    let downtime = control
        .schedule_downtime(
            &ssh,
            "carol",
            "maintenance",
            Timestamp::from_unix_seconds(now.as_unix_seconds() - 60.0),
            Timestamp::from_unix_seconds(now.as_unix_seconds() + 3_600.0),
            None,
        )
        .unwrap();
    let snapshot = engine
        .snapshot(|snapshot| service(snapshot, "lab-01", "ssh").check.downtime_depth == 1)
        .await;
    let stored = &snapshot.downtimes[&ssh];
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].name, downtime);
    assert!(stored[0].in_effect);
    control.remove_downtime(&downtime).unwrap();
    engine
        .snapshot(|snapshot| {
            service(snapshot, "lab-01", "ssh").check.downtime_depth == 0
                && !snapshot.downtimes.contains_key(&ssh)
        })
        .await;

    // Flapping.
    control.emit_raw(json!({
        "type": "Flapping",
        "timestamp": now.as_unix_seconds(),
        "host": "lab-01",
        "service": "ssh",
        "is_flapping": true,
        "flapping_current": 55.5,
        "state": 2,
        "state_type": 1,
    }));
    engine
        .snapshot(|snapshot| service(snapshot, "lab-01", "ssh").check.flapping)
        .await;

    // A host going down handles its services' problems.
    control
        .set_host_state("lab-01", HostState::Down, "PING CRITICAL - 100% loss", true)
        .unwrap();
    let snapshot = engine
        .snapshot(|snapshot| snapshot.hosts[&"lab-01".into()].state == HostState::Down)
        .await;
    assert_eq!(snapshot.overall.down, 1);
    assert_eq!(snapshot.overall.unhandled, 1, "only the host");
    assert_eq!(
        snapshot.overall.handled, 1,
        "the service is handled by its host"
    );

    // Recovery.
    control
        .set_service_state("lab-01", "ssh", ServiceState::Ok, "SSH OK", true)
        .unwrap();
    control
        .set_host_state("lab-01", HostState::Up, "PING OK", true)
        .unwrap();
    let snapshot = engine
        .snapshot(|snapshot| {
            snapshot.hosts[&"lab-01".into()].state == HostState::Up
                && service(snapshot, "lab-01", "ssh").state == ServiceState::Ok
        })
        .await;
    assert_eq!(snapshot.overall.unhandled, 0);

    // None of it needed a re-query.
    assert!(requeried(&control).is_empty(), "{:?}", requeried(&control));
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_changes_and_unknown_objects_are_requeried_by_name() {
    let server = mock(MockConfig::with_scenario(scenarios::lab())).await;
    let control = server.control();
    let mut engine = start_for(&server);
    engine.connected().await;
    control.clear_requests();

    // Modified objects: one round, by name, deduplicated.
    control.touch_object("Service", "lab-01!disk /").unwrap();
    control.touch_object("Service", "lab-01!disk /").unwrap();
    control.touch_object("Host", "lab-01").unwrap();
    assert!(wait_until(|| requeried(&control).len() >= 2).await);
    let rounds = requeried(&control);
    assert!(
        rounds.contains(&(
            "/v1/objects/services".to_owned(),
            vec!["lab-01!disk /".to_owned()]
        )),
        "{rounds:?}"
    );
    assert!(
        rounds.contains(&("/v1/objects/hosts".to_owned(), vec!["lab-01".to_owned()])),
        "{rounds:?}"
    );

    // An event about an object the client doesn't know: re-queried once;
    // Icinga doesn't know it either, so later events don't re-query it.
    control.clear_requests();
    let ghost = |output: &str| {
        json!({
            "type": "CheckResult",
            "timestamp": 1.0,
            "host": "lab-01",
            "service": "ghost",
            "check_result": {
                "state": 2,
                "output": output,
                "vars_after": {"state": 2, "state_type": 1, "attempt": 1, "reachable": true},
            },
        })
    };
    control.emit_raw(ghost("first"));
    assert!(wait_until(|| !requeried(&control).is_empty()).await);
    assert_eq!(
        requeried(&control),
        [(
            "/v1/objects/services".to_owned(),
            vec!["lab-01!ghost".to_owned()]
        )]
    );
    control.emit_raw(ghost("second"));
    // A barrier: a known object's change re-queries alone.
    control.touch_object("Service", "lab-01!ssh").unwrap();
    assert!(wait_until(|| requeried(&control).len() >= 2).await);
    assert_eq!(
        requeried(&control)[1],
        (
            "/v1/objects/services".to_owned(),
            vec!["lab-01!ssh".to_owned()]
        )
    );

    // Created again: re-queried even though it was missing.
    control.emit_raw(json!({
        "type": "ObjectCreated",
        "timestamp": 2.0,
        "object_type": "Service",
        "object_name": "lab-01!ghost",
    }));
    assert!(wait_until(|| requeried(&control).len() >= 3).await);
    assert_eq!(requeried(&control)[2].1, ["lab-01!ghost"]);

    // Comments and downtimes have their own events; groups are reloaded.
    control.clear_requests();
    control.emit_raw(json!({
        "type": "ObjectCreated",
        "timestamp": 3.0,
        "object_type": "Comment",
        "object_name": "lab-01!ssh!c1",
    }));
    control.touch_object("HostGroup", "linux-servers").unwrap();
    assert!(
        wait_until(|| control
            .requests()
            .iter()
            .any(|request| request.path == "/v1/objects/hostgroups"))
        .await
    );
    assert!(requeried(&control).is_empty());
    assert!(
        !control
            .requests()
            .iter()
            .any(|request| request.path == "/v1/objects/comments")
    );
    // Deleted objects are re-queried too (and kept while Icinga still
    // has them).
    control.emit_raw(json!({
        "type": "ObjectDeleted",
        "timestamp": 4.0,
        "object_type": "Service",
        "object_name": "lab-01!load",
    }));
    assert!(wait_until(|| !requeried(&control).is_empty()).await);
    let snapshot = engine.latest().unwrap();
    assert!(
        snapshot
            .services
            .contains_key(&ServiceKey::new("lab-01", "load"))
    );
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn events_during_the_initial_load_are_kept() {
    let server = mock(MockConfig::with_scenario(scenarios::staging())).await;
    let control = server.control();
    // Slow answers stretch the load.
    control.set_latency(Duration::from_millis(150));
    let mut engine = start_for(&server);
    engine
        .wait_state(|state| {
            matches!(
                state,
                ConnectionState::Loading {
                    phase: LoadPhase::Details,
                    ..
                }
            )
        })
        .await;
    // The services are loaded; this change only reaches the client as
    // events, which wait while the details load.
    control
        .set_service_state(
            "stg-web-01",
            "ssh",
            ServiceState::Critical,
            "SSH CRITICAL",
            true,
        )
        .unwrap();
    control.set_latency(Duration::ZERO);
    engine.connected().await;
    let snapshot = engine
        .snapshot(|snapshot| service(snapshot, "stg-web-01", "ssh").state == ServiceState::Critical)
        .await;
    let stored = service(&snapshot, "stg-web-01", "ssh");
    assert_eq!(stored.check.state_type, StateType::Hard);
    assert_eq!(stored.check.output(), "SSH CRITICAL");
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_burst_is_absorbed_and_the_store_matches_icinga() {
    let server = mock(MockConfig::with_scenario(scenarios::large_with_hosts(
        100, 3,
    )))
    .await;
    let control = server.control();
    let mut engine = start_for(&server);
    engine.connected().await;
    let before = engine.latest().unwrap();
    let lean = before
        .services
        .values()
        .filter(|service| service.check.result.is_none())
        .count();
    assert!(lean > 1_000, "OK services load lean: {lean}");
    control.clear_requests();

    // Re-check everything at Icinga's pace: every object gets a result.
    let queued = control.burst();
    assert_eq!(queued, 1_600);
    assert!(
        control
            .wait_for_queued_checks(Duration::from_secs(30))
            .await
    );
    let snapshot = engine
        .snapshot(|snapshot| {
            snapshot
                .services
                .values()
                .all(|service| service.check.result.is_some())
        })
        .await;
    for truth in control.services() {
        let stored = &snapshot.services[&truth.key];
        assert_eq!(stored.state, truth.state, "{}", truth.key);
        assert_eq!(
            stored.check.state_type, truth.check.state_type,
            "{}",
            truth.key
        );
        assert_eq!(stored.check.output(), truth.check.output(), "{}", truth.key);
    }
    for truth in control.hosts() {
        assert_eq!(snapshot.hosts[&truth.name].state, truth.state);
    }
    // No re-query for any of it.
    assert!(requeried(&control).is_empty());
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn snapshots_are_throttled_while_events_pour_in() {
    let server = mock(MockConfig::with_scenario(scenarios::large_with_hosts(
        100, 5,
    )))
    .await;
    let control = server.control();
    let interval = Duration::from_millis(200);
    let mut engine = crate::support::start(
        crate::support::environment(&server),
        crate::support::FakeSecrets::with(crate::support::ENV_ID, crate::support::PASSWORD),
        ic_core::Tuning {
            publish_interval: interval,
            ..crate::support::tuning()
        },
    );
    engine.connected().await;
    let before = engine.latest().unwrap().revision;
    // About 0.8 s of events at 2 000 per second.
    control.set_check_rate(2_000.0).unwrap();
    let started = std::time::Instant::now();
    control.burst();
    engine
        .snapshot(|snapshot| {
            snapshot
                .services
                .values()
                .all(|service| service.check.result.is_some())
        })
        .await;
    let elapsed = started.elapsed();
    let published = engine.latest().unwrap().revision - before;
    let allowed = u64::try_from(elapsed.as_millis() / interval.as_millis()).unwrap() + 2;
    assert!(
        published <= allowed,
        "{published} snapshots in {elapsed:?} (at most {allowed})"
    );
    assert!(
        published >= 2,
        "progress shows while the burst lasts: {published}"
    );
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_event_permissions_there_is_no_stream() {
    let server = mock(MockConfig {
        users: vec![ic_mock::MockUser::new(
            "reader",
            "secret",
            &["objects/query/*", "status/query"],
        )],
        ..MockConfig::with_scenario(scenarios::lab())
    })
    .await;
    let mut environment = crate::support::environment(&server);
    environment.auth = ic_config::AuthConfig::Basic {
        username: "reader".to_owned(),
    };
    let mut engine = crate::support::start(
        environment,
        crate::support::FakeSecrets::with(crate::support::ENV_ID, "secret"),
        crate::support::tuning(),
    );
    engine.connected().await;
    assert_eq!(server.control().event_streams(), 0);
    assert!(
        !server
            .control()
            .requests()
            .iter()
            .any(|request| request.path == "/v1/events")
    );
    let snapshot = engine.latest().unwrap();
    assert_eq!(snapshot.hosts.len(), 2);
    engine.shutdown();
}
