//! The cluster health page (topic 06): one page per environment, reached
//! from *health* in the sidebar's cluster section, the footer switcher's
//! *cluster health* row and the palette. Its header, the health line
//! (endpoints connected and not, Icinga's version and uptime), a banner
//! while a zone is cut off, then four sections that fold: the zones with
//! their endpoints, the checks, the queues and connections, and Icinga's
//! global switches with the node's features. What it shows is worked out
//! in [`super::health`]; this module only draws it.

use std::time::Duration;

use gpui::{
    AnyElement, ClickEvent, Context, Entity, FontWeight, Hsla, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Window, div, prelude::FluentBuilder as _,
};
use ic_core::NodeState;
use ic_model::{FeatureState, ObjectKey, Timestamp, format_compact};
use ic_ui_kit::{
    ActiveTheme as _, Banner, BannerTone, EmptyState, Icon, IconName, Link, Metrics, ObjectMark,
    PaneHeader, StateDot, Theme, px,
};

use super::health::{EndpointRow, HealthBanner, Report, Tile, Tone, ZoneGroup, count_text, report};
use super::spark;
use crate::app_state::AppState;
use crate::chrome::{Controls, WindowDrag};
use crate::workspace::sidebar_reopen;

/// The late checks the banner's fold shows before `+ N more` (the hosts'
/// paging rule).
const LATE_PREVIEW: usize = 7;

/// The page's foldable sections.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Section {
    Zones,
    Checks,
    Queues,
    Switches,
}

impl Section {
    fn index(self) -> usize {
        match self {
            Self::Zones => 0,
            Self::Checks => 1,
            Self::Queues => 2,
            Self::Switches => 3,
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Zones => "health-zones",
            Self::Checks => "health-checks",
            Self::Queues => "health-queues",
            Self::Switches => "health-switches",
        }
    }
}

/// The cluster section's *health* page.
pub(crate) struct HealthPage {
    state: Entity<AppState>,
    sidebar_open: bool,
    drag: WindowDrag,
    focus_handle: gpui::FocusHandle,
    /// The folded sections.
    folded: [bool; 4],
    /// The banner's late checks are shown, and all of them.
    late_open: bool,
    late_all: bool,
    _subscription: Subscription,
}

impl HealthPage {
    pub(crate) fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.observe(&state, |_, _, cx| cx.notify());
        Self {
            state,
            sidebar_open: true,
            drag: WindowDrag::default(),
            focus_handle: cx.focus_handle(),
            folded: [false; 4],
            late_open: false,
            late_all: false,
            _subscription: subscription,
        }
    }

    /// Tells the page whether the sidebar is shown (the header then needs
    /// no window controls).
    pub(crate) fn set_sidebar_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.sidebar_open != open {
            self.sidebar_open = open;
            cx.notify();
        }
    }

    /// Whether `section` is folded.
    fn is_folded(&self, section: Section) -> bool {
        self.folded[section.index()]
    }

    /// Folds or unfolds `section`.
    fn toggle(&mut self, section: Section, cx: &mut Context<Self>) {
        let folded = &mut self.folded[section.index()];
        *folded = !*folded;
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
            );
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

    /// The banner while a zone is cut off (or an endpoint is down), with
    /// *show the late checks* when its zone has some.
    fn render_banner(&self, banner: &HealthBanner, cx: &Context<Self>) -> AnyElement {
        let tone = match banner.tone {
            Tone::Critical => BannerTone::Critical,
            Tone::Warning | Tone::Normal => BannerTone::Warning,
        };
        let mut element =
            Banner::new("health-banner", tone, banner.title.clone()).detail(banner.detail.clone());
        if banner.late_zone.is_some() {
            element = element.child(
                Link::new(
                    "health-late",
                    if self.late_open {
                        "hide the late checks"
                    } else {
                        "show the late checks"
                    },
                )
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.late_open = !this.late_open;
                    cx.notify();
                })),
            );
        }
        element.into_any_element()
    }

    /// The cut-off zone's late checks under the banner, most overdue first:
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

    /// One late check under the banner: its mark, `service on host`, how
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

    /// A section's 36px header (as a stacked view's): the fold chevron,
    /// the icon, the name, what it shows (faint), and anything at the right.
    fn render_section_header(
        &self,
        section: Section,
        icon: IconName,
        name: &'static str,
        about: String,
        right: Option<AnyElement>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let folded = self.is_folded(section);
        div()
            .id(section.id())
            .flex()
            .flex_none()
            .items_center()
            .gap(px(10.))
            .h(Metrics::with_rule(theme.metrics.summary_bar_height))
            .pl(px(14.))
            .pr(theme.metrics.list_padding)
            .bg(colors.pane_background)
            .border_b_1()
            .border_color(colors.border_header)
            .when(section != Section::Zones, gpui::Styled::border_t_1)
            .whitespace_nowrap()
            .text_size(theme.text.small)
            .text_color(colors.text_muted)
            .cursor_pointer()
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
            .child(Icon::new(icon).size(px(13.)).color(colors.text_muted))
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
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle(section, cx)))
            .into_any_element()
    }

    /// The four sections, each a header and (unless folded) its body.
    fn render_sections(&self, report: &Report, cx: &Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme();
        let counts = zone_counts(report, theme);
        let zones_about = format!(
            "{} · {} · {}",
            plural(report.zones.len(), "zone", "zones"),
            plural(
                report.zones.iter().map(|zone| zone.endpoints.len()).sum(),
                "endpoint",
                "endpoints"
            ),
            plural(report.global_zones.len(), "global zone", "global zones")
        );
        let mut body: Vec<AnyElement> = Vec::new();
        body.push(self.render_section_header(
            Section::Zones,
            IconName::List,
            "zones and endpoints",
            zones_about,
            Some(counts),
            cx,
        ));
        if !self.is_folded(Section::Zones) {
            body.push(Self::render_zones(report, cx.theme()));
        }
        body.push(self.render_section_header(
            Section::Checks,
            IconName::ChartBar,
            "checks",
            "last minute, from /v1/status".to_owned(),
            None,
            cx,
        ));
        if !self.is_folded(Section::Checks) {
            body.push(Self::render_tiles(
                "health-check-tiles",
                &report.checks,
                cx.theme(),
            ));
        }
        body.push(self.render_section_header(
            Section::Queues,
            IconName::ChartBar,
            "queues and connections",
            "ApiListener, JsonRpc".to_owned(),
            None,
            cx,
        ));
        if !self.is_folded(Section::Queues) {
            body.push(Self::render_tiles(
                "health-queue-tiles",
                &report.queues,
                cx.theme(),
            ));
        }
        body.push(self.render_section_header(
            Section::Switches,
            IconName::List,
            "Icinga’s global switches",
            "read-only: icygui never changes them".to_owned(),
            None,
            cx,
        ));
        if !self.is_folded(Section::Switches) {
            body.push(Self::render_switches(report, cx.theme()));
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

    /// A line of stat tiles: six columns, a wide tile spanning two.
    fn render_tiles(id: &'static str, tiles: &[Tile], theme: &Theme) -> AnyElement {
        div()
            .id(id)
            .grid()
            .grid_cols(6)
            .gap(px(12.))
            .flex_none()
            .pt(px(14.))
            .pb(px(16.))
            .px(theme.metrics.list_padding)
            .children(tiles.iter().map(|tile| render_tile(tile, theme)))
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
        if report.late.is_empty() {
            self.late_open = false;
            self.late_all = false;
        }
        let header = self.render_header(&report, controls, now, cx);
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
                .child(header)
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
        let banner = report
            .banner
            .as_ref()
            .map(|banner| self.render_banner(banner, cx));
        let late = self.render_late(&report, now, cx);
        let body = self.render_sections(&report, cx);
        div()
            .id("health-page")
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(header)
            .child(health_line)
            .children(banner)
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
        .into_any_element()
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
        .child(div().min_w_0().truncate().child(zone.detail.clone()))
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
fn render_tile(tile: &Tile, theme: &Theme) -> AnyElement {
    let colors = theme.colors;
    let (value, last) = match tile.tone {
        Tone::Critical => (theme.states.text.critical, theme.states.fill.critical),
        Tone::Warning => (theme.states.text.warning, theme.states.fill.warning),
        Tone::Normal => (colors.text_strong, colors.accent),
    };
    let trend: AnyElement = if tile.trend.len() >= 2 {
        spark::sparkline(&tile.trend, colors.text_faint, last).into_any_element()
    } else {
        div().h(px(spark::HEIGHT)).into_any_element()
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
        .child(trend)
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
