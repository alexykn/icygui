//! Heartbeats (`Scenario::with_heartbeats`): real-time checks, Icinga's
//! UNKNOWN for a check pinned to an endpoint that isn't connected, a
//! checker that stops, a zone cut off or hung (its beats silent), and the
//! event stream's filter by name.

mod common;

use std::time::Duration;

use common::{EventStream, start};
use ic_mock::{MockConfig, scenarios};
use serde_json::{Value, json};

fn config() -> MockConfig {
    MockConfig {
        housekeeping_interval: Duration::from_millis(50),
        ..MockConfig::with_scenario(scenarios::prod_cluster().with_heartbeats(1.0))
    }
}

/// Check results until one of `service` on the masters' heartbeat host
/// comes.
async fn result_of(stream: &mut EventStream, service: &str) -> Value {
    result_on(stream, "icygui-hb-master", service).await
}

/// Check results until one of `host!service` comes.
async fn result_on(stream: &mut EventStream, host: &str, service: &str) -> Value {
    loop {
        let event = stream.next().await;
        if event["type"] == "CheckResult" && event["host"] == host && event["service"] == service {
            return event;
        }
    }
}

/// The time of `host!service`'s last check.
fn last_check(control: &ic_mock::MockControl, host: &str, service: &str) -> Option<f64> {
    control
        .service(host, service)
        .and_then(|service| service.check.last_check)
        .map(ic_model::Timestamp::as_unix_seconds)
}

#[tokio::test]
async fn beats_run_in_real_time_and_a_disconnected_endpoint_makes_its_beat_unknown() {
    let (server, client) = start(config()).await;
    let mut stream = EventStream::open(
        &client,
        &server,
        &json!({"types": ["CheckResult"], "queue": "beats"}),
    )
    .await;
    let first = result_of(&mut stream, "beat-master-01").await;
    assert_eq!(first["host"], "icygui-hb-master");
    assert_eq!(first["check_result"]["state"], json!(0));
    let second = result_of(&mut stream, "beat-master-01").await;
    assert!(
        second["check_result"]["execution_end"].as_f64()
            > first["check_result"]["execution_end"].as_f64(),
        "one per interval"
    );
    // master-02 goes away: the check pinned to it is UNKNOWN, with
    // Icinga's words; master-01's stays OK.
    server
        .control()
        .set_endpoint_connected("master-02", false)
        .unwrap();
    // (One result may have been on its way.)
    let mut pinned = result_of(&mut stream, "beat-master-02").await;
    if pinned["check_result"]["state"] == json!(0) {
        pinned = result_of(&mut stream, "beat-master-02").await;
    }
    assert_eq!(pinned["check_result"]["state"], json!(3));
    assert_eq!(
        pinned["check_result"]["output"],
        "Remote Icinga instance 'master-02' is not connected to 'master-01'"
    );
    let other = result_of(&mut stream, "beat-master-01").await;
    assert_eq!(other["check_result"]["state"], json!(0));
}

#[tokio::test]
async fn a_stopped_checker_sends_no_beats() {
    let (server, client) = start(config()).await;
    let mut stream = EventStream::open(
        &client,
        &server,
        &json!({"types": ["CheckResult"], "queue": "beats"}),
    )
    .await;
    result_of(&mut stream, "beat-master-01").await;
    let control = server.control();
    control.stop_checks();
    // Drain what was on its way, then nothing for a few intervals.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let before = last_check(&control, "icygui-hb-master", "beat-master-01");
    tokio::time::sleep(Duration::from_millis(2_500)).await;
    let after = last_check(&control, "icygui-hb-master", "beat-master-01");
    assert_eq!(before, after);
    control.start_checks();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let again = last_check(&control, "icygui-hb-master", "beat-master-01");
    assert!(again > after);
}

/// A zone that is cut off sends nothing: its beat goes silent rather than
/// beating on the master (Icinga's satellite runs it), and comes back with
/// the zone.
#[tokio::test]
async fn a_cut_off_zone_s_beat_goes_silent() {
    let (server, client) = start(config()).await;
    let mut stream = EventStream::open(
        &client,
        &server,
        &json!({"types": ["CheckResult"], "queue": "beats-zone"}),
    )
    .await;
    let beat = result_on(&mut stream, "icygui-hb-fra", "beat").await;
    assert_eq!(beat["check_result"]["state"], json!(0));
    assert_eq!(
        beat["check_result"]["check_source"], "sat-fra-01",
        "the satellite runs it"
    );
    let control = server.control();
    control.set_endpoint_connected("sat-fra-01", false).unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let before = last_check(&control, "icygui-hb-fra", "beat");
    tokio::time::sleep(Duration::from_millis(2_500)).await;
    assert_eq!(last_check(&control, "icygui-hb-fra", "beat"), before);
    // ams still beats.
    let ams = result_on(&mut stream, "icygui-hb-ams", "beat").await;
    assert_eq!(ams["check_result"]["state"], json!(0));
    control.set_endpoint_connected("sat-fra-01", true).unwrap();
    let back = result_on(&mut stream, "icygui-hb-fra", "beat").await;
    assert_eq!(back["check_result"]["state"], json!(0));
}

/// In an HA satellite zone a hung satellite answers nothing pinned to it,
/// and the other runs the zone's beat; with the other gone too, the zone
/// runs no checks: every beat of it silent, though one is connected.
#[tokio::test]
async fn a_hung_satellite_leaves_its_zone_to_the_other() {
    let config = MockConfig {
        housekeeping_interval: Duration::from_millis(50),
        ..MockConfig::with_scenario(
            scenarios::prod_cluster()
                .with_endpoint("sat-fra-02", "fra")
                .with_heartbeats(1.0),
        )
    };
    let (server, client) = start(config).await;
    let mut stream = EventStream::open(
        &client,
        &server,
        &json!({"types": ["CheckResult"], "queue": "beats-ha"}),
    )
    .await;
    result_on(&mut stream, "icygui-hb-fra", "beat-sat-fra-01").await;
    let control = server.control();
    control.stop_checks_on("sat-fra-01");
    tokio::time::sleep(Duration::from_millis(200)).await;
    let pinned = last_check(&control, "icygui-hb-fra", "beat-sat-fra-01");
    let zone = result_on(&mut stream, "icygui-hb-fra", "beat").await;
    assert_eq!(zone["check_result"]["check_source"], "sat-fra-02");
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    assert_eq!(
        last_check(&control, "icygui-hb-fra", "beat-sat-fra-01"),
        pinned,
        "a hung satellite answers nothing"
    );
    control.set_endpoint_connected("sat-fra-02", false).unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let before = last_check(&control, "icygui-hb-fra", "beat");
    tokio::time::sleep(Duration::from_millis(2_500)).await;
    assert_eq!(
        last_check(&control, "icygui-hb-fra", "beat"),
        before,
        "zone fra runs no checks"
    );
}

#[tokio::test]
async fn the_stream_filter_carries_only_the_named_beats() {
    let (server, client) = start(config()).await;
    let filter = r#"event.type != "CheckResult" || (event.host == "icygui-hb-master" && event.service == "beat-master-01")"#;
    let mut stream = EventStream::open(
        &client,
        &server,
        &json!({"types": ["CheckResult", "StateChange"], "queue": "quiet", "filter": filter}),
    )
    .await;
    for _ in 0..4 {
        let event = stream.next().await;
        assert_eq!(event["type"], "CheckResult");
        assert_eq!(event["service"], "beat-master-01", "{event}");
    }
}

#[tokio::test]
async fn the_simulator_leaves_the_beats_to_their_own_clock() {
    let mut config = config();
    config.simulation.enabled = true;
    config.simulation.speed = 50.0;
    let (server, client) = start(config).await;
    let mut stream = EventStream::open(
        &client,
        &server,
        &json!({"types": ["CheckResult"], "queue": "beats-sim"}),
    )
    .await;
    server
        .control()
        .set_endpoint_connected("master-02", false)
        .unwrap();
    // Once UNKNOWN, the pinned beat stays UNKNOWN: the simulated checker
    // never runs it as an ordinary OK check in between.
    let mut pinned = result_of(&mut stream, "beat-master-02").await;
    if pinned["check_result"]["state"] == json!(0) {
        pinned = result_of(&mut stream, "beat-master-02").await;
    }
    assert_eq!(pinned["check_result"]["state"], json!(3));
    for _ in 0..4 {
        let next = result_of(&mut stream, "beat-master-02").await;
        assert_eq!(next["check_result"]["state"], json!(3), "{next}");
    }
}
