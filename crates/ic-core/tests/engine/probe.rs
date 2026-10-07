//! The settings dialog's helpers: "test connection" and reading the
//! server's certificate, without a running engine.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers fail the test loudly"
)]

use std::collections::BTreeSet;

use crate::support::{Launch, PASSWORD, USER, environment, mock};
use ic_config::AuthConfig;
use ic_core::{
    ClusterView, ConnectionFailure, REQUIRED_PERMISSIONS, fetch_certificate, test_connection,
};
use ic_mock::{MockConfig, MockUser, scenarios};
use secrecy::SecretString;

fn password(text: &str) -> SecretString {
    SecretString::from(text.to_owned())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_good_connection_reports_user_version_and_status() {
    let server = mock(MockConfig::with_scenario(scenarios::staging())).await;
    let report = test_connection(environment(&server), 0, Some(password(PASSWORD)))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(report.info.user, "root");
    assert!(!report.info.version.is_empty());
    assert_eq!(report.status.node_name, "stg-master-01");
    assert!(report.missing_permissions.is_empty());
    // The node, its zone and its view (ENV-12).
    assert_eq!(report.node.name, "stg-master-01");
    assert_eq!(report.node.zone.as_deref(), Some("master"));
    assert_eq!(report.node.view, ClusterView::Full);
    assert_eq!(report.node.url, server.url());
    // Cheap requests only: GET /v1, the status and the zones.
    let paths: Vec<String> = server
        .control()
        .requests()
        .into_iter()
        .map(|request| request.path)
        .collect();
    assert!(
        paths.iter().all(|path| path == "/v1"
            || path.starts_with("/v1/status")
            || path == "/v1/objects/zones"),
        "{paths:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_permissions_are_listed() {
    let server = mock(MockConfig {
        users: vec![
            MockUser::read_only("viewer", "secret"),
            MockUser::new("nobody", "secret", &[]),
        ],
        ..MockConfig::default()
    })
    .await;
    let mut viewer = environment(&server);
    viewer.auth = AuthConfig::Basic {
        username: "viewer".to_owned(),
    };
    let report = test_connection(viewer, 0, Some(password("secret")))
        .await
        .unwrap()
        .unwrap();
    assert!(
        report
            .missing_permissions
            .contains(&"actions/acknowledge-problem".to_owned())
    );
    assert!(
        report
            .missing_permissions
            .contains(&"actions/execute-command".to_owned())
    );
    assert!(
        report
            .missing_permissions
            .iter()
            .all(|p| p.starts_with("actions/"))
    );

    // Without any permission: everything is missing, and the status is
    // left empty instead of failing the test.
    let mut nobody = environment(&server);
    nobody.auth = AuthConfig::Basic {
        username: "nobody".to_owned(),
    };
    let report = test_connection(nobody, 0, Some(password("secret")))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(report.missing_permissions.len(), REQUIRED_PERMISSIONS.len());
    assert_eq!(report.status, ic_model::InstanceStatus::default());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failures_say_what_to_fix() {
    let server = mock(MockConfig::default()).await;

    let wrong = test_connection(environment(&server), 0, Some(password("wrong")))
        .await
        .unwrap();
    assert_eq!(wrong, Err(ConnectionFailure::Unauthorized));

    let no_password = test_connection(environment(&server), 0, None)
        .await
        .unwrap();
    assert!(matches!(no_password, Err(ConnectionFailure::Other(_))));

    // Untrusted: the certificate comes along for trust on first use.
    let mut untrusted = environment(&server);
    untrusted.urls[0].pinned_sha256 = None;
    let Err(ConnectionFailure::Tls {
        message,
        certificate,
    }) = test_connection(untrusted, 0, Some(password(PASSWORD)))
        .await
        .unwrap()
    else {
        panic!("expected a TLS failure");
    };
    assert!(message.contains("UnknownIssuer"), "{message}");
    assert_eq!(certificate.unwrap().sha256, server.cert_sha256());

    // Pinned to another certificate: both fingerprints.
    let mut pinned = environment(&server);
    let other = ic_config::format_fingerprint(&[1; 32]);
    pinned.urls[0].pinned_sha256 = Some(other.clone());
    assert_eq!(
        test_connection(pinned, 0, Some(password(PASSWORD)))
            .await
            .unwrap(),
        Err(ConnectionFailure::CertificateMismatch {
            expected: other,
            actual: server.cert_fingerprint(),
        })
    );

    // Nobody listening.
    let gone = environment(&server);
    server.shutdown().await;
    assert!(matches!(
        test_connection(gone, 0, Some(password(PASSWORD)))
            .await
            .unwrap(),
        Err(ConnectionFailure::Unreachable(_))
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_certificate_can_be_read_for_trust_on_first_use() {
    let server = mock(MockConfig::default()).await;
    let certificate = fetch_certificate(server.url(), None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(certificate.sha256, server.cert_sha256());
    assert!(!certificate.subject.is_empty());

    let invalid = fetch_certificate("not a url".to_owned(), None)
        .await
        .unwrap();
    assert!(invalid.unwrap_err().contains("invalid URL"));
}

/// `objects/query/Dependency` → `dependencies`: the URL segment Icinga
/// serves a type's objects under.
fn plural(type_name: &str) -> String {
    let lower = type_name.to_lowercase();
    match lower.strip_suffix('y') {
        Some(stem) => format!("{stem}ies"),
        None => format!("{lower}s"),
    }
}

/// An API user with exactly `REQUIRED_PERMISSIONS` connects and loads
/// everything without a refusal, and each object query permission is used
/// by a query: the list asks for nothing the client doesn't need (a
/// production `ApiUser` gets exactly this list).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_required_permissions_are_exactly_what_a_load_uses() {
    let server = mock(MockConfig {
        users: vec![MockUser::new(USER, PASSWORD, REQUIRED_PERMISSIONS)],
        ..MockConfig::with_scenario(scenarios::staging())
    })
    .await;
    let control = server.control();
    let mut engine = Launch::new(&server).start();
    engine.connected().await;
    // Icinga's `Notification` objects come last, in the background.
    engine
        .snapshot(|snapshot| !snapshot.icinga_notifications.is_empty())
        .await;
    let requests = control.requests();
    let refused: Vec<String> = requests
        .iter()
        .filter(|request| request.status >= 400)
        .map(|request| format!("{} {}", request.status, request.path))
        .collect();
    assert!(refused.is_empty(), "refused: {refused:?}");

    let queried: BTreeSet<String> = requests
        .iter()
        .filter_map(|request| request.path.strip_prefix("/v1/objects/"))
        .map(str::to_owned)
        .collect();
    let permitted: BTreeSet<String> = REQUIRED_PERMISSIONS
        .iter()
        .filter_map(|permission| permission.strip_prefix("objects/query/"))
        .map(plural)
        .collect();
    assert_eq!(queried, permitted);
    assert!(
        requests
            .iter()
            .any(|request| request.path.starts_with("/v1/status"))
    );
    let subscribed: BTreeSet<String> = requests
        .iter()
        .filter(|request| request.path == "/v1/events")
        .filter_map(|request| request.body.clone())
        .flat_map(|body| {
            body["types"]
                .as_array()
                .unwrap()
                .iter()
                .map(|kind| kind.as_str().unwrap().to_owned())
                .collect::<Vec<_>>()
        })
        .collect();
    let events: BTreeSet<String> = REQUIRED_PERMISSIONS
        .iter()
        .filter_map(|permission| permission.strip_prefix("events/"))
        .map(str::to_owned)
        .collect();
    assert_eq!(subscribed, events);
}
