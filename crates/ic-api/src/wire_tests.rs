//! Deserialisation tests for the wire structs, from the real recorded
//! samples in `contract/samples/` (Icinga 2.15.6) and hand-written edge
//! cases.

use ic_model::{CommentKind, PerfdataStatus};
use serde_json::json;

use super::*;

fn sample(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../contract/samples")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn hosts_with(json: &str, detail: Detail) -> Vec<Host> {
    let results: Results<QueryResult<CheckableAttrs>> = serde_json::from_str(json).unwrap();
    results
        .results
        .into_iter()
        .filter_map(|entry| entry.attrs?.into_host(&entry.name.0, detail))
        .collect()
}

fn services_with(json: &str, detail: Detail) -> Vec<Service> {
    let results: Results<QueryResult<CheckableAttrs>> = serde_json::from_str(json).unwrap();
    results
        .results
        .into_iter()
        .filter_map(|entry| entry.attrs?.into_service(&entry.name.0, detail))
        .collect()
}

fn hosts(json: &str) -> Vec<Host> {
    hosts_with(json, Detail::Full)
}

fn services(json: &str) -> Vec<Service> {
    services_with(json, Detail::Full)
}

fn one_host(attrs: &Value) -> Host {
    let json = json!({ "results": [{ "name": "h", "type": "Host", "attrs": attrs, "joins": {}, "meta": {} }] });
    let mut found = hosts(&json.to_string());
    assert_eq!(found.len(), 1);
    found.remove(0)
}

fn one_service(attrs: &Value) -> Service {
    let json = json!({ "results": [{ "name": "h!s", "type": "Service", "attrs": attrs }] });
    let mut found = services(&json.to_string());
    assert_eq!(found.len(), 1);
    found.remove(0)
}

#[test]
fn recorded_hosts() {
    let hosts = hosts(&sample("hosts.json"));
    assert_eq!(hosts.len(), 5);
    let master = hosts
        .iter()
        .find(|h| h.name.as_str() == "icinga-master")
        .unwrap();
    assert_eq!(master.state, HostState::Up);
    assert_eq!(master.address, "127.0.0.1");
    assert_eq!(master.address6, "::1");
    assert_eq!(master.groups, ["linux-servers"]);
    assert_eq!(master.check.state_type, StateType::Hard);
    assert_eq!(master.check.max_attempts, 3);
    assert!((master.check.check_interval - 60.0).abs() < f64::EPSILON);
    assert_eq!(master.check.command_endpoint, None, "empty means local");
    assert_eq!(master.check.zone, None);
    assert_eq!(master.vars["os"], json!("Linux"));
    let result = master.check.result.as_ref().unwrap();
    assert_eq!(result.output, "PING OK - Packet loss = 0%, RTA = 0.08 ms");
    assert_eq!(result.long_output, "");
    assert_eq!(result.perfdata.len(), 2);
    assert_eq!(result.perfdata[0].label, "rta");
    assert_eq!(result.perfdata[0].unit, "ms");
    assert_eq!(result.perfdata[1].label, "pl");
    assert_eq!(result.perfdata[1].unit, "%");
    assert!(result.active);

    let down = hosts
        .iter()
        .find(|h| h.name.as_str() == "k8s-node-11")
        .unwrap();
    assert_eq!(down.state, HostState::Down);
    assert_eq!(down.check.downtime_depth, 1);
    assert!(down.is_handled());
    let db = hosts
        .iter()
        .find(|h| h.name.as_str() == "db-prod-03")
        .unwrap();
    assert_eq!(db.links.notes_url, "https://wiki.example.com/db/db-prod-03");
}

#[test]
fn recorded_services() {
    let services = services(&sample("services.json"));
    assert_eq!(services.len(), 6);
    let find = |host: &str, name: &str| {
        services
            .iter()
            .find(|s| s.key == ServiceKey::new(host, name))
            .unwrap_or_else(|| panic!("{host}!{name}"))
    };

    let pending = find("db-prod-03", "postgres-replication");
    assert_eq!(pending.state, ServiceState::Pending, "no check result yet");
    assert_eq!(pending.check.result, None);
    assert_eq!(pending.check.last_check, None, "-1 means never");
    assert_eq!(pending.check.state_type, StateType::Soft);
    assert!(!pending.check.features.active_checks);
    assert_eq!(pending.groups, ["replication"]);
    assert_eq!(pending.severity(), 16);

    let acked = find("k8s-node-07", "disk /var");
    assert_eq!(acked.state, ServiceState::Critical);
    assert_eq!(acked.check.acknowledgement, AckKind::Sticky);
    let result = acked.check.result.as_ref().unwrap();
    assert_eq!(
        result.exit_status, 2,
        "passive: the state, not exit_status 0"
    );
    assert!(!result.active);
    assert_eq!(result.perfdata[0].label, "/var");
    assert_eq!(result.perfdata[0].status(), PerfdataStatus::Critical);

    let in_downtime = find("db-prod-03", "load");
    assert_eq!(in_downtime.state, ServiceState::Warning);
    assert_eq!(in_downtime.check.downtime_depth, 1);
    assert_eq!(in_downtime.severity(), 288, "same as Icinga's own severity");

    let icinga = find("icinga-master", "icinga");
    let result = icinga.check.result.as_ref().unwrap();
    assert!(result.output.starts_with("Icinga 2 has been running"));
    assert!(result.perfdata.len() > 30, "PerfdataValue dictionaries");
    let uptime = result
        .perfdata
        .iter()
        .find(|p| p.label == "uptime")
        .unwrap();
    assert!(uptime.value.is_some_and(|v| v > 11.0));
    let latency = result
        .perfdata
        .iter()
        .find(|p| p.label == "avg_latency")
        .unwrap();
    assert_eq!(latency.value, None, "null value");
}

#[test]
fn unreachable_host() {
    let host = one_host(&json!({
        "state": 1, "state_type": 1, "last_reachable": false,
        "last_check_result": { "output": "PING CRITICAL", "state": 2, "exit_status": 2 },
    }));
    assert_eq!(host.state, HostState::Unreachable);
    assert!(!host.check.reachable);
    assert_eq!(host.name.as_str(), "h");
    assert_eq!(host.display_name, "h", "falls back to the name");
}

#[test]
fn pending_host_with_state_one_is_still_pending() {
    let host = one_host(&json!({ "state": 1, "last_check_result": null, "last_check": 0 }));
    assert_eq!(host.state, HostState::Pending);
    assert_eq!(host.check.last_check, None);
}

#[test]
fn multi_line_output_and_acknowledgement() {
    let service = one_service(&json!({
        "host_name": "db-prod-03", "name": "postgres-replication", "display_name": "PG replication",
        "state": 2.0, "state_type": 1.0, "check_attempt": 3.0, "max_check_attempts": 3,
        "acknowledgement": 1, "acknowledgement_expiry": 1_791_206_774.5,
        "downtime_depth": 2, "flapping": true, "flapping_current": 41.3,
        "last_state_change": 1_791_203_155.06, "last_hard_state_change": 1_791_203_100,
        "last_check": 1_791_203_176.09, "next_check": 1_791_203_236.09,
        "check_command": "pg_replication", "command_endpoint": "db-prod-03", "zone": "db-prod-03",
        "groups": null, "vars": null,
        "last_check_result": {
            "output": "CRITICAL - lag 412s\nprimary db-01\nstandby db-03\n",
            "performance_data": ["replication_lag=412s;60;300;0;3600", "'quoted label'=5;;;;"],
            "state": 2, "exit_status": 2,
            "schedule_start": 1_791_203_176, "execution_start": 1_791_203_176.01,
            "execution_end": 1_791_203_176.09, "check_source": "db-prod-03", "active": true
        }
    }));
    assert_eq!(
        service.key,
        ServiceKey::new("db-prod-03", "postgres-replication")
    );
    assert_eq!(service.display_name, "PG replication");
    assert_eq!(service.state, ServiceState::Critical);
    assert_eq!(service.check.attempt, 3);
    assert_eq!(service.check.acknowledgement, AckKind::Normal);
    assert_eq!(
        service.check.acknowledgement_expiry,
        Some(Timestamp::from_unix_seconds(1_791_206_774.5))
    );
    assert_eq!(service.check.downtime_depth, 2);
    assert!(service.check.flapping);
    assert_eq!(
        service.check.command_endpoint.as_deref(),
        Some("db-prod-03")
    );
    assert_eq!(
        service.check.last_hard_state_change,
        Timestamp::from_unix_seconds(1_791_203_100.0)
    );
    assert!(service.groups.is_empty(), "null groups");
    assert!(service.vars.is_empty(), "null vars");
    let result = service.check.result.as_ref().unwrap();
    assert_eq!(result.output, "CRITICAL - lag 412s");
    assert_eq!(result.long_output, "primary db-01\nstandby db-03");
    assert_eq!(result.perfdata.len(), 2);
    assert_eq!(result.perfdata[1].label, "quoted label");
    assert!((result.execution_time() - 0.08).abs() < 1e-6);
}

#[test]
fn service_name_from_the_full_name_when_attrs_lack_it() {
    let service =
        one_service(&json!({ "state": 0, "last_check_result": { "output": "OK", "state": 0 } }));
    assert_eq!(service.key, ServiceKey::new("h", "s"));
    assert_eq!(service.state, ServiceState::Ok);
}

#[test]
fn odd_values_never_fail() {
    let service = one_service(&json!({
        "state": 7, "state_type": "x", "check_attempt": -3, "max_check_attempts": 1e12,
        "acknowledgement": "2", "downtime_depth": null, "last_reachable": null,
        "enable_active_checks": 0, "enable_perfdata": "1",
        "last_check_result": { "output": 42, "state": null, "exit_status": 3, "performance_data": "a=1 b=2" },
    }));
    assert_eq!(service.state, ServiceState::Unknown, "out of range");
    assert_eq!(service.check.state_type, StateType::Hard);
    assert_eq!(service.check.attempt, 0);
    assert_eq!(service.check.max_attempts, u32::MAX);
    assert_eq!(service.check.acknowledgement, AckKind::Sticky);
    assert_eq!(service.check.downtime_depth, 0);
    assert!(service.check.reachable);
    assert!(!service.check.features.active_checks);
    assert!(service.check.features.perfdata);
    let result = service.check.result.as_ref().unwrap();
    assert_eq!(result.output, "42");
    assert_eq!(result.exit_status, 3, "exit_status when state is missing");
    assert_eq!(result.perfdata.len(), 2, "a raw perfdata string");
}

#[test]
fn recorded_lean_services() {
    // `services(Detail::Lean)` against the real Icinga 2.15.6: three
    // passive services never checked, three active ones with results.
    let services = services_with(&sample("services-lean.json"), Detail::Lean);
    assert_eq!(services.len(), 6);
    let find = |host: &str, name: &str| {
        services
            .iter()
            .find(|s| s.key == ServiceKey::new(host, name))
            .unwrap_or_else(|| panic!("{host}!{name}"))
    };
    for service in &services {
        assert_eq!(service.check.result, None, "lean: no check result");
    }

    // Icinga reports a never-checked service as state 3 (UNKNOWN): only
    // `last_check` (-1) tells that it's pending, not a problem.
    let pending = find("db-prod-03", "postgres-replication");
    assert_eq!(pending.state, ServiceState::Pending);
    assert!(!pending.is_problem());
    assert_eq!(pending.severity(), 16);
    assert_eq!(pending.check.last_check, None);
    assert!(pending.check.next_check.is_some());
    assert!(!pending.check.features.active_checks, "loaded when lean");
    assert_eq!(pending.groups, ["replication"]);
    assert_eq!(pending.vars["lag_crit"], json!(300));
    assert!((pending.check.check_interval - 60.0).abs() < f64::EPSILON);
    assert!((pending.check.retry_interval - 15.0).abs() < f64::EPSILON);

    let warning = find("db-prod-03", "load");
    assert_eq!(warning.state, ServiceState::Warning, "known without output");
    assert!(warning.check.last_check.is_some());
    assert_eq!(warning.check.output(), "");
    assert!(warning.check.features.active_checks);
    assert_eq!(warning.check.max_attempts, 2);
    assert_eq!(warning.severity(), 32 + 2048);

    let ok = find("k8s-node-07", "ping4");
    assert_eq!(ok.state, ServiceState::Ok);
    // Full-only attributes keep their defaults.
    assert_eq!(ok.check.check_command, "");
    assert_eq!(ok.check.zone, None);
    assert_eq!(ok.links, Links::default());
}

#[test]
fn lean_pending_follows_last_check_only() {
    let lean = |attrs: Value| {
        let json = json!({ "results": [{ "name": "h!s", "type": "Service", "attrs": attrs }] });
        services_with(&json.to_string(), Detail::Lean).remove(0)
    };
    assert_eq!(
        lean(json!({ "state": 3, "last_check": -1 })).state,
        ServiceState::Pending
    );
    assert_eq!(
        lean(json!({ "state": 2, "last_check": 1_791_229_357.1 })).state,
        ServiceState::Critical
    );
    // A last check at the epoch is a check (Icinga's "never" is -1).
    let epoch = lean(json!({ "state": 1, "last_check": 0 }));
    assert_eq!(epoch.state, ServiceState::Warning);
    assert_eq!(epoch.check.last_check, None, "but no time to show");
    // Without `last_check` (an Icinga that doesn't know it) the state is
    // shown rather than hidden as pending.
    assert_eq!(lean(json!({ "state": 2 })).state, ServiceState::Critical);
    // A check result in a lean answer changes nothing about pending.
    let with_result = lean(json!({
        "state": 0, "last_check": -1,
        "last_check_result": { "output": "OK", "state": 0 }
    }));
    assert_eq!(with_result.state, ServiceState::Pending);

    // The same answer read as full: pending means no check result.
    let json = json!({ "results": [{ "name": "h!s", "type": "Service",
        "attrs": { "state": 3, "last_check": 1_791_229_357.1, "last_check_result": null } }] });
    assert_eq!(
        services_with(&json.to_string(), Detail::Full)[0].state,
        ServiceState::Pending
    );
}

#[test]
fn lean_hosts() {
    let lean = |attrs: Value| {
        let json = json!({ "results": [{ "name": "h", "type": "Host", "attrs": attrs }] });
        hosts_with(&json.to_string(), Detail::Lean).remove(0)
    };
    let unreachable = lean(json!({
        "state": 1, "last_check": 1_791_229_357.1, "last_reachable": false, "address": "10.0.4.99"
    }));
    assert_eq!(unreachable.state, HostState::Unreachable);
    assert_eq!(unreachable.address, "10.0.4.99");
    assert_eq!(unreachable.check.result, None);
    let down = lean(json!({ "state": 1, "last_check": 1_791_229_357.1, "last_reachable": true }));
    assert_eq!(down.state, HostState::Down);
    let pending = lean(json!({ "state": 1, "last_check": -1, "last_reachable": true }));
    assert_eq!(pending.state, HostState::Pending);
    assert!(!pending.is_problem());
}

#[test]
fn entries_without_attrs_or_names_are_skipped() {
    let json = json!({ "results": [
        { "name": "a", "type": "Host", "code": 400, "status": "Invalid field specified: nope" },
        { "name": "", "type": "Host", "attrs": {} },
        { "name": "b", "type": "Host", "attrs": { "state": 0 } },
    ]});
    let found = hosts(&json.to_string());
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].name.as_str(), "b");
    assert_eq!(found[0].state, HostState::Pending);
}

#[test]
fn perfdata_value_dictionaries() {
    let perfdata = perfdata_from_wire(&json!([
        { "type": "PerfdataValue", "label": "time", "value": 0.004, "unit": "seconds", "warn": 1, "crit": 2.5, "min": 0, "max": null, "counter": false },
        { "type": "PerfdataValue", "label": "size", "value": 1024, "unit": "bytes", "warn": null, "crit": null },
        { "type": "PerfdataValue", "label": "load", "value": 97, "unit": "percent", "warn": "80", "crit": "@10:20" },
        { "type": "PerfdataValue", "label": "requests", "value": 12, "unit": "", "counter": true },
        { "type": "PerfdataValue", "label": "", "value": 1 },
        "rta=0.42ms;100;200;0",
        7,
    ]));
    assert_eq!(perfdata.len(), 5);
    assert_eq!(perfdata[0].unit, "s");
    assert_eq!(perfdata[0].warn.as_ref().unwrap().display(), "1");
    assert_eq!(perfdata[0].crit.as_ref().unwrap().display(), "2.5");
    assert_eq!(perfdata[0].min, Some(0.0));
    assert_eq!(perfdata[0].max, None);
    assert_eq!(perfdata[1].unit, "B");
    assert_eq!(perfdata[1].warn, None);
    assert_eq!(perfdata[2].unit, "%");
    assert_eq!(perfdata[2].status(), PerfdataStatus::Warning);
    assert!(perfdata[2].crit.as_ref().unwrap().inside);
    assert_eq!(perfdata[3].unit, "c");
    assert_eq!(perfdata[4].label, "rta");
    assert!(perfdata_from_wire(&Value::Null).is_empty());
}

fn comments(json: &str) -> Vec<Comment> {
    let results: Results<QueryResult<CommentAttrs>> = serde_json::from_str(json).unwrap();
    results
        .results
        .into_iter()
        .filter_map(|entry| entry.attrs?.into_model(&entry.name.0))
        .collect()
}

#[test]
fn recorded_comments() {
    let comments = comments(&sample("comments.json"));
    assert_eq!(comments.len(), 2);
    let ack = &comments[0];
    assert_eq!(
        ack.name,
        "k8s-node-07!disk /var!efa9ecb5-3329-40d7-8e31-8f7de5f69bf9"
    );
    assert_eq!(ack.object, ObjectKey::service("k8s-node-07", "disk /var"));
    assert_eq!(ack.kind, CommentKind::Acknowledgement);
    assert_eq!(
        ack.expire_time,
        Some(Timestamp::from_unix_seconds(1_791_206_774.0))
    );
    let user = &comments[1];
    assert_eq!(
        user.object,
        ObjectKey::host("db-prod-03"),
        "empty service_name = host"
    );
    assert_eq!(user.kind, CommentKind::User);
    assert_eq!(user.author, "j.berg");
    assert_eq!(user.expire_time, None, "0 = never");
    assert!(!user.persistent);
}

#[test]
fn comments_without_a_host_are_skipped() {
    let json = json!({ "results": [{ "name": "x", "attrs": { "text": "orphan" } }] });
    assert!(comments(&json.to_string()).is_empty());
}

fn downtimes(json: &str, now: f64) -> Vec<Downtime> {
    let results: Results<QueryResult<DowntimeAttrs>> = serde_json::from_str(json).unwrap();
    results
        .results
        .into_iter()
        .filter_map(|entry| {
            entry
                .attrs?
                .into_model(&entry.name.0, Timestamp::from_unix_seconds(now))
        })
        .collect()
}

#[test]
fn recorded_downtimes() {
    let inside = downtimes(&sample("downtimes.json"), 1_791_203_200.0);
    assert_eq!(inside.len(), 3);
    let fixed = &inside[0];
    assert_eq!(
        fixed.name,
        "k8s-node-11!e96da238-4aa1-4be6-9fb4-b2d9c7f706ca"
    );
    assert_eq!(fixed.object, ObjectKey::host("k8s-node-11"));
    assert_eq!(fixed.author, "a.ivanova");
    assert_eq!(fixed.comment, "rack maintenance");
    assert!(fixed.fixed);
    assert!(fixed.in_effect);
    assert_eq!(fixed.parent, None);
    assert!(!fixed.config_owned);
    let child = &inside[1];
    assert_eq!(
        child.parent.as_deref(),
        Some("k8s-node-11!e96da238-4aa1-4be6-9fb4-b2d9c7f706ca")
    );
    assert_eq!(child.triggered_by.as_deref(), child.parent.as_deref());
    let flexible = &inside[2];
    assert!(!flexible.fixed);
    assert!((flexible.duration - 1800.0).abs() < f64::EPSILON);
    assert_eq!(flexible.object, ObjectKey::service("db-prod-03", "load"));
    assert!(flexible.in_effect, "triggered 24 s ago, lasts 30 min");

    // Later: the fixed window (until 1791210374) is still open, the
    // flexible downtime ran out 30 minutes after it was triggered.
    let later = downtimes(&sample("downtimes.json"), 1_791_210_000.0);
    assert!(later[0].in_effect);
    assert!(!later[2].in_effect);
    // At the window's end nothing is in effect.
    let after = downtimes(&sample("downtimes.json"), 1_791_210_374.0);
    assert!(after.iter().all(|downtime| !downtime.in_effect));
}

#[test]
fn in_effect_follows_icinga() {
    let at = Timestamp::from_unix_seconds;
    // Fixed: [start, end).
    assert!(!downtime_in_effect(
        true,
        at(100.0),
        at(200.0),
        None,
        0.0,
        at(99.0)
    ));
    assert!(downtime_in_effect(
        true,
        at(100.0),
        at(200.0),
        None,
        0.0,
        at(100.0)
    ));
    assert!(!downtime_in_effect(
        true,
        at(100.0),
        at(200.0),
        None,
        0.0,
        at(200.0)
    ));
    // Flexible: only once triggered, for `duration`.
    assert!(!downtime_in_effect(
        false,
        at(100.0),
        at(200.0),
        None,
        50.0,
        at(150.0)
    ));
    assert!(downtime_in_effect(
        false,
        at(100.0),
        at(200.0),
        Some(at(150.0)),
        50.0,
        at(199.0)
    ));
    assert!(!downtime_in_effect(
        false,
        at(100.0),
        at(200.0),
        Some(at(150.0)),
        50.0,
        at(200.0)
    ));
    // A triggered flexible downtime may run past the window's end.
    assert!(downtime_in_effect(
        false,
        at(100.0),
        at(200.0),
        Some(at(190.0)),
        60.0,
        at(240.0)
    ));
}

#[test]
fn removal_ends_a_downtime_that_was_in_effect_until_then() {
    let at = Timestamp::from_unix_seconds;
    let fixed = |removed: f64| {
        downtime_in_effect_until(
            true,
            at(100.0),
            at(200.0),
            Some(at(100.0)),
            0.0,
            at(removed),
        )
    };
    assert!(!fixed(50.0), "cancelled before it started");
    assert!(fixed(150.0), "cancelled inside its window");
    // Icinga removes an expired fixed downtime at `end_time + 0.1`, or
    // later when it was busy: it was in effect until its end.
    assert!(fixed(200.1));
    assert!(fixed(5_000.0));
    assert!(
        !downtime_in_effect_until(true, at(100.0), at(100.0), None, 0.0, at(100.1)),
        "an empty window is never in effect"
    );

    let flexible = |trigger: Option<f64>, removed: f64| {
        downtime_in_effect_until(
            false,
            at(100.0),
            at(200.0),
            trigger.map(at),
            50.0,
            at(removed),
        )
    };
    assert!(!flexible(None, 150.0), "cancelled before it triggered");
    assert!(!flexible(None, 200.1), "ran out without ever triggering");
    assert!(flexible(Some(150.0), 170.0), "cancelled while in effect");
    assert!(
        flexible(Some(150.0), 200.1),
        "ran out `duration` after its trigger"
    );
    assert!(
        flexible(Some(190.0), 240.1),
        "a triggered flexible downtime may outlast its window"
    );
}

#[test]
fn removed_downtimes_from_the_payload() {
    let attrs = |json: Value| -> DowntimeAttrs { serde_json::from_value(json).unwrap() };
    let expired = attrs(json!({
        "__name": "h!a", "host_name": "h", "fixed": true,
        "start_time": 1_791_203_174, "end_time": 1_791_210_374,
        "trigger_time": 1_791_203_175.97, "remove_time": 0
    }));
    // Removed by Icinga's cleanup timer just after the end: it ended.
    let downtime = expired
        .into_removed_model("", Timestamp::from_unix_seconds(1_791_210_374.1))
        .unwrap();
    assert!(downtime.in_effect);
    assert_eq!(downtime.name, "h!a");
    let unborn = attrs(json!({
        "__name": "h!b", "host_name": "h", "fixed": true,
        "start_time": 1_791_210_000, "end_time": 1_791_220_000,
        "trigger_time": 0, "remove_time": 1_791_203_179.88
    }));
    assert!(
        !unborn
            .into_removed_model("", Timestamp::from_unix_seconds(1_791_203_179.89))
            .unwrap()
            .in_effect,
        "cancelled before its window"
    );
    let flagged =
        attrs(json!({ "host_name": "h", "name": "c", "fixed": false, "is_in_effect": true }));
    assert!(
        flagged
            .into_removed_model("", Timestamp::from_unix_seconds(1.0))
            .unwrap()
            .in_effect
    );
}

#[test]
fn downtime_flags_from_the_payload() {
    let json = json!({ "results": [
        { "name": "h!a", "attrs": { "host_name": "h", "fixed": true, "start_time": 0, "end_time": 1, "is_in_effect": true, "config_owner": "h!weekly" } },
        { "name": "h!b", "attrs": { "host_name": "h", "fixed": false, "scheduled_by": "h!weekly", "trigger_time": 0 } },
    ]});
    let found = downtimes(&json.to_string(), 5.0);
    assert!(found[0].in_effect, "is_in_effect wins when present");
    assert!(found[0].config_owned);
    assert!(!found[1].in_effect, "flexible, not triggered");
    assert!(found[1].config_owned);
    assert_eq!(found[1].trigger_time, None);
}

#[test]
fn recorded_groups_dependencies_and_endpoints() {
    let groups: Results<QueryResult<GroupAttrs>> =
        serde_json::from_str(&sample("hostgroups.json")).unwrap();
    let groups: Vec<HostGroup> = groups
        .results
        .into_iter()
        .filter_map(|entry| entry.attrs?.into_host_group(&entry.name.0))
        .collect();
    assert_eq!(groups.len(), 4);
    assert!(groups.contains(&HostGroup {
        name: "databases".to_owned(),
        display_name: "Databases".to_owned()
    }));

    let groups: Results<QueryResult<GroupAttrs>> =
        serde_json::from_str(&sample("servicegroups.json")).unwrap();
    let groups: Vec<ServiceGroup> = groups
        .results
        .into_iter()
        .filter_map(|entry| entry.attrs?.into_service_group(&entry.name.0))
        .collect();
    assert!(
        groups
            .iter()
            .any(|g| g.name == "ping" && g.display_name == "Ping Checks")
    );

    let dependencies: Results<QueryResult<DependencyAttrs>> =
        serde_json::from_str(&sample("dependencies.json")).unwrap();
    let dependencies: Vec<Dependency> = dependencies
        .results
        .into_iter()
        .filter_map(|entry| entry.attrs?.into_model(&entry.name.0))
        .collect();
    assert_eq!(
        dependencies,
        [Dependency {
            name: "behind-node-11!behind-node-11-parent".to_owned(),
            child: ObjectKey::host("behind-node-11"),
            parent: ObjectKey::host("k8s-node-11"),
        }]
    );

    let zones: Results<QueryResult<ZoneAttrs>> =
        serde_json::from_str(&sample("zones.json")).unwrap();
    let members: Vec<(String, Vec<String>)> = zones
        .results
        .into_iter()
        .filter_map(|zone| Some((zone.name.0, zone.attrs?.endpoints.0)))
        .collect();
    let zone_of = |name: &str| {
        members
            .iter()
            .find(|(_, endpoints)| endpoints.iter().any(|e| e == name))
            .map(|(zone, _)| zone.clone())
    };
    let map = |local: Option<&str>| -> Vec<Endpoint> {
        let endpoints: Results<QueryResult<EndpointAttrs>> =
            serde_json::from_str(&sample("endpoints.json")).unwrap();
        endpoints
            .results
            .into_iter()
            .filter_map(|entry| entry.attrs?.into_model(&entry.name.0, zone_of, local))
            .collect()
    };
    assert_eq!(
        map(None),
        [Endpoint {
            name: "icinga-master".to_owned(),
            zone: "master".to_owned(),
            connected: false,
        }],
        "Icinga reports its own endpoint as not connected"
    );
    let node = node_name(&status_of("status-icingaapplication.json"));
    assert_eq!(node.as_deref(), Some("icinga-master"));
    assert!(
        map(node.as_deref())[0].connected,
        "the endpoint we talk to is connected"
    );
    assert!(!map(Some("other-master"))[0].connected);
}

#[test]
fn node_name_needs_a_name() {
    assert_eq!(node_name(&Value::Null), None);
    assert_eq!(
        node_name(&json!({ "icingaapplication": { "app": { "node_name": "" } } })),
        None
    );
}

fn status_of(name: &str) -> Value {
    let results: Results<StatusResult> = serde_json::from_str(&sample(name)).unwrap();
    results
        .results
        .into_iter()
        .next()
        .unwrap()
        .status
        .0
        .unwrap()
}

#[test]
fn recorded_status() {
    let status = instance_status(
        &status_of("status-icingaapplication.json"),
        &status_of("status-cib.json"),
    );
    assert_eq!(status.node_name, "icinga-master");
    assert_eq!(status.version, "v2.15.6");
    assert_eq!(
        status.program_start,
        Timestamp::from_unix_seconds(1_791_203_139.104_647)
    );
    assert!(status.notifications_enabled);
    assert!(status.flap_detection_enabled);
    assert!(
        (status.checks_per_minute - 13.0).abs() < f64::EPSILON,
        "7 host + 6 service checks"
    );
    assert!((status.avg_latency - 0.418_276_786_804_199_2).abs() < 1e-9);
    assert!(status.avg_execution_time > 0.0);
}

#[test]
fn status_tolerates_missing_parts() {
    let status = instance_status(
        &json!({ "icingaapplication": { "app": { "enable_notifications": false, "pid": 1.0 } } }),
        &Value::Null,
    );
    assert!(!status.notifications_enabled);
    assert!(status.host_checks_enabled, "missing switches default to on");
    assert_eq!(status.node_name, "");
    assert!(status.checks_per_minute.abs() < f64::EPSILON);
}

#[test]
fn recorded_info() {
    let results: Results<InfoResult> = serde_json::from_str(&sample("info.json")).unwrap();
    let info = results.results.into_iter().next().unwrap();
    assert_eq!(info.user.0, "icygui");
    assert_eq!(info.version.0, "v2.15.6");
    assert_eq!(
        info.permissions.0,
        ["objects/query/*", "status/query", "events/*", "actions/*"]
    );
}

#[test]
fn recorded_action_results() {
    let results: Results<ActionResultWire> =
        serde_json::from_str(&sample("action-schedule-downtime.json")).unwrap();
    assert!((results.results[0].code.0 - 200.0).abs() < f64::EPSILON);
    assert_eq!(
        results.results[0].name.0,
        "k8s-node-11!e96da238-4aa1-4be6-9fb4-b2d9c7f706ca"
    );
    let results: Results<ActionResultWire> =
        serde_json::from_str(&sample("action-execute-command.json")).unwrap();
    assert!((results.results[0].code.0 - 404.0).abs() < f64::EPSILON);
    assert_eq!(
        results.results[0].status.0,
        "Can't find a valid endpoint for ''."
    );
    assert_eq!(results.results[0].name.0, "");
    // The docs' older float form.
    let results: Results<ActionResultWire> =
        serde_json::from_str(r#"{"results":[{"code":200.0,"status":"ok","legacy_id":26.0}]}"#)
            .unwrap();
    assert!((results.results[0].code.0 - 200.0).abs() < f64::EPSILON);
}
