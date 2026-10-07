//! List rows: the dashboard list's lines (screen 2a), its group headers, and
//! the host pane's compact service rows (screen 2c).

use std::fmt;
use std::ops::Range;

use crate::px;
use gpui::{
    AnyElement, App, ClickEvent, ElementId, FontWeight, HighlightStyle, InteractiveElement as _,
    IntoElement, ParentElement as _, Pixels, RenderOnce, SharedString,
    StatefulInteractiveElement as _, Styled as _, StyledText, Window, div,
    prelude::FluentBuilder as _,
};

use crate::components::{CircleSize, StateCircle};
use crate::theme::{ActiveTheme as _, CHAR_WIDTH, Density, Metrics, Theme};

type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// Width of the accent bar on marked rows.
const MARK_WIDTH: f32 = 2.;

/// The longest time a compact row shows at its right end, in characters
/// (`Oct 13`; `14m`, `213d` and `13:58` are shorter): the slot is this wide
/// whatever it shows, so nothing moves.
const TIME_SLOT_CHARS: f32 = 6.;

/// How a [`ListRow`] is highlighted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum RowEmphasis {
    /// A plain row.
    #[default]
    None,
    /// The row under the keyboard cursor, whose pane is open.
    Selected,
    /// Marked for a bulk action (multi-select).
    Marked,
    /// Both under the cursor and marked.
    SelectedMarked,
}

impl RowEmphasis {
    /// The emphasis for a row's cursor and mark flags.
    #[must_use]
    pub fn new(selected: bool, marked: bool) -> Self {
        match (selected, marked) {
            (false, false) => Self::None,
            (true, false) => Self::Selected,
            (false, true) => Self::Marked,
            (true, true) => Self::SelectedMarked,
        }
    }

    /// Whether the row is marked.
    #[must_use]
    pub fn is_marked(self) -> bool {
        matches!(self, Self::Marked | Self::SelectedMarked)
    }

    /// The row's background, if it has one.
    #[must_use]
    pub fn background(self, theme: &Theme) -> Option<gpui::Hsla> {
        match self {
            Self::None => None,
            Self::Selected | Self::SelectedMarked => Some(theme.colors.row_selected),
            Self::Marked => Some(theme.colors.row_marked),
        }
    }
}

/// The parts of a row title: `postgres-replication on db-prod-03`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Title {
    text: SharedString,
    /// The primary name (strong, medium weight).
    name: Range<usize>,
    /// The connector word (` on `, faint).
    connector: Range<usize>,
    /// The context (the host, secondary text).
    context: Range<usize>,
}

impl Title {
    fn new(name: &str, context: Option<(&str, &str)>) -> Self {
        let Some((connector, context)) = context else {
            return Self {
                text: SharedString::from(name.to_owned()),
                name: 0..name.len(),
                connector: name.len()..name.len(),
                context: name.len()..name.len(),
            };
        };
        let connector = format!(" {connector} ");
        let text = format!("{name}{connector}{context}");
        let connector_end = name.len() + connector.len();
        Self {
            name: 0..name.len(),
            connector: name.len()..connector_end,
            context: connector_end..text.len(),
            text: text.into(),
        }
    }

    /// The title as styled text; a group header's name is emphasised.
    fn styled(self, theme: &Theme, header: bool) -> StyledText {
        let colors = theme.colors;
        let highlights = [
            (
                self.name,
                HighlightStyle {
                    color: Some(if header {
                        colors.text_emphasis
                    } else {
                        colors.text_strong
                    }),
                    font_weight: Some(if header {
                        FontWeight::SEMIBOLD
                    } else {
                        FontWeight::MEDIUM
                    }),
                    ..HighlightStyle::default()
                },
            ),
            (
                self.connector,
                HighlightStyle {
                    color: Some(colors.text_faint),
                    ..HighlightStyle::default()
                },
            ),
            (
                self.context,
                HighlightStyle {
                    color: Some(colors.text_secondary),
                    ..HighlightStyle::default()
                },
            ),
        ]
        .into_iter()
        .filter(|(range, _)| !range.is_empty());
        StyledText::new(self.text).with_highlights(highlights)
    }
}

/// One line of a dashboard list, Icinga Web style: a state circle with the
/// time in state under it, `service on host` with the plugin output under
/// it, and a right-aligned tag (`ack m.keller`, `downtime`).
///
/// Every row has the same height ([`Metrics::row_height`] plus the rule), so
/// lists of any length can be virtualised with `uniform_list`. Group headers
/// are rows too ([`ListRow::header`]): a darker band with an emphasised
/// name; the rows under them are indented ([`ListRow::indent`]).
///
/// The theme's row density decides the layout: comfortable rows have two
/// lines and the time under the circle; compact rows ([`Density::Compact`])
/// are one line with a 14px circle, no detail line, and the time in a
/// fixed slot at the right end.
///
/// ```text
/// ListRow::new(("row", index))
///     .state(StateCircle::new(state).handled(handled), "14m")
///     .title("postgres-replication")
///     .context("on", "db-prod-03")
///     .detail("CRITICAL - standby lag 412s (> 300s)")
///     .emphasis(RowEmphasis::Selected)
/// ```
#[derive(IntoElement)]
#[must_use = "a row does nothing unless rendered"]
pub struct ListRow {
    id: ElementId,
    leading: Option<AnyElement>,
    state: Option<(StateCircle, SharedString)>,
    title: SharedString,
    context: Option<(SharedString, SharedString)>,
    detail: Option<SharedString>,
    tag: Option<SharedString>,
    flag: Option<SharedString>,
    emphasis: RowEmphasis,
    header: bool,
    indent: Pixels,
    on_click: Option<ClickHandler>,
}

impl ListRow {
    /// An empty row; `id` must be unique within the list.
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            leading: None,
            state: None,
            title: SharedString::default(),
            context: None,
            detail: None,
            tag: None,
            flag: None,
            emphasis: RowEmphasis::None,
            header: false,
            indent: px(0.),
            on_click: None,
        }
    }

    /// The element in the 44px column at the left, such as an icon. For a
    /// state circle use [`ListRow::state`], which follows the row density.
    pub fn leading(mut self, element: impl IntoElement) -> Self {
        self.leading = Some(element.into_any_element());
        self.state = None;
        self
    }

    /// The state circle in the column at the left, with `time` (how long
    /// in this state, or since when): under the circle in comfortable rows;
    /// in compact rows the circle is 14px and the time sits at the right
    /// end. The circle's own size and caption are set here.
    pub fn state(mut self, circle: StateCircle, time: impl Into<SharedString>) -> Self {
        self.state = Some((circle, time.into()));
        self.leading = None;
        self
    }

    /// The object's name (strong).
    pub fn title(mut self, title: impl Into<SharedString>) -> Self {
        self.title = title.into();
        self
    }

    /// Context after the name: `context("on", "db-prod-03")` reads
    /// `… on db-prod-03`.
    pub fn context(
        mut self,
        connector: impl Into<SharedString>,
        context: impl Into<SharedString>,
    ) -> Self {
        self.context = Some((connector.into(), context.into()));
        self
    }

    /// The second line: the plugin output (muted, cut off with an ellipsis).
    pub fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// The right-aligned tag (`ack m.keller`, `downtime`, `flapping`).
    pub fn tag(mut self, tag: impl Into<SharedString>) -> Self {
        self.tag = Some(tag.into());
        self
    }

    /// A warning-coloured marker before the tag: `late 12m` for a check
    /// that is overdue.
    pub fn flag(mut self, flag: impl Into<SharedString>) -> Self {
        self.flag = Some(flag.into());
        self
    }

    /// Highlights the row as selected and/or marked.
    pub fn emphasis(mut self, emphasis: RowEmphasis) -> Self {
        self.emphasis = emphasis;
        self
    }

    /// Styles the row as a group header: a darker band with a header rule
    /// and the name in semibold. Pair it with a compact leading element.
    pub fn header(mut self, header: bool) -> Self {
        self.header = header;
        self
    }

    /// Shifts the row's content right by `indent` (rows under a group
    /// header, [`crate::Metrics::row_indent`]).
    pub fn indent(mut self, indent: Pixels) -> Self {
        self.indent = indent;
        self
    }

    /// Runs `handler` when the row is clicked; the event carries the
    /// modifiers for range and toggle selection.
    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }

    /// The time the row shows with its state circle, if it has one.
    #[must_use]
    pub fn time_text(&self) -> Option<&SharedString> {
        self.state.as_ref().map(|(_, time)| time)
    }

    /// The title line's text, as rendered.
    #[must_use]
    pub fn title_text(&self) -> SharedString {
        let context = self
            .context
            .as_ref()
            .map(|(connector, context)| (connector.as_ref(), context.as_ref()));
        Title::new(&self.title, context).text
    }
}

impl fmt::Debug for ListRow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ListRow")
            .field("id", &self.id)
            .field("title", &self.title_text())
            .field("time", &self.state.as_ref().map(|(_, time)| time))
            .field("detail", &self.detail)
            .field("tag", &self.tag)
            .field("flag", &self.flag)
            .field("emphasis", &self.emphasis)
            .field("header", &self.header)
            .field("indent", &self.indent)
            .finish_non_exhaustive()
    }
}

impl RenderOnce for ListRow {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let metrics = theme.metrics;
        let compact = theme.density == Density::Compact;
        let context = self
            .context
            .as_ref()
            .map(|(connector, context)| (connector.as_ref(), context.as_ref()));
        let title = Title::new(&self.title, context).styled(theme, self.header);
        let background = self.emphasis.background(theme).or(if self.header {
            Some(colors.row_header)
        } else {
            None
        });
        let interactive = self.on_click.is_some();
        // The circle and its time: under it, or in the slot at the right.
        let (leading, time) = match self.state {
            Some((circle, time)) if compact => (
                Some(circle.size(CircleSize::Compact).into_any_element()),
                Some(time),
            ),
            Some((circle, time)) => (Some(circle.caption(time).into_any_element()), None),
            None => (self.leading, None),
        };
        let trailing = trailing(self.flag, self.tag, compact, time, theme);
        div()
            .id(self.id)
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(14.))
            .w_full()
            .h(Metrics::with_rule(metrics.row_height))
            .pl(metrics.list_padding + self.indent)
            .pr(metrics.list_padding)
            .border_b_1()
            .border_color(if self.header {
                colors.border_header
            } else {
                colors.border_row
            })
            .when_some(background, gpui::Styled::bg)
            .when(interactive && self.emphasis == RowEmphasis::None, |row| {
                row.hover(|style| style.bg(colors.row_hover))
            })
            .when(self.emphasis.is_marked(), |row| {
                row.child(
                    div()
                        .absolute()
                        .left_0()
                        .top_0()
                        .bottom_0()
                        .w(px(MARK_WIDTH))
                        .bg(colors.accent),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_none()
                    .justify_center()
                    .w(metrics.row_leading)
                    .children(leading),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(4.))
                    .child(div().truncate().text_size(theme.text.row).child(title))
                    .when_some(self.detail.filter(|_| !compact), |column, detail| {
                        column.child(
                            div()
                                .truncate()
                                .text_size(theme.text.small)
                                .text_color(colors.text_muted)
                                .child(detail),
                        )
                    }),
            )
            .children(trailing)
            .when_some(self.on_click, |row, handler| {
                row.cursor_pointer().on_click(handler)
            })
    }
}

/// What a row shows at its right end: the `late` flag, the tag, and in
/// `compact` rows the `time` in its fixed slot, empty or not, so every
/// row's tag ends at the same x.
fn trailing(
    flag: Option<SharedString>,
    tag: Option<SharedString>,
    compact: bool,
    time: Option<SharedString>,
    theme: &Theme,
) -> Vec<AnyElement> {
    let colors = theme.colors;
    let flag = flag.map(|flag| {
        div()
            .flex_none()
            .text_size(theme.text.label)
            .text_color(theme.states.text.warning)
            .child(flag)
            .into_any_element()
    });
    let tag = tag.map(|tag| {
        div()
            .flex_none()
            .max_w(px(220.))
            .truncate()
            .text_size(theme.text.label)
            .text_color(colors.text_faint)
            .child(tag)
            .into_any_element()
    });
    let time = compact.then(|| {
        div()
            .flex_none()
            .w(theme.text.label * (TIME_SLOT_CHARS * CHAR_WIDTH))
            .text_right()
            .whitespace_nowrap()
            .text_size(theme.text.label)
            .text_color(colors.text_faint)
            .children(time)
            .into_any_element()
    });
    [flag, tag, time].into_iter().flatten().collect()
}

/// A row of the host pane's service list: a 14px state circle, the name with
/// the output under it, and the time in state at the right. In compact rows
/// ([`Density::Compact`]) the output line goes, as in the dashboard list.
#[derive(IntoElement)]
#[must_use = "a row does nothing unless rendered"]
pub struct CompactRow {
    id: ElementId,
    leading: Option<AnyElement>,
    title: SharedString,
    detail: Option<SharedString>,
    trailing: Option<SharedString>,
    on_click: Option<ClickHandler>,
}

impl CompactRow {
    /// An empty row; `id` must be unique within the list.
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            leading: None,
            title: SharedString::default(),
            detail: None,
            trailing: None,
            on_click: None,
        }
    }

    /// The element in the 22px column at the left (a compact state circle).
    pub fn leading(mut self, element: impl IntoElement) -> Self {
        self.leading = Some(element.into_any_element());
        self
    }

    /// The name.
    pub fn title(mut self, title: impl Into<SharedString>) -> Self {
        self.title = title.into();
        self
    }

    /// The second line (the plugin output).
    pub fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// The right-aligned text (the time in state).
    pub fn trailing(mut self, trailing: impl Into<SharedString>) -> Self {
        self.trailing = Some(trailing.into());
        self
    }

    /// Runs `handler` when the row is clicked.
    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

impl fmt::Debug for CompactRow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CompactRow")
            .field("id", &self.id)
            .field("title", &self.title)
            .field("detail", &self.detail)
            .field("trailing", &self.trailing)
            .finish_non_exhaustive()
    }
}

impl RenderOnce for CompactRow {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let compact = theme.density == Density::Compact;
        div()
            .id(self.id)
            .flex()
            .flex_none()
            .items_center()
            .gap(px(14.))
            .px(theme.metrics.pane_inset)
            .py(px(if compact { 6. } else { 9. }))
            .border_b_1()
            .border_color(colors.border_row)
            .child(div().flex().flex_none().w(px(22.)).children(self.leading))
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
                            .text_size(theme.text.row)
                            .text_color(colors.text_strong)
                            .child(self.title),
                    )
                    .when_some(self.detail.filter(|_| !compact), |column, detail| {
                        column.child(
                            div()
                                .truncate()
                                .text_size(theme.text.small)
                                .text_color(colors.text_muted)
                                .child(detail),
                        )
                    }),
            )
            .child(
                div()
                    .flex_none()
                    .w(px(48.))
                    .text_right()
                    .whitespace_nowrap()
                    .text_size(theme.text.hint)
                    .text_color(colors.text_faint)
                    .children(self.trailing),
            )
            .when_some(self.on_click, |row, handler| {
                row.cursor_pointer()
                    .hover(|style| style.bg(colors.row_hover))
                    .on_click(handler)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_join_name_connector_and_context() {
        let title = Title::new("postgres-replication", Some(("on", "db-prod-03")));
        assert_eq!(title.text, "postgres-replication on db-prod-03");
        assert_eq!(&title.text[title.name.clone()], "postgres-replication");
        assert_eq!(&title.text[title.connector.clone()], " on ");
        assert_eq!(&title.text[title.context.clone()], "db-prod-03");
    }

    #[test]
    fn titles_without_context_are_just_the_name() {
        let title = Title::new("k8s-node-11", None);
        assert_eq!(title.text, "k8s-node-11");
        assert!(title.connector.is_empty());
        assert!(title.context.is_empty());
    }

    #[test]
    fn titles_handle_multibyte_names() {
        let title = Title::new("disk /var · ü", Some(("on", "höst")));
        assert_eq!(&title.text[title.context.clone()], "höst");
        assert!(title.text.is_char_boundary(title.connector.start));
    }

    #[test]
    fn emphasis_combines_cursor_and_marks() {
        let theme = Theme::dark();
        assert_eq!(RowEmphasis::new(false, false), RowEmphasis::None);
        assert_eq!(RowEmphasis::new(true, true), RowEmphasis::SelectedMarked);
        assert!(RowEmphasis::Marked.is_marked());
        assert!(!RowEmphasis::Selected.is_marked());
        assert_eq!(RowEmphasis::None.background(&theme), None);
        assert_eq!(
            RowEmphasis::Selected.background(&theme),
            Some(theme.colors.row_selected)
        );
        assert_eq!(
            RowEmphasis::Marked.background(&theme),
            Some(theme.colors.row_marked)
        );
    }

    #[test]
    fn rows_record_their_parts() {
        let row = ListRow::new("row")
            .title("http-tls")
            .context("on", "web-edge-02")
            .detail("SSL CRITICAL - certificate expires in 2 days")
            .tag("ack m.keller")
            .flag("late 12m")
            .emphasis(RowEmphasis::Selected)
            .on_click(|_, _, _| {});
        assert_eq!(row.title_text(), "http-tls on web-edge-02");
        assert_eq!(row.tag.as_deref(), Some("ack m.keller"));
        assert_eq!(row.flag.as_deref(), Some("late 12m"));
        assert!(row.on_click.is_some());
        assert!(format!("{row:?}").contains("web-edge-02"));

        let compact = CompactRow::new("svc").title("load").trailing("2d");
        assert_eq!(compact.trailing.as_deref(), Some("2d"));
        assert!(format!("{compact:?}").contains("load"));
    }

    #[test]
    fn a_row_has_a_state_circle_or_another_leading_element() {
        use ic_model::{CheckableState, ServiceState};
        let critical = StateCircle::new(CheckableState::Service(ServiceState::Critical));
        let row = ListRow::new("row").state(critical.clone(), "14m");
        assert_eq!(row.time_text().map(AsRef::as_ref), Some("14m"));
        assert!(format!("{row:?}").contains("14m"));
        // Whichever comes last is the leading element.
        let icon = row.leading(div());
        assert_eq!(icon.time_text(), None);
        assert!(icon.leading.is_some());
        let back = icon.state(critical, "13:58");
        assert!(back.leading.is_none());
        assert_eq!(back.time_text().map(AsRef::as_ref), Some("13:58"));
    }

    #[test]
    fn the_compact_time_slot_fits_the_longest_time() {
        // What a list's time reads: relative (`59s`, `213d`) or a clock time
        // (`13:58`, `Oct 13`, `2025`). The slot scales with the label size,
        // so it fits them at any interface size.
        let times = ["59s", "23h", "213d", "13:58", "Oct 13", "2025"];
        for scale in [0.9, 1., 1.15] {
            let theme = Theme::new(crate::ThemeMode::Dark, scale, Density::Compact);
            let slot = theme.text.label * (TIME_SLOT_CHARS * CHAR_WIDTH);
            for time in times {
                #[expect(clippy::cast_precision_loss, reason = "a few characters")]
                let width = theme.text.label * (time.chars().count() as f32 * CHAR_WIDTH);
                assert!(width <= slot, "{time} at {scale}");
            }
        }
    }
}
