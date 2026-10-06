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

use std::sync::Arc;
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
/// The node name the demo's Icinga reports (the design's).
pub(crate) const ENDPOINT: &str = "master-01";
/// The demo server's API user.
pub(crate) const USER: &str = "icygui";
/// The scenario served unless `ICYGUI_DEMO_SCENARIO` names another.
pub(crate) const DEFAULT_SCENARIO: &str = "prod-cluster";

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
    /// `outage`: 20 seconds in, the server drops every stream and answers
    /// 503 (the connection is lost and retried).
    Outage,
    /// `slow`: every answer takes 0.9 s (the load's progress shows).
    Slow,
    /// `frozen`: the simulated Icinga stops checking, so checks become
    /// late (within about two minutes for the one-minute checks).
    Frozen,
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
            "outage" => Some(Self::Outage),
            "slow" => Some(Self::Slow),
            "frozen" => Some(Self::Frozen),
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
}

impl Default for DemoOptions {
    fn default() -> Self {
        Self {
            scenario: DEFAULT_SCENARIO.to_owned(),
            seed: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(1, |elapsed| elapsed.as_secs()),
            fault: None,
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
}

/// The running demo server. Dropping it stops the server.
#[derive(Debug)]
pub(crate) struct DemoServer {
    stop: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
    password: SecretString,
    fault: Option<DemoFault>,
}

impl DemoServer {
    /// The secret store the core reads the demo user's password from (a
    /// wrong one, or none, for the faults that need it).
    pub(crate) fn secrets(&self) -> Arc<dyn SecretStore> {
        let password = match self.fault {
            Some(DemoFault::Auth) => Some(SecretString::from("not-the-password")),
            Some(DemoFault::MissingSecret) => None,
            _ => Some(self.password.clone()),
        };
        Arc::new(DemoSecrets { password })
    }

    /// Where the environment should point, and the pin it should have,
    /// for `endpoint` (both bent by the faults that need it).
    pub(crate) fn environment_target(&self, endpoint: &DemoEndpoint) -> (String, Option<String>) {
        match self.fault {
            // Port 1 on the loopback: refused at once.
            Some(DemoFault::Offline) => (
                "https://127.0.0.1:1".to_owned(),
                Some(endpoint.fingerprint.clone()),
            ),
            Some(DemoFault::Tls) => (endpoint.url.clone(), None),
            Some(DemoFault::Misconfigured) => {
                (endpoint.url.clone(), Some("not-a-fingerprint".to_owned()))
            }
            _ => (endpoint.url.clone(), Some(endpoint.fingerprint.clone())),
        }
    }
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
                every_ticks: 300,
                size: 24,
                duration_ticks: 90,
            }),
            ..SimulationConfig::default()
        },
        ..MockConfig::with_scenario(scenario)
    };
    let (ready, endpoint) = oneshot::channel();
    let (stop, stopped) = oneshot::channel::<()>();
    let fault = options.fault;
    let thread = std::thread::Builder::new()
        .name("icygui-demo".to_owned())
        .spawn(move || serve(config, fault, ready, stopped))?;
    tracing::info!(scenario = %options.scenario, seed = options.seed, ?fault, "starting the demo server");
    Ok((
        DemoServer {
            stop: Some(stop),
            thread: Some(thread),
            password: SecretString::from(password),
            fault,
        },
        endpoint,
    ))
}

/// Runs the mock server until `stopped` fires.
fn serve(
    config: MockConfig,
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
                let endpoint = DemoEndpoint {
                    url: server.url(),
                    fingerprint: server.cert_fingerprint(),
                };
                tracing::info!(url = %endpoint.url, "the demo server is up");
                let control = server.control();
                match fault {
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

/// The demo user's password, for the core (the user's keychain is never
/// touched).
#[derive(Debug)]
struct DemoSecrets {
    password: Option<SecretString>,
}

impl SecretStore for DemoSecrets {
    fn get(&self, account: &str) -> Result<Option<SecretString>, SecretError> {
        Ok(self.password.clone().filter(|_| account == ENVIRONMENT_ID))
    }

    fn set(&self, _account: &str, _secret: &SecretString) -> Result<(), SecretError> {
        Err(SecretError::new("the demo stores no passwords"))
    }

    fn delete(&self, _account: &str) -> Result<(), SecretError> {
        Ok(())
    }
}

/// The demo's settings: one environment, `prod-cluster`, with the
/// design's folders of dashboards. Its URL and pin are filled in when the
/// server is up (`AppState::set_demo_server`).
pub(crate) fn config() -> Config {
    let environment = Environment {
        id: ENVIRONMENT_ID.to_owned(),
        name: "prod-cluster".to_owned(),
        // Replaced once the demo server listens.
        url: "https://127.0.0.1:5665".to_owned(),
        auth: AuthConfig::Basic {
            username: USER.to_owned(),
        },
        tls: TlsConfig::default(),
        author: Some("demo".to_owned()),
        groups: groups(),
        notifications: NotificationSettings::default(),
    };
    Config {
        version: CONFIG_VERSION,
        general: General::default(),
        active_environment: Some(ENVIRONMENT_ID.to_owned()),
        environments: vec![environment],
    }
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
    fn the_demo_has_one_valid_environment_with_the_designs_folders() {
        let config = config();
        assert_eq!(config.environments.len(), 1);
        assert_eq!(config.active_environment.as_deref(), Some(ENVIRONMENT_ID));
        let environment = &config.environments[0];
        let names: Vec<_> = environment
            .groups
            .iter()
            .map(|group| group.name.as_str())
            .collect();
        assert_eq!(names, ["overview", "platform", "lab"]);
        let mut ids = HashSet::new();
        for group in &environment.groups {
            assert!(ids.insert(group.id.clone()));
            for dashboard in &group.dashboards {
                assert!(ids.insert(dashboard.id.clone()), "{}", dashboard.id);
                ic_filter::Filter::parse(&dashboard.view.filter)
                    .unwrap_or_else(|error| panic!("{}: {error:?}", dashboard.name));
            }
        }
        assert!(config.validate().is_empty(), "{:?}", config.validate());
    }

    #[test]
    fn the_secret_store_knows_only_the_demo_user() {
        let secrets = DemoSecrets {
            password: Some(SecretString::from("p")),
        };
        assert!(secrets.get(ENVIRONMENT_ID).unwrap().is_some());
        assert!(secrets.get("someone-else").unwrap().is_none());
        assert!(
            secrets
                .set(ENVIRONMENT_ID, &SecretString::from("x"))
                .is_err()
        );
    }

    #[test]
    fn faults_bend_the_password_and_the_target() {
        let server = |fault| DemoServer {
            stop: None,
            thread: None,
            password: SecretString::from("right"),
            fault,
        };
        let endpoint = DemoEndpoint {
            url: "https://127.0.0.1:4000".to_owned(),
            fingerprint: "AB".to_owned(),
        };
        let password = |fault| {
            server(fault)
                .secrets()
                .get(ENVIRONMENT_ID)
                .unwrap()
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
            (endpoint.url.clone(), Some("AB".to_owned()))
        );
        assert_eq!(
            server(Some(DemoFault::Tls)).environment_target(&endpoint).1,
            None
        );
        assert!(
            server(Some(DemoFault::Offline))
                .environment_target(&endpoint)
                .0
                .ends_with(":1")
        );
        assert_eq!(
            DemoFault::parse(" Missing-Secret "),
            Some(DemoFault::MissingSecret)
        );
        assert_eq!(DemoFault::parse("outage"), Some(DemoFault::Outage));
        assert_eq!(DemoFault::parse("frozen"), Some(DemoFault::Frozen));
        assert_eq!(DemoFault::parse("nope"), None);
    }

    #[test]
    fn the_password_never_shows_in_logs() {
        let server = DemoServer {
            stop: None,
            thread: None,
            password: SecretString::from("hunter2-secret"),
            fault: None,
        };
        let secrets = DemoSecrets {
            password: Some(SecretString::from("hunter2-secret")),
        };
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
        };
        let (server, endpoint) = start(&options).unwrap();
        let (endpoint, _control) = futures::executor::block_on(endpoint).unwrap().unwrap();
        assert!(
            endpoint.url.starts_with("https://127.0.0.1:"),
            "{endpoint:?}"
        );
        assert_eq!(endpoint.fingerprint.len(), 95, "colon hex SHA-256");
        drop(server);
    }
}
