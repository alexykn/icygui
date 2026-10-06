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
//!   comments, downtimes, dependencies, endpoints, zones, users, Icinga's own
//!   notifications and commands
//! - `POST /v1/actions/<name>` for the runtime actions (checks,
//!   acknowledgements, comments, downtimes, passive results, command
//!   execution), with Icinga's effects and events
//! - `POST /v1/events`: newline-delimited JSON event streams
//!
//! A seeded simulator produces realistic churn, and [`MockControl`] lets
//! tests change state, inject faults, step time without sleeping and start
//! bursts (mass re-checks at Icinga's pace, [`MockControl::burst`]).
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
//! The reference is what a real Icinga 2.15.6 answers: responses recorded
//! from it (the repository's `contract/samples`, read by the tests in place)
//! win over the API documentation, and Icinga's handler sources fill in the
//! rest. `tests/fidelity.rs` checks that the mock's responses, errors and
//! events have the recorded keys and JSON types. In particular:
//! - Whole numbers are written as integers (`"state": 2`), others as
//!   floats; [`NumberFormat::Float`] writes every number with a fraction,
//!   like the documentation's examples.
//! - `filter` needs the `filter-expression` permission
//!   ([`MockConfig::enforce_filter_expression_permission`], on by default
//!   as from Icinga 2.17); name lists (`hosts`, `services`, `comments`,
//!   `downtimes`) need no extra permission, and one unknown name fails the
//!   whole request with 404.
//! - An unknown attribute in `attrs` or `joins` fails the whole request
//!   with 400, as in 2.15 (newer versions report it per object).
//! - Filters are parsed and evaluated by `ic-filter`, in a scope with
//!   Icinga's variables (the object, its joined objects, `filter_vars`,
//!   global constants) and Icinga's errors for undefined variables and
//!   unknown attributes. A filter that doesn't compile or fails answers
//!   404 like Icinga; where `ic-filter` deliberately differs (methods on
//!   `null`, for example) the mock answers as `ic-filter` evaluates.
//! - `queue` is required for `/v1/events`, as in 2.15 (newer versions
//!   ignore it).

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
pub use scenario::{Notification, Scenario, Summary, User, Zone, raw_check_result};
pub use server::MockServer;
pub use tls::{TlsMaterial, format_fingerprint};

/// Built-in scenarios.
///
/// - [`prod_cluster`](scenarios::prod_cluster): the design's sample data in a
///   ~150-host production estate.
/// - [`staging`](scenarios::staging): ten hosts, three warnings.
/// - [`lab`](scenarios::lab): two hosts, one pending.
/// - [`large`](scenarios::large): production scale, 2 000 hosts × 15
///   services with 5-minute intervals and about 5 % problems, like the
///   measurements in docs/performance.md;
///   [`large_with_hosts`](scenarios::large_with_hosts) builds it smaller.
pub mod scenarios {
    pub use crate::scenario::{
        NAMES, by_name, lab, large, large_with_hosts, prod_cluster, staging,
    };
}
