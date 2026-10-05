//! The engine at production scale against `ic-mock`'s `large` scenario
//! (2 000 hosts × 15 services, docs/performance.md): the tiered initial
//! load until the problem lists are complete, and a burst of every object
//! re-checked at Icinga's pace (about 5 000 events/s) until the snapshot
//! matches Icinga again.
//!
//! Ignored by default (slow in unoptimised builds); prints its timings:
//! `cargo test -p ic-core --test scale -- --ignored --nocapture`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers fail the test loudly"
)]

mod support;

use std::time::{Duration, Instant};

use ic_core::{ConnectionState, CoreEvent, LoadPhase};
use ic_mock::{MockConfig, scenarios};
use support::{mock, start_for};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "production size; run with --ignored --nocapture"]
async fn production_scale_load_and_burst() {
    let server = mock(MockConfig {
        event_buffer: 100_000,
        ..MockConfig::with_scenario(scenarios::large(42))
    })
    .await;
    let control = server.control();
    let started = Instant::now();
    let mut engine = start_for(&server);
    let mut marks = Vec::new();
    loop {
        let event = engine.next().await;
        match event {
            CoreEvent::Connection(ConnectionState::Loading { phase, done: 0, .. }) => {
                marks.push((format!("{phase:?} started"), started.elapsed()));
            }
            CoreEvent::Snapshot(snapshot) => {
                marks.push((
                    format!(
                        "snapshot r{}: {} hosts, {} services",
                        snapshot.revision,
                        snapshot.hosts.len(),
                        snapshot.services.len()
                    ),
                    started.elapsed(),
                ));
            }
            CoreEvent::Connection(ConnectionState::Connected { .. }) => break,
            _ => {}
        }
    }
    let loaded = started.elapsed();
    for (what, at) in &marks {
        eprintln!("{at:>12.3?}  {what}");
    }
    eprintln!("initial load complete (problem details included) after {loaded:?}");
    assert!(engine.states().iter().any(|state| matches!(
        state,
        ConnectionState::Loading {
            phase: LoadPhase::Details,
            ..
        }
    )));
    let snapshot = engine.latest().unwrap();
    assert_eq!(snapshot.services.len(), 30_000);
    let problems = snapshot
        .services
        .values()
        .filter(|s| s.is_problem())
        .count();
    let with_output = snapshot
        .services
        .values()
        .filter(|s| s.is_problem() && s.check.result.is_some())
        .count();
    assert_eq!(problems, with_output, "every problem has its details");
    eprintln!("{problems} problems with details");

    let burst = Instant::now();
    let queued = control.burst();
    let snapshot = engine
        .snapshot(|snapshot| {
            snapshot
                .services
                .values()
                .all(|service| service.check.result.is_some())
        })
        .await;
    let absorbed = burst.elapsed();
    eprintln!(
        "burst of {queued} checks absorbed after {absorbed:?} (Icinga needs ~6.5 s to run them)"
    );
    for truth in control.services() {
        assert_eq!(snapshot.services[&truth.key].state, truth.state);
    }
    assert!(absorbed < Duration::from_mins(1));
    engine.shutdown();
}
