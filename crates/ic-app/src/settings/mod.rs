//! The settings panel (`secondary-,`, the app menu's *Settings*, the
//! palette; PLAN.md §4.2, mock-up 02): a large panel over the main window,
//! in the style of Zed's settings.
//!
//! - *Left:* a search field, the six categories (general, appearance,
//!   notifications, icinga, keymap, advanced) with their icons, the open
//!   category's sections on a guide line (the one in view in the accent
//!   colour; a click scrolls to it), and `focus navbar` (`secondary-shift-e`,
//!   then the arrows).
//! - *Right:* the page's header (its name, whom it is for, `✓ saved`,
//!   *edit in settings file*, which opens the settings file in the default
//!   editor) and one row per setting: name, one line of description, the
//!   control at the right.
//!
//! Changes apply at once, as in Zed: every switch, choice and chip writes
//! the settings straight away; a text field applies on Enter or when it
//! loses the keyboard, and a value that doesn't parse shows its problem
//! under the row and isn't applied. So there is no save or cancel; Escape
//! clears the search, or closes the panel.
//!
//! A search looks through every category: the navigation dims the
//! categories without a match and counts the matches of the others; the
//! page lists the matching rows under `category · section` headings with
//! the match marked, and the rows work in place.
//!
//! Everything stays on this computer (D6): Icinga's own notification
//! switches are never touched.

pub(crate) mod about;
mod environment;
mod files;
mod keyboard;
mod model;
mod pages;
mod rows;

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::rc::Rc;

use gpui::{
    Action, AnyElement, App, AppContext as _, Bounds, ClickEvent, Context, Entity, EventEmitter,
    FocusHandle, Focusable, InteractiveElement as _, IntoElement, KeyBinding, MouseButton,
    ParentElement as _, Pixels, Render, ScrollHandle, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, div,
    prelude::FluentBuilder as _,
};
use ic_config::{Appearance, General};
use ic_ui_kit::input::{Escape, InputEvent, InputState};
use ic_ui_kit::{
    ActiveTheme as _, Button, Icon, IconButton, IconName, Metrics, Scrollbar, TextField, Theme,
    Tooltip, px,
};

use self::files::LogSummary;
// Only the UI tests read it, and they run on Linux only.
#[cfg(all(test, target_os = "linux"))]
pub(crate) use self::files::OPENED;
use self::keyboard::{
    CONTROL_CONTEXT, ControlActivate, ControlNext, ControlPrevious, Region, Stop,
};
pub(crate) use self::model::{FieldId, RuleFlag, ScopeKey, SettingsPage};
use self::model::{
    PageMatches, Section, Setting, format_clock, format_min_duration, matching_settings,
    normalized, parse_clock, parse_min_duration, parse_reconcile, parse_retention, parse_threshold,
    parse_window,
};
use crate::app_state::{AppState, NotificationPlan};
use crate::menu_state::OpenMenu;
use crate::operate::dialog::{NextField, PreviousField};

/// Key context of the panel.
pub(crate) const SETTINGS_CONTEXT: &str = "SettingsPanel";
/// Key context of the navigation while it has the keyboard.
const NAV_CONTEXT: &str = "SettingsNav";

/// The panel's size, fitted to smaller windows.
pub(crate) const PANEL_WIDTH: f32 = 1080.;
/// The panel's height, fitted to smaller windows.
const PANEL_HEIGHT: f32 = 760.;
/// The navigation's width.
const NAV_WIDTH: f32 = 248.;
/// A page narrower than this has a narrow header (see `render_header`).
const NARROW_PAGE: f32 = 720.;
/// The default reconcile interval offered when switching from adaptive.
const FIXED_RECONCILE_DEFAULT: u32 = 600;

/// Moves the keyboard to the navigation (`secondary-shift-e`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct FocusNavbar;

/// Moves the keyboard to the search field (`secondary-f`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct FocusSettingsSearch;

/// Escape in the panel: clears the search, or closes the panel.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct SettingsEscape;

/// The next category, in the navigation.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct NavNext;

/// The previous category, in the navigation.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct NavPrevious;

/// Back from the navigation to the page.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct NavOpen;

/// Registers the panel's keys.
pub(crate) fn bind_keys(cx: &mut App) {
    let panel = Some(SETTINGS_CONTEXT);
    let nav = Some(NAV_CONTEXT);
    let field = Some("SettingsPanel > Input");
    let control = Some(CONTROL_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("secondary-shift-e", FocusNavbar, panel),
        KeyBinding::new("secondary-f", FocusSettingsSearch, panel),
        KeyBinding::new("escape", SettingsEscape, panel),
        KeyBinding::new("down", NavNext, nav),
        KeyBinding::new("up", NavPrevious, nav),
        KeyBinding::new("enter", NavOpen, nav),
        KeyBinding::new("tab", NextField, panel),
        KeyBinding::new("shift-tab", PreviousField, panel),
        KeyBinding::new("tab", NextField, field),
        KeyBinding::new("shift-tab", PreviousField, field),
        KeyBinding::new("space", ControlActivate, control),
        KeyBinding::new("enter", ControlActivate, control),
        KeyBinding::new("right", ControlNext, control),
        KeyBinding::new("left", ControlPrevious, control),
    ]);
}

/// The settings shortcut as menus and the palette show it.
pub(crate) fn settings_key() -> &'static str {
    if cfg!(target_os = "macos") {
        "⌘,"
    } else {
        "ctrl-,"
    }
}

/// The quit shortcut as menus and the palette show it.
pub(crate) fn quit_key() -> &'static str {
    if cfg!(target_os = "macos") {
        "⌘Q"
    } else {
        "ctrl-q"
    }
}

/// The navigation shortcut as the panel shows it.
fn navbar_key() -> &'static str {
    if cfg!(target_os = "macos") {
        "⇧⌘E"
    } else {
        "ctrl-shift-e"
    }
}

/// What the panel asks its owner to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SettingsEvent {
    /// Close it.
    Close,
    /// *Start at login* changed to this (the owner writes the login entry
    /// off the UI thread).
    LaunchAtLogin(bool),
    /// Show the about dialog.
    About,
    /// Open the environment editor for the environment with this id.
    EditEnvironment(String),
    /// Open the environment editor for a new environment.
    AddEnvironment,
}

/// The panel's dropdowns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SettingsMenu {
    /// Whose notification rules the page shows.
    Environment,
    /// The log level.
    LogLevel,
    /// An environment's trouble alerts policy.
    TroublePolicy,
}

/// Where the settings' files and folders are (`None` where there is none:
/// the demo has no settings file).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Locations {
    /// The settings file.
    pub(crate) settings_file: Option<PathBuf>,
    /// The keymap file.
    pub(crate) keymap_file: Option<PathBuf>,
    /// The folder of the settings and keymap files.
    pub(crate) config_dir: Option<PathBuf>,
    /// The log folder.
    pub(crate) log_dir: Option<PathBuf>,
}

impl Locations {
    /// The running app's: the live app's paths, or in the demo the
    /// system's (for the keymap and the logs; no settings file).
    fn of_app(cx: &App) -> Self {
        let live = crate::live::session(cx).and_then(|session| session.read(cx).paths().cloned());
        match live {
            Some(paths) => Self {
                settings_file: Some(paths.config_file.clone()),
                keymap_file: Some(paths.keymap_file()),
                config_dir: Some(paths.config_dir()),
                log_dir: Some(paths.log_dir),
            },
            // The demo (and tests without a session) read the user's keymap
            // but have no settings file of their own.
            None if crate::live::session(cx).is_some() => {
                let system = ic_config::Paths::from_system().ok();
                Self {
                    settings_file: None,
                    keymap_file: system.as_ref().map(ic_config::Paths::keymap_file),
                    config_dir: system.as_ref().map(ic_config::Paths::config_dir),
                    log_dir: system.map(|paths| paths.log_dir),
                }
            }
            None => Self::default(),
        }
    }
}

/// The settings panel.
pub(crate) struct SettingsPanel {
    state: Entity<AppState>,
    page: SettingsPage,
    /// The environment whose notification rules the notifications page
    /// shows (the active one when the panel opened).
    environment: Option<String>,
    /// The environment whose own page shows (icinga › environments ›
    /// it), with its fields.
    drill: Option<environment::EnvironmentPage>,
    search: Entity<InputState>,
    /// The search, normalized (empty: no search).
    query: String,
    keymap_filter: Entity<InputState>,
    /// The keymap filter, normalized.
    keymap_query: String,
    inputs: BTreeMap<FieldId, Entity<InputState>>,
    /// What each field showed of its setting when it last showed it: a
    /// field whose setting didn't change since keeps what is typed in it.
    synced: BTreeMap<FieldId, String>,
    errors: BTreeMap<FieldId, String>,
    /// Where Tab stops, as drawn in the last frame.
    stops: RefCell<Vec<Stop>>,
    /// The controls' focus handles, by control.
    control_handles: RefCell<HashMap<SharedString, FocusHandle>>,
    /// Where each stop was drawn last.
    stop_bounds: Rc<RefCell<HashMap<SharedString, Bounds<Pixels>>>>,
    /// The control with the keyboard while it is in use (its ring).
    keyboard_on: RefCell<Option<SharedString>>,
    scroll: ScrollHandle,
    /// The sections drawn last, in order (the scroll handle's items).
    drawn_sections: Vec<Section>,
    menus: OpenMenu<SettingsMenu>,
    /// Whether this desktop shows tray icons (`None` while asking).
    tray_host: Option<bool>,
    /// What the log folder holds (`None` while reading).
    log_summary: Option<LogSummary>,
    locations: Locations,
    demo: bool,
    focus_handle: FocusHandle,
    nav_focus: FocusHandle,
    subscriptions: Vec<Subscription>,
    _tasks: Vec<Task<()>>,
}

impl EventEmitter<SettingsEvent> for SettingsPanel {}

impl Focusable for SettingsPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl SettingsPanel {
    /// The panel on `page`.
    #[expect(
        clippy::too_many_lines,
        reason = "the panel's fields, subscriptions and first reads, in order"
    )]
    pub(crate) fn new(
        state: Entity<AppState>,
        page: SettingsPage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (environment, demo) = {
            let state = state.read(cx);
            (
                state.active_environment_id().map(str::to_owned),
                state.is_demo(),
            )
        };
        let locations = Locations::of_app(cx);
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search settings"));
        let keymap_filter =
            cx.new(|cx| InputState::new(window, cx).placeholder("filter shortcuts by name or key"));
        let mut subscriptions = vec![
            cx.observe_in(&state, window, |this: &mut Self, _, window, cx| {
                this.follow_state(window, cx);
                cx.notify();
            }),
            cx.subscribe_in(
                &search,
                window,
                |this: &mut Self, search, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.query = normalized(&search.read(cx).value());
                        this.scroll.set_offset(gpui::point(px(0.), px(0.)));
                        cx.notify();
                    }
                },
            ),
        ];
        subscriptions.push(cx.subscribe_in(
            &keymap_filter,
            window,
            |this: &mut Self, filter, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.keymap_query = normalized(&filter.read(cx).value());
                    cx.notify();
                }
            },
        ));
        let mut tasks = Vec::new();
        let host_check = cx
            .background_executor()
            .spawn(async { ic_platform::tray::host_available() });
        tasks.push(cx.spawn(async move |this, cx| {
            let available = host_check.await;
            let _ = this.update(cx, |this, cx| {
                this.tray_host = Some(available);
                cx.notify();
            });
        }));
        if let Some(dir) = locations.log_dir.clone() {
            let read = cx
                .background_executor()
                .spawn(async move { LogSummary::of(&dir) });
            tasks.push(cx.spawn(async move |this, cx| {
                let summary = read.await;
                let _ = this.update(cx, |this, cx| {
                    this.log_summary = Some(summary);
                    cx.notify();
                });
            }));
        }
        let mut panel = Self {
            state,
            page,
            environment,
            drill: None,
            search,
            query: String::new(),
            keymap_filter,
            keymap_query: String::new(),
            inputs: BTreeMap::new(),
            synced: BTreeMap::new(),
            errors: BTreeMap::new(),
            stops: RefCell::new(Vec::new()),
            control_handles: RefCell::new(HashMap::new()),
            stop_bounds: Rc::new(RefCell::new(HashMap::new())),
            keyboard_on: RefCell::new(None),
            scroll: ScrollHandle::new(),
            drawn_sections: Vec::new(),
            menus: OpenMenu::default(),
            tray_host: None,
            log_summary: None,
            locations,
            demo,
            focus_handle: cx.focus_handle(),
            nav_focus: cx.focus_handle(),
            subscriptions,
            _tasks: tasks,
        };
        for id in [
            FieldId::Retention,
            FieldId::Reconcile,
            FieldId::QuietStart,
            FieldId::QuietEnd,
            FieldId::StormThreshold,
            FieldId::StormWindow,
            FieldId::MinDuration(ScopeKey::Environment),
        ] {
            panel.add_input(id, window, cx);
        }
        panel.load_fields(window, cx);
        panel
    }

    /// Where the keyboard goes when the panel opens: the search field.
    pub(crate) fn default_focus(&self, cx: &App) -> FocusHandle {
        self.search.focus_handle(cx)
    }

    /// The page shown.
    pub(crate) fn page(&self) -> SettingsPage {
        self.page
    }

    /// The search, normalized.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn query(&self) -> &str {
        &self.query
    }

    /// The environment whose notification rules the page shows.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn environment(&self) -> Option<&str> {
        self.environment.as_deref()
    }

    /// The problems shown, by field.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn errors(&self) -> &BTreeMap<FieldId, String> {
        &self.errors
    }

    /// A text field, for tests.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn input(&self, id: &FieldId) -> Option<&Entity<InputState>> {
        self.inputs.get(id)
    }

    /// Whether a dropdown is open.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn menu_open(&self) -> bool {
        self.menus.current().is_some()
    }

    /// Whether this desktop shows tray icons, once known.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn tray_host(&self) -> Option<bool> {
        self.tray_host
    }

    /// Sets where the files are, for tests.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn set_locations(&mut self, locations: Locations) {
        self.locations = locations;
    }

    /// What the search finds, by page, for tests.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn search_counts(&self, cx: &App) -> Vec<(SettingsPage, usize)> {
        let facts = self.facts(cx);
        SettingsPage::ALL
            .into_iter()
            .map(|page| (page, self.page_matches(page, &facts, cx).1))
            .collect()
    }

    // --- Navigation -------------------------------------------------------

    /// Shows `page` from its top, ending a search.
    pub(crate) fn show_page(
        &mut self,
        page: SettingsPage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.page = page;
        self.menus.close();
        if self.drill.is_some() {
            self.commit_trouble(cx);
            self.drill = None;
        }
        if !self.query.is_empty() {
            self.search
                .update(cx, |search, cx| search.set_value("", window, cx));
            self.query.clear();
        }
        self.scroll.set_offset(gpui::point(px(0.), px(0.)));
        cx.notify();
    }

    /// Scrolls to `section` of the page shown.
    fn show_section(&mut self, section: Section, cx: &mut Context<Self>) {
        // From an environment's page: back to the page's sections.
        self.leave_environment(cx);
        if let Some(index) = self
            .drawn_sections
            .iter()
            .position(|drawn| *drawn == section)
        {
            self.scroll.scroll_to_top_of_item(index);
            cx.notify();
        }
    }

    /// The section in view: the last one whose top has scrolled to the
    /// top of the page (the first before any has; the last once the page
    /// is scrolled to its end).
    fn section_in_view(&self) -> Option<Section> {
        let offset = self.scroll.offset().y;
        // The sections' bounds are laid out unscrolled.
        let top = self.scroll.bounds().top() - offset;
        let max = self.scroll.max_offset().y;
        if max > px(0.) && -offset >= max - px(1.) {
            return self.drawn_sections.last().copied();
        }
        let mut current = self.drawn_sections.first().copied();
        for (index, section) in self.drawn_sections.iter().enumerate() {
            match self.scroll.bounds_for_item(index) {
                Some(bounds) if bounds.top() <= top + px(24.) => current = Some(*section),
                _ => break,
            }
        }
        current
    }

    /// Opens the notifications page of the active environment with
    /// `key`'s custom rule (the sidebar's *custom rule*): the scope turns
    /// custom at once, starting from the rule it notified with.
    pub(crate) fn show_custom_rule(
        &mut self,
        key: &ScopeKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let active = self
            .state
            .read(cx)
            .active_environment_id()
            .map(str::to_owned);
        if active != self.environment
            && let Some(id) = active
        {
            self.commit_pending(window, cx);
            self.set_environment(&id, window, cx);
        }
        self.show_page(SettingsPage::Notifications, window, cx);
        self.choose_scope(key, 3, window, cx);
        self.show_section(Section::GroupsAndDashboards, cx);
    }

    fn on_focus_navbar(&mut self, _: &FocusNavbar, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.nav_focus, cx);
        cx.notify();
    }

    fn on_focus_search(
        &mut self,
        _: &FocusSettingsSearch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.search
            .update(cx, |search, cx| search.focus(window, cx));
    }

    fn on_nav_step(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let page = self.page.step(forward);
        self.show_page(page, window, cx);
    }

    fn on_nav_open(&mut self, _: &NavOpen, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_page(window, cx);
    }

    /// Escape: an open dropdown closes, the keymap filter or a search
    /// clears, else the panel closes.
    fn escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.menus.close() {
            cx.notify();
        } else if !self.keymap_query.is_empty() && self.page == SettingsPage::Keymap {
            self.keymap_filter
                .update(cx, |filter, cx| filter.set_value("", window, cx));
            self.keymap_query.clear();
            cx.notify();
        } else if !self.query.is_empty() {
            self.search
                .update(cx, |search, cx| search.set_value("", window, cx));
            self.query.clear();
            cx.notify();
        } else if self.drill.is_some() {
            // An environment's page goes back to the environments.
            self.leave_environment(cx);
            self.show_section(Section::Environments, cx);
        } else {
            self.commit_pending(window, cx);
            cx.emit(SettingsEvent::Close);
        }
    }

    fn on_escape(&mut self, _: &SettingsEscape, window: &mut Window, cx: &mut Context<Self>) {
        self.escape(window, cx);
    }

    fn on_field_escape(&mut self, _: &Escape, window: &mut Window, cx: &mut Context<Self>) {
        self.escape(window, cx);
    }

    /// Applies every field whose text differs from the setting (the panel
    /// closes while one is being edited); a bad value shows its problem
    /// and isn't applied.
    pub(crate) fn commit_pending(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pending: Vec<FieldId> = self
            .inputs
            .keys()
            .filter(|id| self.text(id, cx) != self.stored_text(id, cx))
            .cloned()
            .collect();
        for id in pending {
            self.commit(&id, window, cx);
        }
        self.commit_trouble(cx);
    }

    fn on_next_field(&mut self, _: &NextField, window: &mut Window, cx: &mut Context<Self>) {
        self.move_focus(true, window, cx);
    }

    fn on_previous_field(
        &mut self,
        _: &PreviousField,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_focus(false, window, cx);
    }

    // --- Applying changes ---------------------------------------------------

    /// Changes the app-wide settings at once.
    fn change_general(&self, cx: &mut Context<Self>, change: impl FnOnce(&mut General)) {
        let mut general = self.state.read(cx).config().general.clone();
        let login = general.launch_at_login;
        change(&mut general);
        let login_changed = general.launch_at_login != login;
        self.state.update(cx, |state, cx| {
            if state.set_general(&general) {
                cx.notify();
            }
        });
        if login_changed {
            cx.emit(SettingsEvent::LaunchAtLogin(general.launch_at_login));
        }
    }

    /// Changes the appearance at once.
    fn change_appearance(&self, cx: &mut Context<Self>, change: impl FnOnce(&mut Appearance)) {
        let mut appearance = *self.state.read(cx).appearance();
        change(&mut appearance);
        self.state.update(cx, |state, cx| {
            if state.set_appearance(appearance) {
                cx.notify();
            }
        });
    }

    /// Changes the page's environment's notification settings at once.
    fn change_plan(&self, cx: &mut Context<Self>, change: impl FnOnce(&mut NotificationPlan)) {
        let Some(id) = self.environment.clone() else {
            return;
        };
        self.state.update(cx, |state, cx| {
            if state.change_notifications(&id, change) {
                cx.notify();
            }
        });
    }

    /// The page's environment's notification settings now.
    fn plan(&self, cx: &App) -> Option<NotificationPlan> {
        self.state
            .read(cx)
            .notification_plan_of(self.environment.as_deref()?)
    }

    /// The environment dropdown (or ← → on it) chose `id`: what is typed
    /// for the page's environment applies to it first; a value that
    /// doesn't read keeps its problem shown and the page where it is.
    pub(crate) fn choose_environment(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.commit_pending(window, cx);
        if !self.errors.is_empty() {
            self.menus.close();
            cx.notify();
            return;
        }
        self.set_environment(id, window, cx);
    }

    /// Shows the notification rules of environment `id`.
    pub(crate) fn set_environment(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.environment.as_deref() == Some(id) {
            return;
        }
        self.environment = Some(id.to_owned());
        self.menus.close();
        // Another environment's scopes: their fields start over.
        self.inputs.retain(
            |field, _| !matches!(field, FieldId::MinDuration(key) if *key != ScopeKey::Environment),
        );
        self.synced.retain(
            |field, _| !matches!(field, FieldId::MinDuration(key) if *key != ScopeKey::Environment),
        );
        self.errors.clear();
        self.load_fields(window, cx);
        cx.notify();
    }

    /// The settings changed elsewhere (the settings file taken over, an
    /// environment deleted): a field whose setting changed shows it again
    /// unless it is being typed in (a field holding a value not yet
    /// applied keeps it), and the notifications page moves to the
    /// environment on screen when its own is gone.
    fn follow_state(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .drill
            .as_ref()
            .is_some_and(|page| self.state.read(cx).environment_by_id(&page.id).is_none())
        {
            self.drill = None;
        }
        let gone = self
            .environment
            .as_deref()
            .is_some_and(|id| self.state.read(cx).environment_by_id(id).is_none());
        if gone || self.environment.is_none() {
            let active = self
                .state
                .read(cx)
                .active_environment_id()
                .map(str::to_owned);
            if let Some(id) = active {
                self.set_environment(&id, window, cx);
            } else {
                self.environment = None;
            }
        }
        let stale: Vec<FieldId> = self
            .inputs
            .iter()
            .filter(|(id, input)| {
                !self.errors.contains_key(*id) && !input.focus_handle(cx).is_focused(window)
            })
            .filter(|(id, _)| self.synced.get(*id) != Some(&self.stored_text(id, cx)))
            .map(|(id, _)| id.clone())
            .collect();
        for id in stale {
            self.show_stored(&id, window, cx);
        }
    }

    /// Turns a rule condition of `key`'s rule on or off.
    pub(crate) fn set_flag(
        &mut self,
        key: &ScopeKey,
        flag: RuleFlag,
        on: bool,
        cx: &mut Context<Self>,
    ) {
        self.change_plan(cx, |plan| {
            if let Some(rule) = plan.rule_mut(key) {
                flag.set(rule, on);
            }
        });
    }

    /// Chooses a group's or dashboard's setting (inherit, on, off,
    /// custom); a custom rule's fields appear under it.
    pub(crate) fn choose_scope(
        &mut self,
        key: &ScopeKey,
        choice: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = key.clone();
        self.change_plan(cx, |plan| {
            plan.choose(&key, choice);
        });
        if choice == 3 {
            self.ensure_rule_input(&key, window, cx);
        }
        cx.notify();
    }

    /// Turns quiet hours on or off.
    pub(crate) fn set_quiet_hours(&mut self, on: bool, cx: &mut Context<Self>) {
        self.change_plan(cx, |plan| plan.settings.quiet_hours.enabled = on);
    }

    // --- Text fields ----------------------------------------------------------

    /// Adds a text field that applies on Enter and when it loses the
    /// keyboard.
    fn add_input(&mut self, id: FieldId, window: &mut Window, cx: &mut Context<Self>) {
        let placeholder = match &id {
            FieldId::Retention => "48",
            FieldId::Reconcile => "600",
            FieldId::MinDuration(_) => "0",
            FieldId::QuietStart => "22:00",
            FieldId::QuietEnd => "07:00",
            FieldId::StormThreshold => "5",
            FieldId::StormWindow => "10",
        };
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        let field = id.clone();
        self.subscriptions.push(cx.subscribe_in(
            &input,
            window,
            move |this: &mut Self, _, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { .. } | InputEvent::Blur => {
                    this.commit(&field, window, cx);
                }
                InputEvent::Change => {
                    // A problem shown updates as the value is fixed; the
                    // value applies on Enter or when leaving the field.
                    if this.errors.contains_key(&field) {
                        match this.parse(&field, cx) {
                            Ok(_) => {
                                this.errors.remove(&field);
                            }
                            Err(error) => {
                                this.errors.insert(field.clone(), error);
                            }
                        }
                        cx.notify();
                    }
                }
                InputEvent::Focus => {}
            },
        ));
        self.inputs.insert(id, input);
    }

    /// Adds the minimum-duration field of a scope's rule, unless it has one.
    fn ensure_rule_input(&mut self, key: &ScopeKey, window: &mut Window, cx: &mut Context<Self>) {
        let id = FieldId::MinDuration(key.clone());
        if self.inputs.contains_key(&id) {
            return;
        }
        self.add_input(id.clone(), window, cx);
        self.show_stored(&id, window, cx);
    }

    /// Adds the fields of the custom rules shown (before drawing them).
    fn ensure_rule_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(plan) = self.plan(cx) else {
            return;
        };
        for key in plan.rule_scopes() {
            self.ensure_rule_input(&key, window, cx);
        }
    }

    /// The text of a field.
    fn text(&self, id: &FieldId, cx: &App) -> String {
        self.inputs
            .get(id)
            .map(|input| input.read(cx).value().to_string())
            .unwrap_or_default()
    }

    /// Sets a field's text.
    fn set_text(&self, id: &FieldId, text: String, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(input) = self.inputs.get(id) {
            input.update(cx, |input, cx| input.set_value(text, window, cx));
        }
    }

    /// Shows the stored setting in a field.
    fn show_stored(&mut self, id: &FieldId, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.stored_text(id, cx);
        if self.text(id, cx) != text {
            self.set_text(id, text.clone(), window, cx);
        }
        self.synced.insert(id.clone(), text);
    }

    /// What a field shows for the stored setting.
    fn stored_text(&self, id: &FieldId, cx: &App) -> String {
        let state = self.state.read(cx);
        let general = &state.config().general;
        let plan = self.plan(cx);
        match id {
            FieldId::Retention => general.event_log_retention_hours.to_string(),
            FieldId::Reconcile => match general.reconcile_interval_secs {
                0 => FIXED_RECONCILE_DEFAULT.to_string(),
                seconds => seconds.to_string(),
            },
            FieldId::MinDuration(key) => format_min_duration(
                plan.as_ref()
                    .and_then(|plan| plan.rule(key))
                    .map_or(0, |rule| rule.min_duration_secs),
            ),
            FieldId::QuietStart => plan
                .map(|plan| format_clock(plan.settings.quiet_hours.start_minute))
                .unwrap_or_default(),
            FieldId::QuietEnd => plan
                .map(|plan| format_clock(plan.settings.quiet_hours.end_minute))
                .unwrap_or_default(),
            FieldId::StormThreshold => plan
                .map(|plan| plan.settings.storm.threshold.to_string())
                .unwrap_or_default(),
            FieldId::StormWindow => plan
                .map(|plan| plan.settings.storm.window_secs.to_string())
                .unwrap_or_default(),
        }
    }

    /// Fills every field from the stored settings.
    fn load_fields(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ids: Vec<FieldId> = self.inputs.keys().cloned().collect();
        for id in ids {
            self.show_stored(&id, window, cx);
        }
    }

    /// Reads a field.
    fn parse(&self, id: &FieldId, cx: &App) -> Result<u32, String> {
        let text = self.text(id, cx);
        match id {
            FieldId::Retention => parse_retention(&text),
            FieldId::Reconcile => parse_reconcile(&text),
            FieldId::MinDuration(_) => parse_min_duration(&text),
            FieldId::QuietStart | FieldId::QuietEnd => parse_clock(&text).map(u32::from),
            FieldId::StormThreshold => parse_threshold(&text),
            FieldId::StormWindow => parse_window(&text),
        }
    }

    /// Applies a field (Enter, or the keyboard left it): its value, or
    /// its problem under the row.
    pub(crate) fn commit(&mut self, id: &FieldId, window: &mut Window, cx: &mut Context<Self>) {
        let value = match self.parse(id, cx) {
            Ok(value) => value,
            Err(error) => {
                self.errors.insert(id.clone(), error);
                cx.notify();
                return;
            }
        };
        self.errors.remove(id);
        match id {
            FieldId::Retention => self.change_general(cx, |general| {
                general.event_log_retention_hours = value;
            }),
            FieldId::Reconcile => self.change_general(cx, |general| {
                general.reconcile_interval_secs = value;
            }),
            FieldId::MinDuration(key) => self.change_plan(cx, |plan| {
                if let Some(rule) = plan.rule_mut(key) {
                    rule.min_duration_secs = value;
                }
            }),
            FieldId::QuietStart | FieldId::QuietEnd => {
                let minute = u16::try_from(value).unwrap_or_default();
                let start = matches!(id, FieldId::QuietStart);
                self.change_plan(cx, |plan| {
                    let quiet = &mut plan.settings.quiet_hours;
                    if start {
                        quiet.start_minute = minute;
                    } else {
                        quiet.end_minute = minute;
                    }
                });
            }
            FieldId::StormThreshold => {
                self.change_plan(cx, |plan| plan.settings.storm.threshold = value);
            }
            FieldId::StormWindow => {
                self.change_plan(cx, |plan| plan.settings.storm.window_secs = value);
            }
        }
        // The field shows the value as stored (`5m` for `300`).
        self.show_stored(id, window, cx);
        cx.notify();
    }

    // --- Files ------------------------------------------------------------------

    /// *Edit in settings file*: writes the settings first if there is no
    /// file yet, then opens it in the default editor.
    fn edit_settings_file(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.locations.settings_file.clone() else {
            return;
        };
        if !path.exists() {
            // Nothing written yet (a first start): the editor opens the
            // settings as they are.
            let config = self.state.read(cx).config().clone();
            if let Err(error) = ic_config::ConfigStore::new(path.clone()).save(&config) {
                self.state.update(cx, |state, cx| {
                    state.inform(
                        "The settings file couldn't be written",
                        Some(error.to_string()),
                    );
                    cx.notify();
                });
                return;
            }
            // What the file holds now, so only an edit counts as one.
            self.state.read(cx).settings_read(&config);
        }
        files::open(&path, cx);
    }

    /// *Edit keymap file*: creates it with a commented template if there
    /// is none, then opens it in the default editor. Changes apply when
    /// the window comes back to the front.
    fn edit_keymap_file(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.locations.keymap_file.clone() else {
            return;
        };
        if let Err(error) = crate::keymap::ensure_file(&path) {
            self.state.update(cx, |state, cx| {
                state.inform(
                    "The keymap file couldn't be created",
                    Some(error.to_string()),
                );
                cx.notify();
            });
            return;
        }
        files::open(&path, cx);
    }

    // --- Rendering --------------------------------------------------------------

    /// What the rows read from the state, gathered once per frame.
    fn facts(&self, cx: &App) -> pages::Facts {
        pages::Facts::read(self, cx)
    }

    /// What a search finds on `page`: its static settings and how many
    /// rows match, data rows included.
    fn page_matches(
        &self,
        page: SettingsPage,
        facts: &pages::Facts,
        cx: &App,
    ) -> (PageMatches, usize) {
        if self.query.is_empty() {
            return (PageMatches::default(), 0);
        }
        let settings: Vec<(Setting, bool)> =
            matching_settings(&self.query, |setting| self.row_text(setting, facts))
                .into_iter()
                .filter(|(setting, _)| setting.section().page() == page)
                .filter(|(setting, _)| Self::is_shown(*setting, facts))
                .collect();
        let data = self.data_matches(page, facts, cx);
        let count = settings.len() + data;
        (PageMatches { settings }, count)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the navigation's three parts, each a few builder calls"
    )]
    fn render_nav(
        &self,
        theme: &Theme,
        facts: &pages::Facts,
        nav_focused: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = theme.colors;
        let searching = !self.query.is_empty();
        let in_view = self.section_in_view();
        let mut list = div().flex().flex_col().py(px(6.));
        for page in SettingsPage::ALL {
            let count = if searching {
                self.page_matches(page, facts, cx).1
            } else {
                0
            };
            let active = !searching && page == self.page;
            let dim = searching && count == 0;
            let text_color = if active {
                colors.text_emphasis
            } else if dim {
                colors.text_faint
            } else {
                colors.text_secondary
            };
            let icon_color = if active {
                colors.text_secondary
            } else if dim {
                colors.text_faint
            } else {
                colors.text_muted
            };
            list = list.child(
                div()
                    .id(SharedString::from(format!("settings-nav-{}", page.label())))
                    .relative()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(12.))
                    .h(px(30.))
                    .pl(px(14.))
                    .pr(px(12.))
                    .text_size(theme.text.row)
                    .text_color(text_color)
                    .when(active, |item| item.bg(colors.item_active))
                    .when(!active, |item| {
                        item.hover(|style| style.bg(colors.item_hover))
                    })
                    .cursor_pointer()
                    .child(Icon::new(page.icon()).size(px(14.)).color(icon_color))
                    .child(div().flex_1().min_w_0().truncate().child(page.label()))
                    .when(searching && count > 0, |item| {
                        item.child(
                            div()
                                .flex_none()
                                .text_size(theme.text.label)
                                .text_color(colors.text_muted)
                                .child(count.to_string()),
                        )
                    })
                    // While the navigation has the keyboard, the open
                    // category says so with the focus ring.
                    .when(active && nav_focused, |item| {
                        item.child(
                            div()
                                .absolute()
                                .top_0()
                                .left_0()
                                .size_full()
                                .border_1()
                                .border_color(colors.accent),
                        )
                    })
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.show_page(page, window, cx);
                    })),
            );
            if active && page != SettingsPage::Keymap {
                for section in page.sections() {
                    let section = *section;
                    let on = in_view == Some(section);
                    list = list.child(
                        div()
                            .id(SharedString::from(format!(
                                "settings-nav-section-{}",
                                section.label()
                            )))
                            .flex()
                            .flex_none()
                            .items_center()
                            .gap(px(12.))
                            .h(px(26.))
                            .pl(px(14.))
                            .pr(px(12.))
                            .text_size(theme.text.body)
                            .text_color(if on {
                                colors.text_strong
                            } else {
                                colors.text_muted
                            })
                            .cursor_pointer()
                            .hover(|style| style.text_color(colors.text))
                            .child(
                                div().relative().flex_none().w(px(14.)).h(px(26.)).child(
                                    div()
                                        .absolute()
                                        .left(px(6.5))
                                        .top_0()
                                        .bottom_0()
                                        .w(px(1.))
                                        .bg(if on {
                                            colors.accent
                                        } else {
                                            colors.border_window
                                        }),
                                ),
                            )
                            .child(div().min_w_0().truncate().child(section.label()))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.show_section(section, cx);
                            })),
                    );
                }
            }
        }
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(NAV_WIDTH))
            .h_full()
            // The card clips square: the corners its fill touches are
            // rounded like the card (inside its border).
            .rounded_l(theme.metrics.modal_radius - px(1.))
            .bg(colors.sidebar_background)
            .border_r_1()
            .border_color(colors.border_split)
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(8.))
                    .h(Metrics::with_rule(theme.metrics.header_height))
                    .px(px(12.))
                    .border_b_1()
                    .border_color(colors.border_header)
                    .child(
                        Icon::new(IconName::Search)
                            .size(px(13.))
                            .color(colors.text_muted),
                    )
                    .child(div().flex_1().min_w_0().child(TextField::new(&self.search)))
                    .when(searching, |bar| {
                        bar.child(
                            IconButton::new("settings-search-clear", IconName::Close)
                                .icon_size(px(12.))
                                .color(colors.text_faint)
                                .tooltip(Tooltip::new("Clear the search").key("esc"))
                                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.search
                                        .update(cx, |search, cx| search.set_value("", window, cx));
                                    this.query.clear();
                                    cx.notify();
                                })),
                        )
                    }),
            )
            .child(
                div()
                    .id("settings-nav")
                    .key_context(NAV_CONTEXT)
                    .track_focus(&self.nav_focus)
                    .on_action(cx.listener(|this, _: &NavNext, window, cx| {
                        this.on_nav_step(true, window, cx);
                    }))
                    .on_action(cx.listener(|this, _: &NavPrevious, window, cx| {
                        this.on_nav_step(false, window, cx);
                    }))
                    .on_action(cx.listener(Self::on_nav_open))
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(list),
            )
            .child(
                div()
                    .id("settings-focus-navbar")
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(8.))
                    .h(Metrics::with_rule(theme.metrics.footer_height))
                    .px(px(14.))
                    .border_t_1()
                    .border_color(colors.border_header)
                    .text_size(theme.text.small)
                    .text_color(colors.text_muted)
                    .cursor_pointer()
                    .child("focus navbar")
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_size(theme.text.hint)
                            .text_color(colors.text_faint)
                            .child(navbar_key()),
                    )
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        window.focus(&this.nav_focus, cx);
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    /// The page's header. `narrow`: the page is too narrow for every
    /// word of it, and *edit in settings file* shows as its icon (its
    /// words in the tooltip), so the close button always fits.
    #[expect(
        clippy::too_many_lines,
        reason = "the header's parts: title, status, edit button and close, each a few builder calls"
    )]
    fn render_header(
        &self,
        theme: &Theme,
        facts: &pages::Facts,
        narrow: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = theme.colors;
        let searching = !self.query.is_empty();
        let (title, subtitle) = if searching {
            let total: usize = SettingsPage::ALL
                .into_iter()
                .map(|page| self.page_matches(page, facts, cx).1)
                .sum();
            let noun = if total == 1 { "setting" } else { "settings" };
            (
                format!("{total} {noun}"),
                format!("match “{}”", self.search.read(cx).value().trim()),
            )
        } else if let Some(name) = self.environment_title(cx) {
            (name, "environment · on this computer".to_owned())
        } else {
            (self.page.label().to_owned(), self.subtitle(facts))
        };
        let keymap = !searching && self.page == SettingsPage::Keymap;
        let save_error = self.state.read(cx).save_error().map(str::to_owned);
        let file_error = self.state.read(cx).file_error().map(str::to_owned);
        let status: AnyElement = if self.demo && !keymap {
            div()
                .id("settings-demo-status")
                .flex_none()
                .text_size(theme.text.small)
                .text_color(colors.text_faint)
                .child("demo · not saved")
                .tooltip(Tooltip::new("The demo saves nothing").builder())
                .into_any_element()
        } else if let Some(error) = file_error.filter(|_| !keymap) {
            div()
                .id("settings-file-error")
                .flex()
                .flex_none()
                .items_center()
                .gap(px(6.))
                .text_size(theme.text.small)
                .text_color(theme.states.text.critical)
                .child(Icon::new(IconName::TriangleAlert).size(px(12.)))
                .child("file has errors")
                .tooltip(
                    Tooltip::new(format!(
                        "{error}. Nothing is written over the file until it reads again."
                    ))
                    .builder(),
                )
                .into_any_element()
        } else if let Some(error) = save_error {
            div()
                .id("settings-not-saved")
                .flex()
                .items_center()
                .gap(px(6.))
                .text_size(theme.text.small)
                .text_color(theme.states.text.critical)
                .child(Icon::new(IconName::TriangleAlert).size(px(12.)))
                .child("not saved")
                .tooltip(Tooltip::new(error).builder())
                .into_any_element()
        } else {
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .text_size(theme.text.small)
                .text_color(colors.text_faint)
                .child(Icon::new(IconName::Check).size(px(12.)))
                .child("saved")
                .into_any_element()
        };
        let (id, label, tooltip, disabled, open): (_, _, _, _, keyboard::Activate) = if keymap {
            (
                "settings-edit-keymap",
                "edit keymap file",
                "Opens keymap.toml in your editor; changes apply when you come back",
                self.locations.keymap_file.is_none(),
                Rc::new(|this, _, cx| this.edit_keymap_file(cx)),
            )
        } else {
            (
                "settings-edit-file",
                "edit in settings file",
                if self.demo {
                    "The demo has no settings file"
                } else {
                    "Opens config.toml in your editor"
                },
                self.locations.settings_file.is_none(),
                Rc::new(|this, _, cx| this.edit_settings_file(cx)),
            )
        };
        let edit = if narrow {
            let click = open.clone();
            let button = IconButton::new(id, IconName::FileCode)
                .color(colors.text_muted)
                .disabled(disabled)
                .tooltip(Tooltip::new(format!("{label}: {tooltip}")))
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    click(this, window, cx);
                }));
            if disabled {
                button.into_any_element()
            } else {
                self.focusable_in(id, button, open, None, Region::Header, theme, cx)
            }
        } else {
            self.button(
                id,
                disabled,
                Button::new(id, label)
                    .icon(IconName::FileCode)
                    .disabled(disabled)
                    .tooltip(Tooltip::new(tooltip)),
                open,
                Region::Header,
                theme,
                cx,
            )
        };
        let close: keyboard::Activate = Rc::new(|this, window, cx| {
            this.commit_pending(window, cx);
            cx.emit(SettingsEvent::Close);
        });
        let click = close.clone();
        let close = self.focusable_in(
            "settings-close",
            IconButton::new("settings-close", IconName::Close)
                .color(colors.text_muted)
                .tooltip(Tooltip::new("Close").key("esc"))
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    click(this, window, cx);
                })),
            close,
            None,
            Region::Header,
            theme,
            cx,
        );
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(12.))
            .h(Metrics::with_rule(theme.metrics.header_height))
            .pl(px(32.))
            .pr(px(12.))
            .border_b_1()
            .border_color(colors.border_header)
            .whitespace_nowrap()
            .child(rows::title(title, theme))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(theme.text.small)
                    .text_color(colors.text_faint)
                    .child(subtitle),
            )
            .child(status)
            .child(edit)
            .child(close)
            .into_any_element()
    }

    /// The faint words after the page's name: whom it is for.
    fn subtitle(&self, facts: &pages::Facts) -> String {
        match self.page {
            SettingsPage::Notifications => match &facts.environment_name {
                Some(name) => format!("for {name} · on this computer only"),
                None => "on this computer only".to_owned(),
            },
            SettingsPage::Keymap => {
                let count = facts.shortcut_count;
                let noun = if count == 1 { "shortcut" } else { "shortcuts" };
                match facts.keymap_problems.len() {
                    0 => format!("keymap.toml · {count} {noun}"),
                    problems => format!("keymap.toml · {count} {noun} · {problems} skipped"),
                }
            }
            _ => "for icygui on this computer".to_owned(),
        }
    }
}

impl Render for SettingsPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.page == SettingsPage::Notifications || !self.query.is_empty() {
            self.ensure_rule_inputs(window, cx);
        }
        let theme = cx.theme().clone();
        let facts = self.facts(cx);
        let viewport = window.viewport_size();
        // Where Tab stops is recorded as the panel draws.
        self.stops.borrow_mut().clear();
        self.add_stop(
            "settings-search".into(),
            self.search.focus_handle(cx),
            Region::Search,
        );
        self.note_keyboard_focus(window);
        let nav_focused = self.nav_focus.is_focused(window) && window.last_input_was_keyboard();
        let height = px(PANEL_HEIGHT)
            .min(viewport.height - px(64.))
            .max(px(320.));
        let (blocks, sections) = self.render_content(&theme, &facts, cx);
        self.drawn_sections = sections;
        let nav = self.render_nav(&theme, &facts, nav_focused, cx);
        // The card's width as the modal sizes it, less the navigation.
        let page_width = px(PANEL_WIDTH).min(viewport.width - px(32.)) - px(NAV_WIDTH);
        let header = self.render_header(&theme, &facts, page_width < px(NARROW_PAGE), cx);
        let scroll = self.scroll.clone();
        div()
            .id("settings-panel")
            .key_context(SETTINGS_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::on_focus_navbar))
            .on_action(cx.listener(Self::on_focus_search))
            .on_action(cx.listener(Self::on_escape))
            .on_action(cx.listener(Self::on_field_escape))
            .on_action(cx.listener(Self::on_next_field))
            .on_action(cx.listener(Self::on_previous_field))
            .flex()
            .w_full()
            .max_w(px(PANEL_WIDTH))
            .h(height)
            .rounded(theme.metrics.modal_radius - px(1.))
            .bg(theme.colors.window_background)
            .child(nav)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(header)
                    .child(
                        div()
                            .relative()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_h_0()
                            .child(
                                div()
                                    .id("settings-content")
                                    .flex()
                                    .flex_col()
                                    .size_full()
                                    .px(px(32.))
                                    .pt(px(8.))
                                    .pb(px(24.))
                                    .overflow_y_scroll()
                                    .track_scroll(&scroll)
                                    .on_scroll_wheel(cx.listener(|_, _, _, cx| cx.notify()))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _, _, cx| {
                                            if this.menus.close() {
                                                cx.notify();
                                            }
                                        }),
                                    )
                                    .children(blocks),
                            )
                            .child(Scrollbar::vertical(&scroll)),
                    ),
            )
    }
}
