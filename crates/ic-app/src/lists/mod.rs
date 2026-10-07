//! The handling and downtimes views of topic 14: tabs in the sidebar's
//! *open* section, opened from the palette. [`model`] holds what the user
//! chooses (chip, sort, *only mine*, timeline or list), [`threads`] builds
//! what they show from the snapshot, [`removal`] what a removal lists and
//! sends, [`view`] draws them, and [`dialog`] asks before anything is
//! removed. The object's pane draws the same entries as its thread
//! ([`crate::pane`]).

pub(crate) mod dialog;
pub(crate) mod draw;
pub(crate) mod model;
pub(crate) mod removal;
pub(crate) mod threads;
pub(crate) mod view;
pub(crate) mod words;

pub(crate) use self::model::ListKind;
pub(crate) use self::view::{RecordList, RecordListEvent};
