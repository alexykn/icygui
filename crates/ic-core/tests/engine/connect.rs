//! Connecting: the tiered initial load and its progress, credentials from
//! the secret store, client certificates, TLS failures with the presented
//! certificate, reconnect backoff, and shutdown.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers fail the test loudly"
)]

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use crate::support::{
    ENV_ID, FakeSecrets, NOW, PASSWORD, environment, mock, start, start_for, tuning, wait_until,
};
use ic_api::Detail;
use ic_config::AuthConfig;
use ic_core::{ClusterView, Command, ConnectedNode, ConnectionState, CoreEvent, LoadPhase, Tuning};
use ic_mock::{MockConfig, MockTls, MockUser, scenarios};
use ic_model::ServiceState;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[expect(clippy::too_many_lines, reason = "one end-to-end scenario")]
async fn the_initial_load_fills_in_tier_by_tier() {
    let server = mock(MockConfig::with_scenario(scenarios::prod_cluster())).await;
    let control = server.control();
    let mut engine = start_for(&server);

    let connected = engine.connected().await;
    let truth = control.status();
    assert_eq!(
        connected,
        ConnectionState::Connected {
            node: ConnectedNode {
                url: server.url(),
                url_index: 0,
                name: truth.node_name.clone(),
                zone: Some("master".to_owned()),
                view: ClusterView::Full,
                passed_over: Vec::new(),
            },
            version: engine_version(&engine),
            since: ic_model::Timestamp::from_unix_seconds(NOW),
        }
    );

    // Connecting, then each tier with progress, then connected.
    let states = engine.states();
    assert_eq!(states[0], ConnectionState::Connecting { attempt: 1 });
    let phases: Vec<LoadPhase> = states
        .iter()
        .filter_map(|state| match state {
            ConnectionState::Loading { phase, .. } => Some(*phase),
            _ => None,
        })
        .collect();
    let mut order = phases.clone();
    order.dedup();
    assert_eq!(
        order,
        [LoadPhase::Hosts, LoadPhase::Services, LoadPhase::Details]
    );
    assert!(states.contains(&ConnectionState::Loading {
        phase: LoadPhase::Hosts,
        done: 8,
        total: Some(8),
    }));
    let problems = control
        .services()
        .iter()
        .filter(|service| service.is_problem())
        .count();
    assert!(states.contains(&ConnectionState::Loading {
        phase: LoadPhase::Details,
        done: problems,
        total: Some(problems),
    }));

    // A snapshot after each tier: hosts first, services after.
    let snapshots: Vec<Arc<ic_core::snapshot::Snapshot>> = engine
        .seen
        .iter()
        .filter_map(|event| match event {
            CoreEvent::Snapshot(snapshot) => Some(Arc::clone(snapshot)),
            _ => None,
        })
        .collect();
    assert!(snapshots.len() >= 3, "{}", snapshots.len());
    assert!(!snapshots[0].hosts.is_empty());
    assert!(
        snapshots[0].services.is_empty(),
        "services come with tier 2"
    );
    let revisions: Vec<u64> = snapshots.iter().map(|snapshot| snapshot.revision).collect();
    assert!(
        revisions.windows(2).all(|pair| pair[0] < pair[1]),
        "{revisions:?}"
    );

    // The last one has everything the mock has.
    let snapshot = snapshots.last().unwrap();
    assert_eq!(snapshot.hosts.len(), control.hosts().len());
    assert_eq!(snapshot.services.len(), control.services().len());
    for host in control.hosts() {
        let stored = &snapshot.hosts[&host.name];
        assert_eq!(stored.state, host.state, "{}", host.name);
        assert_eq!(stored.check.acknowledgement, host.check.acknowledgement);
        assert_eq!(stored.check.downtime_depth, host.check.downtime_depth);
        assert!(stored.check.result.is_some() || stored.state == ic_model::HostState::Pending);
    }
    for service in control.services() {
        let stored = &snapshot.services[&service.key];
        assert_eq!(stored.state, service.state, "{}", service.key);
        assert_eq!(stored.check.state_type, service.check.state_type);
        assert_eq!(stored.check.check_command, service.check.check_command);
        assert_eq!(stored.groups, service.groups);
        if service.is_problem() {
            // Tier 3: problems come with output and links.
            assert_eq!(
                stored.check.output(),
                service.check.output(),
                "{}",
                service.key
            );
            assert_eq!(stored.links, service.links);
        } else {
            assert!(stored.check.result.is_none(), "OK services stay lean");
        }
    }
    let comments: usize = snapshot.comments.values().map(Vec::len).sum();
    assert_eq!(comments, control.comments().len());
    let downtimes: usize = snapshot.downtimes.values().map(Vec::len).sum();
    assert_eq!(downtimes, control.downtimes().len());
    assert_eq!(*snapshot.host_groups, control.host_groups());
    assert_eq!(*snapshot.service_groups, control.service_groups());
    assert_eq!(snapshot.dependencies.len(), control.dependencies().len());
    assert_eq!(snapshot.endpoints.len(), control.endpoints().len());
    assert_eq!(snapshot.status.as_ref().unwrap().node_name, truth.node_name);
    let summary = scenarios::prod_cluster().summary();
    assert_eq!(
        snapshot.overall.critical as usize,
        summary.services_critical
    );
    assert_eq!(snapshot.overall.down as usize, summary.hosts_down);
    assert!(snapshot.overall.worst_unhandled.is_some());
    assert_eq!(snapshot.last_event_at, None, "no event yet");

    // The API user's permissions were reported.
    assert!(engine.seen.iter().any(|event| matches!(
        event,
        CoreEvent::Permissions(info) if info.user == "root"
    )));

    // Gentle on Icinga: the services lean, never a filter, names in
    // batches of at most 200, one event stream with a unique queue.
    let requests = control.requests();
    let services_lean = Detail::Lean.service_attrs();
    assert!(requests.iter().any(|request| {
        request.path == "/v1/objects/services"
            && request.body.as_ref().is_some_and(|body| {
                body.get("services").is_none()
                    && body["attrs"].as_array().map(Vec::len) == Some(services_lean.len())
            })
    }));
    for request in &requests {
        let body = request.body.clone().unwrap_or_default();
        assert!(body.get("filter").is_none(), "{request:?}");
        for names in ["hosts", "services"] {
            if let Some(list) = body.get(names).and_then(|list| list.as_array()) {
                assert!(!list.is_empty() && list.len() <= 200, "{request:?}");
            }
        }
    }
    let streams: Vec<_> = requests
        .iter()
        .filter(|request| request.path == "/v1/events")
        .collect();
    assert_eq!(streams.len(), 1);
    let queue = streams[0].body.as_ref().unwrap()["queue"].as_str().unwrap();
    assert!(queue.starts_with("icygui-") && queue.len() > 20, "{queue}");
    assert_eq!(control.event_streams(), 1);
    engine.shutdown();
}

fn engine_version(engine: &crate::support::Engine) -> String {
    engine
        .seen
        .iter()
        .find_map(|event| match event {
            CoreEvent::Permissions(info) => Some(info.version.clone()),
            _ => None,
        })
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_missing_secret_waits_for_the_user() {
    let server = mock(MockConfig::default()).await;
    let secrets = Arc::new(FakeSecrets::default());
    let mut engine = start(environment(&server), Arc::clone(&secrets), tuning());
    engine
        .wait_state(|state| *state == ConnectionState::MissingSecret)
        .await;
    assert_eq!(server.control().requests().len(), 0, "nothing sent");

    secrets.put(ENV_ID, PASSWORD);
    engine.send(Command::Refresh);
    engine.connected().await;
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wrong_credentials_stop_retrying() {
    let server = mock(MockConfig::default()).await;
    let control = server.control();
    let secrets = FakeSecrets::with(ENV_ID, "wrong");
    let mut engine = start(environment(&server), Arc::clone(&secrets), tuning());
    let state = engine
        .wait_state(|state| matches!(state, ConnectionState::AuthFailed { .. }))
        .await;
    let ConnectionState::AuthFailed { message } = state else {
        unreachable!()
    };
    assert!(message.contains("unauthorized"), "{message}");
    // No automatic retry: several backoff periods pass without a request.
    let requests = control.requests().len();
    let quiet = tokio::time::timeout(Duration::from_millis(500), engine.next()).await;
    assert!(quiet.is_err(), "no event while waiting for the user");
    assert_eq!(control.requests().len(), requests);

    // The user fixes the password: `UpdateEnvironment` (even unchanged)
    // retries.
    secrets.put(ENV_ID, PASSWORD);
    engine.send(Command::UpdateEnvironment(environment(&server)));
    engine.connected().await;
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pin_mismatch_shows_both_fingerprints_and_the_certificate() {
    let server = mock(MockConfig::default()).await;
    let mut environment = environment(&server);
    let pinned = ic_config::format_fingerprint(&[0xAB; 32]);
    environment.urls[0].pinned_sha256 = Some(pinned.clone());
    let mut engine = start(environment, FakeSecrets::with(ENV_ID, PASSWORD), tuning());
    let state = engine
        .wait_state(|state| matches!(state, ConnectionState::TlsFailed { .. }))
        .await;
    let ConnectionState::TlsFailed {
        url,
        message,
        certificate,
    } = state
    else {
        unreachable!()
    };
    assert_eq!(url, server.url(), "the URL whose pin to set");
    assert!(message.contains(&pinned), "{message}");
    assert!(message.contains(&server.cert_fingerprint()), "{message}");
    let certificate = certificate.expect("the presented certificate");
    assert_eq!(certificate.sha256, server.cert_sha256());
    assert_eq!(certificate.fingerprint(), server.cert_fingerprint());

    // Trust on first use: pin the presented certificate.
    let mut trusted = crate::support::environment(&server);
    trusted.urls[0].pinned_sha256 = Some(certificate.fingerprint());
    engine.send(Command::UpdateEnvironment(trusted));
    engine.connected().await;
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_untrusted_certificate_is_offered_for_trust() {
    let server = mock(MockConfig::default()).await;
    let mut environment = environment(&server);
    environment.urls[0].pinned_sha256 = None;
    let mut engine = start(environment, FakeSecrets::with(ENV_ID, PASSWORD), tuning());
    let state = engine
        .wait_state(|state| matches!(state, ConnectionState::TlsFailed { .. }))
        .await;
    let ConnectionState::TlsFailed { certificate, .. } = state else {
        unreachable!()
    };
    assert_eq!(certificate.unwrap().sha256, server.cert_sha256());
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unusable_settings_are_misconfigured() {
    let server = mock(MockConfig::default()).await;
    let mut environment = environment(&server);
    environment.tls.ca_file = Some("/nonexistent/icinga-ca.crt".into());
    let mut engine = start(environment, FakeSecrets::with(ENV_ID, PASSWORD), tuning());
    let state = engine
        .wait_state(|state| matches!(state, ConnectionState::Misconfigured { .. }))
        .await;
    let ConnectionState::Misconfigured { message } = state else {
        unreachable!()
    };
    assert!(message.contains("/nonexistent/icinga-ca.crt"), "{message}");

    let mut environment = crate::support::environment(&server);
    environment.urls[0].url = "http://plain.example".to_owned();
    engine.send(Command::UpdateEnvironment(environment));
    engine
        .wait_state(|state| matches!(state, ConnectionState::Misconfigured { message } if message.contains("https")))
        .await;
    assert_eq!(server.control().requests().len(), 0);
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_certificates_and_the_ca_file_are_read() {
    let server = mock(MockConfig {
        tls: MockTls::CaSigned,
        users: vec![MockUser::new("agent", "", &["*"]).with_client_cn("icygui-agent")],
        ..MockConfig::with_scenario(scenarios::staging())
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let (cert, key) = server.client_certificate("icygui-agent").unwrap();
    let cert_path = dir.path().join("client.crt");
    let key_path = dir.path().join("client.key");
    let ca_path = dir.path().join("ca.crt");
    std::fs::write(&cert_path, cert).unwrap();
    std::fs::write(&key_path, key).unwrap();
    std::fs::write(&ca_path, server.ca_pem().unwrap()).unwrap();

    let mut environment = environment(&server);
    environment.auth = AuthConfig::ClientCertificate {
        cert_path,
        key_path,
    };
    environment.urls[0].pinned_sha256 = None;
    environment.tls.ca_file = Some(ca_path);
    // No secret needed for a client certificate.
    let secrets = Arc::new(FakeSecrets::default());
    let mut engine = start(environment, Arc::clone(&secrets), tuning());
    engine.connected().await;
    assert_eq!(secrets.reads(), 0);
    let snapshot = engine
        .snapshot(|snapshot| !snapshot.services.is_empty())
        .await;
    assert_eq!(snapshot.hosts.len(), 10);
    let warnings = snapshot
        .services
        .values()
        .filter(|service| service.state == ServiceState::Warning)
        .count();
    assert_eq!(warnings, 3);
    engine.shutdown();
}

/// The delay a `Reconnecting` state announces (the fake clock stands
/// still).
fn delay(state: &ConnectionState) -> Duration {
    let ConnectionState::Reconnecting { retry_at, .. } = state else {
        panic!("not reconnecting: {state:?}");
    };
    Duration::from_secs_f64(retry_at.as_unix_seconds() - NOW)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unreachable_servers_are_retried_with_growing_jittered_delays() {
    // A port nobody listens on any more.
    let server = mock(MockConfig::default()).await;
    let environment = environment(&server);
    server.shutdown().await;

    let mut engine = start(environment, FakeSecrets::with(ENV_ID, PASSWORD), tuning());
    let initial = tuning().backoff_initial;
    let max = tuning().backoff_max;
    let mut expected = initial;
    for attempt in 1..=6 {
        engine
            .wait_state(|state| *state == ConnectionState::Connecting { attempt })
            .await;
        let state = engine
            .wait_state(|state| matches!(state, ConnectionState::Reconnecting { .. }))
            .await;
        let ConnectionState::Reconnecting {
            attempt: next,
            error,
            ..
        } = &state
        else {
            unreachable!()
        };
        assert_eq!(*next, attempt + 1);
        assert!(error.contains("connection"), "{error}");
        let waited = delay(&state);
        assert!(
            waited >= expected / 2 && waited < expected,
            "attempt {attempt}: {waited:?} not in [{:?}, {expected:?})",
            expected / 2
        );
        expected = (expected * 2).min(max);
    }

    // `Refresh` retries right away.
    engine.send(Command::Refresh);
    engine
        .wait_state(|state| matches!(state, ConnectionState::Connecting { attempt: 7 }))
        .await;
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dropped_stream_reconnects_and_reloads() {
    let server = mock(MockConfig::with_scenario(scenarios::staging())).await;
    let control = server.control();
    // Healthy never: consecutive drops grow the delay.
    let mut engine = start(
        environment(&server),
        FakeSecrets::with(ENV_ID, PASSWORD),
        Tuning {
            healthy_after: Duration::from_hours(1),
            ..tuning()
        },
    );
    engine.connected().await;
    let loads = |control: &ic_mock::MockControl| {
        control
            .requests()
            .iter()
            .filter(|request| {
                request.path == "/v1/objects/hosts"
                    && request
                        .body
                        .as_ref()
                        .is_some_and(|body| body.get("hosts").is_none())
            })
            .count()
    };
    assert_eq!(loads(&control), 1);

    // Behind the client's back while the stream is down.
    control.drop_event_streams();
    let first = engine
        .wait_state(|state| matches!(state, ConnectionState::Reconnecting { .. }))
        .await;
    assert!(
        matches!(first, ConnectionState::Reconnecting { attempt: 2, .. }),
        "{first:?}"
    );
    let initial = tuning().backoff_initial;
    assert!(delay(&first) < initial);
    control
        .set_service_state(
            "stg-web-01",
            "http",
            ServiceState::Critical,
            "CRITICAL - 503",
            true,
        )
        .unwrap();
    engine.connected().await;
    let snapshot = engine
        .snapshot(|snapshot| {
            snapshot
                .services
                .get(&ic_model::ServiceKey::new("stg-web-01", "http"))
                .is_some_and(|service| service.state == ServiceState::Critical)
        })
        .await;
    assert_eq!(loads(&control), 2, "the reconnect reloaded");
    assert_eq!(snapshot.hosts.len(), 10);

    // Dropped again soon: the next delay doubles.
    assert!(
        control
            .wait_for_event_streams(1, Duration::from_secs(5))
            .await
    );
    control.drop_event_streams();
    let second = engine
        .wait_state(|state| matches!(state, ConnectionState::Reconnecting { .. }))
        .await;
    assert!(
        matches!(second, ConnectionState::Reconnecting { attempt: 3, .. }),
        "{second:?}"
    );
    assert!(delay(&second) >= initial && delay(&second) < initial * 2);
    engine.connected().await;
    engine.shutdown();

    // After a healthy connection the backoff starts over.
    let mut engine = start(
        environment(&server),
        FakeSecrets::with(ENV_ID, PASSWORD),
        Tuning {
            healthy_after: Duration::ZERO,
            ..tuning()
        },
    );
    engine.connected().await;
    for _ in 0..2 {
        assert!(
            control
                .wait_for_event_streams(1, Duration::from_secs(5))
                .await
        );
        control.drop_event_streams();
        let state = engine
            .wait_state(|state| matches!(state, ConnectionState::Reconnecting { .. }))
            .await;
        assert!(
            matches!(state, ConnectionState::Reconnecting { attempt: 2, .. }),
            "{state:?}"
        );
        engine.connected().await;
    }
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_closes_the_stream_and_joins() {
    let server = mock(MockConfig::default()).await;
    let control = server.control();
    let mut engine = start_for(&server);
    engine.connected().await;
    assert_eq!(control.event_streams(), 1);
    let took = engine.shutdown();
    assert!(took < Duration::from_secs(5), "{took:?}");
    assert!(
        wait_until(|| control.event_streams() == 0).await,
        "the event stream was closed"
    );

    // Dropping the handle stops the engine too.
    let mut engine = start_for(&server);
    engine.connected().await;
    assert!(wait_until(|| control.event_streams() == 1).await);
    drop(engine.handle.take());
    assert!(wait_until(|| control.event_streams() == 0).await);
    // The event channel ends once the engine is gone.
    let rest: Vec<CoreEvent> = tokio::time::timeout(
        crate::support::WAIT,
        futures::StreamExt::collect(&mut engine.events),
    )
    .await
    .unwrap();
    drop(rest);
}

/// Polls `condition` for up to five seconds.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restart_found_by_the_status_poll_reloads() {
    let server = mock(MockConfig::with_scenario(scenarios::lab())).await;
    let control = server.control();
    let mut engine = start(
        environment(&server),
        FakeSecrets::with(ENV_ID, PASSWORD),
        Tuning {
            status_interval: Duration::from_millis(100),
            ..tuning()
        },
    );
    engine.connected().await;
    let hosts_loads = || {
        control
            .requests()
            .iter()
            .filter(|request| {
                request.path == "/v1/objects/hosts"
                    && request
                        .body
                        .as_ref()
                        .is_some_and(|body| body.get("hosts").is_none())
            })
            .count()
    };
    // Polls alone don't reload.
    assert!(
        wait_until(|| {
            control
                .requests()
                .iter()
                .filter(|request| request.path == "/v1/status/IcingaApplication")
                .count()
                >= 4
        })
        .await
    );
    assert_eq!(hosts_loads(), 1);

    let restarted = control.now();
    control.set_program_start(restarted);
    engine
        .snapshot(|snapshot| {
            snapshot.status.as_ref().is_some_and(|status| {
                (status.program_start.as_unix_seconds() - restarted.as_unix_seconds()).abs() < 1e-3
            })
        })
        .await;
    assert!(wait_until(|| hosts_loads() == 2).await, "reloaded once");
    let states: BTreeSet<String> = engine
        .states()
        .iter()
        .skip_while(|state| !matches!(state, ConnectionState::Connected { .. }))
        .skip(1)
        .map(|state| format!("{state:?}"))
        .collect();
    assert!(
        states.is_empty(),
        "a reload doesn't touch the connection state: {states:?}"
    );
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refresh_reloads_while_connected() {
    let server = mock(MockConfig::with_scenario(scenarios::lab())).await;
    let control = server.control();
    let mut engine = start_for(&server);
    engine.connected().await;
    // Icinga's notifications load after the problem lists.
    engine
        .snapshot(|snapshot| !snapshot.icinga_notifications.is_empty())
        .await;
    control.clear_requests();
    engine.send(Command::Refresh);
    let paths = || -> BTreeSet<String> {
        control
            .requests()
            .into_iter()
            .map(|request| request.path)
            .collect()
    };
    // A refresh reloads the lists a connect loads; Icinga's notifications
    // stay, since the stream carries their events.
    assert!(
        wait_until(|| {
            let paths = paths();
            ["hosts", "services"]
                .iter()
                .all(|kind| paths.contains(&format!("/v1/objects/{kind}")))
        })
        .await,
        "{:?}",
        paths()
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    let paths = paths();
    assert!(
        !paths.contains("/v1/objects/notifications"),
        "kept current by events: {paths:?}"
    );
    assert!(!paths.contains("/v1/events"), "the stream stays");
    assert_eq!(control.event_streams(), 1);
    let after_connected: Vec<ConnectionState> = engine
        .states()
        .into_iter()
        .skip_while(|state| !matches!(state, ConnectionState::Connected { .. }))
        .skip(1)
        .collect();
    assert!(after_connected.is_empty(), "{after_connected:?}");
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn another_server_starts_from_scratch() {
    let lab = mock(MockConfig::with_scenario(scenarios::lab())).await;
    let staging = mock(MockConfig::with_scenario(scenarios::staging())).await;
    let mut engine = start_for(&lab);
    engine.connected().await;
    assert_eq!(engine.latest().unwrap().hosts.len(), 2);

    let mut moved = environment(&staging);
    moved.name = "renamed".to_owned();
    engine.send(Command::UpdateEnvironment(moved));
    // The old server's objects are gone at once, then the new ones load.
    engine.snapshot(|snapshot| snapshot.hosts.is_empty()).await;
    engine
        .wait_state(|state| *state == ConnectionState::Connecting { attempt: 1 })
        .await;
    engine.connected().await;
    let snapshot = engine.latest().unwrap();
    assert_eq!(snapshot.hosts.len(), 10);
    assert_eq!(lab.control().event_streams(), 0, "the old stream is closed");

    // Settings that don't touch the connection apply in place.
    let mut renamed = environment(&staging);
    renamed.author = Some("someone else".to_owned());
    let streams = staging
        .control()
        .requests()
        .iter()
        .filter(|r| r.path == "/v1/events")
        .count();
    engine.send(Command::UpdateEnvironment(renamed));
    engine.send(Command::Refresh);
    engine
        .snapshot(|latest| latest.revision > snapshot.revision)
        .await;
    assert_eq!(
        staging
            .control()
            .requests()
            .iter()
            .filter(|r| r.path == "/v1/events")
            .count(),
        streams,
        "no reconnect"
    );
    engine.shutdown();
}
