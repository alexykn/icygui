//! Icinga domain types shared by every other crate.
//!
//! This crate is pure: no I/O, no async runtime, no UI. Wire formats live in
//! `ic-api` and are mapped into these types at the edge.

mod state;

pub use state::{HostState, ServiceState, StateType};
