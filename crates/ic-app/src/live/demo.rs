//! `icygui --demo` (ENV-10): the whole app against a simulated Icinga in
//! the same process. An `ic_mock::MockServer` serves the design's
//! `prod-cluster` scenario on 127.0.0.1 over HTTPS, with live simulated
//! changes (checks at their intervals, new problems, recoveries, flapping,
//! outages, downtimes and problem storms that exercise the notification
//! storm control), and the real `ic-core` connects to it like to any
//! Icinga. Actions work too. Nothing is saved, and nothing of the user's
//! settings, keychain or event log is touched: the demo's event log lives
//! in a temporary directory.
//!
//! Development switches (`ICYGUI_DEMO_SCENARIO`, `ICYGUI_DEMO_SEED`,
//! `ICYGUI_DEMO_DASHBOARD`, `ICYGUI_DEMO_OPEN`, `ICYGUI_DEMO_FAULT`,
//! `ICYGUI_DEMO_ENVIRONMENTS`) are
//! read by `crate::dev`; [`DemoFault`] makes the demo show the
//! connection's failure states, for screenshots and tests.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures::channel::oneshot;
use ic_config::{
    AuthConfig, Config, Dashboard, DashboardGroup, Environment, GroupBy, GroupOrder, GroupSource,
    HandledSetting, ObjectKind, Sort, StreamOptions, TlsConfig, View, ViewDisplay, ViewGroups,
};
use ic_core::LogEntry;
use ic_core::ports::{SecretError, SecretStore};
use ic_mock::{
    MockConfig, MockControl, MockServer, MockUser, SimulationConfig, StormConfig, scenarios,
};
use ic_model::{ObjectKey, ServiceState, Timestamp};
use ic_rules::{NotificationSettings, ScopeSetting};
use secrecy::SecretString;

/// The demo environment's id (stable, so tests and screenshots can refer
/// to it).
pub(crate) const ENVIRONMENT_ID: &str = "icygui-demo";
/// The demo's second environment: `ic_mock`'s small `staging` scenario,
/// to switch to (ENV-01).
pub(crate) const STAGING_ID: &str = "icygui-demo-staging";
/// The demo's third environment: `ic_mock`'s nearly empty `lab` scenario.
pub(crate) const LAB_ID: &str = "icygui-demo-lab";
/// The node name the demo's Icinga reports (the design's).
pub(crate) const ENDPOINT: &str = "master-01";
/// The demo server's API user.
pub(crate) const USER: &str = "icygui";
/// The scenario served unless `ICYGUI_DEMO_SCENARIO` names another.
pub(crate) const DEFAULT_SCENARIO: &str = "prod-cluster";

/// Seconds (simulator ticks) between problem storms.
const STORM_EVERY: u64 = 300;

/// When the [`DemoFault::Outage`] begins.
const OUTAGE_AFTER: Duration = Duration::from_secs(20);
/// The API user's permissions with [`DemoFault::NoComments`]: everything
/// but adding comments.
const NO_COMMENTS: &[&str] = &[
    "objects/query/*",
    "events/*",
    "status/*",
    "actions/acknowledge-problem",
    "actions/remove-acknowledgement",
    "actions/schedule-downtime",
    "actions/remove-downtime",
    "actions/reschedule-check",
    "actions/remove-comment",
];
/// The latency of every answer with [`DemoFault::Slow`].
const SLOW_LATENCY: Duration = Duration::from_millis(900);

/// A failure the demo shows on purpose (`ICYGUI_DEMO_FAULT`), to see and
/// test the connection's banners and states without a broken Icinga.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DemoFault {
    /// `offline`: nothing listens where the environment points
    /// (reconnecting with a countdown).
    Offline,
    /// `auth`: the stored password is wrong (login refused).
    Auth,
    /// `tls`: the server's certificate isn't pinned (not trusted).
    Tls,
    /// `missing-secret`: no password is stored.
    MissingSecret,
    /// `misconfigured`: the pinned fingerprint is garbage.
    Misconfigured,
    /// `pin-mismatch`: another certificate is pinned than the one the
    /// server presents (renewed, or intercepted: both fingerprints show).
    PinMismatch,
    /// `outage`: 20 seconds in, the server drops every stream and answers
    /// 503 (the connection is lost and retried); four minutes later it
    /// answers again (16c's `live again after 4m`).
    Outage,
    /// `slow`: every answer takes 0.9 s (the load's progress shows).
    Slow,
    /// `frozen`: the simulated Icinga stops checking, so checks become
    /// late (within about two minutes for the one-minute checks).
    Frozen,
    /// `partial`: the master answers 503 from the start, so the engine
    /// connects to the satellite `sat-ams-01` in the child zone `ams` (the
    /// `prod-cluster` environment's second URL): a partial view, labelled
    /// as such, while the engine keeps asking the master (ENV-12).
    Partial,
    /// `satellite-down`: 20 seconds in, both of `prod-cluster`'s
    /// satellites in zone `fra` (`sat-fra-01`, `sat-fra-02`) drop out of the
    /// cluster, so the zone is cut off: its beats go silent, its checks go
    /// late, and the cluster health page and its sidebar dot turn critical
    /// (06c, 16t: `zone fra: sat-fra-01 and sat-fra-02 disconnected,
    /// heartbeat lost`).
    SatelliteDown,
    /// `satellite-checks-stopped`: 20 seconds in, `sat-fra-02` drops out
    /// and `sat-fra-01`'s checker hangs while it stays connected: zone
    /// `fra` runs no checks though an endpoint answers (16s).
    SatelliteChecksStopped,
    /// `beat-late`: zone `ams`'s heartbeat comes 13 seconds late every
    /// two minutes: the heartbeat row shows it *1 interval late* for a few
    /// seconds, before it arrives (16a2), and no alert.
    BeatLate,
    /// `no-comments`: the API user may do everything but add comments, so
    /// nothing offers to write one (topic 17, frame 17f).
    NoComments,
    /// `master-down`: 20 seconds in, the second master `master-02`
    /// disconnects: its pinned heartbeat comes back UNKNOWN with Icinga's
    /// words, and the endpoint and its beat make one alert (16r); four
    /// minutes later it connects again (16c2's recovery).
    MasterDown,
    /// `checks-stopped`: 20 seconds in, the simulated Icinga stops running
    /// checks, as a hung checker does: the heartbeats stop and the REST
    /// query finds them old, the check rates fall to 0 and the checks go
    /// late, *Icinga runs no checks* (16a3).
    ChecksStopped,
    /// `beat-gone`: 20 seconds in, zone `fra`'s heartbeat is deleted from
    /// the configuration: a finding until its removal is confirmed (16u).
    BeatGone,
}

impl DemoFault {
    /// The fault a switch value names.
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "offline" => Some(Self::Offline),
            "auth" => Some(Self::Auth),
            "tls" => Some(Self::Tls),
            "missing-secret" => Some(Self::MissingSecret),
            "misconfigured" => Some(Self::Misconfigured),
            "pin-mismatch" => Some(Self::PinMismatch),
            "outage" => Some(Self::Outage),
            "slow" => Some(Self::Slow),
            "frozen" => Some(Self::Frozen),
            "partial" => Some(Self::Partial),
            "satellite-down" => Some(Self::SatelliteDown),
            "satellite-checks-stopped" => Some(Self::SatelliteChecksStopped),
            "beat-late" => Some(Self::BeatLate),
            "no-comments" => Some(Self::NoComments),
            "master-down" => Some(Self::MasterDown),
            "checks-stopped" => Some(Self::ChecksStopped),
            "beat-gone" => Some(Self::BeatGone),
            _ => None,
        }
    }
}

/// What the demo runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DemoOptions {
    /// An `ic_mock` scenario name (`prod-cluster`, `staging`, `lab`,
    /// `large`).
    pub(crate) scenario: String,
    /// The simulator's seed; the same seed gives the same story.
    pub(crate) seed: u64,
    /// A failure to show on purpose.
    pub(crate) fault: Option<DemoFault>,
    /// Seconds between problem storms (default: five minutes).
    pub(crate) storm_every: Option<u64>,
}

impl Default for DemoOptions {
    fn default() -> Self {
        Self {
            scenario: DEFAULT_SCENARIO.to_owned(),
            seed: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(1, |elapsed| elapsed.as_secs()),
            fault: None,
            storm_every: None,
        }
    }
}

/// What the demo server's thread sends once it listens: where, and its
/// control (the tests change the simulated Icinga through it).
pub(crate) type Ready = Result<(DemoEndpoint, MockControl), String>;

/// Where the running demo server listens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DemoEndpoint {
    /// `https://127.0.0.1:<port>`.
    pub(crate) url: String,
    /// Its self-signed certificate's SHA-256, pinned by the environment.
    pub(crate) fingerprint: String,
    /// The cluster's other nodes the demo serves, each by its own server:
    /// `prod-cluster`'s satellite `sat-ams-01` in the child zone `ams`,
    /// the environment's second URL (ENV-12).
    pub(crate) others: Vec<DemoNode>,
}

/// Another node of the demo's cluster.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DemoNode {
    /// Its node name (`sat-ams-01`).
    pub(crate) name: String,
    /// `https://127.0.0.1:<port>`.
    pub(crate) url: String,
    /// Its certificate's SHA-256, pinned by the environment.
    pub(crate) fingerprint: String,
}

/// The running demo server. Dropping it stops the server.
#[derive(Debug)]
pub(crate) struct DemoServer {
    stop: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
    password: SecretString,
    fault: Option<DemoFault>,
    /// Where it listens, once it does.
    endpoint: Option<DemoEndpoint>,
}

impl DemoServer {
    /// Whether it listens.
    pub(crate) fn is_up(&self) -> bool {
        self.endpoint.is_some()
    }

    /// Records where it listens.
    pub(crate) fn set_endpoint(&mut self, endpoint: DemoEndpoint) {
        self.endpoint = Some(endpoint);
    }

    /// The password the core gets for this server (a wrong one, or none,
    /// for the faults that need it).
    pub(crate) fn core_password(&self) -> Option<SecretString> {
        match self.fault {
            Some(DemoFault::Auth) => Some(SecretString::from("not-the-password")),
            Some(DemoFault::MissingSecret) => None,
            _ => Some(self.password.clone()),
        }
    }

    /// The URLs the environment should list, in order of preference, and
    /// the pin each should have, for `endpoint` (bent by the faults that
    /// need it). The other nodes follow the master only without a fault
    /// or with one they don't hide (`partial`, `slow`, `frozen`): a
    /// connection failure shows as such only when there is nothing to
    /// fall back on.
    pub(crate) fn environment_target(
        &self,
        endpoint: &DemoEndpoint,
    ) -> Vec<(String, Option<String>)> {
        let master = match self.fault {
            // Port 1 on the loopback: refused at once.
            Some(DemoFault::Offline) => (
                "https://127.0.0.1:1".to_owned(),
                Some(endpoint.fingerprint.clone()),
            ),
            Some(DemoFault::Tls) => (endpoint.url.clone(), None),
            Some(DemoFault::Misconfigured) => {
                (endpoint.url.clone(), Some("not-a-fingerprint".to_owned()))
            }
            Some(DemoFault::PinMismatch) => (endpoint.url.clone(), Some(other_pin())),
            _ => (endpoint.url.clone(), Some(endpoint.fingerprint.clone())),
        };
        let others = matches!(
            self.fault,
            None | Some(
                DemoFault::Partial
                    | DemoFault::Slow
                    | DemoFault::Frozen
                    | DemoFault::SatelliteDown
                    | DemoFault::NoComments
                    | DemoFault::MasterDown
                    | DemoFault::ChecksStopped
                    | DemoFault::BeatGone
                    | DemoFault::SatelliteChecksStopped
                    | DemoFault::BeatLate
            )
        );
        std::iter::once(master)
            .chain(
                endpoint
                    .others
                    .iter()
                    .filter(|_| others)
                    .map(|node| (node.url.clone(), Some(node.fingerprint.clone()))),
            )
            .collect()
    }
}

/// A well-formed pin of a certificate the demo server doesn't have, for
/// [`DemoFault::PinMismatch`].
pub(crate) fn other_pin() -> String {
    ic_config::format_fingerprint(&[0x5a; 32])
}

impl Drop for DemoServer {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        // The server stops within milliseconds; the process may be quitting,
        // so don't wait for it.
        drop(self.thread.take());
    }
}

/// Starts the demo server on its own thread. The receiver says where it
/// listens once it does (or why it couldn't start).
///
/// # Errors
///
/// The thread couldn't be started.
pub(crate) fn start(
    options: &DemoOptions,
) -> std::io::Result<(DemoServer, oneshot::Receiver<Ready>)> {
    let scenario = scenarios::by_name(&options.scenario, options.seed).unwrap_or_else(|| {
        tracing::warn!(scenario = %options.scenario, known = ?scenarios::NAMES, "unknown demo scenario; using prod-cluster");
        scenarios::prod_cluster()
    });
    // `prod-cluster` has heartbeats, as the user guide sets them up and
    // mock-up 16a draws them: zone `fra` is an HA zone of two satellites,
    // every HA zone's endpoints have a pinned beat each, every satellite
    // zone its own (six in all, topic 16).
    let scenario = if scenario.name == DEFAULT_SCENARIO {
        scenario
            .with_endpoint(SECOND_SATELLITE, "fra")
            .with_heartbeats(HEARTBEAT_EVERY)
    } else {
        scenario
    };
    let password = demo_password();
    let permissions: &[&str] = if options.fault == Some(DemoFault::NoComments) {
        NO_COMMENTS
    } else {
        &["*"]
    };
    let config = MockConfig {
        users: vec![MockUser::new(USER, &password, permissions)],
        simulation: SimulationConfig {
            enabled: true,
            seed: options.seed,
            problems_per_hour: 60.,
            storm: Some(StormConfig {
                every_ticks: options.storm_every.unwrap_or(STORM_EVERY),
                size: 24,
                duration_ticks: 90,
            }),
            ..SimulationConfig::default()
        },
        ..MockConfig::with_scenario(scenario)
    };
    // The cluster's nodes in child zones get servers of their own, each
    // serving its zone (ENV-12): `prod-cluster`'s satellite.
    let satellites: Vec<MockConfig> = child_zone_nodes(&config.scenario)
        .into_iter()
        .map(|node| MockConfig {
            scenario: config.scenario.for_node(&node),
            users: config.users.clone(),
            simulation: config.simulation.clone(),
            ..MockConfig::default()
        })
        .collect();
    let (ready, endpoint) = oneshot::channel();
    let (stop, stopped) = oneshot::channel::<()>();
    let fault = options.fault;
    let thread = std::thread::Builder::new()
        .name("icygui-demo".to_owned())
        .spawn(move || serve(config, satellites, fault, ready, stopped))?;
    tracing::info!(scenario = %options.scenario, seed = options.seed, ?fault, "starting the demo server");
    Ok((
        DemoServer {
            stop: Some(stop),
            thread: Some(thread),
            password: SecretString::from(password),
            fault,
            endpoint: None,
        },
        endpoint,
    ))
}

/// The first endpoint of `scenario` in a child zone (a zone with a
/// parent): the satellite the environment lists as its second URL
/// (`prod-cluster`'s `sat-ams-01`; its other satellite only shows on the
/// cluster health page).
fn child_zone_nodes(scenario: &ic_mock::Scenario) -> Vec<String> {
    scenario
        .endpoints
        .iter()
        .filter(|endpoint| {
            scenario
                .zones
                .iter()
                .any(|zone| zone.name == endpoint.zone && zone.parent.is_some())
        })
        .map(|endpoint| endpoint.name.clone())
        .take(1)
        .collect()
}

/// The satellite [`DemoFault::SatelliteDown`] drops (with
/// [`SECOND_SATELLITE`]).
const DROPPED_SATELLITE: &str = "sat-fra-01";
/// The demo's second satellite in zone `fra` (an HA zone, as in 16a).
const SECOND_SATELLITE: &str = "sat-fra-02";
/// How long the outage and the master's absence last before they recover.
const RECOVER_AFTER: Duration = Duration::from_mins(4);
/// How late [`DemoFault::BeatLate`]'s beat comes, and how often.
const BEAT_LATE_BY: Duration = Duration::from_secs(13);
const BEAT_LATE_EVERY: Duration = Duration::from_mins(2);
/// The master [`DemoFault::MasterDown`] drops.
const DROPPED_MASTER: &str = "master-02";
/// How often the demo's heartbeats run (seconds).
const HEARTBEAT_EVERY: f64 = 30.0;

/// Runs the mock server (and the satellites' servers) until `stopped`
/// fires.
#[expect(clippy::too_many_lines, reason = "each demo fault next to the others")]
fn serve(
    config: MockConfig,
    satellites: Vec<MockConfig>,
    fault: Option<DemoFault>,
    ready: oneshot::Sender<Ready>,
    stopped: oneshot::Receiver<()>,
) {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("icygui-demo-worker")
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = ready.send(Err(format!("no runtime for the demo server: {error}")));
            return;
        }
    };
    runtime.block_on(async move {
        match MockServer::start(config).await {
            Ok(server) => {
                let mut others = Vec::new();
                let mut nodes = Vec::new();
                for satellite in satellites {
                    let name = satellite.scenario.status.node_name.clone();
                    match MockServer::start(satellite).await {
                        Ok(node) => {
                            others.push(DemoNode {
                                name,
                                url: node.url(),
                                fingerprint: node.cert_fingerprint(),
                            });
                            nodes.push(node);
                        }
                        // The demo works without it.
                        Err(error) => {
                            tracing::warn!(%name, %error, "a demo satellite couldn't start");
                        }
                    }
                }
                let endpoint = DemoEndpoint {
                    url: server.url(),
                    fingerprint: server.cert_fingerprint(),
                    others,
                };
                tracing::info!(url = %endpoint.url, "the demo server is up");
                let control = server.control();
                match fault {
                    Some(DemoFault::Partial) => {
                        tracing::info!("the demo's master answers 503: the satellite takes over");
                        control.fail_next(u32::MAX, 503);
                    }
                    Some(DemoFault::Slow) => control.set_latency(SLOW_LATENCY),
                    Some(DemoFault::Frozen) => control.pause_simulation(),
                    Some(DemoFault::Outage) => {
                        let control = control.clone();
                        tokio::spawn(async move {
                            tokio::time::sleep(OUTAGE_AFTER).await;
                            tracing::info!("the demo's outage begins");
                            control.fail_next(u32::MAX, 503);
                            control.drop_connections();
                            tokio::time::sleep(RECOVER_AFTER).await;
                            tracing::info!("the demo's outage ends");
                            control.fail_next(0, 503);
                        });
                    }
                    Some(DemoFault::MasterDown) => {
                        let control = control.clone();
                        tokio::spawn(async move {
                            tokio::time::sleep(OUTAGE_AFTER).await;
                            drop_node(&control, DROPPED_MASTER, false);
                            tokio::time::sleep(RECOVER_AFTER).await;
                            drop_node(&control, DROPPED_MASTER, true);
                        });
                    }
                    Some(DemoFault::SatelliteDown) => {
                        let control = control.clone();
                        tokio::spawn(async move {
                            tokio::time::sleep(OUTAGE_AFTER).await;
                            drop_node(&control, DROPPED_SATELLITE, false);
                            drop_node(&control, SECOND_SATELLITE, false);
                        });
                    }
                    Some(DemoFault::SatelliteChecksStopped) => {
                        let control = control.clone();
                        tokio::spawn(async move {
                            tokio::time::sleep(OUTAGE_AFTER).await;
                            drop_node(&control, SECOND_SATELLITE, false);
                            tracing::info!(
                                node = DROPPED_SATELLITE,
                                "a demo satellite's checker hangs"
                            );
                            control.stop_checks_on(DROPPED_SATELLITE);
                        });
                    }
                    Some(DemoFault::BeatLate) => {
                        let control = control.clone();
                        tokio::spawn(async move {
                            loop {
                                tokio::time::sleep(BEAT_LATE_EVERY).await;
                                control.delay_realtime("icygui-hb-ams", "beat", BEAT_LATE_BY);
                            }
                        });
                    }
                    Some(DemoFault::ChecksStopped) => {
                        let control = control.clone();
                        tokio::spawn(async move {
                            tokio::time::sleep(OUTAGE_AFTER).await;
                            tracing::info!("the demo's Icinga stops running checks");
                            control.stop_checks();
                        });
                    }
                    Some(DemoFault::BeatGone) => {
                        let control = control.clone();
                        tokio::spawn(async move {
                            tokio::time::sleep(OUTAGE_AFTER).await;
                            match control.remove_service("icygui-hb-fra", "beat") {
                                Ok(()) => tracing::info!("the demo's fra heartbeat is deleted"),
                                Err(error) => tracing::warn!(%error, "no fra heartbeat to delete"),
                            }
                        });
                    }
                    _ => {}
                }
                let _ = ready.send(Ok((endpoint, control)));
                // A dropped sender (the app is gone) stops it as well.
                let _ = stopped.await;
                server.shutdown().await;
                for node in nodes {
                    node.shutdown().await;
                }
            }
            Err(error) => {
                let _ = ready.send(Err(error.to_string()));
            }
        }
    });
}

/// Disconnects (or, with `connected`, connects again) the demo's node
/// `node`.
fn drop_node(control: &MockControl, node: &str, connected: bool) {
    match control.set_endpoint_connected(node, connected) {
        Ok(()) if connected => tracing::info!(%node, "a demo node connects again"),
        Ok(()) => tracing::info!(%node, "a demo node drops out"),
        Err(error) => tracing::warn!(%error, "the demo has no such node"),
    }
}

/// A password for this run only: the demo server listens on 127.0.0.1.
fn demo_password() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    format!("demo-{nanos:x}-{:x}", std::process::id())
}

/// Passwords for the demo's environments, in memory only: the demo
/// servers' (new every run), and any the environment editor stores while
/// the demo runs. The user's keychain is never touched.
#[derive(Debug, Default)]
pub(crate) struct DemoSecrets {
    passwords: Mutex<HashMap<String, SecretString>>,
}

impl DemoSecrets {
    /// Sets (or, with `None`, removes) `account`'s password.
    pub(crate) fn put(&self, account: &str, password: Option<SecretString>) {
        let mut passwords = self
            .passwords
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        match password {
            Some(password) => {
                passwords.insert(account.to_owned(), password);
            }
            None => {
                passwords.remove(account);
            }
        }
    }
}

impl SecretStore for DemoSecrets {
    fn get(&self, account: &str) -> Result<Option<SecretString>, SecretError> {
        Ok(self
            .passwords
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(account)
            .cloned())
    }

    fn set(&self, account: &str, secret: &SecretString) -> Result<(), SecretError> {
        self.put(account, Some(secret.clone()));
        Ok(())
    }

    fn delete(&self, account: &str) -> Result<(), SecretError> {
        self.put(account, None);
        Ok(())
    }
}

/// Whether `environment_id` is one of the demo's own environments (the
/// simulated `prod-cluster`, `staging` and `lab`, and those
/// `ICYGUI_DEMO_ENVIRONMENTS` adds).
pub(crate) fn is_built_in(environment_id: &str) -> bool {
    [ENVIRONMENT_ID, STAGING_ID, LAB_ID].contains(&environment_id)
        || environment_id.starts_with(MORE_PREFIX)
}

/// The ids of the environments `ICYGUI_DEMO_ENVIRONMENTS` adds start so.
const MORE_PREFIX: &str = "icygui-demo-more-";

/// The names of the environments `ICYGUI_DEMO_ENVIRONMENTS` adds, in
/// order.
const MORE_NAMES: [&str; 8] = [
    "dev-cluster",
    "qa",
    "edge-ams",
    "edge-fra",
    "backup",
    "office",
    "monitoring-eu",
    "monitoring-us",
];

/// Keeps the first `count` demo environments of `config` (1 to 11;
/// development: one environment, or many in the switcher and the
/// notification centre's scopes). Those past the third serve `lab` and
/// storm every 30 minutes.
pub(crate) fn set_count(config: &mut Config, count: usize) {
    let count = count.clamp(1, 3 + MORE_NAMES.len());
    config.environments.truncate(count);
    for (index, name) in MORE_NAMES.iter().take(count.saturating_sub(3)).enumerate() {
        let id = format!("{MORE_PREFIX}{index}");
        let groups = stable_ids(&id, ic_config::default_groups());
        config.environments.push(environment(&id, name, groups));
    }
}

/// The demo server to run for the environment `environment_id`, if it is
/// one of the demo's: `prod-cluster` serves `prod_cluster` (the scenario,
/// seed and fault chosen at start); `staging` and `lab` serve those
/// scenarios without faults. Environments added while the demo runs are
/// real ones (`None`).
///
/// Every demo environment runs from the start and notifies in the
/// background (PLAN.md D2); `staging` and `lab` storm three and six times
/// less often than `prod-cluster` (every 15 and 30 minutes by default;
/// `ICYGUI_DEMO_STORM` sets `prod-cluster`'s), so their storms don't come
/// all at once.
pub(crate) fn options_for(environment_id: &str, prod_cluster: &DemoOptions) -> Option<DemoOptions> {
    let every = prod_cluster.storm_every.unwrap_or(STORM_EVERY);
    let (scenario, storm_every) = match environment_id {
        ENVIRONMENT_ID => return Some(prod_cluster.clone()),
        STAGING_ID => ("staging", 3 * every),
        LAB_ID => ("lab", 6 * every),
        more if more.starts_with(MORE_PREFIX) => ("lab", 6 * every),
        _ => return None,
    };
    Some(DemoOptions {
        scenario: scenario.to_owned(),
        seed: prod_cluster.seed,
        fault: None,
        storm_every: Some(storm_every),
    })
}

/// The demo's settings: three environments to switch between (ENV-01).
/// `prod-cluster` (active) has the design's folders of dashboards;
/// `staging` and `lab` start with the default dashboards of a new
/// environment (DASH-05). Their URLs and pins are filled in when their
/// servers are up (`AppState::set_demo_servers`): `prod-cluster` lists its
/// master and its satellite (ENV-12).
pub(crate) fn config() -> Config {
    Config {
        active_environment: Some(ENVIRONMENT_ID.to_owned()),
        environments: vec![
            environment(ENVIRONMENT_ID, "prod-cluster", groups()),
            environment(
                STAGING_ID,
                "staging",
                stable_ids(STAGING_ID, ic_config::default_groups()),
            ),
            environment(
                LAB_ID,
                "lab",
                stable_ids(LAB_ID, ic_config::default_groups()),
            ),
        ],
        ..Config::default()
    }
}

/// A demo environment (its URL is replaced once its server listens).
fn environment(id: &str, name: &str, groups: Vec<DashboardGroup>) -> Environment {
    Environment {
        id: id.to_owned(),
        name: name.to_owned(),
        urls: vec![ic_config::ApiUrl::new("https://127.0.0.1:5665")],
        auth: AuthConfig::Basic {
            username: USER.to_owned(),
        },
        tls: TlsConfig::default(),
        // The person the demo plays, as the mock-ups draw them: their
        // downtimes and comments are *mine* in the lists (topic 07).
        author: Some("j.berg".to_owned()),
        groups,
        notifications: NotificationSettings::default(),
        ..Environment::default()
    }
}

/// `groups` with ids derived from their names, so the demo's dashboards
/// keep their ids from run to run.
fn stable_ids(environment_id: &str, mut groups: Vec<DashboardGroup>) -> Vec<DashboardGroup> {
    for group in &mut groups {
        group.id = format!("{environment_id}-{}", slug(&group.name));
        for dashboard in &mut group.dashboards {
            dashboard.id = format!("{}-{}", group.id, slug(&dashboard.name));
            for (index, view) in dashboard.views.iter_mut().enumerate() {
                view.id = format!("{}-view-{index}", dashboard.id);
            }
        }
    }
    groups
}

/// One dashboard of the demo (a single view; `databases` and `fleet` have
/// several, see [`databases_views`] and [`fleet_views`]).
struct Spec {
    name: &'static str,
    kind: ObjectKind,
    filter: &'static str,
    problems_only: bool,
    hide_handled: bool,
    group_by: GroupBy,
}

impl Spec {
    /// The dashboard's only view: handled hidden as the settings say, or
    /// shown.
    fn view(&self) -> View {
        let mut view = View {
            object_kind: self.kind,
            filter: self.filter.to_owned(),
            problems_only: self.problems_only,
            handled: if self.hide_handled {
                HandledSetting::SETTINGS
            } else {
                HandledSetting::SHOW
            },
            sort: Sort::default(),
            ..View::default()
        };
        view.set_grouping(self.group_by);
        view
    }
}

/// The database hosts' roles, as an Icinga filter list.
const DB_ROLES: &str = "[\"postgres\", \"mysql\"]";

/// `overview / databases` as topic 04 draws it: summary tiles per
/// database role, the failing database services, a view with nothing to
/// show, and the database hosts' events (the stream's defaults: hard
/// states, no recoveries; [`recent_events`] gives it a history).
fn databases_views() -> Vec<View> {
    vec![
        View {
            name: "clusters".to_owned(),
            display: ViewDisplay::SummaryTiles,
            // Four roles, a tile each (4a).
            filter: "host.vars.role in [\"postgres\", \"mysql\", \"mongodb\", \"redis\"]"
                .to_owned(),
            problems_only: false,
            groups: ViewGroups {
                by: GroupSource::CustomVar,
                custom_var: "role".to_owned(),
                order: GroupOrder::Name,
                ..ViewGroups::default()
            },
            ..View::default()
        },
        View {
            name: "failing services".to_owned(),
            filter: format!("host.vars.role in {DB_ROLES} && service.problem"),
            ..View::default()
        },
        // Nothing to show (4a's empty view): the replication slots are
        // healthy.
        View {
            name: "replication lag".to_owned(),
            filter: "service.name == \"pg-replication-slots\" && service.problem".to_owned(),
            ..View::default()
        },
        View {
            name: "db events".to_owned(),
            display: ViewDisplay::EventStream,
            filter: format!("host.vars.role in {DB_ROLES}"),
            stream: StreamOptions::default(),
            ..View::default()
        },
    ]
}

/// The database hosts' last hour, as 4a's event stream shows it: written
/// to `prod-cluster`'s event log before its engine starts, so the stream
/// (and the panes' history) isn't empty until something happens. `now`:
/// Unix seconds.
pub(crate) fn recent_events(now: f64) -> Vec<LogEntry> {
    use ic_core::LogKind;
    use ic_model::{CheckableState, StateType};
    let state =
        |minutes: f64, host: &str, service: &str, state: ServiceState, text: &str| LogEntry {
            at: Timestamp::from_unix_seconds(now - minutes * 60.),
            object: ObjectKey::service(host, service),
            kind: LogKind::State {
                state: CheckableState::Service(state),
                state_type: StateType::Hard,
            },
            text: text.to_owned(),
            author: None,
        };
    let by = |minutes: f64, host: &str, service: &str, kind: LogKind, author: &str, text: &str| {
        LogEntry {
            at: Timestamp::from_unix_seconds(now - minutes * 60.),
            object: ObjectKey::service(host, service),
            kind,
            text: text.to_owned(),
            author: Some(author.to_owned()),
        }
    };
    // Oldest first, as the engine records them.
    vec![
        state(
            70.,
            "db-mysql-03",
            "mysql-replication",
            ServiceState::Unknown,
            "UNKNOWN - connection refused",
        ),
        state(
            58.,
            "db-prod-05",
            "pg-bloat",
            ServiceState::Warning,
            "WARNING - check_postgres degraded",
        ),
        by(
            46.,
            "db-prod-03",
            "postgres-replication",
            LogKind::CommentAdded,
            "j.berg",
            "failover drill on db-prod-01 at 15:00",
        ),
        by(
            33.,
            "db-prod-05",
            "pg-bloat",
            LogKind::DowntimeStarted,
            "dba-oncall",
            "VACUUM FULL on orders_archive, flexible 2h",
        ),
        state(
            30.,
            "db-mysql-02",
            "mysql-replication",
            ServiceState::Ok,
            "OK - replica in sync, lag 0s",
        ),
        by(
            26.,
            "db-prod-01",
            "pg-autovacuum",
            LogKind::AcknowledgementSet,
            "dba-oncall",
            "vacuum running on orders, about 30 minutes",
        ),
        state(
            19.,
            "db-prod-03",
            "pg-connections",
            ServiceState::Warning,
            "WARNING - 182 of 200 per-db limit (orders)",
        ),
        state(
            14.,
            "db-prod-03",
            "postgres-replication",
            ServiceState::Critical,
            "CRITICAL - standby lag 412s (> 300s)",
        ),
        state(
            8.,
            "db-prod-01",
            "load",
            ServiceState::Warning,
            "WARNING - load average 14.2, 12.8, 11.1",
        ),
    ]
}

/// `platform / fleet` as topic 05 draws it: every host as a square by host
/// group above the service problems.
fn fleet_views() -> Vec<View> {
    vec![
        View {
            name: "hosts by group".to_owned(),
            display: ViewDisplay::HostGroupGrid,
            object_kind: ObjectKind::Hosts,
            ..View::default()
        },
        View {
            name: "service problems".to_owned(),
            filter: "service.problem && !service.handled".to_owned(),
            ..View::default()
        },
    ]
}

const fn spec(
    name: &'static str,
    kind: ObjectKind,
    filter: &'static str,
    problems_only: bool,
    hide_handled: bool,
) -> Spec {
    Spec {
        name,
        kind,
        filter,
        problems_only,
        hide_handled,
        group_by: GroupBy::None,
    }
}

/// The design's folders, with filters in Icinga's language over the
/// scenario's custom variables.
const GROUPS: &[(&str, &[Spec])] = &[
    (
        "overview",
        &[
            spec("overview", ObjectKind::Services, "", true, true),
            spec(
                "production",
                ObjectKind::Services,
                "host.vars.env == \"prod\"",
                true,
                false,
            ),
            Spec {
                group_by: GroupBy::Host,
                ..spec(
                    "databases",
                    ObjectKind::Services,
                    "host.vars.role in [\"postgres\", \"mysql\", \"mongodb\", \"redis\"]",
                    true,
                    false,
                )
            },
            spec("host problems", ObjectKind::Hosts, "", true, false),
            // Topic 10's three hosts saved as a dashboard (10i): a grouped
            // list by host with every service, paged by count (10h).
            Spec {
                group_by: GroupBy::Host,
                ..spec(
                    "db primaries",
                    ObjectKind::Services,
                    "host.name in [\"db-prod-01\", \"db-prod-02\", \"db-prod-03\"]",
                    false,
                    true,
                )
            },
        ],
    ),
    (
        "platform",
        &[
            spec(
                "network",
                ObjectKind::Hosts,
                "host.vars.role in [\"switch\", \"edge\", \"haproxy\", \"vpn\"]",
                false,
                false,
            ),
            spec(
                "kubernetes",
                ObjectKind::Services,
                "match(\"k8s-*\", host.vars.role)",
                true,
                true,
            ),
            spec(
                "certificates",
                ObjectKind::Services,
                "match(\"*cert*\", service.name) || service.name == \"http-tls\"",
                false,
                false,
            ),
            spec("all services", ObjectKind::Services, "", false, false),
            spec("fleet", ObjectKind::Hosts, "", true, true),
        ],
    ),
    (
        "lab",
        &[spec(
            "sandbox",
            ObjectKind::Services,
            "host.vars.env == \"lab\"",
            true,
            true,
        )],
    ),
];

fn groups() -> Vec<DashboardGroup> {
    let mut groups: Vec<DashboardGroup> = GROUPS
        .iter()
        .map(|(name, dashboards)| {
            let group_id = format!("demo-{}", slug(name));
            DashboardGroup {
                id: group_id.clone(),
                name: (*name).to_owned(),
                collapsed: false,
                notifications: ScopeSetting::Inherit,
                dashboards: dashboards
                    .iter()
                    .map(|spec| {
                        let id = format!("{group_id}-{}", slug(spec.name));
                        let views = match spec.name {
                            "databases" => databases_views(),
                            "fleet" => fleet_views(),
                            _ => vec![spec.view()],
                        };
                        Dashboard {
                            views: views
                                .into_iter()
                                .enumerate()
                                .map(|(index, view)| View {
                                    id: format!("{id}-view-{index}"),
                                    ..view
                                })
                                .collect(),
                            id,
                            name: spec.name.to_owned(),
                            notifications: ScopeSetting::Inherit,
                            mark: match spec.name {
                                // A chosen icon instead of the state's dot.
                                "network" => ic_config::SidebarMark::Icon("network".to_owned()),
                                _ => ic_config::SidebarMark::Auto,
                            },
                        }
                    })
                    .collect(),
            }
        })
        .collect();
    groups.insert(1, dba_group());
    groups
}

/// The database team's folder (topic 14, round 5): their problems,
/// handling and downtimes stacked on one dashboard (rows compact on the
/// problems, the downtimes as a timeline), and a full page of each. The
/// handling page's mark is a chosen icon, the downtimes page's its kind's.
fn dba_group() -> DashboardGroup {
    use ic_config::{DowntimesMode, RowDensity, ThreadChip, ThreadOptions};
    let group_id = "demo-dba".to_owned();
    let team = format!("host.vars.role in {DB_ROLES}");
    let view = |dashboard: &str, index: usize, view: View| View {
        id: format!("{group_id}-{dashboard}-view-{index}"),
        ..view
    };
    let dashboard = |name: &str, mark: ic_config::SidebarMark, views: Vec<View>| {
        let id = format!("{group_id}-{}", slug(name));
        Dashboard {
            views: views
                .into_iter()
                .enumerate()
                .map(|(index, one)| view(&slug(name), index, one))
                .collect(),
            id,
            name: name.to_owned(),
            notifications: ScopeSetting::Inherit,
            mark,
        }
    };
    let problems = View {
        name: "problems".to_owned(),
        filter: format!("{team} && service.problem"),
        density: Some(RowDensity::Compact),
        ..View::default()
    };
    let handling = View {
        name: "handling".to_owned(),
        display: ViewDisplay::Handling,
        filter: team.clone(),
        ..View::default()
    };
    let downtimes = View {
        name: "downtimes".to_owned(),
        display: ViewDisplay::Downtimes,
        filter: team.clone(),
        threads: ThreadOptions {
            mode: DowntimesMode::Timeline,
            ..ThreadOptions::default()
        },
        ..View::default()
    };
    DashboardGroup {
        id: group_id.clone(),
        name: "dba".to_owned(),
        collapsed: false,
        notifications: ScopeSetting::Inherit,
        dashboards: vec![
            dashboard(
                "dba",
                ic_config::SidebarMark::Auto,
                vec![problems, handling.clone(), downtimes.clone()],
            ),
            dashboard(
                "dba handling",
                ic_config::SidebarMark::Icon("users".to_owned()),
                vec![View {
                    threads: ThreadOptions {
                        chip: ThreadChip::All,
                        ..ThreadOptions::default()
                    },
                    ..handling
                }],
            ),
            dashboard(
                "dba downtimes",
                ic_config::SidebarMark::Auto,
                vec![downtimes],
            ),
        ],
    }
}

/// `host problems` → `host-problems`.
fn slug(name: &str) -> String {
    name.replace(' ', "-")
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn the_demo_has_three_valid_environments() {
        let config = config();
        let names: Vec<_> = config
            .environments
            .iter()
            .map(|environment| environment.name.as_str())
            .collect();
        assert_eq!(names, ["prod-cluster", "staging", "lab"]);
        assert_eq!(config.active_environment.as_deref(), Some(ENVIRONMENT_ID));
        let environment = &config.environments[0];
        let names: Vec<_> = environment
            .groups
            .iter()
            .map(|group| group.name.as_str())
            .collect();
        assert_eq!(names, ["overview", "dba", "platform", "lab"]);
        let mut ids = HashSet::new();
        for environment in &config.environments {
            for group in &environment.groups {
                assert!(ids.insert(group.id.clone()));
                for dashboard in &group.dashboards {
                    assert!(ids.insert(dashboard.id.clone()), "{}", dashboard.id);
                    for view in &dashboard.views {
                        ic_filter::Filter::parse(&view.filter)
                            .unwrap_or_else(|error| panic!("{}: {error:?}", dashboard.name));
                    }
                }
            }
        }
        assert_eq!(
            config.environments[1].groups[0].dashboards.len(),
            3,
            "the defaults"
        );
        assert_eq!(config, super::config(), "the same ids every run");
        assert!(config.validate().is_empty(), "{:?}", config.validate());
    }

    #[test]
    fn the_demo_runs_one_to_eleven_environments() {
        let mut one = config();
        set_count(&mut one, 1);
        assert_eq!(one.environments.len(), 1);
        assert_eq!(one.active_environment.as_deref(), Some(ENVIRONMENT_ID));
        assert!(one.validate().is_empty());
        let mut config = config();
        set_count(&mut config, 20);
        assert_eq!(
            config.environments.len(),
            3 + MORE_NAMES.len(),
            "at most eleven"
        );
        let extra = &config.environments[3];
        assert_eq!(extra.name, "dev-cluster");
        assert!(is_built_in(&extra.id));
        let options = options_for(&extra.id, &DemoOptions::default()).unwrap();
        assert_eq!(options.scenario, "lab");
        assert!(config.validate().is_empty(), "{:?}", config.validate());
    }

    #[test]
    fn each_demo_environment_has_its_scenario() {
        let prod = DemoOptions {
            scenario: "large".to_owned(),
            seed: 9,
            fault: Some(DemoFault::Slow),
            storm_every: Some(20),
        };
        assert_eq!(options_for(ENVIRONMENT_ID, &prod), Some(prod.clone()));
        let staging = options_for(STAGING_ID, &prod).unwrap();
        assert_eq!(
            (
                staging.scenario.as_str(),
                staging.seed,
                staging.fault,
                staging.storm_every
            ),
            ("staging", 9, None, Some(60))
        );
        let lab = options_for(LAB_ID, &prod).unwrap();
        assert_eq!((lab.scenario.as_str(), lab.storm_every), ("lab", Some(120)));
        // Without `ICYGUI_DEMO_STORM`: every 15 and 30 minutes.
        let default = DemoOptions::default();
        assert_eq!(
            options_for(STAGING_ID, &default).unwrap().storm_every,
            Some(900)
        );
        assert_eq!(
            options_for(LAB_ID, &default).unwrap().storm_every,
            Some(1800)
        );
        assert_eq!(options_for("added-in-the-editor", &prod), None);
    }

    #[test]
    fn the_secret_store_keeps_passwords_in_memory() {
        let secrets = DemoSecrets::default();
        secrets.put(ENVIRONMENT_ID, Some(SecretString::from("p")));
        assert!(secrets.get(ENVIRONMENT_ID).unwrap().is_some());
        assert!(secrets.get("someone-else").unwrap().is_none());
        secrets
            .set("someone-else", &SecretString::from("x"))
            .unwrap();
        assert!(secrets.get("someone-else").unwrap().is_some());
        secrets.delete("someone-else").unwrap();
        assert!(secrets.get("someone-else").unwrap().is_none());
    }

    #[test]
    fn faults_bend_the_password_and_the_target() {
        let server = |fault| DemoServer {
            stop: None,
            thread: None,
            password: SecretString::from("right"),
            fault,
            endpoint: None,
        };
        let endpoint = DemoEndpoint {
            url: "https://127.0.0.1:4000".to_owned(),
            fingerprint: "AB".to_owned(),
            others: vec![DemoNode {
                name: "sat-ams-01".to_owned(),
                url: "https://127.0.0.1:4001".to_owned(),
                fingerprint: "CD".to_owned(),
            }],
        };
        let password = |fault| {
            server(fault)
                .core_password()
                .map(|secret| secrecy::ExposeSecret::expose_secret(&secret).to_owned())
        };
        assert_eq!(password(None).as_deref(), Some("right"));
        assert_eq!(
            password(Some(DemoFault::Auth)).as_deref(),
            Some("not-the-password")
        );
        assert_eq!(password(Some(DemoFault::MissingSecret)), None);
        assert_eq!(
            server(None).environment_target(&endpoint),
            [
                (endpoint.url.clone(), Some("AB".to_owned())),
                ("https://127.0.0.1:4001".to_owned(), Some("CD".to_owned()))
            ],
            "the master, then the satellite"
        );
        assert_eq!(
            server(Some(DemoFault::Partial))
                .environment_target(&endpoint)
                .len(),
            2
        );
        assert_eq!(
            server(Some(DemoFault::Tls)).environment_target(&endpoint),
            [(endpoint.url.clone(), None)],
            "nothing to fall back on"
        );
        assert!(
            server(Some(DemoFault::Offline)).environment_target(&endpoint)[0]
                .0
                .ends_with(":1")
        );
        assert_eq!(DemoFault::parse("partial"), Some(DemoFault::Partial));
        assert_eq!(
            DemoFault::parse(" Missing-Secret "),
            Some(DemoFault::MissingSecret)
        );
        assert_eq!(DemoFault::parse("outage"), Some(DemoFault::Outage));
        assert_eq!(DemoFault::parse("frozen"), Some(DemoFault::Frozen));
        assert_eq!(DemoFault::parse("master-down"), Some(DemoFault::MasterDown));
        assert_eq!(
            DemoFault::parse("checks-stopped"),
            Some(DemoFault::ChecksStopped)
        );
        assert_eq!(DemoFault::parse("beat-gone"), Some(DemoFault::BeatGone));
        assert_eq!(
            DemoFault::parse("satellite-checks-stopped"),
            Some(DemoFault::SatelliteChecksStopped)
        );
        assert_eq!(DemoFault::parse("beat-late"), Some(DemoFault::BeatLate));
        assert_eq!(
            DemoFault::parse("satellite-down"),
            Some(DemoFault::SatelliteDown)
        );
        assert_eq!(
            DemoFault::parse("pin-mismatch"),
            Some(DemoFault::PinMismatch)
        );
        assert_eq!(
            server(Some(DemoFault::PinMismatch)).environment_target(&endpoint),
            [(endpoint.url.clone(), Some(other_pin()))]
        );
        assert!(ic_config::parse_fingerprint(&other_pin()).is_ok());
        assert_eq!(DemoFault::parse("nope"), None);
    }

    #[test]
    fn the_password_never_shows_in_logs() {
        let server = DemoServer {
            stop: None,
            thread: None,
            password: SecretString::from("hunter2-secret"),
            fault: None,
            endpoint: None,
        };
        let secrets = DemoSecrets::default();
        secrets.put(ENVIRONMENT_ID, Some(SecretString::from("hunter2-secret")));
        let text = format!("{server:?} {secrets:?}");
        assert!(!text.contains("hunter2"), "{text}");
    }

    #[test]
    fn passwords_differ_per_run() {
        assert_ne!(demo_password(), "");
        assert!(demo_password().starts_with("demo-"));
    }

    #[test]
    fn the_server_starts_and_stops() {
        let options = DemoOptions {
            scenario: "lab".to_owned(),
            seed: 7,
            fault: None,
            storm_every: None,
        };
        let (server, endpoint) = start(&options).unwrap();
        let (endpoint, _control) = futures::executor::block_on(endpoint).unwrap().unwrap();
        assert!(
            endpoint.url.starts_with("https://127.0.0.1:"),
            "{endpoint:?}"
        );
        assert_eq!(endpoint.fingerprint.len(), 95, "colon hex SHA-256");
        assert!(endpoint.others.is_empty(), "lab is a single master");
        drop(server);
    }

    #[test]
    fn prod_cluster_serves_its_satellite_too() {
        let options = DemoOptions {
            scenario: "prod-cluster".to_owned(),
            seed: 7,
            fault: None,
            storm_every: None,
        };
        let (server, endpoint) = start(&options).unwrap();
        let (endpoint, _control) = futures::executor::block_on(endpoint).unwrap().unwrap();
        assert_eq!(endpoint.others.len(), 1);
        assert_eq!(endpoint.others[0].name, "sat-ams-01");
        assert_ne!(endpoint.others[0].url, endpoint.url);
        assert_eq!(child_zone_nodes(&scenarios::prod_cluster()), ["sat-ams-01"]);
        drop(server);
    }
}
