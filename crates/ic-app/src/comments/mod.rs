//! Comments written in the handling view (topic 17, PLAN.md §4.2
//! *Comment from the handling view*): *+ comment* on a thread's last
//! entry (on hover, or `c` on the focused thread) opens the comment field
//! as the thread's next entry; Enter sends it through the action path
//! every comment takes; it shows dimmed as `sending…` until the event
//! stream brings it, or refused with its reason, `retry` and `discard`.
//! Without the add-comment permission none of it shows.
//!
//! [`field`] is the comment field the object pane's thread uses too (one
//! implementation), [`drafts`] the comments on their way (kept in
//! `AppState`), [`lines`] how both show in a thread.

pub(crate) mod drafts;
pub(crate) mod field;
pub(crate) mod lines;
