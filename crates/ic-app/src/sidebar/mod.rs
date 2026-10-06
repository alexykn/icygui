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
//!   groups and imports or exports them (DASH-06).
//!
//! The footer's "last event" age refreshes with the workspace's clock
//! (UI-04). In the search field, Enter shows the first matching dashboard
//! and Escape clears the search; both hand the keyboard back to the main
//! area.

mod menus;
mod model;

use gpui::{
    AnyElement, AppContext as _, ClickEvent, Context, Div, Entity, EventEmitter, FontWeight,
    InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent, ParentElement as _, Pixels,
    Render, SharedString, Stateful, StatefulInteractiveElement as _, Styled as _, Subscription,
    Window, div, prelude::FluentBuilder as _, px,
};
use ic_model::Timestamp;
use ic_rules::{DashboardRef, ScopeSetting};
use ic_ui_kit::input::{Escape, InputEvent, InputState};
use ic_ui_kit::{
    ActiveTheme as _, Divider, DividerColor, GlyphButton, Icon, IconButton, IconName, Link,
    Metrics, Popover, StateDot, TextField, Theme, Tooltip,
};

use crate::actions::FocusMain;
use crate::app_state::{AppState, Health};
use crate::chrome::{Controls, WindowControls, WindowDrag};
use crate::menu_state::{OpenMenu, down_position};
use crate::workspace::ToggleSidebar;

pub(crate) use self::menus::new_key;
pub(crate) use self::model::Dot;
use self::model::{OpenTab, SidebarGroup, SidebarItem};

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
}

/// The sidebar's popup menus.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SidebarMenu {
    /// The connection details and environment switcher (footer status).
    Status,
    /// The footer's `+`.
    Footer,
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
    rename: Option<Rename>,
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
            rename: None,
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
                    &state.snapshot().dashboards,
                    state.selected(),
                    &self.query,
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
        // While a tab is shown, no dashboard is highlighted.
        let selected = state.selected().filter(|_| state.active_tab().is_none());
        let groups = model::groups(
            environment,
            &state.snapshot().dashboards,
            selected,
            &self.query,
        );
        let tabs = model::open_tabs(state.tabs(), state.active_tab(), state.snapshot());
        if groups.is_empty() && tabs.is_empty() {
            return if self.query.trim().is_empty() {
                empty_note("No dashboards yet", Some("new dashboard"), &theme, cx)
            } else {
                empty_note("No matching dashboards", None, &theme, cx)
            };
        }
        let count = environment.groups.len();
        let mut rows: Vec<AnyElement> = groups
            .iter()
            .map(|group| self.render_group(group, count, &theme, cx))
            .collect();
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

    /// The "open" section: objects opened as tabs ("↗ open as tab").
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
            .pr(theme.metrics.sidebar_padding - GlyphButton::REACH)
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
        let dot = dot(item.dot, theme);
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
            .pr(metrics.sidebar_padding - GlyphButton::REACH)
            .cursor_pointer()
            .when(item.selected || menu_open, |row| row.bg(colors.item_active))
            .when(!item.selected && !menu_open, |row| {
                row.hover(|style| style.bg(colors.item_hover))
            })
            .child(dot.size(metrics.sidebar_dot))
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
                    .pr(GlyphButton::REACH)
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

    /// The footer's connection status (`● master-01 · 2s`, ENV-06): it
    /// opens the connection details and the environment switcher.
    fn render_status(&self, now: Timestamp, cx: &Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let colors = theme.colors;
        let metrics = theme.metrics;
        let state = self.state.read(cx);
        let connection = state.connection();
        let health = connection.health(now);
        let label = connection.label(now);
        let demo = state.is_demo();
        let open = self.menus.is_open(&SidebarMenu::Status);
        let status = div()
            .id("environment-status")
            .flex()
            .flex_1()
            .min_w_0()
            .items_center()
            .gap(px(6.))
            .ml(px(2.))
            .h(px(22.))
            .text_size(theme.text.hint)
            .text_color(if open {
                colors.text_muted
            } else {
                colors.text_faint
            })
            .cursor_pointer()
            .hover(|style| style.text_color(colors.text_muted))
            .child(StateDot::with_color(health_color(health, theme)).size(metrics.status_dot))
            .child(div().min_w_0().truncate().child(label))
            // `--demo` says so wherever the status is (ENV-10).
            .when(demo, |status| {
                status.child(
                    div()
                        .flex_none()
                        .px(px(5.))
                        .rounded(metrics.small_radius)
                        .border_1()
                        .border_color(colors.accent.opacity(0.5))
                        .text_color(colors.accent)
                        .child("demo"),
                )
            })
            .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            .on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
                this.menus.toggle(SidebarMenu::Status, down_position(event));
                cx.notify();
            }));
        if open {
            status
        } else {
            status.tooltip(Tooltip::text("Environments and connection details"))
        }
    }

    fn render_footer(&self, cx: &Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let colors = theme.colors;
        let metrics = theme.metrics;
        let state = self.state.read(cx);
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
            .gap(px(8.))
            .h(Metrics::with_rule(metrics.footer_height))
            .pl(px(9.))
            .pr(px(5.))
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
            .child(
                div()
                    .relative()
                    .flex_none()
                    .child(
                        IconButton::new("notification-centre", IconName::Clock)
                            .icon_size(metrics.icon_large)
                            .tooltip(Tooltip::new(menus::notifications_tooltip(
                                state.unread_notifications(),
                                state.paused_until(),
                                now,
                            )))
                            .on_click(|_, _, _| {
                                tracing::debug!("the notification centre opens here");
                            }),
                    )
                    .when(state.unread_notifications() > 0, |slot| {
                        // An unread dot at the icon's top right.
                        slot.child(
                            div()
                                .absolute()
                                .top(px(5.))
                                .right(px(5.))
                                .size(px(6.))
                                .rounded_full()
                                .bg(colors.accent),
                        )
                    }),
            )
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
                        slot.child(
                            Popover::new(self.footer_menu(cx))
                                .above()
                                .align_right()
                                .gap(px(8.)),
                        )
                    }),
            )
    }
}

/// The footer dot's colour (ENV-06): green while live, yellow when stale,
/// red when reconnecting or failed, grey otherwise.
fn health_color(health: Health, theme: &Theme) -> gpui::Hsla {
    match health {
        Health::Live => theme.states.ok,
        Health::Stale => theme.states.warning,
        Health::Reconnecting | Health::Failed => theme.states.critical,
        Health::Connecting | Health::Idle => theme.states.pending,
    }
}

impl Render for Sidebar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let width = theme.metrics.sidebar_width;
        let border = theme.colors.border_split;
        let header = self.render_header(window, cx);
        let groups = self.render_groups(cx);
        let footer = self.render_footer(cx);
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(width)
            .h_full()
            .border_r_1()
            .border_color(border)
            .child(header)
            .child(groups)
            .child(footer)
    }
}

/// The state dot of a dashboard or an open tab.
fn dot(dot: Dot, theme: &Theme) -> StateDot {
    match dot {
        Dot::State(state) => StateDot::new(state),
        Dot::Ok => StateDot::with_color(theme.states.ok),
        Dot::Empty => StateDot::with_color(theme.states.pending),
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
                .child(StateDot::with_color(theme.states.pending).size(metrics.sidebar_dot))
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
