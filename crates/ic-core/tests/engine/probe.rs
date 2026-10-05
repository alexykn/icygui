//! The settings dialog's helpers: "test connection" and reading the
//! server's certificate, without a running engine.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers fail the test loudly"
)]

use crate::support::{PASSWORD, environment, mock};
use ic_config::AuthConfig;
use ic_core::{ConnectionFailure, REQUIRED_PERMISSIONS, fetch_certificate, test_connection};
use ic_mock::{MockConfig, MockUser, scenarios};
use secrecy::SecretString;

fn password(text: &str) -> SecretString {
    SecretString::from(text.to_owned())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_good_connection_reports_user_version_and_status() {
    let server = mock(MockConfig::with_scenario(scenarios::staging())).await;
    let report = test_connection(environment(&server), Some(password(PASSWORD)))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(report.info.user, "root");
    assert!(!report.info.version.is_empty());
    assert_eq!(report.status.node_name, "stg-master-01");
    assert!(report.missing_permissions.is_empty());
    // Two cheap requests: GET /v1 and the status.
    let paths: Vec<String> = server
        .control()
        .requests()
        .into_iter()
        .map(|request| request.path)
        .collect();
    assert!(
        paths
            .iter()
            .all(|path| path == "/v1" || path.starts_with("/v1/status")),
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
    let report = test_connection(viewer, Some(password("secret")))
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
    let report = test_connection(nobody, Some(password("secret")))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(report.missing_permissions.len(), REQUIRED_PERMISSIONS.len());
    assert_eq!(report.status, ic_model::InstanceStatus::default());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failures_say_what_to_fix() {
    let server = mock(MockConfig::default()).await;

    let wrong = test_connection(environment(&server), Some(password("wrong")))
        .await
        .unwrap();
    assert_eq!(wrong, Err(ConnectionFailure::Unauthorized));

    let no_password = test_connection(environment(&server), None).await.unwrap();
    assert!(matches!(no_password, Err(ConnectionFailure::Other(_))));

    // Untrusted: the certificate comes along for trust on first use.
    let mut untrusted = environment(&server);
    untrusted.tls.pinned_sha256 = None;
    let Err(ConnectionFailure::Tls {
        message,
        certificate,
    }) = test_connection(untrusted, Some(password(PASSWORD)))
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
    pinned.tls.pinned_sha256 = Some(other.clone());
    assert_eq!(
        test_connection(pinned, Some(password(PASSWORD)))
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
        test_connection(gone, Some(password(PASSWORD)))
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
