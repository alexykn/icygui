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
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use futures::channel::mpsc::UnboundedReceiver;
use ic_config::{AuthConfig, Environment, General};
use ic_core::ports::{Clock, Notifier, SecretError, SecretStore};
use ic_core::snapshot::Snapshot;
use ic_core::{
    Command, ConnectionState, CoreEvent, CoreHandle, EnvironmentSpec, LogEntry, NotificationRecord,
    Ports, Start, Tuning,
};
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

/// How long [`Engine::wait_for`] waits at most, events or not.
pub(crate) const WAIT_FOR: Duration = Duration::from_mins(1);

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

    /// Moves the clock forward.
    pub(crate) fn advance(&self, by: Duration) {
        let mut now = self.now.lock().unwrap();
        *now = now.plus(by);
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

/// Fast timing for tests. Every reconnect reloads (the gap counts as long
/// from the start); `gentle.rs` tests the production rule.
pub(crate) fn tuning() -> Tuning {
    Tuning {
        backoff_initial: Duration::from_millis(40),
        backoff_max: Duration::from_millis(320),
        publish_interval: Duration::from_millis(20),
        requery_delay: Duration::from_millis(20),
        watchdog_interval: Duration::from_millis(20),
        reload_jitter: Duration::from_millis(30),
        reload_after_gap: Duration::ZERO,
        load_retry_initial: Duration::from_millis(40),
        rule_tick: Duration::from_millis(20),
        ..Tuning::default()
    }
}

/// Polls `condition` every 10 ms for at most ten seconds; whether it
/// held.
pub(crate) async fn wait_until(mut condition: impl FnMut() -> bool) -> bool {
    for _ in 0..1_000 {
        if condition() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    false
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
    environment.urls[0].pinned_sha256 = Some(server.cert_fingerprint());
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
    /// Every event seen so far, in order (but the liveness ticks).
    pub(crate) seen: Vec<CoreEvent>,
    /// How many liveness ticks ([`CoreEvent::Alive`]) came.
    pub(crate) alive: usize,
    pub(crate) data_dir: PathBuf,
    _dir: Option<tempfile::TempDir>,
}

/// How to start an engine beyond the defaults of [`start`].
pub(crate) struct Launch {
    pub(crate) environment: Environment,
    pub(crate) secrets: Arc<FakeSecrets>,
    pub(crate) tuning: Tuning,
    pub(crate) general: General,
    /// The fake clock's start (Unix seconds).
    pub(crate) now: f64,
    /// Where the event log lives; a fresh temporary directory if `None`.
    pub(crate) data_dir: Option<PathBuf>,
    /// How the engine starts (a background start waits before its first
    /// load).
    pub(crate) start: Start,
}

impl Launch {
    /// The defaults for `server`: root, fast timing, the fake clock at the
    /// real time (rule engine delays and storms count from it), a fresh
    /// data directory.
    pub(crate) fn new(server: &MockServer) -> Self {
        Self {
            environment: environment(server),
            secrets: FakeSecrets::with(ENV_ID, PASSWORD),
            tuning: tuning(),
            general: General::default(),
            now: Timestamp::now().as_unix_seconds(),
            data_dir: None,
            start: Start::User,
        }
    }

    pub(crate) fn start(self) -> Engine {
        launch(self)
    }
}

/// Starts the engine for `environment` with `secrets`.
pub(crate) fn start(environment: Environment, secrets: Arc<FakeSecrets>, tuning: Tuning) -> Engine {
    launch(Launch {
        environment,
        secrets,
        tuning,
        general: General::default(),
        now: NOW,
        data_dir: None,
        start: Start::User,
    })
}

fn launch(launch: Launch) -> Engine {
    let Launch {
        environment,
        secrets,
        tuning,
        general,
        now,
        data_dir,
        start,
    } = launch;
    let (dir, data_dir) = if let Some(data_dir) = data_dir {
        (None, data_dir)
    } else {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_owned();
        (Some(dir), path)
    };
    let clock = FakeClock::at(now);
    let notifier = Arc::new(FakeNotifier::default());
    let spec = EnvironmentSpec {
        environment,
        general,
        hide_handled: ic_config::HideHandled::ALL,
        data_dir: data_dir.clone(),
        start,
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
        alive: 0,
        data_dir,
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

    pub(crate) fn send(&self, command: Command) {
        self.handle().send(command);
    }

    /// The next event, or a panic after [`WAIT`]. The engine's liveness
    /// ticks ([`CoreEvent::Alive`]) are counted, not returned.
    pub(crate) async fn next(&mut self) -> CoreEvent {
        // The ticks come every few seconds: the wait is for another event.
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let event = tokio::time::timeout_at(deadline, self.events.next())
                .await
                .unwrap_or_else(|_| panic!("no event within {WAIT:?}; seen: {:#?}", self.tail()))
                .expect("the engine stopped");
            if let CoreEvent::Alive(_) = event {
                self.alive += 1;
                continue;
            }
            self.seen.push(event.clone());
            return event;
        }
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

    /// Waits for an event `pick` accepts (at most [`WAIT_FOR`], however
    /// many other events come meanwhile).
    pub(crate) async fn wait_for<T>(&mut self, mut pick: impl FnMut(&CoreEvent) -> Option<T>) -> T {
        let deadline = std::time::Instant::now() + WAIT_FOR;
        loop {
            let event = self.next().await;
            if let Some(value) = pick(&event) {
                return value;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "no such event within {WAIT_FOR:?}; seen: {:#?}",
                self.tail()
            );
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

    /// The notifications (`CoreEvent::Notification`) seen so far, in order.
    pub(crate) fn notifications(&self) -> Vec<NotificationRecord> {
        self.seen
            .iter()
            .filter_map(|event| match event {
                CoreEvent::Notification(record) => Some(record.clone()),
                _ => None,
            })
            .collect()
    }

    /// Waits for the next notification.
    pub(crate) async fn notification(&mut self) -> NotificationRecord {
        self.wait_for(|event| match event {
            CoreEvent::Notification(record) => Some(record.clone()),
            _ => None,
        })
        .await
    }

    /// The titles of the notifications the fake notifier showed.
    pub(crate) fn shown(&self) -> Vec<String> {
        self.notifier
            .shown
            .lock()
            .unwrap()
            .iter()
            .map(|intent| intent.title.clone())
            .collect()
    }

    /// `Command::LoadHistory`.
    pub(crate) async fn history(&self, object: Option<ic_model::ObjectKey>) -> Vec<LogEntry> {
        let (reply, answer) = futures::channel::oneshot::channel();
        self.send(Command::LoadHistory {
            object,
            limit: 1_000,
            reply,
        });
        tokio::time::timeout(WAIT, answer)
            .await
            .expect("no history in time")
            .expect("the engine dropped the request")
    }

    /// `Command::LoadNotifications`.
    pub(crate) async fn stored_notifications(&self) -> Vec<NotificationRecord> {
        let (reply, answer) = futures::channel::oneshot::channel();
        self.send(Command::LoadNotifications {
            limit: 1_000,
            reply,
        });
        tokio::time::timeout(WAIT, answer)
            .await
            .expect("no notifications in time")
            .expect("the engine dropped the request")
    }

    /// The data directory, for a second engine on the same event log.
    pub(crate) fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Stops the engine and checks it stopped in time.
    pub(crate) fn shutdown(&mut self) -> Duration {
        let started = std::time::Instant::now();
        self.handle.take().unwrap().shutdown();
        started.elapsed()
    }
}
