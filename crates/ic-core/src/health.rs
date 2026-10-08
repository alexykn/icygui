//! What the cluster health page shows beyond the instance status (topic
//! 06): the masters' and satellites' numbers, the node's `ApiListener`
//! status and features, and the trend of the last 30 minutes of status
//! polls, kept in memory only.
//!
//! The endpoints' numbers come with the cluster nodes' states the status
//! poll already asks for (a few more attributes in the same request). The
//! `ApiListener` status and the features need requests of their own: they
//! are asked for only while the page is open ([`crate::Command::WatchHealth`])
//! and the environment isn't quiet, with the status polls (the listener)
//! or when the page opens and every five minutes (the features), each
//! request taken from the request budget. A closed page costs nothing.

use std::collections::{BTreeMap, VecDeque};
use std::time::Duration;

use ic_model::{EndpointStats, InstanceStatus, ListenerStatus, NodeFeatures, Timestamp};

/// How much trend the page keeps: the last 30 minutes of status polls.
pub const TREND_WINDOW: Duration = Duration::from_mins(30);

/// The most samples kept, whatever the poll interval (a poll every 30 s
/// fills 60).
const MAX_SAMPLES: usize = 240;

/// The numbers of one status poll, for the page's trend lines.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HealthSample {
    /// When the answer arrived (local clock).
    pub at: Timestamp,
    /// Active checks in the last minute.
    pub active_checks: f64,
    /// Passive check results in the last minute.
    pub passive_checks: f64,
    /// Average check latency in seconds.
    pub latency: f64,
    /// Average check execution time in seconds.
    pub execution: f64,
    /// Hosts and services without a check result yet.
    pub pending: u32,
    /// Hosts and services whose check is late (icygui's own tracking).
    pub late: u32,
    /// JSON-RPC messages processed per second, when the listener was
    /// asked for with this poll (the page was open).
    pub work_queue_rate: Option<f64>,
    /// Messages waiting to be relayed, likewise.
    pub relay_queue: Option<f64>,
}

impl HealthSample {
    /// The sample of a status poll answered at `at`, with `late` late
    /// checks and the listener's status if it came with the poll.
    #[must_use]
    pub fn of(
        status: &InstanceStatus,
        listener: Option<&ListenerStatus>,
        late: usize,
        at: Timestamp,
    ) -> Self {
        Self {
            at,
            active_checks: status.checks_per_minute,
            passive_checks: status.passive_checks_per_minute,
            latency: status.avg_latency,
            execution: status.avg_execution_time,
            pending: status
                .counts
                .hosts_pending
                .saturating_add(status.counts.services_pending),
            late: u32::try_from(late).unwrap_or(u32::MAX),
            work_queue_rate: listener.map(|listener| listener.work_queue_rate),
            relay_queue: listener.map(|listener| listener.relay_queue),
        }
    }
}

/// The cluster health page's data for one environment (see the module
/// notes).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClusterHealth {
    /// The masters' and satellites' numbers by endpoint name, as the
    /// connected node last reported them (the node itself has none).
    pub endpoints: BTreeMap<String, EndpointStats>,
    /// When the endpoints' numbers arrived (local clock): their
    /// `last_message` ages are as of then.
    pub endpoints_at: Option<Timestamp>,
    /// The connected node's `ApiListener` status, while the page is open
    /// (the last one after it closed; `None` before the first, and again
    /// after a reconnect).
    pub listener: Option<ListenerStatus>,
    /// When the listener status arrived (local clock).
    pub listener_at: Option<Timestamp>,
    /// The connected node's features, once the page asked (`None` before,
    /// and again after a reconnect).
    pub features: Option<NodeFeatures>,
    /// The status polls of the last [`TREND_WINDOW`], oldest first.
    pub samples: VecDeque<HealthSample>,
    /// How often the status poll runs now (30 s, or 5 minutes while
    /// quiet); zero without `status/query`.
    pub interval: Duration,
}

impl ClusterHealth {
    /// Adds a status poll's sample, dropping the ones older than
    /// [`TREND_WINDOW`] before it (and any that claim to be newer: the
    /// clock was set back).
    pub fn push(&mut self, sample: HealthSample) {
        let window = TREND_WINDOW.as_secs_f64();
        let now = sample.at.as_unix_seconds();
        self.samples.retain(|old| {
            let at = old.at.as_unix_seconds();
            at <= now && now - at <= window
        });
        self.samples.push_back(sample);
        while self.samples.len() > MAX_SAMPLES {
            self.samples.pop_front();
        }
    }

    /// The latest sample.
    #[must_use]
    pub fn latest(&self) -> Option<&HealthSample> {
        self.samples.back()
    }

    /// Forgets what belongs to one node: its listener status and features
    /// (a reconnect may reach another node). The endpoints' numbers and
    /// the trend stay.
    pub fn forget_node(&mut self) {
        self.listener = None;
        self.listener_at = None;
        self.features = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(at: f64, late: u32) -> HealthSample {
        HealthSample {
            at: Timestamp::from_unix_seconds(at),
            late,
            ..HealthSample::default()
        }
    }

    #[test]
    fn the_trend_keeps_thirty_minutes() {
        let mut health = ClusterHealth::default();
        for minute in 0..40_u32 {
            health.push(sample(f64::from(minute) * 60.0, minute));
        }
        let first = health.samples.front().unwrap();
        assert_eq!(first.late, 9, "39 minutes minus 30");
        assert_eq!(health.latest().unwrap().late, 39);
        assert_eq!(health.samples.len(), 31);
    }

    #[test]
    fn a_clock_set_back_starts_the_trend_over() {
        let mut health = ClusterHealth::default();
        health.push(sample(1_000.0, 1));
        health.push(sample(1_030.0, 2));
        health.push(sample(500.0, 3));
        assert_eq!(health.samples.len(), 1);
        assert_eq!(health.latest().unwrap().late, 3);
    }

    #[test]
    fn samples_are_capped() {
        let mut health = ClusterHealth::default();
        for second in 0..1_000_u32 {
            health.push(sample(f64::from(second), 0));
        }
        assert_eq!(health.samples.len(), MAX_SAMPLES);
    }

    #[test]
    fn a_reconnect_forgets_the_node_but_keeps_the_trend() {
        let mut health = ClusterHealth {
            listener: Some(ListenerStatus::default()),
            listener_at: Some(Timestamp::from_unix_seconds(1.0)),
            features: Some(NodeFeatures::default()),
            ..ClusterHealth::default()
        };
        health.push(sample(1.0, 0));
        health.forget_node();
        assert_eq!(health.listener, None);
        assert_eq!(health.features, None);
        assert_eq!(health.samples.len(), 1);
    }

    #[test]
    fn a_sample_reads_the_status_and_the_listener() {
        let status = InstanceStatus {
            checks_per_minute: 3_283.0,
            passive_checks_per_minute: 212.0,
            avg_latency: 0.004,
            avg_execution_time: 1.32,
            counts: ic_model::ObjectCounts {
                hosts_pending: 1,
                services_pending: 2,
                ..ic_model::ObjectCounts::default()
            },
            ..InstanceStatus::default()
        };
        let listener = ListenerStatus {
            relay_queue: 18_402.0,
            work_queue_rate: 412.0,
            ..ListenerStatus::default()
        };
        let at = Timestamp::from_unix_seconds(10.0);
        let with = HealthSample::of(&status, Some(&listener), 7, at);
        assert_eq!(with.pending, 3);
        assert_eq!(with.late, 7);
        assert_eq!(with.relay_queue, Some(18_402.0));
        assert_eq!(with.work_queue_rate, Some(412.0));
        let without = HealthSample::of(&status, None, 0, at);
        assert_eq!(without.relay_queue, None);
        assert!((without.active_checks - 3_283.0).abs() < f64::EPSILON);
    }
}
