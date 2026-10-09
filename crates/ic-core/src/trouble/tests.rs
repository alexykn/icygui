use std::collections::BTreeMap;
use std::time::Duration;

use ic_model::{
    EndpointStats, FeatureState, ListenerStatus, NodeFeatures, ServiceKey, Timestamp, Zone,
};

use super::*;
use crate::health::{ClusterHealth, HealthSample};
use crate::heartbeat::{BeatState, Death, Heartbeat, HeartbeatSetup, Heartbeats, Proves};
use crate::topology::{ClusterNode, NodeState};

/// The mock-ups' cluster: zone master (HA), ams (one satellite), fra (HA).
fn zones() -> Vec<Zone> {
    let zone = |name: &str, parent: Option<&str>, endpoints: &[&str]| Zone {
        name: name.to_owned(),
        parent: parent.map(str::to_owned),
        endpoints: endpoints.iter().map(|e| (*e).to_owned()).collect(),
        global: false,
    };
    vec![
        zone("master", None, &["master-01", "master-02"]),
        zone("ams", Some("master"), &["sat-ams-01"]),
        zone("fra", Some("master"), &["sat-fra-01", "sat-fra-02"]),
    ]
}

fn nodes(down: &[&str]) -> Vec<ClusterNode> {
    [
        ("master-01", "master"),
        ("master-02", "master"),
        ("sat-ams-01", "ams"),
        ("sat-fra-01", "fra"),
        ("sat-fra-02", "fra"),
    ]
    .into_iter()
    .map(|(name, zone)| ClusterNode {
        name: name.to_owned(),
        zone: zone.to_owned(),
        state: if down.contains(&name) {
            NodeState::Disconnected
        } else {
            NodeState::Connected
        },
    })
    .collect()
}

fn at(seconds: f64) -> Timestamp {
    Timestamp::from_unix_seconds(seconds)
}

fn beat(proves: Proves, state: BeatState) -> Heartbeat {
    let host = format!("icygui-hb-{}", proves.subject());
    Heartbeat {
        key: ServiceKey::new(&host, "beat"),
        proves,
        interval: Duration::from_secs(30),
        state,
        last_beat: Some(at(1_000.0)),
        last_check: Some(at(1_000.0)),
        since: at(1_030.0),
        deadline: None,
        allowance: Duration::from_secs(5),
        reason: None,
    }
}

fn pinned(endpoint: &str, zone: &str) -> Proves {
    Proves::Endpoint {
        endpoint: endpoint.to_owned(),
        zone: Some(zone.to_owned()),
    }
}

/// The six beats of the mock-ups, every one in `state` but those in
/// `other`.
fn six(state: BeatState, other: &[(&str, BeatState)]) -> Heartbeats {
    let mut beats = vec![
        beat(pinned("master-01", "master"), state),
        beat(pinned("master-02", "master"), state),
        beat(Proves::Zone("ams".to_owned()), state),
        beat(Proves::Zone("fra".to_owned()), state),
        beat(pinned("sat-fra-01", "fra"), state),
        beat(pinned("sat-fra-02", "fra"), state),
    ];
    for (subject, state) in other {
        for beat in &mut beats {
            if beat.proves.subject() == *subject {
                beat.state = *state;
            }
        }
    }
    Heartbeats {
        setup: HeartbeatSetup::Find {
            variable: "icygui_heartbeat".to_owned(),
        },
        beats,
        polled: false,
    }
}

fn clock(at: Timestamp) -> String {
    format!("t{}", at.as_unix_seconds())
}

fn health_with(down: &[(&str, f64)]) -> ClusterHealth {
    let mut health = ClusterHealth::default();
    for (name, last) in down {
        health.endpoints.insert(
            (*name).to_owned(),
            EndpointStats {
                last_message: at(*last),
                ..EndpointStats::default()
            },
        );
    }
    health
}

fn assess_with(
    nodes: &[ClusterNode],
    health: &ClusterHealth,
    beats: &Heartbeats,
    late: &BTreeMap<Option<String>, usize>,
) -> Vec<Finding> {
    let zones = zones();
    assess(&Facts {
        nodes,
        zones: &zones,
        connected: Some("master-01"),
        health,
        heartbeats: beats,
        late,
        clock: &clock,
    })
}

fn titles(findings: &[Finding]) -> Vec<&str> {
    findings.iter().map(|finding| finding.title.as_str()).collect()
}

#[test]
fn everything_on_time_raises_nothing() {
    let findings = assess_with(
        &nodes(&[]),
        &ClusterHealth::default(),
        &six(BeatState::OnTime, &[]),
        &BTreeMap::new(),
    );
    assert!(findings.is_empty(), "{findings:?}");
}

/// 16a3: every beat stopped at the same time, two endpoints down: one
/// alert for the cause, and the endpoints whose beats were lost one line
/// each, never a heartbeat alert besides.
#[test]
fn every_beat_stopped_is_icinga_running_no_checks() {
    let late = BTreeMap::from([(None, 3_516)]);
    let findings = assess_with(
        &nodes(&["master-02", "sat-fra-01"]),
        &health_with(&[("master-02", 1_040.0), ("sat-fra-01", 1_040.0)]),
        &six(BeatState::Dead(Death::Stopped), &[]),
        &late,
    );
    assert_eq!(
        titles(&findings),
        [
            "Icinga runs no checks: none since t1000.",
            "master-02 disconnected, heartbeat lost",
            "zone fra: sat-fra-01 disconnected, heartbeat lost",
        ]
    );
    let no_checks = &findings[0];
    assert_eq!(
        no_checks.detail,
        "3,516 checks are late, and every heartbeat stopped at t1000."
    );
    assert_eq!(no_checks.grace, Duration::ZERO, "the beats already waited");
    assert!(no_checks.urgent);
    assert_eq!(no_checks.note, "Icinga runs no checks");
    assert_eq!(
        no_checks.action,
        Some(AlertAction::LateChecks { zone: None })
    );
    // The endpoint lines wait their grace from the last message.
    assert_eq!(findings[1].since, Some(at(1_040.0)));
    assert_eq!(findings[1].grace, GRACE);
}

/// 16r: master-02 is down and its pinned check came back UNKNOWN: one
/// alert, with Icinga's reason.
#[test]
fn a_pinned_beat_that_is_not_ok_says_why() {
    let mut beats = six(BeatState::OnTime, &[("master-02", BeatState::Dead(Death::NotOk))]);
    beats.beats[1].reason = Some(
        "Remote Icinga instance 'master-02' is not connected to 'master-01'".to_owned(),
    );
    let findings = assess_with(
        &nodes(&["master-02"]),
        &health_with(&[("master-02", 1_000.0)]),
        &beats,
        &BTreeMap::new(),
    );
    assert_eq!(
        titles(&findings),
        ["heartbeat master-02 dead: Remote Icinga instance 'master-02' is not connected"]
    );
    assert_eq!(
        findings[0].detail,
        "Icinga’s result for the check pinned to master-02 (UNKNOWN); master-01 runs every master check."
    );
    assert_eq!(findings[0].action, Some(AlertAction::ShowNode("master-02".to_owned())));
    assert_eq!(findings[0].recovered, "master-02 connected again");
}

/// 16s: sat-fra-02 went down first (its own line); sat-fra-01 answers but
/// its beat and the zone's stopped: the zone runs no checks.
#[test]
fn a_zone_whose_beats_stopped_while_connected_runs_no_checks() {
    let late = BTreeMap::from([(Some("fra".to_owned()), 1_204)]);
    let findings = assess_with(
        &nodes(&["sat-fra-02"]),
        &health_with(&[("sat-fra-02", 900.0)]),
        &six(
            BeatState::OnTime,
            &[
                ("fra", BeatState::Dead(Death::Stopped)),
                ("sat-fra-01", BeatState::Dead(Death::Stopped)),
                ("sat-fra-02", BeatState::Dead(Death::Stopped)),
            ],
        ),
        &late,
    );
    assert_eq!(
        titles(&findings),
        [
            "zone fra: sat-fra-02 disconnected, heartbeat lost",
            "zone fra runs no checks (sat-fra-01 connected)",
        ]
    );
    assert_eq!(
        findings[1].detail,
        "sat-fra-01 answers, but its beat and the zone fra beat stopped at t1000; 1,204 checks in zone fra are late."
    );
    assert_eq!(
        findings[1].action,
        Some(AlertAction::LateChecks {
            zone: Some("fra".to_owned())
        })
    );
}

/// 16t: sat-fra-01 is down and its beat lost: one line; sat-fra-02 runs
/// the zone and the zone's beat is on time.
#[test]
fn an_endpoint_down_with_its_beat_lost_is_one_line() {
    let findings = assess_with(
        &nodes(&["sat-fra-01"]),
        &health_with(&[("sat-fra-01", 1_000.0)]),
        &six(BeatState::OnTime, &[("sat-fra-01", BeatState::Late)]),
        &BTreeMap::new(),
    );
    assert_eq!(
        titles(&findings),
        ["zone fra: sat-fra-01 disconnected, heartbeat lost"]
    );
    assert_eq!(
        findings[0].detail,
        "sat-fra-02 runs zone fra’s checks alone; the zone fra beat is on time."
    );
    assert_eq!(findings[0].tone, AlertTone::Critical);
    // Without a beat of its own it is a warning, not a lost heartbeat.
    let findings = assess_with(
        &nodes(&["sat-fra-01"]),
        &health_with(&[("sat-fra-01", 1_000.0)]),
        &Heartbeats::default(),
        &BTreeMap::new(),
    );
    assert_eq!(titles(&findings), ["zone fra: sat-fra-01 disconnected"]);
    assert_eq!(findings[0].tone, AlertTone::Warning);
}

/// A zone with all its endpoints down is one line for the zone.
#[test]
fn a_zone_cut_off_is_one_line() {
    let findings = assess_with(
        &nodes(&["sat-fra-01", "sat-fra-02"]),
        &health_with(&[("sat-fra-01", 1_000.0), ("sat-fra-02", 1_010.0)]),
        &six(
            BeatState::OnTime,
            &[
                ("fra", BeatState::Dead(Death::Stopped)),
                ("sat-fra-01", BeatState::Dead(Death::Stopped)),
            ],
        ),
        &BTreeMap::new(),
    );
    assert_eq!(
        titles(&findings),
        ["zone fra: sat-fra-01 and sat-fra-02 disconnected, heartbeat lost"]
    );
    assert_eq!(findings[0].since, Some(at(1_010.0)));
    assert_eq!(findings[0].detail, "zone fra’s results are stale.");
}

/// 16u: a beat discovery no longer finds is a finding.
#[test]
fn a_disappeared_beat_is_a_warning() {
    let mut beats = six(BeatState::OnTime, &[("fra", BeatState::Disappeared)]);
    beats.beats[3].since = at(2_000.0);
    let findings = assess_with(&nodes(&[]), &ClusterHealth::default(), &beats, &BTreeMap::new());
    assert_eq!(titles(&findings), ["heartbeat fra disappeared since t2000"]);
    assert_eq!(
        findings[0].detail,
        "discovery no longer finds icygui-hb-fra!beat; zone fra is unwatched until it is back or removed."
    );
    assert_eq!(findings[0].tone, AlertTone::Warning);
    assert_eq!(findings[0].action, Some(AlertAction::Settings));
}

#[test]
fn the_status_poll_alone_finds_no_checks_after_its_grace() {
    let mut health = ClusterHealth::default();
    for (second, checks) in [(0.0, 3_283.0), (30.0, 3_280.0), (60.0, 0.0)] {
        health.push(HealthSample {
            at: at(second),
            active_checks: checks,
            ..HealthSample::default()
        });
    }
    let findings = assess_with(&nodes(&[]), &health, &Heartbeats::default(), &BTreeMap::new());
    assert_eq!(titles(&findings), ["Icinga runs no checks: none since t30."]);
    assert_eq!(findings[0].grace, GRACE);
    assert_eq!(
        findings[0].detail,
        "active checks fell from 3,280 a minute to 0."
    );
}

#[test]
fn features_and_queues() {
    let mut health = ClusterHealth {
        features: Some(NodeFeatures {
            checker: Some(FeatureState::Off),
            notification: Some(FeatureState::Off),
            icingadb: Some(FeatureState::Paused),
        }),
        listener: Some(ListenerStatus {
            relay_queue: 18_402.0,
            ..ListenerStatus::default()
        }),
        ..ClusterHealth::default()
    };
    for (second, relay) in [(0.0, 10.0), (30.0, 900.0), (60.0, 18_402.0)] {
        health.push(HealthSample {
            at: at(second),
            active_checks: 10.0,
            relay_queue: Some(relay),
            ..HealthSample::default()
        });
    }
    // master-02 is there: IcingaDB paused here is the HA partner's turn.
    let findings = assess_with(&nodes(&[]), &health, &Heartbeats::default(), &BTreeMap::new());
    assert_eq!(
        titles(&findings),
        [
            "the checker feature is off on master-01",
            "the notification feature is off on master-01",
            "the relay queue keeps growing (18,402 messages)",
        ]
    );
    let findings = assess_with(
        &nodes(&["master-02"]),
        &health,
        &Heartbeats::default(),
        &BTreeMap::new(),
    );
    assert!(titles(&findings).contains(&"IcingaDB paused on master-01"));
}

#[test]
fn trouble_ids_name_their_raise() {
    assert!(is_trouble_id("trouble:endpoint:master-02:1000"));
    assert_eq!(
        raised_id("trouble:endpoint:master-02:1000:cleared"),
        Some("trouble:endpoint:master-02:1000")
    );
    assert_eq!(raised_id("db-01!disk:critical:1000:cleared"), None);
    assert_eq!(raised_id("trouble:endpoint:master-02:1000"), None);
}
