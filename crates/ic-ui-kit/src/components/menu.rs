//! The one dropdown system (README *Dropdowns: one system*): the menu card
//! and its items, shared by action menus (`···`, *add view*, a header's
//! sort) and the lists of selects ([`crate::Select`]); the popover that
//! hangs a menu from its trigger; and what makes a shown popup close or
//! take the keyboard: Escape, a press outside it ([`Dismissable`]), the
//! arrows, Enter and typing a name's first letters.
//!
//! **An entry is its icon and its name**, nothing else: no descriptions.
//! Every item is the same: [`Metrics::menu_item_height`](crate::Metrics)
//! high (density-matched), its highlight the card's full width, its text
//! 11px inside the card's outer edge (the 1px border and 10px), an
//! optional 13px icon (or status dot), the name (cut short with an
//! ellipsis, the whole name in a tooltip), a key hint, and the check slot
//! at the right, where it never pushes the text.

use std::fmt;
use std::rc::Rc;
use std::time::{Duration, Instant};

use crate::px;
use gpui::{
    AnyElement, AnyWindowHandle, App, Bounds, BoxShadow, ClickEvent, ElementId, Entity, EntityId,
    Global, Hsla, InteractiveElement as _, IntoElement, KeyboardButton, KeyboardClickEvent,
    Keystroke, MouseButton, ParentElement as _, Pixels, Point, RenderOnce, Role, ScrollHandle,
    SharedString, StatefulInteractiveElement as _, Styled as _, WeakEntity, Window, div, point,
    prelude::FluentBuilder as _,
};

use crate::components::float::{Float, FloatKind, Side};
use crate::components::tooltip::{Beside, TooltipPlacement};
use crate::components::{KeyHint, Tooltip};
use crate::icon::{Icon, IconName};
use crate::theme::{ActiveTheme as _, CHAR_WIDTH, Theme};

type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;
type SharedClick = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;
type DismissHandler = Rc<dyn Fn(&Dismissal, &mut Window, &mut App) + 'static>;
type KeyHandler = Rc<dyn Fn(&Keystroke, &mut Window, &mut App) -> bool + 'static>;

/// Why a menu or popover closes by itself.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Dismissal {
    /// A press outside it, at this window position. The press then does
    /// its own job too (a click on a row selects it).
    Press(Point<Pixels>),
    /// Escape.
    Escape,
}

/// Space left and right of an item's content, inside the card's 1px
/// border: the text sits 11px inside the card's outer edge, as a field's.
pub(crate) const ITEM_PADDING: f32 = 10.;
/// Space between an item's parts.
const ITEM_GAP: f32 = 8.;
/// Width of the icon slot and of the check slot.
const SLOT: f32 = 13.;
/// The card's padding above the first and under the last entry.
const CARD_PADDING: f32 = 4.;
/// An action menu's width: sized to its longest entry, within these.
pub(crate) const MENU_MIN_WIDTH: f32 = 180.;
/// See [`MENU_MIN_WIDTH`].
pub(crate) const MENU_MAX_WIDTH: f32 = 280.;
/// A select's list shows this many items, then scrolls.
pub(crate) const SELECT_ROWS: usize = 10;
/// The space between an action menu and its trigger.
const MENU_GAP: f32 = 4.;
/// Typed letters within this long of each other make one name to jump to.
const TYPE_AHEAD: Duration = Duration::from_millis(900);

/// The group a menu item's hover reveals its [`ItemAction`]s in.
const ITEM_GROUP: &str = "menu-item";

/// The width of an [`ItemAction`]'s slot.
const ACTION_SLOT: f32 = 22.;

/// A small icon button of a [`MenuItem`] for a second command on the same
/// row (this environment's settings, where clicking the row switches to
/// it). Each sits in a slot of its own at the item's right end, so nothing
/// moves when it shows: [`ItemAction::always`] ones are always there,
/// dimmed, brighter while the pointer is on the row or the row is
/// selected; the others show while the pointer is on the row, or while
/// [`ItemAction::shown`], like the sidebar rows' `···`.
#[must_use = "an item action does nothing unless given to a menu item"]
pub struct ItemAction {
    id: ElementId,
    icon: IconName,
    tooltip: Option<Tooltip>,
    always: bool,
    shown: bool,
    on_click: ClickHandler,
}

impl ItemAction {
    /// An action showing `icon` on hover, running `handler` when clicked
    /// (the item's own click doesn't run then).
    pub fn new(
        id: impl Into<ElementId>,
        icon: IconName,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            id: id.into(),
            icon,
            tooltip: None,
            always: false,
            shown: false,
            on_click: Box::new(handler),
        }
    }

    /// Shows a tooltip on hover.
    pub fn tooltip(mut self, tooltip: Tooltip) -> Self {
        self.tooltip = Some(tooltip);
        self
    }

    /// Always shows it, dimmed, brighter on the row's hover or selection.
    pub fn always(mut self) -> Self {
        self.always = true;
        self
    }

    /// Shows it highlighted (what it opened is showing), also without
    /// hovering the row.
    pub fn shown(mut self, shown: bool) -> Self {
        self.shown = shown;
        self
    }

    /// The action's slot: the icon button, dimmed or hidden until the row
    /// is hovered (see the type's notes). `selected`: the row is.
    fn render(self, selected: bool, cx: &App) -> gpui::Stateful<gpui::Div> {
        let theme = cx.theme();
        let colors = theme.colors;
        let handler = self.on_click;
        let resting = if selected || self.shown {
            colors.text_muted
        } else {
            colors.text_faint
        };
        let reveal = self.always || self.shown;
        div()
            .id(self.id)
            .role(Role::Button)
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(ACTION_SLOT - 2.))
            .rounded(theme.metrics.small_radius)
            .text_color(resting)
            .when(self.shown, |button| button.bg(colors.element_hover))
            .when(!reveal, gpui::Styled::invisible)
            // One group-hover style per element: a later one replaces it.
            .group_hover(ITEM_GROUP, move |style| {
                style.visible().text_color(colors.text_muted)
            })
            .hover(|style| {
                style
                    .bg(colors.element_hover)
                    .text_color(colors.text_strong)
            })
            .cursor_pointer()
            .child(Icon::new(self.icon).size(px(12.)))
            .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            .when_some(self.tooltip, |button, tooltip| {
                button.tooltip(tooltip.builder())
            })
            .on_click(move |event, window, cx| {
                cx.stop_propagation();
                handler(event, window, cx);
            })
    }
}

impl fmt::Debug for ItemAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ItemAction")
            .field("id", &self.id)
            .field("icon", &self.icon)
            .field("always", &self.always)
            .field("shown", &self.shown)
            .finish_non_exhaustive()
    }
}

/// One entry of a [`Menu`]: its icon (or status dot) and its name, an
/// optional key hint, and a check mark in the check slot at the right.
#[must_use = "a menu item does nothing unless given to a menu"]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent looks of one item"
)]
pub struct MenuItem {
    id: ElementId,
    label: SharedString,
    checked: Option<bool>,
    key: Option<SharedString>,
    dot: Option<Hsla>,
    icon: Option<IconName>,
    detail: Option<(SharedString, Option<Hsla>)>,
    actions: Vec<ItemAction>,
    selected: bool,
    interactive: bool,
    highlighted: bool,
    disabled: bool,
    tooltip: Option<Tooltip>,
    on_click: Option<ClickHandler>,
}

impl MenuItem {
    /// An item labelled `label`.
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            checked: None,
            key: None,
            dot: None,
            icon: None,
            detail: None,
            actions: Vec::new(),
            selected: false,
            interactive: true,
            highlighted: false,
            disabled: false,
            tooltip: None,
            on_click: None,
        }
    }

    /// Shows a small dot in `color` before the label, in the icon slot
    /// (an environment's health).
    pub fn dot(mut self, color: Hsla) -> Self {
        self.dot = Some(color);
        self
    }

    /// Shows `icon` (muted) before the label, in the icon slot: the kinds
    /// of view in *add view*.
    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Shows faint, smaller text after the label (`master-01 · 2s`). For
    /// status popovers only (the connection details): a dropdown's entry
    /// is its name, with no description.
    pub fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        self.detail = Some((detail.into(), None));
        self
    }

    /// [`MenuItem::detail`] in `color` (a warning) instead of faint.
    pub fn detail_colored(mut self, detail: impl Into<SharedString>, color: Hsla) -> Self {
        self.detail = Some((detail.into(), Some(color)));
        self
    }

    /// Adds a second command at the right end, in a slot of its own (see
    /// [`ItemAction`]); several follow each other in order.
    pub fn action(mut self, action: ItemAction) -> Self {
        self.actions.push(action);
        self
    }

    /// Marks the item as the current one with the selected-row background
    /// of the lists (the environment on screen, the node connected to), in
    /// a status popover. A select's current value is ticked instead
    /// ([`MenuItem::checked`]).
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// `false`: a row that only shows something (a cluster node): no
    /// hover, no pointer, no click; its colours stay as they are.
    pub fn interactive(mut self, interactive: bool) -> Self {
        self.interactive = interactive;
        self
    }

    /// Shows the label in the accent colour (something new behind it):
    /// colour only, like [`SubTabs::marked_tab`](crate::SubTabs::marked_tab).
    pub fn highlighted(mut self, highlighted: bool) -> Self {
        self.highlighted = highlighted;
        self
    }

    /// Shows a tooltip on hover: a disabled item's reason. With
    /// [`TooltipPlacement::Left`] it shows left of the menu, beside the
    /// item, so it never covers the next item.
    pub fn tooltip(mut self, tooltip: Tooltip) -> Self {
        self.tooltip = Some(tooltip);
        self
    }

    /// Gives the item a check slot; `true` draws the mark (radio and
    /// toggle items, a select's current value). When any item of a
    /// [`Menu`] has the slot, all get one, so the key hints line up.
    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = Some(checked);
        self
    }

    /// Shows the key that runs the same command.
    pub fn key_hint(mut self, key: impl Into<SharedString>) -> Self {
        self.key = Some(key.into());
        self
    }

    /// Greys the item out (the faint text colour) and ignores clicks.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Runs `handler` when the item is chosen (a click, or Enter on it).
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

    /// Whether the item is marked as the current one.
    #[must_use]
    pub fn is_selected(&self) -> bool {
        self.selected
    }

    /// Whether the item is greyed out.
    #[must_use]
    pub fn is_disabled(&self) -> bool {
        self.disabled
    }

    /// Whether choosing it does something.
    fn choosable(&self) -> bool {
        !self.disabled && self.interactive && self.on_click.is_some()
    }
}

impl gpui::prelude::FluentBuilder for MenuItem {}

impl fmt::Debug for MenuItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MenuItem")
            .field("id", &self.id)
            .field("label", &self.label)
            .field("checked", &self.checked)
            .field("selected", &self.selected)
            .field("disabled", &self.disabled)
            .finish_non_exhaustive()
    }
}

/// What an item is drawn with, from its menu.
struct ItemLook {
    /// The menu has a check column.
    check_column: bool,
    /// The keyboard cursor is on it.
    cursor: bool,
    /// Its position among the menu's items (for the cursor).
    index: usize,
    /// The width its label has, for the tooltip of a cut-off name.
    label_room: Option<Pixels>,
    /// The popup's state, to move the cursor on hover.
    popup: Option<Entity<ShownPopup>>,
    /// The view drawing the menu.
    view: EntityId,
}

/// Draws `item` (see the module notes).
#[expect(clippy::too_many_lines, reason = "one item, its slots in order")]
fn render_item(
    item: MenuItem,
    on_click: Option<SharedClick>,
    look: &ItemLook,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let theme = cx.theme().clone();
    let colors = theme.colors;
    let enabled = !item.disabled;
    let clickable = enabled && item.interactive;
    let selected = item.selected;
    let cursor = look.cursor && clickable;
    let label_width = text_width(&item.label, theme.text.body);
    // A cut-off name shows whole in a tooltip (unless the item has its
    // own tooltip, a disabled item's reason).
    let tooltip = item.tooltip.clone().or_else(|| {
        look.label_room
            .filter(|room| label_width > *room)
            .map(|_| Tooltip::new(item.label.clone()))
    });
    let beside = tooltip
        .as_ref()
        .filter(|tooltip| tooltip.placed() == TooltipPlacement::Left)
        .map(|_| {
            Beside::state(
                ElementId::NamedChild(std::sync::Arc::new(item.id.clone()), "tip".into()),
                window,
                cx,
            )
        });
    let actions: Vec<_> = item
        .actions
        .into_iter()
        .map(|action| action.render(selected, cx))
        .collect();
    let leading = leading_mark(item.dot, item.icon, enabled, &theme);
    let index = look.index;
    let popup = look.popup.clone();
    let view = look.view;
    let hover_beside = beside.clone();
    let beside_float = beside
        .as_ref()
        .zip(tooltip.as_ref())
        .and_then(|(state, tooltip)| Beside::float(state, tooltip, cx));
    div()
        .id(item.id)
        .role(Role::MenuItem)
        .aria_label(item.label.clone())
        .group(ITEM_GROUP)
        .relative()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(ITEM_GAP))
        .h(theme.metrics.menu_item_height)
        .px(px(ITEM_PADDING))
        .text_size(theme.text.body)
        .text_color(if !enabled {
            colors.text_faint
        } else if item.highlighted {
            colors.accent_text
        } else if cursor {
            colors.text_strong
        } else {
            colors.text
        })
        .when(selected, |row| row.bg(colors.row_selected))
        .when(cursor && !selected, |row| row.bg(colors.element_hover))
        .whitespace_nowrap()
        .children(leading)
        .map(|row| match item.detail {
            // The label keeps its width (cut short only when it alone
            // is too wide); the detail takes what is left.
            Some((detail, color)) => row
                .child(div().min_w_0().truncate().child(item.label))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(theme.text.small)
                        .text_color(color.unwrap_or(colors.text_faint))
                        .child(detail),
                ),
            None => row.child(div().flex_1().min_w_0().truncate().child(item.label)),
        })
        .when(!actions.is_empty(), |row| {
            row.child(div().flex().flex_none().gap(px(2.)).children(actions))
        })
        .when_some(item.key, |row, key| row.child(KeyHint::new(key)))
        .when(look.check_column, |row| {
            row.child(div().flex().flex_none().w(px(SLOT)).when(
                item.checked == Some(true),
                |slot| {
                    slot.child(
                        Icon::new(IconName::Check)
                            .size(px(SLOT))
                            .color(colors.accent),
                    )
                },
            ))
        })
        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
        .on_hover(move |hovered, window, cx| {
            if *hovered
                && clickable
                && let Some(popup) = &popup
            {
                popup.update(cx, |popup, _| {
                    if let Some(nav) = &mut popup.nav {
                        nav.cursor = Some(index);
                    }
                });
                cx.notify(view);
            }
            if let Some(state) = &hover_beside {
                Beside::hover(state, *hovered, view, window, cx);
            }
        })
        .when(clickable, |row| {
            row.cursor_pointer()
                .active(|style| style.bg(colors.element_active))
        })
        .map(|row| match (&beside, tooltip) {
            (None, Some(tooltip)) => row.tooltip(tooltip.builder()),
            _ => row,
        })
        .children(beside_float)
        .when_some(on_click.filter(|_| clickable), |row, handler| {
            row.on_click(move |event, window, cx| {
                cx.stop_propagation();
                handler(event, window, cx);
            })
        })
        .into_any_element()
}

/// The width `text` takes at `size` (the font is monospaced).
fn text_width(text: &str, size: Pixels) -> Pixels {
    #[expect(
        clippy::cast_precision_loss,
        reason = "labels are far shorter than 2^23 characters"
    )]
    let chars = text.chars().count() as f32;
    size * (chars * CHAR_WIDTH)
}

/// What a [`MenuItem`] shows before its label: a status dot, or a muted
/// icon, in the 13px icon slot.
fn leading_mark(
    dot: Option<Hsla>,
    icon: Option<IconName>,
    enabled: bool,
    theme: &Theme,
) -> Option<AnyElement> {
    let slot = || div().flex().flex_none().justify_center().w(px(SLOT));
    if let Some(color) = dot {
        return Some(
            slot()
                .child(
                    div()
                        .flex_none()
                        .size(theme.metrics.status_dot)
                        .rounded_full()
                        .bg(color),
                )
                .into_any_element(),
        );
    }
    icon.map(|icon| {
        let color = if enabled {
            theme.colors.text_muted
        } else {
            theme.colors.text_faint
        };
        slot()
            .child(Icon::new(icon).size(px(SLOT)).color(color))
            .into_any_element()
    })
}

enum Entry {
    Item(Box<MenuItem>),
    Label(SharedString),
    Separator,
    Element(AnyElement),
    /// Items that scroll within `max_height`.
    Scrolled {
        id: ElementId,
        items: Vec<MenuItem>,
        max_height: Pixels,
    },
}

/// How a [`Menu`] is drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Shape {
    /// A card of its own: an action menu or a status popover.
    Card,
    /// A select's list, joined to its field (see [`crate::Select`]):
    /// opening upward (`up`), at most `max_height` high, `width` wide.
    List {
        up: bool,
        max_height: Pixels,
        width: Option<Pixels>,
    },
}

/// A menu's card: items with icons, key hints and check marks, section
/// labels and dividers (see the module notes). An action menu is sized to
/// its longest entry, 180 to 280px; hang it from its trigger with
/// [`Popover`]. A select ([`crate::Select`]) shows one as its list.
#[derive(IntoElement)]
#[must_use = "a menu does nothing unless rendered"]
pub struct Menu {
    id: ElementId,
    entries: Vec<Entry>,
    width: Option<Pixels>,
    on_dismiss: Option<DismissHandler>,
}

impl Menu {
    /// An empty menu.
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            entries: Vec::new(),
            width: None,
            on_dismiss: None,
        }
    }

    /// Adds an item.
    pub fn item(mut self, item: MenuItem) -> Self {
        self.entries.push(Entry::Item(Box::new(item)));
        self
    }

    /// Adds items that scroll when they are taller than `max_height` (a
    /// long list in a short window), lined up with the others.
    pub fn scrolled(
        mut self,
        id: impl Into<ElementId>,
        items: Vec<MenuItem>,
        max_height: Pixels,
    ) -> Self {
        self.entries.push(Entry::Scrolled {
            id: id.into(),
            items,
            max_height,
        });
        self
    }

    /// Adds a section label: one short word (`lists`, `sort by`).
    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.entries.push(Entry::Label(label.into()));
        self
    }

    /// Adds arbitrary content, padded like an item (a status popover's
    /// key/value lines).
    pub fn element(mut self, element: impl IntoElement) -> Self {
        self.entries
            .push(Entry::Element(element.into_any_element()));
        self
    }

    /// Adds a divider, the card's full width.
    pub fn separator(mut self) -> Self {
        self.entries.push(Entry::Separator);
        self
    }

    /// Makes it a status popover (the connection details, which hold the
    /// environment switcher): `width` wide whatever it shows, and the
    /// keyboard stays with the view behind (no cursor, no type-ahead), as
    /// a status popover is no dropdown. Action menus keep the one width
    /// rule and take the keyboard.
    pub fn status_popover(mut self, width: Pixels) -> Self {
        self.width = Some(width);
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

    /// Whether the menu has a check column: some item has a check slot.
    #[must_use]
    pub fn has_check_column(&self) -> bool {
        self.items().any(|item| item.checked.is_some())
    }

    /// The menu's items, in order.
    fn items(&self) -> impl Iterator<Item = &MenuItem> {
        self.entries.iter().flat_map(|entry| match entry {
            Entry::Item(item) => std::slice::from_ref(item.as_ref()).iter(),
            Entry::Scrolled { items, .. } => items.iter(),
            Entry::Label(_) | Entry::Separator | Entry::Element(_) => [].iter(),
        })
    }

    /// The labels of the menu's items, in order.
    #[must_use]
    pub fn item_labels(&self) -> Vec<SharedString> {
        self.items().map(|item| item.label.clone()).collect()
    }

    /// The section labels, in order.
    #[must_use]
    pub fn section_labels(&self) -> Vec<SharedString> {
        self.entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::Label(label) => Some(label.clone()),
                _ => None,
            })
            .collect()
    }

    /// How tall the menu's entries are together, with the card's padding
    /// (a select decides from it whether its list opens upward).
    #[must_use]
    pub fn content_height(&self, theme: &Theme) -> Pixels {
        let item = theme.metrics.menu_item_height;
        let label = theme.text.label * crate::theme::LINE_HEIGHT + px(8.);
        let mut height = px(2. * CARD_PADDING);
        for entry in &self.entries {
            height += match entry {
                Entry::Item(_) | Entry::Element(_) => item,
                Entry::Label(_) => label,
                Entry::Separator => px(9.),
                #[expect(clippy::cast_precision_loss, reason = "a short list")]
                Entry::Scrolled {
                    items, max_height, ..
                } => (item * items.len() as f32).min(*max_height),
            };
        }
        height
    }

    /// The index of the checked item, where a select's first arrow puts
    /// the keyboard cursor.
    fn checked_index(&self) -> Option<usize> {
        self.items().position(MenuItem::is_checked)
    }

    /// Draws the menu as `shape`.
    #[expect(clippy::too_many_lines, reason = "the card and its entries in order")]
    pub(crate) fn build(self, shape: Shape, window: &mut Window, cx: &mut App) -> AnyElement {
        let theme = cx.theme().clone();
        let colors = theme.colors;
        let check_column = self.has_check_column();
        let list = matches!(shape, Shape::List { .. });
        let view = window.current_view();
        // The keyboard: the items' labels and handlers, the cursor.
        let initial = if list { self.checked_index() } else { None };
        let popup = register_popup(
            ElementId::NamedChild(std::sync::Arc::new(self.id.clone()), "popup".into()),
            self.on_dismiss.clone(),
            initial,
            window,
            cx,
        );
        let scroll = list.then(|| {
            window
                .use_keyed_state(
                    ElementId::NamedChild(std::sync::Arc::new(self.id.clone()), "scroll".into()),
                    cx,
                    |_, _| ScrollHandle::new(),
                )
                .read(cx)
                .clone()
        });
        let card_width = match shape {
            Shape::List { width, .. } => width,
            Shape::Card => Some(self.width.unwrap_or(px(MENU_MAX_WIDTH))),
        };
        let has_icons = self
            .items()
            .any(|item| item.icon.is_some() || item.dot.is_some());
        let mut nav_items = Vec::new();
        let mut entries: Vec<AnyElement> = Vec::new();
        let cursor = popup.read(cx).nav.as_ref().and_then(|nav| nav.cursor);
        let mut render =
            |item: MenuItem, entry_index: usize, window: &mut Window, cx: &mut App| -> AnyElement {
                let index = nav_items.len();
                let mut item = item;
                if check_column && item.checked.is_none() {
                    item.checked = Some(false);
                }
                let choosable = item.choosable();
                let handler: Option<SharedClick> = item.on_click.take().map(Rc::from);
                nav_items.push(NavItem {
                    label: item.label.clone(),
                    choosable,
                    on_choose: handler.clone(),
                    entry: entry_index,
                });
                let label_room = card_width.map(|width| {
                    let mut room = width - px(2. * ITEM_PADDING + 2.);
                    if has_icons {
                        room -= px(SLOT + ITEM_GAP);
                    }
                    if check_column {
                        room -= px(SLOT + ITEM_GAP);
                    }
                    if let Some(key) = &item.key {
                        room -= text_width(key, theme.text.hint) + px(ITEM_GAP);
                    }
                    room
                });
                let look = ItemLook {
                    check_column,
                    cursor: cursor == Some(index),
                    index,
                    label_room,
                    popup: Some(popup.clone()),
                    view,
                };
                render_item(item, handler, &look, window, cx)
            };
        for (entry_index, entry) in self.entries.into_iter().enumerate() {
            let element = match entry {
                Entry::Item(item) => render(*item, entry_index, window, cx),
                Entry::Scrolled {
                    id,
                    items,
                    max_height,
                } => {
                    let rendered: Vec<AnyElement> = items
                        .into_iter()
                        .map(|item| render(item, entry_index, window, cx))
                        .collect();
                    div()
                        .id(id)
                        .flex()
                        .flex_col()
                        .flex_none()
                        .max_h(max_height)
                        .overflow_y_scroll()
                        .children(rendered)
                        .into_any_element()
                }
                Entry::Label(label) => div()
                    .flex_none()
                    .px(px(ITEM_PADDING))
                    .pt(px(6.))
                    .pb(px(2.))
                    .whitespace_nowrap()
                    .text_size(theme.text.label)
                    .text_color(colors.text_faint)
                    .child(label)
                    .into_any_element(),
                Entry::Separator => div()
                    .flex_none()
                    .my(px(4.))
                    .h(px(1.))
                    .bg(colors.border_window)
                    .into_any_element(),
                Entry::Element(element) => div()
                    .flex_none()
                    .px(px(ITEM_PADDING))
                    .py(px(4.))
                    .text_size(theme.text.body)
                    .text_color(colors.text)
                    .child(element)
                    .into_any_element(),
            };
            entries.push(element);
        }
        let status = self.width.is_some();
        popup.update(cx, |popup, _| {
            popup.view = Some(view);
            if status {
                popup.nav = None;
            }
            if let Some(nav) = &mut popup.nav {
                nav.cursor = nav.cursor.filter(|cursor| *cursor < nav_items.len());
                nav.items = nav_items;
                nav.scroll.clone_from(&scroll);
            }
        });
        let on_dismiss = self.on_dismiss;
        let shown = match shape {
            Shape::Card => div()
                .id(self.id)
                .role(Role::Menu)
                .occlude()
                .flex()
                .flex_col()
                .flex_none()
                .map(|card| match self.width {
                    Some(width) => card.w(width),
                    None => card.min_w(px(MENU_MIN_WIDTH)).max_w(px(MENU_MAX_WIDTH)),
                })
                .py(px(CARD_PADDING))
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
                .children(entries),
            Shape::List { up, max_height, .. } => {
                let scroll = scroll.unwrap_or_default();
                let radius = theme.metrics.code_radius;
                div()
                    .id(self.id)
                    .role(Role::ListBox)
                    .occlude()
                    .relative()
                    .flex()
                    .flex_col()
                    .flex_none()
                    .w_full()
                    .bg(colors.code_background)
                    .border_color(colors.accent)
                    .border_l_1()
                    .border_r_1()
                    .map(|list| {
                        if up {
                            list.border_t_1().rounded_t(radius).shadow(vec![BoxShadow {
                                color: colors.shadow_strong,
                                offset: point(px(0.), px(-14.)),
                                blur_radius: px(20.),
                                spread_radius: px(-10.),
                                inset: false,
                            }])
                        } else {
                            list.border_b_1().rounded_b(radius).shadow(vec![BoxShadow {
                                color: colors.shadow_strong,
                                offset: point(px(0.), px(14.)),
                                blur_radius: px(20.),
                                spread_radius: px(-10.),
                                inset: false,
                            }])
                        }
                    })
                    .font_family(theme.font_family.clone())
                    .line_height(theme.line_height)
                    .child(
                        div()
                            .id("select-rows")
                            .flex()
                            .flex_col()
                            .max_h(max_height)
                            .py(px(CARD_PADDING))
                            .overflow_y_scroll()
                            .track_scroll(&scroll)
                            .children(entries),
                    )
                    .child(crate::Scrollbar::vertical(&scroll))
            }
        };
        match on_dismiss {
            Some(handler) => shown
                .on_mouse_down_out(move |event, window, cx| {
                    handler(&Dismissal::Press(event.position), window, cx);
                })
                .into_any_element(),
            None => shown.into_any_element(),
        }
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
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        self.build(Shape::Card, window, cx)
    }
}

/// An item as the keyboard sees it.
struct NavItem {
    label: SharedString,
    choosable: bool,
    on_choose: Option<SharedClick>,
    /// Its entry (a child of the list's scrolled box, for a select).
    entry: usize,
}

/// A menu's keyboard: its items, the cursor, the letters typed.
#[derive(Default)]
struct Nav {
    items: Vec<NavItem>,
    cursor: Option<usize>,
    /// Where the first arrow puts the cursor: a select's current value.
    anchor: Option<usize>,
    typed: String,
    typed_at: Option<Instant>,
    scroll: Option<ScrollHandle>,
}

impl Nav {
    /// Moves the cursor `step` choosable items on (wrapping); from no
    /// cursor, down starts at the first and up at the last.
    fn step(&mut self, step: isize) {
        let count = self.items.len();
        if count == 0 {
            return;
        }
        let choosable = |index: usize| self.items[index].choosable;
        if self.cursor.is_none()
            && let Some(anchor) = self
                .anchor
                .filter(|anchor| *anchor < count && choosable(*anchor))
        {
            self.cursor = Some(anchor);
            self.reveal();
            return;
        }
        let mut at = match self.cursor {
            Some(cursor) => cursor,
            None if step > 0 => count - 1,
            None => 0,
        };
        for _ in 0..count {
            at = (at.cast_signed() + step)
                .rem_euclid(count.cast_signed())
                .cast_unsigned();
            if choosable(at) {
                self.cursor = Some(at);
                self.reveal();
                return;
            }
        }
    }

    /// Puts the cursor on the first (`true`) or last choosable item.
    fn edge(&mut self, first: bool) {
        let found = if first {
            self.items.iter().position(|item| item.choosable)
        } else {
            self.items.iter().rposition(|item| item.choosable)
        };
        if found.is_some() {
            self.cursor = found;
            self.reveal();
        }
    }

    /// Typed `letter`: jumps to the next item whose name starts with what
    /// was typed lately.
    fn type_ahead(&mut self, letter: &str, now: Instant) {
        let fresh = self
            .typed_at
            .is_none_or(|at| now.duration_since(at) > TYPE_AHEAD);
        if fresh {
            self.typed.clear();
        }
        self.typed.push_str(&letter.to_lowercase());
        self.typed_at = Some(now);
        let count = self.items.len();
        // One letter goes on to the next match; more letters refine the
        // current one first.
        let start = match self.cursor {
            Some(cursor) if self.typed.chars().count() > 1 => cursor,
            Some(cursor) => cursor + 1,
            None => 0,
        };
        let typed = self.typed.clone();
        let found = (0..count)
            .map(|offset| (start + offset) % count)
            .find(|&index| {
                let item = &self.items[index];
                item.choosable && item.label.to_lowercase().starts_with(&typed)
            });
        if found.is_some() {
            self.cursor = found;
            self.reveal();
        }
    }

    /// Scrolls a select's list to the cursor.
    fn reveal(&self) {
        if let (Some(scroll), Some(cursor)) = (&self.scroll, self.cursor) {
            scroll.scroll_to_item(self.items[cursor].entry);
        }
    }

    /// The handler of the item under the cursor.
    fn chosen(&self) -> Option<SharedClick> {
        let item = self.items.get(self.cursor?)?;
        item.choosable.then(|| item.on_choose.clone()).flatten()
    }
}

/// A shown popup: its dismissal handler and (a menu's) keyboard, kept
/// current by each render. The popup's element state holds it: it goes
/// when the popup is no longer drawn.
struct ShownPopup {
    window: AnyWindowHandle,
    view: Option<EntityId>,
    on_dismiss: Option<DismissHandler>,
    nav: Option<Nav>,
    on_key: Option<KeyHandler>,
}

/// The popups drawn, oldest first (dead ones are pruned as they're found).
#[derive(Default)]
struct ShownPopups(Vec<WeakEntity<ShownPopup>>);

impl Global for ShownPopups {}

/// Registers a popup drawn this frame (keyed `key`): it closes on Escape
/// through `on_dismiss`; a menu (`initial`: where
/// the first arrow puts it, if anywhere) takes the arrows, Enter and typed
/// letters.
fn register_popup(
    key: ElementId,
    on_dismiss: Option<DismissHandler>,
    initial: Option<usize>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<ShownPopup> {
    let shown = window.use_keyed_state(key, cx, move |window, cx| {
        let popup = cx.weak_entity();
        let shown = cx.default_global::<ShownPopups>();
        shown.0.retain(|popup| popup.upgrade().is_some());
        shown.0.push(popup);
        ShownPopup {
            window: window.window_handle(),
            view: None,
            on_dismiss: None,
            nav: Some(Nav {
                anchor: initial,
                ..Nav::default()
            }),
            on_key: None,
        }
    });
    shown.update(cx, |shown, _| shown.on_dismiss = on_dismiss);
    shown
}

/// The newest popup shown in `window`.
fn newest_popup(window: &Window, cx: &App) -> Option<Entity<ShownPopup>> {
    let here = window.window_handle();
    cx.try_global::<ShownPopups>().and_then(|shown| {
        shown
            .0
            .iter()
            .rev()
            .filter_map(WeakEntity::upgrade)
            .find(|popup| popup.read(cx).window == here)
    })
}

/// The newest popup shown in a window takes Escape (it closes) and, a
/// menu, the arrows, Home, End, Enter and typed letters, before anything
/// else sees them. Other keys go on as usual: the list behind keeps its
/// keys. From [`crate::init`].
pub(crate) fn init(cx: &mut App) {
    cx.default_global::<ShownPopups>();
    cx.intercept_keystrokes(|event, window, cx| {
        let keystroke = &event.keystroke;
        let modifiers = keystroke.modifiers;
        if modifiers.control || modifiers.alt || modifiers.platform || modifiers.function {
            return;
        }
        let Some(popup) = newest_popup(window, cx) else {
            return;
        };
        if keystroke.key == "escape" {
            if modifiers.shift {
                return;
            }
            if let Some(on_dismiss) = popup.read(cx).on_dismiss.clone() {
                cx.stop_propagation();
                on_dismiss(&Dismissal::Escape, window, cx);
            }
            return;
        }
        if let Some(on_key) = popup.read(cx).on_key.clone() {
            if on_key(keystroke, window, cx) {
                cx.stop_propagation();
            }
            return;
        }
        if popup.read(cx).nav.is_none() {
            return;
        }
        let view = popup.read(cx).view;
        let key = keystroke.key.as_str();
        let letter = keystroke
            .key_char
            .clone()
            .filter(|text| text.chars().count() == 1 && !text.trim().is_empty());
        let outcome = popup.update(cx, |popup, _| {
            let Some(nav) = &mut popup.nav else {
                return Handled::No;
            };
            match key {
                "down" => nav.step(1),
                "up" => nav.step(-1),
                "home" => nav.edge(true),
                "end" => nav.edge(false),
                "enter" => {
                    return match nav.chosen() {
                        Some(handler) => Handled::Choose(handler),
                        None => Handled::No,
                    };
                }
                _ => match &letter {
                    Some(letter) if !modifiers.shift || letter.chars().all(char::is_alphabetic) => {
                        nav.type_ahead(letter, Instant::now());
                    }
                    _ => return Handled::No,
                },
            }
            Handled::Moved
        });
        match outcome {
            Handled::No => {}
            Handled::Moved => {
                cx.stop_propagation();
                if let Some(view) = view {
                    cx.notify(view);
                }
            }
            Handled::Choose(handler) => {
                cx.stop_propagation();
                let event = ClickEvent::Keyboard(KeyboardClickEvent {
                    button: KeyboardButton::Enter,
                    bounds: Bounds::default(),
                });
                handler(&event, window, cx);
                if let Some(view) = view {
                    cx.notify(view);
                }
            }
        }
    })
    .detach();
}

/// What a key did to a menu.
enum Handled {
    No,
    Moved,
    Choose(SharedClick),
}

/// A shown popover (`content`) that closes itself: Escape (before the view
/// behind sees it) or a press outside it calls `on_dismiss`, which closes
/// it. Menus do this themselves ([`Menu::on_dismiss`]).
#[derive(IntoElement)]
#[must_use = "a popup does nothing unless rendered"]
pub struct Dismissable {
    id: ElementId,
    content: AnyElement,
    on_dismiss: DismissHandler,
    on_key: Option<KeyHandler>,
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
            on_key: None,
        }
    }

    /// Gives the popover the keys first (before the field it holds and
    /// anything else, Escape aside): `handler` returns whether it used the
    /// key (an icon picker's arrows and Enter).
    pub fn on_key(
        mut self,
        handler: impl Fn(&Keystroke, &mut Window, &mut App) -> bool + 'static,
    ) -> Self {
        self.on_key = Some(Rc::new(handler));
        self
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
        let popup = register_popup(self.id.clone(), Some(on_dismiss.clone()), None, window, cx);
        // A popover's own content takes the keyboard (its fields), after
        // its key handler.
        let on_key = self.on_key;
        popup.update(cx, |popup, _| {
            popup.nav = None;
            popup.on_key = on_key;
        });
        div()
            .id(self.id)
            .on_mouse_down_out(move |event, window, cx| {
                on_dismiss(&Dismissal::Press(event.position), window, cx);
            })
            .child(self.content)
    }
}

/// Floats `content` (a [`Menu`], or a popover's card) by the element it
/// is a child of, on top of everything else: an action menu hangs 4px
/// under its trigger, aligned to its left edge or ([`Popover::align_right`])
/// its right edge, and stands 4px above it when there is no room below.
/// The parent must be `relative()`; its trigger is drawn pressed by its
/// owner while the menu is open.
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
    side: Side,
    gap: Pixels,
    outset: Point<Pixels>,
}

impl Popover {
    /// A popover under the parent's left edge.
    pub fn new(content: impl IntoElement) -> Self {
        Self {
            content: content.into_any_element(),
            align_right: false,
            side: Side::Below,
            gap: px(MENU_GAP),
            outset: Point::default(),
        }
    }

    /// Measures from the trigger's pressed background where it reaches
    /// `x` and `y` past the parent's box (a [`GlyphButton::bleed`] or a
    /// header's sort word): the menu lines up with what is drawn.
    ///
    /// [`GlyphButton::bleed`]: crate::GlyphButton::bleed
    pub fn outset(mut self, x: Pixels, y: Pixels) -> Self {
        self.outset = point(x, y);
        self
    }

    /// Opens it above the parent (under it only when there is no room
    /// above): for triggers at the bottom of the window, like the sidebar
    /// footer's.
    pub fn above(mut self) -> Self {
        self.side = Side::Above;
        self
    }

    /// Shows it left of the parent, centred on it (a preview beside a
    /// list's row).
    pub fn left(mut self) -> Self {
        self.side = Side::Left;
        self
    }

    /// Aligns the popover's right edge with the parent's: for a trigger on
    /// the right of its row or header.
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
            .field("side", &self.side)
            .field("gap", &self.gap)
            .finish_non_exhaustive()
    }
}

impl RenderOnce for Popover {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        Float::new(self.side)
            .align_right(self.align_right)
            .gap(self.gap)
            .outset(self.outset)
            .kind(FloatKind::Popover)
            .child(self.content)
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
        assert_eq!(menu.section_labels(), ["sort by"]);
        assert_eq!(menu.checked_index(), Some(0));
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
        let item = MenuItem::new("edit", "edit dashboard")
            .disabled(true)
            .on_click(|_, _, _| {});
        assert!(item.is_disabled());
        assert!(!item.choosable());
        assert!(!item.is_checked());
        assert_eq!(item.label(), "edit dashboard");
        assert!(MenuItem::new("on", "on").checked(true).is_checked());
        assert!(MenuItem::new("go", "go").on_click(|_, _, _| {}).choosable());
    }

    #[test]
    fn scrolled_items_count_as_items() {
        let menu = Menu::new("environments")
            .item(MenuItem::new("first", "first"))
            .scrolled(
                "list",
                vec![
                    MenuItem::new("a", "a").checked(true),
                    MenuItem::new("b", "b"),
                ],
                px(56.),
            )
            .status_popover(px(320.));
        assert_eq!(menu.item_labels(), ["first", "a", "b"]);
        assert!(menu.has_check_column());
        assert_eq!(menu.width, Some(px(320.)));
        assert_eq!(menu.checked_index(), Some(1));
    }

    #[test]
    fn menus_measure_their_entries() {
        let theme = Theme::dark();
        let item = theme.metrics.menu_item_height;
        let menu = Menu::new("m")
            .item(MenuItem::new("a", "a"))
            .item(MenuItem::new("b", "b"))
            .separator();
        assert_eq!(menu.content_height(&theme), item * 2. + px(9. + 8.));
    }

    fn nav(labels: &[(&str, bool)]) -> Nav {
        Nav {
            items: labels
                .iter()
                .enumerate()
                .map(|(entry, (label, choosable))| NavItem {
                    label: SharedString::from((*label).to_owned()),
                    choosable: *choosable,
                    on_choose: None,
                    entry,
                })
                .collect(),
            ..Nav::default()
        }
    }

    #[test]
    fn the_cursor_skips_disabled_items_and_wraps() {
        let mut nav = nav(&[("state", false), ("icon", true), ("other", true)]);
        nav.step(1);
        assert_eq!(
            nav.cursor,
            Some(1),
            "down from nothing: the first choosable"
        );
        nav.step(1);
        assert_eq!(nav.cursor, Some(2));
        nav.step(1);
        assert_eq!(nav.cursor, Some(1), "wraps past the disabled item");
        nav.step(-1);
        assert_eq!(nav.cursor, Some(2));
        nav.edge(true);
        assert_eq!(nav.cursor, Some(1));
        nav.edge(false);
        assert_eq!(nav.cursor, Some(2));
    }

    #[test]
    fn a_selects_first_arrow_lands_on_its_value() {
        let mut nav = nav(&[("a", true), ("b", true), ("c", true)]);
        nav.anchor = Some(1);
        assert_eq!(nav.cursor, None, "nothing is highlighted on opening");
        nav.step(1);
        assert_eq!(nav.cursor, Some(1));
        nav.step(1);
        assert_eq!(nav.cursor, Some(2));
    }

    #[test]
    fn typing_jumps_to_a_name() {
        let mut nav = nav(&[
            ("severity", true),
            ("last state change", true),
            ("host", true),
            ("service", true),
        ]);
        let start = Instant::now();
        nav.type_ahead("s", start);
        assert_eq!(nav.cursor, Some(0));
        nav.type_ahead("e", start + Duration::from_millis(100));
        assert_eq!(nav.cursor, Some(0), "`se` stays on severity");
        nav.type_ahead("r", start + Duration::from_millis(200));
        nav.type_ahead("vi", start + Duration::from_millis(300));
        assert_eq!(nav.cursor, Some(3), "`servi` is service");
        nav.type_ahead("h", start + Duration::from_secs(5));
        assert_eq!(nav.cursor, Some(2), "a pause starts a new name");
    }

    #[test]
    fn popovers_default_to_the_left_edge_below() {
        let popover = Popover::new(div()).gap(px(2.));
        assert!(!popover.align_right);
        assert_eq!(popover.side, Side::Below);
        assert_eq!(popover.gap, px(2.));
        assert!(Popover::new(div()).align_right().align_right);
        assert_eq!(Popover::new(div()).above().side, Side::Above);
        assert_eq!(Popover::new(div()).left().side, Side::Left);
    }
}
