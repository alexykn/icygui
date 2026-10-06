//! The bridge between `ic-core` and the UI: starts the engine for the
//! active environment, pumps its events into [`AppState`] without blocking
//! the UI thread, posts its notifications, saves settings and UI state off
//! the UI thread, and stops everything cleanly when the app quits.
//!
//! - Live: the settings file and the UI state from `ic_config::Paths`,
//!   passwords from the OS keychain (`ic_platform::KeyringSecrets`),
//!   notifications through GPUI ([`notifier`]), the system clock.
//! - `--demo` ([`demo`]): the same engine against an in-process
//!   `ic_mock::MockServer`; nothing is saved.
//!
//! The engine runs on its own thread with its own tokio runtime; its
//! events arrive on an unbounded channel that a GPUI task drains in
//! batches (one re-render per batch, however many snapshots queued up).
//!
//! One environment is active at a time (PLAN.md D2). Switching to another,
//! changing the active one's connection, trusting its certificate or
//! deleting it replaces the engine: the old one stops on another thread
//! (the window never waits for it), then the new one starts, so there is
//! never more than one engine (and one event stream to an Icinga). The
//! environment editor's passwords go to the keychain off the UI thread;
//! deleting an environment deletes its password and its event log
//! (ENV-03) once its engine has stopped.

#[cfg(all(target_os = "linux", not(test)))]
mod dbus;
pub(crate) mod demo;
pub(crate) mod desktop;
#[cfg(all(target_os = "macos", not(test)))]
mod macos;
pub(crate) mod notifier;

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use gpui::{App, AppContext as _, Context, Entity, Global, Subscription, Task};
use ic_config::{AuthConfig, Config, ConfigError, ConfigStore, Environment, Paths};
use ic_core::ports::SecretStore;
use ic_core::{
    ConnectionFailure, ConnectionReport, CoreEvent, EnvironmentSpec, Ports, SystemClock,
};
use ic_model::ObjectKey;
use secrecy::SecretString;

use self::demo::{DemoOptions, DemoSecrets, DemoServer};
use self::desktop::{ACKNOWLEDGE_ACTION, Desktop, Response};
use self::notifier::{GpuiNotifier, Raised};
use crate::actions::{ActionRequest, ObjectAction};
use crate::app_state::environments::EnvironmentSaved;
use crate::app_state::{AppState, ConfigProblem, UserNotice};
use crate::background::window;
use crate::dev::OpenAtStart;
use crate::persist::{Persistence, SaveReport};

/// At most this many events are applied per re-render.
const MAX_EVENTS_PER_BATCH: usize = 512;
/// How long quitting waits for queued saves.
const FLUSH_TIMEOUT: Duration = Duration::from_secs(2);
/// How many notifications are remembered for their click.
const MAX_TARGETS: usize = 256;

/// How the app was started.
#[derive(Clone)]
pub(crate) enum Launch {
    /// Against the configured environments.
    Live {
        /// Where the settings, data and logs are.
        paths: Paths,
        /// The passwords: the OS keychain (`ic_platform::KeyringSecrets`).
        secrets: Arc<dyn SecretStore>,
    },
    /// `--demo`.
    Demo {
        /// What the demo runs.
        options: DemoOptions,
    },
}

/// What to do about a settings file that can't be read (OPS-06).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RecoveryChoice {
    /// Read the file again (after fixing it by hand, or mounting the
    /// volume it lives on).
    Retry,
    /// Save the backup copy as the settings.
    RestoreBackup,
    /// Save empty settings; the unreadable file is kept as a copy.
    StartFresh,
}

/// Work to do on a background thread once the engine has stopped
/// (deleting a removed environment's password and event log). It returns
/// what it couldn't do, in sentences for the user.
type Cleanup = Box<dyn FnOnce() -> Vec<String> + Send>;

/// A notification shown on the desktop, for its clicks.
#[derive(Clone, Debug)]
struct Target {
    /// The intent's id.
    tag: String,
    /// The environment whose engine raised it (its id).
    environment: String,
    /// Its object.
    object: ObjectKey,
}

/// The running session: the engine, its event pump, notifications and
/// persistence. One per app, reachable through [`session`].
pub(crate) struct Session {
    state: Entity<AppState>,
    launch: Launch,
    /// Where each engine's notifier sends its intents.
    intents: UnboundedSender<Raised>,
    /// Where passwords are: the keychain, or the demo's memory.
    secrets: Arc<dyn SecretStore>,
    pump: Option<Task<()>>,
    /// The demo's servers by environment id, started when first shown.
    demo: HashMap<String, DemoServer>,
    /// The demo's passwords (servers' and the editor's), in memory.
    demo_secrets: Arc<DemoSecrets>,
    demo_dir: Option<tempfile::TempDir>,
    pending_open: Option<OpenAtStart>,
    /// Shows notifications on the desktop.
    desktop: Box<dyn Desktop>,
    /// Recent notifications' tags and objects, for their clicks.
    targets: VecDeque<Target>,
    /// An engine is stopping (or a removed environment being cleaned up);
    /// the next engine starts when this finishes.
    stopping: Option<Task<()>>,
    /// Run once the engine stopping now has stopped.
    cleanups: Vec<Cleanup>,
    /// Counts engine replacements, so an engine start that was waiting
    /// (for a demo server) can tell it's no longer wanted.
    generation: u64,
    /// The demo servers' controls, for tests that change the simulated
    /// Icinga.
    #[cfg(all(test, target_os = "linux"))]
    demo_controls: HashMap<String, ic_mock::MockControl>,
    /// What the desktop was asked to show (tests).
    #[cfg(all(test, target_os = "linux"))]
    shown: desktop::RecordingDesktop,
    _tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

/// The global handle on the session.
#[derive(Clone)]
struct SessionHandle(Entity<Session>);

impl Global for SessionHandle {}

/// The session, if one is installed.
pub(crate) fn session(cx: &App) -> Option<Entity<Session>> {
    cx.try_global::<SessionHandle>()
        .map(|handle| handle.0.clone())
}

impl Session {
    /// Creates the session for `state` and makes it global: persistence
    /// (live), the notification pump, the quit hook. Nothing connects
    /// until [`Session::start`].
    pub(crate) fn install(
        state: Entity<AppState>,
        launch: Launch,
        pending_open: Option<OpenAtStart>,
        cx: &mut App,
    ) -> Entity<Self> {
        let session = cx.new(|cx| Self::new(state, launch, pending_open, cx));
        cx.set_global(SessionHandle(session.clone()));
        session
    }

    fn new(
        state: Entity<AppState>,
        launch: Launch,
        pending_open: Option<OpenAtStart>,
        cx: &mut Context<Self>,
    ) -> Self {
        let (intents, raised) = GpuiNotifier::channel();
        let (responses, clicks) = unbounded::<Response>();
        let mut tasks = vec![
            Self::spawn_notifications(raised, cx),
            Self::spawn_clicks(clicks, cx),
        ];
        if let Launch::Live { paths, .. } = &launch {
            tasks.push(Self::attach_persistence(&state, paths, cx));
        }
        // GPUI's backend answers here where it shows them. Linux (D-Bus)
        // and macOS (our own delegate, which this would replace) send to
        // `responses` themselves.
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let gpui_responses = responses.clone();
            cx.on_system_notification_response(move |response, _| {
                let _ = gpui_responses.unbounded_send(Response {
                    tag: response.tag.to_string(),
                    action: response.action_id.map(|action| action.to_string()),
                });
            });
        }
        #[cfg(all(test, target_os = "linux"))]
        let shown = desktop::RecordingDesktop::default();
        #[cfg(all(test, target_os = "linux"))]
        let desktop: Box<dyn Desktop> = {
            drop(responses);
            Box::new(shown.clone())
        };
        #[cfg(all(test, not(target_os = "linux")))]
        let desktop: Box<dyn Desktop> = {
            drop(responses);
            Box::new(desktop::RecordingDesktop::default())
        };
        #[cfg(not(test))]
        let desktop = desktop_for(responses);
        let quit = cx.on_app_quit(|session, cx| {
            session.stop(cx);
            async {}
        });
        let demo_secrets = Arc::new(DemoSecrets::default());
        let secrets: Arc<dyn SecretStore> = match &launch {
            Launch::Live { secrets, .. } => secrets.clone(),
            Launch::Demo { .. } => demo_secrets.clone(),
        };
        Self {
            state,
            launch,
            intents,
            secrets,
            pump: None,
            demo: HashMap::new(),
            demo_secrets,
            demo_dir: None,
            pending_open,
            desktop,
            targets: VecDeque::new(),
            stopping: None,
            cleanups: Vec::new(),
            generation: 0,
            #[cfg(all(test, target_os = "linux"))]
            demo_controls: HashMap::new(),
            #[cfg(all(test, target_os = "linux"))]
            shown,
            _tasks: tasks,
            _subscriptions: vec![quit],
        }
    }

    /// Starts the settings writer and the task that reports its results.
    fn attach_persistence(
        state: &Entity<AppState>,
        paths: &Paths,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        let (sender, mut reports) = unbounded::<SaveReport>();
        let started = Persistence::start(
            paths.config_store(),
            paths.state_store(),
            Box::new(move |report| {
                let _ = sender.unbounded_send(report);
            }),
        );
        match started {
            Ok(persistence) => state.update(cx, |state, _| state.set_persistence(persistence)),
            Err(error) => {
                tracing::error!(%error, "the settings writer couldn't start; nothing will be saved");
                state.update(cx, |state, _| {
                    state.on_saved(SaveReport::Config(Err(format!(
                        "the settings writer couldn't start: {error}"
                    ))));
                });
            }
        }
        let state = state.downgrade();
        cx.spawn(async move |_, cx| {
            while let Some(report) = reports.next().await {
                let alive = state.update(cx, |state, cx| {
                    state.on_saved(report);
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        })
    }

    /// Shows the core's notifications on the desktop, on the UI thread.
    fn spawn_notifications(
        mut raised: UnboundedReceiver<Raised>,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        cx.spawn(async move |this, cx| {
            while let Some(raised) = raised.next().await {
                if this
                    .update(cx, |session, cx| session.post(&raised, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
    }

    /// Shows an engine's intent on the desktop (NOTE-01): with
    /// *Acknowledge* for a problem the API user may acknowledge, and
    /// *Open*. It belongs to the environment whose engine raised it, which
    /// need not be the active one by now (an engine being replaced, or an
    /// intent still queued when the user switched): its clicks are checked
    /// against that environment, and it offers *Acknowledge* only while
    /// that environment is active (the permissions known are the active
    /// environment's).
    fn post(&mut self, raised: &Raised, cx: &mut Context<Self>) {
        let intent = &raised.intent;
        let acknowledge = {
            let state = self.state.read(cx);
            state.active_environment_id() == Some(raised.environment.as_str())
                && state.action_denial(&ObjectAction::Acknowledge).is_none()
        };
        if let Some(object) = &intent.object {
            self.targets.push_front(Target {
                tag: intent.id.clone(),
                environment: raised.environment.clone(),
                object: object.clone(),
            });
            self.targets.truncate(MAX_TARGETS);
        }
        self.desktop.show(desktop::posted(intent, acknowledge), cx);
    }

    /// Carries out clicks on desktop notifications, on the UI thread.
    fn spawn_clicks(mut clicks: UnboundedReceiver<Response>, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            while let Some(response) = clicks.next().await {
                let clicked = this.update(cx, |session, cx| {
                    session.notification_clicked(&response, cx);
                });
                if clicked.is_err() {
                    break;
                }
            }
        })
    }

    /// A desktop notification was clicked (NOTE-01): its object opens (the
    /// window comes back if it was closed) and the notification counts as
    /// read; *Acknowledge* also opens the acknowledge dialog for it.
    pub(crate) fn notification_clicked(&mut self, response: &Response, cx: &mut Context<Self>) {
        let Some(target) = self
            .targets
            .iter()
            .find(|target| target.tag == response.tag)
            .cloned()
        else {
            tracing::debug!(tag = %response.tag, "a notification without an object was clicked");
            window::show(cx);
            return;
        };
        tracing::info!(object = %target.object, action = ?response.action, "notification clicked");
        let from_active = self.state.update(cx, |state, cx| {
            let from_active = state.active_environment_id() == Some(target.environment.as_str());
            if from_active && state.mark_notification_read(&target.tag) {
                cx.notify();
            }
            from_active
        });
        if !from_active {
            // Nothing happens in the active environment: an
            // acknowledgement for one Icinga must never go to another.
            window::show(cx);
            self.state.update(cx, |state, cx| {
                let (title, detail) = other_environment_notice(state, &target);
                state.inform(title, Some(detail));
                cx.notify();
            });
            return;
        }
        if !open_object(&target.object, cx) {
            tracing::warn!(object = %target.object, "no window to show the notification's object in");
            return;
        }
        if response.action.as_deref() == Some(ACKNOWLEDGE_ACTION) {
            self.state.update(cx, |state, cx| {
                // A refusal (no permission) shows as a toast.
                let _ = state.request(ActionRequest {
                    action: ObjectAction::Acknowledge,
                    targets: vec![target.object.clone()],
                });
                cx.notify();
            });
        }
    }

    /// What the desktop was asked to show (tests).
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn shown_notifications(&self) -> Vec<desktop::Posted> {
        self.shown.shown.borrow().clone()
    }

    /// Connects: starts the engine for the active environment (for the
    /// demo's environments, once their server runs). Without an
    /// environment (or while the settings can't be read) nothing starts.
    pub(crate) fn start(&mut self, cx: &mut Context<Self>) {
        match self.launch.clone() {
            Launch::Live { paths, .. } => {
                let secrets = self.secrets.clone();
                self.start_core(paths.data_dir, secrets, cx);
            }
            Launch::Demo { options } => {
                let Some(id) = self
                    .state
                    .read(cx)
                    .active_environment_id()
                    .map(str::to_owned)
                else {
                    return;
                };
                match demo::options_for(&id, &options) {
                    Some(options) => self.start_demo(&id, &options, cx),
                    // Added in the editor while the demo runs: a real one.
                    None => match self.demo_data_dir() {
                        Ok(data_dir) => {
                            let secrets = self.secrets.clone();
                            self.start_core(data_dir, secrets, cx);
                        }
                        Err(error) => self.engine_failed(error, cx),
                    },
                }
            }
        }
    }

    /// Reports that the engine couldn't start.
    fn engine_failed(&self, error: String, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            state.engine_failed(error);
            cx.notify();
        });
    }

    /// The demo's temporary directory for event logs, created on first use.
    fn demo_data_dir(&mut self) -> Result<PathBuf, String> {
        if let Some(dir) = &self.demo_dir {
            return Ok(dir.path().to_path_buf());
        }
        let dir = tempfile::Builder::new()
            .prefix("icygui-demo-")
            .tempdir()
            .map_err(|error| format!("no directory for the demo's event log: {error}"))?;
        let path = dir.path().to_path_buf();
        self.demo_dir = Some(dir);
        Ok(path)
    }

    /// Starts the demo environment `id`'s server unless it runs, then its
    /// engine.
    fn start_demo(&mut self, id: &str, options: &DemoOptions, cx: &mut Context<Self>) {
        if self.demo.get(id).is_some_and(DemoServer::is_up) {
            self.start_demo_core(cx);
            return;
        }
        let (server, endpoint) = match demo::start(options) {
            Ok(started) => started,
            Err(error) => {
                self.engine_failed(format!("the demo server couldn't start: {error}"), cx);
                return;
            }
        };
        self.demo.insert(id.to_owned(), server);
        let generation = self.generation;
        let id = id.to_owned();
        cx.spawn(async move |this, cx| {
            let endpoint = endpoint.await;
            let _ = this.update(cx, |session, cx| {
                let endpoint = match endpoint {
                    Ok(Ok((endpoint, control))) => {
                        session.keep_demo_control(&id, control);
                        endpoint
                    }
                    Ok(Err(error)) => {
                        session.demo.remove(&id);
                        if session.generation == generation {
                            session.engine_failed(
                                format!("the demo server couldn't start: {error}"),
                                cx,
                            );
                        }
                        return;
                    }
                    Err(_) => return,
                };
                let Some(server) = session.demo.get_mut(&id) else {
                    return;
                };
                server.set_endpoint(endpoint.clone());
                session.demo_secrets.put(&id, server.core_password());
                let targets = server.environment_target(&endpoint);
                session.state.update(cx, |state, _| {
                    state.set_demo_servers(&id, &targets);
                });
                // Another environment may have been chosen meanwhile.
                let still_wanted = session.generation == generation
                    && session.state.read(cx).active_environment_id() == Some(id.as_str());
                if still_wanted {
                    session.start_demo_core(cx);
                }
            });
        })
        .detach();
    }

    /// Starts the engine for the active demo environment, whose server
    /// runs.
    fn start_demo_core(&mut self, cx: &mut Context<Self>) {
        match self.demo_data_dir() {
            Ok(data_dir) => {
                let secrets = self.secrets.clone();
                self.start_core(data_dir, secrets, cx);
            }
            Err(error) => self.engine_failed(error, cx),
        }
    }

    /// Starts the engine for the active environment and the event pump.
    fn start_core(
        &mut self,
        data_dir: PathBuf,
        secrets: Arc<dyn SecretStore>,
        cx: &mut Context<Self>,
    ) {
        let (environment, general) = {
            let state = self.state.read(cx);
            (state.environment().cloned(), state.config().general.clone())
        };
        let Some(environment) = environment else {
            tracing::info!("no environment configured; nothing to connect to");
            return;
        };
        let name = environment.name.clone();
        let notifier = self.engine_notifier(&environment.id);
        let spec = EnvironmentSpec {
            environment,
            general,
            data_dir,
        };
        let ports = Ports {
            secrets,
            notifier: Arc::new(notifier),
            clock: Arc::new(SystemClock),
        };
        match ic_core::start(spec, ports) {
            Ok(mut handle) => {
                let events = handle.take_events();
                let recent = self.state.update(cx, |state, cx| {
                    state.set_core(Box::new(handle));
                    cx.notify();
                    state.request_notifications()
                });
                if let Some(recent) = recent {
                    // The notification centre starts with the log's, unless
                    // this engine was replaced meanwhile (its notifications
                    // are another environment's, or already reloaded).
                    let generation = self.generation;
                    cx.spawn(async move |this, cx| {
                        if let Ok(records) = recent.await {
                            let _ = this.update(cx, |session, cx| {
                                if session.generation != generation {
                                    tracing::debug!(
                                        "a replaced engine's notifications were dropped"
                                    );
                                    return;
                                }
                                session.state.update(cx, |state, cx| {
                                    state.load_notifications(records);
                                    cx.notify();
                                });
                            });
                        }
                    })
                    .detach();
                }
                if let Some(events) = events {
                    self.pump = Some(self.spawn_pump(events, cx));
                }
                tracing::info!(environment = %name, "engine started");
            }
            Err(error) => {
                tracing::error!(%error, "the engine couldn't start");
                self.state.update(cx, |state, cx| {
                    state.engine_failed(error.to_string());
                    cx.notify();
                });
            }
        }
    }

    /// The notifier for an engine of the environment `id`: its intents
    /// carry that environment.
    pub(crate) fn engine_notifier(&self, id: &str) -> GpuiNotifier {
        GpuiNotifier::new(self.intents.clone(), id)
    }

    /// Drains the engine's events into the state, a batch per re-render.
    fn spawn_pump(
        &self,
        mut events: UnboundedReceiver<CoreEvent>,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        let state = self.state.downgrade();
        cx.spawn(async move |this, cx| {
            while let Some(first) = events.next().await {
                let mut batch = vec![first];
                while batch.len() < MAX_EVENTS_PER_BATCH {
                    match events.try_recv() {
                        Ok(event) => batch.push(event),
                        Err(_) => break,
                    }
                }
                let applied = state.update(cx, |state, cx| {
                    for event in batch {
                        state.apply(event);
                    }
                    cx.notify();
                });
                if applied.is_err() {
                    return;
                }
                let _ = this.update(cx, Self::after_events);
            }
            // The session drops the pump before it stops an engine, so the
            // stream only ends here when the engine died (a panic on its
            // thread): say so instead of showing stale data as live.
            tracing::error!("the engine's event stream ended: the engine stopped on its own");
            let _ = this.update(cx, Self::engine_stopped_on_its_own);
        })
    }

    /// The engine died: its link goes (commands to it would vanish), the
    /// footer and a banner say so and offer a restart (ENV-07).
    fn engine_stopped_on_its_own(&mut self, cx: &mut Context<Self>) {
        self.pump = None;
        self.state.update(cx, |state, cx| {
            drop(state.take_core());
            state.engine_stopped(
                "Icinga's data is no longer updated. The log file has the details; \
                 Restart starts a new engine."
                    .to_owned(),
            );
            cx.notify();
        });
    }

    /// Starts a new engine for the active environment (the banner's
    /// "Restart" after the engine stopped or couldn't start).
    pub(crate) fn restart_engine(&mut self, cx: &mut Context<Self>) {
        tracing::info!("restarting the engine");
        self.state.update(cx, |state, cx| {
            state.reset_connection();
            cx.notify();
        });
        self.replace_engine(None, cx);
    }

    /// Ends the event stream as an engine that died would (tests).
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn end_event_stream(&mut self, cx: &mut Context<Self>) {
        let (sender, events) = unbounded();
        drop(sender);
        self.pump = Some(self.spawn_pump(events, cx));
    }

    /// Replaces the engine: the running one stops on another thread, the
    /// cleanups run, then an engine starts for the (now) active
    /// environment. Requests while one runs fold into it.
    fn replace_engine(&mut self, cleanup: Option<Cleanup>, cx: &mut Context<Self>) {
        self.generation += 1;
        self.pump = None;
        self.cleanups.extend(cleanup);
        let core = self.state.update(cx, |state, _| state.take_core());
        match core {
            Some(core) => {
                let stopped = core.shutdown_in_background();
                self.stopping = Some(cx.spawn(async move |this, cx| {
                    // Cancelled means it stopped without saying so.
                    let _ = stopped.await;
                    let _ = this.update(cx, Self::engine_stopped);
                }));
            }
            // The engine stopping now: the next one starts after it.
            None if self.stopping.is_some() => {}
            None => self.engine_stopped(cx),
        }
    }

    /// The old engine has stopped: run the cleanups, then start the next
    /// engine.
    fn engine_stopped(&mut self, cx: &mut Context<Self>) {
        self.stopping = None;
        let cleanups = std::mem::take(&mut self.cleanups);
        if cleanups.is_empty() {
            self.start(cx);
            return;
        }
        let work = cx.background_executor().spawn(async move {
            cleanups
                .into_iter()
                .flat_map(|cleanup| cleanup())
                .collect::<Vec<_>>()
        });
        self.stopping = Some(cx.spawn(async move |this, cx| {
            let problems = work.await;
            let _ = this.update(cx, |session, cx| {
                session.report_leftovers(&problems, cx);
                session.engine_stopped(cx);
            });
        }));
    }

    /// Tells the user what a cleanup couldn't remove (ENV-03: a password
    /// left in the keychain must not go unnoticed).
    fn report_leftovers(&self, problems: &[String], cx: &mut Context<Self>) {
        if problems.is_empty() {
            return;
        }
        self.state.update(cx, |state, cx| {
            state.report(UserNotice::problem(
                "Not everything of the environment could be removed.",
                problems.join(" "),
            ));
            cx.notify();
        });
    }

    /// Makes `id` the active environment and connects to it (ENV-01).
    /// Returns whether it switched.
    pub(crate) fn switch_environment(&mut self, id: &str, cx: &mut Context<Self>) -> bool {
        let switched = self.state.update(cx, |state, cx| {
            let switched = state.switch_environment(id);
            cx.notify();
            switched
        });
        if switched {
            self.replace_engine(None, cx);
        }
        switched
    }

    /// Saves an environment from the editor (ENV-02): its password (if one
    /// was typed) into the keychain first, off the UI thread, then the
    /// settings; the engine restarts if the active environment's
    /// connection changed, or starts for the first environment.
    pub(crate) fn save_environment(
        &mut self,
        environment: Environment,
        password: Option<SecretString>,
        cx: &mut Context<Self>,
    ) -> Task<Result<EnvironmentSaved, String>> {
        let secrets = self.secrets.clone();
        cx.spawn(async move |this, cx| {
            let password_changed = password.is_some();
            let account = environment.id.clone();
            if let Some(password) = password {
                let secrets = secrets.clone();
                let account = account.clone();
                let stored = cx
                    .background_executor()
                    .spawn(async move { secrets.set(&account, &password) })
                    .await;
                if let Err(error) = stored {
                    tracing::warn!(%error, "the password couldn't be stored");
                    return Err(format!("The password couldn't be stored: {error}"));
                }
            }
            // A client certificate replaced the password: the password
            // leaves the keychain (ENV-03). Deleting nothing is fine.
            let mut leftovers = Vec::new();
            if !matches!(environment.auth, AuthConfig::Basic { .. }) {
                let label = account.clone();
                let removed = cx
                    .background_executor()
                    .spawn(async move { secrets.delete(&account) })
                    .await;
                if let Err(error) = removed {
                    leftovers.push(password_left_over(&label, &error));
                }
            }
            this.update(cx, |session, cx| {
                session.report_leftovers(&leftovers, cx);
                let saved = session.state.update(cx, |state, cx| {
                    let saved = state.save_environment(environment, password_changed);
                    if saved == EnvironmentSaved::Reconnect {
                        state.reset_connection();
                    }
                    cx.notify();
                    saved
                });
                if saved.needs_engine() {
                    session.replace_engine(None, cx);
                }
                saved
            })
            .map_err(|_| "the window closed".to_owned())
        })
    }

    /// Deletes an environment (ENV-03): its settings and dashboards, then
    /// (once its engine stopped, if it was the active one) its password
    /// and its event log. The next environment, if any, becomes active.
    /// Returns whether it existed.
    pub(crate) fn delete_environment(&mut self, id: &str, cx: &mut Context<Self>) -> bool {
        let Some(was_active) = self.state.update(cx, |state, cx| {
            let removed = state.remove_environment(id);
            cx.notify();
            removed
        }) else {
            return false;
        };
        let secrets = self.secrets.clone();
        let data_dir = self.event_log_dir();
        let account = id.to_owned();
        let cleanup: Cleanup = Box::new(move || {
            let mut problems = Vec::new();
            if let Err(error) = secrets.delete(&account) {
                problems.push(password_left_over(&account, &error));
            }
            if let Some(data_dir) = data_dir
                && let Err(error) = ic_core::delete_event_log(&data_dir, &account)
            {
                tracing::warn!(%error, "the deleted environment's event log couldn't be removed");
                problems.push(format!(
                    "Its event log couldn't be removed ({error}); delete {} by hand.",
                    ic_core::event_log_path(&data_dir, &account).display()
                ));
            }
            problems
        });
        if was_active {
            self.replace_engine(Some(cleanup), cx);
        } else {
            let work = cx.background_executor().spawn(async move { cleanup() });
            cx.spawn(async move |this, cx| {
                let problems = work.await;
                let _ = this.update(cx, |session, cx| session.report_leftovers(&problems, cx));
            })
            .detach();
        }
        true
    }

    /// Trusts `fingerprint` for environment `id`'s URL `url` (ENV-05,
    /// trust on first use; pins are per URL): pins it and reconnects if
    /// it's the active environment.
    pub(crate) fn trust_certificate(
        &mut self,
        id: &str,
        url: &str,
        fingerprint: &str,
        cx: &mut Context<Self>,
    ) {
        let reconnect = self.state.update(cx, |state, cx| {
            let pinned = state.pin_certificate(id, url, fingerprint);
            let active = state.active_environment_id() == Some(id);
            if pinned && active {
                state.reset_connection();
            }
            cx.notify();
            pinned && active
        });
        if reconnect {
            self.replace_engine(None, cx);
        }
    }

    /// Tests one URL (`environment.urls[url]`) of an environment's
    /// settings as edited (ENV-04, ENV-12): with the typed password, else
    /// the one stored for it.
    pub(crate) fn test_environment(
        &self,
        environment: Environment,
        url: usize,
        typed: Option<SecretString>,
        cx: &mut Context<Self>,
    ) -> Task<Result<ConnectionReport, ConnectionFailure>> {
        let secrets = self.secrets.clone();
        cx.spawn(async move |_, cx| {
            let password = match typed {
                Some(password) => Some(password),
                None if matches!(environment.auth, AuthConfig::Basic { .. }) => {
                    let account = environment.id.clone();
                    let stored = cx
                        .background_executor()
                        .spawn(async move { secrets.get(&account) })
                        .await;
                    // A locked or missing keychain isn't a missing password.
                    match stored {
                        Ok(password) => password,
                        Err(error) => {
                            tracing::warn!(error = %error.message, "the keychain couldn't be read");
                            return Err(ConnectionFailure::Other(format!(
                                "the keychain couldn't be read: {}",
                                error.message
                            )));
                        }
                    }
                }
                None => None,
            };
            match ic_core::test_connection(environment, url, password).await {
                Ok(outcome) => outcome,
                Err(_) => Err(ConnectionFailure::Other(
                    "the test stopped before it finished".to_owned(),
                )),
            }
        })
    }

    /// What the about dialog says about where things are.
    pub(crate) fn about_facts(&self) -> crate::settings::about::AboutFacts {
        match &self.launch {
            Launch::Live { paths, .. } => crate::settings::about::AboutFacts {
                settings: Some(paths.config_file.display().to_string()),
                logs: Some(paths.log_dir.display().to_string()),
            },
            Launch::Demo { .. } => crate::settings::about::AboutFacts {
                settings: Some("none: the demo saves nothing".to_owned()),
                logs: Paths::from_system()
                    .ok()
                    .map(|paths| paths.log_dir.display().to_string()),
            },
        }
    }

    /// Where the event logs are: the data directory, or the demo's
    /// temporary one.
    fn event_log_dir(&self) -> Option<PathBuf> {
        match &self.launch {
            Launch::Live { paths, .. } => Some(paths.data_dir.clone()),
            Launch::Demo { .. } => self.demo_dir.as_ref().map(|dir| dir.path().to_path_buf()),
        }
    }

    /// Carries out what the development switches asked to open once the
    /// object shows.
    fn after_events(&mut self, cx: &mut Context<Self>) {
        let Some(open) = self.pending_open.clone() else {
            return;
        };
        let state = self.state.read(cx);
        if !state.connection().is_connected() {
            return;
        }
        let ready = match &open {
            OpenAtStart::Object(key) | OpenAtStart::Linked { cursor: key, .. } => {
                state.dashboard_showing(key).is_some()
            }
            OpenAtStart::Tab(key) => match key {
                ObjectKey::Host { name } => state.snapshot().hosts.contains_key(name),
                ObjectKey::Service { key } => state.snapshot().services.contains_key(key),
            },
        };
        if !ready {
            return;
        }
        self.pending_open = None;
        match open {
            OpenAtStart::Object(key) => {
                open_object(&key, cx);
            }
            OpenAtStart::Linked { cursor, pane } => {
                window::with_workspace(cx, |workspace, window, cx| {
                    workspace.reveal_linked(&cursor, pane.clone(), window, cx);
                });
            }
            OpenAtStart::Tab(key) => self.state.update(cx, |state, cx| {
                if state.open_tab(key) {
                    cx.notify();
                }
            }),
        }
    }

    /// Carries out a choice on the recovery screen (OPS-06) off the UI
    /// thread, then adopts the settings and connects.
    pub(crate) fn resolve_config(&mut self, choice: RecoveryChoice, cx: &mut Context<Self>) {
        let Launch::Live { paths, .. } = self.launch.clone() else {
            return;
        };
        let backup = {
            let state = self.state.read(cx);
            let Some(problem) = state.config_problem() else {
                return;
            };
            if problem.busy {
                return;
            }
            problem.backup.clone().ok().flatten()
        };
        self.state.update(cx, |state, cx| {
            state.update_config_problem(|problem| {
                problem.busy = true;
                problem.failure = None;
            });
            cx.notify();
        });
        let store = paths.config_store();
        let work = cx.background_executor().spawn(async move {
            match choice {
                RecoveryChoice::Retry => load_settings(&store),
                RecoveryChoice::RestoreBackup => {
                    let Some(backup) = backup else {
                        return Err(Box::new(problem_for(
                            &store,
                            "there is no backup to restore",
                        )));
                    };
                    store
                        .save(&backup)
                        .map(|()| backup)
                        .map_err(|error| Box::new(problem_for(&store, &error.to_string())))
                }
                RecoveryChoice::StartFresh => {
                    let fresh = Config::default();
                    store
                        .save(&fresh)
                        .map(|()| fresh)
                        .map_err(|error| Box::new(problem_for(&store, &error.to_string())))
                }
            }
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |session, cx| match result {
                Ok(config) => {
                    tracing::info!(?choice, "settings recovered");
                    session.state.update(cx, |state, cx| {
                        state.adopt_config(config);
                        cx.notify();
                    });
                    session.start(cx);
                }
                Err(problem) => session.state.update(cx, |state, cx| {
                    let failure = problem
                        .failure
                        .clone()
                        .unwrap_or_else(|| problem.message.clone());
                    state.update_config_problem(|current| {
                        if choice == RecoveryChoice::Retry {
                            *current = *problem;
                        }
                        current.busy = false;
                        current.failure = Some(failure);
                    });
                    cx.notify();
                }),
            });
        })
        .detach();
    }

    /// Stops the engine (bounded wait) and writes what is queued. Runs when
    /// the app quits.
    fn stop(&mut self, cx: &mut Context<Self>) {
        self.pump = None;
        let core = self.state.update(cx, |state, _| state.take_core());
        if let Some(core) = core {
            core.shutdown();
        }
        if !self.state.read(cx).flush_persistence(FLUSH_TIMEOUT) {
            tracing::warn!("some settings may not have been saved before quitting");
        }
        self.demo.clear();
        self.demo_dir = None;
        tracing::info!("stopped");
    }
}

impl Session {
    /// Keeps a demo server's control for tests (nothing else changes the
    /// simulated Icinga).
    #[cfg(all(test, target_os = "linux"))]
    fn keep_demo_control(&mut self, id: &str, control: ic_mock::MockControl) {
        self.demo_controls.insert(id.to_owned(), control);
    }

    #[cfg(not(all(test, target_os = "linux")))]
    #[expect(clippy::unused_self, reason = "only the UI tests keep the control")]
    fn keep_demo_control(&mut self, _id: &str, _control: ic_mock::MockControl) {}
}

#[cfg(all(test, target_os = "linux"))]
impl Session {
    /// The `prod-cluster` demo server's control, once it runs.
    pub(crate) fn demo_control(&self) -> Option<ic_mock::MockControl> {
        self.demo_controls.get(demo::ENVIRONMENT_ID).cloned()
    }
}

/// Reads the settings, or describes why they can't be read.
///
/// # Errors
///
/// The settings file can't be read: the problem, with the backup's state.
pub(crate) fn load_settings(store: &ConfigStore) -> Result<Config, Box<ConfigProblem>> {
    store.load().map_err(|error| {
        tracing::error!(%error, path = %store.path().display(), "the settings can't be read");
        Box::new(ConfigProblem {
            message: error.to_string(),
            path: store.path().to_path_buf(),
            backup: store.load_backup().map_err(|error| error.to_string()),
            newer: matches!(error, ConfigError::UnsupportedVersion { .. }),
            busy: false,
            failure: None,
        })
    })
}

/// Says that the password of environment `account` stayed in the
/// keychain, and which entry to remove by hand.
fn password_left_over(account: &str, error: &ic_core::ports::SecretError) -> String {
    tracing::warn!(%error, "a password couldn't be removed from the keychain");
    format!(
        "Its password is still in the keychain ({}): remove the entry for account {account} \
         of service {} by hand.",
        error.message,
        ic_platform::SERVICE
    )
}

/// A problem saying a recovery choice failed with `failure`.
fn problem_for(store: &ConfigStore, failure: &str) -> ConfigProblem {
    ConfigProblem {
        message: String::new(),
        path: store.path().to_path_buf(),
        backup: Ok(None),
        newer: false,
        busy: false,
        failure: Some(failure.to_owned()),
    }
}

/// What a click on a notification of another environment than the active
/// one says: which environment it came from, and what it is about.
fn other_environment_notice(state: &AppState, target: &Target) -> (String, String) {
    let object = crate::operate::forms::describe_objects(std::slice::from_ref(&target.object));
    match state.environment_by_id(&target.environment) {
        Some(environment) => (
            format!("That notification is from {}", environment.name),
            format!("Switch to {} to see {object}.", environment.name),
        ),
        None => (
            "That notification is from a removed environment".to_owned(),
            format!("It was about {object}."),
        ),
    }
}

/// Shows `object`: in a dashboard that lists it, else as a tab, and
/// brings the window forward (opening it if it was closed). Returns
/// whether a window shows it.
pub(crate) fn open_object(object: &ObjectKey, cx: &mut App) -> bool {
    window::with_workspace(cx, |workspace, window, cx| {
        workspace.reveal(object, window, cx);
    })
}

/// The desktop notifications of this platform, with clicks to
/// `responses`: over D-Bus on Linux, through `UNUserNotificationCenter` on
/// macOS; GPUI's elsewhere.
#[cfg(not(test))]
fn desktop_for(responses: UnboundedSender<Response>) -> Box<dyn Desktop> {
    #[cfg(target_os = "linux")]
    {
        Box::new(dbus::DbusDesktop::start(
            crate::APP_NAME,
            crate::APP_ID,
            responses,
        ))
    }
    #[cfg(target_os = "macos")]
    {
        Box::new(macos::MacDesktop::start(responses))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        drop(responses);
        Box::new(desktop::GpuiDesktop)
    }
}
