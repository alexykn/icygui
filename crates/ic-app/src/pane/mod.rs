//! The detail pane for one host or service: beside the list (screens 2b and
//! 2c, 620px) or full width as a tab ("↗ open as tab").
//!
//! The pane follows links (the service's host, the host's services, parents
//! and children) with a back button. Action buttons send typed requests to
//! [`AppState::request`] (the workspace opens their dialogs); an action on
//! its way shows on its button (`acknowledging`), a failed one under the
//! buttons with Icinga's reason. The `···` beside them has the actions
//! without a key and copies the name, the output and a filter expression
//! (PANE-05).
//!
//! The object the pane shows is the one the user opens: it is asked for in
//! full at once, ahead of everything else (`Command::Focus`; once the
//! cursor rests when it moves through the list), and again when the
//! environment wakes up from quiet mode (PERF-09). The pane never waits
//! for the answer: it shows what the snapshot has, and a small `updating`
//! hint in a fixed slot of its header when fresher details take longer
//! than [`UPDATING_HINT_AFTER`].

mod history;
mod host;
pub(crate) mod model;
mod service;

use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, ClickEvent, ClipboardItem, Context, Entity, EventEmitter, FocusHandle,
    Focusable, InteractiveElement as _, IntoElement, ParentElement as _, Pixels, Render,
    ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task,
    Window, div, prelude::FluentBuilder as _,
};
use ic_model::{ObjectKey, Timestamp};
use ic_rules::ObjectMode;
use ic_ui_kit::{
    ActiveTheme as _, Button, Dismissal, EmptyState, GlyphButton, Icon, IconButton, IconName, Link,
    Menu, MenuItem, PaneHeader, Popover, Scrollbar, Theme, Tooltip, px,
};

use crate::actions::{
    Acknowledge, ActionRequest, AddComment, CheckNow, Dismiss, ObjectAction, PANE_CONTEXT,
    ScheduleDowntime,
};
use crate::app_state::hydration::row_worth_asking;
use crate::app_state::{AppState, Hydrated};
use crate::banner;
use crate::chrome::{Controls, WindowDrag};
use crate::dashboard::{HYDRATE_DEBOUNCE, SplitLayout};
use crate::menu_state::{OpenMenu, down_position};
use crate::notifications::{MuteChoice, OverrideChange};
use crate::operate::expression;
use crate::workspace::sidebar_reopen;

/// Where the pane is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PaneMode {
    /// Beside the dashboard list, closed with ×.
    Split,
    /// Full width as a tab; × closes the tab.
    Tab,
}

/// Width a tab's content is kept to, for reading.
const TAB_CONTENT_WIDTH: f32 = 860.;
/// Width of the side column of a wide service tab.
const TAB_SIDE_WIDTH: f32 = 340.;
/// Space between a wide service tab's columns.
const TAB_COLUMN_GAP: f32 = 40.;
/// A service tab at least this wide shows two columns.
const TAB_TWO_COLUMNS_FROM: f32 = 1000.;

/// The cursor moving through the list asks for the object it rests on
/// after this long, not for every row it passes.
const FOCUS_DEBOUNCE: Duration = Duration::from_millis(150);
/// The `updating` hint shows when fresher details take longer than this.
pub(crate) const UPDATING_HINT_AFTER: Duration = Duration::from_millis(300);
/// The width of the hint's slot in the header, kept whether it shows or
/// not, so nothing beside it moves.
const UPDATING_SLOT_WIDTH: f32 = 84.;

/// How a pane's body is laid out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BodyLayout {
    /// One column at the pane's width, beside the list.
    Pane,
    /// One column kept to a readable width: a narrow tab.
    Tab,
    /// A wide tab: the service's output, performance data and notes, with
    /// its check details, variables, groups and links in a column beside
    /// them.
    WideTab,
}

impl BodyLayout {
    /// The layout of a pane in `mode` that is `width` wide.
    pub(crate) fn for_width(mode: PaneMode, width: Pixels) -> Self {
        match mode {
            PaneMode::Split => Self::Pane,
            PaneMode::Tab if width >= px(TAB_TWO_COLUMNS_FROM) => Self::WideTab,
            PaneMode::Tab => Self::Tab,
        }
    }
}

/// What the pane asks its owner to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PaneEvent {
    /// × was clicked.
    Close,
}

/// The host pane's sub-tabs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum HostTab {
    /// The host's services.
    #[default]
    Services,
    /// Recent events from the local log.
    History,
    /// Custom variables.
    Vars,
    /// Check configuration and feature switches, read-only.
    Config,
}

impl HostTab {
    const ALL: [Self; 4] = [Self::Services, Self::History, Self::Vars, Self::Config];

    fn index(self) -> usize {
        Self::ALL.iter().position(|tab| *tab == self).unwrap_or(0)
    }
}

/// The detail pane view.
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent switches of one view: the OK services shown, the sidebar, a resting cursor, marked rows beside it"
)]
pub(crate) struct ObjectPane {
    state: Entity<AppState>,
    mode: PaneMode,
    /// The object the pane was opened for: a tab's identity, which stays
    /// while links inside the tab are followed.
    opened: ObjectKey,
    object: ObjectKey,
    /// Objects visited before, for the back button.
    history: Vec<ObjectKey>,
    host_tab: HostTab,
    /// Whether the host pane lists every OK service.
    show_all_ok: bool,
    scroll: ScrollHandle,
    focus_handle: FocusHandle,
    /// With the sidebar hidden, a tab (or a pane covering a narrow list)
    /// shows the window controls and the sidebar button in its header.
    sidebar_open: bool,
    drag: WindowDrag,
    /// The objects last asked for their details (a host pane's service
    /// rows without output), and in which wake of the environment.
    hydration_wanted: (Vec<ObjectKey>, u64),
    /// Asks for them once the pane rests on an object.
    hydrate_task: Option<Task<()>>,
    /// The object last asked for at once (`Command::Focus`), in which wake
    /// of the environment (`AppState::wake`): asked again when either
    /// changes.
    focused: Option<(ObjectKey, u64)>,
    /// Asks for it.
    focus_task: Option<Task<()>>,
    /// The object shown next comes from the cursor moving through the
    /// list: asked for once it rests there.
    focus_when_resting: bool,
    /// The object shown and since when it has been updating.
    updating_since: Option<(ObjectKey, Instant)>,
    /// Draws the pane again when the hint is due.
    updating_task: Option<Task<()>>,
    /// The `···` menu beside the action buttons.
    menu: OpenMenu<PaneMenu>,
    /// The local event log's entries for the shown object (PANE-04).
    log: Option<history::PaneHistory>,
    /// Reads them.
    log_task: Option<Task<()>>,
    /// Rows are marked in the list beside the pane: the action keys act on
    /// them, not on this object, so the buttons show no key hints.
    keys_elsewhere: bool,
    _subscriptions: Vec<Subscription>,
}

/// The pane's popup menus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PaneMenu {
    /// `···` beside the action buttons.
    More,
}

impl EventEmitter<PaneEvent> for ObjectPane {}

impl Focusable for ObjectPane {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ObjectPane {
    pub(crate) fn new(
        state: Entity<AppState>,
        object: ObjectKey,
        mode: PaneMode,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscriptions = vec![cx.observe(&state, |_, _, cx| cx.notify())];
        // A pane beside the list shows at once; a tab's pane is made for
        // every open tab, and its object counts as seen when the tab shows
        // (the workspace).
        if mode == PaneMode::Split {
            mark_seen(&state, &object, cx);
        }
        Self {
            state,
            mode,
            opened: object.clone(),
            object,
            history: Vec::new(),
            host_tab: HostTab::default(),
            show_all_ok: false,
            scroll: ScrollHandle::new(),
            focus_handle: cx.focus_handle(),
            sidebar_open: true,
            drag: WindowDrag::default(),
            hydration_wanted: (Vec::new(), 0),
            hydrate_task: None,
            focused: None,
            focus_task: None,
            focus_when_resting: false,
            updating_since: None,
            updating_task: None,
            menu: OpenMenu::default(),
            log: None,
            log_task: None,
            keys_elsewhere: false,
            _subscriptions: subscriptions,
        }
    }

    /// The rows this pane offers the engine: a host pane's service rows
    /// (it fetches those it doesn't hold current; the pane's own object is
    /// asked for at once, [`ObjectPane::want_focus`]).
    fn details_wanted(&self, cx: &App) -> Vec<ObjectKey> {
        let state = self.state.read(cx);
        let snapshot = state.snapshot();
        let mut keys = Vec::new();
        if let ObjectKey::Host { name } = &self.object
            && self.host_tab == HostTab::Services
            && let Some(host) = snapshot.hosts.get(name)
        {
            keys.extend(
                model::host_services(snapshot, host, self.show_all_ok)
                    .shown
                    .iter()
                    .map(|service| service.object_key())
                    .filter(|key| row_worth_asking(snapshot, key)),
            );
        }
        keys
    }

    /// Asks for the details of what the pane shows, once it rests on it
    /// (moving the cursor through a list with the pane open asks only for
    /// where it stops).
    fn want_details(&mut self, cx: &mut Context<Self>) {
        let keys = self.details_wanted(cx);
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
                    this.hydration_wanted.0.clear();
                }
            });
        }));
    }

    /// Asks for the shown object in full, ahead of everything else
    /// (`Command::Focus`): at once when the user opened it, once the cursor
    /// rests when it moved there, and again when the environment wakes up.
    /// The engine sends nothing when it holds the object current. Not
    /// connected, it is asked for on a later render.
    fn want_focus(&mut self, cx: &mut Context<Self>) {
        let wake = self.state.read(cx).wake();
        // Only the next change of object can come from the cursor.
        let resting = std::mem::take(&mut self.focus_when_resting);
        if self
            .focused
            .as_ref()
            .is_some_and(|(key, at)| *key == self.object && *at == wake)
        {
            return;
        }
        self.focused = Some((self.object.clone(), wake));
        let delay = if resting {
            FOCUS_DEBOUNCE
        } else {
            Duration::ZERO
        };
        let key = self.object.clone();
        self.focus_task = Some(cx.spawn(async move |this, cx| {
            if !delay.is_zero() {
                cx.background_executor().timer(delay).await;
            }
            let _ = this.update(cx, |this, cx| {
                if this.object == key && !this.state.read(cx).focus(&key) {
                    this.focused = None;
                }
            });
        }));
    }

    /// Whether the shown object has been updating (`Snapshot::updating`)
    /// for longer than [`UPDATING_HINT_AFTER`]: its header says so then.
    fn updating_hint(&mut self, updating: bool, cx: &mut Context<Self>) -> bool {
        if !updating {
            self.updating_since = None;
            self.updating_task = None;
            return false;
        }
        let now = Instant::now();
        if let Some((key, since)) = &self.updating_since
            && *key == self.object
        {
            return now.saturating_duration_since(*since) >= UPDATING_HINT_AFTER;
        }
        self.updating_since = Some((self.object.clone(), now));
        self.updating_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(UPDATING_HINT_AFTER).await;
            let _ = this.update(cx, |_, cx| cx.notify());
        }));
        false
    }

    /// The object shown.
    pub(crate) fn object(&self) -> &ObjectKey {
        &self.object
    }

    /// Whether the header shows the `updating` hint.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn shows_updating(&self) -> bool {
        self.updating_since.as_ref().is_some_and(|(key, since)| {
            *key == self.object && since.elapsed() >= UPDATING_HINT_AFTER
        })
    }

    /// Whether the back button has somewhere to go.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn can_go_back(&self) -> bool {
        !self.history.is_empty()
    }

    /// The host pane's selected tab.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn host_tab(&self) -> HostTab {
        self.host_tab
    }

    /// The scrolling part of the body.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn body_scroll(&self) -> &ScrollHandle {
        &self.scroll
    }

    /// Shows `object` the list's cursor moved to, forgetting the back
    /// history: asked for in full once the cursor rests.
    pub(crate) fn show(&mut self, object: ObjectKey, cx: &mut Context<Self>) {
        if self.object == object {
            return;
        }
        self.focus_when_resting = true;
        self.history.clear();
        self.switch_to(object, cx);
        cx.notify();
    }

    /// Shows `object` the user opened (a click, Enter, a notification, the
    /// palette), forgetting the back history: asked for in full at once.
    pub(crate) fn open(&mut self, object: ObjectKey, cx: &mut Context<Self>) {
        self.focus_when_resting = false;
        if self.object == object {
            return;
        }
        self.history.clear();
        self.switch_to(object, cx);
        cx.notify();
    }

    /// Follows a link to `object`; the back button returns.
    pub(crate) fn navigate(&mut self, object: ObjectKey, cx: &mut Context<Self>) {
        if self.object == object {
            return;
        }
        self.history.push(self.object.clone());
        self.switch_to(object, cx);
        cx.notify();
    }

    /// Goes back to the previous object.
    pub(crate) fn back(&mut self, cx: &mut Context<Self>) {
        if let Some(previous) = self.history.pop() {
            self.switch_to(previous, cx);
            cx.notify();
        }
    }

    /// Selects a host sub-tab.
    pub(crate) fn select_host_tab(&mut self, tab: HostTab, cx: &mut Context<Self>) {
        if self.host_tab != tab {
            self.host_tab = tab;
            self.scroll.set_offset(gpui::point(px(0.), px(0.)));
            cx.notify();
        }
    }

    /// Tells the pane whether the sidebar is shown.
    pub(crate) fn set_sidebar_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.sidebar_open != open {
            self.sidebar_open = open;
            cx.notify();
        }
    }

    /// Tells the pane whether rows are marked in the list beside it (the
    /// action keys then act on them: marked rows, then the pane, then the
    /// cursor). Set by the list as it renders, before the pane does.
    pub(crate) fn set_keys_elsewhere(&mut self, elsewhere: bool) {
        self.keys_elsewhere = elsewhere;
    }

    /// Whether the buttons' key hints are blank because rows are marked.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn keys_elsewhere(&self) -> bool {
        self.keys_elsewhere
    }

    fn switch_to(&mut self, object: ObjectKey, cx: &mut App) {
        mark_seen(&self.state, &object, cx);
        let kind_changed = matches!(self.object, ObjectKey::Host { .. })
            != matches!(object, ObjectKey::Host { .. });
        let host_changed = self.object.host_name() != object.host_name();
        self.object = object;
        if kind_changed || host_changed {
            self.host_tab = HostTab::default();
            self.show_all_ok = false;
        }
        self.scroll.set_offset(gpui::point(px(0.), px(0.)));
    }

    /// The body's layout. A tab is as wide as the window less the sidebar.
    fn body_layout(&self, window: &Window, theme: &Theme) -> BodyLayout {
        let sidebar = if self.sidebar_open {
            theme.metrics.sidebar_width
        } else {
            px(0.)
        };
        BodyLayout::for_width(self.mode, window.viewport_size().width - sidebar)
    }

    /// Closes the pane: beside the list it asks the dashboard to; a tab
    /// closes itself, whatever object it shows after following links.
    pub(crate) fn close(&mut self, cx: &mut Context<Self>) {
        if self.mode == PaneMode::Tab {
            let tab = self.opened.clone();
            self.state.update(cx, |state, cx| {
                if state.close_tab(&tab) {
                    cx.notify();
                }
            });
        }
        cx.emit(PaneEvent::Close);
    }

    /// Asks for an action on the shown object (refused with a toast if
    /// the API user may not run it; the workspace opens its dialog).
    fn request(&self, action: ObjectAction, cx: &mut App) {
        let request = ActionRequest {
            action,
            targets: vec![self.object.clone()],
            review: false,
        };
        self.state.update(cx, |state, cx| {
            let _ = state.request(request);
            cx.notify();
        });
    }

    /// Watches, mutes or unmutes the shown object and says so (NOTE-02).
    fn change_override(&self, change: OverrideChange, cx: &mut App) {
        let objects = [self.object.clone()];
        self.state.update(cx, |state, cx| {
            if let Some(message) = change.apply(state, &objects, Timestamp::now()) {
                state.inform(message, None);
            }
            cx.notify();
        });
    }

    /// Copies `text` and says what was copied.
    fn copy(&self, what: &str, text: String, cx: &mut App) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.state.update(cx, |state, cx| {
            state.inform(format!("Copied {what}"), None);
            cx.notify();
        });
    }

    /// The open popup menu.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn open_menu(&self) -> Option<PaneMenu> {
        self.menu.current().copied()
    }

    /// Opens the `···` menu, as its button does.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn open_more_menu(&mut self, cx: &mut Context<Self>) {
        self.menu.open(PaneMenu::More);
        cx.notify();
    }

    /// The `···` menu's items (`output`: the object's output, if loaded).
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn more_menu_labels(
        &self,
        output: Option<String>,
        cx: &Context<Self>,
    ) -> Vec<String> {
        more_menu(self, output, cx)
            .item_labels()
            .into_iter()
            .map(|label| label.to_string())
            .collect()
    }

    fn on_acknowledge(&mut self, _: &Acknowledge, _: &mut Window, cx: &mut Context<Self>) {
        self.request(ObjectAction::Acknowledge, cx);
    }

    fn on_downtime(&mut self, _: &ScheduleDowntime, _: &mut Window, cx: &mut Context<Self>) {
        self.request(ObjectAction::ScheduleDowntime, cx);
    }

    fn on_check_now(&mut self, _: &CheckNow, _: &mut Window, cx: &mut Context<Self>) {
        self.request(ObjectAction::CheckNow, cx);
    }

    fn on_comment(&mut self, _: &AddComment, _: &mut Window, cx: &mut Context<Self>) {
        self.request(ObjectAction::AddComment, cx);
    }

    /// Escape in a tab: back to the dashboard; the tab stays open.
    fn on_dismiss(&mut self, _: &Dismiss, _: &mut Window, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            if state.show_dashboard() {
                cx.notify();
            }
        });
    }

    fn render_header(&self, updating: bool, window: &Window, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let controls = Controls::of(window, cx);
        let label = match self.object {
            ObjectKey::Host { .. } => "host",
            ObjectKey::Service { .. } => "service",
        };
        let mut header = PaneHeader::new("pane-header");
        // At the window's left edge (a tab, or covering a narrow list) with
        // the sidebar hidden, the header carries the window controls.
        let left_edge = match self.mode {
            PaneMode::Tab => true,
            PaneMode::Split => {
                SplitLayout::for_window(window, self.sidebar_open, &theme.metrics)
                    == SplitLayout::Cover
            }
        };
        if left_edge && !self.sidebar_open {
            header = header.leading(sidebar_reopen(controls, theme));
        }
        if let Some(previous) = self.history.last() {
            header = header.leading(
                IconButton::new("pane-back", IconName::ArrowLeft)
                    .icon_size(theme.metrics.icon_small)
                    .color(theme.colors.text_muted)
                    .tooltip(Tooltip::new(format!("Back to {}", short_name(previous))))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.back(cx))),
            );
        }
        header = header.label(label).status(updating_slot(updating, theme));
        let header = match self.mode {
            PaneMode::Split => header
                .child(
                    Link::new("open-as-tab", "↗ open as tab")
                        .quiet()
                        .text_size(theme.text.small)
                        .tooltip(Tooltip::new("Open as tab").key(open_as_tab_key()))
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            let object = this.object.clone();
                            this.state.update(cx, |state, cx| {
                                if state.open_tab(object) {
                                    cx.notify();
                                }
                            });
                        })),
                )
                .on_close(cx.listener(|this, _: &ClickEvent, _, cx| this.close(cx))),
            PaneMode::Tab => {
                header.on_close(cx.listener(|this, _: &ClickEvent, _, cx| this.close(cx)))
            }
        };
        // The pane's header is part of the window's top edge, beside the list
        // or as a tab.
        self.drag
            .attach(div().id("pane-header-drag").child(header), controls)
            .into_any_element()
    }
}

impl Render for ObjectPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.want_focus(cx);
        self.want_details(cx);
        self.want_history(cx);
        let theme = cx.theme().clone();
        let snapshot = self.state.read(cx).snapshot().clone();
        let updating = self.updating_hint(snapshot.is_updating(&self.object), cx);
        let header = self.render_header(updating, window, cx);
        let now = Timestamp::now();
        let layout = self.body_layout(window, &theme);
        let loading = {
            let state = self.state.read(cx);
            state.has_no_objects() && state.connection().is_starting()
        };
        let body = match &self.object {
            ObjectKey::Service { key } => match snapshot.services.get(key) {
                Some(service) => service::render(self, &snapshot, service, now, layout, cx),
                None => missing(&self.object, loading, &theme),
            },
            ObjectKey::Host { name } => match snapshot.hosts.get(name) {
                Some(host) => host::render(self, &snapshot, host, now, cx),
                None => missing(&self.object, loading, &theme),
            },
        };
        // A tab is the whole main area: the connection banners show over it.
        let banners = if self.mode == PaneMode::Tab {
            banner::banners(&self.state, now, true, cx)
        } else {
            Vec::new()
        };
        div()
            .id("object-pane")
            .flex()
            .flex_col()
            .size_full()
            .min_w_0()
            .bg(theme.colors.pane_background)
            .when(self.mode == PaneMode::Tab, |pane| {
                pane.key_context(PANE_CONTEXT)
                    .track_focus(&self.focus_handle)
                    .on_action(cx.listener(Self::on_acknowledge))
                    .on_action(cx.listener(Self::on_downtime))
                    .on_action(cx.listener(Self::on_check_now))
                    .on_action(cx.listener(Self::on_comment))
                    .on_action(cx.listener(Self::on_dismiss))
            })
            .child(header)
            .children(banners)
            .child(body)
    }
}

/// The header's slot for the `updating` hint: always there and as wide,
/// empty unless fresher details of the shown object take a moment
/// (PERF-09).
fn updating_slot(updating: bool, theme: &Theme) -> impl IntoElement {
    div()
        .id("pane-updating")
        .flex()
        .flex_none()
        .items_center()
        .gap(px(5.))
        .w(px(UPDATING_SLOT_WIDTH))
        .when(updating, |slot| {
            slot.child(
                Icon::new(IconName::Loader)
                    .size(theme.metrics.icon_small)
                    .color(theme.colors.text_faint),
            )
            .child(
                div()
                    .text_size(theme.text.small)
                    .text_color(theme.colors.text_faint)
                    .child("updating…"),
            )
            .tooltip(Tooltip::text(
                "Fetching the latest details from Icinga; shown meanwhile is what icygui has",
            ))
        })
}

/// The pane body for an object the snapshot doesn't have: still loading,
/// or gone.
fn missing(object: &ObjectKey, loading: bool, theme: &Theme) -> AnyElement {
    if loading {
        return EmptyState::new(format!("Loading {}…", short_name(object)))
            .leading(
                Icon::new(IconName::Loader)
                    .size(px(20.))
                    .color(theme.colors.accent),
            )
            .detail("It shows as soon as the objects are loaded.")
            .into_any_element();
    }
    EmptyState::new(format!("{} is gone", short_name(object)))
        .leading(
            Icon::new(IconName::TriangleAlert)
                .size(px(20.))
                .color(theme.colors.text_faint),
        )
        .detail("It was removed from Icinga, or isn't part of the latest snapshot.")
        .into_any_element()
}

/// `postgres-replication on db-prod-03` or `db-prod-03`.
fn short_name(object: &ObjectKey) -> String {
    match object {
        ObjectKey::Host { name } => name.to_string(),
        ObjectKey::Service { key } => format!("{} on {}", key.name, key.host),
    }
}

/// The shortcut for "open as tab", as tooltips show it.
fn open_as_tab_key() -> &'static str {
    if cfg!(target_os = "macos") {
        "⌘↩"
    } else {
        "ctrl-enter"
    }
}

/// A scrolling column for the pane body with the overlay scrollbar.
fn scroll_area(
    id: &'static str,
    scroll: &ScrollHandle,
    content: impl IntoElement,
) -> impl IntoElement {
    div()
        .relative()
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .child(
            div()
                .id(id)
                .flex()
                .flex_col()
                .size_full()
                .overflow_y_scroll()
                .track_scroll(scroll)
                .child(content),
        )
        .child(Scrollbar::vertical(scroll))
}

/// The action buttons shared by service and host panes (PANE-01), with
/// the `···` menu, then the last failure and the watch or mute. Actions
/// the API user may not run are disabled, and their tooltip says why
/// (ENV-09). Nothing in the row moves with the state: the first slot has
/// one width for *acknowledge* (a problem), *remove ack* (acknowledged)
/// and a disabled *acknowledge* (nothing to acknowledge); an action on its
/// way keeps its button's label and shows `…` where its key was, the
/// marker (`ack pending…`) in its tooltip; while rows are marked in the
/// list the keys act on them, so the hints keep their room, empty.
fn action_buttons(
    pane: &ObjectPane,
    acknowledged: bool,
    problem: bool,
    output: Option<String>,
    cx: &Context<ObjectPane>,
) -> impl IntoElement {
    let theme = cx.theme();
    let state = pane.state.read(cx);
    let pending = state.pending_action(&pane.object);
    let keys_elsewhere = pane.keys_elsewhere;
    let button =
        |id: &'static str, label: &'static str, key: Option<&'static str>, action: ObjectAction| {
            if let Some((kind, marker)) = pending
                && *kind == action
            {
                return Button::new(id, label)
                    .key_hint(PENDING_HINT)
                    .disabled(true)
                    .tooltip(Tooltip::new(marker));
            }
            let denial = state.action_denial(&action);
            let mut button = Button::new(id, label);
            if let Some(key) = key {
                button = button.key_hint(key).key_blank(keys_elsewhere);
            }
            match denial {
                Some(denial) => button.disabled(true).tooltip(Tooltip::new(denial)),
                None => button.on_click(cx.listener(
                    move |this: &mut ObjectPane, _: &ClickEvent, _, cx| {
                        this.request(action.clone(), cx);
                    },
                )),
            }
        };
    let first = if acknowledged {
        button(
            "remove-ack",
            "remove ack",
            None,
            ObjectAction::RemoveAcknowledgement,
        )
    } else if problem {
        button(
            "acknowledge",
            "acknowledge",
            Some("a"),
            ObjectAction::Acknowledge,
        )
        .primary()
    } else {
        let what = match &pane.object {
            ObjectKey::Host { .. } => "the host is UP",
            ObjectKey::Service { .. } => "the service is OK",
        };
        Button::new("acknowledge", "acknowledge")
            .key_hint("a")
            .key_blank(keys_elsewhere)
            .disabled(true)
            .tooltip(Tooltip::new(format!("Nothing to acknowledge: {what}")))
    };
    let first_width = Button::width_for(theme, "acknowledge", true).max(Button::width_for(
        theme,
        "remove ack",
        true,
    ));
    let failure = failure_line(pane, cx);
    let watch = override_line(pane, cx);
    let more = more_trigger(pane, output, cx);
    div()
        .flex()
        .flex_col()
        .gap(px(10.))
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(8.))
                .child(first.width(first_width))
                .child(button(
                    "downtime",
                    "downtime",
                    Some("d"),
                    ObjectAction::ScheduleDowntime,
                ))
                .child(button(
                    "check-now",
                    "check now",
                    Some("r"),
                    ObjectAction::CheckNow,
                ))
                .child(button(
                    "comment",
                    "comment",
                    Some("c"),
                    ObjectAction::AddComment,
                ))
                .child(more),
        )
        .children(failure)
        .children(watch)
}

/// Where an action button's key was while the action is on its way: it
/// keeps the button's width (a key hint is one character).
const PENDING_HINT: &str = "\u{2026}";

/// The object's watch or mute (NOTE-02), with a way to end it.
fn override_line(pane: &ObjectPane, cx: &Context<ObjectPane>) -> Option<impl IntoElement> {
    let theme = cx.theme();
    let now = Timestamp::now();
    let entry = pane.state.read(cx).object_override(&pane.object, now)?;
    let (icon, color, end) = match entry.mode {
        ObjectMode::Watch => (IconName::Bell, theme.colors.accent, "stop watching"),
        ObjectMode::Mute => (IconName::BellOff, theme.colors.text_muted, "unmute"),
    };
    Some(
        div()
            .flex()
            .items_center()
            .gap(px(8.))
            .text_size(theme.text.small)
            .child(Icon::new(icon).size(theme.metrics.icon_small).color(color))
            .child(
                div()
                    .text_color(theme.colors.text_muted)
                    .child(crate::notifications::override_text(entry, now)),
            )
            .child(
                Link::new("pane-override-clear", end)
                    .quiet()
                    .on_click(cx.listener(|this: &mut ObjectPane, _: &ClickEvent, _, cx| {
                        this.change_override(OverrideChange::Clear, cx);
                    })),
            ),
    )
}

/// The last failed action on the pane's object, with Icinga's reason and
/// a way to dismiss it ("failures show on the button that caused them").
fn failure_line(pane: &ObjectPane, cx: &Context<ObjectPane>) -> Option<impl IntoElement> {
    let theme = cx.theme();
    let failure = pane.state.read(cx).action_failure(&pane.object)?;
    Some(
        div()
            .flex()
            .items_start()
            .gap(px(8.))
            .text_size(theme.text.small)
            .child(
                div().pt(px(2.)).child(
                    Icon::new(IconName::TriangleAlert)
                        .size(theme.metrics.icon_small)
                        .color(theme.states.fill.critical),
                ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_color(theme.colors.text_muted)
                    .child(format!(
                        "{} failed: {}",
                        failure.action.label(),
                        failure.reason
                    )),
            )
            .child(
                Link::new("dismiss-action-failure", "dismiss")
                    .quiet()
                    .on_click(cx.listener(|this: &mut ObjectPane, _: &ClickEvent, _, cx| {
                        let object = this.object.clone();
                        this.state.update(cx, |state, cx| {
                            if state.dismiss_action_failure(&object) {
                                cx.notify();
                            }
                        });
                    })),
            ),
    )
}

/// The `···` beside the action buttons, with its menu while open.
fn more_trigger(
    pane: &ObjectPane,
    output: Option<String>,
    cx: &Context<ObjectPane>,
) -> impl IntoElement {
    let open = pane.menu.is_open(&PaneMenu::More);
    div()
        .relative()
        .flex_none()
        .child(
            GlyphButton::new("pane-more", "···")
                .text_size(px(13.))
                .color(cx.theme().colors.text_muted)
                .selected(open)
                .when(!open, |trigger| {
                    trigger.tooltip(Tooltip::new("More: check result, command, copy"))
                })
                .on_click(
                    cx.listener(|this: &mut ObjectPane, event: &ClickEvent, _, cx| {
                        this.menu.toggle(PaneMenu::More, down_position(event));
                        cx.notify();
                    }),
                ),
        )
        .when(open, |trigger| {
            trigger.child(Popover::new(more_menu(pane, output, cx)))
        })
}

/// The pane's `···`: the actions without a key, and copying (PANE-05).
fn more_menu(pane: &ObjectPane, output: Option<String>, cx: &Context<ObjectPane>) -> Menu {
    let state = pane.state.read(cx);
    let item = |id: &'static str, label: &'static str, action: ObjectAction| {
        let item = MenuItem::new(id, label);
        match state.action_denial(&action) {
            Some(denial) => item.disabled(true).tooltip(Tooltip::new(denial)),
            None => item.on_click(cx.listener(
                move |this: &mut ObjectPane, _: &ClickEvent, _, cx| {
                    this.menu.close();
                    this.request(action.clone(), cx);
                    cx.notify();
                },
            )),
        }
    };
    let copy = |id: &'static str, label: &'static str, what: &'static str, text: String| {
        MenuItem::new(id, label).on_click(cx.listener(
            move |this: &mut ObjectPane, _: &ClickEvent, _, cx| {
                this.menu.close();
                this.copy(what, text.clone(), cx);
                cx.notify();
            },
        ))
    };
    let objects = std::slice::from_ref(&pane.object);
    let downtimes = state
        .snapshot()
        .downtimes
        .get(&pane.object)
        .map_or(0, Vec::len);
    let mut menu = Menu::new("pane-menu")
        .item(item(
            "pane-result",
            "submit check result",
            ObjectAction::SubmitCheckResult,
        ))
        .item(item(
            "pane-command",
            "run command",
            ObjectAction::RunCommand,
        ));
    if downtimes > 0 {
        menu = menu.item(item(
            "pane-remove-downtimes",
            if downtimes == 1 {
                "remove its downtime"
            } else {
                "remove all its downtimes"
            },
            ObjectAction::RemoveDowntimes,
        ));
    }
    menu = override_items(menu, pane, cx);
    menu = menu
        .separator()
        .item(copy(
            "pane-copy-name",
            "copy name",
            "the name",
            expression::names(objects),
        ))
        .item(copy(
            "pane-copy-filter",
            "copy filter expression",
            "the filter expression",
            expression::filter(objects),
        ));
    if let Some(output) = output.filter(|output| !output.is_empty()) {
        menu = menu.item(copy(
            "pane-copy-output",
            "copy output",
            "the output",
            output,
        ));
    }
    // The notes and action URLs, macros resolved (PANE-05).
    let links = object_links(state.snapshot(), &pane.object);
    if !links.is_empty() {
        menu = menu.separator();
        for (index, (label, url)) in links.into_iter().enumerate() {
            let target = url.clone();
            menu = menu.item(
                MenuItem::new(SharedString::from(format!("pane-open-{index}")), label)
                    .tooltip(Tooltip::new(format!("Open {url} in the browser")))
                    .on_click(
                        cx.listener(move |this: &mut ObjectPane, _: &ClickEvent, _, cx| {
                            this.menu.close();
                            cx.open_url(&target);
                            cx.notify();
                        }),
                    ),
            );
        }
    }
    menu.on_dismiss(
        cx.listener(|this: &mut ObjectPane, dismissal: &Dismissal, _, cx| {
            this.menu.dismissed(*dismissal);
            cx.notify();
        }),
    )
}

/// The pane menu's watch and mute items (NOTE-02): local to this computer.
fn override_items(mut menu: Menu, pane: &ObjectPane, cx: &Context<ObjectPane>) -> Menu {
    let now = Timestamp::now();
    let current = pane
        .state
        .read(cx)
        .object_override(&pane.object, now)
        .map(|entry| entry.mode);
    let change = |id: &'static str, label: String, change: OverrideChange| {
        MenuItem::new(id, label).on_click(cx.listener(
            move |this: &mut ObjectPane, _: &ClickEvent, _, cx| {
                this.menu.close();
                this.change_override(change, cx);
                cx.notify();
            },
        ))
    };
    menu = menu.separator().label("notifications");
    if current != Some(ObjectMode::Watch) {
        menu = menu.item(
            change(
                "pane-watch",
                "watch: always notify".to_owned(),
                OverrideChange::Watch,
            )
            .tooltip(Tooltip::new(
                "Notify with the environment's rule even when no dashboard does",
            )),
        );
    }
    for (index, choice) in MuteChoice::ALL.into_iter().enumerate() {
        menu = menu.item(change(
            [
                "pane-mute-1h",
                "pane-mute-4h",
                "pane-mute-morning",
                "pane-mute-forever",
            ]
            .get(index)
            .copied()
            .unwrap_or("pane-mute"),
            format!("mute {}", choice.label(now)),
            OverrideChange::Mute(choice),
        ));
    }
    if let Some(mode) = current {
        menu = menu.item(change(
            "pane-override-clear-menu",
            match mode {
                ObjectMode::Watch => "stop watching".to_owned(),
                ObjectMode::Mute => "unmute".to_owned(),
            },
            OverrideChange::Clear,
        ));
    }
    menu
}

/// The object's notes and action URLs that open in a browser, macros
/// resolved like Icinga Web (`open notes url`, `open action url 2`).
fn object_links(
    snapshot: &ic_core::snapshot::Snapshot,
    object: &ObjectKey,
) -> Vec<(String, String)> {
    let (links, scope) = match object {
        ObjectKey::Host { name } => {
            let Some(host) = snapshot.hosts.get(name) else {
                return Vec::new();
            };
            (
                &host.links,
                model::MacroScope {
                    host: Some(host),
                    service: None,
                },
            )
        }
        ObjectKey::Service { key } => {
            let Some(service) = snapshot.services.get(key) else {
                return Vec::new();
            };
            (
                &service.links,
                model::MacroScope {
                    host: snapshot.host_of(key).map(AsRef::as_ref),
                    service: Some(service),
                },
            )
        }
    };
    let mut found = Vec::new();
    for (what, raw) in [
        ("notes url", &links.notes_url),
        ("action url", &links.action_url),
    ] {
        let urls: Vec<String> = model::link_urls(raw, scope)
            .into_iter()
            .filter(|url| model::is_web_link(url))
            .collect();
        let several = urls.len() > 1;
        for (index, url) in urls.into_iter().enumerate() {
            let label = if several {
                format!("open {what} {}", index + 1)
            } else {
                format!("open {what}")
            };
            found.push((label, url));
        }
    }
    found
}

/// A copy-to-clipboard button, shown while the mouse is over the element
/// marked with `group` (the pane stays as calm as the design otherwise).
fn copy_button(
    id: &'static str,
    text: String,
    tooltip: &'static str,
    group: &'static str,
    theme: &Theme,
) -> impl IntoElement {
    div()
        .flex_none()
        .invisible()
        .group_hover(group, gpui::Styled::visible)
        .child(
            IconButton::new(id, IconName::Copy)
                .icon_size(theme.metrics.icon_small)
                .color(theme.colors.text_faint)
                .tooltip(Tooltip::new(tooltip))
                .on_click(move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                }),
        )
}

/// The hover group of a pane's title (reveals its copy button).
const TITLE_GROUP: &str = "pane-title";

/// A web link that opens in the browser; a URL too long for its cell is
/// cut with an ellipsis and shown whole in the tooltip.
fn web_link(id: SharedString, url: &str) -> impl IntoElement {
    let target = url.to_owned();
    Link::new(id, url.to_owned())
        .truncate()
        .tooltip(Tooltip::new(format!("Open {url} in the browser")))
        .on_click(move |_, _, cx| cx.open_url(&target))
}

/// `object` shows in a pane: the active environment's notifications about
/// it count as seen (A2), right after this update.
pub(crate) fn mark_seen(state: &Entity<AppState>, object: &ObjectKey, cx: &mut App) {
    let state = state.clone();
    let object = object.clone();
    cx.defer(move |cx| {
        state.update(cx, |state, cx| {
            if state.mark_object_read(&object) {
                cx.notify();
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_tabs_get_two_columns() {
        assert_eq!(
            BodyLayout::for_width(PaneMode::Split, px(2000.)),
            BodyLayout::Pane,
            "beside the list the pane keeps its width"
        );
        assert_eq!(
            BodyLayout::for_width(PaneMode::Tab, px(1140.)),
            BodyLayout::WideTab,
            "1440px window less the sidebar"
        );
        assert_eq!(
            BodyLayout::for_width(PaneMode::Tab, px(TAB_TWO_COLUMNS_FROM - 1.)),
            BodyLayout::Tab
        );
    }

    #[test]
    fn the_menu_opens_links_with_their_macros_resolved() {
        let snapshot = crate::fixture::build(Timestamp::from_unix_seconds(1_790_000_000.)).snapshot;
        let replication = ObjectKey::service("db-prod-03", "postgres-replication");
        let links = object_links(&snapshot, &replication);
        assert!(!links.is_empty());
        for (label, url) in &links {
            assert!(label.starts_with("open "), "{label}");
            assert!(url.starts_with("http"), "{url}");
            assert!(!url.contains('$'), "macros resolved: {url}");
        }
        assert!(object_links(&snapshot, &ObjectKey::host("no-such-host")).is_empty());
    }

    #[test]
    fn short_names_read_service_on_host() {
        assert_eq!(
            short_name(&ObjectKey::service("db-prod-03", "postgres-replication")),
            "postgres-replication on db-prod-03"
        );
        assert_eq!(short_name(&ObjectKey::host("db-prod-03")), "db-prod-03");
    }
}
