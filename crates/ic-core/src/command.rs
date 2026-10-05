//! The boundary between the UI and the engine: [`Command`]s in,
//! [`CoreEvent`]s out.

use std::sync::Arc;

use futures::channel::oneshot;
use ic_api::{ApiInfo, CertificateInfo};
use ic_model::{Action, ActionTarget, CheckableState, ObjectKey, StateType, Timestamp};
use ic_rules::NotificationIntent;

use crate::snapshot::{DashboardResult, Snapshot};

/// What the UI asks the engine to do. Sent with
/// [`CoreHandle::send`](crate::CoreHandle::send), which never blocks.
#[derive(Debug)]
pub enum Command {
    /// Runs an action. The engine answers with [`CoreEvent::ActionFinished`]
    /// carrying the same `id`, so the UI can key optimistic state by it.
    Action {
        /// Chosen by the UI; echoed in [`CoreEvent::ActionFinished`].
        id: u64,
        /// The objects, downtime or comment it applies to.
        target: ActionTarget,
        /// What to do.
        action: Action,
    },
    /// A full re-sync now: a lean reload while connected; otherwise a
    /// connection attempt right away (also after `AuthFailed`, `TlsFailed`,
    /// `MissingSecret` and `Misconfigured`, which never retry by
    /// themselves).
    Refresh,
    /// The environment's settings changed. Changed connection settings
    /// (URL, authentication, TLS) reconnect; anything else (dashboards,
    /// rules, author) applies in place.
    UpdateEnvironment(ic_config::Environment),
    /// The app-wide settings changed (reconcile interval, log retention).
    UpdateGeneral(ic_config::General),
    /// Pauses notifications until the time given (`None` resumes them).
    PauseNotifications(Option<Timestamp>),
    /// The local event log, newest first: for one object, or for every
    /// object.
    LoadHistory {
        /// The object, or `None` for every object.
        object: Option<ObjectKey>,
        /// At most this many entries.
        limit: usize,
        /// Receives the entries.
        reply: oneshot::Sender<Vec<LogEntry>>,
    },
    /// Recent notifications for the notification centre, newest first.
    LoadNotifications {
        /// At most this many.
        limit: usize,
        /// Receives them.
        reply: oneshot::Sender<Vec<NotificationRecord>>,
    },
    /// Marks every notification read.
    MarkNotificationsRead,
    /// Evaluates a view that isn't saved yet (the dashboard editor's live
    /// match count and rows).
    PreviewDashboard {
        /// The view being edited.
        view: ic_config::View,
        /// Receives the result, or why the filter doesn't work.
        reply: oneshot::Sender<Result<DashboardResult, String>>,
    },
    /// Fetches full details (output, perfdata, links) of lean objects: the
    /// rows on screen and an opened pane. Sent debounced by the UI.
    Hydrate(Vec<ObjectKey>),
}

/// What the engine tells the UI. Received from
/// [`CoreHandle::take_events`](crate::CoreHandle::take_events).
#[derive(Clone, Debug)]
pub enum CoreEvent {
    /// The connection changed state.
    Connection(ConnectionState),
    /// A new snapshot of everything known about the environment.
    Snapshot(Arc<Snapshot>),
    /// The API user and its permissions, after every successful connect.
    Permissions(ApiInfo),
    /// A [`Command::Action`] finished.
    ActionFinished {
        /// The command's id.
        id: u64,
        /// What happened.
        outcome: ActionOutcome,
    },
    /// A notification was decided on (silent or not), after it was logged.
    Notification(NotificationRecord),
    /// Notifications are paused until this time (`None`: not paused).
    NotificationsPaused(Option<Timestamp>),
}

/// The connection's state, for the footer and the connection banner.
#[derive(Clone, Debug, PartialEq)]
pub enum ConnectionState {
    /// Connecting: reading the secret, checking credentials, opening the
    /// event stream.
    Connecting {
        /// 1 for the first attempt; counts up while attempts fail.
        attempt: u32,
    },
    /// The tiered initial load after connecting.
    Loading {
        /// What is being loaded.
        phase: LoadPhase,
        /// How much of the phase is done (queries or objects, see
        /// [`LoadPhase`]).
        done: usize,
        /// How much there is, when known.
        total: Option<usize>,
    },
    /// Connected and live: the event stream is open (if the API user may
    /// read events) and the initial load is complete.
    Connected {
        /// The endpoint's node name (`master-01`), or the URL's host.
        endpoint: String,
        /// Icinga's version.
        version: String,
        /// Since when.
        since: Timestamp,
    },
    /// The connection failed or broke; the engine retries at `retry_at`
    /// with exponential backoff and jitter (1 s → 60 s). `Refresh` retries
    /// now.
    Reconnecting {
        /// Why.
        error: String,
        /// The attempt that runs at `retry_at` (the next `Connecting`).
        attempt: u32,
        /// When.
        retry_at: Timestamp,
    },
    /// Icinga refused the credentials (401). No automatic retry: `Refresh`
    /// or `UpdateEnvironment` retries.
    AuthFailed {
        /// What to tell the user.
        message: String,
    },
    /// The server's certificate isn't trusted or doesn't match the pin. No
    /// automatic retry; the UI offers trust on first use with
    /// `certificate`.
    TlsFailed {
        /// What went wrong (with both fingerprints for a pin mismatch).
        message: String,
        /// The certificate the server presented, when it could be read.
        certificate: Option<CertificateInfo>,
    },
    /// Basic authentication, but the keychain has no password for the
    /// environment. No automatic retry.
    MissingSecret,
    /// The environment's own settings can't work: an invalid URL or pinned
    /// fingerprint, an unreadable or invalid CA, certificate or key file.
    /// No automatic retry: fix the settings (`UpdateEnvironment`) or
    /// `Refresh`. (A secret store that can't be read right now, such as a
    /// keyring daemon still starting at login, is `Reconnecting`.)
    Misconfigured {
        /// What is wrong, for the environment editor.
        message: String,
    },
}

/// The tiers of the initial load (docs/performance.md).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LoadPhase {
    /// Tier 1: status, groups, dependencies, endpoints, comments, downtimes
    /// and hosts in full. `done`/`total` count these queries.
    Hosts,
    /// Tier 2: every service, lean. One query; `total` is unknown.
    Services,
    /// Tier 3: the full details of every service in a problem state, by
    /// name. `done`/`total` count services.
    Details,
}

/// How an action went.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ActionOutcome {
    /// Objects it succeeded for.
    pub ok: usize,
    /// Objects it failed for: (full name, Icinga's message).
    pub failed: Vec<(String, String)>,
    /// Why the request failed as a whole, if it did (then `ok` is 0 and
    /// `failed` is empty).
    pub error: Option<String>,
}

/// One entry of the local event log.
#[derive(Clone, Debug, PartialEq)]
pub struct LogEntry {
    /// When it happened.
    pub at: Timestamp,
    /// The host or service.
    pub object: ObjectKey,
    /// What happened.
    pub kind: LogKind,
    /// Output, comment or similar.
    pub text: String,
    /// Who did it (acknowledgements, comments, downtimes).
    pub author: Option<String>,
}

/// What a [`LogEntry`] records.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LogKind {
    /// A state change, soft or hard.
    State {
        /// The new state.
        state: CheckableState,
        /// Soft or hard.
        state_type: StateType,
    },
    /// A problem was acknowledged.
    AcknowledgementSet,
    /// An acknowledgement ended.
    AcknowledgementCleared,
    /// A user comment was added.
    CommentAdded,
    /// A user comment was removed.
    CommentRemoved,
    /// A downtime took effect.
    DowntimeStarted,
    /// A downtime ended or was removed.
    DowntimeEnded,
    /// The object started flapping.
    FlappingStarted,
    /// The object stopped flapping.
    FlappingStopped,
}

/// A notification in the notification centre.
#[derive(Clone, Debug, PartialEq)]
pub struct NotificationRecord {
    /// What the rule engine decided.
    pub intent: NotificationIntent,
    /// Whether the user has seen it.
    pub read: bool,
}
