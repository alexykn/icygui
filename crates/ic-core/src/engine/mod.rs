//! The engine of one environment (the app runs one per environment): one
//! task on the core's runtime that owns the [`Store`] and reacts to
//! commands, to its own background tasks (connect, loads, re-queries,
//! status polls, actions) and to the event stream.
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
//!   hydration, the periodic reconcile, the jittered reload after a
//!   reconnect that followed a long gap, restarts and `Refresh`, all
//!   spaced so a struggling master isn't asked again and again.
//!
//! - Notifications and the event log (`notify.rs`, `crate::event_log`):
//!   every applied change becomes rule inputs and log entries
//!   ([`Engine::record_applied`], [`Engine::record_discovered`]); the rule
//!   engine judges them once the dashboards' memberships are known and
//!   ticks every second; intents are logged, then emitted.
//! - Icinga's own `Notification` objects (who Icinga notified, and when)
//!   load in the background after the problem lists and follow Icinga's
//!   `Notification` events.
//! - Quiet mode and the instant wake-up (`quiet.rs`, PERF-09): the stream
//!   switched to state changes only and back without losing or repeating
//!   an event, quiet schedules, the object the user opens ahead of every
//!   queue, prefetches on notification, background starts.
//! - Every by-name request takes a token from the engine's request budget
//!   ([`ic_api::RequestBudget`]); the object the user opens goes first.

mod actions;
mod fetch;
mod health;
mod heartbeat;
mod load;
mod notify;
mod publish;
mod quiet;
mod recent;
mod stream;
mod sync;
mod trouble;
mod watchdog;

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use ic_api::{ApiError, ApiInfo, Client, Detail, EventLines, RequestBudget};
use ic_model::{
    Event, EventKind, Host, InstanceStatus, Notification, ObjectChange, ObjectKey, Service,
    ServiceKey, Timestamp,
};
use ic_rules::{DashboardRef, NotificationIntent};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot;
use tokio::task::{AbortHandle, JoinSet};
use tokio::time::Instant;

use crate::backoff::Backoff;
use crate::command::{Command, ConnectionState, CoreEvent, LoadPhase, LogEntry};
use crate::connect::{self, Connected, Failure};
use crate::dashboards::Dashboards;
use crate::event_log::{EventLog, event_log_path};
use crate::snapshot::{DashboardResult, Snapshot};
use crate::spec::{EnvironmentSpec, Ports, Start, Tuning};
use crate::store::{Applied, Discovered, ObjectView, Overview, Store, notification_object};
use crate::topology::{self, ConnectedNode};

use fetch::{Answers, FetchQueue, FetchTask, Lists};
use load::LoadTask;
use notify::Notify;
use publish::Previews;
use stream::ReaderMsg;
use sync::Restarts;
use watchdog::Watchdog;

/// The cluster nodes a status poll asked for, and their states.
type NodeStates = (Vec<String>, Result<Vec<ic_api::EndpointState>, ApiError>);

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
    /// A probe of the URLs preferred over the connected one is back:
    /// `better` is one whose node sees more of the cluster (ENV-12).
    Probed { session: u64, better: Option<usize> },
    /// A step of a load.
    Load {
        session: u64,
        load: u64,
        step: LoadStep,
    },
    /// A status poll's answer, and the cluster nodes' states and the
    /// cluster health page's requests when they were asked for too.
    Status {
        session: u64,
        result: Result<InstanceStatus, ApiError>,
        nodes: Option<NodeStates>,
        health: Option<Box<health::Answers>>,
    },
    /// The cluster health page's requests, asked for when it opened.
    Health {
        session: u64,
        answers: Box<health::Answers>,
    },
    /// A re-query round's answers.
    Fetched { session: u64, answers: Box<Answers> },
    /// An action finished.
    ActionDone(Box<actions::Finished>),
    /// The dashboards were evaluated for `snapshot`, which is ready to go
    /// out. `quiet`: nothing but the dashboards could have changed (no
    /// object, mode or updating set).
    /// `broken`: the evaluation failed (a bug); `dashboards` start over.
    Evaluated {
        dashboards: Box<Dashboards>,
        snapshot: Box<Snapshot>,
        quiet: bool,
        broken: bool,
    },
    /// A dashboard preview was answered.
    PreviewDone,
    /// The event log's newest entries, read at the start (`generation`:
    /// which read).
    RecentEvents {
        generation: u64,
        entries: Vec<LogEntry>,
    },
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
    /// The event log has `known` of the notification ids `asked` (problems
    /// the rule engine was seeded with): an earlier run notified them.
    NotifiedBefore {
        asked: Vec<String>,
        known: Vec<String>,
    },
    /// The new stream of a switch between quiet and live mode is open (or
    /// not).
    StreamOpened {
        session: u64,
        switch: u64,
        result: Result<(EventLines, Vec<EventKind>, Option<String>), ApiError>,
    },
    /// The object the user opened (`Command::Focus`), fetched in full when
    /// the reader had read `started` lines.
    Focused {
        session: u64,
        key: ObjectKey,
        started: u64,
        result: Result<ic_api::Fetched, ApiError>,
    },
    /// A background start's size: Icinga's service count, if it could be
    /// read.
    StartSize { session: u64, services: Option<u32> },
    /// The heartbeats the event log remembers (`generation`: which log).
    RememberedBeats {
        generation: u64,
        beats: Vec<crate::event_log::RememberedBeat>,
    },
    /// A query of heartbeats is back (sent when the reader had read
    /// `started` lines): `late` are those a deadline sent, the others came
    /// with a poll.
    Beats {
        session: u64,
        started: u64,
        late: Vec<ServiceKey>,
        result: Result<ic_api::Fetched, ApiError>,
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
            .field("quiet", &self.quiet)
            .field("node", &self.node)
            .finish_non_exhaustive()
    }
}

/// The type of a state change line (a quick test before parsing).
const STATE_CHANGE: &[u8] = b"\"StateChange\"";

/// Icinga's service counts by state at a quiet status poll (see
/// `quiet.rs`): the node that answered, the counts, when.
#[derive(Debug)]
pub(crate) struct QuietCounts {
    node: String,
    counts: [u32; 4],
    at: Instant,
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
    /// The session's first load into an empty store: no rule inputs (it
    /// seeds the rule engine), and `Connected` once it is in.
    First,
    /// A reload after a reconnect that followed a long gap, or an Icinga
    /// restart: every problem's details and Icinga's notifications again
    /// (the gap may hide config changes).
    Reload,
    /// `Refresh` by the user while connected: like a reconcile (the events
    /// keep the rest current), counted in the reload spacing.
    Refresh,
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
    /// The node it reached, and how much of the cluster that node sees.
    node: Arc<ConnectedNode>,
    /// The login, for probes of the URLs preferred over this one.
    login: connect::Login,
    /// When the session connected (its stream opened).
    since: Instant,
    /// When it went live (for the backoff reset and `Connected.since`).
    live_since: Option<(Instant, Timestamp)>,
    /// The next status poll; `None` without `status/query` permission.
    next_status: Option<Instant>,
    status_in_flight: bool,
    /// When the last status poll was sent.
    status_sent: Instant,
    /// When a status poll next asks for the cluster nodes' states too (one
    /// more small request: every poll while on screen, every
    /// [`NODES_QUIET_INTERVAL`] off screen); `None` without
    /// `objects/query/Endpoint`.
    next_nodes: Option<Instant>,
    /// When a status poll last asked for them (before the first: the load
    /// brought them, `live_since`).
    nodes_asked: Option<Instant>,
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
    /// The stream's event types.
    kinds: Vec<EventKind>,
    /// Its filter: a quiet stream's, naming the heartbeats whose results
    /// it carries.
    filter: Option<String>,
    /// The stream carries no check results (quiet mode; but the
    /// heartbeats' with a `filter`).
    quiet: bool,
    /// When the engine last received stream lines (or the stream opened,
    /// or the session went live).
    last_line: Instant,
    /// The cluster health page's requests in this session.
    health: health::Asked,
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
    /// Spaces the retries of a failing first load (its answers are the
    /// big ones), independently of the connection's backoff.
    load_backoff: Backoff,
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
    /// The environment is on screen (`Command::SetActive`): snapshots go
    /// out every `publish_interval`, else every
    /// `background_publish_interval` unless rule inputs wait.
    active: bool,
    /// The cluster health page shows this environment
    /// (`Command::WatchHealth`).
    health_watch: bool,
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
    /// reconnect goes live on it at once (and reloads after a long gap).
    loaded: bool,
    /// The next connect was asked for by the user: reload without jitter.
    reload_now: bool,
    /// The next reload: after a reconnect that followed a long gap
    /// (jittered), a restart or `Refresh` (spaced, see
    /// [`Engine::request_reload`]).
    reload_at: Option<Instant>,
    /// The pending reload must bring everything ([`LoadKind::Reload`]),
    /// not only what isn't current ([`LoadKind::Refresh`]).
    reload_full: bool,
    /// The user asked for the pending reload.
    reload_by_user: bool,
    /// When the last reload (or `Refresh`) started.
    last_reload: Option<Instant>,
    /// When the last load of any kind started.
    last_load_start: Option<Instant>,
    /// `Refresh` reloads started in a row (each waits longer), and when
    /// the last one started.
    user_reloads: u32,
    last_user_reload: Option<Instant>,
    /// When the stream last delivered lines (or a session went live), by
    /// the monotonic and the wall clock: a reconnect after a long gap
    /// reloads.
    last_heard: Option<(Instant, Timestamp)>,
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
    /// Objects whose last action got no answer: the same action on them
    /// is held back for a while, so a retry can't duplicate it.
    doubts: actions::Doubts,
    /// The URL the next connect tries first (a probe found its node sees
    /// more of the cluster); the others follow in order of preference.
    walk_first: Option<usize>,
    /// The next probe of the URLs preferred over the connected one, while
    /// its node doesn't see the whole cluster (ENV-12).
    probe_at: Option<Instant>,
    /// Spaces the probes: `Tuning::probe_initial`, doubling up to
    /// `Tuning::probe_max`, with jitter.
    probe_backoff: Backoff,
    probe_in_flight: bool,
    /// The pending reload follows a switch to a node with another view of
    /// the cluster (ENV-12): it brings every object, so events about
    /// objects the store doesn't hold aren't looked up by name meanwhile.
    view_reload: bool,
    /// Quiet mode is wanted (`Command::SetQuiet`), and the heartbeats a
    /// quiet stream carries; the connect task and switches read it when
    /// they open a stream.
    wish: Arc<connect::StreamWish>,
    /// The latest stream carried no check results (kept across sessions:
    /// a live stream after a quiet one wakes up).
    stream_quiet: bool,
    /// The current stream's reader.
    reader: Option<AbortHandle>,
    /// A switch of the stream between quiet and live in progress.
    switch: Option<quiet::Switch>,
    /// Counts switches (answers of abandoned ones are dropped).
    switches: u64,
    /// A failed switch is tried again then.
    switch_retry_at: Option<Instant>,
    switch_backoff: Backoff,
    /// After a switch: the old stream's lines the new one may repeat.
    dedupe: Option<quiet::Dedupe>,
    /// When the live stream last came back after quiet mode (Icinga's
    /// time); `None` while it never was quiet.
    woke_at: Option<f64>,
    /// Paces every by-name request (`Tuning::request_interval`,
    /// `request_burst`).
    budget: Arc<RequestBudget>,
    /// A background start (until the first load starts or the user came).
    start: Start,
    /// A background start's first load waits until then.
    start_at: Option<Instant>,
    /// The object the user opened, being fetched.
    focus: quiet::Focus,
    /// When the recent prefetches on notification went out.
    prefetched: VecDeque<Instant>,
    /// Periodic reconciles in a row that found nothing the stream missed,
    /// while the stream was continuous: each doubles the interval (twice
    /// at most, never above an hour).
    reconcile_streak: u32,
    /// The load in flight found something no event announced.
    load_found: bool,
    /// Since when the stream has been continuous (the session went live).
    continuous_since: Option<Instant>,
    /// The last quiet status poll's counts (kept across sessions).
    quiet_counts: Option<QuietCounts>,
    /// The last status polls' service counts against the store's: the
    /// references a switch of the stream without proof is checked against.
    count_offsets: VecDeque<quiet::CountOffset>,
    /// The latest of them that a stream line followed.
    count_heard: Option<quiet::CountOffset>,
    /// Such a switch being checked.
    verify: Option<quiet::Verify>,
    /// The reload such a check asked for, to see whether the counts match
    /// again once it is in.
    recheck: Option<quiet::Recheck>,
    /// Icinga's counts include objects the API user doesn't see: switches
    /// aren't checked against them.
    counts_untrusted: bool,
    /// A quiet stream suspected of stalling: when to check, and since when
    /// no line came.
    stall_suspect: Option<(Instant, Instant)>,
    /// What `Snapshot::updating` says, and whether it changed since the
    /// last snapshot.
    updating: Arc<BTreeSet<ObjectKey>>,
    updating_changed: bool,
    /// The stream's mode changed (`Snapshot::quiet`).
    mode_changed: bool,
    /// Every dashboard is evaluated again (after quiet mode).
    dashboards_resume: bool,
    /// The event log's latest entries, newest first, for event stream
    /// views (`recent.rs`).
    recent_events: Arc<Vec<LogEntry>>,
    /// They changed since the last evaluation.
    events_changed: bool,
    /// Counts the reads of the log's newest entries (an older read's
    /// answer is dropped).
    recent_generation: u64,
    /// The heartbeats (`heartbeat.rs`).
    beats: heartbeat::Beats,
    /// The trouble alerts (`trouble.rs`).
    trouble: trouble::Tracker,
    /// The next [`CoreEvent::Alive`].
    alive_at: Instant,
    /// The last tick by both clocks: a wall clock that moved much further
    /// than the monotonic one means the computer slept.
    last_tick: Option<(Instant, Timestamp)>,
}

/// Why the select loop woke up.
enum Wake {
    Shutdown,
    Command(Command),
    Internal(Internal),
    Lines(usize),
    /// The new stream's lines while two streams overlap (a mode switch).
    NewLines(usize),
    Timer,
}

impl Engine {
    #[expect(
        clippy::too_many_lines,
        reason = "one line per part of the engine's state"
    )]
    pub(crate) fn new(
        spec: EnvironmentSpec,
        ports: Ports,
        tuning: Tuning,
        events: futures::channel::mpsc::UnboundedSender<CoreEvent>,
        internal_tx: UnboundedSender<Internal>,
    ) -> Self {
        let mut dashboards = Dashboards::default();
        dashboards.configure(&spec.environment, spec.hide_handled);
        let event_log = EventLog::open(event_log_path(&spec.data_dir, &spec.environment.id));
        let notify = Notify::new(&spec.environment);
        let now = Instant::now();
        let tuning_probe = (tuning.probe_initial, tuning.probe_max);
        let budget = Arc::new(RequestBudget::new(
            tuning.request_interval,
            tuning.request_burst,
        ));
        let start = spec.start;
        Self {
            notify,
            tick_at: now + tuning.rule_tick,
            event_log,
            // Pruned when the engine starts running, then every
            // `prune_interval`.
            prune_at: now,
            notifications_current: false,
            backoff: Backoff::new(tuning.backoff_initial, tuning.backoff_max),
            load_backoff: Backoff::new(tuning.load_retry_initial, sync::LOAD_RETRY_MAX),
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
            active: true,
            health_watch: false,
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
            reload_full: false,
            reload_by_user: false,
            last_reload: None,
            last_load_start: None,
            user_reloads: 0,
            last_user_reload: None,
            last_heard: None,
            reconcile_at: None,
            last_load_end: None,
            restarts: Restarts::default(),
            state: None,
            doubts: actions::Doubts::default(),
            walk_first: None,
            probe_at: None,
            probe_backoff: Backoff::new(tuning_probe.0, tuning_probe.1),
            probe_in_flight: false,
            view_reload: false,
            wish: Arc::new(connect::StreamWish::default()),
            stream_quiet: false,
            reader: None,
            switch: None,
            switches: 0,
            switch_retry_at: None,
            switch_backoff: Backoff::new(quiet::SWITCH_RETRY.0, quiet::SWITCH_RETRY.1),
            dedupe: None,
            woke_at: None,
            budget,
            start,
            start_at: None,
            focus: quiet::Focus::default(),
            prefetched: VecDeque::new(),
            reconcile_streak: 0,
            load_found: false,
            continuous_since: None,
            quiet_counts: None,
            count_offsets: VecDeque::new(),
            count_heard: None,
            verify: None,
            recheck: None,
            counts_untrusted: false,
            stall_suspect: None,
            updating: Arc::default(),
            updating_changed: false,
            mode_changed: false,
            dashboards_resume: false,
            recent_events: Arc::default(),
            events_changed: false,
            recent_generation: 0,
            beats: heartbeat::Beats::default(),
            trouble: trouble::Tracker::default(),
            alive_at: now,
            last_tick: None,
        }
    }

    /// Runs until `shutdown` fires or the handle is gone.
    pub(crate) async fn run(
        mut self,
        mut commands: UnboundedReceiver<Command>,
        mut internal_rx: UnboundedReceiver<Internal>,
        mut shutdown: oneshot::Receiver<()>,
    ) {
        // Pruned before any query can reach the log (a history asked for
        // right after the start is among the commands below).
        self.prune(Instant::now());
        // Event stream views show the log's newest entries.
        self.load_recent_events();
        // Heartbeats seen before, so one that is gone is a finding.
        self.read_remembered_beats();
        // Commands sent right after the start (quiet mode, say) apply
        // before the first connect opens the stream.
        while let Ok(command) = commands.try_recv() {
            self.on_command(command);
        }
        self.connect();
        let mut lines = Vec::new();
        let mut new_lines = Vec::new();
        loop {
            self.reap_tasks();
            let deadline = self.next_deadline(Instant::now());
            let live = self.phase == Phase::Live;
            let max_batch = self.tuning.max_batch.max(1);
            let mut overlap = self.take_overlap();
            let wake = tokio::select! {
                biased;
                _ = &mut shutdown => Wake::Shutdown,
                command = commands.recv() => command.map_or(Wake::Shutdown, Wake::Command),
                Some(message) = internal_rx.recv() => Wake::Internal(message),
                count = receive_lines(self.lines.as_mut().filter(|_| live), &mut lines, max_batch) => Wake::Lines(count),
                count = receive_lines(overlap.as_mut(), &mut new_lines, max_batch) => Wake::NewLines(count),
                () = sleep_until(deadline) => Wake::Timer,
            };
            self.restore_overlap(overlap);
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
                Wake::NewLines(0) => self.new_reader_gone(),
                Wake::NewLines(_) => self.on_new_lines(std::mem::take(&mut new_lines)),
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

    /// When a background start's first load may begin.
    fn first_load_at(&self) -> Option<Instant> {
        (self.phase == Phase::Loading && self.load.is_none())
            .then_some(self.start_at)
            .flatten()
    }

    /// When a failed switch of the stream is tried again: only while live.
    fn switch_retry_due(&self) -> Option<Instant> {
        (self.phase == Phase::Live)
            .then_some(self.switch_retry_at)
            .flatten()
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
            self.probe_due(),
            self.first_load_at(),
            self.switch_retry_due(),
            self.handover_at(),
            self.stall_suspect.map(|(at, _)| at),
            self.beats_due(),
            self.trouble.due(),
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
        if self.first_load_at().is_some_and(|at| at <= now) {
            self.start_at = None;
            self.start = Start::User;
            self.start_load(LoadKind::First);
        }
        if self.handover_at().is_some_and(|at| at <= now) {
            // No line came on both streams: nothing proves the old one
            // delivered everything sent before the new one subscribed.
            self.complete_handover(false);
        }
        if self.switch_retry_due().is_some_and(|at| at <= now) {
            self.switch_retry_at = None;
            self.want_mode();
        }
        if self.stall_suspect.is_some_and(|(at, _)| at <= now) {
            self.check_stall();
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
        if self.probe_due().is_some_and(|at| at <= now) {
            self.start_probe();
        }
        if self.beats_due().is_some_and(|at| at <= now) {
            self.run_beats(now);
        }
        if self.trouble.due().is_some_and(|at| at <= now) {
            self.assess_trouble(now);
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
        let timeouts = (self.tuning.action_timeout, self.tuning.identify_timeout);
        let first = self.walk_start();
        let wish = Arc::clone(&self.wish);
        self.tasks.spawn(async move {
            let connected = connect::connect(&environment, secrets, timeouts, first, wish).await;
            let message = match connected {
                Ok(connected) => Internal::Connected {
                    session,
                    connected: Box::new(connected),
                },
                Err(failure) => Internal::ConnectFailed { session, failure },
            };
            let _ = tx.send(message);
        });
    }

    /// Where the next walk over the URLs starts: one a probe found better,
    /// else the URL of the node the objects come from when that node sees
    /// the whole cluster and isn't the first choice (the master that took
    /// over after a failover), so a reconnect doesn't wait for a preferred
    /// master that hangs, nor ask it again and again; the others follow in
    /// order of preference (ENV-12).
    fn walk_start(&mut self) -> Option<usize> {
        self.walk_first.take().or_else(|| {
            let node = self.store.node()?;
            let index = node.url_index;
            let listed = self
                .spec
                .environment
                .urls
                .get(index)
                .is_some_and(|url| url.url.trim() == node.url.trim());
            (index > 0 && listed && node.view.is_full()).then_some(index)
        })
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
        // A pending reload stays: the next session serves it.
        self.reconcile_at = None;
        self.store.end_annotation_query();
        // The stream gap may have missed `Notification` events.
        self.notifications_current = false;
        // Probes belong to the session.
        self.probe_at = None;
        self.probe_in_flight = false;
        // So do the stream, its switches and the opened object's request.
        self.reader = None;
        self.switch = None;
        self.switch_retry_at = None;
        self.dedupe = None;
        self.focus = quiet::Focus::default();
        self.start_at = None;
        self.stall_suspect = None;
        self.verify = None;
        self.continuous_since = None;
        self.note_updating();
        // What an aborted load found is real all the same.
        self.finish_discovered();
    }

    /// The session failed: back off and retry, or wait for the user.
    fn fail(&mut self, failure: Failure) {
        self.fail_with(failure, std::time::Duration::ZERO);
    }

    /// The session failed: back off (at least `at_least`) and retry, or
    /// wait for the user.
    fn fail_with(&mut self, failure: Failure, at_least: std::time::Duration) {
        let healthy = self
            .conn
            .as_ref()
            .and_then(|conn| conn.live_since)
            .is_some_and(|(since, _)| since.elapsed() >= self.tuning.healthy_after);
        // No live data from here (A): blind after a while.
        self.go_dark(&failure);
        self.teardown();
        self.publish_changes();
        let state = match failure {
            Failure::Transient(error) => self.retry_later(error, None, healthy, at_least),
            Failure::TransientUntrusted { error, untrusted } => {
                self.retry_later(error, Some(*untrusted), healthy, at_least)
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
                url,
                message,
                certificate,
                ..
            } => {
                tracing::warn!(%message, %url, "the server certificate isn't trusted");
                self.phase = Phase::Idle { retry_at: None };
                ConnectionState::TlsFailed {
                    url,
                    message,
                    certificate: certificate.map(|certificate| *certificate),
                }
            }
        };
        self.set_state(state);
    }

    /// Backs off (at least `at_least`, after a reset if the session was
    /// `healthy`) and retries then; the state to report.
    fn retry_later(
        &mut self,
        error: String,
        untrusted: Option<crate::command::UntrustedUrl>,
        healthy: bool,
        at_least: std::time::Duration,
    ) -> ConnectionState {
        if healthy {
            self.backoff.reset();
        }
        let delay = self.backoff.fail().max(at_least);
        tracing::info!(%error, ?delay, "connection lost; retrying");
        self.phase = Phase::Idle {
            retry_at: Some(Instant::now() + delay),
        };
        ConnectionState::Reconnecting {
            error,
            attempt: self.backoff.attempt(),
            retry_at: self.ports.clock.now().plus(delay),
            untrusted,
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the steps of a new connection in their order"
    )]
    fn on_connected(&mut self, connected: Connected) {
        let Connected {
            client,
            info,
            lines,
            kinds,
            filter,
            quiet,
            node,
            login,
        } = connected;
        tracing::info!(
            user = %info.user,
            version = %info.version,
            node = %node.name,
            view = %node.view.label(),
            quiet,
            "connected to Icinga"
        );
        // Every by-name request of the session takes from the budget.
        let client = client.with_budget(Arc::clone(&self.budget));
        let node = Arc::new(node);
        // Objects loaded from a node with another view (a satellite's
        // part, or the whole cluster before a failover to a satellite)
        // aren't this node's: reloaded at once (jittered).
        let other_view = self.loaded
            && self
                .store
                .node()
                .is_some_and(|loaded| !loaded.view.same_data(&node.view));
        // What a satellite's answers leave out may be outside its zone: the
        // store hides it (keeps its last view) rather than forgetting it.
        self.store
            .set_hiding(matches!(node.view, topology::ClusterView::Partial { .. }));
        self.view_reload = other_view;
        self.emit(CoreEvent::Permissions(info.clone()));
        if let Some(stream) = lines {
            let (tx, rx) = mpsc::unbounded_channel();
            self.reader = Some(
                self.tasks
                    .spawn(stream::read(stream, Arc::clone(&self.seq), tx)),
            );
            self.lines = Some(rx);
        }
        let next_status = info
            .allows("status/query")
            .then(|| Instant::now() + self.tuning.status_interval);
        let notifications_allowed = info.allows("objects/query/Notification");
        let notification_events = self.lines.is_some()
            && info.allows(&format!("events/{}", EventKind::Notification.api_name()));
        // A filtered stream carries only the heartbeats' results: its
        // silence says nothing about the other checks.
        let check_events = kinds.contains(&EventKind::CheckResult) && filter.is_none();
        // The gap is judged by the stream that broke (a quiet one may be
        // silent for minutes), whatever mode the new one has.
        let was_quiet = self.stream_quiet;
        let woke = self.loaded && was_quiet && !quiet;
        if self.stream_quiet != quiet {
            self.stream_quiet = quiet;
            self.mode_changed = true;
        }
        // Without `objects/query/<type>` a kind isn't asked for by name at
        // all (events about its objects can't be looked up).
        self.fetch.refuse(
            !info.allows("objects/query/Host"),
            !info.allows("objects/query/Service"),
        );
        let can_poll_status = next_status.is_some();
        let next_nodes = info.allows("objects/query/Endpoint").then(Instant::now);
        self.conn = Some(Conn {
            client,
            info,
            node,
            login,
            since: Instant::now(),
            live_since: None,
            next_status,
            status_in_flight: false,
            status_sent: Instant::now(),
            next_nodes,
            nodes_asked: None,
            notifications_allowed,
            notification_events,
            notifications_in_flight: false,
            notification_events_waiting: Vec::new(),
            check_events,
            kinds,
            filter,
            quiet,
            last_line: Instant::now(),
            health: health::Asked::default(),
        });
        // The listener status and features were another session's node's
        // (the first status poll sets the interval).
        self.store
            .update_health(crate::health::ClusterHealth::forget_node);
        // The stream is continuous from here (lines wait during a load).
        self.continuous_since = Some(Instant::now());
        if self.loaded {
            // A reconnect: live at once on the objects we have (the stream
            // keeps them current).
            let gap = self.stream_gap();
            self.go_live();
            if other_view {
                tracing::info!("the node sees another part of the cluster; reloading");
                self.reload_now = false;
                self.request_reload(sync::ReloadCause::Reconnect);
            } else {
                self.adopt_node(true);
                self.after_reconnect(gap, was_quiet, can_poll_status);
            }
            if woke {
                // Live again after quiet mode, through a reconnect (once
                // it is known whether a full reload follows).
                self.woke();
            }
        } else {
            // The first load's objects are this node's from the start, so
            // a partial view is labelled while it fills in (with its first
            // tier's snapshot).
            self.adopt_node(false);
            self.reload_now = false;
            self.phase = Phase::Loading;
            self.begin_first_load();
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
        let now = Instant::now();
        // The load serves a pending reload, if any.
        let served = self.reload_at.take().is_some();
        self.reconcile_at = None;
        self.last_load_start = Some(now);
        if served || matches!(kind, LoadKind::Reload | LoadKind::Refresh) {
            self.last_reload = Some(now);
        }
        if std::mem::take(&mut self.reload_by_user) {
            self.user_reloads = self.user_reloads.saturating_add(1);
            self.last_user_reload = Some(now);
        }
        if kind == LoadKind::First {
            // Whatever the start was, it is served.
            self.start = Start::User;
            self.start_at = None;
        }
        self.load_found = false;
        self.reload_full = false;
        self.view_reload = false;
        // A fuller view than the one the store was filled from brings
        // objects never seen: the rule engine learns them when it's over.
        self.store.track_appeared(
            kind != LoadKind::First && self.store.node().is_some_and(|node| !node.view.is_full()),
        );
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
                    // The load reloads anyway; a restart is announced.
                    if self.restarts.observe(status, Instant::now()).is_some() {
                        self.icinga_restarted(status);
                    }
                    // The trend's first point (topic 06): the status the
                    // load brought, so the health page has numbers before
                    // the first poll.
                    let sample = crate::health::HealthSample::of(
                        status,
                        None,
                        self.watchdog.late().len(),
                        self.ports.clock.now(),
                    );
                    let interval = if self.quiet() {
                        self.tuning.quiet_status_interval
                    } else {
                        self.tuning.status_interval
                    };
                    self.store.update_health(|health| {
                        health.push(sample);
                        health.interval = interval;
                    });
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
                // The heartbeats are left out of everything from here.
                self.discover_beats();
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
            LoadStep::Done => self.load_done(kind),
            LoadStep::Failed(failure) => {
                self.load = None;
                self.last_load_end = Some(Instant::now());
                self.store.end_annotation_query();
                self.finish_discovered();
                match failure {
                    Failure::Auth(_) => self.fail(failure),
                    Failure::Transient(_) if first => {
                        // The first load's answers are the big ones: a
                        // failing one is retried after a growing wait (up
                        // to the reconcile interval), not with every quick
                        // reconnect.
                        let wait = self.load_backoff.fail_up_to(self.load_retry_cap());
                        self.fail_with(failure, wait);
                    }
                    // An answer that can't be used needs the user.
                    failure if first => self.fail(failure),
                    Failure::Transient(error) | Failure::Misconfigured(error) => {
                        // The stream decides whether the connection is
                        // gone; a failed reload waits a whole interval from
                        // now, so a struggling master (or a proxy answering
                        // 502) isn't asked again right away.
                        tracing::warn!(%error, "reload failed; the next one follows at the usual interval");
                        self.schedule_reconcile();
                    }
                    other => {
                        tracing::warn!(
                            ?other,
                            "reload failed; the next one follows at the usual interval"
                        );
                        self.schedule_reconcile();
                    }
                }
            }
        }
    }

    /// A load is complete: the objects are the node's, the rule engine
    /// judges what it found (and, after the first, learns what is wrong).
    fn load_done(&mut self, kind: LoadKind) {
        let first = kind == LoadKind::First;
        let now = Instant::now();
        self.load = None;
        self.loaded = true;
        // The objects are this node's now.
        self.adopt_node(true);
        // Heartbeats found, gone or back (a complete load says).
        self.discover_beats();
        self.track_continuity(kind);
        self.last_load_end = Some(now);
        self.fetch
            .release_deferred(now, |key| self.store.contains(key));
        self.watchdog.loaded(&self.store);
        self.schedule_reconcile();
        if self.store.hidden_count() > 0
            && self
                .conn
                .as_ref()
                .is_some_and(|conn| conn.node.view.is_full())
        {
            // The whole cluster is in: what it didn't bring back of
            // what a satellite left out is gone.
            self.store.release_hidden();
            self.record_discovered(true);
        }
        self.finish_discovered();
        if first {
            self.load_backoff.reset();
            // What is already wrong doesn't notify, but the rule
            // engine must know it (before any event about it); what
            // changed after the stream subscribed does.
            let waiting = self.seed_first_load();
            self.go_live();
            if !waiting.is_empty() {
                self.on_lines(waiting);
            }
            if self.conn.is_none() {
                // The stream had ended meanwhile.
                return;
            }
        }
        if !self.notifications_current {
            self.load_notifications();
        }
        self.publish_changes();
    }

    /// Seeds the rule engine with the session's first load and returns the
    /// stream lines that waited meanwhile (applied once live). A state
    /// change among them that the load's answers already reflect (the
    /// answer was sent after the line was read, or composed after the
    /// change although its line came later: [`Store::shows_change`]) began
    /// after the stream subscribed: it is judged like any change, not
    /// seeded, so a problem that began during the load, or during a
    /// background start's delay, notifies. (One the answers don't show yet
    /// is applied as a change on top of them.)
    fn seed_first_load(&mut self) -> Vec<ReaderMsg> {
        let mut waiting = Vec::new();
        if let Some(receiver) = &mut self.lines {
            while let Ok(message) = receiver.try_recv() {
                waiting.push(message);
            }
        }
        let mut began = HashSet::new();
        for message in &waiting {
            if let ReaderMsg::Line(seq, line) = message
                && line
                    .windows(STATE_CHANGE.len())
                    .any(|bytes| bytes == STATE_CHANGE)
                && let Some(Event::StateChange {
                    object, result, at, ..
                }) = ic_api::parse_event(line)
            {
                if self.store.reflects(&object, *seq) {
                    began.insert(object);
                } else if self
                    .store
                    .shows_change(&object, result.execution_end.non_zero().unwrap_or(at))
                {
                    // Applied on top of the answer, it would take the
                    // object back (a hard state to soft, say).
                    self.store.note_reflected(&object, *seq);
                    began.insert(object);
                }
            }
        }
        if !began.is_empty() {
            tracing::info!(
                count = began.len(),
                "states that changed while the first load ran are judged, not seeded"
            );
        }
        let at = self.evaluation_time();
        let mut log = Vec::new();
        self.notify.seed(&self.store, &began, at, &mut log);
        self.record_log(log);
        waiting
    }

    /// The first load is in: connected and live.
    fn go_live(&mut self) {
        let now = self.ports.clock.now();
        let Some(conn) = &mut self.conn else {
            return;
        };
        let node = ConnectedNode::clone(&conn.node);
        conn.live_since = Some((Instant::now(), now));
        // Lines waited during the first load: the stall watch starts now.
        conn.last_line = Instant::now();
        self.last_heard = Some((Instant::now(), now));
        let version = conn.info.version.clone();
        self.phase = Phase::Live;
        self.set_state(ConnectionState::Connected {
            node,
            version,
            since: now,
        });
        self.schedule_probe();
        // Live data again; the heartbeats get their grace (Icinga may
        // have rescheduled their checks meanwhile).
        self.go_bright();
        self.beats_grace("connected");
        // The stream follows the wanted mode.
        self.want_mode();
    }

    /// The store's objects come from the connected node (`Snapshot::node`).
    /// `announce`: a snapshot goes out for it alone.
    fn adopt_node(&mut self, announce: bool) {
        if let Some(conn) = &self.conn {
            self.store.set_node(Arc::clone(&conn.node), announce);
        }
    }

    // --- probes of preferred URLs (ENV-12) ------------------------------------------

    /// The URLs worth probing while connected: none while the node sees
    /// the whole cluster.
    fn probe_candidates(&self) -> Vec<usize> {
        let Some(conn) = &self.conn else {
            return Vec::new();
        };
        topology::candidates(
            &conn.node.view,
            conn.node.url_index,
            self.spec.environment.urls.len(),
        )
    }

    /// Schedules the next probe, if the connected node's view is short of
    /// full; a full view starts the probes' spacing over.
    fn schedule_probe(&mut self) {
        if self.probe_candidates().is_empty() {
            self.probe_backoff.reset();
            self.probe_at = None;
        } else {
            self.probe_at = Some(Instant::now() + self.probe_backoff.fail());
        }
    }

    /// When the next probe is due: only while live and none runs.
    fn probe_due(&self) -> Option<Instant> {
        if self.phase != Phase::Live || self.probe_in_flight {
            return None;
        }
        self.probe_at
    }

    /// Asks the preferred URLs (`Tuning::probe_initial` after going live,
    /// then less and less often) whether one answers with a fuller view:
    /// one URL after the other, a login, the node's name and the zones
    /// each (the event stream stays where it is until one does).
    fn start_probe(&mut self) {
        let candidates = self.probe_candidates();
        let Some(conn) = &self.conn else {
            return;
        };
        if candidates.is_empty() {
            self.probe_at = None;
            return;
        }
        self.probe_at = None;
        self.probe_in_flight = true;
        let login = conn.login.clone();
        let urls = self.spec.environment.urls.clone();
        let current = conn.node.view.clone();
        let current_index = conn.node.url_index;
        let action_timeout = self.tuning.action_timeout;
        let quick = Some(self.tuning.identify_timeout);
        let tx = self.internal_tx.clone();
        let session = self.session;
        self.tasks.spawn(async move {
            let mut better = None;
            for index in candidates {
                match connect::reach(&login, &urls, index, action_timeout, quick).await {
                    Ok(reached)
                        if topology::better(&reached.node.view, index, &current, current_index) =>
                    {
                        tracing::info!(
                            node = %reached.node.name,
                            view = %reached.node.view.label(),
                            "a preferred URL answers with a fuller view; switching"
                        );
                        better = Some(index);
                        break;
                    }
                    Ok(reached) => tracing::debug!(
                        node = %reached.node.name,
                        view = %reached.node.view.label(),
                        "probe: no fuller view"
                    ),
                    Err(failure) => {
                        tracing::debug!(reason = %failure.reason(), "probe: URL not usable");
                    }
                }
            }
            let _ = tx.send(Internal::Probed { session, better });
        });
    }

    /// A probe is back: switch to the better node, or probe again later.
    fn on_probed(&mut self, better: Option<usize>) {
        self.probe_in_flight = false;
        match better {
            Some(index) if self.phase == Phase::Live => {
                // Not a failure: a fresh start on the better node (the
                // walk continues with the others if it fails meanwhile).
                self.walk_first = Some(index);
                self.backoff.reset();
                self.connect();
            }
            _ => self.schedule_probe(),
        }
    }

    // --- status poll ------------------------------------------------------------------

    fn poll_status(&mut self) {
        let now = Instant::now();
        let names = self.node_names();
        let quiet = if self.active {
            self.tuning.status_interval
        } else {
            NODES_QUIET_INTERVAL
        };
        let Some(conn) = &mut self.conn else {
            return;
        };
        conn.status_in_flight = true;
        conn.status_sent = now;
        // The cluster nodes' states with it, when due: one more small
        // request by name (the masters and satellites, never the agents).
        let nodes = match conn.next_nodes {
            Some(at) if at <= now && !names.is_empty() => {
                conn.next_nodes = Some(now + quiet);
                conn.nodes_asked = Some(now);
                Some(names)
            }
            _ => None,
        };
        let client = conn.client.clone();
        let tx = self.internal_tx.clone();
        let session = self.session;
        let health = self.health_ask(now);
        self.tasks.spawn(async move {
            let result = client.status().await;
            let nodes = match nodes {
                Some(names) => {
                    let states = client.endpoint_states(&names).await;
                    Some((names, states))
                }
                None => None,
            };
            let health = match health {
                Some(ask) => Some(Box::new(health::fetch(&client, ask).await)),
                None => None,
            };
            let _ = tx.send(Internal::Status {
                session,
                result,
                nodes,
                health,
            });
        });
    }

    /// The environment came on screen (switched to, or the window shown
    /// again; not while quiet): the cluster nodes' states, asked for every
    /// 5 minutes off screen, come with a status poll at once unless they
    /// are younger than the on-screen interval, so the switcher's nodes are
    /// current (ENV-01, ENV-06). At most one such poll per interval; none
    /// while a load runs (it brings them) or without other nodes to ask
    /// about.
    pub(super) fn nodes_on_screen(&mut self) {
        if self.phase != Phase::Live || self.node_names().is_empty() {
            return;
        }
        let now = Instant::now();
        let interval = self.tuning.status_interval;
        let Some(conn) = &mut self.conn else {
            return;
        };
        let Some(next) = &mut conn.next_nodes else {
            return;
        };
        // The load that made the session live brought them too.
        let asked = conn
            .nodes_asked
            .or_else(|| conn.live_since.map(|(at, _)| at));
        match asked {
            Some(asked) if now.saturating_duration_since(asked) < interval => {
                *next = (*next).min(asked + interval);
            }
            _ => {
                *next = now;
                if !conn.status_in_flight
                    && let Some(status) = &mut conn.next_status
                {
                    *status = now;
                }
            }
        }
    }

    /// The cluster nodes whose states a status poll asks for: the masters
    /// and satellites but the connected node itself.
    fn node_names(&self) -> Vec<String> {
        let Some(conn) = &self.conn else {
            return Vec::new();
        };
        let (endpoints, zones) = self.store.cluster();
        topology::cluster_nodes(zones, endpoints, Some(&conn.node))
            .into_iter()
            .filter(|node| {
                node.name != conn.node.name && node.state != topology::NodeState::Unknown
            })
            .map(|node| node.name)
            .collect()
    }

    /// The cluster nodes' states (of the nodes `asked` for) came with a
    /// status poll.
    fn on_node_states(
        &mut self,
        asked: &[String],
        nodes: Result<Vec<ic_api::EndpointState>, ApiError>,
    ) {
        let Some(conn) = &mut self.conn else {
            return;
        };
        match nodes {
            Ok(states) => {
                let local = conn.node.name.clone();
                self.store.set_endpoint_states(&states, &local);
                let now = self.ports.clock.now();
                self.store
                    .update_health(|health| health.endpoints_at = Some(now));
                if asked
                    .iter()
                    .any(|name| !states.iter().any(|state| state.name == *name))
                {
                    // A node of the list is gone (Icinga then fails the
                    // whole request: none came back): the list is reloaded,
                    // and the next poll asks for the nodes it has.
                    tracing::debug!("a cluster node is gone; reloading the endpoints");
                    self.fetch.mark_lists(
                        Lists {
                            endpoints: true,
                            ..Lists::default()
                        },
                        Instant::now(),
                    );
                }
            }
            Err(ApiError::Forbidden(message)) => {
                tracing::warn!(%message, "the API user may not read the endpoints; not asking again");
                conn.next_nodes = None;
            }
            Err(error) => tracing::debug!(%error, "the cluster nodes' states couldn't be read"),
        }
    }

    fn on_status(
        &mut self,
        result: Result<InstanceStatus, ApiError>,
        listener: Option<&ic_model::ListenerStatus>,
    ) {
        let interval = if self.quiet() {
            self.tuning.quiet_status_interval
        } else {
            self.tuning.status_interval
        };
        let Some(conn) = &mut self.conn else {
            return;
        };
        conn.status_in_flight = false;
        conn.next_status = Some(Instant::now() + interval);
        let connected = conn.since;
        let sent = conn.status_sent;
        match result {
            Ok(status) => {
                if let Some(error) = self.stalled(&status) {
                    tracing::warn!(%error, "reconnecting");
                    self.fail(Failure::Transient(error));
                    return;
                }
                self.watch_quiet_stream(&status);
                self.check_counts(&status, sent);
                // Only the same node with another start time restarted: the
                // masters of an HA zone behind a load balancer each have
                // their own.
                let restarted = self.restarts.observe(&status, Instant::now());
                if restarted.is_some() {
                    self.icinga_restarted(&status);
                }
                // The trend of the cluster health page (topic 06): every
                // poll, in memory only.
                let sample = crate::health::HealthSample::of(
                    &status,
                    listener,
                    self.watchdog.late().len(),
                    self.ports.clock.now(),
                );
                self.store.update_health(|health| {
                    health.push(sample);
                    health.interval = interval;
                });
                self.store.set_status(status);
                if let Some(seen) = restarted {
                    // Its features may have changed with the restart.
                    if let Some(conn) = &mut self.conn {
                        conn.health.features_at = None;
                    }
                    if sync::reload_covers_restart(self.last_load_start, connected, seen) {
                        // A restart of the node behind the stream ended the
                        // stream, and a load since the reconnect brought
                        // everything (the other master of an HA zone
                        // restarted with it during a deploy).
                        tracing::debug!("a node restarted; a load since brought everything");
                    } else {
                        tracing::info!("Icinga restarted; reloading");
                        self.request_reload(sync::ReloadCause::Restart);
                    }
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
        // Answers about heartbeats (or services that may have become one)
        // find them again.
        let beats_touched = answers.objects.iter().any(|answer| match &answer.result {
            Ok(fetched) => {
                fetched
                    .services
                    .iter()
                    .any(|service| self.beat_candidate(service))
                    || fetched.missing.iter().any(|key| {
                        key.as_service()
                            .is_some_and(|service| self.beats.watches(service))
                    })
            }
            Err(_) => false,
        });
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
        match answers.endpoints {
            Some(Ok(cluster)) => self.store.set_cluster(cluster.endpoints, cluster.zones),
            Some(Err(error)) => tracing::warn!(%error, "couldn't reload the endpoints"),
            None => {}
        }
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
        if beats_touched {
            self.discover_beats();
        }
        self.note_updating();
        if answers.urgent {
            self.publish_changes();
        }
    }

    // --- the event stream ---------------------------------------------------------------

    /// Applies a batch from the reader: parses, collapses check results,
    /// applies in order; then handles the end of the stream if it came.
    /// While a mode switch reads two streams, the old stream's lines are
    /// remembered, and its end hands over to the new one (`quiet.rs`).
    fn on_lines(&mut self, batch: Vec<ReaderMsg>) {
        let mut lines = Vec::with_capacity(batch.len());
        let mut end = None;
        for message in batch {
            match message {
                ReaderMsg::Line(seq, line) => lines.push((seq, line)),
                ReaderMsg::End(error) => end = Some(error),
            }
        }
        let common = self.note_old_lines(&lines);
        self.apply_lines(lines);
        if let Some(error) = end {
            if self.overlapping() {
                tracing::info!(
                    ?error,
                    "the old event stream ended during a switch; the new one takes over"
                );
                self.complete_handover(false);
                return;
            }
            let error = error.map_or_else(
                || "the event stream was closed by Icinga".to_owned(),
                |error| format!("the event stream broke: {error}"),
            );
            self.fail(Failure::Transient(error));
        } else if common {
            self.complete_handover(true);
        }
    }

    /// Parses, collapses and applies stream lines (those an earlier stream
    /// already brought are dropped after a switch).
    fn apply_lines(&mut self, mut lines: Vec<(u64, Vec<u8>)>) {
        self.drop_duplicates(&mut lines);
        if lines.is_empty() {
            return;
        }
        // Only lines count: the end of the stream (after a laptop woke
        // up, say) says nothing about what it missed.
        self.last_heard = Some((Instant::now(), self.ports.clock.now()));
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
                _ if self.older_check(&event) => {
                    // From the overlap of a switch: a newer state is in.
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
            // A heartbeat's result (the stream's time counts for its
            // budget).
            let beat = match &event {
                Event::CheckResult { object, result, .. }
                | Event::StateChange { object, result, .. } => object
                    .as_service()
                    .filter(|key| self.beats.watches(key))
                    .map(|key| (key.clone(), result.clone())),
                _ => None,
            };
            let applied = self.store.apply(seq, &event);
            if let Some((key, result)) = beat {
                self.beat_result(&key, &result, true);
            }
            match applied {
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
                    } else if self.view_reload && self.reload_at.is_some() {
                        // The pending reload brings it, and answers sent
                        // after this event.
                    } else {
                        self.fetch.mark_unknown(key, now);
                    }
                }
            }
        }
        self.record_log(log);
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
        if load {
            // The stream missed something: reconciles stay frequent, and
            // the status polls' offsets taken meanwhile are no references
            // (see `quiet.rs`).
            self.load_found = true;
            self.count_offsets.clear();
            self.count_heard = None;
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
        self.record_log(log);
    }

    /// A load is over (or cut off): what it found is judged now, and the
    /// rule engine learns the objects a fuller view brought.
    fn finish_discovered(&mut self) {
        let mut log = Vec::new();
        self.notify.load_finished(&self.store, &mut log);
        self.record_log(log);
        let appeared = self.store.take_appeared();
        self.store.track_appeared(false);
        if !appeared.is_empty() {
            tracing::debug!(
                count = appeared.len(),
                "a fuller view of the cluster brought objects never seen"
            );
            let at = self.evaluation_time();
            self.notify.seed_objects(&self.store, &appeared, at);
        }
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
            Command::PreviewDashboard { views, reply } => self.preview(views, reply),
            Command::SetHandledDefaults(defaults) => self.set_handled_defaults(defaults),
            Command::PauseNotifications(until) => {
                let paused = self.notify.pause(until);
                tracing::info!(?paused, "notifications paused");
                self.emit(CoreEvent::NotificationsPaused(paused));
            }
            Command::MarkNotificationsRead => self.event_log.mark_read(),
            Command::MarkNotificationRead(id) => self.event_log.mark_one_read(id),
            Command::LoadHistoryStart { reply } => self.event_log.history_start(reply),
            Command::Hydrate(keys) => self.hydrate(keys),
            Command::SetActive(active) => self.set_active(active),
            Command::SetQuiet(quiet) => self.set_quiet(quiet),
            Command::Focus(key) => self.focus(key),
            Command::StartNow => self.start_now(),
            Command::WatchHealth(watch) => self.watch_health(watch),
            Command::ConfirmHeartbeatRemoval(key) => self.confirm_beat_removal(key),
        }
    }

    /// `Command::SetActive`: an environment on screen gets its snapshots
    /// at the normal pace, and what changed (and, unless quiet, its cluster
    /// nodes' states: [`Engine::nodes_on_screen`]) at once when it comes
    /// back.
    fn set_active(&mut self, active: bool) {
        if self.active == active {
            return;
        }
        tracing::debug!(environment = %self.spec.environment.name, active, "on screen");
        self.active = active;
        if active {
            if !self.quiet() {
                self.nodes_on_screen();
            }
            self.publish_changes();
        }
    }

    fn refresh(&mut self) {
        match self.phase {
            Phase::Idle { retry_at } => {
                if retry_at.is_none() {
                    // After a failure that needed the user: a fresh start.
                    self.backoff.reset();
                    self.load_backoff.reset();
                }
                self.reload_now = true;
                self.connect();
            }
            Phase::Loading if self.start_at.is_some() => {
                // A background start waiting: the user wants it now.
                self.start_now();
            }
            Phase::Connecting | Phase::Loading => {
                tracing::debug!("refresh: a connection attempt or load is already running");
            }
            Phase::Live => {
                // Connected to a node short of the full view: the preferred
                // URLs are asked at once too (ENV-12).
                if !self.probe_in_flight && !self.probe_candidates().is_empty() {
                    self.probe_at = Some(Instant::now());
                }
                self.request_reload(sync::ReloadCause::User);
            }
        }
    }

    /// `Command::SetHandledDefaults`: the views that follow the settings
    /// hide other handled problems now.
    fn set_handled_defaults(&mut self, defaults: ic_config::HideHandled) {
        if self.spec.hide_handled != defaults {
            self.spec.hide_handled = defaults;
            self.dashboards_configured = true;
            self.publish_changes();
        }
    }

    fn update_environment(&mut self, environment: ic_config::Environment) {
        let old = &self.spec.environment;
        let reconnect = old.id != environment.id || old.connection_differs(&environment);
        // Another cluster: no URL in common (adding the other master of an
        // HA zone, reordering or fixing a pin keeps the objects).
        let other_server = old.id != environment.id || !shares_url(old, &environment);
        let other_log = old.id != environment.id;
        let dashboards_changed = old.groups != environment.groups;
        let beats_changed = old.trouble.heartbeats != environment.trouble.heartbeats;
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
            self.reset_recent_events();
            self.read_remembered_beats();
        }
        if other_server {
            // Nothing the rule engine remembers is about this server.
            self.notify.reset(&self.spec.environment);
            self.notifications_current = false;
            self.store.clear();
            self.watchdog.clear();
            self.restarts = Restarts::default();
            self.loaded = false;
            self.reset_beats();
            self.reset_trouble();
            self.publish();
        } else {
            self.notify.set_rules(&self.spec.environment);
            if beats_changed {
                self.discover_beats();
            }
            if dashboards_changed || beats_changed {
                self.publish_changes();
            }
        }
        let waiting_for_user = matches!(self.phase, Phase::Idle { .. });
        if reconnect || waiting_for_user {
            self.backoff.reset();
            self.load_backoff.reset();
            self.reload_now = true;
            self.connect();
        }
    }

    // --- internal messages --------------------------------------------------------------

    fn on_internal(&mut self, message: Internal) {
        match message {
            Internal::ActionDone(finished) => self.on_action_done(*finished),
            Internal::Connected { session, connected } if session == self.session => {
                self.on_connected(*connected);
            }
            Internal::ConnectFailed { session, failure } if session == self.session => {
                self.fail(failure);
            }
            Internal::Probed { session, better } if session == self.session => {
                self.on_probed(better);
            }
            Internal::Load {
                session,
                load,
                step,
            } if session == self.session => self.on_load(load, step),
            Internal::Status {
                session,
                result,
                nodes,
                health,
            } if session == self.session => {
                if let Some((asked, nodes)) = nodes {
                    self.on_node_states(&asked, nodes);
                }
                let listener = health.and_then(|answers| self.on_health(*answers));
                self.on_status(result, listener.as_ref());
            }
            Internal::Health { session, answers } if session == self.session => {
                if let Some(conn) = &mut self.conn {
                    conn.health.in_flight = false;
                }
                self.on_health(*answers);
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
            Internal::RecentEvents {
                generation,
                entries,
            } => self.on_recent_events(generation, entries),
            Internal::Logged(intents) => self.on_logged(intents),
            Internal::NotifiedBefore { asked, known } => self.on_notified_before(&asked, &known),
            Internal::IcingaNotifications {
                session,
                started,
                result,
            } if session == self.session => self.on_icinga_notifications(started, result),
            Internal::StreamOpened {
                session,
                switch,
                result,
            } if session == self.session => self.on_stream_opened(switch, result),
            Internal::Focused {
                session,
                key,
                started,
                result,
            } if session == self.session => self.on_focused(&key, started, result),
            Internal::StartSize { session, services } if session == self.session => {
                self.start_after(services);
            }
            Internal::RememberedBeats { generation, beats } => {
                self.on_remembered_beats(generation, beats);
            }
            Internal::Beats {
                session,
                started,
                late,
                result,
            } if session == self.session => self.on_beats(started, &late, result),
            // An older session's late answer.
            Internal::Connected { .. }
            | Internal::ConnectFailed { .. }
            | Internal::Probed { .. }
            | Internal::Load { .. }
            | Internal::Status { .. }
            | Internal::Health { .. }
            | Internal::Fetched { .. }
            | Internal::IcingaNotifications { .. }
            | Internal::StreamOpened { .. }
            | Internal::Focused { .. }
            | Internal::StartSize { .. }
            | Internal::Beats { .. } => {}
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

/// How often a status poll of an engine off screen asks for the cluster
/// nodes' states (on screen: every poll).
const NODES_QUIET_INTERVAL: std::time::Duration = std::time::Duration::from_mins(5);

/// How long stopping waits at most for the event log to write what it was
/// given.
const LOG_FLUSH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Whether two versions of an environment list a URL in common (compared
/// as parsed, so `https://Master-01:5665/` is `https://master-01:5665`).
fn shares_url(old: &ic_config::Environment, new: &ic_config::Environment) -> bool {
    let parsed = |environment: &ic_config::Environment| -> Vec<String> {
        environment
            .urls
            .iter()
            .map(|url| {
                url.api_url()
                    .map_or_else(|_| url.url.trim().to_owned(), |parsed| parsed.to_string())
            })
            .collect()
    };
    let old = parsed(old);
    parsed(new).iter().any(|url| old.contains(url))
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
