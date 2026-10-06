//! Keeping the store in sync beyond the event stream, gently:
//!
//! - *Reconcile:* a lean reload (tiers 1–3) after every load at an
//!   adaptive interval (5 minutes below 5 000 objects, 15 above, ±10 %
//!   jitter so clients drift apart), or `General::reconcile_interval_secs`
//!   if set, counted from the end of the last load, complete or failed (a
//!   failed one waits a whole interval). Its tier 3 fetches only the
//!   problems not held in full or whose result is older than their last
//!   check. After a reconnect the engine goes live on the objects it has
//!   and reloads after a random delay below `Tuning::reload_jitter`, so
//!   clients reconnecting together after an Icinga restart spread out.
//!   `Refresh` and a restart reload at once, but at most once per
//!   [`RELOAD_SPACING`]; more of them meanwhile coalesce into one reload
//!   when it is over. Never a periodic full-attribute reload.
//! - *Restarts* ([`Restarts`]): a node reporting another `program_start`
//!   than before restarted; another node answering (an HA zone behind a
//!   load balancer) didn't.
//! - *Freshness watchdog* (`watchdog.rs`): overdue objects are re-queried
//!   by name.
//! - *Hydration:* `Command::Hydrate` fetches lean services in full, by name,
//!   deduplicated.
//! - *`Command::UpdateGeneral`:* a new reconcile interval or event log
//!   retention applies at once.

use std::collections::HashMap;
use std::time::Duration;

use ic_api::NAMES_PER_REQUEST;
use ic_model::{InstanceStatus, ObjectKey, Timestamp};
use tokio::time::Instant;

use super::{Engine, LoadKind, Phase};

/// Below this many hosts and services the adaptive reconcile runs every
/// [`SMALL_INTERVAL`], above it every [`LARGE_INTERVAL`].
const ADAPTIVE_THRESHOLD: usize = 5_000;
const SMALL_INTERVAL: Duration = Duration::from_mins(5);
const LARGE_INTERVAL: Duration = Duration::from_mins(15);

/// Reloads asked for by `Refresh` or a restart start at most this often:
/// several on-call engineers pressing Refresh during an incident cost the
/// master one lean reload per client every half minute, not one every few
/// seconds.
pub(super) const RELOAD_SPACING: Duration = Duration::from_secs(30);

/// The nodes [`Restarts`] remembers (an HA zone has two masters; more
/// distinct node names start over).
const MAX_NODES: usize = 16;

/// The `program_start` each Icinga node reported (`/v1/status`), to tell a
/// restart from another node answering: behind a load balancer or a
/// round-robin DNS name each status poll may reach either master of an HA
/// zone, and each has its own start time.
#[derive(Debug, Default)]
pub(super) struct Restarts {
    starts: HashMap<String, Timestamp>,
}

impl Restarts {
    /// Notes `status`; whether it shows a node seen before with another
    /// start time (it restarted).
    pub(super) fn observe(&mut self, status: &InstanceStatus) -> bool {
        if self.starts.len() >= MAX_NODES && !self.starts.contains_key(&status.node_name) {
            self.starts.clear();
        }
        self.starts
            .insert(status.node_name.clone(), status.program_start)
            .is_some_and(|previous| previous != status.program_start)
    }
}

impl Engine {
    /// The periodic reconcile's interval (see the module notes).
    pub(super) fn reconcile_interval(&self) -> Duration {
        self.tuning.reconcile_interval.unwrap_or_else(|| {
            reconcile_interval(
                self.spec.general.reconcile_interval_secs,
                self.store.object_count(),
            )
        })
    }

    /// Schedules the next periodic reconcile, an interval (±10 %) after
    /// the last load ended, complete or failed.
    pub(super) fn schedule_reconcile(&mut self) {
        let base = self.last_load_end.unwrap_or_else(Instant::now);
        let interval = self
            .reconcile_interval()
            .mul_f64(0.9 + 0.2 * fastrand::f64());
        self.reconcile_at = Some(base + interval);
    }

    /// When a reload or the periodic reconcile is due: only while live and
    /// no load runs.
    pub(super) fn reload_due(&self) -> Option<Instant> {
        if self.phase != Phase::Live || self.load.is_some() {
            return None;
        }
        self.reload_at.into_iter().chain(self.reconcile_at).min()
    }

    /// The load due at `now`, if any: a reload before a reconcile.
    pub(super) fn due_load(&self, now: Instant) -> Option<LoadKind> {
        if self.reload_due().is_none_or(|at| at > now) {
            return None;
        }
        Some(if self.reload_at.is_some_and(|at| at <= now) {
            LoadKind::Reload
        } else {
            LoadKind::Reconcile
        })
    }

    /// A reload (`Refresh`, a restart): now, or once [`RELOAD_SPACING`]
    /// passed since the last one started; more requests meanwhile coalesce.
    /// A load in flight brings everything anyway (and Icinga's
    /// notifications after it).
    pub(super) fn request_reload(&mut self) {
        self.notifications_current = false;
        if self.load.is_some() {
            tracing::debug!("reload: a load is already running");
            return;
        }
        let now = Instant::now();
        let at = self
            .last_reload
            .map_or(now, |last| (last + RELOAD_SPACING).max(now));
        if at > now {
            tracing::debug!(?at, "reload: one ran moments ago; the next follows shortly");
        }
        self.reload_at = Some(self.reload_at.map_or(at, |pending| pending.min(at)));
    }

    /// The problem services whose details (`Full`) tier 3 fetches: every
    /// one, except for a reconcile, which skips those the store holds in
    /// full with a current result (events keep them current).
    pub(super) fn problem_details(&self, kind: LoadKind) -> Vec<ObjectKey> {
        self.store
            .services()
            .iter()
            .filter(|(_, service)| service.is_problem())
            .filter(|(key, _)| {
                kind != LoadKind::Reconcile
                    || !self.store.is_full(key)
                    || self.store.result_is_stale(key)
            })
            .map(|(_, service)| service.object_key())
            .collect()
    }

    /// `Command::UpdateGeneral`: a new reconcile interval applies from the
    /// last load.
    pub(super) fn update_general(&mut self, general: ic_config::General) {
        let interval_changed =
            self.spec.general.reconcile_interval_secs != general.reconcile_interval_secs;
        let retention_changed =
            self.spec.general.event_log_retention_hours != general.event_log_retention_hours;
        self.spec.general = general;
        if interval_changed && self.reconcile_at.is_some() {
            self.schedule_reconcile();
        }
        if retention_changed {
            // A shorter retention applies at once.
            self.prune_at = Instant::now();
        }
    }

    // --- the freshness watchdog ----------------------------------------------------

    /// When the watchdog should look for overdue objects: while live and
    /// no load runs (a load brings everything anyway), at most every
    /// `watchdog_interval`.
    pub(super) fn sweep_at(&self, now: Instant) -> Option<Instant> {
        if self.phase != Phase::Live || self.load.is_some() {
            return None;
        }
        let at = self.watchdog.due(now)?;
        Some(match self.last_sweep {
            Some(last) => at.max(last + self.tuning.watchdog_interval),
            None => at,
        })
    }

    /// Re-queries up to one request's worth of overdue objects.
    pub(super) fn sweep(&mut self, now: Instant) {
        self.last_sweep = Some(now);
        // Deadlines follow the store at every snapshot; the events applied
        // since (the one that moved Icinga's clock, say) count too.
        let changes = self.store.changes();
        self.watchdog
            .update(&self.store, &changes.objects, changes.all);
        let keys = self.watchdog.sweep(NAMES_PER_REQUEST);
        if keys.is_empty() {
            return;
        }
        tracing::debug!(count = keys.len(), "re-querying overdue objects");
        let refused: Vec<ObjectKey> = keys
            .into_iter()
            .filter(|key| !self.fetch.mark(key.clone(), now))
            .collect();
        // Recently reported missing: no answer will come.
        self.watchdog.failed(&refused);
    }

    // --- hydration ----------------------------------------------------------------------

    /// `Command::Hydrate`: fetches the services among `keys` that are only
    /// known lean in full (hosts always load in full), deduplicated against
    /// what is queued or in flight, in rounds of at most 1 000 names and
    /// requests of 200.
    pub(super) fn hydrate(&mut self, keys: Vec<ObjectKey>) {
        let wanted: Vec<ObjectKey> = keys
            .into_iter()
            .filter(|key| match key {
                ObjectKey::Host { .. } => false,
                ObjectKey::Service { key } => {
                    self.store.services().contains_key(key) && !self.store.is_full(key)
                }
            })
            .collect();
        if wanted.is_empty() {
            return;
        }
        let added = self.fetch.mark_full(wanted, Instant::now());
        tracing::debug!(added, "hydrating");
    }
}

/// The reconcile interval for the setting `secs` (0: adaptive) with
/// `objects` hosts and services.
fn reconcile_interval(secs: u32, objects: usize) -> Duration {
    match secs {
        0 if objects < ADAPTIVE_THRESHOLD => SMALL_INTERVAL,
        0 => LARGE_INTERVAL,
        secs => Duration::from_secs(u64::from(secs.max(ic_config::MIN_RECONCILE_INTERVAL_SECS))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(node: &str, program_start: f64) -> InstanceStatus {
        InstanceStatus {
            node_name: node.to_owned(),
            program_start: Timestamp::from_unix_seconds(program_start),
            ..InstanceStatus::default()
        }
    }

    #[test]
    fn restarts_are_told_from_another_node_answering() {
        let mut restarts = Restarts::default();
        assert!(!restarts.observe(&status("master-1", 100.0)), "first sight");
        assert!(!restarts.observe(&status("master-1", 100.0)));
        // An HA zone behind a load balancer: the masters alternate.
        for _ in 0..10 {
            assert!(!restarts.observe(&status("master-2", 200.0)));
            assert!(!restarts.observe(&status("master-1", 100.0)));
        }
        // One of them restarts.
        assert!(restarts.observe(&status("master-2", 300.0)));
        assert!(!restarts.observe(&status("master-1", 100.0)));
        assert!(!restarts.observe(&status("master-2", 300.0)));
        // Bounded: many distinct names start over.
        for index in 0..MAX_NODES * 2 {
            assert!(!restarts.observe(&status(&format!("node-{index}"), 1.0)));
        }
        assert!(restarts.starts.len() <= MAX_NODES);
    }

    #[test]
    fn the_reconcile_interval_adapts_to_the_size() {
        assert_eq!(reconcile_interval(0, 0), Duration::from_mins(5));
        assert_eq!(reconcile_interval(0, 4_999), Duration::from_mins(5));
        assert_eq!(reconcile_interval(0, 5_000), Duration::from_mins(15));
        assert_eq!(reconcile_interval(0, 32_000), Duration::from_mins(15));
        // A setting overrides it, but never below the minimum.
        assert_eq!(reconcile_interval(120, 32_000), Duration::from_mins(2));
        assert_eq!(reconcile_interval(10, 10), Duration::from_mins(1));
    }
}
