//! The detail pane for one host or service: beside the list (screens 2b and
//! 2c, 620px) or full width as a tab ("↗ open as tab").
//!
//! The pane follows links (the service's host, the host's services, parents
//! and children) with a back button. Action buttons send typed requests to
//! [`AppState::request`]; their dialogs come with M3.

mod host;
pub(crate) mod model;
mod service;

use gpui::{
    AnyElement, App, ClickEvent, ClipboardItem, Context, Entity, EventEmitter, FocusHandle,
    Focusable, InteractiveElement as _, IntoElement, ParentElement as _, Pixels, Render,
    ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Window,
    div, prelude::FluentBuilder as _, px,
};
use ic_model::{ObjectKey, Timestamp};
use ic_ui_kit::{
    ActiveTheme as _, Button, EmptyState, Icon, IconButton, IconName, Link, PaneHeader, Scrollbar,
    Theme, Tooltip,
};

use crate::actions::{
    Acknowledge, ActionRequest, AddComment, CheckNow, Dismiss, ObjectAction, PANE_CONTEXT,
    ScheduleDowntime,
};
use crate::app_state::AppState;
use crate::chrome::{Controls, WindowDrag};
use crate::dashboard::SplitLayout;
use crate::workspace::sidebar_reopen;

/// Where the pane is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PaneMode {
    /// Beside the dashboard list, closed with ×.
    Split,
    /// Full width as a tab; × closes the tab.
    Tab,
}

/// Width a tab's content is kept to, for reading.
const TAB_CONTENT_WIDTH: f32 = 860.;
/// Width of the side column of a wide service tab.
const TAB_SIDE_WIDTH: f32 = 340.;
/// Space between a wide service tab's columns.
const TAB_COLUMN_GAP: f32 = 40.;
/// A service tab at least this wide shows two columns.
const TAB_TWO_COLUMNS_FROM: f32 = 1000.;

/// How a pane's body is laid out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BodyLayout {
    /// One column at the pane's width, beside the list.
    Pane,
    /// One column kept to a readable width: a narrow tab.
    Tab,
    /// A wide tab: the service's output, performance data and notes, with
    /// its check details, variables, groups and links in a column beside
    /// them.
    WideTab,
}

impl BodyLayout {
    /// The layout of a pane in `mode` that is `width` wide.
    pub(crate) fn for_width(mode: PaneMode, width: Pixels) -> Self {
        match mode {
            PaneMode::Split => Self::Pane,
            PaneMode::Tab if width >= px(TAB_TWO_COLUMNS_FROM) => Self::WideTab,
            PaneMode::Tab => Self::Tab,
        }
    }
}

/// What the pane asks its owner to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PaneEvent {
    /// × was clicked.
    Close,
}

/// The host pane's sub-tabs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum HostTab {
    /// The host's services.
    #[default]
    Services,
    /// Recent events from the local log.
    History,
    /// Custom variables.
    Vars,
    /// Check configuration and feature switches, read-only.
    Config,
}

impl HostTab {
    const ALL: [Self; 4] = [Self::Services, Self::History, Self::Vars, Self::Config];

    fn index(self) -> usize {
        Self::ALL.iter().position(|tab| *tab == self).unwrap_or(0)
    }
}

/// The detail pane view.
pub(crate) struct ObjectPane {
    state: Entity<AppState>,
    mode: PaneMode,
    /// The object the pane was opened for: a tab's identity, which stays
    /// while links inside the tab are followed.
    opened: ObjectKey,
    object: ObjectKey,
    /// Objects visited before, for the back button.
    history: Vec<ObjectKey>,
    host_tab: HostTab,
    /// Whether the host pane lists every OK service.
    show_all_ok: bool,
    scroll: ScrollHandle,
    focus_handle: FocusHandle,
    /// With the sidebar hidden, a tab (or a pane covering a narrow list)
    /// shows the window controls and the sidebar button in its header.
    sidebar_open: bool,
    drag: WindowDrag,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<PaneEvent> for ObjectPane {}

impl Focusable for ObjectPane {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ObjectPane {
    pub(crate) fn new(
        state: Entity<AppState>,
        object: ObjectKey,
        mode: PaneMode,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscriptions = vec![cx.observe(&state, |_, _, cx| cx.notify())];
        Self {
            state,
            mode,
            opened: object.clone(),
            object,
            history: Vec::new(),
            host_tab: HostTab::default(),
            show_all_ok: false,
            scroll: ScrollHandle::new(),
            focus_handle: cx.focus_handle(),
            sidebar_open: true,
            drag: WindowDrag::default(),
            _subscriptions: subscriptions,
        }
    }

    /// The object shown.
    pub(crate) fn object(&self) -> &ObjectKey {
        &self.object
    }

    /// Whether the back button has somewhere to go.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn can_go_back(&self) -> bool {
        !self.history.is_empty()
    }

    /// The host pane's selected tab.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn host_tab(&self) -> HostTab {
        self.host_tab
    }

    /// The scrolling part of the body.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn body_scroll(&self) -> &ScrollHandle {
        &self.scroll
    }

    /// Shows `object`, forgetting the back history (the list's cursor
    /// moved).
    pub(crate) fn show(&mut self, object: ObjectKey, cx: &mut Context<Self>) {
        if self.object == object {
            return;
        }
        self.history.clear();
        self.switch_to(object);
        cx.notify();
    }

    /// Follows a link to `object`; the back button returns.
    pub(crate) fn navigate(&mut self, object: ObjectKey, cx: &mut Context<Self>) {
        if self.object == object {
            return;
        }
        self.history.push(self.object.clone());
        self.switch_to(object);
        cx.notify();
    }

    /// Goes back to the previous object.
    pub(crate) fn back(&mut self, cx: &mut Context<Self>) {
        if let Some(previous) = self.history.pop() {
            self.switch_to(previous);
            cx.notify();
        }
    }

    /// Selects a host sub-tab.
    pub(crate) fn select_host_tab(&mut self, tab: HostTab, cx: &mut Context<Self>) {
        if self.host_tab != tab {
            self.host_tab = tab;
            self.scroll.set_offset(gpui::point(px(0.), px(0.)));
            cx.notify();
        }
    }

    /// Tells the pane whether the sidebar is shown.
    pub(crate) fn set_sidebar_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.sidebar_open != open {
            self.sidebar_open = open;
            cx.notify();
        }
    }

    fn switch_to(&mut self, object: ObjectKey) {
        let kind_changed = matches!(self.object, ObjectKey::Host { .. })
            != matches!(object, ObjectKey::Host { .. });
        let host_changed = self.object.host_name() != object.host_name();
        self.object = object;
        if kind_changed || host_changed {
            self.host_tab = HostTab::default();
            self.show_all_ok = false;
        }
        self.scroll.set_offset(gpui::point(px(0.), px(0.)));
    }

    /// The body's layout. A tab is as wide as the window less the sidebar.
    fn body_layout(&self, window: &Window, theme: &Theme) -> BodyLayout {
        let sidebar = if self.sidebar_open {
            theme.metrics.sidebar_width
        } else {
            px(0.)
        };
        BodyLayout::for_width(self.mode, window.viewport_size().width - sidebar)
    }

    /// Closes the pane: beside the list it asks the dashboard to; a tab
    /// closes itself, whatever object it shows after following links.
    pub(crate) fn close(&mut self, cx: &mut Context<Self>) {
        if self.mode == PaneMode::Tab {
            let tab = self.opened.clone();
            self.state.update(cx, |state, cx| {
                if state.close_tab(&tab) {
                    cx.notify();
                }
            });
        }
        cx.emit(PaneEvent::Close);
    }

    /// Sends an action request for the shown object.
    fn request(&self, action: ObjectAction, cx: &mut App) {
        let request = ActionRequest {
            action,
            targets: vec![self.object.clone()],
        };
        self.state.update(cx, |state, _| state.request(request));
    }

    fn on_acknowledge(&mut self, _: &Acknowledge, _: &mut Window, cx: &mut Context<Self>) {
        self.request(ObjectAction::Acknowledge, cx);
    }

    fn on_downtime(&mut self, _: &ScheduleDowntime, _: &mut Window, cx: &mut Context<Self>) {
        self.request(ObjectAction::ScheduleDowntime, cx);
    }

    fn on_check_now(&mut self, _: &CheckNow, _: &mut Window, cx: &mut Context<Self>) {
        self.request(ObjectAction::CheckNow, cx);
    }

    fn on_comment(&mut self, _: &AddComment, _: &mut Window, cx: &mut Context<Self>) {
        self.request(ObjectAction::AddComment, cx);
    }

    /// Escape in a tab: back to the dashboard; the tab stays open.
    fn on_dismiss(&mut self, _: &Dismiss, _: &mut Window, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            if state.show_dashboard() {
                cx.notify();
            }
        });
    }

    fn render_header(&self, window: &Window, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let controls = Controls::of(window, cx);
        let label = match self.object {
            ObjectKey::Host { .. } => "host",
            ObjectKey::Service { .. } => "service",
        };
        let mut header = PaneHeader::new("pane-header");
        // At the window's left edge (a tab, or covering a narrow list) with
        // the sidebar hidden, the header carries the window controls.
        let left_edge = match self.mode {
            PaneMode::Tab => true,
            PaneMode::Split => {
                SplitLayout::for_window(window, self.sidebar_open, &theme.metrics)
                    == SplitLayout::Cover
            }
        };
        if left_edge && !self.sidebar_open {
            header = header.leading(sidebar_reopen(controls, theme));
        }
        if let Some(previous) = self.history.last() {
            header = header.leading(
                IconButton::new("pane-back", IconName::ArrowLeft)
                    .icon_size(theme.metrics.icon_small)
                    .color(theme.colors.text_muted)
                    .tooltip(Tooltip::new(format!("Back to {}", short_name(previous))))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.back(cx))),
            );
        }
        header = header.label(label);
        let header = match self.mode {
            PaneMode::Split => header
                .child(
                    Link::new("open-as-tab", "↗ open as tab")
                        .quiet()
                        .text_size(theme.text.small)
                        .tooltip(Tooltip::new("Open as tab").key(open_as_tab_key()))
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            let object = this.object.clone();
                            this.state.update(cx, |state, cx| {
                                if state.open_tab(object) {
                                    cx.notify();
                                }
                            });
                        })),
                )
                .on_close(cx.listener(|this, _: &ClickEvent, _, cx| this.close(cx))),
            PaneMode::Tab => {
                header.on_close(cx.listener(|this, _: &ClickEvent, _, cx| this.close(cx)))
            }
        };
        // The pane's header is part of the window's top edge, beside the list
        // or as a tab.
        self.drag
            .attach(div().id("pane-header-drag").child(header), controls)
            .into_any_element()
    }
}

impl Render for ObjectPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let header = self.render_header(window, cx);
        let snapshot = self.state.read(cx).snapshot().clone();
        let now = Timestamp::now();
        let layout = self.body_layout(window, &theme);
        let body = match &self.object {
            ObjectKey::Service { key } => match snapshot.services.get(key) {
                Some(service) => service::render(self, &snapshot, service, now, layout, cx),
                None => missing(&self.object, &theme),
            },
            ObjectKey::Host { name } => match snapshot.hosts.get(name) {
                Some(host) => host::render(self, &snapshot, host, now, cx),
                None => missing(&self.object, &theme),
            },
        };
        div()
            .id("object-pane")
            .flex()
            .flex_col()
            .size_full()
            .min_w_0()
            .bg(theme.colors.pane_background)
            .when(self.mode == PaneMode::Tab, |pane| {
                pane.key_context(PANE_CONTEXT)
                    .track_focus(&self.focus_handle)
                    .on_action(cx.listener(Self::on_acknowledge))
                    .on_action(cx.listener(Self::on_downtime))
                    .on_action(cx.listener(Self::on_check_now))
                    .on_action(cx.listener(Self::on_comment))
                    .on_action(cx.listener(Self::on_dismiss))
            })
            .child(header)
            .child(body)
    }
}

/// The pane body for an object the snapshot no longer has.
fn missing(object: &ObjectKey, theme: &Theme) -> AnyElement {
    EmptyState::new(format!("{} is gone", short_name(object)))
        .leading(
            Icon::new(IconName::TriangleAlert)
                .size(px(20.))
                .color(theme.colors.text_faint),
        )
        .detail("It was removed from Icinga, or isn't part of the latest snapshot.")
        .into_any_element()
}

/// `postgres-replication on db-prod-03` or `db-prod-03`.
fn short_name(object: &ObjectKey) -> String {
    match object {
        ObjectKey::Host { name } => name.to_string(),
        ObjectKey::Service { key } => format!("{} on {}", key.name, key.host),
    }
}

/// The shortcut for "open as tab", as tooltips show it.
fn open_as_tab_key() -> &'static str {
    if cfg!(target_os = "macos") {
        "⌘↩"
    } else {
        "ctrl-enter"
    }
}

/// A scrolling column for the pane body with the overlay scrollbar.
fn scroll_area(
    id: &'static str,
    scroll: &ScrollHandle,
    content: impl IntoElement,
) -> impl IntoElement {
    div()
        .relative()
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .child(
            div()
                .id(id)
                .flex()
                .flex_col()
                .size_full()
                .overflow_y_scroll()
                .track_scroll(scroll)
                .child(content),
        )
        .child(Scrollbar::vertical(scroll))
}

/// The action buttons shared by service and host panes.
fn action_buttons(acknowledged: bool, problem: bool, cx: &Context<ObjectPane>) -> impl IntoElement {
    let request = |action: ObjectAction| {
        cx.listener(move |this: &mut ObjectPane, _: &ClickEvent, _, cx| {
            this.request(action.clone(), cx);
        })
    };
    div()
        .flex()
        .flex_wrap()
        .gap(px(8.))
        .when(problem && !acknowledged, |row| {
            row.child(
                Button::new("acknowledge", "acknowledge")
                    .primary()
                    .key_hint("a")
                    .on_click(request(ObjectAction::Acknowledge)),
            )
        })
        .when(acknowledged, |row| {
            row.child(
                Button::new("remove-ack", "remove ack")
                    .on_click(request(ObjectAction::RemoveAcknowledgement)),
            )
        })
        .child(
            Button::new("downtime", "downtime")
                .key_hint("d")
                .on_click(request(ObjectAction::ScheduleDowntime)),
        )
        .child(
            Button::new("check-now", "check now")
                .key_hint("r")
                .on_click(request(ObjectAction::CheckNow)),
        )
        .child(
            Button::new("comment", "comment")
                .key_hint("c")
                .on_click(request(ObjectAction::AddComment)),
        )
}

/// A copy-to-clipboard button, shown while the mouse is over the element
/// marked with `group` (the pane stays as calm as the design otherwise).
fn copy_button(
    id: &'static str,
    text: String,
    tooltip: &'static str,
    group: &'static str,
    theme: &Theme,
) -> impl IntoElement {
    div()
        .flex_none()
        .invisible()
        .group_hover(group, gpui::Styled::visible)
        .child(
            IconButton::new(id, IconName::Copy)
                .icon_size(theme.metrics.icon_small)
                .color(theme.colors.text_faint)
                .tooltip(Tooltip::new(tooltip))
                .on_click(move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                }),
        )
}

/// The hover group of a pane's title (reveals its copy button).
const TITLE_GROUP: &str = "pane-title";

/// A web link that opens in the browser; a URL too long for its cell is
/// cut with an ellipsis and shown whole in the tooltip.
fn web_link(id: SharedString, url: &str) -> impl IntoElement {
    let target = url.to_owned();
    Link::new(id, url.to_owned())
        .truncate()
        .tooltip(Tooltip::new(format!("Open {url} in the browser")))
        .on_click(move |_, _, cx| cx.open_url(&target))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_tabs_get_two_columns() {
        assert_eq!(
            BodyLayout::for_width(PaneMode::Split, px(2000.)),
            BodyLayout::Pane,
            "beside the list the pane keeps its width"
        );
        assert_eq!(
            BodyLayout::for_width(PaneMode::Tab, px(1140.)),
            BodyLayout::WideTab,
            "1440px window less the sidebar"
        );
        assert_eq!(
            BodyLayout::for_width(PaneMode::Tab, px(TAB_TWO_COLUMNS_FROM - 1.)),
            BodyLayout::Tab
        );
    }

    #[test]
    fn short_names_read_service_on_host() {
        assert_eq!(
            short_name(&ObjectKey::service("db-prod-03", "postgres-replication")),
            "postgres-replication on db-prod-03"
        );
        assert_eq!(short_name(&ObjectKey::host("db-prod-03")), "db-prod-03");
    }
}
