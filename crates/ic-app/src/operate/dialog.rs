//! The action dialogs (ACT-02..06): acknowledge, schedule downtime, add a
//! comment, submit a passive check result, run a command, and removing
//! downtimes (topic 01). One view for all of them, a modal over the
//! window, in the calm v2 look of the other dialogs.
//!
//! The operator sees every target before anything is sent: the box lists
//! every object (it scrolls; only the rows in view are built) and the
//! button counts them. A host's downtime covers its services by default
//! (`all services`, Icinga's `all_services`): the switch sits right above
//! the box, which then lists the host and each of its services. Removing
//! a downtime lists every downtime it removes; a service's downtime that
//! belongs to its host's offers *this service only* or the host's whole
//! downtime.
//!
//! Keyboard first: the first field has the keyboard when it opens, Tab and
//! Shift-Tab move between the fields, Enter sends (Shift-Enter starts a
//! new line in a comment or output; the macros field takes plain Enter for
//! new lines), `secondary-enter` sends from any field, Escape closes. Problems show
//! next to their fields once sending was tried; times and durations show
//! what they mean as they're typed. Running a command asks once more
//! (`secondary-enter` or the button) before anything is sent.
//!
//! The dialog sends through `AppState::submit`; when that refuses (no
//! connection) the dialog stays open with the reason, so nothing typed is
//! lost.

use std::ops::Range;
use std::rc::Rc;

use gpui::{
    Action, AnyElement, App, AppContext as _, ClickEvent, Context, Entity, EventEmitter,
    FocusHandle, Focusable, FontWeight, InteractiveElement as _, IntoElement, KeyBinding,
    ParentElement as _, Render, SharedString, Styled as _, Subscription, UniformListScrollHandle,
    Window, div, prelude::FluentBuilder as _, uniform_list,
};
use ic_core::snapshot::Snapshot;
use ic_model::{ActionTarget, CheckableState, ChildOptions, CommandType, ObjectKey, Timestamp};
use ic_ui_kit::input::{InputEvent, InputState, TextareaState};
use ic_ui_kit::{
    ActiveTheme as _, Button, ButtonVariant, Chip, DialogBody, Field, FieldTone, KvTable,
    ObjectMark, Scrollbar, ScrollbarMode, Segmented, StateDot, Switch, TextArea, TextField, Theme,
    px,
};

use super::ActionSpec;
use super::forms::{
    AckForm, CommandForm, CommentForm, DowntimeForm, Eligible, FormField, Issues, ResultForm,
    TriggerChoice, describe_objects, trigger_choices,
};
use super::when::{self, END_PRESETS, EXPIRY_PRESETS, Preset};
use crate::actions::ObjectAction;
use crate::app_state::AppState;
use crate::downtimes::{self, Removal, Send};

/// Key context of an action dialog.
pub(crate) const DIALOG_CONTEXT: &str = "ActionDialog";
/// Key context of the run-command confirmation step.
const CONFIRM_CONTEXT: &str = "ActionConfirm";
/// Key context of a dialog without text fields (checking objects named
/// by a palette query): Enter sends it.
const FIELDLESS_CONTEXT: &str = "ActionFieldless";
/// Key context of the remove-downtime dialog: Enter removes, ← and → choose
/// the scope.
const REMOVAL_CONTEXT: &str = "ActionRemoval";

/// A row of the target box.
const TARGET_ROW: f32 = 20.;
/// The target box's height at most (it scrolls beyond), as drawn: the
/// action dialogs' box (5.5 rows), and the removal's taller one (10.5
/// rows). Half a row shows at the bottom of a full box, so the cut-off row
/// says there is more.
const TARGET_BOX: f32 = 130.;
const REMOVAL_BOX: f32 = 230.;
/// The target box's padding, top and bottom.
const TARGET_PADDING: f32 = 10.;

/// Moves the keyboard to the dialog's next field.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct NextField;

/// Moves the keyboard to the dialog's previous field.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct PreviousField;

/// Runs the command after the confirmation step.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct ConfirmCommand;

/// Enter in a field: sends the dialog (the macros field takes it for a new
/// line instead).
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct SendDialog;

/// `secondary-enter` in a field: sends the dialog from any field.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct SendDialogNow;

/// → in the remove-downtime dialog: the next (wider) scope.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct NextScope;

/// ← in the remove-downtime dialog: the previous (narrower) scope.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct PreviousScope;

/// Registers the dialogs' keys. Tab and Enter are bound over the text
/// fields (`ActionDialog > Input`), registered after gpui-component's own
/// bindings, so Tab moves between fields instead of indenting and Enter
/// sends instead of typing a new line (Shift-Enter still does). A dialog
/// without fields keeps Tab and Shift-Tab to itself: the keyboard never
/// leaves an open dialog for the views behind it.
pub(crate) fn bind_keys(cx: &mut App) {
    let field = Some("ActionDialog > Input");
    let dialog = Some(DIALOG_CONTEXT);
    let fieldless = Some(FIELDLESS_CONTEXT);
    let confirm = Some(CONFIRM_CONTEXT);
    let removal = Some(REMOVAL_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("tab", NextField, fieldless),
        KeyBinding::new("shift-tab", PreviousField, fieldless),
        KeyBinding::new("tab", NextField, confirm),
        KeyBinding::new("shift-tab", PreviousField, confirm),
        KeyBinding::new("tab", NextField, removal),
        KeyBinding::new("shift-tab", PreviousField, removal),
        KeyBinding::new("enter", SendDialog, removal),
        KeyBinding::new("secondary-enter", SendDialogNow, removal),
        KeyBinding::new("left", PreviousScope, removal),
        KeyBinding::new("right", NextScope, removal),
        KeyBinding::new("tab", NextField, field),
        KeyBinding::new("shift-tab", PreviousField, field),
        KeyBinding::new("enter", SendDialog, field),
        KeyBinding::new("secondary-enter", SendDialogNow, field),
        KeyBinding::new("tab", NextField, dialog),
        KeyBinding::new("shift-tab", PreviousField, dialog),
        KeyBinding::new("secondary-enter", ConfirmCommand, Some(CONFIRM_CONTEXT)),
        KeyBinding::new("enter", SendDialog, Some(FIELDLESS_CONTEXT)),
        KeyBinding::new("secondary-enter", SendDialogNow, Some(FIELDLESS_CONTEXT)),
    ]);
}

/// Which dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DialogKind {
    /// Acknowledge problems (ACT-02).
    Acknowledge,
    /// Schedule a downtime (ACT-03).
    Downtime,
    /// Add a comment (ACT-04).
    Comment,
    /// Submit a passive check result (ACT-05).
    CheckResult,
    /// Run a check or event command (ACT-06).
    Command,
    /// Check objects now (ACT-01): only for objects a palette query named
    /// loosely (*all N matches*), which are listed before anything is
    /// sent; checks of rows and panes go at once.
    Check,
    /// Remove downtimes (topic 01): lists every downtime it removes before
    /// anything is sent ([`ActionDialog::removal`]).
    RemoveDowntime,
}

impl DialogKind {
    /// The dialog for `action`, if it has one.
    pub(crate) fn for_action(action: &ObjectAction) -> Option<Self> {
        match action {
            ObjectAction::Acknowledge => Some(Self::Acknowledge),
            ObjectAction::ScheduleDowntime => Some(Self::Downtime),
            ObjectAction::AddComment => Some(Self::Comment),
            ObjectAction::SubmitCheckResult => Some(Self::CheckResult),
            ObjectAction::RunCommand => Some(Self::Command),
            _ => None,
        }
    }

    /// The dialog that lists the objects a palette query named loosely
    /// before acting on them (`secondary-enter` on a verb's *all N
    /// matches*): the action's own, or for a check the check dialog.
    pub(crate) fn for_review(action: &ObjectAction) -> Option<Self> {
        match action {
            ObjectAction::CheckNow => Some(Self::Check),
            other => Self::for_action(other),
        }
    }

    /// The action it sends.
    pub(crate) fn action(self) -> ObjectAction {
        match self {
            Self::Acknowledge => ObjectAction::Acknowledge,
            Self::Downtime => ObjectAction::ScheduleDowntime,
            Self::Comment => ObjectAction::AddComment,
            Self::CheckResult => ObjectAction::SubmitCheckResult,
            Self::Command => ObjectAction::RunCommand,
            Self::Check => ObjectAction::CheckNow,
            Self::RemoveDowntime => ObjectAction::RemoveDowntimes,
        }
    }

    /// The dialog's title.
    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::Acknowledge => "Acknowledge",
            Self::Downtime => "Schedule downtime",
            Self::Comment => "Add comment",
            Self::CheckResult => "Submit check result",
            Self::Command => "Run command",
            Self::Check => "Check now",
            Self::RemoveDowntime => "Remove downtime",
        }
    }

    fn submit_label(self) -> &'static str {
        match self {
            Self::Acknowledge => "acknowledge",
            Self::Downtime => "schedule downtime",
            Self::Comment => "add comment",
            Self::CheckResult => "submit result",
            Self::Command => "run",
            Self::Check => "check now",
            Self::RemoveDowntime => "remove downtime",
        }
    }

    /// What the dialog says when nothing qualifies.
    fn nothing(self) -> &'static str {
        match self {
            Self::Acknowledge => "Nothing to acknowledge",
            Self::RemoveDowntime => "Nothing to remove",
            _ => "Nothing to do",
        }
    }
}

/// What the dialog asks the workspace to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DialogEvent {
    /// Sent or cancelled: close it.
    Close,
}

/// What the dialog edits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Form {
    /// The acknowledge dialog.
    Ack(AckForm),
    /// The downtime dialog.
    Downtime(DowntimeForm),
    /// The comment dialog.
    Comment(CommentForm),
    /// The passive result dialog.
    Result(ResultForm),
    /// The run command dialog.
    Command(CommandForm),
    /// The check dialog: nothing to fill in.
    Check,
    /// Removing downtimes: the scope to remove.
    Removal(Removal),
}

impl Form {
    fn new(kind: DialogKind) -> Self {
        match kind {
            DialogKind::Acknowledge => Self::Ack(AckForm::default()),
            DialogKind::Downtime => Self::Downtime(DowntimeForm::default()),
            DialogKind::Comment => Self::Comment(CommentForm::default()),
            DialogKind::CheckResult => Self::Result(ResultForm::default()),
            DialogKind::Command => Self::Command(CommandForm::default()),
            DialogKind::Check => Self::Check,
            DialogKind::RemoveDowntime => Self::Removal(Removal::default()),
        }
    }

    /// The text of `field`.
    fn text(&self, field: FormField) -> &str {
        match (self, field) {
            (Self::Ack(form), FormField::Comment) => &form.comment,
            (Self::Ack(form), FormField::Expiry) => &form.expiry,
            (Self::Downtime(form), FormField::Comment) => &form.comment,
            (Self::Downtime(form), FormField::Start) => &form.start,
            (Self::Downtime(form), FormField::End) => &form.end,
            (Self::Downtime(form), FormField::Duration) => &form.duration,
            (Self::Downtime(form), FormField::Trigger) => &form.trigger,
            (Self::Comment(form), FormField::Comment) => &form.text,
            (Self::Comment(form), FormField::Expiry) => &form.expiry,
            (Self::Result(form), FormField::Output) => &form.output,
            (Self::Result(form), FormField::Perfdata) => &form.perfdata,
            (Self::Command(form), FormField::Command) => &form.command,
            (Self::Command(form), FormField::Endpoint) => &form.endpoint,
            (Self::Command(form), FormField::Macros) => &form.macros,
            (Self::Command(form), FormField::Ttl) => &form.ttl,
            _ => "",
        }
    }

    /// Sets the text of `field`.
    fn set_text(&mut self, field: FormField, value: String) {
        let slot = match (self, field) {
            (Self::Ack(form), FormField::Comment) => &mut form.comment,
            (Self::Ack(form), FormField::Expiry) => &mut form.expiry,
            (Self::Downtime(form), FormField::Comment) => &mut form.comment,
            (Self::Downtime(form), FormField::Start) => &mut form.start,
            (Self::Downtime(form), FormField::End) => &mut form.end,
            (Self::Downtime(form), FormField::Duration) => &mut form.duration,
            (Self::Downtime(form), FormField::Trigger) => &mut form.trigger,
            (Self::Comment(form), FormField::Comment) => &mut form.text,
            (Self::Comment(form), FormField::Expiry) => &mut form.expiry,
            (Self::Result(form), FormField::Output) => &mut form.output,
            (Self::Result(form), FormField::Perfdata) => &mut form.perfdata,
            (Self::Command(form), FormField::Command) => &mut form.command,
            (Self::Command(form), FormField::Endpoint) => &mut form.endpoint,
            (Self::Command(form), FormField::Macros) => &mut form.macros,
            (Self::Command(form), FormField::Ttl) => &mut form.ttl,
            _ => return,
        };
        *slot = value;
    }

    /// Whether `field` is shown now (an expiry only when wanted, a
    /// duration only for flexible downtimes).
    fn shows(&self, field: FormField) -> bool {
        match (self, field) {
            (Self::Ack(form), FormField::Expiry) => form.expires,
            (Self::Comment(form), FormField::Expiry) => form.expires,
            (Self::Downtime(form), FormField::Duration) => form.flexible,
            _ => true,
        }
    }

    /// The action, or the problems.
    fn action(&self, now: Timestamp, endpoints: &[String]) -> Result<ic_model::Action, Issues> {
        match self {
            Self::Ack(form) => form.action(now),
            Self::Downtime(form) => form.action(now),
            Self::Comment(form) => form.action(now),
            Self::Result(form) => form.action(),
            Self::Command(form) => form.action(endpoints),
            Self::Check => Ok(ic_model::Action::CheckNow { force: true }),
            // Sent as one removal per downtime or object
            // ([`ActionDialog::removal_specs`]).
            Self::Removal(_) => Ok(ic_model::Action::RemoveAllDowntimes),
        }
    }
}

/// A text input of the dialog.
#[derive(Clone)]
enum Text {
    Line(Entity<InputState>),
    Area(Entity<TextareaState>),
}

impl Text {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match self {
            Self::Line(input) => input.focus_handle(cx),
            Self::Area(input) => input.focus_handle(cx),
        }
    }

    fn focus(&self, window: &mut Window, cx: &mut App) {
        match self {
            Self::Line(input) => input.update(cx, |input, cx| input.focus(window, cx)),
            Self::Area(input) => input.update(cx, |input, cx| input.focus(window, cx)),
        }
    }

    fn set(&self, value: &str, window: &mut Window, cx: &mut App) {
        let value = value.to_owned();
        match self {
            Self::Line(input) => input.update(cx, |input, cx| input.set_value(value, window, cx)),
            Self::Area(input) => input.update(cx, |input, cx| input.set_value(value, window, cx)),
        }
    }
}

/// The fields with a text input, in the order Tab visits them, and their
/// placeholders.
fn text_fields(kind: DialogKind) -> &'static [(FormField, &'static str, bool)] {
    // (field, placeholder, multi-line)
    match kind {
        DialogKind::Acknowledge => &[
            (FormField::Comment, "what's being done about it", true),
            (FormField::Expiry, "+4h, 18:00, tomorrow 08:00", false),
        ],
        DialogKind::Downtime => &[
            (FormField::Comment, "why (maintenance, deployment, …)", true),
            (FormField::Start, "now, 14:30, 2026-10-06 22:00", false),
            (FormField::End, "+2h, 18:00, tomorrow 08:00", false),
            (FormField::Duration, "1h", false),
            (
                FormField::Trigger,
                "host!service!downtime-id (optional)",
                false,
            ),
        ],
        DialogKind::Comment => &[
            (FormField::Comment, "the comment", true),
            (FormField::Expiry, "+1d, tomorrow 08:00", false),
        ],
        DialogKind::CheckResult => &[
            (FormField::Output, "OK - what the check would say", true),
            (
                FormField::Perfdata,
                "load1=0.5;1;2 'disk /'=80%;90;95 (optional)",
                false,
            ),
        ],
        DialogKind::Command => &[
            (FormField::Command, "the object's own (optional)", false),
            (FormField::Endpoint, "", false),
            (
                FormField::Macros,
                "name = value, one per line (optional)",
                true,
            ),
            (FormField::Ttl, "5m", false),
        ],
        DialogKind::Check | DialogKind::RemoveDowntime => &[],
    }
}

/// An action dialog.
pub(crate) struct ActionDialog {
    state: Entity<AppState>,
    /// The environment the objects are in: the dialog sends nothing to
    /// another one.
    environment_id: Option<String>,
    kind: DialogKind,
    eligible: Eligible,
    form: Form,
    inputs: Vec<(FormField, Text)>,
    /// Problems are shown once sending was tried.
    show_issues: bool,
    /// Why the last send was refused.
    error: Option<String>,
    /// The run command dialog's second step.
    confirming: bool,
    author: String,
    endpoints: Vec<String>,
    /// What the endpoint field means when left blank.
    endpoint_default: String,
    /// Downtimes that could trigger a scheduled one (the objects', their
    /// hosts' and those hosts' parents').
    triggers: Vec<TriggerChoice>,
    /// What the target box lists: every object (with all services, each
    /// service of the hosts), or every downtime a removal removes.
    listed: Rc<Vec<Listed>>,
    /// The rows the target box is sized for: the most any choice of the
    /// dialog lists (`all services` on or off, either removal scope), so
    /// the box, and the dialog with it, keeps its size and place when the
    /// choice changes.
    box_rows: usize,
    /// The main button's width when its label changes with a choice: the
    /// longest label, so `cancel` never moves.
    submit_width: Option<gpui::Pixels>,
    /// Scrolls the target box.
    target_scroll: UniformListScrollHandle,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<DialogEvent> for ActionDialog {}

impl Focusable for ActionDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// Why an action asked for in one environment isn't sent once another is
/// active.
pub(crate) const ENVIRONMENT_CHANGED: &str =
    "Another environment is active now; nothing was sent. Ask again there.";

impl ActionDialog {
    /// A dialog of `kind` for `eligible`'s objects in `environment` (`None`:
    /// the active one; another one for a desktop notification's
    /// *Acknowledge*, A1): it sends to that environment's engine only.
    pub(crate) fn new(
        state: Entity<AppState>,
        kind: DialogKind,
        eligible: Eligible,
        environment: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let form = Form::new(kind);
        let environment_id =
            environment.or_else(|| state.read(cx).active_environment_id().map(str::to_owned));
        let (author, endpoints, endpoint_default, triggers, listed) = {
            let current = state.read(cx);
            let snapshot = environment_id
                .as_deref()
                .and_then(|id| current.snapshot_of(id))
                .unwrap_or_else(|| current.snapshot());
            let triggers = if kind == DialogKind::Downtime {
                trigger_choices(snapshot, &eligible.targets, Timestamp::now())
            } else {
                Vec::new()
            };
            let listed = listed_targets(&eligible, &form, snapshot);
            (
                environment_id
                    .as_deref()
                    .and_then(|id| current.environment_by_id(id))
                    .map(|environment| environment.author_name().to_owned())
                    .unwrap_or_default(),
                snapshot
                    .endpoints
                    .iter()
                    .map(|endpoint| endpoint.name.clone())
                    .collect::<Vec<_>>(),
                endpoint_default(snapshot, &eligible.targets),
                triggers,
                Rc::new(listed),
            )
        };
        let mut inputs = Vec::new();
        let mut subscriptions = Vec::new();
        for &(field, placeholder, multi_line) in text_fields(kind) {
            let value = form.text(field).to_owned();
            let placeholder = if field == FormField::Endpoint {
                endpoint_default.clone()
            } else {
                placeholder.to_owned()
            };
            let text = if multi_line {
                let input = cx.new(|cx| {
                    TextareaState::new(window, cx)
                        .placeholder(placeholder)
                        .default_value(value)
                });
                subscriptions.push(cx.subscribe_in(
                    &input,
                    window,
                    move |this: &mut Self, input, event: &InputEvent, _, cx| {
                        let value = input.read(cx).value().to_string();
                        this.on_input(field, value, event, cx);
                    },
                ));
                Text::Area(input)
            } else {
                let input = cx.new(|cx| {
                    InputState::new(window, cx)
                        .placeholder(placeholder)
                        .default_value(value)
                });
                subscriptions.push(cx.subscribe_in(
                    &input,
                    window,
                    move |this: &mut Self, input, event: &InputEvent, _, cx| {
                        let value = input.read(cx).value().to_string();
                        this.on_input(field, value, event, cx);
                    },
                ));
                Text::Line(input)
            };
            inputs.push((field, text));
        }
        if let Some((_, first)) = inputs.first() {
            first.focus(window, cx);
        }
        let mut dialog = Self {
            state,
            environment_id,
            kind,
            eligible,
            form,
            inputs,
            show_issues: false,
            error: None,
            confirming: false,
            author,
            endpoints,
            endpoint_default,
            triggers,
            box_rows: listed.len(),
            submit_width: None,
            listed,
            target_scroll: UniformListScrollHandle::new(),
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        };
        dialog.size_for_every_choice(cx);
        dialog
    }

    /// The removal dialog for `removal` (a pane's *remove downtime*, an
    /// other downtime's `×`, *remove downtimes* on objects): it lists every
    /// downtime that goes, and sends nothing before *remove*.
    pub(crate) fn removal(
        state: Entity<AppState>,
        removal: Removal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let eligible = Eligible {
            targets: removal.objects.clone(),
            skipped: Vec::new(),
        };
        let mut dialog = Self::new(
            state,
            DialogKind::RemoveDowntime,
            eligible,
            None,
            window,
            cx,
        );
        dialog.form = Form::Removal(removal);
        dialog.refresh_listed(cx);
        dialog.size_for_every_choice(cx);
        dialog
    }

    /// The forms a choice in the dialog can lead to: either `all services`
    /// for a host's downtime, every scope of a removal; else the form alone.
    fn choices(&self) -> Vec<Form> {
        match &self.form {
            Form::Removal(removal) => (0..removal.scopes.len().max(1))
                .map(|chosen| {
                    let mut removal = removal.clone();
                    removal.chosen = chosen;
                    Form::Removal(removal)
                })
                .collect(),
            Form::Downtime(form) => [true, false]
                .into_iter()
                .map(|all_services| {
                    let mut form = form.clone();
                    form.all_services = all_services;
                    Form::Downtime(form)
                })
                .collect(),
            form => vec![form.clone()],
        }
    }

    /// Sizes the target box for the choice that lists the most, and the
    /// main button for its longest label: changing a choice then moves
    /// nothing (PLAN 4.3).
    fn size_for_every_choice(&mut self, cx: &App) {
        let state = self.state.read(cx);
        let snapshot = self
            .environment_id
            .as_deref()
            .and_then(|id| state.snapshot_of(id))
            .unwrap_or_else(|| state.snapshot());
        let counts: Vec<usize> = self
            .choices()
            .iter()
            .map(|form| listed_targets(&self.eligible, form, snapshot).len())
            .collect();
        self.box_rows = counts.iter().copied().max().unwrap_or(0);
        let labels: std::collections::BTreeSet<String> = counts
            .iter()
            .map(|count| submit_label(self.kind, &self.form, *count))
            .collect();
        self.submit_width = (labels.len() > 1).then(|| {
            let theme = cx.theme();
            labels
                .iter()
                .map(|label| Button::width_for(theme, label, true))
                .fold(px(0.), gpui::Pixels::max)
        });
    }

    /// Lists the targets again (after `all services` or the removal's
    /// scope changed).
    fn refresh_listed(&mut self, cx: &App) {
        let state = self.state.read(cx);
        let snapshot = self
            .environment_id
            .as_deref()
            .and_then(|id| state.snapshot_of(id))
            .unwrap_or_else(|| state.snapshot());
        self.listed = Rc::new(listed_targets(&self.eligible, &self.form, snapshot));
    }

    /// The objects or downtimes the box lists (tests).
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn listed_objects(&self) -> Vec<ObjectKey> {
        self.listed.iter().map(|row| row.object.clone()).collect()
    }

    /// The rows the target box is sized for, and the main button's fixed
    /// width (tests).
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn box_geometry(&self) -> (usize, Option<gpui::Pixels>) {
        (self.box_rows, self.submit_width)
    }

    /// The main button's label (tests).
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn submit_text(&self) -> String {
        self.submit_label()
    }

    /// The main button: `schedule 24 downtimes`, `remove downtime`; the
    /// others say what they do.
    fn submit_label(&self) -> String {
        submit_label(self.kind, &self.form, self.listed.len())
    }

    /// Where the keyboard goes when the dialog opens: its first field.
    pub(crate) fn default_focus(&self, cx: &App) -> FocusHandle {
        if self.confirming {
            return self.focus_handle.clone();
        }
        self.inputs.first().map_or_else(
            || self.focus_handle.clone(),
            |(_, input)| input.focus_handle(cx),
        )
    }

    /// Whether anything was typed or changed: a press beside the dialog
    /// then doesn't close it (nothing typed is lost by a stray click).
    pub(crate) fn is_dirty(&self) -> bool {
        // Choosing what to remove isn't typing.
        self.kind != DialogKind::RemoveDowntime && self.form != Form::new(self.kind)
    }

    /// Which dialog this is.
    pub(crate) fn kind(&self) -> DialogKind {
        self.kind
    }

    /// The objects it acts on and those it skips.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn eligible(&self) -> &Eligible {
        &self.eligible
    }

    /// The form as edited so far.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn form(&self) -> &Form {
        &self.form
    }

    /// Changes the form (tests flip switches and choose scopes with it).
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn edit_form(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut Form)) {
        edit(&mut self.form);
        self.refresh_listed(cx);
        cx.notify();
    }

    /// The problems shown next to the fields.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn shown_issues(&self) -> Issues {
        self.issues()
    }

    /// Why the last send was refused.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Whether the run command dialog asks for confirmation.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn is_confirming(&self) -> bool {
        self.confirming
    }

    /// Types `text` into `field` (replacing what's there), as the user
    /// would.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn type_into(
        this: &Entity<Self>,
        field: FormField,
        text: &str,
        window: &mut Window,
        cx: &mut App,
    ) {
        let input = this.read(cx).input(field).cloned();
        match input {
            Some(Text::Line(input)) => {
                input.update(cx, |input, cx| input.replace_all(text, window, cx));
            }
            Some(Text::Area(input)) => {
                input.update(cx, |input, cx| input.replace_all(text, window, cx));
            }
            None => {}
        }
    }

    /// Chooses a preset for `field`, as its chip does.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn choose_preset(
        &mut self,
        field: FormField,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.fill(field, text, window, cx);
    }

    /// The field that has the keyboard, if any.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn focused_field(&self, window: &Window, cx: &App) -> Option<FormField> {
        self.inputs
            .iter()
            .find(|(_, input)| input.focus_handle(cx).is_focused(window))
            .map(|(field, _)| *field)
    }

    fn issues(&self) -> Issues {
        if !self.show_issues {
            return Issues::new();
        }
        self.form
            .action(Timestamp::now(), &self.endpoints)
            .err()
            .unwrap_or_default()
    }

    fn on_input(
        &mut self,
        field: FormField,
        value: String,
        event: &InputEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                if self.form.text(field) != value {
                    self.form.set_text(field, value);
                    self.error = None;
                    cx.notify();
                }
            }
            // Enter is the dialog's own key (`SendDialog`).
            InputEvent::PressEnter { .. } | InputEvent::Focus | InputEvent::Blur => {}
        }
    }

    /// Types `text` into `field`, as a preset chip does.
    fn fill(&mut self, field: FormField, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((_, input)) = self
            .inputs
            .iter()
            .find(|(candidate, _)| *candidate == field)
        {
            input.set(text, window, cx);
        }
        self.form.set_text(field, text.to_owned());
        self.error = None;
        cx.notify();
    }

    /// The text fields shown now, in Tab order.
    fn visible_inputs(&self) -> Vec<&Text> {
        self.inputs
            .iter()
            .filter(|(field, _)| self.form.shows(*field))
            .map(|(_, input)| input)
            .collect()
    }

    /// Tab: the next shown field (wrapping), Shift-Tab the previous.
    /// A dialog without fields (or asking for confirmation) keeps the
    /// keyboard where it is.
    fn move_focus(&self, forward: bool, window: &mut Window, cx: &mut App) {
        let inputs = self.visible_inputs();
        if inputs.is_empty() || self.confirming {
            self.focus_handle.focus(window, cx);
            return;
        }
        let current = inputs
            .iter()
            .position(|input| input.focus_handle(cx).is_focused(window));
        let count = inputs.len();
        let next = match (current, forward) {
            (Some(index), true) => (index + 1) % count,
            (Some(index), false) => (index + count - 1) % count,
            (None, true) => 0,
            (None, false) => count - 1,
        };
        inputs[next].focus(window, cx);
    }

    fn on_next_field(&mut self, _: &NextField, window: &mut Window, cx: &mut Context<Self>) {
        self.move_focus(true, window, cx);
    }

    fn on_previous_field(
        &mut self,
        _: &PreviousField,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_focus(false, window, cx);
    }

    /// ← / →: the removal's previous or next scope (the box keeps its
    /// size; only the rows and the count change).
    fn step_scope(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Form::Removal(removal) = &mut self.form else {
            return;
        };
        let count = removal.scopes.len();
        if count < 2 {
            return;
        }
        let chosen = if forward {
            (removal.chosen + 1).min(count - 1)
        } else {
            removal.chosen.saturating_sub(1)
        };
        if chosen != removal.chosen {
            removal.chosen = chosen;
            self.refresh_listed(cx);
            cx.notify();
        }
    }

    fn on_next_scope(&mut self, _: &NextScope, _: &mut Window, cx: &mut Context<Self>) {
        self.step_scope(true, cx);
    }

    fn on_previous_scope(&mut self, _: &PreviousScope, _: &mut Window, cx: &mut Context<Self>) {
        self.step_scope(false, cx);
    }

    fn on_confirm(&mut self, _: &ConfirmCommand, window: &mut Window, cx: &mut Context<Self>) {
        if self.confirming {
            self.submit(window, cx);
        }
    }

    /// Enter: sends, except in the macros field, where it types a new line
    /// (the field's own binding runs).
    fn on_send(&mut self, _: &SendDialog, window: &mut Window, cx: &mut Context<Self>) {
        let in_macros = self
            .input(FormField::Macros)
            .is_some_and(|input| input.focus_handle(cx).is_focused(window));
        if in_macros {
            cx.propagate();
            return;
        }
        self.submit(window, cx);
    }

    fn on_send_now(&mut self, _: &SendDialogNow, window: &mut Window, cx: &mut Context<Self>) {
        self.submit(window, cx);
    }

    /// Enter or the main button: checks the form, asks once more before
    /// running a command, then sends. Problems keep the dialog open, with
    /// the keyboard in the first field that has one.
    pub(crate) fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_issues = true;
        if self.eligible.targets.is_empty() {
            self.error = Some(format!("{}: no object qualifies.", self.kind.nothing()));
            cx.notify();
            return;
        }
        if let Form::Removal(removal) = &self.form {
            let specs = removal_specs(removal);
            self.send(specs, cx);
            return;
        }
        let action = match self.form.action(Timestamp::now(), &self.endpoints) {
            Ok(action) => action,
            Err(issues) => {
                let first = self
                    .inputs
                    .iter()
                    .find(|(field, _)| issues.contains_key(field))
                    .map(|(_, input)| input.clone());
                if let Some(input) = first {
                    input.focus(window, cx);
                }
                cx.notify();
                return;
            }
        };
        if self.kind == DialogKind::Command && !self.confirming {
            self.confirming = true;
            window.focus(&self.focus_handle, cx);
            cx.notify();
            return;
        }
        let spec =
            ActionSpec::for_objects(self.kind.action(), action, self.eligible.targets.clone());
        if !self.send(vec![spec], cx) && self.confirming {
            self.back(window, cx);
        }
    }

    /// Sends `specs` to the engine of the environment the objects are in,
    /// never to another (same host names, maybe production), and closes;
    /// a refusal keeps the dialog open with the reason. Whether it sent.
    fn send(&mut self, specs: Vec<ActionSpec>, cx: &mut Context<Self>) -> bool {
        if specs.is_empty() {
            self.error = Some(format!("{}.", self.kind.nothing()));
            cx.notify();
            return false;
        }
        let environment_id = self.environment_id.clone();
        let sent = self.state.update(cx, |state, cx| {
            let mut sent = Ok(0);
            for spec in specs {
                sent = match environment_id.as_deref() {
                    Some(id) => state.submit_in(id, spec),
                    None => state.submit(spec),
                };
                if sent.is_err() {
                    break;
                }
            }
            cx.notify();
            sent
        });
        match sent {
            Ok(_) => {
                cx.emit(DialogEvent::Close);
                true
            }
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                false
            }
        }
    }

    /// From the confirmation back to the form.
    fn back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.confirming = false;
        if let Some((_, input)) = self.inputs.first() {
            input.focus(window, cx);
        }
        cx.notify();
    }

    fn input(&self, field: FormField) -> Option<&Text> {
        self.inputs
            .iter()
            .find(|(candidate, _)| *candidate == field)
            .map(|(_, input)| input)
    }

    /// A labelled text field with its problem.
    fn text_field(&self, label: &'static str, field: FormField, issues: &Issues) -> Field {
        let error = issues.get(&field).cloned();
        let control: AnyElement = match self.input(field) {
            Some(Text::Line(input)) => TextField::new(input)
                .bordered(true)
                .invalid(error.is_some())
                .into_any_element(),
            Some(Text::Area(input)) => TextArea::new(input)
                .height(px(54.))
                .invalid(error.is_some())
                .into_any_element(),
            None => div().into_any_element(),
        };
        Field::new(label).control(control).error(error)
    }

    /// A time field: its meaning as the status, its problem under it.
    fn time_field(
        &self,
        label: &'static str,
        field: FormField,
        parsed: Option<&Result<Timestamp, String>>,
        issues: &Issues,
    ) -> Field {
        let field_element = self.text_field(label, field, issues);
        match parsed {
            Some(Ok(at)) => {
                field_element.status(when::describe(*at, Timestamp::now()), FieldTone::Good)
            }
            Some(Err(_)) if !issues.contains_key(&field) => {
                field_element.status("not a time yet", FieldTone::Neutral)
            }
            _ => field_element,
        }
    }

    /// Quick choices that type into `field`.
    fn chips(
        &self,
        id: &'static str,
        field: FormField,
        presets: &[Preset],
        cx: &Context<Self>,
    ) -> AnyElement {
        let current = self.form.text(field).trim().to_owned();
        div()
            .flex()
            .flex_wrap()
            .gap(px(6.))
            .children(presets.iter().enumerate().map(|(index, preset)| {
                let text = preset.text;
                Chip::new(SharedString::from(format!("{id}-{index}")), preset.label)
                    .selected(current == text)
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.fill(field, text, window, cx);
                    }))
            }))
            .into_any_element()
    }

    /// Whether the target box shows: always for a removal and for a
    /// host's downtime (the host and its services); otherwise when the
    /// action has several objects or skips some.
    fn shows_targets(&self) -> bool {
        match &self.form {
            Form::Removal(_) => true,
            Form::Downtime(_)
                if self
                    .eligible
                    .targets
                    .iter()
                    .any(|target| matches!(target, ObjectKey::Host { .. })) =>
            {
                true
            }
            _ => self.eligible.targets.len() > 1 || !self.eligible.skipped.is_empty(),
        }
    }

    /// The box listing every object it acts on (or every downtime a removal
    /// removes), scrolling beyond its height, and what it skips.
    fn render_targets(&self, theme: &Theme, cx: &Context<Self>) -> Option<AnyElement> {
        if !self.shows_targets() {
            return None;
        }
        let colors = theme.colors;
        let most = match self.kind {
            DialogKind::RemoveDowntime => REMOVAL_BOX,
            _ => TARGET_BOX,
        } - 2. * TARGET_PADDING;
        #[expect(
            clippy::cast_precision_loss,
            reason = "a row count, far below f32's exact range"
        )]
        let rows = self.box_rows.max(self.listed.len()) as f32;
        let height = (rows * TARGET_ROW).min(most).max(TARGET_ROW);
        let mut column = div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .text_size(theme.text.small);
        if self.listed.is_empty() {
            column = column.child(
                div()
                    .text_color(theme.states.text.warning)
                    .child(format!("{}.", self.kind.nothing())),
            );
        } else {
            let scroll = self.target_scroll.clone();
            column = column.child(
                div()
                    .relative()
                    .h(px(height))
                    .child(
                        uniform_list(
                            "dialog-targets",
                            self.listed.len(),
                            cx.processor(|this, range: Range<usize>, _window, cx| {
                                this.render_target_rows(range, cx)
                            }),
                        )
                        .track_scroll(&scroll)
                        .size_full(),
                    )
                    // Always shown while rows are out of view, as drawn.
                    .child(Scrollbar::vertical(&scroll).mode(ScrollbarMode::Always)),
            );
        }
        let box_element = div()
            .px(px(12.))
            .py(px(TARGET_PADDING))
            .rounded(theme.metrics.code_radius)
            .bg(colors.code_background)
            .child(column);
        let skipped = (!self.eligible.skipped.is_empty()).then(|| {
            let names: Vec<String> = self
                .eligible
                .skipped
                .iter()
                .take(3)
                .map(|skipped| {
                    format!(
                        "{} {}",
                        describe_objects(std::slice::from_ref(&skipped.object)),
                        skipped.reason
                    )
                })
                .collect();
            let more = self.eligible.skipped.len().saturating_sub(names.len());
            let text = if more > 0 {
                format!("skipped: {}, + {more} more", names.join(", "))
            } else {
                format!("skipped: {}", names.join(", "))
            };
            div()
                .text_size(theme.text.small)
                .text_color(colors.text_faint)
                .child(text)
        });
        Some(
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(box_element)
                .children(skipped)
                .into_any_element(),
        )
    }

    /// The target box's rows in `range` (the ones in view): the object's
    /// mark (hollow = handled), `service on host` or the host, and a faint
    /// detail.
    fn render_target_rows(&self, range: Range<usize>, cx: &Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme();
        let state = self.state.read(cx);
        let snapshot = self
            .environment_id
            .as_deref()
            .and_then(|id| state.snapshot_of(id))
            .unwrap_or_else(|| state.snapshot());
        let listed = Rc::clone(&self.listed);
        listed[range.start.min(listed.len())..range.end.min(listed.len())]
            .iter()
            .map(|row| {
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .h(px(TARGET_ROW))
                    .whitespace_nowrap()
                    .text_size(theme.text.small)
                    .child(match object_mark(snapshot, &row.object) {
                        Some(mark) => StateDot::mark(mark).size(px(7.)),
                        None => StateDot::with_color(theme.states.fill.pending).size(px(7.)),
                    })
                    .child(
                        div()
                            .flex_none()
                            .text_color(theme.colors.text)
                            .child(describe_objects(std::slice::from_ref(&row.object))),
                    )
                    .when(!row.detail.is_empty(), |line| {
                        line.child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_color(theme.colors.text_faint)
                                .child(row.detail.clone()),
                        )
                    })
                    .into_any_element()
            })
            .collect()
    }

    fn render_ack(&self, form: &AckForm, issues: &Issues, cx: &Context<Self>) -> Vec<AnyElement> {
        let expiry = form
            .expires
            .then(|| when::parse_time(&form.expiry, Timestamp::now(), Timestamp::now()));
        let mut blocks = vec![
            self.text_field("comment", FormField::Comment, issues)
                .hint("Enter acknowledges · shift-enter for a new line")
                .into_any_element(),
            div()
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(
                    Switch::new("ack-sticky", form.sticky)
                        .label("sticky: stays until the object is OK, through other problem states")
                        .on_change(cx.listener(|this, on: &bool, _, cx| {
                            if let Form::Ack(form) = &mut this.form {
                                form.sticky = *on;
                            }
                            cx.notify();
                        })),
                )
                .child(
                    Switch::new("ack-persistent", form.persistent)
                        .label("persistent: keep the comment after the acknowledgement ends")
                        .on_change(cx.listener(|this, on: &bool, _, cx| {
                            if let Form::Ack(form) = &mut this.form {
                                form.persistent = *on;
                            }
                            cx.notify();
                        })),
                )
                .child(
                    Switch::new("ack-expires", form.expires)
                        .label("expires")
                        .on_change(cx.listener(|this, on: &bool, window, cx| {
                            if let Form::Ack(form) = &mut this.form {
                                form.expires = *on;
                            }
                            this.focus_expiry(*on, window, cx);
                            cx.notify();
                        })),
                )
                .into_any_element(),
        ];
        if form.expires {
            blocks.push(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(self.time_field(
                        "expires at",
                        FormField::Expiry,
                        expiry.as_ref(),
                        issues,
                    ))
                    .child(self.chips("ack-expiry", FormField::Expiry, &EXPIRY_PRESETS, cx))
                    .into_any_element(),
            );
        }
        blocks
    }

    /// Puts the keyboard in the expiry field when it appears.
    fn focus_expiry(&self, shown: bool, window: &mut Window, cx: &mut App) {
        if shown && let Some(input) = self.input(FormField::Expiry) {
            input.focus(window, cx);
        }
    }

    fn render_downtime(
        &self,
        form: &DowntimeForm,
        issues: &Issues,
        cx: &Context<Self>,
    ) -> Vec<AnyElement> {
        let now = Timestamp::now();
        let (start, end) = form.window(now);
        let hosts = self
            .eligible
            .targets
            .iter()
            .filter(|target| matches!(target, ObjectKey::Host { .. }))
            .count();
        let mut blocks = vec![
            self.text_field("comment", FormField::Comment, issues)
                .hint("Enter schedules it · shift-enter for a new line")
                .into_any_element(),
            div()
                .flex()
                .gap(px(10.))
                .child(div().flex_1().min_w_0().child(self.time_field(
                    "start",
                    FormField::Start,
                    Some(&start),
                    issues,
                )))
                .child(div().flex_1().min_w_0().child(self.time_field(
                    "end",
                    FormField::End,
                    Some(&end),
                    issues,
                )))
                .into_any_element(),
            self.chips("downtime-end", FormField::End, &END_PRESETS, cx),
            Field::new("type")
                .control(
                    Segmented::new("downtime-type")
                        .option("fixed: the whole window")
                        .option("flexible: from the first problem")
                        .selected(usize::from(form.flexible))
                        .on_select(cx.listener(|this, index: &usize, window, cx| {
                            let flexible = *index == 1;
                            if let Form::Downtime(form) = &mut this.form {
                                form.flexible = flexible;
                            }
                            if flexible && let Some(input) = this.input(FormField::Duration) {
                                input.focus(window, cx);
                            }
                            cx.notify();
                        })),
                )
                .into_any_element(),
        ];
        if form.flexible {
            blocks.push(
                self.text_field("duration", FormField::Duration, issues)
                    .hint("How long it lasts once a problem in the window starts it.")
                    .into_any_element(),
            );
        }
        if hosts > 0 {
            blocks.push(Self::child_hosts(form, cx));
        }
        let trigger = self.text_field("triggered by", FormField::Trigger, issues);
        blocks.push(if self.triggers.is_empty() {
            trigger
                .hint(
                    "Another downtime's full name: this one starts when it does (optional). \
                     A pane's ··· menu copies its downtime's.",
                )
                .into_any_element()
        } else {
            trigger
                .hint("Another downtime: this one starts when it does (optional). Pick one:")
                .into_any_element()
        });
        if !self.triggers.is_empty() {
            blocks.push(self.trigger_choices(&form.trigger, cx));
        }
        blocks
    }

    /// The downtimes to pick as the trigger: picking one fills the field
    /// with its name; picking it again clears it.
    fn trigger_choices(&self, current: &str, cx: &Context<Self>) -> AnyElement {
        let current = current.trim();
        div()
            .flex()
            .flex_wrap()
            .gap(px(6.))
            .children(self.triggers.iter().enumerate().map(|(index, choice)| {
                let selected = current == choice.name;
                let name = if selected {
                    String::new()
                } else {
                    choice.name.clone()
                };
                Chip::new(
                    SharedString::from(format!("downtime-trigger-{index}")),
                    choice.label.clone(),
                )
                .selected(selected)
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.fill(FormField::Trigger, &name, window, cx);
                }))
            }))
            .into_any_element()
    }

    /// Picks the trigger choice `index` (tests).
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn pick_trigger(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = self.triggers[index].name.clone();
        self.fill(FormField::Trigger, &name, window, cx);
    }

    /// The downtimes offered as the trigger (tests).
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn trigger_offers(&self) -> &[TriggerChoice] {
        &self.triggers
    }

    /// For hosts: downtimes for their child hosts.
    fn child_hosts(form: &DowntimeForm, cx: &Context<Self>) -> AnyElement {
        let options = [
            ChildOptions::None,
            ChildOptions::Triggered,
            ChildOptions::NonTriggered,
        ];
        let selected = options
            .iter()
            .position(|option| *option == form.child_options)
            .unwrap_or(0);
        Field::new("child hosts")
            .control(
                Segmented::new("downtime-children")
                    .option("none")
                    .option("triggered")
                    .option("independent")
                    .selected(selected)
                    .on_select(cx.listener(move |this, index: &usize, _, cx| {
                        if let Form::Downtime(form) = &mut this.form {
                            form.child_options = options.get(*index).copied().unwrap_or_default();
                        }
                        cx.notify();
                    })),
            )
            .hint(
                "Downtimes for the hosts that depend on these: triggered ones start with \
                     this one, independent ones keep its window.",
            )
            .into_any_element()
    }

    /// For a host's downtime: `all services`, on by default, right above
    /// the box that lists what it targets (the host and each of its
    /// services; the host alone when off).
    fn all_services_switch(&self, snapshot: &Snapshot, cx: &Context<Self>) -> Option<AnyElement> {
        let Form::Downtime(form) = &self.form else {
            return None;
        };
        let hosts: Vec<&ic_model::HostName> = self
            .eligible
            .targets
            .iter()
            .filter_map(|target| match target {
                ObjectKey::Host { name } => Some(name),
                ObjectKey::Service { .. } => None,
            })
            .collect();
        if hosts.is_empty() {
            return None;
        }
        let services: usize = hosts
            .iter()
            .map(|host| snapshot.services_of(host).count())
            .sum();
        let noun = if services == 1 { "service" } else { "services" };
        let label = if hosts.len() == 1 {
            format!("all services: the host and its {services} {noun}")
        } else {
            format!("all services: the hosts and their {services} {noun}")
        };
        Some(
            Switch::new("downtime-all-services", form.all_services)
                .label(label)
                .on_change(cx.listener(|this, on: &bool, _, cx| {
                    if let Form::Downtime(form) = &mut this.form {
                        form.all_services = *on;
                    }
                    this.refresh_listed(cx);
                    cx.notify();
                }))
                .into_any_element(),
        )
    }

    /// For a service's downtime that belongs to its host's: what it is,
    /// and the choice of scope (← / → choose it, as the hint says).
    fn scope_choice(removal: &Removal, theme: &Theme, cx: &Context<Self>) -> Vec<AnyElement> {
        let colors = theme.colors;
        let mut blocks = Vec::new();
        let host = removal.host.clone().unwrap_or_default();
        let text = format!(
            "This downtime was scheduled on the host {host} for the host and all its \
             services. Remove it for:"
        );
        let start = "This downtime was scheduled on the host ".len();
        blocks.push(
            div()
                .flex()
                .items_end()
                .gap(px(12.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(theme.text.body)
                        .line_height(gpui::relative(1.5))
                        .text_color(colors.text_secondary)
                        .child(
                            gpui::StyledText::new(SharedString::from(text)).with_highlights([(
                                start..start + host.len(),
                                gpui::HighlightStyle {
                                    color: Some(colors.text_strong),
                                    ..gpui::HighlightStyle::default()
                                },
                            )]),
                        ),
                )
                // The keys that choose the scope, as the buttons show
                // theirs.
                .child(
                    div()
                        .flex_none()
                        .text_size(theme.text.hint)
                        .text_color(colors.text_faint)
                        .child("← →"),
                )
                .into_any_element(),
        );
        let mut choice = Segmented::new("removal-scope");
        for scope in &removal.scopes {
            choice = choice.option(scope.label.clone());
        }
        blocks.push(
            choice
                .selected(removal.chosen)
                .on_select(cx.listener(|this, index: &usize, _, cx| {
                    if let Form::Removal(removal) = &mut this.form {
                        removal.chosen = *index;
                    }
                    this.refresh_listed(cx);
                    cx.notify();
                }))
                .into_any_element(),
        );
        blocks
    }

    /// The removal's body: the choice of scope for a service's downtime
    /// that belongs to its host's, the list of every downtime it removes
    /// with what they share, what it skips, and what follows.
    fn render_removal(
        &self,
        removal: &Removal,
        snapshot: &Snapshot,
        theme: &Theme,
        cx: &Context<Self>,
    ) -> Vec<AnyElement> {
        let colors = theme.colors;
        let now = Timestamp::now();
        let mut blocks = if removal.scopes.len() > 1 {
            Self::scope_choice(removal, theme, cx)
        } else {
            Vec::new()
        };
        let scope = removal.scope();
        let count = scope.removed.len();
        let mut list = Field::new(format!(
            "{count} {}",
            if count == 1 { "downtime" } else { "downtimes" }
        ));
        if let Some(shared) = removal.shared(snapshot, now) {
            list = list.status(shared, FieldTone::Neutral);
        }
        if let Some(targets) = self.render_targets(theme, cx) {
            list = list.control(targets);
        }
        blocks.push(list.into_any_element());
        if !scope.skipped.is_empty() {
            let count = scope.skipped.len();
            blocks.push(
                div()
                    .text_size(theme.text.label)
                    .text_color(colors.text_faint)
                    .child(format!(
                        "skipped: {count} from the config ({}): Icinga refuses to remove {}, and \
                         the config brings {} back",
                        scope
                            .skipped
                            .iter()
                            .map(|skipped| describe_objects(std::slice::from_ref(&skipped.object)))
                            .collect::<Vec<_>>()
                            .join(", "),
                        if count == 1 { "it" } else { "them" },
                        if count == 1 { "it" } else { "them" },
                    ))
                    .into_any_element(),
            );
        }
        // With a choice of scope, what follows keeps two lines' room, so a
        // shorter text for the other scope doesn't move the buttons.
        let reserve = removal.scopes.len() > 1;
        blocks.push(
            div()
                .text_size(theme.text.label)
                .line_height(gpui::relative(1.45))
                .when(reserve, |text| {
                    text.min_h((theme.text.label * 1.45).ceil() * 2. + px(1.))
                })
                .text_color(colors.text_faint)
                .child(removal.consequence(snapshot, now))
                .into_any_element(),
        );
        blocks
    }

    fn render_comment(
        &self,
        form: &CommentForm,
        issues: &Issues,
        cx: &Context<Self>,
    ) -> Vec<AnyElement> {
        let expiry = form
            .expires
            .then(|| when::parse_time(&form.expiry, Timestamp::now(), Timestamp::now()));
        let mut blocks = vec![
            self.text_field("comment", FormField::Comment, issues)
                .hint("Enter adds it · shift-enter for a new line")
                .into_any_element(),
            Switch::new("comment-expires", form.expires)
                .label("expires")
                .on_change(cx.listener(|this, on: &bool, window, cx| {
                    if let Form::Comment(form) = &mut this.form {
                        form.expires = *on;
                    }
                    this.focus_expiry(*on, window, cx);
                    cx.notify();
                }))
                .into_any_element(),
        ];
        if form.expires {
            blocks.push(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(self.time_field(
                        "expires at",
                        FormField::Expiry,
                        expiry.as_ref(),
                        issues,
                    ))
                    .child(self.chips("comment-expiry", FormField::Expiry, &EXPIRY_PRESETS, cx))
                    .into_any_element(),
            );
        }
        blocks
    }

    fn render_result(
        &self,
        form: &ResultForm,
        issues: &Issues,
        cx: &Context<Self>,
    ) -> Vec<AnyElement> {
        let hosts = self
            .eligible
            .targets
            .iter()
            .filter(|target| matches!(target, ObjectKey::Host { .. }))
            .count();
        let only_hosts = hosts > 0 && hosts == self.eligible.targets.len();
        // Hosts know UP (0) and DOWN (2, which Icinga's API takes as 1).
        let (labels, statuses): (&[&str], &[u8]) = if only_hosts {
            (&["up", "down"], &[0, 2])
        } else {
            (&["ok", "warning", "critical", "unknown"], &[0, 1, 2, 3])
        };
        let selected = statuses
            .iter()
            .position(|status| *status == form.exit_status)
            .unwrap_or(0);
        let mut control = Segmented::new("result-state");
        for label in labels {
            control = control.option(*label);
        }
        let statuses = statuses.to_vec();
        let control =
            control
                .selected(selected)
                .on_select(cx.listener(move |this, index: &usize, _, cx| {
                    if let Form::Result(form) = &mut this.form {
                        form.exit_status = statuses.get(*index).copied().unwrap_or(0);
                    }
                    cx.notify();
                }));
        let mut state = Field::new("state").control(control);
        if hosts > 0 && !only_hosts {
            state = state.hint("Hosts take ok and warning as UP, critical and unknown as DOWN.");
        }
        vec![
            state.into_any_element(),
            self.text_field("plugin output", FormField::Output, issues)
                .hint("The first line is the output, the rest the long output · shift-enter for a new line")
                .into_any_element(),
            self.text_field("performance data", FormField::Perfdata, issues)
                .hint("label=value[unit];warn;crit;min;max, separated by spaces (optional)")
                .into_any_element(),
        ]
    }

    fn render_command(
        &self,
        form: &CommandForm,
        issues: &Issues,
        cx: &Context<Self>,
    ) -> Vec<AnyElement> {
        let endpoints = if self.endpoints.is_empty() {
            "Where to run it.".to_owned()
        } else {
            let mut names: Vec<&str> = self.endpoints.iter().map(String::as_str).take(6).collect();
            if self.endpoints.len() > names.len() {
                names.push("…");
            }
            format!("Known: {}.", names.join(", "))
        };
        vec![
            Field::new("command")
                .control(
                    Segmented::new("command-type")
                        .option("event command")
                        .option("check command")
                        .selected(usize::from(form.command_type == CommandType::CheckCommand))
                        .on_select(cx.listener(|this, index: &usize, _, cx| {
                            if let Form::Command(form) = &mut this.form {
                                form.command_type = if *index == 1 {
                                    CommandType::CheckCommand
                                } else {
                                    CommandType::EventCommand
                                };
                            }
                            cx.notify();
                        })),
                )
                .into_any_element(),
            self.text_field("command name", FormField::Command, issues)
                .hint("Another command of that type by name; blank runs the object's own.")
                .into_any_element(),
            self.text_field("endpoint", FormField::Endpoint, issues)
                .hint(endpoints)
                .into_any_element(),
            self.text_field("macros", FormField::Macros, issues)
                .hint("Overrides for the command's macros (optional) · enter for a new line")
                .into_any_element(),
            self.text_field("keep the result for", FormField::Ttl, issues)
                .hint("How long Icinga keeps the execution's result.")
                .into_any_element(),
        ]
    }

    /// The run command dialog's second step: what will run where.
    fn render_confirmation(&self, form: &CommandForm, theme: &Theme) -> Vec<AnyElement> {
        let colors = theme.colors;
        let what = describe_objects(&self.eligible.targets);
        let kind = match form.command_type {
            CommandType::CheckCommand => "check command",
            CommandType::EventCommand => "event command",
        };
        let command = form.command.trim();
        let endpoint = form.endpoint.trim();
        let macros = super::forms::parse_macros(&form.macros).unwrap_or_default();
        let macros = if macros.is_empty() {
            "none".to_owned()
        } else {
            macros
                .iter()
                .map(|(name, value)| format!("{name} = {value}"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let table = KvTable::new()
            .row(
                "command",
                if command.is_empty() {
                    format!("the object's own {kind}")
                } else {
                    format!("{command} ({kind})")
                },
            )
            .row(
                "endpoint",
                if endpoint.is_empty() {
                    self.endpoint_default.clone()
                } else {
                    endpoint.to_owned()
                },
            )
            .row("macros", macros)
            .row("result kept", form.ttl.trim().to_owned());
        vec![
            div()
                .text_size(theme.text.body)
                .text_color(colors.text_strong)
                .child(format!("Run the {kind} of {what} now?"))
                .into_any_element(),
            table.into_any_element(),
            div()
                .text_size(theme.text.small)
                .text_color(theme.states.text.warning)
                .child("Icinga runs it at once on the endpoint; it can't be called back.")
                .into_any_element(),
        ]
    }
}

impl ActionDialog {
    /// Who the action is recorded for, and the buttons: cancel and send,
    /// or back and run while confirming a command.
    fn footer(&self, body: DialogBody, confirming: bool, cx: &Context<Self>) -> DialogBody {
        let mut note = if self.author.is_empty() {
            String::new()
        } else {
            format!("as {}", self.author)
        };
        if self.kind == DialogKind::Acknowledge {
            if !note.is_empty() {
                note.push_str(" · ");
            }
            note.push_str("Icinga isn't asked to notify");
        }
        let body = if note.is_empty() {
            body
        } else {
            body.footer_start(div().text_color(cx.theme().colors.text_faint).child(note))
        };
        if confirming {
            let secondary_enter = if cfg!(target_os = "macos") {
                "⌘↵"
            } else {
                "ctrl-↵"
            };
            return body
                .action(Button::new("action-back", "back").on_click(
                    cx.listener(|this, _: &ClickEvent, window, cx| this.back(window, cx)),
                ))
                .action(
                    Button::new("action-run", "run command")
                        .variant(ButtonVariant::Danger)
                        .key_hint(secondary_enter)
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.submit(window, cx);
                        })),
                );
        }
        body.action(
            Button::new("action-cancel", "cancel")
                .key_hint("esc")
                .on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.emit(DialogEvent::Close))),
        )
        .action({
            let removal = matches!(self.form, Form::Removal(_));
            let mut button = Button::new("action-submit", self.submit_label()).key_hint("↵");
            if let Some(width) = self.submit_width {
                button = button.width(width);
            }
            let button = button
                .disabled(self.eligible.targets.is_empty() || (removal && self.listed.is_empty()))
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.submit(window, cx);
                }));
            if removal {
                button.variant(ButtonVariant::Danger)
            } else {
                button.primary()
            }
        })
    }
}

/// The main button's label for `count` listed targets: `schedule 24
/// downtimes`, `remove downtime`; the others say what they do.
fn submit_label(kind: DialogKind, form: &Form, count: usize) -> String {
    match (form, count) {
        (Form::Downtime(_), 0 | 1) => "schedule downtime".to_owned(),
        (Form::Downtime(_), count) => format!("schedule {count} downtimes"),
        (Form::Removal(_), 0 | 1) => "remove downtime".to_owned(),
        (Form::Removal(_), count) => format!("remove {count} downtimes"),
        _ => kind.submit_label().to_owned(),
    }
}

/// What the endpoint field means when left blank.
fn endpoint_default(snapshot: &Snapshot, targets: &[ObjectKey]) -> String {
    let node = snapshot
        .status
        .as_ref()
        .map(|status| status.node_name.clone())
        .filter(|name| !name.is_empty());
    let own = |object: &ObjectKey| -> Option<String> {
        match object {
            ObjectKey::Host { name } => snapshot.hosts.get(name)?.check.command_endpoint.clone(),
            ObjectKey::Service { key } => {
                snapshot.services.get(key)?.check.command_endpoint.clone()
            }
        }
    };
    match targets {
        [one] => match (own(one), node) {
            (Some(endpoint), _) => format!("its command endpoint, {endpoint}"),
            (None, Some(node)) => format!("{node} (the endpoint icygui talks to)"),
            (None, None) => "the endpoint icygui talks to".to_owned(),
        },
        _ => match node {
            Some(node) => format!("each object's command endpoint, else {node}"),
            None => "each object's command endpoint, else the endpoint icygui talks to".to_owned(),
        },
    }
}

/// An object's mark for its dot: its state, hollow when it counts as
/// handled.
pub(crate) fn object_mark(snapshot: &Snapshot, object: &ObjectKey) -> Option<ObjectMark> {
    match object {
        ObjectKey::Host { name } => snapshot.hosts.get(name).map(|host| ObjectMark::host(host)),
        ObjectKey::Service { key } => snapshot
            .services
            .get(key)
            .map(|service| ObjectMark::service(service, snapshot.host_of(key).map(AsRef::as_ref))),
    }
}

/// A row of the target box: the object, and a faint detail (`host`, a
/// host downtime's window).
#[derive(Clone, Debug, PartialEq)]
struct Listed {
    object: ObjectKey,
    detail: String,
}

/// What the target box lists: for a removal, every downtime it removes
/// (a host's with its window); for a downtime with `all services`, each
/// host followed by each of its services (problems first); otherwise the
/// objects.
fn listed_targets(eligible: &Eligible, form: &Form, snapshot: &Snapshot) -> Vec<Listed> {
    let now = Timestamp::now();
    match form {
        Form::Removal(removal) => removal
            .scope()
            .removed
            .iter()
            .map(|removed| Listed {
                object: removed.object.clone(),
                detail: match &removed.object {
                    ObjectKey::Host { .. } => {
                        downtimes::find(snapshot, Some(&removed.object), &removed.name).map_or_else(
                            || "host".to_owned(),
                            |downtime| format!("host · {}", downtimes::clock_window(downtime, now)),
                        )
                    }
                    ObjectKey::Service { .. } => String::new(),
                },
            })
            .collect(),
        Form::Downtime(form) => {
            let mut seen = std::collections::HashSet::new();
            let mut rows = Vec::new();
            for target in &eligible.targets {
                if !seen.insert(target.clone()) {
                    continue;
                }
                let ObjectKey::Host { name } = target else {
                    rows.push(Listed {
                        object: target.clone(),
                        detail: String::new(),
                    });
                    continue;
                };
                rows.push(Listed {
                    object: target.clone(),
                    detail: "host".to_owned(),
                });
                if !form.all_services {
                    continue;
                }
                let mut services: Vec<_> = snapshot.services_of(name).collect();
                services.sort_by(|a, b| {
                    b.severity()
                        .cmp(&a.severity())
                        .then_with(|| a.display_name.cmp(&b.display_name))
                });
                for service in services {
                    let key = service.object_key();
                    if seen.insert(key.clone()) {
                        rows.push(Listed {
                            object: key,
                            detail: String::new(),
                        });
                    }
                }
            }
            rows
        }
        _ => eligible
            .targets
            .iter()
            .map(|target| Listed {
                object: target.clone(),
                detail: String::new(),
            })
            .collect(),
    }
}

/// What a removal sends: one `remove-downtime` for the downtimes it lists,
/// by their full names (their children go with them; Icinga takes the
/// names in batches). Nothing is removed that the dialog didn't list.
pub(crate) fn removal_specs(removal: &Removal) -> Vec<ActionSpec> {
    let mut names = Vec::new();
    let mut objects = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for Send::One {
        name,
        objects: changed,
    } in &removal.scope().send
    {
        names.push(name.clone());
        for object in changed {
            if seen.insert(object.clone()) {
                objects.push(object.clone());
            }
        }
    }
    if names.is_empty() {
        return Vec::new();
    }
    vec![ActionSpec {
        kind: ObjectAction::RemoveNamedDowntimes(names.clone()),
        action: ic_model::Action::RemoveAllDowntimes,
        target: ActionTarget::Downtimes(names),
        objects,
    }]
}

/// An object's state for its dot.
pub(crate) fn object_state(snapshot: &Snapshot, object: &ObjectKey) -> Option<CheckableState> {
    match object {
        ObjectKey::Host { name } => snapshot
            .hosts
            .get(name)
            .map(|host| CheckableState::Host(host.state)),
        ObjectKey::Service { key } => snapshot
            .services
            .get(key)
            .map(|service| CheckableState::Service(service.state)),
    }
}

impl ActionDialog {
    /// What the title names: the objects, `k8s-node-04 and its 23
    /// services` for a host's downtime with all services.
    fn describe_what(&self, snapshot: &Snapshot) -> String {
        if let (Form::Downtime(form), [ObjectKey::Host { name }]) =
            (&self.form, self.eligible.targets.as_slice())
            && form.all_services
        {
            let services = snapshot.services_of(name).count();
            if services > 0 {
                return format!(
                    "{name} and its {services} {}",
                    if services == 1 { "service" } else { "services" }
                );
            }
        }
        describe_objects(&self.eligible.targets)
    }

    /// The form's fields (the check dialog has none: what it does).
    fn form_blocks(&self, issues: &Issues, theme: &Theme, cx: &Context<Self>) -> Vec<AnyElement> {
        match &self.form {
            Form::Ack(form) => self.render_ack(form, issues, cx),
            Form::Downtime(form) => self.render_downtime(form, issues, cx),
            Form::Comment(form) => self.render_comment(form, issues, cx),
            Form::Result(form) => self.render_result(form, issues, cx),
            Form::Command(form) => self.render_command(form, issues, cx),
            Form::Check => vec![
                div()
                    .text_size(theme.text.small)
                    .text_color(theme.colors.text_muted)
                    .child(
                        "Icinga runs their checks at once, forced, on the endpoints that run them.",
                    )
                    .into_any_element(),
            ],
            // Drawn with its list (`render_removal`).
            Form::Removal(_) => Vec::new(),
        }
    }
}

impl ActionDialog {
    /// The keys that apply: the confirmation step's, the removal's (← / →
    /// choose the scope), a dialog without fields', or the fields'.
    fn key_context(&self) -> &'static str {
        if self.confirming {
            CONFIRM_CONTEXT
        } else if matches!(self.form, Form::Removal(_)) {
            REMOVAL_CONTEXT
        } else if self.inputs.is_empty() {
            FIELDLESS_CONTEXT
        } else {
            DIALOG_CONTEXT
        }
    }
}

impl Render for ActionDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let colors = theme.colors;
        let issues = self.issues();
        let (snapshot, elsewhere) = {
            let state = self.state.read(cx);
            match self.environment_id.as_deref() {
                Some(id) if !state.is_active(id) => (
                    state
                        .snapshot_of(id)
                        .cloned()
                        .unwrap_or_else(|| state.snapshot().clone()),
                    state
                        .environment_by_id(id)
                        .map(|environment| environment.name.clone()),
                ),
                _ => (state.snapshot().clone(), None),
            }
        };
        // Another environment's objects: the title names it.
        let what = match elsewhere {
            Some(name) => format!("{} in {name}", self.describe_what(&snapshot)),
            None => self.describe_what(&snapshot),
        };
        let title = div()
            .flex()
            .items_center()
            .gap(px(10.))
            .min_w_0()
            .child(div().flex_none().child(self.kind.title()))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(theme.text.body)
                    .font_weight(FontWeight::NORMAL)
                    .text_color(colors.text_muted)
                    .child(if self.eligible.targets.is_empty() {
                        String::new()
                    } else {
                        what
                    }),
            );
        let mut body = DialogBody::new(title);
        let confirming = match (&self.form, self.confirming) {
            (Form::Command(form), true) => Some(form.clone()),
            _ => None,
        };
        if let Some(form) = &confirming {
            for block in self.render_confirmation(form, &theme) {
                body = body.child(block);
            }
        } else if let Form::Removal(removal) = &self.form {
            for block in self.render_removal(removal, &snapshot, &theme, cx) {
                body = body.child(block);
            }
        } else {
            let switch = self.all_services_switch(&snapshot, cx);
            let targets = self.render_targets(&theme, cx);
            if switch.is_some() || targets.is_some() {
                body = body.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .children(switch)
                        .children(targets),
                );
            }
            for block in self.form_blocks(&issues, &theme, cx) {
                body = body.child(block);
            }
        }
        if let Some(error) = &self.error {
            body = body.child(
                div()
                    .text_size(theme.text.small)
                    .text_color(theme.states.text.critical)
                    .child(error.clone()),
            );
        }
        let body = self.footer(body, confirming.is_some(), cx);
        div()
            .id("action-dialog")
            .key_context(self.key_context())
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::on_next_field))
            .on_action(cx.listener(Self::on_previous_field))
            .on_action(cx.listener(Self::on_confirm))
            .on_action(cx.listener(Self::on_send))
            .on_action(cx.listener(Self::on_send_now))
            .on_action(cx.listener(Self::on_next_scope))
            .on_action(cx.listener(Self::on_previous_scope))
            .flex()
            .flex_col()
            .min_h_0()
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ic_model::{Host, InstanceStatus, Service};

    use super::*;

    #[test]
    fn every_dialog_action_has_its_dialog() {
        for kind in [
            DialogKind::Acknowledge,
            DialogKind::Downtime,
            DialogKind::Comment,
            DialogKind::CheckResult,
            DialogKind::Command,
        ] {
            assert_eq!(DialogKind::for_action(&kind.action()), Some(kind));
            assert!(!text_fields(kind).is_empty());
            let form = Form::new(kind);
            for (field, _, _) in text_fields(kind) {
                let mut edited = form.clone();
                edited.set_text(*field, "typed".to_owned());
                assert_eq!(edited.text(*field), "typed", "{kind:?} {field:?}");
            }
        }
        assert_eq!(DialogKind::for_action(&ObjectAction::CheckNow), None);
        // Checks named by a palette query are listed first, in a dialog
        // without fields that sends a forced check.
        let check = DialogKind::for_review(&ObjectAction::CheckNow).unwrap();
        assert_eq!(check.action(), ObjectAction::CheckNow);
        assert!(text_fields(check).is_empty());
        assert_eq!(
            Form::new(check).action(Timestamp::now(), &[]),
            Ok(ic_model::Action::CheckNow { force: true })
        );
        assert_eq!(
            DialogKind::for_review(&ObjectAction::Acknowledge),
            Some(DialogKind::Acknowledge)
        );
    }

    #[test]
    fn hidden_fields_are_skipped() {
        let mut form = Form::new(DialogKind::Acknowledge);
        assert!(!form.shows(FormField::Expiry));
        if let Form::Ack(ack) = &mut form {
            ack.expires = true;
        }
        assert!(form.shows(FormField::Expiry));
        assert!(!Form::new(DialogKind::Downtime).shows(FormField::Duration));
        assert!(Form::new(DialogKind::Downtime).shows(FormField::End));
    }

    #[test]
    fn the_endpoint_defaults_to_the_objects_own() {
        let mut remote = Service::new("db-01", "disk");
        remote.check.command_endpoint = Some("sat-eu-01".to_owned());
        let local = Host::new("db-01");
        let snapshot = Snapshot {
            services: Arc::new(
                [(remote.key.clone(), Arc::new(remote))]
                    .into_iter()
                    .collect(),
            ),
            hosts: Arc::new(
                [(local.name.clone(), Arc::new(local))]
                    .into_iter()
                    .collect(),
            ),
            status: Some(Arc::new(InstanceStatus {
                node_name: "master-01".to_owned(),
                ..InstanceStatus::default()
            })),
            ..Snapshot::default()
        };
        assert_eq!(
            endpoint_default(&snapshot, &[ObjectKey::service("db-01", "disk")]),
            "its command endpoint, sat-eu-01"
        );
        assert_eq!(
            endpoint_default(&snapshot, &[ObjectKey::host("db-01")]),
            "master-01 (the endpoint icygui talks to)"
        );
        assert_eq!(
            endpoint_default(
                &snapshot,
                &[
                    ObjectKey::host("db-01"),
                    ObjectKey::service("db-01", "disk")
                ]
            ),
            "each object's command endpoint, else master-01"
        );
        assert_eq!(
            endpoint_default(&Snapshot::default(), &[ObjectKey::host("x")]),
            "the endpoint icygui talks to"
        );
    }
}
