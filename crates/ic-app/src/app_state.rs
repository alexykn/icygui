//! The data the window renders: the configuration, the latest snapshot of the
//! active environment, the connection status and the selected dashboard.
//!
//! [`AppState`] lives in a GPUI entity; views observe it and re-render when it
//! notifies. Today it's filled from [`crate::demo`]; the live core will feed
//! it the same way (`set_snapshot`, `set_connection`). Methods here never
//! touch GPUI, so they're tested directly.

use std::sync::Arc;

use ic_config::{Config, Dashboard, DashboardGroup, Environment};
use ic_core::snapshot::{DashboardResult, Snapshot};
use ic_model::{Timestamp, format_compact};
use ic_rules::DashboardRef;

use crate::demo;

/// Events older than this make the connection look stale (PLAN.md §2.1).
const STALE_AFTER_SECS: u64 = 30;

/// Application data shared by the views.
#[derive(Debug)]
pub(crate) struct AppState {
    config: Config,
    snapshot: Arc<Snapshot>,
    connection: ConnectionStatus,
    selected: Option<DashboardRef>,
    demo: bool,
}

impl AppState {
    /// The built-in demo environment, connected, as of `now`.
    pub(crate) fn demo(now: Timestamp) -> Self {
        let demo = demo::build(now);
        Self {
            config: demo.config,
            snapshot: Arc::new(demo.snapshot),
            connection: ConnectionStatus {
                endpoint: demo::ENDPOINT.to_owned(),
                link: Link::Connected,
                last_event_at: Some(now),
            },
            selected: Some(demo.selected),
            demo: true,
        }
    }

    /// No environment configured yet.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "used once config loading lands")
    )]
    pub(crate) fn empty() -> Self {
        Self {
            config: Config::default(),
            snapshot: Arc::default(),
            connection: ConnectionStatus {
                endpoint: String::new(),
                link: Link::Idle,
                last_event_at: None,
            },
            selected: None,
            demo: false,
        }
    }

    /// Whether this is the built-in demo.
    pub(crate) fn is_demo(&self) -> bool {
        self.demo
    }

    /// The active environment, if any.
    pub(crate) fn environment(&self) -> Option<&Environment> {
        let active = self.config.active_environment.as_deref()?;
        self.config
            .environments
            .iter()
            .find(|environment| environment.id == active)
    }

    /// The latest snapshot of the active environment.
    pub(crate) fn snapshot(&self) -> &Arc<Snapshot> {
        &self.snapshot
    }

    /// The connection to the active environment.
    pub(crate) fn connection(&self) -> &ConnectionStatus {
        &self.connection
    }

    /// The selected dashboard.
    pub(crate) fn selected(&self) -> Option<&DashboardRef> {
        self.selected.as_ref()
    }

    /// The selected dashboard and its group.
    pub(crate) fn selected_dashboard(&self) -> Option<(&DashboardGroup, &Dashboard)> {
        self.dashboard(self.selected.as_ref()?)
    }

    /// A dashboard and its group by reference.
    pub(crate) fn dashboard(
        &self,
        reference: &DashboardRef,
    ) -> Option<(&DashboardGroup, &Dashboard)> {
        let group = self
            .environment()?
            .groups
            .iter()
            .find(|group| group.id == reference.group_id)?;
        let dashboard = group
            .dashboards
            .iter()
            .find(|dashboard| dashboard.id == reference.dashboard_id)?;
        Some((group, dashboard))
    }

    /// A dashboard's evaluated rows and counts.
    pub(crate) fn result(&self, reference: &DashboardRef) -> Option<&DashboardResult> {
        self.snapshot.dashboards.get(reference)
    }

    /// Selects a dashboard. Returns whether the selection changed; unknown
    /// dashboards are ignored.
    pub(crate) fn select(&mut self, reference: DashboardRef) -> bool {
        if self.selected.as_ref() == Some(&reference) || self.dashboard(&reference).is_none() {
            return false;
        }
        self.selected = Some(reference);
        true
    }

    /// Collapses or expands a sidebar group. Returns whether the group exists.
    pub(crate) fn toggle_group(&mut self, group_id: &str) -> bool {
        let active = self.config.active_environment.clone();
        let group = self
            .config
            .environments
            .iter_mut()
            .filter(|environment| Some(&environment.id) == active.as_ref())
            .flat_map(|environment| environment.groups.iter_mut())
            .find(|group| group.id == group_id);
        match group {
            Some(group) => {
                group.collapsed = !group.collapsed;
                true
            }
            None => false,
        }
    }

    /// Replaces the snapshot (the core published a new one).
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "fed by the live core in a later wave")
    )]
    pub(crate) fn set_snapshot(&mut self, snapshot: Arc<Snapshot>) {
        self.snapshot = snapshot;
    }

    /// Replaces the connection status.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "fed by the live core in a later wave")
    )]
    pub(crate) fn set_connection(&mut self, connection: ConnectionStatus) {
        self.connection = connection;
    }

    /// Records that the event stream delivered something at `at`.
    pub(crate) fn record_event(&mut self, at: Timestamp) {
        self.connection.last_event_at = Some(at);
    }
}

/// The state of the link to the active environment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the live core reports these states in a later wave"
    )
)]
pub(crate) enum Link {
    /// No environment, or not started.
    Idle,
    /// Connecting or loading.
    Connecting,
    /// Connected; the event stream is open.
    Connected,
    /// Lost; retrying.
    Reconnecting,
    /// Stopped (authentication or TLS failure); needs the user.
    Failed,
}

/// How the footer colours the connection status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Health {
    /// Connected with recent events (green).
    Live,
    /// Connected, but no event for more than 30 seconds (yellow).
    Stale,
    /// Reconnecting or failed (red).
    Down,
    /// Not connected yet (grey).
    Idle,
}

/// The footer's `● master-01 · 2s`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ConnectionStatus {
    /// The endpoint's name.
    pub(crate) endpoint: String,
    /// The link state.
    pub(crate) link: Link,
    /// When the last event arrived.
    pub(crate) last_event_at: Option<Timestamp>,
}

impl ConnectionStatus {
    /// The colour class at `now`.
    pub(crate) fn health(&self, now: Timestamp) -> Health {
        match self.link {
            Link::Idle | Link::Connecting => Health::Idle,
            Link::Reconnecting | Link::Failed => Health::Down,
            Link::Connected => match self.last_event_at {
                Some(at) if at.elapsed_until(now).as_secs() > STALE_AFTER_SECS => Health::Stale,
                _ => Health::Live,
            },
        }
    }

    /// The footer text at `now`: the endpoint and the age of the last event,
    /// or what the connection is doing.
    pub(crate) fn label(&self, now: Timestamp) -> String {
        let endpoint = if self.endpoint.is_empty() {
            "no environment"
        } else {
            &self.endpoint
        };
        match (self.link, self.last_event_at) {
            (Link::Idle, _) | (Link::Connected, None) => endpoint.to_owned(),
            (Link::Connecting, _) => format!("{endpoint} · connecting"),
            (Link::Reconnecting, _) => format!("{endpoint} · reconnecting"),
            (Link::Failed, _) => format!("{endpoint} · offline"),
            (Link::Connected, Some(at)) => {
                format!("{endpoint} · {}", format_compact(at.elapsed_until(now)))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> Timestamp {
        Timestamp::from_unix_seconds(1_790_000_000.)
    }

    fn reference(group: &str, dashboard: &str) -> DashboardRef {
        DashboardRef {
            group_id: format!("demo-{group}"),
            dashboard_id: format!("demo-{group}-{dashboard}"),
        }
    }

    #[test]
    fn the_demo_starts_on_production() {
        let state = AppState::demo(now());
        assert!(state.is_demo());
        let (group, dashboard) = state.selected_dashboard().unwrap();
        assert_eq!(
            (group.name.as_str(), dashboard.name.as_str()),
            ("overview", "production")
        );
        assert!(state.result(state.selected().unwrap()).is_some());
        assert_eq!(state.environment().unwrap().name, "prod-cluster");
    }

    #[test]
    fn selecting_validates_the_dashboard() {
        let mut state = AppState::demo(now());
        assert!(state.select(reference("platform", "network")));
        assert!(!state.select(reference("platform", "network")), "unchanged");
        assert!(!state.select(reference("platform", "no-such-dashboard")));
        assert_eq!(state.selected(), Some(&reference("platform", "network")));
    }

    #[test]
    fn toggling_a_group_flips_its_collapsed_flag() {
        let mut state = AppState::demo(now());
        assert!(state.toggle_group("demo-lab"));
        let lab = |state: &AppState| {
            state
                .environment()
                .unwrap()
                .groups
                .iter()
                .find(|group| group.id == "demo-lab")
                .unwrap()
                .collapsed
        };
        assert!(lab(&state));
        assert!(state.toggle_group("demo-lab"));
        assert!(!lab(&state));
        assert!(!state.toggle_group("demo-nope"));
    }

    #[test]
    fn an_empty_state_has_nothing_selected() {
        let state = AppState::empty();
        assert!(!state.is_demo());
        assert!(state.environment().is_none());
        assert!(state.selected_dashboard().is_none());
        assert_eq!(state.connection().health(now()), Health::Idle);
        assert_eq!(state.connection().label(now()), "no environment");
    }

    #[test]
    fn new_snapshots_and_connections_replace_the_old_ones() {
        let mut state = AppState::empty();
        let snapshot = Arc::new(Snapshot {
            revision: 7,
            ..Snapshot::default()
        });
        state.set_snapshot(snapshot);
        assert_eq!(state.snapshot().revision, 7);
        state.set_connection(ConnectionStatus {
            endpoint: "master-02".to_owned(),
            link: Link::Reconnecting,
            last_event_at: None,
        });
        assert_eq!(state.connection().label(now()), "master-02 · reconnecting");
    }

    #[test]
    fn the_footer_shows_the_age_of_the_last_event() {
        let mut status = ConnectionStatus {
            endpoint: "master-01".to_owned(),
            link: Link::Connected,
            last_event_at: Some(now()),
        };
        let later = |seconds: f64| Timestamp::from_unix_seconds(now().as_unix_seconds() + seconds);
        assert_eq!(status.label(later(2.4)), "master-01 · 2s");
        assert_eq!(status.health(later(2.4)), Health::Live);
        assert_eq!(status.health(later(30.)), Health::Live);
        assert_eq!(status.health(later(31.)), Health::Stale);
        assert_eq!(status.label(later(125.)), "master-01 · 2m");

        status.link = Link::Failed;
        assert_eq!(status.health(later(1.)), Health::Down);
        assert_eq!(status.label(later(1.)), "master-01 · offline");
        status.link = Link::Connecting;
        assert_eq!(status.health(later(1.)), Health::Idle);
        assert_eq!(status.label(later(1.)), "master-01 · connecting");
    }

    #[test]
    fn recorded_events_reset_the_age() {
        let mut state = AppState::demo(now());
        let later = Timestamp::from_unix_seconds(now().as_unix_seconds() + 60.);
        assert_eq!(state.connection().health(later), Health::Stale);
        state.record_event(later);
        assert_eq!(state.connection().health(later), Health::Live);
        assert_eq!(state.connection().label(later), "master-01 · 0s");
    }
}
