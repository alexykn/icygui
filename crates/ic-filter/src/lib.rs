//! Parser and evaluator for the subset of the Icinga 2 filter language used by
//! dashboards and notification rules (`host.vars.role == "db" && service.state != 0`).
//!
//! Evaluated client-side against `ic-model` objects. Pure.
