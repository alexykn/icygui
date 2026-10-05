//! Shared helpers for the engine's integration tests: fake ports, an
//! environment pointing at an in-process `ic-mock`, and bounded waits on
//! the engine's events (no sleeps).

#![allow(dead_code, reason = "each test binary uses a different subset")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers fail the test loudly"
)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use futures::channel::mpsc::UnboundedReceiver;
use ic_config::{AuthConfig, Environment, General};
use ic_core::ports::{Clock, Notifier, SecretError, SecretStore};
use ic_core::snapshot::Snapshot;
use ic_core::{ConnectionState, CoreEvent, CoreHandle, EnvironmentSpec, Ports, Tuning};
use ic_mock::{MockConfig, MockServer};
use ic_model::Timestamp;
use ic_rules::{LocalTime, NotificationIntent};
use secrecy::{ExposeSecret, SecretString};

/// The mock's default user.
pub(crate) const USER: &str = "root";
/// Its password.
pub(crate) const PASSWORD: &str = "icinga";
/// The id of the test environment (the secret store's account).
pub(crate) const ENV_ID: &str = "11111111-2222-3333-4444-555555555555";
/// How long a test waits for an expected event.
pub(crate) const WAIT: Duration = Duration::from_secs(20);

/// Secrets in memory.
#[derive(Debug, Default)]
pub(crate) struct FakeSecrets {
    secrets: Mutex<HashMap<String, String>>,
    reads: Mutex<u32>,
}

impl FakeSecrets {
    pub(crate) fn with(account: &str, password: &str) -> Arc<Self> {
        let secrets = Self::default();
        secrets
            .secrets
            .lock()
            .unwrap()
            .insert(account.to_owned(), password.to_owned());
        Arc::new(secrets)
    }

    pub(crate) fn put(&self, account: &str, password: &str) {
        self.secrets
            .lock()
            .unwrap()
            .insert(account.to_owned(), password.to_owned());
    }

    pub(crate) fn reads(&self) -> u32 {
        *self.reads.lock().unwrap()
    }
}

impl SecretStore for FakeSecrets {
    fn get(&self, account: &str) -> Result<Option<SecretString>, SecretError> {
        *self.reads.lock().unwrap() += 1;
        Ok(self
            .secrets
            .lock()
            .unwrap()
            .get(account)
            .map(|password| SecretString::from(password.clone())))
    }

    fn set(&self, account: &str, secret: &SecretString) -> Result<(), SecretError> {
        self.put(account, secret.expose_secret());
        Ok(())
    }

    fn delete(&self, account: &str) -> Result<(), SecretError> {
        self.secrets.lock().unwrap().remove(account);
        Ok(())
    }
}

/// Records notifications.
#[derive(Debug, Default)]
pub(crate) struct FakeNotifier {
    pub(crate) shown: Mutex<Vec<NotificationIntent>>,
}

impl Notifier for FakeNotifier {
    fn notify(&self, intent: &NotificationIntent) {
        self.shown.lock().unwrap().push(intent.clone());
    }
}

/// A clock that stands still unless moved.
#[derive(Debug)]
pub(crate) struct FakeClock {
    pub(crate) now: Mutex<Timestamp>,
}

impl FakeClock {
    pub(crate) fn at(seconds: f64) -> Arc<Self> {
        Arc::new(Self {
            now: Mutex::new(Timestamp::from_unix_seconds(seconds)),
        })
    }
}

impl Clock for FakeClock {
    fn now(&self) -> Timestamp {
        *self.now.lock().unwrap()
    }

    fn local(&self) -> LocalTime {
        LocalTime {
            weekday: 0,
            minute_of_day: 12 * 60,
        }
    }
}

/// The fake clock's time.
pub(crate) const NOW: f64 = 1_800_000_000.0;

/// Fast timing for tests.
pub(crate) fn tuning() -> Tuning {
    Tuning {
        backoff_initial: Duration::from_millis(40),
        backoff_max: Duration::from_millis(320),
        publish_interval: Duration::from_millis(20),
        requery_delay: Duration::from_millis(20),
        ..Tuning::default()
    }
}

/// Starts a mock with `config`.
pub(crate) async fn mock(config: MockConfig) -> MockServer {
    MockServer::start(config).await.unwrap()
}

/// An environment for `server`, user `root`, pinned to its certificate.
pub(crate) fn environment(server: &MockServer) -> Environment {
    let mut environment = Environment::new(
        "test",
        &server.url(),
        AuthConfig::Basic {
            username: USER.to_owned(),
        },
    );
    ENV_ID.clone_into(&mut environment.id);
    environment.tls.use_system_roots = false;
    environment.tls.pinned_sha256 = Some(server.cert_fingerprint());
    environment.author = Some("icygui-test".to_owned());
    environment
}

/// A running engine and what the test needs around it.
pub(crate) struct Engine {
    pub(crate) handle: Option<CoreHandle>,
    pub(crate) events: UnboundedReceiver<CoreEvent>,
    pub(crate) secrets: Arc<FakeSecrets>,
    pub(crate) clock: Arc<FakeClock>,
    pub(crate) notifier: Arc<FakeNotifier>,
    /// Every event seen so far, in order.
    pub(crate) seen: Vec<CoreEvent>,
    pub(crate) data_dir: PathBuf,
    _dir: tempfile::TempDir,
}

/// Starts the engine for `environment` with `secrets`.
pub(crate) fn start(environment: Environment, secrets: Arc<FakeSecrets>, tuning: Tuning) -> Engine {
    let dir = tempfile::tempdir().unwrap();
    let clock = FakeClock::at(NOW);
    let notifier = Arc::new(FakeNotifier::default());
    let spec = EnvironmentSpec {
        environment,
        general: General::default(),
        data_dir: dir.path().to_owned(),
    };
    let ports = Ports {
        secrets: Arc::clone(&secrets) as Arc<dyn SecretStore>,
        notifier: Arc::clone(&notifier) as Arc<dyn Notifier>,
        clock: Arc::clone(&clock) as Arc<dyn Clock>,
    };
    let mut handle = ic_core::start_with_tuning(spec, ports, tuning).unwrap();
    let events = handle.take_events().unwrap();
    assert!(handle.take_events().is_none(), "the events are taken once");
    Engine {
        handle: Some(handle),
        events,
        secrets,
        clock,
        notifier,
        seen: Vec::new(),
        data_dir: dir.path().to_owned(),
        _dir: dir,
    }
}

/// Starts the engine for `server` as root.
pub(crate) fn start_for(server: &MockServer) -> Engine {
    start(
        environment(server),
        FakeSecrets::with(ENV_ID, PASSWORD),
        tuning(),
    )
}

impl Engine {
    pub(crate) fn handle(&self) -> &CoreHandle {
        self.handle.as_ref().unwrap()
    }

    pub(crate) fn send(&self, command: ic_core::Command) {
        self.handle().send(command);
    }

    /// The next event, or a panic after [`WAIT`].
    pub(crate) async fn next(&mut self) -> CoreEvent {
        let event = tokio::time::timeout(WAIT, self.events.next())
            .await
            .unwrap_or_else(|_| panic!("no event within {WAIT:?}; seen: {:#?}", self.tail()))
            .expect("the engine stopped");
        self.seen.push(event.clone());
        event
    }

    fn tail(&self) -> Vec<String> {
        self.seen
            .iter()
            .rev()
            .take(8)
            .map(|event| match event {
                CoreEvent::Snapshot(snapshot) => format!(
                    "Snapshot r{} {} hosts {} services",
                    snapshot.revision,
                    snapshot.hosts.len(),
                    snapshot.services.len()
                ),
                other => format!("{other:?}"),
            })
            .collect()
    }

    /// Waits for an event `pick` accepts.
    pub(crate) async fn wait_for<T>(&mut self, mut pick: impl FnMut(&CoreEvent) -> Option<T>) -> T {
        loop {
            let event = self.next().await;
            if let Some(value) = pick(&event) {
                return value;
            }
        }
    }

    /// Waits for a connection state `pick` accepts.
    pub(crate) async fn wait_state(
        &mut self,
        mut pick: impl FnMut(&ConnectionState) -> bool,
    ) -> ConnectionState {
        self.wait_for(|event| match event {
            CoreEvent::Connection(state) if pick(state) => Some(state.clone()),
            _ => None,
        })
        .await
    }

    /// Waits until connected.
    pub(crate) async fn connected(&mut self) -> ConnectionState {
        self.wait_state(|state| matches!(state, ConnectionState::Connected { .. }))
            .await
    }

    /// The latest snapshot seen so far.
    pub(crate) fn latest(&self) -> Option<Arc<Snapshot>> {
        self.seen.iter().rev().find_map(|event| match event {
            CoreEvent::Snapshot(snapshot) => Some(Arc::clone(snapshot)),
            _ => None,
        })
    }

    /// Waits until the latest snapshot is one `accept` accepts (it may
    /// have arrived already).
    pub(crate) async fn snapshot(
        &mut self,
        mut accept: impl FnMut(&Snapshot) -> bool,
    ) -> Arc<Snapshot> {
        if let Some(latest) = self.latest().filter(|snapshot| accept(snapshot)) {
            return latest;
        }
        self.wait_for(|event| match event {
            CoreEvent::Snapshot(snapshot) if accept(snapshot) => Some(Arc::clone(snapshot)),
            _ => None,
        })
        .await
    }

    /// The connection states seen so far.
    pub(crate) fn states(&self) -> Vec<ConnectionState> {
        self.seen
            .iter()
            .filter_map(|event| match event {
                CoreEvent::Connection(state) => Some(state.clone()),
                _ => None,
            })
            .collect()
    }

    /// Stops the engine and checks it stopped in time.
    pub(crate) fn shutdown(&mut self) -> Duration {
        let started = std::time::Instant::now();
        self.handle.take().unwrap().shutdown();
        started.elapsed()
    }
}
