//! Wire fidelity against a real Icinga: responses recorded from Icinga
//! v2.15.6 (`tests/fixtures/icinga-2.15.6`, copied from the repository's
//! `contract/samples`) must have the same shape as the mock's: the same
//! keys (an object may lack only keys that some sample object lacks too),
//! the same JSON kinds per key, and whole numbers written as integers.
//! `queries.json` holds whole exchanges (request, status, answer), which
//! are replayed against the mock: attribute selection, joins, meta, name
//! lists, unknown attributes, filters and request flags.
//!
//! Every mismatch is collected, so one run lists all of them.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code fails loudly"
)]

mod common;

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use common::{EventStream, get, json, post, request, results, start};
use ic_mock::{MockConfig, MockServer, MockUser};
use reqwest::{Client, Method, StatusCode};
use serde_json::{Value, json};

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/icinga-2.15.6")
}

fn sample(name: &str) -> Value {
    let path = fixture_dir().join(name);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn sample_events() -> Vec<Value> {
    let path = fixture_dir().join("events.ndjson");
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Keys that are legitimately `null` for some objects even if no sample
/// object shows it (objects without custom variables, for example).
const NULLABLE: &[&str] = &["vars", "last_check_result", "vars_before", "executions"];

/// The shape of one kind of JSON object, learned from samples.
#[derive(Debug, Default)]
struct Shape {
    required: Option<BTreeSet<String>>,
    allowed: BTreeSet<String>,
    kinds: BTreeMap<String, BTreeSet<&'static str>>,
}

impl Shape {
    fn learn<'a>(objects: impl IntoIterator<Item = &'a Value>) -> Self {
        let mut shape = Self::default();
        for object in objects {
            let map = object.as_object().expect("sample object");
            let keys: BTreeSet<String> = map.keys().cloned().collect();
            shape.required = Some(match shape.required.take() {
                None => keys.clone(),
                Some(required) => required.intersection(&keys).cloned().collect(),
            });
            shape.allowed.extend(keys);
            for (key, value) in map {
                shape
                    .kinds
                    .entry(key.clone())
                    .or_default()
                    .insert(kind(value));
            }
        }
        assert!(shape.required.is_some(), "no samples to learn from");
        shape
    }

    fn check<'a>(
        &self,
        what: &str,
        objects: impl IntoIterator<Item = &'a Value>,
        errors: &mut Vec<String>,
    ) -> usize {
        let required = self.required.clone().unwrap_or_default();
        let mut seen = 0;
        for object in objects {
            seen += 1;
            let Some(map) = object.as_object() else {
                errors.push(format!("{what}: not an object: {object}"));
                continue;
            };
            let name = map
                .get("__name")
                .or_else(|| map.get("name"))
                .or_else(|| map.get("type"))
                .and_then(Value::as_str)
                .unwrap_or("?");
            let keys: BTreeSet<String> = map.keys().cloned().collect();
            for missing in required.difference(&keys) {
                errors.push(format!("{what} ({name}): missing key '{missing}'"));
            }
            for extra in keys.difference(&self.allowed) {
                errors.push(format!("{what} ({name}): unexpected key '{extra}'"));
            }
            for (key, value) in map {
                let Some(kinds) = self.kinds.get(key) else {
                    continue;
                };
                let actual = kind(value);
                let only_null = kinds.len() == 1 && kinds.contains("null");
                let fine = kinds.contains(actual)
                    || only_null
                    || (actual == "null" && NULLABLE.contains(&key.as_str()));
                if !fine {
                    errors.push(format!(
                        "{what} ({name}): '{key}' is {actual}, Icinga sends {kinds:?}"
                    ));
                }
            }
        }
        if seen == 0 {
            errors.push(format!("{what}: the mock produced no objects to compare"));
        }
        seen
    }
}

/// Whole numbers must be integers (`2`, not `2.0`), like Icinga 2.13+.
fn check_numbers(what: &str, value: &Value, errors: &mut Vec<String>) {
    match value {
        Value::Number(number) => {
            if number.is_f64()
                && let Some(float) = number.as_f64()
                && float.fract() == 0.0
            {
                errors.push(format!("{what}: whole number written as float: {number}"));
            }
        }
        Value::Array(items) => {
            for item in items {
                check_numbers(what, item, errors);
            }
        }
        Value::Object(map) => {
            for (key, item) in map {
                check_numbers(&format!("{what}.{key}"), item, errors);
            }
        }
        Value::Null | Value::Bool(_) | Value::String(_) => {}
    }
}

/// Every check result in the samples: objects' `last_check_result` and
/// events' `check_result`.
fn sample_check_results() -> Vec<Value> {
    let hosts = sample("hosts.json");
    let services = sample("services.json");
    attrs(&hosts)
        .into_iter()
        .chain(attrs(&services))
        .map(|a| a["last_check_result"].clone())
        .chain(
            sample_events()
                .into_iter()
                .filter_map(|e| e.get("check_result").cloned()),
        )
        .filter(|r| !r.is_null())
        .collect()
}

/// The non-null `vars_after` / `vars_before` of check results.
fn check_result_vars(results: &[&Value]) -> Vec<Value> {
    results
        .iter()
        .flat_map(|r| [r["vars_after"].clone(), r["vars_before"].clone()])
        .filter(|v| !v.is_null())
        .collect()
}

/// Checks check results (and their `vars_*`) against the samples' shape.
fn check_check_results(what: &str, results: &[&Value], errors: &mut Vec<String>) {
    let samples = sample_check_results();
    let samples: Vec<&Value> = samples.iter().collect();
    Shape::learn(samples.iter().copied()).check(what, results.iter().copied(), errors);
    Shape::learn(&check_result_vars(&samples)).check(
        &format!("{what} vars_after/vars_before"),
        &check_result_vars(results),
        errors,
    );
}

fn attrs(body: &Value) -> Vec<&Value> {
    body["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| &r["attrs"])
        .collect()
}

fn non_null<'a>(values: impl IntoIterator<Item = &'a Value>) -> Vec<&'a Value> {
    values.into_iter().filter(|v| !v.is_null()).collect()
}

async fn mock_get(
    client: &Client,
    server: &MockServer,
    path: &str,
    errors: &mut Vec<String>,
) -> Value {
    let (status, body) = get(client, server, path).await;
    if status != StatusCode::OK {
        errors.push(format!("GET {path}: {status} {body}"));
    }
    check_numbers(path, &body, errors);
    body
}

fn report(errors: &[String]) {
    assert!(
        errors.is_empty(),
        "{} differences from real Icinga 2.15.6:\n  {}",
        errors.len(),
        errors.join("\n  ")
    );
}

#[tokio::test]
async fn objects_have_the_shape_of_real_icinga() {
    let mut errors = Vec::new();
    let (prod, prod_client) = common::prod().await;
    let (lab, lab_client) = common::lab().await;

    // Hosts and services, including pending ones (lab).
    let mut host_attrs = Vec::new();
    let mut service_attrs = Vec::new();
    let mut entries = Vec::new();
    for (server, client) in [(&prod, &prod_client), (&lab, &lab_client)] {
        let hosts = mock_get(client, server, "/v1/objects/hosts", &mut errors).await;
        let services = mock_get(client, server, "/v1/objects/services", &mut errors).await;
        entries.extend(results(&hosts).clone());
        entries.extend(results(&services).clone());
        host_attrs.extend(attrs(&hosts).into_iter().cloned());
        service_attrs.extend(attrs(&services).into_iter().cloned());
    }
    let hosts_sample = sample("hosts.json");
    let services_sample = sample("services.json");
    Shape::learn(
        results(&hosts_sample)
            .iter()
            .chain(results(&services_sample)),
    )
    .check("result entry", &entries, &mut errors);
    Shape::learn(attrs(&hosts_sample)).check("Host", &host_attrs, &mut errors);
    Shape::learn(attrs(&services_sample)).check("Service", &service_attrs, &mut errors);

    // Check results and their nested parts, learned from every sample
    // that carries one (objects and events).
    let mock_results: Vec<&Value> = non_null(
        host_attrs
            .iter()
            .chain(&service_attrs)
            .map(|a| &a["last_check_result"]),
    );
    check_check_results("last_check_result", &mock_results, &mut errors);
    let locations = |objects: &[Value]| -> Vec<Value> {
        objects
            .iter()
            .map(|a| a["source_location"].clone())
            .collect()
    };
    Shape::learn(&locations(
        &attrs(&hosts_sample)
            .into_iter()
            .cloned()
            .collect::<Vec<_>>(),
    ))
    .check("source_location", &locations(&host_attrs), &mut errors);

    check_pending(&host_attrs, &service_attrs, &services_sample, &mut errors);

    // Every other object type.
    for (file, path) in [
        ("comments.json", "/v1/objects/comments"),
        ("downtimes.json", "/v1/objects/downtimes"),
        ("hostgroups.json", "/v1/objects/hostgroups"),
        ("servicegroups.json", "/v1/objects/servicegroups"),
        ("dependencies.json", "/v1/objects/dependencies"),
        ("endpoints.json", "/v1/objects/endpoints"),
        ("zones.json", "/v1/objects/zones"),
    ] {
        let body = mock_get(&prod_client, &prod, path, &mut errors).await;
        let shape = Shape::learn(attrs(&sample(file)));
        shape.check(file, attrs(&body), &mut errors);
        let sample_body = sample(file);
        Shape::learn(&locations(
            &attrs(&sample_body).into_iter().cloned().collect::<Vec<_>>(),
        ))
        .check(
            &format!("{file} source_location"),
            &locations(&attrs(&body).into_iter().cloned().collect::<Vec<_>>()),
            &mut errors,
        );
    }
    report(&errors);
}

/// Pending objects look like Icinga's: no result, `last_check` -1, the
/// default raw state UNKNOWN (3), which a host reports as DOWN (1), and no
/// state change ever (like the recorded pending service).
fn check_pending(
    host_attrs: &[Value],
    service_attrs: &[Value],
    services_sample: &Value,
    errors: &mut Vec<String>,
) {
    let recorded_pending = attrs(services_sample)
        .into_iter()
        .find(|a| a["last_check_result"].is_null())
        .expect("a pending service in the recording")
        .clone();
    for (what, objects, state) in [("service", service_attrs, 3), ("host", host_attrs, 1)] {
        let pending: Vec<&Value> = objects
            .iter()
            .filter(|a| a["last_check_result"].is_null())
            .collect();
        assert!(!pending.is_empty(), "lab has a pending {what}");
        for object in pending {
            if object["last_check"] != json!(-1)
                || object["state"] != json!(state)
                || object["state_type"] != json!(0)
            {
                errors.push(format!(
                    "pending {what} {}: last_check {} state {} state_type {} (Icinga: -1, {state}, 0)",
                    object["__name"], object["last_check"], object["state"], object["state_type"]
                ));
            }
            for key in [
                "last_state_change",
                "last_hard_state_change",
                "previous_state_change",
                "check_attempt",
            ] {
                if object[key] != recorded_pending[key] {
                    errors.push(format!(
                        "pending {what} {}: {key} {} (Icinga: {})",
                        object["__name"], object[key], recorded_pending[key]
                    ));
                }
            }
        }
    }
}

#[tokio::test]
async fn status_and_info_have_the_shape_of_real_icinga() {
    let mut errors = Vec::new();
    let config = MockConfig {
        users: vec![MockUser::new(
            "icygui",
            "icygui-test",
            &["objects/query/*", "status/query", "events/*", "actions/*"],
        )],
        ..MockConfig::with_scenario(ic_mock::scenarios::prod_cluster())
    };
    let (server, client) = start(config).await;
    let as_icygui = |path: &str| {
        client
            .get(format!("{}{path}", server.url()))
            .basic_auth("icygui", Some("icygui-test"))
            .header("Accept", "application/json")
    };

    let (_, info) = json(as_icygui("/v1").send().await.unwrap()).await;
    let info_sample = sample("info.json");
    Shape::learn(results(&info_sample)).check("info", results(&info), &mut errors);
    if results(&info)[0]["permissions"] != results(&info_sample)[0]["permissions"] {
        errors.push(format!(
            "info permissions: {}",
            results(&info)[0]["permissions"]
        ));
    }

    let (_, app) = json(
        as_icygui("/v1/status/IcingaApplication")
            .send()
            .await
            .unwrap(),
    )
    .await;
    check_numbers("IcingaApplication", &app, &mut errors);
    let app_sample = sample("status-icingaapplication.json");
    Shape::learn(results(&app_sample)).check("status entry", results(&app), &mut errors);
    Shape::learn([&results(&app_sample)[0]["status"]["icingaapplication"]["app"]]).check(
        "icingaapplication.app",
        [&results(&app)[0]["status"]["icingaapplication"]["app"]],
        &mut errors,
    );

    let (_, cib) = json(as_icygui("/v1/status/CIB").send().await.unwrap()).await;
    check_numbers("CIB", &cib, &mut errors);
    let cib_sample = sample("status-cib.json");
    Shape::learn([&results(&cib_sample)[0]["status"]]).check(
        "CIB status",
        [&results(&cib)[0]["status"]],
        &mut errors,
    );
    report(&errors);
}

#[tokio::test]
async fn errors_have_the_bodies_of_real_icinga() {
    let errors = RefCell::new(Vec::new());
    let config = MockConfig {
        users: vec![
            MockUser::root(),
            MockUser::new(
                "icygui",
                "icygui-test",
                &["objects/query/*", "status/query", "events/*", "actions/*"],
            ),
            MockUser::new(
                "viewer",
                "viewer-test",
                &["objects/query/Host", "objects/query/Service"],
            ),
        ],
        ..MockConfig::with_scenario(ic_mock::scenarios::prod_cluster())
    };
    let (server, client) = start(config).await;
    let call = |user: &str, password: &str, method: Method, path: &str, body: Value| {
        client
            .request(method, format!("{}{path}", server.url()))
            .basic_auth(user, Some(password))
            .header("Accept", "application/json")
            .json(&body)
    };
    let expect = |file: &str, (status, body): (StatusCode, Value), code: u16| {
        if status.as_u16() != code {
            errors
                .borrow_mut()
                .push(format!("{file}: HTTP {status}, Icinga {code}"));
        }
        if body != sample(file) {
            errors
                .borrow_mut()
                .push(format!("{file}: {body}, Icinga {}", sample(file)));
        }
    };

    // 401, with Icinga's headers.
    let response = call("icygui", "wrong", Method::GET, "/v1", json!({}))
        .send()
        .await
        .unwrap();
    let headers = response.headers().clone();
    expect("error-401.json", json(response).await, 401);
    for header in [
        "server",
        "www-authenticate",
        "connection",
        "content-type",
        "content-length",
    ] {
        if !headers.contains_key(header) {
            errors
                .borrow_mut()
                .push(format!("401: missing header {header}"));
        }
    }
    let recorded = std::fs::read_to_string(fixture_dir().join("error-401.headers")).unwrap();
    for (name, value) in [
        ("www-authenticate", "Basic realm=\"Icinga 2\""),
        ("connection", "close"),
        ("content-type", "application/json"),
    ] {
        assert!(
            recorded
                .to_lowercase()
                .contains(&format!("{name}: {value}").to_lowercase())
        );
        if headers[name] != value {
            errors
                .borrow_mut()
                .push(format!("401 header {name}: {:?}", headers[name]));
        }
    }

    // 403 for a missing action permission and for filters without
    // `filter-expression`.
    let viewer = call(
        "viewer",
        "viewer-test",
        Method::POST,
        "/v1/actions/acknowledge-problem",
        json!({"type": "Service", "services": ["db-prod-03!postgres-replication"], "author": "a", "comment": "c"}),
    );
    expect(
        "error-403-action.json",
        json(viewer.send().await.unwrap()).await,
        403,
    );
    let filtered = call(
        "icygui",
        "icygui-test",
        Method::POST,
        "/v1/objects/services",
        json!({"filter": "service.state == 2"}),
    )
    .header("X-HTTP-Method-Override", "GET");
    expect(
        "error-403-filter-expression.json",
        json(filtered.send().await.unwrap()).await,
        403,
    );

    // 404 when one name of a list doesn't exist (queries and actions).
    let missing = call(
        "icygui",
        "icygui-test",
        Method::POST,
        "/v1/objects/services",
        json!({"services": ["db-prod-03!postgres-replication", "db-prod-03!no-such-service"]}),
    )
    .header("X-HTTP-Method-Override", "GET");
    expect(
        "error-404-missing-name.json",
        json(missing.send().await.unwrap()).await,
        404,
    );
    let action = call(
        "icygui",
        "icygui-test",
        Method::POST,
        "/v1/actions/reschedule-check",
        json!({"type": "Service", "services": ["db-prod-03!no-such-service"]}),
    );
    expect(
        "error-404-action.json",
        json(action.send().await.unwrap()).await,
        404,
    );
    report(&errors.borrow());
}

/// Actions on objects like the recorded ones: same result keys and codes.
#[tokio::test]
async fn action_results_have_the_shape_of_real_icinga() {
    let errors = RefCell::new(Vec::new());
    let (server, client) = common::prod().await;
    let now = server.control().now().as_unix_seconds();
    let run = async |file: &str, action: &str, body: Value| -> Value {
        let (status, response) =
            post(&client, &server, &format!("/v1/actions/{action}"), &body).await;
        let mut errors = errors.borrow_mut();
        check_numbers(file, &response, &mut errors);
        let sample = sample(file);
        let code = results(&sample)[0]["code"].as_u64().unwrap();
        if u64::from(status.as_u16()) != code {
            errors.push(format!("{file}: HTTP {status}, Icinga {code}: {response}"));
        }
        Shape::learn(results(&sample)).check(file, results(&response), &mut errors);
        response
    };
    let service = "k8s-node-07!disk /var";
    run(
        "action-process-check-result.json",
        "process-check-result",
        json!({"type": "Service", "services": [service], "exit_status": 2, "plugin_output": "DISK CRITICAL", "performance_data": ["/var=97%;80;90;0;100"]}),
    )
    .await;
    // Make sure it can be acknowledged (no existing acknowledgement).
    run(
        "action-remove-acknowledgement.json",
        "remove-acknowledgement",
        json!({"type": "Service", "services": [service]}),
    )
    .await;
    run(
        "action-acknowledge.json",
        "acknowledge-problem",
        json!({"type": "Service", "services": [service], "author": "icygui", "comment": "cleaning up logs", "sticky": true, "expiry": now + 3600.0}),
    )
    .await;
    let added = run(
        "action-add-comment.json",
        "add-comment",
        json!({"type": "Host", "hosts": ["db-prod-03"], "author": "j.berg", "comment": "Failover drill"}),
    )
    .await;
    let comment = results(&added)[0]["name"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    run(
        "action-remove-comment.json",
        "remove-comment",
        json!({"comment": comment}),
    )
    .await;
    let scheduled = run(
        "action-schedule-downtime.json",
        "schedule-downtime",
        json!({"type": "Host", "hosts": ["sw-core-fra-02"], "author": "a.ivanova", "comment": "rack maintenance", "start_time": now, "end_time": now + 7200.0, "fixed": true, "all_services": true, "child_options": "DowntimeTriggeredChildren"}),
    )
    .await;
    if let Some(children) = results(&scheduled)[0]["child_downtimes"].as_array() {
        let sample = sample("action-schedule-downtime.json");
        Shape::learn(results(&sample)[0]["child_downtimes"].as_array().unwrap()).check(
            "child_downtimes",
            children,
            &mut errors.borrow_mut(),
        );
    } else {
        errors
            .borrow_mut()
            .push("schedule-downtime: no child_downtimes".to_owned());
    }
    let downtime = results(&scheduled)[0]["name"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    run(
        "action-schedule-downtime-flexible.json",
        "schedule-downtime",
        json!({"type": "Service", "services": ["db-prod-01!load"], "author": "a.ivanova", "comment": "flexible window", "start_time": now, "end_time": now + 7200.0, "fixed": false, "duration": 1800}),
    )
    .await;
    run(
        "action-remove-downtime.json",
        "remove-downtime",
        json!({"downtime": downtime}),
    )
    .await;
    run(
        "action-reschedule-check.json",
        "reschedule-check",
        json!({"type": "Service", "services": ["db-prod-03!load"], "force": true}),
    )
    .await;
    run(
        "action-execute-command.json",
        "execute-command",
        json!({"type": "Host", "hosts": ["db-prod-03"], "command_type": "CheckCommand", "command": "hostalive", "ttl": 30}),
    )
    .await;
    report(&errors.borrow());
}

/// The same operations as in the recording produce events of the same
/// shape (and every recorded event type).
#[tokio::test]
async fn events_have_the_shape_of_real_icinga() {
    let mut errors = Vec::new();
    let (server, client) = common::prod().await;
    let samples = sample_events();
    let types: BTreeSet<String> = samples
        .iter()
        .map(|e| e["type"].as_str().unwrap().to_owned())
        .collect();
    let mut stream = EventStream::open(
        &client,
        &server,
        &json!({"types": types, "queue": "fidelity"}),
    )
    .await;
    run_recorded_operations(&client, &server, &mut errors).await;
    // An active check result as well.
    server.control().step_simulation(120);
    let mut received: Vec<Value> = Vec::new();
    while let Ok(Some(line)) =
        tokio::time::timeout(std::time::Duration::from_millis(300), stream.next_line()).await
    {
        let event: Value = serde_json::from_str(&line).unwrap();
        check_numbers("event", &event, &mut errors);
        received.push(event);
    }
    for ty in &types {
        let of_type = |events: &[Value]| -> Vec<Value> {
            events
                .iter()
                .filter(|e| e["type"] == *ty)
                .cloned()
                .collect()
        };
        let expected = of_type(&samples);
        let actual = of_type(&received);
        if actual.is_empty() {
            errors.push(format!("no {ty} event from the mock"));
            continue;
        }
        Shape::learn(&expected).check(&format!("{ty} event"), &actual, &mut errors);
        let results: Vec<&Value> = non_null(actual.iter().filter_map(|e| e.get("check_result")));
        if !results.is_empty() {
            check_check_results(&format!("{ty} check_result"), &results, &mut errors);
        }
        for nested in ["comment", "downtime"] {
            let sample_nested: Vec<Value> = expected
                .iter()
                .filter_map(|e| e.get(nested).filter(|v| v.is_object()).cloned())
                .collect();
            if sample_nested.is_empty() {
                continue;
            }
            let actual_nested: Vec<Value> = actual
                .iter()
                .filter_map(|e| e.get(nested).filter(|v| v.is_object()).cloned())
                .collect();
            Shape::learn(&sample_nested).check(
                &format!("{ty}.{nested}"),
                &actual_nested,
                &mut errors,
            );
        }
    }
    report(&errors);
}

/// The operations of the recording (a passive result, an acknowledgement,
/// comments, fixed and flexible downtimes, their removal), which together
/// emit every recorded event type.
async fn run_recorded_operations(client: &Client, server: &MockServer, errors: &mut Vec<String>) {
    let now = server.control().now().as_unix_seconds();
    let actions: Vec<(&str, Value)> = vec![
        (
            "process-check-result",
            json!({"type": "Service", "services": ["web-prod-02!ssh"], "exit_status": 2, "plugin_output": "SSH CRITICAL", "performance_data": ["time=10s;;;0"]}),
        ),
        (
            "acknowledge-problem",
            json!({"type": "Service", "services": ["web-prod-02!ssh"], "author": "icygui", "comment": "on it", "sticky": true, "expiry": now + 3600.0}),
        ),
        (
            "add-comment",
            json!({"type": "Host", "hosts": ["db-prod-03"], "author": "j.berg", "comment": "drill"}),
        ),
        (
            "schedule-downtime",
            json!({"type": "Host", "hosts": ["sw-core-fra-02"], "author": "a.ivanova", "comment": "rack", "start_time": now - 60.0, "end_time": now + 7200.0, "child_options": 1}),
        ),
        (
            "schedule-downtime",
            json!({"type": "Service", "services": ["db-prod-01!load"], "author": "a.ivanova", "comment": "flex", "start_time": now - 60.0, "end_time": now + 7200.0, "fixed": false, "duration": 1800}),
        ),
        (
            "remove-acknowledgement",
            json!({"type": "Service", "services": ["web-prod-02!ssh"]}),
        ),
        (
            "remove-comment",
            json!({"type": "Host", "hosts": ["db-prod-03"]}),
        ),
        (
            "remove-downtime",
            json!({"type": "Host", "hosts": ["sw-core-fra-02"]}),
        ),
    ];
    for (action, body) in actions {
        let (status, response) =
            post(client, server, &format!("/v1/actions/{action}"), &body).await;
        if !status.is_success() {
            errors.push(format!("{action}: {status} {response}"));
        }
    }
}

/// The attributes of `ic_api::Detail` (docs/architecture.md), as
/// `contract/record-queries.py` requests them.
const LEAN: &[&str] = &[
    "name",
    "display_name",
    "state",
    "state_type",
    "last_state_change",
    "last_hard_state_change",
    "last_check",
    "next_check",
    "next_update",
    "check_attempt",
    "max_check_attempts",
    "acknowledgement",
    "acknowledgement_expiry",
    "downtime_depth",
    "flapping",
    "last_reachable",
    "check_interval",
    "retry_interval",
    "groups",
    "vars",
];

/// What `Detail::Full` adds to [`LEAN`].
const FULL: &[&str] = &[
    "last_check_result",
    "check_command",
    "command_endpoint",
    "zone",
    "enable_active_checks",
    "enable_passive_checks",
    "enable_notifications",
    "enable_event_handler",
    "enable_flapping",
    "enable_perfdata",
    "flapping_current",
    "notes",
    "notes_url",
    "action_url",
    "icon_image",
];

/// Lean and Full queries carry exactly the selected attributes for every
/// object (pending ones included), with no joins and no meta.
#[tokio::test]
async fn detail_selections_are_exact() {
    let mut errors = Vec::new();
    let (prod, prod_client) = common::prod().await;
    let (lab, lab_client) = common::lab().await;
    for (plural, own) in [
        ("hosts", ["address", "address6"].as_slice()),
        ("services", &["host_name"]),
    ] {
        let lean: Vec<&str> = LEAN.iter().chain(own).copied().collect();
        let full: Vec<&str> = lean.iter().chain(FULL).copied().collect();
        for attrs in [lean, full] {
            for (server, client) in [(&prod, &prod_client), (&lab, &lab_client)] {
                let response = request(
                    client,
                    server,
                    Method::POST,
                    &format!("/v1/objects/{plural}"),
                )
                .header("X-HTTP-Method-Override", "GET")
                .json(&json!({ "attrs": attrs }))
                .send()
                .await
                .unwrap();
                let (status, body) = json(response).await;
                assert_eq!(status, StatusCode::OK, "{body}");
                check_numbers(plural, &body, &mut errors);
                let expected: BTreeSet<&str> = attrs.iter().copied().collect();
                for entry in results(&body) {
                    let keys: BTreeSet<&str> = entry["attrs"]
                        .as_object()
                        .unwrap()
                        .keys()
                        .map(String::as_str)
                        .collect();
                    if keys != expected {
                        errors.push(format!(
                            "{plural} {}: keys {:?}",
                            entry["name"],
                            keys.symmetric_difference(&expected).collect::<Vec<_>>()
                        ));
                    }
                    if entry["joins"] != json!({}) || entry["meta"] != json!({}) {
                        errors.push(format!("{plural} {}: joins or meta", entry["name"]));
                    }
                }
            }
        }
    }
    report(&errors);
}

/// The users of the contract instance: `root` with every permission and
/// `icygui` without `filter-expression`.
fn contract_users() -> MockConfig {
    MockConfig {
        users: vec![
            MockUser::root(),
            MockUser::new(
                "icygui",
                "icygui-test",
                &["objects/query/*", "status/query", "events/*", "actions/*"],
            ),
        ],
        ..MockConfig::with_scenario(ic_mock::scenarios::prod_cluster())
    }
}

/// Object, status and parameter exchanges recorded from Icinga 2.15.6
/// (`queries.json`, written by `contract/record-queries.py`): attribute
/// selection with the Lean and Full sets, joins, meta, name lists
/// (all-or-nothing 404 included), unknown attributes, filters and flags.
/// Each request is replayed against the mock and the answer compared as
/// the exchange's `compare` says (see the recorder).
#[tokio::test]
async fn queries_answer_like_real_icinga() {
    let mut errors = Vec::new();
    let (server, client) = start(contract_users()).await;
    let exchanges = sample("queries.json");
    let exchanges = exchanges.as_array().unwrap();
    assert!(exchanges.len() > 50, "the recording is complete");
    for exchange in exchanges {
        let name = exchange["name"].as_str().unwrap();
        let (user, password) = match exchange["user"].as_str().unwrap() {
            "root" => ("root", "icinga"),
            _ => ("icygui", "icygui-test"),
        };
        let response = client
            .post(format!(
                "{}{}",
                server.url(),
                exchange["path"].as_str().unwrap()
            ))
            .basic_auth(user, Some(password))
            .header("Accept", "application/json")
            .header("X-HTTP-Method-Override", "GET")
            .json(&exchange["body"])
            .send()
            .await
            .unwrap();
        let (status, body) = json(response).await;
        let recorded = &exchange["response"];
        if u64::from(status.as_u16()) != exchange["status"].as_u64().unwrap() {
            errors.push(format!(
                "{name}: HTTP {status}, Icinga {}: {body}",
                exchange["status"]
            ));
            continue;
        }
        check_numbers(name, &body, &mut errors);
        match exchange["compare"].as_str().unwrap() {
            "exact" => {
                if body != *recorded {
                    errors.push(format!("{name}: {body}\n    Icinga: {recorded}"));
                }
            }
            compare @ ("error" | "error-text") => {
                compare_errors(name, &body, recorded, compare == "error-text", &mut errors);
            }
            compare @ ("names" | "shape") => {
                if compare == "names" {
                    let names = |body: &Value| -> Vec<Value> {
                        results(body).iter().map(|r| r["name"].clone()).collect()
                    };
                    if names(&body) != names(recorded) {
                        errors.push(format!(
                            "{name}: names {:?}, Icinga {:?}",
                            names(&body),
                            names(recorded)
                        ));
                    }
                }
                compare_shapes(name, results(&body), results(recorded), &mut errors);
            }
            other => panic!("{name}: unknown comparison {other}"),
        }
    }
    report(&errors);
}

/// Error bodies: the same apart from the text of `diagnostic_information`
/// (`ic-filter` words filter errors its own way), which must be there when
/// Icinga sent it; with `text`, its first line must match too.
fn compare_errors(
    name: &str,
    body: &Value,
    recorded: &Value,
    text: bool,
    errors: &mut Vec<String>,
) {
    let without = |body: &Value| {
        let mut body = body.clone();
        let diagnostic = body
            .as_object_mut()
            .and_then(|map| map.remove("diagnostic_information"));
        (body, diagnostic)
    };
    let (body, diagnostic) = without(body);
    let (recorded, recorded_diagnostic) = without(recorded);
    if body != recorded {
        errors.push(format!("{name}: {body}\n    Icinga: {recorded}"));
    }
    match (diagnostic, recorded_diagnostic) {
        (None, None) => {}
        (Some(actual), Some(expected)) => {
            let first = |v: &Value| {
                v.as_str()
                    .unwrap_or_default()
                    .lines()
                    .next()
                    .map(str::to_owned)
            };
            if !actual.is_string() || (text && first(&actual) != first(&expected)) {
                errors.push(format!("{name}: diagnostic {actual}, Icinga {expected}"));
            }
        }
        (actual, expected) => errors.push(format!(
            "{name}: diagnostic_information {actual:?}, Icinga {expected:?}"
        )),
    }
}

/// The shape of result entries and of their `attrs`, `joins` (per joined
/// object) and `meta` (`used_by` entries and `location` too), learned from
/// the recorded entries.
fn compare_shapes(name: &str, actual: &[Value], recorded: &[Value], errors: &mut Vec<String>) {
    if recorded.is_empty() {
        if !actual.is_empty() {
            errors.push(format!("{name}: results, Icinga none"));
        }
        return;
    }
    if actual.is_empty() {
        errors.push(format!("{name}: no results, Icinga {}", recorded.len()));
        return;
    }
    Shape::learn(recorded).check(&format!("{name} entry"), actual, errors);
    let parts = |entries: &[Value], key: &str| -> Vec<Value> {
        entries
            .iter()
            .filter_map(|entry| entry.get(key).filter(|v| v.is_object()).cloned())
            .collect()
    };
    for key in ["attrs", "joins", "meta"] {
        let expected = parts(recorded, key);
        if !expected.is_empty() {
            Shape::learn(&expected).check(&format!("{name} {key}"), &parts(actual, key), errors);
        }
    }
    let nested = |entries: &[Value], outer: &str, inner: &str| -> Vec<Value> {
        parts(entries, outer)
            .into_iter()
            .filter_map(|part| part.get(inner).cloned())
            .flat_map(|value| match value {
                Value::Array(items) => items,
                other => vec![other],
            })
            .filter(Value::is_object)
            .collect()
    };
    let joined: BTreeSet<String> = parts(recorded, "joins")
        .iter()
        .flat_map(|joins| joins.as_object().unwrap().keys().cloned())
        .collect();
    let mut nested_parts: Vec<(&str, String)> =
        joined.iter().map(|join| ("joins", join.clone())).collect();
    nested_parts.push(("meta", "used_by".to_owned()));
    nested_parts.push(("meta", "location".to_owned()));
    for (outer, inner) in nested_parts {
        let expected = nested(recorded, outer, &inner);
        if !expected.is_empty() {
            Shape::learn(&expected).check(
                &format!("{name} {outer}.{inner}"),
                &nested(actual, outer, &inner),
                errors,
            );
        }
    }
}

/// When a request flag Icinga reads through a number (`pretty=yes`) makes
/// its error handling fail, `HttpServerConnection` answers itself: 500
/// without a `Server` header (recorded with `curl -i`).
#[tokio::test]
async fn unhandled_exceptions_come_from_the_connection() {
    let (server, client) = start(contract_users()).await;
    let response = request(
        &client,
        &server,
        Method::GET,
        "/v1/objects/hosts/db-prod-03?attrs=name&pretty=yes",
    )
    .send()
    .await
    .unwrap();
    assert!(response.headers().get("server").is_none());
    assert_eq!(response.headers()["content-type"], "application/json");
    let (status, body) = json(response).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body, json!({"error": 500, "status": "Unhandled exception"}));
    let response = request(
        &client,
        &server,
        Method::GET,
        "/v1/objects/hosts/db-prod-03?pretty=1",
    )
    .send()
    .await
    .unwrap();
    assert!(
        response.headers().get("server").is_some(),
        "a normal answer"
    );
}
