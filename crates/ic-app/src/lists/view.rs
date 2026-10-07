//! The handling and downtimes views of topic 14, as tabs in the sidebar's
//! *open* section. Laid out like a dashboard: the header (`handling
//! prod-cluster · the whole environment · 20 objects … only mine  latest
//! activity ↓  ···`; the downtimes view adds `timeline | list`), the chips
//! that filter it, the lines (only those on screen are built, so
//! thousands cost the same as ten), the selection bar while entries are
//! marked, and the cursor's object in the pane beside it.
//!
//! **One grouping pattern**: each object is a slim band; its chevron only
//! folds it, a click elsewhere on the band opens the object's pane. A
//! host's downtime with all its services folds them into one row (closed
//! by default; its chevron or `→` opens it, paged by count). Selection,
//! the selection bar and the keys work as in every list (`j` / `k`, `x`,
//! shift-click, `ctrl-a`, `esc`); `⌫` removes the marked entries (or the
//! cursor's), always through a confirmation that lists every target. The
//! lines follow the engine's snapshot, so another operator's downtime or
//! an acknowledgement that expired shows at once; nothing here asks
//! Icinga for anything.

use std::collections::BTreeMap;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    Action, AnyElement, App, AppContext as _, BoxShadow, ClickEvent, ClipboardItem, Context,
    ElementId, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement as _, IntoElement,
    KeyBinding, Modifiers, MouseButton, ParentElement as _, Pixels, Render, ScrollHandle,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, point,
    prelude::FluentBuilder as _,
};
use ic_model::{CheckableState, ObjectKey, Timestamp};
use ic_ui_kit::{
    ActiveTheme as _, Button, Chip as ChipButton, EmptyState, GlyphButton, Icon, IconName, KeyHint,
    Link, Menu, MenuItem, Metrics, ObjectMark, PaneHeader, Popover, RowEmphasis, Scrollbar,
    Segmented, StateDot, Switch, Theme, Tooltip, px,
};

use super::draw::{self, Look, Sizes};
use super::model::{Chip, ListKind, Mode, Options};
use super::removal::{self, BulkRemoval};
use super::threads::{
    self, Covers, EntryKey, EntryKind, Folds, ItemKey, Keyed, Line, Listing, MoreKey,
};
use super::words::{self, Axis};
use crate::actions::{
    Acknowledge, ActionRequest, AddComment, CheckNow, Dismiss, ExtendSelectionNext,
    ExtendSelectionPrevious, MarkAll, ObjectAction, OpenAsTab, OpenSelected, ScheduleDowntime,
    SelectFirst, SelectLast, SelectNext, SelectPageDown, SelectPageUp, SelectPrevious, ToggleMark,
};
use crate::app_state::AppState;
use crate::banner;
use crate::chrome::{Controls, WindowDrag};
use crate::dashboard::selection::{ListSelection, SelectableRow as _};
use crate::dashboard::{HeaderMenu, HeaderMenus, SplitLayout};
use crate::menu_state::down_position;
use crate::operate::expression;
use crate::pane::{ObjectPane, PaneEvent, PaneMode};
use crate::workspace::sidebar_reopen;

/// Key context of a view (with the dashboard list's, whose keys it
/// shares).
pub(crate) const LIST_CONTEXT: &str = "RecordList";

/// Where the views' own keys apply: not in a text field inside them (the
/// pane's comment field).
const LIST_KEYS: &str = "RecordList && !Input";

/// The selection bar's height (as the dashboard's).
const SELECTION_BAR_HEIGHT: f32 = 40.;
/// The selection count's slot, in characters (`999 selected`).
const COUNT_SLOT_CHARS: f32 = 12.;
/// The band's right slot at most, in characters.
const BAND_SLOT_CHARS: f32 = 30.;
/// Below this width the header leaves *only mine* to the `···` menu.
const ROOMY_HEADER: f32 = 720.;

/// Removes the marked entries, or the cursor's (`⌫`): asks first, listing
/// every target.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct RemoveSelected;

/// Opens what the cursor is on (`→`): a folded object, a host's folded
/// services, the rest of a thread.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct Unfold;

/// Folds what the cursor is on (`←`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct Fold;

/// Turns *only mine* on or off (`m`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct ToggleOnlyMine;

/// Sorts by the next order the view offers (`s`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct NextSort;

/// Registers the views' own keys; the dashboard list's keys apply too.
pub(crate) fn bind_keys(cx: &mut App) {
    let list = Some(LIST_KEYS);
    cx.bind_keys([
        KeyBinding::new("m", ToggleOnlyMine, list),
        KeyBinding::new("s", NextSort, list),
        KeyBinding::new("backspace", RemoveSelected, list),
        KeyBinding::new("delete", RemoveSelected, list),
        KeyBinding::new("right", Unfold, list),
        KeyBinding::new("left", Fold, list),
    ]);
}

/// What the view asks the workspace to do.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum RecordListEvent {
    /// Ask whether to remove these, listing every target.
    Remove(BulkRemoval),
}

/// The pane open beside the view.
struct OpenPane {
    view: Entity<ObjectPane>,
    _events: Subscription,
}

/// What a listing was built from: it's built again when any of it changes.
/// The snapshot's revision, plus the maps it read, held (not their
/// addresses: a freed map's address can come back with other contents).
#[derive(Clone, Debug)]
struct Inputs {
    environment: Option<String>,
    revision: u64,
    downtimes: Arc<BTreeMap<ObjectKey, Vec<ic_model::Downtime>>>,
    comments: Arc<BTreeMap<ObjectKey, Vec<ic_model::Comment>>>,
    hosts: Arc<BTreeMap<ic_model::HostName, Arc<ic_model::Host>>>,
    services: Arc<BTreeMap<ic_model::ServiceKey, Arc<ic_model::Service>>>,
    options: Options,
    folds: Folds,
    author: String,
    /// The minute: phases and times left change with the clock.
    minute: i64,
}

impl PartialEq for Inputs {
    fn eq(&self, other: &Self) -> bool {
        self.environment == other.environment
            && self.revision == other.revision
            && Arc::ptr_eq(&self.downtimes, &other.downtimes)
            && Arc::ptr_eq(&self.comments, &other.comments)
            && Arc::ptr_eq(&self.hosts, &other.hosts)
            && Arc::ptr_eq(&self.services, &other.services)
            && self.options == other.options
            && self.folds == other.folds
            && self.author == other.author
            && self.minute == other.minute
    }
}

/// Where each line starts, for the sizes and display it was laid out with.
#[derive(Clone, Debug)]
struct Layout {
    lines: Arc<Vec<Keyed>>,
    sizes: Sizes,
    mode: Mode,
    /// Where each line starts, and (last) where the view ends.
    tops: Rc<Vec<Pixels>>,
}

impl Layout {
    fn top(&self, index: usize) -> Pixels {
        self.tops
            .get(index)
            .copied()
            .unwrap_or_else(|| self.height())
    }

    fn height(&self) -> Pixels {
        self.tops.last().copied().unwrap_or_default()
    }

    /// The line at `y` (the last one past the end).
    fn line_at(&self, y: Pixels) -> Option<usize> {
        let count = self.tops.len().checked_sub(1)?;
        if count == 0 {
            return None;
        }
        let after = self.tops[1..].partition_point(|bottom| *bottom <= y);
        Some(after.min(count - 1))
    }
}

/// The handling view or the downtimes view.
pub(crate) struct RecordList {
    state: Entity<AppState>,
    kind: ListKind,
    focus_handle: FocusHandle,
    folds: Folds,
    built: Option<(Inputs, Listing)>,
    layout: Option<Layout>,
    selection: ListSelection<Keyed>,
    scroll: ScrollHandle,
    pane: Option<OpenPane>,
    menus: HeaderMenus,
    sidebar_open: bool,
    drag: WindowDrag,
    /// The lines built in the last frame.
    visible: Range<usize>,
    /// Where the selection bar's buttons start, as last drawn, for tests.
    #[cfg(test)]
    pub(crate) selection_buttons_x: Rc<std::cell::Cell<Option<Pixels>>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<RecordListEvent> for RecordList {}

impl Focusable for RecordList {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl RecordList {
    pub(crate) fn new(state: Entity<AppState>, kind: ListKind, cx: &mut Context<Self>) -> Self {
        let subscriptions = vec![cx.observe(&state, |_, _, cx| cx.notify())];
        Self {
            state,
            kind,
            focus_handle: cx.focus_handle(),
            folds: Folds::default(),
            built: None,
            layout: None,
            selection: ListSelection::default(),
            scroll: ScrollHandle::new(),
            pane: None,
            menus: HeaderMenus::default(),
            sidebar_open: true,
            drag: WindowDrag::default(),
            visible: 0..0,
            #[cfg(test)]
            selection_buttons_x: Rc::default(),
            _subscriptions: subscriptions,
        }
    }

    /// Tells the view whether the sidebar is shown (the header then needs
    /// no window controls).
    pub(crate) fn set_sidebar_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.sidebar_open != open {
            self.sidebar_open = open;
            if let Some(pane) = &self.pane {
                pane.view
                    .update(cx, |pane, cx| pane.set_sidebar_open(open, cx));
            }
            cx.notify();
        }
    }

    /// The choices as kept with the environment (*only mine* off where it
    /// can't apply).
    fn options(&self, cx: &App) -> Options {
        let state = self.state.read(cx);
        let mut options = Options::saved(self.kind, &state.list_options(self.kind));
        if state.only_mine_denial(self.kind).is_some() {
            options.only_mine = false;
        }
        options
    }

    /// The display: the downtimes view's choice, handling's list.
    fn mode(&self, options: &Options) -> Mode {
        match self.kind {
            ListKind::Handling => Mode::List,
            ListKind::Downtimes => options.mode,
        }
    }

    /// Changes the choices and keeps them with the environment's UI state
    /// (they come back when the view opens again, after a restart too).
    fn set_options(&mut self, change: impl FnOnce(&mut Options), cx: &mut Context<Self>) {
        let mut options = Options::saved(self.kind, &self.state.read(cx).list_options(self.kind));
        change(&mut options);
        self.menus.close();
        let (kind, saved) = (self.kind, options.to_saved(self.kind));
        self.state
            .update(cx, |state, _| state.set_list_options(kind, saved));
        cx.notify();
    }

    /// Builds the listing again if what it shows changed, and moves the
    /// selection onto its lines. Cheap when nothing changed.
    fn sync(&mut self, cx: &App) {
        let options = self.options(cx);
        let state = self.state.read(cx);
        let snapshot = state.snapshot();
        let now = Timestamp::now();
        #[expect(
            clippy::cast_possible_truncation,
            reason = "minutes since 1970 fit in i64"
        )]
        let minute = (now.as_unix_seconds() / 60.).floor() as i64;
        let inputs = Inputs {
            environment: state.active_environment_id().map(str::to_owned),
            revision: snapshot.revision,
            downtimes: Arc::clone(&snapshot.downtimes),
            comments: Arc::clone(&snapshot.comments),
            hosts: Arc::clone(&snapshot.hosts),
            services: Arc::clone(&snapshot.services),
            options: options.clone(),
            folds: self.folds.clone(),
            author: state.author().to_owned(),
            minute,
        };
        if self
            .built
            .as_ref()
            .is_none_or(|(built, _)| *built != inputs)
        {
            let listing = threads::build(
                self.kind,
                snapshot,
                &inputs.author,
                &options,
                &self.folds,
                now,
            );
            self.selection.update_rows(&listing.lines);
            self.built = Some((inputs, listing));
        }
        // The heights follow the theme (density, interface size).
        let sizes = Sizes::of(cx.theme());
        let mode = self.mode(&options);
        let Some((_, listing)) = &self.built else {
            return;
        };
        let current = self.layout.as_ref().is_some_and(|layout| {
            Arc::ptr_eq(&layout.lines, &listing.lines)
                && layout.sizes == sizes
                && layout.mode == mode
        });
        if !current {
            let mut tops = Vec::with_capacity(listing.lines.len() + 1);
            let mut y = px(0.);
            tops.push(y);
            for keyed in listing.lines.iter() {
                y += sizes.height(&keyed.line, mode);
                tops.push(y);
            }
            self.layout = Some(Layout {
                lines: Arc::clone(&listing.lines),
                sizes,
                mode,
                tops: Rc::new(tops),
            });
        }
    }

    /// The listing, built for the current snapshot.
    fn listing(&self) -> Option<&Listing> {
        self.built.as_ref().map(|(_, listing)| listing)
    }

    /// The options the listing was built with.
    fn shown_options(&self) -> Options {
        self.built
            .as_ref()
            .map(|(inputs, _)| inputs.options.clone())
            .unwrap_or_default()
    }

    // --- Selection and keys --------------------------------------------

    /// Runs `change` on the selection; scrolls to the cursor and, if the
    /// pane is open, shows the cursor's object in it.
    fn change_selection(
        &mut self,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut ListSelection<Keyed>) -> Option<usize>,
    ) {
        self.sync(cx);
        if let Some(index) = change(&mut self.selection) {
            self.reveal(index);
            if let (Some(pane), Some(object)) = (&self.pane, self.cursor_object()) {
                pane.view.update(cx, |pane, cx| pane.show(object, cx));
            }
        }
        cx.notify();
    }

    /// Scrolls so line `index` shows, clear of a band stuck to the top.
    fn reveal(&self, index: usize) {
        let Some(layout) = &self.layout else {
            return;
        };
        let viewport = self.scroll.bounds().size.height;
        if viewport <= px(0.) {
            return;
        }
        let top = layout.top(index);
        let bottom = layout.top(index + 1);
        let sticky = if self.band_of(index).is_some_and(|band| band != index) {
            layout.sizes.band
        } else {
            px(0.)
        };
        let offset = -self.scroll.offset().y;
        let target = if top - sticky < offset {
            (top - sticky).max(px(0.))
        } else if bottom > offset + viewport {
            bottom - viewport
        } else {
            return;
        };
        let max = (layout.height() - viewport).max(px(0.));
        self.scroll.set_offset(point(px(0.), -target.min(max)));
    }

    fn page_size(&self) -> isize {
        isize::try_from(self.visible.len().saturating_sub(1).max(1)).unwrap_or(1)
    }

    fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        self.change_selection(cx, |selection| selection.move_by(1));
    }

    fn select_previous(&mut self, _: &SelectPrevious, _: &mut Window, cx: &mut Context<Self>) {
        self.change_selection(cx, |selection| selection.move_by(-1));
    }

    fn extend_next(&mut self, _: &ExtendSelectionNext, _: &mut Window, cx: &mut Context<Self>) {
        self.change_selection(cx, |selection| selection.extend_by(1));
    }

    fn extend_previous(
        &mut self,
        _: &ExtendSelectionPrevious,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.change_selection(cx, |selection| selection.extend_by(-1));
    }

    fn select_first(&mut self, _: &SelectFirst, _: &mut Window, cx: &mut Context<Self>) {
        self.change_selection(cx, |selection| selection.move_to_end(false));
    }

    fn select_last(&mut self, _: &SelectLast, _: &mut Window, cx: &mut Context<Self>) {
        self.change_selection(cx, |selection| selection.move_to_end(true));
    }

    fn select_page_down(&mut self, _: &SelectPageDown, _: &mut Window, cx: &mut Context<Self>) {
        let page = self.page_size();
        self.change_selection(cx, |selection| selection.move_page(page));
    }

    fn select_page_up(&mut self, _: &SelectPageUp, _: &mut Window, cx: &mut Context<Self>) {
        let page = self.page_size();
        self.change_selection(cx, |selection| selection.move_page(-page));
    }

    /// `x`: marks the cursor's entry; on a band, every entry under it.
    fn toggle_mark(&mut self, _: &ToggleMark, _: &mut Window, cx: &mut Context<Self>) {
        self.sync(cx);
        if let Some(index) = self.selection.cursor()
            && matches!(self.line(index), Some(Line::Band { .. }))
        {
            let keys = self.group_entries(index);
            if self.selection.toggle_marks(&keys) {
                cx.notify();
            }
            return;
        }
        self.change_selection(cx, |selection| {
            selection
                .toggle_mark_at_cursor()
                .then(|| selection.cursor())
                .flatten()
        });
    }

    fn mark_all(&mut self, _: &MarkAll, _: &mut Window, cx: &mut Context<Self>) {
        self.change_selection(cx, |selection| {
            selection.mark_all();
            None
        });
    }

    /// Enter: the cursor's object in the pane.
    fn open_selected(&mut self, _: &OpenSelected, _: &mut Window, cx: &mut Context<Self>) {
        self.sync(cx);
        if self.selection.cursor().is_none() {
            self.selection.move_by(1);
        }
        match self.selection.cursor().and_then(|index| self.line(index)) {
            Some(Line::Fold { downtime, .. }) => {
                let name = downtime.clone();
                self.toggle_fold(&name, cx);
            }
            Some(Line::More { key, .. }) => {
                let key = key.clone();
                self.toggle_more(&key, cx);
            }
            _ => {
                if let Some(object) = self.cursor_object() {
                    self.open_pane(object, cx);
                }
            }
        }
    }

    /// Escape: closes a menu, else the pane, else clears the marks.
    fn dismiss(&mut self, _: &Dismiss, _: &mut Window, cx: &mut Context<Self>) {
        if self.menus.close() {
            cx.notify();
            return;
        }
        if self.close_pane(cx) {
            return;
        }
        if self.selection.clear_marks() {
            cx.notify();
        } else {
            cx.propagate();
        }
    }

    fn open_as_tab(&mut self, _: &OpenAsTab, _: &mut Window, cx: &mut Context<Self>) {
        let Some(object) = self.focused_object(cx) else {
            return;
        };
        self.state.update(cx, |state, cx| {
            if state.open_tab(object) {
                cx.notify();
            }
        });
    }

    fn remove_selected(&mut self, _: &RemoveSelected, _: &mut Window, cx: &mut Context<Self>) {
        self.ask_removal(cx);
    }

    /// `→`: opens a folded band, a host's folded services, the rest of a
    /// thread or a fold.
    fn unfold(&mut self, _: &Unfold, _: &mut Window, cx: &mut Context<Self>) {
        self.sync(cx);
        match self.selection.cursor().and_then(|index| self.line(index)) {
            Some(Line::Band {
                object,
                collapsed: true,
                ..
            }) => {
                let object = object.clone();
                self.folds.collapsed.remove(&object);
            }
            Some(Line::Fold {
                downtime,
                open: false,
                ..
            }) => {
                let name = downtime.clone();
                self.folds.open.insert(name);
            }
            Some(Line::More { key, hidden, .. }) if *hidden > 0 => {
                let key = key.clone();
                self.toggle_more(&key, cx);
                return;
            }
            _ => return,
        }
        cx.notify();
    }

    /// `←`: folds a band, a host's services, or a paged thread or fold
    /// back to its first seven.
    fn fold(&mut self, _: &Fold, _: &mut Window, cx: &mut Context<Self>) {
        self.sync(cx);
        match self.selection.cursor().and_then(|index| self.line(index)) {
            Some(Line::Band {
                object,
                collapsed: false,
                ..
            }) => {
                let object = object.clone();
                self.folds.collapsed.insert(object);
            }
            Some(Line::Fold {
                downtime,
                open: true,
                ..
            }) => {
                let name = downtime.clone();
                self.folds.open.remove(&name);
            }
            Some(Line::More { key, hidden: 0 }) => {
                let key = key.clone();
                self.toggle_more(&key, cx);
                return;
            }
            _ => {
                // Inside a group: fold it, the cursor on its band.
                let Some(band) = self
                    .selection
                    .cursor()
                    .and_then(|index| self.band_of(index))
                else {
                    return;
                };
                let Some(Line::Band { object, .. }) = self.line(band).cloned() else {
                    return;
                };
                self.folds.collapsed.insert(object.clone());
                self.sync(cx);
                if self.selection.select_key(&ItemKey::Band(object))
                    && let Some(index) = self.selection.cursor()
                {
                    self.reveal(index);
                }
            }
        }
        cx.notify();
    }

    fn acknowledge(&mut self, _: &Acknowledge, _: &mut Window, cx: &mut Context<Self>) {
        self.request(ObjectAction::Acknowledge, cx);
    }

    fn schedule_downtime(&mut self, _: &ScheduleDowntime, _: &mut Window, cx: &mut Context<Self>) {
        self.request(ObjectAction::ScheduleDowntime, cx);
    }

    fn check_now(&mut self, _: &CheckNow, _: &mut Window, cx: &mut Context<Self>) {
        self.request(ObjectAction::CheckNow, cx);
    }

    /// `c`: the pane's comment field when its thread shows, else the
    /// comment dialog.
    fn add_comment(&mut self, _: &AddComment, window: &mut Window, cx: &mut Context<Self>) {
        if self.selection.marked_count() == 0
            && let Some(pane) = &self.pane
            && pane
                .view
                .update(cx, |pane, cx| pane.focus_comment_field(window, cx))
        {
            return;
        }
        self.request(ObjectAction::AddComment, cx);
    }

    /// An object action's key: on the pane's object, else the cursor's.
    /// While entries are marked the keys do nothing here (the marks are
    /// records, not objects to act on), and the pane's buttons show no
    /// keys.
    fn request(&mut self, action: ObjectAction, cx: &mut Context<Self>) {
        if self.selection.marked_count() > 0 {
            return;
        }
        let Some(object) = self.focused_object(cx) else {
            return;
        };
        self.state.update(cx, |state, cx| {
            // The workspace opens the dialog; a refusal shows as a toast.
            let _ = state.request(ActionRequest {
                action,
                targets: vec![object],
                review: false,
            });
            cx.notify();
        });
    }

    /// The object the keyboard acts on without marks: the pane's, else
    /// the cursor's.
    pub(crate) fn focused_object(&mut self, cx: &mut Context<Self>) -> Option<ObjectKey> {
        self.sync(cx);
        match &self.pane {
            Some(pane) => Some(pane.view.read(cx).object().clone()),
            None => self.cursor_object(),
        }
    }

    /// The objects an action from the palette applies to: the pane's or
    /// the cursor's object (marks are records, not objects).
    pub(crate) fn action_targets(&mut self, cx: &mut Context<Self>) -> Vec<ObjectKey> {
        self.focused_object(cx).into_iter().collect()
    }

    fn line(&self, index: usize) -> Option<&Line> {
        self.listing()?.line(index)
    }

    fn cursor_object(&self) -> Option<ObjectKey> {
        let index = self.selection.cursor()?;
        self.line(index)?.object().cloned()
    }

    /// The band of the group line `index` belongs to (itself for a band).
    fn band_of(&self, index: usize) -> Option<usize> {
        let listing = self.listing()?;
        for at in (0..=index).rev() {
            match listing.line(at)? {
                Line::Band { .. } => return Some(at),
                Line::Section { .. } | Line::Axis | Line::Entry { single: true, .. } => {
                    return None;
                }
                _ => {}
            }
        }
        None
    }

    /// The entries shown under the band on line `band`.
    fn group_entries(&self, band: usize) -> Vec<ItemKey> {
        let Some(listing) = self.listing() else {
            return Vec::new();
        };
        listing
            .lines
            .iter()
            .skip(band + 1)
            .take_while(|keyed| {
                !matches!(
                    keyed.line,
                    Line::Band { .. }
                        | Line::Section { .. }
                        | Line::Axis
                        | Line::Entry { single: true, .. }
                )
            })
            .filter(|keyed| keyed.markable())
            .filter_map(|keyed| keyed.key.clone())
            .collect()
    }

    /// The marked entries, else the cursor's entry.
    fn targets(&self) -> Vec<EntryKey> {
        let marked = self.selection.marked_keys();
        let keys = if marked.is_empty() {
            self.selection.cursor_key().cloned().into_iter().collect()
        } else {
            marked
        };
        keys.into_iter()
            .filter_map(|key| match key {
                ItemKey::Entry(entry) => Some(entry),
                _ => None,
            })
            .collect()
    }

    /// Asks the workspace to confirm removing the marked entries (or the
    /// cursor's), listing every target. Kinds the API user may not remove
    /// are left out, and the dialog says so.
    fn ask_removal(&mut self, cx: &mut Context<Self>) {
        self.sync(cx);
        let targets = self.targets();
        if targets.is_empty() {
            return;
        }
        let mut downtimes = Vec::new();
        let mut comments = Vec::new();
        let mut acknowledgements = Vec::new();
        for key in targets {
            match key {
                EntryKey::Downtime(name) => downtimes.push(name),
                EntryKey::Comment(name) => comments.push(name),
                EntryKey::Ack(object) => acknowledgements.push(object),
            }
        }
        let state = self.state.read(cx);
        let snapshot = state.snapshot();
        let now = Timestamp::now();
        let mut parts = Vec::new();
        let mut denied = Vec::new();
        let mut take = |count: usize,
                        action: ObjectAction,
                        noun: (&'static str, &'static str),
                        part: &dyn Fn() -> BulkRemoval| {
            if count == 0 {
                return;
            }
            match state.action_denial(&action) {
                Some(denial) => {
                    let word = if count == 1 { noun.0 } else { noun.1 };
                    denied.push((noun.1, format!("skipped: {count} {word}: {denial}")));
                }
                None => parts.push(part()),
            }
        };
        take(
            downtimes.len(),
            ObjectAction::RemoveNamedDowntimes(Vec::new()),
            ("downtime", "downtimes"),
            &|| removal::downtimes(snapshot, &downtimes, now),
        );
        take(
            acknowledgements.len(),
            ObjectAction::RemoveAcknowledgement,
            ("acknowledgement", "acknowledgements"),
            &|| removal::acknowledgements(snapshot, &acknowledgements, now),
        );
        take(
            comments.len(),
            ObjectAction::RemoveComments(Vec::new()),
            ("comment", "comments"),
            &|| removal::comments(snapshot, &comments, now),
        );
        if let Some(mut removal) = BulkRemoval::merge(parts) {
            removal
                .skipped
                .extend(denied.into_iter().map(|(_, line)| line));
            cx.emit(RecordListEvent::Remove(removal));
        } else {
            let title = match denied.as_slice() {
                [(noun, _)] => format!("Can't remove {noun}"),
                _ => "Can't remove these".to_owned(),
            };
            let detail = (!denied.is_empty()).then(|| {
                denied
                    .iter()
                    .map(|(_, line)| line.trim_start_matches("skipped: "))
                    .collect::<Vec<_>>()
                    .join(" · ")
            });
            self.state.update(cx, |state, cx| {
                state.inform(title, detail);
                cx.notify();
            });
        }
    }

    /// A click on line `index` (key `key`): plain clicks put the cursor
    /// there and open its object in the pane, shift extends the marks from
    /// the anchor, ctrl/cmd toggles the entry's mark.
    fn click_line(
        &mut self,
        index: usize,
        key: &ItemKey,
        modifiers: Modifiers,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);
        self.menus.close();
        self.sync(cx);
        let Some(index) = self.selection.rows().locate(index, key) else {
            cx.notify();
            return;
        };
        if modifiers.shift {
            self.change_selection(cx, |selection| selection.extend_to(index).then_some(index));
        } else if modifiers.secondary() {
            self.change_selection(cx, |selection| {
                selection.toggle_mark(index).then_some(index)
            });
        } else if self.selection.select(index) {
            match self.line(index) {
                Some(Line::Fold { downtime, .. }) => {
                    let name = downtime.clone();
                    self.toggle_fold(&name, cx);
                }
                Some(Line::More { key, .. }) => {
                    let key = key.clone();
                    self.toggle_more(&key, cx);
                }
                _ => {
                    if let Some(object) = self.cursor_object() {
                        self.open_pane(object, cx);
                    }
                }
            }
        }
    }

    /// A band's chevron: folds or unfolds the object, and nothing else.
    fn toggle_band(&mut self, object: &ObjectKey, cx: &mut Context<Self>) {
        if !self.folds.collapsed.remove(object) {
            self.folds.collapsed.insert(object.clone());
        }
        cx.notify();
    }

    /// Opens or closes the services folded under a host's downtime.
    fn toggle_fold(&mut self, name: &str, cx: &mut Context<Self>) {
        if !self.folds.open.remove(name) {
            self.folds.open.insert(name.to_owned());
        }
        cx.notify();
    }

    /// `+ N more` / `− show fewer`.
    fn toggle_more(&mut self, key: &MoreKey, cx: &mut Context<Self>) {
        match key {
            MoreKey::Thread(object) => {
                if !self.folds.all_entries.remove(object) {
                    self.folds.all_entries.insert(object.clone());
                }
            }
            MoreKey::Fold(name) => {
                if !self.folds.all_services.remove(name) {
                    self.folds.all_services.insert(name.clone());
                }
            }
        }
        cx.notify();
    }

    /// Folds every object to its band, or unfolds them all.
    fn fold_all(&mut self, fold: bool, cx: &mut Context<Self>) {
        self.sync(cx);
        if fold {
            // The cursor inside a group goes to its band, which stays.
            let band = self
                .selection
                .cursor()
                .and_then(|index| self.band_of(index))
                .and_then(|band| match self.line(band) {
                    Some(Line::Band { object, .. }) => Some(object.clone()),
                    _ => None,
                });
            if let Some(object) = band {
                self.selection.select_key(&ItemKey::Band(object));
            }
            if let Some(listing) = self.listing() {
                let objects: Vec<ObjectKey> = listing
                    .lines
                    .iter()
                    .filter_map(|keyed| match &keyed.line {
                        Line::Band { object, .. } => Some(object.clone()),
                        _ => None,
                    })
                    .collect();
                self.folds.collapsed.extend(objects);
            }
        } else {
            self.folds.collapsed.clear();
        }
        self.menus.close();
        cx.notify();
    }

    /// Opens `object`'s pane beside the view, or shows it in the open one.
    fn open_pane(&mut self, object: ObjectKey, cx: &mut Context<Self>) {
        if let Some(pane) = &self.pane {
            pane.view.update(cx, |pane, cx| pane.open(object, cx));
        } else {
            let state = self.state.clone();
            let sidebar_open = self.sidebar_open;
            let focus = self.focus_handle.clone();
            let view = cx.new(|cx| {
                let mut pane = ObjectPane::new(state, object, PaneMode::Split, cx);
                pane.set_sidebar_open(sidebar_open, cx);
                pane.set_return_focus(focus);
                pane
            });
            let events = cx.subscribe(&view, |this, _, event: &PaneEvent, cx| match event {
                PaneEvent::Close => {
                    this.close_pane(cx);
                }
            });
            self.pane = Some(OpenPane {
                view,
                _events: events,
            });
        }
        cx.notify();
    }

    fn close_pane(&mut self, cx: &mut Context<Self>) -> bool {
        let closed = self.pane.take().is_some();
        if closed {
            cx.notify();
        }
        closed
    }

    /// Whether entries are marked (the selection bar shows; toasts float
    /// above it).
    pub(crate) fn has_marks(&self) -> bool {
        self.selection.marked_count() > 0
    }

    fn toggle_only_mine(&mut self, _: &ToggleOnlyMine, _: &mut Window, cx: &mut Context<Self>) {
        if self.state.read(cx).only_mine_denial(self.kind).is_some() {
            return;
        }
        self.set_options(|options| options.only_mine = !options.only_mine, cx);
    }

    fn next_sort(&mut self, _: &NextSort, _: &mut Window, cx: &mut Context<Self>) {
        let kind = self.kind;
        self.set_options(|options| options.sort = Some(options.next_sort(kind)), cx);
    }

    fn copy(&self, what: &str, text: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.state.update(cx, |state, cx| {
            state.inform(format!("Copied {what}"), None);
            cx.notify();
        });
    }

    /// The objects of the marked entries (else every object shown), each
    /// once, in line order.
    fn objects_of(&self, keys: &[ItemKey]) -> Vec<ObjectKey> {
        let Some(listing) = self.listing() else {
            return Vec::new();
        };
        let wanted: std::collections::HashSet<&ItemKey> = keys.iter().collect();
        let mut seen = std::collections::HashSet::new();
        listing
            .lines
            .iter()
            .filter(|keyed| {
                keyed
                    .key
                    .as_ref()
                    .is_some_and(|key| wanted.is_empty() || wanted.contains(key))
            })
            .filter(|keyed| {
                !wanted.is_empty()
                    || matches!(
                        keyed.line,
                        Line::Band { .. } | Line::Entry { single: true, .. }
                    )
            })
            .filter_map(|keyed| keyed.line.object())
            .filter(|object| seen.insert((*object).clone()))
            .cloned()
            .collect()
    }

    /// The marker of an action on its way for `entry` (`removing
    /// downtime…`).
    fn pending(&self, entry: &threads::Entry, cx: &App) -> Option<&'static str> {
        let (action, label) = self.state.read(cx).pending_action(&entry.object)?;
        let ours = matches!(
            (&entry.key, action),
            (
                EntryKey::Downtime(_),
                ObjectAction::RemoveDowntimes
                    | ObjectAction::RemoveDowntime(_)
                    | ObjectAction::RemoveNamedDowntimes(_),
            ) | (EntryKey::Comment(_), ObjectAction::RemoveComments(_))
                | (EntryKey::Ack(_), ObjectAction::RemoveAcknowledgement)
        );
        ours.then_some(label)
    }
}

// --- Rendering ----------------------------------------------------------

/// What a band or a single row says about its object: its mark, name, host
/// and a faint line.
struct Facts {
    mark: Option<ObjectMark>,
    name: String,
    host: Option<String>,
    state: Option<CheckableState>,
    output: String,
}

fn facts(snapshot: &ic_core::snapshot::Snapshot, object: &ObjectKey) -> Facts {
    let first = |text: &str| {
        text.lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or_default()
            .to_owned()
    };
    match object {
        ObjectKey::Host { name } => match snapshot.hosts.get(name) {
            Some(host) => Facts {
                mark: Some(ObjectMark::host(host)),
                name: if host.display_name.is_empty() {
                    host.name.to_string()
                } else {
                    host.display_name.clone()
                },
                host: None,
                state: Some(CheckableState::Host(host.state)),
                output: first(host.check.output()),
            },
            None => Facts {
                mark: None,
                name: name.to_string(),
                host: None,
                state: None,
                output: String::new(),
            },
        },
        ObjectKey::Service { key } => {
            let host = snapshot.host_of(key);
            let host_name =
                host.map_or_else(|| key.host.to_string(), |host| host.display_name.clone());
            match snapshot.services.get(key) {
                Some(service) => Facts {
                    mark: Some(ObjectMark::service(service, host.map(AsRef::as_ref))),
                    name: service.display_name.clone(),
                    host: Some(host_name),
                    state: Some(CheckableState::Service(service.state)),
                    output: first(service.check.output()),
                },
                None => Facts {
                    mark: None,
                    name: key.name.to_string(),
                    host: Some(host_name),
                    state: None,
                    output: String::new(),
                },
            }
        }
    }
}

/// `UP`, `CRITICAL`.
fn state_caps(state: Option<CheckableState>) -> String {
    state.map_or_else(String::new, |state| {
        crate::format::state_word(state).to_uppercase()
    })
}

impl RecordList {
    fn render_header(&self, list_width: Pixels, window: &Window, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let state = self.state.read(cx);
        let controls = Controls::of(window, cx);
        let options = self.shown_options();
        let mut header = PaneHeader::new("list-header").padding(theme.metrics.list_padding);
        if !self.sidebar_open {
            header = header.leading(sidebar_reopen(controls, theme));
        }
        let environment = state
            .environment()
            .map(|environment| environment.name.clone())
            .unwrap_or_default();
        let denial = state.only_mine_denial(self.kind);
        let only_mine = options.only_mine && denial.is_none();
        let roomy = list_width >= px(ROOMY_HEADER);
        let subtitle = if self.pane.is_some() || !roomy {
            environment
        } else {
            let mut parts = vec![environment];
            parts.push(
                options
                    .chip
                    .scope(self.kind)
                    .unwrap_or("the whole environment")
                    .to_owned(),
            );
            if only_mine {
                parts.push(format!("set by {}", state.author()));
            }
            if let Some(listing) = self.listing() {
                let summary = &listing.summary;
                parts.push(match self.kind {
                    ListKind::Handling => plural(summary.objects, "object", "objects"),
                    ListKind::Downtimes => plural(summary.downtimes, "downtime", "downtimes"),
                });
            }
            parts.join(" · ")
        };
        let mut header = header.title(self.kind.title()).subtitle(subtitle);
        if self.kind == ListKind::Downtimes {
            let selected = match options.mode {
                Mode::Timeline => 0,
                Mode::List => 1,
            };
            header = header.child(
                div().flex_none().child(
                    Segmented::new("list-mode")
                        .option("timeline")
                        .option("list")
                        .hug()
                        .selected(selected)
                        .on_select(cx.listener(|this, index: &usize, _, cx| {
                            let mode = if *index == 0 {
                                Mode::Timeline
                            } else {
                                Mode::List
                            };
                            this.set_options(|options| options.pick_mode(mode), cx);
                        })),
                ),
            );
        }
        if roomy {
            let author = state.author().to_owned();
            let switch = Switch::new("only-mine", only_mine)
                .label("only mine")
                .disabled(denial.is_some())
                .on_change(cx.listener(|this, on: &bool, _, cx| {
                    let on = *on;
                    this.set_options(|options| options.only_mine = on, cx);
                }));
            header = header.child(
                div()
                    .id("only-mine-slot")
                    .flex_none()
                    .mr(px(6.))
                    .child(switch)
                    .tooltip(Tooltip::text(denial.unwrap_or_else(|| {
                        format!("Only what {author} set (the environment's author) · m")
                    }))),
            );
        }
        let header = header
            .child(self.sort_trigger(&options, cx))
            .child(self.options_trigger(cx));
        self.drag
            .attach(div().id("list-header-drag").child(header), controls)
            .into_any_element()
    }

    /// The sort, right-aligned in a slot sized for the view's longest sort
    /// label, so a new sort changes only the word.
    fn sort_trigger(&self, options: &Options, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let open = self.menus.open() == Some(HeaderMenu::Sort);
        let sort = options.sort(self.kind);
        let label = sort.label(self.kind, options.chip);
        #[expect(clippy::cast_precision_loss, reason = "a short slot")]
        let slot = draw::chars(theme.text.small, self.kind.sort_slot_chars() as f32);
        div()
            .relative()
            .flex()
            .flex_none()
            .justify_end()
            .w(slot)
            .child(
                div()
                    .id("list-sort-trigger")
                    .text_size(theme.text.small)
                    .text_color(if open { colors.text } else { colors.text_muted })
                    .cursor_pointer()
                    .hover(|style| style.text_color(colors.text))
                    .child(label)
                    .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                    .on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
                        this.menus.toggle(HeaderMenu::Sort, down_position(event));
                        cx.notify();
                    })),
            )
            .when(open, |trigger| {
                let mut menu = Menu::new("list-sort-menu").label("sort by");
                for choice in self.kind.sorts() {
                    let choice = *choice;
                    menu = menu.item(
                        MenuItem::new(choice.id(), choice.menu_label(self.kind))
                            .checked(choice == sort)
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.set_options(|options| options.sort = Some(choice), cx);
                            })),
                    );
                }
                trigger
                    .child(Popover::new(menu.on_dismiss(Self::dismiss_listener(cx))).align_right())
            })
            .into_any_element()
    }

    fn options_trigger(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let open = self.menus.open() == Some(HeaderMenu::Options);
        let trigger = GlyphButton::new("list-options", "···")
            .text_size(px(13.))
            .bleed()
            .color(theme.colors.text_muted)
            .selected(open)
            .on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
                this.menus.toggle(HeaderMenu::Options, down_position(event));
                cx.notify();
            }));
        div()
            .relative()
            .flex_none()
            .child(if open {
                trigger
            } else {
                trigger.tooltip(Tooltip::new("View options"))
            })
            .when(open, |trigger| {
                trigger.child(Popover::new(self.options_menu(cx)).align_right())
            })
            .into_any_element()
    }

    fn options_menu(&self, cx: &Context<Self>) -> Menu {
        let state = self.state.read(cx);
        let options = self.shown_options();
        let denial = state.only_mine_denial(self.kind);
        let objects = self.objects_of(&[]);
        let names = expression::names(&objects);
        let filter = expression::filter(&objects);
        let none = objects.is_empty();
        let only_mine = options.only_mine && denial.is_none();
        Menu::new("list-options-menu")
            .item(
                MenuItem::new("list-only-mine", "only mine")
                    .checked(only_mine)
                    .disabled(denial.is_some())
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.set_options(|options| options.only_mine = !only_mine, cx);
                    })),
            )
            .separator()
            .item(
                MenuItem::new("list-fold-all", "fold every object").on_click(cx.listener(
                    |this, _: &ClickEvent, _, cx| {
                        this.fold_all(true, cx);
                    },
                )),
            )
            .item(
                MenuItem::new("list-unfold-all", "unfold every object")
                    .disabled(self.folds.collapsed.is_empty())
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.fold_all(false, cx);
                    })),
            )
            .separator()
            .item(
                MenuItem::new("list-copy-names", "copy names")
                    .disabled(none)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.menus.close();
                        this.copy("the names", names.clone(), cx);
                    })),
            )
            .item(
                MenuItem::new("list-copy-filter", "copy filter expression")
                    .disabled(none)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.menus.close();
                        this.copy("the filter expression", filter.clone(), cx);
                    })),
            )
            .separator()
            .item(
                MenuItem::new("list-close", format!("close {}", self.kind.title())).on_click(
                    cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.menus.close();
                        let kind = this.kind;
                        this.state.update(cx, |state, cx| {
                            if state.close_list(kind) {
                                cx.notify();
                            }
                        });
                    }),
                ),
            )
            .on_dismiss(Self::dismiss_listener(cx))
    }

    fn dismiss_listener(
        cx: &Context<Self>,
    ) -> impl Fn(&ic_ui_kit::Dismissal, &mut Window, &mut App) + 'static {
        cx.listener(|this, dismissal: &ic_ui_kit::Dismissal, _, cx| {
            this.menus.dismissed(*dismissal);
            cx.notify();
        })
    }

    /// The chips that filter the view (the summary bar's counts): `all`,
    /// `✓ 7 acknowledged`, `● 5 in downtime`; in a narrow view each keeps
    /// its mark and count, its word in the tooltip. At the end, what the
    /// chips don't say.
    fn render_chips(&self, list_width: Pixels, cx: &Context<Self>) -> Option<AnyElement> {
        let theme = cx.theme();
        let colors = theme.colors;
        let listing = self.listing()?;
        let summary = &listing.summary;
        let options = self.shown_options();
        let words: Vec<(Chip, String)> = self
            .kind
            .chips()
            .iter()
            .map(|chip| {
                let label = chip.label(self.kind);
                let text = match summary.count(*chip) {
                    Some(count) => format!("{count} {label}"),
                    None => label.to_owned(),
                };
                (*chip, text)
            })
            .collect();
        let end = self.summary_end(&options, cx);
        // Chips: label, padding, border, the mark and its gap; 6px apart.
        let mark = px(11. + 6.);
        let wide: Pixels = words
            .iter()
            .map(|(chip, text)| {
                ic_ui_kit::chip_width(text, theme.text.label)
                    + if *chip == Chip::All { px(0.) } else { mark }
                    + px(6.)
            })
            .sum::<Pixels>()
            + draw::chars(theme.text.small, char_count(&end))
            + theme.metrics.list_padding * 2.
            + px(18.);
        let narrow = wide > list_width;
        let chips = words.into_iter().map(|(chip, text)| {
            let selected = chip == options.chip;
            let count = summary.count(chip);
            let label = match (narrow, count) {
                (true, Some(count)) => count.to_string(),
                _ => text.clone(),
            };
            let leading = chip_mark(chip, theme);
            let mut button = ChipButton::new(
                SharedString::from(format!("list-chip-{}", chip.id())),
                label,
            )
            .selected(selected)
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.set_options(|options| options.pick_chip(chip), cx);
            }));
            if let Some(leading) = leading {
                button = button.leading(leading);
            }
            div()
                .id(SharedString::from(format!("list-chip-slot-{}", chip.id())))
                .flex_none()
                .child(button)
                .when(narrow, |slot| slot.tooltip(Tooltip::text(text)))
        });
        Some(
            div()
                .id("list-chips")
                .flex()
                .flex_none()
                .items_center()
                .gap(px(6.))
                .h(Metrics::with_rule(theme.metrics.summary_bar_height))
                .px(theme.metrics.list_padding)
                .border_b_1()
                .border_color(colors.border_header)
                .whitespace_nowrap()
                .overflow_hidden()
                .children(chips)
                .child(div().flex_1().min_w_0())
                .child(
                    div()
                        .flex_none()
                        .text_size(theme.text.small)
                        .text_color(colors.text_faint)
                        .child(end),
                )
                .into_any_element(),
        )
    }

    /// What the chips row says at its end: what *only mine* hides, the
    /// sticky acknowledgements, what the API user may not read.
    fn summary_end(&self, options: &Options, cx: &App) -> String {
        let Some(listing) = self.listing() else {
            return String::new();
        };
        let summary = &listing.summary;
        if options.only_mine && summary.by_others > 0 {
            return format!("{} by others hidden", summary.by_others);
        }
        if self.kind == ListKind::Handling
            && let Some(denial) = self.state.read(cx).handling_gap()
        {
            return denial;
        }
        if options.chip == Chip::Acknowledged && summary.sticky > 0 {
            return format!("{} sticky", summary.sticky);
        }
        String::new()
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the virtual lines, the now line and the sticky band in one frame"
    )]
    fn render_body(
        &mut self,
        list_width: Pixels,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let state = self.state.read(cx);
        if state.environment().is_none() {
            return EmptyState::new("No environment yet")
                .detail("Add an Icinga environment to see who is handling what and its downtimes.")
                .max_width(px(560.))
                .into_any_element();
        }
        if let Some(denial) = state.list_denial(self.kind) {
            return EmptyState::new("No permission")
                .leading(
                    Icon::new(IconName::Lock)
                        .size(px(20.))
                        .color(theme.colors.text_muted),
                )
                .detail(denial)
                .max_width(px(560.))
                .into_any_element();
        }
        let Some(listing) = self.listing() else {
            return div().into_any_element();
        };
        if listing.lines.is_empty() {
            if state.has_no_objects() && state.connection().is_starting() {
                return banner::loading_body(state, cx);
            }
            return self.empty(cx);
        }
        let Some(layout) = self.layout.clone() else {
            return div().into_any_element();
        };
        // The viewport as last drawn; before the first frame, the window's
        // height (more than the view shows, never less).
        let drawn = self.scroll.bounds().size.height;
        let viewport = if drawn > px(0.) {
            drawn
        } else {
            window.viewport_size().height
        };
        let max = (layout.height() - viewport).max(px(0.));
        let offset = (-self.scroll.offset().y).clamp(px(0.), max);
        let first = layout.line_at(offset).unwrap_or(0);
        let last = layout.line_at(offset + viewport).unwrap_or(first);
        let range = first..(last + 1).min(layout.tops.len() - 1);
        self.visible = range.clone();
        let now = Timestamp::now();
        let axis = (layout.mode == Mode::Timeline).then(|| words::axis(now));
        let axis_width = draw::axis_width(list_width, &theme);
        let items: Vec<AnyElement> = range
            .clone()
            .map(|index| {
                let height = layout.top(index + 1) - layout.top(index);
                let element = self.render_line(index, false, axis.as_ref(), axis_width, now, cx);
                div()
                    .flex_none()
                    .w_full()
                    .h(height)
                    .overflow_hidden()
                    .child(element)
                    .into_any_element()
            })
            .collect();
        let above = layout.top(range.start);
        let below = layout.height() - layout.top(range.end);
        let sticky = self.sticky_band(&layout, offset, axis.as_ref(), axis_width, now, cx);
        let now_line = axis
            .as_ref()
            .filter(|axis| (0. ..=1.).contains(&axis.now))
            .map(|axis| {
                let left = draw::axis_left(list_width, axis_width, &theme) + axis_width * axis.now;
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(left)
                    .w(Metrics::RULE)
                    .bg(theme.colors.accent)
                    .opacity(0.6)
            });
        let content = div()
            .flex()
            .flex_col()
            .w_full()
            .flex_none()
            .child(div().flex_none().h(above))
            .children(items)
            .child(div().flex_none().h(below));
        div()
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(
                div()
                    .id("record-lines")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(content),
            )
            .children(now_line)
            .children(sticky)
            .child(Scrollbar::vertical(&self.scroll))
            .into_any_element()
    }

    /// The band of the object whose entries are at the top, when its own
    /// band has scrolled away: it sticks to the top.
    fn sticky_band(
        &mut self,
        layout: &Layout,
        offset: Pixels,
        axis: Option<&Axis>,
        axis_width: Pixels,
        now: Timestamp,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let top = layout.line_at(offset)?;
        let band = self.band_of(top)?;
        if band == top && layout.top(band) >= offset {
            return None;
        }
        if layout.top(band) >= offset {
            return None;
        }
        let element = self.render_line(band, true, axis, axis_width, now, cx);
        Some(
            div()
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .h(layout.sizes.band)
                .shadow(vec![BoxShadow {
                    color: cx.theme().colors.shadow_strong,
                    offset: point(px(0.), px(6.)),
                    blur_radius: px(12.),
                    spread_radius: px(-6.),
                    inset: false,
                }])
                .child(element)
                .into_any_element(),
        )
    }

    /// Line `index`.
    fn render_line(
        &mut self,
        index: usize,
        sticky: bool,
        axis: Option<&Axis>,
        axis_width: Pixels,
        now: Timestamp,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(listing) = self.listing().cloned() else {
            return div().into_any_element();
        };
        let Some(keyed) = listing.lines.get(index).cloned() else {
            return div().into_any_element();
        };
        let theme = cx.theme().clone();
        let emphasis = keyed.key.as_ref().map_or(RowEmphasis::None, |key| {
            RowEmphasis::new(
                self.selection.cursor() == Some(index),
                self.selection.is_marked(key),
            )
        });
        let element = match &keyed.line {
            Line::Section { section, count } => {
                let sort = self.shown_options().sort(self.kind);
                draw::section(*section, *count, section.detail(sort), &theme).into_any_element()
            }
            Line::Axis => match axis {
                Some(axis) => axis_line(axis, axis_width, &theme).into_any_element(),
                None => div().into_any_element(),
            },
            Line::Band {
                object,
                slot,
                collapsed,
                covers,
            } => self.render_band(
                index,
                object,
                slot,
                *collapsed,
                *covers,
                sticky,
                emphasis,
                axis.is_some(),
                now,
                cx,
            ),
            Line::Entry {
                entry,
                reply,
                single,
            } => {
                let snapshot = self.state.read(cx).snapshot().clone();
                let pending = self.pending(entry, cx);
                match axis {
                    Some(axis) => timeline_entry(
                        &snapshot, entry, *single, axis, axis_width, emphasis, now, &theme,
                    ),
                    None => list_entry(
                        &snapshot, entry, *reply, *single, pending, emphasis, now, &theme,
                    ),
                }
            }
            Line::Fold {
                downtime,
                count,
                open,
                ..
            } => fold_line(downtime, *count, *open, emphasis, &theme).into_any_element(),
            Line::Service { parent, object, .. } => {
                let snapshot = self.state.read(cx).snapshot().clone();
                let bar = axis.and_then(|axis| {
                    listing
                        .downtime_at(parent)
                        .map(|downtime| words::bar(downtime, axis, now))
                });
                service_line(
                    &snapshot,
                    object,
                    bar.as_ref(),
                    axis_width,
                    emphasis,
                    &theme,
                )
                .into_any_element()
            }
            Line::More { hidden, .. } => more_line(*hidden, emphasis, &theme).into_any_element(),
        };
        let Some(key) = keyed.key.clone() else {
            return element;
        };
        if matches!(keyed.line, Line::Band { .. }) {
            // The band's own clicks are set where it is drawn.
            return element;
        }
        div()
            .id(ElementId::Name(format!("line:{}", key_id(&key)).into()))
            .size_full()
            .cursor_pointer()
            .child(element)
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.click_line(index, &key, event.modifiers(), window, cx);
            }))
            .into_any_element()
    }

    /// An object's band (36px): the chevron that only folds, the state dot
    /// (hollow = handled), `service on host`, the output faint, and what
    /// the thread holds in a slot at the right. The name never shrinks:
    /// the output gives way first, then the slot. A click anywhere but the
    /// chevron opens the object's pane.
    #[expect(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "the band as drawn, in both displays"
    )]
    fn render_band(
        &self,
        index: usize,
        object: &ObjectKey,
        slot: &str,
        collapsed: bool,
        covers: Option<Covers>,
        sticky: bool,
        emphasis: RowEmphasis,
        timeline: bool,
        now: Timestamp,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let snapshot = self.state.read(cx).snapshot().clone();
        let facts = facts(&snapshot, object);
        let state_word = state_caps(facts.state);
        let detail = match (object, covers) {
            (ObjectKey::Host { .. }, Some(covers)) if covers.in_effect => {
                format!(
                    "host and {} in downtime",
                    plural(covers.services, "service", "services")
                )
            }
            (ObjectKey::Host { .. }, Some(covers)) => format!(
                "host and {}, {}",
                plural(covers.services, "service", "services"),
                super::model::short_when(covers.start, now, &chrono::Local)
            ),
            _ => facts.output.clone(),
        };
        let out = match object {
            // A host's one downtime with its services says what it covers;
            // several say how many (as drawn).
            ObjectKey::Host { .. } if timeline => match covers {
                Some(covers) if slot == "1 downtime" => format!(
                    "{state_word} · host and {}",
                    plural(covers.services, "service", "services")
                ),
                _ => format!("{state_word} · {slot}"),
            },
            ObjectKey::Host { .. } if detail.is_empty() => state_word,
            ObjectKey::Host { .. } => format!("{state_word} · {detail}"),
            ObjectKey::Service { .. } if timeline => slot.to_owned(),
            ObjectKey::Service { .. } => detail,
        };
        let lead = match facts.mark {
            Some(mark) => StateDot::mark(mark).size(px(9.)),
            None => StateDot::with_color(theme.states.fill.pending).size(px(9.)),
        };
        let id_suffix = if sticky { ":sticky" } else { "" };
        let chevron_object = object.clone();
        let chevron = div()
            .id(SharedString::from(format!(
                "band-chevron:{}{id_suffix}",
                object.full_name()
            )))
            .absolute()
            .left(px(14.))
            .top_0()
            .bottom_0()
            .w(px(12.))
            .flex()
            .items_center()
            .cursor_pointer()
            .child(draw::chevron(!collapsed, theme))
            .on_mouse_down(MouseButton::Left, |_, window, cx| {
                window.prevent_default();
                cx.stop_propagation();
            })
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                cx.stop_propagation();
                this.toggle_band(&chevron_object, cx);
            }));
        let background = emphasis.background(theme).unwrap_or(colors.row_header);
        let key = ItemKey::Band(object.clone());
        div()
            .id(SharedString::from(format!(
                "band:{}{id_suffix}",
                object.full_name()
            )))
            .relative()
            .flex()
            .items_center()
            .gap(px(draw::COLUMN_GAP))
            .size_full()
            .px(theme.metrics.list_padding)
            .bg(background)
            .border_b_1()
            .border_color(colors.border_header)
            .whitespace_nowrap()
            .cursor_pointer()
            .when(emphasis == RowEmphasis::None, |band| {
                band.hover(|style| style.bg(colors.row_hover))
            })
            .child(chevron)
            .child(
                div()
                    .flex()
                    .flex_none()
                    .justify_center()
                    .w(px(draw::MARK_COLUMN))
                    .child(lead),
            )
            .child(draw::object_label(
                &facts.name,
                facts.host.as_deref(),
                theme.text.row,
                theme,
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(theme.text.small)
                    .text_color(colors.text_faint)
                    .child(out),
            )
            .when(!timeline && !slot.is_empty(), |band| {
                band.child(
                    div()
                        .flex_shrink(1.)
                        .min_w_0()
                        .max_w(draw::chars(theme.text.small, BAND_SLOT_CHARS))
                        .truncate()
                        .text_right()
                        .text_size(theme.text.small)
                        .text_color(colors.text_faint)
                        .child(slot.to_owned()),
                )
            })
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.click_line(index, &key, event.modifiers(), window, cx);
            }))
            .into_any_element()
    }

    /// The body of an empty view: what's not there, and what is hidden.
    fn empty(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let options = self.shown_options();
        let leading = Icon::new(self.kind.icon())
            .size(px(20.))
            .color(theme.colors.text_muted);
        let by_others = self
            .listing()
            .map_or(0, |listing| listing.summary.by_others);
        if options.only_mine && by_others > 0 {
            let author = self.state.read(cx).author().to_owned();
            return EmptyState::new(format!("Nothing set by {author}"))
                .leading(leading)
                .detail(format!(
                    "{by_others} by others {} hidden.",
                    if by_others == 1 { "is" } else { "are" }
                ))
                .child(Link::new("list-show-all", "show all").on_click(cx.listener(
                    |this, _: &ClickEvent, _, cx| {
                        this.set_options(|options| options.only_mine = false, cx);
                    },
                )))
                .into_any_element();
        }
        if options.chip != Chip::All {
            let what = options.chip.label(self.kind);
            return EmptyState::new(format!("Nothing {what}"))
                .leading(leading)
                .child(
                    Link::new("list-show-every", "show all").on_click(cx.listener(
                        |this, _: &ClickEvent, _, cx| {
                            this.set_options(|options| options.pick_chip(Chip::All), cx);
                        },
                    )),
                )
                .into_any_element();
        }
        let (title, detail) = match self.kind {
            ListKind::Handling => (
                "Nobody is handling anything",
                "No problem is acknowledged, nothing is in downtime or scheduled, and no object \
                 has a comment.",
            ),
            ListKind::Downtimes => (
                "No downtimes",
                "Nothing is in downtime, and none is scheduled.",
            ),
        };
        EmptyState::new(title)
            .leading(leading)
            .detail(detail)
            .into_any_element()
    }

    /// The bar under the view while entries are marked: the count in a
    /// fixed slot, then the view's actions, so the buttons never move.
    fn render_selection_bar(&self, list_width: Pixels, cx: &Context<Self>) -> Option<AnyElement> {
        let marked = self.selection.marked_keys();
        if marked.is_empty() {
            return None;
        }
        let theme = cx.theme();
        let colors = theme.colors;
        let compact = bar_is_compact(self.kind, list_width, theme);
        let listing = self.listing();
        // Every marked entry a downtime from the config: nothing to remove.
        let only_config = listing.is_some_and(|listing| {
            listing
                .lines
                .iter()
                .filter(|keyed| keyed.key.as_ref().is_some_and(|key| marked.contains(key)))
                .filter_map(|keyed| keyed.line.entry())
                .all(|entry| entry.config)
        });
        let mut remove = Button::new("list-remove", self.kind.removal_label()).primary();
        if !compact {
            remove = remove.key_hint("⌫");
        }
        let remove = if only_config {
            remove.disabled(true).tooltip(Tooltip::new(
                "Downtimes from the config can't be removed: Icinga refuses, and the config \
                 brings them back",
            ))
        } else {
            remove.on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.ask_removal(cx)))
        };
        let objects = self.objects_of(&marked);
        let names = expression::names(&objects);
        let copy = Button::new("list-bulk-copy-names", "copy names").on_click(cx.listener(
            move |this, _: &ClickEvent, _, cx| {
                this.copy("the names", names.clone(), cx);
            },
        ));
        Some(
            div()
                .id("list-selection-bar")
                .flex()
                .flex_none()
                .items_center()
                .gap(px(8.))
                .h(px(SELECTION_BAR_HEIGHT))
                .px(theme.metrics.list_padding)
                .border_t_1()
                .border_color(colors.border_header)
                .bg(colors.pane_background)
                .text_size(theme.text.small)
                .child(
                    div()
                        .id("list-selection-count")
                        .relative()
                        .flex_none()
                        .mr(px(4.))
                        // Rounded up: the font's advance is a hair over 0.6em.
                        .min_w(draw::chars(theme.text.small, COUNT_SLOT_CHARS) + px(1.))
                        .text_color(colors.accent_text)
                        .child(format!("{} selected", marked.len()))
                        .map(|count| {
                            #[cfg(test)]
                            let count = count.child({
                                let probe = self.selection_buttons_x.clone();
                                gpui::canvas(
                                    move |bounds, _, _| probe.set(Some(bounds.right())),
                                    |_, (), _, _| {},
                                )
                                .absolute()
                                .top_0()
                                .left_0()
                                .size_full()
                            });
                            count
                        }),
                )
                .child(remove)
                .child(copy)
                .child(self.selection_more(&objects, cx))
                .child(div().flex_1())
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(px(6.))
                        .child(
                            Link::new("list-clear", "clear")
                                .quiet()
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                    if this.selection.clear_marks() {
                                        cx.notify();
                                    }
                                })),
                        )
                        .when(!compact, |clear| clear.child(KeyHint::new("esc"))),
                )
                .into_any_element(),
        )
    }

    /// The selection bar's `···`: copying a filter expression.
    fn selection_more(&self, objects: &[ObjectKey], cx: &Context<Self>) -> AnyElement {
        let colors = cx.theme().colors;
        let open = self.menus.open() == Some(HeaderMenu::Selection);
        let filter = expression::filter(objects);
        div()
            .relative()
            .flex_none()
            .child(
                GlyphButton::new("list-bulk-more", "···")
                    .text_size(px(13.))
                    .color(colors.text_muted)
                    .selected(open)
                    .tooltip(Tooltip::new("More for the selection"))
                    .on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
                        this.menus
                            .toggle(HeaderMenu::Selection, down_position(event));
                        cx.notify();
                    })),
            )
            .when(open, |trigger| {
                trigger.child(
                    Popover::new(
                        Menu::new("list-selection-menu")
                            .item(
                                MenuItem::new("list-bulk-copy-filter", "copy filter expression")
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        this.menus.close();
                                        this.copy("the filter expression", filter.clone(), cx);
                                    })),
                            )
                            .on_dismiss(Self::dismiss_listener(cx)),
                    )
                    .above(),
                )
            })
            .into_any_element()
    }

    /// The view's column: header, banners, chips, lines, selection bar.
    fn render_column(
        &mut self,
        list_width: Pixels,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let header = self.render_header(list_width, window, cx);
        let now = Timestamp::now();
        let banners = banner::banners(&self.state, now, true, cx);
        let progress = banner::progress(self.state.read(cx));
        let denied = self.state.read(cx).list_denial(self.kind).is_some();
        let chips = (!denied)
            .then(|| self.render_chips(list_width, cx))
            .flatten();
        let body = self.render_body(list_width, window, cx);
        let bar = self.render_selection_bar(list_width, cx);
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(header)
            .children(progress)
            .children(banners)
            .children(chips)
            .child(body)
            .children(bar)
    }
}

impl Render for RecordList {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync(cx);
        if let Some(pane) = &self.pane {
            let marked = self.selection.marked_count() > 0;
            pane.view
                .update(cx, |pane, _| pane.set_keys_elsewhere(marked));
        }
        let main_width = SplitLayout::main_width(window, self.sidebar_open, &cx.theme().metrics);
        let split = SplitLayout::for_width(main_width, &cx.theme().metrics);
        let pane = self.pane.as_ref().map(|pane| pane.view.clone());
        let root = div()
            .id("record-list")
            .key_context(format!("{} {LIST_CONTEXT}", crate::actions::DASHBOARD_CONTEXT).as_str())
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::extend_next))
            .on_action(cx.listener(Self::extend_previous))
            .on_action(cx.listener(Self::select_first))
            .on_action(cx.listener(Self::select_last))
            .on_action(cx.listener(Self::select_page_down))
            .on_action(cx.listener(Self::select_page_up))
            .on_action(cx.listener(Self::toggle_mark))
            .on_action(cx.listener(Self::mark_all))
            .on_action(cx.listener(Self::open_selected))
            .on_action(cx.listener(Self::dismiss))
            .on_action(cx.listener(Self::open_as_tab))
            .on_action(cx.listener(Self::remove_selected))
            .on_action(cx.listener(Self::unfold))
            .on_action(cx.listener(Self::fold))
            .on_action(cx.listener(Self::toggle_only_mine))
            .on_action(cx.listener(Self::next_sort))
            .on_action(cx.listener(Self::acknowledge))
            .on_action(cx.listener(Self::schedule_downtime))
            .on_action(cx.listener(Self::check_now))
            .on_action(cx.listener(Self::add_comment))
            .flex()
            .flex_1()
            .min_w_0()
            .h_full();
        let pane_width = match (pane, split) {
            (Some(pane), SplitLayout::Cover) => {
                return root.child(div().flex().flex_1().min_w_0().h_full().child(pane));
            }
            (Some(pane), SplitLayout::Side { pane_width }) => Some((pane, pane_width)),
            (None, _) => None,
        };
        let list_width = match pane_width {
            Some((_, pane_width)) => main_width - pane_width - Metrics::RULE,
            None => main_width,
        };
        let column = self.render_column(list_width, window, cx);
        let split_border = cx.theme().colors.border_split;
        root.child(column.when(pane_width.is_some(), |list| {
            list.border_r_1().border_color(split_border)
        }))
        .when_some(pane_width, |view, (pane, width)| {
            view.child(div().flex().flex_none().w(width).h_full().child(pane))
        })
    }
}

/// A chip's mark: handling's ✓, the accent dot of *in effect*, the faint
/// dot of *upcoming*, the speech bubble, the lock.
fn chip_mark(chip: Chip, theme: &Theme) -> Option<AnyElement> {
    let colors = theme.colors;
    Some(match chip {
        Chip::All => return None,
        Chip::Acknowledged => Icon::new(IconName::Check)
            .size(px(11.))
            .color(colors.accent_text)
            .into_any_element(),
        Chip::InEffect => StateDot::with_color(colors.accent)
            .size(px(7.))
            .into_any_element(),
        Chip::Upcoming => StateDot::with_color(theme.states.fill.pending)
            .size(px(7.))
            .into_any_element(),
        Chip::Comments => Icon::new(IconName::MessageSquare)
            .size(px(11.))
            .color(colors.text_muted)
            .into_any_element(),
        Chip::FromConfig => Icon::new(IconName::Lock)
            .size(px(11.))
            .color(colors.text_muted)
            .into_any_element(),
    })
}

/// The look of an emphasised line: its background, and the accent bar of
/// a marked one.
fn emphasised(line: gpui::Div, emphasis: RowEmphasis, theme: &Theme) -> gpui::Div {
    let colors = theme.colors;
    line.relative()
        .when_some(emphasis.background(theme), gpui::Styled::bg)
        .when(emphasis == RowEmphasis::None, |line| {
            line.hover(|style| style.bg(colors.row_hover))
        })
        .when(emphasis.is_marked(), |line| {
            line.child(
                div()
                    .absolute()
                    .left_0()
                    .top_0()
                    .bottom_0()
                    .w(px(draw::MARK_WIDTH))
                    .bg(colors.accent),
            )
        })
}

/// An entry in the handling view or the downtimes list (a single
/// downtime: band and entry in one row).
#[expect(clippy::too_many_arguments, reason = "an entry and how it shows")]
fn list_entry(
    snapshot: &ic_core::snapshot::Snapshot,
    entry: &threads::Entry,
    reply: bool,
    single: bool,
    pending: Option<&'static str>,
    emphasis: RowEmphasis,
    now: Timestamp,
    theme: &Theme,
) -> AnyElement {
    let Some(text) = words::entry_text(snapshot, entry, now) else {
        return div().into_any_element();
    };
    let compact = theme.density == ic_ui_kit::Density::Compact;
    let object = single.then(|| {
        let facts = facts(snapshot, &entry.object);
        (
            facts.mark.unwrap_or(ObjectMark {
                state: CheckableState::Service(ic_model::ServiceState::Pending),
                hollow: false,
            }),
            facts.name,
            facts.host,
        )
    });
    let look = Look {
        reply,
        object,
        pending,
        ..Look::default()
    };
    let body = draw::entry(&text, &look, compact, theme);
    let line = div()
        .size_full()
        .px(theme.metrics.list_padding)
        .when(compact, |line| line.flex().items_center())
        .when(!compact, |line| line.pt(px(9.)))
        .border_b_1()
        .border_color(theme.colors.border_row)
        .child(body);
    emphasised(line, emphasis, theme).into_any_element()
}

/// A downtime on the timeline: one row for an object's only downtime
/// (its dot, `service on host`, `author: text`), else one line inside its
/// group (the kind's icon, `kind  author`, the text); its bar on the axis
/// and its time at the right.
#[expect(clippy::too_many_arguments, reason = "an entry and its axis")]
#[expect(
    clippy::too_many_lines,
    reason = "both shapes of a timeline line, as drawn"
)]
fn timeline_entry(
    snapshot: &ic_core::snapshot::Snapshot,
    entry: &threads::Entry,
    single: bool,
    axis: &Axis,
    axis_width: Pixels,
    emphasis: RowEmphasis,
    now: Timestamp,
    theme: &Theme,
) -> AnyElement {
    let colors = theme.colors;
    let Some(downtime) = words::downtime_of(snapshot, entry) else {
        return div().into_any_element();
    };
    let compact = theme.density == ic_ui_kit::Density::Compact;
    let in_effect = entry.kind == EntryKind::InEffect;
    let (right, tone) = if entry.config && !in_effect {
        ("from config".to_owned(), words::Tone::Faint)
    } else {
        words::downtime_time(downtime, now, &chrono::Local)
    };
    let said = if entry.config {
        format!(
            "from config: {}",
            downtime
                .schedule
                .clone()
                .unwrap_or_else(|| super::model::first_line(&downtime.comment).to_owned())
        )
    } else {
        super::model::first_line(&downtime.comment).to_owned()
    };
    let (mark, first, second): (AnyElement, gpui::Div, String) = if single {
        let facts = facts(snapshot, &entry.object);
        let mark = match facts.mark {
            Some(mark) => StateDot::mark(mark).size(px(9.)),
            None => StateDot::with_color(theme.states.fill.pending).size(px(9.)),
        };
        let label = div()
            .flex()
            .flex_none()
            .text_size(theme.text.body)
            .child(div().text_color(colors.text_strong).child(facts.name))
            .when_some(facts.host, |label, host| {
                label
                    .child(div().text_color(colors.text_faint).child("\u{a0}on\u{a0}"))
                    .child(div().text_color(colors.text).child(host))
            });
        let second = if entry.config {
            said
        } else {
            format!("{}: {said}", downtime.author)
        };
        (mark.into_any_element(), label, second)
    } else {
        let mark = Icon::new(if entry.config {
            IconName::Lock
        } else {
            IconName::CalendarClock
        })
        .size(px(13.))
        .color(if in_effect {
            colors.accent_text
        } else {
            colors.text_faint
        });
        let kind = match (in_effect, entry.config) {
            (true, _) => "in downtime",
            (false, true) => "downtime, from config",
            (false, false) => "downtime, upcoming",
        };
        let author = if entry.config {
            downtime
                .schedule
                .clone()
                .unwrap_or_else(|| downtime.author.clone())
        } else {
            downtime.author.clone()
        };
        let label = div()
            .flex()
            .flex_none()
            .gap(px(9.))
            .text_size(theme.text.body)
            .child(
                div()
                    .text_color(if in_effect {
                        colors.accent_text
                    } else {
                        colors.text_muted
                    })
                    .child(kind),
            )
            .child(div().text_color(colors.text_strong).child(author));
        (
            mark.into_any_element(),
            label,
            super::model::first_line(&downtime.comment).to_owned(),
        )
    };
    let words_column = if compact {
        div()
            .flex()
            .items_baseline()
            .gap(px(9.))
            .flex_1()
            .min_w_0()
            .overflow_hidden()
            .whitespace_nowrap()
            .child(first)
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(theme.text.label)
                    .text_color(colors.text_faint)
                    .child(second),
            )
    } else {
        div()
            .flex()
            .flex_col()
            .gap(px(2.))
            .flex_1()
            .min_w_0()
            .overflow_hidden()
            .whitespace_nowrap()
            .child(first)
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(theme.text.label)
                    .text_color(colors.text_faint)
                    .child(second),
            )
    };
    let bar = words::bar(downtime, axis, now);
    let line = div()
        .flex()
        .items_center()
        .gap(px(draw::COLUMN_GAP))
        .size_full()
        .px(theme.metrics.list_padding)
        .border_b_1()
        .border_color(colors.border_row)
        .child(
            div()
                .flex()
                .flex_none()
                .justify_center()
                .w(px(draw::MARK_COLUMN))
                .child(mark),
        )
        .child(words_column)
        .child(draw::bar(&bar, axis_width, theme))
        .child(draw::right_column(
            right.into(),
            tone,
            entry.config && !in_effect,
            theme,
        ));
    emphasised(line, emphasis, theme).into_any_element()
}

/// The timeline's axis: `noon to midnight · now 14:12`, the hours, `left`.
fn axis_line(axis: &Axis, width: Pixels, theme: &Theme) -> gpui::Div {
    let colors = theme.colors;
    let size = theme.text.hint;
    let ticks = axis.ticks.iter().map(|(fraction, label)| {
        let half = draw::chars(size, char_count(label)) / 2.;
        div()
            .absolute()
            .top_0()
            .bottom_0()
            .flex()
            .items_center()
            .left(width * *fraction - half)
            .child(label.clone())
    });
    div()
        .flex()
        .items_center()
        .gap(px(draw::COLUMN_GAP))
        .size_full()
        .px(theme.metrics.list_padding)
        .bg(colors.row_header)
        .border_b_1()
        .border_color(colors.border_header)
        .whitespace_nowrap()
        .text_size(size)
        .text_color(colors.text_faint)
        .child(div().flex_none().w(px(draw::MARK_COLUMN)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .child(axis.label.clone()),
        )
        .child(
            div()
                .relative()
                .flex_none()
                .w(width)
                .h_full()
                .children(ticks),
        )
        .child(
            div()
                .flex_none()
                .w(px(draw::RIGHT_COLUMN))
                .text_right()
                .child("left"),
        )
}

/// The fold of a host's services: the chevron, `18 services, same
/// downtime · folded: they are identical`. A click opens or closes it.
fn fold_line(
    downtime: &str,
    count: usize,
    open: bool,
    emphasis: RowEmphasis,
    theme: &Theme,
) -> gpui::Div {
    let line = div()
        .id(SharedString::from(format!("fold:{downtime}")))
        .flex()
        .items_center()
        .gap(px(draw::COLUMN_GAP))
        .size_full()
        .px(theme.metrics.list_padding)
        .border_b_1()
        .border_color(theme.colors.border_row)
        .child(
            div()
                .absolute()
                .left(px(14.))
                .top_0()
                .bottom_0()
                .w(px(12.))
                .flex()
                .items_center()
                .child(draw::chevron(open, theme)),
        )
        .child(div().flex_none().w(px(draw::MARK_COLUMN)))
        .child(draw::fold_words(count, open, theme));
    emphasised(div().size_full().child(line), emphasis, theme)
}

/// A service of an open fold: its dot (hollow: in downtime), `disk / on
/// k8s-node-07`; on the timeline the host downtime's bar beside it.
fn service_line(
    snapshot: &ic_core::snapshot::Snapshot,
    object: &ObjectKey,
    bar: Option<&words::Bar>,
    axis_width: Pixels,
    emphasis: RowEmphasis,
    theme: &Theme,
) -> gpui::Div {
    let colors = theme.colors;
    let facts = facts(snapshot, object);
    let dot = match facts.mark {
        Some(mark) => StateDot::mark(mark).size(px(7.)),
        None => StateDot::with_color(theme.states.fill.pending).size(px(7.)),
    };
    let line = div()
        .flex()
        .items_center()
        .gap(px(draw::COLUMN_GAP))
        .size_full()
        .px(theme.metrics.list_padding)
        .border_b_1()
        .border_color(colors.border_row)
        .whitespace_nowrap()
        .child(
            div()
                .flex()
                .flex_none()
                .justify_center()
                .w(px(draw::MARK_COLUMN))
                .child(dot),
        )
        .child(
            div()
                .flex()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .text_size(theme.text.body)
                .child(div().flex_none().text_color(colors.text).child(facts.name))
                .when_some(facts.host, |label, host| {
                    label.child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_color(colors.text_faint)
                            .child(format!("\u{a0}on {host}")),
                    )
                }),
        )
        .when_some(bar, |line, bar| {
            line.child(draw::bar(bar, axis_width, theme))
                .child(div().flex_none().w(px(draw::RIGHT_COLUMN)))
        });
    emphasised(line, emphasis, theme)
}

/// The paging row: `+ 15 more`, or `− show fewer` in the same slot.
fn more_line(hidden: usize, emphasis: RowEmphasis, theme: &Theme) -> gpui::Div {
    let colors = theme.colors;
    let line = div()
        .flex()
        .items_center()
        .gap(px(draw::COLUMN_GAP))
        .size_full()
        .px(theme.metrics.list_padding)
        .border_b_1()
        .border_color(colors.border_row)
        .text_size(theme.text.small)
        .text_color(colors.text_faint)
        .child(div().flex_none().w(px(draw::MARK_COLUMN)))
        .child(crate::paging::more_label(hidden));
    emphasised(line, emphasis, theme)
}

/// An element id for a line's key.
fn key_id(key: &ItemKey) -> String {
    match key {
        ItemKey::Band(object) => format!("band:{}", object.full_name()),
        ItemKey::Entry(EntryKey::Ack(object)) => format!("ack:{}", object.full_name()),
        ItemKey::Entry(EntryKey::Downtime(name)) => format!("downtime:{name}"),
        ItemKey::Entry(EntryKey::Comment(name)) => format!("comment:{name}"),
        ItemKey::Fold(name) => format!("fold:{name}"),
        ItemKey::Service(name, object) => format!("service:{name}:{}", object.full_name()),
        ItemKey::More(MoreKey::Thread(object)) => format!("more:{}", object.full_name()),
        ItemKey::More(MoreKey::Fold(name)) => format!("more-fold:{name}"),
    }
}

/// Whether the selection bar of a view `list_width` wide must drop its
/// key hints: what it shows, with the hints, in characters and padding.
fn bar_is_compact(kind: ListKind, list_width: Pixels, theme: &Theme) -> bool {
    // The count's slot, the main button, `copy names`, `clear`.
    let characters = COUNT_SLOT_CHARS + char_count(kind.removal_label()) + 10. + 5.;
    // The count's margin, five gaps, two buttons' padding, `⌫` with its
    // gap, `···`, `esc` with its gap.
    let fixed = px(4. + 5. * 8. + 2. * 25. + 22. + 24. + 6. + 26.);
    draw::chars(theme.text.small, characters) + theme.metrics.list_padding * 2. + fixed > list_width
}

/// `1 object`, `5 objects`.
fn plural(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

#[expect(
    clippy::cast_precision_loss,
    reason = "a few characters, far below f32's exact range"
)]
fn char_count(text: &str) -> f32 {
    text.chars().count() as f32
}

/// Accessors for the UI tests (`ui_tests`, Linux only).
#[cfg(all(test, target_os = "linux"))]
impl RecordList {
    /// The lines as built for the current snapshot.
    pub(crate) fn lines(&self) -> Vec<Line> {
        self.listing()
            .map(|listing| {
                listing
                    .lines
                    .iter()
                    .map(|keyed| keyed.line.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The view's choices, as kept.
    pub(crate) fn current_options(&self, cx: &App) -> Options {
        self.options(cx)
    }

    /// The counts.
    pub(crate) fn summary(&self) -> Option<threads::Summary> {
        self.listing().map(|listing| listing.summary.clone())
    }

    /// The marked lines' keys, in line order.
    pub(crate) fn marked(&self) -> Vec<ItemKey> {
        self.selection.marked_keys()
    }

    /// The cursor's line.
    pub(crate) fn cursor(&self) -> Option<usize> {
        self.selection.cursor()
    }

    /// The object in the pane beside the view.
    pub(crate) fn pane_object(&self, cx: &App) -> Option<ObjectKey> {
        Some(self.pane.as_ref()?.view.read(cx).object().clone())
    }

    /// The pane beside the view.
    pub(crate) fn pane(&self) -> Option<Entity<ObjectPane>> {
        self.pane.as_ref().map(|pane| pane.view.clone())
    }

    /// The lines built in the last frame.
    pub(crate) fn visible_lines(&self) -> Range<usize> {
        self.visible.clone()
    }

    /// Sets the choices (a test flips *only mine* or picks a chip).
    pub(crate) fn edit_options(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut Options)) {
        self.set_options(edit, cx);
    }

    /// *fold every object* (or *unfold every object*) from the menu.
    pub(crate) fn fold_every_object(&mut self, fold: bool, cx: &mut Context<Self>) {
        self.fold_all(fold, cx);
    }

    /// The middle of line `index`, from the top of the lines (scrolled to
    /// the top).
    pub(crate) fn line_middle(&self, index: usize) -> Option<Pixels> {
        let layout = self.layout.as_ref()?;
        Some((layout.top(index) + layout.top(index + 1)) / 2.)
    }
}
