//! `/v1/events`: validation, type and filter selection, raw injection,
//! dropped streams and slow consumers.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{EventStream, PASSWORD, USER, json, lab, prod, request, start};
use ic_mock::MockConfig;
use ic_model::{ObjectKey, ServiceState};
use reqwest::{Method, StatusCode};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn types_and_queue_are_required() {
    let (server, client) = lab().await;
    let response = request(&client, &server, Method::POST, "/v1/events")
        .json(&json!({"queue": "q"}))
        .send()
        .await
        .unwrap();
    let (status, body) = json(response).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["status"], "'types' query parameter is required.");

    let response = request(
        &client,
        &server,
        Method::POST,
        "/v1/events?types=CheckResult",
    )
    .send()
    .await
    .unwrap();
    let (status, body) = json(response).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["status"], "'queue' query parameter is required.");

    // GET is not an event stream.
    let response = request(
        &client,
        &server,
        Method::GET,
        "/v1/events?types=CheckResult&queue=q",
    )
    .send()
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn streams_only_carry_the_requested_types() {
    let (server, client) = lab().await;
    // Query parameters work as well as the body.
    let mut changes = EventStream::open(
        &client,
        &server,
        &json!({"types": ["StateChange"], "queue": "changes"}),
    )
    .await;
    let control = server.control();
    control
        .set_service_state(
            "lab-01",
            "load",
            ServiceState::Warning,
            "LOAD WARNING",
            false,
        )
        .unwrap();
    let event = changes.next().await;
    assert_eq!(event["type"], "StateChange");
    assert_eq!(event["service"], "load");
    assert_eq!(event["state"], json!(1.0));
    assert_eq!(event["state_type"], json!(0.0));
    assert_eq!(event["acknowledgement"], json!(false));
    assert_eq!(event["downtime_depth"], json!(0.0));
    assert!(event["timestamp"].as_f64().unwrap() > 1.0e9);
    assert_eq!(event["check_result"]["output"], "LOAD WARNING");

    // Nothing else (no CheckResult) arrives: the next event is the next
    // state change.
    control
        .set_service_state("lab-01", "load", ServiceState::Ok, "LOAD OK", false)
        .unwrap();
    let event = changes.next().await;
    assert_eq!(event["type"], "StateChange");
    assert_eq!(event["state"], json!(0.0));
}

#[tokio::test]
async fn event_filters_select_events() {
    let (server, client) = prod().await;
    let mut stream = EventStream::open(
        &client,
        &server,
        &json!({
            "types": ["CheckResult"],
            "queue": "q",
            "filter": "event.host == \"db-prod-01\" && event.check_result.exit_status > 0"
        }),
    )
    .await;
    let control = server.control();
    control
        .process_check_result(
            &ObjectKey::service("db-prod-02", "load"),
            2,
            "other host",
            &[],
        )
        .unwrap();
    control
        .process_check_result(
            &ObjectKey::service("db-prod-01", "load"),
            0,
            "ok result",
            &[],
        )
        .unwrap();
    control
        .process_check_result(
            &ObjectKey::service("db-prod-01", "load"),
            1,
            "match",
            &["load1=9"],
        )
        .unwrap();
    let event = stream.next().await;
    assert_eq!(event["check_result"]["output"], "match");
    assert_eq!(
        event["check_result"]["performance_data"],
        json!(["load1=9"])
    );
}

#[tokio::test]
async fn event_filters_need_filter_expression_permission() {
    let config = MockConfig {
        users: vec![ic_mock::MockUser::new("ev", "pw", &["events/*"])],
        ..MockConfig::default()
    };
    let (server, client) = start(config).await;
    let response = client
        .post(format!("{}/v1/events", server.url()))
        .basic_auth("ev", Some("pw"))
        .header("Accept", "application/json")
        .json(&json!({"types": ["CheckResult"], "queue": "q", "filter": "true"}))
        .send()
        .await
        .unwrap();
    let (status, body) = json(response).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["status"], "Missing permission: filter-expression");
}

#[tokio::test]
async fn raw_events_and_malformed_lines_reach_clients() {
    let (server, client) = lab().await;
    let mut stream = EventStream::open(
        &client,
        &server,
        &json!({"types": ["CheckResult"], "queue": "q"}),
    )
    .await;
    let control = server.control();
    control.emit_raw(json!({"type": "CheckResult", "host": "ghost", "timestamp": 1.0}));
    control.emit_raw_line("this is not json");
    let event = stream.next().await;
    assert_eq!(event["host"], "ghost");
    assert_eq!(stream.next_line().await.unwrap(), "this is not json");
}

#[tokio::test]
async fn dropping_streams_ends_them() {
    let (server, client) = lab().await;
    let mut stream = EventStream::open(
        &client,
        &server,
        &json!({"types": ["CheckResult"], "queue": "q"}),
    )
    .await;
    assert_eq!(server.control().event_streams(), 1);
    assert_eq!(server.control().drop_event_streams(), 1);
    assert!(stream.ends().await);
    assert_eq!(server.control().event_streams(), 0);
}

#[tokio::test]
async fn slow_consumers_are_disconnected() {
    let config = MockConfig {
        event_buffer: 4,
        ..MockConfig::default()
    };
    let (server, client) = start(config).await;
    let mut stream = EventStream::open(
        &client,
        &server,
        &json!({"types": ["CheckResult"], "queue": "q"}),
    )
    .await;
    // Without reading, overflow the per-stream buffer (and the socket
    // buffers) with large events.
    let control = server.control();
    let padding = "x".repeat(64 * 1024);
    for i in 0..200 {
        control.emit_raw(json!({"type": "CheckResult", "n": i, "padding": padding}));
    }
    assert_eq!(control.event_streams(), 0, "the stream was dropped");
    assert!(stream.ends().await);
}

#[tokio::test]
async fn closed_clients_are_forgotten() {
    let (server, client) = lab().await;
    let stream = EventStream::open(
        &client,
        &server,
        &json!({"types": ["CheckResult"], "queue": "q"}),
    )
    .await;
    drop(stream);
    drop(client);
    // The next event notices the closed receiver.
    let control = server.control();
    let mut gone = false;
    for _ in 0..50 {
        control.emit_raw(json!({"type": "CheckResult"}));
        if control.event_streams() == 0 {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(gone);
}

/// Icinga refuses event streams over HTTP/1.0 (no chunked encoding).
#[tokio::test]
async fn http_1_0_event_streams_are_refused() {
    let (server, _client) = lab().await;
    let config = common::pinned_config(server.cert_sha256());
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
    let tcp = tokio::net::TcpStream::connect(server.addr()).await.unwrap();
    let mut tls = connector
        .connect("localhost".try_into().unwrap(), tcp)
        .await
        .unwrap();
    let credentials = base64_encode(&format!("{USER}:{PASSWORD}"));
    let request = format!(
        "POST /v1/events?types=CheckResult&queue=q HTTP/1.0\r\nHost: localhost\r\nAccept: application/json\r\nAuthorization: Basic {credentials}\r\nContent-Length: 0\r\n\r\n"
    );
    tls.write_all(request.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), tls.read_to_end(&mut response)).await;
    let text = String::from_utf8_lossy(&response);
    assert!(
        text.starts_with("HTTP/1.0 400") || text.starts_with("HTTP/1.1 400"),
        "{text}"
    );
    assert!(
        text.contains("HTTP/1.0 not supported for event streams."),
        "{text}"
    );
}

fn base64_encode(text: &str) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(text)
}
