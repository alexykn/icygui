//! The dashboard page as one list of items (topic 04): every view's header
//! (on a dashboard with several views) and body, stacked, each item with
//! the exact height it is drawn at, so the page scrolls as a whole and
//! builds only the items on screen however many rows its views have.
//!
//! - **Lists** are their rows. **Grouped lists** are the host-with-services
//!   style (README, *Rules that span the topics*): a 36px band per group
//!   (by host: the host itself), the group's rows under it without indent,
//!   collapsible ([`Folds`]); grouped by host, a host pages by count
//!   ([`crate::paging`]: up to seven rows, problems first and never
//!   hidden, then `+ N more`, which shows the whole host in place and
//!   becomes `− show fewer`).
//! - **Host-group grids** are lines of up to three groups of squares, or a
//!   group's header and lines of labelled cells.
//! - **Summary tiles** are lines of tiles; an **event stream** is one item
//!   that scrolls its events past its `lines`.
//!
//! Collapsing and paging only change what shows: counts, marks and *mark
//! all* still cover every row of the view.
//!
//! The page also lists the places the keyboard cursor can be
//! ([`Stop`]): rows, bands, paging rows, a grid's hosts, a stream's
//! events, and the headers of views (a collapsed view is only its header).
//! Pure, so it's tested without a window.

use std::cell::OnceCell;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::Arc;

use gpui::Pixels;
use ic_config::{GridCells, GroupBy, GroupSource, ObjectKind, View, ViewDisplay};
use ic_core::LogEntry;
use ic_core::snapshot::{DashboardResult, DashboardRow, Grid, Snapshot, Summary, Tile};
use ic_model::{CheckableState, Host, HostName, HostState, ObjectKey, ServiceState};
use ic_ui_kit::{Density, Metrics, ObjectMark, Theme, px};

use crate::paging;

/// A view's or a group's id on the page.
pub(crate) type Id = Arc<str>;

/// The heights and spacings the page's items are drawn with (the theme's,
/// at the interface size in effect).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Sizes {
    /// A view header (36px) with its rule.
    pub(crate) header: Pixels,
    /// A list row with its rule.
    pub(crate) row: Pixels,
    /// A group band (36px) with its rule.
    pub(crate) band: Pixels,
    /// The paging row (`+ N more`, 30px) with its rule.
    pub(crate) more: Pixels,
    /// A note in place of a view's body (its filter doesn't work).
    pub(crate) note: Pixels,
    /// An event stream's line with its rule.
    pub(crate) event: Pixels,
    /// The page's side padding.
    pub(crate) padding: Pixels,
    /// A grid's square and the gap between squares.
    pub(crate) square: Pixels,
    pub(crate) square_gap: Pixels,
    /// A grid group's header line and the gap under it.
    pub(crate) group_header: Pixels,
    pub(crate) group_gap: Pixels,
    /// Between grid groups side by side, and between their lines.
    pub(crate) column_gap: Pixels,
    pub(crate) line_gap: Pixels,
    /// Above and below a grid's (or tiles') body.
    pub(crate) body_top: Pixels,
    pub(crate) body_bottom: Pixels,
    /// A labelled cell, the gap between cells, the narrowest cell, and the
    /// gap between two groups' sections.
    pub(crate) cell: Pixels,
    pub(crate) cell_gap: Pixels,
    pub(crate) cell_min: Pixels,
    pub(crate) section_gap: Pixels,
    /// A summary tile, the gap between tiles, the narrowest tile, and the
    /// space under the last line of tiles.
    pub(crate) tile: Pixels,
    pub(crate) tile_gap: Pixels,
    pub(crate) tile_min: Pixels,
    pub(crate) tiles_bottom: Pixels,
}

impl Sizes {
    /// The sizes in `theme`.
    pub(crate) fn of(theme: &Theme) -> Self {
        let metrics = theme.metrics;
        let compact = theme.density == Density::Compact;
        Self {
            header: Metrics::with_rule(metrics.summary_bar_height),
            row: Metrics::with_rule(metrics.row_height),
            band: Metrics::with_rule(metrics.group_row_height),
            more: Metrics::with_rule(metrics.item_row_height),
            note: Metrics::with_rule(metrics.summary_bar_height),
            event: Metrics::with_rule(if compact { px(30.) } else { px(46.) }),
            padding: metrics.list_padding,
            square: px(12.),
            square_gap: px(3.),
            group_header: px(18.),
            group_gap: px(8.),
            column_gap: px(32.),
            line_gap: px(16.),
            body_top: px(14.),
            body_bottom: px(18.),
            cell: px(26.),
            cell_gap: px(4.),
            cell_min: px(170.),
            section_gap: px(18.),
            tile: px(83.),
            tile_gap: px(12.),
            tile_min: px(240.),
            tiles_bottom: px(16.),
        }
    }
}

/// What the user folded on a page: views and groups collapsed, hosts
/// expanded past their seven rows. Kept while the app runs; a view starts
/// as its settings say (*collapse by default*).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Folds {
    /// Views collapsed (`true`) or expanded against their default.
    views: HashMap<Id, bool>,
    /// Collapsed groups, by view and group.
    groups: HashSet<(Id, Id)>,
    /// Hosts showing every service, by view and group.
    expanded: HashSet<(Id, Id)>,
}

impl Folds {
    /// Whether `view` shows only its header.
    pub(crate) fn view_collapsed(&self, view: &View) -> bool {
        self.views
            .get(view.id.as_str())
            .copied()
            .unwrap_or(view.collapsed)
    }

    /// Collapses or expands `view`.
    pub(crate) fn set_view(&mut self, view: &View, collapsed: bool) {
        self.views.insert(Id::from(view.id.as_str()), collapsed);
    }

    /// Whether `group` of `view` shows only its band.
    pub(crate) fn group_collapsed(&self, view: &str, group: &str) -> bool {
        self.groups.contains(&(Id::from(view), Id::from(group)))
    }

    /// Collapses or expands `group` of `view`.
    pub(crate) fn set_group(&mut self, view: &Id, group: &Id, collapsed: bool) {
        let key = (view.clone(), group.clone());
        if collapsed {
            self.groups.insert(key);
        } else {
            self.groups.remove(&key);
        }
    }

    /// Whether the host `group` of `view` shows all its services.
    pub(crate) fn expanded(&self, view: &str, group: &str) -> bool {
        self.expanded.contains(&(Id::from(view), Id::from(group)))
    }

    /// Shows all of a host's services, or pages them again.
    pub(crate) fn set_expanded(&mut self, view: &Id, group: &Id, expanded: bool) {
        let key = (view.clone(), group.clone());
        if expanded {
            self.expanded.insert(key);
        } else {
            self.expanded.remove(&key);
        }
    }
}

/// The page filtered to one group for a while (topic 05: a click on a
/// grid group's name or a tile): the other views show only that group's
/// objects. Never saved; Esc or the header chip's × clears it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GroupFilter {
    /// A host group, or a custom variable's value.
    pub(crate) by: GroupSource,
    /// The custom variable, without prefix (`role`).
    pub(crate) var: String,
    /// The host group's name, or the value.
    pub(crate) name: String,
    /// What the chip says after its kind (the group's display name).
    pub(crate) label: String,
}

impl GroupFilter {
    /// Whether `host` is in the group.
    pub(crate) fn includes(&self, host: &Host) -> bool {
        match self.by {
            GroupSource::HostGroup => host.groups.contains(&self.name),
            GroupSource::CustomVar => host
                .vars
                .get(&self.var)
                .is_some_and(|value| var_values(value).contains(&self.name)),
        }
    }

    /// Whether the object `key` (a host, or a service on it) is in the
    /// group.
    pub(crate) fn includes_object(&self, snapshot: &Snapshot, key: &ObjectKey) -> bool {
        snapshot
            .hosts
            .get(key.host_name())
            .is_some_and(|host| self.includes(host))
    }

    /// The header chip's text: `host group edge-ams`, `role postgres`.
    pub(crate) fn chip(&self) -> String {
        match self.by {
            GroupSource::HostGroup => format!("host group {}", self.label),
            GroupSource::CustomVar => format!("{} {}", self.var, self.label),
        }
    }

    /// Whether this filter is a group of a grid or tiles view grouped as
    /// `view` is (its groups ring and dim instead of filtering).
    fn same_groups(&self, view: &View) -> bool {
        self.by == view.groups.by
            && (self.by == GroupSource::HostGroup || self.var == view.groups.custom_var_name())
    }
}

/// The groups a custom variable's value files a host under, as the core
/// files them: a string, a number or a boolean as text, or each of an
/// array's.
fn var_values(value: &serde_json::Value) -> Vec<String> {
    fn scalar(value: &serde_json::Value) -> Option<String> {
        match value {
            serde_json::Value::String(text) => Some(text.clone()),
            serde_json::Value::Number(number) => Some(number.to_string()),
            serde_json::Value::Bool(flag) => Some(flag.to_string()),
            _ => None,
        }
    }
    match value {
        serde_json::Value::Array(items) => items.iter().filter_map(scalar).collect(),
        other => scalar(other).into_iter().collect(),
    }
}

/// What a page is built from.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PageInput<'a> {
    /// The dashboard's views.
    pub(crate) views: &'a [View],
    /// Their evaluation (none yet: every view waits).
    pub(crate) result: Option<&'a DashboardResult>,
    /// The objects.
    pub(crate) snapshot: &'a Snapshot,
    /// What the user folded.
    pub(crate) folds: &'a Folds,
    /// The page's group filter.
    pub(crate) filter: Option<&'a GroupFilter>,
    /// Several views: each gets a header. One list view shows as rc1 did,
    /// under the dashboard's header and summary bar, without one.
    pub(crate) multi: bool,
    /// Views of these kinds can't be read (a permission is missing).
    pub(crate) denied: [bool; 2],
    /// The page's width (grids and tiles lay out to it).
    pub(crate) width: Pixels,
    /// Item heights.
    pub(crate) sizes: Sizes,
}

/// One item of the page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Item {
    /// The view it belongs to (index into [`Page::views`]).
    pub(crate) view: usize,
    /// What it is.
    pub(crate) kind: ItemKind,
}

/// What an item is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ItemKind {
    /// The view's header.
    Header,
    /// A line in place of the body: the filter doesn't work, no
    /// permission, not evaluated yet.
    Note,
    /// An object row: `row` indexes the view's rows, `group` its groups.
    Row { row: usize, group: Option<usize> },
    /// A group's band.
    Band { group: usize },
    /// A host's paging row (`+ N more` / `− show fewer`).
    More { group: usize },
    /// A line of a grid ([`GridLayout::lines`]).
    Grid { line: usize },
    /// A line of tiles.
    Tiles { line: usize },
    /// An event stream's events.
    Stream,
}

/// Where the keyboard cursor can be, by identity, so it stays on the same
/// thing when the page is built again.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Stop {
    /// A view's header (for a collapsed view, all there is of it).
    Header(Id),
    /// An object row (under a group, when grouped: an object filed under
    /// several groups has a row in each).
    Row {
        view: Id,
        group: Option<Id>,
        key: ObjectKey,
    },
    /// A group's band.
    Band { view: Id, group: Id },
    /// A host's paging row.
    More { view: Id, group: Id },
    /// A host on a grid (in a group: a host can be in several).
    Cell { view: Id, group: Id, host: HostName },
    /// An event of a stream.
    Event { view: Id, event: EventKey },
}

impl Stop {
    /// The view the stop is in.
    pub(crate) fn view(&self) -> &Id {
        match self {
            Self::Header(view)
            | Self::Row { view, .. }
            | Self::Band { view, .. }
            | Self::More { view, .. }
            | Self::Cell { view, .. }
            | Self::Event { view, .. } => view,
        }
    }

    /// The object the stop stands for: a row's, a host band's or a grid
    /// cell's (an event's: its object).
    pub(crate) fn object(&self, page: &Page) -> Option<ObjectKey> {
        match self {
            Self::Row { key, .. } => Some(key.clone()),
            Self::Cell { host, .. } => Some(ObjectKey::Host { name: host.clone() }),
            Self::Band { view, group } => page
                .view_by_id(view)
                .and_then(|view| view.groups.iter().find(|candidate| candidate.id == *group))
                .and_then(|group| group.host.clone())
                .map(|name| ObjectKey::Host { name }),
            Self::Event { event, .. } => Some(event.object.clone()),
            Self::Header(_) | Self::More { .. } => None,
        }
    }
}

/// An event of a stream, by what it records.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct EventKey {
    at: u64,
    pub(crate) object: ObjectKey,
    kind: ic_core::LogKind,
    /// Equal events (rare): the first, second, …
    nth: u32,
}

/// A stop on the page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StopEntry {
    /// What it is.
    pub(crate) stop: Stop,
    /// The item it is drawn in.
    pub(crate) item: usize,
    /// Inside the item: a grid cell's index in its group, a stream
    /// event's index.
    pub(crate) sub: usize,
    /// For a grid cell: its group's index.
    pub(crate) group: usize,
}

/// A view's state on the page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ViewState {
    /// Evaluated, with something to show.
    Ready,
    /// Evaluated, nothing to show (the header says so).
    Empty,
    /// Its filter doesn't work.
    Error,
    /// No permission to read its objects.
    Denied,
    /// Not evaluated yet.
    Waiting,
}

/// One view on the page.
#[derive(Clone, Debug)]
pub(crate) struct ViewPage {
    /// The view's id.
    pub(crate) id: Id,
    /// Its index in the dashboard's views.
    pub(crate) index: usize,
    /// Folded to its header.
    pub(crate) collapsed: bool,
    /// Evaluated, empty, …
    pub(crate) state: ViewState,
    /// A list's rows, as the core evaluated them.
    pub(crate) rows: Arc<Vec<DashboardRow>>,
    /// Under the page's group filter: which of `rows` it leaves (by
    /// index); `None`: every row.
    pub(crate) included: Option<Vec<bool>>,
    /// A grouped list's groups, in order.
    pub(crate) groups: Vec<ListGroup>,
    /// A grid's layout.
    pub(crate) grid: Option<GridLayout>,
    /// Tiles' layout.
    pub(crate) tiles: Option<TilesLayout>,
    /// A stream's events (newest first; filtered to the page's group).
    pub(crate) events: Vec<LogEntry>,
    /// Where a stream's events start among the page's stops (one each).
    pub(crate) event_stops: usize,
    /// How many of them show before the stream scrolls.
    pub(crate) lines: usize,
    /// The header's per-state counts: unhandled ones (under a group
    /// filter: of what shows).
    pub(crate) counts: Summary,
    /// Its items on the page.
    pub(crate) items: Range<usize>,
}

impl ViewPage {
    /// Every object row of the view, folded or paged away or not, in
    /// order, each object once; under the page's group filter only the
    /// rows it leaves (ctrl-a never marks what the page hides).
    pub(crate) fn objects(&self) -> Vec<ObjectKey> {
        let mut seen = HashSet::new();
        self.rows
            .iter()
            .enumerate()
            .filter(|(index, _)| {
                self.included
                    .as_ref()
                    .is_none_or(|included| included.get(*index).copied().unwrap_or(false))
            })
            .filter_map(|(_, row)| match row {
                DashboardRow::Object(key) if seen.insert(key.clone()) => Some(key.clone()),
                _ => None,
            })
            .collect()
    }

    /// The group with this id.
    pub(crate) fn group(&self, id: &str) -> Option<&ListGroup> {
        self.groups.iter().find(|group| &*group.id == id)
    }
}

/// A group of a grouped list.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ListGroup {
    /// Its id: the host's name when grouped by host, else the label.
    pub(crate) id: Id,
    /// What its band says: the host's or group's display name.
    pub(crate) label: String,
    /// Grouped by host: the host.
    pub(crate) host: Option<HostName>,
    /// Its rows (indices into the view's rows), every one, in the order
    /// they show: by host, problems first (worst first), then by name.
    pub(crate) members: Vec<usize>,
    /// How many of them show (the rest wait behind `+ N more`).
    pub(crate) shown: usize,
    /// It has a paging row.
    pub(crate) pages: bool,
    /// Shows all its rows (`− show fewer`).
    pub(crate) expanded: bool,
    /// Folded to its band.
    pub(crate) collapsed: bool,
    /// Its rows by state (handled problems left out), for the band.
    pub(crate) counts: Summary,
}

impl ListGroup {
    /// Rows waiting behind `+ N more`.
    pub(crate) fn hidden(&self) -> usize {
        self.members.len() - self.shown
    }
}

/// A grid laid out to the page's width.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct GridLayout {
    /// The evaluated grid.
    pub(crate) grid: Arc<Grid>,
    /// Labelled cells (else squares).
    pub(crate) cells: bool,
    /// Squares: how many groups stand side by side.
    pub(crate) columns: usize,
    /// How many squares (or cells) a group's line holds.
    pub(crate) per_line: usize,
    /// The lines, top to bottom.
    pub(crate) lines: Vec<GridLine>,
    /// The group the page is filtered to (ringed; the others dim).
    pub(crate) on: Option<usize>,
}

/// One line of a grid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum GridLine {
    /// Squares: these groups side by side.
    Groups(Range<usize>),
    /// Cells: a group's header.
    Header(usize),
    /// Cells: a line of a group's cells.
    Cells { group: usize, cells: Range<usize> },
}

/// Tiles laid out to the page's width.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TilesLayout {
    /// The tiles.
    pub(crate) tiles: Arc<Vec<Tile>>,
    /// Tiles side by side.
    pub(crate) columns: usize,
    /// The tile the page is filtered to (ringed; the others dim).
    pub(crate) on: Option<usize>,
}

/// Where `j` / `k` go from a stop, as far as a grid decides it.
enum GridStep {
    /// The stop isn't a grid's host: the plain step applies.
    NotGrid,
    /// The grid's answer: the stop, or none (nothing follows).
    To(Option<usize>),
}

/// The page: its views, items and stops.
#[derive(Debug, Default)]
pub(crate) struct Page {
    /// Every view, in the dashboard's order.
    pub(crate) views: Vec<ViewPage>,
    /// Every item, top to bottom.
    pub(crate) items: Vec<Item>,
    /// Where each item starts, and (last) where the page ends.
    tops: Vec<Pixels>,
    /// Every place the cursor can be, top to bottom.
    stops: Vec<StopEntry>,
    /// Stop → its index, built on first use.
    index: OnceCell<HashMap<Stop, usize>>,
    /// What its items were laid out with.
    sizes: Sizes,
}

impl Page {
    /// Builds the page.
    pub(crate) fn build(input: &PageInput<'_>) -> Self {
        let mut page = Self {
            tops: vec![px(0.)],
            sizes: input.sizes,
            ..Self::default()
        };
        for (index, view) in input.views.iter().enumerate() {
            page.add_view(input, index, view);
        }
        page
    }

    /// A band's height: a band sticks to the top while its rows scroll.
    pub(crate) fn band_height(&self) -> Pixels {
        self.sizes.band
    }

    /// What the items were laid out with.
    pub(crate) fn sizes(&self) -> Sizes {
        self.sizes
    }

    /// The page's height.
    pub(crate) fn height(&self) -> Pixels {
        self.tops.last().copied().unwrap_or_default()
    }

    /// Where item `index` starts.
    pub(crate) fn top(&self, index: usize) -> Pixels {
        self.tops
            .get(index)
            .copied()
            .unwrap_or_else(|| self.height())
    }

    /// Item `index`'s height.
    pub(crate) fn item_height(&self, index: usize) -> Pixels {
        self.top(index + 1) - self.top(index)
    }

    /// The item at `y` (the last one past the end).
    pub(crate) fn item_at(&self, y: Pixels) -> Option<usize> {
        if self.items.is_empty() {
            return None;
        }
        let after = self.tops[1..].partition_point(|bottom| *bottom <= y);
        Some(after.min(self.items.len() - 1))
    }

    /// The items between `from` and `to` (in page pixels).
    pub(crate) fn items_between(&self, from: Pixels, to: Pixels) -> Range<usize> {
        let Some(first) = self.item_at(from.max(px(0.))) else {
            return 0..0;
        };
        let last = self.item_at(to).unwrap_or(first);
        first..(last + 1).max(first + 1).min(self.items.len())
    }

    /// The view with this id.
    pub(crate) fn view_by_id(&self, id: &str) -> Option<&ViewPage> {
        self.views.iter().find(|view| &*view.id == id)
    }

    /// Every stop, top to bottom.
    pub(crate) fn stops(&self) -> &[StopEntry] {
        &self.stops
    }

    /// Where `stop` is among the stops.
    pub(crate) fn position(&self, stop: &Stop) -> Option<usize> {
        self.index
            .get_or_init(|| {
                let mut index = HashMap::with_capacity(self.stops.len());
                for (position, entry) in self.stops.iter().enumerate() {
                    index.entry(entry.stop.clone()).or_insert(position);
                }
                index
            })
            .get(stop)
            .copied()
    }

    /// The first stop at or after item `item`.
    pub(crate) fn stop_at_or_after(&self, item: usize) -> Option<usize> {
        let position = self.stops.partition_point(|entry| entry.item < item);
        (position < self.stops.len()).then_some(position)
    }

    /// The first stop of a view's body (not its header), if it shows one.
    pub(crate) fn first_body_stop(&self, view: usize) -> Option<usize> {
        let items = self.views.get(view)?.items.clone();
        let start = self.stop_at_or_after(items.start)?;
        (start..self.stops.len())
            .take_while(|&position| self.stops[position].item < items.end)
            .find(|&position| !matches!(self.stops[position].stop, Stop::Header(_)))
    }

    /// A view's header stop.
    pub(crate) fn header_stop(&self, view: usize) -> Option<usize> {
        let id = &self.views.get(view)?.id;
        self.position(&Stop::Header(id.clone()))
    }

    /// The view index of the view with `id`.
    pub(crate) fn view_index(&self, id: &str) -> Option<usize> {
        self.views.iter().position(|view| &*view.id == id)
    }

    /// The next (or previous) stop from `from` that isn't a view header,
    /// as `j` / `k` move; a grid moves by its lines ([`Page::grid_step`]).
    pub(crate) fn step(&self, from: usize, forward: bool) -> Option<usize> {
        match self.grid_step(from, forward) {
            GridStep::To(target) => target,
            GridStep::NotGrid => self.plain_step(from, forward),
        }
    }

    /// The next (or previous) stop that isn't a view header.
    fn plain_step(&self, from: usize, forward: bool) -> Option<usize> {
        let not_header = |position: &usize| !matches!(self.stops[*position].stop, Stop::Header(_));
        if forward {
            (from + 1..self.stops.len()).find(not_header)
        } else {
            (0..from.min(self.stops.len())).rev().find(not_header)
        }
    }

    /// The nearest stop that isn't a header at or after `from` (or before,
    /// when nothing follows).
    pub(crate) fn body_stop_near(&self, from: usize, forward: bool) -> Option<usize> {
        let last = self.stops.len().checked_sub(1)?;
        let from = from.min(last);
        let not_header = |position: &usize| !matches!(self.stops[*position].stop, Stop::Header(_));
        let ahead = || (from..=last).find(not_header);
        let behind = || (0..=from).rev().find(not_header);
        if forward {
            ahead().or_else(behind)
        } else {
            behind().or_else(ahead)
        }
    }

    /// `j` / `k` on a grid's host: the host one line down (or up) in its
    /// group, else in the group below (above) in the same column, else
    /// out of the grid (`To(None)`: nothing follows).
    fn grid_step(&self, from: usize, forward: bool) -> GridStep {
        let Some(entry) = self.stops.get(from) else {
            return GridStep::NotGrid;
        };
        let Stop::Cell { view: view_id, .. } = &entry.stop else {
            return GridStep::NotGrid;
        };
        let Some(layout) = self.view_by_id(view_id).and_then(|view| view.grid.as_ref()) else {
            return GridStep::NotGrid;
        };
        let groups = &layout.grid.groups;
        let per_line = layout.per_line.max(1);
        let columns = if layout.cells {
            1
        } else {
            layout.columns.max(1)
        };
        let (group, cell) = (entry.group, entry.sub);
        let column = cell % per_line;
        // Under the page's filter only its group has stops: the step
        // leaves the grid at the group's edge.
        let others = layout.on.is_none();
        let target = if forward {
            if cell + per_line < groups[group].cells.len() {
                Some((group, cell + per_line))
            } else {
                let below = group + columns;
                groups
                    .get(below)
                    .filter(|_| others)
                    .map(|next| (below, column.min(next.cells.len().saturating_sub(1))))
            }
        } else if cell >= per_line {
            Some((group, cell - per_line))
        } else if others && group >= columns {
            let above = group - columns;
            let count = groups[above].cells.len();
            let last_line = count.saturating_sub(1) / per_line;
            Some((
                above,
                (last_line * per_line + column).min(count.saturating_sub(1)),
            ))
        } else {
            None
        };
        GridStep::To(if let Some((group, cell)) = target {
            self.cell_stop(view_id, layout, group, cell)
        } else {
            // Out of the grid: past its last host, or before its first.
            let mut cells = (0..self.stops.len()).filter(|&position| {
                matches!(&self.stops[position].stop, Stop::Cell { view, .. } if view == view_id)
            });
            if forward {
                cells
                    .next_back()
                    .and_then(|last| self.plain_step(last, true))
            } else {
                cells.next().and_then(|first| self.plain_step(first, false))
            }
        })
    }

    /// `←` / `→` on a grid's host: the previous or next host, in reading
    /// order, staying on the grid. `None` when `from` isn't a grid's host.
    pub(crate) fn grid_side_step(&self, from: usize, forward: bool) -> Option<usize> {
        let Stop::Cell { view, .. } = &self.stops.get(from)?.stop else {
            return None;
        };
        let target = if forward {
            from.checked_add(1)
        } else {
            from.checked_sub(1)
        };
        match target.and_then(|target| self.stops.get(target).map(|entry| (target, entry))) {
            Some((
                target,
                StopEntry {
                    stop: Stop::Cell { view: other, .. },
                    ..
                },
            )) if other == view => Some(target),
            _ => Some(from),
        }
    }

    fn cell_stop(
        &self,
        view: &Id,
        layout: &GridLayout,
        group: usize,
        cell: usize,
    ) -> Option<usize> {
        let group_id: Id = Id::from(layout.grid.groups.get(group)?.name.as_str());
        let host = layout.grid.groups[group].cells.get(cell)?.host.clone();
        self.position(&Stop::Cell {
            view: view.clone(),
            group: group_id,
            host,
        })
    }

    fn push(&mut self, view: usize, kind: ItemKind, height: Pixels) -> usize {
        let index = self.items.len();
        self.items.push(Item { view, kind });
        let top = self.height();
        self.tops.push(top + height);
        index
    }

    fn stop(&mut self, stop: Stop, item: usize) {
        self.stops.push(StopEntry {
            stop,
            item,
            sub: 0,
            group: 0,
        });
    }

    fn add_view(&mut self, input: &PageInput<'_>, index: usize, view: &View) {
        let sizes = input.sizes;
        let id: Id = Id::from(view.id.as_str());
        let first = self.items.len();
        let result = input.result.and_then(|result| result.view(&view.id));
        // What the view reads: a grid its hosts (a host's services only
        // colour its square), a stream the local event log.
        let denied = match view.display {
            ViewDisplay::EventStream => false,
            ViewDisplay::HostGroupGrid => input.denied[1],
            _ => match view.object_kind {
                ObjectKind::Services => input.denied[0],
                ObjectKind::Hosts => input.denied[1],
            },
        };
        let state = match result {
            _ if denied => ViewState::Denied,
            None => ViewState::Waiting,
            Some(result) if result.error.is_some() => ViewState::Error,
            Some(result) if result.is_empty() => ViewState::Empty,
            Some(_) => ViewState::Ready,
        };
        let collapsed = input.multi && input.folds.view_collapsed(view);
        let view_index = self.views.len();
        let mut page = ViewPage {
            id: id.clone(),
            index,
            collapsed,
            state,
            rows: result
                .and_then(|result| result.list_rows().cloned())
                .unwrap_or_default(),
            included: None,
            groups: Vec::new(),
            grid: None,
            tiles: None,
            events: Vec::new(),
            event_stops: 0,
            lines: 0,
            counts: result.map(|result| result.counts).unwrap_or_default(),
            items: first..first,
        };
        if input.multi {
            let item = self.push(view_index, ItemKind::Header, sizes.header);
            self.stop(Stop::Header(id.clone()), item);
        }
        let result = result.filter(|_| state == ViewState::Ready);
        match (state, result) {
            (ViewState::Error | ViewState::Denied | ViewState::Waiting, _) if input.multi => {
                if !collapsed {
                    self.push(view_index, ItemKind::Note, sizes.note);
                }
            }
            (_, Some(result)) => {
                use ic_core::snapshot::ViewBody;
                match &result.body {
                    ViewBody::List(_) => {
                        self.add_list(input, view, view_index, &mut page);
                    }
                    ViewBody::Grid(grid) => {
                        self.add_grid(input, view, view_index, &mut page, grid, collapsed);
                    }
                    ViewBody::Tiles(tiles) => {
                        self.add_tiles(input, view, view_index, &mut page, tiles, collapsed);
                    }
                    ViewBody::Stream(events) => {
                        self.add_stream(input, view, view_index, &mut page, events, collapsed);
                    }
                }
            }
            _ => {}
        }
        page.items = first..self.items.len();
        self.views.push(page);
    }

    fn add_list(
        &mut self,
        input: &PageInput<'_>,
        view: &View,
        view_index: usize,
        page: &mut ViewPage,
    ) {
        let snapshot = input.snapshot;
        let rows = page.rows.clone();
        let included = |key: &ObjectKey| {
            input
                .filter
                .is_none_or(|filter| filter.includes_object(snapshot, key))
        };
        if input.filter.is_some() && !filter_list(input, &rows, page, &included) {
            return;
        }
        let grouping = match view.list_grouping() {
            // A host is its own band's only row: a list of hosts grouped by
            // host is a plain list (the same object never gets two rows).
            GroupBy::Host if view.object_kind == ObjectKind::Hosts => GroupBy::None,
            grouping => grouping,
        };
        let collapsed = page.collapsed;
        if grouping == GroupBy::None {
            if collapsed {
                return;
            }
            for (index, row) in rows.iter().enumerate() {
                if let DashboardRow::Object(key) = row
                    && included(key)
                {
                    let item = self.push(
                        view_index,
                        ItemKind::Row {
                            row: index,
                            group: None,
                        },
                        input.sizes.row,
                    );
                    self.stop(
                        Stop::Row {
                            view: page.id.clone(),
                            group: None,
                            key: key.clone(),
                        },
                        item,
                    );
                }
            }
            return;
        }
        page.groups = list_groups(input, &page.id, &rows, grouping == GroupBy::Host, &included);
        if collapsed {
            return;
        }
        let sizes = input.sizes;
        for (index, group) in page.groups.iter().enumerate() {
            let item = self.push(view_index, ItemKind::Band { group: index }, sizes.band);
            self.stop(
                Stop::Band {
                    view: page.id.clone(),
                    group: group.id.clone(),
                },
                item,
            );
            if group.collapsed {
                continue;
            }
            for &row in &group.members[..group.shown] {
                let DashboardRow::Object(key) = &rows[row] else {
                    continue;
                };
                let item = self.push(
                    view_index,
                    ItemKind::Row {
                        row,
                        group: Some(index),
                    },
                    sizes.row,
                );
                self.stop(
                    Stop::Row {
                        view: page.id.clone(),
                        group: Some(group.id.clone()),
                        key: key.clone(),
                    },
                    item,
                );
            }
            if group.pages {
                let item = self.push(view_index, ItemKind::More { group: index }, sizes.more);
                self.stop(
                    Stop::More {
                        view: page.id.clone(),
                        group: group.id.clone(),
                    },
                    item,
                );
            }
        }
    }

    /// A grid view's body: its lines as items, every host a stop.
    fn add_grid(
        &mut self,
        input: &PageInput<'_>,
        view: &View,
        view_index: usize,
        page: &mut ViewPage,
        grid: &Arc<Grid>,
        collapsed: bool,
    ) {
        let sizes = input.sizes;
        let on = input
            .filter
            .filter(|filter| filter.same_groups(view))
            .and_then(|filter| {
                grid.groups
                    .iter()
                    .position(|group| group.name == filter.name)
            });
        if let Some(on) = on {
            page.counts = grid.groups[on].counts;
        }
        let inner = (input.width - sizes.padding * 2.).max(px(0.));
        let cells = view.grid.cells == GridCells::LabelledCells;
        let (columns, per_line) = if cells {
            (1, fit(inner, sizes.cell_min, sizes.cell_gap).min(6))
        } else {
            // Three groups side by side at full width, two beside a pane.
            let columns = if inner >= px(840.) {
                3
            } else if inner >= px(420.) {
                2
            } else {
                1
            };
            #[expect(clippy::cast_precision_loss, reason = "a handful of columns")]
            let column = (inner - sizes.column_gap * (columns - 1) as f32) / columns as f32;
            (columns, fit(column, sizes.square, sizes.square_gap))
        };
        let mut layout = GridLayout {
            grid: grid.clone(),
            cells,
            columns,
            per_line,
            lines: Vec::new(),
            on,
        };
        if collapsed {
            page.grid = Some(layout);
            return;
        }
        let groups = &grid.groups;
        let heights = grid_lines(&mut layout, sizes);
        for (line, height) in heights.into_iter().enumerate() {
            let item = self.push(view_index, ItemKind::Grid { line }, height);
            let groups_here: Vec<(usize, Range<usize>)> = match &layout.lines[line] {
                GridLine::Groups(range) => range
                    .clone()
                    .map(|group| (group, 0..groups[group].cells.len()))
                    .collect(),
                GridLine::Header(_) => Vec::new(),
                GridLine::Cells { group, cells } => vec![(*group, cells.clone())],
            };
            for (group, cells) in groups_here {
                if layout.on.is_some_and(|on| on != group) {
                    // Dimmed by the page's filter: the cursor stays in
                    // the group the page is filtered to.
                    continue;
                }
                let group_id = Id::from(groups[group].name.as_str());
                for cell in cells {
                    self.stops.push(StopEntry {
                        stop: Stop::Cell {
                            view: page.id.clone(),
                            group: group_id.clone(),
                            host: groups[group].cells[cell].host.clone(),
                        },
                        item,
                        sub: cell,
                        group,
                    });
                }
            }
        }
        page.grid = Some(layout);
    }

    fn add_tiles(
        &mut self,
        input: &PageInput<'_>,
        view: &View,
        view_index: usize,
        page: &mut ViewPage,
        tiles: &Arc<Vec<Tile>>,
        collapsed: bool,
    ) {
        let sizes = input.sizes;
        let on = input
            .filter
            .filter(|filter| filter.same_groups(view))
            .and_then(|filter| tiles.iter().position(|tile| tile.name == filter.name));
        if let Some(on) = on {
            // The tile's unhandled counts, as its numbers show them.
            let mut counts = tiles[on].counts;
            counts.ok = 0;
            counts.pending = 0;
            page.counts = counts;
        }
        let inner = (input.width - sizes.padding * 2.).max(px(0.));
        let columns = fit(inner, sizes.tile_min, sizes.tile_gap).clamp(1, 4);
        page.tiles = Some(TilesLayout {
            tiles: tiles.clone(),
            columns,
            on,
        });
        if collapsed {
            return;
        }
        let lines = tiles.len().div_ceil(columns);
        for line in 0..lines {
            let top = if line == 0 { sizes.body_top } else { px(0.) };
            let bottom = if line + 1 == lines {
                sizes.tiles_bottom
            } else {
                sizes.tile_gap
            };
            self.push(
                view_index,
                ItemKind::Tiles { line },
                top + sizes.tile + bottom,
            );
        }
    }

    fn add_stream(
        &mut self,
        input: &PageInput<'_>,
        view: &View,
        view_index: usize,
        page: &mut ViewPage,
        events: &Arc<Vec<LogEntry>>,
        collapsed: bool,
    ) {
        let events: Vec<LogEntry> = events
            .iter()
            .filter(|event| {
                input
                    .filter
                    .is_none_or(|filter| filter.includes_object(input.snapshot, &event.object))
            })
            .cloned()
            .collect();
        let lines = usize::try_from(view.stream.lines)
            .unwrap_or(usize::MAX)
            .clamp(1, ic_core::snapshot::STREAM_EVENTS)
            .min(events.len());
        page.lines = lines;
        if events.is_empty() {
            // Filtered to nothing: the header says so.
            page.state = ViewState::Empty;
        }
        if collapsed || events.is_empty() {
            page.events = events;
            return;
        }
        #[expect(clippy::cast_precision_loss, reason = "at most 200 lines")]
        let height = input.sizes.event * lines as f32;
        let item = self.push(view_index, ItemKind::Stream, height);
        page.event_stops = self.stops.len();
        let mut seen: HashMap<(u64, ObjectKey, ic_core::LogKind), u32> = HashMap::new();
        for (index, event) in events.iter().enumerate() {
            let at = event.at.as_unix_seconds().to_bits();
            let nth = seen
                .entry((at, event.object.clone(), event.kind))
                .and_modify(|count| *count += 1)
                .or_insert(0);
            self.stops.push(StopEntry {
                stop: Stop::Event {
                    view: page.id.clone(),
                    event: EventKey {
                        at,
                        object: event.object.clone(),
                        kind: event.kind,
                        nth: *nth,
                    },
                },
                item,
                sub: index,
                group: 0,
            });
        }
        page.events = events;
    }
}

/// How many things `size` wide, `gap` apart, fit in `width` (at least
/// one).
/// Lays out `layout`'s lines (filling `layout.lines`) and returns each
/// line's height: groups of squares side by side, or each group's header
/// and its lines of labelled cells.
fn grid_lines(layout: &mut GridLayout, sizes: Sizes) -> Vec<Pixels> {
    let (cells, columns, per_line) = (layout.cells, layout.columns, layout.per_line);
    let grid = layout.grid.clone();
    let groups = &grid.groups;
    let lines_of = |count: usize| count.div_ceil(per_line.max(1));
    let mut heights = Vec::new();
    if cells {
        for (group, entry) in groups.iter().enumerate() {
            let gap = if group == 0 {
                sizes.body_top
            } else {
                sizes.section_gap
            };
            layout.lines.push(GridLine::Header(group));
            heights.push(gap + sizes.group_header + sizes.group_gap);
            let count = entry.cells.len();
            for line in 0..lines_of(count) {
                let start = line * per_line;
                layout.lines.push(GridLine::Cells {
                    group,
                    cells: start..(start + per_line).min(count),
                });
                let last = line + 1 == lines_of(count);
                heights.push(sizes.cell + if last { px(0.) } else { sizes.cell_gap });
            }
        }
    } else {
        for (line, start) in (0..groups.len()).step_by(columns).enumerate() {
            let range = start..(start + columns).min(groups.len());
            let tallest = range
                .clone()
                .map(|group| lines_of(groups[group].cells.len()))
                .max()
                .unwrap_or(0);
            #[expect(clippy::cast_precision_loss, reason = "a few hundred lines")]
            let squares = (sizes.square + sizes.square_gap) * tallest as f32 - sizes.square_gap;
            let block = sizes.group_header + sizes.group_gap + squares.max(px(0.));
            let top = if line == 0 {
                sizes.body_top
            } else {
                sizes.line_gap
            };
            layout.lines.push(GridLine::Groups(range));
            heights.push(top + block);
        }
    }
    if let Some(last) = heights.last_mut() {
        *last += if cells {
            sizes.body_top
        } else {
            sizes.body_bottom
        };
    }
    heights
}

fn fit(width: Pixels, size: Pixels, gap: Pixels) -> usize {
    let step = f32::from(size + gap);
    if step <= 0. {
        return 1;
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a non-negative count of a few hundred"
    )]
    let count = ((f32::from(width + gap) / step).floor().max(1.)) as usize;
    count
}

/// Applies the page's group filter to a list's `rows`: the header counts
/// what it leaves, `page.included` marks those rows (ctrl-a, marks), and a
/// list it leaves nothing becomes [`ViewState::Empty`] (`nothing to
/// show`). Returns whether any row is left.
fn filter_list(
    input: &PageInput<'_>,
    rows: &[DashboardRow],
    page: &mut ViewPage,
    included: &dyn Fn(&ObjectKey) -> bool,
) -> bool {
    let flags: Vec<bool> = rows
        .iter()
        .map(|row| matches!(row, DashboardRow::Object(key) if included(key)))
        .collect();
    page.counts = tally(
        rows.iter()
            .zip(&flags)
            .filter(|(_, included)| **included)
            .filter_map(|(row, _)| match row {
                DashboardRow::Object(key) => mark_of(input.snapshot, key),
                DashboardRow::Group { .. } => None,
            }),
        true,
    );
    let any = flags.contains(&true);
    page.included = Some(flags);
    if !any {
        page.state = ViewState::Empty;
    }
    any
}

/// A grouped list's groups from the core's rows: a header row, then its
/// object rows. Grouped by host, a group's rows are ordered and paged by
/// count; the page's group filter leaves out rows (and empty groups).
fn list_groups(
    input: &PageInput<'_>,
    view: &Id,
    rows: &[DashboardRow],
    by_host: bool,
    included: &dyn Fn(&ObjectKey) -> bool,
) -> Vec<ListGroup> {
    let snapshot = input.snapshot;
    let mut groups = Vec::new();
    let mut index = 0;
    while index < rows.len() {
        let (label, start, count) = match &rows[index] {
            DashboardRow::Group { label, count } => (label.clone(), index + 1, *count),
            // Rows before any header (not from the core): a group of
            // their own.
            DashboardRow::Object(_) => {
                let count = rows[index..]
                    .iter()
                    .take_while(|row| matches!(row, DashboardRow::Object(_)))
                    .count();
                (UNGROUPED.to_owned(), index, count)
            }
        };
        let end = (start + count).min(rows.len());
        index = end.max(index + 1);
        let mut members: Vec<usize> = (start..end)
            .filter(|&row| matches!(&rows[row], DashboardRow::Object(key) if included(key)))
            .collect();
        if members.is_empty() {
            continue;
        }
        let host = by_host
            .then(|| match &rows[members[0]] {
                DashboardRow::Object(key) => Some(key.host_name().clone()),
                DashboardRow::Group { .. } => None,
            })
            .flatten();
        let id: Id = match &host {
            Some(host) => Id::from(host.as_str()),
            None => Id::from(label.as_str()),
        };
        let marks: Vec<Option<(ObjectMark, u32, String)>> = members
            .iter()
            .map(|&row| match &rows[row] {
                DashboardRow::Object(key) => facts(snapshot, key),
                DashboardRow::Group { .. } => None,
            })
            .collect();
        let counts = tally(marks.iter().flatten().map(|(mark, _, _)| *mark), true);
        let collapsed = input.folds.group_collapsed(view, &id);
        let expanded = input.folds.expanded(view, &id);
        let (shown, pages) = if by_host {
            let mut order: Vec<(usize, u32, String)> = members
                .iter()
                .zip(&marks)
                .map(|(&row, facts)| match facts {
                    Some((_, severity, name)) => (row, *severity, name.clone()),
                    None => (row, 0, String::new()),
                })
                .collect();
            order.sort_by(|a, b| paging::service_order((a.1, &a.2), (b.1, &b.2)));
            let problems = marks
                .iter()
                .flatten()
                .filter(|(mark, _, _)| !is_ok(mark.state))
                .count();
            members = order.into_iter().map(|(row, _, _)| row).collect();
            let total = members.len();
            (
                paging::shown_count(total, problems, expanded),
                paging::pages(total, problems),
            )
        } else {
            (members.len(), false)
        };
        groups.push(ListGroup {
            id,
            label,
            host,
            members,
            shown,
            pages,
            expanded,
            collapsed,
            counts,
        });
    }
    groups
}

/// The label of rows without a group.
pub(crate) const UNGROUPED: &str = "ungrouped";

/// An object's mark, severity and display name, as the snapshot has it.
fn facts(snapshot: &Snapshot, key: &ObjectKey) -> Option<(ObjectMark, u32, String)> {
    match key {
        ObjectKey::Host { name } => {
            let host = snapshot.hosts.get(name)?;
            Some((
                ObjectMark::host(host),
                host.severity(),
                host.display_name.clone(),
            ))
        }
        ObjectKey::Service { key } => {
            let service = snapshot.services.get(key)?;
            let host = snapshot.host_of(key);
            Some((
                ObjectMark::service(service, host.map(AsRef::as_ref)),
                service.severity(),
                service.display_name.clone(),
            ))
        }
    }
}

/// An object's mark.
pub(crate) fn mark_of(snapshot: &Snapshot, key: &ObjectKey) -> Option<ObjectMark> {
    facts(snapshot, key).map(|(mark, _, _)| mark)
}

/// Whether `state` is OK or UP.
fn is_ok(state: CheckableState) -> bool {
    matches!(
        state,
        CheckableState::Service(ServiceState::Ok) | CheckableState::Host(HostState::Up)
    )
}

/// Counts marks by state: handled problems (hollow) are left out of the
/// state counts (they count as `handled`), so the counts are unhandled
/// ones; with `ok`, OK objects count too.
pub(crate) fn tally(marks: impl Iterator<Item = ObjectMark>, ok: bool) -> Summary {
    let mut summary = Summary::default();
    let mut worst: Option<(u8, CheckableState)> = None;
    for mark in marks {
        let problem = mark.state.is_problem();
        if problem && mark.hollow {
            summary.handled += 1;
            continue;
        }
        let counter = match mark.state {
            CheckableState::Host(HostState::Up) | CheckableState::Service(ServiceState::Ok) => {
                if !ok {
                    continue;
                }
                &mut summary.ok
            }
            CheckableState::Host(HostState::Down) => &mut summary.down,
            CheckableState::Host(HostState::Unreachable) => &mut summary.unreachable,
            CheckableState::Host(HostState::Pending)
            | CheckableState::Service(ServiceState::Pending) => &mut summary.pending,
            CheckableState::Service(ServiceState::Warning) => &mut summary.warning,
            CheckableState::Service(ServiceState::Critical) => &mut summary.critical,
            CheckableState::Service(ServiceState::Unknown) => &mut summary.unknown,
        };
        *counter += 1;
        if problem {
            summary.unhandled += 1;
            let rank = state_rank(mark.state);
            if worst.is_none_or(|(worst, _)| rank > worst) {
                worst = Some((rank, mark.state));
            }
        }
    }
    summary.worst_unhandled = worst.map(|(_, state)| state);
    summary
}

/// How alarming a problem state is, for a band's dot.
fn state_rank(state: CheckableState) -> u8 {
    match state {
        CheckableState::Service(ServiceState::Critical) => 6,
        CheckableState::Host(HostState::Down) => 5,
        CheckableState::Host(HostState::Unreachable) => 4,
        CheckableState::Service(ServiceState::Unknown) => 3,
        CheckableState::Service(ServiceState::Warning) => 2,
        CheckableState::Host(HostState::Pending)
        | CheckableState::Service(ServiceState::Pending) => 1,
        CheckableState::Host(HostState::Up) | CheckableState::Service(ServiceState::Ok) => 0,
    }
}

#[cfg(test)]
#[expect(
    clippy::disallowed_methods,
    reason = "test sizes are real pixels at 100 %"
)]
mod tests {
    use std::collections::BTreeMap;

    use gpui::px;
    use ic_config::{GridOptions, StreamOptions};
    use ic_core::snapshot::{GridCell, GridGroup, ViewBody, ViewResult};
    use ic_model::{Service, ServiceKey, Timestamp};

    use super::*;

    fn sizes() -> Sizes {
        Sizes::of(&Theme::dark())
    }

    fn service(host: &str, name: &str, state: ServiceState) -> Service {
        let mut service = Service::new(host, name);
        service.state = state;
        service
    }

    /// A snapshot with `hosts` (each with `count` services named `s00`…,
    /// the first `problems` critical).
    fn snapshot(hosts: &[(&str, usize, usize)]) -> Snapshot {
        let mut host_map = BTreeMap::new();
        let mut services: BTreeMap<ServiceKey, Arc<Service>> = BTreeMap::new();
        for (name, count, problems) in hosts {
            let mut host = Host::new(name);
            host.state = HostState::Up;
            host.groups = vec!["all".to_owned()];
            host_map.insert(host.name.clone(), Arc::new(host));
            for index in 0..*count {
                let state = if index < *problems {
                    ServiceState::Critical
                } else {
                    ServiceState::Ok
                };
                let service = service(name, &format!("s{index:02}"), state);
                services.insert(service.key.clone(), Arc::new(service));
            }
        }
        Snapshot {
            hosts: Arc::new(host_map),
            services: Arc::new(services),
            ..Snapshot::default()
        }
    }

    /// The core's grouped rows for every service of `snapshot`, by host,
    /// in name order (as `all services` grouped by host with sort by
    /// service).
    fn grouped_rows(snapshot: &Snapshot) -> Vec<DashboardRow> {
        let mut rows = Vec::new();
        for host in snapshot.hosts.keys() {
            let services: Vec<_> = snapshot.services_of(host).collect();
            rows.push(DashboardRow::Group {
                label: host.to_string(),
                count: services.len(),
            });
            rows.extend(
                services
                    .into_iter()
                    .map(|service| DashboardRow::Object(service.object_key())),
            );
        }
        rows
    }

    fn list_view(id: &str, grouped: bool) -> View {
        let mut view = View {
            id: id.to_owned(),
            name: id.to_owned(),
            problems_only: false,
            ..View::default()
        };
        if grouped {
            view.set_grouping(GroupBy::Host);
        }
        view
    }

    fn result(views: Vec<(&str, ViewBody)>) -> DashboardResult {
        DashboardResult {
            views: views
                .into_iter()
                .map(|(id, body)| ViewResult {
                    id: id.to_owned(),
                    body,
                    ..ViewResult::default()
                })
                .collect(),
            ..DashboardResult::default()
        }
    }

    fn build(
        views: &[View],
        result: &DashboardResult,
        snapshot: &Snapshot,
        folds: &Folds,
        multi: bool,
    ) -> Page {
        Page::build(&PageInput {
            views,
            result: Some(result),
            snapshot,
            folds,
            filter: None,
            multi,
            denied: [false, false],
            width: px(1140.),
            sizes: sizes(),
        })
    }

    fn kinds(page: &Page) -> Vec<String> {
        page.items
            .iter()
            .map(|item| match item.kind {
                ItemKind::Header => "header".to_owned(),
                ItemKind::Note => "note".to_owned(),
                ItemKind::Row { row, .. } => format!("row {row}"),
                ItemKind::Band { group } => format!("band {group}"),
                ItemKind::More { group } => format!("more {group}"),
                ItemKind::Grid { line } => format!("grid {line}"),
                ItemKind::Tiles { line } => format!("tiles {line}"),
                ItemKind::Stream => "stream".to_owned(),
            })
            .collect()
    }

    #[test]
    fn a_single_list_is_its_rows_at_the_row_height() {
        let snapshot = snapshot(&[("h1", 3, 3)]);
        let rows: Vec<DashboardRow> = snapshot
            .services
            .values()
            .map(|service| DashboardRow::Object(service.object_key()))
            .collect();
        let views = [list_view("v", false)];
        let result = result(vec![("v", ViewBody::List(Arc::new(rows)))]);
        let page = build(&views, &result, &snapshot, &Folds::default(), false);
        assert_eq!(kinds(&page), ["row 0", "row 1", "row 2"]);
        assert_eq!(page.height(), sizes().row * 3.);
        assert_eq!(page.item_at(sizes().row * 1.5), Some(1));
        assert_eq!(page.items_between(px(0.), sizes().row * 2.), 0..3);
        assert_eq!(page.stops().len(), 3);
    }

    #[test]
    fn hosts_page_by_count_problems_first() {
        // h1: 23 services, 2 problems (s00, s01); h2: 19 all OK; h3: 9
        // problems of 9.
        let snapshot = snapshot(&[("h1", 23, 2), ("h2", 19, 0), ("h3", 9, 9)]);
        let views = [list_view("v", true)];
        let rows = grouped_rows(&snapshot);
        let result = result(vec![("v", ViewBody::List(Arc::new(rows)))]);
        let mut folds = Folds::default();
        let page = build(&views, &result, &snapshot, &folds, false);
        let groups = &page.views[0].groups;
        assert_eq!(groups.len(), 3);
        assert_eq!(
            (groups[0].shown, groups[0].hidden(), groups[0].pages),
            (7, 16, true)
        );
        assert_eq!(
            (groups[1].shown, groups[1].hidden()),
            (7, 12),
            "an all-OK host: 7"
        );
        assert_eq!(
            (groups[2].shown, groups[2].pages),
            (9, false),
            "problems never hide"
        );
        assert_eq!(groups[0].counts.critical, 2);
        assert_eq!(groups[0].counts.ok, 21);
        // Band, 7 rows, the paging row, for each of h1 and h2; band and 9
        // rows for h3.
        assert_eq!(page.items.len(), 9 + 9 + 10);
        assert_eq!(
            page.height(),
            sizes().band * 3. + sizes().row * 23. + sizes().more * 2.
        );
        // `+ 16 more` shows the whole host in place.
        folds.set_expanded(&Id::from("v"), &Id::from("h1"), true);
        let page = build(&views, &result, &snapshot, &folds, false);
        let h1 = &page.views[0].groups[0];
        assert_eq!(
            (h1.shown, h1.hidden(), h1.pages, h1.expanded),
            (23, 0, true, true)
        );
        // Collapsed: the band alone, its counts kept.
        folds.set_group(&Id::from("v"), &Id::from("h1"), true);
        let page = build(&views, &result, &snapshot, &folds, false);
        assert_eq!(kinds(&page)[..2], ["band 0", "band 1"]);
        assert_eq!(page.views[0].groups[0].counts.critical, 2);
        assert_eq!(
            page.views[0].objects().len(),
            51,
            "collapsing hides nothing from marks"
        );
    }

    #[test]
    fn problems_come_first_worst_first() {
        let mut snapshot = snapshot(&[("h1", 9, 0)]);
        let mut services = (*snapshot.services).clone();
        for (name, state) in [
            ("s05", ServiceState::Warning),
            ("s08", ServiceState::Critical),
        ] {
            let key = ServiceKey::new("h1", name);
            services.insert(key, Arc::new(service("h1", name, state)));
        }
        snapshot.services = Arc::new(services);
        let views = [list_view("v", true)];
        let rows = grouped_rows(&snapshot);
        let result = result(vec![("v", ViewBody::List(Arc::new(rows.clone())))]);
        let page = build(&views, &result, &snapshot, &Folds::default(), false);
        let names: Vec<String> = page.views[0].groups[0].members[..7]
            .iter()
            .map(|&row| match &rows[row] {
                DashboardRow::Object(key) => key.as_service().unwrap().name.to_string(),
                DashboardRow::Group { .. } => String::new(),
            })
            .collect();
        assert_eq!(names, ["s08", "s05", "s00", "s01", "s02", "s03", "s04"]);
    }

    #[test]
    fn views_get_headers_and_collapse_to_them() {
        let snapshot = snapshot(&[("h1", 3, 3)]);
        let rows: Vec<DashboardRow> = snapshot
            .services
            .values()
            .map(|service| DashboardRow::Object(service.object_key()))
            .collect();
        let views = [
            list_view("a", false),
            list_view("b", false),
            list_view("c", false),
        ];
        let result = result(vec![
            ("a", ViewBody::List(Arc::new(rows.clone()))),
            ("b", ViewBody::List(Arc::default())),
            ("c", ViewBody::List(Arc::new(rows))),
        ]);
        let mut folds = Folds::default();
        let page = build(&views, &result, &snapshot, &folds, true);
        assert_eq!(
            kinds(&page),
            [
                "header", "row 0", "row 1", "row 2", "header", "header", "row 0", "row 1", "row 2"
            ]
        );
        assert_eq!(
            page.views[1].state,
            ViewState::Empty,
            "nothing to show: its header only"
        );
        // j skips headers; Tab-like moves find a view's first row.
        assert_eq!(
            page.step(3, true),
            Some(6),
            "from a's last row to c's first"
        );
        assert_eq!(page.first_body_stop(2), Some(6));
        assert_eq!(page.first_body_stop(1), None);
        folds.set_view(&views[0], true);
        let page = build(&views, &result, &snapshot, &folds, true);
        assert_eq!(kinds(&page)[..3], ["header", "header", "header"]);
        assert_eq!(page.step(0, true), Some(3), "a collapsed view is skipped");
    }

    #[test]
    fn grids_lay_out_groups_side_by_side_and_move_by_lines() {
        let snapshot = Snapshot::default();
        let cell = |name: &str| GridCell {
            host: HostName::new(name),
            state: CheckableState::Host(HostState::Up),
            handled: false,
            problems: 0,
            worst_service: None,
        };
        let group = |name: &str, count: usize| GridGroup {
            name: name.to_owned(),
            label: name.to_owned(),
            cells: (0..count)
                .map(|index| cell(&format!("{name}-{index:02}")))
                .collect(),
            counts: Summary::default(),
        };
        let grid = Grid {
            groups: vec![group("a", 48), group("b", 6), group("c", 12), group("d", 2)],
            hosts: 68,
        };
        let views = [View {
            id: "g".to_owned(),
            display: ViewDisplay::HostGroupGrid,
            grid: GridOptions::default(),
            ..View::default()
        }];
        let result = result(vec![("g", ViewBody::Grid(Arc::new(grid)))]);
        let page = build(&views, &result, &snapshot, &Folds::default(), true);
        let layout = page.views[0].grid.as_ref().unwrap();
        assert_eq!(layout.columns, 3);
        // (1140 - 36 - 64) / 3 = 346.7px a group: 23 squares a line.
        assert_eq!(layout.per_line, 23);
        assert_eq!(kinds(&page), ["header", "grid 0", "grid 1"]);
        // Line 0: a (3 lines of squares) beside b and c.
        let s = sizes();
        assert_eq!(
            page.item_height(1),
            s.body_top + s.group_header + s.group_gap + (s.square + s.square_gap) * 3.
                - s.square_gap
        );
        // j from a's first host: a line down; from a's last line: the
        // group below in the same column (d), then out.
        let first = page.position(&Stop::Cell {
            view: Id::from("g"),
            group: Id::from("a"),
            host: HostName::new("a-00"),
        });
        let first = first.unwrap();
        let down = page.step(first, true).unwrap();
        assert_eq!(page.stops()[down].sub, 23);
        let bottom = page.step(page.step(down, true).unwrap(), true).unwrap();
        assert!(
            matches!(&page.stops()[bottom].stop, Stop::Cell { group, .. } if &**group == "d"),
            "{:?}",
            page.stops()[bottom]
        );
        assert_eq!(page.step(bottom, true), None, "nothing after the grid");
        // → moves along, ← back.
        assert_eq!(page.grid_side_step(first, true), Some(first + 1));
        assert_eq!(
            page.grid_side_step(first, false),
            Some(first),
            "stays on the grid"
        );
    }

    #[test]
    fn a_stream_shows_its_lines_and_stops_at_each_event() {
        let snapshot = Snapshot::default();
        let event = |seconds: f64| LogEntry {
            at: Timestamp::from_unix_seconds(seconds),
            object: ObjectKey::host("h"),
            kind: ic_core::LogKind::CommentAdded,
            text: String::new(),
            author: None,
        };
        let events: Vec<LogEntry> = (0..12).map(|index| event(f64::from(index))).collect();
        let views = [View {
            id: "s".to_owned(),
            display: ViewDisplay::EventStream,
            stream: StreamOptions {
                lines: 8,
                ..StreamOptions::default()
            },
            ..View::default()
        }];
        let result = result(vec![("s", ViewBody::Stream(Arc::new(events)))]);
        let page = build(&views, &result, &snapshot, &Folds::default(), true);
        assert_eq!(kinds(&page), ["header", "stream"]);
        assert_eq!(page.item_height(1), sizes().event * 8.);
        assert_eq!(page.stops().len(), 1 + 12);
        assert_eq!(page.views[0].lines, 8);
    }

    #[test]
    fn a_group_filter_keeps_its_hosts_and_recounts() {
        let mut snapshot = snapshot(&[("h1", 2, 2), ("h2", 2, 1)]);
        let mut hosts = (*snapshot.hosts).clone();
        let mut h2 = (*hosts[&HostName::new("h2")]).clone();
        h2.groups = vec!["edge".to_owned()];
        hosts.insert(h2.name.clone(), Arc::new(h2));
        snapshot.hosts = Arc::new(hosts);
        let rows: Vec<DashboardRow> = snapshot
            .services
            .values()
            .map(|service| DashboardRow::Object(service.object_key()))
            .collect();
        let views = [list_view("v", false)];
        let result = result(vec![("v", ViewBody::List(Arc::new(rows)))]);
        let filter = GroupFilter {
            by: GroupSource::HostGroup,
            var: String::new(),
            name: "edge".to_owned(),
            label: "edge".to_owned(),
        };
        let page = Page::build(&PageInput {
            views: &views,
            result: Some(&result),
            snapshot: &snapshot,
            folds: &Folds::default(),
            filter: Some(&filter),
            multi: true,
            denied: [false, false],
            width: px(1140.),
            sizes: sizes(),
        });
        assert_eq!(kinds(&page), ["header", "row 2", "row 3"]);
        assert_eq!(page.views[0].counts.critical, 1);
        assert_eq!(filter.chip(), "host group edge");
    }

    fn edge_filter() -> GroupFilter {
        GroupFilter {
            by: GroupSource::HostGroup,
            var: String::new(),
            name: "edge".to_owned(),
            label: "edge".to_owned(),
        }
    }

    fn build_filtered(
        views: &[View],
        result: &DashboardResult,
        snapshot: &Snapshot,
        filter: Option<&GroupFilter>,
        denied: [bool; 2],
    ) -> Page {
        Page::build(&PageInput {
            views,
            result: Some(result),
            snapshot,
            folds: &Folds::default(),
            filter,
            multi: true,
            denied,
            width: px(1140.),
            sizes: sizes(),
        })
    }

    #[test]
    fn a_group_filter_limits_marks_and_says_when_it_leaves_nothing() {
        // h1 (group all) and h2 (group edge), two services each.
        let mut snapshot = snapshot(&[("h1", 2, 2), ("h2", 2, 1)]);
        let mut hosts = (*snapshot.hosts).clone();
        let mut h2 = (*hosts[&HostName::new("h2")]).clone();
        h2.groups = vec!["edge".to_owned()];
        hosts.insert(h2.name.clone(), Arc::new(h2));
        snapshot.hosts = Arc::new(hosts);
        let all: Vec<DashboardRow> = snapshot
            .services
            .values()
            .map(|service| DashboardRow::Object(service.object_key()))
            .collect();
        let h1_only: Vec<DashboardRow> = all[..2].to_vec();
        let views = [list_view("all", false), list_view("h1", false)];
        let result = result(vec![
            ("all", ViewBody::List(Arc::new(all))),
            ("h1", ViewBody::List(Arc::new(h1_only))),
        ]);
        let filter = edge_filter();
        let page = build_filtered(&views, &result, &snapshot, Some(&filter), [false; 2]);
        // ctrl-a marks only what the filter leaves.
        let marked: Vec<String> = page.views[0]
            .objects()
            .iter()
            .map(ObjectKey::full_name)
            .collect();
        assert_eq!(marked, ["h2!s00", "h2!s01"]);
        // A view the filter leaves nothing: its header says so, no body.
        assert_eq!(page.views[1].state, ViewState::Empty);
        assert!(page.views[1].objects().is_empty());
        assert_eq!(
            kinds(&page),
            ["header", "row 2", "row 3", "header"],
            "the emptied view is its header"
        );
        // Without the filter every row counts again.
        let page = build_filtered(&views, &result, &snapshot, None, [false; 2]);
        assert_eq!(page.views[0].objects().len(), 4);
        assert_eq!(page.views[1].state, ViewState::Ready);
    }

    #[test]
    fn hosts_grouped_by_host_are_a_plain_list() {
        let snapshot = snapshot(&[("h1", 0, 0), ("h2", 0, 0)]);
        let rows: Vec<DashboardRow> = snapshot
            .hosts
            .keys()
            .flat_map(|host| {
                [
                    DashboardRow::Group {
                        label: host.to_string(),
                        count: 1,
                    },
                    DashboardRow::Object(ObjectKey::Host { name: host.clone() }),
                ]
            })
            .collect();
        let mut view = list_view("v", true);
        view.object_kind = ObjectKind::Hosts;
        let views = [view];
        let result = result(vec![("v", ViewBody::List(Arc::new(rows)))]);
        let page = build(&views, &result, &snapshot, &Folds::default(), true);
        // No band repeats each host: one row per host.
        assert_eq!(kinds(&page), ["header", "row 1", "row 3"]);
        assert!(page.views[0].groups.is_empty());
    }

    #[test]
    fn views_are_denied_by_what_they_read() {
        let snapshot = Snapshot::default();
        let grid = View {
            id: "g".to_owned(),
            display: ViewDisplay::HostGroupGrid,
            // What a grid made by an earlier editor kept.
            object_kind: ObjectKind::Services,
            ..View::default()
        };
        let stream = View {
            id: "s".to_owned(),
            display: ViewDisplay::EventStream,
            ..View::default()
        };
        let views = [grid, stream, list_view("l", false)];
        let result = result(vec![
            ("g", ViewBody::Grid(Arc::default())),
            ("s", ViewBody::Stream(Arc::default())),
            ("l", ViewBody::List(Arc::default())),
        ]);
        // No service permission: the grid still reads its hosts.
        let page = build_filtered(&views, &result, &snapshot, None, [true, false]);
        let states: Vec<ViewState> = page.views.iter().map(|view| view.state).collect();
        assert_eq!(
            states,
            [ViewState::Empty, ViewState::Empty, ViewState::Denied]
        );
        // No host permission: the grid can't show anything; the stream
        // reads the local log.
        let page = build_filtered(&views, &result, &snapshot, None, [false, true]);
        let states: Vec<ViewState> = page.views.iter().map(|view| view.state).collect();
        assert_eq!(
            states,
            [ViewState::Denied, ViewState::Empty, ViewState::Empty]
        );
    }

    #[test]
    fn a_filtered_grid_keeps_the_cursor_in_its_group() {
        let snapshot = Snapshot::default();
        let cell = |name: &str| GridCell {
            host: HostName::new(name),
            state: CheckableState::Host(HostState::Up),
            handled: false,
            problems: 0,
            worst_service: None,
        };
        let group = |name: &str| GridGroup {
            name: name.to_owned(),
            label: name.to_owned(),
            cells: (0..3)
                .map(|index| cell(&format!("{name}-{index}")))
                .collect(),
            counts: Summary::default(),
        };
        let grid = Grid {
            groups: vec![group("core"), group("edge"), group("web")],
            hosts: 9,
        };
        let views = [
            View {
                id: "g".to_owned(),
                display: ViewDisplay::HostGroupGrid,
                ..View::default()
            },
            list_view("l", false),
        ];
        let result = result(vec![
            ("g", ViewBody::Grid(Arc::new(grid))),
            ("l", ViewBody::List(Arc::default())),
        ]);
        let filter = edge_filter();
        let page = build_filtered(&views, &result, &snapshot, Some(&filter), [false; 2]);
        let cells: Vec<&str> = page
            .stops()
            .iter()
            .filter_map(|entry| match &entry.stop {
                Stop::Cell { host, .. } => Some(host.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            cells,
            ["edge-0", "edge-1", "edge-2"],
            "the dimmed groups have none"
        );
        // Tab's target is the filtered group's first host.
        let first = page.first_body_stop(0).unwrap();
        assert!(
            matches!(&page.stops()[first].stop, Stop::Cell { host, .. } if host.as_str() == "edge-0")
        );
    }

    #[test]
    fn a_tile_filter_counts_the_tile_unhandled() {
        let snapshot = Snapshot::default();
        let tile = |name: &str, critical: u32, handled: u32| Tile {
            name: name.to_owned(),
            label: name.to_owned(),
            hosts: 1,
            summary: Summary {
                critical: critical + handled,
                handled,
                ok: 5,
                ..Summary::default()
            },
            counts: Summary {
                critical,
                ok: 5,
                ..Summary::default()
            },
        };
        let views = [View {
            id: "t".to_owned(),
            display: ViewDisplay::SummaryTiles,
            ..View::default()
        }];
        let result = result(vec![(
            "t",
            ViewBody::Tiles(Arc::new(vec![tile("edge", 1, 1), tile("web", 0, 0)])),
        )]);
        let filter = edge_filter();
        let page = build_filtered(&views, &result, &snapshot, Some(&filter), [false; 2]);
        let counts = page.views[0].counts;
        assert_eq!(
            (counts.critical, counts.ok),
            (1, 0),
            "the handled one is left out"
        );
    }

    #[test]
    fn tallies_leave_handled_problems_out() {
        let mark = |state, hollow| ObjectMark { state, hollow };
        let critical = CheckableState::Service(ServiceState::Critical);
        let warning = CheckableState::Service(ServiceState::Warning);
        let ok = CheckableState::Service(ServiceState::Ok);
        let summary = tally(
            [
                mark(critical, true),
                mark(warning, false),
                mark(ok, true),
                mark(ok, false),
            ]
            .into_iter(),
            true,
        );
        assert_eq!((summary.critical, summary.warning, summary.ok), (0, 1, 2));
        assert_eq!((summary.handled, summary.unhandled), (1, 1));
        assert_eq!(summary.worst_unhandled, Some(warning));
    }
}
