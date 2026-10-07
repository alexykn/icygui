//! The data the window renders and the app's outbound half: the
//! configuration, the latest snapshot of the active environment, the
//! connection, the selection and the objects open as tabs, plus the link
//! to the core and the persistence of settings and UI state.
//!
//! [`AppState`] lives in a GPUI entity; views observe it and re-render when
//! it notifies. The core's events arrive through [`AppState::apply`]
//! (`crate::live` pumps them in); what the user changes goes out from here:
//!
//! - view changes (sort, group-by, the handled toggle) and collapsed
//!   groups send `Command::UpdateEnvironment` and save the settings
//!   (DASH-07);
//! - opening and closing tabs and selecting dashboards save the UI state
//!   (PANE-03);
//! - [`AppState::hydrate`] asks for the details of the rows on screen and
//!   opened panes, [`AppState::refresh`] reloads, both gently.
//!
//! Methods here never touch GPUI, so they're tested directly.

pub(crate) mod connection;
pub(crate) mod editing;
mod engines;
pub(crate) mod environments;
pub(crate) mod hydration;
mod notifications;
mod operations;
pub(crate) mod permissions;
mod presence;
pub(crate) mod settings_file;

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ic_config::{
    AuthConfig, Config, Dashboard, DashboardGroup, Environment, EnvironmentUiState, HideHandled,
    ListOptionsState, MAX_TABS, ObjectKind, UiState, View, WindowState,
};
use ic_core::snapshot::{DashboardResult, Snapshot, ViewResult};
use ic_core::{ApiInfo, Command, ConnectionState, CoreEvent, CoreHandle};
use ic_model::{ObjectKey, ServiceKey, Timestamp};
use ic_rules::DashboardRef;

pub(crate) use self::connection::{
    ConnectionNotice, ConnectionStatus, Health, NoticeAction, NoticeKind, Tone,
};
pub(crate) use self::engines::EngineSlot;
#[cfg(test)]
pub(crate) use self::notifications::GroupPlan;
pub(crate) use self::notifications::NotificationPlan;
#[cfg(all(test, target_os = "linux"))]
pub(crate) use self::operations::NOT_CONNECTED;
use crate::actions::{ActionRequest, ObjectAction};
#[cfg(test)]
use crate::fixture::{self, FixtureOptions};
use crate::lists::ListKind;
use crate::operate::tracker::Tracker;
use crate::persist::{Persistence, SaveReport};

/// The fewest seconds between two reloads the user asks for; Icinga is
/// spared a click storm (the core spaces reloads to 30 s on top).
const REFRESH_SPACING: Duration = Duration::from_secs(2);

/// How many recent notifications are kept for the notification centre.
const MAX_NOTIFICATIONS: usize = 200;

/// Where commands for the core go: the running engine, or a recorder in
/// tests.
pub(crate) trait CoreLink: fmt::Debug {
    /// Sends a command; never blocks.
    fn send(&self, command: Command);
    /// Stops the engine on another thread (its bounded wait), so replacing
    /// or deleting an environment never freezes the window and engines
    /// stop side by side at quit. The receiver completes (or is cancelled)
    /// once it has stopped.
    fn shutdown_in_background(self: Box<Self>) -> futures::channel::oneshot::Receiver<()>;
}

impl CoreLink for CoreHandle {
    fn send(&self, command: Command) {
        Self::send(self, command);
    }

    fn shutdown_in_background(self: Box<Self>) -> futures::channel::oneshot::Receiver<()> {
        let (done, stopped) = futures::channel::oneshot::channel();
        let spawned = std::thread::Builder::new()
            .name("icygui-core-stop".to_owned())
            .spawn(move || {
                (*self).shutdown();
                let _ = done.send(());
            });
        if let Err(error) = spawned {
            // The handle went down with the closure, which signals the
            // engine to stop; the dropped sender cancels the receiver.
            tracing::error!(%error, "no thread to stop the engine on; it stops by itself");
        }
        stopped
    }
}

/// How the app was started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Against the configured environments; settings are saved.
    Live,
    /// `--demo`: the built-in simulated Icinga; nothing is saved.
    Demo,
    /// The design's sample data, evaluated in the app (tests only).
    #[cfg(test)]
    Fixture,
}

/// A settings file that can't be read (OPS-06): the app offers to restore
/// the backup, start fresh, try again or quit, and never replaces the file
/// without asking.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ConfigProblem {
    /// Why it can't be read (with line and column for syntax errors).
    pub(crate) message: String,
    /// The settings file.
    pub(crate) path: PathBuf,
    /// The backup copy: loaded, absent, or itself unreadable.
    pub(crate) backup: Result<Option<Config>, String>,
    /// Written by a newer icygui (starting fresh still keeps it).
    pub(crate) newer: bool,
    /// A choice is being carried out.
    pub(crate) busy: bool,
    /// The last choice failed, and why.
    pub(crate) failure: Option<String>,
}

/// A message about something the user did: an export written, an import
/// that failed, a password that couldn't be removed. Shown as a banner
/// until dismissed (or replaced).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UserNotice {
    /// What happened.
    pub(crate) title: String,
    /// More about it.
    pub(crate) detail: Option<String>,
    /// Whether it went wrong (a warning, not information).
    pub(crate) problem: bool,
}

impl UserNotice {
    /// Something that worked.
    pub(crate) fn info(title: impl Into<String>, detail: Option<String>) -> Self {
        Self {
            title: title.into(),
            detail,
            problem: false,
        }
    }

    /// Something that went wrong.
    pub(crate) fn problem(title: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            detail: Some(detail.into()),
            problem: true,
        }
    }
}

/// What [`AppState::hydrate`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Hydrated {
    /// Asked the core for these.
    Sent(Vec<ObjectKey>),
    /// Everything was asked for recently.
    Nothing,
    /// Not connected (or no core): ask again later.
    NotNow,
}

/// Application data shared by the views, and the outbound half.
#[derive(Debug)]
pub(crate) struct AppState {
    config: Config,
    ui: UiState,
    /// The active environment's engine: its link, snapshot, connection,
    /// permissions and notifications (what the window shows).
    engine: EngineSlot,
    /// Every other environment's engine (they all run, PLAN.md D2), by
    /// environment id: swapped with `engine` when the active environment
    /// changes, so switching is instant.
    parked: BTreeMap<String, EngineSlot>,
    selected: Option<DashboardRef>,
    /// Objects opened as tabs ("↗ open as tab"), in the order they were
    /// opened.
    tabs: Vec<ObjectKey>,
    /// The tab shown instead of the selected dashboard.
    active_tab: Option<ObjectKey>,
    /// The lists of every downtime, comment and acknowledged problem open
    /// as tabs (topic 07), in sidebar order (which is [`ListKind::ALL`]'s).
    lists: Vec<ListKind>,
    /// The list shown instead of the selected dashboard (never together
    /// with `active_tab`).
    active_list: Option<ListKind>,
    /// Each list's choices in the active environment, by list id (kept
    /// with the UI state).
    list_options: BTreeMap<String, ListOptionsState>,
    /// When this client started recording events (the history tab's
    /// "recorded locally since …").
    started_at: Timestamp,
    mode: Mode,
    persistence: Option<Persistence>,
    /// The settings changed before persistence was attached (an active
    /// environment picked at start).
    config_dirty: bool,
    /// Until when notifications are paused in every environment (NOTE-04,
    /// A5: the default pause).
    paused_until: Option<Timestamp>,
    /// Until when each environment's own notifications are paused (from
    /// the switcher or the palette), by environment id.
    environment_pauses: BTreeMap<String, Timestamp>,
    config_problem: Option<ConfigProblem>,
    save_error: Option<String>,
    dismissed_save_error: Option<String>,
    /// Why the settings file, edited by hand, can't be read: icygui
    /// writes nothing over it until it reads again.
    file_error: Option<String>,
    notice: Option<UserNotice>,
    /// An action the user asked for, until the workspace picks it up (and
    /// opens its dialog, asks, or sends it), and the environment it is for
    /// (`None`: the active one; a desktop notification's *Acknowledge*
    /// goes to its own environment without switching).
    requested: Option<(ActionRequest, Option<String>)>,
    /// The last action the user asked for (tests read it).
    last_request: Option<ActionRequest>,
    /// Why the last action asked for was refused.
    last_denial: Option<String>,
    /// The actions sent: markers, failures, toasts.
    tracker: Tracker,
    /// The id of the last action sent (ids are never reused, not even
    /// across environments).
    last_action_id: u64,
    /// The main window is hidden (closed to the tray, out of sight, or not
    /// opened yet in a start in the background): the environment on
    /// screen is quiet too (PERF-09).
    window_hidden: bool,
    /// The window has shown since the app started: engines start their
    /// first load at once (`ic_core::Start::User`).
    user_present: bool,
    /// Counts the times the environment on screen woke up from quiet mode
    /// (or another came on screen).
    wake: u64,
    /// Evaluates dashboards for the fixture; the core does that itself.
    #[cfg(test)]
    evaluator: Option<fixture::Evaluator>,
    /// The event log's entries for the fixture (newest first); the core
    /// answers from its log otherwise.
    #[cfg(test)]
    fake_history: Option<Vec<ic_core::LogEntry>>,
}

impl AppState {
    fn base(config: Config, ui: UiState, mode: Mode, now: Timestamp) -> Self {
        Self {
            config,
            ui,
            engine: EngineSlot::idle(),
            parked: BTreeMap::new(),
            selected: None,
            tabs: Vec::new(),
            active_tab: None,
            lists: Vec::new(),
            active_list: None,
            list_options: BTreeMap::new(),
            started_at: now,
            mode,
            persistence: None,
            config_dirty: false,
            paused_until: None,
            environment_pauses: BTreeMap::new(),
            config_problem: None,
            save_error: None,
            dismissed_save_error: None,
            file_error: None,
            notice: None,
            requested: None,
            last_request: None,
            last_denial: None,
            tracker: Tracker::default(),
            last_action_id: 0,
            // A window opens at start unless `start_hidden` says otherwise.
            window_hidden: false,
            user_present: true,
            wake: 0,
            #[cfg(test)]
            evaluator: None,
            #[cfg(test)]
            fake_history: None,
        }
    }

    /// The configured environments, with the UI state of the last run. The
    /// active environment (the first one if none is marked) connects once
    /// the core starts.
    pub(crate) fn live(config: Config, ui: UiState, now: Timestamp) -> Self {
        let mut state = Self::base(Config::default(), ui, Mode::Live, now);
        state.adopt_config(config);
        state
    }

    /// The `--demo` environment (`config` holds exactly it). Nothing is
    /// saved; the connection says it's starting until the demo server is up.
    pub(crate) fn demo(config: Config, now: Timestamp) -> Self {
        let mut state = Self::base(config, UiState::default(), Mode::Demo, now);
        state.restore_environment_ui();
        state.engine.connection = ConnectionStatus::starting(
            crate::live::demo::ENDPOINT,
            Some(crate::live::demo::USER.to_owned()),
        );
        state
    }

    /// The settings file can't be read: the window offers what to do.
    pub(crate) fn recovery(problem: ConfigProblem, ui: UiState, now: Timestamp) -> Self {
        let mut state = Self::base(Config::default(), ui, Mode::Live, now);
        state.config_problem = Some(problem);
        state
    }

    /// Takes `config` as the settings (at start, or once the settings file
    /// was restored or started fresh): picks the active environment and
    /// restores its selection and tabs.
    pub(crate) fn adopt_config(&mut self, mut config: Config) {
        let active_valid = config
            .active_environment
            .as_deref()
            .is_some_and(|id| config.environment(id).is_some());
        if !active_valid {
            let first = config
                .environments
                .first()
                .map(|environment| environment.id.clone());
            if config.active_environment != first {
                config.active_environment = first;
                self.config_dirty = true;
            }
        }
        let known: std::collections::BTreeSet<String> = config
            .environments
            .iter()
            .map(|environment| environment.id.clone())
            .collect();
        self.ui.retain_environments(|id| known.contains(id));
        self.environment_pauses.retain(|id, _| known.contains(id));
        self.parked.clear();
        self.config = config;
        self.config_problem = None;
        self.reset_connection();
        if self.config_dirty && self.persistence.is_some() {
            self.config_dirty = false;
            self.save_config();
        }
    }

    /// Restores the active environment's selected dashboard and tabs from
    /// the UI state; selects the first dashboard otherwise.
    fn restore_environment_ui(&mut self) {
        let Some(id) = self.config.active_environment.clone() else {
            self.selected = None;
            self.tabs.clear();
            self.active_tab = None;
            self.lists.clear();
            self.active_list = None;
            self.list_options.clear();
            return;
        };
        let saved = self.ui.environment(&id);
        self.list_options = saved.list_options.clone();
        self.selected = saved
            .selected
            .filter(|reference| self.dashboard(reference).is_some())
            .or_else(|| self.first_dashboard());
        self.tabs = saved
            .tabs
            .iter()
            .filter_map(|name| parse_object(name))
            .take(MAX_TABS)
            .collect();
        self.active_tab = None;
        self.lists = saved
            .lists
            .iter()
            .filter_map(|id| ListKind::from_id(id))
            .collect();
        self.lists.sort();
        self.lists.dedup();
        self.active_list = None;
    }

    /// The first dashboard in sidebar order.
    fn first_dashboard(&self) -> Option<DashboardRef> {
        self.environment()?.groups.iter().find_map(|group| {
            group.dashboards.first().map(|dashboard| DashboardRef {
                group_id: group.id.clone(),
                dashboard_id: dashboard.id.clone(),
            })
        })
    }

    /// Attaches the writer that saves settings and UI state (live mode).
    /// Saves the settings at once if they changed at start.
    pub(crate) fn set_persistence(&mut self, persistence: Persistence) {
        self.persistence = Some(persistence);
        if std::mem::take(&mut self.config_dirty) {
            self.save_config();
        }
    }

    /// Writes everything queued, and the latest UI state, waiting at most
    /// `timeout` (at quit). Returns whether it all got written.
    pub(crate) fn flush_persistence(&self, timeout: Duration) -> bool {
        self.save_ui();
        self.persistence
            .as_ref()
            .is_none_or(|persistence| persistence.flush(timeout))
    }

    /// Connects the outbound half to the active environment's running
    /// core (tests; the session uses [`AppState::set_core_for`]).
    #[cfg(test)]
    pub(crate) fn set_core(&mut self, core: Box<dyn CoreLink>) {
        self.engine.core = Some(core);
        self.resend_pause();
    }

    /// Disconnects the outbound half of the active environment (tests).
    #[cfg(test)]
    pub(crate) fn take_core(&mut self) -> Option<Box<dyn CoreLink>> {
        self.engine.core.take()
    }

    /// Sends `command` to the active environment's engine.
    fn send(&self, command: Command) {
        self.engine.send(command);
    }

    /// Whether this is the `--demo` session (or the test fixture): nothing
    /// is saved.
    pub(crate) fn is_demo(&self) -> bool {
        self.mode != Mode::Live
    }

    /// Whether the active environment is simulated: one of `--demo`'s
    /// built-in environments (or the test fixture), labelled "demo" in the
    /// title, the footer and the tray (ENV-10). An environment added
    /// while the demo runs talks to a real Icinga, and its actions are
    /// real, so it isn't.
    pub(crate) fn is_demo_environment(&self) -> bool {
        self.active_environment_id()
            .is_some_and(|id| self.is_demo_environment_id(id))
    }

    /// Whether environment `id` is simulated (see
    /// [`AppState::is_demo_environment`]).
    pub(crate) fn is_demo_environment_id(&self, id: &str) -> bool {
        match self.mode {
            Mode::Live => false,
            Mode::Demo => crate::live::demo::is_built_in(id),
            #[cfg(test)]
            Mode::Fixture => true,
        }
    }

    /// The settings.
    pub(crate) fn config(&self) -> &Config {
        &self.config
    }

    /// How the app looks (theme, interface size, row density, times in
    /// lists), as the settings say. The settings panel changes it
    /// ([`AppState::set_appearance`]); the views read it here.
    pub(crate) fn appearance(&self) -> &ic_config::Appearance {
        &self.config.appearance
    }

    /// The UI state (window, tabs and selections) to save.
    #[cfg(test)]
    pub(crate) fn ui_state(&self) -> &UiState {
        &self.ui
    }

    /// The settings file that couldn't be read, while the app asks what to
    /// do about it.
    pub(crate) fn config_problem(&self) -> Option<&ConfigProblem> {
        self.config_problem.as_ref()
    }

    /// Updates the recovery screen (a choice is running, or failed).
    pub(crate) fn update_config_problem(&mut self, update: impl FnOnce(&mut ConfigProblem)) {
        if let Some(problem) = &mut self.config_problem {
            update(problem);
        }
    }

    /// The active environment, if any.
    pub(crate) fn environment(&self) -> Option<&Environment> {
        let active = self.config.active_environment.as_deref()?;
        self.config.environment(active)
    }

    /// Points the demo environment `id` at its demo servers once they
    /// run: `(URL, pinned certificate)` in order of preference.
    pub(crate) fn set_demo_servers(&mut self, id: &str, servers: &[(String, Option<String>)]) {
        if let Some(environment) = self.config.environment_mut(id) {
            environment.urls = servers
                .iter()
                .map(|(url, fingerprint)| ic_config::ApiUrl {
                    pinned_sha256: fingerprint.clone(),
                    ..ic_config::ApiUrl::new(url)
                })
                .collect();
            environment.tls.use_system_roots = false;
        }
    }

    /// The latest snapshot of the active environment.
    pub(crate) fn snapshot(&self) -> &Arc<Snapshot> {
        &self.engine.snapshot
    }

    /// Whether no objects have arrived yet (connecting, or the first load
    /// still running).
    pub(crate) fn has_no_objects(&self) -> bool {
        self.engine.snapshot.hosts.is_empty() && self.engine.snapshot.services.is_empty()
    }

    /// The connection to the active environment.
    pub(crate) fn connection(&self) -> &ConnectionStatus {
        &self.engine.connection
    }

    /// The connection problem to show at `now`, if any.
    pub(crate) fn connection_notice(&self, now: Timestamp) -> Option<ConnectionNotice> {
        let name = self
            .environment()
            .map_or("the environment", |environment| environment.name.as_str());
        self.engine.connection.notice(name, now)
    }

    /// The API user and its permissions, once connected.
    pub(crate) fn permissions(&self) -> Option<&ApiInfo> {
        self.engine.permissions.as_ref()
    }

    /// Why the user may not run `action`, if it may not (ENV-09).
    pub(crate) fn action_denial(&self, action: &ObjectAction) -> Option<String> {
        permissions::action_denial(self.engine.permissions.as_ref(), action)
    }

    /// Why the user may not read objects of `kind`, if it may not.
    pub(crate) fn query_denial(&self, kind: ObjectKind) -> Option<String> {
        permissions::query_denial(self.engine.permissions.as_ref(), kind)
    }

    /// Why the user may not read what list `kind` shows, if it may not.
    pub(crate) fn list_denial(&self, kind: ListKind) -> Option<String> {
        permissions::list_denial(self.engine.permissions.as_ref(), kind)
    }

    /// Why *only mine* can't be used in the list `kind`: no author, or
    /// (the acknowledged list) no comments to read who acknowledged.
    pub(crate) fn only_mine_denial(&self, kind: ListKind) -> Option<String> {
        if self.author().is_empty() {
            return Some("No author is set for this environment".to_owned());
        }
        match kind {
            ListKind::Acknowledged => {
                permissions::ack_detail_denial(self.engine.permissions.as_ref())
            }
            ListKind::Downtimes | ListKind::Comments => None,
        }
    }

    /// Why the acknowledged list can't say who acknowledged and why, if it
    /// can't.
    pub(crate) fn ack_detail_denial(&self) -> Option<String> {
        permissions::ack_detail_denial(self.engine.permissions.as_ref())
    }

    /// The active environment's author (*only mine*): its `author`, else
    /// the API user; empty without either.
    pub(crate) fn author(&self) -> &str {
        self.environment().map_or("", Environment::author_name)
    }

    /// Whether the user may read who Icinga notified (`None`: unknown yet).
    pub(crate) fn can_read_notifications(&self) -> Option<bool> {
        permissions::can_read_notifications(self.engine.permissions.as_ref())
    }

    /// When this client started recording events.
    pub(crate) fn started_at(&self) -> Timestamp {
        self.started_at
    }

    /// The selected dashboard.
    pub(crate) fn selected(&self) -> Option<&DashboardRef> {
        self.selected.as_ref()
    }

    /// The selected dashboard and its group.
    #[cfg(test)]
    pub(crate) fn selected_dashboard(&self) -> Option<(&DashboardGroup, &Dashboard)> {
        self.dashboard(self.selected.as_ref()?)
    }

    /// A dashboard and its group by reference.
    pub(crate) fn dashboard(
        &self,
        reference: &DashboardRef,
    ) -> Option<(&DashboardGroup, &Dashboard)> {
        let group = self
            .environment()?
            .groups
            .iter()
            .find(|group| group.id == reference.group_id)?;
        let dashboard = group
            .dashboards
            .iter()
            .find(|dashboard| dashboard.id == reference.dashboard_id)?;
        Some((group, dashboard))
    }

    /// The first dashboard called `name` (case-insensitive).
    pub(crate) fn dashboard_named(&self, name: &str) -> Option<DashboardRef> {
        let name = name.to_lowercase();
        self.environment()?.groups.iter().find_map(|group| {
            group
                .dashboards
                .iter()
                .find(|dashboard| dashboard.name.to_lowercase() == name)
                .map(|dashboard| DashboardRef {
                    group_id: group.id.clone(),
                    dashboard_id: dashboard.id.clone(),
                })
        })
    }

    /// The dashboard with `number` (from 1) in sidebar order, skipping
    /// collapsed groups as the sidebar does (`secondary-1` … `secondary-9`).
    pub(crate) fn dashboard_at(&self, number: usize) -> Option<DashboardRef> {
        let index = number.checked_sub(1)?;
        self.environment()?
            .groups
            .iter()
            .filter(|group| !group.collapsed)
            .flat_map(|group| {
                group.dashboards.iter().map(|dashboard| DashboardRef {
                    group_id: group.id.clone(),
                    dashboard_id: dashboard.id.clone(),
                })
            })
            .nth(index)
    }

    /// A dashboard's evaluated views and its counts for the sidebar (as of
    /// the latest snapshot: after an edit, the next result may still be on
    /// its way, so match views by id).
    pub(crate) fn result(&self, reference: &DashboardRef) -> Option<&DashboardResult> {
        self.engine.snapshot.dashboards.get(reference)
    }

    /// One view's evaluated body and counts, by the view's id.
    pub(crate) fn view_result(
        &self,
        reference: &DashboardRef,
        view_id: &str,
    ) -> Option<&ViewResult> {
        self.result(reference)?.view(view_id)
    }

    /// The handled problems list views hide unless they set their own (the
    /// settings' *handled problems*, `[appearance.hide_handled]`).
    pub(crate) fn handled_defaults(&self) -> HideHandled {
        self.config.appearance.hide_handled
    }

    /// The first dashboard (the selected one first) that lists `key`. Its
    /// rows tell, unless they are quiet mode's ([`AppState::rows_settling`]):
    /// those are stale exactly about what changed while quiet (the problem
    /// a notification is about), so the dashboards' views then judge the
    /// object as the snapshot has it now (its state changes arrive in quiet
    /// mode too): what their rows list once they are evaluated again.
    pub(crate) fn dashboard_showing(&self, key: &ObjectKey) -> Option<DashboardRef> {
        let settling = self.rows_settling();
        let snapshot = &self.engine.snapshot;
        let now = Timestamp::now();
        let defaults = self.handled_defaults();
        let shows = |reference: &DashboardRef| {
            if settling {
                return self.dashboard(reference).is_some_and(|(_, dashboard)| {
                    dashboard
                        .views
                        .iter()
                        .any(|view| would_list(snapshot, view, defaults, key, now))
                });
            }
            self.result(reference).is_some_and(|result| {
                result.views.iter().any(|view| {
                    view.rows().iter().any(
                        |row| matches!(row, ic_core::snapshot::DashboardRow::Object(row) if row == key),
                    )
                })
            })
        };
        if let Some(selected) = self.selected.as_ref().filter(|selected| shows(selected)) {
            return Some(selected.clone());
        }
        self.environment()?
            .groups
            .iter()
            .flat_map(|group| {
                group.dashboards.iter().map(|dashboard| DashboardRef {
                    group_id: group.id.clone(),
                    dashboard_id: dashboard.id.clone(),
                })
            })
            .find(|reference| shows(reference))
    }

    /// Whether the dashboards' rows on screen may be quiet mode's
    /// (PERF-09): they keep only memberships while quiet and are evaluated
    /// again once the stream is live, so an object revealed now may not be
    /// listed yet, or listed where it was.
    pub(crate) fn rows_settling(&self) -> bool {
        self.engine.snapshot.quiet
    }

    /// Selects a dashboard and shows it instead of an active tab. Returns
    /// whether that changed what's shown; unknown dashboards are ignored.
    pub(crate) fn select(&mut self, reference: DashboardRef) -> bool {
        if self.dashboard(&reference).is_none() {
            return false;
        }
        let changed = self.selected.as_ref() != Some(&reference)
            || self.active_tab.is_some()
            || self.active_list.is_some();
        self.selected = Some(reference);
        self.active_tab = None;
        self.active_list = None;
        if changed {
            self.remember_environment_ui();
        }
        changed
    }

    /// Changes one view of a dashboard (its sort, handled switches,
    /// grouping, collapsed by default, a display's options), by the view's
    /// id: the core re-evaluates it and the settings are saved (DASH-07).
    /// Returns whether the view changed.
    pub(crate) fn update_view(
        &mut self,
        reference: &DashboardRef,
        view_id: &str,
        update: impl FnOnce(&mut View),
    ) -> bool {
        let Some(id) = self.config.active_environment.clone() else {
            return false;
        };
        let Some(view) = self.config.environment_mut(&id).and_then(|environment| {
            environment
                .dashboard_mut(&reference.group_id, &reference.dashboard_id)?
                .view_mut(view_id)
        }) else {
            return false;
        };
        let before = view.clone();
        update(view);
        if *view == before {
            return false;
        }
        #[cfg(test)]
        self.evaluate_fixture(reference);
        self.environment_changed();
        true
    }

    /// [`AppState::update_view`] for a dashboard's primary view
    /// (`crate::dashboard::primary_view`): all of a single-view dashboard,
    /// which the page shows as rc1 did.
    #[cfg(test)]
    pub(crate) fn update_primary_view(
        &mut self,
        reference: &DashboardRef,
        update: impl FnOnce(&mut View),
    ) -> bool {
        let Some(view_id) = self
            .dashboard(reference)
            .map(|(_, dashboard)| crate::dashboard::primary_view(&dashboard.views).id.clone())
        else {
            return false;
        };
        self.update_view(reference, &view_id, update)
    }

    /// The view header's (or summary bar's) handled button: `N hidden ·
    /// show` shows every handled problem, `N handled · hide` hides them
    /// again ([`handled_after_click`]); saved with the view. Returns
    /// whether the view changed.
    pub(crate) fn toggle_handled(&mut self, reference: &DashboardRef, view_id: &str) -> bool {
        let defaults = self.handled_defaults();
        let hidden = self
            .view_result(reference, view_id)
            .map_or(0, |result| result.hidden);
        self.update_view(reference, view_id, |view| {
            view.handled = handled_after_click(view.handled, defaults, hidden);
        })
    }

    /// The fixture has no core: evaluate the changed dashboard here.
    #[cfg(test)]
    fn evaluate_fixture(&mut self, reference: &DashboardRef) {
        let Some(views) = self
            .dashboard(reference)
            .map(|(_, dashboard)| dashboard.views.clone())
        else {
            return;
        };
        if let Some(evaluator) = self.evaluator {
            let result = evaluator.evaluate(&self.engine.snapshot, &views, self.handled_defaults());
            let mut dashboards = (*self.engine.snapshot.dashboards).clone();
            dashboards.insert(reference.clone(), result);
            self.engine.snapshot = Arc::new(Snapshot {
                revision: self.engine.snapshot.revision + 1,
                dashboards: Arc::new(dashboards),
                ..(*self.engine.snapshot).clone()
            });
        }
    }

    /// The active environment changed in place (views, collapsed groups):
    /// save it and tell the core. While the engine waits to reconnect an
    /// update would make it connect at once, skipping its backoff (a user
    /// clicking through sort keys while Icinga is down would hammer it),
    /// so the update waits until the engine connects by itself.
    fn environment_changed(&mut self) {
        self.save_config();
        self.send_environment();
    }

    /// Sends the active environment's settings to its engine (or once it
    /// connects, while it waits to reconnect).
    fn send_environment(&mut self) {
        if let Some(environment) = self.environment().cloned() {
            self.engine.update_environment(&environment);
        }
    }

    /// Saves the settings again (a save the writer refused because the
    /// file was being edited, once that edit is taken over).
    pub(crate) fn save_settings(&self) {
        self.save_config();
    }

    fn save_config(&self) {
        if self.mode == Mode::Live
            && let Some(persistence) = &self.persistence
        {
            persistence.save_config(self.config.clone());
        }
    }

    /// Saves the UI state (window, tabs, selections).
    pub(crate) fn save_ui(&self) {
        if self.mode == Mode::Live
            && let Some(persistence) = &self.persistence
        {
            persistence.save_ui(self.ui.clone());
        }
    }

    /// Records the active environment's tabs and selection in the UI state
    /// and saves it if that changed.
    fn remember_environment_ui(&mut self) {
        let Some(id) = self.config.active_environment.clone() else {
            return;
        };
        let state = EnvironmentUiState {
            tabs: self.tabs.iter().map(ObjectKey::full_name).collect(),
            lists: self.lists.iter().map(|kind| kind.id().to_owned()).collect(),
            selected: self.selected.clone(),
            list_options: self.list_options.clone(),
        };
        if self.ui.set_environment(&id, state) {
            self.save_ui();
        }
    }

    /// The window's size and position, as saved.
    pub(crate) fn window_state(&self) -> Option<WindowState> {
        self.ui.window
    }

    /// Records the window's size and position (saved by the caller,
    /// debounced, with [`AppState::save_ui`]). Returns whether it changed.
    pub(crate) fn set_window_state(&mut self, window: WindowState) -> bool {
        if self.ui.window == Some(window) {
            return false;
        }
        self.ui.window = Some(window);
        true
    }

    /// The objects open as tabs.
    pub(crate) fn tabs(&self) -> &[ObjectKey] {
        &self.tabs
    }

    /// The tab shown instead of the dashboard, if any.
    pub(crate) fn active_tab(&self) -> Option<&ObjectKey> {
        self.active_tab.as_ref()
    }

    /// Opens `key` as a tab (unless it is one already) and shows it. Returns
    /// whether anything changed.
    pub(crate) fn open_tab(&mut self, key: ObjectKey) -> bool {
        let added = !self.tabs.contains(&key) && self.tabs.len() < MAX_TABS;
        if added {
            self.tabs.push(key.clone());
            self.remember_environment_ui();
        } else if !self.tabs.contains(&key) {
            return false;
        }
        let shown = self.active_tab.as_ref() != Some(&key) || self.active_list.is_some();
        self.active_tab = Some(key);
        self.active_list = None;
        added || shown
    }

    /// Shows an open tab. Returns `false` if `key` isn't open or already shown.
    pub(crate) fn activate_tab(&mut self, key: &ObjectKey) -> bool {
        if !self.tabs.contains(key) || self.active_tab.as_ref() == Some(key) {
            return false;
        }
        self.active_tab = Some(key.clone());
        self.active_list = None;
        true
    }

    /// The lists open as tabs, in sidebar order.
    pub(crate) fn lists(&self) -> &[ListKind] {
        &self.lists
    }

    /// The choices saved for the list `kind` in the active environment
    /// (the defaults when none are).
    pub(crate) fn list_options(&self, kind: ListKind) -> ListOptionsState {
        self.list_options
            .get(kind.id())
            .cloned()
            .unwrap_or_default()
    }

    /// Keeps the list `kind`'s choices with the UI state (saved if they
    /// changed).
    pub(crate) fn set_list_options(&mut self, kind: ListKind, options: ListOptionsState) {
        let changed = if options == ListOptionsState::default() {
            self.list_options.remove(kind.id()).is_some()
        } else {
            self.list_options
                .insert(kind.id().to_owned(), options.clone())
                != Some(options)
        };
        if changed {
            self.remember_environment_ui();
        }
    }

    /// The list shown instead of the dashboard, if any.
    pub(crate) fn active_list(&self) -> Option<ListKind> {
        self.active_list
    }

    /// Opens the list `kind` as a tab (unless it is one already) and shows
    /// it. Returns whether anything changed.
    pub(crate) fn open_list(&mut self, kind: ListKind) -> bool {
        let added = !self.lists.contains(&kind);
        if added {
            self.lists.push(kind);
            self.lists.sort();
            self.remember_environment_ui();
        }
        let shown = self.active_list != Some(kind);
        self.active_list = Some(kind);
        self.active_tab = None;
        added || shown
    }

    /// Closes the list `kind`; closing the shown one goes back to the
    /// dashboard. Returns whether it was open.
    pub(crate) fn close_list(&mut self, kind: ListKind) -> bool {
        if self.active_list == Some(kind) {
            self.active_list = None;
        }
        let before = self.lists.len();
        self.lists.retain(|open| *open != kind);
        let closed = self.lists.len() != before;
        if closed {
            self.remember_environment_ui();
        }
        closed
    }

    /// Shows the selected dashboard instead of the active tab or list,
    /// which stays open. Returns whether a tab or list was shown.
    pub(crate) fn show_dashboard(&mut self) -> bool {
        let tab = self.active_tab.take().is_some();
        let list = self.active_list.take().is_some();
        tab || list
    }

    /// Shows the next open tab (`forward`) or the previous one, cycling
    /// through the dashboard, the lists and the tabs in sidebar order:
    /// after the last tab comes the dashboard. Returns whether anything
    /// changed.
    pub(crate) fn cycle_tab(&mut self, forward: bool) -> bool {
        if self.tabs.is_empty() && self.lists.is_empty() {
            return false;
        }
        // 0 is the dashboard, then the lists, then the tabs.
        let lists = self.lists.len();
        let stops = lists + self.tabs.len() + 1;
        let current = if let Some(list) = self.active_list {
            self.lists
                .iter()
                .position(|open| *open == list)
                .map_or(0, |index| index + 1)
        } else {
            self.active_tab
                .as_ref()
                .and_then(|active| self.tabs.iter().position(|tab| tab == active))
                .map_or(0, |index| index + 1 + lists)
        };
        let next = if forward {
            (current + 1) % stops
        } else {
            (current + stops - 1) % stops
        };
        match next.checked_sub(1) {
            Some(index) if index < lists => {
                let kind = self.lists[index];
                self.open_list(kind)
            }
            Some(index) => {
                let key = self.tabs[index - lists].clone();
                self.activate_tab(&key)
            }
            None => self.show_dashboard(),
        }
    }

    /// Closes a tab; closing the shown one goes back to the dashboard.
    /// Returns whether it was open.
    pub(crate) fn close_tab(&mut self, key: &ObjectKey) -> bool {
        let before = self.tabs.len();
        self.tabs.retain(|tab| tab != key);
        if self.active_tab.as_ref() == Some(key) {
            self.active_tab = None;
        }
        let closed = self.tabs.len() != before;
        if closed {
            self.remember_environment_ui();
        }
        closed
    }

    /// Closes every tab and list.
    pub(crate) fn close_all_tabs(&mut self) -> bool {
        self.active_tab = None;
        self.active_list = None;
        let had_tabs = !self.tabs.is_empty() || !self.lists.is_empty();
        self.tabs.clear();
        self.lists.clear();
        if had_tabs {
            self.remember_environment_ui();
        }
        had_tabs
    }

    /// Collapses or expands a sidebar group (saved). Returns whether the
    /// group exists.
    pub(crate) fn toggle_group(&mut self, group_id: &str) -> bool {
        let Some(id) = self.config.active_environment.clone() else {
            return false;
        };
        let Some(group) = self
            .config
            .environment_mut(&id)
            .and_then(|environment| environment.group_mut(group_id))
        else {
            return false;
        };
        group.collapsed = !group.collapsed;
        self.environment_changed();
        true
    }

    /// Takes one event from the core.
    pub(crate) fn apply(&mut self, event: CoreEvent) {
        match event {
            CoreEvent::Snapshot(snapshot) => self.set_snapshot(snapshot),
            CoreEvent::Connection(state) => {
                if let ConnectionState::Connected { node, version, .. } = &state {
                    tracing::info!(node = %node.name, view = %node.view.label(), %version, "connected");
                }
                self.engine.connection.on_state(state);
                if self.engine.update_pending && !self.engine.connection.is_waiting() {
                    self.send_environment();
                }
            }
            CoreEvent::Permissions(info) => self.engine.permissions = Some(info),
            CoreEvent::ActionFinished { id, outcome } => self.action_finished(id, &outcome),
            CoreEvent::Notification(record) => self.engine.push_notification(record),
            // The pauses are the app's (global and per environment); an
            // engine saying its pause ended lets the app forget the ones
            // that are over.
            CoreEvent::NotificationsPaused(_) => self.expire_pauses(Timestamp::now()),
        }
    }

    /// Replaces the snapshot (the core published a new one).
    pub(crate) fn set_snapshot(&mut self, snapshot: Arc<Snapshot>) {
        self.engine.connection.on_snapshot(&snapshot);
        self.engine.snapshot = snapshot;
        self.tracker.settle(&self.engine.snapshot, Instant::now());
    }

    /// Reloads from Icinga (or connects now after a failure). Ignored while
    /// connecting, and within two seconds of the last one. Returns whether
    /// it was sent.
    pub(crate) fn refresh(&mut self, now: Instant) -> bool {
        if self.engine.core.is_none() || self.engine.connection.is_starting() {
            return false;
        }
        if self
            .engine
            .last_refresh
            .is_some_and(|last| now.saturating_duration_since(last) < REFRESH_SPACING)
        {
            return false;
        }
        self.engine.last_refresh = Some(now);
        self.send(Command::Refresh);
        true
    }

    /// Asks the core for the details of `keys` (rows on screen without
    /// output, an opened pane), skipping those asked for recently. Only
    /// while connected.
    pub(crate) fn hydrate(&mut self, keys: Vec<ObjectKey>, now: Instant) -> Hydrated {
        if self.engine.core.is_none() || !self.engine.connection.is_connected() {
            return Hydrated::NotNow;
        }
        let fresh = self.engine.hydration.take_new(keys, now);
        if fresh.is_empty() {
            return Hydrated::Nothing;
        }
        tracing::debug!(count = fresh.len(), "asking for details");
        self.send(Command::Hydrate(fresh.clone()));
        Hydrated::Sent(fresh)
    }

    /// Asks the core to evaluate unsaved views as one dashboard (the
    /// dashboard editor's live preview, validation and match counts; a
    /// view whose filter doesn't work has its `error` set). `None` without
    /// a core.
    pub(crate) fn preview(
        &self,
        views: Vec<View>,
    ) -> Option<futures::channel::oneshot::Receiver<DashboardResult>> {
        let (reply, receiver) = futures::channel::oneshot::channel();
        #[cfg(test)]
        if self.evaluator.is_some() {
            let result = fixture::preview(&self.engine.snapshot, &views, self.handled_defaults());
            let _ = reply.send(result);
            return Some(receiver);
        }
        let core = self.engine.core.as_ref()?;
        core.send(Command::PreviewDashboard { views, reply });
        Some(receiver)
    }

    /// A save finished (from the writer thread). An edited or unreadable
    /// settings file is the session's to handle (`Session::on_saved`).
    pub(crate) fn on_saved(&mut self, report: SaveReport) {
        match report {
            SaveReport::Config(Ok(())) => {
                self.save_error = None;
                self.file_error = None;
            }
            SaveReport::Config(Err(error)) => self.save_error = Some(error),
            SaveReport::FileUnreadable(error) => self.file_error = Some(error),
            // The UI state is only the layout: logged by the writer.
            SaveReport::FileEdited { .. } | SaveReport::Ui(_) => {}
        }
    }

    /// Why the settings file can't be read, while it can't: nothing is
    /// written over it meanwhile.
    pub(crate) fn file_error(&self) -> Option<&str> {
        self.file_error.as_deref()
    }

    /// The settings file reads (`None`) or doesn't (why).
    pub(crate) fn set_file_error(&mut self, error: Option<String>) {
        self.file_error = error;
    }

    /// Why the settings couldn't be saved, unless dismissed.
    pub(crate) fn save_error(&self) -> Option<&str> {
        self.save_error
            .as_deref()
            .filter(|error| self.dismissed_save_error.as_deref() != Some(*error))
    }

    /// Hides the save error until a different one comes.
    pub(crate) fn dismiss_save_error(&mut self) {
        self.dismissed_save_error.clone_from(&self.save_error);
    }

    /// Shows `notice` (replacing an earlier one).
    pub(crate) fn report(&mut self, notice: UserNotice) {
        if notice.problem {
            tracing::warn!(title = %notice.title, detail = ?notice.detail, "told the user");
        } else {
            tracing::info!(title = %notice.title, "told the user");
        }
        self.notice = Some(notice);
    }

    /// The message shown, if any.
    pub(crate) fn notice(&self) -> Option<&UserNotice> {
        self.notice.as_ref()
    }

    /// Hides the message.
    pub(crate) fn dismiss_notice(&mut self) {
        self.notice = None;
    }
}

/// The endpoint to name before Icinga said its node name: the URL's host.
fn endpoint_of(environment: &Environment) -> String {
    environment
        .urls
        .first()
        .and_then(|url| url.api_url().ok())
        .and_then(|url| url.host_str().map(str::to_owned))
        .unwrap_or_else(|| environment.name.clone())
}

/// The API user, for messages.
fn user_of(environment: &Environment) -> Option<String> {
    match &environment.auth {
        AuthConfig::Basic { username } if !username.trim().is_empty() => {
            Some(username.trim().to_owned())
        }
        AuthConfig::Basic { .. } | AuthConfig::ClientCertificate { .. } => None,
    }
}

/// Whether the list `view` lists `key` as `snapshot` has it at `now`: the
/// filter matches (evaluated by `ic-filter`, as the core does), then
/// `problems_only` and the handled switches (the view's own or the
/// settings' `defaults`; what counts as handled, as the core's rows apply
/// it: acknowledged, a downtime in effect, a service of a host with a
/// problem). Views other than lists, and filters that don't parse or
/// evaluate, list nothing.
fn would_list(
    snapshot: &Snapshot,
    view: &View,
    defaults: HideHandled,
    key: &ObjectKey,
    now: Timestamp,
) -> bool {
    if !view.is_list() {
        return false;
    }
    let Ok(filter) = ic_filter::Filter::parse(&view.filter) else {
        return false;
    };
    let test = |scope: &dyn ic_filter::Scope| filter.is_empty() || filter.matches_at(scope, now);
    let hide = view.hidden_handled(defaults);
    // Why the object counts as handled: (acknowledged, in downtime, a
    // service of a host with a problem), each hidden by its own switch.
    let hidden = |check: &ic_model::CheckInfo, problem: bool, host_down: bool| {
        (hide.acknowledged && problem && check.acknowledgement.is_acknowledged())
            || (hide.in_downtime && check.in_downtime())
            || (hide.host_down && host_down)
    };
    let (matches, problem, hidden) = match (key, view.object_kind) {
        (ObjectKey::Host { name }, ObjectKind::Hosts) => {
            let Some(host) = snapshot.hosts.get(name) else {
                return false;
            };
            let problem = host.is_problem();
            (
                test(&ic_filter::HostScope { host }),
                problem,
                hidden(&host.check, problem, false),
            )
        }
        (ObjectKey::Service { key }, ObjectKind::Services) => {
            let Some(service) = snapshot.services.get(key) else {
                return false;
            };
            let host = snapshot.host_of(key).map(Arc::as_ref);
            let host_problem = host.is_some_and(ic_model::Host::is_problem);
            let problem = service.is_problem();
            (
                test(&ic_filter::ServiceScope { service, host }),
                problem,
                hidden(&service.check, problem, problem && host_problem),
            )
        }
        _ => return false,
    };
    matches && (!view.problems_only || problem) && !hidden
}

/// A view's handled setting after a click on its handled button, which
/// reads `N hidden · show` while the view hides `hidden` handled problems
/// and `N handled · hide` while they show:
///
/// - *show*: every handled problem shows (the kinds the view chose are
///   kept for later);
/// - *hide*, after *show*: back to what hid them before
///   ([`ic_config::HandledSetting::toggled`]);
/// - *hide* while the settings hide only some kinds and nothing is hidden
///   (say only `host down`, and acknowledged problems show): every kind is
///   hidden, on this view.
pub(crate) fn handled_after_click(
    setting: ic_config::HandledSetting,
    defaults: HideHandled,
    hidden: u32,
) -> ic_config::HandledSetting {
    use ic_config::{HandledMode, HandledSetting};
    if hidden > 0 {
        HandledSetting {
            mode: HandledMode::Show,
            hide: setting.hide,
        }
    } else if setting.hidden(defaults).any() && setting.hidden(defaults) != HideHandled::ALL {
        HandledSetting {
            mode: HandledMode::Hide,
            hide: HideHandled::ALL,
        }
    } else {
        setting.toggled(defaults)
    }
}

/// An object from its full name: `host` or `host!service`.
pub(crate) fn parse_object(name: &str) -> Option<ObjectKey> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    match ServiceKey::parse(name) {
        Some(key) => Some(ObjectKey::Service { key }),
        None if !name.contains('!') => Some(ObjectKey::host(name)),
        None => None,
    }
}

#[cfg(test)]
impl AppState {
    /// The design's sample data, connected, as of `now`.
    pub(crate) fn fixture(now: Timestamp) -> Self {
        Self::fixture_with(now, FixtureOptions::default())
    }

    /// The design's sample data with `options` (generated rows for load
    /// tests).
    pub(crate) fn fixture_with(now: Timestamp, options: FixtureOptions) -> Self {
        let fixture = fixture::build_with(now, options);
        let mut state = Self::base(fixture.config, UiState::default(), Mode::Fixture, now);
        state.engine.snapshot = Arc::new(fixture.snapshot);
        state.selected = Some(fixture.selected);
        state.evaluator = Some(fixture.evaluator);
        state.engine.connection =
            ConnectionStatus::starting(fixture::ENDPOINT, Some("icygui".into()));
        state
            .engine
            .connection
            .on_state(ConnectionState::Connected {
                node: connection::full_node(fixture::ENDPOINT),
                version: "r2.15.6-1".to_owned(),
                since: now,
            });
        state.engine.connection.last_event_at = Some(now);
        state
    }

    /// No environment configured yet.
    pub(crate) fn empty() -> Self {
        Self::base(
            Config::default(),
            UiState::default(),
            Mode::Live,
            Timestamp::now(),
        )
    }

    /// Replaces the connection status.
    #[cfg(target_os = "linux")]
    pub(crate) fn set_connection(&mut self, connection: ConnectionStatus) {
        self.engine.connection = connection;
    }

    /// Replaces the API user's permissions.
    pub(crate) fn set_permissions(&mut self, info: Option<ApiInfo>) {
        self.engine.permissions = info;
    }

    /// The connection drops (the engine waits to reconnect).
    pub(crate) fn set_connection_lost(&mut self) {
        self.engine
            .connection
            .on_state(ConnectionState::Reconnecting {
                error: "connect: connection refused".to_owned(),
                attempt: 1,
                retry_at: Timestamp::now(),
                untrusted: None,
            });
    }
}

#[cfg(test)]
pub(crate) mod testing;
#[cfg(test)]
mod tests;
