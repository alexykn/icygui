//! The main area for a dashboard: the list (screen 2a) with its header and
//! summary bar, and the detail pane beside it (screens 2b and 2c). Where the
//! window is too narrow for both, the pane narrows down to a minimum and
//! then covers the list ([`SplitLayout`]).
//!
//! Rows are rendered with GPUI's `uniform_list`, which builds only the rows
//! on screen: nothing per frame scales with the number of rows. The
//! selection follows objects by key across snapshot updates
//! ([`selection::ListSelection`]); every dashboard keeps its own selection,
//! scroll position and open pane.

pub(crate) mod bulk;
pub(crate) mod header;
pub(crate) mod rows;
pub(crate) mod selection;

pub(crate) use self::bulk::SELECTION_BAR_HEIGHT;
#[cfg(all(test, target_os = "linux"))]
pub(crate) use self::header::HeaderMenu;

use std::collections::HashMap;
use std::ops::Range;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, ElementId, Entity, FocusHandle,
    Focusable, InteractiveElement as _, IntoElement, Modifiers, ParentElement as _, Pixels, Render,
    ScrollStrategy, Styled as _, Subscription, Task, UniformListScrollHandle, Window, div,
    prelude::FluentBuilder as _, uniform_list,
};
use ic_config::{GroupBy, ListTimes, View};
use ic_core::snapshot::DashboardRow;
use ic_model::{ObjectKey, Timestamp};
use ic_rules::DashboardRef;
use ic_ui_kit::{
    ActiveTheme as _, CircleSize, CodeBlock, Density, EmptyState, Icon, IconName, Link, ListRow,
    Metrics, RowEmphasis, Scrollbar, StateCircle, Theme, px,
};

use self::header::HeaderMenus;
use self::selection::ListSelection;
use crate::actions::{
    Acknowledge, ActionRequest, AddComment, CheckNow, DASHBOARD_CONTEXT, Dismiss,
    ExtendSelectionNext, ExtendSelectionPrevious, MarkAll, ObjectAction, OpenAsTab, OpenSelected,
    ScheduleDowntime, SelectFirst, SelectLast, SelectNext, SelectPageDown, SelectPageUp,
    SelectPrevious, ToggleMark,
};
use crate::app_state::hydration::row_worth_asking;
use crate::app_state::{AppState, Hydrated};
use crate::banner;
use crate::chrome::WindowDrag;
use crate::pane::{ObjectPane, PaneEvent, PaneMode};

/// The rows on screen are asked for their details once scrolling has
/// rested this long, so flinging through a long list costs one request.
pub(crate) const HYDRATE_DEBOUNCE: Duration = Duration::from_millis(300);

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

/// A dashboard's list state.
struct ListUi {
    selection: ListSelection,
    scroll: UniformListScrollHandle,
    pane: Option<OpenPane>,
}

/// The pane open beside the list.
struct OpenPane {
    view: Entity<ObjectPane>,
    _events: Subscription,
}

/// What the dashboard view asks the workspace to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DashboardEvent {
    /// Open the editor for this dashboard (the header's `···`).
    Edit(DashboardRef),
}

/// The dashboard list with its header, summary bar and detail pane.
pub(crate) struct DashboardView {
    state: Entity<AppState>,
    focus_handle: FocusHandle,
    lists: HashMap<DashboardRef, ListUi>,
    menus: HeaderMenus,
    sidebar_open: bool,
    drag: WindowDrag,
    /// The rows built in the last frame: the rows on screen. Sets the page
    /// size.
    visible: Range<usize>,
    /// The times the object rows built in the last frame show, for tests.
    #[cfg(test)]
    shown_times: Vec<String>,
    /// The rows on screen without output, last asked for (or about to
    /// be), and in which wake of the environment (`AppState::wake`): after
    /// waking up from quiet mode they are asked for again.
    hydration_wanted: (Vec<ObjectKey>, u64),
    /// Asks for them once scrolling rests.
    hydrate_task: Option<Task<()>>,
    /// The object revealed while the rows were quiet mode's.
    reveal: Option<Reveal>,
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
            lists: HashMap::new(),
            menus: HeaderMenus::default(),
            sidebar_open: true,
            drag: WindowDrag::default(),
            visible: 0..0,
            #[cfg(test)]
            shown_times: Vec::new(),
            hydration_wanted: (Vec::new(), 0),
            hydrate_task: None,
            reveal: None,
            _subscriptions: subscriptions,
        }
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
            // A pane covering a narrow list needs the window controls then.
            for pane in self.lists.values().filter_map(|list| list.pane.as_ref()) {
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
        if let Some(list) = self.lists.get_mut(&reference) {
            put_cursor_on(list, key);
        }
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
        if let Some(open) = self
            .lists
            .get(&reference)
            .and_then(|list| list.pane.as_ref())
        {
            open.view.update(cx, |view, cx| view.show(pane, cx));
        }
    }

    /// Opens `key`'s pane beside the list, or shows `key` in the open one.
    fn open_pane(&mut self, reference: &DashboardRef, key: ObjectKey, cx: &mut Context<Self>) {
        let Some(list) = self.lists.get_mut(reference) else {
            return;
        };
        if let Some(pane) = &list.pane {
            pane.view.update(cx, |pane, cx| pane.open(key, cx));
        } else {
            let state = self.state.clone();
            let sidebar_open = self.sidebar_open;
            let view = cx.new(|cx| {
                let mut pane = ObjectPane::new(state, key, PaneMode::Split, cx);
                pane.set_sidebar_open(sidebar_open, cx);
                pane
            });
            let closed = reference.clone();
            let events = cx.subscribe(&view, move |this, _, event: &PaneEvent, cx| match event {
                PaneEvent::Close => {
                    this.close_pane(&closed, cx);
                }
            });
            list.pane = Some(OpenPane {
                view,
                _events: events,
            });
        }
        cx.notify();
    }

    fn close_pane(&mut self, reference: &DashboardRef, cx: &mut Context<Self>) -> bool {
        self.reveal = None;
        let closed = self
            .lists
            .get_mut(reference)
            .and_then(|list| list.pane.take())
            .is_some();
        if closed {
            cx.notify();
        }
        closed
    }

    /// Makes sure the selected dashboard has its list state and that the
    /// selection refers to its current rows. Cheap when nothing changed.
    fn sync(&mut self, cx: &App) -> Option<DashboardRef> {
        let state = self.state.read(cx);
        if self.lists.len() > 1 {
            self.lists
                .retain(|reference, _| state.dashboard(reference).is_some());
        }
        let reference = state.selected()?.clone();
        let rows = state
            .result(&reference)
            .map(|result| result.rows.clone())
            .unwrap_or_default();
        let list = self
            .lists
            .entry(reference.clone())
            .or_insert_with(|| ListUi {
                selection: ListSelection::new(rows.clone()),
                scroll: UniformListScrollHandle::new(),
                pane: None,
            });
        let changed = list.selection.update_rows(&rows);
        if let Some(reveal) = &self.reveal
            && reveal.dashboard == reference
        {
            let settling = state.rows_settling();
            let placed = (changed || !settling) && put_cursor_on(list, &reveal.key);
            if (placed && !settling) || reveal.since.elapsed() > REVEAL_FOLLOW {
                self.reveal = None;
            }
        }
        Some(reference)
    }

    /// Runs `change` on the selected dashboard's selection; scrolls to the
    /// cursor and, if the pane is open, shows the cursor's object in it.
    fn change_selection(
        &mut self,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut ListSelection) -> Option<usize>,
    ) {
        let Some(reference) = self.sync(cx) else {
            return;
        };
        self.reveal = None;
        let Some(list) = self.lists.get_mut(&reference) else {
            return;
        };
        if let Some(index) = change(&mut list.selection) {
            list.scroll.scroll_to_item(index, ScrollStrategy::Nearest);
            if let (Some(pane), Some(key)) = (&list.pane, list.selection.cursor_key()) {
                let key = key.clone();
                pane.view.update(cx, |pane, cx| pane.show(key, cx));
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

    fn open_selected(&mut self, _: &OpenSelected, _: &mut Window, cx: &mut Context<Self>) {
        let Some(reference) = self.sync(cx) else {
            return;
        };
        let Some(list) = self.lists.get_mut(&reference) else {
            return;
        };
        if list.selection.cursor().is_none() {
            list.selection.move_by(1);
        }
        if let Some(key) = list.selection.cursor_key().cloned() {
            self.open_pane(&reference, key, cx);
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
        let cleared = self
            .lists
            .get_mut(&reference)
            .is_some_and(|list| list.selection.clear_marks());
        if cleared {
            cx.notify();
        } else {
            cx.propagate();
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

    /// The objects an action from elsewhere (the command palette) applies
    /// to: the marked rows, else the pane's or the cursor's object.
    pub(crate) fn action_targets(&mut self, cx: &mut Context<Self>) -> Vec<ObjectKey> {
        let Some(reference) = self.sync(cx) else {
            return Vec::new();
        };
        let marked = self
            .lists
            .get(&reference)
            .map(|list| list.selection.marked_keys())
            .unwrap_or_default();
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
        let list = self.lists.get(&reference)?;
        match &list.pane {
            Some(pane) => Some(pane.view.read(cx).object().clone()),
            None => list.selection.cursor_key().cloned(),
        }
    }

    /// Sends `action` for the marked rows, or else the pane's or the
    /// cursor's object.
    fn request(&mut self, action: ObjectAction, cx: &mut Context<Self>) {
        let Some(reference) = self.sync(cx) else {
            return;
        };
        let marked = self
            .lists
            .get(&reference)
            .map(|list| list.selection.marked_keys())
            .unwrap_or_default();
        let targets = if marked.is_empty() {
            self.focused_object(cx).into_iter().collect()
        } else {
            marked
        };
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

    fn add_comment(&mut self, _: &AddComment, _: &mut Window, cx: &mut Context<Self>) {
        self.request(ObjectAction::AddComment, cx);
    }

    /// A click on `key`'s row, drawn as row `index`: plain clicks select the
    /// row and open its pane, shift extends the marks from the anchor,
    /// ctrl/cmd toggles the row's mark. A snapshot may have moved the row
    /// since it was drawn; the click still goes to `key`.
    fn click_row(
        &mut self,
        index: usize,
        key: &ObjectKey,
        modifiers: Modifiers,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);
        self.menus.close();
        let Some(reference) = self.sync(cx) else {
            return;
        };
        self.reveal = None;
        let Some(index) = self
            .lists
            .get(&reference)
            .and_then(|list| list.selection.rows().locate(index, key))
        else {
            // The object left the list.
            cx.notify();
            return;
        };
        if modifiers.shift {
            self.change_selection(cx, |selection| selection.extend_to(index).then_some(index));
        } else if modifiers.secondary() {
            self.change_selection(cx, |selection| {
                selection.toggle_mark(index).then_some(index)
            });
        } else {
            let key = self.lists.get_mut(&reference).and_then(|list| {
                list.selection
                    .select(index)
                    .then(|| list.selection.cursor_key().cloned())
                    .flatten()
            });
            if let Some(key) = key {
                self.open_pane(&reference, key, cx);
            }
        }
    }

    /// A click on a host's group header opens the host's pane.
    fn click_group(&mut self, host: ObjectKey, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle, cx);
        self.reveal = None;
        if let Some(reference) = self.sync(cx) {
            self.open_pane(&reference, host, cx);
        }
    }

    /// Builds the rows in `range` (the ones on screen).
    fn render_rows(
        &mut self,
        reference: &DashboardRef,
        range: Range<usize>,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        self.visible = range.clone();
        #[cfg(test)]
        self.shown_times.clear();
        let state = self.state.read(cx);
        let snapshot = state.snapshot().clone();
        let Some(view) = state
            .dashboard(reference)
            .map(|(_, dashboard)| dashboard.view.clone())
        else {
            return Vec::new();
        };
        let Some(list) = self.lists.get(reference) else {
            return Vec::new();
        };
        let selection = &list.selection;
        let cursor = selection.cursor();
        let theme = cx.theme();
        let now = Timestamp::now();
        let times = state.appearance().list_times;
        // Under a host's header, `on <host>` would repeat it on every row.
        let show_host = view.group_by != GroupBy::Host;
        let grouped = view.group_by != GroupBy::None;
        let indent = if grouped {
            theme.metrics.row_indent
        } else {
            px(0.)
        };
        // Rows are identified by their object (and group: an object can be
        // listed under several), not their position, so a press and release
        // with a reordering snapshot in between can't click another object.
        let mut group = if grouped {
            selection.rows().group_of(range.start).map(str::to_owned)
        } else {
            None
        };
        range
            .filter_map(|index| {
                let row = match selection.rows().get(index)? {
                    DashboardRow::Object(key) => {
                        let emphasis =
                            RowEmphasis::new(cursor == Some(index), selection.is_marked(key));
                        let id = row_id(group.as_deref(), key);
                        let clicked = key.clone();
                        let row = object_row(&snapshot, id, key, show_host, times, now, theme);
                        #[cfg(test)]
                        self.shown_times
                            .push(row.time_text().map(ToString::to_string).unwrap_or_default());
                        // An action on its way shows instead of the tag.
                        let row = match self.state.read(cx).pending_label(key) {
                            Some(pending) => row.tag(pending),
                            None => row,
                        };
                        row.indent(indent).emphasis(emphasis).on_click(cx.listener(
                            move |this, event: &ClickEvent, window, cx| {
                                this.click_row(index, &clicked, event.modifiers(), window, cx);
                            },
                        ))
                    }
                    DashboardRow::Group { label, count } => {
                        group = Some(label.clone());
                        let header =
                            group_header(&snapshot, &view, label, *count, times, now, theme);
                        if view.group_by == GroupBy::Host {
                            let host = ObjectKey::host(label);
                            header.on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.click_group(host.clone(), window, cx);
                            }))
                        } else {
                            header
                        }
                    }
                };
                Some(row.into_any_element())
            })
            .collect()
    }

    /// The rows on screen in `reference`'s list, from its scroll position
    /// and height (every row is equally tall). The list's own calls to
    /// build rows can't say this: it also builds the first row to measure
    /// it.
    fn rows_on_screen(&self, reference: &DashboardRef, total: usize, cx: &App) -> Range<usize> {
        let Some(list) = self.lists.get(reference) else {
            return 0..0;
        };
        let row_height = f32::from(Metrics::with_rule(cx.theme().metrics.row_height));
        let scroll = list.scroll.0.borrow();
        let viewport = f32::from(scroll.base_handle.bounds().size.height);
        let offset = -f32::from(scroll.base_handle.offset().y);
        if row_height <= 0. || viewport <= 0. {
            return 0..0;
        }
        let first = (offset.max(0.) / row_height).floor();
        let count = (viewport / row_height).ceil() + 1.;
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "row indices from non-negative, finite pixel ratios"
        )]
        let (first, count) = (first as usize, count as usize);
        let start = first.min(total);
        start..(start + count).min(total)
    }

    /// Offers the engine the rows on screen, which fetches those it doesn't
    /// hold current (debounced; see [`DashboardView::want_details`]).
    fn hydrate_rows_on_screen(&mut self, reference: &DashboardRef, cx: &mut Context<Self>) {
        let needs: Vec<ObjectKey> = {
            let state = self.state.read(cx);
            let Some(result) = state.result(reference) else {
                return;
            };
            let range = self.rows_on_screen(reference, result.rows.len(), cx);
            let snapshot = state.snapshot();
            result.rows[range]
                .iter()
                .filter_map(|row| match row {
                    DashboardRow::Object(key) if row_worth_asking(snapshot, key) => {
                        Some(key.clone())
                    }
                    _ => None,
                })
                .collect()
        };
        self.want_details(needs, cx);
    }

    fn render_body(&self, reference: &DashboardRef, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let state = self.state.read(cx);
        let Some((_, dashboard)) = state.dashboard(reference) else {
            return note("This dashboard no longer exists.", theme);
        };
        let view = &dashboard.view;
        if let Some(denial) = state.query_denial(view.object_kind) {
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
        let Some(result) = state.result(reference) else {
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
        if result.rows.is_empty() {
            return empty_dashboard(reference, view, &result.summary, cx);
        }
        let Some(list) = self.lists.get(reference) else {
            return note("", theme);
        };
        let scroll = list.scroll.clone();
        let list_reference = reference.clone();
        let rows = uniform_list(
            "dashboard-rows",
            result.rows.len(),
            cx.processor(move |this, range: Range<usize>, _window, cx| {
                this.render_rows(&list_reference, range, cx)
            }),
        )
        .track_scroll(&scroll)
        .size_full();
        div()
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(rows)
            .child(Scrollbar::vertical(&scroll))
            .into_any_element()
    }
}

impl Render for DashboardView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let reference = self.sync(cx);
        let pane = reference
            .as_ref()
            .and_then(|reference| self.lists.get(reference))
            .and_then(|list| list.pane.as_ref())
            .map(|pane| pane.view.clone());
        // Marked rows take the action keys (marked rows, then the pane, then
        // the cursor): the pane's buttons show no key hints meanwhile.
        if let (Some(pane), Some(reference)) = (&pane, &reference) {
            let marked = self
                .lists
                .get(reference)
                .is_some_and(|list| list.selection.marked_count() > 0);
            pane.update(cx, |pane, _| pane.set_keys_elsewhere(marked));
        }
        let main_width = SplitLayout::main_width(window, self.sidebar_open, &cx.theme().metrics);
        let split = SplitLayout::for_width(main_width, &cx.theme().metrics);
        // The list shows unless a pane covers it.
        if let Some(reference) = &reference
            && (pane.is_none() || split != SplitLayout::Cover)
        {
            self.hydrate_rows_on_screen(reference, cx);
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
            .on_action(cx.listener(Self::acknowledge))
            .on_action(cx.listener(Self::schedule_downtime))
            .on_action(cx.listener(Self::check_now))
            .on_action(cx.listener(Self::add_comment))
            .flex()
            .flex_1()
            .min_w_0()
            .h_full();
        let pane_width = match (pane, split) {
            // Too narrow for both: the pane covers the list until it's
            // closed, like Icinga Web's single column.
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
        let column = self.render_list_column(reference.as_ref(), list_width, window, cx);
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
    /// The list's column: header, load progress, banners, summary bar and
    /// body. Before anything arrived, a connection problem or the load
    /// fills the body; afterwards they show over the (last known) list.
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
        let banners = if placeholder.is_some() {
            Vec::new()
        } else {
            banner::banners(&self.state, now, cx)
        };
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
            (None, Some(reference)) => self.render_body(reference, cx),
            (None, None) => note("Select a dashboard in the sidebar.", theme),
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
            .and_then(|reference| self.lists.get(reference))
            .is_some_and(|list| list.selection.marked_count() > 0)
    }
}

/// How the list and an open pane share the main area.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum SplitLayout {
    /// Side by side, the pane `pane_width` wide: the design's 620px, less
    /// where the list would get narrower than its minimum.
    Side {
        /// The pane's width.
        pane_width: Pixels,
    },
    /// Too narrow for both: the pane covers the list.
    Cover,
}

impl SplitLayout {
    /// The layout for a main area `width` wide.
    pub(crate) fn for_width(width: Pixels, metrics: &Metrics) -> Self {
        let pane_width = metrics.pane_width.min(width - metrics.list_min_width);
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

/// Puts `list`'s cursor on `key`'s row and scrolls it to the middle.
/// Returns whether the list has it.
fn put_cursor_on(list: &mut ListUi, key: &ObjectKey) -> bool {
    let Some(index) = list
        .selection
        .select_key(key)
        .then(|| list.selection.cursor())
        .flatten()
    else {
        return false;
    };
    list.scroll.scroll_to_item(index, ScrollStrategy::Center);
    true
}

/// The element id of `key`'s row under `group`.
pub(crate) fn row_id(group: Option<&str>, key: &ObjectKey) -> ElementId {
    let name = match group {
        // A control character can't occur in Icinga object or group names.
        Some(group) => format!("row:{group}\u{1f}{key}"),
        None => format!("row:{key}"),
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

/// A group header row: a darker band with the group's name in semibold;
/// grouped by host, the host's state as a compact circle and its output.
/// Compact rows have no second line: the count moves to the tag.
pub(crate) fn group_header(
    snapshot: &ic_core::snapshot::Snapshot,
    view: &View,
    label: &str,
    count: usize,
    times: ListTimes,
    now: Timestamp,
    theme: &Theme,
) -> ListRow {
    let group = rows::group_row(snapshot, view, label, count, now);
    let row = ListRow::new(ElementId::Name(format!("group:{label}").into()))
        .header(true)
        .title(group.label);
    if let Some(host) = group.host {
        return row
            .state(
                StateCircle::new(host.state)
                    .size(CircleSize::Compact)
                    .handled(host.handled),
                host.time(times).to_owned(),
            )
            .detail(host.output)
            .tag(group.count);
    }
    let row = row.leading(
        Icon::new(IconName::Folder)
            .size(px(14.))
            .color(theme.colors.text_muted),
    );
    if theme.density == Density::Compact {
        row.tag(group.count)
    } else {
        row.detail(group.count)
    }
}

/// The body of a dashboard without rows.
fn empty_dashboard(
    reference: &DashboardRef,
    view: &View,
    summary: &ic_core::snapshot::Summary,
    cx: &Context<DashboardView>,
) -> AnyElement {
    let theme = cx.theme();
    let checked = summary.ok
        + summary.critical
        + summary.warning
        + summary.unknown
        + summary.down
        + summary.unreachable;
    let title = format!("No {}", header::view_label(view));
    let ok = StateCircle::with_color(theme.states.fill.ok).size(CircleSize::Pane);
    if view.hide_handled && summary.handled > 0 {
        let reference = reference.clone();
        let handled = summary.handled;
        return EmptyState::new(title)
            .leading(ok.handled(true))
            .detail(format!(
                "{handled} handled {} hidden.",
                if handled == 1 {
                    "problem is"
                } else {
                    "problems are"
                }
            ))
            .child(
                Link::new("show-handled", "show handled").on_click(cx.listener(
                    move |this, _: &ClickEvent, _, cx| {
                        let reference = reference.clone();
                        this.state.update(cx, |state, cx| {
                            if state.update_view(&reference, |view| view.hide_handled = false) {
                                cx.notify();
                            }
                        });
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
    /// The selected dashboard's list state.
    fn current(&self, cx: &App) -> Option<&ListUi> {
        let reference = self.state.read(cx).selected()?;
        self.lists.get(reference)
    }

    /// The cursor row in the selected dashboard.
    pub(crate) fn cursor_in(&self, cx: &App) -> Option<(usize, ObjectKey)> {
        let selection = &self.current(cx)?.selection;
        Some((selection.cursor()?, selection.cursor_key()?.clone()))
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

    /// The marked objects in row order.
    pub(crate) fn marked(&self, cx: &App) -> Vec<ObjectKey> {
        self.current(cx)
            .map(|list| list.selection.marked_keys())
            .unwrap_or_default()
    }

    /// The rows built in the last frame (the list isn't built while a pane
    /// covers it).
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

    /// Where the list was drawn in the last frame it was drawn in.
    pub(crate) fn list_bounds(&self, cx: &App) -> Option<gpui::Bounds<Pixels>> {
        let scroll = self.current(cx)?.scroll.0.borrow();
        Some(scroll.base_handle.bounds())
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
}
