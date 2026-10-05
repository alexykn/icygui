//! The sidebar: window controls and dashboard search in the header, groups
//! (folders) of dashboards, the objects open as tabs, and the footer with
//! the sidebar toggle, the notification centre, the connection status and
//! `+`.
//!
//! The footer's "last event" age refreshes with the workspace's clock
//! (UI-04).

mod model;

use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Div, Entity, FontWeight,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString, Stateful,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div,
    prelude::FluentBuilder as _, px,
};
use ic_model::Timestamp;
use ic_ui_kit::input::{InputEvent, InputState};
use ic_ui_kit::{
    ActiveTheme as _, Divider, DividerColor, GlyphButton, Icon, IconButton, IconName, Metrics,
    StateDot, TextField, Theme, Tooltip,
};

use crate::app_state::{AppState, Health};
use crate::chrome::{Controls, WindowControls, WindowDrag};
use crate::workspace::ToggleSidebar;

use self::model::{Dot, OpenTab, SidebarGroup, SidebarItem};

/// The sidebar view.
pub(crate) struct Sidebar {
    state: Entity<AppState>,
    search: Entity<InputState>,
    query: String,
    drag: WindowDrag,
    _subscriptions: Vec<Subscription>,
}

impl Sidebar {
    pub(crate) fn new(
        state: Entity<AppState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Search dashboards…")
                .clean_on_escape()
        });
        let subscriptions = vec![
            cx.observe(&state, |_, _, cx| cx.notify()),
            cx.subscribe(
                &search,
                |this: &mut Self, search, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.query = search.read(cx).value().to_string();
                        cx.notify();
                    }
                },
            ),
        ];
        Self {
            state,
            search,
            query: String::new(),
            drag: WindowDrag::default(),
            _subscriptions: subscriptions,
        }
    }

    /// The search field's state.
    #[cfg(test)]
    pub(crate) fn search_input(&self) -> &Entity<InputState> {
        &self.search
    }

    /// The current search query.
    #[cfg(test)]
    pub(crate) fn query(&self) -> &str {
        &self.query
    }

    fn render_header(&self, window: &Window, cx: &App) -> impl IntoElement + use<> {
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
            .child(div().flex_1().min_w_0().child(TextField::new(&self.search)));
        self.drag.attach(header, controls)
    }

    fn render_groups(&self, cx: &Context<Self>) -> AnyElement {
        let state = self.state.read(cx);
        let theme = cx.theme().clone();
        let Some(environment) = state.environment() else {
            return empty_note("No dashboards yet", &theme);
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
            let note = if self.query.trim().is_empty() {
                "No dashboards yet"
            } else {
                "No matching dashboards"
            };
            return empty_note(note, &theme);
        }
        let mut rows: Vec<AnyElement> = groups
            .iter()
            .map(|group| Self::render_group(group, &theme, cx))
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
                            .on_mouse_down(gpui::MouseButton::Left, |_, window, _| {
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

    fn render_group(group: &SidebarGroup<'_>, theme: &Theme, cx: &Context<Self>) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .flex_none()
            .pb(px(6.))
            .child(Self::render_group_header(group, theme, cx))
            .children(
                group
                    .items
                    .iter()
                    .map(|item| Self::render_item(item, theme, cx)),
            )
            .into_any_element()
    }

    fn render_group_header(
        group: &SidebarGroup<'_>,
        theme: &Theme,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        let colors = theme.colors;
        let group_id = group.group.id.clone();
        let hover_group = SharedString::from(format!("sidebar-group-{group_id}"));
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
            .when(group.active, |row| row.bg(colors.group_active))
            .when(!group.active, |row| {
                row.hover(|style| style.bg(colors.group_hover))
            })
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(theme.text.heading)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(if group.active {
                        colors.text_emphasis
                    } else {
                        colors.text_secondary
                    })
                    .child(group.group.name.clone()),
            )
            .child(reveal(
                div()
                    .flex_none()
                    .child(Icon::new(chevron).size(px(12.)).color(colors.text_muted)),
                group.active || !group.expanded,
            ))
            .child(div().flex_1())
            .child(reveal(group_actions(&group_id, theme), group.active))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.state.update(cx, |state, cx| {
                    if state.toggle_group(&group_id) {
                        cx.notify();
                    }
                });
            }))
    }

    fn render_item(item: &SidebarItem<'_>, theme: &Theme, cx: &Context<Self>) -> AnyElement {
        let colors = theme.colors;
        let metrics = theme.metrics;
        let dot = dot(item.dot, theme);
        let reference = item.reference.clone();
        let id = SharedString::from(format!(
            "dashboard-{}-{}",
            item.reference.group_id, item.reference.dashboard_id
        ));
        div()
            .id(id)
            .flex()
            .flex_none()
            .items_center()
            .gap(px(12.))
            .h(metrics.item_row_height)
            .pl(px(14.))
            .pr(metrics.sidebar_padding)
            .cursor_pointer()
            .when(item.selected, |row| row.bg(colors.item_active))
            .when(!item.selected, |row| {
                row.hover(|style| style.bg(colors.item_hover))
            })
            .child(dot.size(metrics.sidebar_dot))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(theme.text.row)
                    .text_color(if item.selected {
                        colors.text_emphasis
                    } else {
                        colors.text_secondary
                    })
                    .child(SharedString::from(item.name.to_owned())),
            )
            .when_some(item.count, |row, count| {
                row.child(
                    div()
                        .flex_none()
                        .text_size(theme.text.label)
                        .text_color(colors.text_muted)
                        .child(count.to_string()),
                )
            })
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                let reference = reference.clone();
                this.state.update(cx, |state, cx| {
                    if state.select(reference) {
                        cx.notify();
                    }
                });
            }))
            .into_any_element()
    }

    fn render_footer(&self, cx: &App) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let colors = theme.colors;
        let metrics = theme.metrics;
        let state = self.state.read(cx);
        let now = Timestamp::now();
        let connection = state.connection();
        let health_color = match connection.health(now) {
            Health::Live => theme.states.ok,
            Health::Stale => theme.states.warning,
            Health::Down => theme.states.critical,
            Health::Idle => theme.states.pending,
        };
        let mut label = connection.label(now);
        if state.is_demo() {
            label.push_str(" · demo");
        }
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
                    .tooltip(Tooltip::new("Hide sidebar").key(toggle_key()))
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(ToggleSidebar), cx);
                    }),
            )
            .child(
                IconButton::new("notification-centre", IconName::Clock)
                    .icon_size(metrics.icon_large)
                    .tooltip(Tooltip::new("Notifications"))
                    .on_click(|_, _, _| {
                        tracing::debug!("notification centre comes with M5");
                    }),
            )
            .child(
                div()
                    .id("environment-status")
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap(px(6.))
                    .ml(px(2.))
                    .h(px(22.))
                    .text_size(theme.text.hint)
                    .text_color(colors.text_faint)
                    .cursor_pointer()
                    .hover(|style| style.text_color(colors.text_muted))
                    .child(StateDot::with_color(health_color).size(metrics.status_dot))
                    .child(div().min_w_0().truncate().child(label))
                    .tooltip(Tooltip::text("Switch environment"))
                    .on_click(|_, _, _| {
                        tracing::debug!("environment switcher comes with M2");
                    }),
            )
            .child(
                IconButton::new("new-dashboard", IconName::Plus)
                    .icon_size(metrics.icon_large)
                    .tooltip(Tooltip::new("New dashboard"))
                    .on_click(|_, _, _| {
                        tracing::debug!("new dashboard: the dashboard editor comes with M4");
                    }),
            )
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

/// The group row's `+` (new dashboard) and `···` (group menu) buttons, sized
/// and spaced like the design's glyphs.
fn group_actions(group_id: &str, theme: &Theme) -> Div {
    let button = |id: String, glyph: &'static str, size: f32, tooltip: &'static str| {
        GlyphButton::new(SharedString::from(id), glyph)
            .text_size(px(size))
            .color(theme.colors.text)
            .tooltip(Tooltip::new(tooltip))
    };
    // The buttons' reach (4px on each side) makes the design's 8px between
    // the glyphs.
    div()
        .flex()
        .flex_none()
        .items_center()
        .child(
            button(format!("group-add-{group_id}"), "+", 15., "New dashboard").on_click(
                |_, _, _| {
                    tracing::debug!("new dashboard: the dashboard editor comes with M4");
                },
            ),
        )
        .child(
            button(
                format!("group-menu-{group_id}"),
                "···",
                13.,
                "Group options",
            )
            .on_click(|_, _, _| {
                tracing::debug!("group menu: rename, reorder and delete come with M4");
            }),
        )
}

/// The state dot of a dashboard or an open tab.
fn dot(dot: Dot, theme: &Theme) -> StateDot {
    match dot {
        Dot::State(state) => StateDot::new(state),
        Dot::Ok => StateDot::with_color(theme.states.ok),
        Dot::Empty => StateDot::with_color(theme.states.pending),
    }
}

/// A row like a dashboard's, with a grey dot: "No dashboards yet".
fn empty_note(text: &'static str, theme: &Theme) -> AnyElement {
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
        .into_any_element()
}

/// The shortcut for [`ToggleSidebar`], as the tooltip shows it.
fn toggle_key() -> &'static str {
    if cfg!(target_os = "macos") {
        "⌘B"
    } else {
        "ctrl-b"
    }
}
