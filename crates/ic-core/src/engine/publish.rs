//! Publishing snapshots, with the dashboards evaluated for them on a
//! blocking thread, and dashboard previews.
//!
//! A snapshot goes out at most every `publish_interval` while things
//! change, and right away after load tiers and actions. When objects
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
    waiting: Option<(View, oneshot::Sender<Result<DashboardResult, String>>)>,
}

impl Engine {
    /// Whether a snapshot should go out.
    fn needs_publish(&self) -> bool {
        self.store.has_changes()
            || self.dashboards_configured
            || self.watchdog.has_changes()
            || self.notify.has_pending()
    }

    /// When the next throttled snapshot (or the time-dependent dashboards'
    /// refresh) is due; `None` while an evaluation runs (its return
    /// publishes).
    pub(super) fn publish_at(&self, now: Instant) -> Option<Instant> {
        self.dashboards.as_ref()?;
        let changes = self.needs_publish().then(|| {
            self.last_publish
                .map_or(now, |last| last + self.tuning.publish_interval)
        });
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
        let reconfigured = std::mem::take(&mut self.dashboards_configured);
        if reconfigured {
            dashboards.configure(&self.spec.environment);
        }
        let refresh_time = self.time_dependent && self.time_refreshed.elapsed() >= TIME_REFRESH;
        let snapshot = self.store.snapshot(
            0,
            self.ports.clock.now(),
            Arc::clone(&self.dashboard_results),
            self.watchdog.late(),
        );
        let evaluate = reconfigured
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
        let quiet = !changes.any && !reconfigured && !late_changed;
        let data = Data {
            hosts: Arc::clone(&snapshot.hosts),
            services: Arc::clone(&snapshot.services),
            host_groups: Arc::clone(&snapshot.host_groups),
            service_groups: Arc::clone(&snapshot.service_groups),
            now: self.evaluation_time(),
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
        }
    }

    /// `Command::PreviewDashboard`: evaluates `view` over the current
    /// objects on a blocking thread.
    pub(super) fn preview(
        &mut self,
        view: View,
        reply: oneshot::Sender<Result<DashboardResult, String>>,
    ) {
        if self.previews.running {
            self.previews.waiting = Some((view, reply));
            return;
        }
        self.previews.running = true;
        let data = self.data();
        let tx = self.internal_tx.clone();
        tokio::task::spawn_blocking(move || {
            let result = catch_unwind(AssertUnwindSafe(|| dashboards::preview(&view, &data)))
                .unwrap_or_else(|_| Err("the preview failed (an internal error)".to_owned()));
            let _ = reply.send(result);
            let _ = tx.send(Internal::PreviewDone);
        });
    }

    pub(super) fn on_preview_done(&mut self) {
        self.previews.running = false;
        if let Some((view, reply)) = self.previews.waiting.take() {
            self.preview(view, reply);
        }
    }
}
