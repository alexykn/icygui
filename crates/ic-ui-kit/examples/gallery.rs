//! Every component of the kit with the design's sample data, laid out like
//! screen 2b's service pane, for checking them by eye:
//!
//! ```sh
//! cargo run -p ic-ui-kit --example gallery
//! ```

use gpui::{
    App, AppContext as _, Bounds, Context, Entity, FontWeight, IntoElement, ParentElement as _,
    Render, Styled as _, Window, WindowBounds, WindowOptions, div, size,
};
use ic_model::{CheckableState, HostState, ServiceState, parse_perfdata};
use ic_ui_kit::input::InputState;
use ic_ui_kit::{
    ActiveTheme as _, Button, CircleSize, CodeBlock, Divider, DividerColor, Icon, IconButton,
    IconName, KvTable, PaneHeader, PerfdataTable, Root, SectionLabel, StateCircle, StateDot,
    SubTabs, SummaryBar, SummaryItem, TextField, Tooltip, px,
};

const CRITICAL: CheckableState = CheckableState::Service(ServiceState::Critical);
const WARNING: CheckableState = CheckableState::Service(ServiceState::Warning);
const UNKNOWN: CheckableState = CheckableState::Service(ServiceState::Unknown);
const OK: CheckableState = CheckableState::Service(ServiceState::Ok);
const UP: CheckableState = CheckableState::Host(HostState::Up);

const OUTPUT: &str = "CRITICAL - standby lag 412s (> 300s)\n\
primary  db-prod-01  lsn 4A/9C21F0D8\n\
standby  db-prod-03  lsn 4A/2E77A120\n\
slot     repl_db03   retained 1.8 GiB";

struct Gallery {
    search: Entity<InputState>,
    bordered: Entity<InputState>,
    tab: usize,
}

impl Gallery {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            search: cx.new(|cx| InputState::new(window, cx).placeholder("Search dashboards…")),
            bordered: cx.new(|cx| InputState::new(window, cx).placeholder("bordered field")),
            tab: 0,
        }
    }

    /// Screen 2b's service pane.
    fn service_pane(cx: &App) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let colors = theme.colors;
        let perfdata = parse_perfdata(
            "replication_lag=412s;60;300;0;3600 wal_retained=1.8GiB;4;8 active_connections=182;400;450;0",
        );
        let title = pane_title(cx);
        let actions = pane_actions();
        let check = KvTable::new()
            .title("check")
            .row("command", "check_postgres")
            .row("interval", "60s · retry 15s")
            .row("last / next", "12s ago / in 48s")
            .row("endpoint", "sat-ams-01")
            .row("notified", "dba-oncall · 14:32");
        let comment = pane_comment(cx);
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(theme.metrics.pane_width)
            .h_full()
            .bg(colors.pane_background)
            .border_l_1()
            .border_color(colors.border_split)
            .child(
                PaneHeader::new("pane")
                    .label("service")
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(4.))
                            .text_size(theme.text.small)
                            .text_color(colors.text_faint)
                            .child(Icon::new(IconName::ArrowUpRight).size(px(12.)))
                            .child("open as tab"),
                    )
                    .on_close(|_, _, _| {}),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(24.))
                    .px(px(24.))
                    .py(theme.metrics.pane_padding)
                    .child(title)
                    .child(actions)
                    .child(CodeBlock::new(OUTPUT).label("plugin output"))
                    .child(PerfdataTable::new(&perfdata))
                    .child(check)
                    .child(comment),
            )
    }

    fn catalogue(&self, cx: &Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let colors = theme.colors;
        let section = |title: &'static str| div().pt(px(8.)).child(SectionLabel::new(title));
        let row = || div().flex().flex_wrap().items_center().gap(px(14.));
        let entity = cx.entity();
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .gap(px(10.))
            .p(px(20.))
            .child(states(theme))
            .child(section("summary bar"))
            .child(
                div().border_t_1().border_color(colors.border_header).child(
                    SummaryBar::new()
                        .child(SummaryItem::new(CRITICAL, 12, "critical"))
                        .child(SummaryItem::new(WARNING, 29, "warning"))
                        .child(SummaryItem::new(UNKNOWN, 24, "unknown"))
                        .end("handled hidden"),
                ),
            )
            .child(section("sub tabs"))
            .child(
                SubTabs::new("host-tabs")
                    .tab_with_count("services", 23)
                    .tab("history")
                    .tab("vars")
                    .tab("config")
                    .selected(self.tab)
                    .on_select(move |index, _, cx| {
                        entity.update(cx, |gallery, cx| {
                            gallery.tab = index;
                            cx.notify();
                        });
                    }),
            )
            .child(section("text fields"))
            .child(
                row()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .w(px(260.))
                            .child(Icon::new(IconName::Search).color(colors.text_muted))
                            .child(div().flex_1().child(TextField::new(&self.search))),
                    )
                    .child(
                        div()
                            .w(px(260.))
                            .child(TextField::new(&self.bordered).bordered(true)),
                    ),
            )
            .child(section("icon buttons and icons"))
            .child(
                row()
                    .child(
                        IconButton::new("panel", IconName::PanelLeft)
                            .tooltip(Tooltip::new("Hide sidebar").key("ctrl-b")),
                    )
                    .child(IconButton::new("clock", IconName::Clock).selected(true))
                    .child(IconButton::new("add", IconName::Plus).disabled(true))
                    .children(IconName::ALL.into_iter().map(|icon| {
                        Icon::new(icon)
                            .size(theme.metrics.icon_large)
                            .color(colors.text_secondary)
                    })),
            )
            .child(section("dividers"))
            .child(
                row()
                    .h(px(24.))
                    .child("a")
                    .child(
                        Divider::vertical()
                            .color(DividerColor::Window)
                            .length(px(18.))
                            .margin(px(6.)),
                    )
                    .child("b"),
            )
            .child(Divider::horizontal().color(DividerColor::Split))
    }
}

impl Render for Gallery {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .flex()
            .size_full()
            .bg(theme.colors.window_background)
            .font_family(theme.font_family.clone())
            .line_height(theme.line_height)
            .text_color(theme.colors.text)
            .child(self.catalogue(cx))
            .child(Self::service_pane(cx))
    }
}

fn pane_title(cx: &App) -> impl IntoElement + use<> {
    let theme = cx.theme();
    let colors = theme.colors;
    div()
        .flex()
        .items_center()
        .gap(px(16.))
        .child(
            StateCircle::new(CRITICAL)
                .size(CircleSize::Pane)
                .state_label(),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(
                    div()
                        .text_size(theme.text.title)
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(colors.text_strong)
                        .child("postgres-replication"),
                )
                .child(
                    div()
                        .flex()
                        .gap(px(6.))
                        .text_size(theme.text.body)
                        .text_color(colors.text_muted)
                        .child("on")
                        .child(div().text_color(colors.accent).child("db-prod-03"))
                        .child("· 14m · hard 3/3"),
                ),
        )
}

fn pane_actions() -> impl IntoElement {
    div()
        .flex()
        .flex_wrap()
        .gap(px(8.))
        .child(Button::new("ack", "acknowledge").primary().key_hint("a"))
        .child(Button::new("downtime", "downtime").key_hint("d"))
        .child(Button::new("check", "check now").key_hint("r"))
        .child(Button::new("comment", "comment").key_hint("c"))
        .child(
            Button::new("run", "run command")
                .disabled(true)
                .tooltip(Tooltip::new("Missing permission: actions/execute-command")),
        )
}

fn pane_comment(cx: &App) -> impl IntoElement + use<> {
    let theme = cx.theme();
    let colors = theme.colors;
    div()
        .flex()
        .gap(px(12.))
        .text_size(theme.text.body)
        .text_color(colors.text_secondary)
        .child(div().flex_none().text_color(colors.text_faint).child("#"))
        .child(
            div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .flex()
                        .gap(px(8.))
                        .child(div().text_color(colors.text).child("j.berg"))
                        .child(div().text_color(colors.text_faint).child("13:58")),
                )
                .child("Failover drill on db-prod-01 at 15:00. Expect lag on 03."),
        )
}

fn states(theme: &ic_ui_kit::Theme) -> impl IntoElement + use<> {
    let section = |title: &'static str| div().pt(px(8.)).child(SectionLabel::new(title));
    let row = || div().flex().flex_wrap().items_center().gap(px(14.));
    div()
        .flex()
        .flex_col()
        .gap(px(10.))
        .child(section("state circles: row, handled, compact, pane"))
        .child(
            row()
                .child(StateCircle::new(CRITICAL).caption("14m"))
                .child(StateCircle::new(CRITICAL).handled(true).caption("2h"))
                .child(StateCircle::new(WARNING).caption("9m"))
                .child(StateCircle::new(WARNING).handled(true).caption("3h"))
                .child(StateCircle::new(UNKNOWN).caption("52m"))
                .child(StateCircle::new(OK).size(CircleSize::Compact))
                .child(
                    StateCircle::new(WARNING)
                        .size(CircleSize::Compact)
                        .handled(true),
                )
                .child(StateCircle::new(UP).size(CircleSize::Pane).state_label()),
        )
        .child(section("state dots"))
        .child(
            row()
                .child(StateDot::new(CRITICAL))
                .child(StateDot::new(UNKNOWN))
                .child(StateDot::new(WARNING))
                .child(StateDot::new(OK))
                .child(StateDot::with_color(theme.states.fill.pending)),
        )
}

fn main() {
    gpui_platform::application()
        .with_assets(ic_ui_kit::Assets)
        .run(|cx: &mut App| {
            if ic_ui_kit::init(cx).is_err() {
                cx.quit();
                return;
            }
            // `GALLERY_THEME=light` shows the light theme.
            if std::env::var("GALLERY_THEME").is_ok_and(|theme| theme == "light") {
                ic_ui_kit::set_theme(ic_ui_kit::Theme::light(), cx);
            }
            #[expect(clippy::disallowed_methods, reason = "window geometry is real pixels")]
            let bounds = Bounds::centered(None, size(gpui::px(1400.), gpui::px(940.)), cx);
            let opened = cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..WindowOptions::default()
                },
                |window, cx| {
                    let gallery = cx.new(|cx| Gallery::new(window, cx));
                    cx.new(|cx| Root::new(gallery, window, cx))
                },
            );
            if opened.is_err() {
                cx.quit();
            }
        });
}
