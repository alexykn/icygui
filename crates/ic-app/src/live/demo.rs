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
//! `ICYGUI_DEMO_DASHBOARD`, `ICYGUI_DEMO_OPEN`, `ICYGUI_DEMO_FAULT`) are
//! read by `crate::dev`; [`DemoFault`] makes the demo show the
//! connection's failure states, for screenshots and tests.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures::channel::oneshot;
use ic_config::{
    AuthConfig, CONFIG_VERSION, Config, Dashboard, DashboardGroup, Environment, General, GroupBy,
    ObjectKind, Sort, TlsConfig, View,
};
use ic_core::ports::{SecretError, SecretStore};
use ic_mock::{
    MockConfig, MockControl, MockServer, MockUser, SimulationConfig, StormConfig, scenarios,
};
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
    /// 503 (the connection is lost and retried).
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
            None | Some(DemoFault::Partial | DemoFault::Slow | DemoFault::Frozen)
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
    let password = demo_password();
    let config = MockConfig {
        users: vec![MockUser::new(USER, &password, &["*"])],
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

/// The endpoints of `scenario` in a child zone (a zone with a parent):
/// the satellites.
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
        .collect()
}

/// Runs the mock server (and the satellites' servers) until `stopped`
/// fires.
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
/// simulated `prod-cluster`, `staging` and `lab`).
pub(crate) fn is_built_in(environment_id: &str) -> bool {
    [ENVIRONMENT_ID, STAGING_ID, LAB_ID].contains(&environment_id)
}

/// The demo server to run for the environment `environment_id`, if it is
/// one of the demo's: `prod-cluster` serves `prod_cluster` (the scenario,
/// seed and fault chosen at start); `staging` and `lab` serve those
/// scenarios without faults. Environments added while the demo runs are
/// real ones (`None`).
pub(crate) fn options_for(environment_id: &str, prod_cluster: &DemoOptions) -> Option<DemoOptions> {
    let scenario = match environment_id {
        ENVIRONMENT_ID => return Some(prod_cluster.clone()),
        STAGING_ID => "staging",
        LAB_ID => "lab",
        _ => return None,
    };
    Some(DemoOptions {
        scenario: scenario.to_owned(),
        seed: prod_cluster.seed,
        fault: None,
        storm_every: None,
    })
}

/// The demo's settings: three environments to switch between (ENV-01).
/// `prod-cluster` (active) has the design's folders of dashboards;
/// `staging` and `lab` start with the default dashboards of a new
/// environment (DASH-05). Their URLs and pins are filled in when their
/// servers are up (`AppState::set_demo_servers`): `prod-cluster` lists its
/// master and its satellite (ENV-12).
pub(crate) fn config() -> Config {
    let environment = |id: &str, name: &str, groups: Vec<DashboardGroup>| Environment {
        id: id.to_owned(),
        name: name.to_owned(),
        // Replaced once the demo server listens.
        urls: vec![ic_config::ApiUrl::new("https://127.0.0.1:5665")],
        auth: AuthConfig::Basic {
            username: USER.to_owned(),
        },
        tls: TlsConfig::default(),
        author: Some("demo".to_owned()),
        groups,
        notifications: NotificationSettings::default(),
    };
    Config {
        version: CONFIG_VERSION,
        general: General::default(),
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
    }
}

/// `groups` with ids derived from their names, so the demo's dashboards
/// keep their ids from run to run.
fn stable_ids(environment_id: &str, mut groups: Vec<DashboardGroup>) -> Vec<DashboardGroup> {
    for group in &mut groups {
        group.id = format!("{environment_id}-{}", slug(&group.name));
        for dashboard in &mut group.dashboards {
            dashboard.id = format!("{}-{}", group.id, slug(&dashboard.name));
        }
    }
    groups
}

/// One dashboard of the demo.
struct Spec {
    name: &'static str,
    kind: ObjectKind,
    filter: &'static str,
    problems_only: bool,
    hide_handled: bool,
    group_by: GroupBy,
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
    GROUPS
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
                    .map(|spec| Dashboard {
                        id: format!("{group_id}-{}", slug(spec.name)),
                        name: spec.name.to_owned(),
                        view: View {
                            object_kind: spec.kind,
                            filter: spec.filter.to_owned(),
                            problems_only: spec.problems_only,
                            hide_handled: spec.hide_handled,
                            sort: Sort::default(),
                            group_by: spec.group_by,
                        },
                        notifications: ScopeSetting::Inherit,
                    })
                    .collect(),
            }
        })
        .collect()
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
        assert_eq!(names, ["overview", "platform", "lab"]);
        let mut ids = HashSet::new();
        for environment in &config.environments {
            for group in &environment.groups {
                assert!(ids.insert(group.id.clone()));
                for dashboard in &group.dashboards {
                    assert!(ids.insert(dashboard.id.clone()), "{}", dashboard.id);
                    ic_filter::Filter::parse(&dashboard.view.filter)
                        .unwrap_or_else(|error| panic!("{}: {error:?}", dashboard.name));
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
            (staging.scenario.as_str(), staging.seed, staging.fault),
            ("staging", 9, None)
        );
        assert_eq!(options_for(LAB_ID, &prod).unwrap().scenario, "lab");
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
