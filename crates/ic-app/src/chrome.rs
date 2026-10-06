//! Window chrome: the traffic-light controls at the left of the sidebar header
//! and the header areas that move the window.
//!
//! - **macOS:** `AppKit` draws the native traffic lights inside our transparent
//!   titlebar (`traffic_light_position`); we keep their space free and move
//!   the window ourselves (`app_owns_titlebar_drag`).
//! - **Linux, client-side decorations:** we draw the design's three circles
//!   (close, minimise, maximise) and move the window from the headers;
//!   gpui-component's `Root` draws the frame, its shadow and the resize edges.
//! - **Linux, server-side decorations** (a window manager without client-side
//!   decoration support, or no compositor): the window manager's titlebar has
//!   the controls, so we draw none.
//!
//! `ICYGUI_WINDOW_CONTROLS=always|never|auto` overrides the Linux choice, for
//! example to see the circles under Xvfb when generating screenshots.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    App, Decorations, Div, Global, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, Pixels, RenderOnce, Role, SharedString, Stateful,
    StatefulInteractiveElement as _, Styled as _, Window, div, prelude::FluentBuilder as _, px,
};
use ic_ui_kit::{ActiveTheme as _, Icon, IconName, Metrics, Tooltip};

/// The environment variable that overrides when the window controls show.
pub(crate) const CONTROLS_ENV: &str = "ICYGUI_WINDOW_CONTROLS";

/// The `ICYGUI_WINDOW_CONTROLS` setting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ControlsPreference {
    /// Draw controls when the window uses client-side decorations.
    #[default]
    Auto,
    /// Always draw controls (Linux).
    Always,
    /// Never draw controls (Linux).
    Never,
}

impl ControlsPreference {
    /// Parses the variable's value; anything unknown means [`Self::Auto`].
    pub(crate) fn parse(value: Option<&str>) -> Self {
        match value
            .map(|value| value.trim().to_ascii_lowercase())
            .as_deref()
        {
            Some("always" | "1" | "true" | "on") => Self::Always,
            Some("never" | "0" | "false" | "off") => Self::Never,
            _ => Self::Auto,
        }
    }

    /// Reads [`CONTROLS_ENV`].
    pub(crate) fn from_env() -> Self {
        Self::parse(std::env::var(CONTROLS_ENV).ok().as_deref())
    }
}

impl Global for ControlsPreference {}

/// What the window's leading header area holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Controls {
    /// Space for macOS's native traffic lights.
    NativeInset,
    /// Our own close, minimise and maximise circles.
    Custom,
    /// Nothing: the window manager draws the controls, or full screen.
    None,
}

impl Controls {
    /// The controls for `window` right now.
    pub(crate) fn of(window: &Window, cx: &App) -> Self {
        let preference = cx
            .try_global::<ControlsPreference>()
            .copied()
            .unwrap_or_default();
        Self::decide(
            cfg!(target_os = "macos"),
            window.is_fullscreen(),
            window.window_decorations(),
            preference,
        )
    }

    fn decide(
        macos: bool,
        fullscreen: bool,
        decorations: Decorations,
        preference: ControlsPreference,
    ) -> Self {
        if fullscreen {
            return Self::None;
        }
        if macos {
            return Self::NativeInset;
        }
        match (preference, decorations) {
            (ControlsPreference::Always, _)
            | (ControlsPreference::Auto, Decorations::Client { .. }) => Self::Custom,
            (ControlsPreference::Never, _) | (ControlsPreference::Auto, Decorations::Server) => {
                Self::None
            }
        }
    }

    /// Whether the headers move the window. On Linux only with our own
    /// controls: with server-side decorations the window manager's titlebar
    /// does that.
    pub(crate) fn drags_window(self) -> bool {
        self != Self::None
    }

    /// The width the controls take up.
    pub(crate) fn width(metrics: &Metrics) -> Pixels {
        metrics.window_control * 3. + metrics.window_control_gap * 2.
    }
}

/// The traffic lights, or the space macOS draws its own into.
#[derive(Clone, Copy, Debug, IntoElement)]
pub(crate) struct WindowControls {
    controls: Controls,
}

impl WindowControls {
    pub(crate) fn new(controls: Controls) -> Self {
        Self { controls }
    }
}

const CONTROLS_GROUP: &str = "window-controls";

#[derive(Clone, Copy, Debug)]
enum Control {
    Close,
    Minimize,
    Maximize,
}

impl RenderOnce for WindowControls {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let metrics = cx.theme().metrics;
        let width = Controls::width(&metrics);
        match self.controls {
            Controls::None => div().into_any_element(),
            Controls::NativeInset => div().flex_none().w(width).into_any_element(),
            Controls::Custom => {
                let supported = window.window_controls();
                div()
                    .group(CONTROLS_GROUP)
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(metrics.window_control_gap)
                    .child(control(Control::Close, window, cx))
                    .when(supported.minimize, |row| {
                        row.child(control(Control::Minimize, window, cx))
                    })
                    .when(supported.maximize, |row| {
                        row.child(control(Control::Maximize, window, cx))
                    })
                    .into_any_element()
            }
        }
    }
}

fn control(kind: Control, window: &Window, cx: &App) -> Stateful<Div> {
    let theme = cx.theme();
    let colors = theme.colors;
    let active = window.is_window_active();
    let (id, color, glyph, tooltip) = match kind {
        Control::Close => (
            "window-close",
            colors.traffic_close,
            IconName::Close,
            "Close",
        ),
        Control::Minimize => (
            "window-minimize",
            colors.traffic_minimize,
            IconName::Minus,
            "Minimize",
        ),
        Control::Maximize => (
            "window-maximize",
            colors.traffic_zoom,
            IconName::Maximize,
            "Maximize",
        ),
    };
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(theme.metrics.window_control)
        .rounded_full()
        .bg(if active {
            color
        } else {
            colors.traffic_inactive
        })
        .child(
            div()
                .invisible()
                .group_hover(CONTROLS_GROUP, gpui::Styled::visible)
                .child(Icon::new(glyph).size(px(8.)).color(colors.traffic_glyph)),
        )
        .role(Role::Button)
        .aria_label(tooltip)
        .tooltip(Tooltip::text(tooltip))
        // Don't let the header behind start a window move.
        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            match kind {
                Control::Close => window.remove_window(),
                Control::Minimize => window.minimize_window(),
                Control::Maximize => window.zoom_window(),
            }
        })
}

/// Turns header elements into window drag areas: dragging moves the window,
/// a double click maximises it (macOS: the user's titlebar double-click
/// action) and, on Linux, a right click opens the window menu.
///
/// Children that handle the mouse themselves (buttons, inputs) call
/// `window.prevent_default()` on mouse down, so pressing them never moves the
/// window. The move starts on the first mouse move, so double clicks still
/// arrive.
#[derive(Clone, Debug, Default)]
pub(crate) struct WindowDrag {
    pressed: Rc<Cell<bool>>,
}

impl WindowDrag {
    /// Adds the drag behaviour to `element` when `controls` call for it.
    pub(crate) fn attach(&self, element: Stateful<Div>, controls: Controls) -> Stateful<Div> {
        if !controls.drags_window() {
            return element;
        }
        let on_down = self.pressed.clone();
        let on_up = self.pressed.clone();
        let on_move = self.pressed.clone();
        element
            .on_mouse_down(MouseButton::Left, move |_, window, _| {
                on_down.set(!window.default_prevented());
            })
            .on_mouse_up(MouseButton::Left, move |_, _, _| on_up.set(false))
            .on_mouse_move(move |event, window, _| {
                if on_move.replace(false) && event.dragging() {
                    window.start_window_move();
                }
            })
            .on_click(|event, window, _| {
                if event.standard_click() && event.click_count() == 2 {
                    if cfg!(target_os = "macos") {
                        window.titlebar_double_click();
                    } else {
                        window.zoom_window();
                    }
                }
            })
            .when(cfg!(not(target_os = "macos")), |element| {
                element.on_mouse_down(MouseButton::Right, |event, window, _| {
                    if !window.default_prevented() {
                        window.show_window_menu(event.position);
                    }
                })
            })
    }
}

/// A name for the window title, shown by window switchers.
pub(crate) fn window_title(environment: Option<&str>, demo: bool) -> SharedString {
    match (environment, demo) {
        (Some(name), false) => format!("{name} — icygui").into(),
        (Some(name), true) => format!("{name} (demo) — icygui").into(),
        (None, _) => "icygui".into(),
    }
}

#[cfg(test)]
mod tests {
    use gpui::Tiling;

    use super::*;

    const CLIENT: Decorations = Decorations::Client {
        tiling: Tiling {
            top: false,
            left: false,
            right: false,
            bottom: false,
        },
    };

    #[test]
    fn preference_parsing_is_forgiving() {
        assert_eq!(ControlsPreference::parse(None), ControlsPreference::Auto);
        assert_eq!(
            ControlsPreference::parse(Some(" Always ")),
            ControlsPreference::Always
        );
        assert_eq!(
            ControlsPreference::parse(Some("1")),
            ControlsPreference::Always
        );
        assert_eq!(
            ControlsPreference::parse(Some("never")),
            ControlsPreference::Never
        );
        assert_eq!(
            ControlsPreference::parse(Some("off")),
            ControlsPreference::Never
        );
        assert_eq!(
            ControlsPreference::parse(Some("maybe")),
            ControlsPreference::Auto
        );
    }

    #[test]
    fn macos_keeps_room_for_the_native_traffic_lights() {
        for preference in [
            ControlsPreference::Auto,
            ControlsPreference::Always,
            ControlsPreference::Never,
        ] {
            assert_eq!(
                Controls::decide(true, false, Decorations::Server, preference),
                Controls::NativeInset
            );
        }
        assert_eq!(
            Controls::decide(true, true, Decorations::Server, ControlsPreference::Auto),
            Controls::None,
            "full screen hides the traffic lights"
        );
    }

    #[test]
    fn linux_draws_controls_only_for_client_side_decorations() {
        assert_eq!(
            Controls::decide(false, false, CLIENT, ControlsPreference::Auto),
            Controls::Custom
        );
        assert_eq!(
            Controls::decide(false, false, Decorations::Server, ControlsPreference::Auto),
            Controls::None
        );
        assert_eq!(
            Controls::decide(
                false,
                false,
                Decorations::Server,
                ControlsPreference::Always
            ),
            Controls::Custom
        );
        assert_eq!(
            Controls::decide(false, false, CLIENT, ControlsPreference::Never),
            Controls::None
        );
        assert_eq!(
            Controls::decide(false, true, CLIENT, ControlsPreference::Always),
            Controls::None
        );
    }

    #[test]
    fn only_windows_with_controls_drag_from_the_headers() {
        assert!(Controls::Custom.drags_window());
        assert!(Controls::NativeInset.drags_window());
        assert!(!Controls::None.drags_window());
    }

    #[test]
    fn controls_take_three_circles_and_two_gaps() {
        assert_eq!(Controls::width(&Metrics::default()), px(52.));
    }

    #[test]
    fn window_titles_name_the_environment() {
        assert_eq!(
            window_title(Some("prod-cluster"), false),
            "prod-cluster — icygui"
        );
        assert_eq!(
            window_title(Some("prod-cluster"), true),
            "prod-cluster (demo) — icygui"
        );
        assert_eq!(window_title(None, false), "icygui");
    }
}
