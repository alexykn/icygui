//! Icinga 2 desktop client.
//!
//! `icygui` connects to the active environment of the settings file;
//! `icygui --demo` runs the same app against a built-in simulated Icinga.
//! `--version` and `--help` answer without opening a window.

mod actions;
mod app_state;
mod banner;
mod chrome;
mod cli;
mod dashboard;
mod dev;
mod editor;
mod environments;
#[cfg(test)]
mod fixture;
mod format;
mod live;
mod logging;
mod menu_state;
mod operate;
mod palette;
mod pane;
mod persist;
mod recovery;
mod sidebar;
#[cfg(test)]
#[cfg(target_os = "linux")]
mod ui_tests;
mod window_state;
mod workspace;

use std::process::ExitCode;
use std::sync::Arc;

use gpui::{
    App, AppContext as _, Bounds, Entity, Pixels, Size, TitlebarOptions,
    WindowBackgroundAppearance, WindowBounds, WindowDecorations, WindowHandle, WindowOptions,
    point, px,
};
use gpui_platform::application;
use ic_config::{Paths, UiState};
use ic_model::Timestamp;
use ic_ui_kit::Root;

use crate::app_state::AppState;
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
    tracing::info!(version = env!("CARGO_PKG_VERSION"), log = ?log_file, demo = options.demo, "starting");
    if let Ok(Err(error)) = &dirs {
        tracing::warn!(%error, "couldn't create icygui's directories");
    }
    let launch = if options.demo {
        let dev = DevOptions::from_env();
        let defaults = DemoOptions::default();
        Startup::Demo {
            options: DemoOptions {
                scenario: dev.scenario.clone().unwrap_or(defaults.scenario),
                seed: dev.seed.unwrap_or(defaults.seed),
                fault: dev.fault,
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
    run(launch);
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

/// Runs the app until it quits.
fn run(startup: Startup) {
    application()
        .with_assets(ic_ui_kit::Assets)
        .run(move |cx: &mut App| {
            cx.set_app_identity(APP_ID, APP_NAME);
            if let Err(error) = ic_ui_kit::init(cx) {
                tracing::error!(%error, "cannot start without the bundled fonts");
                cx.quit();
                return;
            }
            cx.set_global(ControlsPreference::from_env());
            workspace::bind_keys(cx);

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
                    let mut state = AppState::demo(live::demo::config(), now);
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
            let bounds = window_state::initial_bounds(
                state.window_state(),
                &cx.displays()
                    .iter()
                    .map(|display| display.bounds())
                    .collect::<Vec<_>>(),
                WINDOW_SIZE,
            );
            let recovering = state.config_problem().is_some();
            let state = cx.new(|_| state);
            let session = Session::install(state.clone(), launch, pending_open, cx);
            if let Err(error) = open_main_window(state, bounds, cx) {
                tracing::error!(error = %format!("{error:#}"), "failed to open the main window");
                cx.quit();
                return;
            }
            if !recovering {
                session.update(cx, Session::start);
            }
        });
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
            state.is_demo(),
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
