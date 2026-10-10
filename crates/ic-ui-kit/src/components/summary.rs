//! The state summary bar above a dashboard list:
//! `● 12 critical  ● 29 warning  ● 24 unknown      handled hidden`.

use std::fmt;

use crate::px;
use gpui::{
    AnyElement, App, Hsla, IntoElement, ParentElement, Pixels, RenderOnce, SharedString,
    Styled as _, Window, div,
};
use ic_model::CheckableState;

use crate::components::{Paint, StateDot};
use crate::theme::{ActiveTheme as _, CHAR_WIDTH, Metrics, Theme};

/// Space between a summary item's dot and its text.
const DOT_GAP: f32 = 7.;
/// Space between summary items.
const ITEM_GAP: f32 = 18.;

/// One count in the summary bar: a state dot and `12 critical`, or just
/// `12` when compact.
#[derive(Clone, Debug, IntoElement)]
#[must_use = "a summary item does nothing unless rendered"]
pub struct SummaryItem {
    paint: Paint,
    count: u32,
    label: SharedString,
    compact: bool,
}

impl SummaryItem {
    /// `count` objects in `state`, labelled `label` (`critical`).
    pub fn new(state: CheckableState, count: u32, label: impl Into<SharedString>) -> Self {
        Self {
            paint: Paint::State(state),
            count,
            label: label.into(),
            compact: false,
        }
    }

    /// A count with a dot in any colour.
    pub fn with_color(color: Hsla, count: u32, label: impl Into<SharedString>) -> Self {
        Self {
            paint: Paint::Color(color),
            count,
            label: label.into(),
            compact: false,
        }
    }

    /// Shows only the count; the dot's colour says what it counts (for a
    /// list too narrow for the labels, see [`SummaryBar::fits`]).
    pub fn compact(mut self, compact: bool) -> Self {
        self.compact = compact;
        self
    }

    /// The text after the dot.
    #[must_use]
    pub fn text(&self) -> String {
        if self.compact {
            self.count.to_string()
        } else {
            format!("{} {}", self.count, self.label)
        }
    }
}

impl RenderOnce for SummaryItem {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let size = cx.theme().metrics.summary_dot;
        let text = self.text();
        let dot = match self.paint {
            Paint::State(state) => StateDot::new(state),
            Paint::Color(color) => StateDot::with_color(color),
        };
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(DOT_GAP))
            .child(dot.size(size))
            .child(text)
    }
}

/// The 36px bar holding [`SummaryItem`]s, with an optional element at the
/// right end (the `handled hidden` toggle). In a narrow list the items are
/// cut off at the right; the end element always stays visible.
#[derive(IntoElement)]
#[must_use = "a summary bar does nothing unless rendered"]
pub struct SummaryBar {
    items: Vec<AnyElement>,
    end: Option<AnyElement>,
}

impl SummaryBar {
    /// An empty bar.
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            end: None,
        }
    }

    /// Sets the element at the right end.
    pub fn end(mut self, element: impl IntoElement) -> Self {
        self.end = Some(element.into_any_element());
        self
    }

    /// Whether items reading `texts` (`12 critical`) and an end element
    /// reading `end` fit on a bar `width` wide. The bundled font is
    /// monospaced, so this is exact; where they don't fit, show the items
    /// [`SummaryItem::compact`].
    #[must_use]
    pub fn fits<'a>(
        texts: impl IntoIterator<Item = &'a str>,
        end: &str,
        width: Pixels,
        theme: &Theme,
    ) -> bool {
        #[expect(
            clippy::cast_precision_loss,
            reason = "text lengths are far below f32's exact range"
        )]
        let text_width = |text: &str| theme.text.small * (CHAR_WIDTH * text.chars().count() as f32);
        let item = theme.metrics.summary_dot + px(DOT_GAP);
        let items = texts
            .into_iter()
            .map(|text| item + text_width(text))
            .reduce(|total, width| total + px(ITEM_GAP) + width)
            .unwrap_or_default();
        let end = if end.is_empty() {
            px(0.)
        } else {
            px(ITEM_GAP) + text_width(end)
        };
        theme.metrics.list_padding * 2. + items + end <= width
    }
}

impl Default for SummaryBar {
    fn default() -> Self {
        Self::new()
    }
}

impl ParentElement for SummaryBar {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.items.extend(elements);
    }
}

impl fmt::Debug for SummaryBar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SummaryBar")
            .field("items", &self.items.len())
            .field("end", &self.end.is_some())
            .finish()
    }
}

impl RenderOnce for SummaryBar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let colors = theme.colors;
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(ITEM_GAP))
            .h(Metrics::with_rule(theme.metrics.summary_bar_height))
            .px(theme.metrics.list_padding)
            .border_b_1()
            .border_color(colors.border_header)
            .text_size(theme.text.small)
            .text_color(colors.text_muted)
            .whitespace_nowrap()
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap(px(ITEM_GAP))
                    .overflow_hidden()
                    .children(self.items),
            )
            .children(
                self.end
                    .map(|end| div().flex_none().text_color(colors.text_faint).child(end)),
            )
    }
}

#[cfg(test)]
mod tests {
    use ic_model::ServiceState;

    use super::*;

    #[test]
    fn items_read_count_then_label() {
        let item = SummaryItem::new(
            CheckableState::Service(ServiceState::Critical),
            12,
            "critical",
        );
        assert_eq!(item.text(), "12 critical");
    }

    #[test]
    fn compact_items_show_the_count_only() {
        let item = SummaryItem::new(CheckableState::Service(ServiceState::Unknown), 2, "unknown");
        assert_eq!(item.compact(true).text(), "2");
    }

    #[test]
    fn labels_fit_the_design_width_but_not_a_narrow_list() {
        let theme = Theme::dark();
        let texts = ["10 critical", "7 warning", "2 unknown"];
        // The design's list beside the pane is 520px wide.
        assert!(SummaryBar::fits(texts, "handled shown", px(520.), &theme));
        assert!(!SummaryBar::fits(texts, "handled shown", px(440.), &theme));
        assert!(SummaryBar::fits(
            ["10", "7", "2"],
            "handled shown",
            px(440.),
            &theme
        ));
        assert!(SummaryBar::fits([], "", px(36.), &theme));
    }

    #[test]
    fn bars_collect_items() {
        let bar = SummaryBar::new()
            .child(SummaryItem::new(
                CheckableState::Service(ServiceState::Warning),
                29,
                "warning",
            ))
            .end("handled hidden");
        assert_eq!(bar.items.len(), 1);
        assert!(bar.end.is_some());
    }
}
