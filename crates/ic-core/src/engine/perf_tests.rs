//! Throughput of the applier (docs/performance.md: 50 000 recorded events
//! in under 3 s in release builds, no event dropped). The events are
//! recorded from `ic-mock` bursts (every object re-checked at Icinga's
//! pace), then parsed, collapsed and applied in batches like the engine
//! does, with a snapshot per batch (the copy-on-write cost included).
//!
//! The small run is part of the normal test suite with a bound for
//! unoptimised builds; the production-size run is ignored:
//! `cargo test -p ic-core --lib perf -- --ignored --nocapture`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use ic_api::{Client, ConnectionSettings, Credentials, Detail, TlsSettings, Url};
use ic_mock::{MockConfig, MockServer, scenarios};
use ic_model::{EventKind, Timestamp};
use secrecy::SecretString;

use super::stream;
use crate::store::Store;

/// Records the raw event lines of `bursts` bursts of `hosts` × 15 services.
async fn record(hosts: usize, bursts: usize) -> (MockServer, Vec<Vec<u8>>) {
    let server = MockServer::start(MockConfig {
        check_rate: 20_000.0,
        event_buffer: 200_000,
        ..MockConfig::with_scenario(scenarios::large_with_hosts(hosts, 11))
    })
    .await
    .unwrap();
    let client = Client::new(ConnectionSettings::new(
        Url::parse(&server.url()).unwrap(),
        Credentials::Basic {
            username: "root".to_owned(),
            password: SecretString::from("icinga".to_owned()),
        },
        TlsSettings {
            pinned_sha256: Some(server.cert_sha256()),
            ..TlsSettings::default()
        },
    ))
    .unwrap();
    let mut lines = client
        .events("perf", &EventKind::ALL)
        .await
        .unwrap()
        .into_lines();
    let control = server.control();
    let mut recorded = Vec::new();
    for _ in 0..bursts {
        control.burst();
        loop {
            match tokio::time::timeout(Duration::from_millis(500), lines.next()).await {
                Ok(Some(Ok(line))) => recorded.push(line),
                Ok(other) => panic!("the stream ended: {other:?}"),
                Err(_) if control.queued_checks() == 0 => break,
                Err(_) => {}
            }
        }
    }
    (server, recorded)
}

/// Applies `lines` to a store holding the mock's objects; returns the time
/// it took and how many events were applied.
fn apply(server: &MockServer, lines: Vec<Vec<u8>>, batch: usize) -> (Duration, usize) {
    let control = server.control();
    let mut store = Store::default();
    store.replace_hosts(control.hosts(), 0);
    store.replace_services(control.services(), Detail::Lean, 0);
    store.take_changes();
    // The snapshot the UI holds, so every batch copies on write.
    let mut held = store.snapshot(0, Timestamp::EPOCH, Arc::default());
    let numbered: Vec<(u64, Vec<u8>)> = (1..).zip(lines).collect();
    let started = Instant::now();
    let mut applied = 0;
    for (index, chunk) in numbered.chunks(batch).enumerate() {
        let events = stream::prepare(chunk.to_vec());
        for (seq, event) in events {
            if matches!(
                store.apply(seq, &event),
                crate::store::Applied::Changed { .. }
            ) {
                applied += 1;
            }
        }
        store.take_changes();
        held = store.snapshot(
            u64::try_from(index).unwrap_or(u64::MAX),
            Timestamp::EPOCH,
            Arc::default(),
        );
    }
    let took = started.elapsed();
    drop(held);
    // Nothing was lost: the store matches the mock.
    for truth in control.services() {
        let stored = store.check(&truth.object_key()).unwrap();
        assert_eq!(stored.output(), truth.check.output(), "{}", truth.key);
        assert_eq!(stored.state_type, truth.check.state_type, "{}", truth.key);
    }
    (took, applied)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_applier_keeps_up_with_bursts() {
    let (server, lines) = record(200, 2).await;
    let count = lines.len();
    assert!(count >= 6_400, "{count} lines");
    let (took, applied) = apply(&server, lines, 5_000);
    eprintln!("applied {applied} of {count} recorded events (3 000 services) in {took:?}");
    // Release: well under a second. Unoptimised workspace crates are
    // several times slower; the bound only catches pathologies.
    assert!(took < Duration::from_secs(10), "{took:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "production size; run with --ignored --nocapture"]
async fn fifty_thousand_events_at_production_scale() {
    let (server, lines) = record(2_000, 2).await;
    let count = lines.len();
    assert!(count >= 50_000, "{count} lines");
    let lines: Vec<Vec<u8>> = lines.into_iter().take(64_000).collect();
    let count = lines.len();
    let (took, applied) = apply(&server, lines, 5_000);
    eprintln!("applied {applied} of {count} recorded events (30 000 services) in {took:?}");
    let per_second = f64::from(u32::try_from(count).unwrap_or(u32::MAX)) / took.as_secs_f64();
    eprintln!("{per_second:.0} events/s");
}
