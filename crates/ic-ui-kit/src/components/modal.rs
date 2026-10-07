//! Modal surfaces: a card over a dimmed window (the command palette, the
//! environment editor, confirmations).

use std::fmt;

use gpui::{
    AnyElement, App, BoxShadow, ElementId, InteractiveElement as _, IntoElement, MouseButton,
    MouseDownEvent, ParentElement as _, Pixels, RenderOnce, Role, ScrollHandle,
    StatefulInteractiveElement as _, Styled as _, Window, div, point, prelude::FluentBuilder as _,
    px,
};

use crate::theme::ActiveTheme as _;

type DismissHandler = Box<dyn Fn(&MouseDownEvent, &mut Window, &mut App) + 'static>;

/// Where a [`Modal`]'s card sits.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ModalPlacement {
    /// Hangs this far below the window's top edge (the command palette,
    /// 70px in the design).
    Top(Pixels),
    /// Centred in the window (dialogs).
    Center,
}

/// A card floating over the whole window, which is dimmed behind it. Put it
/// last in a `relative()` root so it paints over everything; menus opened
/// from inside it still float above it.
///
/// A press on the dimmed backdrop calls [`Modal::on_dismiss`]; presses
/// inside the card stay there (they never reach the elements behind).
#[derive(IntoElement)]
#[must_use = "a modal does nothing unless rendered"]
pub struct Modal {
    id: ElementId,
    content: AnyElement,
    width: Pixels,
    placement: ModalPlacement,
    on_dismiss: Option<DismissHandler>,
}

impl Modal {
    /// A centred card `width` wide holding `content`.
    pub fn new(id: impl Into<ElementId>, width: Pixels, content: impl IntoElement) -> Self {
        Self {
            id: id.into(),
            content: content.into_any_element(),
            width,
            placement: ModalPlacement::Center,
            on_dismiss: None,
        }
    }

    /// Places the card.
    pub fn placement(mut self, placement: ModalPlacement) -> Self {
        self.placement = placement;
        self
    }

    /// Runs `handler` on a press outside the card (close it there).
    pub fn on_dismiss(
        mut self,
        handler: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_dismiss = Some(Box::new(handler));
        self
    }

    /// The card's width.
    #[must_use]
    pub fn width(&self) -> Pixels {
        self.width
    }
}

impl fmt::Debug for Modal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Modal")
            .field("id", &self.id)
            .field("width", &self.width)
            .field("placement", &self.placement)
            .finish_non_exhaustive()
    }
}

impl RenderOnce for Modal {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let colors = theme.colors;
        // Never wider or taller than the window allows, with a margin.
        let viewport = window.viewport_size();
        let width = self.width.min(viewport.width - px(32.)).max(px(0.));
        let top = match self.placement {
            ModalPlacement::Top(top) => Some(top),
            ModalPlacement::Center => None,
        };
        let max_height = viewport.height - top.unwrap_or(px(32.)) - px(32.);
        let card = div()
            .id(self.id)
            .role(Role::Dialog)
            .occlude()
            .flex()
            .flex_col()
            .w(width)
            .max_h(max_height.max(px(120.)))
            .rounded(theme.metrics.modal_radius)
            .border_1()
            .border_color(colors.border_window)
            .bg(colors.window_background)
            .shadow(vec![BoxShadow {
                color: colors.shadow_modal,
                offset: point(px(0.), px(24.)),
                blur_radius: px(70.),
                spread_radius: px(0.),
                inset: false,
            }])
            .overflow_hidden()
            .font_family(theme.font_family.clone())
            .line_height(theme.line_height)
            .text_color(colors.text)
            // Presses inside the card stay in the card.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .child(self.content);
        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .occlude()
            .flex()
            .flex_col()
            .items_center()
            .bg(colors.backdrop)
            .map(|backdrop| match top {
                Some(top) => backdrop.justify_start().pt(top),
                None => backdrop.justify_center(),
            })
            .when_some(self.on_dismiss, |backdrop, handler| {
                backdrop.on_mouse_down(MouseButton::Left, move |event, window, cx| {
                    cx.stop_propagation();
                    handler(event, window, cx);
                })
            })
            .child(card)
    }
}

/// The standard layout inside a dialog [`Modal`]: a title row, the body
/// and a footer row of buttons, padded like the design's panes.
#[derive(IntoElement)]
#[must_use = "a dialog body does nothing unless rendered"]
pub struct DialogBody {
    title: AnyElement,
    body: Vec<AnyElement>,
    footer: Vec<AnyElement>,
    footer_start: Vec<AnyElement>,
    scroll: Option<ScrollHandle>,
}

impl DialogBody {
    /// A dialog titled `title`.
    pub fn new(title: impl IntoElement) -> Self {
        Self {
            title: title.into_any_element(),
            body: Vec::new(),
            footer: Vec::new(),
            footer_start: Vec::new(),
            scroll: None,
        }
    }

    /// Lets `handle` scroll the body: its items are the blocks, in the
    /// order they were added (`ScrollHandle::scroll_to_item` brings one
    /// into view, say an answer that appeared under the visible part).
    pub fn track_scroll(mut self, handle: &ScrollHandle) -> Self {
        self.scroll = Some(handle.clone());
        self
    }

    /// Adds a block to the body.
    pub fn child(mut self, element: impl IntoElement) -> Self {
        self.body.push(element.into_any_element());
        self
    }

    /// Adds a button at the footer's right end (in order: the last is the
    /// rightmost, the main action).
    pub fn action(mut self, element: impl IntoElement) -> Self {
        self.footer.push(element.into_any_element());
        self
    }

    /// Adds an element at the footer's left end (a destructive action, a
    /// status line).
    pub fn footer_start(mut self, element: impl IntoElement) -> Self {
        self.footer_start.push(element.into_any_element());
        self
    }
}

impl fmt::Debug for DialogBody {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DialogBody")
            .field("blocks", &self.body.len())
            .field("actions", &self.footer.len())
            .finish_non_exhaustive()
    }
}

impl RenderOnce for DialogBody {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let has_footer = !self.footer.is_empty() || !self.footer_start.is_empty();
        div()
            .flex()
            .flex_col()
            .min_h_0()
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(10.))
                    .h(theme.metrics.header_height)
                    .px(theme.metrics.pane_padding)
                    .border_b_1()
                    .border_color(colors.border_header)
                    .text_size(theme.text.heading)
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(colors.text_strong)
                    .child(self.title),
            )
            .child(
                div()
                    .id("dialog-body")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .gap(px(16.))
                    .px(theme.metrics.pane_padding)
                    .py(px(18.))
                    .overflow_y_scroll()
                    .when_some(self.scroll, |body, handle| body.track_scroll(&handle))
                    .text_size(theme.text.body)
                    .children(self.body),
            )
            .when(has_footer, |dialog| {
                dialog.child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(px(8.))
                        .px(theme.metrics.pane_padding)
                        .py(px(12.))
                        .border_t_1()
                        .border_color(colors.border_header)
                        .text_size(theme.text.small)
                        .children(self.footer_start)
                        .child(div().flex_1())
                        .children(self.footer),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modals_are_centred_unless_placed() {
        let modal = Modal::new("palette", px(640.), div());
        assert_eq!(modal.placement, ModalPlacement::Center);
        assert_eq!(modal.width(), px(640.));
        let top = modal.placement(ModalPlacement::Top(px(70.)));
        assert_eq!(top.placement, ModalPlacement::Top(px(70.)));
        assert!(format!("{top:?}").contains("palette"));
    }

    #[test]
    fn dialog_bodies_collect_blocks_and_actions() {
        let body = DialogBody::new("Delete group")
            .child(div())
            .action(div())
            .action(div())
            .footer_start(div());
        assert_eq!(body.body.len(), 1);
        assert_eq!(body.footer.len(), 2);
        assert_eq!(body.footer_start.len(), 1);
    }
}
