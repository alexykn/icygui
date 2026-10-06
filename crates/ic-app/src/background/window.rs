//! The main window's life (BG-01, BG-04): it can close while the app keeps
//! running in the tray, and comes back (where it was, with the same
//! dashboards and tabs: the state outlives it) from the tray, a
//! notification, a second launch, the dock or the app menu.
//!
//! The app runs with `QuitMode::Explicit`, so closing the last window
//! never quits by itself: [`closed`] decides. It keeps running only while
//! the tray icon is there and a tray host shows it (asked off the UI
//! thread), so the app never runs on unseen with no way back; otherwise
//! it quits. Running on in the tray, the window counts as hidden for
//! quiet mode ([`super::presence`]). `--background` (launch at login) starts without a window and
//! waits a while for a tray host, which often starts after the app at
//! login; with none by then, the window opens.

use std::time::{Duration, Instant};

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
    let state = cx.try_global::<MainState>().map(|main| main.0.clone());
    // The environment on screen wakes up before anything opens in it (the
    // system reports the window visible only later).
    if let Some(state) = &state {
        super::presence::shown(state, cx);
    }
    if let Some(window) = main_window(cx) {
        let shown = window
            .update(cx, |_, window, _| window.activate_window())
            .is_ok();
        cx.activate(true);
        return shown;
    }
    let Some(state) = state else {
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
                    if let Some(state) = cx.try_global::<MainState>().map(|main| main.0.clone()) {
                        super::presence::hidden(&state, cx);
                    }
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

/// How long `--background` waits for a tray host before it opens the
/// window: at login, panels and their tray hosts often start after the
/// app.
const HOST_GRACE: Duration = Duration::from_secs(20);
/// How often it asks meanwhile.
const HOST_RETRY: Duration = Duration::from_secs(1);

/// Whether to start without a window (`--background`): only with the tray
/// icon (the settings want one and it was created). Whether a tray host
/// shows it is asked off the UI thread afterwards ([`await_tray_host`]).
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
    true
}

/// What `--background` does after asking for a tray host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HostWait {
    /// A host shows the icon: stay in the tray.
    Found,
    /// None yet: ask again shortly.
    Retry,
    /// None within the grace period: open the window.
    GiveUp,
}

/// What to do when a tray host is (`available`) or isn't there after
/// waiting `waited` for one.
pub(crate) fn host_wait(available: bool, waited: Duration) -> HostWait {
    if available {
        HostWait::Found
    } else if waited < HOST_GRACE {
        HostWait::Retry
    } else {
        HostWait::GiveUp
    }
}

/// Started without a window: asks (off the UI thread) for a tray host
/// that shows the icon, again every second for up to 20 s; with none by
/// then the window opens (at `bounds`), so the app never runs unseen.
pub(crate) fn await_tray_host(state: Entity<AppState>, bounds: InitialBounds, cx: &mut App) {
    let executor = cx.background_executor().clone();
    cx.spawn(async move |cx| {
        let started = Instant::now();
        loop {
            let available = executor
                .spawn(async { ic_platform::tray::host_available() })
                .await;
            match host_wait(available, started.elapsed()) {
                HostWait::Found => {
                    tracing::info!("starting in the background, in the tray");
                    return;
                }
                HostWait::Retry => executor.timer(HOST_RETRY).await,
                HostWait::GiveUp => break,
            }
        }
        cx.update(|cx| {
            // Shown meanwhile (the tray, a notification, a second launch).
            if main_window(cx).is_some() {
                return;
            }
            tracing::info!("--background, but no tray host shows the icon: opening the window");
            if !open_at_start(state, bounds, cx) {
                cx.quit();
            }
        });
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_starts_wait_a_while_for_a_tray_host() {
        assert_eq!(host_wait(true, Duration::ZERO), HostWait::Found);
        assert_eq!(host_wait(false, Duration::ZERO), HostWait::Retry);
        assert_eq!(
            host_wait(false, Duration::from_secs(19)),
            HostWait::Retry,
            "the panel may still be starting"
        );
        assert_eq!(host_wait(true, Duration::from_secs(19)), HostWait::Found);
        assert_eq!(
            host_wait(false, HOST_GRACE),
            HostWait::GiveUp,
            "the window opens"
        );
    }

    #[test]
    fn the_app_keeps_running_only_where_the_tray_shows_it() {
        assert_eq!(after_close(true, true), AfterClose::KeepRunning);
        assert_eq!(after_close(true, false), AfterClose::Quit, "no tray host");
        assert_eq!(after_close(false, true), AfterClose::Quit, "switched off");
        assert_eq!(after_close(false, false), AfterClose::Quit);
    }
}
