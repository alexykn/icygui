//! The main area for a dashboard: the list (screen 2a) with its header and
//! summary bar, and the detail pane beside it (screens 2b and 2c).
//!
//! Rows are rendered with GPUI's `uniform_list`, which builds only the rows
//! on screen: nothing per frame scales with the number of rows. The
//! selection follows objects by key across snapshot updates
//! ([`selection::ListSelection`]); every dashboard keeps its own selection,
//! scroll position and open pane.

mod header;
pub(crate) mod rows;
pub(crate) mod selection;

#[cfg(all(test, target_os = "linux"))]
pub(crate) use self::header::HeaderMenu;

use std::collections::HashMap;
use std::ops::Range;

use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Entity, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, Modifiers, ParentElement as _, Render, ScrollStrategy,
    Styled as _, Subscription, UniformListScrollHandle, Window, div, prelude::FluentBuilder as _,
    px, uniform_list,
};
use ic_config::{GroupBy, View};
use ic_core::snapshot::DashboardRow;
use ic_model::{ObjectKey, Timestamp};
use ic_rules::DashboardRef;
use ic_ui_kit::{
    ActiveTheme as _, CodeBlock, EmptyState, Icon, IconName, Link, ListRow, RowEmphasis, Scrollbar,
    StateCircle, Theme,
};

use self::header::HeaderMenus;
use self::selection::ListSelection;
use crate::actions::{
    Acknowledge, ActionRequest, AddComment, CheckNow, DASHBOARD_CONTEXT, Dismiss,
    ExtendSelectionNext, ExtendSelectionPrevious, MarkAll, ObjectAction, OpenAsTab, OpenSelected,
    ScheduleDowntime, SelectFirst, SelectLast, SelectNext, SelectPageDown, SelectPageUp,
    SelectPrevious, ToggleMark,
};
use crate::app_state::AppState;
use crate::chrome::WindowDrag;
use crate::pane::{ObjectPane, PaneEvent, PaneMode};

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

/// The dashboard list with its header, summary bar and detail pane.
pub(crate) struct DashboardView {
    state: Entity<AppState>,
    focus_handle: FocusHandle,
    lists: HashMap<DashboardRef, ListUi>,
    menus: HeaderMenus,
    sidebar_open: bool,
    drag: WindowDrag,
    /// The rows built in the last frame: the rows on screen. Sets the page
    /// size, and tells the core which rows to load details for (M2).
    visible: Range<usize>,
    _subscriptions: Vec<Subscription>,
}

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
            _subscriptions: subscriptions,
        }
    }

    /// Tells the view whether the sidebar is shown (the header then needs no
    /// window controls).
    pub(crate) fn set_sidebar_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.sidebar_open != open {
            self.sidebar_open = open;
            cx.notify();
        }
    }

    /// Puts the cursor on `key` in the selected dashboard and opens its pane
    /// (the notification and startup paths).
    pub(crate) fn open_object(&mut self, key: &ObjectKey, cx: &mut Context<Self>) {
        let Some(reference) = self.sync(cx) else {
            return;
        };
        if let Some(list) = self.lists.get_mut(&reference)
            && let Some(index) = list
                .selection
                .select_key(key)
                .then(|| list.selection.cursor())
                .flatten()
        {
            list.scroll.scroll_to_item(index, ScrollStrategy::Center);
        }
        self.open_pane(&reference, key.clone(), cx);
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
            pane.view.update(cx, |pane, cx| pane.show(key, cx));
        } else {
            let state = self.state.clone();
            let view = cx.new(|cx| ObjectPane::new(state, key, PaneMode::Split, cx));
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
        list.selection.update_rows(&rows);
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
        self.state.update(cx, |state, _| {
            state.request(ActionRequest { action, targets });
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

    /// A click on row `index`: plain clicks select the row and open its
    /// pane, shift extends the marks from the anchor, ctrl/cmd toggles the
    /// row's mark.
    fn click_row(
        &mut self,
        index: usize,
        modifiers: Modifiers,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);
        self.menus.close();
        if modifiers.shift {
            self.change_selection(cx, |selection| selection.extend_to(index).then_some(index));
        } else if modifiers.secondary() {
            self.change_selection(cx, |selection| {
                selection.toggle_mark(index).then_some(index)
            });
        } else {
            let Some(reference) = self.sync(cx) else {
                return;
            };
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
        // Under a host's header, `on <host>` would repeat it on every row.
        let show_host = view.group_by != GroupBy::Host;
        range
            .filter_map(|index| {
                let row = match selection.rows().get(index)? {
                    DashboardRow::Object(key) => {
                        let emphasis =
                            RowEmphasis::new(cursor == Some(index), selection.is_marked(key));
                        object_row(&snapshot, index, key, show_host, now, theme)
                            .emphasis(emphasis)
                            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                                this.click_row(index, event.modifiers(), window, cx);
                            }))
                    }
                    DashboardRow::Group { label, count } => {
                        let header =
                            group_header(&snapshot, &view, index, label, *count, now, theme);
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

    fn render_body(&self, reference: &DashboardRef, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let state = self.state.read(cx);
        let Some((_, dashboard)) = state.dashboard(reference) else {
            return note("This dashboard no longer exists.", theme);
        };
        let view = &dashboard.view;
        let Some(result) = state.result(reference) else {
            return note(format!("{} is being evaluated…", dashboard.name), theme);
        };
        if let Some(error) = &result.error {
            return EmptyState::new("This dashboard's filter doesn't work")
                .leading(
                    Icon::new(IconName::TriangleAlert)
                        .size(px(20.))
                        .color(theme.states.critical),
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
        let theme = cx.theme();
        let header = self.render_header(reference.as_ref(), window, cx);
        let summary = reference
            .as_ref()
            .and_then(|reference| self.render_summary(reference, cx));
        let body = if let Some(reference) = &reference {
            self.render_body(reference, cx)
        } else {
            let text = if self.state.read(cx).environment().is_some() {
                "Select a dashboard in the sidebar."
            } else {
                "Add an environment to start monitoring."
            };
            note(text, theme)
        };
        let pane = reference
            .as_ref()
            .and_then(|reference| self.lists.get(reference))
            .and_then(|list| list.pane.as_ref())
            .map(|pane| pane.view.clone());
        div()
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
            .h_full()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .when(pane.is_some(), |list| {
                        list.border_r_1().border_color(theme.colors.border_split)
                    })
                    .child(header)
                    .children(summary)
                    .child(body),
            )
            .when_some(pane, |view, pane| {
                view.child(
                    div()
                        .flex()
                        .flex_none()
                        .w(theme.metrics.pane_width)
                        .h_full()
                        .child(pane),
                )
            })
    }
}

/// An object row; `show_host` adds `on <host>` after a service's name.
fn object_row(
    snapshot: &ic_core::snapshot::Snapshot,
    index: usize,
    key: &ObjectKey,
    show_host: bool,
    now: Timestamp,
    theme: &Theme,
) -> ListRow {
    let id = ("row", index);
    match rows::object_row(snapshot, key, now) {
        Some(row) => {
            let list_row = ListRow::new(id)
                .leading(
                    StateCircle::new(row.state)
                        .handled(row.handled)
                        .caption(row.since),
                )
                .title(row.name)
                .detail(row.output);
            let list_row = match row.host.filter(|_| show_host) {
                Some(host) => list_row.context("on", host),
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
            .leading(StateCircle::with_color(theme.states.pending))
            .title(key.full_name())
            .detail("not in the latest snapshot"),
    }
}

/// A group header row.
fn group_header(
    snapshot: &ic_core::snapshot::Snapshot,
    view: &View,
    index: usize,
    label: &str,
    count: usize,
    now: Timestamp,
    theme: &Theme,
) -> ListRow {
    let group = rows::group_row(snapshot, view, label, count, now);
    let row = ListRow::new(("group", index))
        .header(true)
        .title(group.label);
    match group.host {
        Some(host) => row
            .leading(
                StateCircle::new(host.state)
                    .handled(host.handled)
                    .caption(host.since),
            )
            .detail(host.output)
            .tag(group.count),
        None => row
            .leading(
                Icon::new(IconName::Folder)
                    .size(px(16.))
                    .color(theme.colors.text_faint),
            )
            .detail(group.count),
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
    let ok = StateCircle::with_color(theme.states.ok).size(ic_ui_kit::CircleSize::Pane);
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
            .leading(
                StateCircle::with_color(theme.states.pending).size(ic_ui_kit::CircleSize::Pane),
            )
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

    /// The rows built in the last frame.
    pub(crate) fn visible_rows(&self) -> Range<usize> {
        self.visible.clone()
    }

    /// The open header menu.
    pub(crate) fn open_menu(&self) -> Option<HeaderMenu> {
        self.menus.open()
    }
}
