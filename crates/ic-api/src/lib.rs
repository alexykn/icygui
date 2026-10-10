//! Icinga 2 REST API client: object queries, `/v1/actions`, `/v1/status` and the
//! `/v1/events` stream.
//!
//! Read-only towards configuration: this crate never calls `objects/modify`,
//! config packages or anything else that changes Icinga's settings. Objects are
//! always addressed by name, never by `filter` expressions (which need the
//! `filter-expression` permission from Icinga 2.17 on).
//!
//! - [`Client`] talks to one API endpoint over HTTPS (rustls with the ring
//!   provider) with basic auth or a client certificate.
//! - [`TlsSettings`] chooses how the server is trusted: a pinned certificate,
//!   Icinga's CA, and/or the system roots, optionally checking a different
//!   server name than the URL's host.
//! - [`fetch_server_certificate`] reads a server's certificate for trust on
//!   first use.
//! - [`Detail`] chooses how much of a host or service to load: at scale the
//!   services are loaded lean (no check result) and fetched in full by name
//!   only where needed ([`Client::objects`], [`Fetched`]).
//! - Icinga's own `Notification` objects (who Icinga notified, and when)
//!   load with [`Client::notifications`] and by name with
//!   [`Client::notifications_named`].
//! - [`RequestBudget`] paces by-name requests: a token bucket shared by the
//!   clones of a client it is attached to ([`Client::with_budget`]), with
//!   [`Client::priority`] for the object the user is opening.
//! - Wire JSON is mapped into `ic-model` types; the wire structs are private.

mod actions;
mod budget;
mod client;
mod detail;
mod error;
mod events;
mod info;
mod lenient;
mod settings;
mod tls;
mod wire;

pub use budget::RequestBudget;
pub use client::{ActionResult, Client, FEATURE_TYPES, FeaturesRead, NAMES_PER_REQUEST};
pub use detail::{Cluster, Detail, EndpointState, Fetched, FetchedNotifications};
pub use error::ApiError;
pub use events::{EventLines, EventStream, parse_event};
pub use info::ApiInfo;
pub use settings::{
    CONNECT_TIMEOUT, ConnectionSettings, Credentials, DEFAULT_ACTION_TIMEOUT,
    DEFAULT_REQUEST_TIMEOUT, TlsSettings,
};
pub use tls::{CertificateInfo, fetch_server_certificate, format_fingerprint};
pub use url::Url;
