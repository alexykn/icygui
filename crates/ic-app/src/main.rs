//! Icinga 2 desktop client.
//!
//! `icygui` connects to the active environment of the settings file;
//! `icygui --demo` runs the same app against a built-in simulated Icinga.
//! `--background` starts in the tray without the window (launch at
//! login). `--version` and `--help` answer without opening a window. A
//! second launch hands over to the running instance and exits.

mod actions;
mod app_state;
mod appearance;
mod background;
mod banner;
mod chrome;
mod cli;
mod cluster;
mod controls;
mod dashboard;
mod dev;
mod downtimes;
mod editor;
mod environments;
#[cfg(test)]
mod fixture;
mod format;
mod keymap;
mod lists;
mod live;
mod logging;
mod menu_state;
mod notifications;
mod operate;
mod paging;
mod palette;
mod pane;
mod persist;
mod recovery;
mod settings;
mod sidebar;
#[cfg(test)]
#[cfg(target_os = "linux")]
mod ui_tests;
mod window_state;
mod workspace;

use std::process::ExitCode;
use std::sync::Arc;

use gpui::{
    App, AppContext as _, Bounds, Entity, Pixels, QuitMode, Size, TitlebarOptions,
    WindowBackgroundAppearance, WindowBounds, WindowDecorations, WindowHandle, WindowOptions,
    point, px,
};
use gpui_platform::application;
use ic_config::{Paths, UiState};
use ic_model::Timestamp;
use ic_ui_kit::Root;

use crate::app_state::AppState;
use crate::background::instance::{self, Claim, Instance, Request};
use crate::chrome::ControlsPreference;
use crate::cli::Invocation;
use crate::dev::DevOptions;
use crate::live::demo::DemoOptions;
use crate::live::{Launch, Session};
use crate::window_state::InitialBounds;
use crate::workspace::Workspace;

/// Reverse-DNS application id (Wayland `app_id`, bundle id, notification identity).
const APP_ID: &str = "io.github.alexykn.icygui";
const APP_NAME: &str = "icygui";

/// The main window's size at start: the design's.
#[expect(clippy::disallowed_methods, reason = "window geometry is real pixels")]
const WINDOW_SIZE: Size<Pixels> = Size {
    width: px(1440.),
    height: px(900.),
};

fn main() -> ExitCode {
    let options = match cli::parse(std::env::args_os().skip(1)) {
        Ok(Invocation::Run(options)) => options,
        Ok(Invocation::Version) => return print(&mut std::io::stdout(), &cli::version_text()),
        Ok(Invocation::Help) => return print(&mut std::io::stdout(), &cli::help_text()),
        Err(error) => {
            let _ = print(
                &mut std::io::stderr(),
                &format!("{APP_NAME}: {error}\n\n{}", cli::help_text()),
            );
            return ExitCode::from(2);
        }
    };
    let paths = Paths::from_system();
    let dirs = paths.as_ref().map(Paths::create_dirs);
    let log_file = logging::init(
        paths
            .as_ref()
            .ok()
            .filter(|_| matches!(dirs, Ok(Ok(()))))
            .map(|paths| paths.log_dir.as_path()),
    );
    tracing::info!(version = env!("CARGO_PKG_VERSION"), log = ?log_file, demo = options.demo, background = options.background, "starting");
    if let Ok(Err(error)) = &dirs {
        tracing::warn!(%error, "couldn't create icygui's directories");
    }
    // One instance per user (BG-04); the demo runs beside the real app.
    let instance = match (&paths, options.demo) {
        (Ok(paths), false) => {
            let request = if options.background {
                Request::Background
            } else {
                Request::Show
            };
            match instance::claim(&instance::directory(&paths.data_dir), request) {
                Ok(Claim::Primary(instance)) => Some(instance),
                Ok(Claim::Forwarded) => {
                    tracing::info!("icygui is already running; it was asked to show its window");
                    return ExitCode::SUCCESS;
                }
                Err(error) => {
                    tracing::error!(%error, "another instance doesn't answer");
                    let _ = print(&mut std::io::stderr(), &format!("{APP_NAME}: {error}\n"));
                    return ExitCode::FAILURE;
                }
            }
        }
        _ => None,
    };
    let launch = if options.demo {
        let dev = DevOptions::from_env();
        let defaults = DemoOptions::default();
        Startup::Demo {
            options: DemoOptions {
                scenario: dev.scenario.clone().unwrap_or(defaults.scenario),
                seed: dev.seed.unwrap_or(defaults.seed),
                fault: dev.fault,
                storm_every: dev.storm_every,
            },
            dev,
        }
    } else {
        match paths {
            Ok(paths) => Startup::Live { paths },
            Err(error) => {
                tracing::error!(%error, "no home directory: icygui can't find its settings");
                let _ = print(
                    &mut std::io::stderr(),
                    &format!("{APP_NAME}: {error}; try {APP_NAME} --demo\n"),
                );
                return ExitCode::FAILURE;
            }
        }
    };
    run(launch, options.background, instance);
    ExitCode::SUCCESS
}

/// Writes `text` (the help or the version) and flushes.
fn print(out: &mut impl std::io::Write, text: &str) -> ExitCode {
    match out.write_all(text.as_bytes()).and_then(|()| out.flush()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}

/// What to start.
enum Startup {
    Live {
        paths: Paths,
    },
    Demo {
        options: DemoOptions,
        dev: DevOptions,
    },
}

/// Runs the app until it quits: in the background (`--background`, BG-03)
/// without the window when a tray shows it. Closing the window keeps it
/// running in the tray when the settings say so (BG-01).
fn run(startup: Startup, background: bool, mut instance: Option<Instance>) {
    let app = application()
        .with_assets(ic_ui_kit::Assets)
        // Closing the last window doesn't quit by itself;
        // `background::window::closed` decides.
        .with_quit_mode(QuitMode::Explicit);
    // macOS: the dock icon or a launch from Finder while running.
    app.on_reopen(|cx| {
        background::window::show(cx);
    });
    app.run(move |cx: &mut App| {
        cx.set_app_identity(APP_ID, APP_NAME);
        if let Err(error) = ic_ui_kit::init(cx) {
            tracing::error!(%error, "cannot start without the bundled fonts");
            cx.quit();
            return;
        }
        cx.set_global(ControlsPreference::from_env());
        workspace::bind_keys(cx);
        background::menus::install(cx);
        // The user's own bindings on top of the defaults (the demo reads
        // them too: they belong to the user, not to the settings).
        if let Some(path) = keymap_file(&startup) {
            keymap::install(path, cx);
        }

        let now = Timestamp::now();
        let (state, launch, pending_open) = match startup {
            Startup::Live { paths } => {
                let state = live_state(&paths, now);
                let secrets = Arc::new(ic_platform::KeyringSecrets::new());
                (state, Launch::Live { paths, secrets }, None)
            }
            Startup::Demo { options, dev } => {
                if dev.any() {
                    tracing::info!(?dev, "development switches are set");
                }
                let mut config = live::demo::config();
                if let Some(count) = dev.environments {
                    live::demo::set_count(&mut config, count);
                }
                if let Some(appearance) = dev.appearance {
                    config.appearance = appearance;
                }
                let mut state = AppState::demo(config, now);
                if let Some(name) = &dev.dashboard {
                    if let Some(reference) = state.dashboard_named(name) {
                        state.select(reference);
                    } else {
                        tracing::warn!(%name, "{} names no demo dashboard", dev::DASHBOARD_ENV);
                    }
                }
                (state, Launch::Demo { options }, dev.open)
            }
        };
        // The settings are read: their log level from now on.
        logging::apply_setting(state.config().general.log_level);
        let bounds = window_state::initial_bounds(
            state.window_state(),
            &cx.displays()
                .iter()
                .map(|display| display.bounds())
                .collect::<Vec<_>>(),
            WINDOW_SIZE,
        );
        let recovering = state.config_problem().is_some();
        let (launch_at_login, demo) = (state.config().general.launch_at_login, state.is_demo());
        let state = cx.new(|_| state);
        let session = Session::install(state.clone(), launch, pending_open, cx);
        background::window::install(state.clone(), cx);
        background::tray::install(&state, cx);
        cx.on_window_closed(|cx, _| background::window::closed(cx))
            .detach();
        if let Some(requests) = instance.as_mut().and_then(Instance::requests) {
            spawn_instance_requests(requests, cx);
        }
        // The instance lives as long as the app.
        cx.set_global(RunningInstance {
            _instance: instance.take(),
        });
        background::autostart::refresh_at_start(launch_at_login, demo, cx);
        if !recovering && background::window::start_hidden(background, cx) {
            // Nobody looks yet: every environment is quiet and the first
            // loads wait a moment proportional to the installation's size
            // (PERF-09), until the window shows.
            state.update(cx, |state, _| state.start_hidden());
            background::window::await_tray_host(state, bounds, cx);
        } else if !background::window::open_at_start(state, bounds, cx) {
            cx.quit();
            return;
        }
        if !recovering {
            session.update(cx, Session::start);
        }
    });
}

/// Where the keymap file is: next to the settings file.
fn keymap_file(startup: &Startup) -> Option<std::path::PathBuf> {
    match startup {
        Startup::Live { paths } => Some(paths.keymap_file()),
        Startup::Demo { .. } => Paths::from_system().ok().map(|paths| paths.keymap_file()),
    }
}

/// The instance lock and listener, held while the app runs.
struct RunningInstance {
    _instance: Option<Instance>,
}

impl gpui::Global for RunningInstance {}

/// Later launches (BG-04): bring the window forward.
fn spawn_instance_requests(
    mut requests: futures::channel::mpsc::UnboundedReceiver<Request>,
    cx: &mut App,
) {
    use futures::StreamExt as _;
    cx.spawn(async move |cx| {
        while let Some(request) = requests.next().await {
            cx.update(|cx| match request {
                Request::Show => {
                    background::window::show(cx);
                }
                Request::Background => {
                    tracing::info!("started at login while running: nothing to do");
                }
            });
        }
    })
    .detach();
}

/// The live app's state: the settings (or the problem reading them) and the
/// UI state of the last run.
fn live_state(paths: &Paths, now: Timestamp) -> AppState {
    let ui = match paths.state_store().load() {
        Ok(ui) => ui,
        Err(error) => {
            tracing::warn!(%error, "the window and tab state can't be read; starting with defaults");
            UiState::default()
        }
    };
    match live::load_settings(&paths.config_store()) {
        Ok(config) => AppState::live(config, ui, now),
        Err(problem) => AppState::recovery(*problem, ui, now),
    }
}

/// Opens the main window where `bounds` says.
#[expect(
    clippy::disallowed_methods,
    reason = "window geometry is real pixels; the traffic lights are the system's"
)]
fn open_main_window(
    state: Entity<AppState>,
    bounds: InitialBounds,
    cx: &mut App,
) -> anyhow::Result<WindowHandle<Root>> {
    let window_bounds = match bounds {
        InitialBounds::Restored(bounds) => bounds,
        InitialBounds::Centered(size) => WindowBounds::Windowed(Bounds::centered(None, size, cx)),
    };
    let title = {
        let state = state.read(cx);
        chrome::window_title(
            state
                .environment()
                .map(|environment| environment.name.as_str()),
            state.is_demo_environment(),
        )
    };
    let window = cx.open_window(
        WindowOptions {
            window_bounds: Some(window_bounds),
            window_min_size: Some(Size {
                width: px(900.),
                height: px(560.),
            }),
            titlebar: Some(TitlebarOptions {
                title: Some(title),
                // macOS draws its traffic lights inside our sidebar header.
                appears_transparent: true,
                traffic_light_position: Some(point(px(12.), px(13.))),
            }),
            // The headers move the window (chrome::WindowDrag), so AppKit
            // doesn't drag from the titlebar strip itself.
            app_owns_titlebar_drag: cfg!(target_os = "macos"),
            // Linux: we draw the window controls; gpui-component's Root draws
            // the frame, shadow and resize edges. Falls back to server-side
            // decorations where the compositor can't do client-side ones.
            window_decorations: Some(WindowDecorations::Client),
            // The client-side frame's shadow needs a transparent surface; the
            // content paints its own opaque background.
            window_background: if cfg!(target_os = "linux") {
                WindowBackgroundAppearance::Transparent
            } else {
                WindowBackgroundAppearance::Opaque
            },
            app_id: Some(APP_ID.to_owned()),
            // X11 shows it in task bars and switchers (BG-06); Wayland
            // compositors and macOS take the icon of the desktop entry or
            // the app bundle that matches the app id.
            icon: window_icon(),
            ..WindowOptions::default()
        },
        |window, cx| {
            let workspace = cx.new(|cx| Workspace::new(state, window, cx));
            cx.new(|cx| Root::new(workspace, window, cx))
        },
    )?;
    cx.activate(true);
    Ok(window)
}

/// The app icon for the window (the 64 px rendering of the logo).
fn window_icon() -> Option<Arc<image::RgbaImage>> {
    const PNG: &[u8] = include_bytes!("../../../assets/icons/icygui-64.png");
    match image::load_from_memory_with_format(PNG, image::ImageFormat::Png) {
        Ok(icon) => Some(Arc::new(icon.into_rgba8())),
        Err(error) => {
            tracing::warn!(%error, "the window icon couldn't be decoded");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_window_icon_is_the_logo() {
        let icon = super::window_icon().expect("the bundled icon decodes");
        assert_eq!(icon.dimensions(), (64, 64));
        // Not blank: the logo's orange core is opaque.
        assert!(icon.pixels().any(|pixel| pixel.0[3] == 255));
    }
}
