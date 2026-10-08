//! The sidebar's fixed **cluster** section (topic 14, rounds 3 and 5;
//! topic 06): the whole environment's handling, downtimes and events (the
//! same views as a dashboard's, without a filter) and the cluster's health.
//! It sits at the top, above the groups; each entry has its mark in the
//! fixed dot slot and its count in the fixed count slot.

use ic_core::snapshot::Snapshot;
use ic_model::Timestamp;
use ic_ui_kit::IconName;

use crate::lists::ListKind;

pub(crate) mod health;
mod page;
mod spark;

pub(crate) use page::HealthPage;

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
