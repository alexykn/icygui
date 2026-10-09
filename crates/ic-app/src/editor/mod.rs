//! The dashboard editor (DASH-04; topic 04, 4c–4e; topic 05, 5e), in the
//! main area: the header says what is edited, with *discard* and *save
//! dashboard*; the live preview of the whole dashboard fills the left, as
//! the dashboard will show it; the inspector (372px) the right, full
//! height: the dashboard's name, group and notifications, its **views**
//! (each with a drag handle, its display's icon, its name, what it shows
//! and `···`; *add view* under them), and below a rule the settings of the
//! view selected in the list, which depend on its display.
//!
//! The view selected in the inspector is marked in the preview on its
//! header only (the focus bar and a faint accent tint, nothing around its
//! body); a click in a view of the preview selects it in the inspector.
//! Views are reordered by dragging their handle, with alt-↑/↓ or from
//! their `···` (move up, move down), duplicated, collapsed by default, and
//! removed without a question: *discard* brings everything back.
//!
//! Every change asks the core for a preview of every view
//! (`Command::PreviewDashboard`), at once for changes to the views list
//! and once typing rests otherwise: the filters are validated there (a
//! parse error names its line and column, marked under the field), and the
//! counts and rows come back with it. A draft whose filters don't work
//! can't be saved: a save parses every filter at once and waits for a
//! pending preview's verdict.
//!
//! Keys: `secondary-s` saves, Escape discards (asking first when something
//! was changed); with the views list focused, ↑/↓ select a view, alt-↑/↓
//! move it, `secondary-backspace` removes it. Showing another dashboard or
//! tab keeps a changed draft for the next time the same dashboard is
//! edited.

mod inspector;
mod mark;
pub(crate) mod model;

use std::time::Duration;

use gpui::{
    Action, AnyElement, App, AppContext as _, ClickEvent, Context, Entity, EventEmitter,
    FocusHandle, Focusable, FontWeight, InteractiveElement as _, IntoElement, KeyBinding,
    ParentElement as _, Render, ScrollHandle, SharedString, Styled as _, Subscription, Task,
    Window, div, prelude::FluentBuilder as _,
};
use ic_config::{View, ViewDisplay};
use ic_core::snapshot::DashboardResult;
use ic_rules::DashboardRef;
use ic_ui_kit::input::{Escape, InputEvent, InputState, TextareaState};
use ic_ui_kit::{ActiveTheme as _, Button, Metrics, Theme, px};

pub(crate) use self::model::EditorTarget;
use crate::app_state::AppState;
use crate::app_state::editing::DashboardDraft;
use crate::chrome::{Controls, WindowDrag};
use crate::dashboard::{DashboardEvent, DashboardView, PreviewPage};
use crate::lists::model::ListKind;
use crate::lists::view::{ListSource, RecordList, RecordListEvent};
use crate::menu_state::OpenMenu;
use crate::workspace::sidebar_reopen;

/// Key context of the dashboard editor.
pub(crate) const EDITOR_CONTEXT: &str = "DashboardEditor";

/// The preview is asked for once typing has rested this long.
const PREVIEW_DEBOUNCE: Duration = Duration::from_millis(250);

/// Width of the inspector column (the design's).
pub(crate) const INSPECTOR_WIDTH: f32 = 372.;

/// Saves the dashboard being edited.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct SaveDashboard;

/// Leaves the editor without saving.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct DiscardDashboard;

/// Moves the selected view up the list (and the page).
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct MoveViewUp;

/// Moves the selected view down the list (and the page).
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct MoveViewDown;

/// Removes the selected view (*discard* brings it back).
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct RemoveView;

/// Selects the next view of the list.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct SelectNextView;

/// Selects the previous view of the list.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct SelectPreviousView;

/// Registers the editor's key bindings. Escape in a field reaches the
/// editor as the field's own `Escape` once it has nothing to dismiss.
pub(crate) fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("secondary-s", SaveDashboard, Some(EDITOR_CONTEXT)),
        KeyBinding::new("escape", DiscardDashboard, Some(EDITOR_CONTEXT)),
        KeyBinding::new("alt-up", MoveViewUp, Some(EDITOR_CONTEXT)),
        KeyBinding::new("alt-down", MoveViewDown, Some(EDITOR_CONTEXT)),
        KeyBinding::new("secondary-backspace", RemoveView, Some(EDITOR_CONTEXT)),
        KeyBinding::new("up", SelectPreviousView, Some(EDITOR_CONTEXT)),
        KeyBinding::new("down", SelectNextView, Some(EDITOR_CONTEXT)),
    ]);
}

/// What the editor asks the workspace to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum EditorEvent {
    /// Saved (showing the dashboard) or discarded: close the editor.
    Closed,
    /// Discard asked for while the draft has changes: ask first.
    DiscardChanges,
    /// Delete the edited dashboard (after asking).
    Delete(DashboardRef),
}

/// The editor's dropdown menus.
#[derive(Clone, Debug, PartialEq, Eq)]
enum EditorMenu {
    Group,
    Sort,
    Display,
    /// *add view*: the display first (4d).
    AddView,
    /// A view's `···` in the views list (4e), by id.
    ViewOptions(String),
    /// The sidebar mark's dropdown: state or icon.
    Mark,
    /// The sidebar mark's icon picker (14-r5-f).
    IconPicker,
    /// *copy filter from…* (14-r4-e).
    CopyFilter,
    /// A view's *rows*.
    Rows,
    /// A handling or downtimes view's sort.
    ThreadSort,
}

/// The editor's text fields: the dashboard's name, and the selected
/// view's name, filter and custom variable.
struct Inputs {
    name: Entity<InputState>,
    view_name: Entity<InputState>,
    filter: Entity<TextareaState>,
    custom_var: Entity<InputState>,
    icon_search: Entity<InputState>,
    copy_search: Entity<InputState>,
}

impl Inputs {
    /// The fields for `draft`, showing `first` view's values.
    fn new(
        draft: &DashboardDraft,
        first: &View,
        window: &mut Window,
        cx: &mut Context<DashboardEditor>,
    ) -> Self {
        let unnamed = View {
            name: String::new(),
            ..first.clone()
        };
        Self {
            name: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("dashboard name")
                    .default_value(draft.name.clone())
            }),
            view_name: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(model::view_name(&unnamed))
                    .default_value(first.name.clone())
            }),
            filter: cx.new(|cx| {
                TextareaState::new(window, cx)
                    .placeholder(inspector::filter_placeholder(first.display))
                    .default_value(first.filter.clone())
            }),
            custom_var: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("host.vars.site")
                    .default_value(first.groups.custom_var.clone())
            }),
            icon_search: cx.new(|cx| InputState::new(window, cx).placeholder("find an icon")),
            copy_search: cx
                .new(|cx| InputState::new(window, cx).placeholder("find a dashboard or view")),
        }
    }
}

/// Where the check of the draft stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Check {
    /// The last evaluation is of the draft as it is.
    Done,
    /// A preview was asked for and hasn't answered yet (the one shown may
    /// be of an older draft).
    Pending,
    /// Pending, and a save waits for its verdict on the filters.
    PendingSave,
}

/// The dashboard editor view.
pub(crate) struct DashboardEditor {
    state: Entity<AppState>,
    target: EditorTarget,
    /// The dashboard as it was when editing began (the draft's changes
    /// are against it).
    saved: DashboardDraft,
    draft: DashboardDraft,
    /// The view selected in the views list (by id): the settings under
    /// the list are its, and the preview marks it.
    selected: String,
    /// The fields of the selected view show another view's values until
    /// the next frame fills them in (it has the window they need).
    refill: bool,
    /// The placeholders the view name and filter fields show (their
    /// display's).
    placeholders: (SharedString, &'static str),
    name: Entity<InputState>,
    view_name: Entity<InputState>,
    filter: Entity<TextareaState>,
    custom_var: Entity<InputState>,
    /// The icon picker's search (the sidebar mark).
    icon_search: Entity<InputState>,
    /// *copy filter from…*'s search.
    copy_search: Entity<InputState>,
    /// The icon picker's keyboard cursor: an index into the icons found.
    icon_cursor: Option<usize>,
    /// The icon picker's grid, scrolled to keep the cursor in sight.
    icon_scroll: ScrollHandle,
    /// *copy filter from…*'s cursor (the row under the pointer or moved
    /// to with the arrows): an index into the filters found.
    copy_cursor: Option<usize>,
    /// The preview card's count for the row under the cursor: the filter
    /// and how many objects it matches on this environment's snapshot.
    copy_preview: Option<(String, usize)>,
    /// *copy filter from…*'s list, scrolled to keep the cursor in sight.
    copy_scroll: ScrollHandle,
    /// The preview of a dashboard whose only view is handling or
    /// downtimes: that view's own page, as the dashboard shows it (with
    /// the subscription to its changes of the view).
    threads_preview: Option<(Entity<RecordList>, Subscription)>,
    /// The latest evaluation of the draft, with the views it evaluated.
    evaluated: Option<(Vec<View>, DashboardResult)>,
    /// No engine runs (no connection yet): nothing can be checked.
    unavailable: bool,
    /// The preview: the dashboard page, showing the draft.
    preview: Entity<DashboardView>,
    preview_task: Option<Task<()>>,
    menus: OpenMenu<EditorMenu>,
    focus_handle: FocusHandle,
    drag: WindowDrag,
    sidebar_open: bool,
    save_error: Option<String>,
    /// A filter that doesn't parse, found by a save: the view (by id) and
    /// the error, until the view changes.
    checked_error: Option<(String, String)>,
    /// Whether the last evaluation is of the draft as it is, and whether a
    /// save waits for the next one.
    check: Check,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<EditorEvent> for DashboardEditor {}

impl Focusable for DashboardEditor {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl DashboardEditor {
    /// An editor for `target`, whose dashboard is `saved`
    /// ([`model::initial_draft`]), starting from `draft` (`saved`, or the
    /// changes kept from an editor closed earlier).
    pub(crate) fn new(
        state: Entity<AppState>,
        target: EditorTarget,
        saved: DashboardDraft,
        draft: DashboardDraft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let selected = draft
            .views
            .first()
            .map(|view| view.id.clone())
            .unwrap_or_default();
        let first = draft.views.first().cloned().unwrap_or_default();
        let inputs = Inputs::new(&draft, &first, window, cx);
        let preview = cx.new(|cx| {
            DashboardView::preview(
                state.clone(),
                PreviewPage {
                    picked: Some(selected.clone()),
                    reserved: px(INSPECTOR_WIDTH) + Metrics::RULE,
                    ..PreviewPage::default()
                },
                cx,
            )
        });
        let subscriptions = Self::subscribe(&inputs, &preview, &state, window, cx);
        let Inputs {
            name,
            view_name,
            filter,
            custom_var,
            icon_search,
            copy_search,
        } = inputs;
        // A new dashboard starts with its name selected for typing.
        if target == EditorTarget::New {
            name.update(cx, |input, cx| {
                input.focus(window, cx);
                input.select_all(window, cx);
            });
        }
        let mut editor = Self {
            state,
            target,
            saved,
            draft,
            placeholders: (
                model::view_name(&View {
                    name: String::new(),
                    ..first.clone()
                })
                .into(),
                inspector::filter_placeholder(first.display),
            ),
            selected,
            refill: false,
            name,
            view_name,
            filter,
            custom_var,
            icon_search,
            copy_search,
            icon_cursor: None,
            icon_scroll: ScrollHandle::new(),
            copy_cursor: None,
            copy_preview: None,
            copy_scroll: ScrollHandle::new(),
            threads_preview: None,
            evaluated: None,
            unavailable: false,
            preview,
            preview_task: None,
            menus: OpenMenu::default(),
            focus_handle: cx.focus_handle(),
            drag: WindowDrag::default(),
            sidebar_open: true,
            save_error: None,
            checked_error: None,
            check: Check::Done,
            _subscriptions: subscriptions,
        };
        editor.request_preview(Duration::ZERO, cx);
        editor
    }

    /// What the editor follows: its fields, the preview's picks and
    /// changes, and the state.
    fn subscribe(
        inputs: &Inputs,
        preview: &Entity<DashboardView>,
        state: &Entity<AppState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<Subscription> {
        vec![
            cx.subscribe_in(
                &inputs.name,
                window,
                |this: &mut Self, input, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.draft.name = input.read(cx).value().to_string();
                        this.save_error = None;
                        cx.notify();
                    }
                },
            ),
            cx.subscribe_in(
                &inputs.view_name,
                window,
                |this: &mut Self, input, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        let name = input.read(cx).value().to_string();
                        this.change_selected(PREVIEW_DEBOUNCE, cx, |view| view.name = name);
                    }
                },
            ),
            cx.subscribe_in(
                &inputs.filter,
                window,
                |this: &mut Self, input, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        let filter = input.read(cx).value().to_string();
                        this.change_selected(PREVIEW_DEBOUNCE, cx, |view| view.filter = filter);
                    }
                },
            ),
            cx.subscribe_in(
                &inputs.custom_var,
                window,
                |this: &mut Self, input, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        let var = input.read(cx).value().to_string();
                        this.change_selected(PREVIEW_DEBOUNCE, cx, |view| {
                            view.groups.custom_var = var;
                        });
                    }
                },
            ),
            cx.subscribe_in(
                preview,
                window,
                |this: &mut Self, _, event: &DashboardEvent, window, cx| match event {
                    DashboardEvent::Pick(id) => {
                        if this.selected != *id {
                            this.select_and(id.clone(), false, cx);
                        }
                        window.focus(&this.focus_handle, cx);
                    }
                    DashboardEvent::ChangeView(id, change) => {
                        let change = change.clone();
                        if let Some(index) = this.index_of(id) {
                            this.change_view_at(index, Duration::ZERO, cx, move |view| {
                                change.apply(view);
                            });
                        }
                    }
                    DashboardEvent::Edit(_) | DashboardEvent::EditView(..) => {}
                },
            ),
            // Enter and the arrows reach the pickers first (their
            // popovers' keys): a search only narrows what they show.
            cx.subscribe_in(
                &inputs.icon_search,
                window,
                |this: &mut Self, _, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        // The cursor starts on the first icon found.
                        this.icon_cursor = Some(0);
                        this.icon_scroll.scroll_to_item(0);
                        cx.notify();
                    }
                },
            ),
            cx.subscribe_in(
                &inputs.copy_search,
                window,
                |this: &mut Self, _, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.set_copy_cursor(None, cx);
                        cx.notify();
                    }
                },
            ),
            // Its menus and controls show in the editor's header.
            cx.observe(preview, |_, _, cx| cx.notify()),
            cx.observe(state, |_, _, cx| cx.notify()),
        ]
    }

    /// Where the keyboard goes when the editor opens: the name of a new
    /// dashboard, else the editor itself (its keys work, nothing is typed
    /// into a field by accident).
    pub(crate) fn default_focus(&self, cx: &App) -> FocusHandle {
        match self.target {
            EditorTarget::New => self.name.focus_handle(cx),
            EditorTarget::Existing(_) => self.focus_handle.clone(),
        }
    }

    /// What is edited.
    pub(crate) fn target(&self) -> &EditorTarget {
        &self.target
    }

    /// The draft, when it differs from the saved dashboard.
    pub(crate) fn changes(&self) -> Option<&DashboardDraft> {
        (self.draft != self.saved).then_some(&self.draft)
    }

    /// The edited dashboard's name, for questions about it.
    pub(crate) fn title(&self) -> String {
        let name = self.draft.name.trim();
        if name.is_empty() {
            "untitled".to_owned()
        } else {
            name.to_owned()
        }
    }

    /// Tells the editor whether the sidebar is shown (the header then
    /// needs no window controls).
    pub(crate) fn set_sidebar_open(&mut self, open: bool, cx: &mut Context<Self>) {
        self.sidebar_open = open;
        self.preview
            .update(cx, |preview, cx| preview.set_sidebar_open(open, cx));
        cx.notify();
    }

    // --- The views -------------------------------------------------------

    /// The index of view `id` in the draft.
    fn index_of(&self, id: &str) -> Option<usize> {
        self.draft.views.iter().position(|view| view.id == id)
    }

    /// The index of the selected view (the first when it is gone).
    fn selected_index(&self) -> usize {
        self.index_of(&self.selected).unwrap_or(0)
    }

    /// The selected view.
    fn selected_view(&self) -> &View {
        static NONE: std::sync::LazyLock<View> = std::sync::LazyLock::new(View::default);
        self.draft.views.get(self.selected_index()).unwrap_or(&NONE)
    }

    /// Selects view `id`: its settings show under the list, the preview
    /// marks it on its header and scrolls its header into view.
    fn select(&mut self, id: String, cx: &mut Context<Self>) {
        self.select_and(id, true, cx);
    }

    /// Selects the draft's view `id` (a view header's *edit view*); an
    /// unknown one leaves the selection.
    pub(crate) fn select_view(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.index_of(id).is_some() {
            self.select(id.to_owned(), cx);
        }
    }

    /// [`Self::select`]; `reveal`: scroll the preview to the view (not
    /// when it was clicked there).
    fn select_and(&mut self, id: String, reveal: bool, cx: &mut Context<Self>) {
        self.menus.close();
        self.selected.clone_from(&id);
        self.refill = true;
        self.preview
            .update(cx, |preview, cx| preview.pick(Some(id), reveal, cx));
        cx.notify();
    }

    /// Selects the view `delta` places down (negative: up) the list.
    fn select_by(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.draft.views.len();
        let Some(index) = self.selected_index().checked_add_signed(delta) else {
            return;
        };
        if index < count {
            let id = self.draft.views[index].id.clone();
            self.select(id, cx);
        }
    }

    /// Fills the selected view's fields with its values after the
    /// selection changed, and gives them the placeholders of its display
    /// (the window is needed for both).
    fn fill_fields(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let view = self.selected_view().clone();
        let refill = std::mem::take(&mut self.refill);
        let name_placeholder: SharedString = model::view_name(&View {
            name: String::new(),
            ..view.clone()
        })
        .into();
        let filter_placeholder = inspector::filter_placeholder(view.display);
        let placeholders = (name_placeholder, filter_placeholder);
        let new_placeholders = self.placeholders != placeholders;
        self.view_name.update(cx, |input, cx| {
            if refill && input.value() != view.name.as_str() {
                input.set_value(view.name.clone(), window, cx);
            }
            if new_placeholders {
                input.set_placeholder(placeholders.0.clone(), window, cx);
            }
        });
        self.filter.update(cx, |input, cx| {
            if refill && input.value() != view.filter.as_str() {
                input.set_value(view.filter.clone(), window, cx);
            }
            if new_placeholders {
                input.set_placeholder(filter_placeholder, window, cx);
            }
        });
        self.placeholders = placeholders;
        if refill {
            self.custom_var.update(cx, |input, cx| {
                if input.value() != view.groups.custom_var.as_str() {
                    input.set_value(view.groups.custom_var.clone(), window, cx);
                }
            });
        }
    }

    /// Changes the selected view; asks for a new preview after `delay`.
    fn change_selected(
        &mut self,
        delay: Duration,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut View),
    ) {
        let index = self.selected_index();
        self.change_view_at(index, delay, cx, change);
    }

    /// Changes the view at `index`; asks for a new preview after `delay`
    /// when it changed.
    fn change_view_at(
        &mut self,
        index: usize,
        delay: Duration,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut View),
    ) {
        let Some(view) = self.draft.views.get_mut(index) else {
            return;
        };
        let before = view.clone();
        change(view);
        if *view == before {
            return;
        }
        let collapsed = view.collapsed != before.collapsed;
        if self
            .checked_error
            .as_ref()
            .is_some_and(|(id, _)| *id == before.id)
        {
            self.checked_error = None;
        }
        self.save_error = None;
        if collapsed {
            self.preview.update(cx, DashboardView::reset_folds);
        }
        self.sync_threads_preview(cx);
        self.request_preview(delay, cx);
        cx.notify();
    }

    /// Changes the views list (add, duplicate, move, remove): `change`
    /// returns the index of the view to select afterwards, or `None` when
    /// nothing changed.
    fn change_views(
        &mut self,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut Vec<View>) -> Option<usize>,
    ) {
        self.menus.close();
        let Some(index) = change(&mut self.draft.views) else {
            cx.notify();
            return;
        };
        self.save_error = None;
        let id = self.draft.views[index].id.clone();
        self.select(id, cx);
        self.sync_threads_preview(cx);
        self.request_preview(Duration::ZERO, cx);
    }

    /// *add view*: a view of `display` under the selected one, starting
    /// from its filter.
    fn add_view(&mut self, display: ViewDisplay, cx: &mut Context<Self>) {
        let after = self.selected_index();
        let filter = self.selected_view().filter.clone();
        self.change_views(cx, |views| {
            model::insert_view(views, Some(after), model::new_view(display, &filter))
        });
    }

    fn duplicate_view(&mut self, index: usize, cx: &mut Context<Self>) {
        self.change_views(cx, |views| model::duplicate_view(views, index));
    }

    fn move_view(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        self.change_views(cx, |views| model::move_view(views, from, to));
    }

    fn remove_view(&mut self, index: usize, cx: &mut Context<Self>) {
        if self
            .checked_error
            .as_ref()
            .is_some_and(|(id, _)| self.draft.views.get(index).is_some_and(|v| v.id == *id))
        {
            self.checked_error = None;
        }
        self.change_views(cx, |views| model::remove_view(views, index));
    }

    /// Whether the views list's keys apply: the editor itself has the
    /// keyboard (not one of its fields).
    fn list_keys(&self, window: &Window) -> bool {
        self.focus_handle.is_focused(window)
    }

    fn on_move_up(&mut self, _: &MoveViewUp, window: &mut Window, cx: &mut Context<Self>) {
        if !self.list_keys(window) {
            cx.propagate();
            return;
        }
        let index = self.selected_index();
        if let Some(to) = index.checked_sub(1) {
            self.move_view(index, to, cx);
        }
    }

    fn on_move_down(&mut self, _: &MoveViewDown, window: &mut Window, cx: &mut Context<Self>) {
        if !self.list_keys(window) {
            cx.propagate();
            return;
        }
        let index = self.selected_index();
        self.move_view(index, index + 1, cx);
    }

    fn on_remove(&mut self, _: &RemoveView, window: &mut Window, cx: &mut Context<Self>) {
        if !self.list_keys(window) {
            cx.propagate();
            return;
        }
        let index = self.selected_index();
        self.remove_view(index, cx);
    }

    fn on_next(&mut self, _: &SelectNextView, window: &mut Window, cx: &mut Context<Self>) {
        if !self.list_keys(window) {
            cx.propagate();
            return;
        }
        self.select_by(1, cx);
    }

    fn on_previous(&mut self, _: &SelectPreviousView, window: &mut Window, cx: &mut Context<Self>) {
        if !self.list_keys(window) {
            cx.propagate();
            return;
        }
        self.select_by(-1, cx);
    }

    // --- Preview and save ------------------------------------------------

    /// Asks the core to evaluate the draft's views after `delay` (a newer
    /// request replaces a waiting one; the core drops superseded ones
    /// itself). The preview keeps showing the last evaluation meanwhile.
    fn request_preview(&mut self, delay: Duration, cx: &mut Context<Self>) {
        self.ask_for_preview(delay, true, cx);
    }

    /// [`Self::request_preview`]; `at_once`: an answer that is there at
    /// once applies before this returns (a save waiting for the check
    /// goes on when the answer arrives instead).
    fn ask_for_preview(&mut self, delay: Duration, at_once: bool, cx: &mut Context<Self>) {
        if self.check == Check::Done {
            self.check = Check::Pending;
        }
        let views = self.draft.views.clone();
        if delay.is_zero() {
            // An answer that is there at once (the views list changed, and
            // an evaluator without a core) shows without a frame between.
            match self.state.read(cx).preview(views.clone()) {
                None => {
                    self.preview_task = None;
                    self.apply_preview(views, None, cx);
                    return;
                }
                Some(mut receiver) => {
                    if at_once && let Ok(Some(result)) = receiver.try_recv() {
                        self.preview_task = None;
                        self.apply_preview(views, Some(result), cx);
                        return;
                    }
                    self.preview_task = Some(cx.spawn(async move |this, cx| {
                        let Ok(result) = receiver.await else {
                            // Replaced by a newer request in the core.
                            return;
                        };
                        let _ = this.update(cx, |this, cx| {
                            this.apply_preview(views, Some(result), cx);
                        });
                    }));
                    return;
                }
            }
        }
        self.preview_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let Ok(receiver) =
                this.update(cx, |this, cx| this.state.read(cx).preview(views.clone()))
            else {
                return;
            };
            let result = match receiver {
                Some(receiver) => match receiver.await {
                    Ok(result) => Some(result),
                    // Replaced by a newer request in the core.
                    Err(_) => return,
                },
                None => None,
            };
            let _ = this.update(cx, |this, cx| this.apply_preview(views, result, cx));
        }));
    }

    /// The core evaluated `views` (`None`: no engine runs): the preview
    /// shows them, and a save waiting for the verdict goes on.
    fn apply_preview(
        &mut self,
        views: Vec<View>,
        result: Option<DashboardResult>,
        cx: &mut Context<Self>,
    ) {
        let save = self.check == Check::PendingSave;
        self.check = Check::Done;
        match result {
            Some(result) => {
                self.unavailable = false;
                let page = PreviewPage {
                    views: views.clone(),
                    result: result.clone(),
                    picked: Some(self.selected.clone()),
                    reserved: px(INSPECTOR_WIDTH) + Metrics::RULE,
                };
                self.evaluated = Some((views, result));
                self.preview
                    .update(cx, |preview, cx| preview.set_preview(page, cx));
            }
            None => self.unavailable = true,
        }
        self.sync_threads_preview(cx);
        if save {
            self.save(cx);
        }
        cx.notify();
    }

    /// A draft whose only view is handling or downtimes previews as that
    /// view's own page ([`RecordList`] showing the draft's view and its
    /// latest evaluation); any other draft drops it.
    fn sync_threads_preview(&mut self, cx: &mut Context<Self>) {
        let single = match self.draft.views.as_slice() {
            [view] => ListKind::of_display(view.display).map(|kind| (kind, view.clone())),
            _ => None,
        };
        let Some((kind, view)) = single else {
            self.threads_preview = None;
            return;
        };
        let members = self
            .evaluated
            .as_ref()
            .and_then(|(_, result)| result.view(&view.id))
            .and_then(|result| result.members().cloned());
        let current = self
            .threads_preview
            .as_ref()
            .filter(|(list, _)| list.read(cx).kind() == kind)
            .map(|(list, _)| list.clone());
        let list = if let Some(list) = current {
            list
        } else {
            let state = self.state.clone();
            let list = cx.new(|cx| RecordList::new(state, kind, ListSource::Preview, cx));
            let events = cx.subscribe(&list, |this, _, event: &RecordListEvent, cx| {
                if let RecordListEvent::ChangeView(change) = event {
                    let change = change.clone();
                    this.change_view_at(0, Duration::ZERO, cx, move |view| {
                        change.apply(view);
                    });
                }
            });
            self.threads_preview = Some((list.clone(), events));
            list
        };
        list.update(cx, |list, cx| list.set_preview(view, members, cx));
    }

    /// While a new dashboard is made, its provisional sidebar row: the
    /// group it goes into, its name and its mark (14-r5-a); `None` while an
    /// existing dashboard is edited.
    pub(crate) fn provisional(&self) -> Option<crate::sidebar::model::Provisional> {
        if self.target != EditorTarget::New {
            return None;
        }
        let name = self.draft.name.trim();
        let name = if name.is_empty() {
            crate::app_state::editing::NEW_DASHBOARD_NAME
        } else {
            name
        };
        Some(crate::sidebar::model::Provisional {
            group_id: self.draft.group_id.clone(),
            name: name.to_owned(),
            mark: self.sidebar_mark(),
        })
    }

    /// The sidebar mark's icon picker chose `icon`.
    fn pick_icon(&mut self, icon: ic_ui_kit::IconName, cx: &mut Context<Self>) {
        self.menus.close();
        self.draft.mark = ic_config::SidebarMark::Icon(icon.lucide_name().to_owned());
        self.save_error = None;
        cx.notify();
    }

    /// What *copy filter from…* offers for `query`.
    fn copy_sources(&self, query: &str, cx: &App) -> Vec<model::CopySource> {
        let editing = match &self.target {
            EditorTarget::Existing(reference) => Some(reference),
            EditorTarget::New => None,
        };
        model::copy_sources(self.state.read(cx).groups(), editing, query)
    }

    /// *copy filter from…* chose `filter`: it fills the selected view's
    /// filter field as an edit, so the field's undo (`ctrl-z`) brings the
    /// old one back.
    fn copy_filter(&mut self, filter: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.menus.close();
        let filter = filter.to_owned();
        self.filter.update(cx, |input, cx| {
            input.replace_all(filter.clone(), window, cx);
            input.focus(window, cx);
        });
        self.change_selected(PREVIEW_DEBOUNCE, cx, move |view| view.filter = filter);
        cx.notify();
    }

    /// The latest evaluation of view `id`, when it is of the view as it
    /// is now (not of an older draft).
    fn evaluated_view(&self, id: &str) -> Option<&ic_core::snapshot::ViewResult> {
        let (views, result) = self.evaluated.as_ref()?;
        let index = self.index_of(id)?;
        let evaluated = views.iter().find(|view| view.id == id)?;
        (*evaluated == self.draft.views[index]).then(|| result.view(id))?
    }

    /// Why the selected view's filter doesn't work, if it doesn't: a save
    /// found it doesn't parse, or the core's preview says so.
    fn filter_error(&self) -> Option<String> {
        if let Some((id, error)) = &self.checked_error
            && *id == self.selected
        {
            return Some(error.clone());
        }
        let (_, result) = self.evaluated.as_ref()?;
        model::view_error(result, &self.selected).map(ToOwned::to_owned)
    }

    /// Saves the draft: a new dashboard, or the edited one. Refused while
    /// a view's settings don't work: a filter that doesn't parse at once,
    /// one the core can't evaluate once its preview says so (a save waits
    /// for a pending preview); the view in question is selected.
    fn save(&mut self, cx: &mut Context<Self>) {
        let several = self.draft.views.len() > 1;
        if let Err((index, problem)) = model::check_views(&self.draft.views) {
            let view = &self.draft.views[index];
            let id = view.id.clone();
            let name = model::view_name(view);
            if let model::Problem::Filter(error) = &problem {
                self.checked_error = Some((id.clone(), error.clone()));
            }
            self.save_error = Some(problem.message(several.then_some(name.as_str())));
            self.stop_waiting_to_save();
            if self.selected != id {
                self.select(id, cx);
            }
            cx.notify();
            return;
        }
        if self.check != Check::Done {
            // The last change hasn't been checked yet: check it now and
            // save when the answer comes.
            self.check = Check::PendingSave;
            self.ask_for_preview(Duration::ZERO, false, cx);
            return;
        }
        if let Some((_, result)) = &self.evaluated {
            let failed = self.draft.views.iter().find_map(|view| {
                model::view_error(result, &view.id).map(|error| (view.clone(), error.to_owned()))
            });
            if let Some((view, error)) = failed {
                let name = model::view_name(&view);
                self.save_error =
                    Some(model::Problem::Filter(error).message(several.then_some(name.as_str())));
                if self.selected != view.id {
                    self.select(view.id, cx);
                }
                cx.notify();
                return;
            }
        }
        let draft = self.draft.clone();
        // Saving selects the dashboard, and the workspace closes an editor
        // whose dashboard isn't shown any more, keeping its changes: once
        // saved, there are none to keep.
        let previous = std::mem::replace(&mut self.saved, draft.clone());
        let saved = self.state.update(cx, |state, cx| {
            let saved = match &self.target {
                EditorTarget::New => state.add_dashboard(draft),
                EditorTarget::Existing(reference) => state.update_dashboard(reference, draft),
            };
            cx.notify();
            saved
        });
        if saved.is_some() {
            cx.emit(EditorEvent::Closed);
        } else {
            self.saved = previous;
            self.save_error = Some("The dashboard or its group no longer exists.".to_owned());
            cx.notify();
        }
    }

    /// A save that waited for the check doesn't any more.
    fn stop_waiting_to_save(&mut self) {
        if self.check == Check::PendingSave {
            self.check = Check::Pending;
        }
    }

    fn on_save(&mut self, _: &SaveDashboard, _: &mut Window, cx: &mut Context<Self>) {
        self.save(cx);
    }

    fn on_escape(&mut self, _: &Escape, _: &mut Window, cx: &mut Context<Self>) {
        self.discard(cx);
    }

    fn on_discard(&mut self, _: &DiscardDashboard, _: &mut Window, cx: &mut Context<Self>) {
        self.discard(cx);
    }

    /// Escape: closes an open dropdown, else leaves without saving (asking
    /// first when the draft has changes).
    fn discard(&mut self, cx: &mut Context<Self>) {
        if self.menus.close() {
            cx.notify();
            return;
        }
        self.stop_waiting_to_save();
        if self.changes().is_some() {
            cx.emit(EditorEvent::DiscardChanges);
        } else {
            cx.emit(EditorEvent::Closed);
        }
    }

    // --- Rendering -------------------------------------------------------

    fn render_header(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        // A one-view draft: the view's controls, as the dashboard's header
        // will show them (README, *View controls*).
        let width = self.preview_width(window, cx);
        let controls: Vec<AnyElement> = match &self.threads_preview {
            Some((list, _)) => list.update(cx, |list, cx| list.header_controls(width, cx)),
            None if self.draft.views.len() == 1 && self.evaluated.is_some() => self
                .preview
                .update(cx, |preview, cx| preview.preview_controls(cx)),
            None => Vec::new(),
        };
        let controls_row = (!controls.is_empty()).then(|| {
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(8.))
                .children(controls)
        });
        let theme = cx.theme();
        let colors = theme.colors;
        let controls = Controls::of(window, cx);
        let title = self.title();
        // With a one-view draft's controls in the header there is no room
        // for the group (14-r5-a, b): the inspector names it.
        let subtitle = if controls_row.is_some() {
            "editing".to_owned()
        } else {
            let group = self
                .state
                .read(cx)
                .groups()
                .iter()
                .find(|group| group.id == self.draft.group_id)
                .map(|group| group.name.clone())
                .unwrap_or_default();
            format!("editing · {group}")
        };
        let header = div()
            .id("editor-header")
            .flex()
            .flex_none()
            .items_center()
            .gap(px(12.))
            .h(Metrics::with_rule(theme.metrics.header_height))
            .px(theme.metrics.list_padding)
            .border_b_1()
            .border_color(colors.border_header)
            .when(!self.sidebar_open, |header| {
                header.child(sidebar_reopen(controls, theme))
            })
            // The name keeps its room (up to a point); `editing · group`
            // gives way first.
            .child(
                div()
                    .flex_shrink_0()
                    .max_w(px(360.))
                    .truncate()
                    .text_size(theme.text.heading)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(colors.text_strong)
                    .child(title),
            )
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(theme.text.small)
                    .text_color(colors.accent_text)
                    .child(subtitle),
            )
            .child(div().flex_1())
            .children(controls_row)
            .child(
                Button::new("editor-discard", "discard")
                    .key_hint("esc")
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.discard(cx))),
            )
            .child(
                Button::new("editor-save", "save dashboard")
                    .primary()
                    .key_hint(save_key())
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.save(cx))),
            );
        self.drag.attach(header, controls).into_any_element()
    }

    /// The preview's width: the main area but the inspector.
    fn preview_width(&self, window: &Window, cx: &App) -> gpui::Pixels {
        let main = crate::dashboard::SplitLayout::main_width(
            window,
            self.sidebar_open,
            &cx.theme().metrics,
        );
        (main - px(INSPECTOR_WIDTH) - Metrics::RULE).max(px(0.))
    }

    /// The preview: the dashboard page showing the draft, or what stands
    /// in for it before the first evaluation.
    fn render_preview(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        if let Some((list, _)) = &self.threads_preview {
            return list.clone().into_any_element();
        }
        if self.evaluated.is_some() {
            return self.preview.clone().into_any_element();
        }
        if self.unavailable {
            return note(
                "The preview shows once the environment's engine runs.",
                theme,
            );
        }
        note("Evaluating…", theme)
    }
}

impl Render for DashboardEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.fill_fields(window, cx);
        let header = self.render_header(window, cx);
        let preview = self.render_preview(cx);
        let inspector = self.render_inspector(window, cx);
        div()
            .id("dashboard-editor")
            .key_context(EDITOR_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::on_save))
            .on_action(cx.listener(Self::on_escape))
            .on_action(cx.listener(Self::on_discard))
            .on_action(cx.listener(Self::on_move_up))
            .on_action(cx.listener(Self::on_move_down))
            .on_action(cx.listener(Self::on_remove))
            .on_action(cx.listener(Self::on_next))
            .on_action(cx.listener(Self::on_previous))
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
                    .child(header)
                    .child(div().flex().flex_col().flex_1().min_h_0().child(preview)),
            )
            .child(inspector)
    }
}

/// How the filter field's undo is reached, as *copy filter from…* says it.
fn undo_hint() -> &'static str {
    if cfg!(target_os = "macos") {
        "⌘Z undoes it"
    } else {
        "ctrl-z undoes it"
    }
}

/// The save shortcut, as the button shows it.
fn save_key() -> &'static str {
    if cfg!(target_os = "macos") {
        "⌘S"
    } else {
        "ctrl-s"
    }
}

/// A muted line in the middle of the preview.
fn note(text: impl Into<SharedString>, theme: &Theme) -> AnyElement {
    div()
        .flex()
        .flex_1()
        .items_center()
        .justify_center()
        .px(px(24.))
        .text_size(theme.text.body)
        .text_color(theme.colors.text_muted)
        .child(text.into())
        .into_any_element()
}

/// Accessors for the UI tests (`ui_tests`, Linux only).
#[cfg(all(test, target_os = "linux"))]
impl DashboardEditor {
    /// The draft as edited so far.
    pub(crate) fn draft(&self) -> &DashboardDraft {
        &self.draft
    }

    /// The selected view's id.
    pub(crate) fn selected(&self) -> &str {
        &self.selected
    }

    /// The filter field's state.
    pub(crate) fn filter_input(&self) -> &Entity<TextareaState> {
        &self.filter
    }

    /// The name field's state.
    pub(crate) fn name_input(&self) -> &Entity<InputState> {
        &self.name
    }

    /// The selected view's name field.
    pub(crate) fn view_name_input(&self) -> &Entity<InputState> {
        &self.view_name
    }

    /// The selected view's custom variable field.
    pub(crate) fn custom_var_input(&self) -> &Entity<InputState> {
        &self.custom_var
    }

    /// The preview's page.
    pub(crate) fn preview_view(&self) -> &Entity<DashboardView> {
        &self.preview
    }

    /// The latest preview of the selected view: its evaluation, or why its
    /// filter doesn't work.
    pub(crate) fn preview_result(&self) -> Option<Result<DashboardResult, String>> {
        let (_, result) = self.evaluated.as_ref()?;
        Some(match model::view_error(result, &self.selected) {
            Some(error) => Err(error.to_owned()),
            None => Ok(result.clone()),
        })
    }

    /// Why the last save was refused.
    pub(crate) fn save_error(&self) -> Option<&str> {
        self.save_error.as_deref()
    }

    /// Changes the selected view as its settings' controls do.
    pub(crate) fn change_selected_for_test(
        &mut self,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut View),
    ) {
        self.change_selected(Duration::ZERO, cx, change);
    }

    /// The open menu.
    pub(crate) fn open_menu(&self) -> Option<String> {
        self.menus.current().map(|menu| format!("{menu:?}"))
    }

    /// The preview of a draft whose only view is handling or downtimes.
    pub(crate) fn threads_preview(&self) -> Option<&Entity<RecordList>> {
        self.threads_preview.as_ref().map(|(list, _)| list)
    }

    /// The icon picker's choice.
    pub(crate) fn pick_icon_for_test(&mut self, icon: ic_ui_kit::IconName, cx: &mut Context<Self>) {
        self.pick_icon(icon, cx);
    }

    /// *copy filter from…*'s choice.
    pub(crate) fn copy_filter_for_test(
        &mut self,
        filter: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.copy_filter(filter, window, cx);
    }

    /// What *copy filter from…* offers for `query`.
    pub(crate) fn copy_sources_for_test(&self, query: &str, cx: &App) -> Vec<model::CopySource> {
        self.copy_sources(query, cx)
    }

    /// The icon picker's keyboard cursor.
    pub(crate) fn icon_cursor(&self) -> Option<usize> {
        self.icon_cursor
    }

    /// *copy filter from…*'s cursor and its preview's count.
    pub(crate) fn copy_cursor(&self) -> (Option<usize>, Option<&(String, usize)>) {
        (self.copy_cursor, self.copy_preview.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_save_key_is_the_platforms() {
        assert!(!save_key().is_empty());
    }
}
