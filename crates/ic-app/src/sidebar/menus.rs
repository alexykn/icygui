//! The sidebar's popup menus: a group's and a dashboard's `···` (DASH-02,
//! DASH-03), the footer's `+` (new dashboard or group, import and export;
//! DASH-06), and the connection details with the environment switcher
//! (ENV-01, ENV-06).

use std::fmt::Write as _;
use std::time::Instant;

use gpui::{
    ClickEvent, Context, FontWeight, InteractiveElement as _, IntoElement as _, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, div,
};
use ic_config::DashboardGroup;
use ic_model::{Timestamp, format_compact};
use ic_rules::{DashboardRef, ScopeSetting};
use ic_ui_kit::{
    ActiveTheme as _, CHIP_HEIGHT, Chip, Dismissal, IconName, ItemAction, Link, Menu, MenuItem,
    Tooltip, px,
};

use super::{RenameTarget, Sidebar, SidebarEvent};
use crate::app_state::connection::ViewMarker;
use crate::app_state::environments::url_summary;
use crate::app_state::{AppState, Health, permissions};
use crate::notifications::{PauseChoice, when};
use crate::settings::ScopeKey;
use ic_core::NodeState;

/// The connection details' width (fixed: a long error or another
/// environment's state never widens it, so nothing in it moves sideways).
const DETAILS_WIDTH: f32 = 320.;
/// The same with several environments (the switcher's mute row).
const DETAILS_WIDTH_SEVERAL: f32 = 360.;

/// The heights the switcher's budget counts with (the theme's: a menu
/// item, a detail line and the gap between lines, the parts around the
/// lists), so the popover fits above the footer in any window.
mod budget {
    /// A menu item (an environment or node row, *Reload*, *add*).
    pub(super) const ROW: f32 = 28.;
    /// A detail line (12 px text) and the gap after it.
    pub(super) const LINE: f32 = 20.;
    /// The detail lines the budget keeps room for before the lists get
    /// less (status, node, view, two URLs passed over, version, last
    /// event, API user and one more).
    pub(super) const LINES: f32 = 9.;
    /// Under the window's height: the footer, the gap above it and the
    /// margin the popover keeps from the window's top.
    pub(super) const OUTSIDE: f32 = 50.;
    /// The title, its separator, *Reload* and its separator.
    pub(super) const TOP: f32 = 42. + 9. + ROW + 9.;
    /// Around the node list: its separator and label, and the *cluster
    /// health* row under it.
    pub(super) const NODES: f32 = 9. + 23. + ROW;
    /// Around the environment list: its separator and label, the mute row
    /// (several environments), a separator, *add*, the card's padding and
    /// border.
    pub(super) const BOTTOM: f32 = 9. + 23. + 30. + 9. + ROW + 10.;
    /// The fewest rows a list shows before it scrolls.
    pub(super) const MIN_ROWS: usize = 3;
    /// The least height the detail lines get.
    pub(super) const MIN_LINES: f32 = 60.;
}

/// How the switcher's lists fit a window (see [`switcher_budget`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct SwitcherBudget {
    /// The environment rows shown at once (the list scrolls beyond).
    pub(super) environments: usize,
    /// The cluster node rows shown at once (the list scrolls beyond).
    pub(super) nodes: usize,
    /// The most the detail lines take (they scroll beyond).
    pub(super) lines: f32,
}

/// The switcher's lists in a window `viewport` high, for `environments`
/// environments and `nodes` cluster nodes: how many rows each list shows
/// (whole rows; it scrolls beyond; at least three of each, one more for
/// each in turn as room allows) and how tall the detail lines may be
/// (they scroll beyond). None of it depends on the connection's state, so
/// nothing in the popover moves when that changes; a short window shows
/// fewer rows and lines, never fewer controls.
pub(super) fn switcher_budget(viewport: f32, environments: usize, nodes: usize) -> SwitcherBudget {
    use budget::{BOTTOM, LINE, LINES, MIN_LINES, MIN_ROWS, NODES, OUTSIDE, ROW, TOP};
    let available = viewport - OUTSIDE;
    let around = TOP + BOTTOM + if nodes > 0 { NODES } else { 0. };
    let room = available - around - LINES * LINE;
    let environments = environments.max(1);
    let mut shown = (MIN_ROWS.min(environments), MIN_ROWS.min(nodes));
    loop {
        let before = shown;
        if shown.0 < environments && height_of(shown.0 + shown.1 + 1, ROW) <= room {
            shown.0 += 1;
        }
        if shown.1 < nodes && height_of(shown.0 + shown.1 + 1, ROW) <= room {
            shown.1 += 1;
        }
        if shown == before {
            break;
        }
    }
    SwitcherBudget {
        environments: shown.0,
        nodes: shown.1,
        lines: (available - around - height_of(shown.0 + shown.1, ROW) - 8.).max(MIN_LINES),
    }
}

/// `count` rows of `height`.
#[expect(
    clippy::cast_precision_loss,
    reason = "a count of environments, far below 2^23"
)]
fn height_of(count: usize, height: f32) -> f32 {
    count as f32 * height
}

/// The notification settings a scope can take here (a custom rule is
/// edited in the notification settings).
const SCOPE_SETTINGS: [(&str, &str, ScopeSetting); 3] = [
    ("inherit", "inherit", ScopeSetting::Inherit),
    ("on", "on", ScopeSetting::On),
    ("off", "off", ScopeSetting::Off),
];

impl Sidebar {
    /// A listener that closes the open menu on a press outside it.
    fn dismiss_listener(
        cx: &Context<Self>,
    ) -> impl Fn(&Dismissal, &mut gpui::Window, &mut gpui::App) + 'static {
        cx.listener(|this, dismissal: &Dismissal, _, cx| {
            this.menus.dismissed(*dismissal);
            cx.notify();
        })
    }

    /// A menu item that closes the menu and emits `event`.
    fn emit_item(
        id: impl Into<gpui::ElementId>,
        label: &'static str,
        event: SidebarEvent,
        cx: &Context<Self>,
    ) -> MenuItem {
        MenuItem::new(id, label).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            this.menus.close();
            cx.emit(event.clone());
            cx.notify();
        }))
    }

    /// A menu item that closes the menu and changes the state with
    /// `change`.
    fn state_item(
        id: impl Into<gpui::ElementId>,
        label: impl Into<SharedString>,
        cx: &Context<Self>,
        change: impl Fn(&mut AppState) -> bool + 'static,
    ) -> MenuItem {
        MenuItem::new(id, label).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            this.menus.close();
            this.state.update(cx, |state, cx| {
                if change(state) {
                    cx.notify();
                }
            });
            cx.notify();
        }))
    }

    /// A group's `···` menu (DASH-02).
    pub(super) fn group_menu(
        group: &DashboardGroup,
        index: usize,
        group_count: usize,
        cx: &Context<Self>,
    ) -> Menu {
        let id = group.id.clone();
        let rename = RenameTarget::Group(id.clone());
        let mut menu = Menu::new("group-menu")
            .item(Self::emit_item(
                "group-new-dashboard",
                "new dashboard",
                SidebarEvent::NewDashboard(Some(id.clone())),
                cx,
            ))
            .item(
                MenuItem::new("group-rename", "rename").on_click(cx.listener(
                    move |this, _: &ClickEvent, window, cx| {
                        this.start_rename(rename.clone(), window, cx);
                    },
                )),
            )
            .item({
                let after = id.clone();
                Self::state_item("group-new-group", "new group", cx, move |state| {
                    state.create_group("", Some(&after)).is_some()
                })
            })
            .separator();
        let up = id.clone();
        let down = id.clone();
        let toggle = id.clone();
        menu = menu
            .item(
                Self::state_item("group-move-up", "move up", cx, move |state| {
                    state.move_group(&up, -1)
                })
                .disabled(index == 0),
            )
            .item(
                Self::state_item("group-move-down", "move down", cx, move |state| {
                    state.move_group(&down, 1)
                })
                .disabled(index + 1 >= group_count),
            )
            .item(Self::state_item(
                "group-collapse",
                if group.collapsed {
                    "expand"
                } else {
                    "collapse"
                },
                cx,
                move |state| state.toggle_group(&toggle),
            ))
            .separator()
            .label("notifications");
        for (key, label, setting) in SCOPE_SETTINGS {
            let target = id.clone();
            let checked = group.notifications == setting;
            menu = menu.item(
                Self::state_item(format!("group-notify-{key}"), label, cx, move |state| {
                    state.set_group_notifications(&target, setting.clone())
                })
                .checked(checked),
            );
        }
        menu = menu.item(
            Self::emit_item(
                "group-notify-custom",
                "custom rule",
                SidebarEvent::CustomRule(ScopeKey::Group(id.clone())),
                cx,
            )
            .checked(matches!(group.notifications, ScopeSetting::Custom(_))),
        );
        menu.separator()
            .item(Self::emit_item(
                "group-export",
                "export group",
                SidebarEvent::ExportGroups(vec![id.clone()]),
                cx,
            ))
            .item(Self::emit_item(
                "group-delete",
                "delete group",
                SidebarEvent::DeleteGroup(id),
                cx,
            ))
            .on_dismiss(Self::dismiss_listener(cx))
    }

    /// A dashboard's `···` menu (DASH-03).
    pub(super) fn dashboard_menu(&self, reference: &DashboardRef, cx: &Context<Self>) -> Menu {
        let state = self.state.read(cx);
        let groups = state.groups();
        let Some((group, dashboard)) = state.dashboard(reference) else {
            return Menu::new("dashboard-menu").on_dismiss(Self::dismiss_listener(cx));
        };
        let index = group
            .dashboards
            .iter()
            .position(|candidate| candidate.id == dashboard.id)
            .unwrap_or(0);
        let last = group.dashboards.len().saturating_sub(1);
        let rename = RenameTarget::Dashboard(reference.clone());
        let mut menu = Menu::new("dashboard-menu")
            .item(Self::emit_item(
                "dashboard-edit",
                "edit dashboard",
                SidebarEvent::EditDashboard(reference.clone()),
                cx,
            ))
            .item(
                MenuItem::new("dashboard-rename", "rename").on_click(cx.listener(
                    move |this, _: &ClickEvent, window, cx| {
                        this.start_rename(rename.clone(), window, cx);
                    },
                )),
            )
            .item({
                let target = reference.clone();
                Self::state_item("dashboard-duplicate", "duplicate", cx, move |state| {
                    state.duplicate_dashboard(&target).is_some()
                })
            })
            .separator();
        let up = reference.clone();
        let down = reference.clone();
        menu = menu
            .item(
                Self::state_item("dashboard-move-up", "move up", cx, move |state| {
                    state.move_dashboard(&up, -1)
                })
                .disabled(index == 0),
            )
            .item(
                Self::state_item("dashboard-move-down", "move down", cx, move |state| {
                    state.move_dashboard(&down, 1)
                })
                .disabled(index >= last),
            );
        let others: Vec<&DashboardGroup> = groups
            .iter()
            .filter(|other| other.id != reference.group_id)
            .collect();
        if !others.is_empty() {
            menu = menu.label("move to");
            for other in others {
                let target = reference.clone();
                let group_id = other.id.clone();
                menu = menu.item(Self::state_item(
                    gpui::ElementId::Name(format!("dashboard-move-to-{}", other.id).into()),
                    other.name.clone(),
                    cx,
                    move |state| state.move_dashboard_to(&target, &group_id).is_some(),
                ));
            }
        }
        menu = menu.separator().label("notifications");
        for (key, label, setting) in SCOPE_SETTINGS {
            let target = reference.clone();
            let checked = dashboard.notifications == setting;
            menu = menu.item(
                Self::state_item(format!("dashboard-notify-{key}"), label, cx, move |state| {
                    state.set_dashboard_notifications(&target, setting.clone())
                })
                .checked(checked),
            );
        }
        menu = menu.item(
            Self::emit_item(
                "dashboard-notify-custom",
                "custom rule",
                SidebarEvent::CustomRule(ScopeKey::Dashboard(
                    reference.group_id.clone(),
                    reference.dashboard_id.clone(),
                )),
                cx,
            )
            .checked(matches!(dashboard.notifications, ScopeSetting::Custom(_))),
        );
        menu.separator()
            .item(Self::emit_item(
                "dashboard-delete",
                "delete dashboard",
                SidebarEvent::DeleteDashboard(reference.clone()),
                cx,
            ))
            .on_dismiss(Self::dismiss_listener(cx))
    }

    /// The footer's `+` menu.
    pub(super) fn footer_menu(&self, cx: &Context<Self>) -> Menu {
        let has_environment = self.state.read(cx).environment().is_some();
        let has_groups = !self.state.read(cx).groups().is_empty();
        Menu::new("footer-menu")
            .item(
                Self::emit_item(
                    "footer-new-dashboard",
                    "new dashboard",
                    SidebarEvent::NewDashboard(None),
                    cx,
                )
                .key_hint(new_key())
                .disabled(!has_environment),
            )
            .item(
                MenuItem::new("footer-new-group", "new group")
                    .disabled(!has_environment)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.menus.close();
                        let created = this
                            .state
                            .update(cx, |state, _| state.create_group("", None));
                        if let Some(id) = created {
                            this.start_rename(RenameTarget::Group(id), window, cx);
                        }
                        cx.notify();
                    })),
            )
            .separator()
            .item(
                Self::emit_item(
                    "footer-import",
                    "import dashboards",
                    SidebarEvent::ImportGroups,
                    cx,
                )
                .disabled(!has_environment),
            )
            .item(
                Self::emit_item(
                    "footer-export",
                    "export all dashboards",
                    SidebarEvent::ExportGroups(Vec::new()),
                    cx,
                )
                .disabled(!has_groups),
            )
            .on_dismiss(Self::dismiss_listener(cx))
    }

    /// The connection details above the footer status (ENV-06): the
    /// environment, the endpoint and its version, the state, the last
    /// event, the API user, the cluster's nodes and "Reload from Icinga";
    /// then the environment switcher (ENV-01) with "add environment".
    pub(super) fn details_menu(&self, now: Timestamp, cx: &Context<Self>) -> Menu {
        let theme = cx.theme();
        let colors = theme.colors;
        let state = self.state.read(cx);
        let connection = state.connection();
        // With several environments, room for the mute row's name next to
        // its pause chips.
        let width = if state.environments().len() > 1 {
            DETAILS_WIDTH_SEVERAL
        } else {
            DETAILS_WIDTH
        };
        let nodes = node_rows(state);
        // In the design's pixels: the popover's rows scale with the
        // interface size, the window doesn't.
        let budget = switcher_budget(
            f32::from(self.viewport) / ic_ui_kit::scale(),
            state.environments().len(),
            nodes.len(),
        );
        let mut menu = Menu::new("connection-details").status_popover(px(width));
        match state.environment() {
            Some(environment) => {
                let title = if state.is_demo_environment() {
                    format!("{} (demo)", environment.name)
                } else {
                    environment.name.clone()
                };
                menu = menu.element(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(
                            div()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(colors.text_strong)
                                .child(title),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_size(theme.text.small)
                                .text_color(colors.text_muted)
                                .child(connection.node.as_ref().map_or_else(
                                    || url_summary(environment),
                                    |node| node.url.clone(),
                                )),
                        ),
                );
                let lines = detail_lines(state, now)
                    .into_iter()
                    .enumerate()
                    .map(|(index, (key, value))| detail_line(index, key, value, theme));
                menu = menu.separator().element(
                    div()
                        .id("connection-detail-lines")
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .max_h(px(budget.lines))
                        .overflow_y_scroll()
                        .children(lines),
                );
                if !nodes.is_empty() {
                    menu = menu
                        .separator()
                        .label(list_label("nodes", nodes.len(), budget.nodes))
                        .scrolled(
                            "cluster-nodes",
                            Self::node_items(nodes, theme),
                            px(height_of(budget.nodes, budget::ROW)),
                        )
                        .item(Self::cluster_health_item(cx));
                }
                let can_reload = !connection.is_starting();
                menu = menu.separator().item(
                    MenuItem::new("reload", "Reload from Icinga")
                        .disabled(!can_reload)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.menus.close();
                            this.state.update(cx, |state, cx| {
                                if state.refresh(Instant::now()) {
                                    cx.notify();
                                }
                            });
                            cx.notify();
                        })),
                );
            }
            None => {
                menu = menu.element(
                    div()
                        .text_color(colors.text_muted)
                        .child("No environment configured."),
                );
            }
        }
        self.with_switcher(menu, state, now, budget, cx)
            .on_dismiss(Self::dismiss_listener(cx))
    }

    /// The *cluster health* row under the nodes: the whole picture, the
    /// cluster health page (topic 06), as *health* in the cluster section.
    fn cluster_health_item(cx: &Context<Self>) -> MenuItem {
        MenuItem::new("cluster-health", "cluster health")
            .icon(IconName::HeartPulse)
            .key_hint("zones, queues, checks/min")
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.menus.close();
                this.state.update(cx, |state, cx| {
                    if state.show_cluster(crate::cluster::ClusterEntry::Health) {
                        cx.notify();
                    }
                });
                cx.notify();
            }))
    }

    /// The cluster's masters and satellites (`node_rows`) as menu rows in
    /// the environments' style: a dot (green connected, red not, grey
    /// when the connected node can't tell), the name, the zone dimmed; the
    /// node icygui is connected to with the selected-row background. They
    /// only show: no hover, no click.
    fn node_items(nodes: Vec<NodeRow>, theme: &ic_ui_kit::Theme) -> Vec<MenuItem> {
        nodes
            .into_iter()
            .enumerate()
            .map(|(index, node)| {
                let color = match node.state {
                    Some(NodeState::Connected) => theme.states.fill.ok,
                    Some(NodeState::Disconnected) => theme.states.fill.critical,
                    Some(NodeState::Unknown) | None => theme.states.fill.pending,
                };
                MenuItem::new(("cluster-node", index), node.name)
                    .dot(color)
                    .detail(node.zone)
                    .selected(node.current)
                    .interactive(false)
                    .tooltip(Tooltip::new(node.hint))
            })
            .collect()
    }

    /// The environment switcher (ENV-01, B) under the details: every
    /// environment with its health (the footer's dot), its node and the
    /// age of its last event; the one on screen with the selected-row
    /// background (no check mark, so every row keeps its alignment). At
    /// the right of each row a bell (on hover; a bell-off while muted)
    /// points the mute row under the list at it (A5), and a gear opens its
    /// settings without switching. The list scrolls beyond the budget's
    /// rows. Then "add environment".
    fn with_switcher(
        &self,
        mut menu: Menu,
        state: &AppState,
        now: Timestamp,
        budget: SwitcherBudget,
        cx: &Context<Self>,
    ) -> Menu {
        let environments = state.environments();
        if !environments.is_empty() {
            menu = menu.separator().label(list_label(
                "environments",
                environments.len(),
                budget.environments,
            ));
        }
        let several = environments.len() > 1;
        let target = self
            .mute_target
            .as_deref()
            .and_then(|id| state.environment_by_id(id))
            .or_else(|| state.environment());
        let picked = self
            .mute_target
            .as_deref()
            .filter(|id| state.environment_by_id(id).is_some());
        let items = switcher_rows(state, now)
            .into_iter()
            .map(|row| Self::switcher_item(row, several, picked, cx))
            .collect();
        menu = menu.scrolled(
            "environment-list",
            items,
            px(height_of(budget.environments, budget::ROW)),
        );
        if several && let Some(environment) = target {
            menu = menu.element(Self::mute_row(environment, state, now, cx));
        }
        menu.separator().item(Self::emit_item(
            "add-environment",
            "add environment",
            SidebarEvent::AddEnvironment,
            cx,
        ))
    }

    /// One environment's switcher row: its health dot, name and detail,
    /// the bell (when there are several) and the gear at the right; a
    /// click switches to it.
    fn switcher_item(
        row: SwitcherRow,
        several: bool,
        picked: Option<&str>,
        cx: &Context<Self>,
    ) -> MenuItem {
        let theme = cx.theme();
        let SwitcherRow {
            id,
            name,
            active: is_active,
            health,
            detail,
            partial,
            muted,
        } = row;
        let item = MenuItem::new(
            gpui::ElementId::Name(format!("environment-{id}").into()),
            name.clone(),
        )
        .selected(is_active)
        .dot(super::health_color(health, theme));
        let item = if partial {
            item.detail_colored(detail, theme.states.text.warning)
        } else {
            item.detail(detail)
        };
        let item = if several {
            let target = id.clone();
            let bell = ItemAction::new(
                gpui::ElementId::Name(format!("environment-bell-{id}").into()),
                if muted {
                    IconName::BellOff
                } else {
                    IconName::Bell
                },
                cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.mute_target = Some(target.clone());
                    cx.notify();
                }),
            )
            .shown(picked == Some(id.as_str()))
            .tooltip(Tooltip::new(if muted {
                format!("{name} is muted: unmute")
            } else {
                format!("Mute {name}")
            }));
            // A muted environment's bell-off always shows.
            item.action(if muted { bell.always() } else { bell })
        } else {
            item
        };
        let edit = id.clone();
        item.action(
            ItemAction::new(
                gpui::ElementId::Name(format!("environment-settings-{id}").into()),
                IconName::Settings,
                cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.menus.close();
                    cx.emit(SidebarEvent::EditEnvironment(edit.clone()));
                    cx.notify();
                }),
            )
            .always()
            .tooltip(Tooltip::new(format!("Settings of {name}"))),
        )
        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            this.menus.close();
            if !is_active {
                cx.emit(SidebarEvent::SwitchEnvironment(id.clone()));
            }
            cx.notify();
        }))
    }

    /// Mutes one environment (A5; the one on screen, or the one whose bell
    /// was clicked; every environment's pause is in the notification
    /// centre, the palette and the tray): `mute staging` with the pause
    /// chips, or `staging muted until 18:30` with *unmute*. One line of a
    /// chip's height either way, like the notification centre's pause row,
    /// so nothing moves when it changes.
    fn mute_row(
        environment: &ic_config::Environment,
        state: &AppState,
        now: Timestamp,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let id = environment.id.clone();
        // Its text lines up with the items' labels. It takes the menu's
        // width rather than setting it (zero width, at least all of it), so
        // the menu keeps its width whether the environment is muted or
        // not; the name truncates instead.
        let row = div()
            .id("environment-mute")
            .flex()
            .items_center()
            .gap(px(6.))
            .w(px(0.))
            .min_w_full()
            .text_size(theme.text.small);
        match state.environment_paused_until(&id, now) {
            Some(until) => row
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .line_height(px(CHIP_HEIGHT))
                        .text_color(theme.states.text.warning)
                        .child(format!(
                            "{} muted until {}",
                            environment.name,
                            when(until, now)
                        )),
                )
                .child(
                    Link::new("environment-unmute", "unmute")
                        .text_size(theme.text.small)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.state.update(cx, |state, cx| {
                                if state.pause_environment(&id, None) {
                                    cx.notify();
                                }
                            });
                            cx.notify();
                        })),
                )
                .into_any_element(),
            None => row
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .line_height(px(CHIP_HEIGHT))
                        .text_color(colors.text_faint)
                        .child(format!("mute {}", environment.name)),
                )
                .children(PauseChoice::ALL.map(|choice| {
                    let id = id.clone();
                    Chip::new(
                        SharedString::from(format!("environment-mute-{choice:?}")),
                        choice.short_label(),
                    )
                    .on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            this.state.update(cx, |state, cx| {
                                if state
                                    .pause_environment(&id, Some(choice.until(Timestamp::now())))
                                {
                                    cx.notify();
                                }
                            });
                            cx.notify();
                        },
                    ))
                }))
                .into_any_element(),
        }
    }
}

/// A line of the connection details: its key, and its value cut short when
/// too long for the width (an error, a URL passed over), the full text in
/// its tooltip.
fn detail_line(
    index: usize,
    key: &'static str,
    value: String,
    theme: &ic_ui_kit::Theme,
) -> gpui::Div {
    let colors = theme.colors;
    div()
        .flex()
        .gap(px(10.))
        .text_size(theme.text.small)
        .child(
            div()
                .flex_none()
                .w(px(84.))
                .text_color(colors.text_faint)
                .child(key),
        )
        .child(
            div()
                .id(("connection-detail", index))
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(colors.text)
                .tooltip(Tooltip::text(value.clone()))
                .child(value),
        )
}

/// One environment in the switcher.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SwitcherRow {
    /// Its id.
    pub(crate) id: String,
    /// Its name.
    pub(crate) name: String,
    /// It is the one on screen.
    pub(crate) active: bool,
    /// Its connection's health (the dot).
    pub(crate) health: Health,
    /// Its node and the age of its last event (`master-01 · 2s`), what
    /// the connection does, and a view short of the whole cluster.
    pub(crate) detail: String,
    /// Its node sees only part of the cluster (the detail in the warning
    /// colour).
    pub(crate) partial: bool,
    /// Muted on its own (a bell-off).
    pub(crate) muted: bool,
}

/// The switcher's rows (ENV-01, B): every environment, in order.
pub(crate) fn switcher_rows(state: &AppState, now: Timestamp) -> Vec<SwitcherRow> {
    state
        .environments()
        .iter()
        .map(|environment| {
            let id = environment.id.clone();
            let (health, detail, partial) = match state.slot(&id) {
                Some(slot) => {
                    let connection = slot.connection();
                    let marker = connection.view_marker();
                    let mut detail = connection.label(now);
                    if let Some(marker) = &marker {
                        detail = format!("{detail} · {}", marker.label);
                    }
                    (
                        connection.health(now),
                        detail,
                        marker.is_some_and(|marker| marker.partial),
                    )
                }
                None => (Health::Connecting, "starting".to_owned(), false),
            };
            SwitcherRow {
                active: state.is_active(&id),
                muted: state.environment_paused_until(&id, now).is_some(),
                name: environment.name.clone(),
                id,
                health,
                detail,
                partial,
            }
        })
        .collect()
}

/// A section label: `nodes`, or `12 nodes · scroll for more` when a short
/// window shows fewer rows than there are.
fn list_label(what: &str, count: usize, shown: usize) -> String {
    if shown < count {
        format!("{count} {what} · scroll for more")
    } else {
        what.to_owned()
    }
}

/// A master or satellite of the environment on screen, for the switcher.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NodeRow {
    /// The endpoint's name.
    pub(crate) name: String,
    /// Its zone.
    pub(crate) zone: String,
    /// Its state, while connected (`None`: not connected now, so not
    /// known).
    pub(crate) state: Option<NodeState>,
    /// It is the node icygui is connected to.
    pub(crate) current: bool,
    /// The row's tooltip.
    pub(crate) hint: String,
}

/// The cluster's masters and satellites of the environment on screen
/// (`Snapshot::cluster_nodes`): their states only while connected.
pub(crate) fn node_rows(state: &AppState) -> Vec<NodeRow> {
    let connection = state.connection();
    let connected = connection.is_connected();
    let current = connection
        .node
        .as_ref()
        .filter(|_| connected)
        .map(|node| node.name.clone());
    let via = current.clone().unwrap_or_default();
    state
        .snapshot()
        .cluster_nodes()
        .into_iter()
        .map(|node| {
            let is_current = current.as_deref() == Some(node.name.as_str());
            let state = connected.then_some(node.state);
            let hint = match state {
                _ if is_current => format!("{}: icygui is connected to it", node.name),
                Some(NodeState::Connected) => format!("{}: connected to {via}", node.name),
                Some(NodeState::Disconnected) => {
                    format!("{}: not connected to {via}", node.name)
                }
                Some(NodeState::Unknown) => format!(
                    "{}: {via} has no connection of its own to it, so its state isn't known here",
                    node.name
                ),
                None => format!("{}: not known while not connected", node.name),
            };
            NodeRow {
                name: node.name,
                zone: node.zone,
                state,
                current: is_current,
                hint,
            }
        })
        .collect()
}

/// The connection details' lines: the state, the endpoint and its
/// version, the last event and the API user (with how many of the
/// permissions the client asks for it lacks).
pub(super) fn detail_lines(state: &AppState, now: Timestamp) -> Vec<(&'static str, String)> {
    let connection = state.connection();
    let mut lines = vec![("status", connection.describe(now))];
    if let Some(node) = &connection.node {
        // The node, its zone and how much of the cluster it sees (ENV-12),
        // and the URLs it was preferred to.
        lines.push((
            "node",
            match &node.zone {
                Some(zone) => format!("{} · zone {zone}", node.name),
                None => node.name.clone(),
            },
        ));
        lines.push((
            "view",
            ViewMarker::of(&node.view).map_or_else(
                || "full view: the whole cluster".to_owned(),
                |marker| marker.short,
            ),
        ));
        for (url, reason) in &node.passed_over {
            lines.push(("passed over", format!("{url} · {reason}")));
        }
    } else if !connection.endpoint.is_empty() {
        lines.push(("endpoint", connection.endpoint.clone()));
    }
    if let Some(version) = connection.version() {
        lines.push(("version", version.to_owned()));
    }
    lines.push((
        "last event",
        connection.last_event_at.map_or_else(
            || "none yet".to_owned(),
            |at| format!("{} ago", format_compact(at.elapsed_until(now))),
        ),
    ));
    if let Some(info) = state.permissions() {
        // Run command's permission is opt-in (the user guide's ApiUser).
        let missing = permissions::missing_needed(&ic_core::missing_permissions(info));
        lines.push((
            "API user",
            if missing == 0 {
                info.user.clone()
            } else {
                format!("{} · {missing} permissions missing", info.user)
            },
        ));
    }
    lines
}

/// The footer switcher's tooltip: the environment (and whether it is the
/// demo's), its connection and view, and which other environments have
/// unread notifications.
pub(super) fn status_tooltip(
    name: Option<&str>,
    demo: bool,
    connection: &str,
    view: Option<&str>,
    elsewhere: &[&str],
) -> String {
    let mut text = match name {
        Some(name) if demo => format!("{name} (demo) · {connection}"),
        Some(name) => format!("{name} · {connection}"),
        None => connection.to_owned(),
    };
    if let Some(view) = view {
        let _ = write!(text, " · {view}");
    }
    text.push_str(": environments and connection details");
    // Which, not how many: the counts are the notification centre's.
    if !elsewhere.is_empty() {
        let _ = write!(text, " · unread notifications in {}", elsewhere.join(", "));
    }
    text
}

/// The notification centre button's tooltip: unread notifications and a
/// pause.
pub(super) fn notifications_tooltip(
    unread: usize,
    paused_until: Option<Timestamp>,
    now: Timestamp,
) -> String {
    let mut text = "Notifications".to_owned();
    if unread > 0 {
        let _ = write!(text, " · {unread} unread");
    }
    if let Some(until) = paused_until.filter(|until| *until > now) {
        let _ = write!(text, " · paused until {}", crate::format::clock(until, now));
    }
    text
}

/// The shortcut for `ToggleSidebar`, as the tooltip shows it.
pub(super) fn toggle_key() -> &'static str {
    if cfg!(target_os = "macos") {
        "⌘B"
    } else {
        "ctrl-b"
    }
}

/// The shortcut for a new dashboard, as menus show it.
pub(crate) fn new_key() -> &'static str {
    if cfg!(target_os = "macos") {
        "⌘N"
    } else {
        "ctrl-n"
    }
}

#[cfg(test)]
mod tests {
    use ic_core::ApiInfo;
    use ic_model::Timestamp;

    use super::*;

    fn at(seconds: f64) -> Timestamp {
        Timestamp::from_unix_seconds(1_790_000_000. + seconds)
    }

    #[test]
    fn the_details_list_the_connection() {
        let mut state = AppState::fixture(at(0.));
        let lines = detail_lines(&state, at(65.));
        let keys: Vec<_> = lines.iter().map(|(key, _)| *key).collect();
        assert_eq!(keys, ["status", "node", "view", "version", "last event"]);
        assert_eq!(lines[0].1, "connected for 1m");
        assert_eq!(lines[1].1, "master-01 · zone master");
        assert_eq!(lines[2].1, "full view: the whole cluster");
        assert_eq!(lines[4].1, "1m ago");
        state.set_permissions(Some(ApiInfo {
            user: "viewer".to_owned(),
            permissions: vec!["objects/query/*".to_owned()],
            version: "v2.15.6".to_owned(),
        }));
        let lines = detail_lines(&state, at(65.));
        let (key, user) = lines.last().unwrap();
        assert_eq!(*key, "API user");
        assert!(user.starts_with("viewer · "), "{user}");
        assert!(user.ends_with("permissions missing"), "{user}");

        // The user guide's ApiUser: everything but the opt-in run command.
        state.set_permissions(Some(ApiInfo {
            user: "icygui".to_owned(),
            permissions: ic_core::REQUIRED_PERMISSIONS
                .iter()
                .filter(|permission| **permission != "actions/execute-command")
                .map(|permission| (*permission).to_owned())
                .collect(),
            version: "v2.15.6".to_owned(),
        }));
        let lines = detail_lines(&state, at(65.));
        assert_eq!(
            lines.last().unwrap().1,
            "icygui",
            "nothing needed is missing"
        );
    }

    #[test]
    fn the_details_name_a_partial_view_and_the_urls_passed_over() {
        let mut state = AppState::fixture(at(0.));
        state.apply(ic_core::CoreEvent::Connection(
            ic_core::ConnectionState::Connected {
                node: ic_core::ConnectedNode {
                    url: "https://sat-ams-01:5665".to_owned(),
                    url_index: 1,
                    name: "sat-ams-01".to_owned(),
                    zone: Some("ams".to_owned()),
                    view: ic_core::ClusterView::Partial {
                        zone: "ams".to_owned(),
                    },
                    passed_over: vec![(
                        "master-01:5665".to_owned(),
                        "connection refused".to_owned(),
                    )],
                },
                version: "r2.15.6-1".to_owned(),
                since: at(0.),
            },
        ));
        let lines = detail_lines(&state, at(5.));
        assert_eq!(lines[1], ("node", "sat-ams-01 · zone ams".to_owned()));
        assert_eq!(lines[2].0, "view");
        assert_eq!(
            lines[2].1,
            "partial view: zone ams · only that zone and below"
        );
        assert_eq!(
            lines[3],
            (
                "passed over",
                "master-01:5665 · connection refused".to_owned()
            )
        );
        let marker = state.connection().view_marker().unwrap();
        assert!(marker.partial);

        // Not verified: said so, without a warning colour.
        state.apply(ic_core::CoreEvent::Connection(
            ic_core::ConnectionState::Connected {
                node: ic_core::ConnectedNode {
                    view: ic_core::ClusterView::Unverified {
                        reason: "the API user may not read the zones".to_owned(),
                    },
                    zone: None,
                    passed_over: Vec::new(),
                    ..state.connection().node.clone().unwrap()
                },
                version: "r2.15.6-1".to_owned(),
                since: at(0.),
            },
        ));
        let marker = state.connection().view_marker().unwrap();
        assert_eq!(marker.label, "view not verified");
        assert!(!marker.partial);
        assert!(marker.detail.contains("may not read the zones"));
        let lines = detail_lines(&state, at(5.));
        assert_eq!(lines[1], ("node", "sat-ams-01".to_owned()));
    }

    #[test]
    fn the_switcher_fits_any_window_whatever_the_state() {
        // The default window shows every environment of a big list and
        // every node of a cluster of three.
        let big = switcher_budget(900., 11, 3);
        assert_eq!((big.environments, big.nodes), (11, 3));
        assert!(big.lines >= 9. * budget::LINE - 4., "{big:?}");
        // The smallest window (560 px) with 11 environments and 6 nodes:
        // three rows of each, the lines scroll, the controls stay; the
        // whole popover fits above the footer.
        let small = switcher_budget(560., 11, 6);
        assert_eq!((small.environments, small.nodes), (3, 3));
        assert!(
            budget::TOP
                + budget::NODES
                + small.lines
                + 8.
                + height_of(6, budget::ROW)
                + budget::BOTTOM
                + budget::OUTSIDE
                <= 560. + 0.5,
            "{small:?}"
        );
        // A few environments and no nodes (not connected yet): all of them.
        assert_eq!(switcher_budget(560., 2, 0).environments, 2);
        assert_eq!(switcher_budget(900., 1, 0).environments, 1);
        // Room goes to each list in turn.
        let tall = switcher_budget(700., 9, 9);
        assert!(tall.environments.abs_diff(tall.nodes) <= 1, "{tall:?}");
    }

    #[test]
    fn the_footer_shows_a_nodes_first_label() {
        assert_eq!(
            super::super::short_node("icinga-master-02.example.com"),
            "icinga-master-02"
        );
        assert_eq!(super::super::short_node("master-01"), "master-01");
        assert_eq!(super::super::short_node("10.0.4.24"), "10.0.4.24");
        assert_eq!(super::super::short_node("::1"), "::1");
        assert_eq!(super::super::short_node(".odd"), ".odd");
    }

    /// The footer keeps the node while a few characters of it fit beside
    /// the name and the age slot, and drops it rather than show a lone
    /// ellipsis without live data.
    #[test]
    fn the_footer_keeps_the_node_while_it_has_room() {
        use super::super::{AGE_SLOT_CHARS, NO_DATA_SLOT_CHARS, node_has_room};
        // Every length scales with the interface size, so 100 % stands for
        // all of them.
        let theme = ic_ui_kit::Theme::dark();
        // `prod-cluster master-01 59s` fits; `prod-cluster no data 4m`.
        assert!(node_has_room(&theme, 12, AGE_SLOT_CHARS));
        assert!(!node_has_room(&theme, 12, NO_DATA_SLOT_CHARS));
        // `staging stg-m… no data 4m`, `lab master-01 no data 4m`.
        assert!(node_has_room(&theme, 7, NO_DATA_SLOT_CHARS));
        assert!(node_has_room(&theme, 3, NO_DATA_SLOT_CHARS));
    }

    #[test]
    fn the_switcher_tooltip_names_the_environment_and_unread_elsewhere() {
        assert_eq!(
            status_tooltip(Some("prod"), false, "master-01 · 2s", None, &[]),
            "prod · master-01 · 2s: environments and connection details"
        );
        assert_eq!(
            status_tooltip(
                Some("prod-cluster"),
                true,
                "sat-ams-01 · 2s",
                Some("partial view: zone ams"),
                &["staging", "lab"]
            ),
            "prod-cluster (demo) · sat-ams-01 · 2s · partial view: zone ams: environments \
             and connection details · unread notifications in staging, lab"
        );
        assert_eq!(
            status_tooltip(None, false, "no environment", None, &[]),
            "no environment: environments and connection details"
        );
    }

    #[test]
    fn the_notification_tooltip_counts_and_says_paused() {
        assert_eq!(notifications_tooltip(0, None, at(0.)), "Notifications");
        assert_eq!(
            notifications_tooltip(3, None, at(0.)),
            "Notifications · 3 unread"
        );
        let paused = notifications_tooltip(0, Some(at(600.)), at(0.));
        assert!(
            paused.starts_with("Notifications · paused until "),
            "{paused}"
        );
        assert_eq!(
            notifications_tooltip(0, Some(at(-1.)), at(0.)),
            "Notifications",
            "a pause that ended isn't mentioned"
        );
    }

    #[test]
    fn scope_settings_cover_inherit_on_and_off() {
        let settings: Vec<_> = SCOPE_SETTINGS.iter().map(|(_, _, s)| s.clone()).collect();
        assert_eq!(
            settings,
            [ScopeSetting::Inherit, ScopeSetting::On, ScopeSetting::Off]
        );
    }
}
