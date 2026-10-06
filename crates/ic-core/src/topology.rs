//! Which node of its cluster the engine talks to, and how much of the
//! cluster that node sees (ENV-12).
//!
//! An environment lists one or more API URLs in order of preference: one
//! for a single master or a load balancer, one per node for an HA pair or a
//! master with satellites. On every connect the engine asks the node behind
//! a URL for its name (`/v1/status/IcingaApplication`: `node_name`, which
//! names its `Endpoint`) and finds the zone listing that endpoint
//! (`/v1/objects/zones`; an endpoint is a member of exactly one zone,
//! Icinga refuses a configuration where it isn't, so the `Endpoint` object
//! itself needn't be read):
//!
//! - a node in a top-level zone (no parent, not global) has every object
//!   of the cluster: the **full** view;
//! - a node in a child zone (a satellite) has only the objects of its zone
//!   and the zones below it: a **partial** view, allowed but always
//!   labelled;
//! - when the API user may not read the status or the zones, or the node
//!   is in no zone the user may read, the view is **not verified**: the
//!   engine says so instead of guessing.
//!
//! The engine prefers nodes with the full view and takes a partial one
//! only while none of those answers; meanwhile it tries the others again,
//! gently (`Tuning::probe_initial`, doubling up to `Tuning::probe_max`,
//! with jitter), and switches back when one answers.

use ic_model::Zone;

/// How much of its cluster the connected node sees.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ClusterView {
    /// The node is in a top-level zone (no parent, not global): it has
    /// every object of the cluster.
    Full,
    /// The node is in a child zone: it has only the objects of that zone
    /// and the zones below it.
    Partial {
        /// The node's zone (`ams`).
        zone: String,
    },
    /// Not known: the API user may not read the status or the zones, the
    /// node is in no zone the user may read, or reading them failed.
    Unverified {
        /// Why, for the connection details.
        reason: String,
    },
}

impl ClusterView {
    /// Whether the node sees the whole cluster.
    #[must_use]
    pub fn is_full(&self) -> bool {
        matches!(self, Self::Full)
    }

    /// The view in a few words: `full view`, `partial view: zone ams`,
    /// `view not verified`.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Full => "full view".to_owned(),
            Self::Partial { zone } => format!("partial view: zone {zone}"),
            Self::Unverified { .. } => "view not verified".to_owned(),
        }
    }

    /// The order of preference: full, then not verified (it may be full),
    /// then partial (known not to be).
    pub(crate) fn rank(&self) -> u8 {
        match self {
            Self::Full => 0,
            Self::Unverified { .. } => 1,
            Self::Partial { .. } => 2,
        }
    }

    /// Whether data from a node with this view can stand for data from a
    /// node with `other`: the same view of the same zone (the nodes of an
    /// HA zone, or nodes neither of which could be verified).
    pub(crate) fn same_data(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Full, Self::Full) | (Self::Unverified { .. }, Self::Unverified { .. }) => true,
            (Self::Partial { zone }, Self::Partial { zone: other }) => zone == other,
            _ => false,
        }
    }
}

/// The node the engine is connected to (or was, while it reconnects).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectedNode {
    /// The configured URL it was reached through, as written in
    /// `Environment::urls`.
    pub url: String,
    /// That URL's position in the environment's list (0: the first
    /// choice).
    pub url_index: usize,
    /// Icinga's node name (`master-01`), or the URL's host when the API
    /// user may not read the status.
    pub name: String,
    /// The node's zone, when known.
    pub zone: Option<String>,
    /// How much of the cluster it sees.
    pub view: ClusterView,
    /// The other URLs this connect tried and didn't take, in order of
    /// preference, with why (unreachable, a refused login, a certificate,
    /// a partial view): `("master-01:5665", "connection refused")`, the
    /// URL as [`ic_config::ApiUrl::label`] shows it. URLs after the one
    /// taken that weren't needed aren't listed.
    pub passed_over: Vec<(String, String)>,
}

/// The node `node_name`'s zone and view in `zones` (see the module notes).
pub(crate) fn classify(node_name: &str, zones: &[Zone]) -> (Option<String>, ClusterView) {
    let Some(zone) = zones
        .iter()
        .find(|zone| zone.endpoints.iter().any(|endpoint| endpoint == node_name))
    else {
        return (
            None,
            ClusterView::Unverified {
                reason: format!("{node_name} is in no zone the API user may read"),
            },
        );
    };
    let view = if zone.global {
        // Icinga doesn't allow endpoints in a global zone.
        ClusterView::Unverified {
            reason: format!("{node_name} is listed in the global zone {}", zone.name),
        }
    } else if zone.parent.is_some() {
        ClusterView::Partial {
            zone: zone.name.clone(),
        }
    } else {
        ClusterView::Full
    };
    (Some(zone.name.clone()), view)
}

/// Whether a node with view `candidate` at `candidate_index` is better
/// than the current one: a fuller view, or, between nodes neither of which
/// could be verified, an earlier place in the order of preference. Two
/// nodes with the full view are equal: the engine stays where it is
/// (switching would only cost the master a new stream).
pub(crate) fn better(
    candidate: &ClusterView,
    candidate_index: usize,
    current: &ClusterView,
    current_index: usize,
) -> bool {
    candidate.rank() < current.rank()
        || (matches!(current, ClusterView::Unverified { .. })
            && candidate.rank() <= current.rank()
            && candidate_index < current_index)
}

/// The URLs (by index) worth trying while connected to `current` at
/// `current_index`: none with the full view; every other one with a
/// partial view (any of them may be a full one); the ones before it with
/// an unverified view (the user's order is all there is to go by).
pub(crate) fn candidates(current: &ClusterView, current_index: usize, urls: usize) -> Vec<usize> {
    match current {
        ClusterView::Full => Vec::new(),
        ClusterView::Partial { .. } => (0..urls).filter(|&index| index != current_index).collect(),
        ClusterView::Unverified { .. } => (0..current_index.min(urls)).collect(),
    }
}

/// The order in which a connect tries the URLs: `first` (a node a probe
/// found better), then the rest in the order of preference.
pub(crate) fn walk_order(urls: usize, first: Option<usize>) -> Vec<usize> {
    let first = first.filter(|&index| index < urls);
    first
        .into_iter()
        .chain((0..urls).filter(|&index| Some(index) != first))
        .collect()
}

/// The most nodes [`cluster_nodes`] lists: the masters and satellites of
/// any real cluster, and one request's worth of names when their states
/// are asked for ([`ic_api::NAMES_PER_REQUEST`]).
pub(crate) const MAX_CLUSTER_NODES: usize = ic_api::NAMES_PER_REQUEST;

/// A master or satellite of the cluster, for the switcher's node list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClusterNode {
    /// The endpoint's name (`master-01`).
    pub name: String,
    /// Its zone (`master`, `ams`).
    pub zone: String,
    /// Whether it's up, as far as the connected node can tell.
    pub state: NodeState,
}

/// A cluster node's state as the node icygui is connected to sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NodeState {
    /// The node icygui is connected to, or one connected to it.
    Connected,
    /// A node the connected node talks to directly (its own zone, the
    /// parent zone or a child zone) that isn't connected.
    Disconnected,
    /// A node further away (a satellite's own satellites): the connected
    /// node has no connection of its own to it, so its state isn't known
    /// here.
    Unknown,
}

/// The masters and satellites of the cluster, top-level zone first, then
/// each zone's children in name order (depth first), each zone's endpoints
/// in its own order; at most [`MAX_CLUSTER_NODES`].
///
/// Agents are left out: by Icinga's convention (the node wizard's default)
/// an agent's zone is a leaf named like its only endpoint. A zone with
/// child zones, several endpoints or a name of its own is a master's or a
/// satellite's; so is the zone of the connected node and every zone above
/// it. Without the zones (no `objects/query/Zone`) the connected node is
/// all there is to list.
#[must_use]
pub(crate) fn cluster_nodes(
    zones: &[Zone],
    endpoints: &[ic_model::Endpoint],
    connected: Option<&ConnectedNode>,
) -> Vec<ClusterNode> {
    let local = connected.map(|node| node.name.as_str());
    let local_zone = connected.and_then(|node| node.zone.as_deref());
    let zones: Vec<&Zone> = zones.iter().filter(|zone| !zone.global).collect();
    let zone_named = |name: &str| zones.iter().copied().find(|zone| zone.name == name);
    let children = |name: &str| -> Vec<&Zone> {
        let mut children: Vec<&Zone> = zones
            .iter()
            .copied()
            .filter(|zone| zone.parent.as_deref() == Some(name))
            .collect();
        children.sort_by(|a, b| a.name.cmp(&b.name));
        children
    };
    // The connected node's zone and every zone above it.
    let mut path: Vec<&str> = Vec::new();
    let mut next = local_zone;
    while let Some(name) = next {
        if path.contains(&name) {
            break;
        }
        path.push(name);
        next = zone_named(name).and_then(|zone| zone.parent.as_deref());
    }
    let agent_like = |zone: &Zone| {
        children(&zone.name).is_empty()
            && zone.endpoints.len() == 1
            && zone.endpoints[0] == zone.name
            && !path.contains(&zone.name.as_str())
    };
    // Depth first from the top-level zones.
    let mut order: Vec<&Zone> = Vec::new();
    let mut stack: Vec<&Zone> = zones
        .iter()
        .copied()
        .filter(|zone| zone.parent.is_none())
        .collect();
    stack.sort_by(|a, b| b.name.cmp(&a.name));
    while let Some(zone) = stack.pop() {
        if order.iter().any(|seen| seen.name == zone.name) {
            continue;
        }
        order.push(zone);
        stack.extend(children(&zone.name).into_iter().rev());
    }
    // A node's state is known for its own zone, the parent and the
    // children.
    let near = |zone: &Zone| match local_zone {
        Some(local_zone) => {
            zone.name == local_zone
                || zone.parent.as_deref() == Some(local_zone)
                || zone_named(local_zone).and_then(|own| own.parent.as_deref())
                    == Some(zone.name.as_str())
        }
        None => false,
    };
    let mut nodes: Vec<ClusterNode> = Vec::new();
    for zone in order.into_iter().filter(|zone| !agent_like(zone)) {
        for name in &zone.endpoints {
            let state = if Some(name.as_str()) == local {
                NodeState::Connected
            } else if !near(zone) {
                NodeState::Unknown
            } else if endpoints
                .iter()
                .any(|endpoint| endpoint.name == *name && endpoint.connected)
            {
                NodeState::Connected
            } else {
                NodeState::Disconnected
            };
            nodes.push(ClusterNode {
                name: name.clone(),
                zone: zone.name.clone(),
                state,
            });
        }
    }
    if let (Some(node), true) = (connected, nodes.is_empty()) {
        nodes.push(ClusterNode {
            name: node.name.clone(),
            zone: node.zone.clone().unwrap_or_default(),
            state: NodeState::Connected,
        });
    }
    nodes.truncate(MAX_CLUSTER_NODES);
    nodes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zone(name: &str, parent: Option<&str>, endpoints: &[&str], global: bool) -> Zone {
        Zone {
            name: name.to_owned(),
            parent: parent.map(str::to_owned),
            endpoints: endpoints
                .iter()
                .map(|&endpoint| endpoint.to_owned())
                .collect(),
            global,
        }
    }

    fn cluster() -> Vec<Zone> {
        vec![
            zone("master", None, &["master-01", "master-02"], false),
            zone("ams", Some("master"), &["sat-ams-01"], false),
            zone("ams-agents", Some("ams"), &["agent-01"], false),
            zone("global-templates", None, &[], true),
        ]
    }

    #[test]
    fn nodes_in_the_top_level_zone_see_everything() {
        for node in ["master-01", "master-02"] {
            assert_eq!(
                classify(node, &cluster()),
                (Some("master".to_owned()), ClusterView::Full)
            );
        }
        // A single master in its own zone (Icinga's default `master`).
        assert_eq!(
            classify(
                "icinga-master",
                &[zone("master", None, &["icinga-master"], false)]
            ),
            (Some("master".to_owned()), ClusterView::Full)
        );
    }

    #[test]
    fn nodes_in_child_zones_see_part_of_it() {
        assert_eq!(
            classify("sat-ams-01", &cluster()),
            (
                Some("ams".to_owned()),
                ClusterView::Partial {
                    zone: "ams".to_owned()
                }
            )
        );
        assert_eq!(
            classify("agent-01", &cluster()).1,
            ClusterView::Partial {
                zone: "ams-agents".to_owned()
            }
        );
    }

    #[test]
    fn nodes_in_no_readable_zone_are_not_verified() {
        let (found, view) = classify("master-03", &cluster());
        assert_eq!(found, None);
        assert!(matches!(view, ClusterView::Unverified { .. }), "{view:?}");
        // A filtered permission that hides the node's zone.
        let (_, view) = classify("master-01", &cluster()[1..]);
        assert!(matches!(view, ClusterView::Unverified { .. }), "{view:?}");
        let (_, view) = classify("x", &[zone("g", None, &["x"], true)]);
        assert!(matches!(view, ClusterView::Unverified { .. }), "{view:?}");
    }

    #[test]
    fn views_have_labels() {
        assert_eq!(ClusterView::Full.label(), "full view");
        assert_eq!(
            ClusterView::Partial {
                zone: "ams".to_owned()
            }
            .label(),
            "partial view: zone ams"
        );
        assert_eq!(
            ClusterView::Unverified {
                reason: "no permission".to_owned()
            }
            .label(),
            "view not verified"
        );
        assert!(ClusterView::Full.is_full());
    }

    #[test]
    fn fuller_views_are_better_and_full_ones_equal() {
        let full = ClusterView::Full;
        let partial = ClusterView::Partial {
            zone: "ams".to_owned(),
        };
        let unverified = ClusterView::Unverified {
            reason: String::new(),
        };
        assert!(better(&full, 1, &partial, 0));
        assert!(better(&full, 0, &partial, 1));
        assert!(better(&unverified, 1, &partial, 0));
        assert!(!better(&partial, 0, &partial, 1));
        assert!(
            !better(&full, 0, &full, 1),
            "no switching between full nodes"
        );
        assert!(better(&full, 1, &unverified, 0));
        assert!(better(&unverified, 0, &unverified, 1), "the user's order");
        assert!(!better(&unverified, 2, &unverified, 1));
        assert!(!better(&partial, 0, &unverified, 1));
    }

    #[test]
    fn only_views_short_of_full_look_for_others() {
        let partial = ClusterView::Partial {
            zone: "ams".to_owned(),
        };
        let unverified = ClusterView::Unverified {
            reason: String::new(),
        };
        assert!(candidates(&ClusterView::Full, 1, 3).is_empty());
        assert_eq!(candidates(&partial, 1, 3), [0, 2]);
        assert_eq!(candidates(&unverified, 2, 3), [0, 1]);
        assert!(candidates(&unverified, 0, 3).is_empty());
    }

    #[test]
    fn walks_start_with_the_chosen_url() {
        assert_eq!(walk_order(3, None), [0, 1, 2]);
        assert_eq!(walk_order(3, Some(2)), [2, 0, 1]);
        assert_eq!(walk_order(3, Some(7)), [0, 1, 2]);
        assert!(walk_order(0, None).is_empty());
    }

    #[test]
    fn views_of_the_same_data() {
        let ams = ClusterView::Partial {
            zone: "ams".to_owned(),
        };
        let fra = ClusterView::Partial {
            zone: "fra".to_owned(),
        };
        assert!(ClusterView::Full.same_data(&ClusterView::Full));
        assert!(ams.same_data(&ams.clone()));
        assert!(!ams.same_data(&fra));
        assert!(!ClusterView::Full.same_data(&ams));
    }

    fn node(name: &str, zone: &str) -> ConnectedNode {
        ConnectedNode {
            url: format!("https://{name}:5665"),
            url_index: 0,
            name: name.to_owned(),
            zone: Some(zone.to_owned()),
            view: ClusterView::Full,
            passed_over: Vec::new(),
        }
    }

    fn endpoint(name: &str, connected: bool) -> ic_model::Endpoint {
        ic_model::Endpoint {
            name: name.to_owned(),
            zone: String::new(),
            connected,
        }
    }

    #[test]
    fn masters_and_satellites_are_listed_agents_left_out() {
        let zones = vec![
            zone("master", None, &["master-01", "master-02"], false),
            zone("ams", Some("master"), &["sat-ams-01", "sat-ams-02"], false),
            zone("fra", Some("master"), &["sat-fra-01"], false),
            zone(
                "agent-01.example.com",
                Some("ams"),
                &["agent-01.example.com"],
                false,
            ),
            zone("edge", Some("ams"), &["sat-edge-01"], false),
            zone("global-templates", None, &[], true),
        ];
        let endpoints = vec![
            endpoint("master-01", false),
            endpoint("master-02", true),
            endpoint("sat-ams-01", true),
            endpoint("sat-ams-02", false),
            endpoint("sat-fra-01", true),
            endpoint("sat-edge-01", true),
        ];
        let nodes = cluster_nodes(&zones, &endpoints, Some(&node("master-01", "master")));
        let listed: Vec<(&str, &str, NodeState)> = nodes
            .iter()
            .map(|node| (node.name.as_str(), node.zone.as_str(), node.state))
            .collect();
        assert_eq!(
            listed,
            [
                ("master-01", "master", NodeState::Connected),
                ("master-02", "master", NodeState::Connected),
                ("sat-ams-01", "ams", NodeState::Connected),
                ("sat-ams-02", "ams", NodeState::Disconnected),
                // A satellite's satellite: the master has no connection of
                // its own to it.
                ("sat-edge-01", "edge", NodeState::Unknown),
                ("sat-fra-01", "fra", NodeState::Connected),
            ]
        );
        // Seen from a satellite: its parent zone and its children are
        // near, the sibling satellites aren't.
        let nodes = cluster_nodes(&zones, &endpoints, Some(&node("sat-ams-01", "ams")));
        let state = |name: &str| nodes.iter().find(|node| node.name == name).unwrap().state;
        assert_eq!(state("sat-ams-01"), NodeState::Connected);
        assert_eq!(state("master-01"), NodeState::Disconnected);
        assert_eq!(state("sat-edge-01"), NodeState::Connected);
        assert_eq!(state("sat-fra-01"), NodeState::Unknown);
    }

    #[test]
    fn without_the_zones_the_connected_node_is_listed() {
        let nodes = cluster_nodes(&[], &[], Some(&node("master-01", "master")));
        assert_eq!(
            nodes,
            [ClusterNode {
                name: "master-01".to_owned(),
                zone: "master".to_owned(),
                state: NodeState::Connected,
            }]
        );
        assert!(cluster_nodes(&[], &[], None).is_empty());
    }
}
