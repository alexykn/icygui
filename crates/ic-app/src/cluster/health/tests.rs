//! Tests of the cluster health page's content.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use ic_core::health::{ClusterHealth, HealthSample};
use ic_core::snapshot::Snapshot;
use ic_core::{ClusterView, ConnectedNode, NodeState};
use ic_model::{
    Endpoint, EndpointStats, FeatureState, Host, HostName, InstanceStatus, ListenerStatus,
    NodeFeatures, ObjectCounts, ObjectKey, Timestamp, Zone,
};

use super::*;

const NOW: f64 = 1_791_500_000.0;

fn at(seconds_ago: f64) -> Timestamp {
    Timestamp::from_unix_seconds(NOW - seconds_ago)
}

fn now() -> Timestamp {
    at(0.0)
}

fn zone(name: &str, parent: Option<&str>, endpoints: &[&str]) -> Zone {
    Zone {
        name: name.to_owned(),
        parent: parent.map(str::to_owned),
        endpoints: endpoints.iter().map(|name| (*name).to_owned()).collect(),
        global: false,
    }
}

fn endpoint(name: &str, zone: &str, connected: bool) -> Endpoint {
    Endpoint {
        name: name.to_owned(),
        zone: zone.to_owned(),
        connected,
    }
}

fn stats(version: u32, last: f64, rate: f64) -> EndpointStats {
    EndpointStats {
        version,
        last_message: at(last),
        messages_in: rate,
        messages_out: rate / 2.0,
        connecting: false,
    }
}

fn host_in(name: &str, zone: &str) -> (HostName, Arc<Host>) {
    let mut host = Host::new(name);
    host.check.zone = Some(zone.to_owned());
    (host.name.clone(), Arc::new(host))
}

/// The mock-up's cluster: two masters, a satellite each in `ams` and
/// `fra` (an older version), two global zones; healthy.
fn cluster() -> Snapshot {
    let mut zones = vec![
        zone("master", None, &["master-01", "master-02"]),
        zone("ams", Some("master"), &["sat-ams-01"]),
        zone("fra", Some("master"), &["sat-fra-01"]),
    ];
    for name in ["global-templates", "director-global"] {
        zones.push(Zone {
            name: name.to_owned(),
            parent: None,
            endpoints: Vec::new(),
            global: true,
        });
    }
    let endpoints = vec![
        endpoint("master-01", "master", true),
        endpoint("master-02", "master", true),
        endpoint("sat-ams-01", "ams", true),
        endpoint("sat-fra-01", "fra", true),
    ];
    let health = ClusterHealth {
        endpoints: BTreeMap::from([
            ("master-02".to_owned(), stats(21_403, 0.5, 398.0)),
            ("sat-ams-01".to_owned(), stats(21_403, 1.0, 201.0)),
            ("sat-fra-01".to_owned(), stats(21_402, 0.2, 187.0)),
        ]),
        listener: Some(ListenerStatus {
            http_clients: 4,
            endpoints: 3,
            connected_endpoints: 3,
            work_queue_rate: 412.0,
            ..ListenerStatus::default()
        }),
        endpoints_at: Some(at(0.0)),
        listener_at: Some(at(12.0)),
        features: Some(NodeFeatures {
            checker: Some(FeatureState::Running),
            notification: Some(FeatureState::Paused),
            icingadb: Some(FeatureState::Off),
        }),
        samples: (0..12)
            .map(|index| HealthSample {
                at: at(f64::from(12 - index) * 30.0),
                active_checks: 3_280.0,
                passive_checks: 212.0,
                latency: 0.004,
                execution: 1.32,
                relay_queue: Some(0.0),
                work_queue_rate: Some(412.0),
                ..HealthSample::default()
            })
            .collect::<VecDeque<_>>(),
        interval: Duration::from_secs(30),
    };
    let hosts: BTreeMap<HostName, Arc<Host>> = [
        host_in("sw-core-ams-01", "ams"),
        host_in("sw-core-fra-01", "fra"),
        host_in("sw-core-fra-02", "fra"),
    ]
    .into_iter()
    .collect();
    Snapshot {
        zones: Arc::new(zones),
        endpoints: Arc::new(endpoints),
        health: Arc::new(health),
        hosts: Arc::new(hosts),
        status: Some(Arc::new(InstanceStatus {
            node_name: "master-01".to_owned(),
            version: "r2.14.3-1".to_owned(),
            program_start: at(41.0 * 86_400.0 + 6.0 * 3_600.0),
            notifications_enabled: true,
            host_checks_enabled: true,
            service_checks_enabled: true,
            event_handlers_enabled: true,
            flap_detection_enabled: true,
            perfdata_enabled: true,
            checks_per_minute: 3_283.0,
            passive_checks_per_minute: 212.0,
            avg_latency: 0.004,
            max_latency: 0.92,
            avg_execution_time: 1.32,
            max_execution_time: 9.81,
            counts: ObjectCounts {
                hosts_up: 300,
                services_ok: 3_213,
                services_pending: 3,
                ..ObjectCounts::default()
            },
        })),
        node: Some(Arc::new(ConnectedNode {
            url: "https://master-01:5665".to_owned(),
            url_index: 0,
            name: "master-01".to_owned(),
            zone: Some("master".to_owned()),
            view: ClusterView::Full,
            passed_over: Vec::new(),
        })),
        last_event_at: Some(at(0.4)),
        ..Snapshot::default()
    }
}

/// `snapshot` with `sat-fra-01` gone for 3m 12s, `late` checks of its
/// hosts late, and the relay queue growing.
fn degraded(late: &[&str]) -> Snapshot {
    let mut snapshot = cluster();
    let mut endpoints = (*snapshot.endpoints).clone();
    endpoints[3].connected = false;
    snapshot.endpoints = Arc::new(endpoints);
    let mut health = (*snapshot.health).clone();
    health
        .endpoints
        .insert("sat-fra-01".to_owned(), stats(21_402, 192.0, 0.0));
    for (index, sample) in health.samples.iter_mut().enumerate() {
        let step = u8::try_from(index.saturating_sub(8)).unwrap_or(u8::MAX);
        sample.relay_queue = Some(f64::from(step) * 6_000.0);
    }
    if let Some(listener) = &mut health.listener {
        listener.relay_queue = 18_402.0;
        listener.connected_endpoints = 2;
    }
    snapshot.health = Arc::new(health);
    snapshot.late = Arc::new(
        late.iter()
            .map(|host| (ObjectKey::host(host), at(60.0)))
            .collect(),
    );
    snapshot
}

#[test]
fn a_healthy_cluster_reads_like_the_mock_up() {
    let report = report(&cluster(), true, now());
    assert_eq!(report.state, ClusterState::Ok);
    assert_eq!((report.connected, report.not_connected), (4, 0));
    assert_eq!(report.instance, "Icinga r2.14.3-1 · up 41d 6h");
    assert_eq!(report.seen_from.as_deref(), Some("master-01"));
    assert!(report.alerts.is_empty());
    assert_eq!(
        report.beats.tone,
        super::super::beats::BeatTone::Off,
        "no heartbeats"
    );
    let zones: Vec<(&str, &str)> = report
        .zones
        .iter()
        .map(|zone| (zone.name.as_str(), zone.detail.as_str()))
        .collect();
    assert_eq!(
        zones,
        [
            (
                "master",
                "top level · 2 endpoints · checks shared between them"
            ),
            ("ams", "parent master · 1 endpoint · 1 host"),
            ("fra", "parent master · 1 endpoint · 2 hosts"),
        ]
    );
    let master = &report.zones[0].endpoints[0];
    assert!(master.this_node);
    assert_eq!(master.status, "connected · this node");
    assert_eq!(master.version, "2.14.3");
    assert_eq!(master.traffic, "—", "no traffic of its own");
    assert_eq!(master.last_message, "0s ago", "the event stream's");
    let fra = &report.zones[2].endpoints[0];
    assert_eq!(fra.status, "connected · older version");
    assert_eq!(fra.version, "2.14.2");
    assert_eq!(fra.traffic, "187/s · 94/s");
    assert_eq!(report.global_zones, ["director-global", "global-templates"]);
    assert!(report.checks.iter().all(|tile| tile.tone == Tone::Normal));
    assert!(report.queues.iter().all(|tile| tile.tone == Tone::Normal));
    assert_eq!(report.checks[0].value, "3,283");
    assert_eq!(report.checks[0].detail, "of 3,513 objects");
    assert_eq!(report.checks[4].detail, "3 services");
    assert!(report.checks[5].trend.is_empty(), "a flat zero draws none");
    // Without IcingaDB, uptime takes its column too.
    let labels: Vec<&str> = report.queues.iter().map(|tile| tile.label).collect();
    assert_eq!(
        labels,
        [
            "API work queue",
            "relay queue",
            "cluster connections",
            "HTTP clients",
            "uptime"
        ]
    );
    assert!(report.queues[4].wide);
    assert_eq!(report.queues[2].value, "3 of 3");
    assert_eq!(report.switches.len(), 6);
    assert!(report.switches.iter().all(|switch| switch.on));
    let features: Vec<(&str, &str)> = report
        .features
        .iter()
        .map(|feature| (feature.label, feature.word()))
        .collect();
    assert_eq!(
        features,
        [
            ("checker", "running"),
            ("notification", "paused here"),
            ("IcingaDB", "off")
        ]
    );
    assert_eq!(assess(&cluster(), true, now()), ClusterState::Ok);
}

#[test]
fn a_satellite_gone_cuts_its_zone_off() {
    let mut snapshot = degraded(&["sw-core-fra-01", "sw-core-fra-02"]);
    // The engine's alert for it (ic-core's trouble assessment).
    snapshot.trouble = Arc::new(ic_core::trouble::Trouble {
        alerts: vec![ic_core::trouble::Alert {
            key: "zone:fra".to_owned(),
            tone: ic_core::trouble::AlertTone::Critical,
            title: "zone fra: sat-fra-01 disconnected".to_owned(),
            detail: "Zone fra’s results are stale; 2 checks in zone fra are late.".to_owned(),
            action: Some(ic_core::trouble::AlertAction::LateChecks {
                zone: Some("fra".to_owned()),
            }),
            since: at(192.0),
        }],
        blind: None,
    });
    let report = report(&snapshot, true, now());
    assert_eq!(report.state, ClusterState::Critical);
    assert_eq!(assess(&snapshot, true, now()), ClusterState::Critical);
    assert_eq!((report.connected, report.not_connected), (3, 1));
    let fra = &report.zones[2];
    assert_eq!(fra.state, NodeState::Disconnected);
    assert_eq!(fra.endpoints[0].status, "not connected");
    assert_eq!(fra.endpoints[0].tone, Tone::Critical);
    assert_eq!(fra.endpoints[0].last_message, "3m 12s ago");
    assert_eq!(fra.endpoints[0].traffic, "0/s · 0/s");
    let alert = &report.alerts[0];
    assert_eq!(alert.tone, Tone::Critical);
    assert_eq!(alert.title, "zone fra: sat-fra-01 disconnected");
    assert_eq!(
        alert.link.as_ref().map(|(words, _)| words.as_str()),
        Some("show the late checks")
    );
    assert_eq!(report.late.len(), 2, "the alert's zone's late checks");
    let late = &report.checks[5];
    assert_eq!(late.tone, Tone::Critical, "a zone is cut off");
    assert_eq!(late.detail, "all in zone fra");
    let relay = &report.queues[1];
    assert_eq!(relay.tone, Tone::Critical, "beyond 10 000");
    assert_eq!(relay.detail, "growing");
    assert_eq!(report.queues[2].tone, Tone::Critical, "2 of 3 connected");
}

#[test]
fn a_master_gone_from_an_ha_pair_warns_without_cutting_off() {
    let mut snapshot = cluster();
    let mut endpoints = (*snapshot.endpoints).clone();
    endpoints[1].connected = false;
    snapshot.endpoints = Arc::new(endpoints);
    let report = report(&snapshot, true, now());
    // The engine raises its alert after the grace; until then the page
    // shows the endpoint's row only.
    assert!(report.alerts.is_empty());
    assert!(report.late.is_empty());
    // The endpoint is down all the same: the sidebar's dot is critical.
    assert_eq!(report.state, ClusterState::Critical);
}

#[test]
fn late_checks_warn_and_turn_critical_from_one_percent() {
    let mut snapshot = cluster();
    snapshot.late = Arc::new(BTreeMap::from([(
        ObjectKey::host("sw-core-ams-01"),
        at(90.0),
    )]));
    assert_eq!(report(&snapshot, true, now()).checks[5].tone, Tone::Warning);
    assert_eq!(assess(&snapshot, true, now()), ClusterState::Warning);
    let late: BTreeMap<ObjectKey, Timestamp> = (0..40)
        .map(|index| (ObjectKey::host(&format!("h{index}")), at(90.0)))
        .collect();
    snapshot.late = Arc::new(late);
    assert_eq!(
        report(&snapshot, true, now()).checks[5].tone,
        Tone::Critical
    );
}

#[test]
fn fewer_checks_and_slow_latency_need_a_look() {
    let mut snapshot = cluster();
    let mut health = (*snapshot.health).clone();
    if let Some(latest) = health.samples.back_mut() {
        latest.active_checks = 2_104.0;
        latest.latency = 0.064;
    }
    snapshot.health = Arc::new(health);
    let report = report(&snapshot, true, now());
    assert_eq!(report.checks[0].tone, Tone::Warning, "a fall by a third");
    assert_eq!(report.checks[2].tone, Tone::Warning, "16 times the median");
    assert_eq!(report.state, ClusterState::Warning);
}

#[test]
fn a_connected_nodes_last_message_is_as_of_its_numbers() {
    let mut snapshot = cluster();
    let mut health = (*snapshot.health).clone();
    health.endpoints_at = Some(at(20.0));
    health
        .endpoints
        .insert("sat-ams-01".to_owned(), stats(21_403, 21.0, 201.0));
    snapshot.health = Arc::new(health);
    let report = report(&snapshot, true, now());
    assert_eq!(report.zones[1].endpoints[0].last_message, "1s ago");
    assert_eq!(
        report.state,
        ClusterState::Ok,
        "no lag: 20 s since the poll"
    );
}

#[test]
fn a_lagging_node_warns() {
    let mut snapshot = cluster();
    let mut health = (*snapshot.health).clone();
    health
        .endpoints
        .insert("sat-ams-01".to_owned(), stats(21_403, 150.0, 201.0));
    snapshot.health = Arc::new(health);
    let report = report(&snapshot, true, now());
    let ams = &report.zones[1].endpoints[0];
    assert_eq!(ams.status, "connected · no message");
    assert_eq!(ams.tone, Tone::Warning);
    assert_eq!(report.state, ClusterState::Warning);
    assert_eq!(assess(&snapshot, true, now()), ClusterState::Warning);
}

#[test]
fn icingadb_takes_a_tile_only_while_enabled() {
    let mut snapshot = cluster();
    let mut health = (*snapshot.health).clone();
    health.features = Some(NodeFeatures {
        icingadb: Some(FeatureState::Running),
        ..NodeFeatures::default()
    });
    snapshot.health = Arc::new(health);
    let report = report(&snapshot, true, now());
    let labels: Vec<&str> = report.queues.iter().map(|tile| tile.label).collect();
    assert_eq!(
        labels,
        [
            "API work queue",
            "relay queue",
            "cluster connections",
            "HTTP clients",
            "IcingaDB",
            "uptime"
        ]
    );
    assert!(!report.queues[5].wide);
}

#[test]
fn switches_off_warn_on_the_page_only() {
    let mut snapshot = cluster();
    let mut status = (**snapshot.status.as_ref().unwrap()).clone();
    status.perfdata_enabled = false;
    snapshot.status = Some(Arc::new(status));
    let report = report(&snapshot, true, now());
    assert!(!report.switches[5].on);
    assert_eq!(report.state, ClusterState::Ok, "a setting, not health");
}

#[test]
fn before_the_page_asks_the_queues_say_so() {
    let mut snapshot = cluster();
    let mut health = (*snapshot.health).clone();
    health.listener = None;
    health.features = None;
    health.endpoints.clear();
    snapshot.health = Arc::new(health);
    let report = report(&snapshot, true, now());
    assert_eq!(report.queues[0].value, "—");
    assert_eq!(report.queues[0].detail, "not read yet");
    assert!(report.features.is_empty());
    let ams = &report.zones[1].endpoints[0];
    assert_eq!(
        (ams.version.as_str(), ams.last_message.as_str()),
        ("—", "—")
    );
}

#[test]
fn nothing_is_known_before_connecting() {
    assert_eq!(
        report(&cluster(), false, now()).state,
        ClusterState::Unknown
    );
    assert_eq!(
        assess(&Snapshot::default(), true, now()),
        ClusterState::Unknown
    );
}

#[test]
fn counts_get_thousands_separators() {
    assert_eq!(count_text(0), "0");
    assert_eq!(count_text(999), "999");
    assert_eq!(count_text(1_204), "1,204");
    assert_eq!(count_text(18_402), "18,402");
    assert_eq!(count_text(1_234_567), "1,234,567");
}
