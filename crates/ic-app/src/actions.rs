//! Keyboard actions and their default bindings, and the typed requests the
//! object actions produce.
//!
//! Every shortcut is a GPUI action bound in a key context, so a keymap file
//! can rebind it later (PLAN.md §2.3):
//!
//! | Context | Keys | Action |
//! |---|---|---|
//! | `DashboardView` | `j` / `down`, `k` / `up` | [`SelectNext`], [`SelectPrevious`] |
//! | | `shift-j` / `shift-down`, `shift-k` / `shift-up` | [`ExtendSelectionNext`], [`ExtendSelectionPrevious`] |
//! | | `home`, `end`, `pageup`, `pagedown` | [`SelectFirst`], [`SelectLast`], [`SelectPageUp`], [`SelectPageDown`] |
//! | | `enter` | [`OpenSelected`] |
//! | | `escape` | [`Dismiss`]: close the pane, else clear the marks |
//! | | `x`, `secondary-a` | [`ToggleMark`], [`MarkAll`] |
//! | | `secondary-enter` | [`OpenAsTab`] |
//! | `DashboardView`, `ObjectPane` | `a`, `d`, `r`, `c` | [`Acknowledge`], [`ScheduleDowntime`], [`CheckNow`], [`AddComment`] |
//!
//! `secondary` is cmd on macOS and ctrl elsewhere. Single letters are bound
//! only in the list and pane contexts, so they never reach text fields.

use gpui::{Action, App, KeyBinding};
use ic_model::ObjectKey;

/// Key context of the dashboard list and its split pane.
pub(crate) const DASHBOARD_CONTEXT: &str = "DashboardView";
/// Key context of an object opened as a tab.
pub(crate) const PANE_CONTEXT: &str = "ObjectPane";

/// Moves the cursor to the next row.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct SelectNext;

/// Moves the cursor to the previous row.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct SelectPrevious;

/// Moves the cursor down and marks the rows from the anchor to it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct ExtendSelectionNext;

/// Moves the cursor up and marks the rows from the anchor to it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct ExtendSelectionPrevious;

/// Moves the cursor to the first row.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct SelectFirst;

/// Moves the cursor to the last row.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct SelectLast;

/// Moves the cursor up by a screenful of rows.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct SelectPageUp;

/// Moves the cursor down by a screenful of rows.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct SelectPageDown;

/// Opens the cursor's object in the detail pane.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct OpenSelected;

/// Opens the cursor's (or the pane's) object as a tab in the sidebar.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct OpenAsTab;

/// Closes the detail pane, or clears the marks when no pane is open.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct Dismiss;

/// Marks or unmarks the cursor's row for a bulk action.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct ToggleMark;

/// Marks every row of the dashboard.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct MarkAll;

/// Acknowledges the marked rows, or the pane's or cursor's object.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct Acknowledge;

/// Schedules a downtime for the marked rows, or the pane's or cursor's object.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct ScheduleDowntime;

/// Reschedules the check of the marked rows, or the pane's or cursor's
/// object, to now.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct CheckNow;

/// Adds a comment to the marked rows, or the pane's or cursor's object.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct AddComment;

/// Registers the default key bindings of the list and the panes.
pub(crate) fn bind_keys(cx: &mut App) {
    let list = Some(DASHBOARD_CONTEXT);
    let pane = Some(PANE_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("j", SelectNext, list),
        KeyBinding::new("down", SelectNext, list),
        KeyBinding::new("k", SelectPrevious, list),
        KeyBinding::new("up", SelectPrevious, list),
        KeyBinding::new("shift-j", ExtendSelectionNext, list),
        KeyBinding::new("shift-down", ExtendSelectionNext, list),
        KeyBinding::new("shift-k", ExtendSelectionPrevious, list),
        KeyBinding::new("shift-up", ExtendSelectionPrevious, list),
        KeyBinding::new("home", SelectFirst, list),
        KeyBinding::new("end", SelectLast, list),
        KeyBinding::new("pageup", SelectPageUp, list),
        KeyBinding::new("pagedown", SelectPageDown, list),
        KeyBinding::new("enter", OpenSelected, list),
        KeyBinding::new("escape", Dismiss, list),
        KeyBinding::new("x", ToggleMark, list),
        KeyBinding::new("secondary-a", MarkAll, list),
        KeyBinding::new("secondary-enter", OpenAsTab, list),
        KeyBinding::new("a", Acknowledge, list),
        KeyBinding::new("d", ScheduleDowntime, list),
        KeyBinding::new("r", CheckNow, list),
        KeyBinding::new("c", AddComment, list),
        KeyBinding::new("a", Acknowledge, pane),
        KeyBinding::new("d", ScheduleDowntime, pane),
        KeyBinding::new("r", CheckNow, pane),
        KeyBinding::new("c", AddComment, pane),
    ]);
}

/// An operator action the user asked for. The dialogs and the core's
/// `Command`s come with M3; until then a request is logged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ObjectAction {
    /// Open the acknowledge dialog.
    Acknowledge,
    /// Remove the acknowledgement.
    RemoveAcknowledgement,
    /// Open the downtime dialog.
    ScheduleDowntime,
    /// Reschedule the check to now.
    CheckNow,
    /// Open the comment dialog.
    AddComment,
    /// Remove one comment, by its full name.
    RemoveComment(String),
    /// Remove one downtime, by its full name.
    RemoveDowntime(String),
}

impl ObjectAction {
    /// A short verb for logs and messages.
    pub(crate) fn label(&self) -> &'static str {
        match self {
            Self::Acknowledge => "acknowledge",
            Self::RemoveAcknowledgement => "remove acknowledgement",
            Self::ScheduleDowntime => "schedule downtime",
            Self::CheckNow => "check now",
            Self::AddComment => "add comment",
            Self::RemoveComment(_) => "remove comment",
            Self::RemoveDowntime(_) => "remove downtime",
        }
    }
}

/// An action and the objects it applies to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ActionRequest {
    /// What to do.
    pub(crate) action: ObjectAction,
    /// The objects, in list order.
    pub(crate) targets: Vec<ObjectKey>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_name_the_action() {
        assert_eq!(ObjectAction::Acknowledge.label(), "acknowledge");
        assert_eq!(
            ObjectAction::RemoveDowntime("h!d".to_owned()).label(),
            "remove downtime"
        );
        assert_eq!(ObjectAction::CheckNow.label(), "check now");
    }
}
