//! Icinga's own `Notification` objects: served like Icinga 2.15.6 serves
//! them (`contract/samples/notifications.json`), sent on hard state
//! changes to the configured users, recoveries only to the users told
//! about the problem, held back while the problem is handled, and reported
//! as `Notification` events.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers fail the test loudly"
)]

mod common;

use std::collections::BTreeSet;

use common::{EventStream, get, lab, results};
use ic_model::{ObjectKey, ServiceState};
use reqwest::StatusCode;
use serde_json::{Value, json};

fn keys(value: &Value) -> BTreeSet<String> {
    value.as_object().unwrap().keys().cloned().collect()
}

#[tokio::test]
async fn notification_objects_have_icingas_attributes() {
    let (server, client) = lab().await;
    let (status, body) = get(&client, &server, "/v1/objects/notifications").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let answered = results(&body);
    // lab-01 and its four services; lab-02 has no recipients.
    assert_eq!(answered.len(), 5);
    let sample: Value = serde_json::from_str(
        &std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../contract/samples/notifications.json"
        ))
        .unwrap(),
    )
    .unwrap();
    let recorded = &results(&sample)[0];
    for entry in answered {
        assert_eq!(keys(entry), keys(recorded));
        assert_eq!(keys(&entry["attrs"]), keys(&recorded["attrs"]), "{entry}");
        assert_eq!(entry["type"], "Notification");
    }
    let host = answered
        .iter()
        .find(|entry| entry["name"] == "lab-01!mail-lab")
        .unwrap();
    assert_eq!(host["attrs"]["host_name"], "lab-01");
    assert_eq!(host["attrs"]["service_name"], "");
    assert_eq!(host["attrs"]["users"], json!(["lab-admin"]));
    assert_eq!(host["attrs"]["last_notification"], json!(0));
    assert_eq!(host["attrs"]["notified_problem_users"], json!([]));

    // By name, and unknown names like Icinga.
    let (status, body) = get(
        &client,
        &server,
        "/v1/objects/notifications/lab-01!ssh!mail-lab?attrs=service_name",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(results(&body)[0]["attrs"], json!({"service_name": "ssh"}));
    let (status, _) = get(&client, &server, "/v1/objects/notifications/lab-01!nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn hard_changes_notify_and_recoveries_reach_who_heard_of_the_problem() {
    let (server, client) = lab().await;
    let mut events = EventStream::open(
        &client,
        &server,
        &json!({"types": ["Notification"], "queue": "n"}),
    )
    .await;
    let control = server.control();
    // Soft states don't notify; the hard one does.
    control
        .set_service_state(
            "lab-01",
            "load",
            ServiceState::Critical,
            "LOAD CRITICAL",
            true,
        )
        .unwrap();
    let event = events.next().await;
    assert_eq!(event["type"], "Notification");
    assert_eq!(event["host"], "lab-01");
    assert_eq!(event["service"], "load");
    assert_eq!(event["users"], json!(["lab-admin"]));
    assert_eq!(event["notification_type"], "PROBLEM");
    assert_eq!(event["command"], "mail-service-notification");
    assert_eq!(event["check_result"]["output"], "LOAD CRITICAL");
    let notification = control
        .notifications()
        .into_iter()
        .find(|n| n.name == "lab-01!load!mail-lab")
        .unwrap();
    assert_eq!(notification.notified_problem_users, ["lab-admin"]);
    let sent = notification.last_notification.unwrap();
    assert!((sent.as_unix_seconds() - control.now().as_unix_seconds()).abs() < 5.0);

    // The recovery goes to the users told about the problem and clears
    // the list.
    control
        .set_service_state("lab-01", "load", ServiceState::Ok, "LOAD OK", true)
        .unwrap();
    let event = events.next().await;
    assert_eq!(event["notification_type"], "RECOVERY");
    assert_eq!(event["users"], json!(["lab-admin"]));
    let notification = control
        .notifications()
        .into_iter()
        .find(|n| n.name == "lab-01!load!mail-lab")
        .unwrap();
    assert!(notification.notified_problem_users.is_empty());
    assert!(notification.last_notification >= Some(sent));

    // Acknowledged before it turned hard: held back. (The next event is
    // the fence's.)
    let ssh = ObjectKey::service("lab-01", "ssh");
    control
        .set_service_state("lab-01", "ssh", ServiceState::Critical, "refused", false)
        .unwrap();
    control.acknowledge(&ssh, "admin", "known", false).unwrap();
    control
        .set_service_state("lab-01", "ssh", ServiceState::Critical, "refused", true)
        .unwrap();
    control
        .set_service_state("lab-01", "disk /", ServiceState::Critical, "full", true)
        .unwrap();
    let event = events.next().await;
    assert_eq!(event["service"], "disk /", "{event}");
}
