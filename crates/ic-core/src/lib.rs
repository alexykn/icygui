//! UI-agnostic application engine: the per-environment runtime, the sync
//! engine (initial load, event stream, re-queries, freshness watchdog,
//! hydration, reconcile, status polls, reconnects), the object store, the
//! dashboards evaluated over it, command dispatch and the local event log.
//!
//! - [`start`] runs the engine of one environment on its own thread and
//!   returns a [`CoreHandle`]: [`Command`]s go in, [`CoreEvent`]s come out,
//!   among them immutable [`snapshot::Snapshot`]s for the UI to render.
//! - Notifications: every change the engine applies is judged by the
//!   environment's rules (`ic-rules`); every decision is logged and
//!   emitted as [`CoreEvent::Notification`], the audible ones also shown
//!   through the [`ports::Notifier`].
//! - The local event log (`SQLite`, one file per environment:
//!   [`event_log_path`]) keeps state changes, acknowledgements, comments,
//!   downtimes, flapping and notifications for the retention period;
//!   [`delete_event_log`] removes it with its environment.
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

pub use command::{
    ActionOutcome, Command, ConnectionState, CoreEvent, LoadPhase, LogEntry, LogKind,
    NotificationRecord,
};
pub use error::CoreError;
pub use event_log::{delete_event_log, event_log_path};
pub use handle::{CoreHandle, start, start_with_tuning};
pub use ports::SystemClock;
pub use probe::{
    ConnectionFailure, ConnectionReport, REQUIRED_PERMISSIONS, fetch_certificate,
    missing_permissions, test_connection,
};
pub use spec::{EnvironmentSpec, Ports, Tuning};
