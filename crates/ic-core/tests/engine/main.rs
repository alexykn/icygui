//! The engine end to end against an in-process `ic-mock`, one module per
//! area. One test binary for all of them: each binary links every
//! dependency again, and the build directory is shared and small.
//!
//! - `connect`: connecting, the tiered load, reconnects and failures;
//! - `events`: the live event stream, re-queries and bursts;
//! - `actions`: every action end to end;
//! - `dashboards`: dashboard evaluation and previews;
//! - `freshness`: the freshness watchdog, hydration and reconcile;
//! - `gentle`: failing reloads, `Refresh` presses, hidden objects, refused
//!   kinds and stalled streams cost Icinga little;
//! - `notifications`: rule inputs, the rule engine and the notifier;
//! - `event_log`: the local `SQLite` event log;
//! - `notified`: who Icinga notified, and when (its `Notification` objects);
//! - `probe`: `test_connection` and `fetch_certificate`;
//! - `scale`: production-size loads and bursts (ignored by default);
//! - `topology`: several API URLs per environment, against several mocks
//!   forming one cluster (ENV-12).

#[path = "../support/mod.rs"]
mod support;

mod actions;
mod connect;
mod dashboards;
mod event_log;
mod events;
mod freshness;
mod gentle;
mod notifications;
mod notified;
mod probe;
mod scale;
mod topology;
