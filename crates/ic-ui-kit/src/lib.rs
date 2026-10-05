//! GPUI building blocks styled to the design: theme tokens, bundled fonts and
//! reusable components.

mod fonts;
mod theme;

pub use fonts::FontError;
pub use theme::{ActiveTheme, Colors, FONT_FAMILY, Metrics, StateColors, TextSizes, Theme};

use gpui::App;

/// Registers the bundled fonts and installs the dark theme as a global.
///
/// # Errors
///
/// Returns an error if the platform text system rejects the bundled fonts.
pub fn init(cx: &mut App) -> Result<(), FontError> {
    fonts::register(cx)?;
    cx.set_global(Theme::dark());
    Ok(())
}
