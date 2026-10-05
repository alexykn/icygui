//! Text blocks: section labels and the plugin output's code block.

use gpui::{
    App, IntoElement, ParentElement as _, RenderOnce, SharedString, Styled as _, Window, div,
    prelude::FluentBuilder as _, px, relative,
};

use crate::theme::ActiveTheme as _;

/// A small faint label above a pane section (`plugin output`, `check`).
#[derive(Clone, Debug, IntoElement)]
#[must_use = "a label does nothing unless rendered"]
pub struct SectionLabel {
    text: SharedString,
}

impl SectionLabel {
    /// A label with `text`.
    pub fn new(text: impl Into<SharedString>) -> Self {
        Self { text: text.into() }
    }
}

impl RenderOnce for SectionLabel {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .text_size(theme.text.label)
            .text_color(theme.colors.text_faint)
            .whitespace_nowrap()
            .child(self.text)
    }
}

/// Preformatted text on the code surface: plugin output, long output and
/// command lines. Line breaks and runs of spaces are kept; long lines wrap.
#[derive(Clone, Debug, IntoElement)]
#[must_use = "a code block does nothing unless rendered"]
pub struct CodeBlock {
    text: SharedString,
    label: Option<SharedString>,
}

impl CodeBlock {
    /// A block showing `text`.
    pub fn new(text: impl Into<SharedString>) -> Self {
        Self {
            text: text.into(),
            label: None,
        }
    }

    /// Shows a [`SectionLabel`] above the block.
    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }
}

impl RenderOnce for CodeBlock {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let block = div()
            .px(px(14.))
            .py(px(12.))
            .rounded(theme.metrics.code_radius)
            .border_1()
            .border_color(colors.border_header)
            .bg(colors.code_background)
            .text_size(theme.text.body)
            .line_height(relative(1.7))
            .text_color(colors.text_code)
            .overflow_hidden()
            .child(self.text);
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .when_some(self.label, |column, label| {
                column.child(SectionLabel::new(label))
            })
            .child(block)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_text_verbatim() {
        let text = "CRITICAL - standby lag 412s (> 300s)\nprimary  db-prod-01  lsn 4A/9C21F0D8";
        let block = CodeBlock::new(text).label("plugin output");
        assert_eq!(block.text, text);
        assert_eq!(block.label.as_deref(), Some("plugin output"));
    }
}
