//! The main window's life (BG-01, BG-04): it can close while the app keeps
//! running in the tray, and comes back (where it was, with the same
//! dashboards and tabs: the state outlives it) from the tray, a
//! notification, a second launch, the dock or the app menu.
//!
//! The app runs with `QuitMode::Explicit`, so closing the last window
//! never quits by itself: [`closed`] decides. It keeps running only while
//! the tray icon is there and a tray host shows it (asked off the UI
//! thread), so the app never runs on unseen with no way back; otherwise
//! it quits.

use gpui::{App, Context, Entity, Global, Window, WindowHandle};
use ic_ui_kit::Root;

use crate::app_state::AppState;
use crate::window_state::{self, InitialBounds};
use crate::workspace::Workspace;

/// The state the main window shows, for reopening it.
#[derive(Clone)]
struct MainState(Entity<AppState>);

impl Global for MainState {}

/// Remembers the state the main window shows.
pub(crate) fn install(state: Entity<AppState>, cx: &mut App) {
    cx.set_global(MainState(state));
}

/// The main window, if open.
pub(crate) fn main_window(cx: &App) -> Option<WindowHandle<Root>> {
    cx.windows()
        .into_iter()
        .find_map(|window| window.downcast::<Root>())
}

/// Shows the main window: brings it forward, or opens it again where it
/// was (the saved bounds). Returns whether a window is shown.
pub(crate) fn show(cx: &mut App) -> bool {
    if let Some(window) = main_window(cx) {
        let shown = window
            .update(cx, |_, window, _| window.activate_window())
            .is_ok();
        cx.activate(true);
        return shown;
    }
    let Some(state) = cx.try_global::<MainState>().map(|main| main.0.clone()) else {
        return false;
    };
    let bounds = window_state::initial_bounds(
        state.read(cx).window_state(),
        &cx.displays()
            .iter()
            .map(|display| display.bounds())
            .collect::<Vec<_>>(),
        crate::WINDOW_SIZE,
    );
    match crate::open_main_window(state, bounds, cx) {
        Ok(_) => {
            tracing::info!("main window opened");
            true
        }
        Err(error) => {
            tracing::error!(error = %format!("{error:#}"), "the main window couldn't open");
            false
        }
    }
}

/// Opens the main window at start unless starting in the background.
pub(crate) fn open_at_start(state: Entity<AppState>, bounds: InitialBounds, cx: &mut App) -> bool {
    match crate::open_main_window(state, bounds, cx) {
        Ok(_) => true,
        Err(error) => {
            tracing::error!(error = %format!("{error:#}"), "failed to open the main window");
            false
        }
    }
}

/// Runs `f` on the main window's workspace, showing the window first
/// (opening it if it was closed). Returns whether it ran.
pub(crate) fn with_workspace(
    cx: &mut App,
    f: impl FnOnce(&mut Workspace, &mut Window, &mut Context<Workspace>),
) -> bool {
    if !show(cx) {
        return false;
    }
    let Some(window) = main_window(cx) else {
        return false;
    };
    window
        .update(cx, |root, window, cx| {
            let Ok(workspace) = root.view().clone().downcast::<Workspace>() else {
                return false;
            };
            workspace.update(cx, |workspace, cx| f(workspace, window, cx));
            true
        })
        .unwrap_or(false)
}

/// What to do once the last window closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AfterClose {
    /// Keep running in the tray (and keep notifying).
    KeepRunning,
    /// Quit: nobody could bring the window back.
    Quit,
}

/// What to do once the last window closed: keep running only with a tray
/// icon (the settings want one and it was created) that a tray host
/// shows.
pub(crate) fn after_close(tray_shown: bool, host_available: bool) -> AfterClose {
    if tray_shown && host_available {
        AfterClose::KeepRunning
    } else {
        AfterClose::Quit
    }
}

/// The main window closed. With no window left, the app keeps running in
/// the tray if there is one a tray host shows; otherwise it quits.
pub(crate) fn closed(cx: &mut App) {
    if main_window(cx).is_some() {
        return;
    }
    if after_close(super::tray::is_shown(cx), true) == AfterClose::Quit {
        tracing::info!("the window closed and there is no tray: quitting");
        cx.quit();
        return;
    }
    let host = cx
        .background_executor()
        .spawn(async { ic_platform::tray::host_available() });
    cx.spawn(async move |cx| {
        let available = host.await;
        cx.update(|cx| {
            if main_window(cx).is_some() {
                return;
            }
            match after_close(super::tray::is_shown(cx), available) {
                AfterClose::KeepRunning => {
                    tracing::info!("the window closed: running in the tray");
                }
                AfterClose::Quit => {
                    tracing::info!("the window closed and no tray host shows the icon: quitting");
                    cx.quit();
                }
            }
        });
    })
    .detach();
}

/// Whether to start without a window (`--background`): only with a tray
/// icon a tray host shows (asks the session bus, briefly).
pub(crate) fn start_hidden(background: bool, cx: &App) -> bool {
    if !background {
        return false;
    }
    if !super::tray::is_shown(cx) {
        tracing::info!(
            "--background without the tray (switched off in the settings): opening the window"
        );
        return false;
    }
    if !ic_platform::tray::host_available() {
        tracing::info!("--background, but no tray host shows the icon: opening the window");
        return false;
    }
    tracing::info!("starting in the background, in the tray");
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_app_keeps_running_only_where_the_tray_shows_it() {
        assert_eq!(after_close(true, true), AfterClose::KeepRunning);
        assert_eq!(after_close(true, false), AfterClose::Quit, "no tray host");
        assert_eq!(after_close(false, true), AfterClose::Quit, "switched off");
        assert_eq!(after_close(false, false), AfterClose::Quit);
    }
}
