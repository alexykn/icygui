//! The interface size (90 %, 100 % or 115 %): every length the views draw
//! is written in the design's pixels and scaled to the size in effect.
//!
//! The theme's type scale and metrics are scaled when the theme is built
//! ([`crate::Theme::new`]); a length written in a view goes through [`px`],
//! which scales it by the active theme's factor. GPUI can't scale a window's
//! content by itself (its display scale factor belongs to the platform), so
//! the factor lives here, next to the theme that sets it.
//!
//! Window geometry (the saved bounds, the minimum size) and the hairline
//! rules ([`crate::Metrics::RULE`]) are real pixels: they use
//! [`gpui::px`] and don't scale.

use std::cell::Cell;

use gpui::Pixels;

thread_local! {
    /// The active theme's factor. GPUI lays out and paints on one thread
    /// (the app's main thread; a test's own thread in tests), the one that
    /// installs the theme.
    static SCALE: Cell<f32> = const { Cell::new(1.) };
}

/// The smallest and largest factor the theme accepts.
pub(crate) const SCALE_RANGE: (f32, f32) = (0.5, 2.);

/// A length in the design's pixels at the interface size in effect: `px(14.)`
/// is 14 logical pixels at 100 %, 16.1 at 115 %.
#[must_use]
#[expect(
    clippy::disallowed_methods,
    reason = "the scaled length is real pixels"
)]
pub fn px(design: f32) -> Pixels {
    gpui::px(design * scale())
}

/// The interface size in effect, as a factor (1.0 at 100 %).
#[must_use]
pub fn scale() -> f32 {
    SCALE.with(Cell::get)
}

/// Makes `factor` the interface size for the lengths views draw from now
/// on (from [`crate::set_theme`]).
pub(crate) fn set_scale(factor: f32) {
    SCALE.with(|scale| scale.set(clamp(factor)));
}

/// `factor`, within [`SCALE_RANGE`]; 1.0 for something that is no number.
pub(crate) fn clamp(factor: f32) -> f32 {
    if factor.is_finite() {
        factor.clamp(SCALE_RANGE.0, SCALE_RANGE.1)
    } else {
        1.
    }
}

#[cfg(test)]
#[expect(
    clippy::disallowed_methods,
    reason = "compares scaled lengths with real pixels"
)]
mod tests {
    use super::*;

    #[test]
    fn lengths_follow_the_interface_size_on_this_thread() {
        assert_eq!(px(14.), gpui::px(14.), "100 % until a theme says otherwise");
        set_scale(1.15);
        assert_eq!(px(20.), gpui::px(20. * 1.15));
        assert!((scale() - 1.15).abs() < f32::EPSILON);
        // Another thread (another test, another app) has its own.
        std::thread::spawn(|| assert_eq!(px(14.), gpui::px(14.)))
            .join()
            .unwrap();
        set_scale(1.);
        assert_eq!(px(14.), gpui::px(14.));
    }

    #[test]
    fn factors_stay_sensible() {
        assert!((clamp(0.9) - 0.9).abs() < f32::EPSILON);
        assert!((clamp(10.) - SCALE_RANGE.1).abs() < f32::EPSILON);
        assert!((clamp(0.) - SCALE_RANGE.0).abs() < f32::EPSILON);
        assert!((clamp(f32::NAN) - 1.).abs() < f32::EPSILON);
    }
}
