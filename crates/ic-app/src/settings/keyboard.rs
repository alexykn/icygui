//! The settings panel from the keyboard: Tab and Shift-Tab go through the
//! search, every control of the page in order (switches, segmented
//! controls, chips, dropdowns, buttons and text fields) and the header's
//! buttons, then round again; the field left applies.
//!
//! On a control, Space or Enter acts as a click (a switch or chip
//! toggles, a button runs, a dropdown opens) and ← → move a segmented
//! control or a dropdown to the previous or next choice. The control with
//! the keyboard has an accent ring while the keyboard is in use (as
//! `:focus-visible` does), drawn outside it so nothing moves.
//!
//! The controls are the kit's own; the panel wraps each in a focusable slot
//! and records the slots in the order it draws them, with where they were
//! drawn, so a Tab to a control out of view scrolls it in.

use std::rc::Rc;

use gpui::{
    Action, AnyElement, App, Bounds, Context, ElementId, FocusHandle, Focusable as _,
    InteractiveElement as _, IntoElement, ParentElement as _, Pixels, SharedString, Styled as _,
    Window, canvas, div, prelude::FluentBuilder as _,
};
use ic_ui_kit::{Theme, px};

use super::SettingsPanel;

/// Key context of a control slot.
pub(crate) const CONTROL_CONTEXT: &str = "SettingsControl";

/// Space or Enter on a control: as a click.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct ControlActivate;

/// → on a control: the next choice.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct ControlNext;

/// ← on a control: the previous choice.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct ControlPrevious;

/// What a control does when activated from the keyboard.
pub(crate) type Activate = Rc<dyn Fn(&mut SettingsPanel, &mut Window, &mut Context<SettingsPanel>)>;

/// What a control does on ← (false) and → (true).
pub(crate) type Step =
    Rc<dyn Fn(&mut SettingsPanel, bool, &mut Window, &mut Context<SettingsPanel>)>;

/// Where a stop is, for the order Tab goes in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Region {
    /// The search field.
    Search,
    /// The page (or the search's results).
    Page,
    /// The header's buttons.
    Header,
}

/// A place Tab stops at.
#[derive(Clone)]
pub(crate) struct Stop {
    /// Names it (a control's id; a field's).
    pub(crate) key: SharedString,
    pub(crate) handle: FocusHandle,
    pub(crate) region: Region,
}

impl SettingsPanel {
    /// The focus handle of control `key`, the same in every frame.
    fn control_handle(&self, key: &SharedString, cx: &App) -> FocusHandle {
        self.control_handles
            .borrow_mut()
            .entry(key.clone())
            .or_insert_with(|| cx.focus_handle())
            .clone()
    }

    /// Records a stop drawn in this frame.
    pub(super) fn add_stop(&self, key: SharedString, handle: FocusHandle, region: Region) {
        self.stops.borrow_mut().push(Stop {
            key,
            handle,
            region,
        });
    }

    /// Something that records where `key` is drawn, for scrolling a stop
    /// into view.
    pub(super) fn bounds_probe(&self, key: SharedString) -> AnyElement {
        let bounds = self.stop_bounds.clone();
        canvas(
            move |drawn: Bounds<Pixels>, _, _| {
                bounds.borrow_mut().insert(key, drawn);
            },
            |_, (), _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .into_any_element()
    }

    /// `control` in a slot Tab reaches: Space and Enter `activate` it, ←
    /// and → `step` it (when it has choices).
    pub(super) fn focusable(
        &self,
        key: impl Into<SharedString>,
        control: impl IntoElement,
        activate: Activate,
        step: Option<Step>,
        theme: &Theme,
        cx: &Context<Self>,
    ) -> AnyElement {
        self.focusable_in(key, control, activate, step, Region::Page, theme, cx)
    }

    /// [`SettingsPanel::focusable`] in `region`.
    #[expect(
        clippy::too_many_arguments,
        reason = "the slot's parts; a struct would only rename them"
    )]
    pub(super) fn focusable_in(
        &self,
        key: impl Into<SharedString>,
        control: impl IntoElement,
        activate: Activate,
        step: Option<Step>,
        region: Region,
        theme: &Theme,
        cx: &Context<Self>,
    ) -> AnyElement {
        let key = key.into();
        let handle = self.control_handle(&key, cx);
        self.add_stop(key.clone(), handle.clone(), region);
        let ring = self.keyboard_on.borrow().as_ref() == Some(&key);
        div()
            .id(ElementId::Name(format!("focus-{key}").into()))
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .track_focus(&handle)
            .key_context(CONTROL_CONTEXT)
            .on_action(cx.listener(move |this, _: &ControlActivate, window, cx| {
                activate(this, window, cx);
            }))
            .when_some(step, |slot, step| {
                let back = step.clone();
                slot.on_action(cx.listener(move |this, _: &ControlNext, window, cx| {
                    step(this, true, window, cx);
                }))
                .on_action(cx.listener(
                    move |this, _: &ControlPrevious, window, cx| {
                        back(this, false, window, cx);
                    },
                ))
            })
            .child(control)
            // The ring sits outside the control, over the layout, so
            // nothing moves.
            .when(ring, |slot| {
                slot.child(
                    div()
                        .absolute()
                        .top(px(-3.))
                        .left(px(-3.))
                        .right(px(-3.))
                        .bottom(px(-3.))
                        .rounded(theme.metrics.code_radius + px(2.))
                        .border_1()
                        .border_color(theme.colors.accent),
                )
            })
            .child(self.bounds_probe(key))
            .into_any_element()
    }

    /// Notes which control has the keyboard, for its ring: only while the
    /// keyboard is in use (as `:focus-visible`), before the page draws.
    pub(super) fn note_keyboard_focus(&self, window: &Window) {
        let on = window
            .last_input_was_keyboard()
            .then(|| {
                self.control_handles
                    .borrow()
                    .iter()
                    .find(|(_, handle)| handle.is_focused(window))
                    .map(|(key, _)| key.clone())
            })
            .flatten();
        *self.keyboard_on.borrow_mut() = on;
    }

    /// The stops drawn last, in the order Tab goes through them.
    fn ordered_stops(&self) -> Vec<Stop> {
        let mut stops = self.stops.borrow().clone();
        stops.sort_by_key(|stop| stop.region);
        stops
    }

    /// Tab (`forward`) or Shift-Tab: the field being left applies, the
    /// next stop gets the keyboard and is scrolled into view.
    pub(super) fn move_focus(
        &mut self,
        forward: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The field left applies, whether it is still drawn or not (a
        // search result's field after the search changed).
        if let Some(id) = self
            .inputs
            .iter()
            .find(|(_, input)| input.focus_handle(cx).is_focused(window))
            .map(|(id, _)| id.clone())
        {
            self.commit(&id, window, cx);
        }
        let stops = self.ordered_stops();
        if stops.is_empty() {
            return;
        }
        let count = stops.len();
        let current = stops.iter().position(|stop| stop.handle.is_focused(window));
        let next = match (current, forward) {
            (Some(index), true) => (index + 1) % count,
            (Some(index), false) => (index + count - 1) % count,
            // From the navigation (or nowhere): the page's first stop.
            (None, true) => self.first_page_stop(&stops).unwrap_or(0),
            (None, false) => count - 1,
        };
        let target = stops[next].clone();
        window.focus(&target.handle, cx);
        if target.region == Region::Page {
            self.reveal(&target.key);
        }
        cx.notify();
    }

    /// The first stop of the page, after the search.
    fn first_page_stop(&self, stops: &[Stop]) -> Option<usize> {
        let _ = self;
        stops.iter().position(|stop| stop.region == Region::Page)
    }

    /// Gives the page's first stop the keyboard (Enter in the navigation).
    pub(super) fn focus_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let stops = self.ordered_stops();
        match self.first_page_stop(&stops) {
            Some(index) => {
                let target = stops[index].clone();
                window.focus(&target.handle, cx);
                self.reveal(&target.key);
            }
            None => window.focus(&self.focus_handle, cx),
        }
        cx.notify();
    }

    /// Scrolls the page so that stop `key` is in view.
    fn reveal(&self, key: &SharedString) {
        let Some(bounds) = self.stop_bounds.borrow().get(key).copied() else {
            return;
        };
        let view = self.scroll.bounds();
        let margin = px(24.);
        let mut offset = self.scroll.offset();
        if bounds.bottom() > view.bottom() - margin {
            offset.y -= bounds.bottom() - (view.bottom() - margin);
        } else if bounds.top() < view.top() + margin {
            offset.y += view.top() + margin - bounds.top();
        } else {
            return;
        }
        let max = self.scroll.max_offset().y;
        offset.y = offset.y.min(px(0.)).max(-max);
        self.scroll.set_offset(offset);
    }

    /// Where stop `key` was drawn last, for tests.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn drawn_bounds(&self, key: &str) -> Option<Bounds<Pixels>> {
        self.stop_bounds
            .borrow()
            .get(&SharedString::from(key.to_owned()))
            .copied()
    }

    /// Whether the keyboard is on control `key`, for tests.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn has_keyboard(&self, key: &str, window: &Window) -> bool {
        self.control_handles
            .borrow()
            .get(&SharedString::from(key.to_owned()))
            .is_some_and(|handle| handle.is_focused(window))
    }
}
