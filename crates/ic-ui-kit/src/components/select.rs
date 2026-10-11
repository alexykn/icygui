//! A select: a field showing the current value, whose list opens FROM it
//! (README *Dropdowns: one system*). Open, the field and its list are one
//! shape: the list is exactly as wide as the field and joins its bottom
//! edge (or its top edge, opening upward when there is no room below), a
//! shared 1px accent border with a hairline between them, the field's
//! bottom corners square, the list's rounded, the shadow under the list
//! only, the field's background. The names sit on the field's own 11px
//! text inset, so the current value is right over its own row, ticked in
//! the chevron's column. At most ten rows show, then it scrolls.

use std::fmt;

use crate::px;
use gpui::{
    App, ClickEvent, ElementId, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, Pixels, RenderOnce, Role, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, div, prelude::FluentBuilder as _,
};

use crate::components::float::{Float, FloatKind, Measured, Side};
use crate::components::menu::{Menu, SELECT_ROWS, Shape};
use crate::icon::{Icon, IconName};
use crate::theme::{ActiveTheme as _, Theme};

type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// Room kept between an open list and the window's edge.
const WINDOW_MARGIN: f32 = 8.;

/// See the module notes. The owner keeps whether it is open (a click on
/// the field calls [`Select::on_click`]) and gives the list as a
/// [`Menu`] whose items are the choices, the current one
/// [`checked`](crate::MenuItem::checked).
#[derive(IntoElement)]
#[must_use = "a select does nothing unless rendered"]
pub struct Select {
    id: ElementId,
    value: SharedString,
    icon: Option<IconName>,
    dot: Option<gpui::Hsla>,
    open: bool,
    disabled: bool,
    text_size: Option<Pixels>,
    menu: Option<Menu>,
    on_click: Option<ClickHandler>,
}

impl Select {
    /// A select showing `value`.
    pub fn new(id: impl Into<ElementId>, value: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            value: value.into(),
            icon: None,
            dot: None,
            open: false,
            disabled: false,
            text_size: None,
            menu: None,
            on_click: None,
        }
    }

    /// Shows `icon` before the value (a view's display).
    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Shows a status dot in `color` before the value, in the slot its
    /// list's items show theirs (an environment's health), so the value
    /// sits over its own row.
    pub fn dot(mut self, color: gpui::Hsla) -> Self {
        self.dot = Some(color);
        self
    }

    /// Shows the list (`menu`), with the field in its open state.
    pub fn open(mut self, open: bool) -> Self {
        self.open = open;
        self
    }

    /// The list's items (needed only while open).
    pub fn menu(mut self, menu: Menu) -> Self {
        self.menu = Some(menu);
        self
    }

    /// Greys the field out and ignores clicks.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// The value's text size (default: the theme's `row`, 13px).
    pub fn text_size(mut self, size: Pixels) -> Self {
        self.text_size = Some(size);
        self
    }

    /// Runs `handler` when the field is clicked (open or close it).
    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }

    /// The value shown.
    #[must_use]
    pub fn value(&self) -> &SharedString {
        &self.value
    }
}

impl fmt::Debug for Select {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Select")
            .field("id", &self.id)
            .field("value", &self.value)
            .field("open", &self.open)
            .finish_non_exhaustive()
    }
}

/// Where a list `list_height` tall opens from a field at `field` (its
/// bounds last frame, if any) in a window `viewport` high: upward, and how
/// tall it may be.
pub(crate) fn list_placement(
    field: Option<gpui::Bounds<Pixels>>,
    viewport: Pixels,
    list_height: Pixels,
    theme: &Theme,
) -> (bool, Pixels) {
    #[expect(clippy::cast_precision_loss, reason = "ten rows")]
    let cap = theme.metrics.menu_item_height * SELECT_ROWS as f32 + px(8.);
    let wanted = list_height.min(cap);
    let Some(field) = field else {
        return (false, cap);
    };
    let margin = px(WINDOW_MARGIN);
    let below = viewport - margin - field.bottom();
    let above = field.top() - margin;
    let up = wanted > below && above > below;
    let room = if up { above } else { below };
    (
        up,
        cap.min(room.max(theme.metrics.menu_item_height + px(8.))),
    )
}

/// What a field shows before its value: an icon, or a status dot in the
/// 13px slot its list's items use.
fn leading(
    icon: Option<IconName>,
    dot: Option<gpui::Hsla>,
    theme: &Theme,
) -> Option<gpui::AnyElement> {
    if let Some(icon) = icon {
        return Some(
            Icon::new(icon)
                .size(px(13.))
                .color(theme.colors.text_muted)
                .into_any_element(),
        );
    }
    dot.map(|color| {
        div()
            .flex()
            .flex_none()
            .justify_center()
            .w(px(13.))
            .child(
                div()
                    .size(theme.metrics.status_dot)
                    .rounded_full()
                    .bg(color),
            )
            .into_any_element()
    })
}

/// The hairline between an open field and its list, over the field's
/// accent border on the list's side (the field's bottom edge, its top
/// edge when the list opens upward).
fn hairline(up: bool, theme: &Theme) -> gpui::Div {
    let rule = crate::Metrics::RULE;
    div()
        .absolute()
        .left_0()
        .right_0()
        .h(rule)
        .map(|line| {
            if up {
                line.top(-rule)
            } else {
                line.bottom(-rule)
            }
        })
        .bg(theme.colors.border_header)
}

impl RenderOnce for Select {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme().clone();
        let colors = theme.colors;
        let measured: Measured = window
            .use_keyed_state(
                ElementId::NamedChild(std::sync::Arc::new(self.id.clone()), "field".into()),
                cx,
                |_, _| Measured::default(),
            )
            .read(cx)
            .clone();
        let enabled = !self.disabled;
        let open = self.open && enabled && self.menu.is_some();
        let list_height = self
            .menu
            .as_ref()
            .map_or(px(0.), |menu| menu.content_height(&theme));
        let (up, max_height) = list_placement(
            measured.get(),
            window.viewport_size().height,
            list_height,
            &theme,
        );
        let width = measured.get().map(|bounds| bounds.size.width);
        let radius = theme.metrics.code_radius;
        let field = div()
            .id(self.id.clone())
            .role(Role::ComboBox)
            .aria_label(self.value.clone())
            .relative()
            .flex()
            .items_center()
            .gap(px(8.))
            .h(theme.metrics.field_height)
            .px(px(10.))
            .border_1()
            .border_color(if open {
                colors.accent
            } else {
                colors.border_header
            })
            .map(|field| match (open, up) {
                (false, _) => field.rounded(radius),
                (true, false) => field.rounded_t(radius),
                (true, true) => field.rounded_b(radius),
            })
            .bg(colors.code_background)
            .text_size(self.text_size.unwrap_or(theme.text.row))
            .text_color(colors.text)
            .whitespace_nowrap()
            .children(leading(self.icon, self.dot, &theme))
            .child(div().flex_1().min_w_0().truncate().child(self.value))
            .child(
                Icon::new(if open {
                    IconName::ChevronUp
                } else {
                    IconName::ChevronDown
                })
                .size(px(13.))
                .color(if open { colors.text } else { colors.text_muted }),
            )
            .when(open, |field| field.child(hairline(up, &theme)))
            .when(self.disabled, |field| field.opacity(0.5))
            .when(enabled, gpui::Styled::cursor_pointer)
            .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            .when_some(self.on_click.filter(|_| enabled), |field, handler| {
                field.on_click(move |event, window, cx| {
                    cx.stop_propagation();
                    handler(event, window, cx);
                })
            });
        let list = self.menu.filter(|_| open).map(|menu| {
            menu.build(
                Shape::List {
                    up,
                    max_height,
                    width,
                },
                window,
                cx,
            )
        });
        div().relative().min_w_0().child(field).child(
            Float::new(if up {
                Side::StrictlyAbove
            } else {
                Side::StrictlyBelow
            })
            .report(measured)
            .kind(FloatKind::SelectList)
            .map(|float| match list {
                Some(list) => float.child(list),
                None => float,
            }),
        )
    }
}

#[cfg(test)]
#[expect(
    clippy::disallowed_methods,
    reason = "window geometry is in real pixels"
)]
mod tests {
    use gpui::{Bounds, point, size};

    use super::*;
    use crate::MenuItem;

    #[test]
    fn a_list_opens_upward_only_without_room_below() {
        let theme = Theme::dark();
        let item = theme.metrics.menu_item_height;
        let field = |y: f32| {
            Some(Bounds::new(
                point(gpui::px(10.), gpui::px(y)),
                size(gpui::px(200.), gpui::px(32.)),
            ))
        };
        let list = item * 4. + gpui::px(8.);
        let (up, height) = list_placement(field(100.), gpui::px(800.), list, &theme);
        assert!(!up);
        assert_eq!(
            height,
            item * 10. + gpui::px(8.),
            "the cap; the list is shorter"
        );
        let (up, height) = list_placement(field(740.), gpui::px(800.), list, &theme);
        assert!(up, "no room under a field at the window's bottom");
        assert_eq!(height, item * 10. + gpui::px(8.));
        let (up, _) = list_placement(None, gpui::px(800.), list, &theme);
        assert!(!up, "before the field was laid out: down");
    }

    #[test]
    fn selects_keep_their_value_and_list() {
        let select = Select::new("sort", "severity")
            .open(true)
            .menu(Menu::new("sort-list").item(MenuItem::new("severity", "severity").checked(true)));
        assert_eq!(select.value(), "severity");
        assert!(format!("{select:?}").contains("severity"));
    }
}
