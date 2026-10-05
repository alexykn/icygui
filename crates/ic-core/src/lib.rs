//! UI-agnostic application engine: the per-environment runtime, the sync engine
//! (initial load, event stream, periodic reconcile), the object store, command
//! dispatch and the local event log.
//!
//! Platform services (keychain, notifications, tray, autostart) are traits here
//! and implemented in `ic-platform`.

pub mod ports;
pub mod snapshot;
