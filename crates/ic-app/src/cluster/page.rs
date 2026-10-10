//! The cluster health page (topic 06; PLAN.md §4.2 H): one page per
//! environment, reached from *health* in the sidebar's cluster section,
//! the footer switcher's *cluster health* row and the palette. Its header
//! (its `···`: *edit page*), the health line (endpoints connected and not,
//! Icinga's version and uptime), then the pinned parts: the heartbeat row
//! and the alert block (the worst trouble alert in full, the others one
//! line each). Then its views, a built-in dashboard's (the environment's
//! `health_page`, edited in the dashboard editor): the zones with their
//! endpoints and heartbeats, the checks, the queues and connections,
//! Icinga's global switches; each folds. What it shows is worked out in
//! [`super::health`] and [`super::beats`]; this module only draws it.
//!
//! The editor shows the page as its preview (the draft's views instead of
//! the saved ones, without the header): a click on a view's header selects
//! the view in the inspector.
//!
//! **Keyboard** (a dashboard's model, its `DashboardView` keys): one cursor
//! over the heartbeat row's *settings*, the worst alert's link, the late
//! checks and their `+ N more`, the views' headers, the zones and the
//! endpoints (`↑`/`↓`, `j`/`k`; `tab` to the next view's header); `←`/`→`
//! fold and unfold the view (or show fewer and more late checks); `Enter`
//! follows a link, opens a late check as a tab, folds a view; `Home`/`End`
//! go to the first and last stop, `PageUp`/`PageDown` scroll the body;
//! `Esc` closes the late checks, then leaves the cursor. The cursor is the
//! accent bar on its row (on a link, an accent outline), and the body
//! scrolls to it. *show master-02* puts it on that endpoint's row.
//!
//! **No false green:** without live data the page draws no green dot
//! ([`super::health::Liveness`]); the health line says *as of* when the
//! states came.

use std::time::Duration;

use gpui::{
    AnyElement, ClickEvent, Context, Entity, EventEmitter, FontWeight, Hsla,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div,
    prelude::FluentBuilder as _,
};
use ic_config::{HealthPage as HealthLayout, View, ViewDisplay};
use ic_core::trouble::AlertAction;
use ic_model::{FeatureState, ObjectKey, Timestamp, format_compact};
use ic_ui_kit::{
    ActiveTheme as _, BannerTone, Dismissal, EmptyState, GlyphButton, Icon, IconName, Link, Menu,
    MenuItem, Metrics, ObjectMark, PaneHeader, Popover, StateDot, Theme, Tooltip, px,
};

use super::beats::{AlertLine, BeatCell, BeatRow, BeatTone};
use super::health::{EndpointRow, Liveness, Report, Tile, Tone, ZoneGroup, count_text, report};
use super::spark;
use crate::actions::{
    Dismiss, NextView, OpenSelected, PreviousView, SelectFirst, SelectLast, SelectNext,
    SelectPageDown, SelectPageUp, SelectPrevious,
};
use crate::app_state::AppState;
use crate::chrome::{Controls, WindowDrag};
use crate::lists::view::{Fold, Unfold};
use crate::menu_state::{OpenMenu, down_position};
use crate::workspace::sidebar_reopen;

/// The late checks the banner's fold shows before `+ N more` (the hosts'
/// paging rule).
const LATE_PREVIEW: usize = 7;

/// The page's key context: a dashboard's keys (`DashboardView`), and its
/// own name.
const HEALTH_CONTEXT: &str = "DashboardView HealthPage";

/// A place the page's keyboard cursor stands on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Stop {
    /// The heartbeat row's *settings*.
    BeatSettings,
    /// The worst alert's link.
    AlertLink,
    /// A late check under the alert block (by its place).
    Late(usize),
    /// The late checks' `+ N more` / `− show fewer`.
    LateMore,
    /// A view's header.
    Header(ViewDisplay),
    /// A zone's band.
    Zone(String),
    /// An endpoint's row.
    Endpoint(String),
}

impl Stop {
    /// Whether it sits in the scrolling body (the others are pinned).
    fn in_body(&self) -> bool {
        !matches!(self, Self::BeatSettings | Self::AlertLink)
    }
}

/// What the page tells the workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum HealthPageEvent {
    /// *edit page* in its `···`: the dashboard editor, on this page.
    Edit,
    /// The heartbeat row's *settings*: the environment's trouble alerts.
    Settings,
    /// A view's header clicked in the editor's preview: select it there.
    Pick(String),
}

/// How much of the worst alert's late checks shows under the block.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum LateFold {
    /// None (the link reads *show the late checks*).
    #[default]
    Closed,
    /// The first seven, then `+ N more`.
    Preview,
    /// All of them, then `− show fewer`.
    All,
}

impl LateFold {
    fn is_open(self) -> bool {
        self != Self::Closed
    }

    /// `+ N more` / `− show fewer`.
    fn more_or_fewer(self) -> Self {
        match self {
            Self::All => Self::Preview,
            Self::Preview | Self::Closed => Self::All,
        }
    }
}

/// The page's popup menus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PageMenu {
    /// The header's `···`.
    Options,
}

/// What the editor shows in its preview: the draft's views, and the one
/// selected in the inspector (its header marked).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Preview {
    pub(crate) views: Vec<View>,
    pub(crate) picked: Option<String>,
}

/// The id of a view's section header.
fn section_id(display: ViewDisplay) -> SharedString {
    SharedString::from(format!("health-{}", HealthLayout::id_of(display)))
}

/// The cluster section's *health* page.
pub(crate) struct HealthPage {
    state: Entity<AppState>,
    sidebar_open: bool,
    drag: WindowDrag,
    focus_handle: gpui::FocusHandle,
    /// The folded views, by kind.
    folded: Vec<ViewDisplay>,
    /// The worst alert's late checks are shown, and all of them.
    late: LateFold,
    menus: OpenMenu<PageMenu>,
    /// The editor's preview, when the page is one.
    preview: Option<Preview>,
    /// The keyboard cursor.
    cursor: Option<Stop>,
    /// The cursor moved: the body scrolls to it when next drawn.
    reveal: bool,
    /// The body's scroll, and where the stops' rows sit in it (as last
    /// drawn).
    scroll: gpui::ScrollHandle,
    rows: Vec<(Stop, usize)>,
    /// The stops in their order, the worst alert's link and the late
    /// checks, as last drawn.
    stops: Vec<Stop>,
    worst_action: Option<AlertAction>,
    late_keys: Vec<ObjectKey>,
    _subscription: Subscription,
}

impl EventEmitter<HealthPageEvent> for HealthPage {}

impl HealthPage {
    pub(crate) fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.observe(&state, |_, _, cx| cx.notify());
        Self {
            state,
            sidebar_open: true,
            drag: WindowDrag::default(),
            focus_handle: cx.focus_handle(),
            folded: Vec::new(),
            late: LateFold::Closed,
            menus: OpenMenu::default(),
            preview: None,
            cursor: None,
            reveal: false,
            scroll: gpui::ScrollHandle::new(),
            rows: Vec::new(),
            stops: Vec::new(),
            worst_action: None,
            late_keys: Vec::new(),
            _subscription: subscription,
        }
    }

    /// The page as the editor's preview of `preview`.
    pub(crate) fn preview(
        state: Entity<AppState>,
        preview: Preview,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            preview: Some(preview),
            ..Self::new(state, cx)
        }
    }

    /// The editor's draft changed.
    pub(crate) fn set_preview(&mut self, preview: Preview, cx: &mut Context<Self>) {
        if self.preview.as_ref() != Some(&preview) {
            self.preview = Some(preview);
            cx.notify();
        }
    }

    /// The views the page shows, in order: the preview's or the
    /// environment's (switched-off ones left out).
    pub(crate) fn views(&self, cx: &gpui::App) -> Vec<View> {
        let views = match &self.preview {
            Some(preview) => preview.views.clone(),
            None => self.state.read(cx).environment().map_or_else(
                || HealthLayout::default().views,
                |environment| environment.health_page.views.clone(),
            ),
        };
        views
            .into_iter()
            .filter(|view| view.display.is_health() && !view.health.off)
            .collect()
    }

    /// Tells the page whether the sidebar is shown (the header then needs
    /// no window controls).
    pub(crate) fn set_sidebar_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.sidebar_open != open {
            self.sidebar_open = open;
            cx.notify();
        }
    }

    /// Whether the view of `display` is folded.
    fn is_folded(&self, display: ViewDisplay) -> bool {
        self.folded.contains(&display)
    }

    /// Folds or unfolds the view of `display`.
    fn toggle(&mut self, display: ViewDisplay, cx: &mut Context<Self>) {
        if let Some(index) = self.folded.iter().position(|folded| *folded == display) {
            self.folded.remove(index);
        } else {
            self.folded.push(display);
        }
        cx.notify();
    }

    /// The report the page shows now.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn report(&self, cx: &gpui::App) -> Report {
        let state = self.state.read(cx);
        let now = Timestamp::now();
        report(
            state.snapshot(),
            super::liveness(state.snapshot(), state.connection(), now),
            now,
        )
    }

    /// Where the keyboard cursor stands (tests).
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn cursor(&self) -> Option<&Stop> {
        self.cursor.as_ref()
    }

    /// The stops in their order, the worst alert's link and the late
    /// checks, for `report` and the page's `views`.
    fn layout(&self, report: &Report, views: &[View]) -> Vec<Stop> {
        let mut stops = vec![Stop::BeatSettings];
        if report
            .alerts
            .first()
            .is_some_and(|alert| alert.link.is_some())
        {
            stops.push(Stop::AlertLink);
        }
        if self.late.is_open() && !report.late.is_empty() {
            let shown = if self.late == LateFold::All {
                report.late.len()
            } else {
                report.late.len().min(LATE_PREVIEW)
            };
            stops.extend((0..shown).map(Stop::Late));
            if report.late.len() > LATE_PREVIEW {
                stops.push(Stop::LateMore);
            }
        }
        for view in views {
            stops.push(Stop::Header(view.display));
            if view.display == ViewDisplay::ZonesAndEndpoints && !self.is_folded(view.display) {
                for zone in &report.zones {
                    stops.push(Stop::Zone(zone.name.clone()));
                    stops.extend(
                        zone.endpoints
                            .iter()
                            .map(|endpoint| Stop::Endpoint(endpoint.name.clone())),
                    );
                }
            }
        }
        stops
    }

    /// Puts the cursor on `stop` (the body scrolls to it).
    fn set_cursor(&mut self, stop: Option<Stop>, cx: &mut Context<Self>) {
        if self.cursor != stop {
            self.cursor = stop;
            self.reveal = true;
            cx.notify();
        }
    }

    /// Moves the cursor `by` stops (down when positive), from the first or
    /// last stop when there is none.
    fn move_cursor(&mut self, by: isize, cx: &mut Context<Self>) {
        if self.stops.is_empty() {
            return;
        }
        let last = self.stops.len() - 1;
        let next = match self
            .cursor
            .as_ref()
            .and_then(|cursor| self.stops.iter().position(|stop| stop == cursor))
        {
            Some(index) => index.saturating_add_signed(by).min(last),
            None if by >= 0 => 0,
            None => last,
        };
        let stop = self.stops[next].clone();
        self.set_cursor(Some(stop), cx);
    }

    /// The view the cursor is in (its header, or a row of its body).
    fn cursor_view(&self) -> Option<ViewDisplay> {
        match self.cursor.as_ref()? {
            Stop::Header(display) => Some(*display),
            Stop::Zone(_) | Stop::Endpoint(_) => Some(ViewDisplay::ZonesAndEndpoints),
            _ => None,
        }
    }

    fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        self.move_cursor(1, cx);
    }

    fn select_previous(&mut self, _: &SelectPrevious, _: &mut Window, cx: &mut Context<Self>) {
        self.move_cursor(-1, cx);
    }

    fn select_first(&mut self, _: &SelectFirst, _: &mut Window, cx: &mut Context<Self>) {
        let first = self.stops.first().cloned();
        self.set_cursor(first, cx);
        self.scroll.set_offset(gpui::point(px(0.), px(0.)));
        cx.notify();
    }

    fn select_last(&mut self, _: &SelectLast, _: &mut Window, cx: &mut Context<Self>) {
        let last = self.stops.last().cloned();
        self.set_cursor(last, cx);
        self.scroll.scroll_to_bottom();
        cx.notify();
    }

    /// Scrolls the body by a screenful (`down`: towards the end).
    fn page(&mut self, down: bool, cx: &mut Context<Self>) {
        let height = self.scroll.bounds().size.height;
        let offset = self.scroll.offset();
        let max = self.scroll.max_offset().y;
        let y = if down {
            (offset.y - height).max(-max)
        } else {
            (offset.y + height).min(px(0.))
        };
        self.scroll.set_offset(gpui::point(offset.x, y));
        cx.notify();
    }

    fn select_page_down(&mut self, _: &SelectPageDown, _: &mut Window, cx: &mut Context<Self>) {
        self.page(true, cx);
    }

    fn select_page_up(&mut self, _: &SelectPageUp, _: &mut Window, cx: &mut Context<Self>) {
        self.page(false, cx);
    }

    fn next_view(&mut self, _: &NextView, _: &mut Window, cx: &mut Context<Self>) {
        self.jump_view(true, cx);
    }

    fn previous_view(&mut self, _: &PreviousView, _: &mut Window, cx: &mut Context<Self>) {
        self.jump_view(false, cx);
    }

    /// The cursor to the next (or previous) view's header.
    fn jump_view(&mut self, forward: bool, cx: &mut Context<Self>) {
        let at = self
            .cursor
            .as_ref()
            .and_then(|cursor| self.stops.iter().position(|stop| stop == cursor));
        let headers = self
            .stops
            .iter()
            .enumerate()
            .filter(|(_, stop)| matches!(stop, Stop::Header(_)));
        let target = if forward {
            headers
                .filter(|(index, _)| at.is_none_or(|at| *index > at))
                .map(|(_, stop)| stop.clone())
                .next()
        } else {
            headers
                .filter(|(index, _)| at.is_none_or(|at| *index < at))
                .map(|(_, stop)| stop.clone())
                .next_back()
        };
        if target.is_some() {
            self.set_cursor(target, cx);
        }
    }

    fn unfold(&mut self, _: &Unfold, _: &mut Window, cx: &mut Context<Self>) {
        match self.cursor.clone() {
            Some(Stop::LateMore) => {
                self.late = LateFold::All;
                cx.notify();
            }
            Some(Stop::AlertLink)
                if matches!(self.worst_action, Some(AlertAction::LateChecks { .. })) =>
            {
                if !self.late.is_open() {
                    self.late = LateFold::Preview;
                }
                cx.notify();
            }
            _ => {
                if let Some(display) = self.cursor_view()
                    && self.is_folded(display)
                {
                    self.toggle(display, cx);
                }
            }
        }
    }

    fn fold(&mut self, _: &Fold, _: &mut Window, cx: &mut Context<Self>) {
        match self.cursor.clone() {
            Some(Stop::LateMore) if self.late == LateFold::All => {
                self.late = LateFold::Preview;
                cx.notify();
            }
            Some(Stop::Late(_) | Stop::LateMore) => {
                self.late = LateFold::Closed;
                self.set_cursor(Some(Stop::AlertLink), cx);
            }
            _ => {
                if let Some(display) = self.cursor_view()
                    && !self.is_folded(display)
                {
                    // The cursor goes up to the header it folds under.
                    self.set_cursor(Some(Stop::Header(display)), cx);
                    self.toggle(display, cx);
                }
            }
        }
    }

    fn open_selected(&mut self, _: &OpenSelected, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(stop) = self.cursor.clone() {
            self.activate(&stop, window, cx);
        }
    }

    fn dismiss(&mut self, _: &Dismiss, _: &mut Window, cx: &mut Context<Self>) {
        if self.late.is_open() {
            self.late = LateFold::Closed;
            if matches!(self.cursor, Some(Stop::Late(_) | Stop::LateMore)) {
                self.set_cursor(Some(Stop::AlertLink), cx);
            }
            cx.notify();
        } else {
            self.set_cursor(None, cx);
        }
    }

    /// What Enter (or a click) on `stop` does.
    fn activate(&mut self, stop: &Stop, window: &mut Window, cx: &mut Context<Self>) {
        match stop {
            Stop::BeatSettings => {
                if self.preview.is_none() {
                    cx.emit(HealthPageEvent::Settings);
                }
            }
            Stop::AlertLink => {
                if let Some(action) = self.worst_action.clone() {
                    self.alert_action(&action, window, cx);
                }
            }
            Stop::Late(index) => {
                if let Some(key) = self.late_keys.get(*index).cloned() {
                    self.state.update(cx, |state, cx| {
                        if state.open_tab(key) {
                            cx.notify();
                        }
                    });
                }
            }
            Stop::LateMore => {
                self.late = self.late.more_or_fewer();
                cx.notify();
            }
            Stop::Header(display) => self.toggle(*display, cx),
            // Clicking a node asks nothing more (06).
            Stop::Zone(_) | Stop::Endpoint(_) => {}
        }
    }

    /// The page's header: `cluster health`, the environment and the node
    /// the numbers come from, and when they were updated at the right.
    fn render_header(
        &self,
        report: &Report,
        controls: Controls,
        now: Timestamp,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let state = self.state.read(cx);
        let environment = state
            .environment()
            .map(|environment| environment.name.clone())
            .unwrap_or_default();
        let mut header = PaneHeader::new("health-header")
            .padding(theme.metrics.list_padding)
            .title("cluster health")
            .subtitle(match &report.seen_from {
                Some(node) => format!("{environment} · seen from {node}"),
                None => environment,
            })
            // Its age turns the warning colour once it isn't current.
            .child(
                div()
                    .flex_none()
                    .text_size(theme.text.small)
                    .text_color(if report.live.is_live() {
                        colors.text_faint
                    } else {
                        theme.states.text.warning
                    })
                    .child(updated_text(report, now)),
            )
            .child(self.options_trigger(cx));
        if !self.sidebar_open {
            header = header.leading(sidebar_reopen(controls, theme));
        }
        self.drag
            .attach(div().id("health-header-drag").child(header), controls)
            .into_any_element()
    }

    /// The health line: endpoints connected and not in fixed slots (a
    /// count gaining a digit moves nothing), then room for more status
    /// slots of the same kind (dot, label, age) beside them, and Icinga's
    /// version and uptime at the right. Without live data the dots turn
    /// grey and a third slot says when the states came (`as of 06:56`).
    fn render_health_line(report: &Report, now: Timestamp, theme: &Theme) -> AnyElement {
        let colors = theme.colors;
        let live = report.live.is_live();
        let connected = if live && report.connected > 0 {
            theme.states.fill.ok
        } else {
            theme.states.fill.pending
        };
        let not_connected = if report.not_connected > 0 {
            theme.states.fill.critical
        } else {
            theme.states.fill.pending
        };
        let slots = [
            status_slot(
                connected,
                format!("{} connected", report.connected),
                "999 connected",
                theme,
            ),
            status_slot(
                not_connected,
                format!("{} not connected", report.not_connected),
                "999 not connected",
                theme,
            ),
        ];
        let as_of = match report.live {
            Liveness::Live => None,
            Liveness::Stale { as_of } => Some(as_of.map_or_else(
                || "not current".to_owned(),
                |at| format!("as of {}", crate::format::list_clock(at, now)),
            )),
        };
        div()
            .id("health-line")
            .flex()
            .flex_none()
            .items_center()
            .gap(px(18.))
            .h(Metrics::with_rule(theme.metrics.summary_bar_height))
            .px(theme.metrics.list_padding)
            .border_b_1()
            .border_color(colors.border_header)
            .text_size(theme.text.small)
            .text_color(colors.text_muted)
            .whitespace_nowrap()
            .overflow_hidden()
            .children(slots)
            .children(as_of.map(|as_of| {
                div()
                    .id("health-as-of")
                    .flex_none()
                    .text_color(theme.states.text.warning)
                    .child(as_of)
            }))
            .child(div().flex_1().min_w_0())
            .child(
                div()
                    .flex_none()
                    .text_color(colors.text_faint)
                    .child(report.instance.clone()),
            )
            .into_any_element()
    }

    /// The header's `···`: *edit page* (the dashboard editor on this
    /// page).
    fn options_trigger(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let open = self.menus.is_open(&PageMenu::Options);
        let trigger = GlyphButton::new("health-options", "···")
            .text_size(px(13.))
            .bleed()
            .color(theme.colors.text_muted)
            .selected(open)
            .on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
                this.menus.toggle(PageMenu::Options, down_position(event));
                cx.notify();
            }));
        let menu = Menu::new("health-options-menu")
            .item(
                MenuItem::new("health-edit-page", "edit page")
                    .icon(IconName::Pencil)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.menus.close();
                        cx.emit(HealthPageEvent::Edit);
                        cx.notify();
                    })),
            )
            .on_dismiss(cx.listener(|this, dismissal: &Dismissal, _, cx| {
                this.menus.dismissed(*dismissal);
                cx.notify();
            }));
        div()
            .relative()
            .flex_none()
            .ml(px(10.))
            .child(if open {
                trigger
            } else {
                trigger.tooltip(Tooltip::new("Page options"))
            })
            .when(open, |trigger| {
                trigger.child(
                    Popover::new(menu)
                        .align_right()
                        .outset(GlyphButton::reach(), px(0.)),
                )
            })
            .into_any_element()
    }

    /// The heartbeat row (16a2), pinned under the health line: the beats'
    /// dot, `heartbeats 6 of 6` in fixed slots, what they do (and a late
    /// beat's age), and at the right the trouble policy and *settings*.
    fn render_beat_row(
        &self,
        row: &BeatRow,
        policy: &'static str,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let text = |tone: Tone| match tone {
            Tone::Critical => theme.states.text.critical,
            Tone::Warning => theme.states.text.warning,
            Tone::Normal => colors.text_muted,
        };
        let slot = |chars: f32| (theme.text.small * (ic_ui_kit::CHAR_WIDTH * chars)).ceil();
        let preview = self.preview.is_some();
        div()
            .id("health-beats")
            .flex()
            .flex_none()
            .items_center()
            .gap(px(7.))
            .h(Metrics::with_rule(theme.metrics.summary_bar_height))
            .px(theme.metrics.list_padding)
            .border_b_1()
            .border_color(colors.border_header)
            .text_size(theme.text.small)
            .text_color(colors.text_secondary)
            .whitespace_nowrap()
            .overflow_hidden()
            .child(
                StateDot::with_color(beat_color(row.tone, theme)).size(theme.metrics.summary_dot),
            )
            .child(div().flex_none().w(slot(10.)).child(row.label))
            .child(
                div()
                    .flex_none()
                    .w(slot(12.))
                    .truncate()
                    .child(row.subject.clone()),
            )
            // The status in a slot sized for its longest late wording
            // (`99 intervals late`), so a late beat's age starts at a fixed
            // place whatever the words (16a2); a longer status without an
            // age just runs on.
            .child(
                div()
                    .flex_none()
                    .min_w(slot(STATUS_SLOT_CHARS))
                    .text_color(text(row.tone.text()))
                    .child(row.status.clone()),
            )
            .children(row.age.clone().map(|age| {
                div()
                    .flex_none()
                    .w(slot(AGE_SLOT_CHARS))
                    .flex()
                    .justify_end()
                    .text_color(colors.text_secondary)
                    .child(age)
            }))
            .child(div().flex_1().min_w_0())
            .when(row.watched, |line| {
                line.child(
                    div()
                        .flex_none()
                        .text_color(colors.text_faint)
                        .child(policy),
                )
            })
            .child(
                focus_outline(self.cursor == Some(Stop::BeatSettings), theme).child(
                    Link::new("health-beats-settings", "settings").on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            this.cursor = Some(Stop::BeatSettings);
                            if !preview {
                                cx.emit(HealthPageEvent::Settings);
                            }
                            cx.notify();
                        },
                    )),
                ),
            )
            .into_any_element()
    }

    /// The alert block (16a3), pinned under the heartbeat row: the worst
    /// alert in full (its title, what it means, its link and since when),
    /// the others one line each; the worst one's tone tints it.
    #[expect(
        clippy::too_many_lines,
        reason = "the worst alert in full and the others' lines, in one block"
    )]
    fn render_alerts(&self, alerts: &[AlertLine], cx: &Context<Self>) -> Option<AnyElement> {
        let worst = alerts.first()?;
        let theme = cx.theme();
        let colors = theme.colors;
        let tone = match worst.tone {
            Tone::Critical => BannerTone::Critical,
            Tone::Warning | Tone::Normal => BannerTone::Warning,
        };
        let icon = |line: &AlertLine| {
            let color = match line.tone {
                Tone::Critical => theme.states.fill.critical,
                Tone::Warning | Tone::Normal => theme.states.fill.warning,
            };
            div().flex().flex_none().justify_center().w(px(16.)).child(
                Icon::new(IconName::TriangleAlert)
                    .size(theme.metrics.icon)
                    .color(color),
            )
        };
        let since = |line: &AlertLine| {
            div()
                .flex_none()
                .text_size(theme.text.small)
                .text_color(colors.text_muted)
                .child(line.since.clone())
        };
        let link = worst.link.clone().map(|(words, action)| {
            Link::new(
                "health-alert-link",
                if matches!(action, AlertAction::LateChecks { .. }) && self.late.is_open() {
                    "hide the late checks".to_owned()
                } else {
                    words
                },
            )
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.cursor = Some(Stop::AlertLink);
                this.alert_action(&action, window, cx);
            }))
        });
        let link_cursor = self.cursor == Some(Stop::AlertLink);
        let first = div()
            .id("health-alert-0")
            .flex()
            .items_center()
            .gap(px(12.))
            .min_h(px(44.))
            .py(px(8.))
            .child(icon(worst))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(2.))
                    .child(
                        div()
                            .truncate()
                            .text_size(theme.text.body)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(colors.text_strong)
                            .child(worst.title.clone()),
                    )
                    .when(!worst.detail.is_empty(), |column| {
                        column.child(
                            div()
                                .truncate()
                                .text_size(theme.text.small)
                                .text_color(colors.text_muted)
                                .child(worst.detail.clone()),
                        )
                    }),
            )
            .children(link.map(|link| {
                focus_outline(link_cursor, theme)
                    .text_size(theme.text.small)
                    .child(link)
            }))
            .child(since(worst));
        let others = alerts.iter().enumerate().skip(1).map(|(index, line)| {
            div()
                .id(("health-alert", index))
                .flex()
                .items_center()
                .gap(px(12.))
                .h(px(30.))
                .child(icon(line))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(theme.text.body)
                        .text_color(colors.text_strong)
                        .child(line.title.clone()),
                )
                .child(since(line))
        });
        Some(
            div()
                .id("health-alerts")
                .relative()
                .flex()
                .flex_col()
                .flex_none()
                .pl(theme.metrics.list_padding)
                .pr(theme.metrics.list_padding)
                .pb(px(if alerts.len() > 1 { 6. } else { 0. }))
                .bg(tone.tint(theme))
                .border_b_1()
                .border_color(colors.border_header)
                .child(
                    div()
                        .absolute()
                        .left_0()
                        .top_0()
                        .bottom_0()
                        .w(px(2.))
                        .bg(tone.color(theme)),
                )
                .child(first)
                .children(others)
                .into_any_element(),
        )
    }

    /// What the worst alert's link does.
    fn alert_action(&mut self, action: &AlertAction, window: &mut Window, cx: &mut Context<Self>) {
        if self.preview.is_some() {
            return;
        }
        match action {
            AlertAction::LateChecks { .. } => {
                self.late = if self.late.is_open() {
                    LateFold::Closed
                } else {
                    LateFold::Preview
                };
                cx.notify();
            }
            AlertAction::ShowNode(node) => {
                // The node's row is in the zones view: unfold it, put the
                // cursor on the row (marked, and scrolled to) and give the
                // page the keyboard.
                self.folded
                    .retain(|display| *display != ViewDisplay::ZonesAndEndpoints);
                self.set_cursor(Some(Stop::Endpoint(node.clone())), cx);
                window.focus(&self.focus_handle, cx);
                cx.notify();
            }
            AlertAction::Settings => cx.emit(HealthPageEvent::Settings),
        }
    }

    /// The worst alert's late checks under the block, most overdue first:
    /// seven, then `+ N more` (all of them, `− show fewer`). A click opens
    /// one as a tab. Each a row of the body, with its stop.
    fn render_late(
        &self,
        report: &Report,
        now: Timestamp,
        cx: &Context<Self>,
    ) -> Vec<(Option<Stop>, AnyElement)> {
        if !self.late.is_open() || report.late.is_empty() {
            return Vec::new();
        }
        let theme = cx.theme();
        let colors = theme.colors;
        let state = self.state.read(cx);
        let snapshot = state.snapshot();
        let shown = if self.late == LateFold::All {
            report.late.len()
        } else {
            report.late.len().min(LATE_PREVIEW)
        };
        let mut rows: Vec<(Option<Stop>, AnyElement)> = report
            .late
            .iter()
            .take(shown)
            .enumerate()
            .map(|(index, key)| {
                (
                    Some(Stop::Late(index)),
                    self.late_row(index, key, snapshot, now, cx),
                )
            })
            .collect();
        if report.late.len() > LATE_PREVIEW {
            let text = if self.late == LateFold::All {
                "− show fewer".to_owned()
            } else {
                format!("+ {} more", count_text((report.late.len() - shown) as u64))
            };
            let cursor = self.cursor == Some(Stop::LateMore);
            rows.push((
                Some(Stop::LateMore),
                div()
                    .id("health-late-more")
                    .relative()
                    .flex()
                    .flex_none()
                    .items_center()
                    .h(Metrics::with_rule(theme.metrics.item_row_height))
                    .pl(theme.metrics.list_padding + px(26.))
                    .border_b_1()
                    .border_color(colors.border_header)
                    .when(cursor, |row| row.bg(colors.row_selected))
                    .children(cursor_bar(cursor, theme))
                    .text_size(theme.text.small)
                    .text_color(colors.accent_text)
                    .cursor_pointer()
                    .child(text)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.cursor = Some(Stop::LateMore);
                        this.late = this.late.more_or_fewer();
                        cx.notify();
                    }))
                    .into_any_element(),
            ));
        }
        rows
    }

    /// One late check under the alert block: its mark, `service on host`, how
    /// late it is; a click opens it as a tab.
    fn late_row(
        &self,
        index: usize,
        key: &ObjectKey,
        snapshot: &ic_core::snapshot::Snapshot,
        now: Timestamp,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let (mark, name, host) = match key {
            ObjectKey::Host { name } => (
                snapshot.hosts.get(name).map(|host| ObjectMark::host(host)),
                name.to_string(),
                None,
            ),
            ObjectKey::Service { key: service } => (
                snapshot.services.get(service).map(|found| {
                    ObjectMark::service(found, snapshot.hosts.get(&service.host).map(AsRef::as_ref))
                }),
                service.name.to_string(),
                Some(service.host.to_string()),
            ),
        };
        let late = snapshot.late.get(key).map_or_else(
            || "late".to_owned(),
            |due| format!("late {}", format_compact(due.elapsed_until(now))),
        );
        let cursor = self.cursor == Some(Stop::Late(index));
        let key = key.clone();
        div()
            .id(("health-late-row", index))
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(12.))
            .h(Metrics::with_rule(px(34.)))
            .px(theme.metrics.list_padding)
            .border_b_1()
            .border_color(colors.border_row)
            .cursor_pointer()
            .when(cursor, |row| row.bg(colors.row_selected))
            .when(!cursor, |row| row.hover(|row| row.bg(colors.row_hover)))
            .children(cursor_bar(cursor, theme))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .justify_center()
                    .w(px(14.))
                    .children(mark.map(|mark| StateDot::mark(mark).size(px(7.)))),
            )
            .child(crate::lists::draw::object_label(
                &name,
                host.as_deref(),
                theme.text.body,
                theme,
            ))
            .child(div().flex_1().min_w_0())
            .child(
                div()
                    .flex_none()
                    .text_size(theme.text.small)
                    .text_color(theme.states.text.warning)
                    .child(late),
            )
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.cursor = Some(Stop::Late(index));
                let key = key.clone();
                this.state.update(cx, |state, cx| {
                    if state.open_tab(key) {
                        cx.notify();
                    }
                });
            }))
            .into_any_element()
    }

    /// A view's 36px header (as a stacked view's): the fold chevron, the
    /// icon, the name, what it shows (faint), and anything at the right.
    /// In the editor's preview the selected view's header is marked, and a
    /// click selects a view.
    fn render_section_header(
        &self,
        view: &View,
        first: bool,
        about: String,
        right: Option<AnyElement>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let display = view.display;
        let folded = self.is_folded(display);
        let picked = self
            .preview
            .as_ref()
            .and_then(|preview| preview.picked.as_deref())
            == Some(view.id.as_str());
        let name = if view.name.trim().is_empty() {
            section_name(display).to_owned()
        } else {
            view.name.trim().to_owned()
        };
        let id = view.id.clone();
        let in_preview = self.preview.is_some();
        // The keyboard cursor marks it like the editor's pick: the accent
        // bar and the selected tint.
        let picked = picked || (!in_preview && self.cursor == Some(Stop::Header(display)));
        div()
            .id(section_id(display))
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(10.))
            .h(Metrics::with_rule(theme.metrics.summary_bar_height))
            .pl(px(14.))
            .pr(theme.metrics.list_padding)
            .bg(if picked {
                colors.row_selected
            } else {
                colors.pane_background
            })
            .border_b_1()
            .border_color(colors.border_header)
            .when(!first, gpui::Styled::border_t_1)
            .whitespace_nowrap()
            .text_size(theme.text.small)
            .text_color(colors.text_muted)
            .cursor_pointer()
            .when(picked, |header| {
                header.child(
                    div()
                        .absolute()
                        .left_0()
                        .top_0()
                        .bottom_0()
                        .w(px(2.))
                        .bg(colors.accent),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .w(px(12.))
                    .child(
                        Icon::new(if folded {
                            IconName::ChevronRight
                        } else {
                            IconName::ChevronDown
                        })
                        .size(px(12.))
                        .color(colors.text_faint),
                    ),
            )
            .child(
                Icon::new(section_icon(display))
                    .size(px(13.))
                    .color(colors.text_muted),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(theme.text.row)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(colors.text_secondary)
                    .child(name),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(colors.text_faint)
                    .child(about),
            )
            .children(right)
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                if in_preview {
                    cx.emit(HealthPageEvent::Pick(id.clone()));
                } else {
                    this.cursor = Some(Stop::Header(display));
                    this.toggle(display, cx);
                }
            }))
            .into_any_element()
    }

    /// The page's views, each a header and (unless folded) its body: the
    /// body's rows, each with the stop it is (if any).
    fn render_sections(
        &self,
        report: &Report,
        views: &[View],
        cx: &Context<Self>,
    ) -> Vec<(Option<Stop>, AnyElement)> {
        let theme = cx.theme();
        // The editor's preview is narrower: three tiles a row (16k).
        let columns = if self.preview.is_some() { 3 } else { 6 };
        let live = report.live;
        let mut body: Vec<(Option<Stop>, AnyElement)> = Vec::new();
        for (index, view) in views.iter().enumerate() {
            let first = index == 0;
            let folded = self.is_folded(view.display);
            let header = Some(Stop::Header(view.display));
            match view.display {
                ViewDisplay::ZonesAndEndpoints => {
                    let about = format!(
                        "{} · {} · {}",
                        plural(report.zones.len(), "zone", "zones"),
                        plural(
                            report.zones.iter().map(|zone| zone.endpoints.len()).sum(),
                            "endpoint",
                            "endpoints"
                        ),
                        plural(report.global_zones.len(), "global zone", "global zones")
                    );
                    body.push((
                        header,
                        self.render_section_header(
                            view,
                            first,
                            about,
                            Some(zone_counts(report, theme)),
                            cx,
                        ),
                    ));
                    if !folded {
                        body.extend(self.render_zones(report, theme));
                    }
                }
                ViewDisplay::Checks => {
                    body.push((
                        header,
                        self.render_section_header(
                            view,
                            first,
                            "last minute, from /v1/status".to_owned(),
                            None,
                            cx,
                        ),
                    ));
                    if !folded {
                        body.push((
                            None,
                            Self::render_tiles(
                                "health-check-tiles",
                                &report.checks,
                                view,
                                columns,
                                live,
                                theme,
                            ),
                        ));
                    }
                }
                ViewDisplay::QueuesAndConnections => {
                    body.push((
                        header,
                        self.render_section_header(
                            view,
                            first,
                            "ApiListener, JsonRpc".to_owned(),
                            None,
                            cx,
                        ),
                    ));
                    if !folded {
                        body.push((
                            None,
                            Self::render_tiles(
                                "health-queue-tiles",
                                &report.queues,
                                view,
                                columns,
                                live,
                                theme,
                            ),
                        ));
                    }
                }
                ViewDisplay::GlobalSwitches => {
                    body.push((
                        header,
                        self.render_section_header(view, first, "read-only".to_owned(), None, cx),
                    ));
                    if !folded {
                        body.push((None, Self::render_switches(report, cx.theme())));
                    }
                }
                _ => {}
            }
        }
        body
    }

    /// The zones and their endpoints, as a table: its rows, each with its
    /// stop.
    fn render_zones(&self, report: &Report, theme: &Theme) -> Vec<(Option<Stop>, AnyElement)> {
        let colors = theme.colors;
        let live = report.live.is_live();
        let mut rows: Vec<(Option<Stop>, AnyElement)> = vec![(
            None,
            table_row(
                [
                    "endpoint",
                    "zone",
                    "version",
                    "last message",
                    "messages in / out",
                    "status",
                ]
                .map(|text| cell(text, colors.text_faint)),
                None,
                Some(
                    div()
                        .text_color(colors.text_faint)
                        .child("heartbeat")
                        .into_any_element(),
                ),
                RowLook::default(),
                px(28.),
                theme.text.label,
                theme,
            ),
        )];
        for zone in &report.zones {
            let stop = Stop::Zone(zone.name.clone());
            let cursor = self.cursor.as_ref() == Some(&stop);
            rows.push((Some(stop), zone_row(zone, cursor, theme)));
            for endpoint in &zone.endpoints {
                let stop = Stop::Endpoint(endpoint.name.clone());
                let cursor = self.cursor.as_ref() == Some(&stop);
                rows.push((Some(stop), endpoint_row(endpoint, cursor, live, theme)));
            }
        }
        if !report.global_zones.is_empty() {
            rows.push((
                None,
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(10.))
                    .h(px(32.))
                    .px(theme.metrics.list_padding)
                    .text_size(theme.text.small)
                    .text_color(colors.text_muted)
                    .whitespace_nowrap()
                    .child(
                        Icon::new(IconName::Layers)
                            .size(px(12.))
                            .color(colors.text_faint),
                    )
                    .child(div().min_w_0().truncate().child(format!(
                        "global zones: {} (config only, no endpoints)",
                        report.global_zones.join(", ")
                    )))
                    .into_any_element(),
            ));
        }
        rows
    }

    /// A line of stat tiles: six columns, a wide tile spanning two; only
    /// the tiles `view` shows, with or without their trend lines.
    fn render_tiles(
        id: &'static str,
        tiles: &[Tile],
        view: &View,
        columns: u16,
        live: Liveness,
        theme: &Theme,
    ) -> AnyElement {
        let sparklines = view.health.sparklines;
        div()
            .id(id)
            .grid()
            .grid_cols(columns)
            .gap(px(12.))
            .flex_none()
            .pt(px(14.))
            .pb(px(16.))
            .px(theme.metrics.list_padding)
            .children(
                tiles
                    .iter()
                    .filter(|tile| tile.kind.is_none_or(|kind| view.health.shows(kind)))
                    .map(|tile| render_tile(tile, sparklines, live, theme)),
            )
            .into_any_element()
    }

    /// Icinga's global switches, then the node's features.
    fn render_switches(report: &Report, theme: &Theme) -> AnyElement {
        let colors = theme.colors;
        // Without live data nothing is green here either.
        let ok = if report.live.is_live() {
            theme.states.fill.ok
        } else {
            theme.states.fill.pending
        };
        let item = |dot: Hsla, label: &'static str, word: &'static str, word_color: Hsla| {
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(7.))
                .child(StateDot::with_color(dot).size(px(6.)))
                .child(label)
                .child(div().text_color(word_color).child(word))
        };
        let switches = report.switches.iter().map(|switch| {
            if switch.on {
                item(ok, switch.label, "on", colors.text_faint)
            } else {
                item(
                    theme.states.fill.warning,
                    switch.label,
                    "off",
                    theme.states.text.warning,
                )
            }
        });
        let features = report.features.iter().map(|feature| {
            let dot = match feature.state {
                FeatureState::Running => ok,
                FeatureState::Paused | FeatureState::Off => theme.states.fill.pending,
            };
            item(dot, feature.label, feature.word(), colors.text_faint)
        });
        let node = report.seen_from.clone().unwrap_or_default();
        div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(10.))
            .pt(px(12.))
            .pb(px(16.))
            .px(theme.metrics.list_padding)
            .text_size(theme.text.body)
            .text_color(colors.text_secondary)
            .whitespace_nowrap()
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_x(px(18.))
                    .gap_y(px(6.))
                    .children(switches),
            )
            .when(!report.features.is_empty(), |column| {
                column.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_x(px(18.))
                        .gap_y(px(6.))
                        .child(
                            div()
                                .text_color(colors.text_faint)
                                .child(format!("features of {node}")),
                        )
                        .children(features),
                )
            })
            .into_any_element()
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
        reason = "the page's parts in order, with what the keyboard moves over"
    )]
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let controls = Controls::of(window, cx);
        let now = Timestamp::now();
        let state = self.state.read(cx);
        let snapshot = state.snapshot().clone();
        let live = super::liveness(&snapshot, state.connection(), now);
        let report = report(&snapshot, live, now);
        let policy = state
            .environment()
            .map_or("notify", |environment| environment.trouble.policy.label());
        if report.late.is_empty() {
            self.late = LateFold::Closed;
        }
        let preview = self.preview.is_some();
        // The editor's preview has the editor's header; the page shows the
        // connection's banners as every page does (16d).
        let header = (!preview).then(|| self.render_header(&report, controls, now, cx));
        let banners = if preview {
            Vec::new()
        } else {
            crate::banner::banners(&self.state, now, true, cx)
        };
        if report.zones.is_empty() {
            let colors = cx.theme().colors;
            return div()
                .id("health-page")
                .track_focus(&self.focus_handle)
                .key_context(HEALTH_CONTEXT)
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .h_full()
                .children(header)
                .children(banners)
                .child(
                    EmptyState::new("No cluster nodes yet")
                        .leading(
                            Icon::new(IconName::HeartPulse)
                                .size(px(20.))
                                .color(colors.text_muted),
                        )
                        .detail(
                            "They show once icygui is connected and knows the cluster's endpoints.",
                        )
                        .max_width(px(560.)),
                );
        }
        let views = self.views(cx);
        // What the keyboard moves over, as drawn now.
        self.stops = self.layout(&report, &views);
        self.worst_action = report
            .alerts
            .first()
            .and_then(|alert| alert.link.as_ref())
            .map(|(_, action)| action.clone());
        self.late_keys.clone_from(&report.late);
        if self
            .cursor
            .as_ref()
            .is_some_and(|cursor| !self.stops.contains(cursor))
        {
            self.cursor = None;
        }
        let theme = cx.theme();
        let health_line = Self::render_health_line(&report, now, theme);
        let beats = self.render_beat_row(&report.beats, policy, cx);
        let alerts = self.render_alerts(&report.alerts, cx);
        let mut rows = self.render_late(&report, now, cx);
        rows.extend(self.render_sections(&report, &views, cx));
        self.rows = rows
            .iter()
            .enumerate()
            .filter_map(|(index, (stop, _))| stop.clone().map(|stop| (stop, index)))
            .collect();
        if std::mem::take(&mut self.reveal)
            && let Some(cursor) = self.cursor.as_ref().filter(|cursor| cursor.in_body())
            && let Some((_, index)) = self.rows.iter().find(|(stop, _)| stop == cursor)
        {
            self.scroll.scroll_to_item(*index);
        }
        div()
            .id("health-page")
            .track_focus(&self.focus_handle)
            .key_context(HEALTH_CONTEXT)
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::select_first))
            .on_action(cx.listener(Self::select_last))
            .on_action(cx.listener(Self::select_page_down))
            .on_action(cx.listener(Self::select_page_up))
            .on_action(cx.listener(Self::next_view))
            .on_action(cx.listener(Self::previous_view))
            .on_action(cx.listener(Self::unfold))
            .on_action(cx.listener(Self::fold))
            .on_action(cx.listener(Self::open_selected))
            .on_action(cx.listener(Self::dismiss))
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .children(header)
            .children(banners)
            .child(health_line)
            .child(beats)
            .children(alerts)
            .child(
                div()
                    .id("health-body")
                    .track_scroll(&self.scroll)
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(rows.into_iter().map(|(_, row)| row)),
            )
    }
}

/// `updated 12s ago · every 30s` (`quiet: every 5 min` while quiet), or
/// what the page waits for.
fn updated_text(report: &Report, now: Timestamp) -> String {
    let every = if report.interval.is_zero() {
        String::new()
    } else if report.quiet {
        format!(" · quiet: every {}", interval_words(report.interval))
    } else {
        format!(" · every {}", interval_words(report.interval))
    };
    match report.updated {
        Some(at) => format!(
            "updated {} ago{every}",
            format_compact(at.elapsed_until(now))
        ),
        None => "waiting for the status poll".to_owned(),
    }
}

/// `30s`, `5 min`.
fn interval_words(interval: Duration) -> String {
    let seconds = interval.as_secs();
    if seconds < 120 {
        format!("{seconds}s")
    } else {
        format!("{} min", seconds / 60)
    }
}

/// A status slot of the health line: a dot and `4 connected`, as wide as
/// `longest`, so the next slot starts at a fixed place.
fn status_slot(dot: Hsla, text: String, longest: &str, theme: &Theme) -> AnyElement {
    let dot_size = theme.metrics.summary_dot;
    #[expect(clippy::cast_precision_loss, reason = "a short label")]
    let width = dot_size
        + px(7.)
        + (theme.text.small * (ic_ui_kit::CHAR_WIDTH * longest.chars().count() as f32)).ceil();
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(7.))
        .w(width)
        .child(StateDot::with_color(dot).size(dot_size))
        .child(text)
        .into_any_element()
}

/// The zones header's counts: connected endpoints, then those down, each a
/// dot and a three-digit slot (the down slot empty while none is).
fn zone_counts(report: &Report, theme: &Theme) -> AnyElement {
    let slot = |color: Hsla, count: usize| {
        let width = (theme.text.small * (ic_ui_kit::CHAR_WIDTH * 3.)).ceil();
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(6.))
            .w(px(6.) + px(6.) + width)
            .when(count > 0, |slot| {
                slot.child(StateDot::with_color(color).size(px(6.)))
                    .child(count.to_string())
            })
    };
    let connected = if report.live.is_live() {
        theme.states.fill.ok
    } else {
        theme.states.fill.pending
    };
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(10.))
        .child(slot(connected, report.connected))
        .child(slot(theme.states.fill.critical, report.not_connected))
        .into_any_element()
}

/// A table cell's text and colour.
struct Cell {
    text: SharedString,
    color: Hsla,
}

fn cell(text: impl Into<SharedString>, color: Hsla) -> Cell {
    Cell {
        text: text.into(),
        color,
    }
}

/// The table's column widths, as shares of the row (the mock-up's
/// `1.2fr .8fr .8fr .9fr 1.1fr 1.6fr`).
const COLUMNS: [f32; 6] = [1.2, 0.8, 0.8, 0.9, 1.1, 1.6];

/// How a table line is marked.
#[derive(Clone, Copy, Debug, Default)]
struct RowLook {
    /// The node icygui talks to (the selected-row background).
    this_node: bool,
    /// The keyboard cursor (the selected background and the accent bar).
    cursor: bool,
}

/// One line of the endpoints table: the dot column, then the six columns.
fn table_row(
    cells: [Cell; 6],
    dot: Option<Hsla>,
    beat: Option<AnyElement>,
    look: RowLook,
    height: gpui::Pixels,
    size: gpui::Pixels,
    theme: &Theme,
) -> AnyElement {
    let colors = theme.colors;
    div()
        .relative()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(12.))
        .h(Metrics::with_rule(height))
        .px(theme.metrics.list_padding)
        .border_b_1()
        .border_color(colors.border_row)
        .when(look.this_node || look.cursor, |row| {
            row.bg(colors.row_selected)
        })
        .children(cursor_bar(look.cursor, theme))
        .text_size(size)
        .whitespace_nowrap()
        .child(
            div()
                .flex()
                .flex_none()
                .justify_center()
                .w(px(14.))
                .children(dot.map(|dot| StateDot::with_color(dot).size(px(7.)))),
        )
        .children(cells.into_iter().zip(COLUMNS).map(|(cell, share)| {
            // `fr` columns: shares of what the gaps leave.
            div()
                .flex_basis(px(0.))
                .flex_grow(share)
                .flex_shrink(share)
                .min_w_0()
                .truncate()
                .text_color(cell.color)
                .child(cell.text)
        }))
        .child(beat_slot(beat))
        .into_any_element()
}

/// The table's *heartbeat* column: a fixed slot at the right of every
/// line (a zone's band too), empty without a beat.
fn beat_slot(beat: Option<AnyElement>) -> AnyElement {
    div()
        .flex()
        .flex_none()
        .items_center()
        .w(px(BEAT_COLUMN))
        .children(beat)
        .into_any_element()
}

/// The *heartbeat* column's width.
const BEAT_COLUMN: f32 = 104.;

/// The heartbeat row's status slot, in characters: its longest late
/// wording (`99 intervals late`, 16a2's 18ch).
const STATUS_SLOT_CHARS: f32 = 18.;

/// The heartbeat row's age slot, in characters (`59m 59s`, 16a2's 8ch).
const AGE_SLOT_CHARS: f32 = 8.;

/// A beat's dot and age (`● 8s`, `● disappeared`).
fn beat_element(beat: &BeatCell, theme: &Theme) -> AnyElement {
    let text = match beat.tone {
        BeatTone::Critical => theme.states.text.critical,
        BeatTone::Warning => theme.states.text.warning,
        BeatTone::Ok | BeatTone::Off => theme.colors.text_secondary,
    };
    div()
        .flex()
        .items_center()
        .gap(px(6.))
        .min_w_0()
        .child(StateDot::with_color(beat_color(beat.tone, theme)).size(px(6.)))
        .child(div().truncate().text_color(text).child(beat.text.clone()))
        .into_any_element()
}

/// A beat's dot colour.
fn beat_color(tone: BeatTone, theme: &Theme) -> Hsla {
    tone.fill(theme)
}

/// A view's title on the page.
fn section_name(display: ViewDisplay) -> &'static str {
    match display {
        ViewDisplay::ZonesAndEndpoints => "zones and endpoints",
        ViewDisplay::Checks => "checks",
        ViewDisplay::QueuesAndConnections => "queues and connections",
        ViewDisplay::GlobalSwitches => "Icinga’s global switches",
        _ => "",
    }
}

/// A view's icon on the page.
fn section_icon(display: ViewDisplay) -> IconName {
    match display {
        ViewDisplay::ZonesAndEndpoints | ViewDisplay::GlobalSwitches => IconName::List,
        _ => IconName::ChartBar,
    }
}

/// A zone's band: its dot (warning while an HA zone lost an endpoint,
/// critical once it has none or its beat is dead), the name, its line. The
/// dot sits at the band's edge and the name 10px after it (16a: a band
/// reads as the header of the endpoint rows, whose names sit further in).
fn zone_row(zone: &ZoneGroup, cursor: bool, theme: &Theme) -> AnyElement {
    let colors = theme.colors;
    div()
        .relative()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(10.))
        .h(Metrics::with_rule(px(30.)))
        .px(theme.metrics.list_padding)
        .bg(if cursor {
            colors.row_selected
        } else {
            colors.row_header
        })
        .children(cursor_bar(cursor, theme))
        .border_b_1()
        .border_color(colors.border_header)
        .text_size(theme.text.small)
        .text_color(colors.text_muted)
        .whitespace_nowrap()
        .child(
            div()
                .flex()
                .flex_none()
                .w(px(7.))
                .child(StateDot::with_color(zone.dot.fill(theme)).size(px(7.))),
        )
        .child(
            div()
                .flex_none()
                .text_size(theme.text.body)
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(colors.text_strong)
                .child(zone.name.clone()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .child(zone.detail.clone()),
        )
        .child(beat_slot(
            zone.beat.as_ref().map(|beat| beat_element(beat, theme)),
        ))
        .into_any_element()
}

/// An endpoint's line; the node icygui talks to has the selected-row
/// background. Without live data its status is faint (the last known
/// one) and its dot grey where it was green.
fn endpoint_row(row: &EndpointRow, cursor: bool, live: bool, theme: &Theme) -> AnyElement {
    let colors = theme.colors;
    let status = match row.tone {
        Tone::Critical => theme.states.text.critical,
        Tone::Warning => theme.states.text.warning,
        Tone::Normal if !live => colors.text_faint,
        Tone::Normal => colors.text_muted,
    };
    table_row(
        [
            cell(row.name.clone(), colors.text_strong),
            cell(row.zone.clone(), colors.text_muted),
            cell(row.version.clone(), colors.text_secondary),
            cell(row.last_message.clone(), colors.text_secondary),
            cell(row.traffic.clone(), colors.text_muted),
            cell(row.status.clone(), status),
        ],
        Some(row.dot.fill(theme)),
        row.beat.as_ref().map(|beat| beat_element(beat, theme)),
        RowLook {
            this_node: row.this_node,
            cursor,
        },
        px(34.),
        theme.text.body,
        theme,
    )
}

/// The keyboard cursor's accent bar at a row's left edge (the row is
/// `relative`).
fn cursor_bar(on: bool, theme: &Theme) -> Option<AnyElement> {
    on.then(|| {
        div()
            .absolute()
            .left_0()
            .top_0()
            .bottom_0()
            .w(px(2.))
            .bg(theme.colors.accent)
            .into_any_element()
    })
}

/// A link's frame for the keyboard cursor: an accent outline while the
/// cursor is on it, a transparent one otherwise (so nothing moves).
fn focus_outline(on: bool, theme: &Theme) -> gpui::Div {
    div()
        .flex_none()
        .px(px(3.))
        .mx(px(-4.))
        .rounded(px(3.))
        .border_1()
        .border_color(if on {
            theme.colors.accent
        } else {
            gpui::transparent_black()
        })
}

/// One stat tile: label, value (in the warning or critical text colour
/// when wrong; dimmed while not current), what it counts, and its trend (a
/// blank of the same height without one, so every tile of a line is as
/// tall).
fn render_tile(tile: &Tile, sparklines: bool, live: Liveness, theme: &Theme) -> AnyElement {
    let colors = theme.colors;
    let (value, last) = match tile.tone {
        Tone::Critical => (theme.states.text.critical, theme.states.fill.critical),
        Tone::Warning => (theme.states.text.warning, theme.states.fill.warning),
        Tone::Normal if !live.is_live() => (colors.text_muted, colors.text_faint),
        Tone::Normal => (colors.text_strong, colors.accent),
    };
    let trend: Option<AnyElement> = if !sparklines {
        None
    } else if tile.trend.len() >= 2 {
        Some(spark::sparkline(&tile.trend, colors.text_faint, last).into_any_element())
    } else {
        Some(div().h(px(spark::HEIGHT)).into_any_element())
    };
    div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .min_w_0()
        .pt(px(11.))
        .px(px(14.))
        .pb(px(12.))
        .border_1()
        .border_color(colors.border_header)
        .rounded(px(6.))
        .when(tile.wide, |tile| tile.col_span(2))
        .child(
            div()
                .truncate()
                .text_size(theme.text.label)
                .text_color(colors.text_faint)
                .child(tile.label),
        )
        .child(
            div()
                .truncate()
                .text_size(theme.text.title)
                .font_weight(FontWeight::MEDIUM)
                .text_color(value)
                .child(tile.value.clone()),
        )
        .child(
            div()
                .truncate()
                .text_size(theme.text.label)
                .text_color(colors.text_muted)
                .child(tile.detail.clone()),
        )
        .children(trend)
        .into_any_element()
}

/// `1 zone`, `3 zones`.
fn plural(count: usize, one: &str, many: &str) -> String {
    if count == 1 {
        format!("1 {one}")
    } else {
        format!("{count} {many}")
    }
}
