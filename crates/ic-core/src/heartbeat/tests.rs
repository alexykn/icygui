use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use ic_config::{HeartbeatMode, HeartbeatSettings};
use ic_model::{CheckResult, Service, ServiceKey, ServiceState, Timestamp};
use serde_json::json;

use super::*;

fn service(host: &str, name: &str, vars: serde_json::Value) -> Service {
    let mut service = Service::new(host, name);
    service.state = ServiceState::Ok;
    service.check.check_interval = 30.0;
    if let serde_json::Value::Object(map) = vars {
        service.vars = map;
    }
    service
}

fn services(list: Vec<Service>) -> BTreeMap<ServiceKey, Arc<Service>> {
    list.into_iter()
        .map(|service| (service.key.clone(), Arc::new(service)))
        .collect()
}

fn at(seconds: f64) -> Timestamp {
    Timestamp::from_unix_seconds(seconds)
}

#[test]
fn heartbeats_are_found_by_their_custom_variable() {
    let mut pinned = service("icygui-hb-master-01", "beat", json!({ "icygui_heartbeat": true }));
    pinned.check.command_endpoint = Some("master-01".to_owned());
    pinned.check.zone = Some("master".to_owned());
    let mut zone = service("icygui-hb-ams", "beat", json!({ "icygui_heartbeat": "yes" }));
    zone.check.zone = Some("ams".to_owned());
    zone.check.check_interval = 60.0;
    let local = service("icygui-hb-local", "beat", json!({ "icygui_heartbeat": 1 }));
    let off = service("web-01", "ping4", json!({ "icygui_heartbeat": false }));
    let zero = service("web-02", "ping4", json!({ "icygui_heartbeat": "0" }));
    let none = service("web-03", "ping4", json!({ "role": "web" }));
    let all = services(vec![pinned, zone, local, off, zero, none]);
    let (found, missing) = discover(&HeartbeatSettings::default(), &all, Some("master"), Timing::default().min_interval);
    assert!(missing.is_empty());
    let mut proves: Vec<(String, Duration)> = found
        .iter()
        .map(|beat| (beat.proves.label(), beat.interval))
        .collect();
    proves.sort();
    assert_eq!(
        proves,
        [
            ("master-01".to_owned(), Duration::from_secs(30)),
            ("zone ams".to_owned(), Duration::from_secs(60)),
            ("zone master".to_owned(), Duration::from_secs(30)),
        ],
        "pinned beats prove their endpoint, the others their zone (the node's own without one)"
    );
    // Another variable finds nothing here; none at all is off.
    let other = HeartbeatSettings {
        variable: "vars.beat".to_owned(),
        ..HeartbeatSettings::default()
    };
    assert!(discover(&other, &all, None, Timing::default().min_interval).0.is_empty());
}

#[test]
fn listed_heartbeats_are_looked_up_by_name() {
    let all = services(vec![service("icygui-hb-ams", "beat", json!({}))]);
    let settings = HeartbeatSettings {
        mode: HeartbeatMode::List,
        list: vec!["icygui-hb-ams!beat".to_owned(), "icygui-hb-ams!beet".to_owned()],
        interval_secs: Some(3),
        ..HeartbeatSettings::default()
    };
    let (found, missing) = discover(&settings, &all, None, Timing::default().min_interval);
    assert_eq!(found.len(), 1);
    assert_eq!(
        found[0].interval,
        Duration::from_secs(u64::from(ic_config::MIN_HEARTBEAT_INTERVAL_SECS)),
        "an override below the minimum is raised to it"
    );
    assert_eq!(missing, [ServiceKey::new("icygui-hb-ams", "beet")]);
}

fn result(schedule: f64, start: f64, end: f64) -> CheckResult {
    CheckResult {
        output: "icygui heartbeat".to_owned(),
        long_output: String::new(),
        perfdata: Vec::new(),
        exit_status: 0,
        schedule_start: at(schedule),
        execution_start: at(start),
        execution_end: at(end),
        check_source: "master-01".to_owned(),
        active: true,
    }
}

#[test]
fn the_allowance_is_measured_with_a_floor_and_a_cap() {
    let interval = Duration::from_secs(30);
    let mut budget = Budget::default();
    // Nothing measured yet: the floor.
    assert_eq!(budget.allowance(interval, MIN_ALLOWANCE), MIN_ALLOWANCE);
    // Fast beats on a clock 40 s ahead of Icinga's: the offset cancels
    // out, so the floor holds.
    for beat in 0..60 {
        let t = 1_000.0 + f64::from(beat) * 30.0;
        budget.record(&result(t, t + 0.001, t + 0.002), at(t + 40.01));
    }
    assert_eq!(budget.len(), BUDGET_WINDOW);
    assert_eq!(budget.allowance(interval, MIN_ALLOWANCE), MIN_ALLOWANCE);
    // A busy checker: two seconds of scheduling latency on every beat.
    for beat in 0..50 {
        let t = 5_000.0 + f64::from(beat) * 30.0;
        budget.record(&result(t, t + 2.0, t + 2.01), at(t + 2.02 + 40.0));
    }
    let allowance = budget.allowance(interval, MIN_ALLOWANCE);
    assert!(
        allowance > Duration::from_secs(5) && allowance <= Duration::from_secs(7),
        "3 × p99: {allowance:?}"
    );
    // Never more than half the interval.
    for beat in 0..50 {
        let t = 9_000.0 + f64::from(beat) * 30.0;
        budget.record(&result(t, t + 9.0, t + 9.0), at(t + 9.0 + 40.0));
    }
    assert_eq!(budget.allowance(interval, MIN_ALLOWANCE), Duration::from_secs(15));
}

#[test]
fn a_late_beat_is_judged_by_its_last_check() {
    let interval = Duration::from_secs(30);
    let allowance = Duration::from_secs(5);
    let mut beat = service("icygui-hb", "beat", json!({}));
    beat.check.last_check = Some(at(1_000.0));
    // The last beat that arrived ended at 1 000: nothing newer ran.
    assert_eq!(
        judge(&beat, Some(at(1_000.0)), interval, allowance, Some(1_060.0), QUERY_AFTER),
        Verdict::Stopped
    );
    // A newer check ran, the stream didn't bring it.
    beat.check.last_check = Some(at(1_030.0));
    assert_eq!(
        judge(&beat, Some(at(1_000.0)), interval, allowance, Some(1_060.0), QUERY_AFTER),
        Verdict::Fresh
    );
    // No beat arrived in this session: recent enough is fresh, old is
    // stopped.
    assert_eq!(
        judge(&beat, None, interval, allowance, Some(1_060.0), QUERY_AFTER),
        Verdict::Fresh
    );
    assert_eq!(
        judge(&beat, None, interval, allowance, Some(1_200.0), QUERY_AFTER),
        Verdict::Stopped
    );
    // Not OK is dead with Icinga's reason, fresh or not.
    beat.state = ServiceState::Unknown;
    beat.check.result = Some(CheckResult {
        output: "Remote Icinga instance 'master-02' is not connected to 'master-01'".to_owned(),
        ..result(1_030.0, 1_030.0, 1_030.0)
    });
    assert_eq!(
        judge(&beat, Some(at(1_000.0)), interval, allowance, Some(1_060.0), QUERY_AFTER),
        Verdict::NotOk(
            "Remote Icinga instance 'master-02' is not connected to 'master-01'".to_owned()
        )
    );
    // Never checked: stopped.
    let never = service("icygui-hb", "beat", json!({}));
    assert_eq!(
        judge(&never, None, interval, allowance, Some(1_060.0), QUERY_AFTER),
        Verdict::Stopped
    );
}

#[test]
fn the_quiet_filter_names_every_heartbeat() {
    assert_eq!(stream_filter([]), None);
    let keys = [
        ServiceKey::new("icygui-hb-ams", "beat"),
        ServiceKey::new("odd \"host\"", "be\\at"),
    ];
    assert_eq!(
        stream_filter(&keys).unwrap(),
        r#"event.type != "CheckResult" || (event.host == "icygui-hb-ams" && event.service == "beat") || (event.host == "odd \"host\"" && event.service == "be\\at")"#
    );
}

#[test]
fn beats_follow_the_cluster_order() {
    let beat = |proves: Proves| Heartbeat {
        key: ServiceKey::new(proves.subject(), "beat"),
        proves,
        interval: Duration::from_secs(30),
        state: BeatState::OnTime,
        last_beat: None,
        last_check: None,
        since: Timestamp::EPOCH,
        deadline: None,
        allowance: MIN_ALLOWANCE,
        reason: None,
    };
    let pinned = |endpoint: &str, zone: &str| Proves::Endpoint {
        endpoint: endpoint.to_owned(),
        zone: Some(zone.to_owned()),
    };
    let mut beats = vec![
        beat(pinned("sat-fra-02", "fra")),
        beat(Proves::Zone("ams".to_owned())),
        beat(pinned("master-02", "master")),
        beat(Proves::Zone("fra".to_owned())),
        beat(pinned("sat-fra-01", "fra")),
        beat(pinned("master-01", "master")),
    ];
    order(&mut beats, |zone| usize::from(zone != Some("master")));
    let labels: Vec<String> = beats.iter().map(|beat| beat.proves.label()).collect();
    assert_eq!(
        labels,
        [
            "master-01",
            "master-02",
            "zone ams",
            "zone fra",
            "sat-fra-01",
            "sat-fra-02"
        ]
    );
}
