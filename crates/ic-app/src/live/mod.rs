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

pub(crate) mod demo;
pub(crate) mod notifier;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use futures::channel::mpsc::UnboundedReceiver;
use gpui::{
    App, AppContext as _, Context, Entity, Global, Subscription, SystemNotificationResponse, Task,
};
use ic_config::{Config, ConfigError, ConfigStore, Paths};
use ic_core::ports::SecretStore;
use ic_core::{CoreEvent, EnvironmentSpec, Ports, SystemClock};
use ic_model::ObjectKey;
use ic_rules::NotificationIntent;
use ic_ui_kit::Root;

use self::demo::{DemoOptions, DemoServer};
use self::notifier::GpuiNotifier;
use crate::app_state::{AppState, ConfigProblem};
use crate::dev::OpenAtStart;
use crate::persist::{Persistence, SaveReport};
use crate::workspace::Workspace;

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

/// The running session: the engine, its event pump, notifications and
/// persistence. One per app, reachable through [`session`].
pub(crate) struct Session {
    state: Entity<AppState>,
    launch: Launch,
    notifier: Arc<GpuiNotifier>,
    pump: Option<Task<()>>,
    demo: Option<DemoServer>,
    demo_dir: Option<tempfile::TempDir>,
    pending_open: Option<OpenAtStart>,
    /// Recent notifications' tags and objects, for their clicks.
    targets: VecDeque<(String, ObjectKey)>,
    /// The demo server's control, for tests that change the simulated
    /// Icinga.
    #[cfg(all(test, target_os = "linux"))]
    demo_control: Option<ic_mock::MockControl>,
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
        let (notifier, intents) = GpuiNotifier::new();
        let mut tasks = vec![Self::spawn_notifications(intents, cx)];
        if let Launch::Live { paths, .. } = &launch {
            tasks.push(Self::attach_persistence(&state, paths, cx));
        }
        let weak = cx.weak_entity();
        cx.on_system_notification_response(move |response, cx| {
            if let Some(session) = weak.upgrade() {
                session.update(cx, |session, cx| {
                    session.notification_clicked(&response, cx);
                });
            }
        });
        let quit = cx.on_app_quit(|session, cx| {
            session.stop(cx);
            async {}
        });
        Self {
            state,
            launch,
            notifier: Arc::new(notifier),
            pump: None,
            demo: None,
            demo_dir: None,
            pending_open,
            targets: VecDeque::new(),
            #[cfg(all(test, target_os = "linux"))]
            demo_control: None,
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
        let (sender, mut reports) = futures::channel::mpsc::unbounded::<SaveReport>();
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

    /// Posts the core's notifications on the UI thread.
    fn spawn_notifications(
        mut intents: UnboundedReceiver<NotificationIntent>,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        cx.spawn(async move |this, cx| {
            while let Some(intent) = intents.next().await {
                let shown = this.update(cx, |session, cx| {
                    if let Some(object) = &intent.object {
                        session
                            .targets
                            .push_front((intent.id.clone(), object.clone()));
                        session.targets.truncate(MAX_TARGETS);
                    }
                    cx.show_system_notification(notifier::system_notification(&intent));
                });
                if shown.is_err() {
                    break;
                }
            }
        })
    }

    /// A notification (or its "Open" button) was clicked: open its object.
    fn notification_clicked(
        &mut self,
        response: &SystemNotificationResponse,
        cx: &mut Context<Self>,
    ) {
        let target = self
            .targets
            .iter()
            .find(|(tag, _)| tag.as_str() == response.tag.as_ref())
            .map(|(_, object)| object.clone());
        match target {
            Some(object) => {
                if !open_object(&object, cx) {
                    tracing::info!(%object, "no window to open the notification's object in");
                }
            }
            None => {
                tracing::debug!(tag = %response.tag, "a notification without an object was clicked");
            }
        }
    }

    /// Connects: starts the engine for the active environment, or the demo
    /// server and then the engine. Without an environment (or while the
    /// settings can't be read) nothing starts.
    pub(crate) fn start(&mut self, cx: &mut Context<Self>) {
        match self.launch.clone() {
            Launch::Live { paths, secrets } => self.start_core(paths.data_dir, secrets, cx),
            Launch::Demo { options } => self.start_demo(&options, cx),
        }
    }

    fn start_demo(&mut self, options: &DemoOptions, cx: &mut Context<Self>) {
        let (server, endpoint) = match demo::start(options) {
            Ok(started) => started,
            Err(error) => {
                self.state.update(cx, |state, cx| {
                    state.engine_failed(format!("the demo server couldn't start: {error}"));
                    cx.notify();
                });
                return;
            }
        };
        self.demo = Some(server);
        cx.spawn(async move |this, cx| {
            let endpoint = endpoint.await;
            let _ = this.update(cx, |session, cx| {
                let endpoint = match endpoint {
                    Ok(Ok((endpoint, control))) => {
                        session.keep_demo_control(control);
                        endpoint
                    }
                    Ok(Err(error)) => {
                        session.state.update(cx, |state, cx| {
                            state.engine_failed(format!("the demo server couldn't start: {error}"));
                            cx.notify();
                        });
                        return;
                    }
                    Err(_) => return,
                };
                let Some(server) = session.demo.as_ref() else {
                    return;
                };
                let secrets = server.secrets();
                let (url, pin) = server.environment_target(&endpoint);
                session.state.update(cx, |state, _| {
                    state.set_demo_server(&url, pin.as_deref());
                });
                match tempfile::Builder::new().prefix("icygui-demo-").tempdir() {
                    Ok(dir) => {
                        let data_dir = dir.path().to_path_buf();
                        session.demo_dir = Some(dir);
                        session.start_core(data_dir, secrets, cx);
                    }
                    Err(error) => session.state.update(cx, |state, cx| {
                        state.engine_failed(format!(
                            "no directory for the demo's event log: {error}"
                        ));
                        cx.notify();
                    }),
                }
            });
        })
        .detach();
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
        let spec = EnvironmentSpec {
            environment,
            general,
            data_dir,
        };
        let ports = Ports {
            secrets,
            notifier: self.notifier.clone(),
            clock: Arc::new(SystemClock),
        };
        match ic_core::start(spec, ports) {
            Ok(mut handle) => {
                let events = handle.take_events();
                self.state.update(cx, |state, cx| {
                    state.set_core(Box::new(handle));
                    cx.notify();
                });
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
                    break;
                }
                let _ = this.update(cx, Self::after_events);
            }
            tracing::debug!("the engine's event stream ended");
        })
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
                with_workspace(cx, |workspace, window, cx| {
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
        self.demo = None;
        self.demo_dir = None;
        tracing::info!("stopped");
    }
}

impl Session {
    /// Keeps the demo server's control for tests (nothing else changes the
    /// simulated Icinga).
    #[cfg(all(test, target_os = "linux"))]
    fn keep_demo_control(&mut self, control: ic_mock::MockControl) {
        self.demo_control = Some(control);
    }

    #[cfg(not(all(test, target_os = "linux")))]
    #[expect(clippy::unused_self, reason = "only the UI tests keep the control")]
    fn keep_demo_control(&mut self, _control: ic_mock::MockControl) {}
}

#[cfg(all(test, target_os = "linux"))]
impl Session {
    /// The demo server's control, once it runs.
    pub(crate) fn demo_control(&self) -> Option<ic_mock::MockControl> {
        self.demo_control.clone()
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

/// Runs `f` on the main window's workspace, if the window is open.
fn with_workspace(
    cx: &mut App,
    f: impl FnOnce(&mut Workspace, &mut gpui::Window, &mut Context<Workspace>),
) -> bool {
    let Some(window) = cx
        .windows()
        .into_iter()
        .find_map(|window| window.downcast::<Root>())
    else {
        return false;
    };
    window
        .update(cx, |root, window, cx| {
            let Ok(workspace) = root.view().clone().downcast::<Workspace>() else {
                return false;
            };
            workspace.update(cx, |workspace, cx| f(workspace, window, cx));
            window.activate_window();
            true
        })
        .unwrap_or(false)
}

/// Shows `object`: in a dashboard that lists it, else as a tab, and
/// brings the window forward. Returns whether a window was there.
pub(crate) fn open_object(object: &ObjectKey, cx: &mut App) -> bool {
    with_workspace(cx, |workspace, window, cx| {
        workspace.reveal(object, window, cx);
    })
}
