//! UI-agnostic application engine: the per-environment runtime, the sync
//! engine (initial load, event stream, re-queries, freshness watchdog,
//! hydration, reconcile, status polls, reconnects), the object store, the
//! dashboards evaluated over it, command dispatch and the local event log.
//!
//! - [`start`] runs the engine of one environment on its own thread and
//!   returns a [`CoreHandle`]: [`Command`]s go in, [`CoreEvent`]s come out,
//!   among them immutable [`snapshot::Snapshot`]s for the UI to render.
//!   An app runs one engine per environment side by side and tells each
//!   whether its environment is on screen ([`Command::SetActive`]): one off
//!   screen publishes less often and costs Icinga nothing more.
//! - Quiet mode ([`Command::SetQuiet`], PERF-09): an engine nobody looks at
//!   follows Icinga without check results (notifications as prompt as
//!   ever), polls and reconciles less, and wakes up without losing an
//!   event; the object the user opens ([`Command::Focus`]) goes ahead of
//!   every queue and of the request budget all by-name requests share.
//!   Background starts ([`Start::Background`]) wait a delay proportional
//!   to the installation's size before their first load.
//! - Notifications: every change the engine applies is judged by the
//!   environment's rules (`ic-rules`); every decision is logged and
//!   emitted as [`CoreEvent::Notification`], the audible ones also shown
//!   through the [`ports::Notifier`].
//! - The local event log (`SQLite`, one file per environment:
//!   [`event_log_path`]) keeps state changes, acknowledgements, comments,
//!   downtimes, flapping and notifications for the retention period;
//!   [`delete_event_log`] removes it with its environment.
//! - An environment lists its cluster's API URLs in order of preference;
//!   every connect finds out which node answers and how much of the
//!   cluster it sees ([`ClusterView`]), prefers nodes that see all of it
//!   and keeps trying them gently while connected to one that doesn't
//!   (ENV-12).
//! - [`test_connection`] and [`fetch_certificate`] serve the settings
//!   dialog without a running engine.
//!
//! Platform services (keychain, notifications, clock) are traits in
//! [`ports`], implemented in `ic-platform` and `ic-app`; tests use fakes.
//!
//! The engine is built for production scale (2 000 hosts / 30 000
//! services, docs/performance.md) and treats Icinga gently: lean tiered
//! loads, names in batches of 200, one re-query round at a time, jittered
//! exponential reconnects, no periodic full-attribute reloads.

mod backoff;
mod command;
mod connect;
mod dashboards;
mod engine;
mod error;
mod event_log;
mod handle;
pub mod ports;
mod probe;
pub mod snapshot;
mod spec;
mod store;
mod summary;
mod topology;

pub use command::{
    ActionOutcome, Command, ConnectionState, CoreEvent, LoadPhase, LogEntry, LogKind,
    NotificationRecord, UntrustedUrl,
};
pub use dashboards::{evaluate_dashboard, stream_events};
pub use error::CoreError;
pub use event_log::{delete_event_log, event_log_path, seed_event_log};
pub use handle::{CoreHandle, start, start_with_tuning};
/// The API user and permissions ([`CoreEvent::Permissions`]) and a server
/// certificate ([`ConnectionState::TlsFailed`], [`fetch_certificate`]),
/// re-exported so the UI can name them without depending on `ic-api`.
pub use ic_api::{ApiInfo, CertificateInfo};
pub use ports::SystemClock;
pub use probe::{
    ConnectionFailure, ConnectionReport, REQUIRED_PERMISSIONS, fetch_certificate,
    missing_permissions, test_connection,
};
pub use spec::{EnvironmentSpec, Ports, Start, Tuning};
pub use summary::Tally;
pub use topology::{ClusterNode, ClusterView, ConnectedNode, NodeState};
