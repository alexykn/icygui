//! The comment field: one implementation for the object pane's thread and
//! the handling view's threads (topic 17). A framed text field on the code
//! surface (accent border while it has the keyboard) that grows with what
//! is typed: Enter sends, Shift+Enter starts a new line, Escape asks its
//! owner to cancel. What happens to the text is the owner's: it gets
//! [`CommentFieldEvent`]s.

use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString, Styled as _,
    Subscription, Window, div,
};
use ic_ui_kit::TextArea;
use ic_ui_kit::input::{Escape, InputEvent, TextareaState};

/// The most lines the field grows to; past that it scrolls.
const MAX_ROWS: usize = 6;

/// What the field asks of its owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CommentFieldEvent {
    /// Enter: send this text (trimmed, never empty).
    Send(String),
    /// Escape.
    Cancel,
}

/// The comment field.
pub(crate) struct CommentField {
    id: SharedString,
    input: Entity<TextareaState>,
    _events: Subscription,
}

impl EventEmitter<CommentFieldEvent> for CommentField {}

impl Focusable for CommentField {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

impl CommentField {
    /// A field (element id `id`) showing `placeholder` while empty.
    pub(crate) fn new(
        id: impl Into<SharedString>,
        placeholder: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let placeholder = placeholder.into();
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder(placeholder)
                .submit_on_enter(true)
                .auto_grow(1, MAX_ROWS)
        });
        let events = cx.subscribe_in(
            &input,
            window,
            |_: &mut Self, input, event: &InputEvent, _, cx| {
                // Shift+Enter has put a new line in; Enter and Ctrl/Cmd+
                // Enter send.
                if let InputEvent::PressEnter { shift: false, .. } = event {
                    let text = input.read(cx).value().trim().to_owned();
                    if !text.is_empty() {
                        cx.emit(CommentFieldEvent::Send(text));
                    }
                }
            },
        );
        Self {
            id: id.into(),
            input,
            _events: events,
        }
    }

    /// What is typed.
    pub(crate) fn value(&self, cx: &App) -> String {
        self.input.read(cx).value().to_string()
    }

    /// Replaces what is typed.
    pub(crate) fn set_value(&self, text: &str, window: &mut Window, cx: &mut App) {
        let text = text.to_owned();
        self.input
            .update(cx, |input, cx| input.set_value(text, window, cx));
    }

    /// Puts the keyboard in the field.
    pub(crate) fn focus(&self, window: &mut Window, cx: &mut App) {
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }
}

impl Render for CommentField {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id(self.id.clone())
            .w_full()
            .min_w_0()
            // Escape that the text area has nothing to dismiss for.
            .on_action(cx.listener(|_, _: &Escape, _, cx| {
                cx.emit(CommentFieldEvent::Cancel);
            }))
            .child(TextArea::new(&self.input).prose())
    }
}
