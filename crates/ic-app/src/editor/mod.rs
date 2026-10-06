//! The dashboard editor (DASH-04), in the main area like turn 1's screen
//! 1c, in the v2 look: the header says what is edited, with *Discard* and
//! *Save dashboard*; the live preview of the edited view fills the left
//! (summary bar and rows, as the dashboard will show them), the inspector
//! the right: name, group, hosts or services, the filter in Icinga's
//! language, problems only, hide handled, sort, group by and the
//! notification setting.
//!
//! Every change asks the core for a preview (`Command::PreviewDashboard`)
//! once typing rests: the filter is validated there (a parse error names
//! its line and column, marked under the field), and the match count and
//! rows come back with it. A filter that doesn't work can't be saved: a
//! save parses it at once and waits for a pending preview's verdict.
//!
//! Keys: `secondary-s` saves, Escape discards (asking first when something
//! was changed). Showing another dashboard or tab keeps a changed draft
//! for the next time the same dashboard is edited.

pub(crate) mod model;

use std::ops::Range;
use std::time::Duration;

use gpui::{
    Action, AnyElement, App, AppContext as _, ClickEvent, Context, Div, Entity, EventEmitter,
    FocusHandle, Focusable, FontWeight, InteractiveElement as _, IntoElement, KeyBinding,
    MouseButton, ParentElement as _, Render, SharedString, Stateful,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, UniformListScrollHandle,
    Window, div, prelude::FluentBuilder as _, px, uniform_list,
};
use ic_config::{GroupBy, ObjectKind, Sort, SortKey, View};
use ic_core::snapshot::{DashboardResult, DashboardRow};
use ic_model::Timestamp;
use ic_rules::{DashboardRef, ScopeSetting};
use ic_ui_kit::input::{Escape, InputEvent, InputState, TextareaState};
use ic_ui_kit::{
    ActiveTheme as _, Button, Dismissal, EmptyState, Field, FieldTone, Icon, IconName, Link, Menu,
    MenuItem, Metrics, Popover, Scrollbar, Segmented, SummaryBar, SummaryItem, Switch, TextArea,
    TextField, Theme,
};

pub(crate) use self::model::EditorTarget;
use crate::app_state::AppState;
use crate::app_state::editing::DashboardDraft;
use crate::chrome::{Controls, WindowDrag};
use crate::dashboard::header::{natural_descending, summary_items, view_label};
use crate::dashboard::{group_header, object_row, row_id};
use crate::menu_state::{OpenMenu, down_position};
use crate::workspace::sidebar_reopen;

/// Key context of the dashboard editor.
pub(crate) const EDITOR_CONTEXT: &str = "DashboardEditor";

/// The preview is asked for once typing has rested this long.
const PREVIEW_DEBOUNCE: Duration = Duration::from_millis(250);

/// Width of the inspector column (the design's).
const INSPECTOR_WIDTH: f32 = 372.;

/// Saves the dashboard being edited.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct SaveDashboard;

/// Leaves the editor without saving.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct DiscardDashboard;

/// Registers the editor's key bindings. Escape in a field reaches the
/// editor as the field's own `Escape` once it has nothing to dismiss.
pub(crate) fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("secondary-s", SaveDashboard, Some(EDITOR_CONTEXT)),
        KeyBinding::new("escape", DiscardDashboard, Some(EDITOR_CONTEXT)),
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditorMenu {
    Group,
    Sort,
    GroupBy,
}

/// The preview of the edited view.
#[derive(Clone, Debug, PartialEq)]
enum Preview {
    /// Asked for; the answer is pending.
    Waiting,
    /// No engine runs (no connection yet): nothing can be checked.
    Unavailable,
    /// The core's answer.
    Ready(Result<DashboardResult, String>),
}

/// The dashboard editor view.
pub(crate) struct DashboardEditor {
    state: Entity<AppState>,
    target: EditorTarget,
    /// The dashboard as it was when editing began (the draft's changes
    /// are against it).
    saved: DashboardDraft,
    draft: DashboardDraft,
    name: Entity<InputState>,
    filter: Entity<TextareaState>,
    preview: Preview,
    preview_task: Option<Task<()>>,
    menus: OpenMenu<EditorMenu>,
    scroll: UniformListScrollHandle,
    focus_handle: FocusHandle,
    drag: WindowDrag,
    sidebar_open: bool,
    save_error: Option<String>,
    /// A preview was asked for and hasn't answered yet (the one shown may
    /// be of an older draft).
    checking: bool,
    /// A save waits for the pending preview's verdict on the filter.
    save_when_checked: bool,
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
        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("dashboard name")
                .default_value(draft.name.clone())
        });
        let filter = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("host.vars.env == \"prod\" && service.state != 0")
                .default_value(draft.view.filter.clone())
        });
        let subscriptions = vec![
            cx.subscribe_in(
                &name,
                window,
                |this: &mut Self, input, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.draft.name = input.read(cx).value().to_string();
                        cx.notify();
                    }
                },
            ),
            cx.subscribe_in(
                &filter,
                window,
                |this: &mut Self, input, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        let filter = input.read(cx).value().to_string();
                        this.change_view(cx, |view| view.filter = filter);
                    }
                },
            ),
            cx.observe(&state, |_, _, cx| cx.notify()),
        ];
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
            name,
            filter,
            preview: Preview::Waiting,
            preview_task: None,
            menus: OpenMenu::default(),
            scroll: UniformListScrollHandle::new(),
            focus_handle: cx.focus_handle(),
            drag: WindowDrag::default(),
            sidebar_open: true,
            save_error: None,
            checking: false,
            save_when_checked: false,
            _subscriptions: subscriptions,
        };
        editor.request_preview(Duration::ZERO, cx);
        editor
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

    /// The draft as edited so far.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn draft(&self) -> &DashboardDraft {
        &self.draft
    }

    /// The filter field's state.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn filter_input(&self) -> &Entity<TextareaState> {
        &self.filter
    }

    /// The name field's state.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn name_input(&self) -> &Entity<InputState> {
        &self.name
    }

    /// The latest preview: the result, or why there is none.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn preview_result(&self) -> Option<&Result<DashboardResult, String>> {
        match &self.preview {
            Preview::Ready(result) => Some(result),
            Preview::Waiting | Preview::Unavailable => None,
        }
    }

    /// Why the last save was refused.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn save_error(&self) -> Option<&str> {
        self.save_error.as_deref()
    }

    /// Tells the editor whether the sidebar is shown (the header then
    /// needs no window controls).
    pub(crate) fn set_sidebar_open(&mut self, open: bool, cx: &mut Context<Self>) {
        self.sidebar_open = open;
        cx.notify();
    }

    /// Changes the draft's view and asks for a new preview.
    fn change_view(&mut self, cx: &mut Context<Self>, change: impl FnOnce(&mut View)) {
        let before = self.draft.view.clone();
        change(&mut self.draft.view);
        if self.draft.view != before {
            self.save_error = None;
            self.request_preview(PREVIEW_DEBOUNCE, cx);
        }
        cx.notify();
    }

    /// Asks the core to evaluate the draft after `delay` (a newer request
    /// replaces a waiting one; the core drops superseded ones itself).
    fn request_preview(&mut self, delay: Duration, cx: &mut Context<Self>) {
        if !matches!(self.preview, Preview::Ready(Err(_))) {
            self.preview = Preview::Waiting;
        }
        self.checking = true;
        let view = self.draft.view.clone();
        self.preview_task = Some(cx.spawn(async move |this, cx| {
            if !delay.is_zero() {
                cx.background_executor().timer(delay).await;
            }
            let Ok(receiver) = this.update(cx, |this, cx| this.state.read(cx).preview(view)) else {
                return;
            };
            let preview = match receiver {
                Some(receiver) => match receiver.await {
                    Ok(result) => Preview::Ready(result),
                    // Replaced by a newer request in the core.
                    Err(_) => return,
                },
                None => Preview::Unavailable,
            };
            let _ = this.update(cx, |this, cx| {
                this.preview = preview;
                this.checking = false;
                if std::mem::take(&mut this.save_when_checked) {
                    this.save(cx);
                }
                cx.notify();
            });
        }));
    }

    /// Saves the draft: a new dashboard, or the edited one. Refused while
    /// the filter doesn't work: one that doesn't parse at once, one the
    /// core can't evaluate once its preview says so (a save waits for a
    /// pending preview).
    fn save(&mut self, cx: &mut Context<Self>) {
        if let Err(error) = model::check_filter(&self.draft.view.filter) {
            self.save_error = Some(format!("Fix the filter first: {error}"));
            self.preview = Preview::Ready(Err(error));
            cx.notify();
            return;
        }
        if self.checking {
            // The last change hasn't been checked yet: check it now and
            // save when the answer comes.
            self.save_when_checked = true;
            self.request_preview(Duration::ZERO, cx);
            return;
        }
        if let Preview::Ready(Err(error)) = &self.preview {
            self.save_error = Some(format!("Fix the filter first: {error}"));
            cx.notify();
            return;
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
        self.save_when_checked = false;
        if self.changes().is_some() {
            cx.emit(EditorEvent::DiscardChanges);
        } else {
            cx.emit(EditorEvent::Closed);
        }
    }

    fn dismiss_listener(
        cx: &Context<Self>,
    ) -> impl Fn(&Dismissal, &mut Window, &mut App) + 'static {
        cx.listener(|this, dismissal: &Dismissal, _, cx| {
            this.menus.dismissed(*dismissal);
            cx.notify();
        })
    }

    fn render_header(&self, window: &Window, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let controls = Controls::of(window, cx);
        let title = self.title();
        let group = self
            .state
            .read(cx)
            .groups()
            .iter()
            .find(|group| group.id == self.draft.group_id)
            .map(|group| group.name.clone())
            .unwrap_or_default();
        let subtitle = format!("editing · {group}");
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
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(theme.text.heading)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(colors.text_strong)
                    .child(title),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(theme.text.small)
                    .text_color(colors.accent)
                    .child(subtitle),
            )
            .child(div().flex_1())
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

    /// The preview's summary bar and rows, or what the preview says.
    fn render_preview(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let result = match &self.preview {
            Preview::Ready(Ok(result)) => result,
            Preview::Ready(Err(error)) => {
                return EmptyState::new("The filter doesn't work")
                    .leading(
                        Icon::new(IconName::TriangleAlert)
                            .size(px(20.))
                            .color(theme.states.critical),
                    )
                    .detail(error.clone())
                    .max_width(px(520.))
                    .into_any_element();
            }
            Preview::Waiting => return note("Evaluating…", theme),
            Preview::Unavailable => {
                return note(
                    "The preview shows once the environment's engine runs.",
                    theme,
                );
            }
        };
        let items = summary_items(&result.summary, self.draft.view.object_kind);
        let summary = (!items.is_empty()).then(|| {
            SummaryBar::new()
                .children(
                    items
                        .into_iter()
                        .map(|(state, count, label)| SummaryItem::new(state, count, label)),
                )
                .end(div().text_color(theme.colors.text_faint).child("preview"))
        });
        let body = if result.rows.is_empty() {
            let text = if model::matches(&result.summary) == 0 {
                "Nothing matches this filter."
            } else {
                "Everything this dashboard would show is OK or handled."
            };
            note(text, theme)
        } else {
            let rows = result.rows.clone();
            let view = self.draft.view.clone();
            let scroll = self.scroll.clone();
            div()
                .relative()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .child(
                    uniform_list(
                        "editor-preview-rows",
                        rows.len(),
                        cx.processor(move |this, range: Range<usize>, _window, cx| {
                            this.render_rows(&rows, &view, range, cx)
                        }),
                    )
                    .track_scroll(&scroll)
                    .size_full(),
                )
                .child(Scrollbar::vertical(&scroll))
                .into_any_element()
        };
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .children(summary)
            .child(body)
            .into_any_element()
    }

    /// The preview rows in `range` (the ones on screen).
    fn render_rows(
        &self,
        rows: &[DashboardRow],
        view: &View,
        range: Range<usize>,
        cx: &Context<Self>,
    ) -> Vec<AnyElement> {
        let state = self.state.read(cx);
        let snapshot = state.snapshot();
        let theme = cx.theme();
        let now = Timestamp::now();
        let show_host = view.group_by != GroupBy::Host;
        let grouped = view.group_by != GroupBy::None;
        let mut group: Option<String> = None;
        range
            .filter_map(|index| {
                let row = match rows.get(index)? {
                    DashboardRow::Object(key) => {
                        let id = row_id(group.as_deref(), key);
                        object_row(snapshot, id, key, show_host, now, theme).indent(if grouped {
                            theme.metrics.row_indent
                        } else {
                            px(0.)
                        })
                    }
                    DashboardRow::Group { label, count } => {
                        group = Some(label.clone());
                        group_header(snapshot, view, label, *count, now, theme)
                    }
                };
                Some(row.into_any_element())
            })
            .collect()
    }

    /// The inspector: the draft's fields.
    fn render_inspector(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let view = &self.draft.view;
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(INSPECTOR_WIDTH))
            .h_full()
            .border_l_1()
            .border_color(colors.border_split)
            .bg(colors.pane_background)
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .h(Metrics::with_rule(theme.metrics.summary_bar_height))
                    .px(px(18.))
                    .border_b_1()
                    .border_color(colors.border_header)
                    .text_size(theme.text.heading)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(colors.text_strong)
                    .child("dashboard")
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_size(theme.text.label)
                            .font_weight(FontWeight::NORMAL)
                            .text_color(colors.text_faint)
                            .child(sort_summary(view.sort, view.object_kind)),
                    ),
            )
            .child(self.render_inspector_body(cx))
            .child(self.render_inspector_footer(cx))
            .into_any_element()
    }

    /// The inspector's fields, scrolling.
    fn render_inspector_body(&self, cx: &Context<Self>) -> Stateful<Div> {
        let view = &self.draft.view;
        let group_name = self
            .state
            .read(cx)
            .groups()
            .iter()
            .find(|group| group.id == self.draft.group_id)
            .map_or_else(|| "—".to_owned(), |group| group.name.clone());
        div()
            .id("editor-inspector-body")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .gap(px(18.))
            .p(px(18.))
            .overflow_y_scroll()
            .child(Field::new("name").control(TextField::new(&self.name).bordered(true)))
            .child(Field::new("group").control(self.dropdown(
                "editor-group",
                EditorMenu::Group,
                group_name,
                || self.group_menu(cx),
                cx,
            )))
            .child(Self::kind_field(view, cx))
            .child(self.filter_field(cx))
            .child(Self::show_field(view, cx))
            .child(self.sort_fields(cx))
            .child(Field::new("group by").control(self.dropdown(
                "editor-group-by",
                EditorMenu::GroupBy,
                group_by_label(view.group_by).to_owned(),
                || self.group_by_menu(cx),
                cx,
            )))
            .child(self.notifications_field(cx))
    }

    /// Services or hosts.
    fn kind_field(view: &View, cx: &Context<Self>) -> Field {
        Field::new("lists").control(
            Segmented::new("editor-kind")
                .option("services")
                .option("hosts")
                .selected(usize::from(view.object_kind == ObjectKind::Hosts))
                .on_select(cx.listener(|this, index: &usize, _, cx| {
                    let kind = if *index == 1 {
                        ObjectKind::Hosts
                    } else {
                        ObjectKind::Services
                    };
                    this.change_view(cx, |view| {
                        *view = model::with_kind(view.clone(), kind);
                    });
                })),
        )
    }

    /// The filter, with the preview's verdict and where an error points.
    fn filter_field(&self, cx: &Context<Self>) -> Field {
        let theme = cx.theme();
        let (filter_status, filter_tone, filter_error) = match &self.preview {
            Preview::Ready(Ok(result)) => (model::status_text(result), FieldTone::Good, None),
            Preview::Ready(Err(error)) => {
                ("invalid".to_owned(), FieldTone::Bad, Some(error.clone()))
            }
            Preview::Waiting => ("checking…".to_owned(), FieldTone::Neutral, None),
            Preview::Unavailable => ("not checked".to_owned(), FieldTone::Neutral, None),
        };
        let marker = filter_error.as_deref().and_then(|error| {
            let (line, column) = model::error_position(error)?;
            model::error_marker(&self.draft.view.filter, line, column, model::MARKER_CHARS)
        });
        Field::new("filter")
            .status(filter_status, filter_tone)
            .control(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(
                        TextArea::new(&self.filter)
                            .height(px(92.))
                            .invalid(filter_error.is_some()),
                    )
                    .when_some(marker, |field, (line, caret)| {
                        field.child(
                            div()
                                .px(px(13.))
                                .text_size(theme.text.small)
                                .text_color(theme.states.critical)
                                .child(div().whitespace_nowrap().child(line))
                                .child(div().whitespace_nowrap().child(caret)),
                        )
                    }),
            )
            .error(filter_error)
            .hint(
                "Icinga's filter language: host.vars.role == \"db\", match(\"web-*\", host.name) …",
            )
    }

    /// Problems only, hide handled.
    fn show_field(view: &View, cx: &Context<Self>) -> Field {
        Field::new("show").control(
            div()
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(
                    Switch::new("editor-problems-only", view.problems_only)
                        .label("problems only")
                        .on_change(cx.listener(|this, on: &bool, _, cx| {
                            let on = *on;
                            this.change_view(cx, |view| view.problems_only = on);
                        })),
                )
                .child(
                    Switch::new("editor-hide-handled", view.hide_handled)
                        .label("hide handled problems")
                        .on_change(cx.listener(|this, on: &bool, _, cx| {
                            let on = *on;
                            this.change_view(cx, |view| view.hide_handled = on);
                        })),
                ),
        )
    }

    /// The sort key and direction, side by side.
    fn sort_fields(&self, cx: &Context<Self>) -> Div {
        let view = &self.draft.view;
        div()
            .flex()
            .gap(px(10.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Field::new("sort").control(self.dropdown(
                        "editor-sort",
                        EditorMenu::Sort,
                        sort_key_label(view.sort.key, view.object_kind).to_owned(),
                        || self.sort_menu(cx),
                        cx,
                    ))),
            )
            .child(
                div().flex_1().min_w_0().child(
                    Field::new("direction").control(
                        Segmented::new("editor-direction")
                            .option("↓ desc")
                            .option("↑ asc")
                            .selected(usize::from(!view.sort.descending))
                            .on_select(cx.listener(|this, index: &usize, _, cx| {
                                let descending = *index == 0;
                                this.change_view(cx, |view| {
                                    view.sort.descending = descending;
                                });
                            })),
                    ),
                ),
            )
    }

    /// Notifications for the dashboard: inherit, on, off.
    fn notifications_field(&self, cx: &Context<Self>) -> Field {
        let notify_index = match self.draft.notifications {
            ScopeSetting::Inherit | ScopeSetting::Custom(_) => 0,
            ScopeSetting::On => 1,
            ScopeSetting::Off => 2,
        };
        Field::new("notifications").control(
            Segmented::new("editor-notifications")
                .option("inherit")
                .option("on")
                .option("off")
                .selected(notify_index)
                .on_select(cx.listener(|this, index: &usize, _, cx| {
                    this.draft.notifications = match index {
                        1 => ScopeSetting::On,
                        2 => ScopeSetting::Off,
                        _ => ScopeSetting::Inherit,
                    };
                    cx.notify();
                })),
        )
    }

    /// What the dashboard shows as, a save error, and "delete dashboard…".
    fn render_inspector_footer(&self, cx: &Context<Self>) -> Div {
        let theme = cx.theme();
        let colors = theme.colors;
        div()
            .flex()
            .flex_none()
            .flex_col()
            .gap(px(8.))
            .px(px(18.))
            .py(px(12.))
            .border_t_1()
            .border_color(colors.border_header)
            .when_some(self.save_error.clone(), |footer, error| {
                footer.child(
                    div()
                        .text_size(theme.text.label)
                        .text_color(theme.states.critical)
                        .child(error),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .text_size(theme.text.label)
                    .text_color(colors.text_faint)
                    .child(format!("shows as: {}", view_label(&self.draft.view)))
                    .child(div().flex_1())
                    .when_some(
                        match &self.target {
                            EditorTarget::Existing(reference) => Some(reference.clone()),
                            EditorTarget::New => None,
                        },
                        |row, reference| {
                            row.child(
                                Link::new("editor-delete", "delete dashboard…")
                                    .quiet()
                                    .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                                        cx.emit(EditorEvent::Delete(reference.clone()));
                                    })),
                            )
                        },
                    ),
            )
    }

    /// A dropdown trigger showing `value`, with `menu` under it while open.
    fn dropdown(
        &self,
        id: &'static str,
        menu: EditorMenu,
        value: String,
        build: impl FnOnce() -> Menu,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let open = self.menus.is_open(&menu);
        div()
            .relative()
            .child(
                div()
                    .id(id)
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .h(theme.metrics.field_height)
                    .px(px(10.))
                    .rounded(theme.metrics.code_radius)
                    .border_1()
                    .border_color(if open {
                        colors.accent
                    } else {
                        colors.border_header
                    })
                    .bg(colors.code_background)
                    .text_size(theme.text.body)
                    .cursor_pointer()
                    .child(div().flex_1().min_w_0().truncate().child(value))
                    .child(
                        Icon::new(IconName::ChevronDown)
                            .size(px(12.))
                            .color(colors.text_faint),
                    )
                    .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                    .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                        this.menus.toggle(menu, down_position(event));
                        cx.notify();
                    })),
            )
            .when(open, |slot| slot.child(Popover::new(build())))
            .into_any_element()
    }

    fn group_menu(&self, cx: &Context<Self>) -> Menu {
        let state = self.state.read(cx);
        let mut menu = Menu::new("editor-group-menu").min_width(px(240.));
        for group in state.groups() {
            let id = group.id.clone();
            menu = menu.item(
                MenuItem::new(
                    gpui::ElementId::Name(format!("editor-group-{}", group.id).into()),
                    group.name.clone(),
                )
                .checked(group.id == self.draft.group_id)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.menus.close();
                    this.draft.group_id.clone_from(&id);
                    cx.notify();
                })),
            );
        }
        menu.on_dismiss(Self::dismiss_listener(cx))
    }

    fn sort_menu(&self, cx: &Context<Self>) -> Menu {
        let view = &self.draft.view;
        let mut menu = Menu::new("editor-sort-menu").min_width(px(200.));
        for key in [
            SortKey::Severity,
            SortKey::LastStateChange,
            SortKey::Host,
            SortKey::Service,
        ] {
            menu = menu.item(
                MenuItem::new(
                    gpui::ElementId::Name(format!("editor-sort-{key:?}").into()),
                    sort_key_label(key, view.object_kind),
                )
                .checked(view.sort.key == key)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.menus.close();
                    this.change_view(cx, |view| {
                        if view.sort.key != key {
                            view.sort = Sort {
                                key,
                                descending: natural_descending(key),
                            };
                        }
                    });
                })),
            );
        }
        menu.on_dismiss(Self::dismiss_listener(cx))
    }

    fn group_by_menu(&self, cx: &Context<Self>) -> Menu {
        let view = &self.draft.view;
        let mut menu = Menu::new("editor-group-by-menu").min_width(px(200.));
        for group_by in [
            GroupBy::None,
            GroupBy::Host,
            GroupBy::HostGroup,
            GroupBy::ServiceGroup,
        ] {
            if group_by == GroupBy::ServiceGroup && view.object_kind == ObjectKind::Hosts {
                continue;
            }
            menu = menu.item(
                MenuItem::new(
                    gpui::ElementId::Name(format!("editor-group-by-{group_by:?}").into()),
                    group_by_label(group_by),
                )
                .checked(view.group_by == group_by)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.menus.close();
                    this.change_view(cx, |view| view.group_by = group_by);
                })),
            );
        }
        menu.on_dismiss(Self::dismiss_listener(cx))
    }
}

impl Render for DashboardEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = self.render_header(window, cx);
        let preview = self.render_preview(cx);
        let inspector = self.render_inspector(cx);
        div()
            .id("dashboard-editor")
            .key_context(EDITOR_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::on_save))
            .on_action(cx.listener(Self::on_escape))
            .on_action(cx.listener(Self::on_discard))
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(header)
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .child(preview),
                    )
                    .child(inspector),
            )
    }
}

/// A sort key's label (`service` reads `name` for host views).
fn sort_key_label(key: SortKey, kind: ObjectKind) -> &'static str {
    match key {
        SortKey::Severity => "severity",
        SortKey::LastStateChange => "last state change",
        SortKey::Host => "host",
        SortKey::Service => match kind {
            ObjectKind::Services => "service",
            ObjectKind::Hosts => "name",
        },
    }
}

/// The inspector header's note: `severity ↓`.
fn sort_summary(sort: Sort, kind: ObjectKind) -> SharedString {
    crate::dashboard::header::sort_label(sort, kind)
}

/// A grouping's label.
fn group_by_label(group_by: GroupBy) -> &'static str {
    match group_by {
        GroupBy::None => "none",
        GroupBy::Host => "host",
        GroupBy::HostGroup => "host group",
        GroupBy::ServiceGroup => "service group",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_follow_the_view() {
        assert_eq!(sort_key_label(SortKey::Service, ObjectKind::Hosts), "name");
        assert_eq!(
            sort_key_label(SortKey::Service, ObjectKind::Services),
            "service"
        );
        assert_eq!(group_by_label(GroupBy::HostGroup), "host group");
        assert!(!save_key().is_empty());
    }
}
