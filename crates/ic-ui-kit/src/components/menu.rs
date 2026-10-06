//! Popup menus (the list header's sort and `···` menus), the popover that
//! places them, and what makes a shown popup close: Escape, or a press
//! outside it ([`Dismissable`]).

use std::fmt;
use std::rc::Rc;

use gpui::{
    Anchor, AnyElement, AnyWindowHandle, App, BoxShadow, ClickEvent, ElementId, Global, Hsla,
    InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Pixels, Point,
    RenderOnce, Role, SharedString, StatefulInteractiveElement as _, Styled as _, WeakEntity,
    Window, anchored, deferred, div, point, prelude::FluentBuilder as _, px, relative,
};

use crate::components::{KeyHint, Tooltip};
use crate::icon::{Icon, IconName};
use crate::theme::ActiveTheme as _;

type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;
type DismissHandler = Rc<dyn Fn(&Dismissal, &mut Window, &mut App) + 'static>;

/// Why a menu or popover closes by itself.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Dismissal {
    /// A press outside it, at this window position. The press then does
    /// its own job too (a click on a row selects it).
    Press(Point<Pixels>),
    /// Escape.
    Escape,
}

/// Space left and right of an item's content.
const ITEM_PADDING: f32 = 8.;
/// Width of the check mark column.
const CHECK_SLOT: f32 = 14.;
/// Space between the check mark column and the label.
const ITEM_GAP: f32 = 8.;

/// One entry of a [`Menu`].
#[derive(IntoElement)]
#[must_use = "a menu item does nothing unless rendered"]
pub struct MenuItem {
    id: ElementId,
    label: SharedString,
    checked: Option<bool>,
    key: Option<SharedString>,
    trailing_icon: Option<IconName>,
    dot: Option<Hsla>,
    detail: Option<(SharedString, Option<Hsla>)>,
    count: Option<usize>,
    highlighted: bool,
    disabled: bool,
    tooltip: Option<Tooltip>,
    on_click: Option<ClickHandler>,
}

/// The width kept for a [`MenuItem::count`] (`99+` at the label size).
const COUNT_SLOT: f32 = 22.;

impl MenuItem {
    /// An item labelled `label`.
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            checked: None,
            key: None,
            trailing_icon: None,
            dot: None,
            detail: None,
            count: None,
            highlighted: false,
            disabled: false,
            tooltip: None,
            on_click: None,
        }
    }

    /// Shows a small dot in `color` before the label (the footer's status
    /// dot: a connection's health).
    pub fn dot(mut self, color: Hsla) -> Self {
        self.dot = Some(color);
        self
    }

    /// Shows faint, smaller text after the label (`master-01 · 2s`). The
    /// label keeps its width; the detail is cut short first.
    pub fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        self.detail = Some((detail.into(), None));
        self
    }

    /// [`MenuItem::detail`] in `color` (a warning) instead of faint.
    pub fn detail_colored(mut self, detail: impl Into<SharedString>, color: Hsla) -> Self {
        self.detail = Some((detail.into(), Some(color)));
        self
    }

    /// Shows `count` at the right end in the accent colour (unread
    /// notifications, like the footer's badge), nothing for 0. The slot is
    /// there for every count, so the label and detail never move.
    pub fn count(mut self, count: usize) -> Self {
        self.count = Some(count);
        self
    }

    /// Shows the label in the accent colour (something new behind it):
    /// colour only, like [`SubTabs::marked_tab`](crate::SubTabs::marked_tab).
    pub fn highlighted(mut self, highlighted: bool) -> Self {
        self.highlighted = highlighted;
        self
    }

    /// Shows a tooltip on hover (why the item is disabled).
    pub fn tooltip(mut self, tooltip: Tooltip) -> Self {
        self.tooltip = Some(tooltip);
        self
    }

    /// Shows a check mark slot; `true` draws the mark (radio and toggle
    /// items). When any item of a [`Menu`] has the slot, the menu gives the
    /// others an empty one so that all labels line up.
    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = Some(checked);
        self
    }

    /// Shows the key that runs the same command.
    pub fn key_hint(mut self, key: impl Into<SharedString>) -> Self {
        self.key = Some(key.into());
        self
    }

    /// Shows a small faint icon after the label, as the sidebar's rows
    /// show a muted dashboard's bell-off (`None`: none). It takes no room
    /// from the label, so the label never moves when it comes or goes.
    pub fn trailing_icon(mut self, icon: Option<IconName>) -> Self {
        self.trailing_icon = icon;
        self
    }

    /// Greys the item out and ignores clicks.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Runs `handler` when the item is chosen.
    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }

    /// The item's label.
    #[must_use]
    pub fn label(&self) -> &SharedString {
        &self.label
    }

    /// Whether the item shows a check mark.
    #[must_use]
    pub fn is_checked(&self) -> bool {
        self.checked == Some(true)
    }
}

impl fmt::Debug for MenuItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MenuItem")
            .field("id", &self.id)
            .field("label", &self.label)
            .field("checked", &self.checked)
            .field("disabled", &self.disabled)
            .finish_non_exhaustive()
    }
}

impl RenderOnce for MenuItem {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let enabled = !self.disabled;
        div()
            .id(self.id)
            .role(Role::MenuItem)
            .aria_label(self.label.clone())
            .flex()
            .flex_none()
            .items_center()
            .gap(px(ITEM_GAP))
            .h(theme.metrics.menu_item_height)
            .px(px(ITEM_PADDING))
            .rounded(theme.metrics.small_radius)
            .text_size(theme.text.body)
            .text_color(if !enabled {
                colors.text_faint
            } else if self.highlighted {
                colors.accent
            } else {
                colors.text
            })
            .whitespace_nowrap()
            .when_some(self.checked, |item, checked| {
                item.child(
                    div()
                        .flex()
                        .flex_none()
                        .w(px(CHECK_SLOT))
                        .when(checked, |slot| {
                            slot.child(
                                Icon::new(IconName::Check)
                                    .size(px(13.))
                                    .color(colors.accent),
                            )
                        }),
                )
            })
            .when_some(self.dot, |item, color| {
                item.child(
                    div()
                        .flex_none()
                        .size(theme.metrics.status_dot)
                        .rounded_full()
                        .bg(color),
                )
            })
            .map(|item| match self.detail {
                // The label keeps its width (cut short only when it alone
                // is too wide); the detail takes what is left.
                Some((detail, color)) => item
                    .child(div().min_w_0().truncate().child(self.label))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(theme.text.small)
                            .text_color(color.unwrap_or(colors.text_faint))
                            .child(detail),
                    ),
                None => item.child(div().flex_1().child(self.label)),
            })
            .when_some(self.trailing_icon, |item, icon| {
                item.child(Icon::new(icon).size(px(11.)).color(colors.text_faint))
            })
            .when_some(self.count, |item, count| {
                item.child(
                    div()
                        .flex()
                        .flex_none()
                        .justify_end()
                        .w(px(COUNT_SLOT))
                        .text_size(theme.text.label)
                        .text_color(colors.accent)
                        .children(match count {
                            0 => None,
                            1..=99 => Some(count.to_string()),
                            _ => Some("99+".to_owned()),
                        }),
                )
            })
            .when_some(self.key, |item, key| item.child(KeyHint::new(key)))
            .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            .when(enabled, |item| {
                item.cursor_pointer()
                    .hover(|style| {
                        style
                            .bg(colors.element_hover)
                            .text_color(colors.text_strong)
                    })
                    .active(|style| style.bg(colors.element_active))
            })
            .when_some(self.tooltip, |item, tooltip| {
                item.tooltip(tooltip.builder())
            })
            .when_some(self.on_click.filter(|_| enabled), |item, handler| {
                item.on_click(move |event, window, cx| {
                    cx.stop_propagation();
                    handler(event, window, cx);
                })
            })
    }
}

enum Entry {
    Item(Box<MenuItem>),
    Label(SharedString),
    Separator,
    Element(AnyElement),
}

/// A popup menu's card: items with check marks and key hints, section
/// labels and separators. Place it with [`Popover`].
#[derive(IntoElement)]
#[must_use = "a menu does nothing unless rendered"]
pub struct Menu {
    id: ElementId,
    entries: Vec<Entry>,
    min_width: Pixels,
    on_dismiss: Option<DismissHandler>,
}

impl Menu {
    /// An empty menu.
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            entries: Vec::new(),
            min_width: px(200.),
            on_dismiss: None,
        }
    }

    /// Adds an item.
    pub fn item(mut self, item: MenuItem) -> Self {
        self.entries.push(Entry::Item(Box::new(item)));
        self
    }

    /// Adds a faint section label (`sort by`).
    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.entries.push(Entry::Label(label.into()));
        self
    }

    /// Adds arbitrary content (key/value lines, a status paragraph),
    /// padded like an item.
    pub fn element(mut self, element: impl IntoElement) -> Self {
        self.entries
            .push(Entry::Element(element.into_any_element()));
        self
    }

    /// Adds a separator line.
    pub fn separator(mut self) -> Self {
        self.entries.push(Entry::Separator);
        self
    }

    /// Sets the minimum width (default 200px).
    pub fn min_width(mut self, width: Pixels) -> Self {
        self.min_width = width;
        self
    }

    /// Runs `handler` on Escape or a press outside the menu (close it
    /// there; see [`Dismissable`]).
    pub fn on_dismiss(
        mut self,
        handler: impl Fn(&Dismissal, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_dismiss = Some(Rc::new(handler));
        self
    }

    /// Whether the menu has a check mark column: some item has a check mark
    /// slot. Labels and the other items are then indented past it.
    #[must_use]
    pub fn has_check_column(&self) -> bool {
        self.entries
            .iter()
            .any(|entry| matches!(entry, Entry::Item(item) if item.checked.is_some()))
    }

    /// The labels of the menu's items, in order.
    #[must_use]
    pub fn item_labels(&self) -> Vec<SharedString> {
        self.entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::Item(item) => Some(item.label.clone()),
                Entry::Label(_) | Entry::Separator | Entry::Element(_) => None,
            })
            .collect()
    }
}

impl fmt::Debug for Menu {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Menu")
            .field("id", &self.id)
            .field("items", &self.item_labels())
            .finish_non_exhaustive()
    }
}

impl RenderOnce for Menu {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let check_column = self.has_check_column();
        let label_indent = if check_column {
            ITEM_PADDING + CHECK_SLOT + ITEM_GAP
        } else {
            ITEM_PADDING
        };
        let entries: Vec<AnyElement> = self
            .entries
            .into_iter()
            .map(|entry| match entry {
                Entry::Item(mut item) => {
                    if check_column && item.checked.is_none() {
                        item.checked = Some(false);
                    }
                    (*item).into_any_element()
                }
                Entry::Label(label) => div()
                    .pl(px(label_indent))
                    .pr(px(ITEM_PADDING))
                    .pt(px(6.))
                    .pb(px(2.))
                    .text_size(theme.text.label)
                    .text_color(colors.text_faint)
                    .child(label)
                    .into_any_element(),
                Entry::Separator => div()
                    .my(px(4.))
                    .mx(px(-4.))
                    .h(px(1.))
                    .bg(colors.border_window)
                    .into_any_element(),
                Entry::Element(element) => div()
                    .px(px(ITEM_PADDING))
                    .py(px(4.))
                    .text_size(theme.text.body)
                    .text_color(colors.text)
                    .child(element)
                    .into_any_element(),
            })
            .collect();
        let card = div()
            .id(self.id.clone())
            .role(Role::Menu)
            .occlude()
            .flex()
            .flex_col()
            .min_w(self.min_width)
            .p(px(4.))
            .rounded(theme.metrics.code_radius)
            .border_1()
            .border_color(colors.border_window)
            .bg(colors.element_background)
            .shadow(vec![BoxShadow {
                color: colors.shadow_strong,
                offset: point(px(0.), px(6.)),
                blur_radius: px(18.),
                spread_radius: px(0.),
                inset: false,
            }])
            .font_family(theme.font_family.clone())
            .line_height(theme.line_height)
            .children(entries);
        match self.on_dismiss {
            Some(handler) => Dismissable {
                id: ElementId::NamedChild(std::sync::Arc::new(self.id), "popup".into()),
                content: card.into_any_element(),
                on_dismiss: handler,
            }
            .into_any_element(),
            None => card.into_any_element(),
        }
    }
}

/// A shown popup's dismissal handler, kept current by each render. The
/// popup's element state holds it: it goes when the popup is no longer
/// drawn.
struct ShownPopup {
    window: AnyWindowHandle,
    on_dismiss: DismissHandler,
}

/// The popups drawn, oldest first (dead ones are pruned as they're found).
#[derive(Default)]
struct ShownPopups(Vec<WeakEntity<ShownPopup>>);

impl Global for ShownPopups {}

/// Escape closes the newest popup shown in its window before anything else
/// sees the key (the list behind keeps its marks, the pane stays open).
/// The popups don't take the keyboard: the list's keys keep working while
/// one is open. From [`crate::init`].
pub(crate) fn init(cx: &mut App) {
    cx.default_global::<ShownPopups>();
    cx.intercept_keystrokes(|event, window, cx| {
        let keystroke = &event.keystroke;
        if keystroke.key != "escape" || keystroke.modifiers.modified() {
            return;
        }
        let here = window.window_handle();
        let Some(popup) = cx.try_global::<ShownPopups>().and_then(|shown| {
            shown
                .0
                .iter()
                .rev()
                .filter_map(WeakEntity::upgrade)
                .find(|popup| popup.read(cx).window == here)
        }) else {
            return;
        };
        let on_dismiss = popup.read(cx).on_dismiss.clone();
        cx.stop_propagation();
        on_dismiss(&Dismissal::Escape, window, cx);
    })
    .detach();
}

/// A shown menu or popover (`content`) that closes itself: Escape (before
/// the view behind sees it) or a press outside it calls `on_dismiss`,
/// which closes it.
#[derive(IntoElement)]
#[must_use = "a popup does nothing unless rendered"]
pub struct Dismissable {
    id: ElementId,
    content: AnyElement,
    on_dismiss: DismissHandler,
}

impl Dismissable {
    /// `content` as a popup (`id` unique among the popups of its parent),
    /// closed through `on_dismiss`.
    pub fn new(
        id: impl Into<ElementId>,
        content: impl IntoElement,
        on_dismiss: impl Fn(&Dismissal, &mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            id: id.into(),
            content: content.into_any_element(),
            on_dismiss: Rc::new(on_dismiss),
        }
    }
}

impl fmt::Debug for Dismissable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Dismissable")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl RenderOnce for Dismissable {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let on_dismiss = self.on_dismiss;
        let registered = on_dismiss.clone();
        let shown = window.use_keyed_state(self.id.clone(), cx, move |window, cx| {
            let popup = cx.weak_entity();
            let shown = cx.default_global::<ShownPopups>();
            shown.0.retain(|popup| popup.upgrade().is_some());
            shown.0.push(popup);
            ShownPopup {
                window: window.window_handle(),
                on_dismiss: registered,
            }
        });
        let current = on_dismiss.clone();
        shown.update(cx, |shown, _| shown.on_dismiss = current);
        div()
            .id(self.id)
            .on_mouse_down_out(move |event, window, cx| {
                on_dismiss(&Dismissal::Press(event.position), window, cx);
            })
            .child(self.content)
    }
}

/// Floats `content` (a [`Menu`]) under the element it's a child of, on top of
/// everything else and kept inside the window. The parent must be
/// `relative()`.
///
/// ```text
/// div().relative().child(trigger).when(open, |el| {
///     el.child(Popover::new(menu).align_right())
/// })
/// ```
#[derive(IntoElement)]
#[must_use = "a popover does nothing unless rendered"]
pub struct Popover {
    content: AnyElement,
    align_right: bool,
    above: bool,
    gap: Pixels,
}

impl Popover {
    /// A popover under the parent's left edge.
    pub fn new(content: impl IntoElement) -> Self {
        Self {
            content: content.into_any_element(),
            align_right: false,
            above: false,
            gap: px(4.),
        }
    }

    /// Opens the popover above the parent instead of under it (for
    /// triggers at the bottom of the window, like the sidebar footer).
    pub fn above(mut self) -> Self {
        self.above = true;
        self
    }

    /// Aligns the popover's right edge with the parent's.
    pub fn align_right(mut self) -> Self {
        self.align_right = true;
        self
    }

    /// Sets the space between the parent and the popover (default 4px).
    pub fn gap(mut self, gap: Pixels) -> Self {
        self.gap = gap;
        self
    }
}

impl fmt::Debug for Popover {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Popover")
            .field("align_right", &self.align_right)
            .field("above", &self.above)
            .field("gap", &self.gap)
            .finish_non_exhaustive()
    }
}

impl RenderOnce for Popover {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let anchor = match (self.above, self.align_right) {
            (false, false) => Anchor::TopLeft,
            (false, true) => Anchor::TopRight,
            (true, false) => Anchor::BottomLeft,
            (true, true) => Anchor::BottomRight,
        };
        // An empty box at the parent's bottom corner (top corner when
        // above); the content hangs from it (stands on it).
        div()
            .absolute()
            .map(|corner| {
                if self.above {
                    corner.bottom(relative(1.)).mb(self.gap)
                } else {
                    corner.top(relative(1.)).mt(self.gap)
                }
            })
            .map(|corner| {
                if self.align_right {
                    corner.right_0()
                } else {
                    corner.left_0()
                }
            })
            .child(
                deferred(
                    anchored()
                        .anchor(anchor)
                        .snap_to_window_with_margin(px(8.))
                        .child(self.content),
                )
                .with_priority(1),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menus_list_their_items_in_order() {
        let menu = Menu::new("sort")
            .label("sort by")
            .item(MenuItem::new("severity", "severity").checked(true))
            .item(MenuItem::new("host", "host").checked(false))
            .separator()
            .item(MenuItem::new("descending", "descending").key_hint("↓"));
        assert_eq!(menu.item_labels(), ["severity", "host", "descending"]);
        assert!(format!("{menu:?}").contains("severity"));
    }

    #[test]
    fn one_check_mark_slot_makes_a_check_column() {
        let plain = Menu::new("plain")
            .label("actions")
            .item(MenuItem::new("copy", "copy"));
        assert!(!plain.has_check_column());
        let mixed = plain.item(MenuItem::new("hide", "hide handled").checked(false));
        assert!(mixed.has_check_column());
    }

    #[test]
    fn items_record_check_and_disabled_state() {
        let item = MenuItem::new("edit", "edit dashboard…")
            .disabled(true)
            .on_click(|_, _, _| {});
        assert!(item.disabled);
        assert!(!item.is_checked());
        assert_eq!(item.label(), "edit dashboard…");
        assert!(MenuItem::new("on", "on").checked(true).is_checked());
    }

    #[test]
    fn popovers_default_to_the_left_edge() {
        let popover = Popover::new(div()).gap(px(2.));
        assert!(!popover.align_right);
        assert_eq!(popover.gap, px(2.));
        assert!(Popover::new(div()).align_right().align_right);
    }
}
