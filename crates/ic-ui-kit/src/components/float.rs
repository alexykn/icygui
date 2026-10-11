//! Placing a floating card (a menu, a select's list, a popover, a
//! tooltip) by the element it belongs to.
//!
//! [`Float`] is an empty box laid over its parent (which must be
//! `relative()`): it takes the parent's bounds, lays its content out inside
//! them (so `w_full()` content is exactly as wide as the parent, as a
//! select's list is as wide as its field), then moves the content beside
//! the parent and draws it on top of everything else. It can also report
//! the parent's bounds each frame, for decisions made while rendering (a
//! select opening upward).

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    AlignItems, AnyElement, App, Bounds, Display, Edges, Element, ElementId, Global,
    GlobalElementId, InspectorElementId, IntoElement, LayoutId, Length, Pixels, Point, Position,
    Style, Window, point,
};

/// Where a [`Float`]'s content goes, relative to the parent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Side {
    /// Under the parent; above it when there is no room below and more
    /// above.
    Below,
    /// Above the parent; under it when there is no room above and more
    /// below.
    Above,
    /// Exactly under the parent, never moved to the other side (a
    /// select's list, which has decided its side already).
    StrictlyBelow,
    /// Exactly above the parent.
    StrictlyAbove,
    /// Left of the parent, centred on it vertically.
    Left,
}

/// The parent's bounds, as last laid out (shared with whoever renders).
pub(crate) type Measured = Rc<Cell<Option<Bounds<Pixels>>>>;

/// What kind of card a [`Float`] placed, for [`last_placed`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FloatKind {
    /// A select's list ([`crate::Select`]).
    SelectList,
    /// A menu or popover hung from its trigger ([`crate::Popover`]).
    Popover,
}

/// Where a card was drawn, in window coordinates: the element it belongs
/// to and the card.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placed {
    /// The trigger (a select's field, a menu's button).
    pub trigger: Bounds<Pixels>,
    /// The card (a select's list, the menu).
    pub content: Bounds<Pixels>,
}

/// The last card of each kind drawn.
#[derive(Default)]
struct LastPlaced([Option<Placed>; 2]);

impl Global for LastPlaced {}

/// Where the last card of `kind` was drawn: for tests of the layout (a
/// list as wide as its field, a menu 4px under its trigger).
#[must_use]
pub fn last_placed(kind: FloatKind, cx: &App) -> Option<Placed> {
    cx.try_global::<LastPlaced>()
        .and_then(|placed| placed.0[kind as usize])
}

/// See the module notes.
pub(crate) struct Float {
    child: Option<AnyElement>,
    side: Side,
    align_right: bool,
    gap: Pixels,
    margin: Pixels,
    outset: Point<Pixels>,
    report: Option<Measured>,
    kind: Option<FloatKind>,
}

impl Float {
    /// A float with nothing in it yet (it still reports, see
    /// [`Float::report`]).
    pub(crate) fn new(side: Side) -> Self {
        Self {
            child: None,
            side,
            align_right: false,
            gap: Pixels::ZERO,
            margin: crate::px(8.),
            outset: Point::default(),
            report: None,
            kind: None,
        }
    }

    /// The content to float.
    pub(crate) fn child(mut self, child: impl IntoElement) -> Self {
        self.child = Some(child.into_any_element());
        self
    }

    /// Lines the content's right edge up with the parent's (below or
    /// above it).
    pub(crate) fn align_right(mut self, right: bool) -> Self {
        self.align_right = right;
        self
    }

    /// Space between the parent and the content.
    pub(crate) fn gap(mut self, gap: Pixels) -> Self {
        self.gap = gap;
        self
    }

    /// Places the content as if the parent reached `outset` further on
    /// each side (a trigger whose pressed background reaches past its
    /// layout box).
    pub(crate) fn outset(mut self, outset: Point<Pixels>) -> Self {
        self.outset = outset;
        self
    }

    /// Notes where it draws its content, as `kind` ([`last_placed`]).
    pub(crate) fn kind(mut self, kind: FloatKind) -> Self {
        self.kind = Some(kind);
        self
    }

    /// Writes the parent's bounds into `cell` each frame.
    pub(crate) fn report(mut self, cell: Measured) -> Self {
        self.report = Some(cell);
        self
    }
}

/// Where content of `size` goes beside `parent` in a window `viewport`
/// wide and high (keeping `margin` from its edges): the content's origin.
#[must_use]
pub(crate) fn place(
    parent: Bounds<Pixels>,
    card: gpui::Size<Pixels>,
    viewport: gpui::Size<Pixels>,
    side: Side,
    align_right: bool,
    gap: Pixels,
    margin: Pixels,
) -> Point<Pixels> {
    let below = parent.bottom() + gap;
    let above = parent.top() - gap - card.height;
    let fits_below = below + card.height <= viewport.height - margin;
    let fits_above = above >= margin;
    let room_below = viewport.height - margin - below;
    let room_above = parent.top() - gap - margin;
    let mut y = match side {
        Side::Below if fits_below || (!fits_above && room_below >= room_above) => below,
        Side::Above if !fits_above && (fits_below || room_below > room_above) => below,
        Side::StrictlyBelow => below,
        Side::Below | Side::Above | Side::StrictlyAbove => above,
        Side::Left => parent.top() + (parent.size.height - card.height) / 2.,
    };
    let mut x = match side {
        Side::Left => parent.left() - gap - card.width,
        _ if align_right => parent.right() - card.width,
        _ => parent.left(),
    };
    if !matches!(side, Side::StrictlyBelow | Side::StrictlyAbove) {
        x = x.min(viewport.width - margin - card.width).max(margin);
    }
    if side == Side::Left {
        y = y.min(viewport.height - margin - card.height).max(margin);
    }
    point(x, y)
}

impl IntoElement for Float {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Float {
    type RequestLayoutState = Option<LayoutId>;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let child = self
            .child
            .as_mut()
            .map(|child| child.request_layout(window, cx));
        let zero = Length::from(Pixels::ZERO);
        let style = Style {
            position: Position::Absolute,
            inset: Edges {
                top: zero,
                right: zero,
                bottom: zero,
                left: zero,
            },
            display: Display::Flex,
            align_items: Some(AlignItems::FlexStart),
            ..Style::default()
        };
        let layout = window.request_layout(style, child, cx);
        (layout, child)
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        child_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(report) = &self.report {
            report.set(Some(bounds));
        }
        let (Some(child), Some(layout)) = (self.child.take(), *child_layout) else {
            return;
        };
        let laid_out = window.layout_bounds(layout);
        let parent = Bounds {
            origin: bounds.origin - self.outset,
            size: gpui::size(
                bounds.size.width + self.outset.x * 2.,
                bounds.size.height + self.outset.y * 2.,
            ),
        };
        let origin = place(
            parent,
            laid_out.size,
            window.viewport_size(),
            self.side,
            self.align_right,
            self.gap,
            self.margin,
        );
        if let Some(kind) = self.kind {
            cx.default_global::<LastPlaced>().0[kind as usize] = Some(Placed {
                trigger: parent,
                content: Bounds {
                    origin,
                    size: laid_out.size,
                },
            });
        }
        let delta = origin - laid_out.origin;
        let offset = window.element_offset() + point(delta.x.round(), delta.y.round());
        window.defer_draw(child, offset, 1, None);
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        _window: &mut Window,
        _cx: &mut App,
    ) {
    }
}

#[cfg(test)]
#[expect(clippy::disallowed_methods, reason = "placement works in real pixels")]
mod tests {
    use gpui::{Bounds, point, px, size};

    use super::*;

    fn field() -> Bounds<Pixels> {
        Bounds::new(point(px(100.), px(100.)), size(px(200.), px(30.)))
    }

    #[test]
    fn menus_hang_below_or_stand_above_when_there_is_no_room() {
        let viewport = size(px(1000.), px(800.));
        let menu = size(px(220.), px(200.));
        let below = place(field(), menu, viewport, Side::Below, false, px(4.), px(8.));
        assert_eq!(below, point(px(100.), px(134.)));
        let right = place(field(), menu, viewport, Side::Below, true, px(4.), px(8.));
        assert_eq!(right.x, px(80.));
        let low = Bounds::new(point(px(100.), px(700.)), size(px(200.), px(30.)));
        let flipped = place(low, menu, viewport, Side::Below, false, px(4.), px(8.));
        assert_eq!(flipped.y, px(496.), "4px above the trigger");
    }

    #[test]
    fn strict_sides_never_flip_and_left_is_beside() {
        let viewport = size(px(1000.), px(200.));
        let list = size(px(200.), px(300.));
        let strict = place(
            field(),
            list,
            viewport,
            Side::StrictlyBelow,
            false,
            px(0.),
            px(8.),
        );
        assert_eq!(strict, point(px(100.), px(130.)));
        let tip = size(px(80.), px(20.));
        let left = place(field(), tip, viewport, Side::Left, false, px(8.), px(8.));
        assert_eq!(left, point(px(12.), px(105.)));
    }
}
