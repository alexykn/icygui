//! The HTTPS server: listener, TLS handshakes, connections, background
//! tasks (simulator and timers).

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};
use std::time::Duration;

use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::auth::Users;
use crate::config::{MockConfig, MockTls, NumberFormat};
use crate::control::{MockControl, RecordedRequest};
use crate::error::MockError;
use crate::http::{self, ConnInfo};
use crate::model::World;
use crate::model::load::LoadOptions;
use crate::tls::{self, TlsMaterial, TlsState};

/// Locks a mutex, recovering the data if another thread panicked while
/// holding it (the mock's state stays usable).
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Runs `work`, which may block on the world's lock (a big query holds it
/// for a while), without stalling other tasks: on a multi-threaded runtime
/// the worker hands its other tasks off first, so event streams keep
/// flowing. Elsewhere (single-threaded test runtimes) it just runs.
pub(crate) fn blocking<T>(work: impl FnOnce() -> T) -> T {
    match tokio::runtime::Handle::try_current().map(|handle| handle.runtime_flavor()) {
        Ok(tokio::runtime::RuntimeFlavor::MultiThread) => tokio::task::block_in_place(work),
        _ => work(),
    }
}

/// Validates a checker rate (checks per second).
///
/// # Errors
/// Rates that aren't positive, finite numbers.
pub(crate) fn check_rate(rate: f64) -> Result<f64, MockError> {
    if rate.is_finite() && rate > 0.0 {
        Ok(rate)
    } else {
        Err(MockError::InvalidConfig(
            "the check rate must be a positive number of checks per second".to_owned(),
        ))
    }
}

/// How often the real-time checker runs queued checks.
const CHECKER_PERIOD: Duration = Duration::from_millis(10);

/// Injected faults.
#[derive(Debug, Default)]
pub(crate) struct Faults {
    pub(crate) fail_next: u32,
    pub(crate) fail_status: u16,
    /// Requests to these paths that still fail, and with which status.
    pub(crate) fail_path: HashMap<String, (u32, u16)>,
    pub(crate) latency: Duration,
    /// Added after an action ran, before its answer goes out.
    pub(crate) action_answer_delay: Duration,
}

/// State shared by connections, background tasks and control handles.
#[derive(Debug)]
pub(crate) struct Shared {
    world: Mutex<World>,
    pub(crate) users: Users,
    pub(crate) number_format: NumberFormat,
    pub(crate) enforce_filter_permission: bool,
    pub(crate) server_header: String,
    pub(crate) node_name: String,
    tls: RwLock<Arc<TlsState>>,
    faults: Mutex<Faults>,
    requests: Mutex<VecDeque<RecordedRequest>>,
    max_requests: usize,
    /// Bumped to abort every open connection.
    kill: watch::Sender<u64>,
    shut_down: AtomicBool,
}

impl Shared {
    /// The world, locked.
    pub(crate) fn world(&self) -> MutexGuard<'_, World> {
        lock(&self.world)
    }

    pub(crate) fn tls(&self) -> Arc<TlsState> {
        Arc::clone(&self.tls.read().unwrap_or_else(PoisonError::into_inner))
    }

    pub(crate) fn set_tls(&self, state: TlsState) {
        *self.tls.write().unwrap_or_else(PoisonError::into_inner) = Arc::new(state);
    }

    /// Latency to add and, if this request to `path` should fail, its
    /// status.
    pub(crate) fn take_faults(&self, path: &str) -> (Duration, Option<u16>) {
        let mut faults = lock(&self.faults);
        let mut failure = (faults.fail_next > 0).then(|| {
            faults.fail_next -= 1;
            faults.fail_status
        });
        if failure.is_none()
            && let Some((count, status)) = faults.fail_path.get_mut(path)
        {
            *count -= 1;
            failure = Some(*status);
            if *count == 0 {
                faults.fail_path.remove(path);
            }
        }
        (faults.latency, failure)
    }

    pub(crate) fn set_failures(&self, count: u32, status: u16) {
        let mut faults = lock(&self.faults);
        faults.fail_next = count;
        faults.fail_status = status;
    }

    pub(crate) fn set_path_failures(&self, path: &str, count: u32, status: u16) {
        let mut faults = lock(&self.faults);
        if count == 0 {
            faults.fail_path.remove(path);
        } else {
            faults.fail_path.insert(path.to_owned(), (count, status));
        }
    }

    pub(crate) fn set_latency(&self, latency: Duration) {
        lock(&self.faults).latency = latency;
    }

    pub(crate) fn set_action_answer_delay(&self, delay: Duration) {
        lock(&self.faults).action_answer_delay = delay;
    }

    pub(crate) fn action_answer_delay(&self) -> Duration {
        lock(&self.faults).action_answer_delay
    }

    pub(crate) fn record(&self, request: RecordedRequest) {
        let mut requests = lock(&self.requests);
        requests.push_back(request);
        while requests.len() > self.max_requests {
            requests.pop_front();
        }
    }

    pub(crate) fn requests(&self) -> Vec<RecordedRequest> {
        lock(&self.requests).iter().cloned().collect()
    }

    pub(crate) fn clear_requests(&self) {
        lock(&self.requests).clear();
    }

    /// Aborts every open connection (event streams included).
    pub(crate) fn kill_connections(&self) {
        self.kill.send_modify(|generation| *generation += 1);
    }

    pub(crate) fn is_shut_down(&self) -> bool {
        self.shut_down.load(Ordering::SeqCst)
    }
}

/// A running mock Icinga API on `127.0.0.1`.
///
/// Dropping it stops the server like [`MockServer::shutdown`], without
/// waiting for the tasks to finish.
#[derive(Debug)]
pub struct MockServer {
    shared: Arc<Shared>,
    addr: SocketAddr,
    shutdown: watch::Sender<bool>,
    tasks: Vec<JoinHandle<()>>,
}

impl MockServer {
    /// Starts a server: loads the scenario, generates certificates, binds
    /// `127.0.0.1:<port>` and starts the simulator and timers.
    ///
    /// # Errors
    /// Invalid configuration or scenario, certificate generation, or the
    /// port can't be bound.
    pub async fn start(config: MockConfig) -> Result<Self, MockError> {
        let simulation = &config.simulation;
        if !(simulation.speed.is_finite() && simulation.speed > 0.0) {
            return Err(MockError::InvalidConfig(
                "simulation speed must be a positive number".to_owned(),
            ));
        }
        if simulation.tick.is_zero() {
            return Err(MockError::InvalidConfig(
                "simulation tick must not be zero".to_owned(),
            ));
        }
        if config.housekeeping_interval.is_zero() {
            return Err(MockError::InvalidConfig(
                "housekeeping interval must not be zero".to_owned(),
            ));
        }
        check_rate(config.check_rate)?;
        let node_name = config.scenario.status.node_name.clone();
        let material = match &config.tls {
            MockTls::SelfSigned => TlsMaterial::self_signed(&node_name)?,
            MockTls::CaSigned => TlsMaterial::ca_signed(&node_name)?,
            MockTls::Provided(material) => material.clone(),
        };
        let tls_state = tls::server_state(material)?;
        let mut world = World::load(
            &config.scenario,
            &LoadOptions {
                number_format: config.number_format,
                event_buffer: config.event_buffer,
                seed: simulation.seed,
                reschedule_delay: config.reschedule_delay.as_secs_f64(),
                check_rate: config.check_rate,
                now: crate::model::wall_clock(),
            },
        )?;
        world.sim_configure(simulation);
        let server_header = format!("Icinga/{}", world.app.version);
        let listener = TcpListener::bind(("127.0.0.1", config.port)).await?;
        let addr = listener.local_addr()?;
        let (kill, _) = watch::channel(0);
        let shared = Arc::new(Shared {
            world: Mutex::new(world),
            users: Users::new(config.users.clone()),
            number_format: config.number_format,
            enforce_filter_permission: config.enforce_filter_expression_permission,
            server_header,
            node_name,
            tls: RwLock::new(Arc::new(tls_state)),
            faults: Mutex::new(Faults::default()),
            requests: Mutex::new(VecDeque::new()),
            max_requests: config.max_recorded_requests.max(1),
            kill,
            shut_down: AtomicBool::new(false),
        });
        let (shutdown, shutdown_rx) = watch::channel(false);
        let tasks = vec![
            tokio::spawn(accept_loop(
                listener,
                Arc::clone(&shared),
                shutdown_rx.clone(),
            )),
            tokio::spawn(simulator_loop(Arc::clone(&shared), shutdown_rx.clone())),
            tokio::spawn(housekeeping_loop(
                Arc::clone(&shared),
                config.housekeeping_interval,
                shutdown_rx.clone(),
            )),
            tokio::spawn(checker_loop(Arc::clone(&shared), shutdown_rx)),
        ];
        tracing::info!(%addr, scenario = %config.scenario.name, "mock Icinga API listening");
        Ok(Self {
            shared,
            addr,
            shutdown,
            tasks,
        })
    }

    /// `https://127.0.0.1:<port>`.
    pub fn url(&self) -> String {
        format!("https://{}", self.addr)
    }

    /// The bound address.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The bound port.
    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// The server certificate (PEM) currently served.
    pub fn cert_pem(&self) -> String {
        self.shared.tls().material.cert_pem.clone()
    }

    /// The CA certificate (PEM) when the server certificate is CA-signed.
    pub fn ca_pem(&self) -> Option<String> {
        self.shared.tls().material.ca_pem.clone()
    }

    /// SHA-256 of the served certificate's DER encoding (what clients pin).
    pub fn cert_sha256(&self) -> [u8; 32] {
        self.shared.tls().fingerprint
    }

    /// [`Self::cert_sha256`] as colon-separated uppercase hex.
    pub fn cert_fingerprint(&self) -> String {
        tls::format_fingerprint(&self.cert_sha256())
    }

    /// The certificates and keys, e.g. to restart with the same fingerprint
    /// ([`MockTls::Provided`]).
    pub fn tls_material(&self) -> TlsMaterial {
        self.shared.tls().material.clone()
    }

    /// A control handle (cheap to clone).
    pub fn control(&self) -> MockControl {
        MockControl::new(Arc::clone(&self.shared))
    }

    /// Serves a new certificate (new key and fingerprint; same CA when
    /// CA-signed) and drops every open connection, like a restart with a
    /// rotated certificate. Returns the new fingerprint.
    ///
    /// # Errors
    /// Certificate generation failed.
    pub fn rotate_certificate(&self) -> Result<[u8; 32], MockError> {
        self.control().rotate_certificate()
    }

    /// A client certificate (cert PEM, key PEM) for `cn`, signed by the CA.
    /// Users whose [`crate::MockUser::client_cn`] is `cn` authenticate with it.
    ///
    /// # Errors
    /// The server isn't CA-signed.
    pub fn client_certificate(&self, cn: &str) -> Result<(String, String), MockError> {
        self.shared.tls().material.client_certificate(cn)
    }

    /// Stops accepting connections, closes open ones (event streams
    /// included) and waits for the background tasks.
    pub async fn shutdown(mut self) {
        self.stop();
        for task in std::mem::take(&mut self.tasks) {
            let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
        }
    }

    fn stop(&self) {
        self.shared.shut_down.store(true, Ordering::SeqCst);
        let _ = self.shutdown.send(true);
        self.shared.kill_connections();
        self.shared.world().bus.drop_all();
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        if !self.shared.is_shut_down() {
            self.stop();
        }
    }
}

async fn accept_loop(
    listener: TcpListener,
    shared: Arc<Shared>,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            _ = shutdown.changed() => break,
            accepted = listener.accept() => match accepted {
                Ok((stream, peer)) => {
                    tokio::spawn(connection(stream, peer, Arc::clone(&shared), shutdown.clone()));
                }
                Err(error) => {
                    tracing::warn!(%error, "accept failed");
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            },
        }
    }
}

async fn connection(
    stream: TcpStream,
    peer: SocketAddr,
    shared: Arc<Shared>,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut kill = shared.kill.subscribe();
    kill.mark_unchanged();
    let tls_state = shared.tls();
    let acceptor = tls_state.acceptor.clone();
    let tls = match tokio::time::timeout(Duration::from_secs(15), acceptor.accept(stream)).await {
        Ok(Ok(tls)) => tls,
        Ok(Err(error)) => {
            tracing::debug!(%peer, %error, "TLS handshake failed");
            return;
        }
        Err(_) => {
            tracing::debug!(%peer, "TLS handshake timed out");
            return;
        }
    };
    // Like Icinga: a client certificate signed by the CA logs in the API
    // user with that `client_cn`; any other certificate is ignored.
    let cert_user = tls
        .get_ref()
        .1
        .peer_certificates()
        .and_then(|chain| tls_state.verified_client_cn(chain))
        .and_then(|cn| shared.users.by_client_cn(&cn));
    drop(tls_state);
    let info = Arc::new(ConnInfo { cert_user, peer });
    let service_shared = Arc::clone(&shared);
    let service = service_fn(move |request| {
        http::serve(request, Arc::clone(&service_shared), Arc::clone(&info))
    });
    let connection = http1::Builder::new()
        .keep_alive(true)
        .serve_connection(TokioIo::new(tls), service);
    tokio::pin!(connection);
    tokio::select! {
        result = connection.as_mut() => {
            if let Err(error) = result {
                tracing::debug!(%peer, %error, "connection ended with an error");
            }
        }
        _ = kill.changed() => tracing::debug!(%peer, "connection dropped"),
        _ = shutdown.changed() => {}
    }
}

async fn simulator_loop(shared: Arc<Shared>, mut shutdown: watch::Receiver<bool>) {
    loop {
        let period = shared.world().sim.real_tick_seconds();
        tokio::select! {
            _ = shutdown.changed() => break,
            () = tokio::time::sleep(Duration::from_secs_f64(period)) => {}
        }
        blocking(|| {
            let mut world = shared.world();
            if world.sim.running {
                world.sim_tick();
            }
        });
    }
}

async fn housekeeping_loop(
    shared: Arc<Shared>,
    interval: Duration,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = shutdown.changed() => break,
            _ = ticker.tick() => blocking(|| shared.world().housekeeping()),
        }
    }
}

/// The real-time checker: runs queued checks (forced re-checks, bursts) at
/// the configured rate, in small batches so the event stream stays smooth.
async fn checker_loop(shared: Arc<Shared>, mut shutdown: watch::Receiver<bool>) {
    let mut ticker = tokio::time::interval(CHECKER_PERIOD);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last = tokio::time::Instant::now();
    loop {
        tokio::select! {
            _ = shutdown.changed() => break,
            _ = ticker.tick() => {
                let now = tokio::time::Instant::now();
                let elapsed = now.duration_since(last).as_secs_f64();
                last = now;
                blocking(|| shared.world().checker_step(elapsed));
            }
        }
    }
}
