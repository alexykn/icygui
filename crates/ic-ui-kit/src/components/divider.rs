//! One-pixel rules.

use crate::px;
use gpui::{App, Axis, Hsla, IntoElement, Pixels, RenderOnce, Styled as _, Window, div};

use crate::theme::{ActiveTheme as _, Colors};

/// Which of the design's border colours a divider uses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum DividerColor {
    /// Window outline (`#33383c`), also the divider beside the traffic lights.
    Window,
    /// Splits between sidebar, list and pane (`#2e3337`).
    Split,
    /// Rules under header bars (`#2a2f33`).
    #[default]
    Header,
    /// Rules between rows (`#262a2e`).
    Row,
}

impl DividerColor {
    /// The colour in `colors`.
    #[must_use]
    pub fn resolve(self, colors: &Colors) -> Hsla {
        match self {
            Self::Window => colors.border_window,
            Self::Split => colors.border_split,
            Self::Header => colors.border_header,
            Self::Row => colors.border_row,
        }
    }
}

/// A one-pixel horizontal or vertical rule.
#[derive(Clone, Copy, Debug, IntoElement)]
#[must_use = "a divider does nothing unless rendered"]
pub struct Divider {
    axis: Axis,
    color: DividerColor,
    length: Option<Pixels>,
    margin: Pixels,
}

impl Divider {
    /// A rule across the full width.
    pub fn horizontal() -> Self {
        Self {
            axis: Axis::Horizontal,
            color: DividerColor::default(),
            length: None,
            margin: px(0.),
        }
    }

    /// A rule across the full height.
    pub fn vertical() -> Self {
        Self {
            axis: Axis::Vertical,
            ..Self::horizontal()
        }
    }

    /// Sets the colour.
    pub fn color(mut self, color: DividerColor) -> Self {
        self.color = color;
        self
    }

    /// Sets a fixed length instead of spanning the parent.
    pub fn length(mut self, length: Pixels) -> Self {
        self.length = Some(length);
        self
    }

    /// Adds space on both sides, across the rule.
    pub fn margin(mut self, margin: Pixels) -> Self {
        self.margin = margin;
        self
    }
}

impl RenderOnce for Divider {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let color = self.color.resolve(&cx.theme().colors);
        let rule = div().flex_none().bg(color);
        match self.axis {
            Axis::Horizontal => {
                let rule = rule.h(px(1.)).my(self.margin);
                match self.length {
                    Some(length) => rule.w(length),
                    None => rule.w_full(),
                }
            }
            Axis::Vertical => {
                let rule = rule.w(px(1.)).mx(self.margin);
                match self.length {
                    Some(length) => rule.h(length),
                    None => rule.h_full(),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Theme;

    #[test]
    fn colours_follow_the_border_tokens() {
        let colors = Theme::dark().colors;
        assert_eq!(DividerColor::Window.resolve(&colors), colors.border_window);
        assert_eq!(DividerColor::Split.resolve(&colors), colors.border_split);
        assert_eq!(DividerColor::Header.resolve(&colors), colors.border_header);
        assert_eq!(DividerColor::Row.resolve(&colors), colors.border_row);
    }

    #[test]
    fn vertical_dividers_keep_their_options() {
        let divider = Divider::vertical()
            .color(DividerColor::Window)
            .length(px(18.))
            .margin(px(6.));
        assert_eq!(divider.axis, Axis::Vertical);
        assert_eq!(divider.length, Some(px(18.)));
        assert_eq!(divider.margin, px(6.));
    }
}
