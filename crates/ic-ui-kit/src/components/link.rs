//! Clickable text: links to other objects (`on db-prod-03`) and quiet text
//! buttons (`↗ open as tab`, `+ 16 more ok`).

use std::fmt;

use gpui::{
    App, ClickEvent, ElementId, Hsla, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, Pixels, RenderOnce, Role, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, div, prelude::FluentBuilder as _,
};

use crate::components::Tooltip;
use crate::theme::{ActiveTheme as _, Theme};

type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// How a [`Link`] looks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum LinkStyle {
    /// Accent coloured, like the design's `<a>`.
    #[default]
    Accent,
    /// Faint text that brightens on hover: secondary commands in headers
    /// and lists.
    Quiet,
}

impl LinkStyle {
    /// Colour at rest and under the mouse.
    #[must_use]
    pub fn colors(self, theme: &Theme) -> (Hsla, Hsla) {
        match self {
            Self::Accent => (theme.colors.accent, theme.colors.accent_hover),
            Self::Quiet => (theme.colors.text_faint, theme.colors.text),
        }
    }
}

/// Inline clickable text.
#[derive(IntoElement)]
#[must_use = "a link does nothing unless rendered"]
pub struct Link {
    id: ElementId,
    label: SharedString,
    style: LinkStyle,
    text_size: Option<Pixels>,
    tooltip: Option<Tooltip>,
    on_click: Option<ClickHandler>,
}

impl Link {
    /// An accent link.
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            style: LinkStyle::Accent,
            text_size: None,
            tooltip: None,
            on_click: None,
        }
    }

    /// Uses the quiet style.
    pub fn quiet(mut self) -> Self {
        self.style = LinkStyle::Quiet;
        self
    }

    /// Sets the text size (default: inherited).
    pub fn text_size(mut self, size: Pixels) -> Self {
        self.text_size = Some(size);
        self
    }

    /// Shows a tooltip on hover (where the link goes).
    pub fn tooltip(mut self, tooltip: Tooltip) -> Self {
        self.tooltip = Some(tooltip);
        self
    }

    /// Runs `handler` when the link is clicked.
    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

impl fmt::Debug for Link {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Link")
            .field("id", &self.id)
            .field("label", &self.label)
            .field("style", &self.style)
            .finish_non_exhaustive()
    }
}

impl RenderOnce for Link {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let (color, hover) = self.style.colors(cx.theme());
        div()
            .id(self.id)
            .role(Role::Link)
            .aria_label(self.label.clone())
            .flex_none()
            .whitespace_nowrap()
            .text_color(color)
            .when_some(self.text_size, gpui::Styled::text_size)
            .cursor_pointer()
            .hover(move |style| style.text_color(hover))
            .child(self.label)
            .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            .when_some(self.tooltip, |link, tooltip| {
                link.tooltip(tooltip.builder())
            })
            .when_some(self.on_click, |link, handler| {
                link.on_click(move |event, window, cx| {
                    cx.stop_propagation();
                    handler(event, window, cx);
                })
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn styles_brighten_on_hover() {
        let theme = Theme::dark();
        let (rest, hover) = LinkStyle::Accent.colors(&theme);
        assert_eq!(rest, theme.colors.accent);
        assert_eq!(hover, theme.colors.accent_hover);
        let (rest, hover) = LinkStyle::Quiet.colors(&theme);
        assert_eq!(rest, theme.colors.text_faint);
        assert!(hover.l > rest.l);
    }

    #[test]
    fn links_record_their_options() {
        let link = Link::new("host", "db-prod-03")
            .quiet()
            .on_click(|_, _, _| {});
        assert_eq!(link.style, LinkStyle::Quiet);
        assert!(link.on_click.is_some());
        assert!(format!("{link:?}").contains("db-prod-03"));
    }
}
