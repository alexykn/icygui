//! The page's keyboard cursor and marks (topic 04: one cursor for the
//! whole page).
//!
//! The cursor is on a [`Stop`] (a row, a band, a paging row, a grid's
//! host, a stream's event, a view's header) and follows it by identity
//! when the page is built again: a new snapshot, a fold. When its stop
//! leaves the page, the cursor detaches instead of jumping to a neighbour
//! the user never picked (no highlighted row, no target for action keys),
//! and `j` / `k` carry on from where it was.
//!
//! Marks are objects, not rows: an object marked shows marked in every
//! view that lists it, and stays marked while its host is collapsed or
//! paged away (so *mark all* covers every host); the marks of objects that
//! leave every view are dropped, so a bulk action never hits something
//! the user can't find. Only object rows are marked. Pure, so it's tested
//! without a window.

use std::collections::HashSet;

use ic_model::ObjectKey;

use super::page::{Page, Stop};

/// A stop and where it was on the page.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Placed {
    stop: Stop,
    position: usize,
}

impl Placed {
    fn at(page: &Page, position: usize) -> Option<Self> {
        Some(Self {
            stop: page.stops().get(position)?.stop.clone(),
            position,
        })
    }

    /// The same stop on `page`, if it's still there; for a row whose
    /// group changed (the view was grouped another way), the same object's
    /// row in the same view.
    fn find(&self, page: &Page) -> Option<Self> {
        let position = match page.stops().get(self.position) {
            Some(entry) if entry.stop == self.stop => self.position,
            _ => {
                if let Some(position) = page.position(&self.stop) {
                    position
                } else {
                    let Stop::Row { view, key, .. } = &self.stop else {
                        return None;
                    };
                    page.stops().iter().position(|entry| {
                        matches!(&entry.stop, Stop::Row { view: other, key: row, .. } if other == view && row == key)
                    })?
                }
            }
        };
        Some(Self {
            stop: page.stops()[position].stop.clone(),
            position,
        })
    }
}

/// The cursor, the anchor of shift selection and the marks of one page.
#[derive(Debug, Default)]
pub(crate) struct PageSelection {
    cursor: Option<Placed>,
    /// Where a detached cursor was: moving carries on from there.
    detached: Option<usize>,
    /// Where shift selection extends from.
    anchor: Option<Placed>,
    /// Marks from before the current shift extension.
    base: Option<HashSet<ObjectKey>>,
    marked: HashSet<ObjectKey>,
}

impl PageSelection {
    /// Follows the cursor and anchor to `page` (built again). Marks of
    /// objects for which `listed` is false are dropped.
    pub(crate) fn update(&mut self, page: &Page, listed: impl Fn(&ObjectKey) -> bool) {
        if let Some(cursor) = self.cursor.take() {
            self.cursor = cursor.find(page);
            if self.cursor.is_none() {
                self.detached = Some(cursor.position);
            }
        }
        self.anchor = self.anchor.take().and_then(|anchor| anchor.find(page));
        self.marked.retain(|key| listed(key));
        if let Some(base) = self.base.as_mut() {
            base.retain(|key| listed(key));
        }
    }

    /// The cursor's stop position.
    pub(crate) fn cursor(&self) -> Option<usize> {
        self.cursor.as_ref().map(|cursor| cursor.position)
    }

    /// The cursor's stop.
    pub(crate) fn cursor_stop(&self) -> Option<&Stop> {
        self.cursor.as_ref().map(|cursor| &cursor.stop)
    }

    /// Puts the cursor on stop `position`, keeping the marks.
    pub(crate) fn place(&mut self, page: &Page, position: usize) -> bool {
        let Some(placed) = Placed::at(page, position) else {
            return false;
        };
        self.anchor = Some(placed.clone());
        self.cursor = Some(placed);
        self.detached = None;
        self.base = None;
        true
    }

    /// Puts the cursor on stop `position` and clears the marks (a plain
    /// click).
    pub(crate) fn select(&mut self, page: &Page, position: usize) -> bool {
        if !self.place(page, position) {
            return false;
        }
        self.marked.clear();
        true
    }

    /// Moves the cursor `steps` stops down (negative: up), as `j` / `k`
    /// do: view headers are skipped, a grid moves by its lines. Without a
    /// cursor, down starts at the first row and up at the last. Returns
    /// the new position.
    pub(crate) fn move_by(&mut self, page: &Page, steps: isize) -> Option<usize> {
        let target = self.step(page, steps)?;
        self.place(page, target);
        Some(target)
    }

    /// Moves to the first (or last) stop of the page's bodies.
    pub(crate) fn move_to_end(&mut self, page: &Page, last: bool) -> Option<usize> {
        let target = if last {
            page.body_stop_near(page.stops().len().checked_sub(1)?, false)?
        } else {
            page.body_stop_near(0, true)?
        };
        self.place(page, target);
        Some(target)
    }

    /// Moves like [`Self::move_by`] and marks every object row between the
    /// anchor and the cursor (shift-j / shift-down).
    pub(crate) fn extend_by(&mut self, page: &Page, steps: isize) -> Option<usize> {
        let target = self.step(page, steps)?;
        self.extend_to(page, target).then_some(target)
    }

    /// Marks every object row between the anchor and stop `position`,
    /// keeping the marks from before the extension started, and moves the
    /// cursor there (shift-click).
    pub(crate) fn extend_to(&mut self, page: &Page, position: usize) -> bool {
        let Some(target) = Placed::at(page, position) else {
            return false;
        };
        let anchor = self
            .anchor
            .clone()
            .or_else(|| self.cursor.clone())
            .unwrap_or_else(|| target.clone());
        let base = self.base.get_or_insert_with(|| self.marked.clone()).clone();
        self.marked = base;
        let (start, end) = if anchor.position <= target.position {
            (anchor.position, target.position)
        } else {
            (target.position, anchor.position)
        };
        for entry in &page.stops()[start..=end.min(page.stops().len().saturating_sub(1))] {
            if let Stop::Row { key, .. } = &entry.stop {
                self.marked.insert(key.clone());
            }
        }
        self.anchor = Some(anchor);
        self.cursor = Some(target);
        self.detached = None;
        true
    }

    /// Flips the mark of the row at stop `position` and puts the cursor
    /// there (ctrl/cmd-click). Other stops only take the cursor.
    pub(crate) fn toggle_mark(&mut self, page: &Page, position: usize) -> bool {
        let Some(placed) = Placed::at(page, position) else {
            return false;
        };
        if let Stop::Row { key, .. } = &placed.stop
            && !self.marked.remove(key)
        {
            self.marked.insert(key.clone());
        }
        self.place(page, position);
        true
    }

    /// Flips the mark of the cursor's row (`x`); without a cursor, of the
    /// row `j` would go to.
    pub(crate) fn toggle_mark_at_cursor(&mut self, page: &Page) -> Option<usize> {
        let position = self.cursor().or_else(|| self.step(page, 1))?;
        self.toggle_mark(page, position).then_some(position)
    }

    /// Marks exactly `keys` (*mark all* of a view: its rows folded away or
    /// not).
    pub(crate) fn mark_all(&mut self, keys: impl IntoIterator<Item = ObjectKey>) {
        self.base = None;
        self.marked = keys.into_iter().collect();
    }

    /// Clears the marks. Returns whether there were any.
    pub(crate) fn clear_marks(&mut self) -> bool {
        self.base = None;
        let had = !self.marked.is_empty();
        self.marked.clear();
        had
    }

    /// Whether `key` is marked.
    pub(crate) fn is_marked(&self, key: &ObjectKey) -> bool {
        self.marked.contains(key)
    }

    /// Whether any of `keys` is marked.
    pub(crate) fn any_marked<'a>(&self, mut keys: impl Iterator<Item = &'a ObjectKey>) -> bool {
        !self.marked.is_empty() && keys.any(|key| self.marked.contains(key))
    }

    /// How many objects are marked.
    pub(crate) fn marked_count(&self) -> usize {
        self.marked.len()
    }

    /// The marked objects in the order of `objects` (the page's objects,
    /// view by view).
    pub(crate) fn marked_in(&self, objects: impl IntoIterator<Item = ObjectKey>) -> Vec<ObjectKey> {
        if self.marked.is_empty() {
            return Vec::new();
        }
        let mut seen = HashSet::with_capacity(self.marked.len());
        objects
            .into_iter()
            .filter(|key| self.marked.contains(key) && seen.insert(key.clone()))
            .collect()
    }

    /// The stop `steps` stops from the cursor, as `j` / `k` move. Without
    /// a cursor, the first step down lands on the first body stop and the
    /// first step up on the last; from a detached cursor, on the stop that
    /// took its place and on the one above it.
    fn step(&self, page: &Page, steps: isize) -> Option<usize> {
        let forward = steps >= 0;
        let mut position = if let Some(cursor) = self.cursor() {
            cursor
        } else {
            let last = page.stops().len().checked_sub(1)?;
            let first = match self.detached {
                Some(detached) if forward => page.body_stop_near(detached.min(last), true),
                Some(detached) => {
                    page.body_stop_near(detached.min(last + 1).saturating_sub(1), false)
                }
                None if forward => page.body_stop_near(0, true),
                None => page.body_stop_near(last, false),
            }?;
            if steps.unsigned_abs() <= 1 {
                return Some(first);
            }
            first
        };
        let moves = if self.cursor().is_some() {
            steps.unsigned_abs()
        } else {
            steps.unsigned_abs() - 1
        };
        for _ in 0..moves {
            match page.step(position, forward) {
                Some(next) => position = next,
                None => break,
            }
        }
        Some(position)
    }
}

#[cfg(test)]
#[expect(
    clippy::disallowed_methods,
    reason = "test sizes are real pixels at 100 %"
)]
mod tests {
    use std::sync::Arc;

    use gpui::px;
    use ic_config::View;
    use ic_core::snapshot::{DashboardResult, DashboardRow, Snapshot, ViewBody, ViewResult};
    use ic_ui_kit::Theme;

    use super::super::page::{Folds, PageInput, Sizes};
    use super::*;

    fn key(name: &str) -> ObjectKey {
        ObjectKey::service("h", name)
    }

    /// A page of two list views, `a` and `b`, with these rows.
    fn page(a: &[&str], b: &[&str]) -> Page {
        let rows = |names: &[&str]| {
            ViewBody::List(Arc::new(
                names
                    .iter()
                    .map(|name| DashboardRow::Object(key(name)))
                    .collect(),
            ))
        };
        let views = [
            View {
                id: "a".to_owned(),
                ..View::default()
            },
            View {
                id: "b".to_owned(),
                ..View::default()
            },
        ];
        let result = DashboardResult {
            views: vec![
                ViewResult {
                    id: "a".to_owned(),
                    body: rows(a),
                    ..ViewResult::default()
                },
                ViewResult {
                    id: "b".to_owned(),
                    body: rows(b),
                    ..ViewResult::default()
                },
            ],
            ..DashboardResult::default()
        };
        Page::build(&PageInput {
            views: &views,
            result: Some(&result),
            snapshot: &Snapshot::default(),
            folds: &Folds::default(),
            filter: None,
            multi: true,
            denied: [false, false],
            width: px(1000.),
            sizes: Sizes::of(&Theme::dark()),
            by_density: crate::dashboard::page::Densities::of(&Theme::dark()),
            density: ic_config::RowDensity::Comfortable,
            author: "",
            now: ic_model::Timestamp::from_unix_seconds(0.),
            comments: &crate::dashboard::page::StackedComments::default(),
        })
    }

    fn at(selection: &PageSelection, page: &Page) -> Option<ObjectKey> {
        selection
            .cursor()
            .and_then(|position| page.stops()[position].stop.object(page))
    }

    #[test]
    fn j_and_k_go_through_every_view_skipping_headers() {
        let page = page(&["a1", "a2"], &["b1"]);
        let mut selection = PageSelection::default();
        selection.move_by(&page, 1);
        assert_eq!(
            at(&selection, &page),
            Some(key("a1")),
            "down starts at the first row"
        );
        selection.move_by(&page, 2);
        assert_eq!(at(&selection, &page), Some(key("b1")), "into the next view");
        selection.move_by(&page, 1);
        assert_eq!(at(&selection, &page), Some(key("b1")), "stops at the end");
        selection.move_by(&page, -1);
        assert_eq!(at(&selection, &page), Some(key("a2")));
        let mut up = PageSelection::default();
        up.move_by(&page, -1);
        assert_eq!(at(&up, &page), Some(key("b1")), "up starts at the end");
        assert_eq!(
            up.move_to_end(&page, false).map(|p| page.stops()[p].item),
            Some(1)
        );
    }

    #[test]
    fn shift_marks_rows_across_views_and_marks_follow_objects() {
        let page = page(&["a1", "a2"], &["b1", "a1"]);
        let mut selection = PageSelection::default();
        selection.move_by(&page, 1);
        selection.extend_by(&page, 2);
        assert_eq!(
            selection.marked_count(),
            3,
            "a1, a2, b1 (the header isn't a row)"
        );
        // a1 is in both views: one mark.
        assert!(selection.is_marked(&key("a1")));
        let order = vec![key("b1"), key("a2"), key("a1")];
        assert_eq!(selection.marked_in(order.clone()), order);
        // An object that leaves every view loses its mark.
        selection.update(&page, |listed| *listed != key("a2"));
        assert_eq!(selection.marked_count(), 2);
        assert!(selection.clear_marks());
        assert!(!selection.clear_marks());
    }

    #[test]
    fn x_toggles_and_mark_all_takes_a_views_objects() {
        let page = page(&["a1", "a2"], &["b1"]);
        let mut selection = PageSelection::default();
        assert!(
            selection.toggle_mark_at_cursor(&page).is_some(),
            "marks the first row"
        );
        assert!(selection.is_marked(&key("a1")));
        selection.toggle_mark_at_cursor(&page);
        assert!(!selection.is_marked(&key("a1")));
        selection.mark_all(page.views[1].objects());
        assert_eq!(selection.marked_in([key("a1"), key("b1")]), [key("b1")]);
    }

    #[test]
    fn a_vanished_cursor_detaches_and_resumes_in_place() {
        let first = page(&["a1", "a2", "a3"], &[]);
        let mut selection = PageSelection::default();
        selection.move_by(&first, 2);
        assert_eq!(at(&selection, &first), Some(key("a2")));
        let second = page(&["a1", "a3"], &[]);
        selection.update(&second, |_| true);
        assert_eq!(selection.cursor(), None, "no row under the cursor");
        selection.move_by(&second, 1);
        assert_eq!(
            at(&selection, &second),
            Some(key("a3")),
            "the row that took its place"
        );
    }

    #[test]
    fn the_cursor_follows_its_stop_through_rebuilds() {
        let first = page(&["a1", "a2"], &["b1"]);
        let mut selection = PageSelection::default();
        selection.move_by(&first, 3);
        assert_eq!(at(&selection, &first), Some(key("b1")));
        let second = page(&["a0", "a1", "a2"], &["b1"]);
        selection.update(&second, |_| true);
        assert_eq!(at(&selection, &second), Some(key("b1")));
        assert_eq!(selection.cursor(), Some(5));
    }
}
