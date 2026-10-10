//! The sidebar (DASH-01): window controls and dashboard search in the
//! header, groups (folders) of dashboards, the objects open as tabs, and
//! the footer with the sidebar toggle, the notification centre, the
//! connection status and `+`.
//!
//! - Groups and dashboards are managed from their `···` menus (also a
//!   right click on a dashboard): new dashboard (also the group's `+`),
//!   rename (in place), duplicate, move, reorder, notification setting,
//!   export, delete (DASH-02, DASH-03). Deleting asks first, and so do the
//!   editors: the sidebar only says what was asked for ([`SidebarEvent`]),
//!   the workspace opens the dialogs.
//! - The footer's connection status opens the connection details with the
//!   environment switcher (ENV-01, ENV-06); its `+` creates dashboards and
//!   groups and imports or exports them (DASH-06); its clock opens the
//!   notification centre (NOTE-05) and carries the unread count.
//!
//! The footer's "last event" age refreshes with the workspace's clock
//! (UI-04). In the search field, Enter shows the first matching dashboard
//! and Escape clears the search; both hand the keyboard back to the main
//! area.

mod centre;
mod menus;
pub(crate) mod model;

use gpui::{
    AnyElement, AppContext as _, ClickEvent, Context, Div, Entity, EventEmitter, FontWeight,
    InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent, ParentElement as _, Pixels,
    Render, SharedString, Stateful, StatefulInteractiveElement as _, Styled as _, Subscription,
    Window, div, prelude::FluentBuilder as _,
};
use ic_model::{ObjectKey, Timestamp};
use ic_rules::{DashboardRef, ScopeSetting};
use ic_ui_kit::input::{Escape, InputEvent, InputState};
use ic_ui_kit::{
    ActiveTheme as _, Divider, DividerColor, GlyphButton, Icon, IconButton, IconName, Link,
    Metrics, Popover, StateDot, TextField, Theme, Tooltip, px,
};

use crate::actions::FocusMain;
use crate::app_state::{AppState, Health};
use crate::chrome::{Controls, WindowControls, WindowDrag};
use crate::menu_state::{OpenMenu, down_position};
use crate::settings::{ScopeKey, SettingsPage};
use crate::workspace::ToggleSidebar;

pub(crate) use self::menus::new_key;
#[cfg(all(test, target_os = "linux"))]
pub(crate) use self::menus::switcher_rows;
pub(crate) use self::model::Dot;
use self::model::{ClusterRow, Mark, OpenTab, SidebarGroup, SidebarItem};

/// What the sidebar asks the workspace to do: open an editor, a dialog
/// or a file prompt, or switch environments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SidebarEvent {
    /// Create a dashboard in this group (`None`: the selected dashboard's
    /// group, else the first; a group is created if there is none).
    NewDashboard(Option<String>),
    /// Edit this dashboard.
    EditDashboard(DashboardRef),
    /// Delete this group (after asking).
    DeleteGroup(String),
    /// Delete this dashboard (after asking).
    DeleteDashboard(DashboardRef),
    /// Export these groups (empty: all of them) to a file.
    ExportGroups(Vec<String>),
    /// Import groups from a file.
    ImportGroups,
    /// Make this environment the active one.
    SwitchEnvironment(String),
    /// Open the editor for a new environment.
    AddEnvironment,
    /// Open the editor for this environment.
    EditEnvironment(String),
    /// Show this object of this environment (a notification centre
    /// entry), switching to the environment first if it isn't on screen.
    OpenObjectIn {
        /// The environment's id.
        environment: String,
        /// The object.
        object: ObjectKey,
    },
    /// Show this environment's cluster health page (a trouble alert's
    /// entry), switching to the environment first.
    OpenHealthIn(String),
    /// Open the settings on this tab.
    OpenSettings(SettingsPage),
    /// Give this group or dashboard a custom notification rule, in the
    /// notification settings.
    CustomRule(ScopeKey),
}

/// The sidebar's popup menus.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SidebarMenu {
    /// The connection details and environment switcher (footer status).
    Status,
    /// The footer's `+`.
    Footer,
    /// The notification centre (the footer's clock).
    Notifications,
    /// A group's `···`.
    Group(String),
    /// A dashboard's `···` (or right click).
    Dashboard(DashboardRef),
}

/// What is being renamed in place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RenameTarget {
    /// A group, by id.
    Group(String),
    /// A dashboard.
    Dashboard(DashboardRef),
}

/// A rename in progress: a text field in place of the name.
struct Rename {
    target: RenameTarget,
    input: Entity<InputState>,
    _subscription: Subscription,
}

/// The sidebar view.
pub(crate) struct Sidebar {
    state: Entity<AppState>,
    search: Entity<InputState>,
    query: String,
    drag: WindowDrag,
    menus: OpenMenu<SidebarMenu>,
    centre: centre::CentreState,
    /// The environment the switcher's mute row is about: one whose bell
    /// was clicked in the switcher (`None`: the one on screen).
    mute_target: Option<String>,
    /// The window's height at the last render: the footer's popovers fit
    /// into it (a short window shows less of their lists, never less of
    /// their controls).
    viewport: Pixels,
    rename: Option<Rename>,
    /// A dashboard being created, shown as a selected row under its group
    /// while its editor is open.
    provisional: Option<model::Provisional>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<SidebarEvent> for Sidebar {}

impl Sidebar {
    pub(crate) fn new(
        state: Entity<AppState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search dashboards…"));
        let subscriptions = vec![
            cx.observe(&state, |_, _, cx| cx.notify()),
            cx.subscribe_in(
                &search,
                window,
                |this: &mut Self, search, event: &InputEvent, window, cx| match event {
                    InputEvent::Change => {
                        this.query = search.read(cx).value().to_string();
                        cx.notify();
                    }
                    InputEvent::PressEnter { .. } => this.open_first_match(window, cx),
                    InputEvent::Focus | InputEvent::Blur => {}
                },
            ),
        ];
        Self {
            state,
            search,
            query: String::new(),
            drag: WindowDrag::default(),
            menus: OpenMenu::default(),
            centre: centre::CentreState::default(),
            mute_target: None,
            viewport: crate::WINDOW_SIZE.height,
            rename: None,
            provisional: None,
            _subscriptions: subscriptions,
        }
    }

    /// Whether the connection details are open.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn details_open(&self) -> bool {
        self.menus.is_open(&SidebarMenu::Status)
    }

    /// The open menu.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn open_menu(&self) -> Option<&SidebarMenu> {
        self.menus.current()
    }

    /// What is being renamed, and its field.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn renaming(&self) -> Option<(&RenameTarget, &Entity<InputState>)> {
        self.rename
            .as_ref()
            .map(|rename| (&rename.target, &rename.input))
    }

    /// Closes the open menu. Returns whether one was open.
    pub(crate) fn close_menu(&mut self, cx: &mut Context<Self>) -> bool {
        let closed = self.menus.close();
        if closed {
            cx.notify();
        }
        closed
    }

    /// Enter in the search field: shows the first dashboard the search
    /// matches and hands the keyboard to the main area.
    fn open_first_match(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let first = {
            let state = self.state.read(cx);
            state.environment().and_then(|environment| {
                model::groups(
                    environment,
                    state.snapshot(),
                    state.selected(),
                    &self.query,
                    Timestamp::now(),
                )
                .into_iter()
                .flat_map(|group| group.items)
                .map(|item| item.reference)
                .next()
            })
        };
        if let Some(reference) = first {
            self.state.update(cx, |state, cx| {
                if state.select(reference) {
                    cx.notify();
                }
            });
        }
        window.dispatch_action(Box::new(FocusMain), cx);
    }

    /// Escape in the search field (it has nothing of its own to dismiss):
    /// clears the search and hands the keyboard to the main area.
    fn dismiss_search(&mut self, _: &Escape, window: &mut Window, cx: &mut Context<Self>) {
        if !self.query.is_empty() {
            // `set_value` doesn't report a change.
            self.search
                .update(cx, |search, cx| search.set_value("", window, cx));
            self.query.clear();
            cx.notify();
        }
        window.dispatch_action(Box::new(FocusMain), cx);
    }

    /// The search field's state.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn search_input(&self) -> &Entity<InputState> {
        &self.search
    }

    /// The current search query.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn query(&self) -> &str {
        &self.query
    }

    /// Starts renaming `target` in place: a field with its name, selected.
    pub(crate) fn start_rename(
        &mut self,
        target: RenameTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.menus.close();
        let name = {
            let state = self.state.read(cx);
            match &target {
                RenameTarget::Group(id) => state
                    .groups()
                    .iter()
                    .find(|group| &group.id == id)
                    .map(|group| group.name.clone()),
                RenameTarget::Dashboard(reference) => state
                    .dashboard(reference)
                    .map(|(_, dashboard)| dashboard.name.clone()),
            }
        };
        let Some(name) = name else {
            return;
        };
        let input = cx.new(|cx| InputState::new(window, cx).default_value(name));
        let subscription = cx.subscribe_in(
            &input,
            window,
            |this: &mut Self, _, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { .. } | InputEvent::Blur => {
                    this.finish_rename(true, window, cx);
                }
                InputEvent::Change | InputEvent::Focus => {}
            },
        );
        input.update(cx, |input, cx| {
            input.focus(window, cx);
            input.select_all(window, cx);
        });
        self.rename = Some(Rename {
            target,
            input,
            _subscription: subscription,
        });
        cx.notify();
    }

    /// Ends the rename: saves the new name (`commit`), or keeps the old
    /// one; the keyboard goes back to the main area.
    fn finish_rename(&mut self, commit: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(rename) = self.rename.take() else {
            return;
        };
        if commit {
            let name = rename.input.read(cx).value().to_string();
            self.state.update(cx, |state, cx| {
                let renamed = match &rename.target {
                    RenameTarget::Group(id) => state.rename_group(id, &name),
                    RenameTarget::Dashboard(reference) => state.rename_dashboard(reference, &name),
                };
                if renamed {
                    cx.notify();
                }
            });
        }
        cx.notify();
        window.dispatch_action(Box::new(FocusMain), cx);
    }

    /// Escape in the rename field: keeps the old name.
    fn cancel_rename(&mut self, _: &Escape, window: &mut Window, cx: &mut Context<Self>) {
        self.finish_rename(false, window, cx);
    }

    fn render_header(&self, window: &Window, cx: &Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let metrics = theme.metrics;
        let controls = Controls::of(window, cx);
        let header = div()
            .id("sidebar-header")
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.))
            .h(Metrics::with_rule(metrics.header_height))
            .px(metrics.sidebar_padding)
            .border_b_1()
            .border_color(theme.colors.border_header)
            .when(controls != Controls::None, |header| {
                header.child(WindowControls::new(controls)).child(
                    Divider::vertical()
                        .color(DividerColor::Window)
                        .length(px(18.))
                        .margin(px(6.)),
                )
            })
            .child(
                Icon::new(IconName::Search)
                    .size(metrics.icon)
                    .color(theme.colors.text_muted),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .on_action(cx.listener(Self::dismiss_search))
                    .child(TextField::new(&self.search)),
            );
        self.drag.attach(header, controls)
    }

    fn render_groups(&self, cx: &Context<Self>) -> AnyElement {
        let state = self.state.read(cx);
        let theme = cx.theme().clone();
        let Some(environment) = state.environment() else {
            return empty_note("No dashboards yet", None, &theme, cx);
        };
        let now = Timestamp::now();
        // While a tab or a cluster entry is shown, or a new dashboard is
        // being made (its provisional row is the selected one), no
        // dashboard is highlighted.
        let selected = state.selected().filter(|_| {
            state.active_tab().is_none()
                && state.active_cluster().is_none()
                && self.provisional.is_none()
        });
        let mut groups = model::groups(environment, state.snapshot(), selected, &self.query, now);
        // The group a new dashboard goes into holds the selected row.
        if let Some(provisional) = &self.provisional {
            for group in &mut groups {
                group.active |= group.group.id == provisional.group_id;
            }
        }
        let tabs = model::open_tabs(state.tabs(), state.active_tab(), state.snapshot());
        let cluster_state =
            crate::cluster::sidebar_state(state.snapshot(), state.connection(), now);
        let cluster =
            model::cluster_rows(state.snapshot(), state.active_cluster(), cluster_state, now);
        let environment_name = environment.name.clone();
        let mut rows: Vec<AnyElement> = vec![Self::render_cluster(
            &cluster,
            &environment_name,
            &theme,
            cx,
        )];
        if groups.is_empty() && tabs.is_empty() {
            rows.push(if self.query.trim().is_empty() {
                empty_note("No dashboards yet", Some("new dashboard"), &theme, cx)
            } else {
                empty_note("No matching dashboards", None, &theme, cx)
            });
        }
        let count = environment.groups.len();
        rows.extend(
            groups
                .iter()
                .map(|group| self.render_group(group, count, &theme, cx)),
        );
        if !tabs.is_empty() {
            rows.push(Self::render_open_tabs(&tabs, &theme, cx));
        }
        div()
            .id("sidebar-groups")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .children(rows)
            .into_any_element()
    }

    /// The fixed **cluster** section at the top (topic 14): `cluster` with
    /// the environment's name faint beside it, then handling, downtimes,
    /// events and health, each with its mark in the dot slot and its count
    /// in the count slot; a rule under it, above the groups.
    fn render_cluster(
        rows: &[ClusterRow],
        environment: &str,
        theme: &Theme,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = theme.colors;
        let metrics = theme.metrics;
        let header = div()
            .id("cluster-section")
            .flex()
            .flex_none()
            .items_baseline()
            .gap(px(8.))
            .h(metrics.group_row_height)
            .pt(px(9.))
            .px(metrics.sidebar_padding)
            .whitespace_nowrap()
            .overflow_hidden()
            .child(
                div()
                    .flex_none()
                    .text_size(theme.text.heading)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(colors.text_secondary)
                    .child("cluster"),
            )
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(theme.text.row)
                    .text_color(colors.text_faint)
                    .child(environment.to_owned()),
            );
        let entries = rows.iter().map(|row| {
            let entry = row.entry;
            div()
                .id(entry.id())
                .flex()
                .flex_none()
                .items_center()
                .gap(px(12.))
                .h(metrics.item_row_height)
                .pl(px(14.))
                .pr(metrics.sidebar_padding)
                .cursor_pointer()
                .when(row.active, |item| item.bg(colors.item_active))
                .when(!row.active, |item| {
                    item.hover(|style| style.bg(colors.item_hover))
                })
                .child(mark(row.mark, theme))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(theme.text.row)
                        .text_color(if row.active {
                            colors.text_emphasis
                        } else {
                            colors.text_secondary
                        })
                        .child(entry.title()),
                )
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .justify_end()
                        .min_w(px(22.))
                        .pr(GlyphButton::reach())
                        .text_size(theme.text.label)
                        .text_color(colors.text_muted)
                        .children(row.count.map(|count| count.to_string())),
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.state.update(cx, |state, cx| {
                        if state.show_cluster(entry) {
                            cx.notify();
                        }
                    });
                }))
        });
        div()
            .flex()
            .flex_col()
            .flex_none()
            .pb(px(6.))
            .mb(px(6.))
            .border_b_1()
            .border_color(colors.border_header)
            .child(header)
            .children(entries)
            .into_any_element()
    }

    /// The "open" section, shown only while there are any: objects opened
    /// as tabs ("↗ open as tab").
    fn render_open_tabs(tabs: &[OpenTab], theme: &Theme, cx: &Context<Self>) -> AnyElement {
        let colors = theme.colors;
        let active = tabs.iter().any(|tab| tab.active);
        let header = div()
            .id("open-tabs")
            .group("sidebar-open-tabs")
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.))
            .h(theme.metrics.group_row_height)
            .px(theme.metrics.sidebar_padding)
            .when(active, |row| row.bg(colors.group_active))
            .child(
                div()
                    .text_size(theme.text.heading)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(if active {
                        colors.text_emphasis
                    } else {
                        colors.text_secondary
                    })
                    .child("open"),
            )
            .child(div().flex_1())
            .child(
                div()
                    .invisible()
                    .group_hover("sidebar-open-tabs", gpui::Styled::visible)
                    .child(
                        div()
                            .id("close-all-tabs")
                            .px(px(4.))
                            .text_size(theme.text.hint)
                            .text_color(colors.text_muted)
                            .cursor_pointer()
                            .hover(|style| style.text_color(colors.text))
                            .child("close all")
                            .on_mouse_down(MouseButton::Left, |_, window, _| {
                                window.prevent_default();
                            })
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.state.update(cx, |state, cx| {
                                    if state.close_all_tabs() {
                                        cx.notify();
                                    }
                                });
                            })),
                    ),
            );
        div()
            .flex()
            .flex_col()
            .flex_none()
            .pb(px(6.))
            .child(header)
            .children(tabs.iter().map(|tab| Self::render_tab(tab, theme, cx)))
            .into_any_element()
    }

    fn render_tab(tab: &OpenTab, theme: &Theme, cx: &Context<Self>) -> AnyElement {
        let colors = theme.colors;
        let metrics = theme.metrics;
        let id = SharedString::from(format!("tab-{}", tab.key));
        let group = SharedString::from(format!("tab-row-{}", tab.key));
        let activate = tab.key.clone();
        let close = tab.key.clone();
        div()
            .id(id.clone())
            .group(group.clone())
            .flex()
            .flex_none()
            .items_center()
            .gap(px(12.))
            .h(metrics.item_row_height)
            .pl(px(14.))
            .pr(px(6.))
            .cursor_pointer()
            .when(tab.active, |row| row.bg(colors.item_active))
            .when(!tab.active, |row| {
                row.hover(|style| style.bg(colors.item_hover))
            })
            .child(dot(tab.dot, theme).size(metrics.sidebar_dot))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(6.))
                    .text_size(theme.text.row)
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .child(
                        div()
                            .flex_none()
                            .max_w_full()
                            .truncate()
                            .text_color(if tab.active {
                                colors.text_emphasis
                            } else {
                                colors.text_secondary
                            })
                            .child(tab.name.clone()),
                    )
                    .when_some(tab.host.clone(), |label, host| {
                        label.child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_color(colors.text_faint)
                                .child(host),
                        )
                    }),
            )
            .child(
                div()
                    .flex_none()
                    .when(!tab.active, |slot| {
                        slot.invisible().group_hover(group, gpui::Styled::visible)
                    })
                    .child(
                        IconButton::new(SharedString::from(format!("close-{id}")), IconName::Close)
                            .size(px(20.))
                            .icon_size(px(12.))
                            .color(colors.text_muted)
                            .tooltip(Tooltip::new("Close tab"))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                let key = close.clone();
                                this.state.update(cx, |state, cx| {
                                    if state.close_tab(&key) {
                                        cx.notify();
                                    }
                                });
                            })),
                    ),
            )
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                let key = activate.clone();
                this.state.update(cx, |state, cx| {
                    if state.activate_tab(&key) {
                        cx.notify();
                    }
                });
            }))
            .into_any_element()
    }

    fn render_group(
        &self,
        group: &SidebarGroup<'_>,
        group_count: usize,
        theme: &Theme,
        cx: &Context<Self>,
    ) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .flex_none()
            .pb(px(6.))
            .child(self.render_group_header(group, group_count, theme, cx))
            .children(
                group
                    .items
                    .iter()
                    .map(|item| self.render_item(item, theme, cx)),
            )
            .children(
                self.provisional
                    .as_ref()
                    .filter(|provisional| provisional.group_id == group.group.id)
                    .map(|provisional| Self::render_provisional(provisional, theme)),
            )
            .into_any_element()
    }

    /// Shows `provisional` (a dashboard being created) under its group, or
    /// nothing; redraws only when that changes.
    pub(crate) fn set_provisional(
        &mut self,
        provisional: Option<model::Provisional>,
        cx: &mut Context<Self>,
    ) {
        if self.provisional != provisional {
            self.provisional = provisional;
            cx.notify();
        }
    }

    /// The dashboard being created that the sidebar shows (UI tests).
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn provisional(&self) -> Option<&model::Provisional> {
        self.provisional.as_ref()
    }

    /// The row of a dashboard being created (14-r5-a): as a dashboard's,
    /// selected, with the draft's mark and name; it is the editor's, so
    /// it has no count, no `···` and no click of its own.
    fn render_provisional(provisional: &model::Provisional, theme: &Theme) -> AnyElement {
        let colors = theme.colors;
        div()
            .id("dashboard-provisional")
            .flex()
            .flex_none()
            .items_center()
            .gap(px(12.))
            .h(theme.metrics.item_row_height)
            .pl(px(14.))
            .pr(theme.metrics.sidebar_padding)
            .bg(colors.item_active)
            .child(mark(provisional.mark, theme))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(theme.text.row)
                    .text_color(colors.text_emphasis)
                    .child(SharedString::from(provisional.name.clone())),
            )
            .into_any_element()
    }

    fn render_group_header(
        &self,
        group: &SidebarGroup<'_>,
        group_count: usize,
        theme: &Theme,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        let colors = theme.colors;
        let group_id = group.group.id.clone();
        let hover_group = SharedString::from(format!("sidebar-group-{group_id}"));
        let menu_open = self.menus.is_open(&SidebarMenu::Group(group_id.clone()));
        let renaming = self
            .rename
            .as_ref()
            .filter(|rename| rename.target == RenameTarget::Group(group_id.clone()));
        let chevron = if group.expanded {
            IconName::ChevronDown
        } else {
            IconName::ChevronRight
        };
        // The chevron and the + / ··· icons show on the active group; on the
        // others they appear on hover. A collapsed group always shows its
        // chevron.
        let reveal = |element: Div, always: bool| {
            if always {
                element
            } else {
                element
                    .invisible()
                    .group_hover(hover_group.clone(), gpui::Styled::visible)
            }
        };
        let label: AnyElement = match renaming {
            Some(rename) => Self::rename_field(rename, theme.text.heading, cx),
            None => div()
                .min_w_0()
                .truncate()
                .text_size(theme.text.heading)
                .font_weight(FontWeight::MEDIUM)
                .text_color(if group.active {
                    colors.text_emphasis
                } else {
                    colors.text_secondary
                })
                .child(group.group.name.clone())
                .into_any_element(),
        };
        let muted = group.group.notifications == ScopeSetting::Off;
        let index = self
            .state
            .read(cx)
            .groups()
            .iter()
            .position(|candidate| candidate.id == group_id)
            .unwrap_or(0);
        let toggle_id = group_id.clone();
        div()
            .id(SharedString::from(format!("group-{group_id}")))
            .group(hover_group.clone())
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.))
            .h(theme.metrics.group_row_height)
            .pl(theme.metrics.sidebar_padding)
            // The `···` button's reach makes up the rest of the 12px.
            .pr(theme.metrics.sidebar_padding - GlyphButton::reach())
            .cursor_pointer()
            .when(group.active || menu_open, |row| row.bg(colors.group_active))
            .when(!group.active && !menu_open, |row| {
                row.hover(|style| style.bg(colors.group_hover))
            })
            .child(label)
            .when(muted && renaming.is_none(), |row| {
                row.child(
                    Icon::new(IconName::BellOff)
                        .size(px(11.))
                        .color(colors.text_faint),
                )
            })
            .when(renaming.is_none(), |row| {
                row.child(reveal(
                    div()
                        .flex_none()
                        .child(Icon::new(chevron).size(px(12.)).color(colors.text_muted)),
                    group.active || !group.expanded,
                ))
                .child(div().flex_1())
                .child(reveal(
                    self.group_actions(group.group, index, group_count, theme, cx),
                    group.active || menu_open,
                ))
            })
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                if this.rename.is_some() {
                    return;
                }
                this.state.update(cx, |state, cx| {
                    if state.toggle_group(&toggle_id) {
                        cx.notify();
                    }
                });
            }))
    }

    /// The group row's `+` (new dashboard) and `···` (group menu) buttons,
    /// sized and spaced like the design's glyphs.
    fn group_actions(
        &self,
        group: &ic_config::DashboardGroup,
        index: usize,
        group_count: usize,
        theme: &Theme,
        cx: &Context<Self>,
    ) -> Div {
        let group_id = group.id.clone();
        let menu = SidebarMenu::Group(group_id.clone());
        let open = self.menus.is_open(&menu);
        let button = |id: String, glyph: &'static str, size: f32| {
            GlyphButton::new(SharedString::from(id), glyph)
                .text_size(px(size))
                .color(theme.colors.text)
        };
        let add_group = group_id.clone();
        let add = button(format!("group-add-{group_id}"), "+", 15.)
            .tooltip(Tooltip::new("New dashboard"))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.menus.close();
                cx.emit(SidebarEvent::NewDashboard(Some(add_group.clone())));
                cx.notify();
            }));
        let more = button(format!("group-menu-{group_id}"), "···", 13.)
            .selected(open)
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                this.menus.toggle(menu.clone(), down_position(event));
                cx.notify();
            }));
        // The buttons' reach (4px on each side) makes the design's 8px between
        // the glyphs.
        div().flex().flex_none().items_center().child(add).child(
            div()
                .relative()
                .flex_none()
                .child(if open {
                    more
                } else {
                    more.tooltip(Tooltip::new("Group options"))
                })
                .when(open, |slot| {
                    slot.child(
                        Popover::new(Self::group_menu(group, index, group_count, cx)).align_right(),
                    )
                }),
        )
    }

    fn render_item(&self, item: &SidebarItem<'_>, theme: &Theme, cx: &Context<Self>) -> AnyElement {
        let colors = theme.colors;
        let metrics = theme.metrics;
        let reference = item.reference.clone();
        let menu = SidebarMenu::Dashboard(item.reference.clone());
        let menu_open = self.menus.is_open(&menu);
        let renaming = self
            .rename
            .as_ref()
            .filter(|rename| rename.target == RenameTarget::Dashboard(item.reference.clone()));
        let id = format!(
            "dashboard-{}-{}",
            item.reference.group_id, item.reference.dashboard_id
        );
        let hover = SharedString::from(format!("item-{id}"));
        let label: AnyElement = match renaming {
            Some(rename) => Self::rename_field(rename, theme.text.row, cx),
            None => div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(theme.text.row)
                .text_color(if item.selected {
                    colors.text_emphasis
                } else {
                    colors.text_secondary
                })
                .child(SharedString::from(item.name.to_owned()))
                .into_any_element(),
        };
        let right_click_menu = menu.clone();
        div()
            .id(SharedString::from(id.clone()))
            .group(hover.clone())
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(12.))
            .h(metrics.item_row_height)
            .pl(px(14.))
            .pr(metrics.sidebar_padding - GlyphButton::reach())
            .cursor_pointer()
            .when(item.selected || menu_open, |row| row.bg(colors.item_active))
            .when(!item.selected && !menu_open, |row| {
                row.hover(|style| style.bg(colors.item_hover))
            })
            .child(mark(item.mark, theme))
            .child(label)
            .when(item.muted && renaming.is_none(), |row| {
                row.child(
                    Icon::new(IconName::BellOff)
                        .size(px(11.))
                        .color(colors.text_faint),
                )
            })
            .when(renaming.is_none(), |row| {
                row.child(self.item_trailer(item, &id, &hover, theme, cx))
            })
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                    window.prevent_default();
                    this.menus.open(right_click_menu.clone());
                    cx.notify();
                }),
            )
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                if this.rename.is_some() {
                    return;
                }
                let reference = reference.clone();
                this.state.update(cx, |state, cx| {
                    if state.select(reference) {
                        cx.notify();
                    }
                });
            }))
            .into_any_element()
    }

    /// The field a group or dashboard is renamed in, in place of its name.
    fn rename_field(rename: &Rename, text_size: Pixels, cx: &Context<Self>) -> AnyElement {
        div()
            .flex_1()
            .min_w_0()
            .on_action(cx.listener(Self::cancel_rename))
            .child(
                TextField::new(&rename.input)
                    .bordered(true)
                    .text_size(text_size),
            )
            .into_any_element()
    }

    /// A dashboard row's right end: its count, or the `···` button on
    /// hover and while its menu is open.
    fn item_trailer(
        &self,
        item: &SidebarItem<'_>,
        id: &str,
        hover: &SharedString,
        theme: &Theme,
        cx: &Context<Self>,
    ) -> Div {
        let colors = theme.colors;
        let menu = SidebarMenu::Dashboard(item.reference.clone());
        let menu_open = self.menus.is_open(&menu);
        let more = GlyphButton::new(SharedString::from(format!("{id}-menu")), "···")
            .text_size(px(13.))
            .color(colors.text_muted)
            .selected(menu_open)
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                this.menus.toggle(menu.clone(), down_position(event));
                cx.notify();
            }));
        div()
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .justify_end()
            .min_w(px(22.))
            .child(
                div()
                    .when(menu_open, gpui::Styled::invisible)
                    .group_hover(hover.clone(), gpui::Styled::invisible)
                    .pr(GlyphButton::reach())
                    .text_size(theme.text.label)
                    .text_color(colors.text_muted)
                    .children(item.count.map(|count| count.to_string())),
            )
            .child(
                div()
                    .absolute()
                    .right_0()
                    .when(!menu_open, |slot| {
                        slot.invisible()
                            .group_hover(hover.clone(), gpui::Styled::visible)
                    })
                    .child(more),
            )
            .when(menu_open, |slot| {
                slot.child(Popover::new(self.dashboard_menu(&item.reference, cx)).align_right())
            })
    }

    /// The footer's environment switcher (ENV-01, ENV-06, B): the health
    /// dot, the environment's name, the connected node and the age of the
    /// last event (or what the connection is doing), and a chevron; a
    /// button's hover and pressed states. It opens the connection details
    /// with every environment to switch to.
    ///
    /// Nothing in it moves with the state: the dot, the name and the
    /// chevron (pinned at the right) stay put; a long node name is cut
    /// short (the age after it never, ENV-06); a node that sees only part
    /// of the cluster turns the warning colour (ENV-12); another
    /// environment's unread notifications turn the chevron the accent
    /// colour (A3: the badge counts the one on screen).
    fn render_status(&self, now: Timestamp, cx: &Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let colors = theme.colors;
        let metrics = theme.metrics;
        let state = self.state.read(cx);
        let connection = state.connection();
        let health = connection.health(now);
        let (endpoint, suffix) = connection.label_parts(now);
        // No live data (or an engine that stopped saying it runs): the
        // node stays, `no data 3m` takes the age's place (16f).
        let no_data = connection.no_data_for(now).is_some();
        let connected = (connection.is_connected() && health != Health::Failed) || no_data;
        let name = state
            .environment()
            .map(|environment| environment.name.clone());
        let name_chars = name.as_ref().map_or(0, |name| name.chars().count());
        // A node that doesn't see the whole cluster says so (ENV-12).
        let marker = connection.view_marker();
        let partial = marker.as_ref().is_some_and(|marker| marker.partial);
        let elsewhere = state
            .environments()
            .iter()
            .filter(|environment| {
                !state.is_active(&environment.id) && state.unread_in(&environment.id) > 0
            })
            .map(|environment| environment.name.as_str())
            .collect::<Vec<_>>();
        let tooltip = menus::status_tooltip(
            name.as_deref(),
            state.is_demo_environment(),
            &connection.label(now),
            marker.as_ref().map(|marker| marker.label.as_str()),
            &elsewhere,
        );
        let open = self.menus.is_open(&SidebarMenu::Status);
        let status = div()
            .id("environment-status")
            .flex()
            .flex_1()
            .min_w_0()
            .items_center()
            // The dot sits where the design draws it; the hover background
            // reaches around the text like an icon button's.
            .ml(px(-(STATUS_PADDING - 2.)))
            .px(px(STATUS_PADDING))
            .h(metrics.icon_button)
            .rounded(metrics.small_radius)
            .text_size(theme.text.hint)
            .text_color(colors.text_faint)
            .cursor_pointer()
            .when(open, |status| status.bg(colors.element_hover))
            .hover(|style| style.bg(colors.element_hover))
            .active(|style| style.bg(colors.element_active))
            .child(StateDot::with_color(health_color(health, theme)).size(metrics.status_dot))
            .when_some(name, |status, name| {
                // The name (at most `STATUS_NAME_MAX`) stays readable:
                // the node beside it is cut short first (ENV-06, ENV-12).
                status.child(
                    div()
                        .min_w_0()
                        .ml(px(STATUS_NAME_GAP))
                        .max_w(px(STATUS_NAME_MAX))
                        .truncate()
                        .text_color(if open {
                            colors.text_strong
                        } else {
                            colors.text_secondary
                        })
                        .child(name),
                )
            })
            .map(|status| {
                let detail = StatusDetail {
                    connected,
                    endpoint,
                    suffix,
                    partial,
                    no_data,
                    name_chars,
                };
                Self::status_detail(status, detail, theme)
            })
            .child(
                div().flex_none().ml(px(STATUS_CHEVRON_GAP)).child(
                    Icon::new(IconName::ChevronDown)
                        .size(px(STATUS_CHEVRON))
                        .color(if elsewhere.is_empty() {
                            colors.text_muted
                        } else {
                            colors.accent
                        }),
                ),
            )
            .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            .on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
                if !this.menus.is_open(&SidebarMenu::Status) {
                    // The mute row starts with the environment on screen.
                    this.mute_target = None;
                }
                this.menus.toggle(SidebarMenu::Status, down_position(event));
                cx.notify();
            }));
        if open {
            status
        } else {
            status.tooltip(Tooltip::text(tooltip))
        }
    }

    /// The footer switcher's middle: connected, the node (its first DNS
    /// label: `icinga-master-02` of `icinga-master-02.example.com`; the
    /// tooltip and the details have it in full), cut short beyond
    /// [`STATUS_NODE_MAX`] and before the environment's name when room is
    /// short, then the age of the last event (or `no data 3m`) in a slot of
    /// its own right after it (it never shortens the node as it ticks,
    /// ENV-06). Without room for a few characters of the node it gives way
    /// whole ([`node_has_room`]) rather than leave a lone ellipsis. A node
    /// that sees only part of the cluster (a satellite) is coloured,
    /// nothing more (ENV-12): the tooltip, the details and the summary bar
    /// say what it means. Otherwise what the connection does
    /// (`retry in 12s`).
    fn status_detail(status: Stateful<Div>, detail: StatusDetail, theme: &Theme) -> Stateful<Div> {
        let slot = if detail.no_data {
            NO_DATA_SLOT_CHARS
        } else {
            AGE_SLOT_CHARS
        };
        match (detail.connected, detail.suffix) {
            // The node stays while it has room (the connected node is
            // visible, PLAN §4.3); it is cut short before the environment's
            // name. The age (or `no data 3m`) follows it, left-aligned in
            // its fixed slot, so the free room sits before the chevron and
            // nothing after the slot moves with what it says (16f).
            (true, Some(age)) => status
                .when(node_has_room(theme, detail.name_chars, slot), |status| {
                    status.child(
                        div()
                            .flex_shrink(8.)
                            .min_w_0()
                            .max_w(px(STATUS_NODE_MAX))
                            .ml(px(STATUS_NODE_GAP))
                            .truncate()
                            .when(detail.partial, |node| {
                                node.text_color(theme.states.text.warning)
                            })
                            .child(short_node(&detail.endpoint).to_owned()),
                    )
                })
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .ml(px(STATUS_AGE_GAP))
                        .w(theme.text.hint * (slot * ic_ui_kit::CHAR_WIDTH))
                        .when(detail.no_data, |age| {
                            age.text_color(theme.states.text.warning)
                        })
                        .child(age),
                )
                .child(div().flex_1()),
            (_, suffix) => status
                .child(
                    div()
                        .min_w_0()
                        .ml(px(STATUS_NAME_GAP))
                        .truncate()
                        .child(suffix.unwrap_or(detail.endpoint)),
                )
                .child(div().flex_1()),
        }
    }

    /// The footer's clock (NOTE-05): opens the notification centre; the
    /// unread count of the environment on screen on it, a bell-off while
    /// its notifications are paused (all environments', or its own).
    fn render_clock(&self, now: Timestamp, cx: &Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let colors = theme.colors;
        let metrics = theme.metrics;
        let state = self.state.read(cx);
        let centre_open = self.menus.is_open(&SidebarMenu::Notifications);
        let badge = crate::notifications::entry::badge(state.unread_notifications());
        let paused = state.active_pause(now).is_some();
        div()
            .relative()
            .flex_none()
            .child({
                let button = IconButton::new(
                    "notification-centre",
                    if paused {
                        IconName::BellOff
                    } else {
                        IconName::Clock
                    },
                )
                .icon_size(metrics.icon_large)
                .selected(centre_open)
                .on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
                    this.toggle_notifications(down_position(event));
                    cx.notify();
                }));
                if centre_open {
                    button
                } else {
                    button.tooltip(Tooltip::new(menus::notifications_tooltip(
                        state.unread_notifications(),
                        state.active_pause(now),
                        now,
                    )))
                }
            })
            .when_some(badge, |slot, badge| {
                // The unread count at the icon's top right. It is anchored
                // by its right edge and grows over the icon, so `28` or
                // `99+` never covers the health dot next to it (ENV-06).
                slot.child(
                    div()
                        .absolute()
                        .top(px(2.))
                        .right(px(-4.))
                        .h(px(12.))
                        .min_w(px(12.))
                        .px(px(3.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .border_1()
                        .border_color(colors.sidebar_background)
                        .bg(colors.accent)
                        .text_size(px(8.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(colors.on_accent)
                        .child(badge),
                )
            })
            .when(centre_open, |slot| {
                slot.child(
                    Popover::new(self.notification_centre(now, cx))
                        .above()
                        .gap(px(8.)),
                )
            })
    }

    fn render_footer(&self, cx: &Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let colors = theme.colors;
        let metrics = theme.metrics;
        let now = Timestamp::now();
        let details_open = self.menus.is_open(&SidebarMenu::Status);
        let footer_open = self.menus.is_open(&SidebarMenu::Footer);
        let status = self.render_status(now, cx);
        let add = IconButton::new("new-dashboard", IconName::Plus)
            .icon_size(metrics.icon_large)
            .selected(footer_open)
            .on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
                this.menus.toggle(SidebarMenu::Footer, down_position(event));
                cx.notify();
            }));
        // Icon buttons are wider than their icons: the padding and gap put
        // the icons where the design draws them (12px in, 14px apart).
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(FOOTER_GAP))
            .h(Metrics::with_rule(metrics.footer_height))
            .pl(px(FOOTER_LEFT))
            .pr(px(FOOTER_RIGHT))
            .border_t_1()
            .border_color(colors.border_header)
            .child(
                IconButton::new("toggle-sidebar", IconName::PanelLeft)
                    .icon_size(metrics.icon_large)
                    .tooltip(Tooltip::new("Hide sidebar").key(menus::toggle_key()))
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(ToggleSidebar), cx);
                    }),
            )
            .child(self.render_clock(now, cx))
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .child(status)
                    .when(details_open, |slot| {
                        slot.child(Popover::new(self.details_menu(now, cx)).above().gap(px(8.)))
                    }),
            )
            .child(
                div()
                    .relative()
                    .flex_none()
                    .child(if footer_open {
                        add
                    } else {
                        add.tooltip(Tooltip::new("New dashboard or group").key(new_key()))
                    })
                    .when(footer_open, |slot| {
                        slot.child(Popover::new(self.footer_menu(cx)).above().align_right())
                    }),
            )
    }
}

/// Space left and right of the footer switcher's text (its hover
/// background reaches that far, as around an icon button's icon).
const STATUS_PADDING: f32 = 3.;
/// The widest the footer switcher shows an environment's name; it gives
/// way to the node down to about nine characters.
const STATUS_NAME_MAX: f32 = 90.;
/// The widest the footer switcher shows the connected node (about twelve
/// characters: `sat-ams-01`, `icinga-maste…`); the name gets the rest.
const STATUS_NODE_MAX: f32 = 80.;

/// A node's name as the footer shows it: its first DNS label
/// (`icinga-master-02` of `icinga-master-02.example.com`); an IP address
/// or a single label as it is.
pub(super) fn short_node(name: &str) -> &str {
    if name.parse::<std::net::IpAddr>().is_ok() {
        return name;
    }
    match name.split_once('.') {
        Some((first, _)) if !first.is_empty() => first,
        _ => name,
    }
}
/// The footer age's slot, in characters (`59s`, `23h`).
const AGE_SLOT_CHARS: f32 = 3.;
/// The slot of `no data 59m`, in characters.
const NO_DATA_SLOT_CHARS: f32 = 11.;
/// The footer's padding left and right of its buttons, and the gap
/// between them (the icons sit where the design draws them).
const FOOTER_LEFT: f32 = 9.;
const FOOTER_RIGHT: f32 = 5.;
const FOOTER_GAP: f32 = 8.;
/// Room before the environment's name (after the dot), the node, the age
/// and the chevron in the footer switcher, and the chevron's size: about
/// 16f's spacing, so `prod-cluster master-01 59s` fits whole.
const STATUS_NAME_GAP: f32 = 5.;
const STATUS_NODE_GAP: f32 = 4.;
const STATUS_AGE_GAP: f32 = 3.;
const STATUS_CHEVRON_GAP: f32 = 1.;
const STATUS_CHEVRON: f32 = 10.;
/// The fewest characters of the node the footer shows (`stg-m…`); with
/// less room it gives way whole.
const NODE_MIN_CHARS: f32 = 6.;

/// What the footer switcher's middle shows (see `Sidebar::status_detail`).
struct StatusDetail {
    connected: bool,
    endpoint: String,
    suffix: Option<String>,
    partial: bool,
    no_data: bool,
    /// The environment name's length, in characters.
    name_chars: usize,
}

/// Whether the footer switcher has room for at least [`NODE_MIN_CHARS`]
/// of the node beside an environment name of `name_chars` characters and
/// an age slot of `slot_chars`. The text is monospaced and every width in
/// the footer is the theme's, so this is arithmetic: with less room the
/// node gives way whole (`prod-cluster no data 4m`; the banner and the
/// details name it) rather than leave a lone ellipsis.
fn node_has_room(theme: &Theme, name_chars: usize, slot_chars: f32) -> bool {
    let metrics = theme.metrics;
    let char_width = theme.text.hint * ic_ui_kit::CHAR_WIDTH;
    #[expect(
        clippy::cast_precision_loss,
        reason = "an environment name is far shorter than 2^23 characters"
    )]
    let name = (char_width * name_chars as f32).min(px(STATUS_NAME_MAX));
    // The footer (less the sidebar's border and a pixel of rounding), its
    // padding, gaps and three icon buttons, then the switcher's own
    // padding (it reaches a pixel to the left), dot, gaps and chevron.
    let buttons = metrics.icon_button * 3.;
    let footer = px(FOOTER_LEFT + FOOTER_RIGHT + 3. * FOOTER_GAP + 2.);
    let switcher = px(STATUS_PADDING
        + 2.
        + STATUS_NAME_GAP
        + STATUS_NODE_GAP
        + STATUS_AGE_GAP
        + STATUS_CHEVRON_GAP
        + STATUS_CHEVRON)
        + metrics.status_dot;
    let room = metrics.sidebar_width - footer - buttons - switcher - name - char_width * slot_chars;
    room >= char_width * NODE_MIN_CHARS
}

/// The footer dot's colour (ENV-06): green while live, yellow when stale,
/// red when reconnecting or failed, grey otherwise.
pub(crate) fn health_color(health: Health, theme: &Theme) -> gpui::Hsla {
    match health {
        Health::Live => theme.states.fill.ok,
        Health::Stale => theme.states.fill.warning,
        Health::Reconnecting | Health::Failed => theme.states.fill.critical,
        Health::Connecting | Health::Idle => theme.states.fill.pending,
    }
}

impl Render for Sidebar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.viewport = window.viewport_size().height;
        let theme = cx.theme();
        let width = theme.metrics.sidebar_width;
        let border = theme.colors.border_split;
        let background = theme.colors.sidebar_background;
        let header = self.render_header(window, cx);
        let groups = self.render_groups(cx);
        let footer = self.render_footer(cx);
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(width)
            .h_full()
            .bg(background)
            .border_r_1()
            .border_color(border)
            .child(header)
            .child(groups)
            .child(footer)
    }
}

/// A row's mark in its fixed slot: a state dot, or an icon (faint and
/// neutral, so colour keeps meaning state).
pub(crate) fn mark(mark: Mark, theme: &Theme) -> AnyElement {
    let slot = div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .w(theme.metrics.sidebar_dot);
    match mark {
        Mark::Dot(state) => slot
            .child(dot(state, theme).size(theme.metrics.sidebar_dot))
            .into_any_element(),
        Mark::Icon(icon) => slot
            .child(Icon::new(icon).size(px(11.)).color(theme.colors.text_faint))
            .into_any_element(),
    }
}

/// The state dot of a dashboard or an open tab.
fn dot(dot: Dot, theme: &Theme) -> StateDot {
    match dot {
        Dot::State(state) => StateDot::new(state),
        Dot::Handled(state) => StateDot::new(state).hollow(true),
        Dot::Ok => StateDot::with_color(theme.states.fill.ok),
        Dot::Empty => StateDot::with_color(theme.states.fill.pending),
    }
}

/// A row like a dashboard's, with a grey dot: "No dashboards yet", and a
/// link to create one.
fn empty_note(
    text: &'static str,
    link: Option<&'static str>,
    theme: &Theme,
    cx: &Context<Sidebar>,
) -> AnyElement {
    let metrics = theme.metrics;
    div()
        .flex_1()
        .min_h_0()
        .pt(px(6.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(12.))
                .h(metrics.item_row_height)
                .pl(px(14.))
                .pr(metrics.sidebar_padding)
                .text_size(theme.text.row)
                .text_color(theme.colors.text_muted)
                .child(StateDot::with_color(theme.states.fill.pending).size(metrics.sidebar_dot))
                .child(text),
        )
        .when_some(link, |note, link| {
            note.child(
                div().pl(px(34.)).child(
                    Link::new("sidebar-empty-new", link)
                        .text_size(theme.text.small)
                        .on_click(cx.listener(|_, _: &ClickEvent, _, cx| {
                            cx.emit(SidebarEvent::NewDashboard(None));
                        })),
                ),
            )
        })
        .into_any_element()
}
