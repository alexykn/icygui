//! Icinga 2 desktop client.

mod app_state;
mod chrome;
mod demo;
mod sidebar;
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
            let state = cx.new(|_| AppState::demo(Timestamp::now()));
            simulate_demo_events(&state, cx);

            if let Err(error) = open_main_window(state, cx) {
                tracing::error!(error = %format!("{error:#}"), "failed to open the main window");
                cx.quit();
            }
        });
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

// GPUI's headless platform runs on the test thread only on Linux (macOS
// needs the process's main thread).
#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use gpui::{AnyWindowHandle, Window};
    use ic_rules::DashboardRef;

    use super::*;
    use crate::workspace::ToggleSidebar;

    /// Draws `window` once. `update_window` passes the root as a view handle
    /// without leasing it, so the root can render inside.
    fn draw(window: AnyWindowHandle, cx: &mut App) {
        cx.update_window(window, |_, window, cx| window.draw(cx).clear(cx))
            .unwrap();
    }

    fn in_window(window: AnyWindowHandle, cx: &mut App, f: impl FnOnce(&mut Window, &mut App)) {
        cx.update_window(window, |_, window, cx| f(window, cx))
            .unwrap();
    }

    /// Opens the real window on GPUI's headless platform (no display server,
    /// no GPU: layout and text shaping run, painting is discarded), draws it
    /// and drives the sidebar and chrome through their main interactions.
    #[test]
    fn the_main_window_renders_and_reacts_headlessly() {
        // A hung event loop must fail the run instead of blocking it.
        let finished = Arc::new(AtomicBool::new(false));
        let watched = finished.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_mins(2));
            if !watched.load(Ordering::SeqCst) {
                eprintln!("the headless app didn't quit within two minutes");
                std::process::abort();
            }
        });
        let checks: Rc<RefCell<Vec<(&'static str, bool)>>> = Rc::default();
        let record = checks.clone();
        gpui_platform::headless()
            .with_assets(ic_ui_kit::Assets)
            .run(move |cx: &mut App| {
                let check = |name, ok| record.borrow_mut().push((name, ok));
                ic_ui_kit::init(cx).unwrap();
                // Draw our own traffic lights even though headless windows
                // have server-side decorations.
                cx.set_global(ControlsPreference::Always);
                workspace::bind_keys(cx);
                let state = cx.new(|_| AppState::demo(Timestamp::now()));
                let handle = open_main_window(state.clone(), cx).unwrap();
                let workspace = handle
                    .read(cx)
                    .unwrap()
                    .view()
                    .clone()
                    .downcast::<Workspace>()
                    .unwrap();
                let window = AnyWindowHandle::from(handle);
                draw(window, cx);
                check(
                    "the sidebar starts open",
                    workspace.read(cx).is_sidebar_open(),
                );

                // ctrl-b and the footer button dispatch ToggleSidebar.
                let toggle = |cx: &mut App| {
                    in_window(window, cx, |window, cx| {
                        window.dispatch_action(Box::new(ToggleSidebar), cx);
                    });
                };
                toggle(cx);
                draw(window, cx);
                check(
                    "the action hides the sidebar",
                    !workspace.read(cx).is_sidebar_open(),
                );
                toggle(cx);
                draw(window, cx);
                check(
                    "the action shows it again",
                    workspace.read(cx).is_sidebar_open(),
                );

                // Selecting a dashboard re-renders the header and summary.
                let network = DashboardRef {
                    group_id: "demo-platform".to_owned(),
                    dashboard_id: "demo-platform-network".to_owned(),
                };
                let selected = state.update(cx, |state, cx| {
                    cx.notify();
                    state.select(network)
                });
                draw(window, cx);
                check("a dashboard can be selected", selected);

                // Typing in the search field filters the sidebar.
                let sidebar = workspace.read(cx).sidebar().clone();
                let search = sidebar.read(cx).search_input().clone();
                in_window(window, cx, |window, cx| {
                    search.update(cx, |input, cx| input.replace_all("netw", window, cx));
                });
                draw(window, cx);
                check(
                    "the search query follows the field",
                    sidebar.read(cx).query() == "netw",
                );

                // Collapsing a group, then clearing the search.
                state.update(cx, |state, cx| {
                    state.toggle_group("demo-lab");
                    cx.notify();
                });
                in_window(window, cx, |window, cx| {
                    search.update(cx, |input, cx| input.replace_all("", window, cx));
                });
                draw(window, cx);
                check(
                    "clearing the search clears the query",
                    sidebar.read(cx).query().is_empty(),
                );
                // Quitting takes effect once the event loop runs.
                cx.spawn(async |cx| cx.update(|cx| cx.quit())).detach();
            });
        finished.store(true, Ordering::SeqCst);
        let checks = checks.borrow();
        assert_eq!(checks.len(), 6, "the app didn't run to the end: {checks:?}");
        for (name, ok) in checks.iter() {
            assert!(ok, "{name}");
        }
    }
}
