//! A local look-alike of the Icinga 2 REST API, for development and tests.
//! Never shipped with the app (PLAN.md D9, §3.6).
//!
//! It serves the subset of the API the client uses, over HTTPS with Basic
//! auth, with the wire format and behaviour of Icinga's own handlers:
//!
//! - `GET /v1`, `GET /v1/status[/<name>]`
//! - `GET /v1/objects/<type>[/<name>]` (also `POST` with
//!   `X-HTTP-Method-Override: GET`) with `attrs`, `joins`, `meta`, `filter`
//!   and `filter_vars`, for hosts, services, host and service groups,
//!   comments, downtimes, dependencies, endpoints, zones, users and commands
//! - `POST /v1/actions/<name>` for the runtime actions (checks,
//!   acknowledgements, comments, downtimes, passive results, command
//!   execution), with Icinga's effects and events
//! - `POST /v1/events`: newline-delimited JSON event streams
//!
//! A seeded simulator produces realistic churn, and [`MockControl`] lets
//! tests change state, inject faults and step time without sleeping.
//!
//! ```no_run
//! # async fn example() -> Result<(), ic_mock::MockError> {
//! use ic_mock::{MockConfig, MockServer, scenarios};
//!
//! let server = MockServer::start(MockConfig::with_scenario(scenarios::prod_cluster())).await?;
//! println!("{} pinned {}", server.url(), server.cert_fingerprint());
//! let control = server.control();
//! control.set_service_state("db-prod-01", "load", ic_model::ServiceState::Critical, "CRITICAL - load 40", true)?;
//! server.shutdown().await;
//! # Ok(())
//! # }
//! ```
//!
//! Where the API documentation and Icinga's sources disagree, the mock
//! follows the sources (the reference is Icinga 2's `master` handlers).
//! Deliberate choices:
//! - `queue` is required for `/v1/events` (Icinga ≤ 2.14 requires it; newer
//!   versions ignore it), so a client that works here works with both.
//! - Numbers are written as doubles (`200.0`) by default, like the
//!   documentation; [`NumberFormat::Integral`] switches to Icinga 2.13+'s
//!   integral style.
//! - `filter` needs the `filter-expression` permission
//!   ([`MockConfig::enforce_filter_expression_permission`]), Icinga's
//!   current default.

mod auth;
mod config;
mod control;
mod error;
mod events;
mod filter;
mod http;
mod json;
mod model;
mod outputs;
mod rng;
mod scenario;
mod server;
mod sim;
mod tls;

pub use config::{MockConfig, MockTls, MockUser, NumberFormat, SimulationConfig, StormConfig};
pub use control::{MockControl, RecordedRequest};
pub use error::MockError;
pub use scenario::{Scenario, Summary, User, Zone, raw_check_result};
pub use server::MockServer;
pub use tls::{TlsMaterial, format_fingerprint};

/// Built-in scenarios.
///
/// - [`prod_cluster`](scenarios::prod_cluster): the design's sample data in a
///   ~150-host production estate.
/// - [`staging`](scenarios::staging): ten hosts, three warnings.
/// - [`lab`](scenarios::lab): two hosts, one pending.
/// - [`large`](scenarios::large): ~2000 hosts × 10 services, ~3% problems.
pub mod scenarios {
    pub use crate::scenario::{NAMES, by_name, lab, large, prod_cluster, staging};
}
