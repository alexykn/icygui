//! What the engine derives from the store and how it stays in sync beyond
//! the event stream: dashboards (`dashboards.rs`); the freshness watchdog,
//! hydration and reconcile (`freshness.rs`). One test binary, to keep the
//! build small.

#[path = "../support/mod.rs"]
mod support;

mod dashboards;
mod freshness;
