//! Many clients starting at once against the local Docker Icinga from
//! `contract/scale/starts.sh` (PERF-09): N engines (20 by default) start
//! together, each with its real tiered load and event stream, either all
//! at once (users opening the app) or as background starts (launch at
//! login, each waiting its size-proportional random delay). Prints when
//! each was connected and the master's memory (sampled with `docker
//! stats`), to calibrate the pacing factors in docs/performance.md.
//!
//! Ignored, and a no-op without `ICYGUI_SCALE_URL`. Like the contract
//! tests it refuses anything but the local container before a single
//! request: the URL must point to this machine and
//! `ICYGUI_SCALE_CONTAINER` must name a running Docker container. Never
//! point it at a production Icinga.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers fail the test loudly"
)]

use std::process::Command as Process;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::StreamExt;
use ic_config::{AuthConfig, Environment, General};
use ic_core::{ConnectionState, CoreEvent, Start, Tuning};

use crate::support::{Engine, FakeSecrets, Launch};

/// How long a client may take for anything here: many clients pulling the
/// big lists at once on one machine can be silent for a while.
const LONG: Duration = Duration::from_mins(10);

/// Waits for an event `done` accepts (keeping every event, like the
/// helpers' `next`, but with a long patience).
async fn wait(engine: &mut Engine, mut done: impl FnMut(&CoreEvent) -> bool) {
    loop {
        let event = tokio::time::timeout(LONG, engine.events.next())
            .await
            .expect("no event for ten minutes")
            .expect("the engine stopped");
        let finished = done(&event);
        engine.seen.push(event);
        if finished {
            return;
        }
    }
}

/// The master's memory in MiB, from `docker stats`.
fn memory_mib(container: &str) -> Option<f64> {
    let output = Process::new("docker")
        .args([
            "stats",
            "--no-stream",
            "--format",
            "{{.MemUsage}}",
            container,
        ])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let used = text.split('/').next()?.trim();
    let (number, unit) = used.split_at(used.find(|c: char| c.is_ascii_alphabetic())?);
    let number: f64 = number.trim().parse().ok()?;
    Some(match unit {
        "GiB" => number * 1_024.0,
        "KiB" => number / 1_024.0,
        "B" => number / 1_048_576.0,
        _ => number,
    })
}

/// Samples the container's memory every half second until stopped: the
/// peak.
fn sample_memory(container: String, stop: Arc<AtomicBool>) -> std::thread::JoinHandle<f64> {
    std::thread::spawn(move || {
        let mut peak: f64 = 0.0;
        while !stop.load(Ordering::SeqCst) {
            if let Some(mib) = memory_mib(&container) {
                peak = peak.max(mib);
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        peak
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs contract/scale/starts.sh's local Docker Icinga"]
#[expect(clippy::too_many_lines, reason = "one measurement, step by step")]
#[expect(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "percentiles of a few dozen clients"
)]
async fn many_clients_start_at_once() {
    let Ok(url) = std::env::var("ICYGUI_SCALE_URL") else {
        eprintln!("ICYGUI_SCALE_URL isn't set: nothing to measure");
        return;
    };
    let container = std::env::var("ICYGUI_SCALE_CONTAINER").expect("ICYGUI_SCALE_CONTAINER");
    // Only the local container (see the module notes).
    let parsed = ic_api::Url::parse(&url).expect("ICYGUI_SCALE_URL");
    assert!(
        matches!(parsed.host_str(), Some("127.0.0.1" | "localhost" | "[::1]")),
        "the scale measurement only runs against the local Docker Icinga"
    );
    let running = Process::new("docker")
        .args(["inspect", "-f", "{{.State.Running}}", &container])
        .output()
        .expect("docker");
    assert_eq!(
        String::from_utf8_lossy(&running.stdout).trim(),
        "true",
        "the container {container} must be running"
    );
    let clients: usize = std::env::var("ICYGUI_SCALE_CLIENTS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(20);
    let background = std::env::var("ICYGUI_SCALE_START").as_deref() == Ok("background");
    let user = std::env::var("ICYGUI_SCALE_USER").unwrap_or_else(|_| "icygui".to_owned());
    let password =
        std::env::var("ICYGUI_SCALE_PASSWORD").unwrap_or_else(|_| "icygui-test".to_owned());

    let certificate = ic_core::fetch_certificate(url.clone(), None)
        .await
        .unwrap()
        .expect("the container's certificate");
    let pin = ic_api::format_fingerprint(&certificate.sha256);
    let baseline = memory_mib(&container).unwrap_or_default();
    let stop = Arc::new(AtomicBool::new(false));
    let sampler = sample_memory(container.clone(), Arc::clone(&stop));

    let started = Instant::now();
    let mut engines = Vec::new();
    for index in 0..clients {
        let mut environment = Environment::new(
            &format!("scale-{index}"),
            &url,
            AuthConfig::Basic {
                username: user.clone(),
            },
        );
        environment.id = format!("00000000-0000-0000-0000-{index:012}");
        environment.tls.use_system_roots = false;
        environment.urls[0].pinned_sha256 = Some(pin.clone());
        let secrets = FakeSecrets::with(&environment.id, &password);
        let launch = Launch {
            environment,
            secrets,
            tuning: Tuning::default(),
            general: General::default(),
            now: ic_model::Timestamp::now().as_unix_seconds(),
            data_dir: None,
            start: if background {
                Start::Background
            } else {
                Start::User
            },
        };
        engines.push(launch.start());
    }
    let connected: Arc<Mutex<Vec<Duration>>> = Arc::default();
    let mut waits = Vec::new();
    for mut engine in engines {
        let connected = Arc::clone(&connected);
        waits.push(tokio::spawn(async move {
            wait(&mut engine, |event| {
                matches!(
                    event,
                    CoreEvent::Connection(ConnectionState::Connected { .. })
                )
            })
            .await;
            connected.lock().unwrap().push(started.elapsed());
            // The whole first load: Icinga's notifications come last.
            wait(&mut engine, |event| {
                matches!(event, CoreEvent::Snapshot(snapshot) if !snapshot.icinga_notifications.is_empty())
            })
            .await;
            let complete = started.elapsed();
            let services = engine.latest().unwrap().services.len();
            (engine, complete, services)
        }));
    }
    let mut complete = Vec::new();
    let mut services = 0;
    let mut engines = Vec::new();
    for wait in waits {
        let (engine, at, count) = wait.await.unwrap();
        complete.push(at);
        services = count;
        engines.push(engine);
    }
    let all_done = started.elapsed();
    // Live for a moment with every stream open.
    tokio::time::sleep(Duration::from_secs(5)).await;
    stop.store(true, Ordering::SeqCst);
    let peak = sampler.join().unwrap();
    for mut engine in engines {
        engine.shutdown();
    }
    let mut connected = connected.lock().unwrap().clone();
    connected.sort();
    complete.sort();
    let at = |list: &[Duration], share: f64| {
        let index = ((list.len() as f64 - 1.0) * share).round() as usize;
        list[index].as_secs_f64()
    };
    eprintln!(
        "{clients} {} starts, {services} services: connected (problem lists complete) \
         min {:.1} s, median {:.1} s, max {:.1} s; first load complete (Icinga's notifications \
         too) median {:.1} s, max {:.1} s; all done after {:.1} s",
        if background { "background" } else { "user" },
        at(&connected, 0.0),
        at(&connected, 0.5),
        at(&connected, 1.0),
        at(&complete, 0.5),
        at(&complete, 1.0),
        all_done.as_secs_f64(),
    );
    eprintln!(
        "master memory: {baseline:.0} MiB before, peak {peak:.0} MiB (+{:.0} MiB)",
        peak - baseline
    );
}
