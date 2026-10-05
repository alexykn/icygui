//! Tooltips: a short label with an optional key hint, shown by GPUI after a
//! hover delay.

use gpui::{
    AnyView, App, AppContext as _, BoxShadow, Context, IntoElement, ParentElement as _, Render,
    SharedString, Styled as _, Window, div, hsla, point, prelude::FluentBuilder as _, px,
};

use crate::theme::ActiveTheme as _;

/// A tooltip's content. Attach it with GPUI's `.tooltip(...)` on any
/// interactive element:
///
/// ```text
/// div().id("new").tooltip(Tooltip::text("New dashboard"))
/// ```
#[derive(Clone, Debug)]
pub struct Tooltip {
    title: SharedString,
    key: Option<SharedString>,
}

impl Tooltip {
    /// A tooltip with a title.
    pub fn new(title: impl Into<SharedString>) -> Self {
        Self {
            title: title.into(),
            key: None,
        }
    }

    /// Adds the key that runs the same command (`⌘B`, `ctrl-b`).
    #[must_use]
    pub fn key(mut self, key: impl Into<SharedString>) -> Self {
        self.key = Some(key.into());
        self
    }

    /// The tooltip's title; buttons also use it as their accessible label.
    #[must_use]
    pub fn title(&self) -> &SharedString {
        &self.title
    }

    /// A builder for GPUI's `.tooltip(...)` that shows `title`.
    pub fn text(
        title: impl Into<SharedString>,
    ) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
        Self::new(title).builder()
    }

    /// A builder for GPUI's `.tooltip(...)` that shows this tooltip.
    pub fn builder(self) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
        move |_window, cx| cx.new(|_| self.clone()).into()
    }
}

impl Render for Tooltip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let card = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(8.))
            .py(px(4.))
            .rounded(theme.metrics.small_radius)
            .border_1()
            .border_color(colors.border_window)
            .bg(colors.element_background)
            .shadow(vec![BoxShadow {
                color: hsla(0., 0., 0., 0.35),
                offset: point(px(0.), px(2.)),
                blur_radius: px(8.),
                spread_radius: px(0.),
                inset: false,
            }])
            .font_family(theme.font_family.clone())
            .text_size(theme.text.label)
            .text_color(colors.text)
            .whitespace_nowrap()
            .child(self.title.clone())
            .when_some(self.key.clone(), |tooltip, key| {
                tooltip.child(div().text_color(colors.text_faint).child(key))
            });
        // GPUI puts the tooltip's corner at the mouse; the transparent
        // padding keeps the card clear of the pointer.
        div().pt(px(18.)).pl(px(4.)).child(card)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_title_and_key() {
        let tooltip = Tooltip::new("Toggle sidebar").key("ctrl-b");
        assert_eq!(tooltip.title, "Toggle sidebar");
        assert_eq!(tooltip.key.as_deref(), Some("ctrl-b"));
    }
}
