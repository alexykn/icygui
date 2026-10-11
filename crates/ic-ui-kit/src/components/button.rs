//! Buttons: the detail pane's action buttons with key hints, and icon-only
//! buttons for headers, rows and the sidebar footer.

use std::fmt;

use crate::px;
use gpui::{
    App, ClickEvent, ElementId, Hsla, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, Pixels, RenderOnce, Role, SharedString, StatefulInteractiveElement,
    Styled as _, Window, div, prelude::FluentBuilder as _,
};

use crate::components::Tooltip;
use crate::icon::{Icon, IconName};
use crate::theme::{ActiveTheme as _, Theme};

type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// A keyboard shortcut shown next to a button label (`acknowledge a`).
#[derive(Clone, Debug, IntoElement)]
#[must_use = "a key hint does nothing unless rendered"]
pub struct KeyHint {
    key: SharedString,
    color: Option<Hsla>,
}

impl KeyHint {
    /// A hint for `key`, in the faint text colour.
    pub fn new(key: impl Into<SharedString>) -> Self {
        Self {
            key: key.into(),
            color: None,
        }
    }

    /// Sets the colour (the primary button uses a darker accent).
    pub fn color(mut self, color: impl Into<Hsla>) -> Self {
        self.color = Some(color.into());
        self
    }
}

impl RenderOnce for KeyHint {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .flex_none()
            .text_size(theme.text.hint)
            .text_color(self.color.unwrap_or(theme.colors.text_faint))
            .child(self.key)
    }
}

/// The look of a [`Button`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ButtonVariant {
    /// The main action of a pane (accent background).
    Primary,
    /// Every other action.
    #[default]
    Secondary,
    /// A destructive action that can't be undone (delete), confirmed in a
    /// dialog: the critical colour.
    Danger,
}

/// Resolved colours of a button variant.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ButtonColors {
    /// Background at rest.
    pub background: Hsla,
    /// Background under the mouse.
    pub hover: Hsla,
    /// Background while pressed.
    pub active: Hsla,
    /// Label colour.
    pub foreground: Hsla,
    /// Key hint colour.
    pub key: Hsla,
}

impl ButtonVariant {
    /// This variant's colours in `theme`.
    #[must_use]
    pub fn colors(self, theme: &Theme) -> ButtonColors {
        let colors = &theme.colors;
        match self {
            Self::Primary => ButtonColors {
                background: colors.accent,
                hover: colors.accent_button_hover,
                active: colors.accent,
                foreground: colors.on_accent,
                key: colors.on_accent_muted,
            },
            Self::Secondary => ButtonColors {
                background: colors.element_background,
                hover: colors.element_hover,
                active: colors.element_active,
                foreground: colors.text,
                key: colors.text_faint,
            },
            Self::Danger => ButtonColors {
                background: theme.states.fill.critical,
                hover: theme.states.fill.critical.opacity(0.85),
                active: theme.states.fill.critical,
                foreground: colors.on_accent,
                key: colors.on_accent_muted,
            },
        }
    }
}

/// Space left and right of a [`Button`]'s content.
const BUTTON_PADDING: f32 = 10.;
/// Space between a [`Button`]'s icon, label and key hint.
const BUTTON_GAP: f32 = 8.;

/// A labelled action button (28px high), optionally with an icon and a key
/// hint: `acknowledge a`.
#[derive(IntoElement)]
#[must_use = "a button does nothing unless rendered"]
pub struct Button {
    id: ElementId,
    label: SharedString,
    variant: ButtonVariant,
    icon: Option<IconName>,
    key: Option<SharedString>,
    /// The key hint's room is kept, nothing shown in it.
    key_blank: bool,
    width: Option<Pixels>,
    tooltip: Option<Tooltip>,
    disabled: bool,
    on_click: Option<ClickHandler>,
}

impl Button {
    /// A secondary button.
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            variant: ButtonVariant::default(),
            icon: None,
            key: None,
            key_blank: false,
            width: None,
            tooltip: None,
            disabled: false,
            on_click: None,
        }
    }

    /// The width of a button showing `label` and, with `key`, a key hint of
    /// one character (no icon): for a [`Button::width`] that fits every
    /// label a button shows in turn.
    pub fn width_for(theme: &Theme, label: &str, key: bool) -> Pixels {
        #[expect(
            clippy::cast_precision_loss,
            reason = "labels are far shorter than 2^23 characters"
        )]
        let chars = label.chars().count() as f32;
        let hint = if key {
            px(BUTTON_GAP) + theme.text.hint * crate::theme::CHAR_WIDTH
        } else {
            px(0.)
        };
        theme.text.body * (chars * crate::theme::CHAR_WIDTH) + hint + px(2. * BUTTON_PADDING)
    }

    /// Uses the accent style for the pane's main action.
    pub fn primary(mut self) -> Self {
        self.variant = ButtonVariant::Primary;
        self
    }

    /// Sets the variant.
    pub fn variant(mut self, variant: ButtonVariant) -> Self {
        self.variant = variant;
        self
    }

    /// Shows an icon before the label.
    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Shows the key that runs this action after the label.
    pub fn key_hint(mut self, key: impl Into<SharedString>) -> Self {
        self.key = Some(key.into());
        self
    }

    /// Keeps the key hint's room but shows nothing in it (the key acts on
    /// something else for now), so the button keeps its width.
    pub fn key_blank(mut self, blank: bool) -> Self {
        self.key_blank = blank;
        self
    }

    /// Gives the button a fixed width with its content centred: a button
    /// whose label changes with the state keeps its size, and the buttons
    /// after it keep their places.
    pub fn width(mut self, width: Pixels) -> Self {
        self.width = Some(width);
        self
    }

    /// Shows a tooltip on hover (for example why the button is disabled).
    pub fn tooltip(mut self, tooltip: Tooltip) -> Self {
        self.tooltip = Some(tooltip);
        self
    }

    /// Greys the button out and ignores clicks.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Runs `handler` when the button is clicked.
    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

impl fmt::Debug for Button {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Button")
            .field("id", &self.id)
            .field("label", &self.label)
            .field("variant", &self.variant)
            .field("disabled", &self.disabled)
            .finish_non_exhaustive()
    }
}

impl RenderOnce for Button {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let colors = self.variant.colors(theme);
        let enabled = !self.disabled;
        div()
            .id(self.id)
            .role(Role::Button)
            .aria_label(self.label.clone())
            .flex()
            .flex_none()
            .items_center()
            .gap(px(BUTTON_GAP))
            .h(theme.metrics.button_height)
            .px(px(BUTTON_PADDING))
            .when_some(self.width, |button, width| button.w(width).justify_center())
            .rounded(theme.metrics.button_radius)
            .bg(colors.background)
            .text_color(colors.foreground)
            .text_size(theme.text.body)
            .whitespace_nowrap()
            .when_some(self.icon, |button, icon| {
                button.child(Icon::new(icon).size(theme.metrics.icon_small))
            })
            .child(self.label)
            .when_some(self.key, |button, key| {
                let color = if self.key_blank {
                    gpui::transparent_black()
                } else {
                    colors.key
                };
                button.child(KeyHint::new(key).color(color))
            })
            .when(self.disabled, |button| button.opacity(0.5))
            .when(enabled, |button| {
                button
                    .cursor_pointer()
                    .hover(|style| style.bg(colors.hover))
                    .active(|style| style.bg(colors.active))
            })
            // Keep keyboard focus where it is and don't start a window drag.
            .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            .when_some(self.tooltip, |button, tooltip| {
                button.tooltip(tooltip.builder())
            })
            .when_some(self.on_click.filter(|_| enabled), |button, handler| {
                // Handled here: rows and headers behind the button don't see it.
                button.on_click(move |event, window, cx| {
                    cx.stop_propagation();
                    handler(event, window, cx);
                })
            })
    }
}

/// An icon-only button with a hover background, for headers, group rows and
/// the sidebar footer.
#[derive(IntoElement)]
#[must_use = "a button does nothing unless rendered"]
pub struct IconButton {
    id: ElementId,
    icon: IconName,
    icon_size: Option<Pixels>,
    size: Option<Pixels>,
    color: Option<Hsla>,
    hover_color: Option<Hsla>,
    tooltip: Option<Tooltip>,
    selected: bool,
    disabled: bool,
    on_click: Option<ClickHandler>,
}

impl IconButton {
    /// An icon button with the default sizes.
    pub fn new(id: impl Into<ElementId>, icon: IconName) -> Self {
        Self {
            id: id.into(),
            icon,
            icon_size: None,
            size: None,
            color: None,
            hover_color: None,
            tooltip: None,
            selected: false,
            disabled: false,
            on_click: None,
        }
    }

    /// Sets the icon size (default: [`crate::Metrics::icon`]).
    pub fn icon_size(mut self, size: Pixels) -> Self {
        self.icon_size = Some(size);
        self
    }

    /// Sets the square hit area (default: [`crate::Metrics::icon_button`]).
    pub fn size(mut self, size: Pixels) -> Self {
        self.size = Some(size);
        self
    }

    /// Sets the icon colour (default: secondary text).
    pub fn color(mut self, color: impl Into<Hsla>) -> Self {
        self.color = Some(color.into());
        self
    }

    /// Sets the icon colour under the mouse (default: strong text).
    pub fn hover_color(mut self, color: impl Into<Hsla>) -> Self {
        self.hover_color = Some(color.into());
        self
    }

    /// Shows a tooltip on hover.
    pub fn tooltip(mut self, tooltip: Tooltip) -> Self {
        self.tooltip = Some(tooltip);
        self
    }

    /// Shows the button as toggled on.
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Greys the button out and ignores clicks.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Runs `handler` when the button is clicked.
    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

impl fmt::Debug for IconButton {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IconButton")
            .field("id", &self.id)
            .field("icon", &self.icon)
            .field("selected", &self.selected)
            .field("disabled", &self.disabled)
            .finish_non_exhaustive()
    }
}

impl RenderOnce for IconButton {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let metrics = theme.metrics;
        let colors = theme.colors;
        let color = self.color.unwrap_or(colors.text_secondary);
        let hover_color = self.hover_color.unwrap_or(colors.text_strong);
        let enabled = !self.disabled;
        let label = self.tooltip.as_ref().map(|tooltip| tooltip.title().clone());
        div()
            .id(self.id)
            .role(Role::Button)
            .when_some(label, StatefulInteractiveElement::aria_label)
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(self.size.unwrap_or(metrics.icon_button))
            .rounded(metrics.small_radius)
            .text_color(color)
            .when(self.selected, |button| button.bg(colors.element_hover))
            .child(Icon::new(self.icon).size(self.icon_size.unwrap_or(metrics.icon)))
            .when(self.disabled, |button| button.opacity(0.4))
            .when(enabled, |button| {
                button
                    .cursor_pointer()
                    .hover(|style| style.bg(colors.element_hover).text_color(hover_color))
                    .active(|style| style.bg(colors.element_active))
            })
            .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            .when_some(self.tooltip, |button, tooltip| {
                button.tooltip(tooltip.builder())
            })
            .when_some(self.on_click.filter(|_| enabled), |button, handler| {
                // Handled here: rows and headers behind the button don't see it.
                button.on_click(move |event, window, cx| {
                    cx.stop_propagation();
                    handler(event, window, cx);
                })
            })
    }
}

/// A text glyph that acts as a button, the way the design draws `×`, `+`
/// and `···`. The hover background and the hit area reach
/// [`GlyphButton::reach`] around the glyph; place the button with that in
/// mind, or let it [`bleed`](GlyphButton::bleed) into the space around it.
#[derive(IntoElement)]
#[must_use = "a button does nothing unless rendered"]
pub struct GlyphButton {
    id: ElementId,
    glyph: SharedString,
    text_size: Option<Pixels>,
    color: Option<Hsla>,
    hover_color: Option<Hsla>,
    tooltip: Option<Tooltip>,
    selected: bool,
    bleed: bool,
    on_click: Option<ClickHandler>,
}

/// How far a [`GlyphButton`]'s hit area reaches above and below the glyph.
const GLYPH_REACH_Y: f32 = 2.;

impl GlyphButton {
    /// How far the hit area and hover background reach left and right of
    /// the glyph, at the interface size.
    #[must_use]
    pub fn reach() -> Pixels {
        px(4.)
    }

    /// A button showing `glyph`.
    pub fn new(id: impl Into<ElementId>, glyph: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            glyph: glyph.into(),
            text_size: None,
            color: None,
            hover_color: None,
            tooltip: None,
            selected: false,
            bleed: false,
            on_click: None,
        }
    }

    /// Takes only the glyph's width in the layout: the reach overlaps the
    /// neighbours (negative margins). Use it for a direct child of a bar
    /// with a fixed height, so the glyph sits exactly where text would.
    pub fn bleed(mut self) -> Self {
        self.bleed = true;
        self
    }

    /// Sets the glyph's size (default: inherited).
    pub fn text_size(mut self, size: Pixels) -> Self {
        self.text_size = Some(size);
        self
    }

    /// Sets the colour (default: muted text).
    pub fn color(mut self, color: impl Into<Hsla>) -> Self {
        self.color = Some(color.into());
        self
    }

    /// Sets the colour under the mouse (default: strong text).
    pub fn hover_color(mut self, color: impl Into<Hsla>) -> Self {
        self.hover_color = Some(color.into());
        self
    }

    /// Shows a tooltip on hover; its title is also the accessible label.
    pub fn tooltip(mut self, tooltip: Tooltip) -> Self {
        self.tooltip = Some(tooltip);
        self
    }

    /// Shows the button as toggled on (its menu is open).
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Runs `handler` when the button is clicked.
    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

impl fmt::Debug for GlyphButton {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GlyphButton")
            .field("id", &self.id)
            .field("glyph", &self.glyph)
            .field("selected", &self.selected)
            .field("bleed", &self.bleed)
            .finish_non_exhaustive()
    }
}

impl RenderOnce for GlyphButton {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let color = self.color.unwrap_or(colors.text_muted);
        let hover_color = self.hover_color.unwrap_or(colors.text_strong);
        let label = self.tooltip.as_ref().map(|tooltip| tooltip.title().clone());
        div()
            .id(self.id)
            .role(Role::Button)
            .when_some(label, StatefulInteractiveElement::aria_label)
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            // Padding widens the hit area; with `bleed`, equal negative
            // margins keep the layout at the glyph's size.
            .px(Self::reach())
            .py(px(GLYPH_REACH_Y))
            .when(self.bleed, |button| button.mx(-Self::reach()))
            .rounded(theme.metrics.small_radius)
            .whitespace_nowrap()
            .when_some(self.text_size, gpui::Styled::text_size)
            .text_color(if self.selected { hover_color } else { color })
            .when(self.selected, |button| button.bg(colors.element_hover))
            .cursor_pointer()
            .hover(|style| style.bg(colors.element_hover).text_color(hover_color))
            .active(|style| style.bg(colors.element_active))
            .child(self.glyph)
            .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            .when_some(self.tooltip, |button, tooltip| {
                button.tooltip(tooltip.builder())
            })
            .when_some(self.on_click, |button, handler| {
                button.on_click(move |event, window, cx| {
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
    fn primary_buttons_use_the_accent() {
        let theme = Theme::dark();
        let colors = ButtonVariant::Primary.colors(&theme);
        assert_eq!(colors.background, theme.colors.accent);
        assert_eq!(colors.foreground, theme.colors.on_accent);
        assert_eq!(colors.key, theme.colors.on_accent_muted);
    }

    #[test]
    fn secondary_buttons_use_the_element_surface() {
        let theme = Theme::dark();
        let colors = ButtonVariant::Secondary.colors(&theme);
        assert_eq!(colors.background, theme.colors.element_background);
        assert_eq!(colors.foreground, theme.colors.text);
        assert_eq!(colors.key, theme.colors.text_faint);
        assert_ne!(colors.hover, colors.background);
    }

    #[test]
    fn builders_record_their_options() {
        let button = Button::new("ack", "acknowledge")
            .primary()
            .key_hint("a")
            .disabled(true)
            .on_click(|_, _, _| {});
        assert_eq!(button.variant, ButtonVariant::Primary);
        assert_eq!(button.key.as_deref(), Some("a"));
        assert!(button.disabled);
        assert!(button.on_click.is_some());
        assert!(format!("{button:?}").contains("acknowledge"));

        let icon = IconButton::new("add", IconName::Plus).selected(true);
        assert!(icon.selected);
        assert!(format!("{icon:?}").contains("Plus"));

        let glyph = GlyphButton::new("close", "×")
            .text_size(px(15.))
            .selected(true)
            .bleed()
            .on_click(|_, _, _| {});
        assert!(glyph.selected);
        assert!(glyph.bleed);
        assert_eq!(glyph.glyph, "×");
        assert!(glyph.on_click.is_some());
        assert!(format!("{glyph:?}").contains('×'));
    }
}
