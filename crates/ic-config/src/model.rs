//! The persisted configuration. Every struct uses `#[serde(default)]` so
//! files written by older versions keep loading; breaking changes bump
//! [`CONFIG_VERSION`] and get a migration.
//!
//! Secrets (passwords) are never stored here: they live in the OS keychain
//! under the environment's id.

use std::fmt;
use std::path::PathBuf;

use ic_rules::{NotificationSettings, ScopeSetting};
use serde::de::{self, MapAccess, Visitor, value::MapAccessDeserializer};
use serde::{Deserialize, Deserializer, Serialize};

/// Current config file format version. Version 2 replaced an environment's
/// single `url` (with the pin and server name in `tls`) by its list of
/// `urls`, each with its own pin and server name (ENV-12).
pub const CONFIG_VERSION: u32 = 2;

/// The most URLs an environment may list ([`Environment::urls`]). The
/// engine tries them in order on every connect, so a long list would only
/// slow a failover down.
pub const MAX_API_URLS: usize = 16;

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
    /// (the default) is adaptive: the interval follows the number of hosts
    /// and services (5 minutes for small installations, about 15 at 30 000
    /// services) and stretches while the event stream stays continuous.
    /// Other values override it; the engine never goes below
    /// [`crate::MIN_RECONCILE_INTERVAL_SECS`].
    pub reconcile_interval_secs: u32,
    /// Quiet mode (PERF-09, on by default): environments that aren't on
    /// screen, and the one on screen while the window is hidden (closed to
    /// the tray, or minimised where the system reports it), follow Icinga
    /// with state changes only, no check results, and poll and reconcile
    /// less often; notifications are never delayed. Off: every environment
    /// stays fully live.
    pub quiet_when_hidden: bool,
}

impl Default for General {
    fn default() -> Self {
        Self {
            theme: ThemeChoice::Dark,
            close_to_tray: true,
            launch_at_login: false,
            event_log_retention_hours: 48,
            reconcile_interval_secs: 0,
            quiet_when_hidden: true,
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

/// One Icinga cluster (a single master, an HA pair, a master with
/// satellites, or nodes behind a load balancer) and everything configured
/// for it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Environment {
    /// Stable id (UUID); also the keychain account for its password.
    pub id: String,
    /// Display name (`prod-cluster`).
    pub name: String,
    /// The cluster's API URLs, in order of preference (ENV-12): one for a
    /// single master or a load balancer, one per node for an HA pair or a
    /// master with satellites. The engine prefers a node that sees the
    /// whole cluster (in the top-level zone) and takes one in a child zone
    /// only while none of those answers. All of them share the login, the
    /// CA and the system roots; each has its own pin and server name.
    /// At most [`MAX_API_URLS`].
    pub urls: Vec<ApiUrl>,
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

/// How to trust the servers' certificates, for every URL of the
/// environment. Icinga signs its API certificates with its own CA, which
/// covers every node of the cluster, so `ca_file` is the recommended
/// setting; a pinned certificate names one node's certificate and is set
/// per URL ([`ApiUrl::pinned_sha256`]).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TlsConfig {
    /// PEM CA bundle (Icinga's `/var/lib/icinga2/certs/ca.crt`).
    pub ca_file: Option<PathBuf>,
    /// Also trust the operating system's root certificates.
    pub use_system_roots: bool,
}

/// One API URL of an environment, with what is particular to the server
/// behind it.
///
/// In the settings file each entry is a table (`url`, and optionally
/// `pinned_sha256` and `server_name`); a hand-written file may also give a
/// plain string (`urls = ["https://master-01:5665", "https://master-02:5665"]`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ApiUrl {
    /// The API base URL, `https://master-01.example.com:5665`.
    pub url: String,
    /// Accept exactly the server certificate with this SHA-256 fingerprint
    /// (hex, colons optional) at this URL, skipping CA and name checks.
    pub pinned_sha256: Option<String>,
    /// Verify the certificate at this URL against this name instead of
    /// the URL's host (when connecting by IP, a tunnel or an alias).
    pub server_name: Option<String>,
}

impl<'de> Deserialize<'de> for ApiUrl {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        /// The table form, read field by field so unknown keys are found
        /// (and logged) like everywhere else in the file.
        #[derive(Default, Deserialize)]
        #[serde(default)]
        struct Table {
            url: String,
            pinned_sha256: Option<String>,
            server_name: Option<String>,
        }

        struct UrlVisitor;

        impl<'de> Visitor<'de> for UrlVisitor {
            type Value = ApiUrl;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a URL, or a table with `url`")
            }

            fn visit_str<E: de::Error>(self, url: &str) -> Result<ApiUrl, E> {
                Ok(ApiUrl {
                    url: url.to_owned(),
                    ..ApiUrl::default()
                })
            }

            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<ApiUrl, A::Error> {
                let table = Table::deserialize(MapAccessDeserializer::new(map))?;
                Ok(ApiUrl {
                    url: table.url,
                    pinned_sha256: table.pinned_sha256,
                    server_name: table.server_name,
                })
            }
        }

        deserializer.deserialize_any(UrlVisitor)
    }
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
