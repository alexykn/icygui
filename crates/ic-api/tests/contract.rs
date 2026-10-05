//! Contract tests against a real Icinga 2 (see `contract/run-icinga.sh`).
//!
//! They run only when `ICYGUI_CONTRACT_URL` is set (with the other
//! `ICYGUI_CONTRACT_*` variables the script prints) and otherwise pass
//! without doing anything, unless `ICYGUI_CONTRACT_REQUIRED` is set (the
//! nightly contract workflow sets it): then missing variables fail every
//! test. They only read: queries, status, the event stream, and a refused
//! action as the read-only `viewer` user.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers: a failure should panic the test"
)]

mod raw;

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use futures::StreamExt;
use ic_api::{
    ApiError, Client, ConnectionSettings, Credentials, Detail, TlsSettings, Url,
    fetch_server_certificate,
};
use ic_model::{Action, ActionTarget, EventKind, HostState, ObjectKey, Service, ServiceState};
use raw::Raw;
use secrecy::SecretString;
use serde_json::json;

struct Contract {
    url: Url,
    user: String,
    password: String,
    ca_pem: Vec<u8>,
    server_name: String,
}

/// The contract instance, or `None` (the test passes trivially) when
/// `ICYGUI_CONTRACT_URL` isn't set and the contract isn't required.
fn contract() -> Option<Contract> {
    let Ok(url) = std::env::var("ICYGUI_CONTRACT_URL") else {
        assert!(
            std::env::var_os("ICYGUI_CONTRACT_REQUIRED").is_none(),
            "ICYGUI_CONTRACT_REQUIRED is set but ICYGUI_CONTRACT_URL is not: \
             export the variables contract/run-icinga.sh prints"
        );
        return None;
    };
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

    fn raw(&self) -> Raw {
        Raw::new(
            &self.url,
            Some(&self.server_name),
            &self.ca_pem,
            &self.user,
            &self.password,
        )
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
    let services = client.services(Detail::Full).await.unwrap();
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
}

/// Every attribute of [`Detail::Lean`] and [`Detail::Full`] exists: Icinga
/// 2.15 rejects a whole query that names an unknown one (the client would
/// then leave it out and log a warning; here it must not come to that).
#[tokio::test]
async fn real_icinga_knows_every_lean_and_full_attribute() {
    let Some(contract) = contract() else {
        return;
    };
    let raw = contract.raw();
    for detail in [Detail::Lean, Detail::Full] {
        for (plural, attrs) in [
            ("hosts", detail.host_attrs()),
            ("services", detail.service_attrs()),
        ] {
            let answer = raw.query(plural, &json!({ "attrs": attrs })).await;
            assert_eq!(
                answer.status,
                200,
                "{detail:?} {plural}: {}",
                String::from_utf8_lossy(&answer.body)
            );
            let results = answer.json()["results"].as_array().unwrap().clone();
            assert!(!results.is_empty());
            let expected: BTreeSet<&str> = attrs.iter().copied().collect();
            for entry in &results {
                assert!(entry.get("code").is_none(), "per-object error: {entry}");
                let keys: BTreeSet<&str> = entry["attrs"]
                    .as_object()
                    .unwrap()
                    .keys()
                    .map(String::as_str)
                    .collect();
                assert_eq!(keys, expected, "{detail:?} {plural}");
            }
        }
    }

    // What an unknown attribute does: 2.15 and older reject the whole
    // query; newer versions answer every object with the error.
    let answer = raw
        .query(
            "services",
            &json!({ "attrs": ["state", "icygui_no_such_attribute"] }),
        )
        .await;
    let body = answer.json();
    let message = "Invalid field specified: icygui_no_such_attribute";
    if answer.status == 400 {
        assert_eq!(body["status"], message);
    } else {
        assert_eq!(answer.status, 200);
        for entry in body["results"].as_array().unwrap() {
            assert_eq!(entry["code"], 400);
            assert_eq!(entry["status"], message);
        }
    }
    let version = contract.client().info().await.unwrap().version;
    if version.starts_with("v2.15.") {
        assert_eq!(answer.status, 400, "Icinga {version} rejects the query");
    }
}

/// Services that weren't checked between the two queries (their
/// `last_check` is the same in both), by full name.
fn unchanged<'a>(lean: &'a [Service], full: &'a [Service]) -> Vec<(&'a Service, &'a Service)> {
    let full: BTreeMap<String, &Service> = full
        .iter()
        .map(|service| (service.key.full_name(), service))
        .collect();
    lean.iter()
        .filter_map(|lean| {
            let full = full.get(&lean.key.full_name())?;
            (lean.check.last_check == full.check.last_check).then_some((lean, *full))
        })
        .collect()
}

#[tokio::test]
async fn real_icinga_lean_and_full_services() {
    let Some(contract) = contract() else {
        return;
    };
    let client = contract.client();
    let lean = client.services(Detail::Lean).await.unwrap();
    let full = client.services(Detail::Full).await.unwrap();
    let names = |services: &[Service]| -> BTreeSet<String> {
        services
            .iter()
            .map(|service| service.key.full_name())
            .collect()
    };
    assert_eq!(names(&lean), names(&full), "the same services");
    assert!(lean.iter().all(|service| service.check.result.is_none()));

    let pairs = unchanged(&lean, &full);
    assert!(!pairs.is_empty());
    for (lean, full) in pairs {
        let name = lean.key.full_name();
        // Icinga reports never-checked services as state 3 (UNKNOWN); lean
        // services are pending from `last_check` alone, like full ones
        // from their missing check result.
        assert_eq!(lean.state, full.state, "{name}");
        assert_eq!(
            lean.state == ServiceState::Pending,
            full.check.result.is_none()
        );
        assert_eq!(lean.severity(), full.severity(), "{name}");
        assert_eq!(lean.check.state_type, full.check.state_type, "{name}");
        assert_eq!(lean.check.attempt, full.check.attempt, "{name}");
        assert_eq!(lean.check.max_attempts, full.check.max_attempts, "{name}");
        assert_eq!(lean.check.acknowledgement, full.check.acknowledgement);
        assert_eq!(lean.check.downtime_depth, full.check.downtime_depth);
        assert_eq!(lean.check.reachable, full.check.reachable, "{name}");
        assert_eq!(
            lean.check.features.active_checks,
            full.check.features.active_checks
        );
        assert!((lean.check.check_interval - full.check.check_interval).abs() < f64::EPSILON);
        assert!((lean.check.retry_interval - full.check.retry_interval).abs() < f64::EPSILON);
        assert_eq!(lean.groups, full.groups, "{name}");
        assert_eq!(lean.vars, full.vars, "{name}");
        assert_eq!(lean.display_name, full.display_name, "{name}");
        assert!(!full.check.check_command.is_empty(), "{name}");
        assert_eq!(lean.check.check_command, "", "not loaded when lean");
    }

    // Lean is the smaller query (on 30 000 services: 20 MB instead of
    // 46 MB, docs/performance.md).
    let raw = contract.raw();
    let lean_bytes = raw
        .query(
            "services",
            &json!({ "attrs": Detail::Lean.service_attrs() }),
        )
        .await
        .body
        .len();
    let full_bytes = raw
        .query(
            "services",
            &json!({ "attrs": Detail::Full.service_attrs() }),
        )
        .await
        .body
        .len();
    assert!(
        lean_bytes * 2 < full_bytes,
        "lean {lean_bytes} bytes, full {full_bytes} bytes"
    );
}

#[tokio::test]
async fn real_icinga_objects_by_name_with_missing_names() {
    let Some(contract) = contract() else {
        return;
    };
    let client = contract.client();
    let hosts = client.hosts().await.unwrap();
    let services = client.services(Detail::Lean).await.unwrap();

    let mut keys: Vec<ObjectKey> = hosts.iter().map(ic_model::Host::key).collect();
    keys.insert(1, ObjectKey::host("icygui-no-such-host"));
    keys.extend(services.iter().map(Service::object_key));
    keys.push(ObjectKey::service("icygui-no-such-host", "nothing"));
    keys.push(ObjectKey::service(
        hosts[0].name.as_str(),
        "icygui-no-such-service",
    ));
    keys.push(ObjectKey::host("icygui-no-such-host"));
    let expected_missing = [
        ObjectKey::host("icygui-no-such-host"),
        ObjectKey::service("icygui-no-such-host", "nothing"),
        ObjectKey::service(hosts[0].name.as_str(), "icygui-no-such-service"),
    ];

    for detail in [Detail::Lean, Detail::Full] {
        let fetched = client.objects(&keys, detail).await.unwrap();
        assert_eq!(fetched.missing, expected_missing, "{detail:?}");
        assert_eq!(fetched.hosts.len(), hosts.len(), "{detail:?}");
        assert_eq!(fetched.services.len(), services.len(), "{detail:?}");
        let lean = detail == Detail::Lean;
        for service in &fetched.services {
            let pending = service.state == ServiceState::Pending;
            assert_eq!(service.check.result.is_none(), lean || pending);
        }
        for host in &fetched.hosts {
            let pending = host.state == HostState::Pending;
            assert_eq!(host.check.result.is_none(), lean || pending);
        }
    }

    // Only unknown names: nothing found, everything missing.
    let gone = [
        ObjectKey::host("icygui-gone-1"),
        ObjectKey::host("icygui-gone-2"),
    ];
    let fetched = client.objects(&gone, Detail::Full).await.unwrap();
    assert!(fetched.hosts.is_empty() && fetched.services.is_empty());
    assert_eq!(fetched.missing, gone);
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
    // Active checks run every 20–30 s in the fixtures.
    let event = tokio::time::timeout(Duration::from_secs(90), stream.next())
        .await
        .expect("an event within 90 s")
        .expect("the stream is open")
        .unwrap();
    assert!(event.at().as_unix_seconds() > 0.0);
}
