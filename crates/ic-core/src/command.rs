//! The boundary between the UI and the engine: [`Command`]s in,
//! [`CoreEvent`]s out.

use std::sync::Arc;

use futures::channel::oneshot;
use ic_api::{ApiInfo, CertificateInfo};
use ic_model::{Action, ActionTarget, CheckableState, ObjectKey, ServiceKey, StateType, Timestamp};
use ic_rules::NotificationIntent;

use crate::snapshot::{DashboardResult, Snapshot};
use crate::topology::ConnectedNode;

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
    /// (URLs, their order, pins and server names, authentication, TLS)
    /// reconnect; anything else (dashboards, rules, author) applies in
    /// place.
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
    /// Marks one notification read, by its intent id (the notification
    /// centre's entry the user opened). An unknown id changes nothing.
    MarkNotificationRead(String),
    /// When the local event log's oldest entry happened: the history's
    /// "recorded locally since …". `None` while the log is empty (or there
    /// is no log).
    LoadHistoryStart {
        /// Receives the time.
        reply: oneshot::Sender<Option<Timestamp>>,
    },
    /// Evaluates views that aren't saved yet (the dashboard editor's live
    /// preview, match counts and rows) as one dashboard, with the
    /// settings' handled defaults.
    PreviewDashboard {
        /// The dashboard's views being edited.
        views: Vec<ic_config::View>,
        /// Receives the result; a view whose filter doesn't work has its
        /// `error` set.
        reply: oneshot::Sender<DashboardResult>,
    },
    /// The settings' handled defaults changed (`[appearance.hide_handled]`):
    /// the views that follow them are evaluated again.
    SetHandledDefaults(ic_config::HideHandled),
    /// Fetches full details (output, perfdata, links) of lean objects: the
    /// rows on screen and an opened pane. Sent debounced by the UI.
    Hydrate(Vec<ObjectKey>),
    /// Whether the environment is the one on screen (the app runs an
    /// engine for every environment; one is active). An engine starts
    /// active. An inactive one does everything an active one does (the
    /// event stream, reconciles, rules, the event log, notifications) and
    /// costs Icinga no more, but publishes snapshots at most every
    /// `Tuning::background_publish_interval` (2 s) while only check
    /// results change; becoming active publishes what changed at once.
    SetActive(bool),
    /// Quiet mode on or off (PERF-09; an engine starts live). The app
    /// turns it on for every environment that isn't on screen and for the
    /// one on screen while the window is hidden, when the general setting
    /// `quiet_when_hidden` is on.
    ///
    /// Quiet: the event stream is subscribed again without `CheckResult`
    /// events (state changes, acknowledgements, comments, downtimes,
    /// flapping, Icinga's notifications and configuration changes still
    /// arrive at once, so notifications are never delayed); the status
    /// poll runs every `Tuning::quiet_status_interval` (5 minutes) and the
    /// reconcile at least every `Tuning::quiet_reconcile_interval` (30
    /// minutes); no freshness watchdog, no hydration; only the dashboards
    /// that take part in notification decisions are evaluated. Stale while
    /// quiet: check outputs, last-check times and late markers.
    ///
    /// Live again (waking up): the full stream reopens without a gap or a
    /// duplicate (both streams overlap until a line came on both, see
    /// docs/architecture.md), the dashboards are evaluated again, and the
    /// problems whose check result may have changed meanwhile are fetched
    /// in full by name in the background (at most 1 000, most severe
    /// first), after the object the user opens ([`Command::Focus`]) and
    /// the rows on screen ([`Command::Hydrate`]).
    SetQuiet(bool),
    /// The object the user is opening (a notification clicked, the
    /// palette, the selection): fetched in full by name at once, ahead of
    /// every queued request and the request budget, unless the store holds
    /// it in full with a result no check can have replaced (live since
    /// before its last check). One request in flight; a newer `Focus`
    /// while one runs replaces the one waiting. The snapshot goes out as
    /// soon as the answer is in; meanwhile `Snapshot::updating` lists it.
    Focus(ObjectKey),
    /// The user is here (the window was shown): a background start's first
    /// load that still waits ([`crate::Start::Background`]) starts now.
    StartNow,
    /// Whether the cluster health page shows this environment (topic 06;
    /// the app sends `false` while the window is hidden). While it does and
    /// the environment isn't quiet, each status poll also asks for the
    /// node's `ApiListener` status, and the node's features are asked for
    /// when the page opens and every five minutes after (each request from
    /// the request budget); the page opening asks for both at once unless
    /// a poll brought them less than an interval ago. Nothing is asked for
    /// while no page shows it. [`crate::Snapshot::health`] carries them.
    ///
    /// While no page shows it, the trouble alerts still need both (a
    /// growing relay queue, a feature turned off): every five minutes, with
    /// a status poll, through the request budget.
    WatchHealth(bool),
    /// The user confirmed in the settings that a heartbeat that
    /// disappeared ([`crate::heartbeat::BeatState::Disappeared`]) is gone
    /// for good: it is forgotten, and its finding ends (without a
    /// notification).
    ConfirmHeartbeatRemoval(ServiceKey),
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
    /// The engine runs (every [`crate::ALIVE_INTERVAL`], whatever else it
    /// does): the UI takes an environment whose engine stopped saying so
    /// for stale, not for live (no false green). Carries the engine's
    /// clock.
    Alive(Timestamp),
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
        /// The node the engine is connected to: its name (`master-01`, or
        /// the URL's host when the API user may not read the status), the
        /// URL it was reached through, its zone and how much of the
        /// cluster it sees (ENV-12), and the URLs passed over on the way.
        node: ConnectedNode,
        /// Icinga's version.
        version: String,
        /// Since when.
        since: Timestamp,
    },
    /// The connection failed or broke; the engine retries at `retry_at`
    /// with exponential backoff and jitter (1 s → 60 s), trying every URL
    /// again. `Refresh` retries now.
    Reconnecting {
        /// Why.
        error: String,
        /// The attempt that runs at `retry_at` (the next `Connecting`).
        attempt: u32,
        /// When.
        retry_at: Timestamp,
        /// With several URLs: one whose server presented a certificate
        /// that isn't trusted (a standby never trusted on first use),
        /// which the UI offers to trust while the engine keeps retrying.
        untrusted: Option<UntrustedUrl>,
    },
    /// Icinga refused the credentials (401). No automatic retry: `Refresh`
    /// or `UpdateEnvironment` retries.
    AuthFailed {
        /// What to tell the user.
        message: String,
    },
    /// The server's certificate isn't trusted or doesn't match the pin. No
    /// automatic retry; the UI offers trust on first use with
    /// `certificate`. With several URLs, only when none of them may come
    /// back by itself (otherwise `Reconnecting` names each URL's problem).
    TlsFailed {
        /// The configured URL (as written in the environment's `urls`)
        /// whose server presented the certificate: its pin is the one to
        /// set.
        url: String,
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

impl ConnectionState {
    /// The URL whose certificate isn't trusted, what went wrong and the
    /// certificate (when it could be read): a [`ConnectionState::TlsFailed`],
    /// or the untrusted URL of a [`ConnectionState::Reconnecting`].
    #[must_use]
    pub fn untrusted(&self) -> Option<(&str, &str, Option<&CertificateInfo>)> {
        match self {
            Self::TlsFailed {
                url,
                message,
                certificate,
            } => Some((url, message, certificate.as_ref())),
            Self::Reconnecting {
                untrusted: Some(untrusted),
                ..
            } => Some((
                &untrusted.url,
                &untrusted.message,
                Some(&untrusted.certificate),
            )),
            _ => None,
        }
    }
}

/// A URL of the environment whose server presented a certificate that
/// isn't trusted, while the engine keeps retrying because another URL may
/// come back ([`ConnectionState::Reconnecting`]).
#[derive(Clone, Debug, PartialEq)]
pub struct UntrustedUrl {
    /// The configured URL (as written in the environment's `urls`): its
    /// pin is the one to set.
    pub url: String,
    /// What went wrong (with both fingerprints for a pin mismatch).
    pub message: String,
    /// The certificate the server presented.
    pub certificate: CertificateInfo,
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
    /// Objects it failed for: (full name, Icinga's message). Also objects
    /// whose outcome is unknown (Icinga got the request but didn't answer
    /// in time, so it may have applied it; the reason says so), objects
    /// not sent to after that, and repeats held back because an earlier
    /// request for them got no answer (the reason says why).
    pub failed: Vec<(String, String)>,
    /// Why the request failed as a whole, if it did (then `ok` is 0 and
    /// `failed` is empty). Only when nothing can have been applied.
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
