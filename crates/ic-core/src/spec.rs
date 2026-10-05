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
    /// The environment (URL, authentication, TLS, dashboards, rules).
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
    /// How often `/v1/status` is polled while connected (30 s); a changed
    /// `program_start` means Icinga restarted and triggers a reload.
    pub status_interval: Duration,
    /// Snapshots go out at most this often while things change (250 ms).
    pub publish_interval: Duration,
    /// Objects marked for re-query are collected this long before the
    /// request goes out (200 ms), so a burst of config changes costs few
    /// requests.
    pub requery_delay: Duration,
    /// An object Icinga reported as unknown (deleted, or hidden by a
    /// filtered permission) isn't re-queried again for events about it
    /// within this time (10 minutes), unless it is created again.
    pub missing_ttl: Duration,
    /// The applier takes at most this many event lines per batch (5 000).
    pub max_batch: usize,
    /// How long [`crate::CoreHandle::shutdown`] waits for the runtime
    /// thread (5 s).
    pub shutdown_timeout: Duration,
}

impl Default for Tuning {
    fn default() -> Self {
        Self {
            backoff_initial: Duration::from_secs(1),
            backoff_max: Duration::from_mins(1),
            healthy_after: Duration::from_mins(5),
            status_interval: Duration::from_secs(30),
            publish_interval: Duration::from_millis(250),
            requery_delay: Duration::from_millis(200),
            missing_ttl: Duration::from_mins(10),
            max_batch: 5_000,
            shutdown_timeout: Duration::from_secs(5),
        }
    }
}
