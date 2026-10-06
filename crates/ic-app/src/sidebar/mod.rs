//! The sidebar: window controls and dashboard search in the header, groups
//! (folders) of dashboards, the objects open as tabs, and the footer with
//! the sidebar toggle, the notification centre, the connection status and
//! `+`.
//!
//! The footer's "last event" age refreshes with the workspace's clock
//! (UI-04).
//!
//! In the search field, Enter shows the first matching dashboard and Escape
//! clears the search; both hand the keyboard back to the main area.

mod model;

use std::fmt::Write as _;
use std::time::Instant;

use gpui::{
    AnyElement, AppContext as _, ClickEvent, Context, Div, Entity, FontWeight,
    InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent, ParentElement as _, Pixels,
    Point, Render, SharedString, Stateful, StatefulInteractiveElement as _, Styled as _,
    Subscription, Window, div, prelude::FluentBuilder as _, px,
};
use ic_model::{Timestamp, format_compact};
use ic_ui_kit::input::{Escape, InputEvent, InputState};
use ic_ui_kit::{
    ActiveTheme as _, Divider, DividerColor, GlyphButton, Icon, IconButton, IconName, Menu,
    MenuItem, Metrics, Popover, StateDot, TextField, Theme, Tooltip,
};

use crate::actions::FocusMain;
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
    details: Toggle,
    _subscriptions: Vec<Subscription>,
}

/// Whether the connection details are open (ENV-06).
///
/// A press outside closes them; when that press is on the footer status
/// itself, its click must not open them again, so the press that closed
/// them is remembered.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Toggle {
    open: bool,
    dismissed_at: Option<Point<Pixels>>,
}

impl Toggle {
    /// Whether it's open.
    pub(crate) fn is_open(self) -> bool {
        self.open
    }

    /// A click on the trigger that went down at `down`.
    pub(crate) fn toggle(&mut self, down: Option<Point<Pixels>>) {
        let dismissed = self.dismissed_at.take();
        if down.is_some() && dismissed == down {
            return;
        }
        self.open = !self.open;
    }

    /// A press at `at` outside the popover.
    pub(crate) fn dismiss(&mut self, at: Point<Pixels>) {
        if self.open {
            self.open = false;
            self.dismissed_at = Some(at);
        }
    }

    /// Closes it.
    pub(crate) fn close(&mut self) {
        self.open = false;
        self.dismissed_at = None;
    }
}

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
            details: Toggle::default(),
            _subscriptions: subscriptions,
        }
    }

    /// Whether the connection details are open.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn details_open(&self) -> bool {
        self.details.is_open()
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

    /// The footer's connection status (`● master-01 · 2s`, ENV-06): it
    /// opens the connection details.
    fn render_status(&self, now: Timestamp, cx: &Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let colors = theme.colors;
        let metrics = theme.metrics;
        let state = self.state.read(cx);
        let connection = state.connection();
        let health = connection.health(now);
        let label = connection.label(now);
        let demo = state.is_demo();
        let open = self.details.is_open();
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
                let down = match event {
                    ClickEvent::Mouse(click) => Some(click.down.position),
                    ClickEvent::Keyboard(_) | ClickEvent::Touch(_) => None,
                };
                this.details.toggle(down);
                cx.notify();
            }));
        if open {
            status
        } else {
            status.tooltip(Tooltip::text("Connection details"))
        }
    }

    fn render_footer(&self, cx: &Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let colors = theme.colors;
        let metrics = theme.metrics;
        let state = self.state.read(cx);
        let now = Timestamp::now();
        let open = self.details.is_open();
        let status = self.render_status(now, cx);
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
                div()
                    .relative()
                    .flex_none()
                    .child(
                        IconButton::new("notification-centre", IconName::Clock)
                            .icon_size(metrics.icon_large)
                            .tooltip(Tooltip::new(notifications_tooltip(
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
                    .when(open, |slot| {
                        slot.child(Popover::new(self.details_menu(now, cx)).above().gap(px(8.)))
                    }),
            )
            .child(
                IconButton::new("new-dashboard", IconName::Plus)
                    .icon_size(metrics.icon_large)
                    .tooltip(Tooltip::new("New dashboard"))
                    .on_click(|_, _, _| {
                        tracing::debug!("new dashboard: the dashboard editor opens here");
                    }),
            )
    }

    /// The connection details above the footer status (ENV-06): the
    /// environment, the endpoint and its version, the state, the last event,
    /// the API user, and "Reload from Icinga".
    fn details_menu(&self, now: Timestamp, cx: &Context<Self>) -> Menu {
        let theme = cx.theme();
        let colors = theme.colors;
        let state = self.state.read(cx);
        let connection = state.connection();
        let line = |key: &'static str, value: String| {
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
                        .min_w_0()
                        .truncate()
                        .text_color(colors.text)
                        .child(value),
                )
        };
        let mut menu = Menu::new("connection-details").min_width(px(300.));
        match state.environment() {
            Some(environment) => {
                let title = if state.is_demo() {
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
                                .child(environment.url.clone()),
                        ),
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
        let lines = detail_lines(state, now)
            .into_iter()
            .map(|(key, value)| line(key, value));
        menu = menu
            .separator()
            .element(div().flex().flex_col().gap(px(4.)).children(lines));
        let can_reload = state.environment().is_some() && !connection.is_starting();
        menu.separator()
            .item(
                MenuItem::new("reload", "Reload from Icinga")
                    .disabled(!can_reload)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.details.close();
                        this.state.update(cx, |state, cx| {
                            if state.refresh(Instant::now()) {
                                cx.notify();
                            }
                        });
                        cx.notify();
                    })),
            )
            .on_dismiss(cx.listener(|this, event: &MouseDownEvent, _, cx| {
                this.details.dismiss(event.position);
                cx.notify();
            }))
    }
}

/// The connection details' lines: the state, the endpoint and its
/// version, the last event and the API user (with how many of the
/// permissions the client asks for it lacks).
fn detail_lines(state: &AppState, now: Timestamp) -> Vec<(&'static str, String)> {
    let connection = state.connection();
    let mut lines = vec![("status", connection.describe(now))];
    if !connection.endpoint.is_empty() {
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
        let missing = ic_core::missing_permissions(info).len();
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

/// The notification centre button's tooltip: unread notifications and a
/// pause.
fn notifications_tooltip(unread: usize, paused_until: Option<Timestamp>, now: Timestamp) -> String {
    let mut text = "Notifications".to_owned();
    if unread > 0 {
        let _ = write!(text, " · {unread} unread");
    }
    if let Some(until) = paused_until.filter(|until| *until > now) {
        let _ = write!(text, " · paused until {}", crate::format::clock(until, now));
    }
    text
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
        assert_eq!(keys, ["status", "endpoint", "version", "last event"]);
        assert_eq!(lines[0].1, "connected for 1m");
        assert_eq!(lines[1].1, "master-01");
        assert_eq!(lines[3].1, "1m ago");
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
    fn the_details_toggle_ignores_the_press_that_closed_them() {
        use gpui::{point, px};
        let mut toggle = Toggle::default();
        toggle.toggle(Some(point(px(1.), px(1.))));
        assert!(toggle.is_open());
        let press = point(px(150.), px(880.));
        toggle.dismiss(press);
        toggle.toggle(Some(press));
        assert!(!toggle.is_open(), "the press on the trigger closed them");
        toggle.toggle(Some(point(px(151.), px(880.))));
        assert!(toggle.is_open());
        toggle.close();
        assert!(!toggle.is_open());
    }
}
