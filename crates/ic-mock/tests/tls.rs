//! Certificates: pinning, CA verification, stable fingerprints, rotation
//! and client-certificate authentication.

mod common;

use common::{ca_config, client, client_from, get, json, pinned_config, start};
use ic_mock::{MockConfig, MockServer, MockTls, MockUser, TlsMaterial, format_fingerprint};
use reqwest::StatusCode;

#[tokio::test]
async fn self_signed_certificates_are_pinned_by_fingerprint() {
    let (server, client) = start(MockConfig::default()).await;
    let (status, _) = get(&client, &server, "/v1").await;
    assert_eq!(status, StatusCode::OK);

    let fingerprint = server.cert_fingerprint();
    assert_eq!(fingerprint, format_fingerprint(&server.cert_sha256()));
    assert_eq!(fingerprint.len(), 32 * 3 - 1);
    assert!(fingerprint.split(':').all(|byte| {
        byte.len() == 2
            && byte
                .chars()
                .all(|c| c.is_ascii_digit() || c.is_ascii_uppercase())
    }));
    assert!(server.ca_pem().is_none());
    assert!(server.cert_pem().starts_with("-----BEGIN CERTIFICATE-----"));

    // A client pinning another certificate refuses the server.
    let wrong = client_from(pinned_config([0u8; 32]));
    assert!(
        wrong
            .get(format!("{}/v1", server.url()))
            .send()
            .await
            .is_err()
    );
}

#[tokio::test]
async fn ca_signed_certificates_verify_for_ip_and_localhost() {
    let config = MockConfig {
        tls: MockTls::CaSigned,
        ..MockConfig::default()
    };
    let (server, _) = start(config).await;
    let ca = server.ca_pem().expect("CA mode has a CA");
    let verified = client_from(ca_config(&ca, None));
    for host in ["127.0.0.1", "localhost"] {
        let response = verified
            .get(format!("https://{host}:{}/v1", server.port()))
            .basic_auth("root", Some("icinga"))
            .header("Accept", "application/json")
            .send()
            .await
            .unwrap_or_else(|error| panic!("{host}: {error:?}"));
        assert_eq!(response.status(), StatusCode::OK);
    }
    // Another CA doesn't verify it.
    let other = TlsMaterial::ca_signed("other").unwrap();
    let stranger = client_from(ca_config(other.ca_pem.as_deref().unwrap(), None));
    assert!(
        stranger
            .get(format!("{}/v1", server.url()))
            .send()
            .await
            .is_err()
    );
}

#[tokio::test]
async fn provided_material_keeps_the_fingerprint_across_restarts() {
    let material = TlsMaterial::self_signed("master-01").unwrap();
    let expected = material.sha256().unwrap();
    let config = MockConfig {
        tls: MockTls::Provided(material),
        ..MockConfig::default()
    };
    let first = MockServer::start(config.clone()).await.unwrap();
    assert_eq!(first.cert_sha256(), expected);
    first.shutdown().await;
    let second = MockServer::start(config).await.unwrap();
    assert_eq!(second.cert_sha256(), expected);
    let client = client(&second);
    let (status, _) = get(&client, &second, "/v1").await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn rotation_changes_the_certificate_for_new_connections() {
    let (server, old_client) = start(MockConfig::default()).await;
    let (status, _) = get(&old_client, &server, "/v1").await;
    assert_eq!(status, StatusCode::OK);
    let before = server.cert_sha256();
    let after = server.control().rotate_certificate().unwrap();
    assert_ne!(before, after);
    assert_eq!(server.cert_sha256(), after);
    // Rotation drops existing connections; the pinned client now fails.
    assert!(
        old_client
            .get(format!("{}/v1", server.url()))
            .send()
            .await
            .is_err()
    );
    let new_client = client(&server);
    let (status, _) = get(&new_client, &server, "/v1").await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn client_certificates_authenticate_api_users() {
    let config = MockConfig {
        tls: MockTls::CaSigned,
        users: vec![
            MockUser::new("cert-user", "", &["objects/query/Host"]).with_client_cn("ops-client"),
        ],
        ..MockConfig::default()
    };
    let (server, _) = start(config).await;
    let ca = server.ca_pem().unwrap();

    let (cert, key) = server.client_certificate("ops-client").unwrap();
    let with_cert = client_from(ca_config(&ca, Some((&cert, &key))));
    let response = with_cert
        .get(format!("{}/v1/objects/hosts", server.url()))
        .header("Accept", "application/json")
        .send()
        .await
        .unwrap();
    let (status, body) = json(response).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        server.control().requests().last().unwrap().user.as_deref(),
        Some("cert-user")
    );

    // A certificate for an unknown CN is no login.
    let (cert, key) = server.client_certificate("someone-else").unwrap();
    let stranger = client_from(ca_config(&ca, Some((&cert, &key))));
    let response = stranger
        .get(format!("{}/v1/objects/hosts", server.url()))
        .header("Accept", "application/json")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    // Without any client certificate, basic auth is still required.
    let anonymous = client_from(ca_config(&ca, None));
    let response = anonymous
        .get(format!("{}/v1/objects/hosts", server.url()))
        .header("Accept", "application/json")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn dropped_connections_are_reestablished_by_clients() {
    let (server, client) = start(MockConfig::default()).await;
    let (status, _) = get(&client, &server, "/v1").await;
    assert_eq!(status, StatusCode::OK);
    server.control().drop_connections();
    // The pooled connection is gone; a retry on a new connection works.
    let mut ok = false;
    for _ in 0..3 {
        if let Ok(response) = client
            .get(format!("{}/v1", server.url()))
            .basic_auth("root", Some("icinga"))
            .send()
            .await
        {
            ok = response.status() == StatusCode::OK;
            break;
        }
    }
    assert!(ok);
}
