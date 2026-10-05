//! The root view: the sidebar and the main area.
//!
//! The main area shows the selected dashboard's header and summary bar; the
//! dashboard list and the detail panes (design screens 2a–2c) come next.

use gpui::{
    Action, App, AppContext as _, Context, Entity, FocusHandle, InteractiveElement as _,
    IntoElement, KeyBinding, ParentElement as _, Render, SharedString, Styled as _, Subscription,
    Window, div, prelude::FluentBuilder as _, px,
};
use ic_config::{ObjectKind, View};
use ic_core::snapshot::Summary;
use ic_model::{CheckableState, HostState, ServiceState};
use ic_ui_kit::{
    ActiveTheme as _, Divider, DividerColor, IconButton, IconName, PaneHeader, SummaryBar,
    SummaryItem, Theme, Tooltip,
};

use crate::app_state::AppState;
use crate::chrome::{Controls, WindowControls, WindowDrag};
use crate::sidebar::Sidebar;

/// Shows or hides the sidebar.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct ToggleSidebar;

/// Registers the workspace's key bindings (`secondary` is cmd on macOS and
/// ctrl elsewhere).
pub(crate) fn bind_keys(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("secondary-b", ToggleSidebar, None)]);
}

/// The window's content.
pub(crate) struct Workspace {
    state: Entity<AppState>,
    sidebar: Entity<Sidebar>,
    sidebar_open: bool,
    focus_handle: FocusHandle,
    drag: WindowDrag,
    _subscriptions: Vec<Subscription>,
}

impl Workspace {
    pub(crate) fn new(
        state: Entity<AppState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let sidebar = cx.new(|cx| Sidebar::new(state.clone(), window, cx));
        let focus_handle = cx.focus_handle();
        // Keyboard shortcuts reach the workspace through the focus path.
        window.focus(&focus_handle, cx);
        let subscriptions = vec![cx.observe(&state, |_, _, cx| cx.notify())];
        Self {
            state,
            sidebar,
            sidebar_open: true,
            focus_handle,
            drag: WindowDrag::default(),
            _subscriptions: subscriptions,
        }
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

    fn toggle_sidebar(&mut self, _: &ToggleSidebar, _: &mut Window, cx: &mut Context<Self>) {
        self.sidebar_open = !self.sidebar_open;
        cx.notify();
    }

    fn render_main(&self, window: &Window, cx: &App) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let state = self.state.read(cx);
        let controls = Controls::of(window, cx);
        let selected = state.selected_dashboard();
        let result = state
            .selected()
            .and_then(|reference| state.result(reference));

        let mut header = PaneHeader::new("main-header").padding(theme.metrics.list_padding);
        if !self.sidebar_open {
            header = header.leading(sidebar_reopen(controls, theme));
        }
        header = match selected {
            Some((_, dashboard)) => header
                .title(dashboard.name.clone())
                .subtitle(view_label(&dashboard.view)),
            None => header.title("icygui"),
        };
        let header = self
            .drag
            .attach(div().id("main-header-drag").child(header), controls);

        let body = match (state.environment(), selected, result) {
            (None, ..) => centered_note("Add an environment to start monitoring.", theme),
            (Some(_), None, _) => centered_note("Select a dashboard in the sidebar.", theme),
            (Some(_), Some((_, dashboard)), None) => {
                centered_note(format!("{} is being evaluated…", dashboard.name), theme)
            }
            (Some(_), Some((_, dashboard)), Some(result)) => {
                if let Some(error) = &result.error {
                    centered_note(format!("Filter error: {error}"), theme)
                } else {
                    centered_note(rows_label(result.rows.len(), &dashboard.view), theme)
                }
            }
        };
        let summary = selected
            .zip(result)
            .map(|((_, dashboard), result)| summary_bar(&result.summary, &dashboard.view));

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(header)
            .children(summary)
            .child(body)
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let main = self.render_main(window, cx);
        div()
            .id("workspace")
            .key_context("Workspace")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::toggle_sidebar))
            .flex()
            .size_full()
            .bg(theme.colors.window_background)
            .font_family(theme.font_family.clone())
            .text_color(theme.colors.text)
            .when(self.sidebar_open, |workspace| {
                workspace.child(self.sidebar.clone())
            })
            .child(main)
    }
}

/// The window controls and a button to bring the sidebar back, for the main
/// header while the sidebar is hidden.
fn sidebar_reopen(controls: Controls, theme: &Theme) -> impl IntoElement + use<> {
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

/// What a view lists, as the header's subtitle.
fn view_label(view: &View) -> &'static str {
    match (view.object_kind, view.problems_only) {
        (ObjectKind::Services, true) => "service problems",
        (ObjectKind::Hosts, true) => "host problems",
        (ObjectKind::Services, false) => "services",
        (ObjectKind::Hosts, false) => "hosts",
    }
}

fn rows_label(rows: usize, view: &View) -> String {
    let noun = match (view.object_kind, view.problems_only, rows == 1) {
        (ObjectKind::Services, true, true) => "service problem",
        (ObjectKind::Services, true, false) => "service problems",
        (ObjectKind::Hosts, true, true) => "host problem",
        (ObjectKind::Hosts, true, false) => "host problems",
        (ObjectKind::Services, false, true) => "service",
        (ObjectKind::Services, false, false) => "services",
        (ObjectKind::Hosts, false, true) => "host",
        (ObjectKind::Hosts, false, false) => "hosts",
    };
    if rows == 0 {
        format!("No {}", view_label(view))
    } else {
        format!("{rows} {noun}")
    }
}

/// The counts the summary bar shows: problem states with a count, or the OK
/// count when there are none.
fn summary_items(summary: &Summary, kind: ObjectKind) -> Vec<(CheckableState, u32, &'static str)> {
    let problems: Vec<_> = match kind {
        ObjectKind::Services => vec![
            (
                CheckableState::Service(ServiceState::Critical),
                summary.critical,
                "critical",
            ),
            (
                CheckableState::Service(ServiceState::Warning),
                summary.warning,
                "warning",
            ),
            (
                CheckableState::Service(ServiceState::Unknown),
                summary.unknown,
                "unknown",
            ),
        ],
        ObjectKind::Hosts => vec![
            (CheckableState::Host(HostState::Down), summary.down, "down"),
            (
                CheckableState::Host(HostState::Unreachable),
                summary.unreachable,
                "unreachable",
            ),
        ],
    }
    .into_iter()
    .filter(|(_, count, _)| *count > 0)
    .collect();
    if !problems.is_empty() {
        return problems;
    }
    let ok = match kind {
        ObjectKind::Services => CheckableState::Service(ServiceState::Ok),
        ObjectKind::Hosts => CheckableState::Host(HostState::Up),
    };
    vec![(
        ok,
        summary.ok,
        if kind == ObjectKind::Hosts {
            "up"
        } else {
            "ok"
        },
    )]
}

fn summary_bar(summary: &Summary, view: &View) -> SummaryBar {
    let handled = if view.hide_handled {
        "handled hidden"
    } else {
        "handled shown"
    };
    SummaryBar::new()
        .children(
            summary_items(summary, view.object_kind)
                .into_iter()
                .map(|(state, count, label)| SummaryItem::new(state, count, label)),
        )
        .end(handled)
}

fn centered_note(text: impl Into<SharedString>, theme: &Theme) -> gpui::Div {
    div()
        .flex()
        .flex_1()
        .items_center()
        .justify_center()
        .text_size(theme.text.body)
        .text_color(theme.colors.text_muted)
        .child(text.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(kind: ObjectKind, problems_only: bool) -> View {
        View {
            object_kind: kind,
            problems_only,
            ..View::default()
        }
    }

    #[test]
    fn views_are_described_by_kind_and_problems() {
        assert_eq!(
            view_label(&view(ObjectKind::Services, true)),
            "service problems"
        );
        assert_eq!(view_label(&view(ObjectKind::Hosts, true)), "host problems");
        assert_eq!(view_label(&view(ObjectKind::Hosts, false)), "hosts");
    }

    #[test]
    fn row_counts_read_naturally() {
        let services = view(ObjectKind::Services, true);
        assert_eq!(rows_label(19, &services), "19 service problems");
        assert_eq!(rows_label(1, &services), "1 service problem");
        assert_eq!(rows_label(0, &services), "No service problems");
        assert_eq!(
            rows_label(4, &view(ObjectKind::Services, false)),
            "4 services"
        );
    }

    #[test]
    fn the_summary_lists_problem_states_with_counts() {
        let summary = Summary {
            critical: 12,
            warning: 29,
            unknown: 0,
            ok: 400,
            ..Summary::default()
        };
        let items = summary_items(&summary, ObjectKind::Services);
        let labels: Vec<_> = items
            .iter()
            .map(|(_, count, label)| (*count, *label))
            .collect();
        assert_eq!(labels, [(12, "critical"), (29, "warning")]);
    }

    #[test]
    fn a_quiet_summary_shows_the_ok_count() {
        let summary = Summary {
            ok: 7,
            ..Summary::default()
        };
        let items = summary_items(&summary, ObjectKind::Hosts);
        assert_eq!(items, [(CheckableState::Host(HostState::Up), 7, "up")]);
    }
}
