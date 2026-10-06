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
    /// Where the environment's event log lives.
    pub data_dir: PathBuf,
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
    /// ([`crate::Command::SetActive`]`(false)`) go out at most this often
    /// (2 s): only the tray and the environment switcher read them. Rule
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
    /// uses the setting: adaptive for 0 (5 minutes below 5 000 objects,
    /// 15 minutes above), else the setting but at least
    /// `ic_config::MIN_RECONCILE_INTERVAL_SECS`.
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
        }
    }
}
