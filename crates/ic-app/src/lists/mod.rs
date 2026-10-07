//! The lists of every downtime, comment and acknowledged problem (topic
//! 07): tabs in the sidebar's *open* section, opened from the palette.
//! [`model`] builds what they show from the snapshot, [`removal`] what a
//! removal lists and sends, [`view`] draws them, and [`dialog`] asks before
//! anything is removed.

pub(crate) mod dialog;
pub(crate) mod model;
pub(crate) mod removal;
pub(crate) mod view;

pub(crate) use self::model::ListKind;
pub(crate) use self::view::{RecordList, RecordListEvent};
