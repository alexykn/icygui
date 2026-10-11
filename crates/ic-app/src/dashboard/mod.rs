//! The main area for a dashboard: its page (screen 2a; topic 04: every
//! view stacked, each under its header) and the detail pane beside it
//! (screens 2b and 2c). Where the window is too narrow for both, the pane
//! narrows down to a minimum and then covers the page ([`SplitLayout`]).
//!
//! The page ([`page::Page`]) is one list of items with exact heights: a
//! single list view shows as rc1 did (its rows under the dashboard's
//! header and summary bar); a dashboard with several views shows each view
//! under its 36px header. The page scrolls as a whole and builds only the
//! items on screen, so nothing per frame scales with the number of rows.
//! One cursor moves through every view ([`cursor::PageSelection`]); it
//! follows its row, band or host by identity across snapshot updates.
//! Every dashboard keeps its own cursor, marks, folds, scroll position and
//! open pane.
//!
//! Grouped lists are the host-with-services style (README, *Rules that
//! span the topics*): a band per host (its state dot in the rows' mark
//! column, name, address and status, per-state counts), its services as
//! plain rows without indent, up to seven of them (`+ N more` shows the
//! rest in place), and a chevron that only collapses or expands the host;
//! a click anywhere else on the band opens the host in the pane.

pub(crate) mod bulk;
mod comments;
pub(crate) mod cursor;
pub(crate) mod draw;
pub(crate) mod header;
pub(crate) mod page;
pub(crate) mod rows;
pub(crate) mod selection;
pub(crate) mod view_controls;

pub(crate) use self::bulk::SELECTION_BAR_HEIGHT;
pub(crate) use self::header::{HeaderMenu, HeaderMenus};

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, ElementId, Entity, FocusHandle,
    Focusable, InteractiveElement as _, IntoElement, Modifiers, ParentElement as _, Pixels, Render,
    ScrollHandle, Styled as _, Subscription, Task, UniformListScrollHandle, Window, div, point,
    prelude::FluentBuilder as _,
};
use ic_config::{ListTimes, ObjectKind, View};
use ic_core::snapshot::{DashboardResult, DashboardRow, ViewResult};
use ic_model::{ObjectKey, Timestamp};
use ic_rules::DashboardRef;
use ic_ui_kit::{
    ActiveTheme as _, CircleSize, CodeBlock, EmptyState, Icon, IconName, Link, ListRow, Metrics,
    StateCircle, Theme, px,
};

use self::cursor::PageSelection;
use self::page::{Folds, GroupFilter, Id, ItemKind, Page, PageInput, Sizes, Stop};
use crate::actions::{
    Acknowledge, ActionRequest, AddComment, CheckNow, DASHBOARD_CONTEXT, Dismiss,
    ExtendSelectionNext, ExtendSelectionPrevious, MarkAll, NextView, ObjectAction, OpenAsTab,
    OpenSelected, PreviousView, ScheduleDowntime, SelectFirst, SelectLast, SelectNext,
    SelectPageDown, SelectPageUp, SelectPrevious, ToggleMark,
};
use crate::app_state::hydration::row_worth_asking;
use crate::app_state::{AppState, Hydrated};
use crate::banner;
use crate::chrome::WindowDrag;
use crate::lists::threads::{self, ItemKey};
use crate::lists::view::{Fold, Unfold};
use crate::pane::{ObjectPane, PaneEvent, PaneMode};

/// The rows on screen are asked for their details once scrolling has
/// rested this long, so flinging through a long list costs one request.
pub(crate) const HYDRATE_DEBOUNCE: Duration = Duration::from_millis(300);

/// The view an rc1-style page shows (a single list, under the summary bar): a dashboard's
/// first list view, else its first view. A single-view dashboard (every
/// dashboard from rc1) shows as rc1 did. A dashboard without views (a
/// settings file edited by hand) reads as the default view.
pub(crate) fn primary_view(views: &[View]) -> &View {
    static NONE: std::sync::LazyLock<View> = std::sync::LazyLock::new(View::default);
    views
        .iter()
        .find(|view| view.is_list())
        .or_else(|| views.first())
        .unwrap_or(&NONE)
}

/// The result of the [`primary_view`].
pub(crate) fn primary_result<'a>(
    result: &'a DashboardResult,
    views: &[View],
) -> Option<&'a ViewResult> {
    result.view(&primary_view(views).id)
}

/// Why `view` can't be read, if a permission is missing: a list's or
/// tiles' objects, a grid's hosts (a stream reads the local log; handling
/// and downtimes say so on their own page).
fn view_denial(state: &AppState, view: &View) -> Option<String> {
    use ic_config::ViewDisplay;
    match view.display {
        ViewDisplay::EventStream
        | ViewDisplay::Handling
        | ViewDisplay::Downtimes
        | ViewDisplay::ZonesAndEndpoints
        | ViewDisplay::Checks
        | ViewDisplay::QueuesAndConnections
        | ViewDisplay::GlobalSwitches => None,
        ViewDisplay::HostGroupGrid => state.query_denial(ObjectKind::Hosts),
        ViewDisplay::List | ViewDisplay::GroupedList | ViewDisplay::SummaryTiles => {
            state.query_denial(view.object_kind)
        }
    }
}

/// What a one-view page says when its view has nothing to show.
fn nothing_to_show(view: &View) -> &'static str {
    use ic_config::ViewDisplay;
    match view.display {
        ViewDisplay::HostGroupGrid => "No host of these groups matches this view's filter.",
        ViewDisplay::SummaryTiles => "Nothing of these groups matches this view's filter.",
        ViewDisplay::EventStream => "No events yet: they show here as they happen.",
        _ => "Nothing to show.",
    }
}

/// Whether a dashboard shows its views under headers (topic 04): several
/// views. A single view's header is the page's (topic 14, round 5): its
/// controls in the page header, a list's (a grid's, tiles') counts in the
/// summary bar under it.
pub(crate) fn is_multi_view(views: &[View]) -> bool {
    views.len() > 1
}

/// The pane's width beside a page of several views (4b): narrower than
/// beside a single list, so the views' headers keep their counts, handled
/// slot, sort and `···`.
const MULTI_VIEW_PANE: f32 = 560.;

/// How long the cursor follows an object revealed while the rows were
/// quiet mode's ([`Reveal`]) at most: the handover to the live stream
/// takes a few seconds at worst.
const REVEAL_FOLLOW: Duration = Duration::from_secs(10);

/// An object revealed (a notification clicked, the palette) while the
/// selected dashboard's rows were still quiet mode's (PERF-09): they may
/// not list it yet, or list it where it was. Until they are live again
/// (and at most [`REVEAL_FOLLOW`]), every new evaluation puts the cursor
/// back on it and scrolls it into view; the user moving the cursor, a
/// click or closing the pane ends that.
struct Reveal {
    dashboard: DashboardRef,
    key: ObjectKey,
    since: Instant,
}

/// What a page was built from: it is built again when any of it changes.
#[derive(Clone, Debug, PartialEq)]
struct Built {
    /// The snapshot (by address: every snapshot is a new `Arc`).
    snapshot: usize,
    views: Vec<View>,
    folds: Folds,
    filter: Option<GroupFilter>,
    width: Pixels,
    sizes: Sizes,
    by_density: page::Densities,
    density: ic_config::RowDensity,
    author: String,
    /// The minute: handling and downtimes change with the clock.
    minute: i64,
    denied: [bool; 2],
    /// The editor's preview: its evaluation's revision (0 on a dashboard,
    /// whose evaluation comes with the snapshot).
    preview: u64,
    /// What topic 17 added to the handling views' threads.
    comments: page::StackedComments,
}

/// A dashboard's page state.
struct PageUi {
    selection: PageSelection,
    scroll: ScrollHandle,
    pane: Option<OpenPane>,
    folds: Folds,
    filter: Option<GroupFilter>,
    page: Rc<Page>,
    built: Option<Built>,
    /// Event streams' own scrolling, by view.
    streams: HashMap<Id, UniformListScrollHandle>,
}

impl PageUi {
    fn new() -> Self {
        Self {
            selection: PageSelection::default(),
            scroll: ScrollHandle::new(),
            pane: None,
            folds: Folds::default(),
            filter: None,
            page: Rc::default(),
            built: None,
            streams: HashMap::new(),
        }
    }

    /// The stop under the cursor.
    fn cursor_entry(&self) -> Option<(usize, &Stop)> {
        let position = self.selection.cursor()?;
        Some((position, &self.page.stops().get(position)?.stop))
    }

    /// The view holding the cursor (its header shows the accent bar).
    fn focused_view(&self) -> Option<usize> {
        let (_, stop) = self.cursor_entry()?;
        self.page.view_index(stop.view())
    }

    /// Every object of the page's views, view by view (for the marks'
    /// order).
    fn objects(&self) -> Vec<ObjectKey> {
        self.page
            .views
            .iter()
            .flat_map(page::ViewPage::objects)
            .collect()
    }
}

/// The pane open beside the page.
struct OpenPane {
    view: Entity<ObjectPane>,
    _events: Subscription,
}

/// What the dashboard view asks the workspace (or, as the editor's
/// preview, the editor) to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DashboardEvent {
    /// Open the editor for this dashboard (the header's `···`).
    Edit(DashboardRef),
    /// Open the editor for this dashboard with one of its views selected
    /// (by id; a view header's `···`).
    EditView(DashboardRef, String),
    /// The editor's preview: a view was clicked; select it (by id).
    Pick(String),
    /// The editor's preview: change a view of the draft (its header's
    /// sort, handled button and `···`), by id.
    ChangeView(String, ViewChange),
}

/// A change to a view of the editor's draft, from its header in the
/// preview.
#[derive(Clone)]
pub(crate) struct ViewChange(Rc<dyn Fn(&mut View)>);

impl ViewChange {
    /// A change made by `change`.
    pub(crate) fn new(change: impl Fn(&mut View) + 'static) -> Self {
        Self(Rc::new(change))
    }

    /// Applies the change to `view`.
    pub(crate) fn apply(&self, view: &mut View) {
        (self.0)(view);
    }
}

impl std::fmt::Debug for ViewChange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ViewChange")
    }
}

impl PartialEq for ViewChange {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for ViewChange {}

/// What the dashboard editor's preview shows (topic 04, 4c): the draft's
/// views as last evaluated, the core's evaluation of them, and the view
/// selected in the inspector, which is marked on its header only.
#[derive(Clone, Debug, Default)]
pub(crate) struct PreviewPage {
    /// The views evaluated (the draft as it was when asked).
    pub(crate) views: Vec<View>,
    /// Their evaluation.
    pub(crate) result: DashboardResult,
    /// The view selected in the inspector (by id).
    pub(crate) picked: Option<String>,
    /// The width the editor takes beside the preview (its inspector).
    pub(crate) reserved: Pixels,
}

/// The cluster section's events (topic 14): one event stream view without
/// a filter, over the latest events the engine already keeps
/// (`Snapshot::events`), so it costs no evaluation.
struct EventsPage {
    /// The one view (its density and stream options as chosen).
    views: Vec<View>,
    /// Its evaluation: the events its options let through.
    result: DashboardResult,
    /// What the result was built from: the events (by address) and the
    /// view; bumped `revision` when rebuilt.
    built: Option<(usize, View)>,
    revision: u64,
}

impl EventsPage {
    fn new() -> Self {
        Self {
            views: vec![events_view(None)],
            result: DashboardResult::default(),
            built: None,
            revision: 0,
        }
    }
}

/// The cluster section's events view: an event stream without a filter,
/// as many lines as it holds (the page scrolls), rows at `density`.
fn events_view(density: Option<ic_config::RowDensity>) -> View {
    View {
        id: EVENTS_VIEW.to_owned(),
        name: "events".to_owned(),
        display: ic_config::ViewDisplay::EventStream,
        stream: ic_config::StreamOptions {
            lines: *ic_config::STREAM_LINES.end(),
            ..ic_config::StreamOptions::default()
        },
        density,
        ..View::default()
    }
}

/// The id of the cluster section's events view.
pub(crate) const EVENTS_VIEW: &str = "events";

/// The key the cluster section's events page is kept under.
pub(crate) fn events_reference() -> DashboardRef {
    DashboardRef {
        group_id: String::new(),
        dashboard_id: EVENTS_VIEW.to_owned(),
    }
}

/// The key the preview's page state is kept under: no dashboard has
/// empty ids.
fn preview_reference() -> DashboardRef {
    DashboardRef {
        group_id: String::new(),
        dashboard_id: String::new(),
    }
}

/// The dashboard page with its header, summary bar and detail pane.
pub(crate) struct DashboardView {
    state: Entity<AppState>,
    focus_handle: FocusHandle,
    pages: HashMap<DashboardRef, PageUi>,
    menus: HeaderMenus,
    sidebar_open: bool,
    drag: WindowDrag,
    /// The page's width as last drawn (grids and tiles lay out to it).
    width: Pixels,
    /// The items built in the last frame: the items on screen.
    visible: Range<usize>,
    /// The times the object rows built in the last frame show, for tests.
    #[cfg(test)]
    shown_times: Vec<String>,
    /// Where the selection bar's buttons start, as last drawn, for tests.
    #[cfg(test)]
    pub(crate) selection_buttons_x: Rc<std::cell::Cell<Option<Pixels>>>,
    /// The rows on screen without output, last asked for (or about to
    /// be), and in which wake of the environment (`AppState::wake`): after
    /// waking up from quiet mode they are asked for again.
    hydration_wanted: (Vec<ObjectKey>, u64),
    /// Asks for them once scrolling rests.
    hydrate_task: Option<Task<()>>,
    /// The object revealed while the rows were quiet mode's.
    reveal: Option<Reveal>,
    /// As the dashboard editor's preview: the draft it shows, and the
    /// revision of its evaluation (the page is built again when it
    /// changes).
    preview: Option<(PreviewPage, u64)>,
    /// As the cluster section's events: the one view and its events.
    events: Option<EventsPage>,
    /// The comment field open in a stacked handling view (topic 17).
    composer: Option<comments::StackedComposer>,
    _subscriptions: Vec<Subscription>,
}

impl gpui::EventEmitter<DashboardEvent> for DashboardView {}

impl Focusable for DashboardView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl DashboardView {
    pub(crate) fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let subscriptions = vec![cx.observe(&state, |_, _, cx| cx.notify())];
        Self {
            state,
            focus_handle: cx.focus_handle(),
            pages: HashMap::new(),
            menus: HeaderMenus::default(),
            sidebar_open: true,
            drag: WindowDrag::default(),
            width: crate::WINDOW_SIZE.width,
            visible: 0..0,
            #[cfg(test)]
            shown_times: Vec::new(),
            #[cfg(test)]
            selection_buttons_x: Rc::default(),
            hydration_wanted: (Vec::new(), 0),
            hydrate_task: None,
            reveal: None,
            preview: None,
            events: None,
            composer: None,
            _subscriptions: subscriptions,
        }
    }

    /// The cluster section's *events* page: the environment's latest
    /// events in one event stream without a filter, with the dashboard
    /// page's keys and pane.
    pub(crate) fn cluster_events(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let mut view = Self::new(state, cx);
        view.events = Some(EventsPage::new());
        view
    }

    /// Whether this is the cluster section's events page.
    pub(crate) fn is_events(&self) -> bool {
        self.events.is_some()
    }

    /// The page shown: the selected dashboard, the events page or the
    /// editor's preview.
    fn current_reference(&self, cx: &App) -> Option<DashboardRef> {
        if self.preview.is_some() {
            Some(preview_reference())
        } else if self.events.is_some() {
            Some(events_reference())
        } else {
            self.state.read(cx).selected().cloned()
        }
    }

    /// A page for the dashboard editor's preview: it shows `page` instead
    /// of the selected dashboard, has no pane, cursor or keys of its own,
    /// and a click in a view picks it ([`DashboardEvent::Pick`]).
    pub(crate) fn preview(
        state: Entity<AppState>,
        page: PreviewPage,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut view = Self::new(state, cx);
        view.preview = Some((page, 1));
        view
    }

    /// Shows the draft's views as evaluated now (the editor's preview).
    pub(crate) fn set_preview(&mut self, page: PreviewPage, cx: &mut Context<Self>) {
        if let Some((shown, revision)) = &mut self.preview {
            let evaluated = shown.views != page.views || shown.result != page.result;
            *shown = page;
            if evaluated {
                *revision += 1;
            }
            cx.notify();
        }
    }

    /// The editor's preview: marks view `picked` on its header and
    /// (`reveal`) scrolls its header into view.
    pub(crate) fn pick(&mut self, picked: Option<String>, reveal: bool, cx: &mut Context<Self>) {
        let Some((shown, _)) = &mut self.preview else {
            return;
        };
        if shown.picked == picked {
            return;
        }
        shown.picked.clone_from(&picked);
        let reference = preview_reference();
        if reveal && let (Some(picked), Some(ui)) = (picked, self.pages.get(&reference)) {
            reveal_view(ui, &picked);
        }
        cx.notify();
    }

    /// The editor's preview: forgets the views folded by a click, so each
    /// shows as its settings say (*collapse by default* changed).
    pub(crate) fn reset_folds(&mut self, cx: &mut Context<Self>) {
        if let Some(ui) = self.pages.get_mut(&preview_reference()) {
            ui.folds = Folds::default();
            cx.notify();
        }
    }

    /// Whether this is the editor's preview.
    fn is_preview(&self) -> bool {
        self.preview.is_some()
    }

    /// The view picked in the editor's preview.
    fn picked(&self) -> Option<&str> {
        self.preview
            .as_ref()
            .and_then(|(page, _)| page.picked.as_deref())
    }

    /// The views of the page kept under `reference`: the dashboard's, or
    /// the editor's draft in its preview.
    fn shown_views<'a>(&'a self, reference: &DashboardRef, cx: &'a App) -> Option<&'a [View]> {
        if let Some(events) = &self.events {
            return Some(&events.views);
        }
        match &self.preview {
            Some((page, _)) => Some(&page.views),
            None => self
                .state
                .read(cx)
                .dashboard(reference)
                .map(|(_, dashboard)| dashboard.views.as_slice()),
        }
    }

    /// The evaluation of view `view_id` of the page kept under `reference`.
    fn shown_result<'a>(
        &'a self,
        reference: &DashboardRef,
        view_id: &str,
        cx: &'a App,
    ) -> Option<&'a ViewResult> {
        if let Some(events) = &self.events {
            return events.result.view(view_id);
        }
        match &self.preview {
            Some((page, _)) => page.result.view(view_id),
            None => self.state.read(cx).view_result(reference, view_id),
        }
    }

    /// Changes view `view_id` of the page kept under `reference`: the saved
    /// dashboard's, or (the editor's preview) the draft's, which the editor
    /// does.
    fn change_view(
        &mut self,
        reference: &DashboardRef,
        view_id: &str,
        change: impl Fn(&mut View) + 'static,
        cx: &mut Context<Self>,
    ) {
        if self.is_preview() {
            cx.emit(DashboardEvent::ChangeView(
                view_id.to_owned(),
                ViewChange(Rc::new(change)),
            ));
            return;
        }
        if self.events.is_some() {
            // The events page keeps only its rows' density.
            let mut view = events_view(self.state.read(cx).events_density());
            change(&mut view);
            self.state.update(cx, |state, cx| {
                if state.set_events_density(view.density) {
                    cx.notify();
                }
            });
            cx.notify();
            return;
        }
        self.state.update(cx, |state, cx| {
            if state.update_view(reference, view_id, change) {
                cx.notify();
            }
        });
    }

    /// The handled button of a list view was clicked: show or hide its
    /// handled problems.
    fn toggle_handled(&mut self, reference: &DashboardRef, view_id: &str, cx: &mut Context<Self>) {
        if self.is_preview() {
            let defaults = self.state.read(cx).handled_defaults();
            let hidden = self
                .shown_result(reference, view_id, cx)
                .map_or(0, |result| result.hidden);
            self.change_view(
                reference,
                view_id,
                move |view| {
                    view.handled =
                        crate::app_state::handled_after_click(view.handled, defaults, hidden);
                },
                cx,
            );
            return;
        }
        self.state.update(cx, |state, cx| {
            if state.toggle_handled(reference, view_id) {
                cx.notify();
            }
        });
    }

    /// Remembers the rows on screen and, if they changed, offers them to
    /// the engine once scrolling rests.
    fn want_details(&mut self, keys: Vec<ObjectKey>, cx: &mut Context<Self>) {
        let wake = self.state.read(cx).wake();
        if keys == self.hydration_wanted.0 && wake == self.hydration_wanted.1 {
            return;
        }
        self.hydration_wanted = (keys.clone(), wake);
        if keys.is_empty() {
            self.hydrate_task = None;
            return;
        }
        self.hydrate_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(HYDRATE_DEBOUNCE).await;
            let _ = this.update(cx, |this, cx| {
                let outcome = this
                    .state
                    .update(cx, |state, _| state.hydrate(keys, Instant::now()));
                if outcome == Hydrated::NotNow {
                    // Not connected yet: the next frame asks again.
                    this.hydration_wanted.0.clear();
                }
            });
        }));
    }

    /// Tells the view whether the sidebar is shown (the header then needs no
    /// window controls).
    pub(crate) fn set_sidebar_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.sidebar_open != open {
            self.sidebar_open = open;
            // A pane covering a narrow page needs the window controls then.
            for pane in self.pages.values().filter_map(|ui| ui.pane.as_ref()) {
                pane.view
                    .update(cx, |pane, cx| pane.set_sidebar_open(open, cx));
            }
            cx.notify();
        }
    }

    /// Puts the cursor on `key` in the selected dashboard and opens its pane
    /// (the notification and startup paths). While the rows are quiet
    /// mode's, the cursor follows the object until they are live ([`Reveal`]).
    pub(crate) fn open_object(&mut self, key: &ObjectKey, cx: &mut Context<Self>) {
        let Some(reference) = self.sync(cx) else {
            return;
        };
        self.put_cursor_on(&reference, key, cx);
        self.open_pane(&reference, key.clone(), cx);
        self.reveal = self.state.read(cx).rows_settling().then(|| Reveal {
            dashboard: reference,
            key: key.clone(),
            since: Instant::now(),
        });
    }

    /// Puts the cursor on `cursor` and shows `pane` in the pane, as after
    /// following a link in it (the startup path for screen 2c).
    pub(crate) fn open_linked(
        &mut self,
        cursor: &ObjectKey,
        pane: ObjectKey,
        cx: &mut Context<Self>,
    ) {
        self.open_object(cursor, cx);
        let Some(reference) = self.sync(cx) else {
            return;
        };
        if let Some(open) = self.pages.get(&reference).and_then(|ui| ui.pane.as_ref()) {
            open.view.update(cx, |view, cx| view.show(pane, cx));
        }
    }

    /// Opens `key`'s pane beside the page, or shows `key` in the open one.
    fn open_pane(&mut self, reference: &DashboardRef, key: ObjectKey, cx: &mut Context<Self>) {
        let Some(ui) = self.pages.get_mut(reference) else {
            return;
        };
        if let Some(pane) = &ui.pane {
            pane.view.update(cx, |pane, cx| pane.open(key, cx));
        } else {
            let state = self.state.clone();
            let sidebar_open = self.sidebar_open;
            let focus = self.focus_handle.clone();
            let view = cx.new(|cx| {
                let mut pane = ObjectPane::new(state, key, PaneMode::Split, cx);
                pane.set_sidebar_open(sidebar_open, cx);
                pane.set_return_focus(focus);
                pane
            });
            let closed = reference.clone();
            let events = cx.subscribe(&view, move |this, _, event: &PaneEvent, cx| match event {
                PaneEvent::Close => {
                    this.close_pane(&closed, cx);
                }
            });
            ui.pane = Some(OpenPane {
                view,
                _events: events,
            });
        }
        cx.notify();
    }

    fn close_pane(&mut self, reference: &DashboardRef, cx: &mut Context<Self>) -> bool {
        self.reveal = None;
        let closed = self
            .pages
            .get_mut(reference)
            .and_then(|ui| ui.pane.take())
            .is_some();
        if closed {
            cx.notify();
        }
        closed
    }

    /// The cluster's events page: its view (with the density chosen for
    /// it) over the snapshot's latest events, rebuilt when either changed.
    fn sync_events(&mut self, cx: &App) {
        let Some(events) = &mut self.events else {
            return;
        };
        let state = self.state.read(cx);
        let view = events_view(state.events_density());
        let snapshot = state.snapshot();
        let address = std::sync::Arc::as_ptr(&snapshot.events) as usize;
        if events.built.as_ref() != Some(&(address, view.clone())) {
            let shown = ic_core::stream_events(view.stream, &snapshot.events);
            events.result = DashboardResult {
                summary: ic_core::snapshot::Summary::default(),
                views: vec![ViewResult {
                    id: EVENTS_VIEW.to_owned(),
                    body: ic_core::snapshot::ViewBody::Stream(std::sync::Arc::new(shown)),
                    ..ViewResult::default()
                }],
            };
            events.views = vec![view.clone()];
            events.built = Some((address, view));
            events.revision += 1;
        }
    }

    /// Makes sure the selected dashboard has its page state and that its
    /// page is built from the current views, results and folds. Cheap
    /// when nothing changed.
    #[expect(
        clippy::too_many_lines,
        reason = "one page build: its inputs, whether they changed, the rebuild"
    )]
    fn sync(&mut self, cx: &App) -> Option<DashboardRef> {
        self.sync_events(cx);
        let state = self.state.read(cx);
        let (reference, views, result, revision) = if let Some(events) = &self.events {
            (
                events_reference(),
                events.views.as_slice(),
                Some(&events.result),
                events.revision,
            )
        } else if let Some((page, revision)) = &self.preview {
            (
                preview_reference(),
                page.views.as_slice(),
                Some(&page.result),
                *revision,
            )
        } else {
            if self.pages.len() > 1 {
                self.pages
                    .retain(|reference, _| state.dashboard(reference).is_some());
            }
            let reference = state.selected()?.clone();
            let (_, dashboard) = state.dashboard(&reference)?;
            let result = state.result(&reference);
            (reference, dashboard.views.as_slice(), result, 0)
        };
        let snapshot = state.snapshot();
        let multi = is_multi_view(views);
        let denied = [
            state.query_denial(ObjectKind::Services).is_some(),
            state.query_denial(ObjectKind::Hosts).is_some(),
        ];
        let sizes = Sizes::of(cx.theme());
        let by_density = page::Densities::of(cx.theme());
        let density = state.appearance().row_density;
        let author = state.author().to_owned();
        let now = Timestamp::now();
        #[expect(
            clippy::cast_possible_truncation,
            reason = "minutes since 1970 fit in i64"
        )]
        let minute = (now.as_unix_seconds() / 60.).floor() as i64;
        // Handling and downtimes follow the clock (phases, times left).
        let clocked = views.iter().any(|view| view.display.is_threads());
        let has_handling = views.iter().any(|view| {
            crate::lists::model::ListKind::of_display(view.display)
                == Some(crate::lists::model::ListKind::Handling)
        });
        let comments = self.stacked_comments(&reference, has_handling, cx);
        let width = self.width;
        let ui = self
            .pages
            .entry(reference.clone())
            .or_insert_with(PageUi::new);
        let fresh = ui.built.as_ref().is_some_and(|built| {
            built.snapshot == std::sync::Arc::as_ptr(snapshot) as usize
                && built.views == views
                && built.folds == ui.folds
                && built.filter == ui.filter
                && built.width == width
                && built.sizes == sizes
                && built.by_density == by_density
                && built.density == density
                && built.author == author
                && (!clocked || built.minute == minute)
                && built.denied == denied
                && built.preview == revision
                && built.comments == comments
        });
        if !fresh {
            let page = Page::build(&PageInput {
                views,
                result,
                snapshot,
                folds: &ui.folds,
                filter: ui.filter.as_ref(),
                multi,
                denied,
                width,
                sizes,
                by_density,
                density,
                author: &author,
                now,
                comments: &comments,
            });
            if ui.selection.marked_count() > 0 {
                let listed: HashSet<ObjectKey> = page
                    .views
                    .iter()
                    .flat_map(page::ViewPage::objects)
                    .collect();
                ui.selection.update(&page, |key| listed.contains(key));
            } else {
                ui.selection.update(&page, |_| true);
            }
            ui.page = Rc::new(page);
            ui.built = Some(Built {
                snapshot: std::sync::Arc::as_ptr(snapshot) as usize,
                views: views.to_vec(),
                folds: ui.folds.clone(),
                filter: ui.filter.clone(),
                width,
                sizes,
                by_density,
                density,
                author,
                minute,
                denied,
                preview: revision,
                comments,
            });
        }
        let settling = state.rows_settling();
        self.follow_reveal(&reference, fresh, settling);
        Some(reference)
    }

    /// An object to reveal on `reference`'s page (a notification, the
    /// palette): placed once the page shows it, followed while its rows
    /// settle (`settling`) and `fresh` pages don't move it again.
    fn follow_reveal(&mut self, reference: &DashboardRef, fresh: bool, settling: bool) {
        let Some(reveal) = &self.reveal else {
            return;
        };
        if reveal.dashboard != *reference {
            return;
        }
        let key = reveal.key.clone();
        let since = reveal.since;
        let placed = (!fresh || !settling) && self.place_on(reference, &key);
        if (placed && !settling) || since.elapsed() > REVEAL_FOLLOW {
            self.reveal = None;
        }
    }

    /// Puts the cursor on `key`'s first stop on the page (no unfolding)
    /// and scrolls it to the middle. Returns whether the page shows it.
    fn place_on(&mut self, reference: &DashboardRef, key: &ObjectKey) -> bool {
        let Some(ui) = self.pages.get_mut(reference) else {
            return false;
        };
        let page = ui.page.clone();
        let Some(position) = page
            .stops()
            .iter()
            .position(|entry| matches!(&entry.stop, Stop::Row { key: row, .. } if row == key))
            .or_else(|| {
                page.stops()
                    .iter()
                    .position(|entry| entry.stop.object(&page).as_ref() == Some(key))
            })
        else {
            return false;
        };
        ui.selection.place(&page, position);
        reveal_stop(ui, &page, position, true);
        true
    }

    /// Puts the cursor on `key`, unfolding what hides it (its view, its
    /// host, its host's paging), and scrolls it into view.
    fn put_cursor_on(&mut self, reference: &DashboardRef, key: &ObjectKey, cx: &mut Context<Self>) {
        if self.place_on(reference, key) {
            return;
        }
        let views = self
            .shown_views(reference, cx)
            .map(<[View]>::to_vec)
            .unwrap_or_default();
        let Some(ui) = self.pages.get_mut(reference) else {
            return;
        };
        let page = ui.page.clone();
        let mut unfolded = false;
        for view in &page.views {
            let has = view
                .rows
                .iter()
                .any(|row| matches!(row, DashboardRow::Object(object) if object == key));
            if !has {
                continue;
            }
            if view.collapsed
                && let Some(config) = views.iter().find(|candidate| *candidate.id == *view.id)
            {
                ui.folds.set_view(config, false);
            }
            for group in &view.groups {
                let member = group.members.iter().position(
                    |&row| matches!(&view.rows[row], DashboardRow::Object(object) if object == key),
                );
                if let Some(member) = member {
                    ui.folds.set_group(&view.id, &group.id, false);
                    if member >= group.shown {
                        ui.folds.set_expanded(&view.id, &group.id, true);
                    }
                }
            }
            unfolded = true;
            break;
        }
        if unfolded {
            self.sync(cx);
            self.place_on(reference, key);
        }
    }

    /// Runs `change` on the selected dashboard's cursor; scrolls to the
    /// cursor and, if the pane is open, shows the cursor's object in it.
    fn change_selection(
        &mut self,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut PageSelection, &Page) -> Option<usize>,
    ) {
        let Some(reference) = self.sync(cx) else {
            return;
        };
        self.reveal = None;
        let Some(ui) = self.pages.get_mut(&reference) else {
            return;
        };
        let page = ui.page.clone();
        if let Some(position) = change(&mut ui.selection, &page) {
            reveal_stop(ui, &page, position, false);
            if let Some(pane) = &ui.pane
                && let Some(key) = page
                    .stops()
                    .get(position)
                    .and_then(|entry| entry.stop.object(&page))
            {
                pane.view.update(cx, |pane, cx| pane.show(key, cx));
            }
        }
        cx.notify();
    }

    fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        self.change_selection(cx, |selection, page| selection.move_by(page, 1));
    }

    fn select_previous(&mut self, _: &SelectPrevious, _: &mut Window, cx: &mut Context<Self>) {
        self.change_selection(cx, |selection, page| selection.move_by(page, -1));
    }

    fn extend_next(&mut self, _: &ExtendSelectionNext, _: &mut Window, cx: &mut Context<Self>) {
        self.change_selection(cx, |selection, page| selection.extend_by(page, 1));
    }

    fn extend_previous(
        &mut self,
        _: &ExtendSelectionPrevious,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.change_selection(cx, |selection, page| selection.extend_by(page, -1));
    }

    fn select_first(&mut self, _: &SelectFirst, _: &mut Window, cx: &mut Context<Self>) {
        self.change_selection(cx, |selection, page| selection.move_to_end(page, false));
    }

    fn select_last(&mut self, _: &SelectLast, _: &mut Window, cx: &mut Context<Self>) {
        self.change_selection(cx, |selection, page| selection.move_to_end(page, true));
    }

    fn select_page_down(&mut self, _: &SelectPageDown, _: &mut Window, cx: &mut Context<Self>) {
        self.move_page(true, cx);
    }

    fn select_page_up(&mut self, _: &SelectPageUp, _: &mut Window, cx: &mut Context<Self>) {
        self.move_page(false, cx);
    }

    /// Moves the cursor a screenful down (or up): to the stop a viewport's
    /// height away.
    fn move_page(&mut self, forward: bool, cx: &mut Context<Self>) {
        let viewport = self
            .sync(cx)
            .and_then(|reference| self.pages.get(&reference))
            .map(viewport_height)
            .unwrap_or_default();
        self.change_selection(cx, |selection, page| {
            let Some(position) = selection.cursor() else {
                return selection.move_by(page, if forward { 1 } else { -1 });
            };
            let item = page.stops()[position].item;
            let y = if forward {
                page.top(item) + viewport
            } else {
                page.top(item) - viewport
            };
            let target_item = page.item_at(y.max(px(0.)))?;
            let target = page
                .stop_at_or_after(target_item)
                .and_then(|target| page.body_stop_near(target, forward))
                .or_else(|| page.body_stop_near(page.stops().len().saturating_sub(1), false))?;
            let target = if forward {
                target.max(position)
            } else {
                target.min(position)
            };
            selection.place(page, target).then_some(target)
        });
    }

    fn toggle_mark(&mut self, _: &ToggleMark, _: &mut Window, cx: &mut Context<Self>) {
        self.change_selection(cx, PageSelection::toggle_mark_at_cursor);
    }

    /// ctrl-a: marks every row of the view holding the cursor (else the
    /// first list), its folded and paged-away rows too.
    fn mark_all(&mut self, _: &MarkAll, _: &mut Window, cx: &mut Context<Self>) {
        let Some(reference) = self.sync(cx) else {
            return;
        };
        let Some(ui) = self.pages.get_mut(&reference) else {
            return;
        };
        let view = ui
            .focused_view()
            .or_else(|| ui.page.views.iter().position(|view| !view.rows.is_empty()));
        if let Some(view) = view.and_then(|view| ui.page.views.get(view)) {
            ui.selection.mark_all(view.objects());
            cx.notify();
        }
    }

    /// Enter: opens the cursor's object in the pane (a row, a host's band,
    /// a grid's host, an event); on a paging row shows all or fewer; on a
    /// view's header or a group's band (not a host's) folds or unfolds it.
    fn open_selected(&mut self, _: &OpenSelected, _: &mut Window, cx: &mut Context<Self>) {
        let Some(reference) = self.sync(cx) else {
            return;
        };
        let Some(ui) = self.pages.get_mut(&reference) else {
            return;
        };
        if ui.selection.cursor().is_none() {
            let page = ui.page.clone();
            ui.selection.move_by(&page, 1);
        }
        let Some((_, stop)) = ui.cursor_entry() else {
            return;
        };
        let stop = stop.clone();
        self.activate(&reference, &stop, true, cx);
    }

    /// What Enter (or a click on it) does with `stop`.
    fn activate(
        &mut self,
        reference: &DashboardRef,
        stop: &Stop,
        keyboard: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(ui) = self.pages.get(reference) else {
            return;
        };
        let page = ui.page.clone();
        match stop {
            Stop::Header(view) => {
                let collapsed = page.view_by_id(view).is_some_and(|view| view.collapsed);
                self.fold_view(reference, view, !collapsed, cx);
            }
            Stop::More { view, group } => {
                let expanded = page
                    .view_by_id(view)
                    .and_then(|view| view.group(group))
                    .is_some_and(|group| group.expanded);
                self.set_paging(reference, view, group, !expanded, keyboard, cx);
            }
            Stop::Band { view, group } => {
                if let Some(host) = stop.object(&page) {
                    self.open_pane(reference, host, cx);
                } else {
                    let collapsed = page
                        .view_by_id(view)
                        .and_then(|view| view.group(group))
                        .is_some_and(|group| group.collapsed);
                    self.fold_group(reference, view, group, !collapsed, cx);
                }
            }
            Stop::Row { .. } | Stop::Cell { .. } | Stop::Event { .. } => {
                if let Some(key) = stop.object(&page) {
                    self.open_pane(reference, key, cx);
                }
            }
            Stop::Thread { view, key } => match key {
                ItemKey::Fold(name) => {
                    let name = name.clone();
                    self.fold_thread(reference, view, None, cx, move |folds| {
                        if !folds.open.remove(&name) {
                            folds.open.insert(name);
                        }
                    });
                }
                ItemKey::More(more) => {
                    let more = more.clone();
                    self.fold_thread(reference, view, None, cx, move |folds| {
                        toggle_more(folds, &more);
                    });
                }
                ItemKey::Band(_) | ItemKey::Entry(_) | ItemKey::Service(..) => {
                    if let Some(key) = stop.object(&page) {
                        self.open_pane(reference, key, cx);
                    }
                }
            },
        }
    }

    /// Changes the folds of handling or downtimes view `view` and builds
    /// the page again; the cursor goes to `cursor` when given.
    fn fold_thread(
        &mut self,
        reference: &DashboardRef,
        view: &Id,
        cursor: Option<Stop>,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut threads::Folds),
    ) {
        let Some(ui) = self.pages.get_mut(reference) else {
            return;
        };
        change(ui.folds.threads_mut(view));
        self.after_fold(reference, cursor, cx);
    }

    /// `→`: unfolds what the cursor is on (a view's header, a band, a
    /// host's `+ N more`); on a grid, the next host.
    fn unfold(&mut self, _: &Unfold, _: &mut Window, cx: &mut Context<Self>) {
        self.fold_at_cursor(true, cx);
    }

    /// `←`: folds what the cursor is on; on a grid, the previous host.
    /// With nothing to fold there (a row, an event, a folded band, the
    /// grid's first host), the cursor goes to what holds it: a row's band,
    /// else its view's header, where `←` folds the view. So every view
    /// folds from the keyboard.
    fn fold(&mut self, _: &Fold, _: &mut Window, cx: &mut Context<Self>) {
        self.fold_at_cursor(false, cx);
    }

    fn fold_at_cursor(&mut self, open: bool, cx: &mut Context<Self>) {
        let Some(reference) = self.sync(cx) else {
            return;
        };
        let Some((position, stop, page)) = self.pages.get(&reference).and_then(|ui| {
            ui.cursor_entry()
                .map(|(position, stop)| (position, stop.clone(), ui.page.clone()))
        }) else {
            cx.propagate();
            return;
        };
        let header = |view: &Id| Stop::Header(view.clone());
        let parent = match &stop {
            Stop::Header(_) => None,
            Stop::Row {
                view,
                group: Some(group),
                ..
            } => Some(Stop::Band {
                view: view.clone(),
                group: group.clone(),
            }),
            Stop::Row {
                view, group: None, ..
            }
            | Stop::Event { view, .. }
            | Stop::Cell { view, .. } => Some(header(view)),
            Stop::Band { view, group } => page
                .view_by_id(view)
                .and_then(|page_view| page_view.group(group))
                .filter(|band| band.collapsed)
                .map(|_| header(view)),
            Stop::More { view, group } => page
                .view_by_id(view)
                .and_then(|page_view| page_view.group(group))
                .filter(|host| !host.expanded)
                .map(|_| Stop::Band {
                    view: view.clone(),
                    group: group.clone(),
                }),
            // A thread's line with nothing to fold goes to its object's
            // band; a band folded already, to the view's header.
            Stop::Thread { view, key } => thread_parent(&page, view, key, position),
        }
        .filter(|_| !open)
        .and_then(|parent| page.position(&parent));
        match &stop {
            Stop::Cell { .. } => {
                let side = page.grid_side_step(position, open);
                let target = side.filter(|&target| target != position).or(parent);
                if let Some(target) = target {
                    self.change_selection(cx, |selection, page| {
                        selection.place(page, target).then_some(target)
                    });
                }
            }
            _ if parent.is_some() => {
                self.change_selection(cx, |selection, page| {
                    let target = parent?;
                    selection.place(page, target).then_some(target)
                });
            }
            Stop::Header(view) => self.fold_view(&reference, view, !open, cx),
            Stop::Band { view, group } => self.fold_group(&reference, view, group, !open, cx),
            Stop::More { view, group } => {
                self.set_paging(&reference, view, group, open, true, cx);
            }
            Stop::Thread { view, key } => {
                let thread = page.view_by_id(view).and_then(|view| view.thread.as_ref());
                match key {
                    ItemKey::Band(object) => {
                        let object = object.clone();
                        self.fold_thread(&reference, view, None, cx, move |folds| {
                            if open {
                                folds.collapsed.remove(&object);
                            } else {
                                folds.collapsed.insert(object);
                            }
                        });
                    }
                    ItemKey::Fold(name) => {
                        let name = name.clone();
                        self.fold_thread(&reference, view, None, cx, move |folds| {
                            if open {
                                folds.open.insert(name);
                            } else {
                                folds.open.remove(&name);
                            }
                        });
                    }
                    ItemKey::More(more) if thread.is_some() => {
                        let more = more.clone();
                        self.fold_thread(&reference, view, None, cx, move |folds| {
                            set_more(folds, &more, open);
                        });
                    }
                    _ => cx.propagate(),
                }
            }
            Stop::Row { .. } | Stop::Event { .. } => cx.propagate(),
        }
    }

    /// Collapses (or expands) a view to its header; a cursor inside it
    /// goes to the header.
    fn fold_view(
        &mut self,
        reference: &DashboardRef,
        view: &Id,
        collapsed: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(config) = self
            .shown_views(reference, cx)
            .and_then(|views| views.iter().find(|candidate| *candidate.id == **view))
            .cloned()
        else {
            return;
        };
        let Some(ui) = self.pages.get_mut(reference) else {
            return;
        };
        let inside = ui
            .cursor_entry()
            .is_some_and(|(_, stop)| stop.view() == view);
        ui.folds.set_view(&config, collapsed);
        self.after_fold(reference, inside.then(|| Stop::Header(view.clone())), cx);
    }

    /// Collapses (or expands) a group to its band; a cursor on one of its
    /// rows goes to the band.
    fn fold_group(
        &mut self,
        reference: &DashboardRef,
        view: &Id,
        group: &Id,
        collapsed: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(ui) = self.pages.get_mut(reference) else {
            return;
        };
        let inside = ui.cursor_entry().is_some_and(|(_, stop)| match stop {
            Stop::Row {
                view: row_view,
                group: Some(row_group),
                ..
            }
            | Stop::More {
                view: row_view,
                group: row_group,
            } => row_view == view && row_group == group,
            _ => false,
        });
        ui.folds.set_group(view, group, collapsed);
        let band = Stop::Band {
            view: view.clone(),
            group: group.clone(),
        };
        self.after_fold(reference, inside.then_some(band), cx);
    }

    /// Shows all of a host's services (`+ N more`) or pages them again
    /// (`− show fewer`). From the keyboard, showing all puts the cursor
    /// on the first row that appeared, where the paging row was.
    fn set_paging(
        &mut self,
        reference: &DashboardRef,
        view: &Id,
        group: &Id,
        expanded: bool,
        keyboard: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(ui) = self.pages.get_mut(reference) else {
            return;
        };
        let first_new = ui
            .page
            .view_by_id(view)
            .and_then(|page_view| {
                let group = page_view.group(group)?;
                let row = *group.members.get(group.shown)?;
                match &page_view.rows[row] {
                    DashboardRow::Object(key) => Some(Stop::Row {
                        view: view.clone(),
                        group: Some(group.id.clone()),
                        key: key.clone(),
                    }),
                    DashboardRow::Group { .. } => None,
                }
            })
            .filter(|_| expanded && keyboard);
        ui.folds.set_expanded(view, group, expanded);
        self.after_fold(reference, first_new, cx);
    }

    /// Builds the page again after a fold, puts the cursor on `cursor`
    /// (when given) and keeps it in view.
    fn after_fold(
        &mut self,
        reference: &DashboardRef,
        cursor: Option<Stop>,
        cx: &mut Context<Self>,
    ) {
        self.sync(cx);
        if let Some(ui) = self.pages.get_mut(reference) {
            let page = ui.page.clone();
            if let Some(position) = cursor.and_then(|stop| page.position(&stop)) {
                ui.selection.place(&page, position);
            }
            if let Some(position) = ui.selection.cursor() {
                reveal_stop(ui, &page, position, false);
            }
        }
        cx.notify();
    }

    /// Tab / shift-Tab: the cursor to the next (previous) view's first row,
    /// or its header when it shows none (collapsed).
    /// `f`, `shift-f`, `v` on a stacked handling or downtimes view: change
    /// the chip or the mode of the view holding the cursor, as a click on
    /// its header does.
    fn step_thread_view(
        &mut self,
        change: fn(&mut crate::lists::model::Options, crate::lists::model::ListKind),
        cx: &mut Context<Self>,
    ) {
        let Some(reference) = self.sync(cx) else {
            return;
        };
        let target = self.pages.get(&reference).and_then(|ui| {
            let index = ui.focused_view()?;
            let view = ui.page.views.get(index)?;
            Some((view.id.to_string(), view.thread.as_ref()?.kind))
        });
        let Some((view_id, kind)) = target else {
            cx.propagate();
            return;
        };
        self.change_view(
            &reference,
            &view_id,
            move |view| {
                let mut options = crate::lists::model::Options::of_view(kind, view.threads);
                change(&mut options, kind);
                view.threads = options.to_view();
            },
            cx,
        );
        cx.notify();
    }

    fn next_chip(
        &mut self,
        _: &crate::lists::view::NextChip,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.step_thread_view(|options, kind| options.step_chip(kind, true), cx);
    }

    fn previous_chip(
        &mut self,
        _: &crate::lists::view::PreviousChip,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.step_thread_view(|options, kind| options.step_chip(kind, false), cx);
    }

    fn toggle_timeline(
        &mut self,
        _: &crate::lists::view::ToggleTimeline,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.step_thread_view(crate::lists::model::Options::toggle_mode, cx);
    }

    fn next_view(&mut self, _: &NextView, _: &mut Window, cx: &mut Context<Self>) {
        self.jump_view(true, cx);
    }

    fn previous_view(&mut self, _: &PreviousView, _: &mut Window, cx: &mut Context<Self>) {
        self.jump_view(false, cx);
    }

    fn jump_view(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some(reference) = self.sync(cx) else {
            return;
        };
        let Some(ui) = self.pages.get(&reference) else {
            return;
        };
        let count = ui.page.views.len();
        if count < 2 && ui.page.header_stop(0).is_none() {
            // A single list: nothing to jump to.
            cx.propagate();
            return;
        }
        let current = ui.focused_view();
        let candidates: Vec<usize> = match (current, forward) {
            (Some(current), true) => (current + 1..count).collect(),
            (Some(current), false) => (0..current).rev().collect(),
            (None, true) => (0..count).collect(),
            (None, false) => (0..count).rev().collect(),
        };
        let target = candidates.into_iter().find_map(|view| {
            ui.page
                .first_body_stop(view)
                .or_else(|| ui.page.header_stop(view))
        });
        if let Some(target) = target {
            self.change_selection(cx, |selection, page| {
                selection.place(page, target).then_some(target)
            });
        }
    }

    fn dismiss(&mut self, _: &Dismiss, _: &mut Window, cx: &mut Context<Self>) {
        if self.menus.close() {
            cx.notify();
            return;
        }
        let Some(reference) = self.sync(cx) else {
            cx.propagate();
            return;
        };
        if self.close_pane(&reference, cx) {
            return;
        }
        let Some(ui) = self.pages.get_mut(&reference) else {
            cx.propagate();
            return;
        };
        if ui.selection.clear_marks() {
            cx.notify();
        } else if ui.filter.take().is_some() {
            self.after_fold(&reference, None, cx);
        } else {
            cx.propagate();
        }
    }

    /// Filters the page to one group (a click on a grid group's name or a
    /// tile), or clears the filter when it is that group already.
    fn filter_to(&mut self, reference: &DashboardRef, filter: GroupFilter, cx: &mut Context<Self>) {
        if self.is_preview() {
            // The editor's preview isn't filtered: the click picked the view.
            return;
        }
        let Some(ui) = self.pages.get_mut(reference) else {
            return;
        };
        ui.filter = if ui.filter.as_ref() == Some(&filter) {
            None
        } else {
            Some(filter)
        };
        self.after_fold(reference, None, cx);
    }

    /// Clears the page's group filter (the chip's ×).
    fn clear_filter(&mut self, cx: &mut Context<Self>) {
        let Some(reference) = self.sync(cx) else {
            return;
        };
        if let Some(ui) = self.pages.get_mut(&reference)
            && ui.filter.take().is_some()
        {
            self.after_fold(&reference, None, cx);
        }
    }

    fn open_as_tab(&mut self, _: &OpenAsTab, _: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.focused_object(cx) else {
            return;
        };
        self.state.update(cx, |state, cx| {
            if state.open_tab(key) {
                cx.notify();
            }
        });
    }

    /// The marked objects of the selected dashboard, in page order.
    fn marked_keys(&mut self, cx: &mut Context<Self>) -> Vec<ObjectKey> {
        let Some(reference) = self.sync(cx) else {
            return Vec::new();
        };
        self.pages
            .get(&reference)
            .filter(|ui| ui.selection.marked_count() > 0)
            .map(|ui| ui.selection.marked_in(ui.objects()))
            .unwrap_or_default()
    }

    /// The objects an action from elsewhere (the command palette) applies
    /// to: the marked rows, else the pane's or the cursor's object.
    pub(crate) fn action_targets(&mut self, cx: &mut Context<Self>) -> Vec<ObjectKey> {
        let marked = self.marked_keys(cx);
        if marked.is_empty() {
            self.focused_object(cx).into_iter().collect()
        } else {
            marked
        }
    }

    /// Runs `action` as its key would: on the marked rows, else the
    /// pane's or the cursor's object.
    pub(crate) fn run_action(&mut self, action: ObjectAction, cx: &mut Context<Self>) {
        self.request(action, cx);
    }

    /// The object the keyboard acts on without marks: the pane's, else the
    /// cursor's.
    fn focused_object(&mut self, cx: &mut Context<Self>) -> Option<ObjectKey> {
        let reference = self.sync(cx)?;
        let ui = self.pages.get(&reference)?;
        match &ui.pane {
            Some(pane) => Some(pane.view.read(cx).object().clone()),
            None => ui.cursor_entry()?.1.object(&ui.page),
        }
    }

    /// Sends `action` for the marked rows, or else the pane's or the
    /// cursor's object.
    fn request(&mut self, action: ObjectAction, cx: &mut Context<Self>) {
        let targets = self.action_targets(cx);
        if targets.is_empty() {
            return;
        }
        self.state.update(cx, |state, cx| {
            // The workspace opens the dialog; a refusal shows as a toast.
            let _ = state.request(ActionRequest {
                action,
                targets,
                review: false,
            });
            cx.notify();
        });
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

    /// `c`: in a stacked handling view, the comment field below the
    /// cursor's thread (topic 17); elsewhere the comment dialog.
    fn add_comment(&mut self, _: &AddComment, window: &mut Window, cx: &mut Context<Self>) {
        if self.comment_at_cursor(window, cx) {
            return;
        }
        self.request(ObjectAction::AddComment, cx);
    }

    /// A click on the thing drawn for `stop`: plain clicks put the cursor
    /// there and open its object in the pane (a row, a host's band, a grid
    /// host, an event) or do what Enter does (a paging row); shift extends
    /// the marks from the anchor, ctrl/cmd toggles the row's mark. A
    /// snapshot may have moved it since it was drawn; the click still goes
    /// to the same thing.
    fn click_stop(
        &mut self,
        stop: &Stop,
        modifiers: Modifiers,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_preview() {
            // The editor's preview: the click picked the view (the item's
            // own listener); a host's paging still shows all or fewer.
            self.menus.close();
            if let (Stop::More { view, group }, Some(reference)) = (stop, self.sync(cx)) {
                let expanded = self
                    .pages
                    .get(&reference)
                    .and_then(|ui| ui.page.view_by_id(view)?.group(group).map(|g| g.expanded))
                    .unwrap_or(false);
                self.set_paging(&reference, view, group, !expanded, false, cx);
            }
            cx.notify();
            return;
        }
        window.focus(&self.focus_handle, cx);
        self.menus.close();
        let Some(reference) = self.sync(cx) else {
            return;
        };
        self.reveal = None;
        let Some(position) = self
            .pages
            .get(&reference)
            .and_then(|ui| ui.page.position(stop))
        else {
            // It left the page.
            cx.notify();
            return;
        };
        let row = matches!(stop, Stop::Row { .. });
        if row && modifiers.shift {
            self.change_selection(cx, |selection, page| {
                selection.extend_to(page, position).then_some(position)
            });
            return;
        }
        if row && modifiers.secondary() {
            self.change_selection(cx, |selection, page| {
                selection.toggle_mark(page, position).then_some(position)
            });
            return;
        }
        if let Some(ui) = self.pages.get_mut(&reference) {
            let page = ui.page.clone();
            if row {
                ui.selection.select(&page, position);
            } else if !matches!(stop, Stop::More { .. }) {
                ui.selection.place(&page, position);
            }
        }
        if !matches!(stop, Stop::Header(_)) {
            self.activate(&reference, stop, false, cx);
        }
        cx.notify();
    }

    /// The view header's or a band's chevron: folds or unfolds, nothing
    /// else (it never opens anything).
    fn click_chevron(&mut self, stop: &Stop, window: &mut Window, cx: &mut Context<Self>) {
        if !self.is_preview() {
            window.focus(&self.focus_handle, cx);
        }
        self.menus.close();
        let Some(reference) = self.sync(cx) else {
            return;
        };
        let Some(page) = self.pages.get(&reference).map(|ui| ui.page.clone()) else {
            return;
        };
        match stop {
            Stop::Header(view) => {
                let collapsed = page.view_by_id(view).is_some_and(|view| view.collapsed);
                self.fold_view(&reference, view, !collapsed, cx);
            }
            Stop::Band { view, group } => {
                let collapsed = page
                    .view_by_id(view)
                    .and_then(|page_view| page_view.group(group))
                    .is_some_and(|group| group.collapsed);
                self.fold_group(&reference, view, group, !collapsed, cx);
            }
            Stop::Thread {
                view,
                key: ItemKey::Band(object),
            } => {
                let object = object.clone();
                self.fold_thread(&reference, view, None, cx, move |folds| {
                    if !folds.collapsed.remove(&object) {
                        folds.collapsed.insert(object);
                    }
                });
            }
            _ => {}
        }
    }

    /// The rows on screen and the hosts of the bands there, which the
    /// engine fetches details for if it doesn't hold them current
    /// (debounced; see [`DashboardView::want_details`]).
    fn hydrate_rows_on_screen(&mut self, reference: &DashboardRef, cx: &mut Context<Self>) {
        let needs: Vec<ObjectKey> = {
            let state = self.state.read(cx);
            let Some(ui) = self.pages.get(reference) else {
                return;
            };
            let page = &ui.page;
            let snapshot = state.snapshot();
            let range =
                self.visible.start.min(page.items.len())..self.visible.end.min(page.items.len());
            page.items[range]
                .iter()
                .filter_map(|item| {
                    let view = page.views.get(item.view)?;
                    let key = match item.kind {
                        ItemKind::Row { row, .. } => match view.rows.get(row)? {
                            DashboardRow::Object(key) => key.clone(),
                            DashboardRow::Group { .. } => return None,
                        },
                        ItemKind::Band { group } => ObjectKey::Host {
                            name: view.groups.get(group)?.host.clone()?,
                        },
                        _ => return None,
                    };
                    row_worth_asking(snapshot, &key).then_some(key)
                })
                .collect()
        };
        self.want_details(needs, cx);
    }

    fn render_body(
        &mut self,
        reference: &DashboardRef,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let state = self.state.read(cx);
        if let Some(events) = &self.events {
            // The cluster section's events.
            if events.result.views.first().is_none_or(ViewResult::is_empty) {
                return note("No events yet: they show here as they happen.", theme);
            }
            return self.render_page(reference, window, cx);
        }
        let Some((_, dashboard)) = state.dashboard(reference) else {
            return note("This dashboard no longer exists.", theme);
        };
        if !is_multi_view(&dashboard.views) {
            // One view: its header is the page's.
            let view = primary_view(&dashboard.views);
            if let Some(denial) = view_denial(state, view) {
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
            let Some(result) = state.view_result(reference, &view.id) else {
                if state.connection().is_starting() {
                    return banner::loading_body(state, cx);
                }
                return note(format!("{} is being evaluated…", dashboard.name), theme);
            };
            if let Some(error) = &result.error {
                return EmptyState::new("This dashboard's filter doesn't work")
                    .leading(
                        Icon::new(IconName::TriangleAlert)
                            .size(px(20.))
                            .color(theme.states.fill.critical),
                    )
                    .detail(error.clone())
                    .max_width(px(560.))
                    .child(
                        div()
                            .w(px(520.))
                            .text_left()
                            .child(CodeBlock::new(view.filter.clone())),
                    )
                    .into_any_element();
            }
            if result.is_empty() {
                if view.is_list() {
                    return empty_dashboard(reference, view, result, cx);
                }
                return note(nothing_to_show(view), theme);
            }
        } else if state.result(reference).is_none() && state.connection().is_starting() {
            return banner::loading_body(state, cx);
        }
        self.render_page(reference, window, cx)
    }
}

/// Where `←` takes the cursor from a thread's line `key` (at stop
/// `position`) when there is nothing to fold there: `None` when the line
/// folds itself (an open band or fold, a paging row showing everything);
/// a folded band's view header; else its object's band.
fn thread_parent(page: &Page, view: &Id, key: &ItemKey, position: usize) -> Option<Stop> {
    let thread = page.view_by_id(view).and_then(|view| view.thread.as_ref());
    let line = |wanted: &dyn Fn(&threads::Line) -> Option<bool>| {
        thread.and_then(|thread| {
            thread
                .listing
                .lines
                .iter()
                .find_map(|keyed| wanted(&keyed.line))
        })
    };
    let folds_itself = match key {
        ItemKey::Band(object) => line(&|candidate| match candidate {
            threads::Line::Band {
                object: band,
                collapsed,
                ..
            } if band == object => Some(!collapsed),
            _ => None,
        }),
        ItemKey::Fold(name) => line(&|candidate| match candidate {
            threads::Line::Fold { downtime, open, .. } if downtime == name => Some(*open),
            _ => None,
        }),
        ItemKey::More(more) => line(&|candidate| match candidate {
            threads::Line::More { key, hidden } if key == more => Some(*hidden == 0),
            _ => None,
        }),
        ItemKey::Entry(_) | ItemKey::Service(..) => Some(false),
    }
    .unwrap_or(false);
    if folds_itself {
        return None;
    }
    if matches!(key, ItemKey::Band(_)) {
        return Some(Stop::Header(view.clone()));
    }
    // The nearest band above it in the same view, else the view's header.
    page.stops()[..position]
        .iter()
        .rev()
        .take_while(|entry| entry.stop.view() == view)
        .find_map(|entry| match &entry.stop {
            Stop::Thread {
                key: ItemKey::Band(_),
                ..
            } => Some(entry.stop.clone()),
            _ => None,
        })
        .or_else(|| Some(Stop::Header(view.clone())))
}

/// A paging row of a thread clicked: shows everything, or fewer again.
fn toggle_more(folds: &mut threads::Folds, more: &threads::MoreKey) {
    let open = match more {
        threads::MoreKey::Thread(object) => !folds.all_entries.contains(object),
        threads::MoreKey::Fold(name) => !folds.all_services.contains(name),
    };
    set_more(folds, more, open);
}

/// Shows everything a thread's paging row holds back (`all`), or fewer.
fn set_more(folds: &mut threads::Folds, more: &threads::MoreKey, all: bool) {
    match (more, all) {
        (threads::MoreKey::Thread(object), true) => {
            folds.all_entries.insert(object.clone());
        }
        (threads::MoreKey::Thread(object), false) => {
            folds.all_entries.remove(object);
        }
        (threads::MoreKey::Fold(name), true) => {
            folds.all_services.insert(name.clone());
        }
        (threads::MoreKey::Fold(name), false) => {
            folds.all_services.remove(name);
        }
    }
}

/// The page's viewport height as last drawn (the window's before the first
/// frame).
fn viewport_height(ui: &PageUi) -> Pixels {
    ui.scroll.bounds().size.height
}

/// Scrolls `ui`'s page so stop `position` shows: just into view, or
/// (`center`) in the middle. A row under a band keeps clear of the band
/// that sticks to the top while its rows scroll; an event also scrolls its
/// stream.
fn reveal_stop(ui: &PageUi, page: &Page, position: usize, center: bool) {
    let Some(entry) = page.stops().get(position) else {
        return;
    };
    let item = entry.item;
    let (top, bottom) =
        draw::cell_extent(page, entry).unwrap_or((page.top(item), page.top(item + 1)));
    if let Stop::Event { view, .. } = &entry.stop
        && let Some(handle) = ui.streams.get(view)
    {
        handle.scroll_to_item(entry.sub, gpui::ScrollStrategy::Nearest);
    }
    let viewport = viewport_height(ui);
    if viewport <= px(0.) {
        return;
    }
    let sticky = match page.items.get(item).map(|item| item.kind) {
        Some(ItemKind::Row { group: Some(_), .. } | ItemKind::More { .. }) => page.band_height(),
        _ => px(0.),
    };
    let offset = -ui.scroll.offset().y;
    let target = if center {
        (top - (viewport - (bottom - top)) / 2.).max(px(0.))
    } else if top - sticky < offset {
        (top - sticky).max(px(0.))
    } else if bottom > offset + viewport {
        bottom - viewport
    } else {
        return;
    };
    let max = (page.height() - viewport).max(px(0.));
    ui.scroll.set_offset(point(px(0.), -target.min(max)));
}

/// Scrolls `ui`'s page so the header of view `view_id` shows (the view
/// picked in the editor's inspector): at the top when it is out of view,
/// not at all when it shows.
fn reveal_view(ui: &PageUi, view_id: &str) {
    let page = &ui.page;
    let Some(view) = page.view_index(view_id) else {
        return;
    };
    let items = page.views[view].items.clone();
    if items.is_empty() {
        return;
    }
    let viewport = viewport_height(ui);
    if viewport <= px(0.) {
        return;
    }
    let top = page.top(items.start);
    let bottom = page.top(items.start + 1);
    let offset = -ui.scroll.offset().y;
    if top >= offset && bottom <= offset + viewport {
        return;
    }
    let max = (page.height() - viewport).max(px(0.));
    ui.scroll.set_offset(point(px(0.), -top.clamp(px(0.), max)));
}

impl Render for DashboardView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.is_preview() {
            return self.render_preview(window, cx);
        }
        let main_width = SplitLayout::main_width(window, self.sidebar_open, &cx.theme().metrics);
        let reference = self.current_reference(cx);
        let multi = reference
            .as_ref()
            .and_then(|reference| self.state.read(cx).dashboard(reference))
            .is_some_and(|(_, dashboard)| is_multi_view(&dashboard.views));
        let metrics = cx.theme().metrics;
        let split = SplitLayout::for_width_at_most(
            main_width,
            &metrics,
            if multi {
                metrics.pane_width.min(px(MULTI_VIEW_PANE))
            } else {
                metrics.pane_width
            },
        );
        let pane = reference
            .as_ref()
            .and_then(|reference| self.pages.get(reference))
            .and_then(|ui| ui.pane.as_ref())
            .map(|pane| pane.view.clone());
        let pane_width = match (&pane, split) {
            (Some(_), SplitLayout::Side { pane_width }) => Some(pane_width),
            _ => None,
        };
        let list_width = match pane_width {
            Some(pane_width) => main_width - pane_width - Metrics::RULE,
            None => main_width,
        };
        if pane.is_none() || split != SplitLayout::Cover {
            self.width = list_width;
        }
        let reference = self.sync(cx);
        // Marked rows take the action keys (marked rows, then the pane, then
        // the cursor): the pane's buttons show no key hints meanwhile.
        if let (Some(pane), Some(reference)) = (&pane, &reference) {
            let marked = self
                .pages
                .get(reference)
                .is_some_and(|ui| ui.selection.marked_count() > 0);
            pane.update(cx, |pane, _| pane.set_keys_elsewhere(marked));
        }
        let root = div()
            .id("dashboard-view")
            .key_context(DASHBOARD_CONTEXT)
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
            .on_action(cx.listener(Self::fold))
            .on_action(cx.listener(Self::unfold))
            .on_action(cx.listener(Self::next_view))
            .on_action(cx.listener(Self::next_chip))
            .on_action(cx.listener(Self::previous_chip))
            .on_action(cx.listener(Self::toggle_timeline))
            .on_action(cx.listener(Self::previous_view))
            .on_action(cx.listener(Self::acknowledge))
            .on_action(cx.listener(Self::schedule_downtime))
            .on_action(cx.listener(Self::check_now))
            .on_action(cx.listener(Self::add_comment))
            .flex()
            .flex_1()
            .min_w_0()
            .h_full();
        let pane_width = match (pane, split) {
            // Too narrow for both: the pane covers the page until it's
            // closed, like Icinga Web's single column.
            (Some(pane), SplitLayout::Cover) => {
                return root.child(div().flex().flex_1().min_w_0().h_full().child(pane));
            }
            (Some(pane), SplitLayout::Side { pane_width }) => Some((pane, pane_width)),
            (None, _) => None,
        };
        let column = self.render_list_column(reference.as_ref(), list_width, window, cx);
        if let Some(reference) = &reference {
            self.hydrate_rows_on_screen(reference, cx);
        }
        let split_border = cx.theme().colors.border_split;
        root.child(column.when(pane_width.is_some(), |list| {
            list.border_r_1().border_color(split_border)
        }))
        .when_some(pane_width, |view, (pane, width)| {
            view.child(div().flex().flex_none().w(width).h_full().child(pane))
        })
    }
}

impl DashboardView {
    /// The editor's preview: a single list's summary bar and rows as the
    /// dashboard will show them, or every view under its header; the
    /// view picked in the inspector is marked on its header.
    fn render_preview(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let reserved = self
            .preview
            .as_ref()
            .map_or(px(0.), |(page, _)| page.reserved);
        let main_width = SplitLayout::main_width(window, self.sidebar_open, &cx.theme().metrics);
        let width = (main_width - reserved).max(px(0.));
        self.width = width;
        let root = div()
            .id("editor-preview")
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full();
        let Some(reference) = self.sync(cx) else {
            return root;
        };
        let summary = self.render_summary(&reference, width, cx);
        let body = self.render_preview_body(&reference, window, cx);
        self.hydrate_rows_on_screen(&reference, cx);
        root.children(summary).child(body)
    }

    /// The preview's body: the page, or why a single list shows no rows.
    fn render_preview_body(
        &mut self,
        reference: &DashboardRef,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let Some((page, _)) = &self.preview else {
            return div().into_any_element();
        };
        if !is_multi_view(&page.views) {
            let view = primary_view(&page.views);
            let Some(result) = page.result.view(&view.id) else {
                return note("Evaluating…", theme);
            };
            if !view.is_list() && result.error.is_none() && result.is_empty() {
                return note(nothing_to_show(view), theme);
            }
            if let Some(error) = &result.error {
                return EmptyState::new("The filter doesn't work")
                    .leading(
                        Icon::new(IconName::TriangleAlert)
                            .size(px(20.))
                            .color(theme.states.fill.critical),
                    )
                    .detail(error.clone())
                    .max_width(px(520.))
                    .into_any_element();
            }
            if view.is_list() && result.rows().is_empty() {
                let text = if crate::editor::model::matches(&result.summary) == 0 {
                    "Nothing matches this filter."
                } else {
                    "Everything this dashboard would show is OK or handled."
                };
                return note(text, theme);
            }
        }
        self.render_page(reference, window, cx)
    }

    /// The page's column: header, load progress, banners, summary bar and
    /// body. Before anything arrived, a connection problem or the load
    /// fills the body; afterwards they show over the (last known) page.
    fn render_list_column(
        &mut self,
        reference: Option<&DashboardRef>,
        list_width: Pixels,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let header = self.render_header(reference, window, cx);
        let theme = cx.theme();
        let now = Timestamp::now();
        let state = self.state.read(cx);
        let placeholder = if state.environment().is_none() {
            Some(no_environment(theme))
        } else if state.has_no_objects() {
            match state.connection_notice(now) {
                Some(notice) => Some(banner::connection_body(&self.state, &notice, cx)),
                None if state.connection().is_starting() => Some(banner::loading_body(state, cx)),
                None => None,
            }
        } else {
            None
        };
        // The placeholder shows the connection's problem itself; notices
        // and settings problems still show over it.
        let banners = banner::banners(&self.state, now, placeholder.is_none(), cx);
        let progress = if placeholder.is_some() {
            None
        } else {
            banner::progress(state)
        };
        let summary = reference
            .filter(|_| placeholder.is_none())
            .and_then(|reference| self.render_summary(reference, list_width, cx));
        let selection_bar = reference
            .filter(|_| placeholder.is_none())
            .and_then(|reference| self.render_selection_bar(reference, list_width, cx));
        let body = match (placeholder, reference) {
            (Some(placeholder), _) => placeholder,
            (None, Some(reference)) => self.render_body(reference, window, cx),
            (None, None) => note("Select a dashboard in the sidebar.", cx.theme()),
        };
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
            .children(selection_bar)
    }

    /// Whether the selected dashboard has marked rows (the selection bar
    /// shows; toasts float above it).
    pub(crate) fn has_marks(&self, cx: &App) -> bool {
        self.state
            .read(cx)
            .selected()
            .and_then(|reference| self.pages.get(reference))
            .is_some_and(|ui| ui.selection.marked_count() > 0)
    }
}

/// How the page and an open pane share the main area.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum SplitLayout {
    /// Side by side, the pane `pane_width` wide: the design's 620px, less
    /// where the page would get narrower than its minimum.
    Side {
        /// The pane's width.
        pane_width: Pixels,
    },
    /// Too narrow for both: the pane covers the page.
    Cover,
}

impl SplitLayout {
    /// The layout for a main area `width` wide.
    pub(crate) fn for_width(width: Pixels, metrics: &Metrics) -> Self {
        Self::for_width_at_most(width, metrics, metrics.pane_width)
    }

    /// The layout for a main area `width` wide with a pane at most `widest`
    /// (a page of several views gives its headers more room: 4b's 560px).
    pub(crate) fn for_width_at_most(width: Pixels, metrics: &Metrics, widest: Pixels) -> Self {
        let pane_width = widest.min(width - metrics.list_min_width);
        if pane_width < metrics.pane_min_width {
            Self::Cover
        } else {
            Self::Side { pane_width }
        }
    }

    /// The layout in `window`.
    pub(crate) fn for_window(window: &Window, sidebar_open: bool, metrics: &Metrics) -> Self {
        Self::for_width(Self::main_width(window, sidebar_open, metrics), metrics)
    }

    /// The main area's width in `window`: the window less the sidebar.
    pub(crate) fn main_width(window: &Window, sidebar_open: bool, metrics: &Metrics) -> Pixels {
        let sidebar = if sidebar_open {
            metrics.sidebar_width
        } else {
            px(0.)
        };
        (window.viewport_size().width - sidebar).max(px(0.))
    }
}

/// The element id of `key`'s row in `view` under `group`: an object can
/// be listed in several views and under several groups.
pub(crate) fn row_id(view: Option<&str>, group: Option<&str>, key: &ObjectKey) -> ElementId {
    let name = match (view, group) {
        // A control character can't occur in Icinga object or group names.
        (Some(view), Some(group)) => format!("row:{view}\u{1f}{group}\u{1f}{key}"),
        (Some(view), None) => format!("row:{view}\u{1f}{key}"),
        (None, Some(group)) => format!("row:{group}\u{1f}{key}"),
        (None, None) => format!("row:{key}"),
    };
    ElementId::Name(name.into())
}

/// An object row; `show_host` adds `on <host>` after a service's name;
/// `times` says how its time reads.
pub(crate) fn object_row(
    snapshot: &ic_core::snapshot::Snapshot,
    id: ElementId,
    key: &ObjectKey,
    show_host: bool,
    times: ListTimes,
    now: Timestamp,
    theme: &Theme,
) -> ListRow {
    match rows::object_row(snapshot, key, now) {
        Some(row) => {
            let list_row = ListRow::new(id)
                .state(
                    StateCircle::new(row.state).handled(row.handled),
                    row.time(times).to_owned(),
                )
                .title(row.name)
                .detail(row.output);
            let list_row = match row.host.filter(|_| show_host) {
                Some(host) => list_row.context("on", host),
                None => list_row,
            };
            let list_row = match row.late {
                Some(late) => list_row.flag(late),
                None => list_row,
            };
            match row.tag {
                Some(tag) => list_row.tag(tag),
                None => list_row,
            }
        }
        // The core removed the object between evaluating the dashboard and
        // publishing the snapshot; the next snapshot drops the row.
        None => ListRow::new(id)
            .state(StateCircle::with_color(theme.states.fill.pending), "")
            .title(key.full_name())
            .detail("not in the latest snapshot"),
    }
}

/// The body of a single-list dashboard without rows.
fn empty_dashboard(
    reference: &DashboardRef,
    view: &View,
    result: &ViewResult,
    cx: &Context<DashboardView>,
) -> AnyElement {
    let theme = cx.theme();
    let summary = &result.summary;
    let checked = summary.ok
        + summary.critical
        + summary.warning
        + summary.unknown
        + summary.down
        + summary.unreachable;
    let title = format!("No {}", header::view_label(view));
    let ok = StateCircle::with_color(theme.states.fill.ok).size(CircleSize::Pane);
    if result.hidden > 0 {
        let reference = reference.clone();
        let view_id = view.id.clone();
        let handled = result.hidden as usize;
        // `3 handled problems are hidden.`, `1 handled service is hidden.`
        let what = rows::count_label(handled, view);
        let (number, noun) = what.split_once(' ').unwrap_or((what.as_str(), ""));
        return EmptyState::new(title)
            .leading(ok.handled(true))
            .detail(format!(
                "{number} handled {noun} {} hidden.",
                if handled == 1 { "is" } else { "are" }
            ))
            .child(
                Link::new("show-handled", "show handled").on_click(cx.listener(
                    move |this, _: &ClickEvent, _, cx| {
                        this.toggle_handled(&reference, &view_id, cx);
                    },
                )),
            )
            .into_any_element();
    }
    if checked == 0 && summary.pending == 0 {
        let filter = if view.filter.trim().is_empty() {
            "(no filter)".to_owned()
        } else {
            view.filter.clone()
        };
        return EmptyState::new("Nothing matches this dashboard")
            .leading(StateCircle::with_color(theme.states.fill.pending).size(CircleSize::Pane))
            .detail("No host or service matches its filter:")
            .max_width(px(560.))
            .child(div().w(px(520.)).text_left().child(CodeBlock::new(filter)))
            .into_any_element();
    }
    EmptyState::new(title)
        .leading(ok)
        .detail("Everything this dashboard shows is OK.")
        .into_any_element()
}

/// The main area without an environment (UI-05).
fn no_environment(theme: &Theme) -> AnyElement {
    EmptyState::new("No environment yet")
        .leading(
            Icon::new(IconName::Plus)
                .size(px(20.))
                .color(theme.colors.text_muted),
        )
        .detail(
            "Add an Icinga environment to start monitoring, or run icygui --demo to try \
             the app against a simulated Icinga.",
        )
        .max_width(px(560.))
        .into_any_element()
}

/// A muted line in the middle of the main area.
fn note(text: impl Into<gpui::SharedString>, theme: &Theme) -> AnyElement {
    div()
        .flex()
        .flex_1()
        .items_center()
        .justify_center()
        .text_size(theme.text.body)
        .text_color(theme.colors.text_muted)
        .child(text.into())
        .into_any_element()
}

/// Accessors for the UI tests (`ui_tests`, Linux only).
#[cfg(all(test, target_os = "linux"))]
impl DashboardView {
    /// The selected dashboard's page state (the draft's in the editor's
    /// preview).
    fn current(&self, cx: &App) -> Option<&PageUi> {
        if self.is_preview() {
            return self.pages.get(&preview_reference());
        }
        let reference = self.state.read(cx).selected()?;
        self.pages.get(reference)
    }

    /// The view picked in the editor's preview (marked on its header).
    pub(crate) fn picked_view(&self) -> Option<&str> {
        self.picked()
    }

    /// The cursor's object row in the selected dashboard: its index in its
    /// view's rows, and its object.
    pub(crate) fn cursor_in(&self, cx: &App) -> Option<(usize, ObjectKey)> {
        let ui = self.current(cx)?;
        let (position, stop) = ui.cursor_entry()?;
        let Stop::Row { key, .. } = stop else {
            return None;
        };
        let entry = &ui.page.stops()[position];
        match ui.page.items[entry.item].kind {
            ItemKind::Row { row, .. } => Some((row, key.clone())),
            _ => None,
        }
    }

    /// The stop under the cursor.
    pub(crate) fn cursor_stop(&self, cx: &App) -> Option<Stop> {
        Some(self.current(cx)?.cursor_entry()?.1.clone())
    }

    /// The object in the open pane, if any.
    pub(crate) fn pane_object(&self, cx: &App) -> Option<ObjectKey> {
        let pane = self.current(cx)?.pane.as_ref()?;
        Some(pane.view.read(cx).object().clone())
    }

    /// The open pane's view.
    pub(crate) fn pane(&self, cx: &App) -> Option<Entity<ObjectPane>> {
        Some(self.current(cx)?.pane.as_ref()?.view.clone())
    }

    /// The marked objects in page order.
    pub(crate) fn marked(&self, cx: &App) -> Vec<ObjectKey> {
        self.current(cx)
            .map(|ui| ui.selection.marked_in(ui.objects()))
            .unwrap_or_default()
    }

    /// The items built in the last frame (the page isn't built while a
    /// pane covers it). On a single list, items are its rows.
    pub(crate) fn visible_rows(&self) -> Range<usize> {
        self.visible.clone()
    }

    /// The times the object rows built in the last frame show.
    #[cfg(test)]
    pub(crate) fn shown_times(&self) -> &[String] {
        &self.shown_times
    }

    /// The open header menu.
    pub(crate) fn open_menu(&self) -> Option<HeaderMenu> {
        self.menus.open()
    }

    /// Where the page was drawn in the last frame it was drawn in.
    pub(crate) fn list_bounds(&self, cx: &App) -> Option<gpui::Bounds<Pixels>> {
        Some(self.current(cx)?.scroll.bounds())
    }

    /// The selected dashboard's page as last built.
    pub(crate) fn page(&self, cx: &App) -> Option<Rc<Page>> {
        Some(self.current(cx)?.page.clone())
    }

    /// Where item `index` of the page is in the window now, as drawn
    /// (scrolled).
    pub(crate) fn item_bounds(&self, index: usize, cx: &App) -> Option<gpui::Bounds<Pixels>> {
        let ui = self.current(cx)?;
        let viewport = ui.scroll.bounds();
        let offset = ui.scroll.offset().y;
        let top = viewport.top() + offset + ui.page.top(index);
        Some(gpui::Bounds::new(
            point(viewport.left(), top),
            gpui::size(viewport.size.width, ui.page.item_height(index)),
        ))
    }

    /// The page's group filter.
    pub(crate) fn group_filter(&self, cx: &App) -> Option<GroupFilter> {
        self.current(cx)?.filter.clone()
    }

    /// The comment field open in a stacked handling view (topic 17): the
    /// view's id, the object, the field.
    pub(crate) fn stacked_composer(
        &self,
    ) -> Option<(
        String,
        ObjectKey,
        Entity<crate::comments::field::CommentField>,
    )> {
        let open = self.composer.as_ref()?;
        Some((
            open.view.to_string(),
            open.composer.object.clone(),
            open.composer.field.clone(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pane_narrows_then_covers_the_list() {
        let metrics = Theme::dark().metrics;
        let side = |width: f32| SplitLayout::for_width(px(width), &metrics);
        // The design: a 1440px window less the 300px sidebar.
        assert_eq!(
            side(1140.),
            SplitLayout::Side {
                pane_width: px(620.)
            }
        );
        // 1280px: the list keeps its minimum, the pane gives way.
        assert_eq!(
            side(980.),
            SplitLayout::Side {
                pane_width: px(540.)
            }
        );
        assert_eq!(
            side(860.),
            SplitLayout::Side {
                pane_width: metrics.pane_min_width
            }
        );
        // The 900px minimum window with the sidebar: one column.
        assert_eq!(side(859.), SplitLayout::Cover);
        assert_eq!(side(600.), SplitLayout::Cover);
        assert_eq!(side(0.), SplitLayout::Cover);
        // Without the sidebar, 900px still fits both.
        assert_eq!(
            side(900.),
            SplitLayout::Side {
                pane_width: px(460.)
            }
        );
    }

    #[test]
    fn multi_view_dashboards_are_more_than_one_list() {
        let list = View::default();
        let grid = View {
            display: ic_config::ViewDisplay::HostGroupGrid,
            ..View::default()
        };
        assert!(!is_multi_view(std::slice::from_ref(&list)));
        assert!(is_multi_view(&[list.clone(), list.clone()]));
        assert!(
            !is_multi_view(&[grid]),
            "one view: its header is the page's"
        );
        assert!(!is_multi_view(&[]));
    }
}
