//! Many icygui clients on one master for a long while (PLAN.md §4.2, *Many
//! clients on one master*; docs/performance.md, *Fifty clients on one
//! master*): real engines, each with its own tiered load, event stream,
//! timers, request budget and local event log, started, switched and
//! stopped by commands from outside while the master is restarted,
//! reloaded and flooded around them. `contract/scale/fleet.sh` drives it
//! and measures the master; this side records what the clients see and
//! do.
//!
//! Commands, one per line, from the file or FIFO `ICYGUI_FLEET_CONTROL`:
//!
//! - `add <n> <live|quiet> <user|background> [spread s]`: starts `n`
//!   engines (`Start::User` as a window opening, `Start::Background` as a
//!   start at login with its own random wait), each at a random moment
//!   within the spread (0: at once). A quiet one gets `SetQuiet(true)`
//!   right away, as a window hidden in the tray.
//! - `mode <n> <live|quiet>`: switches `n` engines of the other mode,
//!   lowest first.
//! - `stop <n|all>`: shuts the newest `n` engines down.
//! - `data <dir|none>`: engines started from now on keep their event logs
//!   in `<dir>/<index>` (an engine started again with the same index finds
//!   what it kept: adaptive pacing's keys, baselines and team size), or in
//!   a fresh directory each (`none`, the default).
//! - `mark <label>`: a line in the timeline (phases for the analysis).
//! - `quit` (or the end of the file): stops everything.
//!
//! What it writes to `ICYGUI_FLEET_OUT` (CSV, Unix seconds):
//!
//! - `timeline.csv`: commands, marks, every engine's connection states,
//!   when its first load was complete;
//! - `delays.csv`: every new check result of the marker service
//!   (`ICYGUI_FLEET_MARKER`, `host!service`; `fleet.sh` flips its state
//!   every second) in each engine's published snapshots: when it was seen,
//!   its `execution_end` and the delay between them, live or quiet (live
//!   engines see it with its `CheckResult`, quiet ones with its
//!   `StateChange`; snapshots go out every 250 ms live, every 2 s quiet);
//! - `requests.csv`: every request of every engine (ic-api's debug log,
//!   attributed to the engine whose runtime thread sent it);
//! - `loads.csv`, `plans.csv`, `found.csv`: every load with how many
//!   objects it found changed without an event, every reconcile planned,
//!   and each object a periodic reconcile found, with how long ago Icinga
//!   says its state began (ic-core's instrumentation);
//! - `cpu.csv`: every 10 s the CPU time of each engine's threads, of the
//!   threads no engine owns, and the process's resident memory;
//! - `engine.log`: ic-core's info lines and every warning and error, with
//!   the engine.
//!
//! `ICYGUI_FLEET_RECONCILE_S` sets a fixed reconcile interval (as the
//! settings can; at least 5 minutes at this size) for more reconciles in a
//! short run, and `ICYGUI_FLEET_QUIET_RECONCILE_S` lowers quiet mode's
//! reconcile floor (30 minutes) for the same reason. Live engines also open an object now and then (`Command::Focus` of a
//! scale service, on average every `ICYGUI_FLEET_BROWSE_S`, default 120 s;
//! 0: never), as someone looking at a dashboard does.
//!
//! Ignored, and a no-op without `ICYGUI_FLEET_URL`. Refused before a single
//! request for anything but the disposable demo cluster of
//! `demo/docker-compose.yml`: the URL must point to this machine, the
//! fixture-only `viewer` must log in, the node must be `master-01`, and the
//! compose project must run it. Never point it at a production Icinga.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers fail the test loudly"
)]

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::PathBuf;
use std::process::Command as Process;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures::StreamExt;
use futures::channel::mpsc::UnboundedReceiver;
use ic_config::{AuthConfig, Environment, General};
use ic_core::{Command, ConnectionState, CoreEvent, Start, Tuning};
use ic_model::{ObjectKey, ServiceKey, Timestamp};
use tracing_subscriber::Layer as _;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

use crate::support::{Engine, FakeSecrets, Launch};

/// Now, in Unix seconds.
fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |since| since.as_secs_f64())
}

/// One output file, written by many threads, flushed every few seconds.
struct Csv(Mutex<BufWriter<File>>);

impl Csv {
    fn create(dir: &std::path::Path, name: &str, header: &str) -> Self {
        let mut file = BufWriter::new(File::create(dir.join(name)).expect("output file"));
        writeln!(file, "{header}").unwrap();
        Self(Mutex::new(file))
    }

    fn line(&self, line: &str) {
        let mut file = self.0.lock().unwrap();
        let _ = writeln!(file, "{line}");
    }

    fn flush(&self) {
        let _ = self.0.lock().unwrap().flush();
    }
}

/// Everything the harness writes.
struct Out {
    timeline: Csv,
    delays: Csv,
    requests: Csv,
    loads: Csv,
    plans: Csv,
    found: Csv,
    cpu: Csv,
    warnings: Csv,
}

impl Out {
    fn create(dir: &std::path::Path) -> Self {
        std::fs::create_dir_all(dir).expect("output directory");
        Self {
            timeline: Csv::create(dir, "timeline.csv", "t,client,event,detail"),
            delays: Csv::create(
                dir,
                "delays.csv",
                "t,client,mode,execution_end,delay_s,state",
            ),
            requests: Csv::create(
                dir,
                "requests.csv",
                "t,client,method,path,status,elapsed_ms,error",
            ),
            loads: Csv::create(
                dir,
                "loads.csv",
                "t,client,kind,quiet,took_ms,found,reconciles,reconciles_found",
            ),
            plans: Csv::create(dir, "plans.csv", "t,client,cadence,interval_s,in_s"),
            found: Csv::create(
                dir,
                "found.csv",
                "t,client,object,changed,since_shift_s,from,to,began_s_ago,acknowledged,downtime_depth",
            ),
            cpu: Csv::create(dir, "cpu.csv", "t,client,ticks,threads,rss_kib"),
            warnings: Csv::create(dir, "engine.log", "t client level target message fields"),
        }
    }

    fn flush(&self) {
        for csv in [
            &self.timeline,
            &self.delays,
            &self.requests,
            &self.loads,
            &self.plans,
            &self.found,
            &self.cpu,
            &self.warnings,
        ] {
            csv.flush();
        }
    }

    fn event(&self, client: Option<usize>, event: &str, detail: &str) {
        self.timeline.line(&format!(
            "{:.3},{},{event},{}",
            now(),
            client_label(client),
            detail.replace(',', ";")
        ));
    }
}

fn client_label(client: Option<usize>) -> String {
    client.map_or_else(|| "-1".to_owned(), |client| client.to_string())
}

thread_local! {
    /// This thread's id as the kernel knows it (`/proc/self/task/<tid>`).
    static TID: Cell<u32> = const { Cell::new(0) };
}

/// The calling thread's kernel id.
fn tid() -> u32 {
    TID.with(|cell| {
        if cell.get() == 0 {
            let tid = std::fs::read_link("/proc/thread-self")
                .ok()
                .and_then(|path| path.file_name()?.to_str()?.parse().ok())
                .unwrap_or(u32::MAX);
            cell.set(tid);
        }
        cell.get()
    })
}

/// The process's threads now.
fn threads_now() -> HashSet<u32> {
    std::fs::read_dir("/proc/self/task")
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse().ok())
                .collect()
        })
        .unwrap_or_default()
}

/// A thread's CPU time so far in clock ticks (`utime + stime`).
fn thread_ticks(tid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/self/task/{tid}/stat")).ok()?;
    // The name may hold spaces: fields count from after its `)`.
    let rest = &stat[stat.rfind(')')? + 2..];
    let fields: Vec<&str> = rest.split(' ').collect();
    // Fields 14 and 15 of `stat`, 12 and 13 after the name.
    Some(fields.get(11)?.parse::<u64>().ok()? + fields.get(12)?.parse::<u64>().ok()?)
}

/// The process's resident memory in KiB.
fn rss_kib() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find_map(|line| line.strip_prefix("VmRSS:"))
                .and_then(|value| value.trim().trim_end_matches("kB").trim().parse().ok())
        })
        .unwrap_or(0)
}

/// Which engine each runtime thread belongs to (found by the threads that
/// appeared while it started).
type Owners = Arc<RwLock<HashMap<u32, usize>>>;

/// The fields of one log event.
#[derive(Default)]
struct Fields {
    message: String,
    values: Vec<(&'static str, String)>,
}

impl Fields {
    fn get(&self, name: &str) -> &str {
        self.values
            .iter()
            .find(|(field, _)| *field == name)
            .map_or("", |(_, value)| value.as_str())
    }
}

impl tracing::field::Visit for Fields {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        } else {
            self.values.push((field.name(), format!("{value:?}")));
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            value.clone_into(&mut self.message);
        } else {
            self.values.push((field.name(), value.to_owned()));
        }
    }
}

/// The engine an environment name `fleet-<n>` stands for.
fn client_of(environment: &str) -> Option<usize> {
    environment.strip_prefix("fleet-")?.parse().ok()
}

/// Records ic-api's requests and ic-core's load instrumentation from the
/// log, per engine.
struct Capture {
    out: Arc<Out>,
    owners: Owners,
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Capture {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _context: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut fields = Fields::default();
        event.record(&mut fields);
        let owner = || self.owners.read().unwrap().get(&tid()).copied();
        let t = now();
        match fields.message.as_str() {
            "request" | "request failed" => self.out.requests.line(&format!(
                "{t:.3},{},{},{},{},{},{}",
                client_label(owner()),
                fields.get("method"),
                fields.get("path"),
                fields.get("status"),
                fields.get("elapsed_ms"),
                fields.get("error").replace(',', ";")
            )),
            "load done" => self.out.loads.line(&format!(
                "{t:.3},{},{},{},{},{},{},{}",
                client_label(client_of(fields.get("environment")).or_else(owner)),
                fields.get("kind"),
                fields.get("quiet"),
                fields.get("took_ms"),
                fields.get("found"),
                fields.get("reconciles"),
                fields.get("reconciles_found")
            )),
            "a reconcile found a change no event announced" => self.out.found.line(&format!(
                "{t:.3},{},{},{},{},{},{},{},{},{}",
                client_label(client_of(fields.get("environment")).or_else(owner)),
                fields.get("object").replace(',', ";"),
                fields.get("changed"),
                fields.get("since_shift_s").replace(',', ";"),
                fields.get("from").replace(',', ";"),
                fields.get("to").replace(',', ";"),
                fields.get("began_s_ago").replace(',', ";"),
                fields.get("acknowledged").replace(',', ";"),
                fields.get("downtime_depth").replace(',', ";")
            )),
            "reconcile planned" => self.out.plans.line(&format!(
                "{t:.3},{},{},{},{}",
                client_label(client_of(fields.get("environment")).or_else(owner)),
                fields.get("cadence"),
                fields.get("interval_s"),
                fields.get("in_s")
            )),
            _ => {
                let metadata = event.metadata();
                if *metadata.level() <= tracing::Level::INFO {
                    let mut rest = String::new();
                    for (name, value) in &fields.values {
                        let _ = write!(rest, " {name}={value}");
                    }
                    self.out.warnings.line(&format!(
                        "{t:.3} {} {} {} {}{rest}",
                        client_label(owner()),
                        metadata.level(),
                        metadata.target(),
                        fields.message
                    ));
                }
            }
        }
    }
}

/// What every engine's task shares.
struct Shared {
    out: Arc<Out>,
    marker: ServiceKey,
    /// Engines whose first load is complete.
    complete: AtomicU64,
    /// Marker results seen.
    seen: AtomicU64,
}

/// One engine of the fleet.
struct Client {
    engine: Engine,
    live: bool,
    connected: Arc<AtomicBool>,
}

/// How to start engines.
struct Fleet {
    url: String,
    user: String,
    password: String,
    pin: String,
    owners: Owners,
    general: General,
    tuning: Tuning,
    shared: Arc<Shared>,
    clients: Mutex<Vec<Option<Client>>>,
    /// One engine starts at a time, so the threads that appear are its.
    starting: Mutex<()>,
    /// Where engines keep their event logs (`data <dir|none>`): engine `i`
    /// in `<dir>/<i>`, so one started again with the same index finds what
    /// it kept (adaptive pacing's keys and baselines); `None`: a fresh
    /// directory each.
    data: Mutex<Option<PathBuf>>,
}

impl Fleet {
    /// Starts engine `index` and returns it with its connection flag.
    fn start(&self, index: usize, live: bool, start: Start) -> Client {
        let _one = self.starting.lock().unwrap();
        let before = threads_now();
        let mut environment = Environment::new(
            &format!("fleet-{index:02}"),
            &self.url,
            AuthConfig::Basic {
                username: self.user.clone(),
            },
        );
        environment.id = format!("00000000-0000-0000-0001-{index:012}");
        environment.tls.use_system_roots = false;
        environment.urls[0].pinned_sha256 = Some(self.pin.clone());
        let secrets = FakeSecrets::with(&environment.id, &self.password);
        // Production timing, the timers' seed included (random: no
        // synchronised clients, as real ones), on the wall clock.
        let mut engine = Launch {
            environment,
            secrets,
            tuning: self.tuning.clone(),
            general: self.general.clone(),
            now: Timestamp::now().as_unix_seconds(),
            data_dir: self
                .data
                .lock()
                .unwrap()
                .as_ref()
                .map(|dir| dir.join(index.to_string())),
            start,
            real_clock: true,
        }
        .start();
        if !live {
            engine.send(Command::SetQuiet(true));
        }
        {
            let mut owners = self.owners.write().unwrap();
            for tid in threads_now().difference(&before) {
                owners.insert(*tid, index);
            }
        }
        let events = std::mem::replace(&mut engine.events, futures::channel::mpsc::unbounded().1);
        let connected = Arc::new(AtomicBool::new(false));
        tokio::spawn(consume(
            index,
            events,
            Arc::clone(&self.shared),
            Arc::clone(&connected),
        ));
        self.shared.out.event(
            Some(index),
            "start",
            &format!(
                "{} {}",
                if live { "live" } else { "quiet" },
                match start {
                    Start::User => "user",
                    Start::Background => "background",
                }
            ),
        );
        Client {
            engine,
            live,
            connected,
        }
    }
}

fn state_name(state: &ConnectionState) -> String {
    match state {
        ConnectionState::Connecting { attempt } => format!("connecting {attempt}"),
        ConnectionState::Loading { phase, done, .. } => format!("loading {phase:?} {done}"),
        ConnectionState::Connected { node, .. } => format!("connected {}", node.name),
        ConnectionState::Reconnecting { error, attempt, .. } => {
            format!("reconnecting {attempt} {error}")
        }
        ConnectionState::AuthFailed { message } => format!("auth-failed {message}"),
        ConnectionState::TlsFailed { message, .. } => format!("tls-failed {message}"),
        ConnectionState::MissingSecret => "missing-secret".to_owned(),
        ConnectionState::Misconfigured { message } => format!("misconfigured {message}"),
    }
}

/// Reads one engine's events as the app does (the latest snapshot held, as
/// a window holds it) and records what the analysis needs.
async fn consume(
    index: usize,
    mut events: UnboundedReceiver<CoreEvent>,
    shared: Arc<Shared>,
    connected: Arc<AtomicBool>,
) {
    let mut last_end: Option<f64> = None;
    let mut complete = false;
    let mut held: Option<Arc<ic_core::snapshot::Snapshot>> = None;
    let mut last_state = String::new();
    while let Some(event) = events.next().await {
        match event {
            CoreEvent::Connection(state) => {
                let up = matches!(state, ConnectionState::Connected { .. });
                connected.store(up, Ordering::SeqCst);
                let name = state_name(&state);
                // Loading progress comes often: its phase is enough.
                let kind: String = name.split(' ').take(2).collect::<Vec<_>>().join(" ");
                if kind != last_state {
                    shared.out.event(Some(index), "state", &name);
                    last_state = kind;
                }
            }
            CoreEvent::Snapshot(snapshot) => {
                if let Some(service) = snapshot.services.get(&shared.marker)
                    && let Some(result) = &service.check.result
                {
                    let end = result.execution_end.as_unix_seconds();
                    if last_end.is_none_or(|last| end > last) {
                        let seen = now();
                        if last_end.is_some() {
                            shared.seen.fetch_add(1, Ordering::Relaxed);
                            shared.out.delays.line(&format!(
                                "{seen:.3},{index},{},{end:.3},{:.3},{:?}",
                                if snapshot.quiet { "quiet" } else { "live" },
                                seen - end,
                                service.state
                            ));
                        }
                        last_end = Some(end);
                    }
                }
                if !complete && !snapshot.icinga_notifications.is_empty() {
                    complete = true;
                    shared.complete.fetch_add(1, Ordering::Relaxed);
                    shared.out.event(
                        Some(index),
                        "complete",
                        &format!("{} services", snapshot.services.len()),
                    );
                }
                // Held like the window holds it, until the next one.
                drop(held.replace(snapshot));
            }
            CoreEvent::Notification(record) => {
                shared
                    .out
                    .event(Some(index), "notification", &record.intent.title);
            }
            _ => {}
        }
    }
}

/// Samples every engine's CPU time and the process's memory every 10 s.
fn sample_cpu(out: &Out, owners: &RwLock<HashMap<u32, usize>>, stop: &AtomicBool) {
    while !stop.load(Ordering::SeqCst) {
        let t = now();
        let owners_now = owners.read().unwrap().clone();
        let mut per: HashMap<Option<usize>, (u64, usize)> = HashMap::new();
        for tid in threads_now() {
            if let Some(ticks) = thread_ticks(tid) {
                let entry = per.entry(owners_now.get(&tid).copied()).or_default();
                entry.0 += ticks;
                entry.1 += 1;
            }
        }
        let rss = rss_kib();
        for (client, (ticks, threads)) in per {
            out.cpu.line(&format!(
                "{t:.3},{},{ticks},{threads},{rss}",
                client_label(client)
            ));
        }
        std::thread::sleep(Duration::from_secs(10));
    }
}

/// A small, fast, good enough random number generator.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    #[expect(clippy::cast_precision_loss, reason = "a random fraction")]
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1_u64 << 53) as f64
    }
}

/// A random service of `contract/scale/generate.py`'s workload.
fn scale_service(rng: &mut Rng) -> ObjectKey {
    const ROLES: [&str; 10] = [
        "web", "api", "db", "k8s", "mq", "cache", "lb", "edge", "backup", "storage",
    ];
    const SERVICES: [&str; 6] = ["ping4", "ssh", "load", "disk /", "memory", "http"];
    let host = rng.next() % 2_000;
    let role = ROLES[usize::try_from(host % 10).unwrap()];
    let service = SERVICES[usize::try_from(rng.next() % 6).unwrap()];
    ObjectKey::Service {
        key: ServiceKey::new(&format!("{role}-{host:04}.prod.example.com"), service),
    }
}

/// Refuses anything but the disposable demo cluster (see the module
/// notes), before any load.
async fn check_disposable(url: &str, pin: [u8; 32]) {
    const REFUSED: &str = "not the disposable demo cluster of demo/docker-compose.yml; \
                           the fleet measurement never runs elsewhere";
    let parsed = ic_api::Url::parse(url).expect("ICYGUI_FLEET_URL");
    let host = parsed.host_str().unwrap_or_default();
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    assert!(loopback, "{REFUSED}: {host} is not this machine");
    let client = |user: &str, password: &str| {
        ic_api::Client::new(ic_api::ConnectionSettings::new(
            parsed.clone(),
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
        ))
        .unwrap()
    };
    let viewer = client("viewer", "viewer-test").info().await;
    assert!(
        matches!(&viewer, Ok(info) if info.user == "viewer"),
        "{REFUSED}: its fixture-only viewer can't log in ({viewer:?})"
    );
    let node = client("viewer", "viewer-test")
        .status()
        .await
        .expect("status")
        .node_name;
    assert_eq!(node, "master-01", "{REFUSED}");
    let compose = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../demo/docker-compose.yml");
    let output = Process::new("docker")
        .args(["compose", "-f"])
        .arg(&compose)
        .args(["ps", "-q", "master-01"])
        .output()
        .expect("docker");
    assert!(
        output.status.success() && !output.stdout.is_empty(),
        "{REFUSED}: {} runs no master-01",
        compose.display()
    );
}

/// The commands, line by line, from the control file (a FIFO stays open
/// until its writer closes it).
fn read_commands(path: PathBuf, commands: tokio::sync::mpsc::UnboundedSender<String>) {
    std::thread::spawn(move || {
        let file = File::open(&path).expect("ICYGUI_FLEET_CONTROL");
        for line in BufReader::new(file).lines() {
            let Ok(line) = line else { break };
            let line = line.trim().to_owned();
            if !line.is_empty() && commands.send(line).is_err() {
                return;
            }
        }
        let _ = commands.send("quit".to_owned());
    });
}

/// Shuts `clients` down side by side (each may wait for its engine).
fn shut_down(clients: Vec<Client>) {
    let threads: Vec<_> = clients
        .into_iter()
        .map(|mut client| std::thread::spawn(move || client.engine.shutdown()))
        .collect();
    for thread in threads {
        let _ = thread.join();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[ignore = "needs contract/scale/fleet.sh and the demo cluster"]
#[expect(clippy::too_many_lines, reason = "one command loop")]
async fn fleet() {
    let Ok(url) = std::env::var("ICYGUI_FLEET_URL") else {
        eprintln!("ICYGUI_FLEET_URL isn't set: nothing to measure");
        return;
    };
    let control =
        PathBuf::from(std::env::var("ICYGUI_FLEET_CONTROL").expect("ICYGUI_FLEET_CONTROL"));
    let out_dir = PathBuf::from(std::env::var("ICYGUI_FLEET_OUT").expect("ICYGUI_FLEET_OUT"));
    let user = std::env::var("ICYGUI_FLEET_USER").unwrap_or_else(|_| "icygui-demo".to_owned());
    let password = std::env::var("ICYGUI_FLEET_PASSWORD")
        .unwrap_or_else(|_| "icygui-demo-password".to_owned());
    let marker = std::env::var("ICYGUI_FLEET_MARKER")
        .unwrap_or_else(|_| "storage-1999.prod.example.com!smart".to_owned());
    let (marker_host, marker_service) = marker.split_once('!').expect("host!service");
    // A fixed reconcile interval (`General::reconcile_interval_secs`, as
    // the settings set it; at least 5 minutes at this size), for more
    // reconciles in a short run; the adaptive interval without it.
    let reconcile_secs: u32 = std::env::var("ICYGUI_FLEET_RECONCILE_S")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    // Quiet mode's reconcile floor (`Tuning::quiet_reconcile_interval`,
    // 30 minutes), lowered only to see in a short run what quiet engines'
    // reconciles find.
    let quiet_reconcile_secs: Option<u64> = std::env::var("ICYGUI_FLEET_QUIET_RECONCILE_S")
        .ok()
        .and_then(|value| value.parse().ok());
    let browse: f64 = std::env::var("ICYGUI_FLEET_BROWSE_S")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(120.0);

    let certificate = ic_core::fetch_certificate(url.clone(), None)
        .await
        .unwrap()
        .expect("master-01's certificate");
    check_disposable(&url, certificate.sha256).await;

    let out = Arc::new(Out::create(&out_dir));
    let owners: Owners = Arc::default();
    tracing_subscriber::registry()
        .with(
            Capture {
                out: Arc::clone(&out),
                owners: Arc::clone(&owners),
            }
            .with_filter(
                tracing_subscriber::filter::Targets::new()
                    .with_target("ic_api", tracing::Level::DEBUG)
                    .with_target("ic_core::engine::sync", tracing::Level::DEBUG)
                    .with_target("ic_core", tracing::Level::INFO)
                    .with_default(tracing::Level::WARN),
            ),
        )
        .init();

    let stop = Arc::new(AtomicBool::new(false));
    {
        let out = Arc::clone(&out);
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_secs(2));
                out.flush();
            }
        });
    }
    let sampler = {
        let (out, owners, stop) = (Arc::clone(&out), Arc::clone(&owners), Arc::clone(&stop));
        std::thread::spawn(move || sample_cpu(&out, &owners, &stop))
    };

    let fleet = Arc::new(Fleet {
        url,
        user,
        password,
        pin: ic_api::format_fingerprint(&certificate.sha256),
        owners,
        general: General {
            reconcile_interval_secs: reconcile_secs,
            ..General::default()
        },
        tuning: {
            let mut tuning = Tuning::default();
            if let Some(secs) = quiet_reconcile_secs {
                tuning.quiet_reconcile_interval = Duration::from_secs(secs);
            }
            tuning
        },
        shared: Arc::new(Shared {
            out: Arc::clone(&out),
            marker: ServiceKey::new(marker_host, marker_service),
            complete: AtomicU64::new(0),
            seen: AtomicU64::new(0),
        }),
        clients: Mutex::new(Vec::new()),
        data: Mutex::new(None),
        starting: Mutex::new(()),
    });

    // Live engines open an object now and then.
    if browse > 0.0 {
        let fleet = Arc::clone(&fleet);
        tokio::spawn(async move {
            let mut rng = Rng(0x9E37_79B9_7F4A_7C15 ^ now().to_bits());
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                let clients = fleet.clients.lock().unwrap();
                for client in clients.iter().flatten() {
                    if client.live
                        && client.connected.load(Ordering::SeqCst)
                        && rng.unit() < 1.0 / browse
                    {
                        client.engine.send(Command::Focus(scale_service(&mut rng)));
                    }
                }
            }
        });
    }
    // Progress on stderr.
    {
        let fleet = Arc::clone(&fleet);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(30)).await;
                let clients = fleet.clients.lock().unwrap();
                let running = clients.iter().flatten().count();
                let connected = clients
                    .iter()
                    .flatten()
                    .filter(|client| client.connected.load(Ordering::SeqCst))
                    .count();
                let live = clients
                    .iter()
                    .flatten()
                    .filter(|client| client.live)
                    .count();
                eprintln!(
                    "fleet: {running} engines, {connected} connected, {live} live, {} loaded, \
                     {} marker results seen, rss {} MiB",
                    fleet.shared.complete.load(Ordering::Relaxed),
                    fleet.shared.seen.load(Ordering::Relaxed),
                    rss_kib() / 1_024
                );
            }
        });
    }

    let (commands_tx, mut commands) = tokio::sync::mpsc::unbounded_channel();
    read_commands(control, commands_tx);
    let mut rng = Rng(0xD1B5_4A32_D192_ED03 ^ now().to_bits());
    while let Some(line) = commands.recv().await {
        out.event(None, "command", &line);
        eprintln!("fleet: {line}");
        let words: Vec<&str> = line.split_whitespace().collect();
        match words.as_slice() {
            ["add", n, mode, start, rest @ ..] => {
                let n: usize = n.parse().expect("add <n>");
                let live = *mode == "live";
                let start = if *start == "background" {
                    Start::Background
                } else {
                    Start::User
                };
                let spread: f64 = rest.first().map_or(0.0, |s| s.parse().expect("spread"));
                let first = {
                    let mut clients = fleet.clients.lock().unwrap();
                    let first = clients.len();
                    clients.resize_with(first + n, || None);
                    first
                };
                let mut offsets: Vec<f64> = (0..n).map(|_| rng.unit() * spread).collect();
                offsets.sort_by(f64::total_cmp);
                let fleet = Arc::clone(&fleet);
                tokio::spawn(async move {
                    let began = tokio::time::Instant::now();
                    for (k, offset) in offsets.into_iter().enumerate() {
                        tokio::time::sleep_until(began + Duration::from_secs_f64(offset)).await;
                        let index = first + k;
                        let client = fleet.start(index, live, start);
                        fleet.clients.lock().unwrap()[index] = Some(client);
                    }
                });
            }
            ["mode", n, mode] => {
                let n: usize = n.parse().expect("mode <n>");
                let live = *mode == "live";
                let mut clients = fleet.clients.lock().unwrap();
                for (index, client) in clients
                    .iter_mut()
                    .enumerate()
                    .filter_map(|(index, client)| Some((index, client.as_mut()?)))
                    .filter(|(_, client)| client.live != live)
                    .take(n)
                {
                    client.live = live;
                    client.engine.send(Command::SetQuiet(!live));
                    out.event(Some(index), "mode", if live { "live" } else { "quiet" });
                }
            }
            ["stop", n] => {
                let stopping: Vec<Client> = {
                    let mut clients = fleet.clients.lock().unwrap();
                    let n = if *n == "all" {
                        clients.len()
                    } else {
                        n.parse().expect("stop <n|all>")
                    };
                    let keep = clients.len().saturating_sub(n);
                    clients.split_off(keep).into_iter().flatten().collect()
                };
                let count = stopping.len();
                tokio::task::spawn_blocking(move || shut_down(stopping))
                    .await
                    .unwrap();
                out.event(None, "stopped", &count.to_string());
            }
            ["mark", ..] => out.event(None, "mark", line.trim_start_matches("mark").trim()),
            ["data", dir] => {
                *fleet.data.lock().unwrap() = (*dir != "none").then(|| PathBuf::from(dir));
            }
            ["quit"] => break,
            _ => eprintln!("fleet: unknown command {line:?}"),
        }
    }
    let all: Vec<Client> = fleet.clients.lock().unwrap().drain(..).flatten().collect();
    let count = all.len();
    tokio::task::spawn_blocking(move || shut_down(all))
        .await
        .unwrap();
    out.event(None, "stopped", &count.to_string());
    stop.store(true, Ordering::SeqCst);
    let _ = sampler.join();
    out.flush();
}
