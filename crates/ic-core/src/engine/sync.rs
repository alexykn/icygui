//! Keeping the store in sync beyond the event stream, gently:
//!
//! - *Reconcile:* a lean reload (tiers 1–3) after every load at an
//!   adaptive interval that follows the installation's size
//!   ([`reconcile_interval`]: [`PER_OBJECT`] per host and service, at least
//!   [`MIN_INTERVAL`], at most [`MAX_INTERVAL`]: 5 minutes up to about
//!   10 700 objects, 15 at 32 000), doubled after each periodic reconcile
//!   that found nothing the stream missed while the stream was continuous
//!   (at most [`MAX_STREAK`] times, never above [`MAX_INTERVAL`]; a gap, a
//!   reconnect or a finding starts over), or
//!   `General::reconcile_interval_secs` if set (never below 5 minutes from
//!   5 000 objects on); quiet mode makes it at least
//!   `Tuning::quiet_reconcile_interval`. Counted from the end of the last
//!   load, complete or failed (a failed one waits a whole interval), ±10 %
//!   jitter so clients drift apart. Its tier 3 fetches only the problems
//!   not held in full or whose result is older than their last check (in
//!   quiet mode only those without a result: outputs may be stale there).
//! - *Reconnects:* the engine goes live on the objects it has. Only after
//!   a long gap (`Tuning::reload_after_gap` without a line, plus
//!   `Tuning::quiet_status_interval` for a quiet stream, which may be
//!   silent that long and is heard at every quiet status poll: a laptop
//!   that slept, an outage, a stall) does it reload, after a random delay
//!   below `Tuning::reload_jitter`, so clients reconnecting together spread
//!   out. (A quiet reconnect whose next status poll finds Icinga's state
//!   counts changed reloads too.)
//!   After a short gap the events since bring every object checked again,
//!   the status poll catches an Icinga restart, and the next periodic
//!   reconcile the rest: a stream a proxy ends every few minutes doesn't
//!   cost a reload each time. Without `status/query` (no restart
//!   detection) a reconnect reloads, but at most every
//!   [`MIN_INTERVAL`].
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

/// From this many hosts and services on, a fixed reconcile interval is
/// never shorter than [`MIN_INTERVAL`].
const ADAPTIVE_THRESHOLD: usize = 5_000;
/// The adaptive reconcile interval per host and service: a lean reload
/// costs the master about 900 bytes per object, so each client's reconcile
/// costs it about 2 MB a minute whatever the size (28 MB every 15 minutes
/// at 32 000 objects).
pub(super) const PER_OBJECT: Duration = Duration::from_millis(28);
/// The adaptive interval's floor (small installations: cheap, frequent).
pub(super) const MIN_INTERVAL: Duration = Duration::from_mins(5);
/// Its ceiling, also with the stretch for a continuous stream.
pub(super) const MAX_INTERVAL: Duration = Duration::from_hours(1);
/// At most this many doublings for a continuous stream.
pub(super) const MAX_STREAK: u32 = 2;

/// The longest wait between attempts of a failing first load.
pub(super) const LOAD_RETRY_MAX: Duration = Duration::from_mins(15);

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

/// How long a stream may have been silent before a reconnect reloads: a
/// quiet stream (no check results) may be silent for a whole quiet status
/// interval longer.
pub(super) fn long_gap(tuning: &crate::spec::Tuning, quiet: bool) -> Duration {
    if quiet {
        tuning.reload_after_gap + tuning.quiet_status_interval
    } else {
        tuning.reload_after_gap
    }
}

impl Engine {
    /// The periodic reconcile's interval (see the module notes).
    pub(super) fn reconcile_interval(&self) -> Duration {
        let interval = self.tuning.reconcile_interval.unwrap_or_else(|| {
            reconcile_interval(
                self.spec.general.reconcile_interval_secs,
                self.store.object_count(),
                self.reconcile_streak,
            )
        });
        if self.quiet() {
            interval.max(self.tuning.quiet_reconcile_interval)
        } else {
            interval
        }
    }

    /// How long a failing first load waits at most between attempts: as
    /// for a large Icinga (its size isn't known yet), or the fixed setting.
    pub(super) fn load_retry_cap(&self) -> Duration {
        self.tuning
            .reconcile_interval
            .unwrap_or(match self.spec.general.reconcile_interval_secs {
                0 => LOAD_RETRY_MAX,
                secs => reconcile_interval(secs, usize::MAX, 0),
            })
    }

    /// A load of `kind` completed: whether the stream has been continuous
    /// since the previous load ended decides the next interval's stretch.
    /// A periodic reconcile that found nothing the stream missed doubles
    /// it (up to [`MAX_STREAK`] times); one that found something, a reload
    /// (a reconnect after a gap, a restart, the first load) or a stream
    /// that broke since starts over. `Refresh` leaves it.
    pub(super) fn track_continuity(&mut self, kind: LoadKind) {
        let continuous = self
            .continuous_since
            .is_some_and(|since| self.last_load_end.is_some_and(|previous| since <= previous));
        self.reconcile_streak = match kind {
            _ if self.load_found => 0,
            LoadKind::Reconcile if continuous => (self.reconcile_streak + 1).min(MAX_STREAK),
            LoadKind::Refresh if continuous => self.reconcile_streak,
            _ => 0,
        };
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
    /// the events since bring (see the module notes). `was_quiet`: the
    /// stream that broke was quiet.
    pub(super) fn after_reconnect(
        &mut self,
        gap: Duration,
        was_quiet: bool,
        can_poll_status: bool,
    ) {
        let now = Instant::now();
        // A quiet stream may be silent for a whole quiet status interval
        // (each quiet poll that finds nothing missed counts as hearing it).
        let long_gap = long_gap(&self.tuning, was_quiet);
        if std::mem::take(&mut self.reload_now) {
            // The user asked for it.
            self.reload_full = true;
            self.notifications_current = false;
            self.reload_at = Some(now);
        } else if gap >= long_gap {
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
                .map_or(now, |start| (start + MIN_INTERVAL).max(now))
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
    /// current). In quiet mode a periodic reconcile fetches only problems
    /// without any result (outputs may be stale while quiet; waking up
    /// refreshes them).
    pub(super) fn problem_details(&self, kind: LoadKind) -> Vec<ObjectKey> {
        let all = matches!(kind, LoadKind::First | LoadKind::Reload);
        let quiet = kind == LoadKind::Reconcile && self.quiet();
        self.store
            .services()
            .iter()
            .filter(|(_, service)| service.is_problem())
            .filter(|(key, service)| {
                if all {
                    true
                } else if quiet {
                    service.check.result.is_none()
                } else {
                    !self.store.is_full(key) || self.store.result_is_stale(key)
                }
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
    /// `watchdog_interval`; never in quiet mode (no check results move the
    /// deadlines).
    pub(super) fn sweep_at(&self, now: Instant) -> Option<Instant> {
        if self.phase != Phase::Live || self.load.is_some() || self.quiet() || self.stream_quiet() {
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

    /// `Command::Hydrate` (the rows on screen): fetches in full the
    /// services among `keys` that are only known lean, and, after quiet
    /// mode, the objects whose result a check may have replaced meanwhile
    /// (moved ahead of the wake-up refresh's background lane), deduplicated
    /// against what is queued or in flight, in rounds of at most 1 000
    /// names and requests of 200. Hosts always load in full.
    pub(super) fn hydrate(&mut self, keys: Vec<ObjectKey>) {
        if self.quiet() {
            tracing::debug!(count = keys.len(), "quiet mode: no hydration");
            return;
        }
        let wanted: Vec<ObjectKey> =
            keys.into_iter()
                .filter(|key| match key {
                    ObjectKey::Host { name } => self.store.hosts().get(name).is_some_and(|host| {
                        host.check.result.is_some() && !self.result_current(key)
                    }),
                    ObjectKey::Service { key: service } => {
                        self.store.services().get(service).is_some_and(|found| {
                            !self.store.is_full(service)
                                || (found.check.result.is_some() && !self.result_current(key))
                        })
                    }
                })
                .collect();
        if wanted.is_empty() {
            return;
        }
        let added = self.fetch.mark_full(wanted, Instant::now());
        self.note_updating();
        tracing::debug!(added, "hydrating");
    }
}

/// The reconcile interval for the setting `secs` (0: adaptive) with
/// `objects` hosts and services, after `streak` periodic reconciles that
/// found nothing a continuous stream missed.
///
/// Adaptive: `objects ×` [`PER_OBJECT`] within [`MIN_INTERVAL`] and
/// [`MAX_INTERVAL`], doubled per streak (at most [`MAX_STREAK`] times),
/// never above [`MAX_INTERVAL`]. A fixed interval is at least
/// `ic_config::MIN_RECONCILE_INTERVAL_SECS`, and at least
/// [`MIN_INTERVAL`] from [`ADAPTIVE_THRESHOLD`] objects on (a lean reload
/// of 30 000 services every minute would cost the master about 34 MB a
/// minute per client); it doesn't stretch (the user chose it).
pub(super) fn reconcile_interval(secs: u32, objects: usize, streak: u32) -> Duration {
    match secs {
        0 => {
            let objects = u32::try_from(objects).unwrap_or(u32::MAX);
            let base = PER_OBJECT
                .saturating_mul(objects)
                .clamp(MIN_INTERVAL, MAX_INTERVAL);
            base.saturating_mul(1 << streak.min(MAX_STREAK))
                .min(MAX_INTERVAL)
        }
        secs => {
            let fixed =
                Duration::from_secs(u64::from(secs.max(ic_config::MIN_RECONCILE_INTERVAL_SECS)));
            if objects < ADAPTIVE_THRESHOLD {
                fixed
            } else {
                fixed.max(MIN_INTERVAL)
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
    fn the_reconcile_interval_follows_the_size() {
        let minutes = |objects, streak| reconcile_interval(0, objects, streak).as_secs_f64() / 60.0;
        // Small installations: cheap, every 5 minutes.
        assert!((minutes(0, 0) - 5.0).abs() < 1e-9);
        assert!((minutes(2_000, 0) - 5.0).abs() < 1e-9);
        assert!((minutes(10_000, 0) - 5.0).abs() < 1e-9);
        // Then proportional, without steps: about 15 minutes at the
        // production size (2 000 hosts, 30 000 services).
        assert!((minutes(16_000, 0) - 7.466).abs() < 0.01);
        assert!((minutes(32_000, 0) - 14.933).abs() < 0.01);
        assert!(minutes(32_001, 0) > minutes(32_000, 0));
        assert!((minutes(500_000, 0) - 60.0).abs() < 1e-9, "at most an hour");
        // A continuous stream stretches it, up to an hour.
        assert!((minutes(32_000, 1) - 29.866).abs() < 0.01);
        assert!((minutes(32_000, 2) - 59.733).abs() < 0.01);
        assert!((minutes(32_000, 9) - 59.733).abs() < 0.01, "twice at most");
        assert!((minutes(2_000, 2) - 20.0).abs() < 1e-9);
        assert!((minutes(64_000, 2) - 60.0).abs() < 1e-9);
        // A setting overrides it, but never below the minimum, and never
        // below 5 minutes for a large Icinga; it doesn't stretch.
        assert_eq!(reconcile_interval(120, 4_999, 0), Duration::from_mins(2));
        assert_eq!(reconcile_interval(120, 32_000, 2), Duration::from_mins(5));
        assert_eq!(
            reconcile_interval(1_800, 32_000, 1),
            Duration::from_mins(30)
        );
        assert_eq!(reconcile_interval(10, 10, 0), Duration::from_mins(1));
    }
}
