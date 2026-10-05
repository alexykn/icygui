//! Development switches read from the environment, for screenshots and load
//! tests. They only apply to the built-in demo.
//!
//! - `ICYGUI_DEMO_ROWS=20000` adds that many generated services and selects
//!   the `lab / load-test` dashboard that lists them.
//! - `ICYGUI_DEMO_DASHBOARD=databases` selects a dashboard by name.
//! - `ICYGUI_DEMO_OPEN` opens an object at start: `service` (the design's
//!   postgres-replication, screen 2b), `host` (its host db-prod-03 beside
//!   the list, screen 2c), `tab` (postgres-replication as a tab), or an
//!   object name (`db-prod-03`, `db-prod-03!postgres-replication`, or
//!   `tab:<name>` for a tab).

use ic_model::{ObjectKey, ServiceKey};

/// Generated rows for load tests.
pub(crate) const ROWS_ENV: &str = "ICYGUI_DEMO_ROWS";
/// The dashboard selected at start.
pub(crate) const DASHBOARD_ENV: &str = "ICYGUI_DEMO_DASHBOARD";
/// The object opened at start.
pub(crate) const OPEN_ENV: &str = "ICYGUI_DEMO_OPEN";

/// The most generated rows accepted; more would only exhaust memory.
const MAX_ROWS: usize = 1_000_000;

/// What to open at start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum OpenAtStart {
    /// Put the cursor on the object and open its pane.
    Object(ObjectKey),
    /// Put the cursor on `cursor` and show `pane` in the pane, as after
    /// following the service's host link (screen 2c).
    Linked {
        /// The list row under the cursor.
        cursor: ObjectKey,
        /// The object in the pane.
        pane: ObjectKey,
    },
    /// Open the object as a tab.
    Tab(ObjectKey),
}

/// The switches that are set.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct DevOptions {
    /// `ICYGUI_DEMO_ROWS`.
    pub(crate) generated_rows: usize,
    /// `ICYGUI_DEMO_DASHBOARD`.
    pub(crate) dashboard: Option<String>,
    /// `ICYGUI_DEMO_OPEN`.
    pub(crate) open: Option<OpenAtStart>,
}

impl DevOptions {
    /// Reads the switches from the process environment.
    pub(crate) fn from_env() -> Self {
        Self::parse(|name| std::env::var(name).ok())
    }

    /// Reads the switches through `get`; values that don't parse are
    /// ignored with a warning.
    pub(crate) fn parse(get: impl Fn(&str) -> Option<String>) -> Self {
        let generated_rows = get(ROWS_ENV)
            .and_then(|value| {
                let parsed = value.trim().replace('_', "").parse::<usize>().ok();
                if parsed.is_none() {
                    tracing::warn!(%value, "{ROWS_ENV} is not a number; ignoring it");
                }
                parsed
            })
            .map_or(0, |rows| rows.min(MAX_ROWS));
        let dashboard = get(DASHBOARD_ENV)
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty());
        let open = get(OPEN_ENV).and_then(|value| parse_open(value.trim()));
        Self {
            generated_rows,
            dashboard,
            open,
        }
    }

    /// Whether any switch is set.
    pub(crate) fn any(&self) -> bool {
        self.generated_rows > 0 || self.dashboard.is_some() || self.open.is_some()
    }
}

fn replication() -> ObjectKey {
    ObjectKey::service("db-prod-03", "postgres-replication")
}

fn parse_open(value: &str) -> Option<OpenAtStart> {
    match value {
        "" => None,
        "service" => Some(OpenAtStart::Object(replication())),
        "host" => Some(OpenAtStart::Linked {
            cursor: replication(),
            pane: ObjectKey::host("db-prod-03"),
        }),
        "tab" => Some(OpenAtStart::Tab(replication())),
        other => match other.strip_prefix("tab:") {
            Some(name) => object(name).map(OpenAtStart::Tab),
            None => object(other).map(OpenAtStart::Object),
        },
    }
}

fn object(name: &str) -> Option<ObjectKey> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    match ServiceKey::parse(name) {
        Some(key) => Some(ObjectKey::Service { key }),
        None if !name.contains('!') => Some(ObjectKey::host(name)),
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn parse(values: &[(&str, &str)]) -> DevOptions {
        let values: HashMap<String, String> = values
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        DevOptions::parse(|name| values.get(name).cloned())
    }

    #[test]
    fn nothing_set_means_nothing_happens() {
        let options = parse(&[]);
        assert_eq!(options, DevOptions::default());
        assert!(!options.any());
    }

    #[test]
    fn rows_accept_separators_and_are_capped() {
        assert_eq!(parse(&[(ROWS_ENV, "20_000")]).generated_rows, 20_000);
        assert_eq!(parse(&[(ROWS_ENV, " 500 ")]).generated_rows, 500);
        assert_eq!(parse(&[(ROWS_ENV, "lots")]).generated_rows, 0);
        assert_eq!(parse(&[(ROWS_ENV, "999999999")]).generated_rows, MAX_ROWS);
        assert!(parse(&[(ROWS_ENV, "1")]).any());
    }

    #[test]
    fn open_shortcuts_follow_the_design() {
        assert_eq!(
            parse(&[(OPEN_ENV, "service")]).open,
            Some(OpenAtStart::Object(replication()))
        );
        assert_eq!(
            parse(&[(OPEN_ENV, "host")]).open,
            Some(OpenAtStart::Linked {
                cursor: replication(),
                pane: ObjectKey::host("db-prod-03"),
            })
        );
        assert_eq!(
            parse(&[(OPEN_ENV, "tab")]).open,
            Some(OpenAtStart::Tab(replication()))
        );
    }

    #[test]
    fn open_takes_object_names() {
        assert_eq!(
            parse(&[(OPEN_ENV, "mq-prod-01!rabbitmq-queue")]).open,
            Some(OpenAtStart::Object(ObjectKey::service(
                "mq-prod-01",
                "rabbitmq-queue"
            )))
        );
        assert_eq!(
            parse(&[(OPEN_ENV, "tab:k8s-node-11")]).open,
            Some(OpenAtStart::Tab(ObjectKey::host("k8s-node-11")))
        );
        assert_eq!(parse(&[(OPEN_ENV, "!broken")]).open, None);
        assert_eq!(parse(&[(OPEN_ENV, "tab:")]).open, None);
        assert_eq!(parse(&[(OPEN_ENV, "  ")]).open, None);
    }

    #[test]
    fn dashboards_by_name() {
        assert_eq!(
            parse(&[(DASHBOARD_ENV, " databases ")])
                .dashboard
                .as_deref(),
            Some("databases")
        );
        assert_eq!(parse(&[(DASHBOARD_ENV, "")]).dashboard, None);
    }
}
