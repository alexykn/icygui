//! The bridge between `ic-core` and the UI: starts an engine for every
//! saved environment, pumps their events into [`AppState`] without
//! blocking the UI thread, posts their notifications, saves settings and
//! UI state off the UI thread, and stops everything cleanly when the app
//! quits.
//!
//! - Live: the settings file and the UI state from `ic_config::Paths`,
//!   passwords from the OS keychain (`ic_platform::KeyringSecrets`),
//!   notifications through GPUI ([`notifier`]), the system clock.
//! - `--demo` ([`demo`]): the same engines against in-process
//!   `ic_mock::MockServer`s; nothing is saved.
//!
//! Every engine runs on its own thread with its own tokio runtime; its
//! events arrive on an unbounded channel that a GPUI task drains in
//! batches (one re-render per batch, however many snapshots queued up),
//! tagged with the engine's environment.
//!
//! Every saved environment runs its own engine (PLAN.md D2), from the
//! start (also with `--background`) until it is deleted: event stream,
//! rules, event log and notifications, whichever environment is on screen.
//! The active one drives the window; the others are told they aren't on
//! screen and publish less often, and cost Icinga no more than the active
//! one (one stream and one lean load per environment). Switching only
//! swaps what the window shows. Changing an environment's connection or
//! trusting its certificate replaces its engine: the old one stops on
//! another thread (the window never waits for it), then the new one
//! starts. The environment editor's passwords go to the keychain off the
//! UI thread; deleting an environment stops its engine, then deletes its
//! password and its event log (ENV-03).

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

use futures::FutureExt as _;
use futures::StreamExt as _;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use futures::channel::oneshot;
use futures::future::Shared;
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
/// How long quitting waits for the engines (each stops within 5 s, side
/// by side).
const ENGINES_STOP_TIMEOUT: Duration = Duration::from_secs(7);

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
    /// Every environment's engine run, by environment id.
    engines: HashMap<String, EngineRun>,
    /// The demo's servers by environment id.
    demo: HashMap<String, DemoServer>,
    /// The demo's passwords (servers' and the editor's), in memory.
    demo_secrets: Arc<DemoSecrets>,
    demo_dir: Option<tempfile::TempDir>,
    pending_open: Option<OpenAtStart>,
    /// The settings file's problem last reported (an edit that doesn't
    /// read), so coming back to the window doesn't repeat it.
    reported_file_error: Option<String>,
    /// Shows notifications on the desktop.
    desktop: Box<dyn Desktop>,
    /// Recent notifications' tags and objects, for their clicks.
    targets: VecDeque<Target>,
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

/// One environment's engine as the session runs it.
#[derive(Default)]
struct EngineRun {
    /// Drains the engine's events into the state.
    pump: Option<Task<()>>,
    /// Counts the engine's replacements, so a start that was waiting (for
    /// a demo server, for the log's notifications) can tell it's no
    /// longer wanted.
    generation: u64,
    /// The engine stopping now; the next one starts when it has.
    stopping: Option<Task<()>>,
    /// Completes when the engine stopping now has stopped, also for the
    /// quit, which waits for it.
    stopped: Option<Shared<oneshot::Receiver<()>>>,
    /// Run once the engine stopping now has stopped (a deleted
    /// environment's password and event log).
    cleanups: Vec<Cleanup>,
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
            engines: HashMap::new(),
            demo: HashMap::new(),
            demo_secrets,
            demo_dir: None,
            pending_open,
            reported_file_error: None,
            desktop,
            targets: VecDeque::new(),
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
            Ok(persistence) => state.update(cx, |state, _| {
                // What the file holds now: an edit by hand shows as a
                // difference from it (`reload_settings_file`).
                if state.config_problem().is_none() {
                    persistence.read_from_disk(state.config().clone());
                }
                state.set_persistence(persistence);
            }),
            Err(error) => {
                tracing::error!(%error, "the settings writer couldn't start; nothing will be saved");
                state.update(cx, |state, _| {
                    state.on_saved(SaveReport::Config(Err(format!(
                        "the settings writer couldn't start: {error}"
                    ))));
                });
            }
        }
        cx.spawn(async move |this, cx| {
            while let Some(report) = reports.next().await {
                if this
                    .update(cx, |session, cx| session.on_saved(report, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
    }

    /// A save finished: an edited settings file is merged and taken over
    /// (nothing was written over it), an unreadable one reported.
    fn on_saved(&mut self, report: SaveReport, cx: &mut Context<Self>) {
        match report {
            SaveReport::FileEdited { file, known } => {
                let current = self.state.read(cx).settings_on_disk();
                if current.as_ref() == Some(&*known) {
                    self.take_settings_file(*file, Some(&known), cx);
                } else {
                    // Taken over meanwhile (the window came to the front):
                    // the refused save goes again, merged with it.
                    self.state.update(cx, |state, _| state.save_settings());
                }
            }
            SaveReport::FileUnreadable(error) => self.file_unreadable(&error, cx),
            report => self.state.update(cx, |state, cx| {
                state.on_saved(report);
                cx.notify();
            }),
        }
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

    /// Shows an engine's intent on the desktop (NOTE-01, A1): from every
    /// environment, whichever is on screen. With more than one environment
    /// the title starts with the environment's name. *Acknowledge* is
    /// offered for a problem that environment's API user may acknowledge;
    /// it goes to that environment's engine, whichever is active by then.
    fn post(&mut self, raised: &Raised, cx: &mut Context<Self>) {
        let intent = &raised.intent;
        let trouble = ic_core::trouble::is_trouble_id(&intent.id);
        let (acknowledge, name, prefix, output, persistent) = {
            let state = self.state.read(cx);
            let Some(environment) = state.environment_by_id(&raised.environment) else {
                tracing::debug!(id = %intent.id, "a notification of a removed environment was dropped");
                return;
            };
            let acknowledge = state
                .action_denial_in(&raised.environment, &ObjectAction::Acknowledge)
                .is_none();
            // A trouble alert names its environment already.
            let prefix =
                (state.environments().len() > 1 && !trouble).then(|| environment.name.clone());
            let output = state.config().general.show_plugin_output;
            let persistent = environment.trouble.policy == ic_config::TroublePolicy::Persistent;
            (
                acknowledge,
                environment.name.clone(),
                prefix,
                output,
                persistent,
            )
        };
        if let Some(object) = &intent.object {
            self.targets.push_front(Target {
                tag: intent.id.clone(),
                environment: raised.environment.clone(),
                object: object.clone(),
            });
            self.targets.truncate(MAX_TARGETS);
        }
        let mut posted = desktop::posted(intent, &name, acknowledge, output);
        if let Some(name) = prefix {
            posted.title = desktop::prefixed_title(&posted.title, &name);
        }
        if trouble {
            posted = desktop::trouble(posted, intent, persistent);
        }
        self.desktop.show(posted, cx);
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

    /// A desktop notification was clicked (NOTE-01, A1): it counts as read
    /// and the window comes back (opened again if it was closed). A click
    /// on it or *Open* switches to its environment and shows the object;
    /// *Acknowledge* opens the acknowledge dialog for the object in its own
    /// environment, without switching: the acknowledgement goes to that
    /// environment's engine, never to the one on screen.
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
        let (exists, active) = {
            let state = self.state.read(cx);
            (
                state.environment_by_id(&target.environment).is_some(),
                state.is_active(&target.environment),
            )
        };
        if !exists {
            window::show(cx);
            self.state.update(cx, |state, cx| {
                state.inform(
                    "That notification is from a removed environment",
                    Some(format!(
                        "It was about {}.",
                        crate::operate::forms::describe_objects(std::slice::from_ref(
                            &target.object
                        ))
                    )),
                );
                cx.notify();
            });
            return;
        }
        self.state.update(cx, |state, cx| {
            if state.mark_notification_read_in(&target.environment, &target.tag) {
                cx.notify();
            }
        });
        let acknowledge = ActionRequest {
            action: ObjectAction::Acknowledge,
            targets: vec![target.object.clone()],
            review: false,
        };
        if response.action.as_deref() == Some(ACKNOWLEDGE_ACTION) && !active {
            window::show(cx);
            self.state.update(cx, |state, cx| {
                // A refusal (no permission) shows as a toast.
                let _ = state.request_in(&target.environment, acknowledge);
                cx.notify();
            });
            return;
        }
        if !active {
            // The object shows once the window followed the switch.
            self.switch_environment(&target.environment, cx);
            let object = target.object.clone();
            cx.defer(move |cx| {
                if !open_object(&object, cx) {
                    tracing::warn!(%object, "no window to show the notification's object in");
                }
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
                let _ = state.request(acknowledge);
                cx.notify();
            });
        }
    }

    /// Where the settings, data and logs are (`None` in the demo, which
    /// keeps nothing).
    pub(crate) fn paths(&self) -> Option<&Paths> {
        match &self.launch {
            Launch::Live { paths, .. } => Some(paths),
            Launch::Demo { .. } => None,
        }
    }

    /// What the desktop was asked to show (tests).
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn shown_notifications(&self) -> Vec<desktop::Posted> {
        self.shown.shown.borrow().clone()
    }

    /// Starts the engine of every environment that has none yet (at start,
    /// also with `--background`, and once the settings were recovered).
    /// The demo's environments start once their server runs. Without an
    /// environment (or while the settings can't be read) nothing starts.
    pub(crate) fn start(&mut self, cx: &mut Context<Self>) {
        let ids: Vec<String> = self
            .state
            .read(cx)
            .environments()
            .iter()
            .map(|environment| environment.id.clone())
            .collect();
        if ids.is_empty() {
            tracing::info!("no environment configured; nothing to connect to");
        }
        for id in ids {
            if !self.engines.contains_key(&id) {
                self.start_environment(&id, cx);
            }
        }
    }

    /// Starts environment `id`'s engine (for the demo's environments, once
    /// their server runs).
    fn start_environment(&mut self, id: &str, cx: &mut Context<Self>) {
        self.engines.entry(id.to_owned()).or_default();
        match self.launch.clone() {
            Launch::Live { paths, .. } => {
                let secrets = self.secrets.clone();
                self.start_core(id, paths.data_dir, secrets, cx);
            }
            Launch::Demo { options } => match demo::options_for(id, &options) {
                Some(options) => self.start_demo(id, &options, cx),
                // Added in the editor while the demo runs: a real one.
                None => match self.demo_data_dir() {
                    Ok(data_dir) => {
                        let secrets = self.secrets.clone();
                        self.start_core(id, data_dir, secrets, cx);
                    }
                    Err(error) => self.engine_failed(id, error, cx),
                },
            },
        }
    }

    /// Reports that environment `id`'s engine couldn't start.
    fn engine_failed(&self, id: &str, error: String, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            state.engine_failed_for(id, error);
            cx.notify();
        });
    }

    /// The current generation of environment `id`'s engine.
    fn generation(&self, id: &str) -> u64 {
        self.engines.get(id).map_or(0, |run| run.generation)
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
            self.start_demo_core(id, cx);
            return;
        }
        let (server, endpoint) = match demo::start(options) {
            Ok(started) => started,
            Err(error) => {
                self.engine_failed(id, format!("the demo server couldn't start: {error}"), cx);
                return;
            }
        };
        self.demo.insert(id.to_owned(), server);
        let generation = self.generation(id);
        let id = id.to_owned();
        cx.spawn(async move |this, cx| {
            let endpoint = endpoint.await;
            let _ = this.update(cx, |session, cx| {
                let still_wanted = session.generation(&id) == generation
                    && session.state.read(cx).environment_by_id(&id).is_some();
                let endpoint = match endpoint {
                    Ok(Ok((endpoint, control))) => {
                        session.keep_demo_control(&id, control);
                        endpoint
                    }
                    Ok(Err(error)) => {
                        session.demo.remove(&id);
                        if still_wanted {
                            session.engine_failed(
                                &id,
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
                // The environment may have been deleted, or its engine
                // replaced, meanwhile.
                if still_wanted {
                    session.start_demo_core(&id, cx);
                }
            });
        })
        .detach();
    }

    /// Starts the engine for demo environment `id`, whose server runs.
    fn start_demo_core(&mut self, id: &str, cx: &mut Context<Self>) {
        match self.demo_data_dir() {
            Ok(data_dir) => {
                // `prod-cluster`'s database hosts get a recent history
                // (4a's event stream), once per demo.
                if id == demo::ENVIRONMENT_ID && !ic_core::event_log_path(&data_dir, id).exists() {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0., |since| since.as_secs_f64());
                    if let Err(error) =
                        ic_core::seed_event_log(&data_dir, id, &demo::recent_events(now))
                    {
                        tracing::warn!(%error, "couldn't seed the demo's event log");
                    }
                }
                let secrets = self.secrets.clone();
                self.start_core(id, data_dir, secrets, cx);
            }
            Err(error) => self.engine_failed(id, error, cx),
        }
    }

    /// Starts environment `id`'s engine and its event pump.
    fn start_core(
        &mut self,
        id: &str,
        data_dir: PathBuf,
        secrets: Arc<dyn SecretStore>,
        cx: &mut Context<Self>,
    ) {
        let (environment, general, hide_handled, start) = {
            let state = self.state.read(cx);
            (
                state.environment_by_id(id).cloned(),
                state.config().general.clone(),
                state.handled_defaults(),
                state.start_mode(),
            )
        };
        let Some(environment) = environment else {
            tracing::debug!(environment = %id, "removed before its engine started");
            self.engines.remove(id);
            return;
        };
        let name = environment.name.clone();
        let notifier = self.engine_notifier(id);
        let spec = EnvironmentSpec {
            environment,
            general,
            hide_handled,
            data_dir,
            // Started at login without the window, the first load waits a
            // moment proportional to the installation (PERF-09) until the
            // window shows (`Command::StartNow`).
            start,
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
                    state.set_core_for(id, Box::new(handle));
                    cx.notify();
                    state.request_notifications_of(id)
                });
                if let Some(recent) = recent {
                    // The notification centre starts with the log's,
                    // unless this engine was replaced meanwhile (its
                    // notifications are reloaded by the next one).
                    let generation = self.generation(id);
                    let id = id.to_owned();
                    cx.spawn(async move |this, cx| {
                        if let Ok(records) = recent.await {
                            let _ = this.update(cx, |session, cx| {
                                if session.generation(&id) != generation {
                                    tracing::debug!(
                                        "a replaced engine's notifications were dropped"
                                    );
                                    return;
                                }
                                session.state.update(cx, |state, cx| {
                                    state.load_notifications_of(&id, records);
                                    cx.notify();
                                });
                            });
                        }
                    })
                    .detach();
                }
                if let Some(events) = events {
                    let pump = self.spawn_pump(id, events, cx);
                    self.engines.entry(id.to_owned()).or_default().pump = Some(pump);
                }
                tracing::info!(environment = %name, "engine started");
            }
            Err(error) => {
                tracing::error!(%error, environment = %name, "the engine couldn't start");
                self.engine_failed(id, error.to_string(), cx);
            }
        }
    }

    /// The notifier for an engine of the environment `id`: its intents
    /// carry that environment.
    pub(crate) fn engine_notifier(&self, id: &str) -> GpuiNotifier {
        GpuiNotifier::new(self.intents.clone(), id)
    }

    /// Drains environment `id`'s engine's events into the state, a batch
    /// per re-render.
    fn spawn_pump(
        &self,
        id: &str,
        mut events: UnboundedReceiver<CoreEvent>,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        let state = self.state.downgrade();
        let id = id.to_owned();
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
                        state.apply_from(&id, event);
                    }
                    cx.notify();
                });
                if applied.is_err() {
                    return;
                }
                let _ = this.update(cx, |session, cx| session.after_events(&id, cx));
            }
            // The session drops the pump before it stops an engine, so the
            // stream only ends here when the engine died (a panic on its
            // thread): say so instead of showing stale data as live.
            tracing::error!(environment = %id, "the engine's event stream ended: the engine stopped on its own");
            let _ = this.update(cx, |session, cx| session.engine_stopped_on_its_own(&id, cx));
        })
    }

    /// Environment `id`'s engine died: its link goes (commands to it would
    /// vanish), the footer and a banner say so and offer a restart
    /// (ENV-07).
    fn engine_stopped_on_its_own(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(run) = self.engines.get_mut(id) {
            run.pump = None;
        }
        self.state.update(cx, |state, cx| {
            state.engine_stopped_for(
                id,
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
        let Some(id) = self
            .state
            .read(cx)
            .active_environment_id()
            .map(str::to_owned)
        else {
            return;
        };
        tracing::info!("restarting the engine");
        self.replace_engine(&id, None, cx);
    }

    /// Ends the active environment's event stream as an engine that died
    /// would (tests).
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn end_event_stream(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self
            .state
            .read(cx)
            .active_environment_id()
            .map(str::to_owned)
        else {
            return;
        };
        let (sender, events) = unbounded();
        drop(sender);
        let pump = self.spawn_pump(&id, events, cx);
        self.engines.entry(id).or_default().pump = Some(pump);
    }

    /// Replaces environment `id`'s engine: the running one stops on
    /// another thread, the cleanups run, then an engine starts for the
    /// environment (unless it was deleted). Requests while one stops fold
    /// into it. The environment's slot starts over at once.
    fn replace_engine(&mut self, id: &str, cleanup: Option<Cleanup>, cx: &mut Context<Self>) {
        let core = self.state.update(cx, |state, cx| {
            let core = state.take_core_of(id);
            state.reset_environment(id);
            cx.notify();
            core
        });
        self.stop_engine(id, core, cleanup, cx);
    }

    /// Stops environment `id`'s engine (`core`, taken from the state) on
    /// another thread; then the cleanups run and, if the environment still
    /// exists, its next engine starts.
    fn stop_engine(
        &mut self,
        id: &str,
        core: Option<Box<dyn crate::app_state::CoreLink>>,
        cleanup: Option<Cleanup>,
        cx: &mut Context<Self>,
    ) {
        let run = self.engines.entry(id.to_owned()).or_default();
        run.generation += 1;
        run.pump = None;
        run.cleanups.extend(cleanup);
        match core {
            Some(core) => {
                let stopped = core.shutdown_in_background().shared();
                run.stopped = Some(stopped.clone());
                let id = id.to_owned();
                run.stopping = Some(cx.spawn(async move |this, cx| {
                    // Cancelled means it stopped without saying so.
                    let _ = stopped.await;
                    let _ = this.update(cx, |session, cx| session.engine_stopped(&id, cx));
                }));
            }
            // The engine stopping now: the next one starts after it.
            None if run.stopping.is_some() => {}
            None => self.engine_stopped(id, cx),
        }
    }

    /// Environment `id`'s old engine has stopped: run the cleanups, then
    /// start its next engine (unless the environment was deleted).
    fn engine_stopped(&mut self, id: &str, cx: &mut Context<Self>) {
        let cleanups = match self.engines.get_mut(id) {
            Some(run) => {
                run.stopping = None;
                run.stopped = None;
                std::mem::take(&mut run.cleanups)
            }
            None => Vec::new(),
        };
        if cleanups.is_empty() {
            if self.state.read(cx).environment_by_id(id).is_some() {
                self.start_environment(id, cx);
            } else {
                self.engines.remove(id);
                self.demo.remove(id);
            }
            return;
        }
        let work = cx.background_executor().spawn(async move {
            cleanups
                .into_iter()
                .flat_map(|cleanup| cleanup())
                .collect::<Vec<_>>()
        });
        let stopped = id.to_owned();
        let task = cx.spawn(async move |this, cx| {
            let problems = work.await;
            let _ = this.update(cx, |session, cx| {
                session.report_leftovers(&problems, cx);
                session.engine_stopped(&stopped, cx);
            });
        });
        if let Some(run) = self.engines.get_mut(id) {
            run.stopping = Some(task);
        } else {
            task.detach();
        }
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

    /// Makes `id` the active environment (ENV-01): at once, since its
    /// engine runs already (one that never started, starts). Returns
    /// whether it switched.
    pub(crate) fn switch_environment(&mut self, id: &str, cx: &mut Context<Self>) -> bool {
        let switched = self.state.update(cx, |state, cx| {
            let switched = state.switch_environment(id);
            cx.notify();
            switched
        });
        if switched && !self.engines.contains_key(id) {
            self.start_environment(id, cx);
        }
        switched
    }

    /// Saves an environment from the editor (ENV-02): its password (if one
    /// was typed) into the keychain first, off the UI thread, then the
    /// settings; a new environment's engine starts, and one whose
    /// connection changed restarts.
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
                let account = account.clone();
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
                    cx.notify();
                    saved
                });
                match saved {
                    EnvironmentSaved::AddedActive | EnvironmentSaved::Added => {
                        session.start_environment(&account, cx);
                    }
                    EnvironmentSaved::Reconnect => session.replace_engine(&account, None, cx),
                    EnvironmentSaved::InPlace | EnvironmentSaved::Unchanged => {}
                }
                saved
            })
            .map_err(|_| "the window closed".to_owned())
        })
    }

    /// Deletes an environment (ENV-03): its settings and dashboards, then
    /// (once its engine stopped) its password and its event log. The next
    /// environment, if any, becomes active when it was the active one.
    /// Returns whether it existed.
    pub(crate) fn delete_environment(&mut self, id: &str, cx: &mut Context<Self>) -> bool {
        let removed = self.state.update(cx, |state, cx| {
            let core = state.take_core_of(id);
            let removed = state.remove_environment(id);
            cx.notify();
            removed.map(|_| core)
        });
        let Some(core) = removed else {
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
        self.stop_engine(id, core, Some(cleanup), cx);
        // The environment shown now may never have started (its engine
        // failed): every remaining one runs.
        self.start(cx);
        true
    }

    /// Trusts `fingerprint` for environment `id`'s URL `url` (ENV-05,
    /// trust on first use; pins are per URL): pins it and restarts that
    /// environment's engine.
    pub(crate) fn trust_certificate(
        &mut self,
        id: &str,
        url: &str,
        fingerprint: &str,
        cx: &mut Context<Self>,
    ) {
        let pinned = self.state.update(cx, |state, cx| {
            let pinned = state.pin_certificate(id, url, fingerprint);
            cx.notify();
            pinned
        });
        if pinned {
            self.replace_engine(id, None, cx);
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
    /// object shows (after events of environment `id`; only the active
    /// one's count).
    fn after_events(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(open) = self.pending_open.clone() else {
            return;
        };
        let state = self.state.read(cx);
        if !state.is_active(id) || !state.connection().is_connected() {
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
            OpenAtStart::List { .. } => !state.snapshot().services.is_empty(),
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
            OpenAtStart::List { kind, chip, mode } => self.state.update(cx, |state, cx| {
                if let Some(mode) = mode {
                    let mut options =
                        crate::lists::model::Options::saved(kind, &state.list_options(kind));
                    options.pick_mode(mode);
                    state.set_list_options(kind, options.to_saved(kind));
                }
                let changed = match chip {
                    Some(chip) => state.open_list_on(kind, chip),
                    None => state.open_list(kind),
                };
                if changed {
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
                        state.settings_read(&config);
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

    /// The window came back to the front: takes over the settings file
    /// if it was edited meanwhile (*edit in settings file*), read off the
    /// UI thread and merged with the window's own changes. A file that
    /// can't be read is reported once and changes nothing; a missing one
    /// is left alone.
    pub(crate) fn reload_settings_file(&mut self, cx: &mut Context<Self>) {
        let Some(store) = self.paths().map(Paths::config_store) else {
            return;
        };
        let known = {
            let state = self.state.read(cx);
            if state.config_problem().is_some() {
                return;
            }
            state.settings_on_disk()
        };
        let Some(known) = known else {
            return;
        };
        let read = cx
            .background_executor()
            .spawn(async move { store.read().map_err(|error| error.to_string()) });
        cx.spawn(async move |this, cx| {
            let result = read.await;
            let _ = this.update(cx, |session, cx| {
                let (current, in_window) = {
                    let state = session.state.read(cx);
                    (state.settings_on_disk(), state.config().clone())
                };
                if current.as_ref() != Some(&known) {
                    // icygui wrote the file while it was read: this read is
                    // out of date (the writer checks the file itself).
                    return;
                }
                match result {
                    Ok(None) => {}
                    Ok(Some(file)) => {
                        let had_error = session.reported_file_error.take().is_some();
                        session.state.update(cx, |state, cx| {
                            state.set_file_error(None);
                            cx.notify();
                        });
                        // Edited, or holding back changes icygui couldn't
                        // write while it didn't read.
                        if file != known || (had_error && in_window != known) {
                            session.take_settings_file(file, Some(&known), cx);
                        }
                    }
                    Err(error) => session.file_unreadable(&error, cx),
                }
            });
        })
        .detach();
    }

    /// The settings file can't be read (an edit by hand with a mistake):
    /// said once per problem, in a banner and in the settings panel's
    /// header; nothing is written over it until it reads again.
    fn file_unreadable(&mut self, error: &str, cx: &mut Context<Self>) {
        let repeated = self.reported_file_error.as_deref() == Some(error);
        self.reported_file_error = Some(error.to_owned());
        self.state.update(cx, |state, cx| {
            state.set_file_error(Some(error.to_owned()));
            if !repeated {
                tracing::warn!(%error, "the edited settings file can't be read");
                state.report(UserNotice::problem(
                    "The settings file can't be read; icygui keeps its settings.",
                    format!(
                        "{error}. Nothing is written over the file until it reads again; \
                         changes made in icygui meanwhile are kept and added to it then."
                    ),
                ));
            }
            cx.notify();
        });
    }

    /// Takes over settings edited in the file, merged with the window's
    /// changes since `known`: the state takes them, then engines stop,
    /// restart or start as their environments changed, and the login
    /// entry follows *start at login*.
    fn take_settings_file(&mut self, file: Config, known: Option<&Config>, cx: &mut Context<Self>) {
        let changes = self.state.update(cx, |state, cx| {
            let changes = state.take_settings_from_file(file, known);
            cx.notify();
            changes
        });
        let Some(changes) = changes else {
            return;
        };
        for (id, core) in changes.removed {
            self.stop_engine(&id, core, None, cx);
        }
        for id in &changes.reconnect {
            self.replace_engine(id, None, cx);
        }
        if !changes.added.is_empty() {
            self.start(cx);
        }
        if let Some(enabled) = changes.launch_at_login {
            crate::background::autostart::change(enabled, &self.state, cx);
        }
        self.state.update(cx, |state, cx| {
            state.inform("Settings taken over from the settings file", None);
            cx.notify();
        });
    }

    /// Stops every engine (side by side, each with its bounded wait),
    /// finishes what waits for engines already stopping (a deleted
    /// environment's password and event log, ENV-03) and writes what is
    /// queued. Runs when the app quits.
    fn stop(&mut self, cx: &mut Context<Self>) {
        let mut stopping = Vec::new();
        let mut cleanups = Vec::new();
        for run in self.engines.values_mut() {
            run.pump = None;
            run.stopping = None;
            stopping.extend(run.stopped.take().map(Stopping::Earlier));
            cleanups.append(&mut run.cleanups);
        }
        let cores = self.state.update(cx, |state, _| state.take_all_cores());
        let count = cores.len();
        stopping.extend(
            cores
                .into_iter()
                .map(|core| Stopping::Now(core.shutdown_in_background())),
        );
        wait_for_all(stopping, ENGINES_STOP_TIMEOUT);
        run_cleanups_before_quitting(cleanups, CLEANUP_TIMEOUT);
        if !self.state.read(cx).flush_persistence(FLUSH_TIMEOUT) {
            tracing::warn!("some settings may not have been saved before quitting");
        }
        self.engines.clear();
        self.demo.clear();
        self.demo_dir = None;
        tracing::info!(engines = count, "stopped");
    }
}

/// An engine stopping when the app quits: told to now, or earlier (a
/// deleted environment's, or one being replaced).
enum Stopping {
    Now(oneshot::Receiver<()>),
    Earlier(Shared<oneshot::Receiver<()>>),
}

impl Stopping {
    /// Whether it stopped (or will never say).
    fn done(&mut self) -> bool {
        match self {
            Self::Now(receiver) => !matches!(receiver.try_recv(), Ok(None)),
            Self::Earlier(stopped) => stopped.clone().now_or_never().is_some(),
        }
    }
}

/// How long quitting waits for the cleanups of deleted environments (the
/// keychain, the event log) after their engines stopped.
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(3);

/// Runs the cleanups still waiting when the app quits, on another thread,
/// waiting at most `timeout`; what they couldn't remove is logged (the
/// window is gone).
fn run_cleanups_before_quitting(cleanups: Vec<Cleanup>, timeout: Duration) {
    if cleanups.is_empty() {
        return;
    }
    let (done, finished) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("icygui-cleanup".to_owned())
        .spawn(move || {
            let problems: Vec<String> =
                cleanups.into_iter().flat_map(|cleanup| cleanup()).collect();
            let _ = done.send(problems);
        });
    if let Err(error) = spawned {
        tracing::warn!(%error, "a deleted environment's password and event log couldn't be removed");
        return;
    }
    if let Ok(problems) = finished.recv_timeout(timeout) {
        for problem in problems {
            tracing::warn!(%problem, "a deleted environment left something behind");
        }
    } else {
        tracing::warn!(
            "removing a deleted environment's password and event log didn't finish before quitting"
        );
    }
}

/// Waits until every engine stopped (or never will say), at most
/// `timeout` in all (the engines stop on their own threads meanwhile).
fn wait_for_all(mut stopping: Vec<Stopping>, timeout: Duration) {
    let deadline = std::time::Instant::now() + timeout;
    while !stopping.is_empty() {
        stopping.retain_mut(|stopping| !stopping.done());
        if stopping.is_empty() {
            break;
        }
        if std::time::Instant::now() >= deadline {
            tracing::warn!(left = stopping.len(), "some engines didn't stop in time");
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
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
        self.demo_control_of(demo::ENVIRONMENT_ID)
    }

    /// Demo environment `id`'s server's control, once it runs.
    pub(crate) fn demo_control_of(&self, id: &str) -> Option<ic_mock::MockControl> {
        self.demo_controls.get(id).cloned()
    }

    /// Whether environment `id` has an engine run (started, or stopping).
    pub(crate) fn has_engine(&self, id: &str) -> bool {
        self.engines.contains_key(id)
    }

    /// Where environment `id`'s event log is (or would be).
    pub(crate) fn event_log_of(&self, id: &str) -> Option<PathBuf> {
        self.event_log_dir()
            .map(|dir| ic_core::event_log_path(&dir, id))
    }

    /// Whether environment `id` has a password in the (demo) keychain.
    pub(crate) fn has_password(&self, id: &str) -> bool {
        self.secrets.get(id).ok().flatten().is_some()
    }

    /// What quitting does: stops every engine and finishes what waits.
    pub(crate) fn quit_now(&mut self, cx: &mut Context<Self>) {
        self.stop(cx);
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
