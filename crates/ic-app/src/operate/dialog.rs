//! The action dialogs (ACT-02..06): acknowledge, schedule downtime, add a
//! comment, submit a passive check result, run a command. One view for
//! all five, a modal over the window, in the calm v2 look of the other
//! dialogs.
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

use gpui::{
    Action, AnyElement, App, AppContext as _, ClickEvent, Context, Entity, EventEmitter,
    FocusHandle, Focusable, FontWeight, InteractiveElement as _, IntoElement, KeyBinding,
    ParentElement as _, Render, SharedString, Styled as _, Subscription, Window, div, px,
};
use ic_core::snapshot::Snapshot;
use ic_model::{CheckableState, ChildOptions, CommandType, ObjectKey, Timestamp};
use ic_ui_kit::input::{InputEvent, InputState, TextareaState};
use ic_ui_kit::{
    ActiveTheme as _, Button, ButtonVariant, Chip, DialogBody, Field, FieldTone, KvTable,
    Segmented, StateDot, Switch, TextArea, TextField, Theme,
};

use super::ActionSpec;
use super::forms::{
    AckForm, CommandForm, CommentForm, DowntimeForm, Eligible, FormField, Issues, ResultForm,
    describe_objects,
};
use super::when::{self, END_PRESETS, EXPIRY_PRESETS, Preset};
use crate::actions::ObjectAction;
use crate::app_state::AppState;

/// Key context of an action dialog.
pub(crate) const DIALOG_CONTEXT: &str = "ActionDialog";
/// Key context of the run-command confirmation step.
const CONFIRM_CONTEXT: &str = "ActionConfirm";

/// The most objects a dialog lists by name.
const LISTED_OBJECTS: usize = 5;

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

/// Registers the dialogs' keys. Tab and Enter are bound over the text
/// fields (`ActionDialog > Input`), registered after gpui-component's own
/// bindings, so Tab moves between fields instead of indenting and Enter
/// sends instead of typing a new line (Shift-Enter still does).
pub(crate) fn bind_keys(cx: &mut App) {
    let field = Some("ActionDialog > Input");
    let dialog = Some(DIALOG_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("tab", NextField, field),
        KeyBinding::new("shift-tab", PreviousField, field),
        KeyBinding::new("enter", SendDialog, field),
        KeyBinding::new("secondary-enter", SendDialogNow, field),
        KeyBinding::new("tab", NextField, dialog),
        KeyBinding::new("shift-tab", PreviousField, dialog),
        KeyBinding::new("secondary-enter", ConfirmCommand, Some(CONFIRM_CONTEXT)),
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

    /// The action it sends.
    pub(crate) fn action(self) -> ObjectAction {
        match self {
            Self::Acknowledge => ObjectAction::Acknowledge,
            Self::Downtime => ObjectAction::ScheduleDowntime,
            Self::Comment => ObjectAction::AddComment,
            Self::CheckResult => ObjectAction::SubmitCheckResult,
            Self::Command => ObjectAction::RunCommand,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Acknowledge => "Acknowledge",
            Self::Downtime => "Schedule downtime",
            Self::Comment => "Add comment",
            Self::CheckResult => "Submit check result",
            Self::Command => "Run command",
        }
    }

    fn submit_label(self) -> &'static str {
        match self {
            Self::Acknowledge => "acknowledge",
            Self::Downtime => "schedule downtime",
            Self::Comment => "add comment",
            Self::CheckResult => "submit result",
            Self::Command => "run…",
        }
    }

    /// What the dialog says when nothing qualifies.
    fn nothing(self) -> &'static str {
        match self {
            Self::Acknowledge => "Nothing to acknowledge",
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
}

impl Form {
    fn new(kind: DialogKind) -> Self {
        match kind {
            DialogKind::Acknowledge => Self::Ack(AckForm::default()),
            DialogKind::Downtime => Self::Downtime(DowntimeForm::default()),
            DialogKind::Comment => Self::Comment(CommentForm::default()),
            DialogKind::CheckResult => Self::Result(ResultForm::default()),
            DialogKind::Command => Self::Command(CommandForm::default()),
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
    }
}

/// An action dialog.
pub(crate) struct ActionDialog {
    state: Entity<AppState>,
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
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<DialogEvent> for ActionDialog {}

impl Focusable for ActionDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ActionDialog {
    /// A dialog of `kind` for `eligible`'s objects.
    pub(crate) fn new(
        state: Entity<AppState>,
        kind: DialogKind,
        eligible: Eligible,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let form = Form::new(kind);
        let (author, endpoints, endpoint_default) = {
            let current = state.read(cx);
            let snapshot = current.snapshot();
            (
                current
                    .environment()
                    .map(|environment| environment.author_name().to_owned())
                    .unwrap_or_default(),
                snapshot
                    .endpoints
                    .iter()
                    .map(|endpoint| endpoint.name.clone())
                    .collect::<Vec<_>>(),
                endpoint_default(snapshot, &eligible.targets),
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
        Self {
            state,
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
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
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
        self.form != Form::new(self.kind)
    }

    /// Which dialog this is.
    #[cfg(all(test, target_os = "linux"))]
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

    /// Changes the form (tests flip switches with it).
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn edit_form(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut Form)) {
        edit(&mut self.form);
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
    fn move_focus(&self, forward: bool, window: &mut Window, cx: &mut App) {
        let inputs = self.visible_inputs();
        if inputs.is_empty() {
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
        let sent = self.state.update(cx, |state, cx| {
            let sent = state.submit(spec);
            cx.notify();
            sent
        });
        match sent {
            Ok(_) => cx.emit(DialogEvent::Close),
            Err(error) => {
                self.error = Some(error);
                if self.confirming {
                    self.back(window, cx);
                }
                cx.notify();
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

    /// The objects it acts on (when more than one) and those it skips.
    fn render_targets(&self, snapshot: &Snapshot, theme: &Theme) -> Option<AnyElement> {
        let targets = &self.eligible.targets;
        if targets.len() <= 1 && self.eligible.skipped.is_empty() {
            return None;
        }
        let colors = theme.colors;
        let mut column = div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .text_size(theme.text.small);
        if targets.is_empty() {
            column = column.child(
                div()
                    .text_color(theme.states.warning)
                    .child(format!("{}.", self.kind.nothing())),
            );
        }
        for object in targets.iter().take(LISTED_OBJECTS) {
            column = column.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(match object_state(snapshot, object) {
                        Some(state) => StateDot::new(state).size(px(7.)),
                        None => StateDot::with_color(theme.states.pending).size(px(7.)),
                    })
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_color(colors.text)
                            .child(describe_objects(std::slice::from_ref(object))),
                    ),
            );
        }
        if targets.len() > LISTED_OBJECTS {
            column = column.child(
                div()
                    .pl(px(15.))
                    .text_color(colors.text_faint)
                    .child(format!("+ {} more", targets.len() - LISTED_OBJECTS)),
            );
        }
        if !self.eligible.skipped.is_empty() {
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
            column = column.child(div().pt(px(2.)).text_color(colors.text_faint).child(text));
        }
        Some(
            div()
                .px(px(12.))
                .py(px(10.))
                .rounded(theme.metrics.code_radius)
                .bg(colors.code_background)
                .child(column)
                .into_any_element(),
        )
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
            blocks.extend(Self::host_options(form, hosts, cx));
        }
        blocks.push(
            self.text_field("triggered by", FormField::Trigger, issues)
                .hint("Another downtime's full name: this one starts when it does (optional).")
                .into_any_element(),
        );
        blocks
    }

    /// For hosts: their services too, and their child hosts.
    fn host_options(form: &DowntimeForm, hosts: usize, cx: &Context<Self>) -> [AnyElement; 2] {
        let options = [
            ChildOptions::None,
            ChildOptions::Triggered,
            ChildOptions::NonTriggered,
        ];
        let selected = options
            .iter()
            .position(|option| *option == form.child_options)
            .unwrap_or(0);
        [
            Switch::new("downtime-all-services", form.all_services)
                .label(if hosts == 1 {
                    "also the host's services"
                } else {
                    "also the hosts' services"
                })
                .on_change(cx.listener(|this, on: &bool, _, cx| {
                    if let Form::Downtime(form) = &mut this.form {
                        form.all_services = *on;
                    }
                    cx.notify();
                }))
                .into_any_element(),
            Field::new("child hosts")
                .control(
                    Segmented::new("downtime-children")
                        .option("none")
                        .option("triggered")
                        .option("independent")
                        .selected(selected)
                        .on_select(cx.listener(move |this, index: &usize, _, cx| {
                            if let Form::Downtime(form) = &mut this.form {
                                form.child_options =
                                    options.get(*index).copied().unwrap_or_default();
                            }
                            cx.notify();
                        })),
                )
                .hint(
                    "Downtimes for the hosts that depend on these: triggered ones start with \
                     this one, independent ones keep its window.",
                )
                .into_any_element(),
        ]
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
                .text_color(theme.states.warning)
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
        .action(
            Button::new("action-submit", self.kind.submit_label())
                .primary()
                .key_hint("↵")
                .disabled(self.eligible.targets.is_empty())
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.submit(window, cx);
                })),
        )
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

/// An object's state for its dot.
fn object_state(snapshot: &Snapshot, object: &ObjectKey) -> Option<CheckableState> {
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

impl Render for ActionDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let colors = theme.colors;
        let issues = self.issues();
        let snapshot = self.state.read(cx).snapshot().clone();
        let what = describe_objects(&self.eligible.targets);
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
        } else {
            if let Some(targets) = self.render_targets(&snapshot, &theme) {
                body = body.child(targets);
            }
            let blocks = match &self.form {
                Form::Ack(form) => self.render_ack(form, &issues, cx),
                Form::Downtime(form) => self.render_downtime(form, &issues, cx),
                Form::Comment(form) => self.render_comment(form, &issues, cx),
                Form::Result(form) => self.render_result(form, &issues, cx),
                Form::Command(form) => self.render_command(form, &issues, cx),
            };
            for block in blocks {
                body = body.child(block);
            }
        }
        if let Some(error) = &self.error {
            body = body.child(
                div()
                    .text_size(theme.text.small)
                    .text_color(theme.states.critical)
                    .child(error.clone()),
            );
        }
        let body = self.footer(body, confirming.is_some(), cx);
        div()
            .id("action-dialog")
            .key_context(if self.confirming {
                CONFIRM_CONTEXT
            } else {
                DIALOG_CONTEXT
            })
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::on_next_field))
            .on_action(cx.listener(Self::on_previous_field))
            .on_action(cx.listener(Self::on_confirm))
            .on_action(cx.listener(Self::on_send))
            .on_action(cx.listener(Self::on_send_now))
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
