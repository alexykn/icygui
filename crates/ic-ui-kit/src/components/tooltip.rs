//! Tooltips: a short label with an optional key hint, shown by GPUI after a
//! hover delay, at the pointer or ([`TooltipPlacement::Left`]) beside the
//! element they belong to.

use std::time::Duration;

use crate::px;
use gpui::{
    AnyView, App, AppContext as _, BoxShadow, Context, Div, ElementId, Entity, EntityId,
    IntoElement, ParentElement as _, Render, SharedString, Styled as _, Window, div, point,
    prelude::FluentBuilder as _,
};

use crate::components::float::{Float, Side};
use crate::theme::ActiveTheme as _;

/// How long the pointer rests on an element before its tooltip shows
/// (GPUI's own delay, for tooltips placed beside their element).
const DELAY: Duration = Duration::from_millis(500);

/// Where a tooltip shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TooltipPlacement {
    /// At the pointer (GPUI's tooltips).
    #[default]
    Pointer,
    /// Left of its element, centred on it, 8px away: so it never covers
    /// what is under or beside the pointer (a select's disabled row, whose
    /// reason would otherwise cover the next row).
    Left,
}

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
    placement: TooltipPlacement,
}

impl Tooltip {
    /// A tooltip with a title.
    pub fn new(title: impl Into<SharedString>) -> Self {
        Self {
            title: title.into(),
            key: None,
            placement: TooltipPlacement::Pointer,
        }
    }

    /// Shows it at `placement` instead of at the pointer (elements that
    /// support it: menu items).
    #[must_use]
    pub fn placement(mut self, placement: TooltipPlacement) -> Self {
        self.placement = placement;
        self
    }

    /// Where it shows.
    #[must_use]
    pub fn placed(&self) -> TooltipPlacement {
        self.placement
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

impl Tooltip {
    /// The tooltip's card.
    fn card(&self, cx: &App) -> Div {
        let theme = cx.theme();
        let colors = theme.colors;
        div()
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
                color: colors.shadow,
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
            })
    }
}

impl Render for Tooltip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let card = self.card(cx);
        // GPUI puts the tooltip's corner at the mouse; the transparent
        // padding keeps the card clear of the pointer.
        div().pt(px(18.)).pl(px(4.)).child(card)
    }
}

/// Whether a tooltip placed beside its element shows (the pointer has
/// rested on the element long enough).
#[derive(Debug, Default)]
pub(crate) struct BesideState {
    shown: bool,
    /// Bumped on every hover change, so a stale delay does nothing.
    epoch: u64,
}

/// A tooltip placed beside its element ([`TooltipPlacement::Left`]): the
/// element calls [`Beside::hover`] from its hover listener and adds
/// [`Beside::float`] as a child (it must be `relative()`).
pub(crate) struct Beside;

impl Beside {
    /// The state of the tooltip of the element keyed `key`.
    pub(crate) fn state(key: ElementId, window: &mut Window, cx: &mut App) -> Entity<BesideState> {
        window.use_keyed_state(key, cx, |_, _| BesideState::default())
    }

    /// The pointer entered (`true`) or left the element: shows the tooltip
    /// after the delay, or hides it; redraws `view`.
    pub(crate) fn hover(
        state: &Entity<BesideState>,
        hovered: bool,
        view: EntityId,
        window: &mut Window,
        cx: &mut App,
    ) {
        let epoch = state.update(cx, |state, _| {
            state.epoch += 1;
            state.shown = false;
            state.epoch
        });
        cx.notify(view);
        if !hovered {
            return;
        }
        let state = state.clone();
        window
            .spawn(cx, async move |cx| {
                cx.background_executor().timer(DELAY).await;
                cx.update(|_, cx| {
                    let current = state.update(cx, |state, _| {
                        let current = state.epoch == epoch;
                        if current {
                            state.shown = true;
                        }
                        current
                    });
                    if current {
                        cx.notify(view);
                    }
                })
                .ok();
            })
            .detach();
    }

    /// The tooltip beside its element, while shown.
    pub(crate) fn float(
        state: &Entity<BesideState>,
        tooltip: &Tooltip,
        cx: &App,
    ) -> Option<impl IntoElement + use<>> {
        if !state.read(cx).shown {
            return None;
        }
        let side = match tooltip.placement {
            TooltipPlacement::Left | TooltipPlacement::Pointer => Side::Left,
        };
        Some(
            Float::new(side)
                .gap(px(8.))
                .child(tooltip.card(cx).flex_none()),
        )
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
        assert_eq!(tooltip.placed(), TooltipPlacement::Pointer);
        let left = tooltip.placement(TooltipPlacement::Left);
        assert_eq!(left.placed(), TooltipPlacement::Left);
    }
}
