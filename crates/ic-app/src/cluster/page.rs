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

use std::time::Duration;

use gpui::{
    AnyElement, ClickEvent, Context, Entity, EventEmitter, FontWeight, Hsla,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div,
    prelude::FluentBuilder as _,
};
use ic_config::{HealthPage as HealthLayout, View, ViewDisplay};
use ic_core::NodeState;
use ic_core::trouble::AlertAction;
use ic_model::{FeatureState, ObjectKey, Timestamp, format_compact};
use ic_ui_kit::{
    ActiveTheme as _, BannerTone, Dismissal, EmptyState, GlyphButton, Icon, IconName, Link, Menu,
    MenuItem, Metrics, ObjectMark, PaneHeader, Popover, StateDot, Theme, Tooltip, px,
};

use super::beats::{AlertLine, BeatCell, BeatRow, BeatTone};
use super::health::{EndpointRow, Report, Tile, Tone, ZoneGroup, count_text, report};
use super::spark;
use crate::app_state::AppState;
use crate::chrome::{Controls, WindowDrag};
use crate::menu_state::{OpenMenu, down_position};
use crate::workspace::sidebar_reopen;

/// The late checks the banner's fold shows before `+ N more` (the hosts'
/// paging rule).
const LATE_PREVIEW: usize = 7;

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
    late_open: bool,
    late_all: bool,
    menus: OpenMenu<PageMenu>,
    /// The editor's preview, when the page is one.
    preview: Option<Preview>,
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
            late_open: false,
            late_all: false,
            menus: OpenMenu::default(),
            preview: None,
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
            None => self
                .state
                .read(cx)
                .environment()
                .map(|environment| environment.health_page.views.clone())
                .unwrap_or_else(|| HealthLayout::default().views),
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
        report(
            state.snapshot(),
            state.connection().is_connected(),
            Timestamp::now(),
        )
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
            .child(
                div()
                    .flex_none()
                    .text_size(theme.text.small)
                    .text_color(colors.text_faint)
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
    /// version and uptime at the right.
    fn render_health_line(report: &Report, theme: &Theme) -> AnyElement {
        let colors = theme.colors;
        let not_connected = if report.not_connected > 0 {
            theme.states.fill.critical
        } else {
            theme.states.fill.pending
        };
        let slots = [
            status_slot(
                theme.states.fill.ok,
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
    fn render_beat_row(&self, row: &BeatRow, policy: &'static str, cx: &Context<Self>) -> AnyElement {
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
            .child(StateDot::with_color(beat_color(row.tone, theme)).size(theme.metrics.summary_dot))
            .child(div().flex_none().w(slot(10.)).child(row.label))
            .child(div().flex_none().w(slot(12.)).truncate().child(row.subject.clone()))
            .child(
                div()
                    .flex_none()
                    .text_color(text(row.tone.text()))
                    .child(row.status.clone()),
            )
            .children(row.age.clone().map(|age| {
                div()
                    .flex_none()
                    .ml(px(24.))
                    .text_color(colors.text_secondary)
                    .child(age)
            }))
            .child(div().flex_1().min_w_0())
            .when(row.watched, |line| {
                line.child(div().flex_none().text_color(colors.text_faint).child(policy))
            })
            .child(
                Link::new("health-beats-settings", "settings").on_click(cx.listener(
                    move |_, _: &ClickEvent, _, cx| {
                        if !preview {
                            cx.emit(HealthPageEvent::Settings);
                        }
                    },
                )),
            )
            .into_any_element()
    }

    /// The alert block (16a3), pinned under the heartbeat row: the worst
    /// alert in full (its title, what it means, its link and since when),
    /// the others one line each; the worst one's tone tints it.
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
            div()
                .flex()
                .flex_none()
                .justify_center()
                .w(px(16.))
                .child(Icon::new(IconName::TriangleAlert).size(theme.metrics.icon).color(color))
        };
        let since = |line: &AlertLine| {
            div()
                .flex_none()
                .text_size(theme.text.small)
                .text_color(colors.text_muted)
                .child(line.since.clone())
        };
        let link = worst.link.clone().map(|(words, action)| {
            Link::new("health-alert-link", if matches!(action, AlertAction::LateChecks { .. }) && self.late_open {
                "hide the late checks".to_owned()
            } else {
                words
            })
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.alert_action(&action, cx);
            }))
        });
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
            .children(link.map(|link| div().flex_none().text_size(theme.text.small).child(link)))
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
    fn alert_action(&mut self, action: &AlertAction, cx: &mut Context<Self>) {
        if self.preview.is_some() {
            return;
        }
        match action {
            AlertAction::LateChecks { .. } => {
                self.late_open = !self.late_open;
                cx.notify();
            }
            AlertAction::ShowNode(_) => {
                // The node's row is in the zones view: unfold it.
                self.folded.retain(|display| *display != ViewDisplay::ZonesAndEndpoints);
                cx.notify();
            }
            AlertAction::Settings => cx.emit(HealthPageEvent::Settings),
        }
    }

    /// The worst alert's late checks under the block, most overdue first:
    /// seven, then `+ N more` (all of them, `− show fewer`). A click opens
    /// one as a tab.
    fn render_late(
        &self,
        report: &Report,
        now: Timestamp,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        if !self.late_open || report.late.is_empty() {
            return None;
        }
        let theme = cx.theme();
        let colors = theme.colors;
        let state = self.state.read(cx);
        let snapshot = state.snapshot();
        let shown = if self.late_all {
            report.late.len()
        } else {
            report.late.len().min(LATE_PREVIEW)
        };
        let rows = report
            .late
            .iter()
            .take(shown)
            .enumerate()
            .map(|(index, key)| Self::late_row(index, key, snapshot, now, cx));
        let more = (report.late.len() > LATE_PREVIEW).then(|| {
            let text = if self.late_all {
                "− show fewer".to_owned()
            } else {
                format!("+ {} more", count_text((report.late.len() - shown) as u64))
            };
            div()
                .id("health-late-more")
                .flex()
                .flex_none()
                .items_center()
                .h(Metrics::with_rule(theme.metrics.item_row_height))
                .pl(theme.metrics.list_padding + px(26.))
                .border_b_1()
                .border_color(colors.border_row)
                .text_size(theme.text.small)
                .text_color(colors.accent_text)
                .cursor_pointer()
                .child(text)
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.late_all = !this.late_all;
                    cx.notify();
                }))
                .into_any_element()
        });
        Some(
            div()
                .id("health-late-list")
                .flex()
                .flex_col()
                .flex_none()
                .border_b_1()
                .border_color(colors.border_header)
                .children(rows)
                .children(more)
                .into_any_element(),
        )
    }

    /// One late check under the alert block: its mark, `service on host`, how
    /// late it is; a click opens it as a tab.
    fn late_row(
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
        let key = key.clone();
        div()
            .id(("health-late-row", index))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(12.))
            .h(Metrics::with_rule(px(34.)))
            .px(theme.metrics.list_padding)
            .border_b_1()
            .border_color(colors.border_row)
            .cursor_pointer()
            .hover(|row| row.bg(colors.row_hover))
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
            .child(Icon::new(section_icon(display)).size(px(13.)).color(colors.text_muted))
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
                    this.toggle(display, cx);
                }
            }))
            .into_any_element()
    }

    /// The page's views, each a header and (unless folded) its body.
    fn render_sections(&self, report: &Report, views: &[View], cx: &Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme();
        // The editor's preview is narrower: three tiles a row (16k).
        let columns = if self.preview.is_some() { 3 } else { 6 };
        let mut body: Vec<AnyElement> = Vec::new();
        for (index, view) in views.iter().enumerate() {
            let first = index == 0;
            let folded = self.is_folded(view.display);
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
                    body.push(self.render_section_header(
                        view,
                        first,
                        about,
                        Some(zone_counts(report, theme)),
                        cx,
                    ));
                    if !folded {
                        body.push(Self::render_zones(report, theme));
                    }
                }
                ViewDisplay::Checks => {
                    body.push(self.render_section_header(
                        view,
                        first,
                        "last minute, from /v1/status".to_owned(),
                        None,
                        cx,
                    ));
                    if !folded {
                        body.push(Self::render_tiles(
                            "health-check-tiles",
                            &report.checks,
                            view,
                            columns,
                            theme,
                        ));
                    }
                }
                ViewDisplay::QueuesAndConnections => {
                    body.push(self.render_section_header(
                        view,
                        first,
                        "ApiListener, JsonRpc".to_owned(),
                        None,
                        cx,
                    ));
                    if !folded {
                        body.push(Self::render_tiles(
                            "health-queue-tiles",
                            &report.queues,
                            view,
                            columns,
                            theme,
                        ));
                    }
                }
                ViewDisplay::GlobalSwitches => {
                    body.push(self.render_section_header(
                        view,
                        first,
                        "read-only".to_owned(),
                        None,
                        cx,
                    ));
                    if !folded {
                        body.push(Self::render_switches(report, cx.theme()));
                    }
                }
                _ => {}
            }
        }
        body
    }

    /// The zones and their endpoints, as a table.
    fn render_zones(report: &Report, theme: &Theme) -> AnyElement {
        let colors = theme.colors;
        let mut rows: Vec<AnyElement> = vec![table_row(
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
            false,
            px(28.),
            theme.text.label,
            theme,
        )];
        for zone in &report.zones {
            rows.push(zone_row(zone, theme));
            for endpoint in &zone.endpoints {
                rows.push(endpoint_row(endpoint, theme));
            }
        }
        if !report.global_zones.is_empty() {
            rows.push(
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
            );
        }
        div()
            .flex()
            .flex_col()
            .flex_none()
            .children(rows)
            .into_any_element()
    }

    /// A line of stat tiles: six columns, a wide tile spanning two; only
    /// the tiles `view` shows, with or without their trend lines.
    fn render_tiles(
        id: &'static str,
        tiles: &[Tile],
        view: &View,
        columns: u16,
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
                    .map(|tile| render_tile(tile, sparklines, theme)),
            )
            .into_any_element()
    }

    /// Icinga's global switches, then the node's features.
    fn render_switches(report: &Report, theme: &Theme) -> AnyElement {
        let colors = theme.colors;
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
                item(theme.states.fill.ok, switch.label, "on", colors.text_faint)
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
                FeatureState::Running => theme.states.fill.ok,
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let controls = Controls::of(window, cx);
        let now = Timestamp::now();
        let state = self.state.read(cx);
        let snapshot = state.snapshot().clone();
        let report = report(&snapshot, state.connection().is_connected(), now);
        let policy = state
            .environment()
            .map_or("notify", |environment| environment.trouble.policy.label());
        if report.late.is_empty() {
            self.late_open = false;
            self.late_all = false;
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
        let theme = cx.theme();
        let colors = theme.colors;
        if report.zones.is_empty() {
            return div()
                .id("health-page")
                .track_focus(&self.focus_handle)
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
        let health_line = Self::render_health_line(&report, theme);
        let beats = self.render_beat_row(&report.beats, policy, cx);
        let alerts = self.render_alerts(&report.alerts, cx);
        let late = self.render_late(&report, now, cx);
        let views = self.views(cx);
        let body = self.render_sections(&report, &views, cx);
        div()
            .id("health-page")
            .track_focus(&self.focus_handle)
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
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(late)
                    .children(body),
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
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(10.))
        .child(slot(theme.states.fill.ok, report.connected))
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

/// One line of the endpoints table: the dot column, then the six columns.
fn table_row(
    cells: [Cell; 6],
    dot: Option<Hsla>,
    beat: Option<AnyElement>,
    selected: bool,
    height: gpui::Pixels,
    size: gpui::Pixels,
    theme: &Theme,
) -> AnyElement {
    let colors = theme.colors;
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(12.))
        .h(Metrics::with_rule(height))
        .px(theme.metrics.list_padding)
        .border_b_1()
        .border_color(colors.border_row)
        .when(selected, |row| row.bg(colors.row_selected))
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

/// A zone's band: its worst endpoint's dot, the name, its line.
fn zone_row(zone: &ZoneGroup, theme: &Theme) -> AnyElement {
    let colors = theme.colors;
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(10.))
        .h(Metrics::with_rule(px(30.)))
        .px(theme.metrics.list_padding)
        .bg(colors.row_header)
        .border_b_1()
        .border_color(colors.border_header)
        .text_size(theme.text.small)
        .text_color(colors.text_muted)
        .whitespace_nowrap()
        .child(
            div()
                .flex()
                .flex_none()
                .justify_center()
                .w(px(14.))
                .child(StateDot::with_color(node_color(zone.state, theme)).size(px(7.))),
        )
        .child(
            div()
                .flex_none()
                .text_size(theme.text.body)
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(colors.text_strong)
                .child(zone.name.clone()),
        )
        .child(div().flex_1().min_w_0().truncate().child(zone.detail.clone()))
        .child(beat_slot(
            zone.beat
                .as_ref()
                .map(|beat| beat_element(beat, theme)),
        ))
        .into_any_element()
}

/// An endpoint's line; the node icygui talks to has the selected-row
/// background.
fn endpoint_row(row: &EndpointRow, theme: &Theme) -> AnyElement {
    let colors = theme.colors;
    let status = match row.tone {
        Tone::Critical => theme.states.text.critical,
        Tone::Warning => theme.states.text.warning,
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
        Some(node_color(row.state, theme)),
        row.beat.as_ref().map(|beat| beat_element(beat, theme)),
        row.this_node,
        px(34.),
        theme.text.body,
        theme,
    )
}

/// A node's dot: green connected, red down, grey unknown.
fn node_color(state: NodeState, theme: &Theme) -> Hsla {
    match state {
        NodeState::Connected => theme.states.fill.ok,
        NodeState::Disconnected => theme.states.fill.critical,
        NodeState::Unknown => theme.states.fill.pending,
    }
}

/// One stat tile: label, value (in the warning or critical text colour
/// when wrong), what it counts, and its trend (a blank of the same height
/// without one, so every tile of a line is as tall).
fn render_tile(tile: &Tile, sparklines: bool, theme: &Theme) -> AnyElement {
    let colors = theme.colors;
    let (value, last) = match tile.tone {
        Tone::Critical => (theme.states.text.critical, theme.states.fill.critical),
        Tone::Warning => (theme.states.text.warning, theme.states.fill.warning),
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
