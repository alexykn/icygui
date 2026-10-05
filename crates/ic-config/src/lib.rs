//! Local settings: environments, dashboard groups, dashboards and notification
//! rules, stored as versioned TOML with migrations and atomic writes.

mod model;

pub use model::{
    AuthConfig, CONFIG_VERSION, Config, Dashboard, DashboardGroup, Environment, General, GroupBy,
    ObjectKind, Sort, SortKey, ThemeChoice, TlsConfig, View,
};
