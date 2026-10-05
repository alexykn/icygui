//! The engine of the active environment: one task on the core's runtime
//! that owns the [`Store`] and reacts to commands, to its own background
//! tasks (connect, loads, re-queries, status polls, actions) and to the
//! event stream.
//!
//! Background tasks never touch the store: they send [`Internal`] messages
//! tagged with the connection session they belong to, and answers of an
//! older session are dropped. Every task of a session lives in its
//! `JoinSet`, so tearing a session down aborts them all (and closes the
//! event stream).
//!
//! - Snapshots and dashboards: [`Engine::publish`] cuts a snapshot and
//!   evaluates the dashboards for the changes since the last one on a
//!   blocking thread (`publish.rs`).
//! - Keeping in sync beyond the stream (`sync.rs`): the freshness watchdog,
//!   hydration, the periodic reconcile and the jittered reload after a
//!   reconnect.
//!
//! Extension points for stage 3 are marked with `Stage 3:` comments: the
//! rule engine and the event log plug into [`Engine::record_applied`] and
//! [`Engine::record_discovered`], and the one-second tick into
//! [`Engine::run_due`].

mod actions;
mod fetch;
mod load;
mod publish;
mod stream;
mod sync;
mod watchdog;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use ic_api::{ApiError, ApiInfo, Client, Detail};
use ic_model::{
    Event, Host, InstanceStatus, ObjectChange, ObjectKey, Service, ServiceKey, Timestamp,
};
use ic_rules::DashboardRef;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot;
use tokio::task::JoinSet;
use tokio::time::Instant;

use crate::backoff::Backoff;
use crate::command::{ActionOutcome, Command, ConnectionState, CoreEvent, LoadPhase};
use crate::connect::{self, Connected, Failure};
use crate::dashboards::Dashboards;
use crate::snapshot::{DashboardResult, Snapshot};
use crate::spec::{EnvironmentSpec, Ports, Tuning};
use crate::store::{Applied, Discovered, ObjectView, Overview, Store};

use fetch::{Answers, FetchQueue, FetchTask, Lists};
use load::LoadTask;
use publish::Previews;
use stream::ReaderMsg;
use watchdog::Watchdog;

/// Messages from the engine's background tasks.
#[derive(Debug)]
pub(crate) enum Internal {
    /// The connect task succeeded.
    Connected {
        session: u64,
        connected: Box<Connected>,
    },
    /// The connect task failed.
    ConnectFailed { session: u64, failure: Failure },
    /// A step of a load.
    Load {
        session: u64,
        load: u64,
        step: LoadStep,
    },
    /// A status poll's answer.
    Status {
        session: u64,
        result: Result<InstanceStatus, ApiError>,
    },
    /// A re-query round's answers.
    Fetched { session: u64, answers: Box<Answers> },
    /// An action finished.
    ActionDone {
        id: u64,
        dirty: Vec<ObjectKey>,
        outcome: ActionOutcome,
    },
    /// The dashboards were evaluated for `snapshot`, which is ready to go
    /// out. `quiet`: nothing but the dashboards could have changed.
    /// `broken`: the evaluation failed (a bug); `dashboards` start over.
    Evaluated {
        dashboards: Box<Dashboards>,
        snapshot: Box<Snapshot>,
        quiet: bool,
        broken: bool,
    },
    /// A dashboard preview was answered.
    PreviewDone,
}

/// A load's progress and answers.
#[derive(Debug)]
pub(crate) enum LoadStep {
    Progress {
        phase: LoadPhase,
        done: usize,
        total: Option<usize>,
    },
    /// Tier 1.
    Overview {
        started: u64,
        overview: Box<Overview>,
        hosts: Vec<Host>,
    },
    /// Tier 2.
    Services {
        started: u64,
        services: Vec<Service>,
    },
    /// A batch of tier 3.
    Details {
        started: u64,
        fetched: ic_api::Fetched,
    },
    Done,
    Failed(Failure),
}

impl std::fmt::Debug for Connected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connected")
            .field("client", &self.client)
            .field("user", &self.info.user)
            .field("stream", &self.lines.is_some())
            .finish()
    }
}

/// An event the store applied, with the object's view before and after:
/// what the rule engine and the event log (stage 3) work from.
#[derive(Debug)]
pub(crate) struct AppliedEvent {
    /// The reader's sequence number.
    pub(crate) seq: u64,
    /// The event.
    pub(crate) event: Event,
    /// The object before the event (`None` if unknown).
    pub(crate) before: Option<ObjectView>,
    /// The object after it.
    pub(crate) after: Option<ObjectView>,
}

/// Where the connection is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// Not connected: waiting for `retry_at` (reconnecting), or for a
    /// command (after a failure that needs the user).
    Idle { retry_at: Option<Instant> },
    /// The connect task runs.
    Connecting,
    /// Connected; the session's first load runs and stream lines wait in
    /// their channel.
    Loading,
    /// Connected and loaded; stream lines are applied as they come.
    Live,
}

/// The established connection of the current session.
struct Conn {
    client: Client,
    info: ApiInfo,
    /// When it went live (for the backoff reset and `Connected.since`).
    live_since: Option<(Instant, Timestamp)>,
    /// The next status poll; `None` without `status/query` permission.
    next_status: Option<Instant>,
    status_in_flight: bool,
}

/// The engine.
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent flags of the engine's state machine parts"
)]
pub(crate) struct Engine {
    spec: EnvironmentSpec,
    ports: Ports,
    tuning: Tuning,
    events: futures::channel::mpsc::UnboundedSender<CoreEvent>,
    internal_tx: UnboundedSender<Internal>,
    store: Store,
    phase: Phase,
    backoff: Backoff,
    /// Counts connection sessions; answers of older ones are dropped.
    session: u64,
    /// The current session's background tasks.
    tasks: JoinSet<()>,
    conn: Option<Conn>,
    /// The current session's stream lines.
    lines: Option<UnboundedReceiver<ReaderMsg>>,
    /// Lines read so far, over every session (see the store's ordering
    /// notes).
    seq: Arc<AtomicU64>,
    /// The load in flight: its id, and whether it is the session's first.
    load: Option<(u64, bool)>,
    loads: u64,
    fetch: FetchQueue,
    revision: u64,
    last_publish: Option<Instant>,
    /// The dashboards' state; `None` while an evaluation runs on a
    /// blocking thread.
    dashboards: Option<Box<Dashboards>>,
    /// The latest evaluated dashboards.
    dashboard_results: Arc<BTreeMap<DashboardRef, DashboardResult>>,
    /// The environment's dashboards changed since the last evaluation.
    dashboards_configured: bool,
    /// Some dashboard filter calls `get_time()`, and when they were last
    /// evaluated in full.
    time_dependent: bool,
    time_refreshed: Instant,
    /// Publish as soon as the evaluation in flight is back.
    publish_soon: bool,
    /// Events produced while a snapshot was being evaluated, in order.
    outbox: Vec<CoreEvent>,
    /// Stops evaluations in flight (shutdown).
    cancel: Arc<AtomicBool>,
    previews: Previews,
    watchdog: Watchdog,
    last_sweep: Option<Instant>,
    /// The store holds a complete load from this environment's server: a
    /// reconnect goes live on it at once and reconciles shortly after.
    loaded: bool,
    /// The next connect was asked for by the user: reload without jitter.
    reload_now: bool,
    /// The reload after a reconnect, jittered.
    reload_at: Option<Instant>,
    /// The next periodic reconcile.
    reconcile_at: Option<Instant>,
    /// When the last load completed.
    last_load_done: Option<Instant>,
    /// The connection state last emitted.
    state: Option<ConnectionState>,
}

/// Why the select loop woke up.
enum Wake {
    Shutdown,
    Command(Command),
    Internal(Internal),
    Lines(usize),
    Timer,
}

impl Engine {
    pub(crate) fn new(
        spec: EnvironmentSpec,
        ports: Ports,
        tuning: Tuning,
        events: futures::channel::mpsc::UnboundedSender<CoreEvent>,
        internal_tx: UnboundedSender<Internal>,
    ) -> Self {
        let mut dashboards = Dashboards::default();
        dashboards.configure(&spec.environment);
        Self {
            backoff: Backoff::new(tuning.backoff_initial, tuning.backoff_max),
            fetch: FetchQueue::new(tuning.missing_ttl),
            spec,
            ports,
            tuning,
            events,
            internal_tx,
            store: Store::default(),
            phase: Phase::Idle { retry_at: None },
            session: 0,
            tasks: JoinSet::new(),
            conn: None,
            lines: None,
            seq: Arc::new(AtomicU64::new(0)),
            load: None,
            loads: 0,
            revision: 0,
            last_publish: None,
            dashboards: Some(Box::new(dashboards)),
            dashboard_results: Arc::default(),
            dashboards_configured: false,
            time_dependent: false,
            time_refreshed: Instant::now(),
            publish_soon: false,
            outbox: Vec::new(),
            cancel: Arc::new(AtomicBool::new(false)),
            previews: Previews::default(),
            watchdog: Watchdog::default(),
            last_sweep: None,
            loaded: false,
            reload_now: false,
            reload_at: None,
            reconcile_at: None,
            last_load_done: None,
            state: None,
        }
    }

    /// Runs until `shutdown` fires or the handle is gone.
    pub(crate) async fn run(
        mut self,
        mut commands: UnboundedReceiver<Command>,
        mut internal_rx: UnboundedReceiver<Internal>,
        mut shutdown: oneshot::Receiver<()>,
    ) {
        self.connect();
        let mut lines = Vec::new();
        loop {
            while self.tasks.try_join_next().is_some() {}
            let deadline = self.next_deadline(Instant::now());
            let live = self.phase == Phase::Live;
            let max_batch = self.tuning.max_batch.max(1);
            let wake = tokio::select! {
                biased;
                _ = &mut shutdown => Wake::Shutdown,
                command = commands.recv() => command.map_or(Wake::Shutdown, Wake::Command),
                Some(message) = internal_rx.recv() => Wake::Internal(message),
                count = receive_lines(self.lines.as_mut().filter(|_| live), &mut lines, max_batch) => Wake::Lines(count),
                () = sleep_until(deadline) => Wake::Timer,
            };
            match wake {
                Wake::Shutdown => break,
                Wake::Command(command) => self.on_command(command),
                Wake::Internal(message) => self.on_internal(message),
                Wake::Lines(0) => {
                    // The reader is gone without saying why (aborted).
                    self.lines = None;
                }
                Wake::Lines(_) => self.on_lines(std::mem::take(&mut lines)),
                Wake::Timer => {}
            }
            self.run_due();
        }
        self.stop();
    }

    // --- events out ---------------------------------------------------------------

    /// Emits an event. While a snapshot is being evaluated, events wait
    /// until it went out, so the UI sees them in the order they happened:
    /// `Connected` after the snapshot of the load that completed it, an
    /// action's result after the snapshot cut before it.
    fn emit(&mut self, event: CoreEvent) {
        if self.dashboards.is_none() {
            self.outbox.push(event);
        } else {
            self.send_event(event);
        }
    }

    fn send_event(&self, event: CoreEvent) {
        // The UI may be gone (closing): nothing to tell.
        let _ = self.events.unbounded_send(event);
    }

    fn set_state(&mut self, state: ConnectionState) {
        if self.state.as_ref() != Some(&state) {
            self.state = Some(state.clone());
            self.emit(CoreEvent::Connection(state));
        }
    }

    // --- timers -------------------------------------------------------------------

    fn retry_at(&self) -> Option<Instant> {
        match self.phase {
            Phase::Idle { retry_at } => retry_at,
            Phase::Connecting | Phase::Loading | Phase::Live => None,
        }
    }

    fn status_at(&self) -> Option<Instant> {
        let conn = self.conn.as_ref()?;
        if self.phase != Phase::Live || conn.status_in_flight {
            return None;
        }
        conn.next_status
    }

    fn fetch_at(&self) -> Option<Instant> {
        if self.phase != Phase::Live {
            return None;
        }
        self.fetch.due(self.tuning.requery_delay)
    }

    fn next_deadline(&self, now: Instant) -> Option<Instant> {
        [
            self.publish_at(now),
            self.retry_at(),
            self.status_at(),
            self.fetch_at(),
            self.reload_due(),
            self.sweep_at(now),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// Runs whatever is due. Called after every wake-up, so a busy stream
    /// can't starve the timers.
    fn run_due(&mut self) {
        let now = Instant::now();
        if self.retry_at().is_some_and(|at| at <= now) {
            self.connect();
        }
        if self.status_at().is_some_and(|at| at <= now) {
            self.poll_status();
        }
        if self.reload_due().is_some_and(|at| at <= now) {
            self.start_load(false);
        }
        if self.sweep_at(now).is_some_and(|at| at <= now) {
            self.sweep(now);
        }
        if self.fetch_at().is_some_and(|at| at <= now) {
            self.start_fetch(now);
        }
        // Stage 3: the rule engine's one-second tick.
        if self.publish_at(now).is_some_and(|at| at <= now) {
            self.publish();
        }
    }

    // --- connection lifecycle -------------------------------------------------------

    /// Starts a connect attempt (a new session).
    fn connect(&mut self) {
        self.teardown();
        self.session += 1;
        self.phase = Phase::Connecting;
        self.set_state(ConnectionState::Connecting {
            attempt: self.backoff.attempt(),
        });
        let session = self.session;
        let environment = self.spec.environment.clone();
        let secrets = Arc::clone(&self.ports.secrets);
        let tx = self.internal_tx.clone();
        self.tasks.spawn(async move {
            let message = match connect::connect(&environment, secrets).await {
                Ok(connected) => Internal::Connected {
                    session,
                    connected: Box::new(connected),
                },
                Err(failure) => Internal::ConnectFailed { session, failure },
            };
            let _ = tx.send(message);
        });
    }

    /// Ends the current session: aborts its tasks (closing the event
    /// stream) and forgets its pending work.
    fn teardown(&mut self) {
        self.tasks.abort_all();
        self.tasks = JoinSet::new();
        self.lines = None;
        self.conn = None;
        self.load = None;
        self.fetch.reset();
        self.watchdog.reset_awaiting();
        self.reload_at = None;
        self.reconcile_at = None;
        self.store.end_annotation_query();
    }

    /// The session failed: back off and retry, or wait for the user.
    fn fail(&mut self, failure: Failure) {
        let healthy = self
            .conn
            .as_ref()
            .and_then(|conn| conn.live_since)
            .is_some_and(|(since, _)| since.elapsed() >= self.tuning.healthy_after);
        self.teardown();
        self.publish_changes();
        let state = match failure {
            Failure::Transient(error) => {
                if healthy {
                    self.backoff.reset();
                }
                let delay = self.backoff.fail();
                tracing::info!(%error, ?delay, "connection lost; retrying");
                self.phase = Phase::Idle {
                    retry_at: Some(Instant::now() + delay),
                };
                ConnectionState::Reconnecting {
                    error,
                    attempt: self.backoff.attempt(),
                    retry_at: self.ports.clock.now().plus(delay),
                }
            }
            Failure::MissingSecret => {
                self.phase = Phase::Idle { retry_at: None };
                ConnectionState::MissingSecret
            }
            Failure::Misconfigured(message) => {
                tracing::warn!(%message, "the environment's settings can't work");
                self.phase = Phase::Idle { retry_at: None };
                ConnectionState::Misconfigured { message }
            }
            Failure::Auth(message) => {
                tracing::warn!(%message, "Icinga refused the credentials");
                self.phase = Phase::Idle { retry_at: None };
                ConnectionState::AuthFailed { message }
            }
            Failure::Tls {
                message,
                certificate,
                ..
            } => {
                tracing::warn!(%message, "the server certificate isn't trusted");
                self.phase = Phase::Idle { retry_at: None };
                ConnectionState::TlsFailed {
                    message,
                    certificate,
                }
            }
        };
        self.set_state(state);
    }

    fn on_connected(&mut self, connected: Connected) {
        let Connected {
            client,
            info,
            lines,
        } = connected;
        tracing::info!(user = %info.user, version = %info.version, "connected to Icinga");
        self.emit(CoreEvent::Permissions(info.clone()));
        if let Some(stream) = lines {
            let (tx, rx) = mpsc::unbounded_channel();
            self.tasks
                .spawn(stream::read(stream, Arc::clone(&self.seq), tx));
            self.lines = Some(rx);
        }
        let next_status = info
            .allows("status/query")
            .then(|| Instant::now() + self.tuning.status_interval);
        self.conn = Some(Conn {
            client,
            info,
            live_since: None,
            next_status,
            status_in_flight: false,
        });
        if self.loaded {
            // A reconnect: live at once on the objects we have (the stream
            // keeps them current), and a reconcile shortly after for what
            // the gap missed, jittered unless the user asked.
            self.go_live();
            let delay = if std::mem::take(&mut self.reload_now) {
                std::time::Duration::ZERO
            } else {
                self.tuning.reload_jitter.mul_f64(fastrand::f64())
            };
            self.reload_at = Some(Instant::now() + delay);
        } else {
            self.reload_now = false;
            self.phase = Phase::Loading;
            self.start_load(true);
        }
    }

    /// Starts a load (tiers 1–3) unless one runs. `first`: the session's
    /// first load into an empty store, which ends in `Connected`; other
    /// loads (reconcile, `Refresh`, a restart, a reconnect) leave the
    /// connection state alone, and query answers that differ from the
    /// store without an event explaining it are recorded
    /// ([`Engine::record_discovered`]).
    fn start_load(&mut self, first: bool) {
        if self.load.is_some() {
            return;
        }
        let Some(conn) = &self.conn else {
            return;
        };
        self.reload_at = None;
        self.reconcile_at = None;
        self.loads += 1;
        self.load = Some((self.loads, first));
        self.store.begin_annotation_query();
        let task = LoadTask {
            client: conn.client.clone(),
            seq: Arc::clone(&self.seq),
            tx: self.internal_tx.clone(),
            session: self.session,
            load: self.loads,
        };
        self.tasks.spawn(task.run());
    }

    fn on_load(&mut self, load: u64, step: LoadStep) {
        let Some((current, first)) = self.load else {
            return;
        };
        if current != load {
            return;
        }
        match step {
            LoadStep::Progress { phase, done, total } => {
                if first {
                    self.set_state(ConnectionState::Loading { phase, done, total });
                }
            }
            LoadStep::Overview {
                started,
                overview,
                hosts,
            } => {
                self.store.apply_overview(*overview, started);
                self.store.replace_hosts(hosts, started);
                self.record_discovered(true);
                self.publish();
            }
            LoadStep::Services { started, services } => {
                self.store.replace_services(services, Detail::Lean, started);
                self.record_discovered(true);
                self.publish();
            }
            LoadStep::Details { started, fetched } => {
                self.store.apply_fetched(
                    fetched.hosts,
                    fetched.services,
                    Detail::Full,
                    &fetched.missing,
                    started,
                );
                self.record_discovered(true);
                self.publish();
            }
            LoadStep::Done => {
                let now = Instant::now();
                self.load = None;
                self.loaded = true;
                self.last_load_done = Some(now);
                self.fetch.release_deferred(now);
                self.watchdog.loaded(&self.store);
                self.schedule_reconcile();
                if first {
                    self.go_live();
                }
                self.publish_changes();
            }
            LoadStep::Failed(failure) => {
                self.load = None;
                self.store.end_annotation_query();
                if first || matches!(failure, Failure::Auth(_)) {
                    self.fail(failure);
                } else if let Failure::Transient(error) = failure {
                    // The stream decides whether the connection is gone;
                    // the next reconcile comes at the usual interval.
                    tracing::warn!(%error, "reload failed");
                    self.schedule_reconcile();
                }
            }
        }
    }

    /// The first load is in: connected and live.
    fn go_live(&mut self) {
        let now = self.ports.clock.now();
        let endpoint = self.endpoint_name();
        let Some(conn) = &mut self.conn else {
            return;
        };
        conn.live_since = Some((Instant::now(), now));
        let version = conn.info.version.clone();
        self.phase = Phase::Live;
        self.set_state(ConnectionState::Connected {
            endpoint,
            version,
            since: now,
        });
    }

    /// The endpoint's node name, or the URL's host.
    fn endpoint_name(&self) -> String {
        self.store
            .status()
            .map(|status| status.node_name.clone())
            .filter(|name| !name.is_empty())
            .or_else(|| {
                self.conn
                    .as_ref()
                    .and_then(|conn| conn.client.base_url().host_str().map(str::to_owned))
            })
            .unwrap_or_default()
    }

    // --- status poll ------------------------------------------------------------------

    fn poll_status(&mut self) {
        let Some(conn) = &mut self.conn else {
            return;
        };
        conn.status_in_flight = true;
        let client = conn.client.clone();
        let tx = self.internal_tx.clone();
        let session = self.session;
        self.tasks.spawn(async move {
            let result = client.status().await;
            let _ = tx.send(Internal::Status { session, result });
        });
    }

    fn on_status(&mut self, result: Result<InstanceStatus, ApiError>) {
        let interval = self.tuning.status_interval;
        let Some(conn) = &mut self.conn else {
            return;
        };
        conn.status_in_flight = false;
        conn.next_status = Some(Instant::now() + interval);
        match result {
            Ok(status) => {
                let program_start = status.program_start;
                let previous = self.store.set_status(status);
                if previous.is_some_and(|previous| previous.program_start != program_start) {
                    tracing::info!("Icinga restarted; reloading");
                    self.start_load(false);
                }
            }
            Err(ApiError::Unauthorized) => {
                self.fail(Failure::Auth(ApiError::Unauthorized.to_string()));
            }
            Err(ApiError::Forbidden(message)) => {
                tracing::warn!(%message, "status polling refused; stopping it");
                conn.next_status = None;
            }
            Err(error) => tracing::debug!(%error, "status poll failed"),
        }
    }

    // --- re-queries -------------------------------------------------------------------

    fn start_fetch(&mut self, now: Instant) {
        let Some(conn) = &self.conn else {
            return;
        };
        let round = self.fetch.take(now);
        let (mut full, lean): (Vec<ObjectKey>, Vec<ObjectKey>) =
            round.keys.into_iter().partition(|key| match key {
                ObjectKey::Host { .. } => true,
                ObjectKey::Service { key } => self.store.is_full(key),
            });
        full.extend(round.full);
        let task = FetchTask {
            client: conn.client.clone(),
            seq: Arc::clone(&self.seq),
            tx: self.internal_tx.clone(),
            session: self.session,
            full,
            lean,
            lists: round.lists,
            urgent: round.urgent,
        };
        self.tasks.spawn(task.run());
    }

    fn on_fetched(&mut self, answers: Answers) {
        let mut missing = Vec::new();
        for answer in answers.objects {
            match answer.result {
                Ok(fetched) => {
                    self.store.apply_fetched(
                        fetched.hosts,
                        fetched.services,
                        answer.detail,
                        &fetched.missing,
                        answer.started,
                    );
                    self.watchdog.answered(&self.store, &answer.keys);
                    missing.extend(fetched.missing);
                }
                Err(ApiError::Unauthorized) => {
                    self.fail(Failure::Auth(ApiError::Unauthorized.to_string()));
                    return;
                }
                Err(error) => {
                    tracing::warn!(%error, "re-query failed");
                    self.watchdog.failed(&answer.keys);
                }
            }
        }
        self.record_discovered(false);
        apply_list(answers.host_groups, "host groups", |groups| {
            self.store.set_host_groups(groups);
        });
        apply_list(answers.service_groups, "service groups", |groups| {
            self.store.set_service_groups(groups);
        });
        apply_list(answers.dependencies, "dependencies", |dependencies| {
            self.store.set_dependencies(dependencies);
        });
        apply_list(answers.endpoints, "endpoints", |endpoints| {
            self.store.set_endpoints(endpoints);
        });
        self.fetch.finished(&missing, Instant::now());
        if answers.urgent {
            self.publish_changes();
        }
    }

    // --- the event stream ---------------------------------------------------------------

    /// Applies a batch from the reader: parses, collapses check results,
    /// applies in order; then handles the end of the stream if it came.
    fn on_lines(&mut self, batch: Vec<ReaderMsg>) {
        let mut lines = Vec::with_capacity(batch.len());
        let mut end = None;
        for message in batch {
            match message {
                ReaderMsg::Line(seq, line) => lines.push((seq, line)),
                ReaderMsg::End(error) => end = Some(error),
            }
        }
        if !lines.is_empty() {
            self.store.set_last_event_at(self.ports.clock.now());
            let events = stream::prepare(lines);
            if let Some(latest) = events
                .iter()
                .map(|(_, event)| event.at())
                .max_by(|a, b| a.as_unix_seconds().total_cmp(&b.as_unix_seconds()))
            {
                self.watchdog.clock().observe(latest, Instant::now());
            }
            let applied = self.apply_events(events);
            self.record_applied(&applied);
        }
        if let Some(error) = end {
            let error = error.map_or_else(
                || "the event stream was closed by Icinga".to_owned(),
                |error| format!("the event stream broke: {error}"),
            );
            self.fail(Failure::Transient(error));
        }
    }

    fn apply_events(&mut self, events: Vec<(u64, Event)>) -> Vec<AppliedEvent> {
        let now = Instant::now();
        let mut applied = Vec::with_capacity(events.len());
        for (seq, event) in events {
            if let Event::ObjectLifecycle {
                change,
                object_type,
                name,
                ..
            } = &event
            {
                self.on_lifecycle(*change, object_type, name, now);
                continue;
            }
            match self.store.apply(seq, &event) {
                Applied::Changed { before, after } => applied.push(AppliedEvent {
                    seq,
                    event,
                    before,
                    after,
                }),
                Applied::Stale | Applied::Ignored => {}
                Applied::Unknown(key) => {
                    if self.load.is_some() {
                        self.fetch.defer(key);
                    } else {
                        self.fetch.mark(key, now);
                    }
                }
            }
        }
        applied
    }

    /// A config object was created, modified or deleted: re-query hosts and
    /// services by name, reload the small lists. Comments and downtimes
    /// have events of their own.
    fn on_lifecycle(&mut self, change: ObjectChange, object_type: &str, name: &str, now: Instant) {
        let key = match object_type {
            "Host" => Some(ObjectKey::host(name)),
            "Service" => ServiceKey::parse(name).map(ObjectKey::from),
            _ => None,
        };
        if let Some(key) = key {
            if change == ObjectChange::Created {
                self.fetch.mark_created(key, now);
            } else {
                self.fetch.mark(key, now);
            }
            return;
        }
        let lists = Lists {
            host_groups: object_type == "HostGroup",
            service_groups: object_type == "ServiceGroup",
            dependencies: object_type == "Dependency",
            endpoints: matches!(object_type, "Endpoint" | "Zone"),
        };
        self.fetch.mark_lists(lists, now);
    }

    /// What the applied events mean beyond the store.
    ///
    /// Stage 3: turn them into rule inputs (previous state from `before`,
    /// `handled` from the store after applying, memberships from the
    /// dashboards) and event-log entries; also repeat states whose
    /// `handled` changed without an event of their own.
    #[expect(
        clippy::unused_self,
        reason = "the extension point for stage 3, which records into the engine"
    )]
    fn record_applied(&mut self, applied: &[AppliedEvent]) {
        for entry in applied {
            let (Some(before), Some(after)) = (entry.before, entry.after) else {
                continue;
            };
            if before.state != after.state || before.state_type != after.state_type {
                tracing::debug!(
                    seq = entry.seq,
                    object = ?entry.event.object(),
                    from = ?before.state,
                    to = ?after.state,
                    state_type = ?after.state_type,
                    "state changed"
                );
            }
        }
    }

    /// What query answers revealed without an event: state changes the
    /// stream missed, flapping found by a reconcile, objects gone. `load`:
    /// from a load, whose tier 3 brings every problem's details anyway.
    ///
    /// A service whose state such an answer changed keeps its last check
    /// result (a lean answer has none, and never replaces one), but that
    /// result is now older than the state: it is fetched in full again, so
    /// the output matches the state within a second (problems found by a
    /// load come with its tier 3).
    ///
    /// Stage 3: turn them into rule inputs (missed problems and recoveries
    /// notify, removals end what the rule engine remembers), except during
    /// the session's first load (`self.load` is `Some((_, true))`), which
    /// may follow a partial one: the initial load produces no rule inputs.
    fn record_discovered(&mut self, load: bool) {
        let found = self.store.take_discovered();
        if found.is_empty() {
            return;
        }
        let mut stale = Vec::new();
        for Discovered {
            object,
            before,
            after,
        } in &found
        {
            tracing::debug!(
                %object,
                from = ?before.state,
                to = ?after.map(|after| after.state),
                "a query found a change no event announced"
            );
            let Some(after) = after else {
                continue;
            };
            let covered = load && after.state.is_problem();
            if after.state != before.state
                && !covered
                && object
                    .as_service()
                    .is_some_and(|key| self.store.result_is_stale(key))
            {
                stale.push(object.clone());
            }
        }
        if !stale.is_empty() {
            self.fetch.mark_full(stale, Instant::now());
        }
    }

    // --- commands ---------------------------------------------------------------------

    fn on_command(&mut self, command: Command) {
        match command {
            Command::Action { id, target, action } => self.run_action(id, target, action),
            Command::Refresh => self.refresh(),
            Command::UpdateEnvironment(environment) => self.update_environment(environment),
            Command::UpdateGeneral(general) => self.update_general(general),
            Command::LoadHistory { reply, .. } => {
                // Stage 3: the event log.
                tracing::debug!("LoadHistory is not implemented yet");
                let _ = reply.send(Vec::new());
            }
            Command::LoadNotifications { reply, .. } => {
                // Stage 3: the event log.
                tracing::debug!("LoadNotifications is not implemented yet");
                let _ = reply.send(Vec::new());
            }
            Command::PreviewDashboard { view, reply } => self.preview(view, reply),
            Command::PauseNotifications(_) | Command::MarkNotificationsRead => {
                // Stage 3: notifications.
                tracing::debug!("notification commands are not implemented yet");
            }
            Command::Hydrate(keys) => self.hydrate(keys),
        }
    }

    fn refresh(&mut self) {
        match self.phase {
            Phase::Idle { retry_at } => {
                if retry_at.is_none() {
                    // After a failure that needed the user: a fresh start.
                    self.backoff.reset();
                }
                self.reload_now = true;
                self.connect();
            }
            Phase::Connecting | Phase::Loading => {
                tracing::debug!("refresh: a connection attempt or load is already running");
            }
            Phase::Live => self.start_load(false),
        }
    }

    fn update_environment(&mut self, environment: ic_config::Environment) {
        let old = &self.spec.environment;
        let reconnect = old.id != environment.id
            || old.url != environment.url
            || old.auth != environment.auth
            || old.tls != environment.tls;
        let other_server = old.id != environment.id || old.url != environment.url;
        let dashboards_changed = old.groups != environment.groups;
        self.spec.environment = environment;
        // Stage 3: rebuild the rule set.
        if dashboards_changed {
            self.dashboards_configured = true;
        }
        if other_server {
            self.store.clear();
            self.watchdog.clear();
            self.loaded = false;
            self.publish();
        } else if dashboards_changed {
            self.publish_changes();
        }
        let waiting_for_user = matches!(self.phase, Phase::Idle { .. });
        if reconnect || waiting_for_user {
            self.backoff.reset();
            self.reload_now = true;
            self.connect();
        }
    }

    // --- internal messages --------------------------------------------------------------

    fn on_internal(&mut self, message: Internal) {
        match message {
            Internal::ActionDone { id, dirty, outcome } => self.on_action_done(id, dirty, outcome),
            Internal::Connected { session, connected } if session == self.session => {
                self.on_connected(*connected);
            }
            Internal::ConnectFailed { session, failure } if session == self.session => {
                self.fail(failure);
            }
            Internal::Load {
                session,
                load,
                step,
            } if session == self.session => self.on_load(load, step),
            Internal::Status { session, result } if session == self.session => {
                self.on_status(result);
            }
            Internal::Fetched { session, answers } if session == self.session => {
                self.on_fetched(*answers);
            }
            Internal::Evaluated {
                dashboards,
                snapshot,
                quiet,
                broken,
            } => self.on_evaluated(dashboards, *snapshot, quiet, broken),
            Internal::PreviewDone => self.on_preview_done(),
            // An older session's late answer.
            Internal::Connected { .. }
            | Internal::ConnectFailed { .. }
            | Internal::Load { .. }
            | Internal::Status { .. }
            | Internal::Fetched { .. } => {}
        }
    }

    /// Stops: ends the session (closing the stream).
    ///
    /// Stage 3: flush the event log here.
    fn stop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        for event in std::mem::take(&mut self.outbox) {
            self.send_event(event);
        }
        self.teardown();
        tracing::debug!("engine stopped");
    }
}

/// Applies a reloaded small list, if it was reloaded and allowed.
fn apply_list<T>(answer: Option<Result<Vec<T>, ApiError>>, what: &str, apply: impl FnOnce(Vec<T>)) {
    match answer {
        Some(Ok(list)) => apply(list),
        Some(Err(error)) => tracing::warn!(%error, "couldn't reload the {what}"),
        None => {}
    }
}

/// Receives up to `limit` reader messages; pends forever without a
/// receiver. Returns 0 when the reader is gone.
async fn receive_lines(
    receiver: Option<&mut UnboundedReceiver<ReaderMsg>>,
    buffer: &mut Vec<ReaderMsg>,
    limit: usize,
) -> usize {
    match receiver {
        Some(receiver) => receiver.recv_many(buffer, limit).await,
        None => std::future::pending().await,
    }
}

/// Sleeps until `deadline`; forever without one.
async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod perf_tests;
