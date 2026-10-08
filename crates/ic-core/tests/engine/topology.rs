//! Several API URLs per environment (ENV-12) against several in-process
//! `ic-mock` servers forming one cluster: a single master, an HA pair in
//! one zone (failover), a master with a child zone (a partial view that is
//! labelled, the preference for the full view and the way back to it), an
//! API user who may not read the zones (a view not verified), and testing
//! each URL on its own.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers fail the test loudly"
)]

use std::time::Duration;

use futures::StreamExt as _;

use crate::support::{ENV_ID, Engine, FakeSecrets, Launch, PASSWORD, USER, environment, mock};
use ic_config::{ApiUrl, AuthConfig, Environment};
use ic_core::{ClusterView, ConnectedNode, ConnectionState, NodeState, Tuning, test_connection};
use ic_mock::{MockConfig, MockServer, MockTls, MockUser, Scenario, scenarios};
use ic_model::{FeatureState, HostState, ServiceState};
use secrecy::SecretString;

/// The `prod-cluster` scenario: two masters in the top-level zone (an HA
/// pair), its satellites `sat-ams-01` and `sat-fra-01` in the child zones
/// `ams` and `fra`.
fn cluster() -> Scenario {
    scenarios::prod_cluster()
}

/// A mock serving `cluster` as the node `node`.
async fn node(cluster: &Scenario, node: &str) -> MockServer {
    mock(MockConfig::with_scenario(cluster.for_node(node))).await
}

/// The URL of `server`, pinned to its certificate.
fn url_of(server: &MockServer) -> ApiUrl {
    ApiUrl {
        pinned_sha256: Some(server.cert_fingerprint()),
        ..ApiUrl::new(&server.url())
    }
}

/// An environment listing `servers` in this order of preference.
fn environment_of(servers: &[&MockServer]) -> Environment {
    let mut environment = environment(servers[0]);
    environment.urls = servers.iter().map(|server| url_of(server)).collect();
    environment
}

/// Fast probes of the preferred URLs.
fn tuning() -> Tuning {
    Tuning {
        probe_initial: Duration::from_millis(40),
        probe_max: Duration::from_millis(160),
        ..crate::support::tuning()
    }
}

fn start(environment: Environment, server: &MockServer) -> Engine {
    Launch {
        environment,
        tuning: tuning(),
        ..Launch::new(server)
    }
    .start()
}

/// Waits until connected to a node `pick` accepts; returns it.
async fn connected_to(
    engine: &mut Engine,
    mut pick: impl FnMut(&ConnectedNode) -> bool,
) -> ConnectedNode {
    let state = engine
        .wait_state(|state| matches!(state, ConnectionState::Connected { node, .. } if pick(node)))
        .await;
    let ConnectionState::Connected { node, .. } = state else {
        unreachable!()
    };
    node
}

/// `host:port` of a mock, as the passed-over list names it.
fn label(server: &MockServer) -> String {
    ApiUrl::new(&server.url()).label()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_single_master_sees_the_whole_cluster() {
    let master = node(&scenarios::prod_cluster(), "master-01").await;
    let mut engine = start(environment_of(&[&master]), &master);
    let node = connected_to(&mut engine, |_| true).await;
    assert_eq!(
        node,
        ConnectedNode {
            url: master.url(),
            url_index: 0,
            name: "master-01".to_owned(),
            zone: Some("master".to_owned()),
            view: ClusterView::Full,
            passed_over: Vec::new(),
        }
    );
    let snapshot = engine
        .snapshot(|snapshot| snapshot.node.is_some() && !snapshot.services.is_empty())
        .await;
    assert_eq!(snapshot.node.as_deref(), Some(&node));
    assert_eq!(snapshot.hosts.len(), master.control().hosts().len());
    // A full view probes nothing: the master sees GET /v1, the node's
    // name and the zones once, then only the load and the stream.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let logins = master
        .control()
        .requests()
        .iter()
        .filter(|request| request.path == "/v1")
        .count();
    assert_eq!(logins, 1);
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_ha_pair_fails_over_and_stays_on_the_other_master() {
    let cluster = cluster();
    let first = node(&cluster, "master-01").await;
    let second = node(&cluster, "master-02").await;
    let (first_port, first_tls, first_label) = (first.port(), first.tls_material(), label(&first));
    let mut engine = start(environment_of(&[&first, &second]), &first);

    let node = connected_to(&mut engine, |_| true).await;
    assert_eq!(node.name, "master-01");
    assert_eq!(node.view, ClusterView::Full);
    let hosts = first.control().hosts().len();
    engine
        .snapshot(|snapshot| snapshot.hosts.len() == hosts && snapshot.node.is_some())
        .await;

    // master-01 goes away: the engine fails over to master-02, which sees
    // the same (the whole cluster), and says why it passed over the first.
    first.shutdown().await;
    let node = connected_to(&mut engine, |node| node.name == "master-02").await;
    assert_eq!(node.url_index, 1);
    assert_eq!(node.view, ClusterView::Full);
    assert_eq!(node.passed_over.len(), 1);
    assert_eq!(node.passed_over[0].0, first_label);
    let snapshot = engine
        .snapshot(|snapshot| {
            snapshot
                .node
                .as_ref()
                .is_some_and(|node| node.name == "master-02")
        })
        .await;
    assert_eq!(snapshot.hosts.len(), hosts, "the objects stay");

    // master-01 comes back: both see everything, so the engine stays on
    // master-02 (switching would only cost a new stream) and doesn't even
    // ask master-01.
    let back = mock(MockConfig {
        port: first_port,
        tls: MockTls::Provided(first_tls),
        ..MockConfig::with_scenario(cluster.for_node("master-01"))
    })
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(back.control().requests().is_empty());
    assert!(matches!(
        engine.states().last(),
        Some(ConnectionState::Connected { node, .. }) if node.name == "master-02"
    ));
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_satellite_is_a_labelled_partial_view_until_the_master_is_back() {
    let cluster = cluster();
    let master = node(&cluster, "master-01").await;
    let satellite = node(&cluster, "sat-ams-01").await;
    let all_hosts = master.control().hosts().len();
    let zone_hosts = satellite.control().hosts().len();
    assert!(zone_hosts < all_hosts);

    // The master fails every request: the satellite is all there is.
    master.control().fail_next(u32::MAX, 503);
    let mut engine = start(environment_of(&[&master, &satellite]), &master);
    let node = connected_to(&mut engine, |_| true).await;
    assert_eq!(node.name, "sat-ams-01");
    assert_eq!(node.url_index, 1);
    assert_eq!(node.zone.as_deref(), Some("ams"));
    assert_eq!(
        node.view,
        ClusterView::Partial {
            zone: "ams".to_owned()
        }
    );
    assert_eq!(node.view.label(), "partial view: zone ams");
    assert_eq!(node.passed_over.len(), 1);
    assert_eq!(node.passed_over[0].0, label(&master));
    assert!(
        node.passed_over[0].1.contains("503"),
        "{:?}",
        node.passed_over
    );
    // The objects are the zone's, and labelled as such.
    let snapshot = engine
        .snapshot(|snapshot| snapshot.node.is_some() && !snapshot.hosts.is_empty())
        .await;
    assert_eq!(snapshot.hosts.len(), zone_hosts);
    assert!(!snapshot.node.as_ref().unwrap().view.is_full());

    // Meanwhile the engine asks the master again, gently: with growing
    // waits (40 ms doubling to 160 ms here), never in a burst.
    tokio::time::sleep(Duration::from_millis(900)).await;
    let probes = master
        .control()
        .requests()
        .iter()
        .filter(|request| request.path == "/v1")
        .count();
    assert!((3..=20).contains(&probes), "{probes} probes in 0.9 s");

    // The master answers again: the engine switches back to it and
    // reloads; the snapshot says partial until the whole cluster is in.
    master.control().fail_next(0, 503);
    let node = connected_to(&mut engine, |node| node.name == "master-01").await;
    assert_eq!(node.view, ClusterView::Full);
    assert_eq!(node.url_index, 0);
    let snapshot = engine
        .snapshot(|snapshot| {
            snapshot
                .node
                .as_ref()
                .is_some_and(|node| node.view.is_full())
        })
        .await;
    assert_eq!(snapshot.hosts.len(), all_hosts, "the whole cluster");
    for event in &engine.seen {
        if let ic_core::CoreEvent::Snapshot(snapshot) = event
            && snapshot
                .node
                .as_ref()
                .is_some_and(|node| node.view.is_full())
        {
            assert_eq!(snapshot.hosts.len(), all_hosts, "never complete-looking");
        }
    }
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_node_that_sees_everything_is_preferred_over_the_first_url() {
    let cluster = cluster();
    let satellite = node(&cluster, "sat-ams-01").await;
    let master = node(&cluster, "master-01").await;
    // Listed first, but the satellite sees only its zone.
    let mut engine = start(environment_of(&[&satellite, &master]), &satellite);
    let node = connected_to(&mut engine, |_| true).await;
    assert_eq!(node.name, "master-01");
    assert_eq!(node.view, ClusterView::Full);
    assert_eq!(
        node.passed_over,
        [(label(&satellite), "partial view: zone ams".to_owned())]
    );
    // No event stream on the satellite.
    assert_eq!(satellite.control().event_streams(), 0);
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_the_zones_the_view_is_not_verified() {
    // Everything the client reads, but not the zones.
    let permissions: Vec<&str> = ic_core::REQUIRED_PERMISSIONS
        .iter()
        .copied()
        .filter(|permission| *permission != "objects/query/Zone")
        .collect();
    let server = mock(MockConfig {
        users: vec![MockUser::new("nozones", PASSWORD, &permissions)],
        ..MockConfig::with_scenario(scenarios::prod_cluster().for_node("sat-ams-01"))
    })
    .await;
    let mut environment = environment_of(&[&server]);
    environment.auth = AuthConfig::Basic {
        username: "nozones".to_owned(),
    };
    let mut engine = Launch {
        environment,
        secrets: FakeSecrets::with(ENV_ID, PASSWORD),
        tuning: tuning(),
        ..Launch::new(&server)
    }
    .start();
    let node = connected_to(&mut engine, |_| true).await;
    assert_eq!(node.name, "sat-ams-01", "the status names the node");
    assert_eq!(node.zone, None);
    let ClusterView::Unverified { reason } = &node.view else {
        panic!("expected an unverified view, got {:?}", node.view);
    };
    assert!(reason.contains("objects/query/Zone"), "{reason}");
    assert_eq!(node.view.label(), "view not verified");
    engine.shutdown();

    // Without the status, the URL's host names the node.
    let server = mock(MockConfig {
        users: vec![MockUser::new(
            "blind",
            PASSWORD,
            &["objects/query/Host", "objects/query/Service"],
        )],
        ..MockConfig::with_scenario(scenarios::lab())
    })
    .await;
    let mut environment = environment_of(&[&server]);
    environment.auth = AuthConfig::Basic {
        username: "blind".to_owned(),
    };
    let mut engine = start(environment, &server);
    let node = connected_to(&mut engine, |_| true).await;
    assert_eq!(node.name, "127.0.0.1");
    assert!(matches!(node.view, ClusterView::Unverified { .. }));
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn several_urls_keep_retrying_while_one_may_come_back() {
    let cluster = cluster();
    let first = node(&cluster, "master-01").await;
    let second = node(&cluster, "master-02").await;
    let mut environment = environment_of(&[&first, &second]);
    // The second's certificate isn't the pinned one; the first is down.
    environment.urls[1].pinned_sha256 = Some(ic_config::format_fingerprint(&[0xAB; 32]));
    let first_label = label(&first);
    first.shutdown().await;
    let mut engine = start(environment, &second);
    let state = engine
        .wait_state(|state| matches!(state, ConnectionState::Reconnecting { .. }))
        .await;
    let ConnectionState::Reconnecting { error, .. } = state else {
        unreachable!()
    };
    assert!(error.contains(&first_label), "{error}");
    assert!(
        error.contains(&format!(
            "{}: the certificate doesn't match the pinned one",
            label(&second)
        )),
        "{error}"
    );
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_url_is_tested_on_its_own() {
    let cluster = cluster();
    let satellite = node(&cluster, "sat-ams-01").await;
    let master = node(&cluster, "master-02").await;
    let environment = environment_of(&[&satellite, &master]);
    let password = || Some(SecretString::from(PASSWORD.to_owned()));

    let report = test_connection(environment.clone(), 0, password())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(report.info.user, USER);
    assert_eq!(report.node.name, "sat-ams-01");
    assert_eq!(
        report.node.view,
        ClusterView::Partial {
            zone: "ams".to_owned()
        }
    );
    let report = test_connection(environment.clone(), 1, password())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(report.node.name, "master-02");
    assert_eq!(report.node.url_index, 1);
    assert_eq!(report.node.view, ClusterView::Full);
    // A URL the environment doesn't have.
    assert!(
        test_connection(environment, 2, password())
            .await
            .unwrap()
            .is_err()
    );
    // Nothing but logins, node names and zones went to the nodes.
    for server in [&satellite, &master] {
        for request in server.control().requests() {
            assert!(
                ["/v1", "/v1/status/IcingaApplication", "/v1/objects/zones"]
                    .contains(&request.path.as_str())
                    || request.path.starts_with("/v1/status"),
                "{}",
                request.path
            );
        }
    }
}

// --- notifications across node switches ---------------------------------------------

/// Faster probes and reloads for the switches below (the second reload
/// otherwise waits `reload_spacing` after the first).
fn switching() -> Tuning {
    Tuning {
        reload_spacing: Duration::from_millis(50),
        ..tuning()
    }
}

fn start_switching(environment: Environment, server: &MockServer) -> Engine {
    Launch {
        environment,
        tuning: switching(),
        ..Launch::new(server)
    }
    .start()
}

/// An OK service outside the satellite's zone, on a healthy host, with no
/// downtime or flapping: one the default rule notifies about.
fn outside_the_zone(master: &MockServer, satellite: &MockServer) -> (String, String, String) {
    let zone: std::collections::HashSet<String> = satellite
        .control()
        .hosts()
        .iter()
        .map(|host| host.name.to_string())
        .collect();
    let control = master.control();
    let hosts = control.hosts();
    let service = control
        .services()
        .into_iter()
        .find(|service| {
            service.state == ServiceState::Ok
                && service.check.downtime_depth == 0
                && !service.check.flapping
                && !zone.contains(service.key.host.as_str())
                && hosts.iter().any(|host| {
                    host.name == service.key.host
                        && host.state == HostState::Up
                        && host.check.downtime_depth == 0
                })
        })
        .expect("an OK service outside the zone");
    let host = hosts
        .iter()
        .find(|host| host.name == service.key.host)
        .unwrap();
    (
        service.key.host.to_string(),
        service.key.name.to_string(),
        format!("{} on {}", service.display_name, host.display_name),
    )
}

fn set_state(server: &MockServer, host: &str, service: &str, state: ServiceState) {
    server
        .control()
        .set_service_state(
            host,
            service,
            state,
            &format!("{state:?} - {service}"),
            true,
        )
        .unwrap();
}

/// Waits until the engine emitted `count` notifications, and a moment
/// longer; then every notification title so far, in order (anything that
/// shouldn't have notified shows up, also while the test waited for
/// something else).
async fn titles(engine: &mut Engine, count: usize) -> Vec<String> {
    while engine.notifications().len() < count {
        engine.notification().await;
    }
    while tokio::time::timeout(Duration::from_millis(300), engine.events.next())
        .await
        .ok()
        .flatten()
        .map(|event| engine.seen.push(event))
        .is_some()
    {}
    engine
        .notifications()
        .into_iter()
        .map(|record| record.intent.title)
        .collect()
}

/// Waits until the engine shows `node`'s data: connected to it and its
/// load in (`hosts` objects).
async fn showing(engine: &mut Engine, node: &str, hosts: usize) {
    connected_to(engine, |connected| connected.name == node).await;
    engine
        .snapshot(|snapshot| {
            snapshot
                .node
                .as_ref()
                .is_some_and(|shown| shown.name == node)
                && snapshot.hosts.len() == hosts
        })
        .await;
}

/// The master stops answering and its stream ends: the engine falls back
/// to the satellite.
fn master_down(master: &MockServer) {
    master.control().fail_next(u32::MAX, 503);
    master.control().drop_event_streams();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn what_happens_outside_the_zone_during_a_fallback_notifies_once_the_master_is_back() {
    let cluster = cluster();
    let master = node(&cluster, "master-01").await;
    let satellite = node(&cluster, "sat-ams-01").await;
    let all_hosts = master.control().hosts().len();
    let zone_hosts = satellite.control().hosts().len();
    let (host, service, title) = outside_the_zone(&master, &satellite);
    let mut engine = start_switching(environment_of(&[&master, &satellite]), &master);
    showing(&mut engine, "master-01", all_hosts).await;

    // A problem notified before the fallback recovers during it: the
    // recovery is notified once the master is back.
    set_state(&master, &host, &service, ServiceState::Critical);
    let critical = format!("CRITICAL · {title}");
    assert_eq!(
        titles(&mut engine, 1).await,
        std::slice::from_ref(&critical)
    );
    master_down(&master);
    showing(&mut engine, "sat-ams-01", zone_hosts).await;
    set_state(&master, &host, &service, ServiceState::Ok);
    master.control().fail_next(0, 503);
    showing(&mut engine, "master-01", all_hosts).await;
    let recovered = format!("RECOVERED · {title}");
    assert_eq!(
        titles(&mut engine, 2).await,
        [critical.clone(), recovered.clone()],
        "the recovery, and nothing in the zone again"
    );

    // A problem that starts during a fallback is notified once the master
    // is back.
    master_down(&master);
    showing(&mut engine, "sat-ams-01", zone_hosts).await;
    set_state(&master, &host, &service, ServiceState::Critical);
    master.control().fail_next(0, 503);
    showing(&mut engine, "master-01", all_hosts).await;
    assert_eq!(
        titles(&mut engine, 3).await,
        [critical.clone(), recovered, critical.clone()]
    );
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn switching_nodes_notifies_nothing_again() {
    let cluster = cluster();
    let first = node(&cluster, "master-01").await;
    let second = node(&cluster, "master-02").await;
    let satellite = node(&cluster, "sat-ams-01").await;
    let all_hosts = first.control().hosts().len();
    let zone_hosts = satellite.control().hosts().len();
    let (host, service, title) = outside_the_zone(&first, &satellite);
    let mut engine = start_switching(environment_of(&[&first, &second, &satellite]), &first);
    showing(&mut engine, "master-01", all_hosts).await;

    // An HA failover, then a fallback to the satellite and back: the
    // problems the cluster had all along don't notify again.
    first.shutdown().await;
    showing(&mut engine, "master-02", all_hosts).await;
    master_down(&second);
    showing(&mut engine, "sat-ams-01", zone_hosts).await;
    second.control().fail_next(0, 503);
    showing(&mut engine, "master-02", all_hosts).await;
    // A fence: the next notification is this one.
    set_state(&second, &host, &service, ServiceState::Critical);
    let critical = format!("CRITICAL · {title}");
    assert_eq!(
        titles(&mut engine, 1).await,
        std::slice::from_ref(&critical)
    );
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn objects_a_master_adds_to_a_satellites_view_keep_what_was_notified() {
    let cluster = cluster();
    let master = node(&cluster, "master-01").await;
    let satellite = node(&cluster, "sat-ams-01").await;
    let all_hosts = master.control().hosts().len();
    let zone_hosts = satellite.control().hosts().len();
    let (host, service, title) = outside_the_zone(&master, &satellite);

    // An earlier run on the master notified a problem outside the zone.
    let mut earlier = Launch {
        environment: environment_of(&[&master]),
        tuning: switching(),
        ..Launch::new(&master)
    }
    .start();
    showing(&mut earlier, "master-01", all_hosts).await;
    set_state(&master, &host, &service, ServiceState::Critical);
    let critical = format!("CRITICAL · {title}");
    assert_eq!(titles(&mut earlier, 1).await, [critical]);
    earlier.shutdown();

    // This run starts on the satellite (the master doesn't answer), then
    // the master comes back with the problem the satellite never showed.
    master.control().fail_next(u32::MAX, 503);
    let mut engine = Launch {
        environment: environment_of(&[&master, &satellite]),
        tuning: switching(),
        data_dir: Some(earlier.data_dir().to_owned()),
        ..Launch::new(&master)
    }
    .start();
    showing(&mut engine, "sat-ams-01", zone_hosts).await;
    master.control().fail_next(0, 503);
    showing(&mut engine, "master-01", all_hosts).await;
    // The event log says which problems were notified (asynchronously).
    tokio::time::sleep(Duration::from_millis(300)).await;
    // Its recovery is notified: the rule engine learned it was.
    set_state(&master, &host, &service, ServiceState::Ok);
    let recovered = format!("RECOVERED · {title}");
    assert_eq!(titles(&mut engine, 1).await, [recovered]);
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn coming_back_to_the_master_asks_for_no_object_by_name_before_the_reload() {
    let cluster = cluster();
    let master = node(&cluster, "master-01").await;
    let satellite = node(&cluster, "sat-ams-01").await;
    let all_hosts = master.control().hosts().len();
    let zone_hosts = satellite.control().hosts().len();
    let (host, service, _) = outside_the_zone(&master, &satellite);
    let mut engine = Launch {
        environment: environment_of(&[&master, &satellite]),
        tuning: Tuning {
            // The reload follows the switch back after a while.
            reload_spacing: Duration::from_millis(600),
            ..tuning()
        },
        ..Launch::new(&master)
    }
    .start();
    showing(&mut engine, "master-01", all_hosts).await;
    master_down(&master);
    showing(&mut engine, "sat-ams-01", zone_hosts).await;
    master.control().fail_next(0, 503);
    connected_to(&mut engine, |node| node.name == "master-01").await;
    master.control().clear_requests();
    // Events about objects the satellite didn't serve, before the reload.
    for state in [ServiceState::Critical, ServiceState::Warning] {
        set_state(&master, &host, &service, state);
    }
    engine
        .snapshot(|snapshot| {
            snapshot
                .node
                .as_ref()
                .is_some_and(|node| node.name == "master-01")
        })
        .await;
    // The requests before the reload's first list (every host).
    let by_name = |request: &ic_mock::RecordedRequest| {
        request
            .body
            .as_ref()
            .is_some_and(|body| body.get("hosts").is_some() || body.get("services").is_some())
    };
    let requests = master.control().requests();
    let before_reload: Vec<_> = requests
        .iter()
        .take_while(|request| request.path != "/v1/objects/hosts" || by_name(request))
        .collect();
    assert!(before_reload.len() < requests.len(), "the reload ran");
    let lookups = before_reload
        .iter()
        .filter(|request| request.path.starts_with("/v1/objects/") && by_name(request))
        .count();
    assert_eq!(lookups, 0, "the reload brings them");
    // And the reload brought the service's latest state.
    let snapshot = engine
        .snapshot(|snapshot| {
            snapshot.services.values().any(|candidate| {
                candidate.key.host.as_str() == host
                    && &*candidate.key.name == service.as_str()
                    && candidate.state == ServiceState::Warning
            })
        })
        .await;
    assert_eq!(snapshot.hosts.len(), all_hosts);
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_master_that_hangs_is_passed_over_quickly_and_not_asked_on_every_reconnect() {
    let cluster = cluster();
    let first = node(&cluster, "master-01").await;
    let second = node(&cluster, "master-02").await;
    // master-01 accepts connections but answers nothing for two minutes.
    first.control().set_latency(Duration::from_mins(2));
    let mut engine = Launch {
        environment: environment_of(&[&first, &second]),
        tuning: Tuning {
            identify_timeout: Duration::from_millis(300),
            ..tuning()
        },
        ..Launch::new(&second)
    }
    .start();
    let started = std::time::Instant::now();
    let node = connected_to(&mut engine, |_| true).await;
    assert_eq!(node.name, "master-02");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(
        node.passed_over,
        [(label(&first), "no answer within 300ms".to_owned())]
    );

    // master-02's stream ends: the reconnect starts at master-02, which
    // took over, and doesn't wait for master-01 again.
    let asked = first.control().requests().len();
    second.control().drop_event_streams();
    engine
        .wait_state(|state| {
            matches!(
                state,
                ConnectionState::Reconnecting { .. } | ConnectionState::Connecting { .. }
            )
        })
        .await;
    let node = connected_to(&mut engine, |_| true).await;
    assert_eq!(node.name, "master-02");
    assert!(node.passed_over.is_empty(), "{:?}", node.passed_over);
    assert_eq!(first.control().requests().len(), asked);
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_standby_never_trusted_is_offered_for_trust_while_retrying() {
    let cluster = cluster();
    let first = node(&cluster, "master-01").await;
    let second = node(&cluster, "master-02").await;
    let mut environment = environment_of(&[&first, &second]);
    // master-02 was never trusted: no pin, no CA, no system roots.
    environment.urls[1].pinned_sha256 = None;
    let second_url = environment.urls[1].url.clone();
    first.shutdown().await;
    let mut engine = start(environment, &second);
    let state = engine
        .wait_state(|state| matches!(state, ConnectionState::Reconnecting { .. }))
        .await;
    let ConnectionState::Reconnecting { untrusted, .. } = &state else {
        unreachable!()
    };
    let untrusted = untrusted.as_ref().expect("the standby is offered");
    assert_eq!(untrusted.url, second_url);
    assert_eq!(
        untrusted.certificate.fingerprint(),
        second.cert_fingerprint()
    );
    let (url, _, certificate) = state.untrusted().unwrap();
    assert_eq!(url, second_url);
    assert!(certificate.is_some());
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_cluster_nodes_follow_the_status_poll() {
    let cluster = cluster();
    let master = node(&cluster, "master-01").await;
    let mut engine = Launch {
        environment: environment_of(&[&master]),
        tuning: Tuning {
            status_interval: Duration::from_millis(100),
            ..tuning()
        },
        ..Launch::new(&master)
    }
    .start();
    connected_to(&mut engine, |_| true).await;
    let states = |snapshot: &ic_core::snapshot::Snapshot| -> Vec<(String, String, NodeState)> {
        snapshot
            .cluster_nodes()
            .into_iter()
            .map(|node| (node.name, node.zone, node.state))
            .collect()
    };
    let snapshot = engine
        .snapshot(|snapshot| snapshot.cluster_nodes().len() == 4)
        .await;
    assert_eq!(
        states(&snapshot),
        [
            (
                "master-01".to_owned(),
                "master".to_owned(),
                NodeState::Connected
            ),
            (
                "master-02".to_owned(),
                "master".to_owned(),
                NodeState::Connected
            ),
            (
                "sat-ams-01".to_owned(),
                "ams".to_owned(),
                NodeState::Connected
            ),
            (
                "sat-fra-01".to_owned(),
                "fra".to_owned(),
                NodeState::Connected
            ),
        ]
    );
    // The other master drops out of the cluster: the next status poll
    // says so.
    master
        .control()
        .set_endpoint_connected("master-02", false)
        .unwrap();
    engine
        .snapshot(|snapshot| {
            snapshot
                .cluster_nodes()
                .iter()
                .any(|node| node.name == "master-02" && node.state == NodeState::Disconnected)
        })
        .await;
    // One small request per status poll, by name, never for the node
    // itself.
    let requests = master.control().requests();
    let polls = requests
        .iter()
        .filter(|request| request.path == "/v1/status/CIB")
        .count();
    let asked: Vec<_> = requests
        .iter()
        .filter(|request| {
            request.path == "/v1/objects/endpoints"
                && request
                    .body
                    .as_ref()
                    .is_some_and(|body| body.get("endpoints").is_some())
        })
        .collect();
    assert!(!asked.is_empty());
    assert!(asked.len() <= polls, "{} for {polls} polls", asked.len());
    for request in asked {
        let body = request.body.as_ref().unwrap();
        assert_eq!(
            body["endpoints"],
            serde_json::json!(["master-02", "sat-ams-01", "sat-fra-01"])
        );
        assert_eq!(body["attrs"][0], "connected");
    }
    // Their numbers come with them (the cluster health page's columns).
    let snapshot = engine
        .snapshot(|snapshot| snapshot.health.endpoints.len() == 3)
        .await;
    let numbers = &snapshot.health.endpoints;
    assert_eq!(numbers["sat-fra-01"].version, 21_402, "r2.14.2-1");
    assert_eq!(numbers["sat-ams-01"].version, 21_403);
    assert!(numbers["sat-ams-01"].messages_in > 0.0);
    assert!(
        numbers["master-02"].messages_in.abs() < f64::EPSILON,
        "gone: no messages, its last one kept"
    );
    assert!(numbers["master-02"].last_message.non_zero().is_some());
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_cluster_nodes_are_current_when_the_environment_comes_on_screen() {
    let cluster = cluster();
    let master = node(&cluster, "master-01").await;
    let interval = Duration::from_secs(3);
    let mut engine = Launch {
        environment: environment_of(&[&master]),
        tuning: Tuning {
            status_interval: interval,
            quiet_status_interval: Duration::from_mins(10),
            ..tuning()
        },
        ..Launch::new(&master)
    }
    .start();
    connected_to(&mut engine, |_| true).await;
    // Off screen and quiet, as the app keeps an environment it doesn't
    // show: the nodes' states come every 5 minutes.
    engine.send(ic_core::Command::SetActive(false));
    engine.send(ic_core::Command::SetQuiet(true));
    let control = master.control();
    let asked = || {
        control
            .requests()
            .iter()
            .filter(|request| {
                request.path == "/v1/objects/endpoints"
                    && request
                        .body
                        .as_ref()
                        .is_some_and(|body| body.get("endpoints").is_some())
            })
            .count()
    };
    assert!(crate::support::wait_until(|| asked() == 1).await);
    tokio::time::sleep(interval + Duration::from_millis(200)).await;
    control.set_endpoint_connected("master-02", false).unwrap();

    // Switched to: the states, older than the on-screen interval, come at
    // once rather than with the next poll on the old schedule.
    let switched = std::time::Instant::now();
    engine.send(ic_core::Command::SetActive(true));
    engine.send(ic_core::Command::SetQuiet(false));
    engine
        .snapshot(|snapshot| {
            snapshot
                .cluster_nodes()
                .iter()
                .any(|node| node.name == "master-02" && node.state == NodeState::Disconnected)
        })
        .await;
    assert!(
        switched.elapsed() < interval,
        "at once: {:?}",
        switched.elapsed()
    );
    assert_eq!(asked(), 2);

    // Away and back right after: they are current, nothing more is asked.
    engine.send(ic_core::Command::SetQuiet(true));
    engine.send(ic_core::Command::SetActive(false));
    engine.send(ic_core::Command::SetActive(true));
    engine.send(ic_core::Command::SetQuiet(false));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(asked(), 2);
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cluster_node_that_is_gone_reloads_the_node_list() {
    let cluster = cluster();
    let master = node(&cluster, "master-01").await;
    let mut engine = Launch {
        environment: environment_of(&[&master]),
        tuning: Tuning {
            status_interval: Duration::from_millis(100),
            ..tuning()
        },
        ..Launch::new(&master)
    }
    .start();
    connected_to(&mut engine, |_| true).await;
    engine
        .snapshot(|snapshot| snapshot.cluster_nodes().len() == 4)
        .await;
    // master-02 leaves the cluster unannounced: Icinga answers the next
    // node query naming it with a 404 for all of them.
    let control = master.control();
    control.remove_endpoint("master-02").unwrap();
    control.set_endpoint_connected("sat-ams-01", false).unwrap();
    let snapshot = engine
        .snapshot(|snapshot| {
            snapshot
                .cluster_nodes()
                .iter()
                .any(|node| node.name == "sat-ams-01" && node.state == NodeState::Disconnected)
        })
        .await;
    let names: Vec<String> = snapshot
        .cluster_nodes()
        .into_iter()
        .map(|node| node.name)
        .collect();
    assert_eq!(names, ["master-01", "sat-ams-01", "sat-fra-01"]);
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_single_node_costs_no_poll_when_it_comes_on_screen() {
    let server = mock(MockConfig::with_scenario(scenarios::staging())).await;
    let mut engine = Launch {
        tuning: Tuning {
            status_interval: Duration::from_secs(3),
            ..tuning()
        },
        ..Launch::new(&server)
    }
    .start();
    engine.connected().await;
    // No other node to ask about: switching back and forth asks nothing.
    let control = server.control();
    control.clear_requests();
    for _ in 0..3 {
        engine.send(ic_core::Command::SetActive(false));
        engine.send(ic_core::Command::SetActive(true));
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    let polls = control
        .requests()
        .iter()
        .filter(|request| request.path.starts_with("/v1/status"))
        .count();
    assert_eq!(polls, 0);
    engine.shutdown();
}

/// The cluster health page (topic 06): its listener status and features
/// are asked for only while it shows the environment and it isn't quiet,
/// the listener with the status polls, the features when it opens; the
/// trend comes from every poll, open or not.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_health_page_asks_only_while_it_is_open() {
    let cluster = cluster();
    let master = node(&cluster, "master-01").await;
    let interval = Duration::from_millis(200);
    let mut engine = Launch {
        environment: environment_of(&[&master]),
        tuning: Tuning {
            status_interval: interval,
            ..tuning()
        },
        ..Launch::new(&master)
    }
    .start();
    connected_to(&mut engine, |_| true).await;
    let control = master.control();
    let count = |path: &str| {
        control
            .requests()
            .iter()
            .filter(|request| request.path == path)
            .count()
    };
    // Closed: the polls build the trend, nothing else is asked.
    engine
        .snapshot(|snapshot| snapshot.health.samples.len() >= 3)
        .await;
    assert_eq!(count("/v1/status/ApiListener"), 0);
    assert_eq!(count("/v1/objects/checkercomponents"), 0);

    // Open: the listener and the features at once, then the listener with
    // every poll, the features not again.
    let opened = std::time::Instant::now();
    engine.send(ic_core::Command::WatchHealth(true));
    let snapshot = engine
        .snapshot(|snapshot| {
            snapshot.health.features.is_some() && snapshot.health.listener.is_some()
        })
        .await;
    assert!(opened.elapsed() < interval * 3, "{:?}", opened.elapsed());
    let features = snapshot.health.features.unwrap();
    assert_eq!(features.checker, Some(FeatureState::Running));
    assert_eq!(features.notification, Some(FeatureState::Running));
    assert_eq!(features.icingadb, Some(FeatureState::Off));
    let listener = snapshot.health.listener.clone().unwrap();
    assert_eq!(
        (listener.connected_endpoints, listener.endpoints),
        (3, 3),
        "master-02 and both satellites"
    );
    engine
        .snapshot(|snapshot| {
            snapshot
                .health
                .samples
                .iter()
                .filter(|sample| sample.relay_queue.is_some())
                .count()
                >= 3
        })
        .await;
    let listeners = count("/v1/status/ApiListener");
    let polls = count("/v1/status/CIB");
    assert!(
        listeners >= 3 && listeners <= polls + 1,
        "{listeners} for {polls} polls"
    );
    assert_eq!(count("/v1/objects/checkercomponents"), 1);
    assert_eq!(count("/v1/objects/notificationcomponents"), 1);
    assert_eq!(count("/v1/objects/icingadbs"), 1);

    // Quiet (the window hidden): nothing more, though the page is open.
    engine.send(ic_core::Command::SetQuiet(true));
    tokio::time::sleep(interval).await;
    let quiet_from = count("/v1/status/ApiListener");
    tokio::time::sleep(interval * 4).await;
    assert_eq!(count("/v1/status/ApiListener"), quiet_from);
    engine.send(ic_core::Command::SetQuiet(false));

    // Closed again: the polls go on alone.
    engine.send(ic_core::Command::WatchHealth(false));
    tokio::time::sleep(interval).await;
    let closed_at = count("/v1/status/ApiListener");
    let polls_at = count("/v1/status/CIB");
    assert!(crate::support::wait_until(|| count("/v1/status/CIB") >= polls_at + 3).await);
    assert_eq!(count("/v1/status/ApiListener"), closed_at);

    // Opening and closing in a hurry costs no more than the polls would.
    let before = count("/v1/status/ApiListener");
    for _ in 0..5 {
        engine.send(ic_core::Command::WatchHealth(true));
        engine.send(ic_core::Command::WatchHealth(false));
    }
    tokio::time::sleep(interval / 2).await;
    assert!(count("/v1/status/ApiListener") <= before + 1);
    engine.shutdown();
}
