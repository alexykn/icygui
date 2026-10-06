//! Contract tests against a real Icinga 2 (see `contract/run-icinga.sh`).
//!
//! They run only when `ICYGUI_CONTRACT_URL` is set (with the other
//! `ICYGUI_CONTRACT_*` variables the script prints) and otherwise pass
//! without doing anything, unless `ICYGUI_CONTRACT_REQUIRED` is set (the
//! nightly contract workflow sets it): then missing variables fail every
//! test. They only read: queries, status, the event stream, and a refused
//! action as the read-only `viewer` user.
//!
//! They load every object several times, which is fine for the small
//! disposable instance but is load a production Icinga must not get from a
//! test run. So [`fixture`] refuses any other instance before sending a
//! single query: the URL must point to this machine and the fixture-only
//! `viewer` user must log in. Load and scale tests belong against `ic-mock`
//! or `contract/scale/benchmark.sh`'s own local Icinga.
//!
//! A freshly started Icinga runs its first checks within a minute (see
//! [`wait_for_first_checks`]); tests that need checked objects wait for
//! them.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers: a failure should panic the test"
)]

mod raw;

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use futures::StreamExt;
use ic_api::{
    ApiError, Client, ConnectionSettings, Credentials, Detail, TlsSettings, Url,
    fetch_server_certificate,
};
use ic_model::{
    Action, ActionTarget, Event, EventKind, HostState, Links, ObjectKey, Service, ServiceState,
};
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

/// [`contract`], after making sure it is the disposable Icinga from
/// `contract/run-icinga.sh` and never a real one (see the module docs).
/// Panics otherwise, before any query is sent.
async fn fixture() -> Option<Contract> {
    const REFUSED: &str = "ICYGUI_CONTRACT_URL is not the disposable Icinga from \
                           contract/run-icinga.sh; contract tests never run against \
                           other instances";
    let contract = contract()?;
    let host = contract.url.host_str().unwrap_or_default();
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    assert!(loopback, "{REFUSED}: {host} is not this machine");
    let viewer = contract.client_as("viewer", "viewer-test").info().await;
    assert!(
        matches!(&viewer, Ok(info) if info.user == "viewer"),
        "{REFUSED}: its fixture-only `viewer` user can't log in ({viewer:?})"
    );
    Some(contract)
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

/// How long a fresh Icinga may take to check every object once: 2.15
/// schedules each first check within `min(check_interval, 60 s)` of its
/// start (`Checkable::Start`).
const FIRST_CHECKS: Duration = Duration::from_secs(150);

/// Waits until Icinga has checked every object with active checks (the
/// fixtures' passive services are never checked). On a long-running
/// instance that is at once; `contract/run-icinga.sh` returns as soon as
/// the API answers, before any check ran.
async fn wait_for_first_checks(raw: &Raw) {
    let deadline = Instant::now() + FIRST_CHECKS;
    let attrs = json!({ "attrs": ["last_check", "enable_active_checks"] });
    loop {
        let mut unchecked = Vec::new();
        for plural in ["hosts", "services"] {
            let answer = raw.query(plural, &attrs).await;
            let body = answer.json();
            assert_eq!(answer.status, 200, "{plural}: {body}");
            for entry in body["results"].as_array().unwrap() {
                let attrs = &entry["attrs"];
                let active = attrs["enable_active_checks"].as_bool().unwrap_or(true);
                let checked = attrs["last_check"].as_f64().is_some_and(|at| at > 0.0);
                if active && !checked {
                    unchecked.push(entry["name"].to_string());
                }
            }
        }
        if unchecked.is_empty() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "never checked within {FIRST_CHECKS:?}: {}",
            unchecked.join(", ")
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

#[tokio::test]
async fn real_icinga_ca_and_pin() {
    let Some(contract) = fixture().await else {
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
    let Some(contract) = fixture().await else {
        return;
    };
    wait_for_first_checks(&contract.raw()).await;
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
    // The node, its endpoint and its zone (ENV-12): the fixture is a
    // single master in the top-level zone `master`.
    let node = client.node_name().await.unwrap();
    assert_eq!(node.as_deref(), Some(status.node_name.as_str()));
    let zones = client.zones().await.unwrap();
    let zone = zones
        .iter()
        .find(|zone| zone.endpoints.contains(&status.node_name))
        .expect("the node is a member of a zone");
    assert_eq!(zone.parent, None);
    assert!(!zone.global);
    assert!(
        zones
            .iter()
            .any(|zone| zone.global && zone.endpoints.is_empty())
    );
}

/// Every attribute the client asks for exists: Icinga 2.15 rejects a
/// whole query that names an unknown one, newer versions every object
/// (the client would then leave it out and log a warning; here it must
/// not come to that). Both [`Detail`] lists come back verbatim.
#[tokio::test]
async fn real_icinga_knows_every_requested_attribute() {
    let Some(contract) = fixture().await else {
        return;
    };
    // Every query the client makes. Icinga only notices an unknown
    // attribute while it serialises an object, so types without objects
    // here (comments, downtimes) are checked against the recorded samples
    // instead (`every_requested_attribute_exists_in_icinga`).
    let client = contract.client();
    let hosts = client.hosts().await.unwrap();
    for detail in [Detail::Lean, Detail::Full] {
        let services = client.services(detail).await.unwrap();
        let keys = [hosts[0].key(), services[0].object_key()];
        client.objects(&keys, detail).await.unwrap();
    }
    client.comments().await.unwrap();
    client.downtimes().await.unwrap();
    assert!(!client.host_groups().await.unwrap().is_empty());
    assert!(!client.service_groups().await.unwrap().is_empty());
    assert!(!client.dependencies().await.unwrap().is_empty());
    assert!(!client.endpoints().await.unwrap().is_empty());
    assert!(!client.zones().await.unwrap().is_empty());
    assert_eq!(client.unknown_attributes(), []);

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
    let Some(contract) = fixture().await else {
        return;
    };
    let raw = contract.raw();
    wait_for_first_checks(&raw).await;
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
    assert!(
        pairs
            .iter()
            .any(|(lean, _)| lean.state != ServiceState::Pending),
        "a checked service"
    );
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
        assert_eq!(lean.check.features, full.check.features, "{name}");
        assert!((lean.check.check_interval - full.check.check_interval).abs() < f64::EPSILON);
        assert!((lean.check.retry_interval - full.check.retry_interval).abs() < f64::EPSILON);
        assert!(
            (lean.check.flapping_current - full.check.flapping_current).abs() < f64::EPSILON,
            "{name}"
        );
        assert_eq!(lean.groups, full.groups, "{name}");
        assert_eq!(lean.vars, full.vars, "{name}");
        assert_eq!(lean.display_name, full.display_name, "{name}");
        // The check configuration is lean too; the links are Full only.
        assert!(!full.check.check_command.is_empty(), "{name}");
        assert_eq!(lean.check.check_command, full.check.check_command);
        assert_eq!(lean.check.command_endpoint, full.check.command_endpoint);
        assert_eq!(lean.check.zone, full.check.zone, "{name}");
        assert_eq!(lean.links, Links::default(), "{name}");
    }

    // Lean leaves out at least the check results (64 % of the bytes of
    // 30 000 services, docs/performance.md). Compared with the results'
    // bytes rather than as a ratio: here one service's result (`icinga`'s
    // about 10 KB) is most of them.
    let lean = raw
        .query(
            "services",
            &json!({ "attrs": Detail::Lean.service_attrs() }),
        )
        .await;
    let full = raw
        .query(
            "services",
            &json!({ "attrs": Detail::Full.service_attrs() }),
        )
        .await;
    let (lean, results, full) = (
        lean.body.len(),
        full.attr_bytes("last_check_result"),
        full.body.len(),
    );
    assert!(
        lean + results < full,
        "lean {lean} bytes, full {full} bytes, check results {results} bytes"
    );
}

#[tokio::test]
async fn real_icinga_objects_by_name_with_missing_names() {
    let Some(contract) = fixture().await else {
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

/// Icinga's own `Notification` objects (the default `conf.d` notifies
/// `icingaadmins` about every host and service): the whole list, by name
/// with unknown names, and the read-only `viewer`'s missing permission.
#[tokio::test]
async fn real_icinga_notifications() {
    let Some(contract) = fixture().await else {
        return;
    };
    let client = contract.client();
    let notifications = client.notifications().await.unwrap();
    assert!(!notifications.is_empty());
    let hosts: BTreeSet<String> = client
        .hosts()
        .await
        .unwrap()
        .iter()
        .map(|host| host.name.as_str().to_owned())
        .collect();
    for notification in &notifications {
        assert!(
            notification
                .name
                .starts_with(&format!("{}!", notification.object.full_name())),
            "{notification:?}"
        );
        assert!(hosts.contains(notification.object.host_name().as_str()));
    }
    assert_eq!(client.unknown_attributes(), []);

    let mut names: Vec<String> = notifications.iter().map(|n| n.name.clone()).collect();
    names.insert(1, "icygui-no-such-host!mail".to_owned());
    let fetched = client.notifications_named(&names).await.unwrap();
    assert_eq!(fetched.missing, ["icygui-no-such-host!mail"]);
    assert_eq!(fetched.notifications.len(), notifications.len());

    let viewer = contract.client_as("viewer", "viewer-test");
    assert!(
        matches!(viewer.notifications().await, Err(ApiError::Forbidden(_))),
        "the viewer lacks objects/query/Notification"
    );
}

#[tokio::test]
async fn real_icinga_refuses_actions_without_permission() {
    let Some(contract) = fixture().await else {
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
    let Some(contract) = fixture().await else {
        return;
    };
    let mut stream = contract
        .client()
        .events("icygui-contract-ic-api", &EventKind::ALL)
        .await
        .unwrap();
    // Active checks run every 20–30 s in the fixtures. Every check result
    // carries the object's state after processing (`vars_after`).
    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    loop {
        let event = tokio::time::timeout_at(deadline, stream.next())
            .await
            .expect("a check result within 90 s")
            .expect("the stream is open")
            .unwrap();
        assert!(event.at().as_unix_seconds() > 0.0);
        if let Event::CheckResult { after, .. } = &event {
            let after = after.unwrap_or_else(|| panic!("no vars_after: {event:?}"));
            assert!(after.attempt >= 1, "{event:?}");
            break;
        }
    }
}
