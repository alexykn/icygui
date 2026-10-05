//! The persisted configuration. Every struct uses `#[serde(default)]` so
//! files written by older versions keep loading; breaking changes bump
//! [`CONFIG_VERSION`] and get a migration.
//!
//! Secrets (passwords) are never stored here: they live in the OS keychain
//! under the environment's id.

use std::path::PathBuf;

use ic_rules::{NotificationSettings, ScopeSetting};
use serde::{Deserialize, Serialize};

/// Current config file format version.
pub const CONFIG_VERSION: u32 = 1;

/// The whole configuration file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Format version, see [`CONFIG_VERSION`].
    pub version: u32,
    /// App-wide preferences.
    pub general: General,
    /// The environment shown at startup.
    pub active_environment: Option<String>,
    /// Configured Icinga environments.
    pub environments: Vec<Environment>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            general: General::default(),
            active_environment: None,
            environments: Vec::new(),
        }
    }
}

/// App-wide preferences.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct General {
    /// Colour theme.
    pub theme: ThemeChoice,
    /// Keep running in the tray / menu bar when the window closes.
    pub close_to_tray: bool,
    /// Start at login.
    pub launch_at_login: bool,
    /// How long the local event log keeps events, in hours.
    pub event_log_retention_hours: u32,
    /// How often the client reconciles with a lean reload, in seconds. `0`
    /// (the default) is adaptive: every 5 minutes below 5 000 hosts and
    /// services, every 15 minutes above. Other values override it; the
    /// engine never goes below [`crate::MIN_RECONCILE_INTERVAL_SECS`].
    pub reconcile_interval_secs: u32,
}

impl Default for General {
    fn default() -> Self {
        Self {
            theme: ThemeChoice::Dark,
            close_to_tray: true,
            launch_at_login: false,
            event_log_retention_hours: 48,
            reconcile_interval_secs: 0,
        }
    }
}

/// Colour theme.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeChoice {
    /// The design's dark theme.
    #[default]
    Dark,
    /// Light theme.
    Light,
    /// Follow the OS appearance.
    System,
}

/// One Icinga 2 API endpoint and everything configured for it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Environment {
    /// Stable id (UUID); also the keychain account for its password.
    pub id: String,
    /// Display name (`prod-cluster`).
    pub name: String,
    /// API base URL, `https://master-01.example.com:5665`.
    pub url: String,
    /// How to authenticate.
    pub auth: AuthConfig,
    /// How to trust the server certificate.
    pub tls: TlsConfig,
    /// Author name for acknowledgements, downtimes and comments; defaults to
    /// the API username.
    pub author: Option<String>,
    /// Dashboard groups shown in the sidebar.
    pub groups: Vec<DashboardGroup>,
    /// Notification rules.
    pub notifications: NotificationSettings,
}

/// How to authenticate against the API.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuthConfig {
    /// HTTP basic auth; the password is in the keychain.
    Basic {
        /// API username (`ApiUser` name).
        username: String,
    },
    /// TLS client certificate (`ApiUser` with `client_cn`).
    ClientCertificate {
        /// PEM certificate file.
        cert_path: PathBuf,
        /// PEM private key file (unencrypted).
        key_path: PathBuf,
    },
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self::Basic {
            username: String::new(),
        }
    }
}

/// How to trust the server certificate. Icinga signs its API certificate
/// with its own CA, so one of `ca_file` or `pinned_sha256` is usually set.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TlsConfig {
    /// PEM CA bundle (Icinga's `/var/lib/icinga2/certs/ca.crt`).
    pub ca_file: Option<PathBuf>,
    /// Accept exactly the server certificate with this SHA-256 fingerprint
    /// (hex, colons optional), skipping CA and name checks.
    pub pinned_sha256: Option<String>,
    /// Verify the certificate against this name instead of the URL's host
    /// (when connecting by IP or an alias).
    pub server_name: Option<String>,
    /// Also trust the operating system's root certificates.
    pub use_system_roots: bool,
}

/// A sidebar group: a named folder of dashboards.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DashboardGroup {
    /// Stable id (UUID).
    pub id: String,
    /// Display name.
    pub name: String,
    /// Collapsed in the sidebar.
    pub collapsed: bool,
    /// Notification setting relative to the environment.
    pub notifications: ScopeSetting,
    /// Dashboards, in sidebar order.
    pub dashboards: Vec<Dashboard>,
}

impl Default for DashboardGroup {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            collapsed: false,
            notifications: ScopeSetting::Inherit,
            dashboards: Vec::new(),
        }
    }
}

/// A dashboard ("thread"): one filtered, sorted list of hosts or services.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Dashboard {
    /// Stable id (UUID).
    pub id: String,
    /// Display name.
    pub name: String,
    /// What it shows.
    pub view: View,
    /// Notification setting relative to its group.
    pub notifications: ScopeSetting,
}

impl Default for Dashboard {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            view: View::default(),
            notifications: ScopeSetting::Inherit,
        }
    }
}

/// What a dashboard lists and how.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct View {
    /// Hosts or services.
    pub object_kind: ObjectKind,
    /// Icinga filter expression; empty = everything.
    pub filter: String,
    /// Only objects in a problem state.
    pub problems_only: bool,
    /// Hide handled problems (the design's `handled hidden`).
    pub hide_handled: bool,
    /// Sort order.
    pub sort: Sort,
    /// Optional grouping.
    pub group_by: GroupBy,
}

impl Default for View {
    fn default() -> Self {
        Self {
            object_kind: ObjectKind::Services,
            filter: String::new(),
            problems_only: true,
            hide_handled: true,
            sort: Sort::default(),
            group_by: GroupBy::None,
        }
    }
}

/// Hosts or services.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectKind {
    /// Services.
    #[default]
    Services,
    /// Hosts.
    Hosts,
}

/// Sort order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct Sort {
    /// Sort key.
    pub key: SortKey,
    /// Largest / newest first.
    pub descending: bool,
}

impl Default for Sort {
    fn default() -> Self {
        Self {
            key: SortKey::Severity,
            descending: true,
        }
    }
}

/// Sort keys. Ties fall back to severity, then name.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortKey {
    /// Icinga severity (the design's default, `severity ↓`).
    #[default]
    Severity,
    /// Time of the last state change.
    LastStateChange,
    /// Host name.
    Host,
    /// Service name (host name for host views).
    Service,
}

/// Grouping of list rows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupBy {
    /// Flat list.
    #[default]
    None,
    /// By host.
    Host,
    /// By host group (an object in several groups appears in each).
    HostGroup,
    /// By service group.
    ServiceGroup,
}
