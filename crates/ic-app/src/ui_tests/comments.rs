//! Topic 17 on the real window: comments written in the handling view.
//! *+ comment* on a thread's last entry while hovered (no row added, so
//! nothing moves), `c` on the cursor's thread, the field below the entry
//! (Enter sends through the action path, Shift+Enter is a new line, Escape
//! cancels, it grows with what is typed), the comment dimmed as `sending…`
//! until the event stream brings it (then the thread rises under *latest
//! activity*), a refusal kept with its reason, *retry* and *discard*,
//! nothing without the add-comment permission, and the same in a handling
//! view stacked on a dashboard.

use std::sync::Arc;

use gpui::{App, Entity, Modifiers, Pixels, Point, px};
use ic_config::{SidebarMark, View, ViewDisplay};
use ic_core::snapshot::Snapshot;
use ic_core::{ActionOutcome, ApiInfo, CoreEvent};
use ic_model::{Action, ActionTarget, Comment, CommentKind, ObjectKey, Timestamp};
use ic_rules::ScopeSetting;

use super::actions::record;
use super::lists::{band_of, line_position, lines, open};
use super::{Harness, production, replication, run};
use crate::app_state::editing::DashboardDraft;
use crate::comments::drafts::Phase;
use crate::comments::lines::probe;
use crate::dashboard::page::{ItemKind, Page, Stop};
use crate::fixture::FixtureOptions;
use crate::lists::threads::Line;
use crate::lists::{ListKind, RecordList};

/// The last entry of `object`'s thread.
fn last_entry(lines: &[Line], object: &ObjectKey) -> usize {
    let band = band_of(lines, object).expect("the thread");
    let mut last = None;
    for (index, line) in lines.iter().enumerate().skip(band + 1) {
        match line {
            Line::Band { .. } | Line::Section { .. } | Line::Axis => break,
            Line::Entry { .. } => last = Some(index),
            _ => {}
        }
    }
    last.expect("an entry")
}

/// Where the open field is.
fn composer_line(lines: &[Line]) -> Option<usize> {
    lines
        .iter()
        .position(|line| matches!(line, Line::Composer { .. }))
}

/// The drafts' lines: their id and whether refused.
fn draft_lines(lines: &[Line]) -> Vec<(usize, u64, bool)> {
    lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| match line {
            Line::Draft { id, refused, .. } => Some((index, *id, *refused)),
            _ => None,
        })
        .collect()
}

/// Whether the keyboard is in `field`.
fn focused<V: gpui::Focusable>(app: &Harness, cx: &mut App, field: &Entity<V>) -> bool {
    app.in_window(cx, |window, cx| {
        gpui::Focusable::focus_handle(field.read(cx), cx).is_focused(window)
    })
}

/// The cursor on `object`'s last entry, with no pane open: a click (which
/// opens the pane), then Escape (which closes it).
fn cursor_on_thread(app: &Harness, cx: &mut App, view: &Entity<RecordList>, object: &ObjectKey) {
    let last = last_entry(&lines(view, cx), object);
    app.click(cx, line_position(view, cx, last), Modifiers::default());
    app.keys(cx, "escape");
    assert_eq!(view.read(cx).pane_object(cx), None);
}

/// The middle of `bounds`.
fn middle(bounds: gpui::Bounds<Pixels>) -> Point<Pixels> {
    bounds.center()
}

/// Opens handling, writes `words` (keystrokes) below replication's thread
/// with `c` and sends it with Enter. Returns the view.
fn write_and_send(app: &Harness, cx: &mut App, words: &str) -> Entity<RecordList> {
    let handling = open(app, cx, ListKind::Handling);
    cursor_on_thread(app, cx, &handling, &replication());
    app.keys(cx, "c");
    app.keys(cx, words);
    app.keys(cx, "enter");
    handling
}

/// The core's answer to action `id`.
fn finish(app: &Harness, cx: &mut App, id: u64, error: Option<&str>) {
    app.state.update(cx, |state, cx| {
        state.apply(CoreEvent::ActionFinished {
            id,
            outcome: ActionOutcome {
                ok: usize::from(error.is_none()),
                failed: Vec::new(),
                error: error.map(str::to_owned),
            },
        });
        cx.notify();
    });
    app.draw(cx);
}

/// The event stream brings a comment on `object` with `text`, written now.
fn comment_arrives(app: &Harness, cx: &mut App, object: &ObjectKey, text: &str) {
    app.state.update(cx, |state, cx| {
        let old = state.snapshot().clone();
        let mut comments = (*old.comments).clone();
        let author = state.author().to_owned();
        comments.entry(object.clone()).or_default().push(Comment {
            name: format!("{}!from-the-view", object.full_name()),
            object: object.clone(),
            author,
            text: text.to_owned(),
            kind: CommentKind::User,
            entry_time: Timestamp::now(),
            expire_time: None,
            persistent: false,
        });
        state.set_snapshot(Arc::new(Snapshot {
            revision: old.revision + 1,
            comments: Arc::new(comments),
            ..(*old).clone()
        }));
        cx.notify();
    });
    app.draw(cx);
}

#[test]
fn plus_comment_shows_on_the_last_entry_and_opens_the_field_there() {
    run(FixtureOptions::default(), |app, cx| {
        let handling = open(app, cx, ListKind::Handling);
        let before = lines(&handling, cx);
        let last = last_entry(&before, &replication());
        let next = last + 1;
        let next_middle = handling.read(cx).line_middle(next).unwrap();

        // Hovered, the last entry shows *+ comment* in its time slot, on
        // its own line: no row is added and nothing moves.
        app.hover(cx, line_position(&handling, cx, last));
        let name = format!("plus-comment:{}", replication().full_name());
        let plus = probe(&name).expect("+ comment, laid out");
        let entry_y = line_position(&handling, cx, last).y;
        let height = handling.read(cx).line_height(last).unwrap();
        assert!(
            (plus.center().y - entry_y).abs() < height / 2.,
            "on the entry's line: {plus:?} vs {entry_y:?}"
        );
        assert!(plus.right() > px(1300.), "at the right end: {plus:?}");
        assert_eq!(lines(&handling, cx), before, "no line added");
        assert_eq!(handling.read(cx).line_middle(next), Some(next_middle));

        // A click on it opens the field below that entry, with the
        // keyboard; no pane.
        app.hover(cx, middle(plus));
        app.click(cx, middle(plus), Modifiers::default());
        let (object, field) = handling.read(cx).composer().expect("the field");
        assert_eq!(object, replication());
        let now = lines(&handling, cx);
        assert_eq!(composer_line(&now), Some(last + 1), "below the last entry");
        assert!(focused(app, cx, &field));
        assert_eq!(handling.read(cx).pane_object(cx), None, "no pane");

        // Escape cancels it: nothing sent, the keyboard back on the list.
        app.keys(cx, "o k escape");
        assert!(handling.read(cx).composer().is_none());
        assert_eq!(lines(&handling, cx), before);
        assert!(focused(app, cx, &handling));
    });
}

#[test]
fn c_opens_the_field_and_enter_sends_through_the_action_path() {
    run(FixtureOptions::default(), |app, cx| {
        let recorder = record(app, cx);
        let handling = open(app, cx, ListKind::Handling);
        cursor_on_thread(app, cx, &handling, &replication());
        let one_line = {
            app.keys(cx, "c");
            let at = composer_line(&lines(&handling, cx)).expect("the field");
            handling.read(cx).line_height(at).unwrap()
        };
        let (_, field) = handling.read(cx).composer().unwrap();
        assert!(focused(app, cx, &field), "the keyboard in the field");

        // The list's letters are the field's; Shift+Enter is a new line,
        // and the field grows with it.
        app.keys(cx, "o n space i t shift-enter j x");
        assert_eq!(field.read(cx).value(cx), "on it\njx");
        let at = composer_line(&lines(&handling, cx)).unwrap();
        app.draw(cx);
        let two_lines = handling.read(cx).line_height(at).unwrap();
        assert!(two_lines > one_line, "{two_lines:?} > {one_line:?}");
        assert!(recorder.actions().is_empty(), "nothing sent yet");

        // Enter sends: one comment through the action path, by the
        // environment's author.
        app.keys(cx, "enter");
        let actions = recorder.actions();
        assert_eq!(actions.len(), 1, "{actions:?}");
        assert_eq!(actions[0].1, ActionTarget::Objects(vec![replication()]));
        assert_eq!(
            actions[0].2,
            Action::AddComment {
                text: "on it\njx".to_owned(),
                expiry: None
            }
        );
        assert!(handling.read(cx).composer().is_none(), "the field closes");
        assert!(focused(app, cx, &handling), "the keyboard back on the list");
        let now = lines(&handling, cx);
        let drafts = draft_lines(&now);
        assert_eq!(drafts.len(), 1, "the comment shows at once");
        let (at, id, refused) = drafts[0];
        assert!(!refused);
        assert_eq!(
            at,
            last_entry(&now, &replication()) + 1,
            "as the thread's next entry"
        );
        let draft = app.state.read(cx).comment_draft(id).cloned().unwrap();
        assert_eq!(draft.text, "on it\njx");
        assert_eq!(
            draft.phase,
            Phase::Sending {
                action: Some(actions[0].0)
            }
        );
    });
}

#[test]
fn a_sent_comment_waits_for_its_event_then_the_thread_rises() {
    run(FixtureOptions::default(), |app, cx| {
        let recorder = record(app, cx);
        let handling = write_and_send(app, cx, "s w a p p e d");
        let id = recorder.actions()[0].0;
        let first_band = |_: &Harness, cx: &App| {
            lines(&handling, cx)
                .iter()
                .find_map(|line| match line {
                    Line::Band { object, .. } => Some(object.clone()),
                    _ => None,
                })
                .unwrap()
        };
        assert_ne!(first_band(app, cx), replication(), "not on top yet");

        // Icinga says yes: still pending until the stream shows it.
        finish(app, cx, id, None);
        assert!(matches!(
            draft_lines(&lines(&handling, cx))[..],
            [(_, _, false)]
        ));
        assert!(matches!(
            app.state.read(cx).comment_drafts()[0].phase,
            Phase::Accepted { .. }
        ));

        // The event comes: the real entry takes its place (never two rows
        // for it) and the thread rises under *latest activity*.
        comment_arrives(app, cx, &replication(), "swapped");
        let now = lines(&handling, cx);
        assert!(draft_lines(&now).is_empty(), "{now:?}");
        assert!(app.state.read(cx).comment_drafts().is_empty());
        assert_eq!(first_band(app, cx), replication(), "the thread rose");
        let shown = now
            .iter()
            .filter_map(Line::entry)
            .filter(|entry| entry.object == replication())
            .count();
        let before = app
            .state
            .read(cx)
            .snapshot()
            .comments
            .get(&replication())
            .map_or(0, Vec::len);
        assert!(shown >= before, "every comment once: {shown} of {before}");
    });
}

#[test]
fn a_refused_comment_keeps_its_text_with_retry_and_discard() {
    run(FixtureOptions::default(), |app, cx| {
        let recorder = record(app, cx);
        let handling = write_and_send(app, cx, "s w a p");
        let id = recorder.actions()[0].0;
        finish(
            app,
            cx,
            id,
            Some("Icinga refused it (403, no permission for comments)"),
        );
        let now = lines(&handling, cx);
        let [(at, draft, true)] = draft_lines(&now)[..] else {
            panic!("a refused comment: {now:?}");
        };
        let state = app.state.read(cx);
        let kept = state.comment_draft(draft).unwrap();
        assert_eq!(kept.text, "swap", "never lost");
        assert_eq!(
            kept.refusal(),
            Some("Icinga refused it (403, no permission for comments)")
        );
        // Its reason takes a line of its own under the text.
        let entry = handling.read(cx).line_height(at - 1).unwrap();
        let refused = handling.read(cx).line_height(at).unwrap();
        assert!(refused > entry, "{refused:?} > {entry:?}");

        // *retry* sends the same text again.
        app.hover(cx, line_position(&handling, cx, at));
        let retry = probe(&format!("draft-retry-{draft}")).expect("retry");
        app.click(cx, middle(retry), Modifiers::default());
        let actions = recorder.actions();
        assert_eq!(actions.len(), 2, "{actions:?}");
        assert_eq!(
            actions[1].2,
            Action::AddComment {
                text: "swap".to_owned(),
                expiry: None
            }
        );
        assert!(matches!(
            draft_lines(&lines(&handling, cx))[..],
            [(_, _, false)]
        ));
        assert_eq!(handling.read(cx).pane_object(cx), None, "no pane");

        // Refused again; *discard* lets it go.
        finish(app, cx, actions[1].0, Some("not connected"));
        let now = lines(&handling, cx);
        let [(at, _, true)] = draft_lines(&now)[..] else {
            panic!("refused again: {now:?}");
        };
        app.hover(cx, line_position(&handling, cx, at));
        let discard = probe(&format!("draft-discard-{draft}")).expect("discard");
        app.click(cx, middle(discard), Modifiers::default());
        assert!(draft_lines(&lines(&handling, cx)).is_empty());
        assert!(app.state.read(cx).comment_drafts().is_empty());
        assert_eq!(recorder.actions().len(), 2, "nothing more sent");
    });
}

#[test]
fn without_the_permission_nothing_offers_a_comment() {
    run(FixtureOptions::default(), |app, cx| {
        let recorder = record(app, cx);
        app.state.update(cx, |state, cx| {
            state.set_permissions(Some(ApiInfo {
                user: "viewer".to_owned(),
                permissions: vec!["objects/query/*".to_owned(), "events/*".to_owned()],
                version: "v2.15.6".to_owned(),
            }));
            cx.notify();
        });
        let handling = open(app, cx, ListKind::Handling);
        let before = lines(&handling, cx);
        let last = last_entry(&before, &replication());
        app.hover(cx, line_position(&handling, cx, last));
        let name = format!("plus-comment:{}", replication().full_name());
        assert_eq!(probe(&name), None, "no + comment");

        // `c` does nothing: no field, no dialog, nothing sent.
        cursor_on_thread(app, cx, &handling, &replication());
        app.keys(cx, "c");
        assert!(handling.read(cx).composer().is_none());
        assert_eq!(app.workspace.read(cx).modal(cx), None);
        assert_eq!(lines(&handling, cx), before);
        assert!(recorder.actions().is_empty());
    });
}

/// `production` as two stacked views: handling, then a problems list (the
/// threads on screen without scrolling).
fn stacked(app: &Harness, cx: &mut App) {
    app.state.update(cx, |state, cx| {
        state
            .update_dashboard(
                &production(),
                DashboardDraft {
                    name: "production".to_owned(),
                    views: vec![
                        View {
                            id: "handling".to_owned(),
                            name: "handling".to_owned(),
                            display: ViewDisplay::Handling,
                            ..View::default()
                        },
                        View {
                            id: "problems".to_owned(),
                            name: "problems".to_owned(),
                            ..View::default()
                        },
                    ],
                    notifications: ScopeSetting::Inherit,
                    group_id: "demo-overview".to_owned(),
                    mark: SidebarMark::Auto,
                },
            )
            .unwrap();
        state.select(production());
        cx.notify();
    });
    app.draw(cx);
}

/// The page's item showing `wanted` in the handling view.
fn thread_item(page: &Page, wanted: impl Fn(&Line) -> bool) -> Option<usize> {
    (0..page.items.len()).find(|&index| {
        let item = page.items[index];
        let ItemKind::Thread { line } = item.kind else {
            return false;
        };
        page.views[item.view]
            .thread
            .as_ref()
            .and_then(|thread| thread.listing.line(line))
            .is_some_and(&wanted)
    })
}

/// The handling view's last entry of `object`'s thread, as an item.
fn stacked_last_entry(page: &Page, object: &ObjectKey) -> usize {
    let mut last = None;
    for index in 0..page.items.len() {
        let item = page.items[index];
        let ItemKind::Thread { line } = item.kind else {
            continue;
        };
        let thread = page.views[item.view].thread.as_ref().unwrap();
        if matches!(thread.listing.line(line), Some(Line::Entry { entry, .. }) if entry.object == *object)
            && thread.listing.is_last_entry(line)
        {
            last = Some(index);
        }
    }
    last.expect("the thread's last entry")
}

#[test]
fn a_stacked_handling_view_writes_comments_the_same_way() {
    run(FixtureOptions::default(), |app, cx| {
        let recorder = record(app, cx);
        stacked(app, cx);
        let dashboard = app.dashboard(cx);
        let page = super::views_page(app, cx);
        let last = stacked_last_entry(&page, &replication());
        let bounds = dashboard.read(cx).item_bounds(last, cx).unwrap();

        // Hovered, its last entry offers *+ comment*.
        app.hover(cx, bounds.center());
        let name = format!("plus-comment:handling:{}", replication().full_name());
        let plus = probe(&name).expect("+ comment");
        assert!((plus.center().y - bounds.center().y).abs() < bounds.size.height / 2.);

        // `c` on the thread holding the cursor (the pane closed) opens the
        // field below it.
        app.click(cx, bounds.center(), Modifiers::default());
        app.keys(cx, "escape");
        assert_eq!(app.pane_object(cx), None);
        assert!(matches!(
            dashboard.read(cx).cursor_stop(cx),
            Some(Stop::Thread { .. })
        ));
        app.keys(cx, "c");
        let (view, object, field) = dashboard.read(cx).stacked_composer().expect("the field");
        assert_eq!((view.as_str(), &object), ("handling", &replication()));
        assert!(focused(app, cx, &field));
        let page = super::views_page(app, cx);
        let composer =
            thread_item(&page, |line| matches!(line, Line::Composer { .. })).expect("on the page");
        assert_eq!(composer, stacked_last_entry(&page, &replication()) + 1);

        // Enter sends it; it shows as the thread's next entry, pending.
        app.keys(cx, "o n space i t enter");
        let actions = recorder.actions();
        assert_eq!(actions.len(), 1, "{actions:?}");
        assert_eq!(
            actions[0].2,
            Action::AddComment {
                text: "on it".to_owned(),
                expiry: None
            }
        );
        assert!(dashboard.read(cx).stacked_composer().is_none());
        let page = super::views_page(app, cx);
        let draft = thread_item(&page, |line| {
            matches!(line, Line::Draft { refused: false, .. })
        })
        .expect("the comment on its way");
        assert_eq!(draft, stacked_last_entry(&page, &replication()) + 1);

        // Escape in an open field closes it, nothing sent.
        app.keys(cx, "c x escape");
        assert!(dashboard.read(cx).stacked_composer().is_none());
        assert_eq!(recorder.actions().len(), 1);
    });
}
