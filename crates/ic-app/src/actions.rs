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
//! | `ObjectPane` (a tab) | `escape` | [`Dismiss`]: back to the dashboard; the tab stays open |
//! | `Workspace` (everywhere) | `secondary-1` … `secondary-9` | [`SelectDashboard`]: the n-th dashboard in the sidebar |
//! | | `ctrl-tab`, `ctrl-shift-tab` | [`ActivateNextTab`], [`ActivatePreviousTab`]: cycle through the dashboard and the open tabs |
//! | | `secondary-w` | [`CloseTab`]: close the tab shown |
//! | | `secondary-b` | `ToggleSidebar` |
//! | | — | [`FocusMain`]: hand the keyboard to the list or the tab shown (Enter and Escape in the sidebar search do this) |
//! | | — | [`ReviewCertificate`], [`EditEnvironment`]: from the connection banner |
//!
//! `secondary` is cmd on macOS and ctrl elsewhere. Single letters are bound
//! only in the list and pane contexts, so they never reach text fields.

use gpui::{Action, App, KeyBinding};
use ic_model::ObjectKey;
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer};

/// Key context of the window's root, an ancestor of everything focusable.
pub(crate) const WORKSPACE_CONTEXT: &str = "Workspace";
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

/// Closes the detail pane, or clears the marks when no pane is open; in a
/// tab, goes back to the dashboard.
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

/// Shows the dashboard with this number in the sidebar, counting from 1
/// in sidebar order. Dashboards in collapsed groups are skipped, as the
/// sidebar doesn't list them; a search doesn't renumber them. In a keymap:
/// `["icygui::SelectDashboard", 3]`.
#[derive(Clone, Debug, Default, PartialEq, Eq, JsonSchema, Action)]
#[action(namespace = icygui)]
pub(crate) struct SelectDashboard(pub(crate) usize);

// By hand rather than derived: clippy flags deriving `Deserialize` next to
// the `unsafe` in GPUI's generated action registration.
impl<'de> Deserialize<'de> for SelectDashboard {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        usize::deserialize(deserializer).map(Self)
    }
}

/// Shows the next open tab; after the last one, the dashboard again.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct ActivateNextTab;

/// Shows the previous open tab; before the first one, the dashboard.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct ActivatePreviousTab;

/// Closes the tab shown, back to the dashboard.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct CloseTab;

/// Gives the keyboard focus to the main area: the dashboard list, or the
/// tab shown.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct FocusMain;

/// Shows the certificate of a server whose certificate isn't trusted, to
/// decide whether to trust it (the connection banner's "Review
/// certificate…"; the environment settings handle it).
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct ReviewCertificate;

/// Opens the active environment's settings (the connection banner's "Edit
/// environment…" after a refused login, a missing password or settings
/// that can't work).
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct EditEnvironment;

/// Registers the default key bindings of the workspace, the list and the
/// panes.
pub(crate) fn bind_keys(cx: &mut App) {
    let workspace = Some(WORKSPACE_CONTEXT);
    let list = Some(DASHBOARD_CONTEXT);
    let pane = Some(PANE_CONTEXT);
    cx.bind_keys((1..=9).map(|number| {
        KeyBinding::new(
            &format!("secondary-{number}"),
            SelectDashboard(number),
            workspace,
        )
    }));
    cx.bind_keys([
        KeyBinding::new("ctrl-tab", ActivateNextTab, workspace),
        KeyBinding::new("ctrl-shift-tab", ActivatePreviousTab, workspace),
        KeyBinding::new("secondary-w", CloseTab, workspace),
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
        KeyBinding::new("escape", Dismiss, pane),
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
    fn dashboard_numbers_come_from_keymaps_as_plain_numbers() {
        let action = SelectDashboard::build(serde_json::json!(3)).unwrap();
        assert!(action.partial_eq(&SelectDashboard(3)));
        assert!(SelectDashboard::build(serde_json::json!("three")).is_err());
    }

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
