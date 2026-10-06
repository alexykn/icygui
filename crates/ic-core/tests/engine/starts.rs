//! Many clients starting at once against the local Docker Icinga from
//! `contract/scale/starts.sh` (PERF-09), and what the request budget and
//! the reconciles cost the master (`by_name_requests_and_reconciles_cost_the_master`,
//! below). N engines (20 by default) start
//! together, each with its real tiered load and event stream, either all
//! at once (users opening the app) or as background starts (launch at
//! login, each waiting its size-proportional random delay). Prints when
//! each was connected, the master's memory (sampled with `docker stats`)
//! and how long a small by-name query of another client (`curl`, every
//! half second: someone opening an object meanwhile) takes before and
//! during the starts, to calibrate the pacing factors in
//! docs/performance.md. `ICYGUI_SCALE_DELAY_PER_THOUSAND_MS` and
//! `ICYGUI_SCALE_DELAY_MAX_S` try other background-start factors.
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

/// Sends a small by-name query (one service, two attributes, as a pane
/// opens it) every half second with `curl` until stopped: the response
/// times in seconds, as anyone else using the master sees them.
fn probe_latency(
    url: String,
    credentials: String,
    stop: Arc<AtomicBool>,
) -> std::thread::JoinHandle<Vec<f64>> {
    std::thread::spawn(move || {
        let query = format!(
            "{url}/v1/objects/services?service=web-0000.prod.example.com!ping4\
             &attrs=state&attrs=last_check_result"
        );
        let mut times = Vec::new();
        while !stop.load(Ordering::SeqCst) {
            let output = Process::new("curl")
                .args(["-sk", "-o", "/dev/null", "-w", "%{time_total}", "-u"])
                .arg(&credentials)
                .args(["-H", "Accept: application/json", "--max-time", "60"])
                .arg(&query)
                .output();
            if let Ok(output) = output
                && let Ok(seconds) = String::from_utf8_lossy(&output.stdout).trim().parse()
            {
                times.push(seconds);
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        times
    })
}

/// `share` (0–1) of the sorted `values`.
#[expect(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "percentiles of a few hundred samples"
)]
fn percentile(values: &[f64], share: f64) -> f64 {
    if values.is_empty() {
        return f64::NAN;
    }
    let index = ((values.len() as f64 - 1.0) * share).round() as usize;
    values[index]
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
    let mut tuning = Tuning::default();
    if let Some(per_thousand) = std::env::var("ICYGUI_SCALE_DELAY_PER_THOUSAND_MS")
        .ok()
        .and_then(|value| value.parse().ok())
    {
        tuning.start_delay_per_thousand = Duration::from_millis(per_thousand);
    }
    if let Some(max) = std::env::var("ICYGUI_SCALE_DELAY_MAX_S")
        .ok()
        .and_then(|value| value.parse().ok())
    {
        tuning.start_delay_max = Duration::from_secs(max);
    }
    let credentials = format!("{user}:{password}");
    // The master's response time at rest.
    let idle_stop = Arc::new(AtomicBool::new(false));
    let idle_probe = probe_latency(url.clone(), credentials.clone(), Arc::clone(&idle_stop));
    tokio::time::sleep(Duration::from_secs(5)).await;
    idle_stop.store(true, Ordering::SeqCst);
    let mut idle = idle_probe.join().unwrap();
    idle.sort_by(f64::total_cmp);

    let baseline = memory_mib(&container).unwrap_or_default();
    let stop = Arc::new(AtomicBool::new(false));
    let sampler = sample_memory(container.clone(), Arc::clone(&stop));
    let probe = probe_latency(url.clone(), credentials, Arc::clone(&stop));

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
            tuning: tuning.clone(),
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
    let mut busy = probe.join().unwrap();
    busy.sort_by(f64::total_cmp);
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
        "{clients} {} starts ({} s per 1 000 services, at most {} s), {services} services: \
         connected (problem lists complete) min {:.1} s, median {:.1} s, max {:.1} s; first load \
         complete (Icinga's notifications too) median {:.1} s, max {:.1} s; all done after {:.1} s",
        if background { "background" } else { "user" },
        tuning.start_delay_per_thousand.as_secs_f64(),
        tuning.start_delay_max.as_secs(),
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
    eprintln!(
        "master response time (one service by name): at rest median {:.0} ms; during the starts \
         median {:.0} ms, 95th percentile {:.0} ms, max {:.0} ms ({} samples)",
        percentile(&idle, 0.5) * 1_000.0,
        percentile(&busy, 0.5) * 1_000.0,
        percentile(&busy, 0.95) * 1_000.0,
        percentile(&busy, 1.0) * 1_000.0,
        busy.len()
    );
}

/// The container's CPU time so far in seconds (its cgroup's accounting:
/// cgroup v1 `cpuacct.usage`, else v2 `cpu.stat`).
fn cpu_seconds(container: &str) -> Option<f64> {
    let read = |path: &str| {
        let output = Process::new("docker")
            .args(["exec", container, "cat", path])
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
    };
    if let Some(text) = read("/sys/fs/cgroup/cpuacct/cpuacct.usage") {
        let nanos: f64 = text.trim().parse().ok()?;
        return Some(nanos / 1e9);
    }
    let text = read("/sys/fs/cgroup/cpu.stat")?;
    let micros: f64 = text
        .lines()
        .find_map(|line| line.strip_prefix("usage_usec "))?
        .trim()
        .parse()
        .ok()?;
    Some(micros / 1e6)
}

/// A client of the container, with a request budget of one request per
/// `interval` (bursts of 10; zero: no budget).
fn scale_client(
    url: &str,
    user: &str,
    password: &str,
    pin: [u8; 32],
    interval: Duration,
) -> ic_api::Client {
    let settings = ic_api::ConnectionSettings::new(
        ic_api::Url::parse(url).unwrap(),
        ic_api::Credentials::Basic {
            username: user.to_owned(),
            password: secrecy::SecretString::from(password.to_owned()),
        },
        ic_api::TlsSettings {
            ca_pem: None,
            pinned_sha256: Some(pin),
            server_name: None,
            use_system_roots: false,
        },
    );
    let client = ic_api::Client::new(settings).unwrap();
    if interval.is_zero() {
        client
    } else {
        client.with_budget(Arc::new(ic_api::RequestBudget::new(interval, 10)))
    }
}

/// Calibrates the request budget and the reconcile factor (PERF-09,
/// docs/performance.md) against `contract/scale/starts.sh`'s container:
///
/// - `ICYGUI_SCALE_PACING=budget:<ms>`: N clients (20 by default) each
///   fetch an outage's problem details at once, `ICYGUI_SCALE_BATCHES`
///   by-name requests of 200 services in full (140 by default: 28 000
///   problems, the worst tier 3), paced by a request budget of one request
///   per `<ms>` with bursts of 10 (0: no budget). Prints how long they
///   took, the master's CPU time per request, its memory and the response
///   time of another client's small query meanwhile.
/// - `ICYGUI_SCALE_PACING=reconcile`: one client runs the lean reconcile's
///   big queries (the hosts in full, every service lean) a few times. Prints
///   the master's CPU time per reconcile and per 1 000 objects, and what a
///   client's reconciles cost it on average at the adaptive interval
///   (28 ms per object).
///
/// Ignored, a no-op without `ICYGUI_SCALE_URL`, and refused for anything
/// but the local container, like [`many_clients_start_at_once`].
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs contract/scale/starts.sh's local Docker Icinga"]
#[expect(clippy::too_many_lines, reason = "one measurement, step by step")]
#[expect(
    clippy::cast_precision_loss,
    reason = "counts of requests and objects as rates"
)]
async fn by_name_requests_and_reconciles_cost_the_master() {
    let Ok(url) = std::env::var("ICYGUI_SCALE_URL") else {
        eprintln!("ICYGUI_SCALE_URL isn't set: nothing to measure");
        return;
    };
    let container = std::env::var("ICYGUI_SCALE_CONTAINER").expect("ICYGUI_SCALE_CONTAINER");
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
    let pacing = std::env::var("ICYGUI_SCALE_PACING").unwrap_or_else(|_| "budget:200".to_owned());
    let clients: usize = std::env::var("ICYGUI_SCALE_CLIENTS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(20);
    let batches: usize = std::env::var("ICYGUI_SCALE_BATCHES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(140);
    let user = std::env::var("ICYGUI_SCALE_USER").unwrap_or_else(|_| "icygui".to_owned());
    let password =
        std::env::var("ICYGUI_SCALE_PASSWORD").unwrap_or_else(|_| "icygui-test".to_owned());
    let certificate = ic_core::fetch_certificate(url.clone(), None)
        .await
        .unwrap()
        .expect("the container's certificate");
    let pin = certificate.sha256;
    let lister = scale_client(&url, &user, &password, pin, Duration::ZERO);
    // What the master spends at rest (its own checks): subtracted below.
    let idle_cpu = cpu_seconds(&container).expect("the container's CPU time");
    let idle_from = Instant::now();
    let idle_rate = |cpu: f64| (cpu - idle_cpu) / idle_from.elapsed().as_secs_f64();

    if pacing == "reconcile" {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let idle_rate = idle_rate(cpu_seconds(&container).unwrap());
        let rounds = 3;
        let before = cpu_seconds(&container).expect("the container's CPU time");
        let started = Instant::now();
        let mut objects = 0;
        for _ in 0..rounds {
            let hosts = lister.hosts().await.unwrap();
            let services = lister.services(ic_api::Detail::Lean).await.unwrap();
            objects = hosts.len() + services.len();
        }
        let elapsed = started.elapsed().as_secs_f64();
        let took = elapsed / f64::from(rounds);
        let cpu =
            (cpu_seconds(&container).unwrap() - before - idle_rate * elapsed) / f64::from(rounds);
        let per_thousand = cpu / (objects as f64 / 1_000.0);
        let interval = (0.028 * objects as f64).clamp(300.0, 3_600.0);
        let share = cpu / interval;
        eprintln!(
            "a lean reconcile of {objects} objects (the hosts in full, every service lean): \
             {took:.1} s, master CPU {:.0} ms beyond its {:.0} % of a core at rest ({:.0} ms per \
             1 000 objects); every {:.0} min (28 ms per object): {:.2} % of one master core per \
             client, {:.1} % for 50 clients",
            cpu * 1_000.0,
            idle_rate * 100.0,
            per_thousand * 1_000.0,
            interval / 60.0,
            share * 100.0,
            share * 5_000.0,
        );
        return;
    }

    let interval_ms: u64 = pacing
        .strip_prefix("budget:")
        .and_then(|ms| ms.parse().ok())
        .expect("ICYGUI_SCALE_PACING=budget:<ms> or reconcile");
    let interval = Duration::from_millis(interval_ms);
    let keys: Vec<ic_model::ObjectKey> = lister
        .services(ic_api::Detail::Lean)
        .await
        .unwrap()
        .iter()
        .map(ic_model::Service::object_key)
        .collect();
    assert!(!keys.is_empty(), "the container has services");
    let credentials = format!("{user}:{password}");
    let idle_stop = Arc::new(AtomicBool::new(false));
    let idle_probe = probe_latency(url.clone(), credentials.clone(), Arc::clone(&idle_stop));
    tokio::time::sleep(Duration::from_secs(5)).await;
    idle_stop.store(true, Ordering::SeqCst);
    let mut idle = idle_probe.join().unwrap();
    idle.sort_by(f64::total_cmp);
    let idle_rate = idle_rate(cpu_seconds(&container).unwrap());

    let baseline = memory_mib(&container).unwrap_or_default();
    let cpu_before = cpu_seconds(&container).expect("the container's CPU time");
    let stop = Arc::new(AtomicBool::new(false));
    let sampler = sample_memory(container.clone(), Arc::clone(&stop));
    let probe = probe_latency(url.clone(), credentials, Arc::clone(&stop));
    let started = Instant::now();
    let per_request = ic_api::NAMES_PER_REQUEST;
    let mut tasks = Vec::new();
    for index in 0..clients {
        let client = scale_client(&url, &user, &password, pin, interval);
        let keys = keys.clone();
        tasks.push(tokio::spawn(async move {
            for batch in 0..batches {
                // Each client its own stretch of the services, wrapping.
                let first = (index * 997 + batch * per_request) % keys.len();
                let names: Vec<ic_model::ObjectKey> = keys
                    .iter()
                    .cycle()
                    .skip(first)
                    .take(per_request.min(keys.len()))
                    .cloned()
                    .collect();
                client
                    .objects(&names, ic_api::Detail::Full)
                    .await
                    .expect("a by-name query");
            }
            started.elapsed()
        }));
    }
    let mut done = Vec::new();
    for task in tasks {
        done.push(task.await.unwrap());
    }
    let took = started.elapsed();
    let cpu = cpu_seconds(&container).unwrap() - cpu_before - idle_rate * took.as_secs_f64();
    tokio::time::sleep(Duration::from_secs(2)).await;
    stop.store(true, Ordering::SeqCst);
    let peak = sampler.join().unwrap();
    let mut busy = probe.join().unwrap();
    busy.sort_by(f64::total_cmp);
    done.sort();
    let requests = (clients * batches) as f64;
    let rate = if interval.is_zero() {
        "no budget".to_owned()
    } else {
        format!("budget {:.1}/s, bursts of 10", 1.0 / interval.as_secs_f64())
    };
    eprintln!(
        "{clients} clients x {batches} by-name requests of {per_request} services in full \
         ({rate}): done median {:.1} s, max {:.1} s; master CPU {cpu:.1} s beyond its {:.0} % \
         of a core at rest ({:.0} ms per request, {:.0} % of one core over the {:.1} s)",
        done[done.len() / 2].as_secs_f64(),
        done[done.len() - 1].as_secs_f64(),
        idle_rate * 100.0,
        cpu / requests * 1_000.0,
        cpu / took.as_secs_f64() * 100.0,
        took.as_secs_f64(),
    );
    eprintln!(
        "master memory: {baseline:.0} MiB before, peak {peak:.0} MiB (+{:.0} MiB)",
        peak - baseline
    );
    eprintln!(
        "master response time (one service by name): at rest median {:.0} ms; meanwhile \
         median {:.0} ms, 95th percentile {:.0} ms, max {:.0} ms ({} samples)",
        percentile(&idle, 0.5) * 1_000.0,
        percentile(&busy, 0.5) * 1_000.0,
        percentile(&busy, 0.95) * 1_000.0,
        percentile(&busy, 1.0) * 1_000.0,
        busy.len()
    );
}
