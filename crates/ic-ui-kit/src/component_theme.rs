//! Restyles gpui-component (text inputs, its window frame on Linux, menus)
//! with our [`Theme`], so its controls look like the rest of the app.

use gpui::{App, px};
use gpui_component::{Theme as ComponentTheme, ThemeMode};

use crate::theme::Theme;

/// gpui-component's base font size; GPUI's rem size follows it. Our own
/// components use pixel sizes, so this only affects gpui-component controls.
const BASE_FONT_SIZE: f32 = 16.;

/// Installs `theme`'s colours, font and radii into gpui-component's theme.
pub(crate) fn apply(theme: &Theme, cx: &mut App) {
    ComponentTheme::change(ThemeMode::Dark, None, cx);
    let colors = theme.colors;
    let states = theme.states;
    let font_family = theme.font_family.clone();
    let radius = theme.metrics.button_radius;
    ComponentTheme::update(cx, move |component| {
        component.font_family = font_family.clone();
        component.mono_font_family = font_family;
        component.font_size = px(BASE_FONT_SIZE);
        component.radius = radius;
        component.radius_lg = radius * 2.;
        component.shadow = true;

        component.colors.background = colors.window_background;
        component.colors.foreground = colors.text;
        component.colors.muted = colors.element_background;
        component.colors.muted_foreground = colors.text_muted;
        component.colors.border = colors.border_window;
        component.colors.input = colors.border_window;
        component.colors.ring = colors.accent;
        component.colors.caret = colors.accent;
        component.colors.selection = colors.selection;

        component.colors.primary = colors.accent;
        component.colors.primary_hover = colors.accent_button_hover;
        component.colors.primary_active = colors.accent;
        component.colors.primary_foreground = colors.on_accent;
        component.colors.secondary = colors.element_background;
        component.colors.secondary_hover = colors.element_hover;
        component.colors.secondary_active = colors.element_active;
        component.colors.secondary_foreground = colors.text;
        component.colors.accent = colors.element_hover;
        component.colors.accent_foreground = colors.text_strong;

        component.colors.popover = colors.pane_background;
        component.colors.popover_foreground = colors.text;
        component.colors.list = colors.window_background;
        component.colors.list_hover = colors.item_hover;
        component.colors.list_active = colors.row_selected;
        component.colors.list_active_border = colors.accent;

        component.colors.link = colors.accent;
        component.colors.link_hover = colors.accent_hover;
        component.colors.link_active = colors.accent;

        component.colors.danger = states.critical;
        component.colors.warning = states.warning;
        component.colors.success = states.ok;
        component.colors.info = colors.accent;

        component.colors.title_bar = colors.window_background;
        component.colors.title_bar_border = colors.border_header;
        component.colors.sidebar = colors.window_background;
        component.colors.sidebar_foreground = colors.text_secondary;
        component.colors.sidebar_border = colors.border_split;
        component.colors.scrollbar_thumb = colors.element_active;
        component.colors.scrollbar_thumb_hover = colors.text_faint;
    });
}
