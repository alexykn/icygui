//! Selection in a dashboard list: the cursor row (keyboard focus, whose pane
//! is open), the rows marked for bulk actions, and the anchor that shift
//! selection extends from.
//!
//! Everything is keyed by [`ObjectKey`], so a new snapshot that reorders,
//! adds or removes rows keeps the selection on the same objects
//! ([`ListSelection::update_rows`]). Pure and free of GPUI, so it's tested
//! directly.

use std::cell::OnceCell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use ic_core::snapshot::DashboardRow;
use ic_model::ObjectKey;

/// A dashboard's rows plus an index from object to row, built on first use
/// (only reconciling a moved cursor needs it).
#[derive(Debug, Default)]
pub(crate) struct Rows {
    rows: Arc<Vec<DashboardRow>>,
    index: OnceCell<HashMap<ObjectKey, usize>>,
}

impl Rows {
    pub(crate) fn new(rows: Arc<Vec<DashboardRow>>) -> Self {
        Self {
            rows,
            index: OnceCell::new(),
        }
    }

    /// Whether these are exactly `rows` (the same evaluation result).
    pub(crate) fn is(&self, rows: &Arc<Vec<DashboardRow>>) -> bool {
        Arc::ptr_eq(&self.rows, rows)
    }

    /// Number of rows, group headers included.
    pub(crate) fn len(&self) -> usize {
        self.rows.len()
    }

    /// Row `index`.
    pub(crate) fn get(&self, index: usize) -> Option<&DashboardRow> {
        self.rows.get(index)
    }

    /// The object in row `index`; `None` for group headers and out of range.
    pub(crate) fn key(&self, index: usize) -> Option<&ObjectKey> {
        match self.rows.get(index)? {
            DashboardRow::Object(key) => Some(key),
            DashboardRow::Group { .. } => None,
        }
    }

    /// The first row showing `key`.
    pub(crate) fn position(&self, key: &ObjectKey) -> Option<usize> {
        self.index
            .get_or_init(|| {
                let mut index = HashMap::with_capacity(self.rows.len());
                for (position, row) in self.rows.iter().enumerate() {
                    if let DashboardRow::Object(key) = row {
                        index.entry(key.clone()).or_insert(position);
                    }
                }
                index
            })
            .get(key)
            .copied()
    }

    /// The object row nearest to `index` in the given direction, `index`
    /// itself included.
    fn object_from(&self, index: usize, forward: bool) -> Option<usize> {
        if forward {
            (index..self.len()).find(|&row| self.key(row).is_some())
        } else {
            (0..=index.min(self.len().checked_sub(1)?))
                .rev()
                .find(|&row| self.key(row).is_some())
        }
    }

    /// The object row nearest to `index`, preferring later rows.
    fn object_near(&self, index: usize) -> Option<usize> {
        let index = index.min(self.len().checked_sub(1)?);
        self.object_from(index, true)
            .or_else(|| self.object_from(index, false))
    }

    /// Object keys in rows `from..=to` (either order).
    fn keys_between(&self, from: usize, to: usize) -> impl Iterator<Item = &ObjectKey> {
        let (start, end) = if from <= to { (from, to) } else { (to, from) };
        (start..=end.min(self.len().saturating_sub(1))).filter_map(|row| self.key(row))
    }
}

/// A row position that remembers which object it was on.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Position {
    key: ObjectKey,
    index: usize,
}

impl Position {
    fn at(rows: &Rows, index: usize) -> Option<Self> {
        Some(Self {
            key: rows.key(index)?.clone(),
            index,
        })
    }

    /// The same object in `rows`; the row nearest to the old position if the
    /// object left the list.
    fn follow(&self, rows: &Rows) -> Option<Self> {
        if rows.key(self.index) == Some(&self.key) {
            return Some(self.clone());
        }
        match rows.position(&self.key) {
            Some(index) => Some(Self {
                key: self.key.clone(),
                index,
            }),
            None => Self::at(rows, rows.object_near(self.index)?),
        }
    }
}

/// The selection state of one dashboard list.
#[derive(Debug, Default)]
pub(crate) struct ListSelection {
    rows: Rows,
    cursor: Option<Position>,
    /// Where shift selection extends from.
    anchor: Option<Position>,
    /// Marks from before the current shift extension, kept as it grows and
    /// shrinks.
    base: Option<HashSet<ObjectKey>>,
    marked: HashSet<ObjectKey>,
}

impl ListSelection {
    pub(crate) fn new(rows: Arc<Vec<DashboardRow>>) -> Self {
        Self {
            rows: Rows::new(rows),
            ..Self::default()
        }
    }

    /// The rows the selection refers to.
    pub(crate) fn rows(&self) -> &Rows {
        &self.rows
    }

    /// Switches to newly evaluated rows (a new snapshot): the cursor and the
    /// anchor stay on their objects or, if those left the list, on the
    /// nearest row; marks of objects that left are dropped, so a bulk
    /// action never hits rows the user can't see. Returns whether the rows
    /// changed.
    pub(crate) fn update_rows(&mut self, rows: &Arc<Vec<DashboardRow>>) -> bool {
        if self.rows.is(rows) {
            return false;
        }
        self.rows = Rows::new(rows.clone());
        self.cursor = self
            .cursor
            .take()
            .and_then(|cursor| cursor.follow(&self.rows));
        self.anchor = self
            .anchor
            .take()
            .and_then(|anchor| anchor.follow(&self.rows));
        let rows = &self.rows;
        self.marked.retain(|key| rows.position(key).is_some());
        if let Some(base) = self.base.as_mut() {
            base.retain(|key| rows.position(key).is_some());
        }
        true
    }

    /// The cursor's row.
    pub(crate) fn cursor(&self) -> Option<usize> {
        self.cursor.as_ref().map(|cursor| cursor.index)
    }

    /// The object under the cursor.
    pub(crate) fn cursor_key(&self) -> Option<&ObjectKey> {
        self.cursor.as_ref().map(|cursor| &cursor.key)
    }

    /// Puts the cursor on row `index` and clears the marks (a plain click).
    /// Returns `false` for group headers and rows out of range.
    pub(crate) fn select(&mut self, index: usize) -> bool {
        let Some(position) = Position::at(&self.rows, index) else {
            return false;
        };
        self.marked.clear();
        self.place(position);
        true
    }

    /// Puts the cursor on `key`'s first row, keeping the marks.
    pub(crate) fn select_key(&mut self, key: &ObjectKey) -> bool {
        let Some(position) = self
            .rows
            .position(key)
            .and_then(|index| Position::at(&self.rows, index))
        else {
            return false;
        };
        self.place(position);
        true
    }

    /// Moves the cursor `steps` object rows down (negative: up), skipping
    /// group headers and stopping at the ends. Without a cursor, moving down
    /// starts at the first row and moving up at the last. Marks are kept.
    /// Returns the new cursor row.
    pub(crate) fn move_by(&mut self, steps: isize) -> Option<usize> {
        let target = self.step(steps)?;
        self.place(Position::at(&self.rows, target)?);
        Some(target)
    }

    /// Moves the cursor by `rows` rows (a page), group headers counting,
    /// then onto the nearest object row in that direction.
    pub(crate) fn move_page(&mut self, rows: isize) -> Option<usize> {
        let last = self.rows.len().checked_sub(1)?;
        let forward = rows >= 0;
        let target = match self.cursor() {
            Some(cursor) => cursor.saturating_add_signed(rows).min(last),
            None if forward => 0,
            None => last,
        };
        let target = self
            .rows
            .object_from(target, forward)
            .or_else(|| self.rows.object_from(target, !forward))?;
        self.place(Position::at(&self.rows, target)?);
        Some(target)
    }

    /// Moves the cursor to the first (or last) object row.
    pub(crate) fn move_to_end(&mut self, last: bool) -> Option<usize> {
        let target = if last {
            self.rows
                .object_from(self.rows.len().checked_sub(1)?, false)?
        } else {
            self.rows.object_from(0, true)?
        };
        self.place(Position::at(&self.rows, target)?);
        Some(target)
    }

    /// Moves the cursor like [`Self::move_by`] and marks every object row
    /// between the anchor and the cursor (shift-j / shift-down).
    pub(crate) fn extend_by(&mut self, steps: isize) -> Option<usize> {
        let target = self.step(steps)?;
        self.extend_to(target).then_some(target)
    }

    /// Marks every object row between the anchor and `index`, keeping the
    /// marks from before the extension started, and moves the cursor there
    /// (shift-click). Returns `false` for group headers.
    pub(crate) fn extend_to(&mut self, index: usize) -> bool {
        let Some(target) = Position::at(&self.rows, index) else {
            return false;
        };
        let anchor = match self.anchor.clone().or_else(|| self.cursor.clone()) {
            Some(anchor) => anchor,
            None => target.clone(),
        };
        let base = self.base.get_or_insert_with(|| self.marked.clone()).clone();
        self.marked = base;
        self.marked
            .extend(self.rows.keys_between(anchor.index, target.index).cloned());
        self.anchor = Some(anchor);
        self.cursor = Some(target);
        true
    }

    /// Flips the mark of row `index` and puts the cursor there
    /// (ctrl/cmd-click).
    pub(crate) fn toggle_mark(&mut self, index: usize) -> bool {
        let Some(position) = Position::at(&self.rows, index) else {
            return false;
        };
        if !self.marked.remove(&position.key) {
            self.marked.insert(position.key.clone());
        }
        self.place(position);
        true
    }

    /// Flips the mark of the cursor's row (`x`); without a cursor, of the
    /// first row.
    pub(crate) fn toggle_mark_at_cursor(&mut self) -> bool {
        let index = match self.cursor() {
            Some(index) => index,
            None => match self.rows.object_from(0, true) {
                Some(index) => index,
                None => return false,
            },
        };
        self.toggle_mark(index)
    }

    /// Marks every object in the list.
    pub(crate) fn mark_all(&mut self) {
        self.base = None;
        self.marked = (0..self.rows.len())
            .filter_map(|row| self.rows.key(row).cloned())
            .collect();
    }

    /// Clears the marks. Returns whether there were any.
    pub(crate) fn clear_marks(&mut self) -> bool {
        self.base = None;
        let had_marks = !self.marked.is_empty();
        self.marked.clear();
        had_marks
    }

    /// Whether `key` is marked.
    pub(crate) fn is_marked(&self, key: &ObjectKey) -> bool {
        self.marked.contains(key)
    }

    /// Number of marked objects.
    pub(crate) fn marked_count(&self) -> usize {
        self.marked.len()
    }

    /// The marked objects in row order.
    pub(crate) fn marked_keys(&self) -> Vec<ObjectKey> {
        if self.marked.is_empty() {
            return Vec::new();
        }
        let mut seen = HashSet::with_capacity(self.marked.len());
        (0..self.rows.len())
            .filter_map(|row| self.rows.key(row))
            .filter(|key| self.marked.contains(*key) && seen.insert(*key))
            .cloned()
            .collect()
    }

    /// Moves the cursor and the anchor to `position`; the next shift
    /// extension starts from the marks as they are now.
    fn place(&mut self, position: Position) {
        self.anchor = Some(position.clone());
        self.cursor = Some(position);
        self.base = None;
    }

    /// The object row `steps` object rows away from the cursor.
    fn step(&self, steps: isize) -> Option<usize> {
        let Some(mut row) = self.cursor() else {
            return if steps >= 0 {
                self.rows.object_from(0, true)
            } else {
                self.rows
                    .object_from(self.rows.len().checked_sub(1)?, false)
            };
        };
        for _ in 0..steps.unsigned_abs() {
            let next = if steps > 0 {
                row.checked_add(1)
                    .and_then(|next| self.rows.object_from(next, true))
            } else {
                row.checked_sub(1)
                    .and_then(|previous| self.rows.object_from(previous, false))
            };
            match next {
                Some(next) => row = next,
                None => break,
            }
        }
        Some(row)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service(name: &str) -> ObjectKey {
        ObjectKey::service("host", name)
    }

    fn object(name: &str) -> DashboardRow {
        DashboardRow::Object(service(name))
    }

    fn group(label: &str, count: usize) -> DashboardRow {
        DashboardRow::Group {
            label: label.to_owned(),
            count,
        }
    }

    fn rows(rows: Vec<DashboardRow>) -> Arc<Vec<DashboardRow>> {
        Arc::new(rows)
    }

    fn flat(names: &[&str]) -> Arc<Vec<DashboardRow>> {
        rows(names.iter().map(|name| object(name)).collect())
    }

    fn names(keys: &[ObjectKey]) -> Vec<String> {
        keys.iter()
            .map(|key| key.as_service().unwrap().name.to_string())
            .collect()
    }

    #[test]
    fn moving_starts_at_the_ends_and_stops_there() {
        let mut selection = ListSelection::new(flat(&["a", "b", "c"]));
        assert_eq!(selection.cursor(), None);
        assert_eq!(selection.move_by(1), Some(0), "down starts at the top");
        assert_eq!(selection.move_by(1), Some(1));
        assert_eq!(selection.move_by(5), Some(2), "stops at the end");
        assert_eq!(selection.move_by(-1), Some(1));
        assert_eq!(selection.move_by(-9), Some(0), "stops at the top");

        let mut from_below = ListSelection::new(flat(&["a", "b", "c"]));
        assert_eq!(from_below.move_by(-1), Some(2), "up starts at the bottom");
    }

    #[test]
    fn moving_skips_group_headers() {
        let mut selection = ListSelection::new(rows(vec![
            group("db-prod-03", 2),
            object("a"),
            object("b"),
            group("db-prod-01", 1),
            object("c"),
        ]));
        assert_eq!(selection.move_by(1), Some(1));
        assert_eq!(selection.move_by(1), Some(2));
        assert_eq!(selection.move_by(1), Some(4), "jumps over the header");
        assert_eq!(selection.move_by(-1), Some(2));
        assert_eq!(selection.move_to_end(false), Some(1));
        assert_eq!(selection.move_to_end(true), Some(4));
        assert!(!selection.select(0), "headers can't be selected");
        assert_eq!(selection.cursor(), Some(4));
    }

    #[test]
    fn empty_lists_have_no_cursor() {
        let mut selection = ListSelection::new(rows(Vec::new()));
        assert_eq!(selection.move_by(1), None);
        assert_eq!(selection.move_by(-1), None);
        assert_eq!(selection.move_page(10), None);
        assert_eq!(selection.move_to_end(true), None);
        assert!(!selection.toggle_mark_at_cursor());
        let mut headers_only = ListSelection::new(rows(vec![group("g", 0)]));
        assert_eq!(headers_only.move_by(1), None);
        assert_eq!(headers_only.move_page(-3), None);
    }

    #[test]
    fn pages_count_rows_and_land_on_objects() {
        let mut names: Vec<String> = (0..30).map(|index| format!("s{index}")).collect();
        names.sort();
        let mut list: Vec<DashboardRow> = names.iter().map(|name| object(name)).collect();
        list.insert(10, group("g", 20));
        let mut selection = ListSelection::new(rows(list));
        assert_eq!(
            selection.move_page(10),
            Some(0),
            "no cursor: page down starts at the top"
        );
        assert_eq!(selection.move_page(10), Some(11), "row 10 is a header");
        assert_eq!(selection.move_page(100), Some(30));
        assert_eq!(selection.move_page(-25), Some(5));
        assert_eq!(selection.move_page(-25), Some(0));
    }

    #[test]
    fn clicking_selects_one_row_and_clears_marks() {
        let mut selection = ListSelection::new(flat(&["a", "b", "c"]));
        selection.toggle_mark(0);
        selection.toggle_mark(2);
        assert_eq!(selection.marked_count(), 2);
        assert!(selection.select(1));
        assert_eq!(selection.cursor_key(), Some(&service("b")));
        assert_eq!(selection.marked_count(), 0);
        assert!(!selection.select(7), "out of range");
    }

    #[test]
    fn x_toggles_the_cursor_row() {
        let mut selection = ListSelection::new(flat(&["a", "b", "c"]));
        assert!(selection.toggle_mark_at_cursor(), "marks the first row");
        assert!(selection.is_marked(&service("a")));
        selection.move_by(1);
        selection.toggle_mark_at_cursor();
        assert_eq!(names(&selection.marked_keys()), ["a", "b"]);
        selection.toggle_mark_at_cursor();
        assert_eq!(names(&selection.marked_keys()), ["a"]);
        assert!(selection.clear_marks());
        assert!(!selection.clear_marks(), "nothing left to clear");
    }

    #[test]
    fn shift_extends_from_the_anchor_and_can_shrink_again() {
        let mut selection = ListSelection::new(flat(&["a", "b", "c", "d", "e"]));
        selection.select(1);
        assert_eq!(selection.extend_by(2), Some(3));
        assert_eq!(names(&selection.marked_keys()), ["b", "c", "d"]);
        assert_eq!(selection.extend_by(-1), Some(2));
        assert_eq!(names(&selection.marked_keys()), ["b", "c"], "shrinks");
        assert_eq!(selection.extend_by(-2), Some(0));
        assert_eq!(
            names(&selection.marked_keys()),
            ["a", "b"],
            "crosses the anchor"
        );
        assert_eq!(selection.cursor(), Some(0));
    }

    #[test]
    fn shift_click_keeps_marks_made_before() {
        let mut selection = ListSelection::new(flat(&["a", "b", "c", "d", "e", "f"]));
        selection.toggle_mark(0);
        selection.toggle_mark(4);
        // The anchor is the last toggled row.
        assert!(selection.extend_to(5));
        assert_eq!(names(&selection.marked_keys()), ["a", "e", "f"]);
        assert!(selection.extend_to(2));
        assert_eq!(
            names(&selection.marked_keys()),
            ["a", "c", "d", "e"],
            "f is released, a stays"
        );
    }

    #[test]
    fn shift_click_without_a_cursor_marks_one_row() {
        let mut selection = ListSelection::new(flat(&["a", "b"]));
        assert!(selection.extend_to(1));
        assert_eq!(names(&selection.marked_keys()), ["b"]);
        assert_eq!(selection.cursor(), Some(1));
    }

    #[test]
    fn ranges_skip_headers() {
        let mut selection = ListSelection::new(rows(vec![
            object("a"),
            group("g", 2),
            object("b"),
            object("c"),
        ]));
        selection.select(0);
        assert!(selection.extend_to(3));
        assert_eq!(names(&selection.marked_keys()), ["a", "b", "c"]);
        assert!(!selection.extend_to(1), "headers can't be extended to");
    }

    #[test]
    fn mark_all_and_marked_keys_in_row_order() {
        let mut selection = ListSelection::new(flat(&["c", "a", "b"]));
        selection.mark_all();
        assert_eq!(selection.marked_count(), 3);
        assert_eq!(names(&selection.marked_keys()), ["c", "a", "b"]);
    }

    #[test]
    fn duplicate_rows_are_marked_once() {
        // Grouping by host group lists an object under each of its groups.
        let mut selection = ListSelection::new(rows(vec![
            group("g1", 2),
            object("a"),
            object("b"),
            group("g2", 1),
            object("a"),
        ]));
        selection.mark_all();
        assert_eq!(names(&selection.marked_keys()), ["a", "b"]);
        assert_eq!(selection.rows().position(&service("a")), Some(1));
    }

    #[test]
    fn the_cursor_follows_its_object_through_updates() {
        let mut selection = ListSelection::new(flat(&["a", "b", "c"]));
        selection.select(1);
        assert!(selection.update_rows(&flat(&["x", "c", "a", "b"])));
        assert_eq!(selection.cursor(), Some(3));
        assert_eq!(selection.cursor_key(), Some(&service("b")));
        let same = selection.rows().rows.clone();
        assert!(
            !selection.update_rows(&same),
            "the same rows change nothing"
        );
    }

    #[test]
    fn a_vanished_cursor_object_leaves_the_cursor_nearby() {
        let mut selection = ListSelection::new(flat(&["a", "b", "c", "d"]));
        selection.select(2);
        // c recovered and left the problem list.
        selection.update_rows(&flat(&["a", "b", "d"]));
        assert_eq!(selection.cursor(), Some(2));
        assert_eq!(selection.cursor_key(), Some(&service("d")));
        // The last row left: the cursor moves up.
        selection.update_rows(&flat(&["a", "b"]));
        assert_eq!(selection.cursor_key(), Some(&service("b")));
        // Everything left.
        selection.update_rows(&rows(Vec::new()));
        assert_eq!(selection.cursor(), None);
    }

    #[test]
    fn a_vanished_object_next_to_a_header_moves_to_an_object() {
        let mut selection = ListSelection::new(rows(vec![
            group("g1", 1),
            object("a"),
            group("g2", 1),
            object("b"),
        ]));
        selection.select(1);
        selection.update_rows(&rows(vec![group("g2", 1), object("b")]));
        assert_eq!(selection.cursor_key(), Some(&service("b")));
        assert_eq!(selection.cursor(), Some(1));
    }

    #[test]
    fn marks_of_objects_that_left_are_dropped() {
        let mut selection = ListSelection::new(flat(&["a", "b", "c"]));
        selection.mark_all();
        selection.update_rows(&flat(&["c", "a", "d"]));
        assert_eq!(names(&selection.marked_keys()), ["c", "a"]);
        assert!(!selection.is_marked(&service("b")));
    }

    #[test]
    fn the_anchor_survives_updates_too() {
        let mut selection = ListSelection::new(flat(&["a", "b", "c", "d"]));
        selection.select(1);
        selection.update_rows(&flat(&["d", "c", "b", "a"]));
        // The anchor is still on b (now row 2): extending up to d marks b..d.
        assert!(selection.extend_to(0));
        assert_eq!(names(&selection.marked_keys()), ["d", "c", "b"]);
    }

    #[test]
    fn selecting_by_key() {
        let mut selection = ListSelection::new(flat(&["a", "b"]));
        assert!(selection.select_key(&service("b")));
        assert_eq!(selection.cursor(), Some(1));
        assert!(!selection.select_key(&service("zz")));
        assert_eq!(selection.cursor(), Some(1));
    }
}
