//! Contract tests against a real Icinga 2 (see `contract/run-icinga.sh`).
//!
//! They run only when `ICYGUI_CONTRACT_URL` is set (with the other
//! `ICYGUI_CONTRACT_*` variables the script prints) and otherwise pass
//! without doing anything. They only read: queries, status, the event
//! stream, and a refused action as the read-only `viewer` user.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers: a failure should panic the test"
)]

use std::time::Duration;

use futures::StreamExt;
use ic_api::{
    ApiError, Client, ConnectionSettings, Credentials, TlsSettings, Url, fetch_server_certificate,
};
use ic_model::{Action, ActionTarget, EventKind, ObjectKey, ServiceState};
use secrecy::SecretString;

struct Contract {
    url: Url,
    user: String,
    password: String,
    ca_pem: Vec<u8>,
    server_name: String,
}

fn contract() -> Option<Contract> {
    let url = std::env::var("ICYGUI_CONTRACT_URL").ok()?;
    let var = |name: &str| std::env::var(name).expect(name);
    Some(Contract {
        url: Url::parse(&url).expect("ICYGUI_CONTRACT_URL"),
        user: var("ICYGUI_CONTRACT_USER"),
        password: var("ICYGUI_CONTRACT_PASSWORD"),
        ca_pem: std::fs::read(var("ICYGUI_CONTRACT_CA_FILE")).expect("CA file"),
        server_name: var("ICYGUI_CONTRACT_SERVER_NAME"),
    })
}

impl Contract {
    fn client_as(&self, user: &str, password: &str) -> Client {
        Client::new(ConnectionSettings::new(
            self.url.clone(),
            Credentials::Basic {
                username: user.to_owned(),
                password: SecretString::from(password.to_owned()),
            },
            TlsSettings {
                ca_pem: Some(self.ca_pem.clone()),
                server_name: Some(self.server_name.clone()),
                ..TlsSettings::default()
            },
        ))
        .unwrap()
    }

    fn client(&self) -> Client {
        self.client_as(&self.user, &self.password)
    }
}

#[tokio::test]
async fn real_icinga_ca_and_pin() {
    let Some(contract) = contract() else {
        return;
    };
    let info = contract.client().info().await.unwrap();
    assert_eq!(info.user, contract.user);
    assert!(info.version.starts_with('v'));
    assert!(info.allows("objects/query/Host"));

    let certificate = fetch_server_certificate(&contract.url, None).await.unwrap();
    assert!(certificate.subject.contains(&contract.server_name));
    let pinned = Client::new(ConnectionSettings::new(
        contract.url.clone(),
        Credentials::Basic {
            username: contract.user.clone(),
            password: SecretString::from(contract.password.clone()),
        },
        TlsSettings {
            pinned_sha256: Some(certificate.sha256),
            ..TlsSettings::default()
        },
    ))
    .unwrap();
    pinned.info().await.unwrap();

    let wrong = contract.client_as(&contract.user, "wrong password");
    assert_eq!(wrong.info().await.unwrap_err(), ApiError::Unauthorized);
}

#[tokio::test]
async fn real_icinga_queries() {
    let Some(contract) = contract() else {
        return;
    };
    let client = contract.client();
    let status = client.status().await.unwrap();
    assert!(!status.node_name.is_empty());
    let hosts = client.hosts().await.unwrap();
    let services = client.services().await.unwrap();
    assert!(!hosts.is_empty());
    assert!(!services.is_empty());
    assert!(
        services
            .iter()
            .any(|service| service.state != ServiceState::Pending)
    );
    client.comments().await.unwrap();
    client.downtimes().await.unwrap();
    client.host_groups().await.unwrap();
    client.service_groups().await.unwrap();
    client.dependencies().await.unwrap();
    let endpoints = client.endpoints().await.unwrap();
    assert!(endpoints.iter().any(|endpoint| !endpoint.zone.is_empty()));

    let mut keys: Vec<ObjectKey> = hosts.iter().map(ic_model::Host::key).collect();
    keys.push(ObjectKey::host("icygui-no-such-host"));
    keys.extend(services.iter().map(ic_model::Service::object_key));
    keys.push(ObjectKey::service("icygui-no-such-host", "nothing"));
    let (again, services_again) = client.objects(&keys).await.unwrap();
    assert_eq!(again.len(), hosts.len());
    assert_eq!(services_again.len(), services.len());
}

#[tokio::test]
async fn real_icinga_refuses_actions_without_permission() {
    let Some(contract) = contract() else {
        return;
    };
    let viewer = contract.client_as("viewer", "viewer-test");
    let info = viewer.info().await.unwrap();
    assert!(!info.allows("actions/acknowledge-problem"));
    let hosts = viewer.hosts().await.unwrap();
    let action = Action::Acknowledge {
        comment: "contract test".to_owned(),
        sticky: false,
        persistent: false,
        expiry: None,
    };
    let error = viewer
        .run_action(
            &action,
            &ActionTarget::Objects(vec![hosts[0].key()]),
            "viewer",
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&error, ApiError::Forbidden(message) if message.contains("actions/acknowledge-problem")),
        "{error:?}"
    );
}

#[tokio::test]
async fn real_icinga_event_stream() {
    let Some(contract) = contract() else {
        return;
    };
    let mut stream = contract
        .client()
        .events("icygui-contract-ic-api", &EventKind::ALL)
        .await
        .unwrap();
    // Active checks run every 30 s in the fixtures.
    let event = tokio::time::timeout(Duration::from_secs(90), stream.next())
        .await
        .expect("an event within 90 s")
        .expect("the stream is open")
        .unwrap();
    assert!(event.at().as_unix_seconds() > 0.0);
}
