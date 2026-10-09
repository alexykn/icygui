//! Comments written in a stacked handling view (topic 17, frame 17e): the
//! same *+ comment* on a thread's last entry, the same field below it
//! (`c` on the thread holding the cursor), the same dimmed `sending…` and
//! refused lines as a handling view of its own ([`crate::lists::view`]).
//! One field at a time on the page; Enter sends through the action path
//! every comment takes, Escape closes it.

use gpui::{App, Context, Window, point};
use ic_model::ObjectKey;
use ic_rules::DashboardRef;
use ic_ui_kit::px;

use super::page::{Id, ItemKind, Page, StackedComments, Stop, ThreadPage};
use super::{DashboardView, PageUi, viewport_height};
use crate::actions::ObjectAction;
use crate::comments::field::CommentFieldEvent;
use crate::comments::lines::Composer;
use crate::lists::model::ListKind;
use crate::lists::threads::{ItemKey, Line};

/// The comment field open in a stacked handling view's thread.
pub(super) struct StackedComposer {
    /// The dashboard it is on.
    pub(super) dashboard: DashboardRef,
    /// The handling view, by id.
    pub(super) view: Id,
    /// The field, and the object it writes on.
    pub(super) composer: Composer,
}

/// The object of the thread `key` belongs to in `thread`: the band above
/// it (a fold's or a paging row's too).
fn thread_object(thread: &ThreadPage, key: &ItemKey) -> Option<ObjectKey> {
    let lines = &thread.listing.lines;
    let index = lines
        .iter()
        .position(|keyed| keyed.key.as_ref() == Some(key))?;
    lines[..=index]
        .iter()
        .rev()
        .find_map(|keyed| match &keyed.line {
            Line::Band { object, .. } => Some(Some(object.clone())),
            Line::Section { .. } | Line::Axis => Some(None),
            _ => None,
        })
        .flatten()
}

impl DashboardView {
    /// Whether the page's handling views offer *+ comment* (and `c`): not
    /// in the editor's preview or the events page, and only with the
    /// add-comment permission.
    pub(super) fn may_comment(&self, cx: &App) -> bool {
        if self.is_preview() || self.events.is_some() {
            return false;
        }
        let state = self.state.read(cx);
        state.environment().is_some() && state.action_denial(&ObjectAction::AddComment).is_none()
    }

    /// What the page's handling views add to their threads: the comments
    /// sent that the snapshot doesn't show yet, and the open field (on
    /// `reference`'s page).
    pub(super) fn stacked_comments(
        &self,
        reference: &DashboardRef,
        has_handling: bool,
        cx: &App,
    ) -> StackedComments {
        if !has_handling || self.is_preview() || self.events.is_some() {
            return StackedComments::default();
        }
        let composer = self
            .composer
            .as_ref()
            .filter(|open| open.dashboard == *reference);
        StackedComments {
            drafts: self
                .state
                .read(cx)
                .comment_drafts()
                .into_iter()
                .map(|draft| (draft.object.clone(), draft.id, draft.refusal().is_some()))
                .collect(),
            composer: composer.map(|open| (open.view.clone(), open.composer.object.clone())),
            composer_height: composer.and_then(|open| open.composer.height.get()),
        }
    }

    /// The thread holding the cursor in a stacked handling view, with
    /// nothing marked: its view and object.
    fn cursor_thread(&mut self, cx: &mut Context<Self>) -> Option<(DashboardRef, Id, ObjectKey)> {
        let reference = self.sync(cx)?;
        let ui = self.pages.get(&reference)?;
        if ui.selection.marked_count() > 0 {
            return None;
        }
        let (_, Stop::Thread { view, key }) = ui.cursor_entry()? else {
            return None;
        };
        let thread = ui.page.view_by_id(view)?.thread.as_ref()?;
        if thread.kind != ListKind::Handling {
            return None;
        }
        let object = thread_object(thread, key)?;
        Some((reference.clone(), view.clone(), object))
    }

    /// `c` in a stacked handling view: the comment field below the
    /// cursor's thread (nothing without the add-comment permission).
    /// Returns whether the cursor was in such a thread.
    pub(super) fn comment_at_cursor(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some((reference, view, object)) = self.cursor_thread(cx) else {
            return false;
        };
        self.open_stacked_composer(&reference, &view, object, window, cx);
        true
    }

    /// Opens the comment field below `object`'s thread in handling view
    /// `view` of `reference` (unfolding it) and puts the keyboard in it.
    /// One field on the page: another thread's closes.
    pub(super) fn open_stacked_composer(
        &mut self,
        reference: &DashboardRef,
        view: &Id,
        object: ObjectKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.may_comment(cx) {
            return;
        }
        self.menus.close();
        let folded = self
            .pages
            .get(reference)
            .is_some_and(|ui| ui.folds.threads(view).collapsed.contains(&object));
        if folded {
            let unfold = object.clone();
            self.fold_thread(reference, view, None, cx, move |folds| {
                folds.collapsed.remove(&unfold);
            });
        }
        let same = self.composer.as_ref().is_some_and(|open| {
            open.dashboard == *reference && open.view == *view && open.composer.object == object
        });
        if same {
            if let Some(open) = &self.composer {
                open.composer
                    .field
                    .update(cx, |field, cx| field.focus(window, cx));
            }
        } else {
            let id = format!("stacked-comment-field:{view}:{}", object.full_name());
            let composer = Composer::open(
                object,
                id,
                window,
                cx,
                |this: &mut Self, event, window, cx| match event {
                    CommentFieldEvent::Send(text) => {
                        let text = text.clone();
                        this.send_stacked_composer(&text, window, cx);
                    }
                    CommentFieldEvent::Cancel => this.close_stacked_composer(window, cx),
                },
            );
            self.composer = Some(StackedComposer {
                dashboard: reference.clone(),
                view: view.clone(),
                composer,
            });
        }
        // The field in view.
        self.sync(cx);
        if let Some(ui) = self.pages.get(reference) {
            let page = ui.page.clone();
            if let Some(item) = composer_item(&page) {
                reveal_item(ui, &page, item);
            }
        }
        cx.notify();
    }

    /// Escape in the field: closes it, its text with it; the keyboard goes
    /// back to the page.
    pub(super) fn close_stacked_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.composer.take().is_some() {
            window.focus(&self.focus_handle, cx);
            cx.notify();
        }
    }

    /// Enter in the field: sends `text` as a comment on its object through
    /// the action path; it shows dimmed as `sending…` until the event
    /// stream brings it (or with the reason it was refused).
    fn send_stacked_composer(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(open) = self.composer.take() else {
            return;
        };
        self.state.update(cx, |state, cx| {
            state.send_comment(&open.composer.object, text);
            cx.notify();
        });
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }
}

/// The page's item holding the open comment field.
fn composer_item(page: &Page) -> Option<usize> {
    page.items.iter().position(|item| {
        let ItemKind::Thread { line } = item.kind else {
            return false;
        };
        page.views
            .get(item.view)
            .and_then(|view| view.thread.as_ref())
            .and_then(|thread| thread.listing.line(line))
            .is_some_and(|line| matches!(line, Line::Composer { .. }))
    })
}

/// Scrolls `ui`'s page just enough for item `item` to show.
fn reveal_item(ui: &PageUi, page: &Page, item: usize) {
    let viewport = viewport_height(ui);
    if viewport <= px(0.) {
        return;
    }
    let (top, bottom) = (page.top(item), page.top(item + 1));
    let offset = -ui.scroll.offset().y;
    let target = if top < offset {
        top
    } else if bottom > offset + viewport {
        bottom - viewport
    } else {
        return;
    };
    let max = (page.height() - viewport).max(px(0.));
    ui.scroll
        .set_offset(point(px(0.), -target.clamp(px(0.), max)));
}
