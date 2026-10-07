//! The list view of topic 07: every downtime, comment or acknowledged
//! problem of the environment, as a tab in the sidebar's *open* section.
//! It is laid out like a dashboard: the header (`downtimes  prod-cluster ·
//! every object … only mine  ends soonest ↑  ···`), the summary bar, the
//! rows (only those on screen are built, so thousands cost the same as
//! ten), the selection bar while rows are marked, and the cursor row's
//! object in the pane beside it.
//!
//! Selection, the selection bar and the keys work as in every list (`j`
//! / `k`, `x`, shift-click, `ctrl-a`, `esc`); `⌫` removes the marked rows
//! (or the cursor's), always through a confirmation that lists every
//! target; `→` and `←` unfold and fold a host's downtime with its
//! services. The rows follow the engine's snapshot, so changes from the
//! event stream (another operator's downtime, an acknowledgement that
//! expired) show at once.

use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::Arc;

use gpui::{
    Action, AnyElement, App, AppContext as _, ClickEvent, ClipboardItem, Context, ElementId,
    Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement as _, IntoElement, KeyBinding,
    Modifiers, MouseButton, ParentElement as _, Pixels, Render, ScrollStrategy,
    StatefulInteractiveElement as _, Styled as _, Subscription, UniformListScrollHandle, Window,
    div, prelude::FluentBuilder as _, relative, uniform_list,
};
use ic_model::{ObjectKey, Timestamp};
use ic_ui_kit::{
    ActiveTheme as _, Button, CHAR_WIDTH, Density, EmptyState, GlyphButton, Icon, IconName,
    KeyHint, Link, ListRow, Menu, MenuItem, Metrics, PaneHeader, Popover, RowEmphasis, Scrollbar,
    StateCircle, SummaryBar, SummaryItem, Switch, Theme, Tooltip, px,
};

use super::model::{self, KindSlot, Line, ListKind, Listing, Options, RecordKey, RowText, Tag};
use super::removal::{self, BulkRemoval};
use crate::actions::{
    Acknowledge, ActionRequest, AddComment, CheckNow, Dismiss, ExtendSelectionNext,
    ExtendSelectionPrevious, MarkAll, ObjectAction, OpenAsTab, OpenSelected, ScheduleDowntime,
    SelectFirst, SelectLast, SelectNext, SelectPageDown, SelectPageUp, SelectPrevious, ToggleMark,
};
use crate::app_state::AppState;
use crate::banner;
use crate::chrome::{Controls, WindowDrag};
use crate::dashboard::selection::ListSelection;
use crate::dashboard::{HeaderMenu, HeaderMenus, SplitLayout};
use crate::menu_state::down_position;
use crate::operate::expression;
use crate::pane::{ObjectPane, PaneEvent, PaneMode};
use crate::workspace::sidebar_reopen;

/// Key context of a list (with the dashboard list's, whose keys it shares).
pub(crate) const LIST_CONTEXT: &str = "RecordList";

/// The selection bar's height (as the dashboard's).
const SELECTION_BAR_HEIGHT: f32 = 40.;
/// The selection count's slot, in characters (`999 selected`).
const COUNT_SLOT_CHARS: f32 = 12.;
/// The downtime tag's progress line.
const PROGRESS_WIDTH: f32 = 56.;
/// The downtime tag's time slot, in characters (`by Sat 06:00`).
const TIME_SLOT_CHARS: f32 = 12.;
/// The comment tag's detail slot, in characters (`expires Fri 12:00`).
const DETAIL_SLOT_CHARS: f32 = 17.;
/// The acknowledged tag's slots, as drawn: `sticky` and the expiry.
const STICKY_SLOT: f32 = 48.;
const EXPIRY_SLOT: f32 = 136.;
/// Space between a tag's slots.
const SLOT_GAP: f32 = 10.;

/// Removes the marked rows, or the cursor's (`⌫`): asks first, listing
/// every target.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct RemoveSelected;

/// Shows a host's downtime with its services' rows under it (`→`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct Unfold;

/// Folds a host's downtime into one row again (`←`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct Fold;

/// Turns *only mine* on or off (`m`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct ToggleOnlyMine;

/// Sorts by the next order the list offers (`s`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct NextSort;

/// Shows or hides downtime and flapping comments in the comment list (`h`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct ToggleSystemComments;

/// Registers the lists' own keys; the dashboard list's keys apply too.
pub(crate) fn bind_keys(cx: &mut App) {
    let list = Some(LIST_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("m", ToggleOnlyMine, list),
        KeyBinding::new("s", NextSort, list),
        KeyBinding::new("h", ToggleSystemComments, list),
        KeyBinding::new("backspace", RemoveSelected, list),
        KeyBinding::new("delete", RemoveSelected, list),
        KeyBinding::new("right", Unfold, list),
        KeyBinding::new("left", Fold, list),
    ]);
}

/// What the list asks the workspace to do.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum RecordListEvent {
    /// Ask whether to remove these, listing every target.
    Remove(BulkRemoval),
}

/// The pane open beside the list.
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
            && self.author == other.author
            && self.minute == other.minute
    }
}

/// A list of every downtime, comment or acknowledged problem.
pub(crate) struct RecordList {
    state: Entity<AppState>,
    kind: ListKind,
    focus_handle: FocusHandle,
    options: Options,
    built: Option<(Inputs, Listing)>,
    selection: ListSelection<Line>,
    scroll: UniformListScrollHandle,
    pane: Option<OpenPane>,
    menus: HeaderMenus,
    sidebar_open: bool,
    drag: WindowDrag,
    /// The rows built in the last frame (a page's size).
    visible: Range<usize>,
    /// Where the selection bar's buttons start, as last drawn, for tests.
    #[cfg(test)]
    pub(crate) selection_buttons_x: std::rc::Rc<std::cell::Cell<Option<Pixels>>>,
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
        let options = Options::saved(kind, &state.read(cx).list_options(kind));
        Self {
            state,
            kind,
            focus_handle: cx.focus_handle(),
            options,
            built: None,
            selection: ListSelection::default(),
            scroll: UniformListScrollHandle::new(),
            pane: None,
            menus: HeaderMenus::default(),
            sidebar_open: true,
            drag: WindowDrag::default(),
            visible: 0..0,
            #[cfg(test)]
            selection_buttons_x: std::rc::Rc::default(),
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

    /// Builds the listing again if what it shows changed, and moves the
    /// selection onto its rows. Cheap when nothing changed.
    fn sync(&mut self, cx: &App) {
        let state = self.state.read(cx);
        let snapshot = state.snapshot();
        let now = Timestamp::now();
        #[expect(
            clippy::cast_possible_truncation,
            reason = "minutes since 1970 fit in i64"
        )]
        let minute = (now.as_unix_seconds() / 60.).floor() as i64;
        // *only mine* can't apply where it can't tell whose a record is.
        let mut options = self.options.clone();
        if state.only_mine_denial(self.kind).is_some() {
            options.only_mine = false;
        }
        let inputs = Inputs {
            environment: state.active_environment_id().map(str::to_owned),
            revision: snapshot.revision,
            downtimes: Arc::clone(&snapshot.downtimes),
            comments: Arc::clone(&snapshot.comments),
            hosts: Arc::clone(&snapshot.hosts),
            services: Arc::clone(&snapshot.services),
            options: options.clone(),
            author: state.author().to_owned(),
            minute,
        };
        if self
            .built
            .as_ref()
            .is_some_and(|(built, _)| *built == inputs)
        {
            return;
        }
        let listing = model::build(self.kind, snapshot, &inputs.author, &options, now);
        self.selection.update_rows(&listing.rows);
        self.built = Some((inputs, listing));
    }

    /// The options the listing was built with: *only mine* is off where
    /// it can't apply (see [`AppState::only_mine_denial`]).
    fn shown_options(&self) -> &Options {
        self.built
            .as_ref()
            .map_or(&self.options, |(inputs, _)| &inputs.options)
    }

    /// The listing, built for the current snapshot.
    fn listing(&self) -> Option<&Listing> {
        self.built.as_ref().map(|(_, listing)| listing)
    }

    // --- Selection and keys --------------------------------------------

    /// Runs `change` on the selection; scrolls to the cursor and, if the
    /// pane is open, shows the cursor's object in it.
    fn change_selection(
        &mut self,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut ListSelection<Line>) -> Option<usize>,
    ) {
        self.sync(cx);
        if let Some(index) = change(&mut self.selection) {
            self.scroll.scroll_to_item(index, ScrollStrategy::Nearest);
            if let (Some(pane), Some(object)) = (&self.pane, self.cursor_object()) {
                pane.view.update(cx, |pane, cx| pane.show(object, cx));
            }
        }
        cx.notify();
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

    fn toggle_mark(&mut self, _: &ToggleMark, _: &mut Window, cx: &mut Context<Self>) {
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
        if let Some(object) = self.cursor_object() {
            self.open_pane(object, cx);
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

    fn unfold(&mut self, _: &Unfold, _: &mut Window, cx: &mut Context<Self>) {
        self.sync(cx);
        let Some(Line::Downtime {
            key: RecordKey::Downtime(name),
            children,
            child: false,
            ..
        }) = self.cursor_line()
        else {
            return;
        };
        if children.total() > 0 && self.options.unfolded.insert(name) {
            cx.notify();
        }
    }

    fn fold(&mut self, _: &Fold, _: &mut Window, cx: &mut Context<Self>) {
        self.sync(cx);
        let Some(line) = self.cursor_line() else {
            return;
        };
        let parent = match (
            &line,
            self.listing().and_then(|listing| listing.downtime(&line)),
        ) {
            (Line::Downtime { child: true, .. }, Some(downtime)) => downtime.parent.clone(),
            (
                Line::Downtime {
                    key: RecordKey::Downtime(name),
                    ..
                },
                _,
            ) => Some(name.clone()),
            _ => None,
        };
        if let Some(parent) = parent
            && self.options.unfolded.remove(&parent)
        {
            self.sync(cx);
            let key = RecordKey::Downtime(parent);
            if self.selection.select_key(&key)
                && let Some(index) = self.selection.cursor()
            {
                self.scroll.scroll_to_item(index, ScrollStrategy::Nearest);
            }
            cx.notify();
        }
    }

    /// A click on `+ 18 services`: unfolds or folds that host's downtime.
    fn toggle_unfold(&mut self, name: &str, cx: &mut Context<Self>) {
        if !self.options.unfolded.remove(name) {
            self.options.unfolded.insert(name.to_owned());
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

    fn add_comment(&mut self, _: &AddComment, _: &mut Window, cx: &mut Context<Self>) {
        self.request(ObjectAction::AddComment, cx);
    }

    /// An object action's key: on the pane's object, else the cursor's.
    /// While rows are marked the keys do nothing here (the marks are
    /// downtimes, comments or acknowledgements, not objects to act on), and
    /// the pane's buttons show no keys.
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

    fn cursor_line(&self) -> Option<Line> {
        let index = self.selection.cursor()?;
        self.selection.rows().get(index).cloned()
    }

    fn cursor_object(&self) -> Option<ObjectKey> {
        self.cursor_line()?.object().cloned()
    }

    /// The marked rows' keys, else the cursor's.
    fn targets(&self) -> Vec<RecordKey> {
        let marked = self.selection.marked_keys();
        if marked.is_empty() {
            self.selection.cursor_key().cloned().into_iter().collect()
        } else {
            marked
        }
    }

    /// Asks the workspace to confirm removing the marked rows (or the
    /// cursor's), listing every target.
    fn ask_removal(&mut self, cx: &mut Context<Self>) {
        self.sync(cx);
        let targets = self.targets();
        if targets.is_empty() {
            return;
        }
        if let Some(denial) = self
            .state
            .read(cx)
            .action_denial(&self.kind.removal_action())
        {
            self.state.update(cx, |state, cx| {
                state.inform(format!("Can't {}", self.kind.removal_label()), Some(denial));
                cx.notify();
            });
            return;
        }
        let removal = {
            let snapshot = self.state.read(cx).snapshot();
            let now = Timestamp::now();
            match self.kind {
                ListKind::Downtimes => removal::downtimes(snapshot, &names(&targets), now),
                ListKind::Comments => removal::comments(snapshot, &names(&targets), now),
                ListKind::Acknowledged => {
                    let objects: Vec<ObjectKey> = targets
                        .iter()
                        .filter_map(|key| match key {
                            RecordKey::Problem(object) => Some(object.clone()),
                            _ => None,
                        })
                        .collect();
                    removal::acknowledgements(snapshot, &objects, now)
                }
            }
        };
        cx.emit(RecordListEvent::Remove(removal));
    }

    /// A click on `key`'s row, drawn as row `index`: plain clicks put the
    /// cursor there and open its object in the pane, shift extends the
    /// marks from the anchor, ctrl/cmd toggles the row's mark.
    fn click_row(
        &mut self,
        index: usize,
        key: &RecordKey,
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
        } else if self.selection.select(index)
            && let Some(object) = self.cursor_object()
        {
            self.open_pane(object, cx);
        }
    }

    /// Opens `object`'s pane beside the list, or shows it in the open one.
    fn open_pane(&mut self, object: ObjectKey, cx: &mut Context<Self>) {
        if let Some(pane) = &self.pane {
            pane.view.update(cx, |pane, cx| pane.open(object, cx));
        } else {
            let state = self.state.clone();
            let sidebar_open = self.sidebar_open;
            let view = cx.new(|cx| {
                let mut pane = ObjectPane::new(state, object, PaneMode::Split, cx);
                pane.set_sidebar_open(sidebar_open, cx);
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

    /// Whether rows are marked (the selection bar shows; toasts float
    /// above it).
    pub(crate) fn has_marks(&self) -> bool {
        self.selection.marked_count() > 0
    }

    /// Changes the options and keeps them with the environment's UI state
    /// (they come back when the list opens again, after a restart too).
    fn set_options(&mut self, change: impl FnOnce(&mut Options), cx: &mut Context<Self>) {
        change(&mut self.options);
        self.menus.close();
        self.save_options(cx);
        cx.notify();
    }

    fn save_options(&self, cx: &mut Context<Self>) {
        let (kind, saved) = (self.kind, self.options.to_saved(self.kind));
        self.state
            .update(cx, |state, _| state.set_list_options(kind, saved));
    }

    fn toggle_only_mine(&mut self, _: &ToggleOnlyMine, _: &mut Window, cx: &mut Context<Self>) {
        if self.state.read(cx).only_mine_denial(self.kind).is_some() {
            return;
        }
        self.set_options(|options| options.only_mine = !options.only_mine, cx);
    }

    fn next_sort(&mut self, _: &NextSort, _: &mut Window, cx: &mut Context<Self>) {
        let kind = self.kind;
        self.set_options(|options| options.sort = options.next_sort(kind), cx);
    }

    fn toggle_system_comments(
        &mut self,
        _: &ToggleSystemComments,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.kind == ListKind::Comments {
            self.set_options(
                |options| options.system_comments = !options.system_comments,
                cx,
            );
        }
    }

    fn copy(&self, what: &str, text: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.state.update(cx, |state, cx| {
            state.inform(format!("Copied {what}"), None);
            cx.notify();
        });
    }

    /// The objects of the marked rows (else the listed rows'), each once,
    /// in row order.
    fn objects_of(&self, keys: &[RecordKey]) -> Vec<ObjectKey> {
        let wanted: std::collections::HashSet<&RecordKey> = keys.iter().collect();
        let mut seen = std::collections::HashSet::new();
        (0..self.selection.rows().len())
            .filter_map(|index| self.selection.rows().get(index))
            .filter(|line| wanted.is_empty() || line.key().is_some_and(|key| wanted.contains(key)))
            .filter_map(Line::object)
            .filter(|object| seen.insert((*object).clone()))
            .cloned()
            .collect()
    }
}

use crate::dashboard::selection::SelectableRow as _;

/// The names of downtime or comment keys.
fn names(keys: &[RecordKey]) -> Vec<String> {
    keys.iter()
        .filter_map(|key| match key {
            RecordKey::Downtime(name) | RecordKey::Comment(name) => Some(name.clone()),
            RecordKey::Problem(_) => None,
        })
        .collect()
}

// --- Rendering ----------------------------------------------------------

impl RecordList {
    fn render_header(&self, window: &Window, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let state = self.state.read(cx);
        let controls = Controls::of(window, cx);
        let mut header = PaneHeader::new("list-header").padding(theme.metrics.list_padding);
        if !self.sidebar_open {
            header = header.leading(sidebar_reopen(controls, theme));
        }
        let environment = state
            .environment()
            .map(|environment| environment.name.clone())
            .unwrap_or_default();
        let denial = state.only_mine_denial(self.kind);
        let only_mine = self.options.only_mine && denial.is_none();
        let subtitle = if self.pane.is_some() {
            environment
        } else if self.kind == ListKind::Acknowledged && state.ack_detail_denial().is_some() {
            format!("{environment} · who and why need objects/query/Comment")
        } else if only_mine {
            let by = match self.kind {
                ListKind::Acknowledged => "acknowledged by",
                ListKind::Downtimes | ListKind::Comments => "set by",
            };
            format!("{environment} · {by} {}", state.author())
        } else {
            format!("{environment} · {}", self.kind.every())
        };
        let author = state.author().to_owned();
        let switch = Switch::new("only-mine", only_mine)
            .label("only mine")
            .disabled(denial.is_some())
            .on_change(cx.listener(|this, on: &bool, _, cx| {
                let on = *on;
                this.set_options(|options| options.only_mine = on, cx);
            }));
        let switch = div()
            .id("only-mine-slot")
            .flex_none()
            .mr(px(6.))
            .child(switch)
            .tooltip(Tooltip::text(denial.unwrap_or_else(|| {
                format!("Only what {author} set (the environment's author) · m")
            })));
        let header = header
            .title(self.kind.title())
            .subtitle(subtitle)
            .child(switch)
            .child(self.sort_trigger(cx))
            .child(self.options_trigger(cx));
        self.drag
            .attach(div().id("list-header-drag").child(header), controls)
            .into_any_element()
    }

    fn sort_trigger(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let open = self.menus.open() == Some(HeaderMenu::Sort);
        div()
            .relative()
            .flex_none()
            .child(
                div()
                    .id("list-sort-trigger")
                    .text_size(theme.text.small)
                    .text_color(if open { colors.text } else { colors.text_muted })
                    .cursor_pointer()
                    .hover(|style| style.text_color(colors.text))
                    .child(self.options.sort.label())
                    .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                    .on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
                        this.menus.toggle(HeaderMenu::Sort, down_position(event));
                        cx.notify();
                    })),
            )
            .when(open, |trigger| {
                let mut menu = Menu::new("list-sort-menu").label("sort by");
                for sort in self.kind.sorts() {
                    let sort = *sort;
                    menu = menu.item(
                        MenuItem::new(sort.id(), sort.menu_label())
                            .checked(sort == self.options.sort)
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.set_options(|options| options.sort = sort, cx);
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
                trigger.tooltip(Tooltip::new("List options"))
            })
            .when(open, |trigger| {
                trigger.child(Popover::new(self.options_menu(cx)).align_right())
            })
            .into_any_element()
    }

    fn options_menu(&self, cx: &Context<Self>) -> Menu {
        let objects = self.objects_of(&[]);
        let mut menu = Menu::new("list-options-menu");
        if self.kind == ListKind::Comments {
            let shown = self.options.system_comments;
            menu = menu
                .item(
                    MenuItem::new(
                        "list-system-comments",
                        "show downtime and flapping comments",
                    )
                    .checked(shown)
                    .on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            this.set_options(|options| options.system_comments = !shown, cx);
                        },
                    )),
                )
                .separator();
        }
        let names = expression::names(&objects);
        let filter = expression::filter(&objects);
        let none = objects.is_empty();
        menu.item(
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
            MenuItem::new("list-close", "close list").on_click(cx.listener(
                |this, _: &ClickEvent, _, cx| {
                    this.menus.close();
                    let kind = this.kind;
                    this.state.update(cx, |state, cx| {
                        if state.close_list(kind) {
                            cx.notify();
                        }
                    });
                },
            )),
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

    /// The summary bar: what the list counts, and at the end what it
    /// leaves out.
    #[expect(
        clippy::too_many_lines,
        reason = "one arm per list, each its items as drawn"
    )]
    fn render_summary(&self, list_width: Pixels, cx: &Context<Self>) -> Option<AnyElement> {
        let theme = cx.theme();
        let colors = theme.colors;
        let listing = self.listing()?;
        let summary = &listing.summary;
        let texts = summary_texts(self.kind, summary);
        let end_text = summary_end(self.kind, summary, self.shown_options());
        let fits = |width: Pixels| {
            SummaryBar::fits(texts.iter().map(String::as_str), &end_text, width, theme)
        };
        // As drawn, the end may take half the right padding before the
        // items shorten.
        let half = theme.metrics.list_padding / 2.;
        let (compact, snug) = if fits(list_width) {
            (false, px(0.))
        } else if fits(list_width + half) {
            (false, half)
        } else {
            (true, px(0.))
        };
        let icon_item = |icon: IconName, color: gpui::Hsla, text: String| {
            let text = if compact {
                text.split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .to_owned()
            } else {
                text
            };
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(7.))
                .child(Icon::new(icon).size(px(11.)).color(color))
                .child(text)
                .into_any_element()
        };
        let by_others = (self.shown_options().only_mine && summary.by_others > 0)
            .then(|| format!("{} by others hidden", summary.by_others));
        let mut items: Vec<AnyElement> = Vec::new();
        let end: Option<AnyElement> = match self.kind {
            ListKind::Downtimes => {
                items.push(
                    SummaryItem::with_color(
                        colors.accent,
                        u32::try_from(summary.in_effect).unwrap_or(u32::MAX),
                        "in effect",
                    )
                    .compact(compact)
                    .into_any_element(),
                );
                items.push(
                    SummaryItem::with_color(
                        theme.states.fill.pending,
                        u32::try_from(summary.upcoming).unwrap_or(u32::MAX),
                        "upcoming",
                    )
                    .compact(compact)
                    .into_any_element(),
                );
                if summary.from_config > 0 {
                    items.push(icon_item(
                        IconName::Lock,
                        colors.text_faint,
                        format!("{} from config", summary.from_config),
                    ));
                }
                let text = by_others.or_else(|| {
                    (summary.folded > 0).then(|| format!("{} folded into hosts", summary.folded))
                });
                text.map(|text| div().child(text).into_any_element())
            }
            ListKind::Comments => {
                items.push(icon_item(
                    IconName::MessageSquare,
                    colors.text_faint,
                    plural(summary.comments, "comment", "comments"),
                ));
                items.push(icon_item(
                    IconName::Check,
                    colors.accent_text,
                    plural(
                        summary.acknowledgements,
                        "acknowledgement",
                        "acknowledgements",
                    ),
                ));
                let shown = self.options.system_comments;
                let toggle = if shown {
                    "downtime and flapping comments shown"
                } else {
                    "downtime and flapping comments hidden"
                };
                let longest = "downtime and flapping comments hidden";
                let toggle = div()
                    .id("system-comments-toggle")
                    .min_w(theme.text.small * (CHAR_WIDTH * char_count(longest)))
                    .text_right()
                    .cursor_pointer()
                    .hover(|style| style.text_color(colors.text_muted))
                    .child(toggle)
                    .tooltip(Tooltip::text(format!(
                        "{} downtime and flapping comments; the downtime list and the objects' \
                         state already say what they do",
                        summary.system
                    )))
                    .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.set_options(|options| options.system_comments = !shown, cx);
                    }));
                Some(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(14.))
                        .children(by_others.map(|text| div().child(text)))
                        .child(toggle)
                        .into_any_element(),
                )
            }
            ListKind::Acknowledged => {
                let word = |state: ic_model::CheckableState| crate::format::state_word(state);
                for (state, count) in &summary.states {
                    items.push(
                        SummaryItem::new(
                            *state,
                            u32::try_from(*count).unwrap_or(u32::MAX),
                            word(*state),
                        )
                        .compact(compact)
                        .into_any_element(),
                    );
                }
                let text = by_others.unwrap_or_else(|| {
                    format!("{} sticky · {} expire", summary.sticky, summary.expiring)
                });
                Some(div().child(text).into_any_element())
            }
        };
        let mut bar = SummaryBar::new().children(items);
        if let Some(end) = end {
            bar = bar.end(div().mr(-snug).child(end));
        }
        Some(bar.into_any_element())
    }

    /// Builds the rows in `range` (the ones on screen).
    fn render_rows(&mut self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        self.visible = range.clone();
        let Some(listing) = self.listing().cloned() else {
            return Vec::new();
        };
        let state = self.state.read(cx);
        let snapshot = state.snapshot().clone();
        let times = state.appearance().list_times;
        let theme = cx.theme().clone();
        let now = Timestamp::now();
        let cursor = self.selection.cursor();
        let sort = self.options.sort;
        range
            .filter_map(|index| {
                let line = listing.rows.get(index)?;
                if let Line::Section { section, count } = line {
                    let row = ListRow::new(ElementId::Name(
                        format!("list-section-{}", section.title(0)).into(),
                    ))
                    .header(true)
                    .leading(
                        Icon::new(section.icon())
                            .size(px(14.))
                            .color(theme.colors.text_muted),
                    )
                    .title(section.title(*count));
                    let row = if theme.density == Density::Compact {
                        row.tag(section.detail(sort))
                    } else {
                        row.detail(section.detail(sort))
                    };
                    return Some(row.into_any_element());
                }
                let key = line.key()?.clone();
                let text = model::row_text(&listing, line, &snapshot, times, now)?;
                let emphasis =
                    RowEmphasis::new(cursor == Some(index), self.selection.is_marked(&key));
                let pending = line
                    .object()
                    .and_then(|object| self.pending_removal(object, cx));
                let row = record_row(&key, &text, pending, &theme);
                let row = match (&text.more, line) {
                    (
                        Some(more),
                        Line::Downtime {
                            key: RecordKey::Downtime(name),
                            ..
                        },
                    ) => {
                        let unfolded = self.options.unfolded.contains(name);
                        let name = name.clone();
                        row.after_title(
                            div()
                                .id(ElementId::Name(format!("unfold:{name}").into()))
                                .text_size(theme.text.row)
                                .text_color(theme.colors.text_faint)
                                .cursor_pointer()
                                .hover(|style| style.text_color(theme.colors.text_muted))
                                .child(if unfolded {
                                    more.replacen('+', "−", 1)
                                } else {
                                    more.clone()
                                })
                                .tooltip(Tooltip::text(if unfolded {
                                    "Fold them into the host's row (←)"
                                } else {
                                    "List them under the host's row (→)"
                                }))
                                .on_mouse_down(MouseButton::Left, |_, window, cx| {
                                    window.prevent_default();
                                    cx.stop_propagation();
                                })
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    cx.stop_propagation();
                                    this.toggle_unfold(&name, cx);
                                })),
                        )
                    }
                    _ => row,
                };
                let clicked = key.clone();
                Some(
                    row.emphasis(emphasis)
                        .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                            this.click_row(index, &clicked, event.modifiers(), window, cx);
                        }))
                        .into_any_element(),
                )
            })
            .collect()
    }

    /// The marker of a removal on its way for `object`'s rows in this list
    /// (`removing downtime…`).
    fn pending_removal(&self, object: &ObjectKey, cx: &App) -> Option<&'static str> {
        let (action, label) = self.state.read(cx).pending_action(object)?;
        let ours = matches!(
            (self.kind, action),
            (
                ListKind::Downtimes,
                ObjectAction::RemoveDowntimes
                    | ObjectAction::RemoveDowntime(_)
                    | ObjectAction::RemoveNamedDowntimes(_),
            ) | (ListKind::Comments, ObjectAction::RemoveComments(_))
                | (ListKind::Acknowledged, ObjectAction::RemoveAcknowledgement)
        );
        ours.then_some(label)
    }

    fn render_body(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let state = self.state.read(cx);
        if state.environment().is_none() {
            return EmptyState::new("No environment yet")
                .detail("Add an Icinga environment to see its downtimes, comments and acknowledgements.")
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
        if listing.rows.is_empty() {
            if state.has_no_objects() && state.connection().is_starting() {
                return banner::loading_body(state, cx);
            }
            return self.empty(listing, cx);
        }
        let rows = uniform_list(
            "record-rows",
            listing.rows.len(),
            cx.processor(|this, range: Range<usize>, _window, cx| this.render_rows(range, cx)),
        )
        .track_scroll(&self.scroll)
        .size_full();
        div()
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(rows)
            .child(Scrollbar::vertical(&self.scroll))
            .into_any_element()
    }

    /// The body of an empty list: what's not there, and what is hidden.
    fn empty(&self, listing: &Listing, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let summary = &listing.summary;
        let (title, detail) = match self.kind {
            ListKind::Downtimes => (
                "No downtimes",
                "Nothing is in downtime, and none is scheduled.",
            ),
            ListKind::Comments => ("No comments", "No host or service has a comment."),
            ListKind::Acknowledged => (
                "No acknowledged problems",
                "No problem is acknowledged right now.",
            ),
        };
        let leading = Icon::new(self.kind.icon())
            .size(px(20.))
            .color(theme.colors.text_muted);
        if self.shown_options().only_mine && summary.by_others > 0 {
            let author = self.state.read(cx).author().to_owned();
            return EmptyState::new(format!("None set by {author}"))
                .leading(leading)
                .detail(format!(
                    "{} by others {} hidden.",
                    summary.by_others,
                    if summary.by_others == 1 { "is" } else { "are" }
                ))
                .child(Link::new("list-show-all", "show all").on_click(cx.listener(
                    |this, _: &ClickEvent, _, cx| {
                        this.set_options(|options| options.only_mine = false, cx);
                    },
                )))
                .into_any_element();
        }
        if self.kind == ListKind::Comments && summary.system > 0 && !self.options.system_comments {
            return EmptyState::new(title)
                .leading(leading)
                .detail(format!(
                    "{} downtime and flapping {} hidden.",
                    summary.system,
                    if summary.system == 1 {
                        "comment is"
                    } else {
                        "comments are"
                    }
                ))
                .child(
                    Link::new("list-show-system", "show them").on_click(cx.listener(
                        |this, _: &ClickEvent, _, cx| {
                            this.set_options(|options| options.system_comments = true, cx);
                        },
                    )),
                )
                .into_any_element();
        }
        EmptyState::new(title)
            .leading(leading)
            .detail(detail)
            .into_any_element()
    }

    /// The bar under the list while rows are marked: the count in a fixed
    /// slot, then this list's actions, so the buttons never move.
    #[expect(
        clippy::too_many_lines,
        reason = "the bar as drawn, its count probed in tests"
    )]
    fn render_selection_bar(&self, list_width: Pixels, cx: &Context<Self>) -> Option<AnyElement> {
        let marked = self.selection.marked_keys();
        if marked.is_empty() {
            return None;
        }
        let theme = cx.theme();
        let colors = theme.colors;
        let compact = bar_is_compact(self.kind, list_width, theme);
        let state = self.state.read(cx);
        let listing = self.listing();
        // Every marked downtime from the config: nothing to remove.
        let only_config = self.kind == ListKind::Downtimes
            && listing.is_some_and(|listing| {
                marked.iter().all(|key| {
                    (0..listing.rows.len())
                        .filter_map(|index| listing.rows.get(index))
                        .filter(|line| line.key() == Some(key))
                        .filter_map(|line| listing.downtime(line))
                        .all(|downtime| downtime.config_owned)
                })
            });
        let mut remove = Button::new("list-remove", self.kind.removal_label()).primary();
        if !compact {
            remove = remove.key_hint("⌫");
        }
        let remove = if let Some(denial) = state.action_denial(&self.kind.removal_action()) {
            remove.disabled(true).tooltip(Tooltip::new(denial))
        } else if only_config {
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
                        .min_w((theme.text.small * (COUNT_SLOT_CHARS * CHAR_WIDTH)).ceil() + px(1.))
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

    /// The list's column: header, banners, summary, rows, selection bar.
    fn render_column(
        &mut self,
        list_width: Pixels,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let header = self.render_header(window, cx);
        let now = Timestamp::now();
        let banners = banner::banners(&self.state, now, true, cx);
        let progress = banner::progress(self.state.read(cx));
        let summary = self.render_summary(list_width, cx);
        let body = self.render_body(cx);
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
            .children(summary)
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
            .on_action(cx.listener(Self::toggle_system_comments))
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

/// A record's row: the object's circle (or the downtime's: hollow while
/// it's in effect), `service on host`, the second line, and the tag in its
/// fixed slots.
fn record_row(
    key: &RecordKey,
    text: &RowText,
    pending: Option<&'static str>,
    theme: &Theme,
) -> ListRow {
    let circle = match text.state {
        Some(state) => StateCircle::new(state).handled(text.hollow),
        None => StateCircle::with_color(theme.states.fill.pending),
    };
    let row = ListRow::new(ElementId::Name(format!("record:{}", key.name()).into()))
        .state(circle, text.caption.clone())
        .title(text.name.clone())
        .detail(text.line.clone())
        .trailing(tag_element(&text.tag, pending, text.child, theme));
    match &text.host {
        Some(host) => row.context("on", host.clone()),
        None => row,
    }
}

/// A tag's fixed slots: what changes width gets a slot sized for its
/// longest value, so the slots line up from row to row.
#[expect(
    clippy::too_many_lines,
    reason = "one arm per list, each its fixed slots"
)]
fn tag_element(tag: &Tag, pending: Option<&'static str>, child: bool, theme: &Theme) -> AnyElement {
    let colors = theme.colors;
    let label = theme.text.label;
    let chars = |count: f32| label * (count * CHAR_WIDTH);
    match tag {
        Tag::Downtime {
            progress,
            text,
            accent,
        } => {
            let slot = match progress {
                Some(fraction) => div()
                    .flex_none()
                    .w(px(PROGRESS_WIDTH))
                    .h(px(3.))
                    .rounded(px(2.))
                    .overflow_hidden()
                    .bg(colors.border_header)
                    .child(
                        div()
                            .h_full()
                            .w(relative(fraction.clamp(0., 1.)))
                            // A child's line is its parent's: faint.
                            .bg(if child {
                                colors.text_faint
                            } else {
                                colors.accent
                            }),
                    )
                    .into_any_element(),
                None => div()
                    .flex()
                    .flex_none()
                    .justify_end()
                    .w(px(PROGRESS_WIDTH))
                    .child(
                        Icon::new(IconName::Lock)
                            .size(px(11.))
                            .color(colors.text_faint),
                    )
                    .into_any_element(),
            };
            let (text, color) = match pending {
                Some(pending) => (pending.to_owned(), colors.text_muted),
                None if *accent => (text.clone(), colors.accent_text),
                None => (text.clone(), colors.text_faint),
            };
            div()
                .flex()
                .items_center()
                .gap(px(SLOT_GAP))
                .child(slot)
                .child(
                    div()
                        .flex_none()
                        .w(chars(TIME_SLOT_CHARS))
                        .text_right()
                        .text_color(color)
                        .child(text),
                )
                .into_any_element()
        }
        Tag::Comment { kind, detail } => {
            let icon_color = if *kind == KindSlot::Acknowledgement {
                colors.accent_text
            } else {
                colors.text_faint
            };
            let detail = pending.map_or_else(|| detail.clone(), str::to_owned);
            div()
                .flex()
                .items_center()
                .gap(px(SLOT_GAP))
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(px(6.))
                        .w(px(11. + 6.) + chars(char_count(KindSlot::LONGEST)))
                        .child(Icon::new(kind.icon()).size(px(11.)).color(icon_color))
                        .child(kind.word()),
                )
                .child(div().flex_none().w(chars(DETAIL_SLOT_CHARS)).child(detail))
                .into_any_element()
        }
        Tag::Ack { sticky, expiry } => {
            let expiry = pending.map_or_else(|| expiry.clone(), str::to_owned);
            div()
                .flex()
                .items_center()
                .gap(px(SLOT_GAP))
                .child(
                    div()
                        .flex_none()
                        .w(px(STICKY_SLOT).max(chars(6.)))
                        .text_right()
                        .child(if *sticky { "sticky" } else { "" }),
                )
                .child(
                    div()
                        .flex_none()
                        .w(px(EXPIRY_SLOT).max(chars(DETAIL_SLOT_CHARS)))
                        .text_right()
                        .child(expiry),
                )
                .into_any_element()
        }
    }
}

/// What the summary bar's items read in full, to see whether they fit.
fn summary_texts(kind: ListKind, summary: &model::ListSummary) -> Vec<String> {
    match kind {
        ListKind::Downtimes => {
            let mut texts = vec![
                format!("{} in effect", summary.in_effect),
                format!("{} upcoming", summary.upcoming),
            ];
            if summary.from_config > 0 {
                texts.push(format!("{} from config", summary.from_config));
            }
            texts
        }
        ListKind::Comments => vec![
            plural(summary.comments, "comment", "comments"),
            plural(
                summary.acknowledgements,
                "acknowledgement",
                "acknowledgements",
            ),
        ],
        ListKind::Acknowledged => summary
            .states
            .iter()
            .map(|(state, count)| format!("{count} {}", crate::format::state_word(*state)))
            .collect(),
    }
}

/// What the summary bar's end reads (its longest form for a toggle).
fn summary_end(kind: ListKind, summary: &model::ListSummary, options: &Options) -> String {
    let by_others = (options.only_mine && summary.by_others > 0)
        .then(|| format!("{} by others hidden", summary.by_others));
    match kind {
        ListKind::Downtimes => by_others
            .or_else(|| {
                (summary.folded > 0).then(|| format!("{} folded into hosts", summary.folded))
            })
            .unwrap_or_default(),
        ListKind::Comments => {
            let toggle = "downtime and flapping comments hidden";
            by_others.map_or_else(|| toggle.to_owned(), |text| format!("{text}  {toggle}"))
        }
        ListKind::Acknowledged => by_others
            .unwrap_or_else(|| format!("{} sticky · {} expire", summary.sticky, summary.expiring)),
    }
}

/// Whether the selection bar of a list `list_width` wide must drop its key
/// hints: what it shows, with the hints, in characters and padding.
fn bar_is_compact(kind: ListKind, list_width: Pixels, theme: &Theme) -> bool {
    // The count's slot, the main button, `copy names`, `clear`.
    let characters = COUNT_SLOT_CHARS + char_count(kind.removal_label()) + 10. + 5.;
    // The count's margin, five gaps, two buttons' padding, `⌫` with its
    // gap, `···`, `esc` with its gap.
    let fixed = px(4. + 5. * 8. + 2. * 25. + 22. + 24. + 6. + 26.);
    theme.text.small * (characters * CHAR_WIDTH) + theme.metrics.list_padding * 2. + fixed
        > list_width
}

/// `1 comment`, `5 comments`.
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
    /// The rows as built for the current snapshot.
    pub(crate) fn rows(&self) -> Vec<Line> {
        self.listing()
            .map(|listing| listing.rows.to_vec())
            .unwrap_or_default()
    }

    /// The list's choices.
    pub(crate) fn options(&self) -> &Options {
        &self.options
    }

    /// The summary's counts.
    pub(crate) fn summary(&self) -> Option<model::ListSummary> {
        self.listing().map(|listing| listing.summary.clone())
    }

    /// The marked rows' keys, in row order.
    pub(crate) fn marked(&self) -> Vec<RecordKey> {
        self.selection.marked_keys()
    }

    /// The cursor's row.
    pub(crate) fn cursor(&self) -> Option<usize> {
        self.selection.cursor()
    }

    /// The object in the pane beside the list.
    pub(crate) fn pane_object(&self, cx: &App) -> Option<ObjectKey> {
        Some(self.pane.as_ref()?.view.read(cx).object().clone())
    }

    /// The rows built in the last frame.
    pub(crate) fn visible_rows(&self) -> Range<usize> {
        self.visible.clone()
    }

    /// Sets the options (a test flips *only mine* or the sort).
    pub(crate) fn edit_options(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut Options)) {
        edit(&mut self.options);
        self.save_options(cx);
        cx.notify();
    }
}
