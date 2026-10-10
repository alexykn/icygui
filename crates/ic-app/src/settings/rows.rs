//! The settings panel's building blocks, drawn as mock-up 02 draws them: a
//! section label over its rows, and rows of a name, one line of
//! description and a right-aligned control, split by the list's row
//! rules. Rows that depend on a switch above them are indented by 20px; a
//! value that doesn't parse shows its problem under the row's name, in the
//! description's line, so nothing moves while it shows.
//!
//! A row too narrow for its name and its control (a small window, a large
//! interface size) puts the control on a line of its own under the name
//! rather than over it; names and descriptions that don't fit end in
//! `…`.

use gpui::{
    AnyElement, FontWeight, HighlightStyle, IntoElement, ParentElement as _, SharedString,
    Styled as _, StyledText, div, prelude::FluentBuilder as _,
};
use ic_ui_kit::{Icon, IconName, Theme, px};

use super::model::match_ranges;

/// Space above a section label.
const SECTION_TOP: f32 = 22.;
/// Space between a section label and its first row.
const SECTION_BOTTOM: f32 = 8.;
/// A row's padding above and below.
const ROW_PADDING: f32 = 12.;
/// Space between a row's name and its description: with the line heights
/// rounded to whole pixels, this makes a two-line row 61px with its rule,
/// as in the frames.
const DESCRIPTION_GAP: f32 = 3.;
/// The narrowest a row's name and description get before its control
/// moves under them.
const TEXT_MIN_WIDTH: f32 = 200.;
/// The indent of a row that depends on the one above.
pub(crate) const SUB_INDENT: f32 = 20.;
/// The gap between a row's text and its control.
const ROW_GAP: f32 = 24.;

/// A section label (`default rule · groups and dashboards notify with it
/// unless they say otherwise`), faint. In search results the label names
/// the category (`notifications · quiet hours`) in the secondary colour,
/// with what matches `query` (a section found by its name) in the accent
/// colour.
pub(crate) fn section_label(
    label: &str,
    note: Option<&str>,
    heading: bool,
    query: &str,
    theme: &Theme,
) -> gpui::Div {
    let colors = theme.colors;
    div()
        .flex()
        .items_baseline()
        .gap(px(10.))
        .pt(px(if heading { 20. } else { SECTION_TOP }))
        .pb(px(SECTION_BOTTOM))
        .text_size(theme.text.label)
        .text_color(if heading {
            colors.text_secondary
        } else {
            colors.text_faint
        })
        .whitespace_nowrap()
        .child(marked(label, query, theme))
        .when_some(note, |row, note| {
            row.child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(colors.text_faint)
                    .child(format!("· {note}")),
            )
        })
}

/// Text with the parts matching `query` (normalized) in the accent
/// colour, as the palette marks its matches.
pub(crate) fn marked(text: &str, query: &str, theme: &Theme) -> StyledText {
    let accent = HighlightStyle {
        color: Some(theme.colors.accent_text),
        ..HighlightStyle::default()
    };
    StyledText::new(SharedString::from(text.to_owned())).with_highlights(
        match_ranges(text, query)
            .into_iter()
            .map(|range| (range, accent)),
    )
}

/// One row of settings.
pub(crate) struct Row {
    /// The name, already marked for a search.
    pub(crate) name: AnyElement,
    /// The description (empty for none), marked for a search.
    pub(crate) description: Option<AnyElement>,
    /// What the value's problem is, under the row.
    pub(crate) error: Option<String>,
    /// The control at the right.
    pub(crate) control: Option<AnyElement>,
    /// Indented under the row it depends on.
    pub(crate) indent: f32,
}

impl Row {
    /// A row named `name`.
    pub(crate) fn new(name: impl IntoElement) -> Self {
        Self {
            name: name.into_any_element(),
            description: None,
            error: None,
            control: None,
            indent: 0.,
        }
    }

    /// Adds the line of description.
    pub(crate) fn description(mut self, description: Option<impl IntoElement>) -> Self {
        self.description = description.map(IntoElement::into_any_element);
        self
    }

    /// Adds the control.
    pub(crate) fn control(mut self, control: impl IntoElement) -> Self {
        self.control = Some(control.into_any_element());
        self
    }

    /// Shows a problem under the row.
    pub(crate) fn error(mut self, error: Option<String>) -> Self {
        self.error = error;
        self
    }

    /// Indents the row.
    pub(crate) fn indent(mut self, indent: f32) -> Self {
        self.indent = indent;
        self
    }

    /// The row, drawn.
    pub(crate) fn render(self, theme: &Theme) -> AnyElement {
        let colors = theme.colors;
        // The problem takes the description's line while it lasts.
        let second_line = match (self.error, self.description) {
            (Some(error), _) => Some(
                div()
                    .text_size(theme.text.small)
                    .text_color(theme.states.text.critical)
                    .truncate()
                    .child(error)
                    .into_any_element(),
            ),
            (None, Some(description)) => Some(
                div()
                    .text_size(theme.text.small)
                    .text_color(colors.text_muted)
                    .truncate()
                    .child(description)
                    .into_any_element(),
            ),
            (None, None) => None,
        };
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_x(px(ROW_GAP))
            .gap_y(px(8.))
            .py(px(ROW_PADDING))
            .pl(px(self.indent))
            .min_h(px(34. + 2. * ROW_PADDING))
            .border_t_1()
            .border_color(colors.border_row)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .flex_basis(px(0.))
                    .min_w(px(TEXT_MIN_WIDTH))
                    .overflow_hidden()
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_size(theme.text.row)
                            .text_color(colors.text_strong)
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(self.name),
                    )
                    .when_some(second_line, |text, line| {
                        text.child(div().mt(px(DESCRIPTION_GAP)).min_w_0().child(line))
                    }),
            )
            .when_some(self.control, |row, control| {
                row.child(
                    div()
                        .flex()
                        .flex_none()
                        .ml_auto()
                        .items_center()
                        .justify_end()
                        .gap(px(6.))
                        .child(control),
                )
            })
            .into_any_element()
    }
}

/// A note under a section: an info icon and muted text that may wrap.
pub(crate) fn info_note(text: impl Into<SharedString>, theme: &Theme) -> AnyElement {
    let colors = theme.colors;
    div()
        .flex()
        .items_start()
        .gap(px(10.))
        .py(px(ROW_PADDING))
        .border_t_1()
        .border_color(colors.border_row)
        .text_size(theme.text.small)
        .line_height(gpui::relative(1.5))
        .text_color(colors.text_muted)
        .child(
            div().pt(px(2.)).flex_none().child(
                Icon::new(IconName::Info)
                    .size(px(14.))
                    .color(colors.text_faint),
            ),
        )
        .child(div().flex_1().min_w_0().child(text.into()))
        .into_any_element()
}

/// Muted words beside a control (`at most`, `seconds`).
pub(crate) fn words(text: &'static str, theme: &Theme) -> AnyElement {
    div()
        .flex_none()
        .text_size(theme.text.small)
        .text_color(theme.colors.text_muted)
        .child(text)
        .into_any_element()
}

/// A key as the keymap table shows it: a small framed hint, as tall as a
/// chip.
pub(crate) fn key_cap(key: &str, theme: &Theme) -> AnyElement {
    let colors = theme.colors;
    div()
        .flex()
        .flex_none()
        .items_center()
        .h(px(ic_ui_kit::CHIP_HEIGHT))
        .px(px(6.))
        .rounded(theme.metrics.small_radius)
        .border_1()
        .border_color(colors.border_header)
        .bg(colors.element_background)
        .text_size(theme.text.label)
        .text_color(colors.text)
        .whitespace_nowrap()
        .child(key.to_owned())
        .into_any_element()
}

/// The panel's page title in the header (`general`), like a dashboard's.
pub(crate) fn title(text: impl Into<SharedString>, theme: &Theme) -> gpui::Div {
    div()
        .flex_none()
        .text_size(theme.text.heading)
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme.colors.text_strong)
        .child(text.into())
}
