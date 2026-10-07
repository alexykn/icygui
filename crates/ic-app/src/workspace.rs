//! The root view: the sidebar and the main area, which shows the selected
//! dashboard (list and detail pane, screens 2a–2c), an object opened as a
//! tab, the dashboard editor (DASH-04), or on the first run without
//! environments the onboarding form (ENV-08). Over everything, at most
//! one modal: the command palette (UI-03), the environment editor, the
//! certificate review (ENV-05), an action dialog (ACT-02..06), a
//! confirmation, or a file path; and in the bottom-right corner the
//! toasts that report actions (ACT-07).
//!
//! The sidebar, the dashboard header, the editors and the palette say
//! what they want through events; the workspace opens the dialogs and
//! carries out what was chosen (with `crate::live::Session` for anything
//! that touches engines, the keychain or files). Actions asked for
//! anywhere (keys, buttons, the palette) arrive through
//! `AppState::take_request`: the workspace opens their dialog, asks
//! first, or sends them (`crate::operate`).
//!
//! The keyboard belongs to the main area: a click on a spot that takes no
//! focus of its own (the sidebar, its footer, a header) hands the focus to
//! the list or the tab shown, and so does losing the focus, so the
//! shortcuts keep working. While a modal is open, it has the keyboard.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gpui::{
    Action, AnyElement, App, AppContext as _, ClickEvent, ClipboardItem, Context, Entity,
    FocusHandle, Focusable as _, InteractiveElement as _, IntoElement, KeyBinding, MouseDownEvent,
    ParentElement as _, PathPromptOptions, Render, SharedString, Styled as _, Subscription, Task,
    Window, div, prelude::FluentBuilder as _,
};
use ic_core::snapshot::Snapshot;
use ic_model::{ObjectKey, Timestamp};
use ic_rules::DashboardRef;
use ic_ui_kit::input::{Escape, InputEvent, InputState};
use ic_ui_kit::{
    ActiveTheme as _, Button, ButtonVariant, DialogBody, Divider, DividerColor, Field, IconButton,
    IconName, Modal, ModalPlacement, Root, TextField, Theme, Tooltip, px,
};

use crate::actions::{
    self, ActivateNextTab, ActivatePreviousTab, CloseTab, EditEnvironment, FocusMain,
    OpenNotifications, OpenSettings, ReviewCertificate, SelectDashboard, ShowAbout,
    WORKSPACE_CONTEXT,
};
use crate::app_state::editing::DashboardDraft;
use crate::app_state::{AppState, UserNotice};
use crate::appearance;
use crate::background::presence;
use crate::chrome::{self, Controls, WindowControls, WindowDrag};
use crate::dashboard::{DashboardEvent, DashboardView};
use crate::editor::{DashboardEditor, EditorEvent, EditorTarget};
use crate::environments::{
    CertificateEvent, CertificateReview, EditorMode, EnvironmentEditor, EnvironmentEditorEvent,
};
use crate::lists::dialog::RemovalDialog;
use crate::lists::{ListKind, RecordList, RecordListEvent};
use crate::operate::dialog::{ActionDialog, DialogEvent, DialogKind};
use crate::operate::forms::{self, describe_objects};
use crate::operate::{ActionSpec, CHECK_CONFIRM_ABOVE};
use crate::palette::{CommandPalette, Focus, PaletteCommand, PaletteEvent};
use crate::pane::{ObjectPane, PaneMode};
use crate::settings::{ScopeKey, SettingsEvent, SettingsPage, SettingsPanel};
use crate::sidebar::{Sidebar, SidebarEvent};
use crate::{live, recovery, window_state};

/// How often relative times (time in state, the footer's last event, the
/// reconnect countdown) refresh (UI-04). Only the rows on screen are
/// rebuilt, so this costs the same for 30 000 rows as for 10.
const CLOCK_TICK: Duration = Duration::from_secs(1);

/// The window's size and position are saved this long after it last
/// moved or changed size (BG-06).
const WINDOW_SAVE_DELAY: Duration = Duration::from_millis(750);

/// Key context of the modal layer.
pub(crate) const MODAL_CONTEXT: &str = "Modal";
/// Key context of a confirmation dialog.
const CONFIRM_CONTEXT: &str = "ConfirmDialog";

/// The name of a group created for a first dashboard.
const FIRST_GROUP_NAME: &str = "dashboards";

/// Shows or hides the sidebar.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct ToggleSidebar;

/// Opens (or closes) the command palette.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct ToggleCommandPalette;

/// Creates a dashboard in the selected dashboard's group (the editor
/// opens).
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct NewDashboard;

/// Closes the open modal.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct CloseModal;

/// Confirms the open confirmation dialog.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct ConfirmModal;

/// Registers the workspace's key bindings (`secondary` is cmd on macOS and
/// ctrl elsewhere).
pub(crate) fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("secondary-b", ToggleSidebar, None),
        KeyBinding::new("secondary-k", ToggleCommandPalette, Some(WORKSPACE_CONTEXT)),
        KeyBinding::new("secondary-n", NewDashboard, Some(WORKSPACE_CONTEXT)),
        KeyBinding::new("escape", CloseModal, Some(MODAL_CONTEXT)),
        KeyBinding::new("enter", ConfirmModal, Some(CONFIRM_CONTEXT)),
    ]);
    actions::bind_keys(cx);
    crate::editor::bind_keys(cx);
    crate::palette::bind_keys(cx);
    crate::operate::dialog::bind_keys(cx);
    crate::settings::bind_keys(cx);
    crate::lists::view::bind_keys(cx);
    crate::lists::dialog::bind_keys(cx);
}

/// What the main area shows.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Shown {
    Dashboard(Option<DashboardRef>),
    Tab(ObjectKey),
    List(ListKind),
}

/// An object open as a tab.
struct TabPane {
    view: Entity<ObjectPane>,
}

/// The dashboard editor in the main area, and what was shown when it
/// opened (showing something else closes it).
struct OpenEditor {
    view: Entity<DashboardEditor>,
    opened_over: Shown,
    _events: Subscription,
}

/// What a confirmation carries out.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Confirmed {
    /// Delete a group and its dashboards.
    Group(String),
    /// Delete a dashboard.
    Dashboard(DashboardRef),
    /// Delete an environment, its password and its event log.
    Environment(String),
    /// Send an action (many checks, bulk removals).
    Action(ActionSpec),
    /// Leave the dashboard editor, dropping its changes.
    DiscardEdits,
    /// Leave the environment editor, dropping its changes.
    DiscardEnvironmentEdits,
    /// Close the window, dropping unsaved work in it.
    CloseWindow,
}

/// A question before something that can't be undone, or that weighs on
/// Icinga.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Confirmation {
    /// The question.
    pub(crate) title: String,
    /// What it means.
    pub(crate) detail: String,
    /// The confirming button's label.
    pub(crate) confirm: &'static str,
    /// Whether confirming destroys something (a danger-styled button).
    pub(crate) danger: bool,
    /// What happens on confirming.
    pub(crate) action: Confirmed,
}

/// Which file a path dialog is for.
#[derive(Clone, Debug, PartialEq, Eq)]
enum PathPurpose {
    /// Write this export there.
    Export(String),
    /// Import groups from there.
    Import,
}

/// A path typed by hand where no file chooser is available.
struct PathPrompt {
    purpose: PathPurpose,
    input: Entity<InputState>,
    error: Option<String>,
    _events: Subscription,
}

/// A modal's view and the subscription to its events.
struct Held<T> {
    view: Entity<T>,
    _events: Subscription,
}

impl<T> Held<T> {
    fn new(view: Entity<T>, events: Subscription) -> Self {
        Self {
            view,
            _events: events,
        }
    }
}

/// The open modal.
enum OpenModal {
    Palette(Held<CommandPalette>),
    Environment(Held<EnvironmentEditor>),
    Certificate(Held<CertificateReview>),
    Action(Held<ActionDialog>),
    Confirm(Confirmation),
    Path(PathPrompt),
    About,
    /// Removing from a list (topic 07), listing every target.
    Removal(Held<RemovalDialog>),
}

/// Which modal is open, for tests.
#[cfg(all(test, target_os = "linux"))]
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ModalKind {
    /// The command palette.
    Palette,
    /// The environment editor.
    Environment,
    /// The certificate review.
    Certificate,
    /// An action dialog.
    Action(DialogKind),
    /// A confirmation.
    Confirm(Box<Confirmation>),
    /// A file path.
    Path,
    /// The about dialog.
    About,
    /// Removing from a list.
    Removal(ListKind),
}

/// The window's content.
pub(crate) struct Workspace {
    state: Entity<AppState>,
    sidebar: Entity<Sidebar>,
    dashboard: Entity<DashboardView>,
    tabs: HashMap<ObjectKey, TabPane>,
    /// The lists open as tabs (topic 07), each with its own selection,
    /// scroll position and pane.
    lists: HashMap<ListKind, Held<RecordList>>,
    editor: Option<OpenEditor>,
    /// Changes of an editor that closed because something else was shown:
    /// editing the same dashboard again continues with them.
    kept_draft: Option<(EditorTarget, DashboardDraft)>,
    /// The kept changes of the environments not on screen, by id: an
    /// environment switch keeps them for when it is back (a dashboard of
    /// one environment never gets another's draft).
    drafts_elsewhere: HashMap<String, (EditorTarget, DashboardDraft)>,
    onboarding: Option<(Entity<EnvironmentEditor>, Subscription)>,
    modal: Option<OpenModal>,
    /// The settings panel, over the main area and under the modals (an
    /// environment's editor or the about dialog opens over it).
    settings: Option<Held<SettingsPanel>>,
    /// The environment active when the open modal opened.
    modal_environment: Option<String>,
    /// The modal the question before closing the window replaced (a
    /// dialog with something typed), with its environment: it comes back
    /// when the window stays open.
    behind_close: Option<(OpenModal, Option<String>)>,
    /// The active environment, as last seen: dialogs about another one
    /// close when it changes.
    environment: Option<String>,
    /// A requested action waits for the open modal to close, and the user
    /// was told.
    request_waits: bool,
    modal_focus: FocusHandle,
    sidebar_open: bool,
    shown: Shown,
    title: SharedString,
    /// The recovery screen's header moves the window.
    drag: WindowDrag,
    /// Saves the window's bounds once it stops moving.
    save_window: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
    _clock: Task<()>,
}

impl Workspace {
    pub(crate) fn new(
        state: Entity<AppState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // A window is open: the environment on screen is live (the system
        // may first report it hidden until it is mapped; only changes
        // count).
        presence::shown(&state, cx);
        // The appearance settings, with the desktop's light or dark mode as
        // this window reports it, before the first frame.
        let appearance = *state.read(cx).appearance();
        appearance::window_opened(window.appearance(), appearance, cx);
        let sidebar = cx.new(|cx| Sidebar::new(state.clone(), window, cx));
        let dashboard = cx.new(|cx| DashboardView::new(state.clone(), cx));
        // Keyboard shortcuts reach the list through the focus path.
        window.focus(&dashboard.focus_handle(cx), cx);
        let subscriptions = vec![
            cx.observe_in(&state, window, |this, _, window, cx| {
                this.sync(window, cx);
            }),
            // The focused element went away (a closed pane): the keys go
            // back to the main area, or the open modal.
            cx.on_focus_lost(window, |this, window, cx| {
                this.focus_main(window, cx);
            }),
            cx.observe_window_bounds(window, |this, window, cx| {
                this.window_moved(window, cx);
            }),
            // The desktop switched between light and dark: *follow system*
            // follows at once.
            cx.observe_window_appearance(window, |this, window, cx| {
                let appearance = *this.state.read(cx).appearance();
                appearance::system_changed(window.appearance(), appearance, cx);
            }),
            // Out of sight for a while (minimised, another desktop), the
            // environment on screen turns quiet too (PERF-09).
            cx.observe_window_visibility(window, |this, visibility, _, cx| {
                presence::visibility_changed(&this.state, visibility, cx);
            }),
            // Back from an editor (*edit in settings file*, *edit keymap
            // file*): what changed there applies now.
            cx.observe_window_activation(window, |_, window, cx| {
                if window.is_window_active() {
                    Self::files_may_have_changed(cx);
                }
            }),
            cx.subscribe_in(
                &sidebar,
                window,
                |this, _, event: &SidebarEvent, window, cx| {
                    this.on_sidebar(event, window, cx);
                },
            ),
            cx.subscribe_in(
                &dashboard,
                window,
                |this, _, event: &DashboardEvent, window, cx| match event {
                    DashboardEvent::Edit(reference) => {
                        this.open_editor(EditorTarget::Existing(reference.clone()), "", window, cx);
                    }
                },
            ),
        ];
        let clock = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(CLOCK_TICK).await;
                let ticked = this.update(cx, Workspace::tick);
                if ticked.is_err() {
                    break;
                }
            }
        });
        let (shown, title, environment) = {
            let state = state.read(cx);
            (
                Shown::Dashboard(state.selected().cloned()),
                title_of(state),
                state.active_environment_id().map(str::to_owned),
            )
        };
        // The window manager's close (and macOS's red button) asks first
        // when unsaved work would be lost; ours does too (`close_window`).
        let this = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            this.update(cx, |workspace, cx| workspace.may_close(window, cx))
                .unwrap_or(true)
        });
        let mut workspace = Self {
            state,
            sidebar,
            dashboard,
            tabs: HashMap::new(),
            lists: HashMap::new(),
            editor: None,
            kept_draft: None,
            drafts_elsewhere: HashMap::new(),
            onboarding: None,
            modal: None,
            settings: None,
            modal_environment: None,
            behind_close: None,
            environment,
            request_waits: false,
            modal_focus: cx.focus_handle(),
            sidebar_open: true,
            shown,
            title,
            drag: WindowDrag::default(),
            save_window: None,
            _subscriptions: subscriptions,
            _clock: clock,
        };
        workspace.sync_onboarding(window, cx);
        workspace
    }

    /// The window came to the front: the keymap and settings files are
    /// read again if they were edited meanwhile.
    fn files_may_have_changed(cx: &mut Context<Self>) {
        if crate::keymap::reload_if_changed(cx) {
            cx.notify();
        }
        if let Some(session) = live::session(cx) {
            session.update(cx, live::Session::reload_settings_file);
        }
    }

    /// The window moved or changed size: remember where, and save it once
    /// it rests. A maximized window keeps the size it returns to.
    fn window_moved(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut current = window_state::to_state(window.window_bounds());
        let changed = self.state.update(cx, |state, _| {
            if current.maximized
                && let Some(previous) = state.window_state()
            {
                current = ic_config::WindowState {
                    maximized: true,
                    ..previous
                };
            }
            state.set_window_state(current)
        });
        if !changed {
            return;
        }
        let state = self.state.downgrade();
        self.save_window = Some(cx.spawn(async move |_, cx| {
            cx.background_executor().timer(WINDOW_SAVE_DELAY).await;
            let _ = state.update(cx, |state, _| state.save_ui());
        }));
    }

    /// Shows `key`: in the first dashboard that lists it (the selected one
    /// first), with its pane open, or else as a tab (a notification's
    /// click, the startup switches, the palette).
    pub(crate) fn reveal(&mut self, key: &ObjectKey, window: &mut Window, cx: &mut Context<Self>) {
        let target = self.state.read(cx).dashboard_showing(key);
        match target {
            Some(reference) => {
                self.state.update(cx, |state, cx| {
                    if state.select(reference) {
                        cx.notify();
                    }
                });
                self.sync(window, cx);
                self.dashboard
                    .update(cx, |dashboard, cx| dashboard.open_object(key, cx));
            }
            None => self.state.update(cx, |state, cx| {
                if state.open_tab(key.clone()) {
                    cx.notify();
                }
            }),
        }
    }

    /// Shows `object` of environment `environment`: at once if it is on
    /// screen, else after switching to it (the window follows the switch
    /// first; a notification centre entry of another environment).
    pub(crate) fn reveal_in(
        &mut self,
        environment: &str,
        object: &ObjectKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.state.read(cx).is_active(environment) {
            self.reveal(object, window, cx);
            return;
        }
        if self.state.read(cx).environment_by_id(environment).is_none() {
            return;
        }
        self.switch_environment(environment, cx);
        let object = object.clone();
        cx.defer_in(window, move |this, window, cx| {
            this.reveal(&object, window, cx);
        });
    }

    /// Puts the cursor on `cursor` and shows `pane` in its pane, as after
    /// following a link (screen 2c at start).
    pub(crate) fn reveal_linked(
        &mut self,
        cursor: &ObjectKey,
        pane: ObjectKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.reveal(cursor, window, cx);
        self.dashboard
            .update(cx, |dashboard, cx| dashboard.open_linked(cursor, pane, cx));
    }

    /// The dashboard view.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn dashboard(&self) -> &Entity<DashboardView> {
        &self.dashboard
    }

    /// Whether the sidebar is shown.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn is_sidebar_open(&self) -> bool {
        self.sidebar_open
    }

    /// The sidebar view.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn sidebar(&self) -> &Entity<Sidebar> {
        &self.sidebar
    }

    /// The pane of an object open as a tab.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn tab(&self, key: &ObjectKey) -> Option<&Entity<ObjectPane>> {
        self.tabs.get(key).map(|tab| &tab.view)
    }

    /// The window title last set.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn title(&self) -> &str {
        &self.title
    }

    /// The dashboard editor, while open.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn editor(&self) -> Option<&Entity<DashboardEditor>> {
        self.editor.as_ref().map(|editor| &editor.view)
    }

    /// The onboarding form, while shown.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn onboarding(&self) -> Option<&Entity<EnvironmentEditor>> {
        self.onboarding.as_ref().map(|(editor, _)| editor)
    }

    /// Which modal is open.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn modal(&self, cx: &App) -> Option<ModalKind> {
        self.modal.as_ref().map(|modal| match modal {
            OpenModal::Palette(_) => ModalKind::Palette,
            OpenModal::Environment(_) => ModalKind::Environment,
            OpenModal::Certificate(_) => ModalKind::Certificate,
            OpenModal::Action(dialog) => ModalKind::Action(dialog.view.read(cx).kind()),
            OpenModal::Confirm(confirmation) => ModalKind::Confirm(Box::new(confirmation.clone())),
            OpenModal::Path(_) => ModalKind::Path,
            OpenModal::About => ModalKind::About,
            OpenModal::Removal(dialog) => ModalKind::Removal(dialog.view.read(cx).kind()),
        })
    }

    /// The open removal confirmation of a list.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn removal_dialog(&self) -> Option<&Entity<RemovalDialog>> {
        match &self.modal {
            Some(OpenModal::Removal(dialog)) => Some(&dialog.view),
            _ => None,
        }
    }

    /// The list `kind`'s view, while it is open.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn list(&self, kind: ListKind) -> Option<&Entity<RecordList>> {
        self.lists.get(&kind).map(|list| &list.view)
    }

    /// The settings panel, while open.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn settings(&self) -> Option<&Entity<SettingsPanel>> {
        self.settings.as_ref().map(|settings| &settings.view)
    }

    /// The open palette.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn palette(&self) -> Option<&Entity<CommandPalette>> {
        match &self.modal {
            Some(OpenModal::Palette(palette)) => Some(&palette.view),
            _ => None,
        }
    }

    /// The open action dialog.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn action_dialog(&self) -> Option<&Entity<ActionDialog>> {
        match &self.modal {
            Some(OpenModal::Action(dialog)) => Some(&dialog.view),
            _ => None,
        }
    }

    /// The open certificate review.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn certificate_review(&self) -> Option<&Entity<CertificateReview>> {
        match &self.modal {
            Some(OpenModal::Certificate(review)) => Some(&review.view),
            _ => None,
        }
    }

    /// The open environment editor dialog.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn environment_editor(&self) -> Option<&Entity<EnvironmentEditor>> {
        match &self.modal {
            Some(OpenModal::Environment(editor)) => Some(&editor.view),
            _ => None,
        }
    }

    /// Redraws everything that shows relative times, and lets toasts and
    /// action markers whose time is up go.
    fn tick(&mut self, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            if state.tick_actions(Instant::now()) {
                cx.notify();
            }
        });
        self.sidebar.update(cx, |_, cx| cx.notify());
        self.dashboard.update(cx, |_, cx| cx.notify());
        if let Shown::Tab(key) = &self.shown
            && let Some(tab) = self.tabs.get(key)
        {
            tab.view.update(cx, |_, cx| cx.notify());
        }
        if let Shown::List(kind) = &self.shown
            && let Some(list) = self.lists.get(kind)
        {
            list.view.update(cx, |_, cx| cx.notify());
        }
        cx.notify();
    }

    /// Follows the state: creates and drops tab panes, closes the editor
    /// when something else is shown, keeps the window title and the
    /// onboarding form current, and moves the focus when the main area
    /// switches between the dashboard and a tab.
    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // A changed appearance setting (the settings panel, the settings
        // file) applies at once.
        let appearance = *self.state.read(cx).appearance();
        appearance::apply(appearance, cx);
        let active = self
            .state
            .read(cx)
            .active_environment_id()
            .map(str::to_owned);
        if active != self.environment {
            let previous = std::mem::replace(&mut self.environment, active);
            self.environment_changed(previous, window, cx);
        }
        let state = self.state.read(cx);
        let open: Vec<ObjectKey> = state.tabs().to_vec();
        let open_lists: Vec<ListKind> = state.lists().to_vec();
        let shown = match (state.active_tab(), state.active_list()) {
            (Some(key), _) => Shown::Tab(key.clone()),
            (None, Some(kind)) => Shown::List(kind),
            (None, None) => Shown::Dashboard(state.selected().cloned()),
        };
        let title = title_of(state);
        if title != self.title {
            window.set_window_title(&title);
            self.title = title;
        }
        self.lists.retain(|kind, _| open_lists.contains(kind));
        for kind in open_lists {
            if !self.lists.contains_key(&kind) {
                let state = self.state.clone();
                let sidebar_open = self.sidebar_open;
                let view = cx.new(|cx| {
                    let mut list = RecordList::new(state, kind, cx);
                    list.set_sidebar_open(sidebar_open, cx);
                    list
                });
                let events = cx.subscribe_in(
                    &view,
                    window,
                    |this, _, event: &RecordListEvent, window, cx| match event {
                        RecordListEvent::Remove(removal) => {
                            this.open_list_removal(removal.clone(), window, cx);
                        }
                    },
                );
                self.lists.insert(kind, Held::new(view, events));
            }
        }
        self.tabs.retain(|key, _| open.contains(key));
        for key in open {
            if !self.tabs.contains_key(&key) {
                let state = self.state.clone();
                let sidebar_open = self.sidebar_open;
                let view = cx.new(|cx| {
                    let mut pane = ObjectPane::new(state, key.clone(), PaneMode::Tab, cx);
                    pane.set_sidebar_open(sidebar_open, cx);
                    pane
                });
                self.tabs.insert(key, TabPane { view });
            }
        }
        if self
            .editor
            .as_ref()
            .is_some_and(|editor| editor.opened_over != shown)
            && let Some(editor) = self.editor.take()
        {
            self.keep_changes(&editor, None, cx);
        }
        if shown != self.shown {
            // A tab shown: its object counts as seen (A2).
            if let Shown::Tab(key) = &shown {
                crate::pane::mark_seen(&self.state, key, cx);
            }
            self.shown = shown;
            if self.modal.is_none() {
                self.focus_main(window, cx);
            }
        }
        self.sync_onboarding(window, cx);
        self.pick_up_request(window, cx);
        cx.notify();
    }

    /// Another environment is active (from the footer, the palette, the
    /// tray or a notification of another environment): what was opened
    /// for the old one (`previous`) goes, so nothing meant for it reaches
    /// the new one (an acknowledgement for `staging` must never go to
    /// production). The dashboard editor's changes are kept for when the
    /// old environment is back (`drafts_elsewhere`), and the new one's
    /// kept changes come back. The environment editor and the about
    /// dialog stay.
    fn environment_changed(
        &mut self,
        previous: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(editor) = self.editor.take() {
            self.keep_changes(&editor, previous.as_deref(), cx);
        }
        // A list's marks and pane belong to the environment they were made
        // in; the new one's lists start afresh.
        self.lists.clear();
        if let Some(draft) = self.kept_draft.take()
            && let Some(previous) = previous
        {
            self.drafts_elsewhere.insert(previous, draft);
        }
        self.kept_draft = self
            .environment
            .as_ref()
            .and_then(|id| self.drafts_elsewhere.remove(id));
        self.request_waits = false;
        self.state.update(cx, |state, _| {
            if let Some(request) = state.drop_unbound_request() {
                tracing::info!(
                    action = request.action.label(),
                    "dropped a request for the previous environment"
                );
            }
        });
        // The environment editor stays, also behind its question.
        let keep = matches!(
            self.modal,
            None | Some(OpenModal::Environment(_) | OpenModal::About)
        ) || matches!(
            &self.modal,
            Some(OpenModal::Confirm(confirmation))
                if confirmation.action == Confirmed::DiscardEnvironmentEdits
        );
        if !keep {
            self.close_modal(window, cx);
        }
    }

    /// Carries out a requested action once no modal is open; while one
    /// is, the request waits (a desktop notification's Acknowledge while
    /// the palette is open) and a toast says so.
    fn pick_up_request(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.state.read(cx).has_request() {
            self.request_waits = false;
            return;
        }
        if self.modal.is_some() {
            if !self.request_waits {
                self.request_waits = true;
                self.state.update(cx, |state, cx| {
                    state.inform(
                        "An action waits for this dialog",
                        Some("It opens once the dialog is closed.".to_owned()),
                    );
                    cx.notify();
                });
            }
            return;
        }
        self.request_waits = false;
        let request = self.state.update(cx, |state, _| state.take_request());
        if let Some((request, environment)) = request {
            self.handle_request(request, environment, window, cx);
        }
    }

    /// Shows the onboarding form while there is no environment (not in the
    /// demo, not while the settings can't be read).
    fn sync_onboarding(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let wanted = {
            let state = self.state.read(cx);
            state.environments().is_empty() && !state.is_demo() && state.config_problem().is_none()
        };
        match (wanted, self.onboarding.is_some()) {
            (true, false) => {
                let editor =
                    cx.new(|cx| EnvironmentEditor::new(None, EditorMode::Onboarding, window, cx));
                let events = cx.subscribe_in(
                    &editor,
                    window,
                    |this, _, event: &EnvironmentEditorEvent, window, cx| {
                        if matches!(event, EnvironmentEditorEvent::Close) {
                            this.onboarding = None;
                            this.focus_main(window, cx);
                            cx.notify();
                        }
                    },
                );
                self.onboarding = Some((editor, events));
            }
            (false, true) => self.onboarding = None,
            _ => {}
        }
    }

    /// Gives the keyboard focus to the open modal, else the main area:
    /// the editor, the tab shown, or the dashboard list.
    fn focus_main(&self, window: &mut Window, cx: &mut App) {
        // The view that should have the keyboard, and where in it the
        // keyboard goes when it has none yet (a just-opened view isn't
        // drawn yet, so its fields can't be found inside it).
        let (container, default) = if let Some(modal) = &self.modal {
            match modal {
                OpenModal::Palette(palette) => (
                    palette.view.focus_handle(cx),
                    palette.view.read(cx).default_focus(cx),
                ),
                OpenModal::Environment(editor) => (
                    editor.view.focus_handle(cx),
                    editor.view.read(cx).default_focus(cx),
                ),
                OpenModal::Certificate(review) => {
                    let handle = review.view.focus_handle(cx);
                    (handle.clone(), handle)
                }
                OpenModal::Action(dialog) => (
                    dialog.view.focus_handle(cx),
                    dialog.view.read(cx).default_focus(cx),
                ),
                OpenModal::Path(prompt) => {
                    let handle = prompt.input.focus_handle(cx);
                    (handle.clone(), handle)
                }
                OpenModal::Confirm(_) | OpenModal::About => {
                    (self.modal_focus.clone(), self.modal_focus.clone())
                }
                OpenModal::Removal(dialog) => {
                    let handle = dialog.view.focus_handle(cx);
                    (handle.clone(), handle)
                }
            }
        } else if let Some(settings) = &self.settings {
            (
                settings.view.focus_handle(cx),
                settings.view.read(cx).default_focus(cx),
            )
        } else if let Some(editor) = &self.editor {
            (
                editor.view.focus_handle(cx),
                editor.view.read(cx).default_focus(cx),
            )
        } else if let Some((onboarding, _)) = &self.onboarding {
            (
                onboarding.focus_handle(cx),
                onboarding.read(cx).default_focus(cx),
            )
        } else {
            let handle = match &self.shown {
                Shown::Tab(key) => self.tabs.get(key).map_or_else(
                    || self.dashboard.focus_handle(cx),
                    |tab| tab.view.focus_handle(cx),
                ),
                Shown::List(kind) => self.lists.get(kind).map_or_else(
                    || self.dashboard.focus_handle(cx),
                    |list| list.view.focus_handle(cx),
                ),
                Shown::Dashboard(_) => self.dashboard.focus_handle(cx),
            };
            (handle.clone(), handle)
        };
        // A text field inside keeps the focus it has.
        if !container.contains_focused(window, cx) && !default.is_focused(window) {
            window.focus(&default, cx);
        }
    }

    fn on_focus_main(&mut self, _: &FocusMain, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_main(window, cx);
    }

    /// `secondary-1` … `secondary-9`: shows that dashboard and focuses its
    /// list (also when it's already shown, say from the search field).
    fn select_dashboard(
        &mut self,
        action: &SelectDashboard,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.modal.is_some() {
            return;
        }
        let Some(reference) = self.state.read(cx).dashboard_at(action.0) else {
            return;
        };
        self.show_dashboard(reference, window, cx);
    }

    /// Shows `reference` and focuses its list.
    fn show_dashboard(
        &mut self,
        reference: DashboardRef,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state.update(cx, |state, cx| {
            if state.select(reference) {
                cx.notify();
            }
        });
        // If that switched the main area, `sync` moves the focus again.
        self.focus_main(window, cx);
    }

    fn next_tab(&mut self, _: &ActivateNextTab, _: &mut Window, cx: &mut Context<Self>) {
        self.cycle_tab(true, cx);
    }

    fn previous_tab(&mut self, _: &ActivatePreviousTab, _: &mut Window, cx: &mut Context<Self>) {
        self.cycle_tab(false, cx);
    }

    fn cycle_tab(&self, forward: bool, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            if state.cycle_tab(forward) {
                cx.notify();
            }
        });
    }

    fn close_tab(&mut self, _: &CloseTab, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(kind) = self.state.read(cx).active_list() {
            self.state.update(cx, |state, cx| {
                if state.close_list(kind) {
                    cx.notify();
                }
            });
            return;
        }
        let Some(active) = self.state.read(cx).active_tab().cloned() else {
            cx.propagate();
            return;
        };
        self.state.update(cx, |state, cx| {
            if state.close_tab(&active) {
                cx.notify();
            }
        });
    }

    fn toggle_sidebar(&mut self, _: &ToggleSidebar, _: &mut Window, cx: &mut Context<Self>) {
        self.sidebar_open = !self.sidebar_open;
        let open = self.sidebar_open;
        self.dashboard
            .update(cx, |dashboard, cx| dashboard.set_sidebar_open(open, cx));
        for tab in self.tabs.values() {
            tab.view
                .update(cx, |pane, cx| pane.set_sidebar_open(open, cx));
        }
        for list in self.lists.values() {
            list.view
                .update(cx, |list, cx| list.set_sidebar_open(open, cx));
        }
        if let Some(editor) = &self.editor {
            editor
                .view
                .update(cx, |editor, cx| editor.set_sidebar_open(open, cx));
        }
        cx.notify();
    }

    // --- Modals -------------------------------------------------------

    /// Opens `modal` (replacing an open one) and gives it the keyboard.
    fn open_modal(&mut self, modal: OpenModal, window: &mut Window, cx: &mut Context<Self>) {
        self.sidebar.update(cx, |sidebar, cx| {
            sidebar.close_menu(cx);
        });
        self.modal = Some(modal);
        self.modal_environment = self
            .state
            .read(cx)
            .active_environment_id()
            .map(str::to_owned);
        self.focus_main(window, cx);
        cx.notify();
    }

    /// Closes the open modal; the keyboard goes back to the main area, or
    /// to the dialog of an action that waited for this one. Closing the
    /// question before closing the window, or before dropping the
    /// environment editor's changes, brings back the dialog it replaced
    /// (unless another environment is active by now).
    pub(crate) fn close_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(closed) = self.modal.take() {
            let behind = self.behind_close.take();
            // The environment editor belongs to no environment on screen;
            // other dialogs come back only to their own.
            if replaces_a_dialog(&closed)
                && let Some((modal, environment)) = behind
                && (matches!(modal, OpenModal::Environment(_))
                    || environment.as_deref() == self.state.read(cx).active_environment_id())
            {
                self.modal = Some(modal);
                self.modal_environment = environment;
            }
            self.focus_main(window, cx);
            self.pick_up_request(window, cx);
            cx.notify();
        }
    }

    fn on_close_modal(&mut self, _: &CloseModal, window: &mut Window, cx: &mut Context<Self>) {
        self.close_modal(window, cx);
    }

    fn on_modal_escape(&mut self, _: &Escape, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.modal, Some(OpenModal::Environment(_))) {
            self.leave_environment_editor(window, cx);
        } else {
            self.close_modal(window, cx);
        }
    }

    /// Escape or *cancel* in the environment editor: it closes, unless
    /// something changed (a field, a URL, a trusted certificate, a typed
    /// password); then it asks first, and coming back from the question
    /// keeps editing (like the dashboard editor, DASH-04).
    fn leave_environment_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let subject = match &self.modal {
            Some(OpenModal::Environment(editor)) if editor.view.read(cx).has_changes() => {
                editor.view.read(cx).subject()
            }
            _ => {
                self.close_modal(window, cx);
                return;
            }
        };
        let behind = self
            .modal
            .take()
            .map(|modal| (modal, self.modal_environment.clone()));
        let confirmation = Confirmation {
            title: format!("Discard the changes to {subject}?"),
            detail: "Nothing you changed is saved, and a password you typed is forgotten."
                .to_owned(),
            confirm: "discard changes",
            danger: true,
            action: Confirmed::DiscardEnvironmentEdits,
        };
        self.open_modal(OpenModal::Confirm(confirmation), window, cx);
        self.behind_close = behind;
    }

    /// `secondary-k`: opens the command palette, or closes it.
    fn toggle_palette(
        &mut self,
        _: &ToggleCommandPalette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match &self.modal {
            Some(OpenModal::Palette(_)) => self.close_modal(window, cx),
            Some(_) => {}
            None => self.open_palette(window, cx),
        }
    }

    /// Opens the command palette over what the main area shows.
    pub(crate) fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let targets = match &self.shown {
            Shown::Tab(key) => vec![key.clone()],
            Shown::List(kind) => self
                .lists
                .get(kind)
                .map(|list| list.view.update(cx, RecordList::action_targets))
                .unwrap_or_default(),
            Shown::Dashboard(_) if self.editor.is_none() => {
                self.dashboard.update(cx, DashboardView::action_targets)
            }
            Shown::Dashboard(_) => Vec::new(),
        };
        let focus = Focus { targets };
        let state = self.state.clone();
        let palette = cx.new(|cx| CommandPalette::new(&state, &focus, window, cx));
        let events = cx.subscribe_in(
            &palette,
            window,
            |this, _, event: &PaletteEvent, window, cx| match event {
                PaletteEvent::Close => this.close_modal(window, cx),
                PaletteEvent::Run(command) => {
                    this.close_modal(window, cx);
                    this.run_command(command.clone(), window, cx);
                }
            },
        );
        self.open_modal(OpenModal::Palette(Held::new(palette, events)), window, cx);
    }

    /// Carries out what was chosen in the palette.
    #[expect(clippy::too_many_lines, reason = "one arm per palette command")]
    fn run_command(
        &mut self,
        command: PaletteCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match command {
            PaletteCommand::ShowDashboard(reference) => self.show_dashboard(reference, window, cx),
            PaletteCommand::OpenObject(key) => self.reveal(&key, window, cx),
            PaletteCommand::OpenTab(key) => self.state.update(cx, |state, cx| {
                if state.open_tab(key) {
                    cx.notify();
                }
            }),
            PaletteCommand::Act(action, targets) if targets.is_empty() => match &self.shown {
                Shown::Tab(key) => self.request(action, vec![key.clone()], cx),
                Shown::List(kind) => {
                    let targets = self
                        .lists
                        .get(kind)
                        .map(|list| list.view.update(cx, RecordList::action_targets))
                        .unwrap_or_default();
                    if !targets.is_empty() {
                        self.request(action, targets, cx);
                    }
                }
                Shown::Dashboard(_) => self
                    .dashboard
                    .update(cx, |dashboard, cx| dashboard.run_action(action, cx)),
            },
            PaletteCommand::OpenList(kind) => self.state.update(cx, |state, cx| {
                if state.open_list(kind) {
                    cx.notify();
                }
            }),
            PaletteCommand::Act(action, targets) => self.act_on_named(action, targets, window, cx),
            PaletteCommand::Copy { what, text } => self.copy(what, text, cx),
            PaletteCommand::Reload => self.state.update(cx, |state, cx| {
                if state.refresh(Instant::now()) {
                    cx.notify();
                }
            }),
            PaletteCommand::ToggleSidebar => self.toggle_sidebar(&ToggleSidebar, window, cx),
            PaletteCommand::NewDashboard => self.new_dashboard(None, window, cx),
            PaletteCommand::NewGroup => {
                let created = self
                    .state
                    .update(cx, |state, _| state.create_group("", None));
                if let Some(id) = created {
                    self.sidebar.update(cx, |sidebar, cx| {
                        sidebar.start_rename(crate::sidebar::RenameTarget::Group(id), window, cx);
                    });
                }
            }
            PaletteCommand::EditDashboard(reference) => {
                self.open_editor(EditorTarget::Existing(reference), "", window, cx);
            }
            PaletteCommand::ImportDashboards => self.import_groups(window, cx),
            PaletteCommand::ExportDashboards => self.export_groups(&[], window, cx),
            PaletteCommand::Pause(choice) => self.state.update(cx, |state, cx| {
                let now = Timestamp::now();
                state.pause_notifications(Some(choice.until(now)));
                state.inform(
                    format!("Notifications paused {}", choice.label(now)),
                    Some("They're recorded in the notification centre meanwhile.".to_owned()),
                );
                cx.notify();
            }),
            PaletteCommand::Resume => self.state.update(cx, |state, cx| {
                state.pause_notifications(None);
                state.inform("Notifications resumed", None);
                cx.notify();
            }),
            PaletteCommand::MuteEnvironment(id, choice) => self.state.update(cx, |state, cx| {
                let now = Timestamp::now();
                let name = state
                    .environment_by_id(&id)
                    .map(|environment| environment.name.clone())
                    .unwrap_or_default();
                if state.pause_environment(&id, choice.map(|choice| choice.until(now))) {
                    match choice {
                        Some(choice) => state.inform(
                            format!("Muted {name} {}", choice.label(now)),
                            Some(
                                "Its notifications are recorded in the notification centre \
                                 meanwhile; the other environments still notify."
                                    .to_owned(),
                            ),
                        ),
                        None => state.inform(format!("Unmuted {name}"), None),
                    }
                }
                cx.notify();
            }),
            PaletteCommand::Override(change, targets) => self.change_override(change, &targets, cx),
            PaletteCommand::OpenNotifications => self.open_notifications(window, cx),
            PaletteCommand::MarkNotificationsRead => self.state.update(cx, |state, cx| {
                if state.mark_all_notifications_read() {
                    cx.notify();
                }
            }),
            PaletteCommand::Settings(tab) => self.open_settings(tab, None, window, cx),
            PaletteCommand::About => self.open_about(window, cx),
            PaletteCommand::Quit => cx.quit(),
            PaletteCommand::SwitchEnvironment(id) => self.switch_environment(&id, cx),
            PaletteCommand::AddEnvironment => self.open_environment_editor(None, window, cx),
            PaletteCommand::EditEnvironment(id) => {
                self.open_environment_editor(Some(&id), window, cx);
            }
        }
    }

    /// Asks for an action (refused with a toast if the API user may not
    /// run it); the state's observer opens its dialog.
    fn request(
        &self,
        action: actions::ObjectAction,
        targets: Vec<ObjectKey>,
        cx: &mut Context<Self>,
    ) {
        self.request_reviewed(action, targets, false, cx);
    }

    /// Acts on objects a palette query named: one is shown first; several
    /// (a verb's *all N matches*, `secondary-enter`) are listed in the
    /// action's dialog before anything is sent.
    fn act_on_named(
        &mut self,
        action: actions::ObjectAction,
        targets: Vec<ObjectKey>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let [only] = targets.as_slice() {
            self.reveal(only, window, cx);
        }
        let review = targets.len() > 1;
        self.request_reviewed(action, targets, review, cx);
    }

    /// [`Workspace::request`]; with `review`, a dialog lists the objects
    /// before anything is sent (a palette query's *all N matches*).
    fn request_reviewed(
        &self,
        action: actions::ObjectAction,
        targets: Vec<ObjectKey>,
        review: bool,
        cx: &mut Context<Self>,
    ) {
        self.state.update(cx, |state, cx| {
            // A refusal shows as a toast.
            let _ = state.request(actions::ActionRequest {
                action,
                targets,
                review,
            });
            cx.notify();
        });
    }

    // --- Actions (ACT-01..07) -----------------------------------------

    /// Carries out an action asked for: opens its dialog for the objects
    /// it applies to, asks first (bulk removals, many checks), or sends
    /// it. When none of the objects qualifies, a toast says why. An action
    /// for another environment than the one on screen (`environment`; a
    /// desktop notification's *Acknowledge*, A1) opens its dialog for that
    /// environment's objects, and the dialog sends it to that
    /// environment's engine.
    fn handle_request(
        &mut self,
        request: actions::ActionRequest,
        environment: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let actions::ActionRequest {
            action,
            targets,
            review,
        } = request;
        let elsewhere = environment.filter(|id| !self.state.read(cx).is_active(id));
        let snapshot = match &elsewhere {
            Some(id) => match self.state.read(cx).snapshot_of(id) {
                Some(snapshot) => snapshot.clone(),
                None => return,
            },
            None => self.state.read(cx).snapshot().clone(),
        };
        let eligible = match action {
            actions::ObjectAction::Acknowledge
            | actions::ObjectAction::RemoveAcknowledgement
            | actions::ObjectAction::RemoveDowntimes => {
                forms::eligible(&action, &snapshot, &targets)
            }
            _ => forms::known(&snapshot, &targets),
        };
        if eligible.targets.is_empty() {
            let detail = match eligible.skipped.as_slice() {
                [one] => format!(
                    "{} {}.",
                    describe_objects(std::slice::from_ref(&one.object)),
                    one.reason
                ),
                _ => format!(
                    "{}: {}.",
                    describe_objects(&targets),
                    eligible.skipped_summary()
                ),
            };
            self.state.update(cx, |state, cx| {
                state.inform(format!("Nothing to {}", action.label()), Some(detail));
                cx.notify();
            });
            return;
        }
        // Removing downtimes always asks first, listing every downtime it
        // removes (topic 01).
        if elsewhere.is_none()
            && self.ask_removal(&action, &snapshot, &eligible.targets, window, cx)
        {
            return;
        }
        // So does removing several acknowledgements, listing each one with
        // who set it, sticky and expiry (topic 07).
        if elsewhere.is_none()
            && action == actions::ObjectAction::RemoveAcknowledgement
            && eligible.targets.len() > 1
        {
            let removal = crate::lists::removal::acknowledgements(
                &snapshot,
                &eligible.targets,
                Timestamp::now(),
            );
            self.open_list_removal(removal, window, cx);
            return;
        }
        // Objects a palette query named loosely are always listed first.
        let dialog = if review {
            DialogKind::for_review(&action)
        } else {
            DialogKind::for_action(&action)
        };
        if let Some(kind) = dialog {
            let state = self.state.clone();
            let bound = elsewhere.clone();
            let dialog = cx.new(|cx| ActionDialog::new(state, kind, eligible, bound, window, cx));
            let events = cx.subscribe_in(
                &dialog,
                window,
                |this, _, event: &DialogEvent, window, cx| match event {
                    DialogEvent::Close => this.close_modal(window, cx),
                },
            );
            self.open_modal(OpenModal::Action(Held::new(dialog, events)), window, cx);
            return;
        }
        let Some(spec) = ActionSpec::immediate(&action, eligible.targets.clone()) else {
            return;
        };
        if let Some(id) = elsewhere {
            // Only an acknowledgement (a dialog) comes from another
            // environment; anything else is sent there without asking.
            let label = spec.kind.label();
            self.state.update(cx, |state, cx| {
                if let Err(error) = state.submit_in(&id, spec) {
                    state.inform(format!("Couldn't {label}"), Some(error));
                }
                cx.notify();
            });
            return;
        }
        match confirmation_for(&spec, &eligible) {
            Some(confirmation) => {
                self.open_modal(OpenModal::Confirm(confirmation), window, cx);
            }
            None => self.submit(spec, cx),
        }
    }

    /// For a removal of downtimes: opens the dialog listing what goes, or
    /// says the downtime is gone already. Whether it was one.
    fn ask_removal(
        &mut self,
        action: &actions::ObjectAction,
        snapshot: &Snapshot,
        targets: &[ObjectKey],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let removal = match action {
            actions::ObjectAction::RemoveDowntime(name) => targets
                .first()
                .and_then(|object| crate::downtimes::Removal::downtime(snapshot, object, name)),
            actions::ObjectAction::RemoveDowntimes => {
                Some(crate::downtimes::Removal::all_of(snapshot, targets))
            }
            _ => return false,
        };
        match removal {
            Some(removal) => self.open_removal(removal, window, cx),
            None => self.state.update(cx, |state, cx| {
                state.inform(
                    "Nothing to remove",
                    Some("The downtime is gone already.".to_owned()),
                );
                cx.notify();
            }),
        }
        true
    }

    /// Opens the dialog that lists every downtime `removal` removes.
    fn open_removal(
        &mut self,
        removal: crate::downtimes::Removal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let state = self.state.clone();
        let dialog = cx.new(|cx| ActionDialog::removal(state, removal, window, cx));
        let events = cx.subscribe_in(
            &dialog,
            window,
            |this, _, event: &DialogEvent, window, cx| match event {
                DialogEvent::Close => this.close_modal(window, cx),
            },
        );
        self.open_modal(OpenModal::Action(Held::new(dialog, events)), window, cx);
    }

    /// Opens the confirmation that lists every target of a removal from a
    /// list (or of removing several acknowledgements).
    fn open_list_removal(
        &mut self,
        removal: crate::lists::removal::BulkRemoval,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let state = self.state.clone();
        let dialog = cx.new(|cx| RemovalDialog::new(state, removal, cx));
        let events = cx.subscribe_in(
            &dialog,
            window,
            |this, _, event: &DialogEvent, window, cx| match event {
                DialogEvent::Close => this.close_modal(window, cx),
            },
        );
        self.open_modal(OpenModal::Removal(Held::new(dialog, events)), window, cx);
    }

    /// Sends `spec`; a refusal shows as a toast.
    fn submit(&self, spec: ActionSpec, cx: &mut Context<Self>) {
        let label = spec.kind.label();
        self.state.update(cx, |state, cx| {
            if let Err(error) = state.submit(spec) {
                state.inform(format!("Couldn't {label}"), Some(error));
            }
            cx.notify();
        });
    }

    /// Copies `text` and says what was copied.
    pub(crate) fn copy(&self, what: &str, text: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.state.update(cx, |state, cx| {
            state.inform(format!("Copied {what}"), None);
            cx.notify();
        });
    }

    fn on_sidebar(&mut self, event: &SidebarEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            SidebarEvent::NewDashboard(group) => self.new_dashboard(group.clone(), window, cx),
            SidebarEvent::EditDashboard(reference) => {
                self.open_editor(EditorTarget::Existing(reference.clone()), "", window, cx);
            }
            SidebarEvent::DeleteGroup(id) => self.confirm_delete_group(id, window, cx),
            SidebarEvent::DeleteDashboard(reference) => {
                self.confirm_delete_dashboard(reference, window, cx);
            }
            SidebarEvent::ExportGroups(ids) => self.export_groups(ids, window, cx),
            SidebarEvent::ImportGroups => self.import_groups(window, cx),
            SidebarEvent::SwitchEnvironment(id) => self.switch_environment(id, cx),
            SidebarEvent::AddEnvironment => self.open_environment_editor(None, window, cx),
            SidebarEvent::EditEnvironment(id) => {
                self.open_environment_editor(Some(id), window, cx);
            }
            SidebarEvent::OpenObjectIn {
                environment,
                object,
            } => self.reveal_in(environment, object, window, cx),
            SidebarEvent::OpenSettings(page) => self.open_settings(*page, None, window, cx),
            SidebarEvent::CustomRule(key) => {
                self.open_settings(SettingsPage::Notifications, Some(key), window, cx);
            }
        }
    }

    // --- Notifications and settings (NOTE-02..06, BG-01, BG-03) --------

    /// Watches, mutes or unmutes `targets` and says so.
    pub(crate) fn change_override(
        &self,
        change: crate::notifications::OverrideChange,
        targets: &[ObjectKey],
        cx: &mut Context<Self>,
    ) {
        self.state.update(cx, |state, cx| {
            if let Some(message) = change.apply(state, targets, Timestamp::now()) {
                state.inform(message, None);
                cx.notify();
            }
        });
    }

    /// Opens the notification centre above the footer.
    pub(crate) fn open_notifications(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.sidebar_open {
            self.toggle_sidebar(&ToggleSidebar, window, cx);
        }
        self.sidebar.update(cx, Sidebar::open_notifications);
    }

    /// Opens the settings panel on `page` (or shows `page` in the open
    /// one); with `custom`, that group or dashboard of the environment on
    /// screen gets a custom rule to edit, on the notifications page.
    pub(crate) fn open_settings(
        &mut self,
        page: SettingsPage,
        custom: Option<&ScopeKey>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A modal over the main area (the palette that opened this) goes.
        if self.modal.is_some() {
            self.close_modal(window, cx);
        }
        self.sidebar.update(cx, |sidebar, cx| {
            sidebar.close_menu(cx);
        });
        let panel = if let Some(settings) = &self.settings {
            settings.view.clone()
        } else {
            {
                let state = self.state.clone();
                let panel = cx.new(|cx| SettingsPanel::new(state, page, window, cx));
                let events = cx.subscribe_in(
                    &panel,
                    window,
                    |this, _, event: &SettingsEvent, window, cx| match event {
                        SettingsEvent::Close => this.close_settings(window, cx),
                        SettingsEvent::About => this.open_about(window, cx),
                        SettingsEvent::LaunchAtLogin(enabled) => {
                            crate::background::autostart::change(*enabled, &this.state, cx);
                        }
                        SettingsEvent::EditEnvironment(id) => {
                            this.open_environment_editor(Some(id), window, cx);
                        }
                        SettingsEvent::AddEnvironment => {
                            this.open_environment_editor(None, window, cx);
                        }
                    },
                );
                self.settings = Some(Held::new(panel.clone(), events));
                panel
            }
        };
        panel.update(cx, |panel, cx| match custom {
            Some(key) => panel.show_custom_rule(key, window, cx),
            None => panel.show_page(page, window, cx),
        });
        let focus = panel.read(cx).default_focus(cx);
        window.focus(&focus, cx);
        cx.notify();
    }

    /// Closes the settings panel; the keyboard goes back to the main area.
    pub(crate) fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings.take().is_some() {
            self.focus_main(window, cx);
            cx.notify();
        }
    }

    /// The settings panel over the dimmed window, centred, about
    /// 1080×760 and fitted to smaller windows.
    fn render_settings(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let settings = self.settings.as_ref()?;
        Some(
            Modal::new(
                "settings",
                px(crate::settings::PANEL_WIDTH),
                settings.view.clone(),
            )
            .on_dismiss(cx.listener(|this, _: &MouseDownEvent, window, cx| {
                // Everything applied already but a field being typed in.
                if let Some(settings) = &this.settings {
                    settings
                        .view
                        .update(cx, |panel, cx| panel.commit_pending(window, cx));
                }
                this.close_settings(window, cx);
            }))
            .into_any_element(),
        )
    }

    /// Shows the about dialog.
    pub(crate) fn open_about(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_modal(OpenModal::About, window, cx);
    }

    fn on_open_settings(&mut self, _: &OpenSettings, window: &mut Window, cx: &mut Context<Self>) {
        // ctrl-, again shows the page already open.
        let page = self
            .settings
            .as_ref()
            .map_or(SettingsPage::General, |settings| {
                settings.view.read(cx).page()
            });
        self.open_settings(page, None, window, cx);
    }

    fn on_show_about(&mut self, _: &ShowAbout, window: &mut Window, cx: &mut Context<Self>) {
        self.open_about(window, cx);
    }

    fn on_open_notifications(
        &mut self,
        _: &OpenNotifications,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_notifications(window, cx);
    }

    // --- Dashboards ---------------------------------------------------

    /// `secondary-n` (and the `+` buttons): a new dashboard in `group`, or
    /// the selected dashboard's group, or the first; a group is created
    /// when there is none.
    fn new_dashboard(
        &mut self,
        group: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let group = self.state.update(cx, |state, _| {
            state.environment()?;
            group
                .or_else(|| state.selected().map(|reference| reference.group_id.clone()))
                .or_else(|| state.groups().first().map(|group| group.id.clone()))
                .or_else(|| state.create_group(FIRST_GROUP_NAME, None))
        });
        if let Some(group) = group {
            self.open_editor(EditorTarget::New, &group, window, cx);
        }
    }

    fn on_new_dashboard(&mut self, _: &NewDashboard, window: &mut Window, cx: &mut Context<Self>) {
        if self.modal.is_none() {
            self.new_dashboard(None, window, cx);
        }
    }

    /// Opens the dashboard editor in the main area.
    fn open_editor(
        &mut self,
        target: EditorTarget,
        group_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_modal(window, cx);
        // Editing shows the dashboard's place: its own list, not a tab.
        if let EditorTarget::Existing(reference) = &target {
            self.state.update(cx, |state, cx| {
                if state.select(reference.clone()) {
                    cx.notify();
                }
            });
            self.sync(window, cx);
        } else if self.state.read(cx).active_tab().is_some() {
            self.state.update(cx, |state, cx| {
                if state.show_dashboard() {
                    cx.notify();
                }
            });
            self.sync(window, cx);
        }
        let Some(saved) =
            crate::editor::model::initial_draft(self.state.read(cx), &target, group_id)
        else {
            return;
        };
        // Changes kept from the last time this dashboard was edited (a new
        // one: in the same group) come back.
        let draft = match self.kept_draft.take() {
            Some((kept, draft))
                if kept == target
                    && (target != EditorTarget::New || draft.group_id == saved.group_id) =>
            {
                draft
            }
            other => {
                self.kept_draft = other;
                saved.clone()
            }
        };
        let state = self.state.clone();
        let sidebar_open = self.sidebar_open;
        let view = cx.new(|cx| {
            let mut editor = DashboardEditor::new(state, target, saved, draft, window, cx);
            editor.set_sidebar_open(sidebar_open, cx);
            editor
        });
        let events =
            cx.subscribe_in(
                &view,
                window,
                |this, _, event: &EditorEvent, window, cx| match event {
                    EditorEvent::Closed => {
                        this.editor = None;
                        this.focus_main(window, cx);
                        cx.notify();
                    }
                    EditorEvent::DiscardChanges => this.confirm_discard(window, cx),
                    EditorEvent::Delete(reference) => {
                        this.confirm_delete_dashboard(reference, window, cx);
                    }
                },
            );
        self.editor = Some(OpenEditor {
            view,
            opened_over: self.shown.clone(),
            _events: events,
        });
        self.focus_main(window, cx);
        cx.notify();
    }

    /// Keeps the changes of `editor`, closing because something else is
    /// shown, for the next time the same dashboard is edited, and says so.
    /// `environment` (its id) is the editor's when another one is about to
    /// be shown: the message says to come back to it.
    fn keep_changes(
        &mut self,
        editor: &OpenEditor,
        environment: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        let editor = editor.view.read(cx);
        let Some(draft) = editor.changes().cloned() else {
            return;
        };
        let title = editor.title();
        self.kept_draft = Some((editor.target().clone(), draft));
        let elsewhere = environment
            .and_then(|id| self.state.read(cx).environment_by_id(id))
            .map(|environment| environment.name.clone());
        let detail = match elsewhere {
            Some(name) => format!("Switch back to {name} and edit it again to continue."),
            None => "Edit it again to continue, or discard them there.".to_owned(),
        };
        self.state.update(cx, |state, cx| {
            state.inform(format!("Your changes to {title} are kept"), Some(detail));
            cx.notify();
        });
    }

    /// Asks before the dashboard editor drops its changes (DASH-04).
    fn confirm_discard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = &self.editor else {
            return;
        };
        let editor = editor.view.read(cx);
        let detail = match editor.target() {
            EditorTarget::New => "The new dashboard isn't created.",
            EditorTarget::Existing(_) => "The dashboard keeps what was saved.",
        };
        let confirmation = Confirmation {
            title: format!("Discard the changes to {}?", editor.title()),
            detail: detail.to_owned(),
            confirm: "discard changes",
            danger: true,
            action: Confirmed::DiscardEdits,
        };
        self.open_modal(OpenModal::Confirm(confirmation), window, cx);
    }

    fn confirm_delete_group(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(group) = self
            .state
            .read(cx)
            .groups()
            .iter()
            .find(|group| group.id == id)
            .cloned()
        else {
            return;
        };
        let count = group.dashboards.len();
        let detail = match count {
            0 => "It has no dashboards.".to_owned(),
            1 => "Its dashboard is deleted with it. This can't be undone.".to_owned(),
            _ => format!("Its {count} dashboards are deleted with it. This can't be undone."),
        };
        let confirmation = Confirmation {
            title: format!("Delete the group {}?", group.name),
            detail,
            confirm: "delete group",
            danger: true,
            action: Confirmed::Group(id.to_owned()),
        };
        self.open_modal(OpenModal::Confirm(confirmation), window, cx);
    }

    fn confirm_delete_dashboard(
        &mut self,
        reference: &DashboardRef,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(name) = self
            .state
            .read(cx)
            .dashboard(reference)
            .map(|(_, dashboard)| dashboard.name.clone())
        else {
            return;
        };
        let confirmation = Confirmation {
            title: format!("Delete the dashboard {name}?"),
            detail: "Its view and notification setting are deleted. This can't be undone."
                .to_owned(),
            confirm: "delete dashboard",
            danger: true,
            action: Confirmed::Dashboard(reference.clone()),
        };
        self.open_modal(OpenModal::Confirm(confirmation), window, cx);
    }

    fn confirm_delete_environment(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(name) = self
            .state
            .read(cx)
            .environment_by_id(id)
            .map(|environment| environment.name.clone())
        else {
            return;
        };
        let confirmation = Confirmation {
            title: format!("Delete the environment {name}?"),
            detail: "Its dashboards, its password in the keychain and its local event log are \
                     deleted. Icinga itself isn't touched. This can't be undone."
                .to_owned(),
            confirm: "delete environment",
            danger: true,
            action: Confirmed::Environment(id.to_owned()),
        };
        self.open_modal(OpenModal::Confirm(confirmation), window, cx);
    }

    /// Carries out the open confirmation.
    pub(crate) fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(OpenModal::Confirm(confirmation)) = self.modal.take() else {
            return;
        };
        let same_environment =
            self.modal_environment.as_deref() == self.state.read(cx).active_environment_id();
        match confirmation.action {
            Confirmed::Action(spec) if !same_environment => {
                let label = spec.kind.label();
                self.state.update(cx, |state, cx| {
                    state.inform(
                        format!("Couldn't {label}"),
                        Some(crate::operate::dialog::ENVIRONMENT_CHANGED.to_owned()),
                    );
                    cx.notify();
                });
            }
            Confirmed::Action(spec) => self.submit(spec, cx),
            Confirmed::DiscardEdits => {
                self.editor = None;
            }
            Confirmed::DiscardEnvironmentEdits => {
                self.behind_close = None;
            }
            Confirmed::CloseWindow => {
                self.behind_close = None;
                self.editor = None;
                self.kept_draft = None;
                self.drafts_elsewhere.clear();
                window.remove_window();
                return;
            }
            Confirmed::Group(id) => {
                self.state.update(cx, |state, cx| {
                    if state.delete_group(&id) {
                        cx.notify();
                    }
                });
            }
            Confirmed::Dashboard(reference) => {
                let edited = EditorTarget::Existing(reference.clone());
                if self
                    .editor
                    .as_ref()
                    .is_some_and(|editor| *editor.view.read(cx).target() == edited)
                {
                    self.editor = None;
                }
                if self
                    .kept_draft
                    .as_ref()
                    .is_some_and(|(target, _)| *target == edited)
                {
                    self.kept_draft = None;
                }
                self.state.update(cx, |state, cx| {
                    if state.delete_dashboard(&reference) {
                        cx.notify();
                    }
                });
            }
            Confirmed::Environment(id) => {
                let deleted = match live::session(cx) {
                    Some(session) => {
                        session.update(cx, |session, cx| session.delete_environment(&id, cx))
                    }
                    None => self
                        .state
                        .update(cx, |state, _| state.remove_environment(&id).is_some()),
                };
                if deleted {
                    self.editor = None;
                    self.drafts_elsewhere.remove(&id);
                }
            }
        }
        self.focus_main(window, cx);
        self.pick_up_request(window, cx);
        cx.notify();
    }

    fn on_confirm(&mut self, _: &ConfirmModal, window: &mut Window, cx: &mut Context<Self>) {
        self.confirm(window, cx);
    }

    /// The window is about to close (its close button, the window
    /// manager). It may, unless that loses unsaved work: the dashboard
    /// editor's changes (also those kept for the next edit, in any
    /// environment), the environment editor's, or a dialog with something
    /// typed. Then it asks first and stays open; the dialog comes back if
    /// the window stays.
    pub(crate) fn may_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.modal.as_ref().is_some_and(is_close_question) {
            return false;
        }
        let lost = self.unsaved_work(cx);
        if lost.is_empty() {
            return true;
        }
        // The question replaces the open dialog, or the question that
        // replaced one (that dialog comes back if the window stays).
        let behind = match self.modal.take() {
            Some(modal) if replaces_a_dialog(&modal) => self.behind_close.take(),
            other => other.map(|modal| (modal, self.modal_environment.clone())),
        };
        let confirmation = Confirmation {
            title: "Close the window and lose your changes?".to_owned(),
            detail: format!("Closing it drops {}.", lost.join(" and ")),
            confirm: "close window",
            danger: true,
            action: Confirmed::CloseWindow,
        };
        self.open_modal(OpenModal::Confirm(confirmation), window, cx);
        self.behind_close = behind;
        false
    }

    /// What closing the window would lose, in words.
    fn unsaved_work(&self, cx: &App) -> Vec<String> {
        let mut lost = Vec::new();
        let behind = self.behind_close.as_ref().map(|(modal, _)| modal);
        for modal in self.modal.iter().chain(behind) {
            match modal {
                OpenModal::Action(dialog) => {
                    let dialog = dialog.view.read(cx);
                    if dialog.is_dirty() {
                        lost.push(format!("what you typed in “{}”", dialog.kind().title()));
                    }
                }
                OpenModal::Environment(editor) => {
                    let editor = editor.view.read(cx);
                    if editor.has_changes() {
                        lost.push(format!("your changes to {}", editor.subject()));
                    }
                }
                _ => {}
            }
        }
        if let Some(editor) = &self.editor {
            let editor = editor.view.read(cx);
            if editor.changes().is_some() {
                lost.push(format!("your changes to {}", editor.title()));
            }
        }
        if let Some((_, draft)) = &self.kept_draft {
            let name = draft.name.trim();
            lost.push(if name.is_empty() {
                "the changes kept from your last edit".to_owned()
            } else {
                format!("the changes kept for {name}")
            });
        }
        let state = self.state.read(cx);
        let mut elsewhere: Vec<_> = self
            .drafts_elsewhere
            .iter()
            .filter_map(|(id, (_, draft))| {
                let environment = &state.environment_by_id(id)?.name;
                let name = draft.name.trim();
                Some(if name.is_empty() {
                    format!("the changes kept in {environment}")
                } else {
                    format!("the changes kept for {name} in {environment}")
                })
            })
            .collect();
        elsewhere.sort();
        lost.extend(elsewhere);
        lost
    }

    // --- Environments ---------------------------------------------------

    /// Makes `id` the active environment (ENV-01). The dashboard editor
    /// stays until the switch is seen (`Workspace::environment_changed`
    /// keeps its changes for when this environment is back).
    fn switch_environment(&mut self, id: &str, cx: &mut Context<Self>) {
        match live::session(cx) {
            Some(session) => {
                session.update(cx, |session, cx| session.switch_environment(id, cx));
            }
            None => self.state.update(cx, |state, cx| {
                if state.switch_environment(id) {
                    cx.notify();
                }
            }),
        }
    }

    /// Opens the environment editor for `id` (`None`: a new one).
    pub(crate) fn open_environment_editor(
        &mut self,
        id: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let environment = id.and_then(|id| self.state.read(cx).environment_by_id(id).cloned());
        if id.is_some() && environment.is_none() {
            return;
        }
        let demo = self.state.read(cx).is_demo();
        let editor = cx.new(|cx| {
            EnvironmentEditor::new(environment.as_ref(), EditorMode::Dialog, window, cx)
                .in_demo(demo)
        });
        let events = cx.subscribe_in(
            &editor,
            window,
            |this, _, event: &EnvironmentEditorEvent, window, cx| match event {
                EnvironmentEditorEvent::Close => this.close_modal(window, cx),
                EnvironmentEditorEvent::Cancel => this.leave_environment_editor(window, cx),
                EnvironmentEditorEvent::Delete(id) => {
                    this.confirm_delete_environment(id, window, cx);
                }
            },
        );
        self.open_modal(
            OpenModal::Environment(Held::new(editor, events)),
            window,
            cx,
        );
    }

    fn on_edit_environment(
        &mut self,
        _: &EditEnvironment,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = self
            .state
            .read(cx)
            .active_environment_id()
            .map(str::to_owned);
        match id {
            Some(id) => self.open_environment_editor(Some(&id), window, cx),
            None => self.open_environment_editor(None, window, cx),
        }
    }

    /// The banner's "Restart": a new engine for the active environment.
    #[expect(
        clippy::unused_self,
        reason = "an action handler; the session does the work"
    )]
    fn on_restart_engine(
        &mut self,
        _: &actions::RestartEngine,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(session) = live::session(cx) {
            session.update(cx, live::Session::restart_engine);
        }
    }

    /// The banner's "Review certificate" (ENV-05).
    fn on_review_certificate(
        &mut self,
        _: &ReviewCertificate,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let state = self.state.clone();
        let review = cx.new(|cx| CertificateReview::new(state, cx));
        let events = cx.subscribe_in(
            &review,
            window,
            |this, _, event: &CertificateEvent, window, cx| match event {
                CertificateEvent::Close => this.close_modal(window, cx),
                CertificateEvent::Trust {
                    environment_id,
                    url,
                    fingerprint,
                } => {
                    this.close_modal(window, cx);
                    this.trust_certificate(environment_id, url, fingerprint, cx);
                }
                CertificateEvent::Edit(id) => {
                    let id = id.clone();
                    this.open_environment_editor(Some(&id), window, cx);
                }
            },
        );
        self.open_modal(
            OpenModal::Certificate(Held::new(review, events)),
            window,
            cx,
        );
    }

    fn trust_certificate(
        &mut self,
        environment_id: &str,
        url: &str,
        fingerprint: &str,
        cx: &mut Context<Self>,
    ) {
        match live::session(cx) {
            Some(session) => session.update(cx, |session, cx| {
                session.trust_certificate(environment_id, url, fingerprint, cx);
            }),
            None => self.state.update(cx, |state, cx| {
                state.pin_certificate(environment_id, url, fingerprint);
                cx.notify();
            }),
        }
    }

    // --- Export and import (DASH-06) -------------------------------------

    /// Writes groups `ids` (all when empty) to a file the user chooses.
    pub(crate) fn export_groups(
        &mut self,
        ids: &[String],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (text, name) = {
            let state = self.state.read(cx);
            let text = state.export_groups(ids);
            let name = match ids {
                [one] => state
                    .groups()
                    .iter()
                    .find(|group| &group.id == one)
                    .map_or_else(|| "dashboards".to_owned(), |group| group.name.clone()),
                _ => state.environment().map_or_else(
                    || "dashboards".to_owned(),
                    |environment| environment.name.clone(),
                ),
            };
            (text, format!("{}.icygui-dashboards.toml", file_stem(&name)))
        };
        let text = match text {
            Ok(text) => text,
            Err(error) => {
                self.notify_user(UserNotice::problem("Nothing was exported.", error), cx);
                return;
            }
        };
        let directory = default_directory();
        let prompt = cx.prompt_for_new_path(&directory, Some(&name));
        let fallback = directory.join(&name);
        let window_handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let chosen = prompt.await;
            let _ = cx.update_window(window_handle, |_, window, cx| {
                let _ = this.update(cx, |this, cx| match chosen {
                    Ok(Ok(Some(path))) => Self::write_export(path, text, cx),
                    Ok(Ok(None)) => {}
                    Ok(Err(error)) => {
                        tracing::info!(%error, "no file chooser; asking for a path");
                        this.open_path_prompt(PathPurpose::Export(text), &fallback, window, cx);
                    }
                    Err(_) => {
                        this.open_path_prompt(PathPurpose::Export(text), &fallback, window, cx);
                    }
                });
            });
        })
        .detach();
    }

    /// Reads groups from a file the user chooses and adds them.
    pub(crate) fn import_groups(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.read(cx).environment().is_none() {
            return;
        }
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Import".into()),
        });
        let fallback = default_directory();
        let window_handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let chosen = prompt.await;
            let _ = cx.update_window(window_handle, |_, window, cx| {
                let _ = this.update(cx, |this, cx| match chosen {
                    Ok(Ok(Some(paths))) => {
                        if let Some(path) = paths.into_iter().next() {
                            Self::read_import(path, cx);
                        }
                    }
                    Ok(Ok(None)) => {}
                    Ok(Err(error)) => {
                        tracing::info!(%error, "no file chooser; asking for a path");
                        this.open_path_prompt(PathPurpose::Import, &fallback, window, cx);
                    }
                    Err(_) => this.open_path_prompt(PathPurpose::Import, &fallback, window, cx),
                });
            });
        })
        .detach();
    }

    /// Opens the path dialog as if no file chooser were available: to
    /// export `export` (when given), else to import.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn ask_for_path(
        &mut self,
        export: Option<String>,
        suggested: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let purpose = export.map_or(PathPurpose::Import, PathPurpose::Export);
        self.open_path_prompt(purpose, suggested, window, cx);
    }

    /// Asks for a path by hand (no file chooser on this desktop).
    fn open_path_prompt(
        &mut self,
        purpose: PathPurpose,
        suggested: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let value = suggested.display().to_string();
        let input = cx.new(|cx| InputState::new(window, cx).default_value(value));
        let events = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.finish_path_prompt(cx);
            }
        });
        self.open_modal(
            OpenModal::Path(PathPrompt {
                purpose,
                input,
                error: None,
                _events: events,
            }),
            window,
            cx,
        );
    }

    /// Carries out the path dialog.
    fn finish_path_prompt(&mut self, cx: &mut Context<Self>) {
        let Some(OpenModal::Path(prompt)) = &mut self.modal else {
            return;
        };
        let typed = prompt.input.read(cx).value().trim().to_owned();
        if typed.is_empty() {
            prompt.error = Some("Enter a file's path.".to_owned());
            cx.notify();
            return;
        }
        let path = expand_home(&typed);
        let purpose = prompt.purpose.clone();
        self.modal = None;
        match purpose {
            PathPurpose::Export(text) => Self::write_export(path, text, cx),
            PathPurpose::Import => Self::read_import(path, cx),
        }
        cx.notify();
    }

    /// Writes an export off the UI thread and says how it went.
    fn write_export(path: PathBuf, text: String, cx: &mut Context<Self>) {
        let target = path.clone();
        let write = cx
            .background_executor()
            .spawn(async move { std::fs::write(&target, text) });
        cx.spawn(async move |this, cx| {
            let written = write.await;
            let _ = this.update(cx, |this, cx| {
                let notice = match written {
                    Ok(()) => {
                        UserNotice::info("Dashboards exported.", Some(path.display().to_string()))
                    }
                    Err(error) => UserNotice::problem(
                        "The dashboards couldn't be exported.",
                        format!("{}: {error}", path.display()),
                    ),
                };
                this.notify_user(notice, cx);
            });
        })
        .detach();
    }

    /// Reads an export off the UI thread, then adds its groups.
    fn read_import(path: PathBuf, cx: &mut Context<Self>) {
        let source = path.clone();
        let read = cx.background_executor().spawn(async move {
            let text = std::fs::read_to_string(&source).map_err(|error| error.to_string())?;
            ic_config::import_groups(&text).map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let imported = read.await;
            let _ = this.update(cx, |this, cx| {
                let notice = match imported {
                    Ok(groups) => {
                        let count = this
                            .state
                            .update(cx, |state, _| state.import_groups(groups));
                        UserNotice::info(
                            match count {
                                1 => "1 group of dashboards imported.".to_owned(),
                                count => format!("{count} groups of dashboards imported."),
                            },
                            Some(path.display().to_string()),
                        )
                    }
                    Err(error) => UserNotice::problem(
                        "Nothing was imported.",
                        format!("{}: {error}", path.display()),
                    ),
                };
                this.notify_user(notice, cx);
            });
        })
        .detach();
    }

    /// Shows a message in the banner area.
    fn notify_user(&self, notice: UserNotice, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            state.report(notice);
            cx.notify();
        });
    }

    // --- Rendering ------------------------------------------------------

    fn render_modal(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let modal = self.modal.as_ref()?;
        let theme = cx.theme().clone();
        let (width, placement, content): (f32, ModalPlacement, AnyElement) = match modal {
            OpenModal::Palette(palette) => (
                640.,
                ModalPlacement::Top(px(70.)),
                palette.view.clone().into_any_element(),
            ),
            OpenModal::Environment(editor) => (
                620.,
                ModalPlacement::Center,
                editor.view.clone().into_any_element(),
            ),
            OpenModal::Certificate(review) => (
                560.,
                ModalPlacement::Center,
                review.view.clone().into_any_element(),
            ),
            OpenModal::Action(dialog) => (
                // The removal's list is narrower, as drawn (topic 01).
                if dialog.view.read(cx).kind() == DialogKind::RemoveDowntime {
                    520.
                } else {
                    580.
                },
                ModalPlacement::Center,
                dialog.view.clone().into_any_element(),
            ),
            OpenModal::Confirm(confirmation) => (
                460.,
                ModalPlacement::Center,
                self.render_confirmation(confirmation, &theme, cx),
            ),
            OpenModal::Path(prompt) => (
                520.,
                ModalPlacement::Center,
                Self::render_path_prompt(prompt, cx),
            ),
            OpenModal::About => (460., ModalPlacement::Center, self.render_about(cx)),
            OpenModal::Removal(dialog) => (
                dialog.view.read(cx).width(),
                ModalPlacement::Center,
                dialog.view.clone().into_any_element(),
            ),
        };
        Some(
            div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .key_context(MODAL_CONTEXT)
                .on_action(cx.listener(Self::on_close_modal))
                .on_action(cx.listener(Self::on_modal_escape))
                .child(
                    Modal::new("modal", px(width), content)
                        .placement(placement)
                        .on_dismiss(cx.listener(|this, _: &MouseDownEvent, window, cx| {
                            // A dialog with something typed stays: a stray
                            // click must not lose it (Escape still closes).
                            let dirty = match &this.modal {
                                Some(OpenModal::Action(dialog)) => dialog.view.read(cx).is_dirty(),
                                Some(OpenModal::Environment(editor)) => {
                                    editor.view.read(cx).has_changes()
                                }
                                _ => false,
                            };
                            if !dirty {
                                this.close_modal(window, cx);
                            }
                        })),
                )
                .into_any_element(),
        )
    }

    fn render_confirmation(
        &self,
        confirmation: &Confirmation,
        theme: &Theme,
        cx: &Context<Self>,
    ) -> AnyElement {
        div()
            .id("confirmation")
            .key_context(CONFIRM_CONTEXT)
            .track_focus(&self.modal_focus)
            .on_action(cx.listener(Self::on_confirm))
            .child(
                DialogBody::new(confirmation.title.clone())
                    .child(
                        div()
                            .text_color(theme.colors.text_muted)
                            .child(confirmation.detail.clone()),
                    )
                    .action(
                        Button::new("confirm-cancel", "cancel")
                            .key_hint("esc")
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.close_modal(window, cx);
                            })),
                    )
                    .action(
                        Button::new("confirm-ok", confirmation.confirm)
                            .variant(if confirmation.danger {
                                ButtonVariant::Danger
                            } else {
                                ButtonVariant::Primary
                            })
                            .key_hint("↵")
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.confirm(window, cx);
                            })),
                    ),
            )
            .into_any_element()
    }

    fn render_about(&self, cx: &Context<Self>) -> AnyElement {
        let facts = live::session(cx)
            .map(|session| session.read(cx).about_facts())
            .unwrap_or_default();
        div()
            .id("about")
            .track_focus(&self.modal_focus)
            .child(crate::settings::about::render(
                &facts,
                Button::new("about-close", "close")
                    .key_hint("esc")
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.close_modal(window, cx);
                    })),
                cx,
            ))
            .into_any_element()
    }

    fn render_path_prompt(prompt: &PathPrompt, cx: &Context<Self>) -> AnyElement {
        let (title, action, hint) = match prompt.purpose {
            PathPurpose::Export(_) => (
                "Export dashboards",
                "export",
                "No file chooser is available here: type where to write the file.",
            ),
            PathPurpose::Import => (
                "Import dashboards",
                "import",
                "No file chooser is available here: type the export file's path.",
            ),
        };
        DialogBody::new(title)
            .child(
                Field::new("file")
                    .control(
                        TextField::new(&prompt.input)
                            .bordered(true)
                            .invalid(prompt.error.is_some()),
                    )
                    .error(prompt.error.clone())
                    .hint(hint),
            )
            .action(
                Button::new("path-cancel", "cancel")
                    .key_hint("esc")
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.close_modal(window, cx);
                    })),
            )
            .action(
                Button::new("path-ok", action).primary().on_click(
                    cx.listener(|this, _: &ClickEvent, _, cx| this.finish_path_prompt(cx)),
                ),
            )
            .into_any_element()
    }
}

/// Whether `modal` asks before closing the window.
fn is_close_question(modal: &OpenModal) -> bool {
    matches!(
        modal,
        OpenModal::Confirm(Confirmation {
            action: Confirmed::CloseWindow,
            ..
        })
    )
}

/// Whether `modal` is a question that replaced a dialog with unsaved work
/// (`Workspace::behind_close`), which comes back when it is answered no.
fn replaces_a_dialog(modal: &OpenModal) -> bool {
    matches!(
        modal,
        OpenModal::Confirm(Confirmation {
            action: Confirmed::CloseWindow | Confirmed::DiscardEnvironmentEdits,
            ..
        })
    )
}

/// Our window controls' close button: closes the window unless that loses
/// unsaved work, like the window manager's close
/// ([`Workspace::may_close`]).
pub(crate) fn close_window(window: &mut Window, cx: &mut App) {
    let workspace = window
        .root::<Root>()
        .flatten()
        .and_then(|root| root.read(cx).view().clone().downcast::<Workspace>().ok());
    let close = workspace.is_none_or(|workspace| {
        workspace.update(cx, |workspace, cx| workspace.may_close(window, cx))
    });
    if close {
        window.remove_window();
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(problem) = self.state.read(cx).config_problem().cloned() {
            return div()
                .id("workspace")
                .size_full()
                .font_family(cx.theme().font_family.clone())
                .line_height(cx.theme().line_height)
                .text_color(cx.theme().colors.text)
                .child(recovery::render(&problem, &self.drag, window, cx))
                .into_any_element();
        }
        let main = if let Some((onboarding, _)) = &self.onboarding {
            onboarding.clone().into_any_element()
        } else if let Some(editor) = &self.editor {
            editor.view.clone().into_any_element()
        } else {
            match &self.shown {
                Shown::Tab(key) => self
                    .tabs
                    .get(key)
                    .map(|tab| tab.view.clone().into_any_element()),
                Shown::List(kind) => self
                    .lists
                    .get(kind)
                    .map(|list| list.view.clone().into_any_element()),
                Shown::Dashboard(_) => None,
            }
            .unwrap_or_else(|| self.dashboard.clone().into_any_element())
        };
        let modal = self.render_modal(cx);
        let settings = self.render_settings(cx);
        let modal_open = self.modal.is_some() || self.settings.is_some();
        // Above the list's selection bar while rows are marked.
        let bar = self.editor.is_none()
            && self.onboarding.is_none()
            && match &self.shown {
                Shown::Dashboard(_) => self.dashboard.read(cx).has_marks(cx),
                Shown::List(kind) => self
                    .lists
                    .get(kind)
                    .is_some_and(|list| list.view.read(cx).has_marks()),
                Shown::Tab(_) => false,
            };
        let toasts = crate::operate::toasts::render(
            &self.state,
            if bar {
                16. + crate::dashboard::SELECTION_BAR_HEIGHT
            } else {
                16.
            },
            cx,
        );
        let theme = cx.theme();
        div()
            .id("workspace")
            .key_context(WORKSPACE_CONTEXT)
            .relative()
            // Runs after the element under the mouse had its say: one that
            // takes the focus (the list, a tab, the search field), or keeps
            // it where it is (a menu trigger), prevents the default.
            .on_any_mouse_down(cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                if !modal_open && !window.default_prevented() {
                    this.focus_main(window, cx);
                }
            }))
            .on_action(cx.listener(Self::toggle_sidebar))
            .on_action(cx.listener(Self::on_focus_main))
            .on_action(cx.listener(Self::select_dashboard))
            .on_action(cx.listener(Self::next_tab))
            .on_action(cx.listener(Self::previous_tab))
            .on_action(cx.listener(Self::close_tab))
            .on_action(cx.listener(Self::toggle_palette))
            .on_action(cx.listener(Self::on_new_dashboard))
            .on_action(cx.listener(Self::on_review_certificate))
            .on_action(cx.listener(Self::on_edit_environment))
            .on_action(cx.listener(Self::on_restart_engine))
            .on_action(cx.listener(Self::on_open_settings))
            .on_action(cx.listener(Self::on_show_about))
            .on_action(cx.listener(Self::on_open_notifications))
            .flex()
            .size_full()
            .bg(theme.colors.window_background)
            .font_family(theme.font_family.clone())
            .line_height(theme.line_height)
            .text_color(theme.colors.text)
            .when(self.sidebar_open, |workspace| {
                workspace.child(self.sidebar.clone())
            })
            .child(div().flex().flex_1().min_w_0().h_full().child(main))
            .children(settings)
            .children(toasts)
            .children(modal)
            .into_any_element()
    }
}

/// The question to ask before sending `spec`, if it needs one: checking
/// many objects at once (a burst of work for the satellites). (Removing
/// downtimes, and several acknowledgements, have their own dialogs, which
/// list every target.)
pub(crate) fn confirmation_for(
    spec: &ActionSpec,
    eligible: &forms::Eligible,
) -> Option<Confirmation> {
    let what = describe_objects(&spec.objects);
    let skipped = if eligible.skipped.is_empty() {
        String::new()
    } else {
        format!(" Skipped: {}.", eligible.skipped_summary())
    };
    let (title, detail, confirm, danger) = match &spec.kind {
        actions::ObjectAction::CheckNow if spec.objects.len() > CHECK_CONFIRM_ABOVE => (
            format!("Check {what} now?"),
            format!(
                "Icinga runs their checks at once, forced, on the endpoints that run them.{skipped}"
            ),
            "check now",
            false,
        ),
        _ => return None,
    };
    Some(Confirmation {
        title,
        detail,
        confirm,
        danger,
        action: Confirmed::Action(spec.clone()),
    })
}

/// The window title for the active environment.
fn title_of(state: &AppState) -> SharedString {
    chrome::window_title(
        state
            .environment()
            .map(|environment| environment.name.as_str()),
        state.is_demo_environment(),
    )
}

/// Where file prompts start: `~/Downloads` if it exists, else home, else
/// the current directory.
fn default_directory() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match home {
        Some(home) if home.join("Downloads").is_dir() => home.join("Downloads"),
        Some(home) => home,
        None => PathBuf::from("."),
    }
}

/// `name` as a file name: letters, digits, `-`, `_` and `.`, the rest `-`.
fn file_stem(name: &str) -> String {
    let stem: String = name
        .trim()
        .chars()
        .map(|character| {
            if character.is_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '-'
            }
        })
        .collect();
    if stem.is_empty() {
        "dashboards".to_owned()
    } else {
        stem
    }
}

/// A typed path with `~/` meaning the home directory.
fn expand_home(text: &str) -> PathBuf {
    match (text.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(text),
    }
}

/// The window controls and a button to bring the sidebar back, for the main
/// area's header while the sidebar is hidden.
pub(crate) fn sidebar_reopen(controls: Controls, theme: &Theme) -> impl IntoElement + use<> {
    let metrics = theme.metrics;
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(8.))
        // Keep the controls where the sidebar header has them (12px in).
        .ml(metrics.sidebar_padding - metrics.list_padding)
        .when(controls != Controls::None, |row| {
            row.child(WindowControls::new(controls)).child(
                Divider::vertical()
                    .color(DividerColor::Window)
                    .length(px(18.))
                    .margin(px(6.)),
            )
        })
        .child(
            IconButton::new("show-sidebar", IconName::PanelLeft)
                .icon_size(theme.metrics.icon_small)
                .tooltip(Tooltip::new("Show sidebar"))
                .on_click(|_, window, cx| window.dispatch_action(Box::new(ToggleSidebar), cx)),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_names_are_safe_file_names() {
        assert_eq!(file_stem("prod cluster/db"), "prod-cluster-db");
        assert_eq!(file_stem("  "), "dashboards");
        assert_eq!(file_stem("ops_2.0"), "ops_2.0");
    }

    #[test]
    fn typed_paths_expand_the_home_directory() {
        if let Some(home) = std::env::var_os("HOME") {
            assert_eq!(expand_home("~/x.toml"), PathBuf::from(home).join("x.toml"));
        }
        assert_eq!(expand_home("/tmp/x"), PathBuf::from("/tmp/x"));
    }
}
