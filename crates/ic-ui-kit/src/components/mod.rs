//! Components styled to the design. All of them are stateless
//! [`gpui::RenderOnce`] elements configured with builder methods; they read
//! colours and sizes from the active [`crate::Theme`].

mod button;
mod divider;
mod header;
mod state;
mod summary;
mod table;
mod text;
mod text_field;
mod tooltip;

pub use button::{Button, ButtonColors, ButtonVariant, IconButton, KeyHint};
pub use divider::{Divider, DividerColor};
pub use header::{PaneHeader, SubTabs};
pub use state::{CircleSize, Paint, StateCircle, StateDot};
pub use summary::{SummaryBar, SummaryItem};
pub use table::{KvTable, PerfdataRow, PerfdataTable};
pub use text::{CodeBlock, SectionLabel};
pub use text_field::TextField;
pub use tooltip::Tooltip;
