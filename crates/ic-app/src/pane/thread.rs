//! The object's thread in its pane (topic 14, frame 14-r2-f): a
//! *handling* section with the same entries as the handling view, oldest
//! first like a chat (its acknowledgement, its downtimes, its free-standing
//! comments), and a field to add a comment that goes through the action
//! path every comment takes (permissions, the pending marker, toasts): the
//! comment field the handling view's threads open too (topic 17).
//!
//! The downtime the banner above shows keeps its place in the thread but
//! doesn't repeat its text (`the downtime in the banner above`). A
//! comment's or a downtime's `×` shows on hover in a fixed slot at the end
//! of its line (removing a downtime asks first, listing what goes; a
//! config downtime's says why it can't be removed). The acknowledgement
//! goes with the *remove ack* button.

use gpui::{
    AnyElement, AppContext as _, ClickEvent, Context, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, Styled as _, Window, div, prelude::FluentBuilder as _,
};
use ic_core::snapshot::Snapshot;
use ic_model::{Action, ObjectKey, Timestamp};
use ic_ui_kit::{ActiveTheme as _, Button, IconButton, IconName, SectionLabel, Tooltip, px};

use super::ObjectPane;
use crate::actions::ObjectAction;
use crate::comments::field::{CommentField, CommentFieldEvent};
use crate::lists::draw::{self, Look};
use crate::lists::threads::{self, EntryKey};
use crate::lists::words;
use crate::operate::ActionSpec;

/// The hover group of an entry (reveals its remove button).
const ENTRY_GROUP: &str = "pane-thread-entry";

/// The thread section, when the object has anything in it.
pub(super) fn section(
    pane: &ObjectPane,
    snapshot: &Snapshot,
    object: &ObjectKey,
    now: Timestamp,
    cx: &Context<ObjectPane>,
) -> Option<AnyElement> {
    let entries = threads::thread_of(snapshot, object, now);
    if entries.is_empty() {
        return None;
    }
    let theme = cx.theme();
    let colors = theme.colors;
    let state = pane.state.read(cx);
    // The banner's downtime is said above: its entry keeps its place.
    let banner = crate::downtimes::primary(crate::downtimes::of(snapshot, object), now)
        .map(|downtime| downtime.name.clone());
    let pending = state
        .pending_action(object)
        .map(|(action, label)| (action.clone(), label));
    let mut lines = div().flex().flex_col();
    for (index, entry) in entries.iter().enumerate() {
        let Some(mut text) = words::entry_text(snapshot, entry, now) else {
            continue;
        };
        let in_banner =
            matches!(&entry.key, EntryKey::Downtime(name) if Some(name) == banner.as_ref());
        if in_banner {
            "the downtime in the banner above".clone_into(&mut text.text);
        }
        let marker = pending.as_ref().and_then(|(action, label)| {
            let ours = matches!(
                (&entry.key, action),
                (
                    EntryKey::Downtime(_),
                    ObjectAction::RemoveDowntime(_)
                        | ObjectAction::RemoveDowntimes
                        | ObjectAction::RemoveNamedDowntimes(_),
                ) | (EntryKey::Comment(_), ObjectAction::RemoveComments(_))
                    | (EntryKey::Ack(_), ObjectAction::RemoveAcknowledgement)
            );
            ours.then_some(*label)
        });
        let look = Look {
            pane: true,
            reply: index > 0,
            faint_text: in_banner,
            pending: marker,
            ..Look::default()
        };
        let remove = remove_button(pane, entry, cx);
        lines = lines.child(
            div()
                .id(SharedString::from(format!("pane-entry-{index}")))
                .group(ENTRY_GROUP)
                .flex()
                .items_start()
                .gap(px(6.))
                .pl(px(6.))
                .pr(theme.metrics.pane_inset - px(26.))
                .pt(px(9.))
                .pb(px(10.))
                .border_b_1()
                .border_color(colors.border_row)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(draw::entry(&text, &look, false, theme)),
                )
                .child(
                    div()
                        .flex_none()
                        .w(px(20.))
                        .rounded(theme.metrics.small_radius)
                        .invisible()
                        .group_hover(ENTRY_GROUP, gpui::Styled::visible)
                        .children(remove),
                ),
        );
    }
    let count = entries.len();
    let header = div()
        .flex()
        .items_baseline()
        .gap(px(10.))
        .child(SectionLabel::new("handling"))
        .child(
            div()
                .text_size(theme.text.label)
                .text_color(colors.text_faint)
                .child(format!("{count} · oldest first")),
        );
    Some(
        div()
            .id("pane-thread")
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(header)
            .child(
                // The entries reach the pane's edges, as drawn.
                div().mx(-theme.metrics.pane_inset).child(lines),
            )
            .child(comment_field(pane, cx))
            .into_any_element(),
    )
}

/// The `×` of a comment or a downtime (a config downtime's says why it
/// can't be removed; without the permission, why not).
fn remove_button(
    pane: &ObjectPane,
    entry: &threads::Entry,
    cx: &Context<ObjectPane>,
) -> Option<IconButton> {
    let theme = cx.theme();
    let (id, tooltip, action) = match &entry.key {
        EntryKey::Ack(_) => return None,
        EntryKey::Comment(name) => (
            format!("remove-comment-{name}"),
            "Remove comment",
            ObjectAction::RemoveComments(vec![name.clone()]),
        ),
        EntryKey::Downtime(name) => (
            format!("remove-downtime-{name}"),
            "Remove downtime",
            ObjectAction::RemoveDowntime(name.clone()),
        ),
    };
    let button = IconButton::new(SharedString::from(id), IconName::Close)
        .size(px(20.))
        .icon_size(px(12.))
        .color(theme.colors.text_faint);
    if entry.config {
        return Some(
            button
                .disabled(true)
                .tooltip(Tooltip::new(super::downtime::config_reason(None))),
        );
    }
    Some(match pane.state.read(cx).action_denial(&action) {
        Some(denial) => button.disabled(true).tooltip(Tooltip::new(denial)),
        None => button.tooltip(Tooltip::new(tooltip)).on_click(cx.listener(
            move |pane: &mut ObjectPane, _: &ClickEvent, _, cx| {
                pane.request(action.clone(), cx);
            },
        )),
    })
}

/// `add a comment (c)` and `comment ↵`; without the permission, why not.
/// The field is the comment field the handling view's threads open too
/// (topic 17): Enter sends, Shift+Enter starts a new line.
fn comment_field(pane: &ObjectPane, cx: &Context<ObjectPane>) -> AnyElement {
    let theme = cx.theme();
    let colors = theme.colors;
    if let Some(denial) = pane.state.read(cx).action_denial(&ObjectAction::AddComment) {
        return div()
            .mt(px(8.))
            .text_size(theme.text.small)
            .text_color(colors.text_faint)
            .child(denial)
            .into_any_element();
    }
    let Some(field) = &pane.comment_input else {
        return div().into_any_element();
    };
    div()
        .flex()
        .flex_col()
        .gap(px(4.))
        .mt(px(8.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(div().flex_1().min_w_0().child(field.clone()))
                .child(
                    Button::new("pane-comment-send", "comment")
                        .key_hint("↵")
                        .on_click(cx.listener(
                            |pane: &mut ObjectPane, _: &ClickEvent, window, cx| {
                                pane.send_comment(window, cx);
                            },
                        )),
                ),
        )
        .when_some(pane.comment_error.clone(), |field, error| {
            field.child(
                div()
                    .text_size(theme.text.small)
                    .text_color(theme.states.text.critical)
                    .child(error),
            )
        })
        .into_any_element()
}

impl ObjectPane {
    /// Makes the comment field once there is a window to make it in.
    pub(super) fn ensure_comment_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.comment_input.is_some() {
            return;
        }
        let field = cx.new(|cx| {
            CommentField::new("pane-comment-field", "add a comment (c)", window, cx)
        });
        let events = cx.subscribe_in(
            &field,
            window,
            |pane: &mut Self, _, event: &CommentFieldEvent, window, cx| match event {
                CommentFieldEvent::Send(_) => pane.send_comment(window, cx),
                CommentFieldEvent::Cancel => pane.on_comment_escape(window, cx),
            },
        );
        self.comment_input = Some(field);
        self.comment_events = Some(events);
    }

    /// Puts the keyboard in the comment field when the pane shows its
    /// thread (`c`). Returns whether it did.
    pub(crate) fn focus_comment_field(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let shows = {
            let state = self.state.read(cx);
            let snapshot = state.snapshot();
            state.action_denial(&ObjectAction::AddComment).is_none()
                && !threads::thread_of(snapshot, &self.object, Timestamp::now()).is_empty()
        };
        let Some(field) = self.comment_input.clone().filter(|_| shows) else {
            return false;
        };
        field.update(cx, |field, cx| field.focus(window, cx));
        true
    }

    /// Escape in the field: clears what is typed, else gives the keyboard
    /// back to the list.
    fn on_comment_escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(field) = self.comment_input.clone() else {
            return;
        };
        if field.read(cx).value(cx).is_empty() {
            self.give_back_focus(window, cx);
        } else {
            field.update(cx, |field, cx| field.set_value("", window, cx));
        }
        self.comment_error = None;
        cx.notify();
    }

    /// Adds what is typed as a comment on the shown object, through the
    /// action path (by the environment's author, like the dialog's).
    pub(crate) fn send_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(field) = self.comment_input.clone() else {
            return;
        };
        let text = field.read(cx).value(cx).trim().to_owned();
        if text.is_empty() {
            return;
        }
        let spec = ActionSpec::for_objects(
            ObjectAction::AddComment,
            Action::AddComment { text, expiry: None },
            vec![self.object.clone()],
        );
        let sent = self.state.update(cx, |state, cx| {
            let sent = match state.action_denial(&ObjectAction::AddComment) {
                Some(denial) => Err(denial),
                None => state.submit(spec),
            };
            cx.notify();
            sent
        });
        match sent {
            Ok(_) => {
                self.comment_error = None;
                field.update(cx, |field, cx| field.set_value("", window, cx));
                self.give_back_focus(window, cx);
            }
            Err(error) => self.comment_error = Some(error),
        }
        cx.notify();
    }

    /// The keyboard back to where it was before the field: the list beside
    /// the pane, or the tab.
    fn give_back_focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        match &self.return_focus {
            Some(handle) => window.focus(handle, cx),
            None => window.focus(&self.focus_handle, cx),
        }
    }
}

/// Accessors for the UI tests (`ui_tests`, Linux only).
#[cfg(all(test, target_os = "linux"))]
impl ObjectPane {
    /// The comment field, once the pane has drawn it.
    pub(crate) fn comment_input(&self) -> Option<gpui::Entity<CommentField>> {
        self.comment_input.clone()
    }
}
