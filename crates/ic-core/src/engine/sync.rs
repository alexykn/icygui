//! Keeping the store in sync beyond the event stream, gently:
//!
//! - *Reconcile:* a lean reload (tiers 1–3) after every load at an
//!   adaptive interval (5 minutes below 5 000 objects, 15 above, ±10 %
//!   jitter so clients drift apart), or `General::reconcile_interval_secs`
//!   if set. After a reconnect the engine goes live on the objects it has
//!   and reloads after a random delay below `Tuning::reload_jitter`, so
//!   clients reconnecting together after an Icinga restart spread out.
//!   Never a periodic full-attribute reload.
//! - *Freshness watchdog* (`watchdog.rs`): overdue objects are re-queried
//!   by name.
//! - *Hydration:* `Command::Hydrate` fetches lean services in full, by name,
//!   deduplicated.
//! - *`Command::UpdateGeneral`:* a new reconcile interval or event log
//!   retention applies at once.

use std::time::Duration;

use ic_api::NAMES_PER_REQUEST;
use ic_model::ObjectKey;
use tokio::time::Instant;

use super::{Engine, Phase};

/// Below this many hosts and services the adaptive reconcile runs every
/// [`SMALL_INTERVAL`], above it every [`LARGE_INTERVAL`].
const ADAPTIVE_THRESHOLD: usize = 5_000;
const SMALL_INTERVAL: Duration = Duration::from_mins(5);
const LARGE_INTERVAL: Duration = Duration::from_mins(15);

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
    /// the last load.
    pub(super) fn schedule_reconcile(&mut self) {
        let base = self.last_load_done.unwrap_or_else(Instant::now);
        let interval = self
            .reconcile_interval()
            .mul_f64(0.9 + 0.2 * fastrand::f64());
        self.reconcile_at = Some(base + interval);
    }

    /// When a reload is due (after a reconnect, or the periodic
    /// reconcile): only while live and no load runs.
    pub(super) fn reload_due(&self) -> Option<Instant> {
        if self.phase != Phase::Live || self.load.is_some() {
            return None;
        }
        self.reload_at.into_iter().chain(self.reconcile_at).min()
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
