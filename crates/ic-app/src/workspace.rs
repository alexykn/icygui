//! The root view: the sidebar and the main area, which shows the selected
//! dashboard (list and detail pane, screens 2a–2c) or an object opened as a
//! tab, full width.
//!
//! The keyboard belongs to the main area: a click on a spot that takes no
//! focus of its own (the sidebar, its footer, a header) hands the focus to
//! the list or the tab shown, and so does losing the focus, so the
//! shortcuts keep working.

use std::collections::HashMap;
use std::time::Duration;

use gpui::{
    Action, App, AppContext as _, Context, Entity, Focusable as _, InteractiveElement as _,
    IntoElement, KeyBinding, MouseDownEvent, ParentElement as _, Render, Styled as _, Subscription,
    Task, Window, div, prelude::FluentBuilder as _, px,
};
use ic_model::ObjectKey;
use ic_rules::DashboardRef;
use ic_ui_kit::{ActiveTheme as _, Divider, DividerColor, IconButton, IconName, Theme, Tooltip};

use crate::actions::{
    self, ActivateNextTab, ActivatePreviousTab, CloseTab, EditEnvironment, FocusMain,
    ReviewCertificate, SelectDashboard, WORKSPACE_CONTEXT,
};
use crate::app_state::AppState;
use crate::chrome::{Controls, WindowControls, WindowDrag};
use crate::dashboard::DashboardView;
use crate::pane::{ObjectPane, PaneMode};
use crate::sidebar::Sidebar;
use crate::{recovery, window_state};

/// How often relative times (time in state, the footer's last event, the
/// reconnect countdown) refresh (UI-04). Only the rows on screen are
/// rebuilt, so this costs the same for 30 000 rows as for 10.
const CLOCK_TICK: Duration = Duration::from_secs(1);

/// The window's size and position are saved this long after it last
/// moved or changed size (BG-06).
const WINDOW_SAVE_DELAY: Duration = Duration::from_millis(750);

/// Shows or hides the sidebar.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct ToggleSidebar;

/// Registers the workspace's key bindings (`secondary` is cmd on macOS and
/// ctrl elsewhere).
pub(crate) fn bind_keys(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("secondary-b", ToggleSidebar, None)]);
    actions::bind_keys(cx);
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
    /// The recovery screen's header moves the window.
    drag: WindowDrag,
    /// Saves the window's bounds once it stops moving.
    save_window: Option<Task<()>>,
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
        let subscriptions = vec![
            cx.observe_in(&state, window, |this, _, window, cx| {
                this.sync(window, cx);
            }),
            // The focused element went away (a closed pane): the keys go
            // back to the main area.
            cx.on_focus_lost(window, |this, window, cx| {
                this.focus_main(window, cx);
            }),
            cx.observe_window_bounds(window, |this, window, cx| {
                this.window_moved(window, cx);
            }),
        ];
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
            drag: WindowDrag::default(),
            save_window: None,
            _subscriptions: subscriptions,
            _clock: clock,
        }
    }

    /// The window moved or changed size: remember where, and save it once
    /// it rests. A maximized window keeps the size it returns to.
    fn window_moved(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut current = window_state::to_state(window.window_bounds());
        let changed = self.state.update(cx, |state, _| {
            if current.maximized
                && let Some(previous) = state.window_state()
            {
                current = ic_config::WindowState {
                    maximized: true,
                    ..previous
                };
            }
            state.set_window_state(current)
        });
        if !changed {
            return;
        }
        let state = self.state.downgrade();
        self.save_window = Some(cx.spawn(async move |_, cx| {
            cx.background_executor().timer(WINDOW_SAVE_DELAY).await;
            let _ = state.update(cx, |state, _| state.save_ui());
        }));
    }

    /// Shows `key`: in the first dashboard that lists it (the selected one
    /// first), with its pane open, or else as a tab (a notification's
    /// click, the startup switches).
    pub(crate) fn reveal(&mut self, key: &ObjectKey, window: &mut Window, cx: &mut Context<Self>) {
        let target = self.state.read(cx).dashboard_showing(key);
        match target {
            Some(reference) => {
                self.state.update(cx, |state, cx| {
                    if state.select(reference) {
                        cx.notify();
                    }
                });
                self.sync(window, cx);
                self.dashboard
                    .update(cx, |dashboard, cx| dashboard.open_object(key, cx));
            }
            None => self.state.update(cx, |state, cx| {
                if state.open_tab(key.clone()) {
                    cx.notify();
                }
            }),
        }
    }

    /// Puts the cursor on `cursor` and shows `pane` in its pane, as after
    /// following a link (screen 2c at start).
    pub(crate) fn reveal_linked(
        &mut self,
        cursor: &ObjectKey,
        pane: ObjectKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.reveal(cursor, window, cx);
        self.dashboard
            .update(cx, |dashboard, cx| dashboard.open_linked(cursor, pane, cx));
    }

    /// The dashboard view.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn dashboard(&self) -> &Entity<DashboardView> {
        &self.dashboard
    }

    /// Whether the sidebar is shown.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn is_sidebar_open(&self) -> bool {
        self.sidebar_open
    }

    /// The sidebar view.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn sidebar(&self) -> &Entity<Sidebar> {
        &self.sidebar
    }

    /// The pane of an object open as a tab.
    #[cfg(all(test, target_os = "linux"))]
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

    /// Gives the keyboard focus to the main area: the tab shown, else the
    /// dashboard list.
    fn focus_main(&self, window: &mut Window, cx: &mut App) {
        let tab = match &self.shown {
            Shown::Tab(key) => self.tabs.get(key),
            Shown::Dashboard(_) => None,
        };
        let handle = match tab {
            Some(tab) => tab.view.focus_handle(cx),
            None => self.dashboard.focus_handle(cx),
        };
        window.focus(&handle, cx);
    }

    fn on_focus_main(&mut self, _: &FocusMain, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_main(window, cx);
    }

    /// `secondary-1` … `secondary-9`: shows that dashboard and focuses its
    /// list (also when it's already shown, say from the search field).
    fn select_dashboard(
        &mut self,
        action: &SelectDashboard,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(reference) = self.state.read(cx).dashboard_at(action.0) else {
            return;
        };
        self.state.update(cx, |state, cx| {
            if state.select(reference) {
                cx.notify();
            }
        });
        // If that switched the main area, `sync` moves the focus again.
        self.focus_main(window, cx);
    }

    fn next_tab(&mut self, _: &ActivateNextTab, _: &mut Window, cx: &mut Context<Self>) {
        self.cycle_tab(true, cx);
    }

    fn previous_tab(&mut self, _: &ActivatePreviousTab, _: &mut Window, cx: &mut Context<Self>) {
        self.cycle_tab(false, cx);
    }

    fn cycle_tab(&self, forward: bool, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            if state.cycle_tab(forward) {
                cx.notify();
            }
        });
    }

    fn close_tab(&mut self, _: &CloseTab, _: &mut Window, cx: &mut Context<Self>) {
        let Some(active) = self.state.read(cx).active_tab().cloned() else {
            cx.propagate();
            return;
        };
        self.state.update(cx, |state, cx| {
            if state.close_tab(&active) {
                cx.notify();
            }
        });
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(problem) = self.state.read(cx).config_problem().cloned() {
            return div()
                .id("workspace")
                .size_full()
                .font_family(cx.theme().font_family.clone())
                .line_height(cx.theme().line_height)
                .text_color(cx.theme().colors.text)
                .child(recovery::render(&problem, &self.drag, window, cx))
                .into_any_element();
        }
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
            .key_context(WORKSPACE_CONTEXT)
            // Runs after the element under the mouse had its say: one that
            // takes the focus (the list, a tab, the search field), or keeps
            // it where it is (a menu trigger), prevents the default.
            .on_any_mouse_down(cx.listener(|this, _: &MouseDownEvent, window, cx| {
                if !window.default_prevented() {
                    this.focus_main(window, cx);
                }
            }))
            .on_action(cx.listener(Self::toggle_sidebar))
            .on_action(cx.listener(Self::on_focus_main))
            .on_action(cx.listener(Self::select_dashboard))
            .on_action(cx.listener(Self::next_tab))
            .on_action(cx.listener(Self::previous_tab))
            .on_action(cx.listener(Self::close_tab))
            // The environment settings handle these (the banner's links).
            .on_action(|_: &ReviewCertificate, _, _| {
                tracing::info!("reviewing the certificate is part of the environment settings");
            })
            .on_action(|_: &EditEnvironment, _, _| {
                tracing::info!("the environment settings open here");
            })
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
            .into_any_element()
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
