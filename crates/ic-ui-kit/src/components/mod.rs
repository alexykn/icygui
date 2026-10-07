//! Components styled to the design. All of them are stateless
//! [`gpui::RenderOnce`] elements configured with builder methods; they read
//! colours and sizes from the active [`crate::Theme`].

mod banner;
mod button;
mod divider;
mod form;
mod header;
mod link;
mod list;
mod menu;
mod modal;
mod notice;
mod state;
mod summary;
mod table;
mod text;
mod text_field;
mod toast;
mod tooltip;

pub use banner::{Banner, BannerTone, PaneBanner, PaneBannerTone, ProgressBar};
pub use button::{Button, ButtonColors, ButtonVariant, GlyphButton, IconButton, KeyHint};
pub use divider::{Divider, DividerColor};
pub use form::{CHIP_HEIGHT, Chip, Field, FieldTone, Segmented, Switch, TextArea, chip_width};
pub use header::{PaneHeader, SubTabs, sub_tab_gap, sub_tab_width};
pub use link::{Link, LinkStyle};
pub use list::{CompactRow, ListRow, RowEmphasis};
pub use menu::{Dismissable, Dismissal, ItemAction, Menu, MenuItem, Popover};
pub use modal::{DialogBody, Modal, ModalPlacement};
pub use notice::{EmptyState, NoteEntry, TreeLine, TreeTable};
pub use state::{CircleSize, ObjectMark, Paint, StateCircle, StateDot};
pub use summary::{SummaryBar, SummaryItem};
pub use table::{KvTable, PerfdataRow, PerfdataTable};
pub use text::{CodeBlock, SectionLabel};
pub use text_field::TextField;
pub use toast::{TOAST_WIDTH, Toast, ToastTone};
pub use tooltip::Tooltip;

/// Sets up what the components need app-wide (from [`crate::init`]).
pub(crate) fn init(cx: &mut gpui::App) {
    menu::init(cx);
}
