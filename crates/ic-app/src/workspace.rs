//! The root view: the sidebar and the main area, which shows the selected
//! dashboard (list and detail pane, screens 2a–2c) or an object opened as a
//! tab, full width.

use std::collections::HashMap;
use std::time::Duration;

use gpui::{
    Action, App, AppContext as _, Context, Entity, FocusHandle, Focusable as _,
    InteractiveElement as _, IntoElement, KeyBinding, ParentElement as _, Render, Styled as _,
    Subscription, Task, Window, div, prelude::FluentBuilder as _, px,
};
use ic_model::ObjectKey;
use ic_rules::DashboardRef;
use ic_ui_kit::{ActiveTheme as _, Divider, DividerColor, IconButton, IconName, Theme, Tooltip};

use crate::app_state::AppState;
use crate::chrome::{Controls, WindowControls};
use crate::dashboard::DashboardView;
use crate::pane::{ObjectPane, PaneMode};
use crate::sidebar::Sidebar;

/// How often relative times (time in state, the footer's last event)
/// refresh (UI-04).
const CLOCK_TICK: Duration = Duration::from_secs(1);

/// Shows or hides the sidebar.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct ToggleSidebar;

/// Registers the workspace's key bindings (`secondary` is cmd on macOS and
/// ctrl elsewhere).
pub(crate) fn bind_keys(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("secondary-b", ToggleSidebar, None)]);
    crate::actions::bind_keys(cx);
}

/// What the main area shows.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Shown {
    Dashboard(Option<DashboardRef>),
    Tab(ObjectKey),
}

/// An object open as a tab.
struct TabPane {
    view: Entity<ObjectPane>,
}

/// The window's content.
pub(crate) struct Workspace {
    state: Entity<AppState>,
    sidebar: Entity<Sidebar>,
    dashboard: Entity<DashboardView>,
    tabs: HashMap<ObjectKey, TabPane>,
    sidebar_open: bool,
    shown: Shown,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
    _clock: Task<()>,
}

impl Workspace {
    pub(crate) fn new(
        state: Entity<AppState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let sidebar = cx.new(|cx| Sidebar::new(state.clone(), window, cx));
        let dashboard = cx.new(|cx| DashboardView::new(state.clone(), cx));
        // Keyboard shortcuts reach the list through the focus path.
        window.focus(&dashboard.focus_handle(cx), cx);
        let subscriptions = vec![cx.observe_in(&state, window, |this, _, window, cx| {
            this.sync(window, cx);
        })];
        let clock = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(CLOCK_TICK).await;
                let ticked = this.update(cx, Workspace::tick);
                if ticked.is_err() {
                    break;
                }
            }
        });
        let shown = Shown::Dashboard(state.read(cx).selected().cloned());
        Self {
            state,
            sidebar,
            dashboard,
            tabs: HashMap::new(),
            sidebar_open: true,
            shown,
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
            _clock: clock,
        }
    }

    /// The dashboard view.
    pub(crate) fn dashboard(&self) -> &Entity<DashboardView> {
        &self.dashboard
    }

    /// Whether the sidebar is shown.
    #[cfg(test)]
    pub(crate) fn is_sidebar_open(&self) -> bool {
        self.sidebar_open
    }

    /// The sidebar view.
    #[cfg(test)]
    pub(crate) fn sidebar(&self) -> &Entity<Sidebar> {
        &self.sidebar
    }

    /// The pane of an object open as a tab.
    #[cfg(test)]
    pub(crate) fn tab(&self, key: &ObjectKey) -> Option<&Entity<ObjectPane>> {
        self.tabs.get(key).map(|tab| &tab.view)
    }

    /// Redraws everything that shows relative times.
    fn tick(&mut self, cx: &mut Context<Self>) {
        self.sidebar.update(cx, |_, cx| cx.notify());
        self.dashboard.update(cx, |_, cx| cx.notify());
        if let Shown::Tab(key) = &self.shown
            && let Some(tab) = self.tabs.get(key)
        {
            tab.view.update(cx, |_, cx| cx.notify());
        }
        cx.notify();
    }

    /// Follows the state: creates and drops tab panes, and moves the focus
    /// when the main area switches between the dashboard and a tab.
    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let open: Vec<ObjectKey> = state.tabs().to_vec();
        let shown = match state.active_tab() {
            Some(key) => Shown::Tab(key.clone()),
            None => Shown::Dashboard(state.selected().cloned()),
        };
        self.tabs.retain(|key, _| open.contains(key));
        for key in open {
            if !self.tabs.contains_key(&key) {
                let state = self.state.clone();
                let sidebar_open = self.sidebar_open;
                let view = cx.new(|cx| {
                    let mut pane = ObjectPane::new(state, key.clone(), PaneMode::Tab, cx);
                    pane.set_sidebar_open(sidebar_open, cx);
                    pane
                });
                self.tabs.insert(key, TabPane { view });
            }
        }
        if shown != self.shown {
            match &shown {
                Shown::Tab(key) => {
                    if let Some(tab) = self.tabs.get(key) {
                        window.focus(&tab.view.focus_handle(cx), cx);
                    }
                }
                Shown::Dashboard(_) => window.focus(&self.dashboard.focus_handle(cx), cx),
            }
            self.shown = shown;
        }
        cx.notify();
    }

    fn toggle_sidebar(&mut self, _: &ToggleSidebar, _: &mut Window, cx: &mut Context<Self>) {
        self.sidebar_open = !self.sidebar_open;
        let open = self.sidebar_open;
        self.dashboard
            .update(cx, |dashboard, cx| dashboard.set_sidebar_open(open, cx));
        for tab in self.tabs.values() {
            tab.view
                .update(cx, |pane, cx| pane.set_sidebar_open(open, cx));
        }
        cx.notify();
    }
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let main = match &self.shown {
            Shown::Tab(key) => self
                .tabs
                .get(key)
                .map(|tab| tab.view.clone().into_any_element()),
            Shown::Dashboard(_) => None,
        }
        .unwrap_or_else(|| self.dashboard.clone().into_any_element());
        div()
            .id("workspace")
            .key_context("Workspace")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::toggle_sidebar))
            .flex()
            .size_full()
            .bg(theme.colors.window_background)
            .font_family(theme.font_family.clone())
            .line_height(theme.line_height)
            .text_color(theme.colors.text)
            .when(self.sidebar_open, |workspace| {
                workspace.child(self.sidebar.clone())
            })
            .child(div().flex().flex_1().min_w_0().h_full().child(main))
    }
}

/// The window controls and a button to bring the sidebar back, for the main
/// area's header while the sidebar is hidden.
pub(crate) fn sidebar_reopen(controls: Controls, theme: &Theme) -> impl IntoElement + use<> {
    let metrics = theme.metrics;
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(8.))
        // Keep the controls where the sidebar header has them (12px in).
        .ml(metrics.sidebar_padding - metrics.list_padding)
        .when(controls != Controls::None, |row| {
            row.child(WindowControls::new(controls)).child(
                Divider::vertical()
                    .color(DividerColor::Window)
                    .length(px(18.))
                    .margin(px(6.)),
            )
        })
        .child(
            IconButton::new("show-sidebar", IconName::PanelLeft)
                .icon_size(theme.metrics.icon_small)
                .tooltip(Tooltip::new("Show sidebar"))
                .on_click(|_, window, cx| window.dispatch_action(Box::new(ToggleSidebar), cx)),
        )
}
