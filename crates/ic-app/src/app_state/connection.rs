//! The connection to the active environment as the UI shows it: the
//! footer's `● master-01 · 2s` (ENV-06), the connection banner (ENV-07),
//! and the load progress. Built from the core's `ConnectionState` and the
//! snapshots; pure, so it's tested without a window.

use ic_core::ConnectionState;
use ic_core::LoadPhase;
use ic_core::snapshot::Snapshot;
use ic_model::{Timestamp, format_compact};

/// Events older than this make a live connection look stale (PLAN.md §2.1),
/// unless Icinga ran no checks in the last minute (then nothing is sent).
pub(crate) const STALE_AFTER_SECS: u64 = 30;

/// How the footer colours the connection status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Health {
    /// Connected with recent events (green).
    Live,
    /// Connected, but no event for more than 30 seconds while Icinga is
    /// checking (yellow).
    Stale,
    /// Lost; retrying with backoff (red).
    Reconnecting,
    /// Stopped: the login, the certificate or the settings need the user
    /// (red).
    Failed,
    /// Connecting or loading (grey).
    Connecting,
    /// No environment, or not started (grey).
    Idle,
}

/// How urgent a [`ConnectionNotice`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tone {
    /// Broken: the connection is lost or refused.
    Critical,
    /// Needs attention.
    Warning,
}

/// What a notice offers to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NoticeAction {
    /// Connect (or reload) now: `Command::Refresh`.
    RetryNow,
    /// Look at the certificate and decide whether to trust it (the trust
    /// flow of the environment settings).
    ReviewCertificate,
    /// Open the environment's settings (password, URL, files).
    EditEnvironment,
    /// Start the connection engine again (after it stopped or couldn't
    /// start).
    RestartEngine,
}

impl NoticeAction {
    /// The link's text.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::RetryNow => "Retry now",
            Self::ReviewCertificate => "Review certificate…",
            Self::EditEnvironment => "Edit environment…",
            Self::RestartEngine => "Restart",
        }
    }
}

/// A problem with the connection, for the banner over the list (or the
/// whole main area while there is nothing to show yet).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConnectionNotice {
    /// What kind of problem, for the icon.
    pub(crate) kind: NoticeKind,
    /// How urgent.
    pub(crate) tone: Tone,
    /// One sentence.
    pub(crate) title: String,
    /// What Icinga or the network said.
    pub(crate) detail: Option<String>,
    /// What the user can do, in order.
    pub(crate) actions: Vec<NoticeAction>,
}

/// The kinds of [`ConnectionNotice`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NoticeKind {
    /// Lost or unreachable; retrying.
    Reconnecting,
    /// Icinga refused the credentials.
    AuthFailed,
    /// The certificate isn't trusted.
    TlsFailed,
    /// No password in the keychain.
    MissingSecret,
    /// The environment's settings can't work.
    Misconfigured,
    /// The engine itself couldn't start.
    EngineFailed,
}

/// How far the initial load has got.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Progress {
    /// 0–1, for the progress bar.
    pub(crate) fraction: f32,
    /// What is happening: `Loading services…`.
    pub(crate) text: String,
}

/// The connection to the active environment.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ConnectionStatus {
    /// The endpoint's name: Icinga's node name once known, else the URL's
    /// host.
    pub(crate) endpoint: String,
    /// The API user, for messages (`None` for client certificates).
    pub(crate) user: Option<String>,
    /// The core's latest state; `None` without an environment.
    pub(crate) state: Option<ConnectionState>,
    /// When the latest event-stream message arrived (local clock).
    pub(crate) last_event_at: Option<Timestamp>,
    /// Whether Icinga ran active checks in the last minute (from the
    /// status poll); `None` while unknown.
    pub(crate) checks_active: Option<bool>,
    /// Whether this session was connected at some point.
    pub(crate) ever_connected: bool,
    /// The engine couldn't start (a thread or runtime error), or stopped
    /// on its own.
    pub(crate) engine_error: Option<String>,
    /// The engine ran and stopped on its own (`engine_error` says why).
    engine_stopped: bool,
    /// Icinga's version, once connected.
    version: Option<String>,
}

impl ConnectionStatus {
    /// No environment.
    pub(crate) fn idle() -> Self {
        Self {
            endpoint: String::new(),
            user: None,
            state: None,
            last_event_at: None,
            checks_active: None,
            ever_connected: false,
            engine_error: None,
            engine_stopped: false,
            version: None,
        }
    }

    /// About to connect to `endpoint` (the URL's host) as `user`.
    pub(crate) fn starting(endpoint: &str, user: Option<String>) -> Self {
        Self {
            endpoint: endpoint.to_owned(),
            user,
            state: Some(ConnectionState::Connecting { attempt: 1 }),
            ..Self::idle()
        }
    }

    /// The core reported `state`.
    pub(crate) fn on_state(&mut self, state: ConnectionState) {
        if let ConnectionState::Connected {
            endpoint, version, ..
        } = &state
        {
            if !endpoint.trim().is_empty() {
                endpoint.clone_into(&mut self.endpoint);
            }
            if !version.trim().is_empty() {
                self.version = Some(version.clone());
            }
            self.ever_connected = true;
        }
        self.engine_error = None;
        self.engine_stopped = false;
        self.state = Some(state);
    }

    /// The core published `snapshot`.
    pub(crate) fn on_snapshot(&mut self, snapshot: &Snapshot) {
        if snapshot.last_event_at.is_some() {
            self.last_event_at = snapshot.last_event_at;
        }
        if let Some(status) = &snapshot.status {
            self.checks_active = Some(status.checks_per_minute >= 1.);
        }
    }

    /// The engine couldn't start.
    pub(crate) fn on_engine_error(&mut self, error: String) {
        self.engine_error = Some(error);
        self.engine_stopped = false;
    }

    /// The engine stopped on its own (its event stream ended): what was
    /// shown is no longer kept up to date.
    pub(crate) fn on_engine_stopped(&mut self, error: String) {
        self.engine_error = Some(error);
        self.engine_stopped = true;
    }

    /// The engine's problem in a word or two.
    fn engine_word(&self) -> &'static str {
        if self.engine_stopped {
            "stopped"
        } else {
            "not started"
        }
    }

    /// Whether the connection is up and the initial load complete.
    pub(crate) fn is_connected(&self) -> bool {
        matches!(self.state, Some(ConnectionState::Connected { .. }))
    }

    /// Whether the engine waits: to reconnect (backoff), or for the user
    /// (login, certificate, password, settings). An environment update
    /// then makes it connect at once, so view changes wait until it
    /// connects by itself.
    pub(crate) fn is_waiting(&self) -> bool {
        matches!(
            self.state,
            Some(
                ConnectionState::Reconnecting { .. }
                    | ConnectionState::AuthFailed { .. }
                    | ConnectionState::TlsFailed { .. }
                    | ConnectionState::MissingSecret
                    | ConnectionState::Misconfigured { .. }
            )
        )
    }

    /// Whether the first connection or load is still running.
    pub(crate) fn is_starting(&self) -> bool {
        matches!(
            self.state,
            Some(ConnectionState::Connecting { .. } | ConnectionState::Loading { .. })
        ) && self.engine_error.is_none()
    }

    /// The colour class at `now`.
    pub(crate) fn health(&self, now: Timestamp) -> Health {
        if self.engine_error.is_some() {
            return Health::Failed;
        }
        match &self.state {
            None => Health::Idle,
            Some(ConnectionState::Connecting { .. } | ConnectionState::Loading { .. }) => {
                Health::Connecting
            }
            Some(ConnectionState::Reconnecting { .. }) => Health::Reconnecting,
            Some(
                ConnectionState::AuthFailed { .. }
                | ConnectionState::TlsFailed { .. }
                | ConnectionState::MissingSecret
                | ConnectionState::Misconfigured { .. },
            ) => Health::Failed,
            Some(ConnectionState::Connected { since, .. }) => {
                let quiet = self.quiet_for(*since, now).as_secs() > STALE_AFTER_SECS;
                if quiet && self.checks_active != Some(false) {
                    Health::Stale
                } else {
                    Health::Live
                }
            }
        }
    }

    /// How long nothing arrived: since the last event, or since the
    /// connection came up if no event arrived after that.
    fn quiet_for(&self, since: Timestamp, now: Timestamp) -> std::time::Duration {
        let last = match self.last_event_at {
            Some(at) if at > since => at,
            _ => since,
        };
        last.elapsed_until(now)
    }

    /// The footer text at `now`: the endpoint and the age of the last event,
    /// or what the connection is doing.
    pub(crate) fn label(&self, now: Timestamp) -> String {
        let (endpoint, status) = self.label_parts(now);
        match status {
            Some(status) => format!("{endpoint} · {status}"),
            None => endpoint,
        }
    }

    /// [`ConnectionStatus::label`] in its two parts: the endpoint (`no
    /// environment` without one) and the age of the last event or what the
    /// connection is doing. The footer shortens the endpoint, never
    /// the part after it (ENV-06): production endpoints are long FQDNs.
    pub(crate) fn label_parts(&self, now: Timestamp) -> (String, Option<String>) {
        let endpoint = if self.endpoint.is_empty() {
            "no environment".to_owned()
        } else {
            self.endpoint.clone()
        };
        if self.engine_error.is_some() {
            return (endpoint, Some(self.engine_word().to_owned()));
        }
        let Some(state) = &self.state else {
            return (endpoint, None);
        };
        let status = match state {
            ConnectionState::Connected { since, .. } => format_compact(self.quiet_for(*since, now)),
            ConnectionState::Connecting { attempt } if *attempt > 1 => {
                format!("connecting ({attempt})")
            }
            ConnectionState::Connecting { .. } => "connecting".to_owned(),
            ConnectionState::Loading { .. } => "loading".to_owned(),
            ConnectionState::Reconnecting { retry_at, .. } => {
                let wait = retry_at.remaining_from(now);
                if wait.as_secs() == 0 {
                    "retrying".to_owned()
                } else {
                    format!("retry in {}", format_compact(wait))
                }
            }
            ConnectionState::AuthFailed { .. } => "login refused".to_owned(),
            ConnectionState::TlsFailed { .. } => "not trusted".to_owned(),
            ConnectionState::MissingSecret => "no password".to_owned(),
            ConnectionState::Misconfigured { .. } => "invalid settings".to_owned(),
        };
        (endpoint, Some(status))
    }

    /// The state in a word or two, without times (the tray's tooltip):
    /// `connected`, `reconnecting`, `login refused`, …
    pub(crate) fn short_state(&self) -> &'static str {
        if self.engine_error.is_some() {
            return self.engine_word();
        }
        match &self.state {
            None => "not connected",
            Some(ConnectionState::Connected { .. }) => "connected",
            Some(ConnectionState::Connecting { .. }) => "connecting",
            Some(ConnectionState::Loading { .. }) => "loading",
            Some(ConnectionState::Reconnecting { .. }) => "reconnecting",
            Some(ConnectionState::AuthFailed { .. }) => "login refused",
            Some(ConnectionState::TlsFailed { .. }) => "certificate not trusted",
            Some(ConnectionState::MissingSecret) => "no password",
            Some(ConnectionState::Misconfigured { .. }) => "invalid settings",
        }
    }

    /// Icinga's version, once known.
    pub(crate) fn version(&self) -> Option<&str> {
        self.version.as_deref()
    }

    /// The state in words, for the connection details: `connected for 3h`,
    /// `retrying in 12s (attempt 4): connection refused`.
    pub(crate) fn describe(&self, now: Timestamp) -> String {
        if let Some(error) = &self.engine_error {
            return format!("{}: {error}", self.engine_word());
        }
        let Some(state) = &self.state else {
            return "no environment".to_owned();
        };
        match state {
            ConnectionState::Connected { since, .. } => {
                format!("connected for {}", format_compact(since.elapsed_until(now)))
            }
            ConnectionState::Connecting { attempt: 1 } => "connecting".to_owned(),
            ConnectionState::Connecting { attempt } => format!("connecting (attempt {attempt})"),
            ConnectionState::Loading { .. } => self
                .progress()
                .map_or_else(|| "loading".to_owned(), |progress| progress.text),
            ConnectionState::Reconnecting {
                error,
                attempt,
                retry_at,
            } => format!(
                "retrying in {} (attempt {attempt}): {error}",
                format_compact(retry_at.remaining_from(now))
            ),
            ConnectionState::AuthFailed { message } => format!("login refused: {message}"),
            ConnectionState::TlsFailed { message, .. } => {
                format!("certificate not trusted: {message}")
            }
            ConnectionState::MissingSecret => "no password in the keychain".to_owned(),
            ConnectionState::Misconfigured { message } => format!("settings can't work: {message}"),
        }
    }

    /// The load's progress while connecting or loading.
    pub(crate) fn progress(&self) -> Option<Progress> {
        if self.engine_error.is_some() {
            return None;
        }
        let (fraction, text) = match self.state.as_ref()? {
            ConnectionState::Connecting { attempt } => (
                0.02,
                if *attempt > 1 {
                    format!("Connecting to {} (attempt {attempt})…", self.endpoint)
                } else {
                    format!("Connecting to {}…", self.endpoint)
                },
            ),
            ConnectionState::Loading { phase, done, total } => {
                let part = |done: usize, total: Option<usize>| match total {
                    Some(total) if total > 0 => ratio(done.min(total), total),
                    _ => 0.,
                };
                match phase {
                    LoadPhase::Hosts => (
                        0.05 + 0.3 * part(*done, *total),
                        format!("Loading hosts and groups{}…", counted(*done, *total)),
                    ),
                    LoadPhase::Services => (0.4, "Loading services…".to_owned()),
                    LoadPhase::Details => (
                        0.7 + 0.3 * part(*done, *total),
                        format!("Loading problem details{}…", counted(*done, *total)),
                    ),
                }
            }
            _ => return None,
        };
        Some(Progress { fraction, text })
    }

    /// The problem to show over the list at `now`, if any.
    pub(crate) fn notice(&self, environment: &str, now: Timestamp) -> Option<ConnectionNotice> {
        if let Some(error) = &self.engine_error {
            let title = if self.engine_stopped {
                "The connection engine stopped: what is shown is no longer updated."
            } else {
                "The connection engine couldn't start."
            };
            return Some(ConnectionNotice {
                kind: NoticeKind::EngineFailed,
                tone: Tone::Critical,
                title: title.to_owned(),
                detail: Some(error.clone()),
                actions: vec![NoticeAction::RestartEngine],
            });
        }
        let endpoint = &self.endpoint;
        let notice = match self.state.as_ref()? {
            ConnectionState::Reconnecting {
                error,
                attempt,
                retry_at,
            } => {
                let wait = retry_at.remaining_from(now);
                let when = if wait.as_secs() == 0 {
                    "Retrying now…".to_owned()
                } else {
                    format!("Retrying in {}.", format_compact(wait))
                };
                let what = if self.ever_connected {
                    format!("Connection to {endpoint} lost.")
                } else {
                    format!("Can't connect to {endpoint}.")
                };
                ConnectionNotice {
                    kind: NoticeKind::Reconnecting,
                    tone: Tone::Critical,
                    title: format!("{what} {when}"),
                    detail: Some(format!("attempt {attempt} · {error}")),
                    actions: vec![NoticeAction::RetryNow],
                }
            }
            ConnectionState::AuthFailed { message } => ConnectionNotice {
                kind: NoticeKind::AuthFailed,
                tone: Tone::Critical,
                title: match &self.user {
                    Some(user) => format!("Icinga refused the login of {user}."),
                    None => "Icinga refused the client certificate.".to_owned(),
                },
                detail: Some(format!(
                    "{message} · check the credentials in the environment settings"
                )),
                actions: vec![NoticeAction::EditEnvironment, NoticeAction::RetryNow],
            },
            ConnectionState::TlsFailed {
                message,
                certificate,
            } => ConnectionNotice {
                kind: NoticeKind::TlsFailed,
                tone: Tone::Critical,
                title: format!("The certificate of {endpoint} isn't trusted."),
                detail: Some(match certificate {
                    Some(certificate) => format!(
                        "{message} · {} · SHA-256 {}",
                        certificate.subject,
                        certificate.fingerprint()
                    ),
                    None => message.clone(),
                }),
                actions: vec![NoticeAction::ReviewCertificate, NoticeAction::RetryNow],
            },
            ConnectionState::MissingSecret => ConnectionNotice {
                kind: NoticeKind::MissingSecret,
                tone: Tone::Warning,
                title: match &self.user {
                    Some(user) => {
                        format!("No password for {user} on {environment} in the keychain.")
                    }
                    None => format!("No password for {environment} in the keychain."),
                },
                detail: Some("Enter it in the environment settings to connect.".to_owned()),
                actions: vec![NoticeAction::EditEnvironment, NoticeAction::RetryNow],
            },
            ConnectionState::Misconfigured { message } => ConnectionNotice {
                kind: NoticeKind::Misconfigured,
                tone: Tone::Warning,
                title: format!("The settings of {environment} can't work."),
                detail: Some(message.clone()),
                actions: vec![NoticeAction::EditEnvironment, NoticeAction::RetryNow],
            },
            ConnectionState::Connecting { .. }
            | ConnectionState::Loading { .. }
            | ConnectionState::Connected { .. } => return None,
        };
        Some(notice)
    }
}

/// `done / total` as a fraction.
#[expect(
    clippy::cast_precision_loss,
    reason = "a progress fraction: object counts are far below 2^24"
)]
fn ratio(done: usize, total: usize) -> f32 {
    done as f32 / total as f32
}

/// ` (3/8)`, or nothing without a total.
fn counted(done: usize, total: Option<usize>) -> String {
    match total {
        Some(total) if total > 0 => format!(" ({}/{total})", done.min(total)),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ic_core::CertificateInfo;
    use ic_model::InstanceStatus;

    use super::*;

    const NOW: f64 = 1_790_000_000.;

    fn at(offset: f64) -> Timestamp {
        Timestamp::from_unix_seconds(NOW + offset)
    }

    fn connected() -> ConnectionStatus {
        let mut status = ConnectionStatus::starting("master-01.example.com", Some("icygui".into()));
        status.on_state(ConnectionState::Connected {
            endpoint: "master-01".to_owned(),
            version: "r2.15.6-1".to_owned(),
            since: at(0.),
        });
        status
    }

    fn snapshot(last_event: Option<f64>, checks_per_minute: Option<f64>) -> Snapshot {
        Snapshot {
            last_event_at: last_event.map(at),
            status: checks_per_minute.map(|checks| {
                Arc::new(InstanceStatus {
                    checks_per_minute: checks,
                    ..InstanceStatus::default()
                })
            }),
            ..Snapshot::default()
        }
    }

    #[test]
    fn the_footer_shows_the_age_of_the_last_event() {
        let mut status = connected();
        assert_eq!(status.endpoint, "master-01", "Icinga's node name");
        status.on_snapshot(&snapshot(Some(10.), Some(120.)));
        assert_eq!(status.label(at(12.4)), "master-01 · 2s");
        assert_eq!(status.health(at(12.)), Health::Live);
        assert_eq!(status.health(at(40.)), Health::Live);
        assert_eq!(status.health(at(41.)), Health::Stale);
        assert_eq!(status.label(at(135.)), "master-01 · 2m");
    }

    #[test]
    fn a_quiet_icinga_is_not_stale() {
        let mut status = connected();
        // Icinga ran no checks in the last minute: no events are expected.
        status.on_snapshot(&snapshot(None, Some(0.)));
        assert_eq!(status.health(at(600.)), Health::Live);
        assert_eq!(
            status.label(at(65.)),
            "master-01 · 1m",
            "counted from the connect"
        );
        // Unknown (no status permission): stale after 30 s like PLAN.md says.
        let mut unknown = connected();
        unknown.on_snapshot(&snapshot(None, None));
        assert_eq!(unknown.health(at(31.)), Health::Stale);
    }

    #[test]
    fn snapshots_without_events_keep_the_last_one() {
        let mut status = connected();
        status.on_snapshot(&snapshot(Some(5.), None));
        status.on_snapshot(&snapshot(None, None));
        assert_eq!(status.last_event_at, Some(at(5.)));
    }

    #[test]
    fn states_have_labels_and_colours() {
        let mut status = ConnectionStatus::starting("master-01", Some("icygui".into()));
        assert_eq!(status.health(at(0.)), Health::Connecting);
        assert_eq!(status.label(at(0.)), "master-01 · connecting");
        status.on_state(ConnectionState::Connecting { attempt: 3 });
        assert_eq!(status.label(at(0.)), "master-01 · connecting (3)");
        status.on_state(ConnectionState::Reconnecting {
            error: "connection refused".to_owned(),
            attempt: 4,
            retry_at: at(12.),
        });
        assert_eq!(status.health(at(0.)), Health::Reconnecting);
        assert_eq!(status.label(at(0.)), "master-01 · retry in 12s");
        assert_eq!(status.label(at(13.)), "master-01 · retrying");
        status.on_state(ConnectionState::AuthFailed {
            message: "401".to_owned(),
        });
        assert_eq!(status.health(at(0.)), Health::Failed);
        assert_eq!(status.label(at(0.)), "master-01 · login refused");
        status.on_state(ConnectionState::MissingSecret);
        assert_eq!(status.label(at(0.)), "master-01 · no password");
        assert_eq!(ConnectionStatus::idle().health(at(0.)), Health::Idle);
        assert_eq!(ConnectionStatus::idle().label(at(0.)), "no environment");
        assert_eq!(
            ConnectionStatus::idle().label_parts(at(0.)),
            ("no environment".to_owned(), None)
        );
    }

    #[test]
    fn the_footer_keeps_the_state_apart_from_a_long_endpoint() {
        let mut status = connected();
        status.endpoint = "icinga-master1.prod.example.com".to_owned();
        status.on_state(ConnectionState::Reconnecting {
            error: "connection refused".to_owned(),
            attempt: 4,
            retry_at: at(12.),
        });
        assert_eq!(
            status.label_parts(at(0.)),
            (
                "icinga-master1.prod.example.com".to_owned(),
                Some("retry in 12s".to_owned())
            )
        );
        assert_eq!(
            status.label(at(0.)),
            "icinga-master1.prod.example.com · retry in 12s"
        );
    }

    #[test]
    fn reconnecting_counts_down_and_offers_retry() {
        let mut status = connected();
        status.on_state(ConnectionState::Reconnecting {
            error: "stream ended".to_owned(),
            attempt: 2,
            retry_at: at(30.),
        });
        let notice = status.notice("prod-cluster", at(18.)).unwrap();
        assert_eq!(notice.kind, NoticeKind::Reconnecting);
        assert_eq!(
            notice.title,
            "Connection to master-01 lost. Retrying in 12s."
        );
        assert_eq!(notice.detail.as_deref(), Some("attempt 2 · stream ended"));
        assert_eq!(notice.actions, [NoticeAction::RetryNow]);
        assert_eq!(
            status.notice("prod-cluster", at(31.)).unwrap().title,
            "Connection to master-01 lost. Retrying now…"
        );

        let mut never = ConnectionStatus::starting("master-01", None);
        never.on_state(ConnectionState::Reconnecting {
            error: "refused".to_owned(),
            attempt: 1,
            retry_at: at(1.),
        });
        assert!(
            never
                .notice("prod-cluster", at(0.))
                .unwrap()
                .title
                .starts_with("Can't connect to master-01.")
        );
    }

    #[test]
    fn failures_that_need_the_user_say_what_to_do() {
        let mut status = ConnectionStatus::starting("master-01", Some("icygui".into()));
        status.on_state(ConnectionState::AuthFailed {
            message: "401 Unauthorized".to_owned(),
        });
        let auth = status.notice("prod-cluster", at(0.)).unwrap();
        assert_eq!(auth.title, "Icinga refused the login of icygui.");
        assert_eq!(
            auth.actions,
            [NoticeAction::EditEnvironment, NoticeAction::RetryNow]
        );

        status.on_state(ConnectionState::TlsFailed {
            message: "unknown issuer".to_owned(),
            certificate: Some(CertificateInfo {
                sha256: [0xab; 32],
                subject: "CN=master-01".to_owned(),
                issuer: "CN=Icinga CA".to_owned(),
                names: vec!["master-01".to_owned()],
                not_before: at(0.),
                not_after: at(1.),
            }),
        });
        let tls = status.notice("prod-cluster", at(0.)).unwrap();
        assert_eq!(tls.kind, NoticeKind::TlsFailed);
        assert_eq!(tls.actions[0], NoticeAction::ReviewCertificate);
        let detail = tls.detail.unwrap();
        assert!(detail.contains("CN=master-01"), "{detail}");
        assert!(detail.contains("AB:AB:AB"), "{detail}");

        status.on_state(ConnectionState::MissingSecret);
        let missing = status.notice("prod-cluster", at(0.)).unwrap();
        assert_eq!(
            missing.title,
            "No password for icygui on prod-cluster in the keychain."
        );
        assert_eq!(missing.tone, Tone::Warning);

        status.on_state(ConnectionState::Misconfigured {
            message: "invalid pinned fingerprint".to_owned(),
        });
        let settings = status.notice("prod-cluster", at(0.)).unwrap();
        assert_eq!(settings.title, "The settings of prod-cluster can't work.");
        assert_eq!(
            settings.detail.as_deref(),
            Some("invalid pinned fingerprint")
        );

        status.on_state(ConnectionState::Connecting { attempt: 1 });
        assert_eq!(status.notice("prod-cluster", at(0.)), None);
    }

    #[test]
    fn the_details_describe_the_state() {
        let mut status = connected();
        assert_eq!(status.version(), Some("r2.15.6-1"));
        assert_eq!(status.describe(at(3. * 3600.)), "connected for 3h");
        status.on_state(ConnectionState::Reconnecting {
            error: "refused".to_owned(),
            attempt: 4,
            retry_at: at(12.),
        });
        assert_eq!(
            status.describe(at(0.)),
            "retrying in 12s (attempt 4): refused"
        );
        assert_eq!(
            status.version(),
            Some("r2.15.6-1"),
            "kept while reconnecting"
        );
        status.on_state(ConnectionState::Loading {
            phase: LoadPhase::Services,
            done: 0,
            total: None,
        });
        assert_eq!(status.describe(at(0.)), "Loading services…");
        assert_eq!(ConnectionStatus::idle().describe(at(0.)), "no environment");
    }

    #[test]
    fn loading_shows_progress() {
        let mut status = ConnectionStatus::starting("master-01", None);
        let connecting = status.progress().unwrap();
        assert_eq!(connecting.text, "Connecting to master-01…");
        status.on_state(ConnectionState::Loading {
            phase: LoadPhase::Hosts,
            done: 4,
            total: Some(8),
        });
        let hosts = status.progress().unwrap();
        assert!((hosts.fraction - 0.2).abs() < 1e-6);
        assert_eq!(hosts.text, "Loading hosts and groups (4/8)…");
        status.on_state(ConnectionState::Loading {
            phase: LoadPhase::Services,
            done: 0,
            total: None,
        });
        assert_eq!(status.progress().unwrap().text, "Loading services…");
        status.on_state(ConnectionState::Loading {
            phase: LoadPhase::Details,
            done: 500,
            total: Some(400),
        });
        let details = status.progress().unwrap();
        assert!((details.fraction - 1.).abs() < 1e-6, "clamped");
        assert_eq!(details.text, "Loading problem details (400/400)…");
        assert!(status.is_starting());
        status.on_state(ConnectionState::Connected {
            endpoint: String::new(),
            version: String::new(),
            since: at(0.),
        });
        assert_eq!(status.progress(), None);
        assert_eq!(status.endpoint, "master-01", "a blank node name is ignored");
        assert!(!status.is_starting());
    }

    #[test]
    fn an_engine_that_cant_start_is_a_failure() {
        let mut status = ConnectionStatus::starting("master-01", None);
        status.on_engine_error("no threads left".to_owned());
        assert_eq!(status.health(at(0.)), Health::Failed);
        assert_eq!(status.label(at(0.)), "master-01 · not started");
        let notice = status.notice("prod-cluster", at(0.)).unwrap();
        assert_eq!(notice.kind, NoticeKind::EngineFailed);
        assert_eq!(notice.actions, [NoticeAction::RestartEngine]);
        assert!(!status.is_starting());
        assert_eq!(status.progress(), None);
    }

    #[test]
    fn an_engine_that_stopped_says_so_and_offers_a_restart() {
        let mut status = connected();
        status.on_engine_stopped("the engine's event stream ended".to_owned());
        assert_eq!(status.health(at(0.)), Health::Failed);
        assert_eq!(status.label(at(0.)), "master-01 · stopped");
        assert_eq!(status.short_state(), "stopped");
        let notice = status.notice("prod-cluster", at(0.)).unwrap();
        assert_eq!(notice.kind, NoticeKind::EngineFailed);
        assert!(notice.title.contains("stopped"), "{}", notice.title);
        assert_eq!(notice.actions, [NoticeAction::RestartEngine]);
        // A new engine's first state clears it.
        status.on_state(ConnectionState::Connecting { attempt: 1 });
        assert_eq!(status.notice("prod-cluster", at(0.)), None);
    }
}
