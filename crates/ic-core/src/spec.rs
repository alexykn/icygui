//! What [`crate::start`] needs: the environment, the platform ports and the
//! engine's timing.

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::ports::{Clock, Notifier, SecretStore};

/// The environment to run and the settings around it.
#[derive(Clone, Debug)]
pub struct EnvironmentSpec {
    /// The environment (URLs, authentication, TLS, dashboards, rules).
    pub environment: ic_config::Environment,
    /// App-wide settings (reconcile interval, log retention).
    pub general: ic_config::General,
    /// The settings' handled defaults (`[appearance.hide_handled]`), which
    /// list views follow unless they set their own;
    /// [`crate::Command::SetHandledDefaults`] changes them.
    pub hide_handled: ic_config::HideHandled,
    /// Where the environment's event log lives.
    pub data_dir: PathBuf,
    /// Whether the user is waiting for this engine ([`Start::User`]) or
    /// the app started in the background ([`Start::Background`]: launch at
    /// login, `--background`), when the first load waits a random delay
    /// proportional to the installation's size (PERF-09).
    pub start: Start,
}

/// How an engine starts (PERF-09).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Start {
    /// The user is there (the window is shown): the first load starts as
    /// soon as the engine is connected.
    #[default]
    User,
    /// The app started in the background (launch at login): the first
    /// load waits a random delay of up to `Tuning::start_delay_per_thousand`
    /// per 1 000 services (at most `Tuning::start_delay_max`), the size
    /// read from `/v1/status/CIB` first, so many clients started together
    /// (a team logging in at nine) spread their big queries out. Small
    /// installations wait under a second. [`crate::Command::StartNow`]
    /// (the window was shown) ends the wait.
    Background,
}

/// The operating-system services the engine uses.
#[derive(Clone)]
pub struct Ports {
    /// Passwords, by environment id.
    pub secrets: Arc<dyn SecretStore>,
    /// OS notifications.
    pub notifier: Arc<dyn Notifier>,
    /// Wall-clock time.
    pub clock: Arc<dyn Clock>,
}

impl fmt::Debug for Ports {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Ports").finish_non_exhaustive()
    }
}

/// The engine's timing. [`Tuning::default`] holds the production values
/// (docs/architecture.md, ic-core); tests shorten them with
/// [`crate::start_with_tuning`] instead of sleeping.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tuning {
    /// The first reconnect delay (1 s); it doubles per failed attempt.
    pub backoff_initial: Duration,
    /// The longest reconnect delay (60 s).
    pub backoff_max: Duration,
    /// A connection that stayed up this long (5 minutes) resets the
    /// backoff, so its next failure retries after `backoff_initial`.
    pub healthy_after: Duration,
    /// How often `/v1/status` is polled while connected (30 s); a node
    /// reporting another `program_start` than before restarted, which
    /// triggers a reload.
    pub status_interval: Duration,
    /// The event stream counts as stalled (and the engine reconnects) when
    /// a status poll reports active checks in the last minute but no line
    /// arrived for this long (2 minutes): every check sends a
    /// `CheckResult` event, and a stream behind a proxy can stop without
    /// closing.
    pub stall_after: Duration,
    /// Snapshots go out at most this often while things change (250 ms).
    pub publish_interval: Duration,
    /// Snapshots of an engine whose environment isn't on screen
    /// ([`crate::Command::SetActive`]`(false)`), or that is quiet
    /// ([`crate::Command::SetQuiet`]), go out at most this often (2 s):
    /// only the tray and the environment switcher read them. Rule
    /// inputs waiting for their dashboard memberships still go out after
    /// `publish_interval`, so its notifications are as prompt as the
    /// active environment's.
    pub background_publish_interval: Duration,
    /// Objects marked for re-query are collected this long before the
    /// request goes out (200 ms), so a burst of config changes costs few
    /// requests.
    pub requery_delay: Duration,
    /// An object Icinga reported as unknown (deleted, or hidden by a
    /// filtered permission) isn't re-queried again for events about it
    /// within this time (10 minutes), unless it is created again; each
    /// time it comes back unknown the wait doubles (up to 4 hours).
    pub missing_ttl: Duration,
    /// The applier takes at most this many event lines per batch (5 000).
    pub max_batch: usize,
    /// How long [`crate::CoreHandle::shutdown`] waits for the runtime
    /// thread (5 s).
    pub shutdown_timeout: Duration,
    /// The freshness watchdog looks for overdue objects at most this often
    /// (5 s), re-querying at most 200 of them per look.
    pub watchdog_interval: Duration,
    /// After a reconnect the client goes live on the objects it has and,
    /// if the stream was gone for `reload_after_gap`, reloads after a
    /// random delay below this (10 s), so clients reconnecting together
    /// don't reload at the same instant.
    pub reload_jitter: Duration,
    /// A reconnect reloads only when the stream was silent this long (2
    /// minutes, wall-clock or monotonic, whichever is longer: a laptop
    /// that slept, a network outage, a stall). After a shorter gap the
    /// events since bring every object that is checked again, an Icinga
    /// restart is caught by the status poll, and the periodic reconcile
    /// catches up the rest: a stream that keeps ending (a proxy's maximum
    /// response time) doesn't cost a reload each time.
    pub reload_after_gap: Duration,
    /// Reloads asked for by a detected restart start at most this often
    /// (30 s); `Refresh` by the user waits this long after the previous
    /// reload, and longer the more often it is pressed (4× after the
    /// second, 10× after the third, until it rests for 20×).
    pub reload_spacing: Duration,
    /// A failed first load (the big lean lists) is retried after this
    /// (30 s, half of it jitter), doubling with every further failure up
    /// to the reconcile interval, instead of with each quick reconnect.
    pub load_retry_initial: Duration,
    /// The periodic reconcile's interval, overriding
    /// `General::reconcile_interval_secs` (tests); `None` (the default)
    /// uses the setting: adaptive for 0 (28 ms per host and service, 5 to
    /// 60 minutes, doubled up to twice while the event stream stays
    /// continuous and reconciles find nothing it missed, at most 60
    /// minutes), else the setting but at least
    /// `ic_config::MIN_RECONCILE_INTERVAL_SECS`. Quiet mode makes it at
    /// least `quiet_reconcile_interval` either way.
    pub reconcile_interval: Option<Duration>,
    /// How often the notification rule engine ticks (1 s): delayed
    /// notifications, storm summaries, pauses and mutes ending.
    pub rule_tick: Duration,
    /// How often the event log is pruned to
    /// `General::event_log_retention_hours` (1 hour); also when the engine
    /// starts.
    pub prune_interval: Duration,
    /// How long an action request waits for Icinga's answer (5 minutes,
    /// `ic_api::DEFAULT_ACTION_TIMEOUT`): Icinga answers only once it has
    /// run the action for every object of the request. Without an answer
    /// the outcome is unknown (Icinga may have applied it), and the same
    /// action on those objects is held back for a while (see
    /// [`crate::ActionOutcome`]).
    pub action_timeout: Duration,
    /// While connected to a node that doesn't see the whole cluster (a
    /// partial view, or one that couldn't be verified at a URL after the
    /// first), the engine asks the URLs it prefers whether one of them
    /// answers with a fuller view (ENV-12): first after this (30 s),
    /// doubling up to `probe_max`, half of each wait jitter. A probe costs
    /// each node that answers three small requests.
    pub probe_initial: Duration,
    /// The longest wait between such probes (10 minutes).
    pub probe_max: Duration,
    /// How often `/v1/status` is polled in quiet mode
    /// ([`crate::Command::SetQuiet`]; 5 minutes instead of
    /// `status_interval`).
    pub quiet_status_interval: Duration,
    /// The shortest reconcile interval in quiet mode (30 minutes): the
    /// adaptive interval, stretched while the stream is continuous, but at
    /// least this.
    pub quiet_reconcile_interval: Duration,
    /// How long the old and the new event stream may overlap when quiet
    /// mode switches the subscription (2 s): the old one is closed once a
    /// line came on both (it has delivered everything sent before the new
    /// one subscribed), or after this.
    pub stream_handover: Duration,
    /// The request budget for by-name queries ([`ic_api::RequestBudget`]):
    /// one request per `request_interval` (200 ms: 5 per second) after a
    /// burst of `request_burst` (10). Zero: no limit. The object the user
    /// is opening ([`crate::Command::Focus`]) never waits.
    pub request_interval: Duration,
    /// The budget's burst (10 requests).
    pub request_burst: u32,
    /// A background start ([`Start::Background`]) waits a random delay of
    /// up to this per 1 000 services (3 s) before its first load …
    pub start_delay_per_thousand: Duration,
    /// … but at most this (90 s).
    pub start_delay_max: Duration,
    /// The heartbeats' time budget ([`crate::heartbeat::Timing`]).
    pub heartbeat: crate::heartbeat::Timing,
    /// How long a trouble condition (no live data, an Icinga health
    /// alert) must hold before it is raised ([`crate::trouble::GRACE`],
    /// 2 minutes).
    pub trouble_grace: Duration,
    /// With several URLs, how long logging in at one and finding out its
    /// node (`GET /v1`, the node's name, the zones) may take while other
    /// URLs remain to try, and in a probe (8 s): a node that accepts
    /// connections but doesn't answer (an Icinga busy reloading) is passed
    /// over after this instead of the full request timeout. The last URL
    /// of a walk waits as long as any query.
    pub identify_timeout: Duration,
}

impl Default for Tuning {
    fn default() -> Self {
        Self {
            backoff_initial: Duration::from_secs(1),
            backoff_max: Duration::from_mins(1),
            healthy_after: Duration::from_mins(5),
            status_interval: Duration::from_secs(30),
            stall_after: Duration::from_mins(2),
            publish_interval: Duration::from_millis(250),
            background_publish_interval: Duration::from_secs(2),
            requery_delay: Duration::from_millis(200),
            missing_ttl: Duration::from_mins(10),
            max_batch: 5_000,
            shutdown_timeout: Duration::from_secs(5),
            watchdog_interval: Duration::from_secs(5),
            reload_jitter: Duration::from_secs(10),
            reload_after_gap: Duration::from_mins(2),
            reload_spacing: Duration::from_secs(30),
            load_retry_initial: Duration::from_secs(30),
            reconcile_interval: None,
            rule_tick: Duration::from_secs(1),
            prune_interval: Duration::from_hours(1),
            action_timeout: ic_api::DEFAULT_ACTION_TIMEOUT,
            probe_initial: Duration::from_secs(30),
            probe_max: Duration::from_mins(10),
            identify_timeout: Duration::from_secs(8),
            quiet_status_interval: Duration::from_mins(5),
            quiet_reconcile_interval: Duration::from_mins(30),
            stream_handover: Duration::from_secs(2),
            request_interval: Duration::from_millis(200),
            request_burst: 10,
            start_delay_per_thousand: Duration::from_secs(3),
            start_delay_max: Duration::from_secs(90),
            heartbeat: crate::heartbeat::Timing::default(),
            trouble_grace: crate::trouble::GRACE,
        }
    }
}
