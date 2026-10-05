//! Icinga 2 desktop client.

mod actions;
mod app_state;
mod chrome;
mod dashboard;
mod demo;
mod dev;
mod format;
mod pane;
mod sidebar;
#[cfg(test)]
#[cfg(target_os = "linux")]
mod ui_tests;
mod workspace;

use std::time::Duration;

use gpui::{
    App, AppContext as _, Bounds, Entity, Size, TitlebarOptions, WindowBackgroundAppearance,
    WindowBounds, WindowDecorations, WindowHandle, WindowOptions, point, px, size,
};
use gpui_platform::application;
use ic_model::Timestamp;
use ic_ui_kit::Root;
use tracing_subscriber::EnvFilter;

use crate::app_state::AppState;
use crate::chrome::ControlsPreference;
use crate::demo::DemoOptions;
use crate::dev::{DevOptions, OpenAtStart};
use crate::workspace::Workspace;

/// Reverse-DNS application id (Wayland `app_id`, bundle id, notification identity).
const APP_ID: &str = "io.github.alexykn.icygui";
const APP_NAME: &str = "icygui";

/// How often the demo pretends an event arrived, so its connection stays
/// "live" in the footer.
const DEMO_EVENT_INTERVAL: Duration = Duration::from_secs(2);

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    application()
        .with_assets(ic_ui_kit::Assets)
        .run(|cx: &mut App| {
            cx.set_app_identity(APP_ID, APP_NAME);
            if let Err(error) = ic_ui_kit::init(cx) {
                tracing::error!(%error, "cannot start without the bundled fonts");
                cx.quit();
                return;
            }
            cx.set_global(ControlsPreference::from_env());
            workspace::bind_keys(cx);

            // Until the live core lands, the window shows the built-in demo.
            let dev = DevOptions::from_env();
            if dev.any() {
                tracing::info!(?dev, "development switches are set");
            }
            let state = cx.new(|_| demo_state(&dev, Timestamp::now()));
            simulate_demo_events(&state, cx);

            match open_main_window(state.clone(), cx) {
                Ok(window) => open_at_start(dev.open, &state, window, cx),
                Err(error) => {
                    tracing::error!(error = %format!("{error:#}"), "failed to open the main window");
                    cx.quit();
                }
            }
        });
}

/// The demo, with the development switches applied.
fn demo_state(dev: &DevOptions, now: Timestamp) -> AppState {
    let mut state = AppState::demo_with(
        now,
        DemoOptions {
            generated_rows: dev.generated_rows,
        },
    );
    if let Some(name) = &dev.dashboard {
        if let Some(reference) = state.dashboard_named(name) {
            state.select(reference);
        } else {
            tracing::warn!(%name, "{} names no demo dashboard", dev::DASHBOARD_ENV);
        }
    }
    state
}

/// Opens what `ICYGUI_DEMO_OPEN` asks for.
fn open_at_start(
    open: Option<OpenAtStart>,
    state: &Entity<AppState>,
    window: WindowHandle<Root>,
    cx: &mut App,
) {
    let Some(open) = open else {
        return;
    };
    let workspace = window
        .read(cx)
        .ok()
        .and_then(|root| root.view().clone().downcast::<Workspace>().ok());
    let Some(workspace) = workspace else {
        return;
    };
    let dashboard = workspace.read(cx).dashboard().clone();
    match open {
        OpenAtStart::Object(key) => dashboard.update(cx, |view, cx| view.open_object(&key, cx)),
        OpenAtStart::Linked { cursor, pane } => {
            dashboard.update(cx, |view, cx| view.open_linked(&cursor, pane, cx));
        }
        OpenAtStart::Tab(key) => state.update(cx, |state, cx| {
            if state.open_tab(key) {
                cx.notify();
            }
        }),
    }
}

fn open_main_window(state: Entity<AppState>, cx: &mut App) -> anyhow::Result<WindowHandle<Root>> {
    let bounds = Bounds::centered(None, size(px(1440.), px(900.)), cx);
    let title = chrome::window_title(
        state
            .read(cx)
            .environment()
            .map(|environment| environment.name.as_str()),
    );
    let window = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
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

/// Keeps the demo's "last event" fresh, like a live event stream would.
fn simulate_demo_events(state: &Entity<AppState>, cx: &mut App) {
    if !state.read(cx).is_demo() {
        return;
    }
    let state = state.downgrade();
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(DEMO_EVENT_INTERVAL).await;
            let updated = state.update(cx, |state, cx| {
                state.record_event(Timestamp::now());
                cx.notify();
            });
            if updated.is_err() {
                break;
            }
        }
    })
    .detach();
}
