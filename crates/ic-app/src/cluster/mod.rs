//! The sidebar's fixed **cluster** section (topic 14, rounds 3 and 5;
//! topic 06): the whole environment's handling, downtimes and events (the
//! same views as a dashboard's, without a filter) and the cluster's health.
//! It sits at the top, above the groups; each entry has its mark in the
//! fixed dot slot and its count in the fixed count slot.

use ic_core::snapshot::Snapshot;
use ic_model::Timestamp;
use ic_ui_kit::IconName;

use crate::lists::ListKind;

pub(crate) mod beats;
pub(crate) mod health;
mod page;
mod spark;

#[cfg(all(test, target_os = "linux"))]
pub(crate) use page::Stop as HealthStop;
pub(crate) use page::{HealthPage, HealthPageEvent, Preview as HealthPreview};

/// An entry of the cluster section.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ClusterEntry {
    /// Who is handling what, environment-wide.
    Handling,
    /// Every downtime in effect or to come, environment-wide.
    Downtimes,
    /// The environment's latest events.
    Events,
    /// The cluster's health (topic 06).
    Health,
}

impl ClusterEntry {
    /// The entries, in the sidebar's order: the pages people work through
    /// first, health last.
    pub(crate) const ALL: [Self; 4] = [Self::Handling, Self::Downtimes, Self::Events, Self::Health];

    /// The entry's label (and its page's title).
    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::Handling => "handling",
            Self::Downtimes => "downtimes",
            Self::Events => "events",
            Self::Health => "health",
        }
    }

    /// Its element id in the sidebar.
    pub(crate) fn id(self) -> &'static str {
        match self {
            Self::Handling => "cluster-handling",
            Self::Downtimes => "cluster-downtimes",
            Self::Events => "cluster-events",
            Self::Health => "cluster-health",
        }
    }

    /// The entry showing `kind`.
    pub(crate) fn of_list(kind: ListKind) -> Self {
        match kind {
            ListKind::Handling => Self::Handling,
            ListKind::Downtimes => Self::Downtimes,
        }
    }

    /// The handling or downtimes view it shows.
    pub(crate) fn list(self) -> Option<ListKind> {
        match self {
            Self::Handling => Some(ListKind::Handling),
            Self::Downtimes => Some(ListKind::Downtimes),
            Self::Events | Self::Health => None,
        }
    }

    /// The icon in its mark slot (health has the cluster's state dot).
    pub(crate) fn icon(self) -> Option<IconName> {
        match self {
            Self::Handling => Some(IconName::Users),
            Self::Downtimes => Some(IconName::CalendarClock),
            Self::Events => Some(IconName::Activity),
            Self::Health => None,
        }
    }
}

/// The cluster's state, as health's dot shows it (topic 06, by the
/// health page's rules: [`health::assess`]): critical while an endpoint
/// the connected node talks to is down or a number is critical, warning
/// while a node lags or a number needs a look, ok otherwise, unknown
/// before anything is known.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ClusterState {
    /// Every node is connected and every number is fine.
    Ok,
    /// A node lags, or a number needs a look (late checks, a queue that
    /// grows, latency, fewer checks).
    Warning,
    /// An endpoint is down, or a number is critical.
    Critical,
    /// Not connected yet, or no node is known.
    #[default]
    Unknown,
}

/// The cluster's state in `snapshot` at `now` (`connected`: the engine is
/// connected).
pub(crate) fn cluster_state(snapshot: &Snapshot, connected: bool, now: Timestamp) -> ClusterState {
    health::assess(snapshot, connected, now)
}

/// Whether what the health page shows for this environment is current
/// (no false green, PLAN.md §4.2): the connection is live by the same
/// timestamps the footer reads (events, the engine's ticks, no live data),
/// and the endpoints' states came within three status polls.
pub(crate) fn liveness(
    snapshot: &Snapshot,
    connection: &crate::app_state::ConnectionStatus,
    now: Timestamp,
) -> health::Liveness {
    let health = &snapshot.health;
    let as_of = health
        .endpoints_at
        .or_else(|| health.latest().map(|sample| sample.at));
    let old = !health.interval.is_zero()
        && health.endpoints_at.is_some_and(|at| {
            at.elapsed_until(now) > health.interval * 3 + std::time::Duration::from_secs(30)
        });
    if connection.health(now) == crate::app_state::connection::Health::Live && !old {
        health::Liveness::Live
    } else {
        health::Liveness::Stale { as_of }
    }
}

/// The sidebar's *health* dot (no false green, PLAN.md §4.2): the
/// cluster's state while what icygui shows is current; without that,
/// never green or grey once a session was live (yellow, or the last known
/// state when it was worse), and red once the engine stopped saying it
/// runs. Grey only before the first connection.
pub(crate) fn sidebar_state(
    snapshot: &Snapshot,
    connection: &crate::app_state::ConnectionStatus,
    now: Timestamp,
) -> ClusterState {
    if connection
        .engine_silent(now)
        .is_some_and(|silent| silent > crate::app_state::connection::ENGINE_STUCK_AFTER)
    {
        return ClusterState::Critical;
    }
    let state = cluster_state(snapshot, connection.is_connected(), now);
    if liveness(snapshot, connection, now).is_live() {
        return state;
    }
    if !connection.ever_connected && connection.no_data_for(now).is_none() {
        // Not connected yet: nothing is known.
        return state;
    }
    // The last known state, at least yellow.
    match health::assess(snapshot, true, now) {
        ClusterState::Critical => ClusterState::Critical,
        _ => ClusterState::Warning,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_know_their_views() {
        for kind in ListKind::ALL {
            assert_eq!(ClusterEntry::of_list(kind).list(), Some(kind));
        }
        assert_eq!(ClusterEntry::Events.list(), None);
        assert_eq!(ClusterEntry::Health.icon(), None, "health has a dot");
        let titles: Vec<&str> = ClusterEntry::ALL
            .iter()
            .map(|entry| entry.title())
            .collect();
        assert_eq!(titles, ["handling", "downtimes", "events", "health"]);
    }

    /// No false green: without live data the dot is yellow, never green
    /// or grey; a stuck engine turns it red.
    #[test]
    fn no_live_data_is_never_green_in_the_sidebar() {
        use std::sync::Arc;
        let since = Timestamp::from_unix_seconds(1_000.0);
        let now = Timestamp::from_unix_seconds(1_180.0);
        let mut connection = crate::app_state::ConnectionStatus::starting("master-01", None);
        let blind = Snapshot {
            trouble: Arc::new(ic_core::trouble::Trouble {
                alerts: Vec::new(),
                blind: Some(ic_core::trouble::Blind {
                    since,
                    reason: "connection lost".to_owned(),
                }),
            }),
            ..Snapshot::default()
        };
        assert_eq!(
            sidebar_state(&blind, &connection, now),
            ClusterState::Unknown
        );
        connection.on_snapshot_at(&blind, now);
        assert_eq!(
            sidebar_state(&blind, &connection, now),
            ClusterState::Warning
        );
        // An engine silent for longer than a minute: red.
        let mut silent = crate::app_state::ConnectionStatus::starting("master-01", None);
        silent.on_alive(since);
        assert_eq!(
            sidebar_state(&Snapshot::default(), &silent, now),
            ClusterState::Critical
        );
    }

    /// After a live session was lost (reconnecting), the dot is yellow,
    /// not grey, from the first moment; a stale event stream turns it
    /// yellow with the footer.
    #[test]
    fn a_lost_session_is_yellow_at_once() {
        let now = Timestamp::from_unix_seconds(2_000.0);
        let mut connection = crate::app_state::ConnectionStatus::starting("master-01", None);
        connection.on_state(ic_core::ConnectionState::Connected {
            node: crate::app_state::connection::full_node("master-01"),
            version: "r2.15.6".to_owned(),
            since: Timestamp::from_unix_seconds(1_000.0),
        });
        connection.last_event_at = Some(Timestamp::from_unix_seconds(1_999.0));
        assert_eq!(
            sidebar_state(&Snapshot::default(), &connection, now),
            ClusterState::Unknown,
            "no nodes known"
        );
        connection.on_state(ic_core::ConnectionState::Reconnecting {
            error: "connection reset".to_owned(),
            attempt: 1,
            retry_at: Timestamp::from_unix_seconds(2_004.0),
            untrusted: None,
        });
        assert_eq!(
            sidebar_state(&Snapshot::default(), &connection, now),
            ClusterState::Warning
        );
    }

    #[test]
    fn the_cluster_is_unknown_until_connected() {
        let now = Timestamp::from_unix_seconds(1_000.0);
        assert_eq!(
            cluster_state(&Snapshot::default(), false, now),
            ClusterState::Unknown
        );
        assert_eq!(
            cluster_state(&Snapshot::default(), true, now),
            ClusterState::Unknown
        );
    }
}
