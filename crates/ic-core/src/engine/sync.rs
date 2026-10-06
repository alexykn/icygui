//! Keeping the store in sync beyond the event stream, gently:
//!
//! - *Reconcile:* a lean reload (tiers 1–3) after every load at an
//!   adaptive interval (5 minutes below 5 000 objects, 15 above, ±10 %
//!   jitter so clients drift apart), or `General::reconcile_interval_secs`
//!   if set (never below 5 minutes from 5 000 objects on), counted from
//!   the end of the last load, complete or failed (a failed one waits a
//!   whole interval). Its tier 3 fetches only the problems not held in
//!   full or whose result is older than their last check.
//! - *Reconnects:* the engine goes live on the objects it has. Only after
//!   a long gap (`Tuning::reload_after_gap` without a line: a laptop that
//!   slept, an outage, a stall) does it reload, after a random delay below
//!   `Tuning::reload_jitter`, so clients reconnecting together spread out.
//!   After a short gap the events since bring every object checked again,
//!   the status poll catches an Icinga restart, and the next periodic
//!   reconcile the rest: a stream a proxy ends every few minutes doesn't
//!   cost a reload each time. Without `status/query` (no restart
//!   detection) a reconnect reloads, but at most every
//!   [`SMALL_INTERVAL`].
//! - *Restarts* ([`Restarts`]): a node reporting another `program_start`
//!   than before restarted; another node answering (an HA zone behind a
//!   load balancer) didn't. A restart reloads at most every
//!   `Tuning::reload_spacing`, and not at all if a load started since the
//!   stream (re)connected and since the node was last seen: a deploy
//!   restarts both masters of an HA zone, and one reload brings it.
//! - *`Refresh`* reloads what isn't current (like a reconcile) at once,
//!   then spaced: `reload_spacing` after the previous reload, 4× that after
//!   the second, 10× after the third and later, until the user rests
//!   for 20×. Presses meanwhile coalesce. Never a periodic full-attribute
//!   reload.
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
/// [`SMALL_INTERVAL`], above it every [`LARGE_INTERVAL`]; a fixed interval
/// is never shorter than [`SMALL_INTERVAL`] above it.
const ADAPTIVE_THRESHOLD: usize = 5_000;
const SMALL_INTERVAL: Duration = Duration::from_mins(5);
const LARGE_INTERVAL: Duration = Duration::from_mins(15);

/// The longest wait between attempts of a failing first load.
pub(super) const LOAD_RETRY_MAX: Duration = LARGE_INTERVAL;

/// `Refresh` presses in a row wait this many `Tuning::reload_spacing`
/// after the previous reload: several on-call engineers pressing Refresh
/// during an incident cost the master a few lean reloads, not two a
/// minute each.
const USER_SPACING: [u32; 4] = [1, 1, 4, 10];
/// A `Refresh` this many `reload_spacing` after the last one starts the
/// spacing over.
const USER_RESET: u32 = 20;

/// Why a reload is asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ReloadCause {
    /// `Refresh` while connected: what isn't current, spaced more and
    /// more.
    User,
    /// An Icinga restart: everything (its config may have changed).
    Restart,
    /// A reconnect after a long gap: everything.
    Reconnect,
}

/// Whether a load covers a restart Icinga reported for a node last seen
/// (with its old start time) at `seen`: it started after the stream
/// (re)connected at `connected` and after `seen`. The restart of the node
/// behind the stream ended the stream; the restart of another node (the
/// other master of an HA zone, restarted with it by a deploy) changed
/// nothing the stream's node doesn't serve.
pub(super) fn reload_covers_restart(
    last_load_start: Option<Instant>,
    connected: Instant,
    seen: Instant,
) -> bool {
    last_load_start.is_some_and(|start| start > connected && start > seen)
}

/// The nodes [`Restarts`] remembers (an HA zone has two masters; more
/// distinct node names start over).
const MAX_NODES: usize = 16;

/// The `program_start` each Icinga node reported (`/v1/status`), to tell a
/// restart from another node answering: behind a load balancer or a
/// round-robin DNS name each status poll may reach either master of an HA
/// zone, and each has its own start time.
#[derive(Debug, Default)]
pub(super) struct Restarts {
    /// Per node: its start time, and when it was last seen with it.
    starts: HashMap<String, (Timestamp, Instant)>,
}

impl Restarts {
    /// Notes `status`, seen at `now`. If it shows a node seen before with
    /// another start time (it restarted): when it was last seen with the
    /// old one.
    pub(super) fn observe(&mut self, status: &InstanceStatus, now: Instant) -> Option<Instant> {
        if self.starts.len() >= MAX_NODES && !self.starts.contains_key(&status.node_name) {
            self.starts.clear();
        }
        self.starts
            .insert(status.node_name.clone(), (status.program_start, now))
            .filter(|(previous, _)| *previous != status.program_start)
            .map(|(_, seen)| seen)
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

    /// How long a failing first load waits at most between attempts: the
    /// reconcile interval as for a large Icinga (its size isn't known yet).
    pub(super) fn load_retry_cap(&self) -> Duration {
        self.tuning.reconcile_interval.unwrap_or_else(|| {
            reconcile_interval(self.spec.general.reconcile_interval_secs, usize::MAX)
        })
    }

    /// How long the stream was silent: since it last delivered lines (or
    /// the session went live), by the monotonic or the wall clock,
    /// whichever says longer (the monotonic clock may stand still while
    /// the computer sleeps).
    pub(super) fn stream_gap(&self) -> Duration {
        let Some((instant, wall)) = self.last_heard else {
            return Duration::MAX;
        };
        let wall = Duration::try_from_secs_f64(
            self.ports.clock.now().as_unix_seconds() - wall.as_unix_seconds(),
        )
        .unwrap_or_default();
        instant.elapsed().max(wall)
    }

    /// The session reconnected (live on the objects it has) after the
    /// stream was silent for `gap`: reload if it may have missed more than
    /// the events since bring (see the module notes).
    pub(super) fn after_reconnect(&mut self, gap: Duration, can_poll_status: bool) {
        let now = Instant::now();
        if std::mem::take(&mut self.reload_now) {
            // The user asked for it.
            self.reload_full = true;
            self.notifications_current = false;
            self.reload_at = Some(now);
        } else if gap >= self.tuning.reload_after_gap {
            tracing::debug!(?gap, "reconnected after a long gap; reloading");
            self.request_reload(ReloadCause::Reconnect);
        } else if can_poll_status {
            tracing::debug!(
                ?gap,
                "reconnected after a short gap; the next reconcile catches up"
            );
            self.schedule_reconcile();
        } else {
            // A restart can't be told: reload, but not often.
            let jitter = self.tuning.reload_jitter.mul_f64(fastrand::f64());
            let at = self
                .last_load_start
                .map_or(now, |start| (start + SMALL_INTERVAL).max(now))
                + jitter;
            self.reload_full = true;
            self.notifications_current = false;
            self.reload_at = Some(self.reload_at.map_or(at, |pending| pending.min(at)));
            self.schedule_reconcile();
        }
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

    /// The load due at `now`, if any: a pending reload (or `Refresh`)
    /// runs before a reconcile, and a reconcile due first serves it.
    pub(super) fn due_load(&self, now: Instant) -> Option<LoadKind> {
        if self.reload_due().is_none_or(|at| at > now) {
            return None;
        }
        Some(match self.reload_at {
            Some(_) if self.reload_full => LoadKind::Reload,
            Some(_) => LoadKind::Refresh,
            None => LoadKind::Reconcile,
        })
    }

    /// A reload: after a restart or a reconnect everything (Icinga's
    /// notifications too) at most every `Tuning::reload_spacing`; for
    /// `Refresh` what isn't current, spaced more the more often it is
    /// pressed (see the module notes). More requests meanwhile coalesce;
    /// a load in flight serves them (it brings Icinga's notifications
    /// after it if they may be stale).
    pub(super) fn request_reload(&mut self, cause: ReloadCause) {
        if cause != ReloadCause::User {
            self.notifications_current = false;
        }
        if self.load.is_some() {
            tracing::debug!(?cause, "reload: a load is already running");
            return;
        }
        let now = Instant::now();
        let base = self.tuning.reload_spacing;
        let spacing = match cause {
            ReloadCause::User => {
                if self
                    .last_user_reload
                    .is_some_and(|last| now.duration_since(last) >= base * USER_RESET)
                {
                    self.user_reloads = 0;
                }
                let step = usize::try_from(self.user_reloads)
                    .unwrap_or(usize::MAX)
                    .min(USER_SPACING.len() - 1);
                self.reload_by_user = true;
                base * USER_SPACING[step]
            }
            ReloadCause::Restart | ReloadCause::Reconnect => {
                self.reload_full = true;
                base
            }
        };
        let mut at = self
            .last_reload
            .map_or(now, |last| (last + spacing).max(now));
        if cause == ReloadCause::Reconnect {
            at += self.tuning.reload_jitter.mul_f64(fastrand::f64());
        }
        if at > now + self.tuning.reload_jitter {
            tracing::debug!(?cause, wait = ?(at - now), "reload: one ran moments ago; the next follows later");
        }
        self.reload_at = Some(self.reload_at.map_or(at, |pending| pending.min(at)));
    }

    /// The problem services whose details (`Full`) tier 3 fetches: every
    /// one for a reload, else (a reconcile, `Refresh`) only those the store
    /// doesn't hold in full with a current result (events keep them
    /// current).
    pub(super) fn problem_details(&self, kind: LoadKind) -> Vec<ObjectKey> {
        let all = matches!(kind, LoadKind::First | LoadKind::Reload);
        self.store
            .services()
            .iter()
            .filter(|(_, service)| service.is_problem())
            .filter(|(key, _)| all || !self.store.is_full(key) || self.store.result_is_stale(key))
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
/// `objects` hosts and services. A fixed interval is at least
/// `ic_config::MIN_RECONCILE_INTERVAL_SECS`, and at least
/// [`SMALL_INTERVAL`] from [`ADAPTIVE_THRESHOLD`] objects on: a lean
/// reload of 30 000 services every minute would cost the master about
/// 34 MB a minute per client.
fn reconcile_interval(secs: u32, objects: usize) -> Duration {
    match secs {
        0 if objects < ADAPTIVE_THRESHOLD => SMALL_INTERVAL,
        0 => LARGE_INTERVAL,
        secs => {
            let fixed =
                Duration::from_secs(u64::from(secs.max(ic_config::MIN_RECONCILE_INTERVAL_SECS)));
            if objects < ADAPTIVE_THRESHOLD {
                fixed
            } else {
                fixed.max(SMALL_INTERVAL)
            }
        }
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
        let now = Instant::now();
        let observe = |restarts: &mut Restarts, node: &str, start: f64| {
            restarts.observe(&status(node, start), now)
        };
        assert!(
            observe(&mut restarts, "master-1", 100.0).is_none(),
            "first sight"
        );
        assert!(observe(&mut restarts, "master-1", 100.0).is_none());
        // An HA zone behind a load balancer: the masters alternate.
        for _ in 0..10 {
            assert!(observe(&mut restarts, "master-2", 200.0).is_none());
            assert!(observe(&mut restarts, "master-1", 100.0).is_none());
        }
        // One of them restarts.
        assert!(observe(&mut restarts, "master-2", 300.0).is_some());
        assert!(observe(&mut restarts, "master-1", 100.0).is_none());
        assert!(observe(&mut restarts, "master-2", 300.0).is_none());
        // Bounded: many distinct names start over.
        for index in 0..MAX_NODES * 2 {
            assert!(observe(&mut restarts, &format!("node-{index}"), 1.0).is_none());
        }
        assert!(restarts.starts.len() <= MAX_NODES);
    }

    #[test]
    fn a_deploy_restarting_both_masters_reloads_once() {
        let start = Instant::now();
        let at = |seconds: u64| start + Duration::from_secs(seconds);
        let mut restarts = Restarts::default();
        // Both masters seen before the deploy.
        restarts.observe(&status("master-1", 100.0), at(0));
        restarts.observe(&status("master-2", 200.0), at(30));
        // The deploy restarts both; the stream reconnects at 70 s, the
        // next poll reaches master-1: no load since the reconnect.
        let seen = restarts
            .observe(&status("master-1", 1_000.0), at(90))
            .unwrap();
        assert_eq!(seen, at(0));
        let mut last_load = Some(at(10));
        assert!(!reload_covers_restart(last_load, at(70), seen));
        // It reloads at 90 s; the poll after reaches master-2.
        last_load = Some(at(90));
        let seen = restarts
            .observe(&status("master-2", 1_001.0), at(120))
            .unwrap();
        assert!(reload_covers_restart(last_load, at(70), seen), "covered");
        // Later, master-2 alone restarts again (after the last load): the
        // stream on master-1 lost nothing, but there was no load since it
        // was last seen, so it reloads (the safe side).
        let seen = restarts
            .observe(&status("master-2", 2_000.0), at(600))
            .unwrap();
        assert!(!reload_covers_restart(last_load, at(70), seen));
    }

    #[test]
    fn the_reconcile_interval_adapts_to_the_size() {
        assert_eq!(reconcile_interval(0, 0), Duration::from_mins(5));
        assert_eq!(reconcile_interval(0, 4_999), Duration::from_mins(5));
        assert_eq!(reconcile_interval(0, 5_000), Duration::from_mins(15));
        assert_eq!(reconcile_interval(0, 32_000), Duration::from_mins(15));
        // A setting overrides it, but never below the minimum, and never
        // below 5 minutes for a large Icinga.
        assert_eq!(reconcile_interval(120, 4_999), Duration::from_mins(2));
        assert_eq!(reconcile_interval(120, 32_000), Duration::from_mins(5));
        assert_eq!(reconcile_interval(1_800, 32_000), Duration::from_mins(30));
        assert_eq!(reconcile_interval(10, 10), Duration::from_mins(1));
    }
}
