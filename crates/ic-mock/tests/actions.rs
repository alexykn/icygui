//! `/v1/actions/*`: results, effects on the objects and the events each
//! action emits.

mod common;

use std::time::Duration;

use common::{EventStream, get, lab, post, prod, results};
use ic_mock::MockConfig;
use reqwest::StatusCode;
use serde_json::json;

const ALL_EVENTS: [&str; 11] = [
    "CheckResult",
    "StateChange",
    "AcknowledgementSet",
    "AcknowledgementCleared",
    "CommentAdded",
    "CommentRemoved",
    "DowntimeAdded",
    "DowntimeRemoved",
    "DowntimeStarted",
    "DowntimeTriggered",
    "ObjectCreated",
];

async fn all_events(client: &reqwest::Client, server: &ic_mock::MockServer) -> EventStream {
    EventStream::open(
        client,
        server,
        &json!({"types": ALL_EVENTS, "queue": "test"}),
    )
    .await
}

#[tokio::test]
async fn process_check_result_drives_soft_and_hard_states() {
    let (server, client) = lab().await;
    let mut events = all_events(&client, &server).await;
    let body = json!({
        "type": "Service",
        "filter": "host.name == \"lab-01\" && service.name == \"ssh\"",
        "exit_status": 2,
        "plugin_output": "SSH CRITICAL - connection refused",
        "performance_data": ["time=0.1s;1;2"],
        "check_source": "test-runner"
    });
    for attempt in 1..=3 {
        let (status, response) =
            post(&client, &server, "/v1/actions/process-check-result", &body).await;
        assert_eq!(status, StatusCode::OK, "{response}");
        assert_eq!(
            results(&response)[0],
            json!({"code": 200, "status": "Successfully processed check result for object 'lab-01!ssh'."})
        );
        let check = events.next().await;
        assert_eq!(check["type"], "CheckResult");
        assert_eq!(check["host"], "lab-01");
        assert_eq!(check["service"], "ssh");
        assert_eq!(check["check_result"]["state"], json!(2));
        assert_eq!(
            check["check_result"]["output"],
            "SSH CRITICAL - connection refused"
        );
        assert_eq!(check["check_result"]["check_source"], "test-runner");
        assert_eq!(
            check["check_result"]["performance_data"],
            json!(["time=0.1s;1;2"])
        );
        assert_eq!(
            check["check_result"]["vars_after"]["attempt"],
            json!(if attempt == 3 { 1 } else { attempt })
        );
        // Icinga signals a state change for every soft result and for the
        // hard transition, after the check result.
        let change = events.next().await;
        assert_eq!(change["type"], "StateChange");
        assert_eq!(change["state"], json!(2));
        assert_eq!(change["state_type"], json!(u8::from(attempt == 3)));
    }
    let (_, body) = get(
        &client,
        &server,
        "/v1/objects/services/lab-01!ssh?attrs=state&attrs=state_type&attrs=last_hard_state",
    )
    .await;
    assert_eq!(
        results(&body)[0]["attrs"],
        json!({"state": 2, "state_type": 1, "last_hard_state": 2})
    );

    // Hosts take 0 (UP) and 1 (DOWN) only.
    let (status, body) = post(
        &client,
        &server,
        "/v1/actions/process-check-result",
        &json!({"type": "Host", "host": "lab-01", "exit_status": 2, "plugin_output": "x"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        results(&body)[0]["status"],
        "Invalid 'exit_status' for Host lab-01."
    );
}

#[tokio::test]
async fn pending_objects_leave_pending_on_their_first_result() {
    let (server, client) = lab().await;
    let (_, body) = get(
        &client,
        &server,
        "/v1/objects/hosts/lab-02?attrs=last_check_result&attrs=state",
    )
    .await;
    assert_eq!(results(&body)[0]["attrs"]["last_check_result"], json!(null));
    let (status, _) = post(
        &client,
        &server,
        "/v1/actions/process-check-result",
        &json!({"type": "Host", "host": "lab-02", "exit_status": 0, "plugin_output": "PING OK"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, body) = get(
        &client,
        &server,
        "/v1/objects/hosts/lab-02?attrs=last_check_result",
    )
    .await;
    assert_eq!(
        results(&body)[0]["attrs"]["last_check_result"]["output"],
        "PING OK"
    );
}

#[tokio::test]
async fn acknowledgements_set_clear_and_reject() {
    let (server, client) = prod().await;
    let mut events = all_events(&client, &server).await;
    let target = json!({"type": "Service", "service": "db-prod-03!postgres-replication"});
    let mut body = target.clone();
    body["author"] = json!("tester");
    body["comment"] = json!("looking into it");
    body["sticky"] = json!(true);
    let (status, response) = post(&client, &server, "/v1/actions/acknowledge-problem", &body).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(
        results(&response)[0]["status"],
        "Successfully acknowledged problem for object 'db-prod-03!postgres-replication'."
    );
    let set = events.until("AcknowledgementSet").await;
    let ack = set.last().unwrap();
    assert_eq!(ack["author"], "tester");
    assert_eq!(ack["comment"], "looking into it");
    assert_eq!(ack["acknowledgement_type"], json!(2));
    assert!(
        set.iter()
            .any(|e| e["type"] == "CommentAdded" && e["comment"]["entry_type"] == json!(4)),
        "{set:?}"
    );

    // Already acknowledged.
    let (status, response) = post(&client, &server, "/v1/actions/acknowledge-problem", &body).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(
        results(&response)[0]["status"]
            .as_str()
            .unwrap()
            .contains("already acknowledged")
    );

    // OK services can't be acknowledged.
    let mut ok = body.clone();
    ok["service"] = json!("db-prod-03!ping4");
    let (status, response) = post(&client, &server, "/v1/actions/acknowledge-problem", &ok).await;
    assert_eq!(status, StatusCode::CONFLICT, "{response}");
    assert_eq!(
        results(&response)[0]["status"],
        "Service db-prod-03!ping4 is OK."
    );

    // Missing author is a 400 from the per-object result.
    let (status, _) = post(&client, &server, "/v1/actions/acknowledge-problem", &target).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, response) = post(
        &client,
        &server,
        "/v1/actions/remove-acknowledgement",
        &target,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        results(&response)[0]["status"],
        "Successfully removed acknowledgement for object 'db-prod-03!postgres-replication'."
    );
    events.until("AcknowledgementCleared").await;
    // The acknowledgement comment goes after the acknowledgement.
    let removed = events.until("CommentRemoved").await;
    assert_eq!(removed.last().unwrap()["comment"]["entry_type"], json!(4));
    let (_, body) = get(
        &client,
        &server,
        "/v1/objects/services/db-prod-03!postgres-replication?attrs=acknowledgement",
    )
    .await;
    assert_eq!(results(&body)[0]["attrs"]["acknowledgement"], json!(0));
}

#[tokio::test]
async fn non_sticky_acks_clear_on_any_state_change_sticky_only_on_ok() {
    let (server, client) = prod().await;
    let control = server.control();
    let service = ic_model::ObjectKey::service("db-prod-03", "postgres-replication");
    control.acknowledge(&service, "a", "normal", false).unwrap();
    control
        .set_service_state(
            "db-prod-03",
            "postgres-replication",
            ic_model::ServiceState::Warning,
            "WARN",
            true,
        )
        .unwrap();
    assert_eq!(
        control
            .object_attrs("Service", "db-prod-03!postgres-replication")
            .unwrap()["acknowledgement"],
        json!(0)
    );

    control
        .set_service_state(
            "db-prod-03",
            "postgres-replication",
            ic_model::ServiceState::Critical,
            "CRIT",
            true,
        )
        .unwrap();
    control.acknowledge(&service, "a", "sticky", true).unwrap();
    control
        .set_service_state(
            "db-prod-03",
            "postgres-replication",
            ic_model::ServiceState::Warning,
            "WARN",
            true,
        )
        .unwrap();
    assert_eq!(
        control
            .object_attrs("Service", "db-prod-03!postgres-replication")
            .unwrap()["acknowledgement"],
        json!(2),
        "sticky acks survive problem state changes"
    );
    control
        .set_service_state(
            "db-prod-03",
            "postgres-replication",
            ic_model::ServiceState::Ok,
            "OK",
            true,
        )
        .unwrap();
    assert_eq!(
        control
            .object_attrs("Service", "db-prod-03!postgres-replication")
            .unwrap()["acknowledgement"],
        json!(0)
    );
    let _ = client;
}

#[tokio::test]
async fn comments_add_and_remove() {
    let (server, client) = lab().await;
    let mut events = all_events(&client, &server).await;
    let (status, body) = post(
        &client,
        &server,
        "/v1/actions/add-comment",
        &json!({"type": "Host", "host": "lab-01", "author": "tester", "comment": "hello"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let result = &results(&body)[0];
    let name = result["name"].as_str().unwrap().to_owned();
    assert!(name.starts_with("lab-01!"));
    assert!(result["legacy_id"].as_f64().unwrap() >= 1.0);
    assert_eq!(
        result["status"],
        format!("Successfully added comment '{name}' for object 'lab-01'.")
    );
    let added = events.until("CommentAdded").await;
    let comment = &added.last().unwrap()["comment"];
    assert_eq!(comment["author"], "tester");
    assert_eq!(comment["text"], "hello");
    assert_eq!(comment["entry_type"], json!(1));
    assert_eq!(comment["host_name"], "lab-01");
    assert_eq!(
        events.until("ObjectCreated").await.last().unwrap()["object_name"],
        name.as_str()
    );

    let (status, _) = get(&client, &server, &format!("/v1/objects/comments/{name}")).await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = post(
        &client,
        &server,
        "/v1/actions/remove-comment",
        &json!({"type": "Comment", "comment": name}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        results(&body)[0]["status"],
        format!("Successfully removed comment '{name}'.")
    );
    events.until("CommentRemoved").await;
    let (status, _) = get(&client, &server, &format!("/v1/objects/comments/{name}")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Missing parameters.
    let (status, body) = post(
        &client,
        &server,
        "/v1/actions/add-comment",
        &json!({"type": "Host", "host": "lab-01", "author": "tester"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        results(&body)[0]["status"],
        "Comments require author and comment."
    );
}

#[tokio::test]
async fn fixed_downtimes_start_and_flexible_ones_trigger_on_problems() {
    let (server, client) = lab().await;
    let mut events = all_events(&client, &server).await;
    let now = server.control().now().as_unix_seconds();

    // Fixed, in effect right away.
    let (status, body) = post(
        &client,
        &server,
        "/v1/actions/schedule-downtime",
        &json!({
            "type": "Host", "host": "lab-01", "author": "tester", "comment": "patching",
            "start_time": now - 10.0, "end_time": now + 3600.0, "all_services": true
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let result = &results(&body)[0];
    let name = result["name"].as_str().unwrap().to_owned();
    assert_eq!(
        result["status"],
        format!("Successfully scheduled downtime '{name}' for object 'lab-01'.")
    );
    assert_eq!(result["service_downtimes"].as_array().unwrap().len(), 4);
    let seen = events.until("DowntimeStarted").await;
    assert!(seen.iter().any(|e| e["type"] == "DowntimeAdded"));
    let (_, body) = get(
        &client,
        &server,
        "/v1/objects/hosts/lab-01?attrs=downtime_depth",
    )
    .await;
    assert_eq!(results(&body)[0]["attrs"]["downtime_depth"], json!(1));

    // The service downtimes are its children and go with it.
    let (status, body) = post(
        &client,
        &server,
        "/v1/actions/remove-downtime",
        &json!({"type": "Downtime", "downtime": name}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        results(&body)[0]["status"],
        format!("Successfully removed downtime '{name}' and 4 child downtimes.")
    );
    events.until("DowntimeRemoved").await;

    // Flexible: waits for a problem.
    let (status, body) = post(
        &client,
        &server,
        "/v1/actions/schedule-downtime",
        &json!({
            "type": "Service", "service": "lab-01!load", "author": "tester", "comment": "flex",
            "start_time": now - 10.0, "end_time": now + 3600.0, "fixed": false, "duration": 600
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let flexible = results(&body)[0]["name"].as_str().unwrap().to_owned();
    let (_, body) = get(
        &client,
        &server,
        &format!("/v1/objects/downtimes/{flexible}?attrs=trigger_time"),
    )
    .await;
    assert_eq!(results(&body)[0]["attrs"]["trigger_time"], json!(0));
    server
        .control()
        .set_service_state(
            "lab-01",
            "load",
            ic_model::ServiceState::Critical,
            "LOAD CRITICAL",
            false,
        )
        .unwrap();
    let triggered = events.until("DowntimeTriggered").await;
    assert_eq!(triggered.last().unwrap()["downtime"]["__name"], flexible);
    let (_, body) = get(
        &client,
        &server,
        &format!("/v1/objects/downtimes/{flexible}?attrs=trigger_time"),
    )
    .await;
    assert!(results(&body)[0]["attrs"]["trigger_time"].as_f64().unwrap() > 0.0);

    // Validation.
    let (status, body) = post(
        &client,
        &server,
        "/v1/actions/schedule-downtime",
        &json!({"type": "Host", "host": "lab-01", "author": "a", "comment": "c", "start_time": now}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        results(&body)[0]["status"]
            .as_str()
            .unwrap()
            .contains("end_time"),
        "{body}"
    );
}

#[tokio::test]
async fn config_owned_downtimes_cannot_be_removed() {
    let (server, client) = prod().await;
    let owned = server
        .control()
        .downtimes()
        .into_iter()
        .find(|d| d.config_owned)
        .expect("prod-cluster has a ScheduledDowntime-owned downtime");
    let (status, body) = post(
        &client,
        &server,
        "/v1/actions/remove-downtime",
        &json!({"type": "Downtime", "downtime": owned.name}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        results(&body)[0]["status"]
            .as_str()
            .unwrap()
            .contains("owned by scheduled downtime")
    );
}

#[tokio::test]
async fn reschedule_check_runs_a_check_soon() {
    let config = MockConfig {
        reschedule_delay: Duration::from_millis(50),
        ..MockConfig::default()
    };
    let (server, client) = common::start(config).await;
    let mut events = EventStream::open(
        &client,
        &server,
        &json!({"types": ["CheckResult"], "queue": "q"}),
    )
    .await;
    let (status, body) = post(
        &client,
        &server,
        "/v1/actions/reschedule-check",
        &json!({"type": "Service", "service": "lab-01!disk /", "force": true}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        results(&body)[0]["status"],
        "Successfully rescheduled check for object 'lab-01!disk /'."
    );
    let event = events.next().await;
    assert_eq!(event["service"], "disk /");
}

#[tokio::test]
async fn execute_command_is_accepted_and_reports_back() {
    let config = MockConfig {
        reschedule_delay: Duration::from_millis(50),
        ..MockConfig::default()
    };
    let (server, client) = common::start(config).await;
    let (status, body) = post(
        &client,
        &server,
        "/v1/actions/execute-command",
        &json!({"type": "Host", "host": "lab-01", "command_type": "CheckCommand", "command": "hostalive", "ttl": 30, "endpoint": "lab-icinga"}),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let result = &results(&body)[0];
    assert_eq!(result["code"], json!(202));
    assert_eq!(result["status"], "Accepted");
    assert_eq!(result["checkable"], "lab-01");
    let execution = result["execution"].as_str().unwrap().to_owned();
    // The execution is pending, then carries the result.
    let mut finished = None;
    for _ in 0..40 {
        let attrs = server.control().object_attrs("Host", "lab-01").unwrap();
        let entry = attrs["executions"][&execution].clone();
        assert!(entry.is_object(), "{attrs}");
        if entry.get("pending").is_none() {
            finished = Some(entry);
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let finished = finished.expect("execution finishes");
    assert_eq!(finished["exit"], json!(0));
    assert!(finished["output"].is_string());

    // Without a command endpoint, Icinga needs an explicit endpoint.
    let (status, body) = post(
        &client,
        &server,
        "/v1/actions/execute-command",
        &json!({"type": "Host", "host": "lab-01", "command_type": "CheckCommand", "command": "hostalive", "ttl": 30}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        results(&body)[0]["status"],
        "Can't find a valid endpoint for ''."
    );

    let (status, body) = post(
        &client,
        &server,
        "/v1/actions/execute-command",
        &json!({"type": "Host", "host": "lab-01", "command_type": "CheckCommand", "command": "nope", "ttl": 30, "endpoint": "lab-icinga"}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

/// Icinga reads `sticky`, `notify`, `persistent`, `fixed` and
/// `all_services` through numbers (`apiactions.cpp`, like `pretty`):
/// `"0"` is false, and a string that isn't a number fails the action for
/// the object with a 500.
#[tokio::test]
async fn boolean_parameters_are_read_through_numbers() {
    let (server, client) = prod().await;
    let failed = |body: &serde_json::Value, value: &str| {
        let status = results(body)[0]["status"].as_str().unwrap().to_owned();
        assert!(
            status.starts_with("Action execution failed: '")
                && status.contains(&format!(
                    "Can't convert '{value}' to a floating point number."
                )),
            "{status}"
        );
    };
    let ack = json!({
        "type": "Service", "service": "db-prod-03!postgres-replication",
        "author": "tester", "comment": "looking into it"
    });
    for key in ["sticky", "notify", "persistent"] {
        let mut body = ack.clone();
        body[key] = json!("false");
        let (status, response) =
            post(&client, &server, "/v1/actions/acknowledge-problem", &body).await;
        assert_eq!(
            status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "{key}: {response}"
        );
        failed(&response, "false");
    }
    let mut body = ack.clone();
    body["sticky"] = json!("0");
    let (status, response) = post(&client, &server, "/v1/actions/acknowledge-problem", &body).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    let (_, body) = get(
        &client,
        &server,
        "/v1/objects/services/db-prod-03!postgres-replication?attrs=acknowledgement",
    )
    .await;
    assert_eq!(
        results(&body)[0]["attrs"]["acknowledgement"],
        json!(1),
        "\"0\" is not sticky"
    );

    let now = server.control().now().as_unix_seconds();
    let downtime = json!({
        "type": "Host", "host": "db-prod-03", "author": "tester", "comment": "patching",
        "start_time": now + 3_600.0, "end_time": now + 7_200.0
    });
    let mut body = downtime.clone();
    body["fixed"] = json!("0");
    let (status, response) = post(&client, &server, "/v1/actions/schedule-downtime", &body).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "flexible: {response}");
    assert_eq!(
        results(&response)[0]["status"],
        "Option 'duration' is required for flexible downtime"
    );
    body["fixed"] = json!("false");
    let (status, response) = post(&client, &server, "/v1/actions/schedule-downtime", &body).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{response}");
    failed(&response, "false");
    let count = |body: &serde_json::Value| results(body).len();
    let (_, before) = get(&client, &server, "/v1/objects/downtimes?attrs=name").await;
    // `all_services` is read once the host's downtime exists: the action
    // fails, but that downtime stays.
    let mut body = downtime.clone();
    body["all_services"] = json!("yes");
    let (status, response) = post(&client, &server, "/v1/actions/schedule-downtime", &body).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{response}");
    failed(&response, "yes");
    let (_, after) = get(&client, &server, "/v1/objects/downtimes?attrs=name").await;
    assert_eq!(count(&after), count(&before) + 1);
    body["all_services"] = json!("0");
    let (status, response) = post(&client, &server, "/v1/actions/schedule-downtime", &body).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert!(results(&response)[0].get("service_downtimes").is_none());
}

#[tokio::test]
async fn action_errors() {
    let (server, client) = lab().await;
    let (status, body) = post(&client, &server, "/v1/actions/nonexistent", &json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["status"], "Action 'nonexistent' does not exist.");

    let (status, body) = post(
        &client,
        &server,
        "/v1/actions/reschedule-check",
        &json!({"type": "Host", "host": "missing"}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["status"], "No objects found.");

    // Types the action doesn't take: GetFilterTargets fails, so 404.
    let (status, body) = post(
        &client,
        &server,
        "/v1/actions/reschedule-check",
        &json!({"type": "Comment", "verbose": true}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert!(
        body["diagnostic_information"]
            .as_str()
            .unwrap()
            .contains("Invalid type specified for this query.")
    );

    // Several objects: one result each, 200 overall.
    let (status, body) = post(
        &client,
        &server,
        "/v1/actions/reschedule-check",
        &json!({"type": "Service", "filter": "host.name == \"lab-01\""}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(results(&body).len(), 4);
}

#[tokio::test]
async fn late_answers_come_after_the_action_ran() {
    // `delay_action_answers`: a busy Icinga still answering. A client that
    // gives up first has still added the comment.
    let (server, client) = lab().await;
    let control = server.control();
    control.delay_action_answers(Duration::from_secs(5));
    let body = json!({
        "type": "Host", "hosts": ["lab-01"], "author": "late", "comment": "answered late"
    });
    let gave_up = tokio::time::timeout(
        Duration::from_millis(300),
        post(&client, &server, "/v1/actions/add-comment", &body),
    )
    .await;
    assert!(gave_up.is_err(), "the answer is late");
    assert!(
        control
            .comments()
            .iter()
            .any(|comment| comment.author == "late" && comment.text == "answered late"),
        "the comment exists"
    );
    assert!(
        control
            .requests()
            .iter()
            .any(|request| request.path == "/v1/actions/add-comment" && request.status == 200)
    );
    // Queries aren't delayed, and turning it off answers actions at once.
    let started = std::time::Instant::now();
    let (status, _) = get(&client, &server, "/v1/objects/hosts").await;
    assert_eq!(status, StatusCode::OK);
    control.delay_action_answers(Duration::ZERO);
    let (status, _) = post(&client, &server, "/v1/actions/add-comment", &body).await;
    assert_eq!(status, StatusCode::OK);
    assert!(started.elapsed() < Duration::from_secs(4));
}
