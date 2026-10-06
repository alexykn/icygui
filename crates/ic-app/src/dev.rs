//! Development switches read from the environment, for screenshots and load
//! tests. They only apply to `--demo`.
//!
//! - `ICYGUI_DEMO_SCENARIO=large` serves another `ic_mock` scenario
//!   (`prod-cluster`, the default; `staging`; `lab`; `large`, production
//!   scale with 2 000 hosts and 30 000 services).
//! - `ICYGUI_DEMO_SEED=7` fixes the simulator's seed (the same seed tells
//!   the same story; the default changes every run).
//! - `ICYGUI_DEMO_DASHBOARD=databases` selects a dashboard by name.
//! - `ICYGUI_DEMO_FAULT` shows a connection failure on purpose: `offline`,
//!   `auth`, `tls`, `missing-secret`, `misconfigured`, `outage` (lost
//!   after 20 s), `slow` (every answer takes 0.9 s) or `frozen` (Icinga
//!   stops checking: checks become late).
//! - `ICYGUI_DEMO_STORM=20` starts a problem storm every 20 seconds
//!   instead of every 5 minutes (24 services fail: a few notifications,
//!   the rest silent, then a summary), for the notification centre.
//! - `ICYGUI_DEMO_OPEN` opens an object at start: `service` (the design's
//!   postgres-replication, screen 2b), `host` (its host db-prod-03 beside
//!   the list, screen 2c), `tab` (postgres-replication as a tab), or an
//!   object name (`db-prod-03`, `db-prod-03!postgres-replication`, or
//!   `tab:<name>` for a tab).

use ic_model::ObjectKey;

use crate::live::demo::DemoFault;

/// The demo's scenario.
pub(crate) const SCENARIO_ENV: &str = "ICYGUI_DEMO_SCENARIO";
/// The demo simulator's seed.
pub(crate) const SEED_ENV: &str = "ICYGUI_DEMO_SEED";
/// A connection failure the demo shows on purpose.
pub(crate) const FAULT_ENV: &str = "ICYGUI_DEMO_FAULT";
/// The dashboard selected at start.
pub(crate) const DASHBOARD_ENV: &str = "ICYGUI_DEMO_DASHBOARD";
/// The object opened at start.
pub(crate) const OPEN_ENV: &str = "ICYGUI_DEMO_OPEN";
/// Seconds between the demo's problem storms.
pub(crate) const STORM_ENV: &str = "ICYGUI_DEMO_STORM";

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
    /// `ICYGUI_DEMO_SCENARIO`.
    pub(crate) scenario: Option<String>,
    /// `ICYGUI_DEMO_SEED`.
    pub(crate) seed: Option<u64>,
    /// `ICYGUI_DEMO_FAULT`.
    pub(crate) fault: Option<DemoFault>,
    /// `ICYGUI_DEMO_DASHBOARD`.
    pub(crate) dashboard: Option<String>,
    /// `ICYGUI_DEMO_OPEN`.
    pub(crate) open: Option<OpenAtStart>,
    /// `ICYGUI_DEMO_STORM`.
    pub(crate) storm_every: Option<u64>,
}

impl DevOptions {
    /// Reads the switches from the process environment.
    pub(crate) fn from_env() -> Self {
        Self::parse(|name| std::env::var(name).ok())
    }

    /// Reads the switches through `get`; values that don't parse are
    /// ignored with a warning.
    pub(crate) fn parse(get: impl Fn(&str) -> Option<String>) -> Self {
        let scenario = get(SCENARIO_ENV)
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty());
        let seed = get(SEED_ENV).and_then(|value| {
            let parsed = value.trim().replace('_', "").parse::<u64>().ok();
            if parsed.is_none() {
                tracing::warn!(%value, "{SEED_ENV} is not a number; ignoring it");
            }
            parsed
        });
        let fault = get(FAULT_ENV).and_then(|value| {
            let parsed = DemoFault::parse(&value);
            if parsed.is_none() {
                tracing::warn!(%value, "{FAULT_ENV} names no fault; ignoring it");
            }
            parsed
        });
        let dashboard = get(DASHBOARD_ENV)
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty());
        let open = get(OPEN_ENV).and_then(|value| parse_open(value.trim()));
        let storm_every = get(STORM_ENV).and_then(|value| {
            let parsed = value
                .trim()
                .parse::<u64>()
                .ok()
                .filter(|seconds| *seconds > 0);
            if parsed.is_none() {
                tracing::warn!(%value, "{STORM_ENV} is not a number of seconds; ignoring it");
            }
            parsed
        });
        Self {
            scenario,
            seed,
            fault,
            dashboard,
            open,
            storm_every,
        }
    }

    /// Whether any switch is set.
    pub(crate) fn any(&self) -> bool {
        self.scenario.is_some()
            || self.seed.is_some()
            || self.fault.is_some()
            || self.dashboard.is_some()
            || self.open.is_some()
            || self.storm_every.is_some()
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
    crate::app_state::parse_object(name)
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
    fn scenario_and_seed() {
        let options = parse(&[(SCENARIO_ENV, " large "), (SEED_ENV, "1_000")]);
        assert_eq!(options.scenario.as_deref(), Some("large"));
        assert_eq!(options.seed, Some(1_000));
        assert!(options.any());
        assert_eq!(parse(&[(SEED_ENV, "lots")]).seed, None);
        assert_eq!(parse(&[(SCENARIO_ENV, "")]).scenario, None);
        assert_eq!(parse(&[(FAULT_ENV, "auth")]).fault, Some(DemoFault::Auth));
        assert_eq!(parse(&[(FAULT_ENV, "everything")]).fault, None);
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
