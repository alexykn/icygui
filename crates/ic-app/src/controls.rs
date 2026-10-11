//! Controls every view shares in its header (topic 14, round 5; README,
//! *View controls* and *Row density per view*): the rows toggle of a
//! one-view header, the *rows* group of a stacked view's `···`, and what
//! they say. One component for every view kind, so nothing drifts.
//!
//! A list-like view's rows follow Settings → appearance → *row density*
//! until a density is chosen on the view ([`ic_config::View::density`]);
//! *follow the default* removes the choice.

use std::rc::Rc;

use gpui::{
    AnyElement, App, ClickEvent, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    prelude::FluentBuilder as _,
};
use ic_config::RowDensity;
use ic_ui_kit::{Density, Icon, IconName, Menu, MenuItem, Theme, Tooltip, px};

/// What picking a density does: `None` follows the settings again.
pub(crate) type PickDensity = Rc<dyn Fn(Option<RowDensity>, &mut Window, &mut App)>;

/// The density a view's rows have: its own, else the settings'.
pub(crate) fn effective(chosen: Option<RowDensity>, global: RowDensity) -> RowDensity {
    chosen.unwrap_or(global)
}

/// The theme's density for `density`.
pub(crate) fn ui_density(density: RowDensity) -> Density {
    match density {
        RowDensity::Comfortable => Density::Comfortable,
        RowDensity::Compact => Density::Compact,
    }
}

/// `theme` with rows at `density` (a view's own rows).
pub(crate) fn theme_for(theme: &Theme, density: RowDensity) -> Theme {
    theme.with_density(ui_density(density))
}

/// A density's word: `comfortable`, `compact`.
pub(crate) fn word(density: RowDensity) -> &'static str {
    match density {
        RowDensity::Comfortable => "comfortable",
        RowDensity::Compact => "compact",
    }
}

/// What the editor's *rows* field and the toggle's tooltip say: `as in
/// settings (comfortable)`, or the density chosen on the view.
pub(crate) fn label(chosen: Option<RowDensity>, global: RowDensity) -> String {
    match chosen {
        None => format!("as in settings ({})", word(global)),
        Some(density) => word(density).to_owned(),
    }
}

/// The rows toggle of a one-view header (14-r5-d): two icons, spacious
/// rows and compact rows, in a fixed slot left of the sort. The density
/// chosen on the view is filled, as a segmented control's *on*; while the
/// view follows the settings, the settings' density has a dashed inset
/// outline. A click on the chosen one goes back to the settings.
pub(crate) fn rows_toggle(
    id: impl Into<SharedString>,
    chosen: Option<RowDensity>,
    global: RowDensity,
    on_pick: &PickDensity,
    theme: &Theme,
) -> AnyElement {
    let colors = theme.colors;
    let id: SharedString = id.into();
    let tooltip = match chosen {
        None => format!("rows: {}", label(None, global)),
        Some(density) => format!("rows: {} (set on this view)", word(density)),
    };
    let cell = |density: RowDensity, icon: IconName| {
        let on = chosen == Some(density);
        let following = chosen.is_none() && density == global;
        let pick = Rc::clone(on_pick);
        div()
            .id(SharedString::from(format!("{id}-{}", word(density))))
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .w(px(28.))
            .h_full()
            .cursor_pointer()
            .when(on, |cell| cell.bg(colors.element_background))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(20.))
                    .rounded(px(3.))
                    .when(following, |icon| {
                        icon.border_1()
                            .border_dashed()
                            .border_color(colors.text_faint)
                    })
                    .child(Icon::new(icon).size(px(13.)).color(if on || following {
                        colors.text
                    } else {
                        colors.text_muted
                    })),
            )
            .hover(|style| style.bg(colors.element_hover))
            .on_mouse_down(MouseButton::Left, |_, window, cx| {
                window.prevent_default();
                cx.stop_propagation();
            })
            .on_click(move |_: &ClickEvent, window, cx| {
                cx.stop_propagation();
                // The chosen one again: follow the settings.
                pick(if on { None } else { Some(density) }, window, cx);
            })
    };
    div()
        .id(id.clone())
        .flex()
        .flex_none()
        .h(px(26.))
        .rounded(theme.metrics.code_radius)
        .border_1()
        .border_color(colors.border_header)
        .overflow_hidden()
        .child(cell(RowDensity::Comfortable, IconName::Rows2))
        .child(div().w(px(1.)).h_full().bg(colors.border_header))
        .child(cell(RowDensity::Compact, IconName::Rows4))
        .tooltip(Tooltip::text(tooltip))
        .into_any_element()
}

/// The *rows* group of a view's `···` (14-r5-c): comfortable, compact,
/// and *follow the default* naming the settings' density, the current one
/// checked.
pub(crate) fn rows_menu(
    menu: Menu,
    id: &str,
    chosen: Option<RowDensity>,
    global: RowDensity,
    on_pick: &PickDensity,
) -> Menu {
    let item = |suffix: &str, text: String, icon: Option<IconName>, pick: Option<RowDensity>| {
        let handler = Rc::clone(on_pick);
        let item = MenuItem::new(SharedString::from(format!("{id}-rows-{suffix}")), text)
            .checked(chosen == pick)
            .on_click(move |_: &ClickEvent, window, cx| handler(pick, window, cx));
        match icon {
            Some(icon) => item.icon(icon),
            None => item,
        }
    };
    menu.label("rows")
        .item(item(
            "comfortable",
            "comfortable".to_owned(),
            Some(IconName::Rows2),
            Some(RowDensity::Comfortable),
        ))
        .item(item(
            "compact",
            "compact".to_owned(),
            Some(IconName::Rows4),
            Some(RowDensity::Compact),
        ))
        .item(item(
            "default",
            format!("follow the default ({})", word(global)),
            None,
            None,
        ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn views_follow_the_settings_until_they_choose() {
        assert_eq!(effective(None, RowDensity::Compact), RowDensity::Compact);
        assert_eq!(
            effective(Some(RowDensity::Comfortable), RowDensity::Compact),
            RowDensity::Comfortable
        );
        assert_eq!(
            label(None, RowDensity::Comfortable),
            "as in settings (comfortable)"
        );
        assert_eq!(
            label(Some(RowDensity::Compact), RowDensity::Comfortable),
            "compact"
        );
    }
}
