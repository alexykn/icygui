//! Single-line text fields: gpui-component's input, styled like the design.

use crate::px;
use gpui::{
    App, Entity, Focusable as _, IntoElement, ParentElement as _, Pixels, RenderOnce, Styled,
    Window, div,
};
use gpui_component::input::{Input, InputState};

use crate::components::form::field_border;
use crate::theme::ActiveTheme as _;

/// A single-line text field bound to an [`InputState`] (create the state with
/// `cx.new(|cx| InputState::new(window, cx).placeholder("…"))` and subscribe
/// to its `InputEvent`s).
///
/// Borderless by default, like the sidebar's search field; text and
/// placeholder colours come from the theme. [`TextField::bordered`] frames
/// it like the design's inspector fields: 30px high on the code surface,
/// with an accent border while focused and a critical one while
/// [`TextField::invalid`].
#[derive(Clone, Debug, IntoElement)]
#[must_use = "a text field does nothing unless rendered"]
pub struct TextField {
    state: Entity<InputState>,
    bordered: bool,
    invalid: bool,
    text_size: Option<Pixels>,
}

impl TextField {
    /// A borderless field for `state`.
    pub fn new(state: &Entity<InputState>) -> Self {
        Self {
            state: state.clone(),
            bordered: false,
            invalid: false,
            text_size: None,
        }
    }

    /// Draws a frame (border, code-surface background, focus colour).
    pub fn bordered(mut self, bordered: bool) -> Self {
        self.bordered = bordered;
        self
    }

    /// Marks the content as invalid (a critical border when framed).
    pub fn invalid(mut self, invalid: bool) -> Self {
        self.invalid = invalid;
        self
    }

    /// Sets the text size (default: [`crate::TextSizes::row`]).
    pub fn text_size(mut self, size: Pixels) -> Self {
        self.text_size = Some(size);
        self
    }
}

impl RenderOnce for TextField {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let focused = self.state.focus_handle(cx).is_focused(window);
        let theme = cx.theme();
        let text_size = self.text_size.unwrap_or(if self.bordered {
            theme.text.body
        } else {
            theme.text.row
        });
        let input = Input::new(&self.state).text_size(text_size);
        // `Input::h` sizes multi-line inputs only; the style height applies here.
        let input = Styled::h(input.appearance(false), px(20.))
            .px(px(0.))
            .py(px(0.));
        if !self.bordered {
            return div().flex_1().min_w_0().child(input);
        }
        div()
            .flex()
            .items_center()
            .min_w_0()
            .h(theme.metrics.field_height)
            .px(px(10.))
            .rounded(theme.metrics.code_radius)
            .border_1()
            .border_color(field_border(theme, focused, self.invalid))
            .bg(theme.colors.code_background)
            .text_color(theme.colors.text)
            .child(input)
    }
}
