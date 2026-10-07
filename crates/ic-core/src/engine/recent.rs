//! The event log's latest entries, kept in memory for event stream views
//! (v1, topic 04).
//!
//! At the start (and when the environment's log changes) the engine reads
//! the newest [`RECENT_EVENTS`] entries from its local log once, on the
//! log's thread; after that every entry the engine records is added as it
//! is written. Nothing here asks Icinga for anything: the stream views show
//! what the engine already logs.

use std::sync::Arc;

use futures::channel::oneshot;
use ic_model::Timestamp;

use super::{Engine, Internal};
use crate::command::LogEntry;
use crate::dashboards::RECENT_EVENTS;

impl Engine {
    /// Writes entries to the event log and adds them to the recent ones.
    pub(super) fn record_log(&mut self, log: Vec<LogEntry>) {
        if log.is_empty() {
            return;
        }
        let mut recent = Vec::with_capacity(RECENT_EVENTS);
        // Recorded oldest first; the recent ones are newest first.
        recent.extend(log.iter().rev().cloned());
        recent.extend(
            self.recent_events
                .iter()
                .take(RECENT_EVENTS.saturating_sub(log.len()))
                .cloned(),
        );
        recent.truncate(RECENT_EVENTS);
        self.recent_events = Arc::new(recent);
        self.events_changed = true;
        self.event_log.record(log);
    }

    /// Reads the newest entries of the event log (once, at the start or
    /// after the log changed); they go behind the entries recorded since.
    pub(super) fn load_recent_events(&mut self) {
        self.recent_generation += 1;
        let generation = self.recent_generation;
        let (reply, answer) = oneshot::channel();
        // Queued on the log's thread before anything this engine records
        // from now on, so the answer holds only older entries.
        self.event_log.history(None, RECENT_EVENTS, reply);
        let tx = self.internal_tx.clone();
        tokio::spawn(async move {
            if let Ok(entries) = answer.await {
                let _ = tx.send(Internal::RecentEvents {
                    generation,
                    entries,
                });
            }
        });
    }

    /// The log's newest entries are back.
    pub(super) fn on_recent_events(&mut self, generation: u64, entries: Vec<LogEntry>) {
        if generation != self.recent_generation || entries.is_empty() {
            return;
        }
        let mut recent = Vec::with_capacity(RECENT_EVENTS);
        recent.extend(self.recent_events.iter().cloned());
        recent.extend(entries);
        recent.truncate(RECENT_EVENTS);
        self.recent_events = Arc::new(recent);
        self.events_changed = true;
        if self
            .dashboards
            .as_deref()
            .is_some_and(crate::dashboards::Dashboards::has_streams)
        {
            self.publish_changes();
        }
    }

    /// Another log: the recent entries are its.
    pub(super) fn reset_recent_events(&mut self) {
        self.recent_events = Arc::default();
        self.events_changed = true;
        self.load_recent_events();
    }

    /// Drops the recent entries from before `before` (the log's retention).
    pub(super) fn prune_recent_events(&mut self, before: Timestamp) {
        if self
            .recent_events
            .last()
            .is_some_and(|oldest| oldest.at < before)
        {
            let kept: Vec<LogEntry> = self
                .recent_events
                .iter()
                .filter(|entry| entry.at >= before)
                .cloned()
                .collect();
            self.recent_events = Arc::new(kept);
            self.events_changed = true;
        }
    }

    /// Whether new events wait for the event stream views.
    pub(super) fn streams_wait(&self) -> bool {
        self.events_changed
            && self
                .dashboards
                .as_deref()
                .is_some_and(crate::dashboards::Dashboards::has_streams)
    }
}
