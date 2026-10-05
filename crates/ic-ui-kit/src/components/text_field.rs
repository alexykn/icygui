//! Single-line text fields: gpui-component's input, styled like the design.

use gpui::{App, Entity, IntoElement, Pixels, RenderOnce, Styled, Window, px};
use gpui_component::input::{Input, InputState};

use crate::theme::ActiveTheme as _;

/// A single-line text field bound to an [`InputState`] (create the state with
/// `cx.new(|cx| InputState::new(window, cx).placeholder("…"))` and subscribe
/// to its `InputEvent`s).
///
/// Borderless by default, like the sidebar's search field; text and
/// placeholder colours come from the theme.
#[derive(Clone, Debug, IntoElement)]
#[must_use = "a text field does nothing unless rendered"]
pub struct TextField {
    state: Entity<InputState>,
    bordered: bool,
    text_size: Option<Pixels>,
}

impl TextField {
    /// A borderless field for `state`.
    pub fn new(state: &Entity<InputState>) -> Self {
        Self {
            state: state.clone(),
            bordered: false,
            text_size: None,
        }
    }

    /// Draws gpui-component's input frame (border, background, focus ring).
    pub fn bordered(mut self, bordered: bool) -> Self {
        self.bordered = bordered;
        self
    }

    /// Sets the text size (default: [`crate::TextSizes::row`]).
    pub fn text_size(mut self, size: Pixels) -> Self {
        self.text_size = Some(size);
        self
    }
}

impl RenderOnce for TextField {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let text_size = self.text_size.unwrap_or(theme.text.row);
        let input = Input::new(&self.state).text_size(text_size);
        // `Input::h` sizes multi-line inputs only; the style height applies here.
        if self.bordered {
            Styled::h(input, theme.metrics.button_height)
        } else {
            Styled::h(input.appearance(false), px(20.))
                .px(px(0.))
                .py(px(0.))
        }
    }
}
