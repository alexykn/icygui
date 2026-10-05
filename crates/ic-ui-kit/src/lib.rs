//! GPUI building blocks styled to the design: theme tokens, bundled fonts,
//! icons and reusable components.
//!
//! The kit holds no application state. Components are stateless elements
//! configured with builders; colours and sizes come from the active
//! [`Theme`]. Text input comes from gpui-component, restyled to the theme.
//!
//! Call [`init`] once at startup and pass [`Assets`] to
//! `Application::with_assets`, then wrap each window's root view in [`Root`].

mod assets;
mod component_theme;
mod components;
mod fonts;
mod icon;
mod theme;

pub use assets::Assets;
pub use components::{
    Button, ButtonColors, ButtonVariant, CircleSize, CodeBlock, CompactRow, Divider, DividerColor,
    EmptyState, GlyphButton, IconButton, KeyHint, KvTable, Link, LinkStyle, ListRow, Menu,
    MenuItem, NoteEntry, Paint, PaneHeader, PerfdataRow, PerfdataTable, Popover, RowEmphasis,
    SectionLabel, StateCircle, StateDot, SubTabs, SummaryBar, SummaryItem, TextField, Tooltip,
    TreeLine, TreeTable,
};
pub use fonts::FontError;
pub use icon::{Icon, IconName};
pub use theme::{
    ActiveTheme, Colors, FONT_FAMILY, LINE_HEIGHT, Metrics, StateColors, TextSizes, Theme,
};

/// gpui-component's window root: hosts its overlays and, on Linux with
/// client-side decorations, draws the window frame, shadow and resize edges.
pub use gpui_component::Root;

/// gpui-component's overlay scrollbar, styled by the theme. Put it in a
/// `relative()` container next to the scrolled element:
/// `Scrollbar::vertical(&scroll_handle)` works with GPUI's `ScrollHandle`
/// and `UniformListScrollHandle`.
pub use gpui_component::scroll::Scrollbar;

/// Text input state and events, for [`TextField`].
pub mod input {
    pub use gpui_component::input::{InputEvent, InputState};
}

use gpui::App;

/// Registers the bundled fonts, initialises gpui-component and installs the
/// dark theme as a global.
///
/// # Errors
///
/// Returns an error if the platform text system rejects the bundled fonts.
pub fn init(cx: &mut App) -> Result<(), FontError> {
    fonts::register(cx)?;
    gpui_component::init(cx);
    set_theme(Theme::dark(), cx);
    Ok(())
}

/// Makes `theme` the active theme, for our components and gpui-component's,
/// and redraws every window.
pub fn set_theme(theme: Theme, cx: &mut App) {
    component_theme::apply(&theme, cx);
    cx.set_global(theme);
    cx.refresh_windows();
}
