//! Local settings: environments, dashboard groups, dashboards and notification
//! rules, stored as versioned TOML with migrations and atomic writes.
//!
//! - [`Paths`] says where files live; [`ConfigStore`] loads and saves the
//!   settings file. Loading upgrades older formats ([`migrate()`]); saving is
//!   atomic, keeps one `.bak` copy and makes the file user-only on Unix.
//! - [`Config::validate`] finds settings that load but can't work (bad URLs,
//!   missing names, duplicate ids, unparsable fingerprints).
//! - [`export_groups`] and [`import_groups`] share dashboards as files.
//!
//! Secrets never appear here: passwords live in the OS keychain under the
//! environment's id, saving refuses settings that would write one into the
//! file, and errors never quote one. Apart from reading and writing its
//! files this crate does no I/O, and it has no async runtime.

mod config;
mod environment;
mod error;
mod files;
mod fingerprint;
mod migrate;
mod model;
mod paths;
mod share;
mod store;
mod validate;

pub use config::new_id;
pub use environment::default_groups;
pub use error::ConfigError;
pub use fingerprint::{format_fingerprint, parse_fingerprint};
pub use migrate::migrate;
pub use model::{
    AuthConfig, CONFIG_VERSION, Config, Dashboard, DashboardGroup, Environment, General, GroupBy,
    ObjectKind, Sort, SortKey, ThemeChoice, TlsConfig, View,
};
pub use paths::Paths;
pub use share::{export_groups, import_groups};
pub use store::ConfigStore;
pub use validate::{MIN_EVENT_LOG_RETENTION_HOURS, MIN_RECONCILE_INTERVAL_SECS, ValidationIssue};
