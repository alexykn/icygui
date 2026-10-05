//! The client against an in-process HTTPS server: TLS trust (pinning, CA,
//! name override, CN-only certificates, IP addresses, validity dates,
//! TLS 1.2, client certificates), auth headers, request shapes, timeouts,
//! error mapping, actions (with Icinga's HTTP status rules) and the event
//! stream.

#![expect(
    clippy::unwrap_used,
    reason = "test harness and helpers: a failure should panic the test"
)]

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use bytes::Bytes;
use futures::StreamExt;
use ic_api::{ApiError, Client, TlsSettings, fetch_server_certificate, format_fingerprint};
use ic_model::{
    Action, ActionTarget, Event, EventKind, HostState, ObjectChange, ObjectKey, ServiceState,
};
use serde_json::{Value, json};
use support::{
    Issued, PASSWORD, Pki, Reply, SERVER_NAME, ServerOptions, TestServer, basic_settings, ca_trust,
    certificate_settings, error_json, icinga_action_reply, ok_json,
};
use tokio::sync::mpsc;

fn info_reply() -> Reply {
    ok_json(&json!({ "results": [{
        "info": "More information about API requests is available in the documentation at https://icinga.com/docs/icinga2/latest/",
        "permissions": ["objects/query/*", "status/query", "events/*", "actions/*"],
        "user": "icygui",
        "version": "v2.15.6"
    }]}))
}

async fn server_with(
    leaf: Issued,
    handler: impl Fn(&support::Recorded) -> Reply + Send + Sync + 'static,
) -> TestServer {
    TestServer::start(ServerOptions::new(leaf, handler)).await
}

async fn info_server(pki: &Pki) -> TestServer {
    server_with(pki.issue(SERVER_NAME, &[SERVER_NAME]), |_| info_reply()).await
}

fn client(server: &TestServer, tls: TlsSettings) -> Client {
    Client::new(basic_settings(server.url(), tls)).unwrap()
}

// --- TLS -----------------------------------------------------------------

#[tokio::test]
async fn pinned_certificate_is_accepted_without_ca_or_name() {
    let pki = Pki::new();
    let server = info_server(&pki).await;
    let tls = TlsSettings {
        pinned_sha256: Some(server.sha256()),
        ..TlsSettings::default()
    };
    let info = client(&server, tls).info().await.unwrap();
    assert_eq!(info.user, "icygui");
    assert_eq!(info.version, "v2.15.6");
    assert!(info.allows("actions/acknowledge-problem"));
}

#[tokio::test]
async fn pin_mismatch_reports_both_fingerprints() {
    let pki = Pki::new();
    let server = info_server(&pki).await;
    let pinned = [0x42_u8; 32];
    let tls = TlsSettings {
        pinned_sha256: Some(pinned),
        // Even a trusted CA doesn't override a pin.
        ca_pem: Some(pki.ca_pem().into_bytes()),
        server_name: Some(SERVER_NAME.to_owned()),
        use_system_roots: false,
    };
    let error = client(&server, tls).info().await.unwrap_err();
    assert_eq!(
        error,
        ApiError::CertificateMismatch {
            expected: format_fingerprint(&pinned),
            actual: format_fingerprint(&server.sha256()),
        }
    );
    assert!(!error.is_transient());
    let ApiError::CertificateMismatch { expected, actual } = error else {
        unreachable!();
    };
    assert!(expected.starts_with("42:42:"));
    assert_eq!(actual.len(), 95, "colon-separated uppercase hex");
    assert_eq!(actual, actual.to_uppercase());
}

#[tokio::test]
async fn ca_trust_with_server_name_override() {
    let pki = Pki::new();
    let server = info_server(&pki).await;
    let info = client(&server, ca_trust(&pki)).info().await.unwrap();
    assert_eq!(info.user, "icygui");
}

#[tokio::test]
async fn ca_trust_checks_the_name() {
    let pki = Pki::new();
    let server = info_server(&pki).await;
    // The URL host is 127.0.0.1, which the certificate doesn't name.
    let tls = TlsSettings {
        ca_pem: Some(pki.ca_pem().into_bytes()),
        ..TlsSettings::default()
    };
    let error = client(&server, tls).info().await.unwrap_err();
    assert!(
        matches!(&error, ApiError::Tls(message) if message.contains("not valid for")),
        "{error:?}"
    );
    // A wrong override fails too.
    let tls = TlsSettings {
        server_name: Some("other-master".to_owned()),
        ..ca_trust(&pki)
    };
    assert!(matches!(
        client(&server, tls).info().await,
        Err(ApiError::Tls(_))
    ));
}

#[tokio::test]
async fn a_foreign_ca_is_not_trusted() {
    let pki = Pki::new();
    let server = info_server(&pki).await;
    // Another CA with the same name, as every Icinga installation's
    // "Icinga CA" is: the signature check catches it.
    let other = Pki::new();
    let error = client(&server, ca_trust(&other)).info().await.unwrap_err();
    assert!(
        matches!(&error, ApiError::Tls(message) if message.contains("BadSignature")),
        "{error:?}"
    );
    assert!(!error.is_transient());
}

#[tokio::test]
async fn nothing_trusted_rejects_as_unknown_issuer() {
    let pki = Pki::new();
    let server = info_server(&pki).await;
    let error = client(&server, TlsSettings::default())
        .info()
        .await
        .unwrap_err();
    assert!(
        matches!(&error, ApiError::Tls(message) if message.contains("UnknownIssuer")),
        "{error:?}"
    );
}

#[tokio::test]
async fn certificates_without_san_match_by_common_name() {
    let pki = Pki::new();
    let server = server_with(pki.issue("icinga-legacy", &[]), |_| info_reply()).await;
    let tls = TlsSettings {
        server_name: Some("ICINGA-LEGACY".to_owned()),
        ..ca_trust(&pki)
    };
    assert!(client(&server, tls).info().await.is_ok());
    let wrong = TlsSettings {
        server_name: Some("icinga-other".to_owned()),
        ..ca_trust(&pki)
    };
    assert!(matches!(
        client(&server, wrong).info().await,
        Err(ApiError::Tls(_))
    ));
}

#[tokio::test]
async fn certificates_with_san_do_not_fall_back_to_the_common_name() {
    let pki = Pki::new();
    let server = server_with(pki.issue("icinga-cn", &["icinga-san"]), |_| info_reply()).await;
    let tls = TlsSettings {
        server_name: Some("icinga-cn".to_owned()),
        ..ca_trust(&pki)
    };
    assert!(matches!(
        client(&server, tls).info().await,
        Err(ApiError::Tls(_))
    ));
}

#[tokio::test]
async fn client_certificate_authentication() {
    let pki = Pki::new();
    let server = TestServer::start(ServerOptions {
        client_ca: Some(pki.ca_pem()),
        ..ServerOptions::new(pki.issue(SERVER_NAME, &[SERVER_NAME]), |_| info_reply())
    })
    .await;
    let identity = pki.issue("icygui-client", &[]);
    let client = Client::new(certificate_settings(
        server.url(),
        ca_trust(&pki),
        &identity,
    ))
    .unwrap();
    client.info().await.unwrap();
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].client_certificate);
    assert_eq!(requests[0].header("authorization"), None, "no basic auth");

    // Without a certificate the server refuses the handshake.
    let anonymous = client_for_url(&server, ca_trust(&pki));
    let error = anonymous.info().await.unwrap_err();
    assert!(
        matches!(error, ApiError::Tls(_) | ApiError::Connect(_)),
        "{error:?}"
    );
}

fn client_for_url(server: &TestServer, tls: TlsSettings) -> Client {
    Client::new(basic_settings(server.url(), tls)).unwrap()
}

#[tokio::test]
async fn bad_settings_are_rejected_up_front() {
    let pki = Pki::new();
    let url = ic_api::Url::parse("http://127.0.0.1:5665").unwrap();
    assert!(matches!(
        Client::new(basic_settings(url, TlsSettings::default())),
        Err(ApiError::InvalidSettings(_))
    ));
    let url = ic_api::Url::parse("https://127.0.0.1:5665").unwrap();
    let tls = TlsSettings {
        ca_pem: Some(b"garbage".to_vec()),
        ..TlsSettings::default()
    };
    assert!(matches!(
        Client::new(basic_settings(url.clone(), tls)),
        Err(ApiError::InvalidSettings(_))
    ));
    let broken_key = Issued {
        key_pem: "-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n".to_owned(),
        ..pki.issue("x", &[])
    };
    assert!(matches!(
        Client::new(certificate_settings(url, ca_trust(&pki), &broken_key)),
        Err(ApiError::InvalidSettings(_))
    ));
}

#[tokio::test]
async fn fetches_the_server_certificate_without_trusting_it() {
    let pki = Pki::new();
    let server = info_server(&pki).await;
    let info = fetch_server_certificate(&server.url(), None).await.unwrap();
    assert_eq!(info.sha256, server.sha256());
    assert_eq!(info.fingerprint(), format_fingerprint(&server.sha256()));
    assert!(
        info.subject.contains("CN=icinga-master"),
        "{}",
        info.subject
    );
    assert!(info.issuer.contains("CN=Icinga CA"), "{}", info.issuer);
    assert_eq!(info.names, [SERVER_NAME]);
    assert!(info.not_before < info.not_after);
    assert!(server.requests().is_empty(), "no HTTP request is sent");

    let with_sni = fetch_server_certificate(&server.url(), Some(SERVER_NAME))
        .await
        .unwrap();
    assert_eq!(with_sni.sha256, info.sha256);
}

#[tokio::test]
async fn fetching_a_certificate_from_nowhere_fails_cleanly() {
    let url = ic_api::Url::parse(&format!("https://127.0.0.1:{}", closed_port().await)).unwrap();
    let error = fetch_server_certificate(&url, None).await.unwrap_err();
    assert!(matches!(error, ApiError::Connect(_)), "{error:?}");
    let http = ic_api::Url::parse("http://127.0.0.1:1").unwrap();
    assert!(matches!(
        fetch_server_certificate(&http, None).await,
        Err(ApiError::InvalidSettings(_))
    ));
}

async fn closed_port() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

// --- Requests and errors ---------------------------------------------------

#[tokio::test]
async fn sends_basic_auth_and_exact_accept_header() {
    let pki = Pki::new();
    let server = info_server(&pki).await;
    client(&server, ca_trust(&pki)).info().await.unwrap();
    let request = &server.requests()[0];
    assert_eq!(request.method, "GET");
    assert_eq!(request.path, "/v1");
    assert_eq!(request.header("accept"), Some("application/json"));
    assert_eq!(
        request.header("authorization"),
        Some("Basic aWN5Z3VpOmljeWd1aS10ZXN0")
    );
}

#[tokio::test]
async fn maps_http_errors() {
    let pki = Pki::new();
    let server = server_with(
        pki.issue(SERVER_NAME, &[SERVER_NAME]),
        |request| match request.path.as_str() {
            "/v1/objects/hosts" => {
                error_json(401, "Unauthorized. Please check your user credentials.")
            }
            "/v1/objects/services" => error_json(403, "Missing permission: objects/query/service"),
            "/v1/objects/comments" => error_json(404, "No objects found."),
            "/v1/objects/downtimes" => error_json(500, "Internal Server Error"),
            "/v1/objects/hostgroups" => Reply::Json(503, "<html>reloading</html>".to_owned()),
            _ => Reply::Json(200, "this is not json".to_owned()),
        },
    )
    .await;
    let client = client(&server, ca_trust(&pki));
    assert_eq!(client.hosts().await.unwrap_err(), ApiError::Unauthorized);
    assert_eq!(
        client.services().await.unwrap_err(),
        ApiError::Forbidden("Missing permission: objects/query/service".to_owned())
    );
    assert_eq!(
        client.comments().await.unwrap_err(),
        ApiError::NotFound("No objects found.".to_owned())
    );
    let server_error = client.downtimes().await.unwrap_err();
    assert_eq!(
        server_error,
        ApiError::Http {
            status: 500,
            message: "Internal Server Error".to_owned()
        }
    );
    assert!(server_error.is_transient());
    assert_eq!(
        client.host_groups().await.unwrap_err(),
        ApiError::Http {
            status: 503,
            message: "<html>reloading</html>".to_owned()
        }
    );
    assert!(matches!(
        client.service_groups().await,
        Err(ApiError::Decode(_))
    ));
}

#[tokio::test]
async fn connection_refused_is_transient() {
    let url = ic_api::Url::parse(&format!("https://127.0.0.1:{}", closed_port().await)).unwrap();
    let client = Client::new(basic_settings(url, TlsSettings::default())).unwrap();
    let error = client.info().await.unwrap_err();
    assert!(matches!(error, ApiError::Connect(_)), "{error:?}");
    assert!(error.is_transient());
}

#[tokio::test]
async fn slow_responses_time_out() {
    let pki = Pki::new();
    let server = server_with(pki.issue(SERVER_NAME, &[SERVER_NAME]), |_| {
        Reply::Slow(Duration::from_secs(5), Box::new(info_reply()))
    })
    .await;
    let mut settings = basic_settings(server.url(), ca_trust(&pki));
    settings.request_timeout = Duration::from_millis(200);
    let error = Client::new(settings).unwrap().info().await.unwrap_err();
    assert_eq!(error, ApiError::Timeout);
    assert!(error.is_transient());
}

fn host_result(name: &str) -> Value {
    json!({
        "name": name, "type": "Host", "joins": {}, "meta": {},
        "attrs": {
            "name": name, "display_name": name, "state": 1, "state_type": 1,
            "last_reachable": false, "downtime_depth": 0, "acknowledgement": 0,
            "last_check_result": { "output": "PING CRITICAL - Packet loss = 100%", "state": 2, "exit_status": 2 }
        }
    })
}

fn service_result(full: &str) -> Value {
    let (host, service) = full.split_once('!').unwrap();
    json!({
        "name": full, "type": "Service",
        "attrs": {
            "host_name": host, "name": service, "state": 2, "state_type": 0,
            "last_check_result": { "output": "CRITICAL\nline 2", "state": 2, "exit_status": 0, "performance_data": ["a=1;2;3"] }
        }
    })
}

#[tokio::test]
async fn queries_post_with_method_override_and_attrs() {
    let pki = Pki::new();
    let server = server_with(
        pki.issue(SERVER_NAME, &[SERVER_NAME]),
        |request| match request.path.as_str() {
            "/v1/objects/hosts" => ok_json(&json!({ "results": [host_result("behind-node-11")] })),
            "/v1/objects/services" => {
                ok_json(&json!({ "results": [service_result("db-prod-03!postgres")] }))
            }
            _ => error_json(404, "nope"),
        },
    )
    .await;
    let client = client(&server, ca_trust(&pki));
    let hosts = client.hosts().await.unwrap();
    assert_eq!(hosts.len(), 1);
    assert_eq!(hosts[0].state, HostState::Unreachable);
    let services = client.services().await.unwrap();
    assert_eq!(services[0].state, ServiceState::Critical);
    assert_eq!(
        services[0].check.result.as_ref().unwrap().long_output,
        "line 2"
    );

    let requests = server.requests();
    for request in &requests {
        assert_eq!(request.method, "POST");
        assert_eq!(request.header("x-http-method-override"), Some("GET"));
        assert_eq!(request.header("accept"), Some("application/json"));
        let body = request.json();
        assert!(body.get("filter").is_none(), "never a filter expression");
        let attrs = body["attrs"].as_array().unwrap();
        assert!(attrs.contains(&json!("last_check_result")));
        assert!(attrs.contains(&json!("last_reachable")));
        assert!(body.get("hosts").is_none() && body.get("services").is_none());
    }
    assert!(
        requests[1].json()["attrs"]
            .as_array()
            .unwrap()
            .contains(&json!("host_name"))
    );
}

#[tokio::test]
async fn queries_retry_without_attrs_when_an_attribute_is_unknown() {
    let pki = Pki::new();
    let server = server_with(pki.issue(SERVER_NAME, &[SERVER_NAME]), |request| {
        if request.json().get("attrs").is_some() {
            ok_json(&json!({ "results": [
                { "name": "a", "type": "Host", "code": 400, "status": "Invalid field specified: flapping_current" }
            ]}))
        } else {
            ok_json(&json!({ "results": [host_result("a")] }))
        }
    })
    .await;
    let hosts = client(&server, ca_trust(&pki)).hosts().await.unwrap();
    assert_eq!(hosts.len(), 1);
    assert_eq!(server.requests().len(), 2);
}

#[tokio::test]
async fn status_combines_application_and_cib() {
    let pki = Pki::new();
    let server = server_with(pki.issue(SERVER_NAME, &[SERVER_NAME]), |request| {
        match request.path.as_str() {
            "/v1/status/IcingaApplication" => ok_json(&json!({ "results": [{ "name": "IcingaApplication", "perfdata": [], "status": { "icingaapplication": { "app": {
                "enable_notifications": false, "enable_host_checks": true, "enable_service_checks": true,
                "enable_event_handlers": true, "enable_flapping": true, "enable_perfdata": true,
                "node_name": "master-01", "program_start": 1_791_203_139.1, "version": "r2.15.0-1"
            }}}}]})),
            "/v1/status/CIB" => ok_json(&json!({ "results": [{ "name": "CIB", "perfdata": [], "status": {
                "active_host_checks_1min": 7, "active_service_checks_1min": 60.0, "avg_latency": 0.25, "avg_execution_time": 1.5
            }}]})),
            _ => error_json(404, "nope"),
        }
    })
    .await;
    let status = client(&server, ca_trust(&pki)).status().await.unwrap();
    assert_eq!(status.node_name, "master-01");
    assert_eq!(status.version, "r2.15.0-1");
    assert!(!status.notifications_enabled);
    assert!((status.checks_per_minute - 67.0).abs() < f64::EPSILON);
    assert!((status.avg_execution_time - 1.5).abs() < f64::EPSILON);
    let methods: Vec<String> = server.requests().iter().map(|r| r.method.clone()).collect();
    assert_eq!(methods, ["GET", "GET"]);
}

#[tokio::test]
async fn endpoints_get_their_zone_from_zones() {
    let pki = Pki::new();
    let server = server_with(
        pki.issue(SERVER_NAME, &[SERVER_NAME]),
        |request| match request.path.as_str() {
            "/v1/objects/endpoints" => ok_json(&json!({ "results": [
                { "name": "master-01", "attrs": { "connected": false, "zone": "" } },
                { "name": "agent-07", "attrs": { "connected": true, "zone": "" } }
            ]})),
            "/v1/objects/zones" => ok_json(&json!({ "results": [
                { "name": "master", "attrs": { "endpoints": ["master-01"] } },
                { "name": "global-templates", "attrs": { "endpoints": null } }
            ]})),
            _ => error_json(404, "nope"),
        },
    )
    .await;
    let endpoints = client(&server, ca_trust(&pki)).endpoints().await.unwrap();
    assert_eq!(endpoints[0].zone, "master");
    assert_eq!(endpoints[1].zone, "", "not in any zone");
    assert!(endpoints[1].connected);
}

#[tokio::test]
async fn endpoints_without_zone_permission_still_load() {
    let pki = Pki::new();
    let server = server_with(
        pki.issue(SERVER_NAME, &[SERVER_NAME]),
        |request| match request.path.as_str() {
            "/v1/objects/endpoints" => ok_json(&json!({ "results": [
                { "name": "master-01", "attrs": { "connected": true, "zone": "master" } }
            ]})),
            _ => error_json(403, "Missing permission: objects/query/zone"),
        },
    )
    .await;
    let endpoints = client(&server, ca_trust(&pki)).endpoints().await.unwrap();
    assert_eq!(endpoints[0].zone, "master");
}

/// Answers name-list queries like Icinga: 404 if any name is unknown.
fn name_list_handler(known: &'static [&'static str]) -> impl Fn(&support::Recorded) -> Reply {
    move |request| {
        let body = request.json();
        let (key, make): (&str, fn(&str) -> Value) = if request.path == "/v1/objects/hosts" {
            ("hosts", host_result)
        } else {
            ("services", service_result)
        };
        let names: Vec<&str> = body[key]
            .as_array()
            .map(|names| names.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        if names.is_empty() {
            return error_json(400, "test server: an empty name list returns every object");
        }
        if names.iter().any(|name| !known.contains(name)) {
            return error_json(404, "No objects found.");
        }
        ok_json(&json!({ "results": names.iter().map(|name| make(name)).collect::<Vec<_>>() }))
    }
}

#[tokio::test]
async fn objects_requery_by_name_and_skip_deleted_ones() {
    let pki = Pki::new();
    let server = server_with(
        pki.issue(SERVER_NAME, &[SERVER_NAME]),
        name_list_handler(&["a", "b", "c", "d", "a!s1", "a!s2"]),
    )
    .await;
    let keys = vec![
        ObjectKey::host("a"),
        ObjectKey::host("gone"),
        ObjectKey::host("b"),
        ObjectKey::host("c"),
        ObjectKey::host("a"),
        ObjectKey::host("d"),
        ObjectKey::service("a", "s1"),
        ObjectKey::service("a", "s2"),
    ];
    let (hosts, services) = client(&server, ca_trust(&pki))
        .objects(&keys)
        .await
        .unwrap();
    let names: Vec<&str> = hosts.iter().map(|h| h.name.as_str()).collect();
    assert_eq!(names, ["a", "b", "c", "d"]);
    assert_eq!(services.len(), 2);
    let requests = server.requests();
    assert!(requests.iter().all(|r| r.json().get("filter").is_none()));
    let first_hosts = requests
        .iter()
        .find(|r| r.path == "/v1/objects/hosts")
        .unwrap()
        .json();
    assert_eq!(
        first_hosts["hosts"],
        json!(["a", "gone", "b", "c", "d"]),
        "deduplicated"
    );
    assert_eq!(
        requests
            .iter()
            .filter(|r| r.path == "/v1/objects/services")
            .count(),
        1
    );
}

#[tokio::test]
async fn objects_are_requested_in_chunks() {
    let pki = Pki::new();
    let server = server_with(pki.issue(SERVER_NAME, &[SERVER_NAME]), |request| {
        let body = request.json();
        let names: Vec<String> = body["hosts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n.as_str().unwrap().to_owned())
            .collect();
        ok_json(&json!({ "results": names.iter().map(|n| host_result(n)).collect::<Vec<_>>() }))
    })
    .await;
    let keys: Vec<ObjectKey> = (0..450)
        .map(|i| ObjectKey::host(&format!("h{i}")))
        .collect();
    let (hosts, services) = client(&server, ca_trust(&pki))
        .objects(&keys)
        .await
        .unwrap();
    assert_eq!(hosts.len(), 450);
    assert!(services.is_empty());
    let sizes: Vec<usize> = server
        .requests()
        .iter()
        .map(|r| r.json()["hosts"].as_array().unwrap().len())
        .collect();
    assert_eq!(
        sizes,
        [ic_api::NAMES_PER_REQUEST, ic_api::NAMES_PER_REQUEST, 50]
    );
}

#[tokio::test]
async fn no_keys_send_no_request() {
    let pki = Pki::new();
    let server = info_server(&pki).await;
    let client = client(&server, ca_trust(&pki));
    let (hosts, services) = client.objects(&[]).await.unwrap();
    assert!(hosts.is_empty() && services.is_empty());
    let results = client
        .run_action(
            &Action::RemoveAcknowledgement,
            &ActionTarget::Objects(Vec::new()),
            "me",
        )
        .await
        .unwrap();
    assert!(results.is_empty());
    assert!(
        server.requests().is_empty(),
        "an empty name list means *all* objects to Icinga"
    );
}

// --- Actions -----------------------------------------------------------------

/// The names an action request targets.
fn action_names(request: &support::Recorded) -> Vec<String> {
    let body = request.json();
    body.get("hosts")
        .or_else(|| body.get("services"))
        .and_then(Value::as_array)
        .map(|names| {
            names
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Acknowledges like Icinga: objects whose name ends in "ok" are OK and
/// refuse with 409; the HTTP status follows `actionshandler.cpp`.
fn acknowledging(request: &support::Recorded) -> Reply {
    let results = action_names(request)
        .iter()
        .map(|name| {
            if name.ends_with("ok") {
                json!({ "code": 409, "status": format!("Service {name} is OK.") })
            } else {
                json!({ "code": 200, "status": format!("Successfully acknowledged problem for object '{name}'.") })
            }
        })
        .collect();
    icinga_action_reply(results)
}

fn acknowledge() -> Action {
    Action::Acknowledge {
        comment: "on it".to_owned(),
        sticky: false,
        persistent: false,
        expiry: None,
    }
}

#[tokio::test]
async fn actions_split_hosts_and_services_and_return_per_object_results() {
    let pki = Pki::new();
    let server = server_with(pki.issue(SERVER_NAME, &[SERVER_NAME]), acknowledging).await;
    let target = ActionTarget::Objects(vec![
        ObjectKey::service("db", "pg"),
        ObjectKey::host("k8s-node-11"),
        ObjectKey::service("db", "ok"),
    ]);
    // The service request is answered with HTTP 409: one object failed.
    let results = client(&server, ca_trust(&pki))
        .run_action(&acknowledge(), &target, "j.berg")
        .await
        .unwrap();
    assert_eq!(results.len(), 3);
    assert_eq!(results[0].target.as_deref(), Some("k8s-node-11"));
    assert!(results[0].is_success());
    assert_eq!(results[1].target.as_deref(), Some("db!pg"));
    assert!(
        results[1].is_success(),
        "acknowledged despite the 409 next to it"
    );
    assert_eq!(results[2].target.as_deref(), Some("db!ok"));
    assert_eq!(results[2].code, 409);
    assert_eq!(
        results[2].status, "Service db!ok is OK.",
        "Icinga's own message"
    );
    assert!(!results[2].is_success());

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/v1/actions/acknowledge-problem");
        assert_eq!(request.header("accept"), Some("application/json"));
        assert_eq!(request.header("x-http-method-override"), None);
        assert_eq!(request.json()["notify"], json!(false));
        assert_eq!(request.json()["author"], json!("j.berg"));
    }
    assert_eq!(requests[0].json()["type"], json!("Host"));
    assert_eq!(requests[0].json()["hosts"], json!(["k8s-node-11"]));
    assert_eq!(requests[1].json()["type"], json!("Service"));
    assert_eq!(requests[1].json()["services"], json!(["db!pg", "db!ok"]));
}

#[tokio::test]
async fn actions_on_deleted_objects_get_404_results() {
    let pki = Pki::new();
    let server = server_with(pki.issue(SERVER_NAME, &[SERVER_NAME]), |request| {
        let body = request.json();
        let names: Vec<&str> = body["hosts"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        if names.contains(&"gone") {
            return error_json(404, "No objects found.");
        }
        ok_json(&json!({ "results": names.iter().map(|n| json!({ "code": 200, "status": format!("Successfully rescheduled check for object '{n}'.") })).collect::<Vec<_>>() }))
    })
    .await;
    let target = ActionTarget::Objects(vec![
        ObjectKey::host("a"),
        ObjectKey::host("gone"),
        ObjectKey::host("b"),
    ]);
    let results = client(&server, ca_trust(&pki))
        .run_action(&Action::CheckNow { force: true }, &target, "me")
        .await
        .unwrap();
    let mut summary: Vec<(String, u16)> = results
        .iter()
        .map(|r| (r.target.clone().unwrap(), r.code))
        .collect();
    summary.sort();
    assert_eq!(
        summary,
        [
            ("a".to_owned(), 200),
            ("b".to_owned(), 200),
            ("gone".to_owned(), 404)
        ]
    );
    let first = server.requests()[0].json();
    assert_eq!(first["force"], json!(true));
    assert!(
        first.get("next_check").is_none(),
        "Icinga's own clock decides what now is"
    );
}

#[tokio::test]
async fn action_errors_map_to_api_errors() {
    let pki = Pki::new();
    let server = server_with(
        pki.issue(SERVER_NAME, &[SERVER_NAME]),
        |request| match request.path.as_str() {
            "/v1/actions/acknowledge-problem" => {
                error_json(403, "Missing permission: actions/acknowledge-problem")
            }
            _ => error_json(404, "Action 'execute-command' does not exist."),
        },
    )
    .await;
    let client = client(&server, ca_trust(&pki));
    let target = ActionTarget::Objects(vec![ObjectKey::host("a"), ObjectKey::host("b")]);
    let ack = Action::Acknowledge {
        comment: String::new(),
        sticky: false,
        persistent: false,
        expiry: None,
    };
    assert_eq!(
        client.run_action(&ack, &target, "me").await.unwrap_err(),
        ApiError::Forbidden("Missing permission: actions/acknowledge-problem".to_owned())
    );
    let execute = Action::ExecuteCommand {
        command_type: ic_model::CommandType::CheckCommand,
        command: None,
        endpoint: Some("master-01".to_owned()),
        macros: ic_model::Vars::new(),
        ttl: 30.0,
    };
    assert_eq!(
        client
            .run_action(&execute, &target, "me")
            .await
            .unwrap_err(),
        ApiError::NotFound("Action 'execute-command' does not exist.".to_owned()),
        "a missing action isn't retried name by name"
    );
    assert_eq!(server.requests().len(), 2);
}

#[tokio::test]
async fn removing_one_downtime_or_comment() {
    let pki = Pki::new();
    let server = server_with(pki.issue(SERVER_NAME, &[SERVER_NAME]), |request| {
        ok_json(&json!({ "results": [{ "code": 200, "status": format!("Successfully removed {}", request.path) }] }))
    })
    .await;
    let client = client(&server, ca_trust(&pki));
    let downtime = client
        .run_action(
            &Action::RemoveAllDowntimes,
            &ActionTarget::Downtime("h!e96da238".to_owned()),
            "me",
        )
        .await
        .unwrap();
    assert_eq!(downtime[0].target.as_deref(), Some("h!e96da238"));
    client
        .run_action(
            &Action::RemoveAllDowntimes,
            &ActionTarget::Comment("h!s!dc3b4066".to_owned()),
            "me",
        )
        .await
        .unwrap();
    let requests = server.requests();
    assert_eq!(requests[0].path, "/v1/actions/remove-downtime");
    assert_eq!(
        requests[0].json(),
        json!({ "type": "Downtime", "downtime": "h!e96da238", "author": "me" })
    );
    assert_eq!(requests[1].path, "/v1/actions/remove-comment");
    assert_eq!(
        requests[1].json(),
        json!({ "type": "Comment", "comment": "h!s!dc3b4066", "author": "me" })
    );
}

// --- Event stream ----------------------------------------------------------

struct StreamServer {
    server: TestServer,
    chunks: mpsc::UnboundedSender<Result<Bytes, std::io::Error>>,
    pki: Pki,
}

async fn stream_server() -> StreamServer {
    let pki = Pki::new();
    let (sender, receiver) = mpsc::unbounded_channel();
    let receiver = std::sync::Mutex::new(Some(receiver));
    let opened = Arc::new(AtomicUsize::new(0));
    let server = server_with(pki.issue(SERVER_NAME, &[SERVER_NAME]), move |request| {
        if request.path != "/v1/events" {
            return error_json(404, "nope");
        }
        opened.fetch_add(1, Ordering::SeqCst);
        match receiver.lock().unwrap().take() {
            Some(receiver) => Reply::Stream(receiver),
            None => error_json(500, "stream already taken"),
        }
    })
    .await;
    StreamServer {
        server,
        chunks: sender,
        pki,
    }
}

const STATE_CHANGE: &str = r#"{"acknowledgement": false, "check_result": {"active": false, "check_source": "icinga-master", "command": null, "execution_end": 1791203174.695258, "execution_start": 1791203174.695258, "exit_status": 0, "output": "DISK CRITICAL - /var 97% used (1.2 GiB free)", "performance_data": ["/var=97%;80;90;0;100"], "previous_hard_state": 99, "schedule_end": 1791203174.695258, "schedule_start": 1791203174.695258, "scheduling_source": "icinga-master", "state": 2, "ttl": 0, "type": "CheckResult", "vars_after": {"attempt": 2, "reachable": true, "state": 2, "state_type": 0}, "vars_before": null}, "downtime_depth": 0, "host": "k8s-node-07", "service": "disk /var", "state": 2, "state_type": 0, "timestamp": 1791203174.695517, "type": "StateChange"}"#;

const CREATED: &str = r#"{"object_name": "k8s-node-07!disk /var!efa9ecb5", "object_type": "Comment", "timestamp": 1791203175.847941, "type": "ObjectCreated"}"#;

#[tokio::test]
async fn event_stream_reassembles_split_lines_and_skips_junk() {
    let StreamServer {
        server,
        chunks,
        pki,
    } = stream_server().await;
    let client = client(&server, ca_trust(&pki));
    let mut stream = client
        .events(
            "icygui-test",
            &[EventKind::StateChange, EventKind::ObjectCreated],
        )
        .await
        .unwrap();

    let request = &server.requests()[0];
    assert_eq!(request.method, "POST");
    assert_eq!(
        request.json(),
        json!({ "queue": "icygui-test", "types": ["StateChange", "ObjectCreated"] })
    );

    // One event split across three chunks, a malformed line and an
    // unknown event type in between, then another event.
    let (a, rest) = STATE_CHANGE.split_at(40);
    let (b, c) = rest.split_at(300);
    for part in [a, b] {
        chunks.send(Ok(Bytes::from(part.to_owned()))).unwrap();
    }
    chunks
        .send(Ok(Bytes::from(format!(
            "{c}\n{{\"type\": \"StateChange\", broken\n{{\"type\":\"Notification\",\"host\":\"h\",\"users\":[\"a\"]}}\n{}",
            &CREATED[..10]
        ))))
        .unwrap();
    chunks
        .send(Ok(Bytes::from(format!("{}\n", &CREATED[10..]))))
        .unwrap();

    let first = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let Event::StateChange { object, state, .. } = first else {
        panic!("wrong event {first:?}");
    };
    assert_eq!(object, ObjectKey::service("k8s-node-07", "disk /var"));
    assert_eq!(
        state,
        ic_model::CheckableState::Service(ServiceState::Critical)
    );
    let second = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(matches!(
        second,
        Event::ObjectLifecycle {
            change: ObjectChange::Created,
            ..
        }
    ));

    // The server ends the response: the stream ends.
    drop(chunks);
    let end = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .unwrap();
    assert!(end.is_none(), "{end:?}");
}

#[tokio::test]
async fn event_stream_ends_on_abrupt_disconnect() {
    let StreamServer {
        server,
        chunks,
        pki,
    } = stream_server().await;
    let mut stream = client(&server, ca_trust(&pki))
        .events("q", &EventKind::ALL)
        .await
        .unwrap();
    chunks
        .send(Ok(Bytes::from(format!("{CREATED}\n"))))
        .unwrap();
    let first = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .unwrap();
    assert!(matches!(first, Some(Ok(Event::ObjectLifecycle { .. }))));
    chunks
        .send(Err(std::io::Error::other("connection lost")))
        .unwrap();
    let error = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .unwrap();
    let Some(Err(error)) = error else {
        panic!("expected an error, got {error:?}");
    };
    assert!(error.is_transient(), "{error:?}");
    let end = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .unwrap();
    assert!(end.is_none());
}

#[tokio::test]
async fn event_stream_has_no_read_timeout() {
    let StreamServer {
        server,
        chunks,
        pki,
    } = stream_server().await;
    let mut settings = basic_settings(server.url(), ca_trust(&pki));
    settings.request_timeout = Duration::from_millis(200);
    let mut stream = Client::new(settings)
        .unwrap()
        .events("q", &[EventKind::ObjectCreated])
        .await
        .unwrap();
    // Silence for longer than the request timeout.
    tokio::time::sleep(Duration::from_millis(600)).await;
    chunks
        .send(Ok(Bytes::from(format!("{CREATED}\n"))))
        .unwrap();
    let event = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .unwrap();
    assert!(matches!(event, Some(Ok(_))), "{event:?}");
}

/// Icinga's answer to `/v1/events` when an `events/<type>` permission is
/// missing (recorded from 2.15.6): its generic 404, not a 403.
const EVENTS_HIDDEN_404: &str = "The requested path 'v1/events' could not be found or the request method is not valid for this path.";

/// `GET /v1` for the contract fixtures' `viewer` user.
fn viewer_info() -> Reply {
    ok_json(&json!({ "results": [{
        "permissions": ["objects/query/Host", "objects/query/Service", "status/query", "events/CheckResult", "events/StateChange"],
        "user": "viewer",
        "version": "v2.15.6"
    }]}))
}

#[tokio::test]
async fn a_missing_event_permission_is_forbidden_and_named() {
    let pki = Pki::new();
    let server = server_with(
        pki.issue(SERVER_NAME, &[SERVER_NAME]),
        |request| match request.path.as_str() {
            "/v1/events" => error_json(404, EVENTS_HIDDEN_404),
            "/v1" => viewer_info(),
            _ => error_json(404, "nope"),
        },
    )
    .await;
    let client = client(&server, ca_trust(&pki));
    let error = client.events("q", &EventKind::ALL).await.unwrap_err();
    let ApiError::Forbidden(message) = &error else {
        panic!("expected Forbidden, got {error:?}");
    };
    assert!(message.starts_with("Missing permission: "), "{message}");
    assert!(message.contains("events/AcknowledgementSet"), "{message}");
    assert!(message.contains("events/ObjectDeleted"), "{message}");
    assert!(
        !message.contains("events/StateChange"),
        "the viewer may see state changes: {message}"
    );
    assert!(!error.is_transient());
}

#[tokio::test]
async fn a_hidden_event_error_without_readable_permissions_is_still_forbidden() {
    let pki = Pki::new();
    let server = server_with(
        pki.issue(SERVER_NAME, &[SERVER_NAME]),
        |request| match request.path.as_str() {
            "/v1/events" => error_json(404, EVENTS_HIDDEN_404),
            _ => error_json(500, "Internal Server Error"),
        },
    )
    .await;
    let error = client(&server, ca_trust(&pki))
        .events("q", &[EventKind::ObjectCreated])
        .await
        .unwrap_err();
    assert_eq!(
        error,
        ApiError::Forbidden("Missing permission: one of events/ObjectCreated".to_owned())
    );
}

#[tokio::test]
async fn a_404_from_events_with_every_permission_stays_not_found() {
    let pki = Pki::new();
    let server = server_with(
        pki.issue(SERVER_NAME, &[SERVER_NAME]),
        |request| match request.path.as_str() {
            "/v1/events" => error_json(404, EVENTS_HIDDEN_404),
            _ => info_reply(),
        },
    )
    .await;
    assert_eq!(
        client(&server, ca_trust(&pki))
            .events("q", &[EventKind::StateChange])
            .await
            .unwrap_err(),
        ApiError::NotFound(EVENTS_HIDDEN_404.to_owned())
    );
}

#[tokio::test]
async fn event_stream_arguments_are_checked_before_sending() {
    let pki = Pki::new();
    let server = info_server(&pki).await;
    let client = client(&server, ca_trust(&pki));
    assert!(matches!(
        client.events("q", &[]).await,
        Err(ApiError::InvalidSettings(_))
    ));
    // Icinga 2.15 answers 400 "'queue' query parameter is required."
    for queue in ["", "   "] {
        assert!(matches!(
            client.events(queue, &[EventKind::StateChange]).await,
            Err(ApiError::InvalidSettings(message)) if message.contains("queue")
        ));
    }
    assert!(server.requests().is_empty());
}

// --- TLS: validity, TLS 1.2, IP addresses, SNI ---------------------------

#[tokio::test]
async fn expired_and_not_yet_valid_certificates_are_rejected() {
    let pki = Pki::new();
    let expired = server_with(
        pki.issue_valid(SERVER_NAME, &[SERVER_NAME], (2000, 1, 1), (2001, 1, 1)),
        |_| info_reply(),
    )
    .await;
    let error = client(&expired, ca_trust(&pki)).info().await.unwrap_err();
    assert!(
        matches!(&error, ApiError::Tls(message) if message.contains("expired")),
        "{error:?}"
    );
    assert!(!error.is_transient());

    let future = server_with(
        pki.issue_valid(SERVER_NAME, &[SERVER_NAME], (2090, 1, 1), (2091, 1, 1)),
        |_| info_reply(),
    )
    .await;
    let error = client(&future, ca_trust(&pki)).info().await.unwrap_err();
    assert!(
        matches!(&error, ApiError::Tls(message) if message.contains("not valid yet")),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_pinned_certificate_is_accepted_whatever_its_dates() {
    // Pinning means exactly this certificate, which the user compared and
    // accepted: chain, name and validity dates are not checked.
    let pki = Pki::new();
    let server = server_with(
        pki.issue_valid(SERVER_NAME, &[SERVER_NAME], (2000, 1, 1), (2001, 1, 1)),
        |_| info_reply(),
    )
    .await;
    let tls = TlsSettings {
        pinned_sha256: Some(server.sha256()),
        ..TlsSettings::default()
    };
    assert!(client(&server, tls).info().await.is_ok());
}

#[tokio::test]
async fn tls12_only_servers_work_with_pins_and_with_ca_trust() {
    let pki = Pki::new();
    let server = TestServer::start(ServerOptions {
        tls12_only: true,
        ..ServerOptions::new(pki.issue(SERVER_NAME, &[SERVER_NAME]), |_| info_reply())
    })
    .await;
    let pinned = TlsSettings {
        pinned_sha256: Some(server.sha256()),
        ..TlsSettings::default()
    };
    client(&server, pinned).info().await.unwrap();
    client(&server, ca_trust(&pki)).info().await.unwrap();
    let wrong_pin = TlsSettings {
        pinned_sha256: Some([7; 32]),
        ..TlsSettings::default()
    };
    assert!(matches!(
        client(&server, wrong_pin).info().await,
        Err(ApiError::CertificateMismatch { .. })
    ));
}

#[tokio::test]
async fn ip_address_urls_match_ip_subject_alt_names() {
    let pki = Pki::new();
    let server = server_with(pki.issue(SERVER_NAME, &["127.0.0.1"]), |_| info_reply()).await;
    let tls = TlsSettings {
        ca_pem: Some(pki.ca_pem().into_bytes()),
        ..TlsSettings::default()
    };
    client(&server, tls.clone()).info().await.unwrap();
    // Another address isn't the same thing.
    let other = server_with(pki.issue(SERVER_NAME, &["10.0.0.1"]), |_| info_reply()).await;
    assert!(matches!(
        client(&other, tls).info().await,
        Err(ApiError::Tls(_))
    ));
}

#[tokio::test]
async fn ip_address_urls_fall_back_to_the_common_name_without_san() {
    let pki = Pki::new();
    let server = server_with(pki.issue("127.0.0.1", &[]), |_| info_reply()).await;
    let tls = TlsSettings {
        ca_pem: Some(pki.ca_pem().into_bytes()),
        ..TlsSettings::default()
    };
    client(&server, tls).info().await.unwrap();
}

#[tokio::test]
async fn a_blank_server_name_counts_as_unset() {
    // A cleared settings field may arrive as `Some("")`: the URL's host
    // (127.0.0.1, in the certificate) is verified as if nothing was set.
    let pki = Pki::new();
    let server = server_with(pki.issue(SERVER_NAME, &["127.0.0.1"]), |_| info_reply()).await;
    for blank in ["", "  "] {
        let tls = TlsSettings {
            ca_pem: Some(pki.ca_pem().into_bytes()),
            server_name: Some(blank.to_owned()),
            ..TlsSettings::default()
        };
        client(&server, tls).info().await.unwrap();
        assert!(
            fetch_server_certificate(&server.url(), Some(blank))
                .await
                .is_ok()
        );
    }
    let invalid = TlsSettings {
        server_name: Some("not a name!".to_owned()),
        ..ca_trust(&pki)
    };
    assert!(matches!(
        Client::new(basic_settings(server.url(), invalid)),
        Err(ApiError::InvalidSettings(_))
    ));
    assert!(matches!(
        fetch_server_certificate(&server.url(), Some("not a name!")).await,
        Err(ApiError::InvalidSettings(_))
    ));
}

#[tokio::test]
async fn fetching_a_certificate_sends_the_same_sni_as_the_client() {
    let pki = Pki::new();
    let server = info_server(&pki).await;
    let by_name = ic_api::Url::parse(&format!("https://localhost:{}", server.addr.port())).unwrap();
    // The override is only for verifying: a proxy that picks (or routes)
    // by SNI must see the same name for the fetch and for the client, or
    // the pinned certificate isn't the one the client gets.
    let certificate = fetch_server_certificate(&by_name, Some(SERVER_NAME))
        .await
        .unwrap();
    let tls = TlsSettings {
        pinned_sha256: Some(certificate.sha256),
        server_name: Some(SERVER_NAME.to_owned()),
        ..TlsSettings::default()
    };
    Client::new(basic_settings(by_name, tls))
        .unwrap()
        .info()
        .await
        .unwrap();
    assert_eq!(
        server.handshakes(),
        [Some("localhost".to_owned()), Some("localhost".to_owned())]
    );

    // IP addresses: no SNI either way.
    let pki = Pki::new();
    let server = info_server(&pki).await;
    fetch_server_certificate(&server.url(), Some(SERVER_NAME))
        .await
        .unwrap();
    client(&server, ca_trust(&pki)).info().await.unwrap();
    assert_eq!(server.handshakes(), [None, None]);
}

#[tokio::test]
async fn fetching_a_certificate_from_a_server_that_wants_a_client_certificate() {
    // The handshake fails after the certificate arrived: that's enough.
    let pki = Pki::new();
    let server = TestServer::start(ServerOptions {
        client_ca: Some(pki.ca_pem()),
        ..ServerOptions::new(pki.issue(SERVER_NAME, &[SERVER_NAME]), |_| info_reply())
    })
    .await;
    let certificate = fetch_server_certificate(&server.url(), None).await.unwrap();
    assert_eq!(certificate.sha256, server.sha256());
}

/// A TCP server that accepts connections and never says anything.
async fn silent_server() -> (ic_api::Url, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = ic_api::Url::parse(&format!("https://{}", listener.local_addr().unwrap())).unwrap();
    let task = tokio::spawn(async move {
        let mut open = Vec::new();
        while let Ok((tcp, _)) = listener.accept().await {
            open.push(tcp);
        }
    });
    (url, task)
}

#[tokio::test]
async fn requests_to_a_silent_server_time_out() {
    let (url, task) = silent_server().await;
    let mut settings = basic_settings(url, TlsSettings::default());
    settings.request_timeout = Duration::from_millis(300);
    let client = Client::new(settings).unwrap();
    let started = std::time::Instant::now();
    assert_eq!(client.info().await.unwrap_err(), ApiError::Timeout);
    assert_eq!(
        client
            .events("q", &[EventKind::StateChange])
            .await
            .unwrap_err(),
        ApiError::Timeout
    );
    assert!(started.elapsed() < Duration::from_secs(5));
    task.abort();
}

// --- Security --------------------------------------------------------------

#[tokio::test]
async fn debug_output_never_shows_secrets() {
    let pki = Pki::new();
    let identity = pki.issue("icygui-client", &[]);
    let key_body: String = identity
        .key_pem
        .lines()
        .filter(|line| !line.starts_with("-----"))
        .collect();
    let url = ic_api::Url::parse("https://127.0.0.1:5665").unwrap();

    let basic = basic_settings(url.clone(), ca_trust(&pki));
    let text = format!("{basic:?}");
    assert!(!text.contains(PASSWORD), "{text}");
    assert!(text.contains("icygui"), "the user name is fine: {text}");
    let client = Client::new(basic).unwrap();
    let text = format!("{client:?}");
    assert!(!text.contains(PASSWORD), "{text}");

    let certificate = certificate_settings(url, ca_trust(&pki), &identity);
    let text = format!("{certificate:?}");
    assert!(!key_body.is_empty());
    for line in identity
        .key_pem
        .lines()
        .filter(|line| !line.starts_with("-----"))
    {
        assert!(!text.contains(line), "{text}");
    }
    assert!(!text.contains("PRIVATE KEY"), "{text}");
    let client = Client::new(certificate).unwrap();
    assert!(!format!("{client:?}").contains("PRIVATE KEY"));
}

#[tokio::test]
async fn redirects_are_not_followed() {
    let pki = Pki::new();
    let server = server_with(pki.issue(SERVER_NAME, &[SERVER_NAME]), |request| {
        if request.path == "/v1" {
            Reply::Redirect("https://elsewhere.example/v1".to_owned())
        } else {
            info_reply()
        }
    })
    .await;
    let error = client(&server, ca_trust(&pki)).info().await.unwrap_err();
    assert!(
        matches!(error, ApiError::Http { status: 302, .. }),
        "{error:?}"
    );
    assert_eq!(server.requests().len(), 1);
}

// --- Bodies and timeouts ---------------------------------------------------

/// A 200 response sent as `pieces`, `gap` apart; with `forever`, keeps
/// sending a space every `gap` after them.
fn trickle(pieces: Vec<String>, gap: Duration, forever: bool) -> Reply {
    let (sender, receiver) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        for piece in pieces {
            if sender.send(Ok(Bytes::from(piece))).is_err() {
                return;
            }
            tokio::time::sleep(gap).await;
        }
        while forever && sender.send(Ok(Bytes::from_static(b" "))).is_ok() {
            tokio::time::sleep(gap).await;
        }
    });
    Reply::Stream(receiver)
}

#[tokio::test]
async fn slow_but_steady_bodies_are_read_whole() {
    // Longer in total than the request timeout, but never silent for that
    // long: a big object list over a slow link.
    let pki = Pki::new();
    let server = server_with(pki.issue(SERVER_NAME, &[SERVER_NAME]), |_| {
        let document = json!({ "results": (0..30).map(|i| host_result(&format!("h{i}"))).collect::<Vec<_>>() }).to_string();
        let pieces = document
            .as_bytes()
            .chunks(document.len() / 15 + 1)
            .map(|piece| String::from_utf8_lossy(piece).into_owned())
            .collect();
        trickle(pieces, Duration::from_millis(100), false)
    })
    .await;
    let mut settings = basic_settings(server.url(), ca_trust(&pki));
    settings.request_timeout = Duration::from_millis(600);
    let started = std::time::Instant::now();
    let hosts = Client::new(settings).unwrap().hosts().await.unwrap();
    assert_eq!(hosts.len(), 30);
    assert!(started.elapsed() > Duration::from_millis(600));
}

#[tokio::test]
async fn stalled_bodies_time_out() {
    let pki = Pki::new();
    let server = server_with(pki.issue(SERVER_NAME, &[SERVER_NAME]), |_| {
        Reply::Stalled(200, r#"{"results": ["#.to_owned())
    })
    .await;
    let mut settings = basic_settings(server.url(), ca_trust(&pki));
    settings.request_timeout = Duration::from_millis(300);
    let started = std::time::Instant::now();
    let error = Client::new(settings).unwrap().hosts().await.unwrap_err();
    assert_eq!(error, ApiError::Timeout);
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn bodies_that_never_end_time_out_in_total() {
    // A byte every 10 ms never trips the 300 ms pause limit, but the whole
    // body may take at most 20 request timeouts (6 s here).
    let pki = Pki::new();
    let server = server_with(pki.issue(SERVER_NAME, &[SERVER_NAME]), |_| {
        trickle(
            vec![r#"{"results": []"#.to_owned()],
            Duration::from_millis(10),
            true,
        )
    })
    .await;
    let mut settings = basic_settings(server.url(), ca_trust(&pki));
    settings.request_timeout = Duration::from_millis(300);
    let started = std::time::Instant::now();
    let error = Client::new(settings).unwrap().hosts().await.unwrap_err();
    assert_eq!(error, ApiError::Timeout);
    let elapsed = started.elapsed();
    assert!(elapsed >= Duration::from_secs(4), "{elapsed:?}");
    assert!(elapsed < Duration::from_secs(30), "{elapsed:?}");
}

#[tokio::test]
async fn a_stalled_error_body_does_not_hang_the_event_stream() {
    // A proxy (or a dying connection) sends an error status, then nothing.
    let pki = Pki::new();
    let server = server_with(pki.issue(SERVER_NAME, &[SERVER_NAME]), |_| {
        Reply::Stalled(503, r#"{"error":503,"#.to_owned())
    })
    .await;
    let mut settings = basic_settings(server.url(), ca_trust(&pki));
    settings.request_timeout = Duration::from_millis(300);
    let error = tokio::time::timeout(
        Duration::from_secs(5),
        Client::new(settings)
            .unwrap()
            .events("q", &[EventKind::StateChange]),
    )
    .await
    .expect("events() returns")
    .unwrap_err();
    assert!(
        matches!(error, ApiError::Http { status: 503, .. }),
        "{error:?}"
    );
    assert!(error.is_transient());
}

#[tokio::test]
async fn the_event_stream_must_begin_within_the_request_timeout() {
    let pki = Pki::new();
    let server = server_with(pki.issue(SERVER_NAME, &[SERVER_NAME]), |_| {
        Reply::Slow(Duration::from_secs(5), Box::new(info_reply()))
    })
    .await;
    let mut settings = basic_settings(server.url(), ca_trust(&pki));
    settings.request_timeout = Duration::from_millis(200);
    let started = std::time::Instant::now();
    assert_eq!(
        Client::new(settings)
            .unwrap()
            .events("q", &[EventKind::StateChange])
            .await
            .unwrap_err(),
        ApiError::Timeout
    );
    assert!(started.elapsed() < Duration::from_secs(3));
}

// --- Queries: more cases ---------------------------------------------------

#[tokio::test]
async fn targeted_queries_retry_without_attrs_too() {
    let pki = Pki::new();
    let server = server_with(pki.issue(SERVER_NAME, &[SERVER_NAME]), |request| {
        let body = request.json();
        let names: Vec<&str> = body["hosts"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        let results: Vec<Value> = if body.get("attrs").is_some() {
            names
                .iter()
                .map(|name| json!({ "name": name, "type": "Host", "code": 400, "status": "Invalid field specified: flapping_current" }))
                .collect()
        } else {
            names.iter().map(|name| host_result(name)).collect()
        };
        ok_json(&json!({ "results": results }))
    })
    .await;
    let (hosts, _) = client(&server, ca_trust(&pki))
        .objects(&[ObjectKey::host("a"), ObjectKey::host("b")])
        .await
        .unwrap();
    assert_eq!(hosts.len(), 2);
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[1].json().get("attrs").is_none());
    assert_eq!(requests[1].json()["hosts"], json!(["a", "b"]));
}

#[tokio::test]
async fn status_fails_when_either_request_fails() {
    let pki = Pki::new();
    let server = server_with(
        pki.issue(SERVER_NAME, &[SERVER_NAME]),
        |request| match request.path.as_str() {
            "/v1/status/IcingaApplication" => ok_json(
                &json!({ "results": [{ "name": "IcingaApplication", "status": { "icingaapplication": { "app": { "node_name": "m" } } } }] }),
            ),
            _ => error_json(403, "Missing permission: status/query"),
        },
    )
    .await;
    assert_eq!(
        client(&server, ca_trust(&pki)).status().await.unwrap_err(),
        ApiError::Forbidden("Missing permission: status/query".to_owned())
    );
}

#[tokio::test]
async fn endpoints_count_the_local_endpoint_as_connected() {
    let pki = Pki::new();
    let server = server_with(
        pki.issue(SERVER_NAME, &[SERVER_NAME]),
        |request| match request.path.as_str() {
            "/v1/objects/endpoints" => ok_json(&json!({ "results": [
                { "name": "master-01", "attrs": { "connected": false, "zone": "" } },
                { "name": "master-02", "attrs": { "connected": false, "zone": "" } }
            ]})),
            "/v1/objects/zones" => ok_json(&json!({ "results": [
                { "name": "master", "attrs": { "endpoints": ["master-01", "master-02"] } }
            ]})),
            "/v1/status/IcingaApplication" => ok_json(&json!({ "results": [{
                "name": "IcingaApplication",
                "status": { "icingaapplication": { "app": { "node_name": "master-01" } } }
            }]})),
            _ => error_json(404, "nope"),
        },
    )
    .await;
    let endpoints = client(&server, ca_trust(&pki)).endpoints().await.unwrap();
    assert_eq!(endpoints[0].name, "master-01");
    assert!(endpoints[0].connected, "the endpoint we talk to");
    assert!(!endpoints[1].connected, "its peer really is disconnected");
    assert_eq!(endpoints[1].zone, "master");
}

// --- Actions: Icinga's status rules, chunks, partial failures -------------

#[tokio::test]
async fn a_single_failed_object_is_a_result_not_an_error() {
    let pki = Pki::new();
    let server = server_with(pki.issue(SERVER_NAME, &[SERVER_NAME]), acknowledging).await;
    let results = client(&server, ca_trust(&pki))
        .run_action(
            &acknowledge(),
            &ActionTarget::Objects(vec![ObjectKey::service("db", "ok")]),
            "me",
        )
        .await
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].code, 409);
    assert_eq!(results[0].status, "Service db!ok is OK.");
    assert_eq!(results[0].target.as_deref(), Some("db!ok"));
}

#[tokio::test]
async fn several_kinds_of_failure_are_results_not_a_server_error() {
    // Two different failure codes make Icinga answer HTTP 500, which
    // must not look like a (retryable) server error.
    let pki = Pki::new();
    let server = server_with(pki.issue(SERVER_NAME, &[SERVER_NAME]), |request| {
        let results = action_names(request)
            .iter()
            .map(|name| match name.as_str() {
                "a" => json!({ "code": 409, "status": "Host a is UP." }),
                "b" => json!({ "code": 400, "status": "Acknowledgements require author and comment." }),
                _ => json!({ "code": 200, "status": "Successfully acknowledged problem for object 'c'." }),
            })
            .collect();
        icinga_action_reply(results)
    })
    .await;
    let results = client(&server, ca_trust(&pki))
        .run_action(
            &acknowledge(),
            &ActionTarget::Objects(vec![
                ObjectKey::host("a"),
                ObjectKey::host("b"),
                ObjectKey::host("c"),
            ]),
            "me",
        )
        .await
        .unwrap();
    let codes: Vec<(Option<&str>, u16)> = results
        .iter()
        .map(|result| (result.target.as_deref(), result.code))
        .collect();
    assert_eq!(
        codes,
        [(Some("a"), 409), (Some("b"), 400), (Some("c"), 200)]
    );
}

#[tokio::test]
async fn execute_command_without_an_endpoint_is_a_per_object_404() {
    // The recorded answer (HTTP 404 because every object failed with 404):
    // per-object results, not a missing path, and not "No objects found."
    let pki = Pki::new();
    let server = server_with(pki.issue(SERVER_NAME, &[SERVER_NAME]), |request| {
        let results = action_names(request)
            .iter()
            .map(|_| json!({ "code": 404, "status": "Can't find a valid endpoint for ''." }))
            .collect();
        icinga_action_reply(results)
    })
    .await;
    let execute = Action::ExecuteCommand {
        command_type: ic_model::CommandType::CheckCommand,
        command: None,
        endpoint: None,
        macros: ic_model::Vars::new(),
        ttl: 30.0,
    };
    let results = client(&server, ca_trust(&pki))
        .run_action(
            &execute,
            &ActionTarget::Objects(vec![ObjectKey::host("a"), ObjectKey::host("b")]),
            "me",
        )
        .await
        .unwrap();
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|result| result.code == 404));
    assert_eq!(results[1].target.as_deref(), Some("b"));
    assert_eq!(server.requests().len(), 1, "not split like unknown names");
}

#[tokio::test]
async fn a_per_object_403_is_not_a_missing_permission() {
    let pki = Pki::new();
    let server = server_with(pki.issue(SERVER_NAME, &[SERVER_NAME]), |request| {
        let results = action_names(request)
            .iter()
            .map(|name| json!({ "code": 403, "status": format!("Passive checks are disabled for object '{name}'.") }))
            .collect();
        icinga_action_reply(results)
    })
    .await;
    let submit = Action::ProcessCheckResult {
        exit_status: 2,
        output: "DOWN".to_owned(),
        perfdata: Vec::new(),
        ttl: None,
    };
    let results = client(&server, ca_trust(&pki))
        .run_action(
            &submit,
            &ActionTarget::Objects(vec![ObjectKey::host("a")]),
            "me",
        )
        .await
        .unwrap();
    assert_eq!(results[0].code, 403);
    assert!(results[0].status.starts_with("Passive checks are disabled"));
    let body = server.requests()[0].json();
    assert_eq!(
        body["exit_status"],
        json!(1),
        "plugin CRITICAL is host DOWN"
    );
}

#[tokio::test]
async fn large_actions_are_chunked_and_failures_do_not_stop_them() {
    let pki = Pki::new();
    let server = server_with(pki.issue(SERVER_NAME, &[SERVER_NAME]), acknowledging).await;
    // Every tenth host is up ("…ok") and refuses.
    let keys: Vec<ObjectKey> = (0..450)
        .map(|i| {
            if i % 10 == 0 {
                ObjectKey::host(&format!("h{i}-ok"))
            } else {
                ObjectKey::host(&format!("h{i}"))
            }
        })
        .collect();
    let results = client(&server, ca_trust(&pki))
        .run_action(&acknowledge(), &ActionTarget::Objects(keys), "me")
        .await
        .unwrap();
    assert_eq!(results.len(), 450);
    assert_eq!(results.iter().filter(|r| !r.is_success()).count(), 45);
    assert_eq!(results[0].target.as_deref(), Some("h0-ok"));
    assert_eq!(results[0].code, 409);
    assert_eq!(results[449].target.as_deref(), Some("h449"));
    let sizes: Vec<usize> = server
        .requests()
        .iter()
        .map(|request| action_names(request).len())
        .collect();
    assert_eq!(
        sizes,
        [ic_api::NAMES_PER_REQUEST, ic_api::NAMES_PER_REQUEST, 50]
    );
}

#[tokio::test]
async fn a_request_failing_after_others_were_answered_keeps_their_results() {
    // The second of three requests hits a reloading Icinga: the first
    // was applied, so the action reports per object instead of failing
    // as a whole, and nothing more is sent.
    let pki = Pki::new();
    let counter = AtomicUsize::new(0);
    let server = server_with(pki.issue(SERVER_NAME, &[SERVER_NAME]), move |request| {
        if counter.fetch_add(1, Ordering::SeqCst) == 1 {
            return error_json(503, "Shutting down.");
        }
        acknowledging(request)
    })
    .await;
    let keys: Vec<ObjectKey> = (0..450)
        .map(|i| ObjectKey::host(&format!("h{i}")))
        .collect();
    let results = client(&server, ca_trust(&pki))
        .run_action(&acknowledge(), &ActionTarget::Objects(keys), "me")
        .await
        .unwrap();
    assert_eq!(results.len(), 450);
    assert!(results[..200].iter().all(ic_api::ActionResult::is_success));
    assert!(results[200..].iter().all(|result| result.code == 503));
    assert!(
        results[200].status.starts_with("request failed"),
        "{}",
        results[200].status
    );
    assert!(
        results[400].status.starts_with("not sent"),
        "{}",
        results[400].status
    );
    assert_eq!(results[449].target.as_deref(), Some("h449"));
    assert_eq!(server.requests().len(), 2);
}

#[tokio::test]
async fn a_timeout_after_others_were_answered_keeps_their_results() {
    // Host request answered, the service request never: code 0, no HTTP
    // status.
    let pki = Pki::new();
    let server = server_with(pki.issue(SERVER_NAME, &[SERVER_NAME]), |request| {
        if request.json()["type"] == json!("Service") {
            return Reply::Slow(Duration::from_secs(5), Box::new(acknowledging(request)));
        }
        acknowledging(request)
    })
    .await;
    let mut settings = basic_settings(server.url(), ca_trust(&pki));
    settings.request_timeout = Duration::from_millis(300);
    let results = Client::new(settings)
        .unwrap()
        .run_action(
            &acknowledge(),
            &ActionTarget::Objects(vec![ObjectKey::host("a"), ObjectKey::service("a", "s")]),
            "me",
        )
        .await
        .unwrap();
    assert_eq!(results.len(), 2);
    assert!(results[0].is_success());
    assert_eq!(results[1].code, 0);
    assert_eq!(results[1].target.as_deref(), Some("a!s"));
    assert!(
        results[1].status.contains("timed out"),
        "{}",
        results[1].status
    );
}

#[tokio::test]
async fn removing_a_vanished_downtime_or_comment_is_a_404_result() {
    let pki = Pki::new();
    let server = server_with(pki.issue(SERVER_NAME, &[SERVER_NAME]), |_| {
        error_json(404, "No objects found.")
    })
    .await;
    let client = client(&server, ca_trust(&pki));
    for target in [
        ActionTarget::Downtime("h!gone".to_owned()),
        ActionTarget::Comment("h!s!gone".to_owned()),
    ] {
        let results = client
            .run_action(&Action::RemoveAllDowntimes, &target, "me")
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].code, 404);
        assert!(results[0].target.as_deref().unwrap().ends_with("gone"));
    }
}

#[tokio::test]
async fn other_actions_on_a_comment_are_refused_before_sending() {
    let pki = Pki::new();
    let server = info_server(&pki).await;
    let error = client(&server, ca_trust(&pki))
        .run_action(
            &acknowledge(),
            &ActionTarget::Comment("h!c".to_owned()),
            "me",
        )
        .await
        .unwrap_err();
    assert!(matches!(error, ApiError::InvalidSettings(_)), "{error:?}");
    assert!(server.requests().is_empty(), "nothing was removed");
}
