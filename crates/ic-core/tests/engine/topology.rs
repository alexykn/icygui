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

use crate::support::{ENV_ID, Engine, FakeSecrets, Launch, PASSWORD, USER, environment, mock};
use ic_config::{ApiUrl, AuthConfig, Environment};
use ic_core::{ClusterView, ConnectedNode, ConnectionState, Tuning, test_connection};
use ic_mock::{MockConfig, MockServer, MockTls, MockUser, Scenario, scenarios};
use ic_model::Endpoint;
use secrecy::SecretString;

/// The `prod-cluster` scenario with a second master in the top-level zone
/// (an HA pair); its satellite `sat-ams-01` is in the child zone `ams`.
fn cluster() -> Scenario {
    let mut cluster = scenarios::prod_cluster();
    cluster.endpoints.push(Endpoint {
        name: "master-02".to_owned(),
        zone: "master".to_owned(),
        connected: true,
    });
    cluster
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
