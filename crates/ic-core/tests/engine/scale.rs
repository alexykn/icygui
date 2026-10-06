//! The engine at production scale against `ic-mock`'s `large` scenario
//! (2 000 hosts × 15 services, docs/performance.md): the tiered initial
//! load until the problem lists are complete, and a burst of every object
//! re-checked at Icinga's pace (about 5 000 events/s) until the snapshot
//! matches Icinga again. Icinga's 32 000 `Notification` objects load in
//! the background after the problem lists. And the steady state: the
//! simulator checking every object at its interval (about 110 events/s)
//! while the engine's threads' CPU time is measured.
//!
//! And quiet mode (PERF-09): the same engine live, then quiet, then
//! awake again, with what its stream carries, the requests it sends and
//! its CPU, and how fast an opened object fills in.
//!
//! Ignored by default (slow in unoptimised builds); prints its timings:
//! `cargo test -p ic-core --test engine scale -- --ignored --nocapture
//! --test-threads 1`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers fail the test loudly"
)]

use std::time::{Duration, Instant};

use ic_core::{ConnectionState, CoreEvent, LoadPhase};
use ic_mock::{MockConfig, scenarios};

use crate::support::{mock, start_for};

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
    // PERF-01: the problem lists are complete within 5 s (release builds;
    // the nightly `perf` workflow runs this with `--release`).
    if !cfg!(debug_assertions) {
        assert!(loaded < Duration::from_secs(5), "PERF-01: {loaded:?}");
    }
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
    // Icinga's notifications follow in the background.
    let snapshot = engine
        .snapshot(|snapshot| !snapshot.icinga_notifications.is_empty())
        .await;
    let count: usize = snapshot
        .icinga_notifications
        .values()
        .map(|list| list.len())
        .sum();
    eprintln!(
        "Icinga's {count} notifications loaded after {:?}",
        started.elapsed()
    );
    assert_eq!(count, 32_000);

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

/// The steady state's CPU time, read from Linux's `/proc`.
#[cfg(target_os = "linux")]
mod steady {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use futures::StreamExt;
    use ic_core::snapshot::Snapshot;
    use ic_core::{Command, CoreEvent, Tuning};
    use ic_mock::{MockConfig, SimulationConfig, scenarios};
    use ic_model::{ObjectKey, ServiceState};

    use crate::support::{ENV_ID, FakeSecrets, PASSWORD, environment, mock, start};

    /// The CPU time (seconds) of each of the engine's threads (`icygui-core`,
    /// its runtime workers and blocking threads), by thread id, from `/proc`.
    fn engine_threads_cpu(latest: &mut HashMap<u32, f64>) {
        let Ok(tasks) = std::fs::read_dir("/proc/self/task") else {
            return;
        };
        for task in tasks.flatten() {
            let path = task.path();
            let Ok(name) = std::fs::read_to_string(path.join("comm")) else {
                continue;
            };
            if !name.trim().starts_with("icygui-core") {
                continue;
            }
            let Some(tid) = task.file_name().to_str().and_then(|id| id.parse().ok()) else {
                continue;
            };
            // Nanoseconds on the CPU; else user + system time in clock ticks.
            let seconds = std::fs::read_to_string(path.join("schedstat"))
                .ok()
                .and_then(|text| text.split_whitespace().next()?.parse::<f64>().ok())
                .map(|nanos| nanos / 1e9)
                .filter(|seconds| *seconds > 0.0)
                .or_else(|| {
                    let stat = std::fs::read_to_string(path.join("stat")).ok()?;
                    let fields: Vec<&str> = stat.rsplit_once(')')?.1.split_whitespace().collect();
                    let user: f64 = fields.get(11)?.parse().ok()?;
                    let system: f64 = fields.get(12)?.parse().ok()?;
                    Some((user + system) / 100.0)
                });
            if let Some(seconds) = seconds {
                latest.insert(tid, seconds);
            }
        }
    }

    /// Takes the engine's events for `how_long`, keeping the latest snapshot in
    /// `held` (the UI holds one, so copy on write applies). Returns how many
    /// snapshots came.
    async fn drain(
        engine: &mut crate::support::Engine,
        how_long: Duration,
        held: &mut Option<Arc<Snapshot>>,
    ) -> u32 {
        let until = tokio::time::Instant::now() + how_long;
        let mut snapshots = 0;
        while let Ok(Some(event)) = tokio::time::timeout_at(until, engine.events.next()).await {
            if let CoreEvent::Snapshot(snapshot) = event {
                *held = Some(snapshot);
                snapshots += 1;
            }
        }
        snapshots
    }

    /// PERF-03's steady state: every object of the `large` scenario checked at
    /// its interval (about 110 `CheckResult` events per second), the three
    /// default dashboards, a snapshot held like the UI holds one, production
    /// timing. Prints the engine threads' share of one core.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "production size, a minute; run with --ignored --nocapture"]
    async fn steady_state_cpu_at_production_scale() {
        let server = mock(MockConfig {
            event_buffer: 100_000,
            simulation: SimulationConfig {
                // Checks only: incidents would add bursts.
                problems_per_hour: 0.0,
                flapping: false,
                outages: false,
                downtimes: false,
                ..SimulationConfig::running(7)
            },
            ..MockConfig::with_scenario(scenarios::large(42))
        })
        .await;
        let control = server.control();
        let mut engine = start(
            environment(&server),
            FakeSecrets::with(ENV_ID, PASSWORD),
            Tuning::default(),
        );
        engine.connected().await;
        engine
            .snapshot(|snapshot| !snapshot.icinga_notifications.is_empty())
            .await;
        let mut held: Option<Arc<Snapshot>> = engine.latest();
        // Warm up: the first dashboard evaluations, the notification list.
        drain(&mut engine, Duration::from_secs(10), &mut held).await;
        let window = Duration::from_secs(40);
        let mut first = HashMap::new();
        engine_threads_cpu(&mut first);
        let mut latest = first.clone();
        let started = Instant::now();
        let mut snapshots = 0;
        while started.elapsed() < window {
            snapshots += drain(&mut engine, Duration::from_secs(1), &mut held).await;
            engine_threads_cpu(&mut latest);
        }
        let elapsed = started.elapsed().as_secs_f64();
        let cpu: f64 = latest
            .iter()
            .map(|(tid, seconds)| seconds - first.get(tid).copied().unwrap_or(0.0))
            .sum();
        let rate = control.status().checks_per_minute / 60.0;
        let share = cpu / elapsed * 100.0;
        eprintln!(
            "steady state: {rate:.0} checks/s, {snapshots} snapshots in {elapsed:.1} s, \
             engine threads {cpu:.2} s CPU = {share:.1} % of one core ({})",
            if cfg!(debug_assertions) {
                "dev profile"
            } else {
                "release"
            }
        );
        assert!(rate > 80.0, "the simulator checks at Icinga's pace: {rate}");
        assert!(held.is_some());
        // Measured 5.2 % with unoptimised workspace crates (dev profile);
        // the release budget (PERF-03) is 5 %, checked by the nightly
        // `perf` workflow's release build.
        assert!(share < 25.0, "{share:.1} %");
        if !cfg!(debug_assertions) {
            assert!(share < 5.0, "PERF-03: {share:.1} % of one core");
        }
        engine.shutdown();
    }

    /// What one window of the engine costs: the stream's lines and bytes,
    /// requests to the mock (event streams aside), the engine threads' CPU
    /// time.
    struct Window {
        seconds: f64,
        lines: u64,
        bytes: u64,
        requests: usize,
        cpu: f64,
    }

    impl Window {
        #[expect(
            clippy::cast_precision_loss,
            reason = "counts of a minute's events, far below 2^52"
        )]
        fn report(&self, what: &str) -> String {
            format!(
                "{what}: {:.1} events/s, {:.1} KB/s on the stream, {:.1} requests/min, \
                 engine {:.2} % of one core",
                self.lines as f64 / self.seconds,
                self.bytes as f64 / self.seconds / 1_000.0,
                self.requests as f64 / self.seconds * 60.0,
                self.cpu / self.seconds * 100.0,
            )
        }
    }

    /// Measures `how_long` of whatever the engine does now.
    async fn measure(
        engine: &mut crate::support::Engine,
        control: &ic_mock::MockControl,
        how_long: Duration,
        held: &mut Option<Arc<Snapshot>>,
    ) -> Window {
        let (lines, bytes) = control.events_delivered();
        control.clear_requests();
        let mut first = HashMap::new();
        engine_threads_cpu(&mut first);
        let mut latest = first.clone();
        let started = Instant::now();
        while started.elapsed() < how_long {
            drain(engine, Duration::from_secs(1), held).await;
            engine_threads_cpu(&mut latest);
        }
        let seconds = started.elapsed().as_secs_f64();
        let (lines_after, bytes_after) = control.events_delivered();
        Window {
            seconds,
            lines: lines_after - lines,
            bytes: bytes_after - bytes,
            requests: control
                .requests()
                .iter()
                .filter(|request| request.path != "/v1/events")
                .count(),
            cpu: latest
                .iter()
                .map(|(tid, seconds)| seconds - first.get(tid).copied().unwrap_or(0.0))
                .sum(),
        }
    }

    /// PERF-09: quiet versus live at production scale. The `large`
    /// scenario with the simulator checking every object at its interval
    /// (about 100 check results per second) and two new problems a minute,
    /// production timing; one engine live for a window, then quiet for a
    /// window: what its stream carries, the requests it sends, its CPU.
    /// Then waking up: the time until the full stream is back, and the
    /// time from opening an object (`Command::Focus`) to its filled pane,
    /// for an object a notification prefetched and one never loaded in
    /// full.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "production size, two minutes; run with --ignored --nocapture"]
    #[expect(clippy::too_many_lines, reason = "one measurement, step by step")]
    #[expect(
        clippy::cast_precision_loss,
        reason = "byte counts of a minute's stream, far below 2^52"
    )]
    async fn quiet_versus_live_at_production_scale() {
        let server = mock(MockConfig {
            event_buffer: 100_000,
            simulation: SimulationConfig {
                problems_per_hour: 120.0,
                ..SimulationConfig::running(11)
            },
            ..MockConfig::with_scenario(scenarios::large(42))
        })
        .await;
        let control = server.control();
        let mut engine = start(
            environment(&server),
            FakeSecrets::with(ENV_ID, PASSWORD),
            Tuning::default(),
        );
        engine.connected().await;
        engine
            .snapshot(|snapshot| !snapshot.icinga_notifications.is_empty())
            .await;
        let mut held: Option<Arc<Snapshot>> = engine.latest();
        drain(&mut engine, Duration::from_secs(10), &mut held).await;
        let window = Duration::from_secs(40);

        let live = measure(&mut engine, &control, window, &mut held).await;
        eprintln!("{}", live.report("live "));
        engine.send(Command::SetQuiet(true));
        let switched = Instant::now();
        while !control
            .event_stream_stats()
            .iter()
            .all(|stream| !stream.types.contains(&"CheckResult"))
            || control.event_streams() != 1
        {
            drain(&mut engine, Duration::from_millis(50), &mut held).await;
        }
        eprintln!("quiet stream in {:?}", switched.elapsed());
        drain(&mut engine, Duration::from_secs(5), &mut held).await;
        let quiet = measure(&mut engine, &control, window, &mut held).await;
        eprintln!("{}", quiet.report("quiet"));
        let fewer = 100.0 * (1.0 - quiet.bytes as f64 / live.bytes.max(1) as f64);
        let cpu = 100.0 * (1.0 - quiet.cpu / live.cpu.max(f64::EPSILON));
        eprintln!("quiet: {fewer:.1} % fewer stream bytes, {cpu:.0} % less engine CPU");

        // A problem notified while quiet: prefetched, so opening it after
        // waking up needs no request.
        let snapshot = held.clone().unwrap();
        let mut lean = snapshot
            .services
            .values()
            .filter(|service| service.state == ServiceState::Ok && service.check.result.is_none())
            .map(|service| service.key.clone());
        let notified = lean.next().unwrap();
        let opened = lean.next().unwrap();
        control
            .set_service_state(
                &notified.host.to_string(),
                &notified.name,
                ServiceState::Critical,
                "CRITICAL - measured",
                true,
            )
            .unwrap();
        let mut events = 0;
        let notified_at = Instant::now();
        loop {
            let event = engine.next().await;
            events += 1;
            if let CoreEvent::Notification(record) = &event
                && record.intent.object == Some(ObjectKey::from(notified.clone()))
            {
                break;
            }
            assert!(events < 1_000_000);
        }
        eprintln!(
            "the notification while quiet came {:?} after the state change",
            notified_at.elapsed()
        );
        // The prefetch.
        let snapshot = engine
            .snapshot(|snapshot| {
                let key = ObjectKey::from(notified.clone());
                !snapshot.is_updating(&key)
                    && snapshot.services[&notified]
                        .check
                        .result
                        .as_ref()
                        .is_some_and(|result| result.output == "CRITICAL - measured")
            })
            .await;
        held = Some(snapshot);

        // Waking up, and opening the notified object (a click).
        let woke = Instant::now();
        engine.send(Command::SetQuiet(false));
        engine.send(Command::Focus(ObjectKey::from(notified.clone())));
        let snapshot = engine
            .snapshot(|snapshot| !snapshot.is_updating(&ObjectKey::from(notified.clone())))
            .await;
        eprintln!(
            "notification click to a filled pane: {:?} (prefetched: {})",
            woke.elapsed(),
            snapshot.services[&notified].check.result.is_some()
        );
        // An object never loaded in full.
        let focused = Instant::now();
        engine.send(Command::Focus(ObjectKey::from(opened.clone())));
        engine
            .snapshot(|snapshot| snapshot.services[&opened].check.result.is_some())
            .await;
        eprintln!(
            "opening an object never loaded in full: filled after {:?}",
            focused.elapsed()
        );
        while !control
            .event_stream_stats()
            .iter()
            .all(|stream| stream.types.contains(&"CheckResult"))
            || control.event_streams() != 1
        {
            drain(&mut engine, Duration::from_millis(20), &mut held).await;
        }
        eprintln!(
            "the full stream was back {:?} after waking up",
            woke.elapsed()
        );
        drain(&mut engine, Duration::from_secs(5), &mut held).await;
        let awake = measure(&mut engine, &control, Duration::from_secs(20), &mut held).await;
        eprintln!("{}", awake.report("awake"));
        assert!(
            fewer > 90.0,
            "quiet mode drops at least 90 % of the stream: {fewer:.1} %"
        );
        assert!(quiet.requests <= live.requests);
        if !cfg!(debug_assertions) {
            assert!(quiet.cpu < live.cpu);
        }
        engine.shutdown();
    }
}
