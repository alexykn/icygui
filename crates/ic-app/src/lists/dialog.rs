//! The confirmation before removing from a list (topic 07, frames 7c, 7d
//! and 7g): `Remove downtimes  3 selected · 26 downtimes`, a box listing
//! every target (it scrolls; only the rows in view are built), grouped by
//! the downtime they belong to; what follows; what it skips and why; and a
//! danger button that counts what goes. Nothing is sent before the button
//! or Enter; Escape closes it.

use std::ops::Range;
use std::rc::Rc;

use gpui::{
    Action, AnyElement, App, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable,
    FontWeight, InteractiveElement as _, IntoElement, KeyBinding, ParentElement as _, Render,
    Styled as _, UniformListScrollHandle, Window, div, uniform_list,
};
use ic_ui_kit::{
    ActiveTheme as _, Button, ButtonVariant, DialogBody, Scrollbar, ScrollbarMode, StateDot, Theme,
    px,
};

use super::ListKind;
use super::removal::{BulkRemoval, TargetRow};
use crate::app_state::AppState;
use crate::operate::dialog::{DialogEvent, object_mark};
use crate::operate::forms::describe_objects;

/// Key context of the confirmation.
const REMOVAL_CONTEXT: &str = "ListRemoval";

/// A row of the box.
const ROW: f32 = 20.;
/// The box's padding, top and bottom.
const PADDING: f32 = 10.;
/// The box's height at most (it scrolls beyond), as drawn: the downtimes'
/// taller box (11.5 rows), the others' (9.5 rows). Half a row shows at the
/// bottom of a full box, so the cut-off row says there is more.
const DOWNTIME_BOX: f32 = 250.;
const OTHER_BOX: f32 = 210.;

/// Enter: removes what the dialog lists.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct ConfirmRemoval;

/// Tab and Shift-Tab: the confirmation has no fields, and keeps the
/// keyboard (nothing behind it takes the keys that follow).
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct KeepFocus;

/// Registers the confirmation's keys.
pub(crate) fn bind_keys(cx: &mut App) {
    let context = Some(REMOVAL_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("enter", ConfirmRemoval, context),
        KeyBinding::new("tab", KeepFocus, context),
        KeyBinding::new("shift-tab", KeepFocus, context),
    ]);
}

/// The confirmation's view.
pub(crate) struct RemovalDialog {
    state: Entity<AppState>,
    removal: BulkRemoval,
    /// The environment the targets are in: nothing goes to another one.
    environment_id: Option<String>,
    author: String,
    rows: Rc<Vec<TargetRow>>,
    scroll: UniformListScrollHandle,
    error: Option<String>,
    focus_handle: FocusHandle,
}

impl EventEmitter<DialogEvent> for RemovalDialog {}

impl Focusable for RemovalDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl RemovalDialog {
    pub(crate) fn new(
        state: Entity<AppState>,
        removal: BulkRemoval,
        cx: &mut Context<Self>,
    ) -> Self {
        let (environment_id, author) = {
            let current = state.read(cx);
            (
                current.active_environment_id().map(str::to_owned),
                current.author().to_owned(),
            )
        };
        Self {
            state,
            rows: Rc::new(removal.rows.clone()),
            removal,
            environment_id,
            author,
            scroll: UniformListScrollHandle::new(),
            error: None,
            focus_handle: cx.focus_handle(),
        }
    }

    /// What it would remove (tests).
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn removal(&self) -> &BulkRemoval {
        &self.removal
    }

    /// Which list it removes from.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn kind(&self) -> ListKind {
        self.removal.kind
    }

    /// The modal's width, as drawn.
    pub(crate) fn width(&self) -> f32 {
        match self.removal.kind {
            ListKind::Acknowledged => 600.,
            ListKind::Downtimes | ListKind::Comments => 560.,
        }
    }

    fn on_confirm(&mut self, _: &ConfirmRemoval, _: &mut Window, cx: &mut Context<Self>) {
        self.send(cx);
    }

    /// Sends what it lists to the environment it lists it in, and closes;
    /// a refusal keeps it open with the reason.
    pub(crate) fn send(&mut self, cx: &mut Context<Self>) {
        if self.removal.specs.is_empty() {
            self.error = Some("Nothing to remove.".to_owned());
            cx.notify();
            return;
        }
        let specs = self.removal.specs.clone();
        let environment = self.environment_id.clone();
        let sent = self.state.update(cx, |state, cx| {
            if environment.as_deref() != state.active_environment_id() {
                return Err(crate::operate::dialog::ENVIRONMENT_CHANGED.to_owned());
            }
            let mut sent = Ok(0);
            for spec in specs {
                sent = state.submit(spec);
                if sent.is_err() {
                    break;
                }
            }
            cx.notify();
            sent
        });
        match sent {
            Ok(_) => cx.emit(DialogEvent::Close),
            Err(error) => {
                self.error = Some(error);
                cx.notify();
            }
        }
    }

    fn render_rows(&self, range: Range<usize>, cx: &Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme();
        let state = self.state.read(cx);
        let snapshot = state.snapshot();
        let rows = Rc::clone(&self.rows);
        rows[range.start.min(rows.len())..range.end.min(rows.len())]
            .iter()
            .map(|row| match row {
                TargetRow::Heading(text) => div()
                    .flex()
                    .items_center()
                    .h(px(ROW))
                    .whitespace_nowrap()
                    .truncate()
                    .text_size(theme.text.label)
                    .text_color(theme.colors.text_muted)
                    .child(text.clone())
                    .into_any_element(),
                TargetRow::Target { object, meta } => div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .h(px(ROW))
                    .whitespace_nowrap()
                    .text_size(theme.text.small)
                    .child(match object_mark(snapshot, object) {
                        Some(mark) => StateDot::mark(mark).size(px(7.)),
                        None => StateDot::with_color(theme.states.fill.pending).size(px(7.)),
                    })
                    .child(
                        div()
                            .flex_none()
                            .text_color(theme.colors.text)
                            .child(describe_objects(std::slice::from_ref(object))),
                    )
                    .when_not_empty(meta, |line, meta| {
                        line.child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_color(theme.colors.text_faint)
                                .child(meta.to_owned()),
                        )
                    })
                    .into_any_element(),
            })
            .collect()
    }

    /// The box: every target, and for comments what it skips under them.
    fn render_box(&self, theme: &Theme, cx: &Context<Self>) -> AnyElement {
        let colors = theme.colors;
        let most = match self.removal.kind {
            ListKind::Downtimes => DOWNTIME_BOX,
            ListKind::Comments | ListKind::Acknowledged => OTHER_BOX,
        } - 2. * PADDING;
        #[expect(
            clippy::cast_precision_loss,
            reason = "a row count, far below f32's exact range"
        )]
        let rows = self.rows.len() as f32;
        let height = (rows * ROW).min(most).max(ROW);
        let mut column = div().flex().flex_col().gap(px(6.));
        if self.rows.is_empty() {
            column = column.child(
                div()
                    .text_size(theme.text.small)
                    .text_color(theme.states.text.warning)
                    .child("Nothing to remove."),
            );
        } else {
            let scroll = self.scroll.clone();
            column = column.child(
                div()
                    .relative()
                    .h(px(height))
                    .child(
                        uniform_list(
                            "removal-targets",
                            self.rows.len(),
                            cx.processor(|this, range: Range<usize>, _window, cx| {
                                this.render_rows(range, cx)
                            }),
                        )
                        .track_scroll(&scroll)
                        .size_full(),
                    )
                    // Always shown while rows are out of view, as drawn.
                    .child(Scrollbar::vertical(&scroll).mode(ScrollbarMode::Always)),
            );
        }
        // The comments' skipped line sits in the box, under the list.
        if self.removal.kind == ListKind::Comments {
            for line in &self.removal.skipped {
                column = column.child(
                    div()
                        .pt(px(2.))
                        .text_size(theme.text.small)
                        .line_height(gpui::relative(1.45))
                        .text_color(colors.text_faint)
                        .child(line.clone()),
                );
            }
        }
        div()
            .px(px(12.))
            .py(px(PADDING))
            .rounded(theme.metrics.code_radius)
            .bg(colors.code_background)
            .child(column)
            .into_any_element()
    }
}

/// `when` for a text that may be empty.
trait WhenNotEmpty: Sized {
    fn when_not_empty(self, text: &str, then: impl FnOnce(Self, &str) -> Self) -> Self;
}

impl WhenNotEmpty for gpui::Div {
    fn when_not_empty(self, text: &str, then: impl FnOnce(Self, &str) -> Self) -> Self {
        if text.is_empty() {
            self
        } else {
            then(self, text)
        }
    }
}

impl Render for RemovalDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let colors = theme.colors;
        let removal = &self.removal;
        let title = div()
            .flex()
            .items_center()
            .gap(px(10.))
            .min_w_0()
            .child(div().flex_none().child(removal.title()))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(theme.text.body)
                    .font_weight(FontWeight::NORMAL)
                    .text_color(colors.text_muted)
                    .child(removal.what()),
            );
        let mut list = div().flex().flex_col().gap(px(6.));
        if let Some(label) = removal.label() {
            list = list.child(
                div()
                    .text_size(theme.text.small)
                    .text_color(colors.text_secondary)
                    .child(label),
            );
        }
        list = list.child(self.render_box(&theme, cx));
        let mut body = DialogBody::new(title).child(list);
        let mut words = div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .text_size(theme.text.label)
            .line_height(gpui::relative(1.45));
        let mut any_words = false;
        if let Some(consequence) = &removal.consequence {
            any_words = true;
            words = words.child(
                div()
                    .text_color(colors.text_muted)
                    .child(consequence.clone()),
            );
        }
        if let Some(note) = &removal.note {
            any_words = true;
            words = words.child(div().text_color(colors.text_faint).child(note.clone()));
        }
        if removal.kind != ListKind::Comments {
            for line in &removal.skipped {
                any_words = true;
                words = words.child(div().text_color(colors.text_faint).child(line.clone()));
            }
        }
        if any_words {
            body = body.child(words);
        }
        if let Some(error) = &self.error {
            body = body.child(
                div()
                    .text_size(theme.text.small)
                    .text_color(theme.states.text.critical)
                    .child(error.clone()),
            );
        }
        if !self.author.is_empty() {
            body = body.footer_start(
                div()
                    .text_color(colors.text_faint)
                    .child(format!("as {}", self.author)),
            );
        }
        let body = body
            .action(
                Button::new("removal-cancel", "cancel")
                    .key_hint("esc")
                    .on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.emit(DialogEvent::Close))),
            )
            .action(
                Button::new("removal-submit", removal.button())
                    .variant(ButtonVariant::Danger)
                    .key_hint("↵")
                    .disabled(removal.specs.is_empty())
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.send(cx))),
            );
        div()
            .id("removal-dialog")
            .key_context(REMOVAL_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::on_confirm))
            .on_action(cx.listener(|this, _: &KeepFocus, window, cx| {
                this.focus_handle.focus(window, cx);
            }))
            .flex()
            .flex_col()
            .min_h_0()
            .child(body)
    }
}
