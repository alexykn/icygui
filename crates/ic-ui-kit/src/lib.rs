//! GPUI building blocks styled to the design: theme tokens, bundled fonts,
//! icons and reusable components.
//!
//! The kit holds no application state. Components are stateless elements
//! configured with builders; colours and sizes come from the active
//! [`Theme`]. Text input comes from gpui-component, restyled to the theme.
//!
//! Lengths are written in the design's pixels with [`px`], which scales
//! them to the interface size of the active theme; [`gpui::px`] is for real
//! pixels only (window geometry, hairline rules).
//!
//! Call [`init`] once at startup and pass [`Assets`] to
//! `Application::with_assets`, then wrap each window's root view in [`Root`].

mod assets;
mod component_theme;
mod components;
mod fonts;
mod icon;
mod scale;
mod theme;

pub use assets::Assets;
pub use components::{
    Banner, BannerTone, Button, ButtonColors, ButtonVariant, CHIP_HEIGHT, Chip, CircleSize,
    CodeBlock, CompactRow, DialogBody, Dismissable, Dismissal, Divider, DividerColor, EmptyState,
    Field, FieldTone, GlyphButton, IconButton, ItemAction, KeyHint, KvTable, Link, LinkStyle,
    ListRow, Menu, MenuItem, Modal, ModalPlacement, NoteEntry, ObjectMark, Paint, PaneBanner,
    PaneBannerTone, PaneHeader, PerfdataRow, PerfdataTable, Popover, ProgressBar, RowEmphasis,
    SectionLabel, Segmented, StateCircle, StateDot, SubTabs, SummaryBar, SummaryItem, Switch,
    TOAST_WIDTH, TextArea, TextField, Toast, ToastTone, Tooltip, TreeLine, TreeTable, chip_width,
    sub_tab_gap, sub_tab_width,
};
pub use fonts::FontError;
pub use icon::{Icon, IconName};
pub use scale::{px, scale};
pub use theme::{
    ActiveTheme, CHAR_WIDTH, Colors, Density, FONT_FAMILY, LINE_HEIGHT, Metrics, StateColors,
    StateShades, TextSizes, Theme, ThemeMode, contrast_ratio,
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
    /// The action Escape runs in a text field. A field that has nothing to
    /// dismiss (a selection, a completion) lets it bubble up to the field's
    /// parents, which can handle it with `on_action`.
    pub use gpui_component::input::Escape;
    /// The text fields' editing actions, for an Edit menu (macOS): they act
    /// on the focused field.
    pub use gpui_component::input::{Copy, Cut, Paste, Redo, SelectAll, Undo};
    pub use gpui_component::input::{InputEvent, InputState, TextareaState};
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
    components::init(cx);
    set_theme(Theme::dark(), cx);
    Ok(())
}

/// Makes `theme` the active theme, for our components and gpui-component's,
/// with its interface size for the lengths views draw ([`px`]), and redraws
/// every window.
pub fn set_theme(theme: Theme, cx: &mut App) {
    component_theme::apply(&theme, cx);
    scale::set_scale(theme.scale);
    cx.set_global(theme);
    cx.refresh_windows();
}
