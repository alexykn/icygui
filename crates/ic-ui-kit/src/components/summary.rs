//! The state summary bar above a dashboard list:
//! `● 12 critical  ● 29 warning  ● 24 unknown      handled hidden`.

use std::fmt;

use gpui::{
    AnyElement, App, Hsla, IntoElement, ParentElement, RenderOnce, SharedString, Styled as _,
    Window, div, px,
};
use ic_model::CheckableState;

use crate::components::{Paint, StateDot};
use crate::theme::{ActiveTheme as _, Metrics};

/// One count in the summary bar: a state dot and `12 critical`.
#[derive(Clone, Debug, IntoElement)]
#[must_use = "a summary item does nothing unless rendered"]
pub struct SummaryItem {
    paint: Paint,
    count: u32,
    label: SharedString,
}

impl SummaryItem {
    /// `count` objects in `state`, labelled `label` (`critical`).
    pub fn new(state: CheckableState, count: u32, label: impl Into<SharedString>) -> Self {
        Self {
            paint: Paint::State(state),
            count,
            label: label.into(),
        }
    }

    /// A count with a dot in any colour.
    pub fn with_color(color: Hsla, count: u32, label: impl Into<SharedString>) -> Self {
        Self {
            paint: Paint::Color(color),
            count,
            label: label.into(),
        }
    }

    /// The text after the dot.
    #[must_use]
    pub fn text(&self) -> String {
        format!("{} {}", self.count, self.label)
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
            .gap(px(7.))
            .child(dot.size(size))
            .child(text)
    }
}

/// The 36px bar holding [`SummaryItem`]s, with an optional element at the
/// right end (the `handled hidden` toggle).
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
            .gap(px(18.))
            .h(Metrics::with_rule(theme.metrics.summary_bar_height))
            .px(theme.metrics.list_padding)
            .border_b_1()
            .border_color(colors.border_header)
            .text_size(theme.text.small)
            .text_color(colors.text_muted)
            .whitespace_nowrap()
            .overflow_hidden()
            .children(self.items)
            .child(div().flex_1())
            .children(
                self.end
                    .map(|end| div().text_color(colors.text_faint).child(end)),
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
