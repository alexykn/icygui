//! Publishing snapshots, with the dashboards evaluated for them on a
//! blocking thread, and dashboard previews.
//!
//! A snapshot goes out at most every `publish_interval` while things
//! change (every `background_publish_interval` while the environment
//! isn't on screen or is quiet, and no rule input waits), and right away
//! after load tiers, actions and the opened object's answer. When objects
//! changed, the dashboards are brought up to date first: the engine cuts
//! the snapshot (cheap: shared maps), hands it and the dashboards' state to
//! a blocking thread, and emits it with the results once they are back.
//! One evaluation runs at a time; changes meanwhile wait for the next
//! snapshot, and a snapshot asked for right away goes out as soon as the
//! evaluation is back. The event stream keeps being applied all along.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use futures::channel::oneshot;
use ic_config::View;
use ic_model::Timestamp;
use tokio::time::Instant;

use super::{Engine, Internal};
use crate::command::CoreEvent;
use crate::dashboards::{self, Dashboards, Data, TIME_REFRESH};
use crate::snapshot::{DashboardResult, Snapshot};

/// The dashboard editor's previews: one evaluates at a time, the newest
/// request waits; a waiting one replaced by a newer request is dropped
/// (its receiver sees `Canceled`).
#[derive(Debug, Default)]
pub(super) struct Previews {
    running: bool,
    waiting: Option<(Vec<View>, oneshot::Sender<DashboardResult>)>,
}

impl Engine {
    /// Whether a snapshot should go out.
    fn needs_publish(&self) -> bool {
        self.store.has_changes()
            || self.dashboards_configured
            || self.dashboards_resume
            || self.watchdog.has_changes()
            || self.notify.has_pending()
            || self.updating_changed
            || self.mode_changed
            || self.beats.changed
            || self.beats.news
            || self.trouble.news
            || self.streams_wait()
    }

    /// When the next throttled snapshot (or the time-dependent dashboards'
    /// refresh) is due; `None` while an evaluation runs (its return
    /// publishes).
    pub(super) fn publish_at(&self, now: Instant) -> Option<Instant> {
        self.dashboards.as_ref()?;
        // Nobody looks at the lists of an environment that isn't on screen
        // or is quiet (the window is hidden): its snapshots go out less
        // often (the tray reads them), unless notifications wait for them.
        let interval = if (self.active && !self.quiet()) || self.notify.has_pending() {
            self.tuning.publish_interval
        } else {
            self.tuning.background_publish_interval
        };
        let changes = self
            .needs_publish()
            .then(|| self.last_publish.map_or(now, |last| last + interval));
        let time = self
            .time_dependent
            .then(|| self.time_refreshed + TIME_REFRESH);
        changes.into_iter().chain(time).min()
    }

    /// Publishes now if anything changed.
    pub(super) fn publish_changes(&mut self) {
        if self.needs_publish() {
            self.publish();
        }
    }

    /// Publishes a snapshot now: at once if no dashboard needs evaluating,
    /// else when the evaluation is back.
    pub(super) fn publish(&mut self) {
        let Some(mut dashboards) = self.dashboards.take() else {
            self.publish_soon = true;
            return;
        };
        self.publish_soon = false;
        let changes = self.store.take_changes();
        self.watchdog
            .update(&self.store, &changes.objects, changes.all);
        let late_changed = self.watchdog.take_changed();
        if late_changed {
            // The alerts quote how many checks are late: worked out again
            // from the late flags this snapshot carries, so the health
            // page's alert and its late tile say the same number.
            self.assess_trouble(Instant::now());
        }
        let reconfigured = std::mem::take(&mut self.dashboards_configured);
        if reconfigured {
            dashboards.configure(&self.spec.environment, self.spec.hide_handled);
        }
        // Quiet mode keeps only the memberships notifications depend on
        // current; rows, summaries and the other dashboards come back when
        // it ends.
        let resumed = std::mem::take(&mut self.dashboards_resume);
        dashboards.set_scope(if self.quiet() {
            dashboards::Scope::Quiet(self.notify.decisive_dashboards())
        } else {
            dashboards::Scope::All
        });
        // New events: the snapshot carries them (the cluster section's
        // events), and the event stream views show them.
        let new_events = std::mem::take(&mut self.events_changed);
        // The stream's mode, the objects being updated and new events are
        // news of their own: such a snapshot goes out even if nothing else
        // changed.
        self.refresh_beats();
        let news = std::mem::take(&mut self.updating_changed)
            | std::mem::take(&mut self.mode_changed)
            | std::mem::take(&mut self.beats.news)
            | std::mem::take(&mut self.trouble.news)
            | new_events;
        let refresh_time = self.time_dependent && self.time_refreshed.elapsed() >= TIME_REFRESH;
        let events = new_events && dashboards.has_streams();
        let mut snapshot = self.store.snapshot(
            0,
            self.ports.clock.now(),
            Arc::clone(&self.dashboard_results),
            self.watchdog.late(),
        );
        snapshot.quiet = self.stream_quiet();
        snapshot.updating = Arc::clone(&self.updating);
        snapshot.events = Arc::clone(&self.recent_events);
        snapshot.heartbeats = Arc::clone(self.beats.published());
        snapshot.trouble = Arc::clone(self.trouble.published());
        let evaluate = reconfigured
            || resumed
            || events
            || refresh_time
            || changes.all
            || changes.groups
            || !changes.objects.is_empty();
        if !evaluate {
            self.dashboards = Some(dashboards);
            self.emit_snapshot(snapshot);
            // The memberships are current: judge the changes now.
            self.judge(false);
            return;
        }
        // The rule inputs so far are judged with this evaluation's
        // memberships.
        self.notify.begin_evaluation();
        if refresh_time {
            self.time_refreshed = Instant::now();
        }
        let quiet = !changes.any && !reconfigured && !late_changed && !news && !resumed;
        let data = Data {
            hosts: Arc::clone(&snapshot.hosts),
            services: Arc::clone(&snapshot.services),
            host_groups: Arc::clone(&snapshot.host_groups),
            service_groups: Arc::clone(&snapshot.service_groups),
            now: self.evaluation_time(),
            events: Arc::clone(&self.recent_events),
            excluded: Arc::clone(&snapshot.excluded),
        };
        let tx = self.internal_tx.clone();
        let cancel = Arc::clone(&self.cancel);
        tokio::task::spawn_blocking(move || {
            // A bug in an evaluation must not stop the snapshots: the
            // dashboards start over (and are evaluated in full next time).
            let evaluated = catch_unwind(AssertUnwindSafe(|| {
                let results = dashboards.update(&data, &changes, refresh_time, &cancel);
                (dashboards, results)
            }));
            let (dashboards, results, broken) = if let Ok((dashboards, results)) = evaluated {
                (dashboards, results, false)
            } else {
                tracing::error!("evaluating the dashboards failed; starting them over");
                (Box::default(), Arc::clone(&snapshot.dashboards), true)
            };
            let _ = tx.send(Internal::Evaluated {
                dashboards,
                snapshot: Box::new(Snapshot {
                    dashboards: results,
                    ..snapshot
                }),
                quiet,
                broken,
            });
        });
    }

    /// An evaluation is back: the snapshot goes out (unless only the
    /// time-dependent dashboards were refreshed and nothing changed).
    pub(super) fn on_evaluated(
        &mut self,
        dashboards: Box<Dashboards>,
        snapshot: Snapshot,
        quiet: bool,
        broken: bool,
    ) {
        if broken {
            self.dashboards_configured = true;
        }
        self.time_dependent = dashboards.time_dependent();
        let unchanged = Arc::ptr_eq(&snapshot.dashboards, &self.dashboard_results);
        self.dashboard_results = Arc::clone(&snapshot.dashboards);
        self.dashboards = Some(dashboards);
        if !(quiet && unchanged) {
            self.emit_snapshot(snapshot);
        }
        if broken {
            // Judged after the next (full) evaluation.
            self.notify.evaluation_failed();
        } else {
            self.judge(true);
        }
        if std::mem::take(&mut self.publish_soon) {
            self.publish_changes();
        }
        // Events that happened during the evaluation (or the one just
        // started for a snapshot asked for meanwhile) follow it.
        if self.dashboards.is_some() {
            for event in std::mem::take(&mut self.outbox) {
                self.send_event(event);
            }
        }
    }

    fn emit_snapshot(&mut self, mut snapshot: Snapshot) {
        self.revision += 1;
        snapshot.revision = self.revision;
        self.send_event(CoreEvent::Snapshot(Arc::new(snapshot)));
        self.last_publish = Some(Instant::now());
    }

    /// What `get_time()` returns in dashboard filters: Icinga's clock as
    /// far as it is known, else the local one.
    pub(super) fn evaluation_time(&self) -> Timestamp {
        self.watchdog
            .icinga_now()
            .map_or_else(|| self.ports.clock.now(), Timestamp::from_unix_seconds)
    }

    /// The store's objects for an evaluation.
    fn data(&self) -> Data {
        Data {
            hosts: Arc::clone(self.store.hosts()),
            services: Arc::clone(self.store.services()),
            host_groups: Arc::clone(self.store.host_groups()),
            service_groups: Arc::clone(self.store.service_groups()),
            now: self.evaluation_time(),
            events: Arc::clone(&self.recent_events),
            excluded: Arc::clone(self.store.excluded()),
        }
    }

    /// `Command::PreviewDashboard`: evaluates `views` over the current
    /// objects on a blocking thread. If the evaluation fails (a bug), the
    /// reply is dropped: the receiver sees `Canceled`.
    pub(super) fn preview(&mut self, views: Vec<View>, reply: oneshot::Sender<DashboardResult>) {
        if self.previews.running {
            self.previews.waiting = Some((views, reply));
            return;
        }
        self.previews.running = true;
        let data = self.data();
        let defaults = self.spec.hide_handled;
        let tx = self.internal_tx.clone();
        tokio::task::spawn_blocking(move || {
            if let Ok(result) = catch_unwind(AssertUnwindSafe(|| {
                dashboards::preview(&views, &data, defaults)
            })) {
                let _ = reply.send(result);
            } else {
                tracing::error!("a dashboard preview failed");
            }
            let _ = tx.send(Internal::PreviewDone);
        });
    }

    pub(super) fn on_preview_done(&mut self) {
        self.previews.running = false;
        if let Some((views, reply)) = self.previews.waiting.take() {
            self.preview(views, reply);
        }
    }
}
