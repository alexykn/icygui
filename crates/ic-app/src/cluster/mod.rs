//! The sidebar's fixed **cluster** section (topic 14, rounds 3 and 5;
//! topic 06): the whole environment's handling, downtimes and events (the
//! same views as a dashboard's, without a filter) and the cluster's health.
//! It sits at the top, above the groups; each entry has its mark in the
//! fixed dot slot and its count in the fixed count slot.

use gpui::{
    AnyElement, Context, Entity, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, StatefulInteractiveElement as _, Styled as _, Subscription, Window,
    div, prelude::FluentBuilder as _,
};
use ic_core::NodeState;
use ic_core::snapshot::Snapshot;
use ic_ui_kit::{ActiveTheme as _, EmptyState, Icon, IconName, Metrics, PaneHeader, StateDot, px};

use crate::app_state::AppState;
use crate::chrome::{Controls, WindowDrag};
use crate::lists::ListKind;
use crate::workspace::sidebar_reopen;

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

/// The cluster's state, as health's dot shows it (topic 06): critical
/// while an endpoint the connected node talks to is down, ok while every
/// one is connected, unknown before anything is known.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClusterState {
    /// Every node the connected node talks to is connected.
    Ok,
    /// An endpoint is down.
    Critical,
    /// Not connected yet, or no node is known.
    Unknown,
}

/// The cluster's state in `snapshot` (`connected`: the engine is
/// connected).
pub(crate) fn cluster_state(snapshot: &Snapshot, connected: bool) -> ClusterState {
    if !connected {
        return ClusterState::Unknown;
    }
    let nodes = snapshot.cluster_nodes();
    if nodes.is_empty() {
        return ClusterState::Unknown;
    }
    if nodes
        .iter()
        .any(|node| node.state == NodeState::Disconnected)
    {
        ClusterState::Critical
    } else {
        ClusterState::Ok
    }
}

/// The cluster section's *health* page: the cluster's nodes by zone, as
/// the connected node sees them, from the endpoints and zones icygui
/// already loads (topic 06; nothing is fetched for it).
pub(crate) struct HealthPage {
    state: Entity<AppState>,
    sidebar_open: bool,
    drag: WindowDrag,
    focus_handle: gpui::FocusHandle,
    _subscription: Subscription,
}

impl HealthPage {
    pub(crate) fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.observe(&state, |_, _, cx| cx.notify());
        Self {
            state,
            sidebar_open: true,
            drag: WindowDrag::default(),
            focus_handle: cx.focus_handle(),
            _subscription: subscription,
        }
    }

    /// Tells the page whether the sidebar is shown (the header then needs
    /// no window controls).
    pub(crate) fn set_sidebar_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.sidebar_open != open {
            self.sidebar_open = open;
            cx.notify();
        }
    }
}

impl gpui::Focusable for HealthPage {
    fn focus_handle(&self, _cx: &gpui::App) -> gpui::FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for HealthPage {
    #[expect(
        clippy::too_many_lines,
        reason = "one page, its sections in reading order"
    )]
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let state = self.state.read(cx);
        let snapshot = state.snapshot();
        let controls = Controls::of(window, cx);
        let environment = state
            .environment()
            .map(|environment| environment.name.clone())
            .unwrap_or_default();
        let seen_from = snapshot.node.as_ref().map(|node| node.name.clone());
        let mut header = PaneHeader::new("health-header")
            .padding(theme.metrics.list_padding)
            .title("cluster health")
            .subtitle(match &seen_from {
                Some(node) => format!("{environment} · seen from {node}"),
                None => environment,
            });
        if !self.sidebar_open {
            header = header.leading(sidebar_reopen(controls, theme));
        }
        let nodes = snapshot.cluster_nodes();
        let body: AnyElement = if nodes.is_empty() {
            EmptyState::new("No cluster nodes yet")
                .leading(
                    Icon::new(IconName::HeartPulse)
                        .size(px(20.))
                        .color(colors.text_muted),
                )
                .detail("They show once icygui is connected and knows the cluster's endpoints.")
                .max_width(px(560.))
                .into_any_element()
        } else {
            let mut rows: Vec<AnyElement> = Vec::new();
            let mut zone: Option<&str> = None;
            for node in &nodes {
                if zone != Some(node.zone.as_str()) {
                    zone = Some(node.zone.as_str());
                    let count = nodes.iter().filter(|other| other.zone == node.zone).count();
                    let worst = nodes
                        .iter()
                        .filter(|other| other.zone == node.zone)
                        .map(|other| other.state)
                        .max_by_key(|state| match state {
                            NodeState::Disconnected => 2,
                            NodeState::Unknown => 1,
                            NodeState::Connected => 0,
                        })
                        .unwrap_or(NodeState::Unknown);
                    rows.push(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .gap(px(14.))
                            .h(Metrics::with_rule(theme.metrics.group_row_height))
                            .px(theme.metrics.list_padding)
                            .bg(colors.row_header)
                            .border_b_1()
                            .border_color(colors.border_header)
                            .child(
                                div()
                                    .flex()
                                    .flex_none()
                                    .justify_center()
                                    .w(px(44.))
                                    .child(node_dot(worst, theme).size(px(9.))),
                            )
                            .child(
                                div()
                                    .text_size(theme.text.row)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(colors.text_emphasis)
                                    .child(node.zone.clone()),
                            )
                            .child(
                                div()
                                    .text_size(theme.text.small)
                                    .text_color(colors.text_faint)
                                    .child(if count == 1 {
                                        "1 endpoint".to_owned()
                                    } else {
                                        format!("{count} endpoints")
                                    }),
                            )
                            .into_any_element(),
                    );
                }
                let this_node = seen_from.as_deref() == Some(node.name.as_str());
                rows.push(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(px(14.))
                        .h(Metrics::with_rule(px(36.)))
                        .px(theme.metrics.list_padding)
                        .border_b_1()
                        .border_color(colors.border_row)
                        .when(this_node, |row| row.bg(colors.row_selected))
                        .child(
                            div()
                                .flex()
                                .flex_none()
                                .justify_center()
                                .w(px(44.))
                                .child(node_dot(node.state, theme).size(px(9.))),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(theme.text.row)
                                .text_color(colors.text_strong)
                                .child(node.name.clone()),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_size(theme.text.small)
                                .text_color(colors.text_faint)
                                .child(match (this_node, node.state) {
                                    (true, _) => "this node",
                                    (false, NodeState::Connected) => "connected",
                                    (false, NodeState::Disconnected) => "not connected",
                                    (false, NodeState::Unknown) => "not seen from here",
                                }),
                        )
                        .into_any_element(),
                );
            }
            div()
                .id("health-nodes")
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .children(rows)
                .into_any_element()
        };
        div()
            .id("health-page")
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(
                self.drag
                    .attach(div().id("health-header-drag").child(header), controls),
            )
            .child(body)
    }
}

/// A node's dot: green connected, red down, grey unknown.
fn node_dot(state: NodeState, theme: &ic_ui_kit::Theme) -> StateDot {
    match state {
        NodeState::Connected => StateDot::with_color(theme.states.fill.ok),
        NodeState::Disconnected => StateDot::with_color(theme.states.fill.critical),
        NodeState::Unknown => StateDot::with_color(theme.states.fill.pending),
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

    #[test]
    fn the_cluster_is_unknown_until_connected() {
        assert_eq!(
            cluster_state(&Snapshot::default(), false),
            ClusterState::Unknown
        );
        assert_eq!(
            cluster_state(&Snapshot::default(), true),
            ClusterState::Unknown
        );
    }
}
