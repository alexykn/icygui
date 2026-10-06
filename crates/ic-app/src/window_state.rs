//! The main window's size and position between runs (BG-06): saved from
//! the window's bounds, restored on a display that still exists.

use gpui::{Bounds, Pixels, Size, WindowBounds, point, px, size};
use ic_config::WindowState;

/// How much of a restored window must be on a display (so its title bar
/// can be grabbed); otherwise it opens centred.
const MIN_VISIBLE: f32 = 64.;

/// Where the main window opens.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum InitialBounds {
    /// Where it was (or moved onto the display it overlaps most).
    Restored(WindowBounds),
    /// Centred on the primary display, this large.
    Centered(Size<Pixels>),
}

/// Where to open the window: `saved` if it still fits on one of
/// `displays` (moved and shrunk onto the display it overlaps most), else
/// centred with the saved size (or `default` without one).
pub(crate) fn initial_bounds(
    saved: Option<WindowState>,
    displays: &[Bounds<Pixels>],
    default: Size<Pixels>,
) -> InitialBounds {
    let Some(saved) = saved.filter(WindowState::is_plausible) else {
        return InitialBounds::Centered(default);
    };
    let wanted = Bounds {
        origin: point(px(saved.x), px(saved.y)),
        size: size(px(saved.width), px(saved.height)),
    };
    let best = displays
        .iter()
        .map(|display| (display, overlap(&wanted, display)))
        .filter(|(_, (width, height))| *width >= MIN_VISIBLE && *height >= MIN_VISIBLE)
        .max_by(|(_, a), (_, b)| (a.0 * a.1).total_cmp(&(b.0 * b.1)))
        .map(|(display, _)| *display);
    let Some(display) = best else {
        let fitted = displays.first().map_or(wanted.size, |display| {
            size(
                wanted.size.width.min(display.size.width),
                wanted.size.height.min(display.size.height),
            )
        });
        return InitialBounds::Centered(fitted);
    };
    let bounds = fit(wanted, display);
    InitialBounds::Restored(if saved.maximized {
        WindowBounds::Maximized(bounds)
    } else {
        WindowBounds::Windowed(bounds)
    })
}

/// `wanted` moved and shrunk to lie within `display`.
fn fit(wanted: Bounds<Pixels>, display: Bounds<Pixels>) -> Bounds<Pixels> {
    let width = wanted.size.width.min(display.size.width);
    let height = wanted.size.height.min(display.size.height);
    let max_x = display.origin.x + display.size.width - width;
    let max_y = display.origin.y + display.size.height - height;
    Bounds {
        origin: point(
            wanted.origin.x.clamp(display.origin.x, max_x),
            wanted.origin.y.clamp(display.origin.y, max_y),
        ),
        size: size(width, height),
    }
}

/// The width and height of the intersection of `a` and `b` (0 if apart).
fn overlap(a: &Bounds<Pixels>, b: &Bounds<Pixels>) -> (f32, f32) {
    let left = a.origin.x.max(b.origin.x);
    let right = (a.origin.x + a.size.width).min(b.origin.x + b.size.width);
    let top = a.origin.y.max(b.origin.y);
    let bottom = (a.origin.y + a.size.height).min(b.origin.y + b.size.height);
    (
        f32::from(right - left).max(0.),
        f32::from(bottom - top).max(0.),
    )
}

/// What to save for the window's `bounds` (the restore bounds of a
/// maximized or full-screen window).
pub(crate) fn to_state(bounds: WindowBounds) -> WindowState {
    let (rect, maximized) = match bounds {
        // A full-screen window comes back windowed, where it was.
        WindowBounds::Windowed(rect) | WindowBounds::Fullscreen(rect) => (rect, false),
        WindowBounds::Maximized(rect) => (rect, true),
    };
    WindowState {
        x: f32::from(rect.origin.x),
        y: f32::from(rect.origin.y),
        width: f32::from(rect.size.width),
        height: f32::from(rect.size.height),
        maximized,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f32, y: f32, width: f32, height: f32) -> Bounds<Pixels> {
        Bounds {
            origin: point(px(x), px(y)),
            size: size(px(width), px(height)),
        }
    }

    fn state(x: f32, y: f32, width: f32, height: f32, maximized: bool) -> WindowState {
        WindowState {
            x,
            y,
            width,
            height,
            maximized,
        }
    }

    const DEFAULT: Size<Pixels> = Size {
        width: px(1440.),
        height: px(900.),
    };

    #[test]
    fn nothing_saved_opens_centred_at_the_default_size() {
        assert_eq!(
            initial_bounds(None, &[rect(0., 0., 1920., 1080.)], DEFAULT),
            InitialBounds::Centered(DEFAULT)
        );
    }

    #[test]
    fn a_window_on_a_display_comes_back_where_it_was() {
        let displays = [rect(0., 0., 1920., 1080.), rect(1920., 0., 2560., 1440.)];
        assert_eq!(
            initial_bounds(
                Some(state(2100., 100., 1600., 1000., false)),
                &displays,
                DEFAULT
            ),
            InitialBounds::Restored(WindowBounds::Windowed(rect(2100., 100., 1600., 1000.)))
        );
        assert_eq!(
            initial_bounds(Some(state(10., 20., 1200., 800., true)), &displays, DEFAULT),
            InitialBounds::Restored(WindowBounds::Maximized(rect(10., 20., 1200., 800.)))
        );
    }

    #[test]
    fn a_window_partly_off_screen_moves_and_shrinks_onto_it() {
        let displays = [rect(0., 0., 1280., 800.)];
        assert_eq!(
            initial_bounds(
                Some(state(1000., 600., 1440., 900., false)),
                &displays,
                DEFAULT
            ),
            InitialBounds::Restored(WindowBounds::Windowed(rect(0., 0., 1280., 800.)))
        );
    }

    #[test]
    fn a_window_on_a_display_that_is_gone_opens_centred() {
        let displays = [rect(0., 0., 1920., 1080.)];
        assert_eq!(
            initial_bounds(
                Some(state(2500., 100., 1600., 1000., false)),
                &displays,
                DEFAULT
            ),
            InitialBounds::Centered(size(px(1600.), px(1000.)))
        );
        // Barely touching (less than 64px visible) counts as gone.
        assert_eq!(
            initial_bounds(
                Some(state(1900., 100., 800., 600., false)),
                &displays,
                DEFAULT
            ),
            InitialBounds::Centered(size(px(800.), px(600.)))
        );
        // Without any display known, the saved size is kept.
        assert_eq!(
            initial_bounds(Some(state(0., 0., 800., 600., false)), &[], DEFAULT),
            InitialBounds::Centered(size(px(800.), px(600.)))
        );
    }

    #[test]
    fn implausible_states_are_ignored() {
        assert_eq!(
            initial_bounds(
                Some(state(0., 0., f32::NAN, 600., false)),
                &[rect(0., 0., 1920., 1080.)],
                DEFAULT
            ),
            InitialBounds::Centered(DEFAULT)
        );
    }

    #[test]
    fn bounds_become_states_with_their_restore_size() {
        assert_eq!(
            to_state(WindowBounds::Windowed(rect(1., 2., 1300., 700.))),
            state(1., 2., 1300., 700., false)
        );
        assert_eq!(
            to_state(WindowBounds::Maximized(rect(1., 2., 1300., 700.))),
            state(1., 2., 1300., 700., true)
        );
        assert_eq!(
            to_state(WindowBounds::Fullscreen(rect(1., 2., 1300., 700.))),
            state(1., 2., 1300., 700., false)
        );
    }
}
