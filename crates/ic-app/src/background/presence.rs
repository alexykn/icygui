//! Whether anybody can see the main window, for quiet mode (PERF-09).
//!
//! The window counts as *hidden* once it has been out of sight for
//! [`HIDDEN_GRACE`]: closed to the tray, or reported by the system as not
//! shown at all (`gpui::WindowVisibility::Hidden`: minimised, on another
//! virtual desktop or Space, on a display that sleeps; macOS also reports
//! a window that other windows cover completely; X11 under a compositing
//! window manager and Wayland compositors without the `suspended` state
//! report only what they can). A window that is merely unfocused or partly
//! covered stays shown: a dashboard on a second screen keeps its
//! environment fully live. The grace keeps a quick look elsewhere (Alt-Tab
//! over a full-screen window, a Space switched through) from switching
//! the event stream back and forth.
//!
//! Shown again (opened, brought forward by the tray, a notification or a
//! second launch, or reported visible), the window counts as shown at
//! once, so the environment on screen wakes up before anything opens in
//! it.

use std::time::Duration;

use gpui::{App, Entity, Global, Task, WindowVisibility};

use crate::app_state::AppState;

/// How long the window must be out of sight before the environment on
/// screen turns quiet.
pub(crate) const HIDDEN_GRACE: Duration = Duration::from_secs(30);

/// The window going out of sight: its quiet mode waits for the grace.
#[derive(Default)]
struct Presence {
    /// Turns the window hidden once the grace is over (dropped: cancelled).
    hiding: Option<Task<()>>,
    /// The grace (tests shorten it).
    grace: Option<Duration>,
}

impl Global for Presence {}

/// The window is shown: the environment on screen wakes up now.
pub(crate) fn shown(state: &Entity<AppState>, cx: &mut App) {
    cx.default_global::<Presence>().hiding = None;
    state.update(cx, |state, _| {
        state.set_window_hidden(false);
    });
}

/// The window went out of sight (closed to the tray, or hidden as the
/// system reports it): hidden once that lasted [`HIDDEN_GRACE`], unless it
/// is shown meanwhile.
pub(crate) fn hidden(state: &Entity<AppState>, cx: &mut App) {
    let (pending, grace) = {
        let presence = cx.default_global::<Presence>();
        (
            presence.hiding.is_some(),
            presence.grace.unwrap_or(HIDDEN_GRACE),
        )
    };
    if pending || state.read(cx).window_hidden() {
        return;
    }
    let state = state.downgrade();
    let task = cx.spawn(async move |cx| {
        cx.background_executor().timer(grace).await;
        let _ = state.update(cx, |state, _| {
            if state.set_window_hidden(true) {
                tracing::info!("the window is hidden: quiet mode for the environment on screen");
            }
        });
    });
    cx.default_global::<Presence>().hiding = Some(task);
}

/// The system reports the window's `visibility` (`Window::observe_window_visibility`).
pub(crate) fn visibility_changed(
    state: &Entity<AppState>,
    visibility: WindowVisibility,
    cx: &mut App,
) {
    tracing::debug!(?visibility, "window visibility");
    match visibility {
        WindowVisibility::Visible => shown(state, cx),
        WindowVisibility::Hidden => hidden(state, cx),
    }
}

/// Shortens the grace (tests).
#[cfg(all(test, target_os = "linux"))]
pub(crate) fn set_grace(grace: Duration, cx: &mut App) {
    cx.default_global::<Presence>().grace = Some(grace);
}
