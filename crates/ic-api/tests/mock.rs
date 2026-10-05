//! The client against `ic-mock`, the in-process Icinga look-alike: pinning
//! to its self-signed certificate, the tiered loads (lean and full) of the
//! `prod_cluster` and `large` scenarios, objects by name with deleted names,
//! actions with per-object results, the event stream (with a burst), and
//! error mapping.
//!
//! The full-size `large` load is `#[ignore]`d (slow in debug builds):
//! `cargo test -p ic-api --test mock -- --ignored --nocapture` prints its
//! timings and payload sizes.

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
    ApiError, Client, ConnectionSettings, Credentials, Detail, EventStream, TlsSettings, Url,
    fetch_server_certificate, format_fingerprint,
};
use ic_mock::{MockConfig, MockServer, MockTls, MockUser, scenarios};
use ic_model::{
    AckKind, Action, ActionTarget, DowntimeMode, Event, EventKind, HostState, Links, ObjectKey,
    Service, ServiceState, StateType, Timestamp,
};
use raw::Raw;
use secrecy::SecretString;
use serde_json::json;

const ROOT: (&str, &str) = ("root", "icinga");

async fn start(config: MockConfig) -> MockServer {
    MockServer::start(config).await.unwrap()
}

fn url(server: &MockServer) -> Url {
    Url::parse(&server.url()).unwrap()
}

fn pinned(server: &MockServer) -> TlsSettings {
    TlsSettings {
        pinned_sha256: Some(server.cert_sha256()),
        ..TlsSettings::default()
    }
}

fn client_as(server: &MockServer, (user, password): (&str, &str), tls: TlsSettings) -> Client {
    Client::new(ConnectionSettings::new(
        url(server),
        Credentials::Basic {
            username: user.to_owned(),
            password: SecretString::from(password.to_owned()),
        },
        tls,
    ))
    .unwrap()
}

/// `root` with every permission, pinned to the mock's certificate.
fn root(server: &MockServer) -> Client {
    client_as(server, ROOT, pinned(server))
}

fn names(services: &[Service]) -> BTreeSet<String> {
    services.iter().map(|s| s.key.full_name()).collect()
}

// --- TLS ---------------------------------------------------------------------

#[tokio::test]
async fn pinning_to_the_mock_certificate() {
    let server = start(MockConfig::default()).await;

    // Trust on first use: read the certificate, then pin it.
    let certificate = fetch_server_certificate(&url(&server), None).await.unwrap();
    assert_eq!(certificate.sha256, server.cert_sha256());
    assert_eq!(certificate.fingerprint(), server.cert_fingerprint());
    let info = root(&server).info().await.unwrap();
    assert_eq!(info.user, "root");
    assert!(info.allows("actions/acknowledge-problem"));

    // Another pin: both fingerprints are reported.
    let wrong = [7_u8; 32];
    let error = client_as(
        &server,
        ROOT,
        TlsSettings {
            pinned_sha256: Some(wrong),
            ..TlsSettings::default()
        },
    )
    .info()
    .await
    .unwrap_err();
    assert_eq!(
        error,
        ApiError::CertificateMismatch {
            expected: format_fingerprint(&wrong),
            actual: server.cert_fingerprint(),
        }
    );

    // No pin and nothing trusted: an untrusted issuer, so the caller can
    // offer trust on first use.
    let error = client_as(&server, ROOT, TlsSettings::default())
        .info()
        .await
        .unwrap_err();
    assert!(
        matches!(&error, ApiError::Tls(message) if message.contains("UnknownIssuer")),
        "{error:?}"
    );

    // A rotated certificate breaks the pin.
    let rotated = server.rotate_certificate().unwrap();
    let error = root_with_pin(&server, certificate.sha256)
        .info()
        .await
        .unwrap_err();
    assert_eq!(
        error,
        ApiError::CertificateMismatch {
            expected: format_fingerprint(&certificate.sha256),
            actual: format_fingerprint(&rotated),
        }
    );
    root(&server).info().await.unwrap();
}

fn root_with_pin(server: &MockServer, sha256: [u8; 32]) -> Client {
    client_as(
        server,
        ROOT,
        TlsSettings {
            pinned_sha256: Some(sha256),
            ..TlsSettings::default()
        },
    )
}

#[tokio::test]
async fn trusting_the_mock_ca() {
    let server = start(MockConfig {
        tls: MockTls::CaSigned,
        ..MockConfig::default()
    })
    .await;
    let tls = TlsSettings {
        ca_pem: Some(server.ca_pem().unwrap().into_bytes()),
        ..TlsSettings::default()
    };
    // The URL is https://127.0.0.1:<port>, an IP subjectAltName.
    client_as(&server, ROOT, tls.clone()).info().await.unwrap();
    let node = server.control().status().node_name;
    let named = TlsSettings {
        server_name: Some(node),
        ..tls
    };
    client_as(&server, ROOT, named).info().await.unwrap();
}

// --- Tiered loading ----------------------------------------------------------

#[tokio::test]
async fn prod_cluster_loads_lean_and_full() {
    let server = start(MockConfig::with_scenario(scenarios::prod_cluster())).await;
    let control = server.control();
    let client = root(&server);

    let hosts = client.hosts().await.unwrap();
    let expected_hosts = control.hosts();
    assert_eq!(hosts.len(), expected_hosts.len());
    let host_states: BTreeMap<&str, HostState> = expected_hosts
        .iter()
        .map(|host| (host.name.as_str(), host.state))
        .collect();
    for host in &hosts {
        assert_eq!(Some(&host.state), host_states.get(host.name.as_str()));
        assert_eq!(
            host.check.result.is_some(),
            host.state != HostState::Pending
        );
    }
    assert!(hosts.iter().any(|host| host.state == HostState::Down));
    assert!(
        hosts
            .iter()
            .any(|host| host.state == HostState::Unreachable)
    );

    let lean = client.services(Detail::Lean).await.unwrap();
    let full = client.services(Detail::Full).await.unwrap();
    let expected = control.services();
    assert_eq!(names(&lean), names(&expected));
    assert_eq!(names(&full), names(&expected));
    let expected: BTreeMap<String, &Service> = expected
        .iter()
        .map(|service| (service.key.full_name(), service))
        .collect();
    let full: BTreeMap<String, &Service> = full
        .iter()
        .map(|service| (service.key.full_name(), service))
        .collect();
    let mut problems = 0;
    for service in &lean {
        let name = service.key.full_name();
        let mock = expected[&name];
        let full = full[&name];
        assert_eq!(service.state, mock.state, "{name}");
        assert_eq!(full.state, mock.state, "{name}");
        assert_eq!(service.check.result, None, "{name}: lean");
        assert_eq!(service.severity(), full.severity(), "{name}");
        assert_eq!(service.check.acknowledgement, full.check.acknowledgement);
        assert_eq!(service.check.downtime_depth, full.check.downtime_depth);
        assert_eq!(service.check.state_type, full.check.state_type, "{name}");
        assert_eq!(service.groups, full.groups, "{name}");
        assert_eq!(service.vars, full.vars, "{name}");
        // The check configuration and the switches come with lean
        // objects (no event would ever bring them); the links don't.
        assert_eq!(service.check.check_command, full.check.check_command);
        assert_eq!(service.check.command_endpoint, full.check.command_endpoint);
        assert_eq!(service.check.zone, full.check.zone, "{name}");
        assert_eq!(service.check.features, full.check.features, "{name}");
        assert!(
            (service.check.flapping_current - full.check.flapping_current).abs() < f64::EPSILON
        );
        assert_eq!(service.links, Links::default(), "{name}: lean");
        assert_eq!(full.links, mock.links, "{name}");
        if mock.state == ServiceState::Pending {
            assert_eq!(full.check.result, None);
        } else {
            assert_eq!(full.check.output(), mock.check.output(), "{name}");
            assert!(full.check.last_check.is_some());
        }
        problems += usize::from(service.is_problem());
    }
    assert!(problems > 10, "{problems} problems");
    assert!(
        lean.iter()
            .all(|service| !service.check.check_command.is_empty())
    );
    assert!(lean.iter().any(|service| service.check.zone.is_some()));
}

#[tokio::test]
async fn lean_objects_are_pending_only_before_their_first_check() {
    // `lab` has a pending host with a pending service. Like Icinga, the
    // mock reports them with state 1 (host) and 3 (service).
    let server = start(MockConfig::default()).await;
    let control = server.control();
    let client = root(&server);
    let pending_host = control
        .hosts()
        .into_iter()
        .find(|host| host.state == HostState::Pending)
        .unwrap()
        .key();
    let fetched = client.objects(&[pending_host], Detail::Lean).await.unwrap();
    assert_eq!(fetched.hosts[0].state, HostState::Pending);
    assert!(!fetched.hosts[0].is_problem());

    let lean = client.services(Detail::Lean).await.unwrap();
    let pending: Vec<&Service> = lean
        .iter()
        .filter(|service| service.state == ServiceState::Pending)
        .collect();
    let expected_pending = control
        .services()
        .iter()
        .filter(|service| service.state == ServiceState::Pending)
        .count();
    assert_eq!(pending.len(), expected_pending);
    assert!(!pending.is_empty(), "lab has pending services");

    // After a check, the lean object has a state but no output.
    let first = pending[0].key.clone();
    control
        .set_service_state(
            first.host.as_str(),
            &first.name,
            ServiceState::Critical,
            "CRITICAL - first check",
            false,
        )
        .unwrap();
    let fetched = client
        .objects(&[ObjectKey::Service { key: first.clone() }], Detail::Lean)
        .await
        .unwrap();
    let checked = &fetched.services[0];
    assert_eq!(checked.state, ServiceState::Critical);
    assert_eq!(checked.check.state_type, StateType::Soft);
    assert_eq!(checked.check.result, None);
    // Hydrated, it has its output.
    let fetched = client
        .objects(&[ObjectKey::Service { key: first }], Detail::Full)
        .await
        .unwrap();
    assert_eq!(fetched.services[0].check.output(), "CRITICAL - first check");
}

#[tokio::test]
async fn objects_by_name_report_deleted_names_as_missing() {
    let server = start(MockConfig::with_scenario(scenarios::prod_cluster())).await;
    let control = server.control();
    let client = root(&server);
    let services = control.services();
    let hosts = control.hosts();

    // More names than one request carries, with unknown names in several
    // batches and duplicates.
    let mut keys: Vec<ObjectKey> = services.iter().take(450).map(Service::object_key).collect();
    keys.insert(3, ObjectKey::service("gone-host", "disk"));
    keys.insert(250, ObjectKey::service(hosts[0].name.as_str(), "gone"));
    keys.push(ObjectKey::host("gone-host"));
    keys.extend(hosts.iter().take(5).map(ic_model::Host::key));
    keys.push(ObjectKey::service("gone-host", "disk"));
    let expected_missing = [
        ObjectKey::service("gone-host", "disk"),
        ObjectKey::service(hosts[0].name.as_str(), "gone"),
        ObjectKey::host("gone-host"),
    ];

    for detail in [Detail::Lean, Detail::Full] {
        control.clear_requests();
        let fetched = client.objects(&keys, detail).await.unwrap();
        assert_eq!(fetched.missing, expected_missing, "{detail:?}");
        assert_eq!(fetched.services.len(), 450);
        assert_eq!(fetched.hosts.len(), 5);
        let lean = detail == Detail::Lean;
        assert!(
            fetched
                .services
                .iter()
                .all(|s| s.check.result.is_none() == (lean || s.state == ServiceState::Pending))
        );
        let requests = control.requests();
        assert!(requests.iter().all(|r| {
            r.body
                .as_ref()
                .is_some_and(|body| body.get("filter").is_none())
        }));
        let largest = requests
            .iter()
            .filter_map(|r| r.body.as_ref()?["services"].as_array().map(Vec::len))
            .max()
            .unwrap();
        assert_eq!(largest, ic_api::NAMES_PER_REQUEST);
        assert!(requests.len() < 60, "{} requests", requests.len());
    }
}

/// Icinga's own `Notification` objects: the whole list matches the mock's,
/// by name in batches with unknown names isolated, and a missing
/// permission is `Forbidden`.
#[tokio::test]
async fn notifications_load_whole_and_by_name() {
    let server = start(MockConfig::with_scenario(scenarios::prod_cluster())).await;
    let control = server.control();
    let client = root(&server);
    let sorted = |mut list: Vec<ic_model::Notification>| {
        list.sort_by(|a, b| a.name.cmp(&b.name));
        list
    };
    let all = sorted(client.notifications().await.unwrap());
    let expected = sorted(control.notifications());
    assert_eq!(all.len(), expected.len());
    for (got, want) in all.iter().zip(&expected) {
        // The wire's doubles may differ in the last digits.
        let near = match (got.last_notification, want.last_notification) {
            (Some(a), Some(b)) => (a.as_unix_seconds() - b.as_unix_seconds()).abs() < 1e-3,
            (a, b) => a == b,
        };
        assert!(near, "{got:?} {want:?}");
        assert_eq!(
            (&got.name, &got.object, &got.notified_problem_users),
            (&want.name, &want.object, &want.notified_problem_users)
        );
    }
    assert!(all.len() > ic_api::NAMES_PER_REQUEST);
    assert!(
        all.iter().any(
            |notification| !notification.notified_problem_users.is_empty()
                && notification.last_notification.is_some()
        ),
        "the scenario's notified problems"
    );

    let mut names: Vec<String> = all.iter().map(|n| n.name.clone()).collect();
    names.insert(5, "gone-host!gone!mail".to_owned());
    names.push("gone-host!mail".to_owned());
    names.push(names[0].clone());
    control.clear_requests();
    let fetched = client.notifications_named(&names).await.unwrap();
    assert_eq!(fetched.missing, ["gone-host!gone!mail", "gone-host!mail"]);
    let found: Vec<String> = sorted(fetched.notifications)
        .into_iter()
        .map(|n| n.name)
        .collect();
    let names: Vec<String> = expected.iter().map(|n| n.name.clone()).collect();
    assert_eq!(found, names, "each once");
    let largest = control
        .requests()
        .iter()
        .filter_map(|r| r.body.as_ref()?["notifications"].as_array().map(Vec::len))
        .max()
        .unwrap();
    assert_eq!(largest, ic_api::NAMES_PER_REQUEST);
    assert!(
        client
            .notifications_named(&[])
            .await
            .unwrap()
            .notifications
            .is_empty()
    );

    let server = start(MockConfig {
        users: vec![MockUser::new("viewer", "secret", &["objects/query/Host"])],
        ..MockConfig::with_scenario(scenarios::lab())
    })
    .await;
    let viewer = client_as(&server, ("viewer", "secret"), pinned(&server));
    assert!(matches!(
        viewer.notifications().await,
        Err(ApiError::Forbidden(message)) if message.contains("objects/query/notification")
    ));
}

// --- Actions -----------------------------------------------------------------

/// An unacknowledged critical service and an OK one.
fn problem_and_ok(server: &MockServer) -> (ObjectKey, ObjectKey) {
    let services = server.control().services();
    let problem = services
        .iter()
        .find(|s| s.state == ServiceState::Critical && s.check.acknowledgement == AckKind::None)
        .unwrap()
        .object_key();
    let ok = services
        .iter()
        .find(|s| s.state == ServiceState::Ok)
        .unwrap()
        .object_key();
    (problem, ok)
}

#[tokio::test]
async fn actions_return_per_object_results() {
    let server = start(MockConfig::with_scenario(scenarios::prod_cluster())).await;
    let control = server.control();
    let client = root(&server);
    let (problem, ok) = problem_and_ok(&server);
    let gone = ObjectKey::service("gone-host", "disk");

    let acknowledge = Action::Acknowledge {
        comment: "looking into it".to_owned(),
        sticky: false,
        persistent: false,
        expiry: None,
    };
    let results = client
        .run_action(
            &acknowledge,
            &ActionTarget::Objects(vec![problem.clone(), ok.clone(), gone.clone()]),
            "alice",
        )
        .await
        .unwrap();
    let by_target: BTreeMap<String, u16> = results
        .iter()
        .map(|result| (result.target.clone().unwrap(), result.code))
        .collect();
    assert_eq!(by_target[&problem.full_name()], 200, "{results:?}");
    assert_eq!(by_target[&ok.full_name()], 409, "nothing to acknowledge");
    assert_eq!(by_target[&gone.full_name()], 404, "deleted");
    let ObjectKey::Service { key } = &problem else {
        unreachable!()
    };
    let acknowledged = control.service(key.host.as_str(), &key.name).unwrap();
    assert_eq!(acknowledged.check.acknowledgement, AckKind::Normal);

    // Check now: accepted per object.
    let results = client
        .run_action(
            &Action::CheckNow { force: true },
            &ActionTarget::Objects(vec![problem, ok]),
            "alice",
        )
        .await
        .unwrap();
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(ic_api::ActionResult::is_success));
}

#[tokio::test]
async fn comments_and_downtimes_are_removed_by_the_names_actions_return() {
    let server = start(MockConfig::with_scenario(scenarios::prod_cluster())).await;
    let control = server.control();
    let client = root(&server);
    let (_, ok) = problem_and_ok(&server);
    let results = client
        .run_action(
            &Action::AddComment {
                text: "note".to_owned(),
                expiry: None,
            },
            &ActionTarget::Objects(vec![ok.clone()]),
            "alice",
        )
        .await
        .unwrap();
    assert!(results[0].is_success());
    let comment = results[0].name.clone().unwrap();
    assert!(control.comments().iter().any(|c| c.name == comment));
    let removed = client
        .run_action(
            &Action::RemoveAllDowntimes,
            &ActionTarget::Comment(comment.clone()),
            "alice",
        )
        .await
        .unwrap();
    assert!(removed[0].is_success(), "{removed:?}");
    assert!(!control.comments().iter().any(|c| c.name == comment));

    let now = control.now().as_unix_seconds();
    let results = client
        .run_action(
            &Action::ScheduleDowntime {
                comment: "maintenance".to_owned(),
                start: Timestamp::from_unix_seconds(now - 60.0),
                end: Timestamp::from_unix_seconds(now + 3600.0),
                mode: DowntimeMode::Fixed,
                all_services: false,
                child_options: ic_model::ChildOptions::None,
                trigger_name: None,
            },
            &ActionTarget::Objects(vec![ok]),
            "alice",
        )
        .await
        .unwrap();
    assert!(results[0].is_success(), "{results:?}");
    let downtime = results[0].name.clone().unwrap();
    let removed = client
        .run_action(
            &Action::RemoveAllDowntimes,
            &ActionTarget::Downtime(downtime.clone()),
            "alice",
        )
        .await
        .unwrap();
    assert!(removed[0].is_success(), "{removed:?}");
    assert!(!control.downtimes().iter().any(|d| d.name == downtime));
}

// --- Events ------------------------------------------------------------------

/// The next event, within `timeout`.
async fn next_event(stream: &mut EventStream, timeout: Duration) -> Event {
    tokio::time::timeout(timeout, stream.next())
        .await
        .expect("an event in time")
        .expect("the stream is open")
        .unwrap()
}

#[tokio::test]
async fn the_event_stream_follows_changes_and_absorbs_a_burst() {
    let server = start(MockConfig::with_scenario(scenarios::prod_cluster())).await;
    let control = server.control();
    let client = root(&server);
    let mut stream = client.events("icygui-test", &EventKind::ALL).await.unwrap();
    assert!(
        control
            .wait_for_event_streams(1, Duration::from_secs(5))
            .await
    );

    let target = control
        .services()
        .into_iter()
        .find(|s| s.state == ServiceState::Ok)
        .unwrap();
    control
        .set_service_state(
            target.key.host.as_str(),
            &target.key.name,
            ServiceState::Critical,
            "CRITICAL - disk full",
            true,
        )
        .unwrap();
    let object = target.object_key();
    let mut hard = None;
    while hard.is_none() {
        if let Event::StateChange {
            object: changed,
            state_type: StateType::Hard,
            result,
            ..
        } = next_event(&mut stream, Duration::from_secs(5)).await
            && changed == object
        {
            hard = Some(result);
        }
    }
    assert_eq!(hard.unwrap().output, "CRITICAL - disk full");

    // A burst: every object is re-checked, each one reported.
    let queued = control.burst();
    let expected = control.hosts().len() + control.services().len();
    assert_eq!(queued, expected);
    let mut checked = BTreeSet::new();
    let deadline = Instant::now() + Duration::from_mins(1);
    while checked.len() < expected {
        assert!(Instant::now() < deadline, "{} of {expected}", checked.len());
        if let Event::CheckResult { object, .. } =
            next_event(&mut stream, Duration::from_secs(10)).await
        {
            checked.insert(object);
        }
    }
    assert!(control.wait_for_queued_checks(Duration::from_secs(5)).await);
}

// --- Errors ------------------------------------------------------------------

#[tokio::test]
async fn errors_map_like_icinga() {
    let mut config = MockConfig::with_scenario(scenarios::prod_cluster());
    config.users.push(MockUser::new(
        "viewer",
        "viewer-test",
        &["objects/query/Host", "status/query", "events/CheckResult"],
    ));
    let server = start(config).await;
    let control = server.control();

    // 401
    let wrong = client_as(&server, ("root", "wrong"), pinned(&server));
    assert_eq!(wrong.info().await.unwrap_err(), ApiError::Unauthorized);

    // 403: a missing permission, with Icinga's message (which lowercases
    // the permission).
    let viewer = client_as(&server, ("viewer", "viewer-test"), pinned(&server));
    viewer.hosts().await.unwrap();
    assert_eq!(
        viewer.services(Detail::Lean).await.unwrap_err(),
        ApiError::Forbidden("Missing permission: objects/query/service".to_owned())
    );
    let host = control.hosts()[0].key();
    let error = viewer
        .run_action(
            &Action::CheckNow { force: false },
            &ActionTarget::Objects(vec![host]),
            "viewer",
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&error, ApiError::Forbidden(message) if message.contains("actions/reschedule-check")),
        "{error:?}"
    );
    // A missing event permission comes back as Icinga's generic 404 and is
    // turned back into the permission it lacks.
    let error = viewer
        .events(
            "icygui-viewer",
            &[EventKind::CheckResult, EventKind::StateChange],
        )
        .await
        .unwrap_err();
    assert_eq!(
        error,
        ApiError::Forbidden("Missing permission: events/StateChange".to_owned())
    );

    // 404 and server errors.
    let client = root(&server);
    control.fail_next(1, 404);
    assert!(matches!(
        client.hosts().await.unwrap_err(),
        ApiError::NotFound(_)
    ));
    control.fail_next(1, 503);
    let error = client.services(Detail::Lean).await.unwrap_err();
    assert!(
        matches!(error, ApiError::Http { status: 503, .. }),
        "{error:?}"
    );
    assert!(error.is_transient());
    client.info().await.unwrap();
}

// --- Scale -------------------------------------------------------------------

/// The tiered load ic-core does on connect: hosts in full, services lean,
/// then the services in a problem state in full, by name.
async fn tiered_load(client: &Client) -> (usize, usize, usize) {
    let hosts = client.hosts().await.unwrap();
    let services = client.services(Detail::Lean).await.unwrap();
    let problems: Vec<ObjectKey> = services
        .iter()
        .filter(|service| service.is_problem())
        .map(Service::object_key)
        .collect();
    let fetched = client.objects(&problems, Detail::Full).await.unwrap();
    assert!(fetched.missing.is_empty());
    assert_eq!(fetched.services.len(), problems.len());
    assert!(fetched.services.iter().all(|s| s.check.result.is_some()));
    (hosts.len(), services.len(), problems.len())
}

/// The sizes of the service lists, in bytes.
struct Payloads {
    /// [`Detail::Lean`].
    lean: usize,
    /// [`Detail::Full`].
    full: usize,
    /// The check results in the full list (`last_check_result` values).
    results: usize,
    /// All attributes (no `attrs`).
    all: usize,
}

impl Payloads {
    async fn measure(server: &MockServer) -> Self {
        let raw = Raw::new(
            &url(server),
            None,
            server.ca_pem().unwrap().as_bytes(),
            ROOT.0,
            ROOT.1,
        );
        let services = server.control().services().len();
        let query = async |body| {
            let answer = raw.query("services", &body).await;
            assert_eq!(answer.status, 200);
            assert_eq!(answer.json()["results"].as_array().unwrap().len(), services);
            answer
        };
        let lean = query(json!({ "attrs": Detail::Lean.service_attrs() })).await;
        let full = query(json!({ "attrs": Detail::Full.service_attrs() })).await;
        let all = query(json!({})).await;
        Self {
            lean: lean.body.len(),
            full: full.body.len(),
            results: full.attr_bytes("last_check_result"),
            all: all.body.len(),
        }
    }

    /// Lean leaves out at least every check result.
    fn assert_lean_saves_the_results(&self) {
        let Self {
            lean,
            full,
            results,
            all,
        } = *self;
        assert!(
            lean + results < full,
            "lean {lean} bytes, full {full} bytes, check results {results} bytes"
        );
        assert!(full < all, "full {full} bytes, all attributes {all} bytes");
    }
}

#[tokio::test]
async fn a_tenth_of_the_large_scenario_loads_in_tiers() {
    let server = start(MockConfig {
        tls: MockTls::CaSigned,
        ..MockConfig::with_scenario(scenarios::large_with_hosts(200, 7))
    })
    .await;
    let client = root(&server);
    let started = Instant::now();
    let (hosts, services, problems) = tiered_load(&client).await;
    assert_eq!(hosts, 200);
    assert_eq!(services, 3_000);
    assert!(problems > 50, "about 5 % problems: {problems}");
    assert!(started.elapsed() < Duration::from_mins(1));

    Payloads::measure(&server)
        .await
        .assert_lean_saves_the_results();
    let (count, bytes) = notification_payload(&server).await;
    assert_eq!(count, 3_200, "one per host and service");
    assert!(
        bytes / count < 250,
        "{} bytes per notification",
        bytes / count
    );
}

/// Icinga's notifications as `Client::notifications` asks for them: how
/// many, and the answer's size in bytes.
async fn notification_payload(server: &MockServer) -> (usize, usize) {
    let raw = Raw::new(
        &url(server),
        None,
        server.ca_pem().unwrap().as_bytes(),
        ROOT.0,
        ROOT.1,
    );
    let answer = raw
        .query(
            "notifications",
            &json!({ "attrs": ["host_name", "service_name", "last_notification", "notified_problem_users"] }),
        )
        .await;
    assert_eq!(answer.status, 200);
    let count = answer.json()["results"].as_array().unwrap().len();
    (count, answer.body.len())
}

/// Production scale: 2 000 hosts, 30 000 services (docs/performance.md).
#[tokio::test]
#[ignore = "slow in debug builds; run with --ignored --nocapture"]
async fn the_large_scenario_loads_in_tiers() {
    let server = start(MockConfig {
        tls: MockTls::CaSigned,
        ..MockConfig::with_scenario(scenarios::large(1))
    })
    .await;
    let client = root(&server);
    let started = Instant::now();
    let (hosts, services, problems) = tiered_load(&client).await;
    let elapsed = started.elapsed();
    assert_eq!(hosts, 2_000);
    assert_eq!(services, 30_000);
    println!(
        "tiered load: {hosts} hosts, {services} services, {problems} problems in {elapsed:.2?}"
    );
    assert!(elapsed < Duration::from_mins(5), "{elapsed:?}");

    let payloads = Payloads::measure(&server).await;
    let Payloads {
        lean,
        full,
        results,
        all,
    } = payloads;
    println!(
        "services: lean {lean} bytes ({:.0} per service), full {full} bytes ({:.0}, check results {results}), all attributes {all} bytes ({:.0})",
        per_service(lean, services),
        per_service(full, services),
        per_service(all, services),
    );
    payloads.assert_lean_saves_the_results();
    let started = Instant::now();
    let (count, bytes) = notification_payload(&server).await;
    println!(
        "notifications: {count} in {bytes} bytes ({:.0} per notification), {:.2?}",
        per_service(bytes, count),
        started.elapsed()
    );
}

#[expect(clippy::cast_precision_loss, reason = "a rough average for the report")]
fn per_service(bytes: usize, services: usize) -> f64 {
    bytes as f64 / services as f64
}
