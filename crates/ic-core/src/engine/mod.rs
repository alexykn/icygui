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
//! - Notifications and the event log (`notify.rs`, `crate::event_log`):
//!   every applied change becomes rule inputs and log entries
//!   ([`Engine::record_applied`], [`Engine::record_discovered`]); the rule
//!   engine judges them once the dashboards' memberships are known and
//!   ticks every second; intents are logged, then emitted.
//! - Icinga's own `Notification` objects (who Icinga notified, and when)
//!   load in the background after the problem lists and follow Icinga's
//!   `Notification` events.

mod actions;
mod fetch;
mod load;
mod notify;
mod publish;
mod stream;
mod sync;
mod watchdog;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use ic_api::{ApiError, ApiInfo, Client, Detail};
use ic_model::{
    Event, EventKind, Host, InstanceStatus, Notification, ObjectChange, ObjectKey, Service,
    ServiceKey, Timestamp,
};
use ic_rules::{DashboardRef, NotificationIntent};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot;
use tokio::task::JoinSet;
use tokio::time::Instant;

use crate::backoff::Backoff;
use crate::command::{ActionOutcome, Command, ConnectionState, CoreEvent, LoadPhase, LogEntry};
use crate::connect::{self, Connected, Failure};
use crate::dashboards::Dashboards;
use crate::event_log::{EventLog, event_log_path};
use crate::snapshot::{DashboardResult, Snapshot};
use crate::spec::{EnvironmentSpec, Ports, Tuning};
use crate::store::{Applied, Discovered, ObjectView, Overview, Store, notification_object};

use fetch::{Answers, FetchQueue, FetchTask, Lists};
use load::LoadTask;
use notify::Notify;
use publish::Previews;
use stream::ReaderMsg;
use sync::Restarts;
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
    /// Notifications are in the event log (the new ones; an id already
    /// there from an earlier run is dropped): emit them.
    Logged(Vec<NotificationIntent>),
    /// Every `Notification` object (Icinga's own notifications), queried
    /// when the reader had read `started` lines.
    IcingaNotifications {
        session: u64,
        started: u64,
        result: Result<Vec<Notification>, ApiError>,
    },
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
    /// Tier 2. The engine answers with the problems whose details tier 3
    /// fetches.
    Services {
        started: u64,
        services: Vec<Service>,
        details: oneshot::Sender<Vec<ObjectKey>>,
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
    /// For a removed downtime: whether the store had it in effect.
    pub(crate) downtime_was_in_effect: bool,
    /// For a check result: the object's previous check, if known.
    pub(crate) previous_check: Option<Timestamp>,
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

/// Why a load runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LoadKind {
    /// The session's first load into an empty store: no rule inputs, and
    /// `Connected` once it is in.
    First,
    /// A reload after a reconnect, an Icinga restart or `Refresh`: every
    /// problem's details again (the gap may hide config changes).
    Reload,
    /// The periodic reconcile: tier 3 fetches only the problems the store
    /// doesn't hold in full or whose result is older than their last check
    /// (events keep the others current), so an outage with thousands of
    /// problems doesn't reload all their details every interval.
    Reconcile,
}

/// The established connection of the current session.
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent facts about the session and its requests"
)]
struct Conn {
    client: Client,
    info: ApiInfo,
    /// When it went live (for the backoff reset and `Connected.since`).
    live_since: Option<(Instant, Timestamp)>,
    /// The next status poll; `None` without `status/query` permission.
    next_status: Option<Instant>,
    status_in_flight: bool,
    /// The API user may read `Notification` objects (cleared when Icinga
    /// refuses them after all).
    notifications_allowed: bool,
    /// The stream carries Icinga's `Notification` events, which keep the
    /// `Notification` objects current between reloads.
    notification_events: bool,
    /// The notification list query is running.
    notifications_in_flight: bool,
    /// `Notification` events read while it runs: the objects whose
    /// notifications to re-read if the list turns out older.
    notification_events_waiting: Vec<(u64, ObjectKey)>,
    /// The stream carries `CheckResult` events: a status poll reporting
    /// checks while no line arrived for `Tuning::stall_after` means it
    /// stalled.
    check_events: bool,
    /// When the engine last received stream lines (or the stream opened,
    /// or the session went live).
    last_line: Instant,
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
    /// The load in flight: its id and kind.
    load: Option<(u64, LoadKind)>,
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
    /// The next reload ([`LoadKind::Reload`]): after a reconnect
    /// (jittered), a restart or `Refresh` (spaced, see
    /// [`Engine::request_reload`]).
    reload_at: Option<Instant>,
    /// When the last reload started.
    last_reload: Option<Instant>,
    /// The next periodic reconcile.
    reconcile_at: Option<Instant>,
    /// When the last load ended, complete or failed: the next reconcile
    /// counts from it.
    last_load_end: Option<Instant>,
    /// The `program_start` each Icinga node reported (restart detection).
    restarts: Restarts,
    /// The connection state last emitted.
    state: Option<ConnectionState>,
    /// Notifications: the rule engine and its inputs.
    notify: Notify,
    /// The rule engine's next tick.
    tick_at: Instant,
    /// The local event log.
    event_log: EventLog,
    /// The next pruning of the event log.
    prune_at: Instant,
    /// The `Notification` objects in the store are current: loaded in this
    /// session, whose stream carries `Notification` events since. Periodic
    /// reconciles then skip them; a reconnect, a restart or `Refresh`
    /// reloads them.
    notifications_current: bool,
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
        let event_log = EventLog::open(event_log_path(&spec.data_dir, &spec.environment.id));
        let notify = Notify::new(&spec.environment);
        let now = Instant::now();
        Self {
            notify,
            tick_at: now + tuning.rule_tick,
            event_log,
            // Pruned when the engine starts running, then every
            // `prune_interval`.
            prune_at: now,
            notifications_current: false,
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
            last_reload: None,
            reconcile_at: None,
            last_load_end: None,
            restarts: Restarts::default(),
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
        // Pruned before any query can reach the log.
        self.prune(Instant::now());
        let mut lines = Vec::new();
        loop {
            self.reap_tasks();
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
                    // The reader is gone without saying why (it panicked:
                    // a teardown, which aborts it, also drops the
                    // receiver). Without a stream nothing stays current.
                    tracing::error!("the event stream reader ended unexpectedly");
                    self.fail(Failure::Transient(
                        "the event stream ended unexpectedly".to_owned(),
                    ));
                }
                Wake::Lines(_) => {
                    if let Some(conn) = &mut self.conn {
                        conn.last_line = Instant::now();
                    }
                    self.on_lines(std::mem::take(&mut lines));
                }
                Wake::Timer => {}
            }
            self.run_due();
        }
        self.stop();
    }

    /// Collects the session's finished tasks. A task that panicked (a bug)
    /// leaves its part of the session stuck (a load, the re-query queue,
    /// the status poll), so the session starts over: logged, then a
    /// reconnect with backoff and a reload.
    fn reap_tasks(&mut self) {
        while let Some(result) = self.tasks.try_join_next() {
            if let Err(error) = result
                && error.is_panic()
            {
                tracing::error!(%error, "a background task panicked; reconnecting");
                self.fail(Failure::Transient(
                    "an internal task failed; reconnecting".to_owned(),
                ));
            }
        }
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
            Some(self.tick_at),
            Some(self.prune_at),
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
        if let Some(kind) = self.due_load(now) {
            self.start_load(kind);
        }
        if self.sweep_at(now).is_some_and(|at| at <= now) {
            self.sweep(now);
        }
        if self.fetch_at().is_some_and(|at| at <= now) {
            self.start_fetch(now);
        }
        if self.tick_at <= now {
            self.tick(now);
        }
        if self.prune_at <= now {
            self.prune(now);
        }
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
        // The stream gap may have missed `Notification` events.
        self.notifications_current = false;
        // What an aborted load found is real all the same.
        self.finish_discovered();
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
        let notifications_allowed = info.allows("objects/query/Notification");
        let notification_events = self.lines.is_some()
            && info.allows(&format!("events/{}", EventKind::Notification.api_name()));
        let check_events = self.lines.is_some()
            && info.allows(&format!("events/{}", EventKind::CheckResult.api_name()));
        // Without `objects/query/<type>` a kind isn't asked for by name at
        // all (events about its objects can't be looked up).
        self.fetch.refuse(
            !info.allows("objects/query/Host"),
            !info.allows("objects/query/Service"),
        );
        self.conn = Some(Conn {
            client,
            info,
            live_since: None,
            next_status,
            status_in_flight: false,
            notifications_allowed,
            notification_events,
            notifications_in_flight: false,
            notification_events_waiting: Vec::new(),
            check_events,
            last_line: Instant::now(),
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
            self.start_load(LoadKind::First);
        }
    }

    /// Starts a load (tiers 1–3) unless one runs. The session's first
    /// load into an empty store ends in `Connected`; other loads (reconcile,
    /// `Refresh`, a restart, a reconnect) leave the connection state alone,
    /// and query answers that differ from the store without an event
    /// explaining it are recorded ([`Engine::record_discovered`]).
    fn start_load(&mut self, kind: LoadKind) {
        if self.load.is_some() {
            return;
        }
        let Some(conn) = &self.conn else {
            return;
        };
        self.reload_at = None;
        self.reconcile_at = None;
        if kind == LoadKind::Reload {
            self.last_reload = Some(Instant::now());
        }
        self.loads += 1;
        self.load = Some((self.loads, kind));
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
        let Some((current, kind)) = self.load else {
            return;
        };
        if current != load {
            return;
        }
        let first = kind == LoadKind::First;
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
                if let Some(status) = &overview.status {
                    // The load reloads anyway.
                    self.restarts.observe(status);
                }
                self.store.apply_overview(*overview, started);
                self.store.replace_hosts(hosts, started);
                self.record_discovered(true);
                self.publish();
            }
            LoadStep::Services {
                started,
                services,
                details,
            } => {
                self.store.replace_services(services, Detail::Lean, started);
                self.record_discovered(true);
                // The load task is gone if the session ended meanwhile.
                let _ = details.send(self.problem_details(kind));
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
                self.last_load_end = Some(now);
                self.fetch
                    .release_deferred(now, |key| self.store.contains(key));
                self.watchdog.loaded(&self.store);
                self.schedule_reconcile();
                self.finish_discovered();
                if first {
                    self.go_live();
                }
                if !self.notifications_current {
                    self.load_notifications();
                }
                self.publish_changes();
            }
            LoadStep::Failed(failure) => {
                self.load = None;
                self.last_load_end = Some(Instant::now());
                self.store.end_annotation_query();
                self.finish_discovered();
                if first || matches!(failure, Failure::Auth(_)) {
                    self.fail(failure);
                } else if let Failure::Transient(error) = failure {
                    // The stream decides whether the connection is gone; a
                    // failed reload waits a whole interval from now, so a
                    // struggling master (or a proxy answering 502) isn't
                    // asked again right away.
                    tracing::warn!(%error, "reload failed; the next one follows at the usual interval");
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
        // Lines waited during the first load: the stall watch starts now.
        conn.last_line = Instant::now();
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
                if let Some(error) = self.stalled(&status) {
                    tracing::warn!(%error, "reconnecting");
                    self.fail(Failure::Transient(error));
                    return;
                }
                // Only the same node with another start time restarted: the
                // masters of an HA zone behind a load balancer each have
                // their own.
                let restarted = self.restarts.observe(&status);
                self.store.set_status(status);
                if restarted {
                    tracing::info!("Icinga restarted; reloading");
                    self.request_reload();
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

    /// Whether the event stream stalled without closing (a proxy or a
    /// stuck queue; Icinga sends nothing on an idle stream, and TCP
    /// keepalives may be answered by a middlebox): Icinga reports active
    /// checks in the last minute, which each send a `CheckResult` event, yet
    /// no line arrived for `Tuning::stall_after` (2 minutes, so the checks
    /// counted happened well after the last line). Returns why.
    fn stalled(&self, status: &InstanceStatus) -> Option<String> {
        let conn = self.conn.as_ref()?;
        let silent = conn.last_line.elapsed();
        let idle = self.lines.as_ref().is_some_and(UnboundedReceiver::is_empty);
        (conn.check_events
            && idle
            && status.checks_per_minute >= 1.0
            && silent >= self.tuning.stall_after)
            .then(|| {
                format!(
                    "the event stream stalled: Icinga ran {:.0} checks in the last minute, but no event arrived for {} s",
                    status.checks_per_minute,
                    silent.as_secs()
                )
            })
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
            unknown: round.unknown,
            lists: round.lists,
            urgent: round.urgent,
            notifications: round.notifications,
        };
        self.tasks.spawn(task.run());
    }

    fn on_fetched(&mut self, answers: Answers) {
        let mut missing = Vec::new();
        for answer in answers.objects {
            match answer.result {
                Ok(fetched) => {
                    // A whole-batch lookup's missing names weren't found,
                    // which doesn't make them gone (one name of the batch
                    // was unknown): they wait like missing ones, but leave
                    // the store alone.
                    let gone: &[ObjectKey] = if answer.whole { &[] } else { &fetched.missing };
                    self.store.apply_fetched(
                        fetched.hosts,
                        fetched.services,
                        answer.detail,
                        gone,
                        answer.started,
                    );
                    self.watchdog.answered(&self.store, &answer.keys);
                    let store = &self.store;
                    missing.extend(
                        fetched
                            .missing
                            .into_iter()
                            .filter(|key| !answer.whole || !store.contains(key)),
                    );
                }
                Err(ApiError::Unauthorized) => {
                    self.fail(Failure::Auth(ApiError::Unauthorized.to_string()));
                    return;
                }
                Err(ApiError::Forbidden(message)) => {
                    // The user may not query this kind after all: not asked
                    // for again this session (events about such objects
                    // can't be looked up).
                    let hosts = answer
                        .keys
                        .iter()
                        .any(|key| matches!(key, ObjectKey::Host { .. }));
                    let kind = if hosts { "hosts" } else { "services" };
                    tracing::warn!(%message, "the API user may not query {kind} by name; not asking again");
                    self.fetch.refuse(hosts, !hosts);
                    self.watchdog.failed(&answer.keys);
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
        if let Some((started, result)) = answers.notifications {
            match result {
                Ok(fetched) => self.store.apply_fetched_notifications(
                    fetched.notifications,
                    &fetched.missing,
                    started,
                ),
                Err(ApiError::Unauthorized) => {
                    self.fail(Failure::Auth(ApiError::Unauthorized.to_string()));
                    return;
                }
                Err(error) => self.notifications_refused(&error),
            }
        }
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
            self.apply_events(events);
        }
        if let Some(error) = end {
            let error = error.map_or_else(
                || "the event stream was closed by Icinga".to_owned(),
                |error| format!("the event stream broke: {error}"),
            );
            self.fail(Failure::Transient(error));
        }
    }

    /// Applies events in order; each applied change is recorded at once
    /// (rule inputs, log entries), while the store shows its result.
    fn apply_events(&mut self, events: Vec<(u64, Event)>) {
        let now = Instant::now();
        let mut log = Vec::new();
        for (seq, event) in events {
            match &event {
                Event::ObjectLifecycle {
                    change,
                    object_type,
                    name,
                    ..
                } => {
                    self.on_lifecycle(seq, *change, object_type, name, now);
                    continue;
                }
                Event::Notification { object, .. } => {
                    self.on_icinga_notification(seq, object, now);
                    continue;
                }
                _ => {}
            }
            let downtime_was_in_effect = match &event {
                Event::DowntimeRemoved { downtime, .. } => self
                    .store
                    .downtime_of(&downtime.object, &downtime.name)
                    .is_some_and(|stored| stored.in_effect),
                _ => false,
            };
            let previous_check = match &event {
                Event::CheckResult { object, .. } => {
                    self.store.check(object).and_then(|check| check.last_check)
                }
                _ => None,
            };
            match self.store.apply(seq, &event) {
                Applied::Changed { before, after } => {
                    let entry = AppliedEvent {
                        seq,
                        event,
                        before,
                        after,
                        downtime_was_in_effect,
                        previous_check,
                    };
                    self.record_applied(&entry, &mut log);
                }
                Applied::Stale | Applied::Ignored => {}
                Applied::Unknown(key) => {
                    if self.load.is_some() {
                        self.fetch.defer(key);
                    } else {
                        self.fetch.mark_unknown(key, now);
                    }
                }
            }
        }
        self.event_log.record(log);
    }

    /// A config object was created, modified or deleted (event `seq`):
    /// re-query hosts and services by name, reload the small lists.
    /// Comments and downtimes have events of their own.
    fn on_lifecycle(
        &mut self,
        seq: u64,
        change: ObjectChange,
        object_type: &str,
        name: &str,
        now: Instant,
    ) {
        let key = match object_type {
            "Host" => Some(ObjectKey::host(name)),
            "Service" => ServiceKey::parse(name).map(ObjectKey::from),
            _ => None,
        };
        if let Some(key) = key {
            if change == ObjectChange::Deleted {
                // Answers sent before can't bring it (back) into the store.
                self.store.note_deleted(key.clone(), seq);
            }
            match change {
                ObjectChange::Created => self.fetch.mark_created(key, now),
                _ if self.store.contains(&key) => {
                    self.fetch.mark(key, now);
                }
                // Deleted, and not in the store (its tombstone keeps it
                // out).
                ObjectChange::Deleted => {}
                // Changed, but unknown (hidden from this user, or left out
                // by a load): looked up like other unknown objects.
                ObjectChange::Modified => {
                    self.fetch.mark_unknown(key, now);
                }
            }
            return;
        }
        if object_type == "Notification" {
            // Created, changed or deleted by name (a deleted one comes back
            // missing and leaves the store).
            if self
                .conn
                .as_ref()
                .is_some_and(|conn| conn.notifications_allowed)
                && notification_object(name).is_some()
            {
                self.fetch.mark_notifications([name.to_owned()], now);
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

    /// What an applied event means beyond the store: rule inputs (judged
    /// once the dashboards' memberships are known) and log entries,
    /// appended to `log`.
    fn record_applied(&mut self, entry: &AppliedEvent, log: &mut Vec<LogEntry>) {
        if let (Some(before), Some(after)) = (entry.before, entry.after)
            && (before.state != after.state || before.state_type != after.state_type)
        {
            tracing::debug!(
                seq = entry.seq,
                object = ?entry.event.object(),
                from = ?before.state,
                to = ?after.state,
                state_type = ?after.state_type,
                "state changed"
            );
        }
        self.notify.applied(&self.store, entry, log);
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
    /// They become rule inputs (missed problems and recoveries notify,
    /// removals end what the rule engine remembers) and log entries; a
    /// load's wait until it is over, when its last tier brought the
    /// problems' output. Not during the session's first load
    /// (`self.load` is `Some((_, LoadKind::First))`), which may follow a partial one:
    /// the initial load produces no rule inputs.
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
        if matches!(self.load, Some((_, LoadKind::First))) {
            return;
        }
        let mut log = Vec::new();
        let at = self.evaluation_time();
        self.notify
            .discovered(&self.store, found, load, at, &mut log);
        self.event_log.record(log);
    }

    /// A load is over (or cut off): what it found is judged now.
    fn finish_discovered(&mut self) {
        let mut log = Vec::new();
        self.notify.load_finished(&self.store, &mut log);
        self.event_log.record(log);
    }

    // --- commands ---------------------------------------------------------------------

    fn on_command(&mut self, command: Command) {
        match command {
            Command::Action { id, target, action } => self.run_action(id, target, action),
            Command::Refresh => self.refresh(),
            Command::UpdateEnvironment(environment) => self.update_environment(environment),
            Command::UpdateGeneral(general) => self.update_general(general),
            Command::LoadHistory {
                object,
                limit,
                reply,
            } => self.event_log.history(object, limit, reply),
            Command::LoadNotifications { limit, reply } => {
                self.event_log.notifications(limit, reply);
            }
            Command::PreviewDashboard { view, reply } => self.preview(view, reply),
            Command::PauseNotifications(until) => {
                let paused = self.notify.pause(until);
                tracing::info!(?paused, "notifications paused");
                self.emit(CoreEvent::NotificationsPaused(paused));
            }
            Command::MarkNotificationsRead => self.event_log.mark_read(),
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
            Phase::Live => self.request_reload(),
        }
    }

    fn update_environment(&mut self, environment: ic_config::Environment) {
        let old = &self.spec.environment;
        let reconnect = old.id != environment.id
            || old.url != environment.url
            || old.auth != environment.auth
            || old.tls != environment.tls;
        let other_server = old.id != environment.id || old.url != environment.url;
        let other_log = old.id != environment.id;
        let dashboards_changed = old.groups != environment.groups;
        self.spec.environment = environment;
        if dashboards_changed {
            self.dashboards_configured = true;
        }
        if other_log {
            // Each environment has its own log; the old one finishes its
            // work on its own thread.
            self.event_log = EventLog::open(event_log_path(
                &self.spec.data_dir,
                &self.spec.environment.id,
            ));
            self.prune(Instant::now());
        }
        if other_server {
            // Nothing the rule engine remembers is about this server.
            self.notify.reset(&self.spec.environment);
            self.notifications_current = false;
            self.store.clear();
            self.watchdog.clear();
            self.restarts = Restarts::default();
            self.loaded = false;
            self.publish();
        } else {
            self.notify.set_rules(&self.spec.environment);
            if dashboards_changed {
                self.publish_changes();
            }
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
            Internal::Logged(intents) => self.on_logged(intents),
            Internal::IcingaNotifications {
                session,
                started,
                result,
            } if session == self.session => self.on_icinga_notifications(started, result),
            // An older session's late answer.
            Internal::Connected { .. }
            | Internal::ConnectFailed { .. }
            | Internal::Load { .. }
            | Internal::Status { .. }
            | Internal::Fetched { .. }
            | Internal::IcingaNotifications { .. } => {}
        }
    }

    /// Stops: ends the session (closing the stream) and lets the event log
    /// write what it was given (a bounded wait).
    fn stop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        for event in std::mem::take(&mut self.outbox) {
            self.send_event(event);
        }
        self.teardown();
        let flush = (self.tuning.shutdown_timeout / 2).min(LOG_FLUSH_TIMEOUT);
        self.event_log.close(flush);
        tracing::debug!("engine stopped");
    }
}

/// How long stopping waits at most for the event log to write what it was
/// given.
const LOG_FLUSH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

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
