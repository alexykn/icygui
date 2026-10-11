//! Topic 17's lines in a handling view's thread, drawn once for a view of
//! its own and a dashboard's stacked view: the open comment field (the
//! thread's next entry: the comment mark in the accent, the author and
//! `now`, the field, its keys), a comment on its way (dimmed, `sending…`
//! in the time slot) or refused (its reason in red, `retry · discard`),
//! and the *+ comment* `c` that the thread's last entry shows in its time
//! slot on hover.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    AnyElement, App, ClickEvent, Context, ElementId, Entity, EntityId, InteractiveElement as _,
    IntoElement, ParentElement as _, Pixels, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Window, canvas, div, prelude::FluentBuilder as _,
};
use ic_model::{ObjectKey, Timestamp};
use ic_ui_kit::{Icon, IconName, KeyHint, Link, Theme, px};

use super::drafts::{Draft, Phase};
use super::field::CommentField;
use crate::lists::draw::{self, Extras, Look};
use crate::lists::words;

/// The hover group of a thread's last entry (its *+ comment*).
pub(crate) const LAST_ENTRY_GROUP: &str = "thread-last-entry";

/// The comment field open in a view's thread.
pub(crate) struct Composer {
    /// The object the comment is for.
    pub(crate) object: ObjectKey,
    /// The field.
    pub(crate) field: Entity<CommentField>,
    /// How tall its line was last drawn (`None`: not yet): the view lays
    /// its lines out with it.
    pub(crate) height: Rc<Cell<Option<Pixels>>>,
    _events: Subscription,
}

impl Composer {
    /// The field `field` for `object`; `events` is the owner's
    /// subscription to it.
    pub(crate) fn new(
        object: ObjectKey,
        field: Entity<CommentField>,
        events: Subscription,
    ) -> Self {
        Self {
            object,
            field,
            height: Rc::default(),
            _events: events,
        }
    }

    /// Makes a field in `cx`'s view and subscribes `on_event` to it.
    pub(crate) fn open<V: 'static>(
        object: ObjectKey,
        id: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<V>,
        on_event: impl Fn(&mut V, &super::field::CommentFieldEvent, &mut Window, &mut Context<V>)
        + 'static,
    ) -> Self {
        use gpui::AppContext as _;
        let id = id.into();
        let field = cx.new(|cx| CommentField::new(id, "add a comment", window, cx));
        let events = cx.subscribe_in(&field, window, move |view, _, event, window, cx| {
            on_event(view, event, window, cx);
        });
        field.update(cx, |field, cx| field.focus(window, cx));
        Self::new(object, field, events)
    }
}

/// The width of an entry's tag (its two slots), which the field's line
/// keeps free so the field ends where the entries' text does.
fn tag_width(theme: &Theme) -> Pixels {
    px(draw::SLOT_A + draw::SLOT_GAP) + draw::chars(theme.text.label, draw::SLOT_B_CHARS)
}

/// The open field's line: as tall as it draws (it grows with what is
/// typed); the height it drew at goes to `composer.height`, and `owner` is
/// told when it changed.
pub(crate) fn composer_line(
    author: &str,
    composer: &Composer,
    owner: EntityId,
    theme: &Theme,
) -> AnyElement {
    let colors = theme.colors;
    let key = |key: &'static str, word: &'static str| {
        div()
            .flex()
            .items_center()
            .gap(px(5.))
            .child(KeyHint::new(key))
            .child(word)
    };
    let header = div()
        .flex()
        .items_baseline()
        .gap(px(9.))
        .h(px(18.))
        .whitespace_nowrap()
        .text_size(theme.text.small)
        .text_color(colors.text_faint)
        .when(!author.is_empty(), |header| {
            header.child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(theme.text.body)
                    .text_color(colors.text_strong)
                    .child(author.to_owned()),
            )
        })
        .child(div().flex_none().child("now"));
    let keys = div()
        .flex()
        .items_center()
        .gap(px(14.))
        .mt(px(6.))
        .h(px(18.))
        .whitespace_nowrap()
        .overflow_hidden()
        .text_size(theme.text.label)
        .text_color(colors.text_faint)
        .child(key("↵", "send"))
        .child(key("shift-↵", "new line"))
        .child(key("esc", "cancel"));
    let height = composer.height.clone();
    div()
        .id("comment-composer")
        .relative()
        .w_full()
        .flex()
        .items_start()
        .gap(px(draw::COLUMN_GAP))
        .px(theme.metrics.list_padding)
        .pt(px(9.))
        .pb(px(10.))
        .border_b_1()
        .border_color(colors.border_row)
        .child(
            div()
                .flex()
                .flex_none()
                .justify_center()
                .items_center()
                .w(px(draw::MARK_COLUMN))
                .h(px(18.))
                .child(
                    Icon::new(IconName::MessageSquare)
                        .size(px(13.))
                        .color(colors.accent_text),
                ),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .child(header)
                .child(div().mt(px(4.)).child(composer.field.clone()))
                .child(keys),
        )
        .child(div().flex_none().w(tag_width(theme)))
        // Its height as drawn, for the view's layout.
        .child(
            canvas(
                move |bounds, _, cx: &mut App| {
                    let drawn = bounds.size.height;
                    if height.get() != Some(drawn) {
                        height.set(Some(drawn));
                        cx.notify(owner);
                    }
                },
                |_, (), _, _| {},
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full(),
        )
        .into_any_element()
}

/// A comment sent from the view: dimmed with `sending…` while on its
/// way, or refused with its reason and `retry · discard` (whose clicks
/// the caller gives).
pub(crate) fn draft_line(
    draft: &Draft,
    now: Timestamp,
    theme: &Theme,
    retry: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    discard: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let colors = theme.colors;
    let compact = theme.density == ic_ui_kit::Density::Compact;
    let text = words::draft_text(draft, now);
    let refusal = draft.refusal().map(str::to_owned);
    let sending = matches!(draft.phase, Phase::Sending { .. } | Phase::Accepted { .. });
    let look = Look {
        reply: true,
        ..Look::default()
    };
    let extras = match refusal {
        Some(reason) => Extras {
            slot_b: Some(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .child(probed(format!("draft-retry-{}", draft.id), |name| {
                        Link::new(ElementId::Name(name.into()), "retry").on_click(
                            move |event, window, cx| {
                                cx.stop_propagation();
                                retry(event, window, cx);
                            },
                        )
                    }))
                    .child(div().text_color(colors.text_faint).child("·"))
                    .child(probed(format!("draft-discard-{}", draft.id), |name| {
                        Link::new(ElementId::Name(name.into()), "discard")
                            .quiet()
                            .on_click(move |event, window, cx| {
                                cx.stop_propagation();
                                discard(event, window, cx);
                            })
                    }))
                    .into_any_element(),
            ),
            note: Some(div().child(not_sent(&reason)).into_any_element()),
            mark_color: Some(theme.states.text.critical),
            ..Extras::default()
        },
        // On its way: dimmed, `sending…` faint in the time slot.
        None => Extras {
            dimmed: true,
            slot_b: sending.then(|| {
                div()
                    .text_color(colors.text_faint)
                    .child("sending…")
                    .into_any_element()
            }),
            ..Extras::default()
        },
    };
    let body = draw::entry_with(&text, &look, compact, theme, extras);
    div()
        .id(ElementId::Name(format!("draft-{}", draft.id).into()))
        .size_full()
        .px(theme.metrics.list_padding)
        .when(compact, |line| line.flex().items_center())
        .when(!compact, |line| line.pt(px(9.)))
        .border_b_1()
        .border_color(colors.border_row)
        .child(body)
        .into_any_element()
}

/// A refused comment's line: `not sent: ` and the reason, unless the
/// reason says so itself (the app's own, as when there is no connection).
fn not_sent(reason: &str) -> String {
    if reason.contains(" sent") {
        reason.to_owned()
    } else {
        format!("not sent: {reason}")
    }
}

/// *+ comment* `c` (element `name`): what the thread's last entry shows
/// in its time slot on hover (a click opens the field).
pub(crate) fn plus_comment(
    name: String,
    theme: &Theme,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    probed(name, |name| plus_comment_element(name, theme, on_click)).into_any_element()
}

fn plus_comment_element(
    name: String,
    theme: &Theme,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let colors = theme.colors;
    div()
        .id(ElementId::Name(name.into()))
        .flex()
        .items_center()
        .gap(px(6.))
        .cursor_pointer()
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(4.))
                .text_color(colors.accent_text)
                .hover(|style| style.text_color(colors.accent_hover))
                .child(
                    Icon::new(IconName::Plus)
                        .size(px(11.))
                        .color(colors.accent_text),
                )
                .child("comment"),
        )
        .child(KeyHint::new("c"))
        .on_click(move |event, window, cx| {
            cx.stop_propagation();
            on_click(event, window, cx);
        })
}

/// The element `make` builds from `name`; in the UI tests, where it was
/// laid out goes to [`probe`] under `name` (hidden or not).
fn probed<E: IntoElement>(name: String, make: impl FnOnce(String) -> E) -> gpui::Div {
    #[cfg(all(test, target_os = "linux"))]
    let key = name.clone();
    div().relative().child(make(name)).map(|element| {
        #[cfg(all(test, target_os = "linux"))]
        let element = element.child(
            canvas(
                move |bounds, _, _| {
                    PROBES.with(|probes| probes.borrow_mut().insert(key, bounds));
                },
                |_, (), _, _| {},
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full(),
        );
        element
    })
}

#[cfg(all(test, target_os = "linux"))]
thread_local! {
    /// Where the probed elements were last laid out, by name.
    static PROBES: std::cell::RefCell<std::collections::HashMap<String, gpui::Bounds<Pixels>>> =
        std::cell::RefCell::default();
}

/// Where element `name` (*+ comment*, *retry*, *discard*) was last laid
/// out, for the UI tests.
#[cfg(all(test, target_os = "linux"))]
pub(crate) fn probe(name: &str) -> Option<gpui::Bounds<Pixels>> {
    PROBES.with(|probes| probes.borrow().get(name).copied())
}
