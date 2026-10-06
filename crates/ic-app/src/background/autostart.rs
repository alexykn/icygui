//! Launch at login (BG-03): the settings' switch writes or removes the
//! login entry (`ic_platform::autostart`: a launch agent on macOS, an XDG
//! autostart entry on Linux) that starts `icygui --background`, so the app
//! comes up in the tray without its window. At every start with the switch
//! on, the entry is written again, so it follows the app when it moved
//! (and comes back if something removed it).
//!
//! The work happens off the UI thread (macOS asks `launchctl`). The demo
//! never touches it; tests record instead.

use std::path::PathBuf;

use gpui::{App, Entity};

use crate::app_state::AppState;

/// The program the login entry starts: the `AppImage` when running from
/// one (its mount point changes every run), else this executable.
///
/// # Errors
///
/// The executable's path is unknown.
pub(crate) fn login_executable() -> Result<PathBuf, String> {
    if let Some(appimage) = std::env::var_os("APPIMAGE").map(PathBuf::from)
        && appimage.is_absolute()
    {
        return Ok(appimage);
    }
    std::env::current_exe()
        .map(|exe| exe.canonicalize().unwrap_or(exe))
        .map_err(|error| format!("the program's path is unknown: {error}"))
}

/// Turns the login entry on or off.
///
/// # Errors
///
/// What went wrong, for the user.
#[cfg(not(test))]
fn set(enabled: bool) -> Result<(), String> {
    let exe = login_executable()?;
    ic_platform::autostart::set_enabled(enabled, crate::APP_ID, crate::APP_NAME, &exe)
        .map_err(|error| error.to_string())
}

/// Tests never write the user's login entries: they record the request.
#[cfg(test)]
#[expect(
    clippy::unnecessary_wraps,
    reason = "the signature of the real one, which can fail"
)]
fn set(enabled: bool) -> Result<(), String> {
    tests::REQUESTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(enabled);
    Ok(())
}

/// The settings switched launch at login: writes or removes the entry off
/// the UI thread, and says so if that fails.
pub(crate) fn change(enabled: bool, state: &Entity<AppState>, cx: &mut App) {
    if state.read(cx).is_demo() && !cfg!(test) {
        return;
    }
    let state = state.downgrade();
    let work = cx.background_executor().spawn(async move { set(enabled) });
    cx.spawn(async move |cx| {
        let outcome = work.await;
        match outcome {
            Ok(()) => tracing::info!(enabled, "launch at login changed"),
            Err(error) => {
                tracing::warn!(%error, enabled, "launch at login couldn't be changed");
                let _ = state.update(cx, |state, cx| {
                    state.inform(
                        if enabled {
                            "icygui couldn't set itself to start at login"
                        } else {
                            "icygui couldn't stop starting at login"
                        },
                        Some(error),
                    );
                    cx.notify();
                });
            }
        }
    })
    .detach();
}

/// At start: with launch at login on, writes the entry again (the app may
/// have moved). Failures are only logged; the settings show the switch.
pub(crate) fn refresh_at_start(enabled: bool, demo: bool, cx: &mut App) {
    if !enabled || demo {
        return;
    }
    cx.background_executor()
        .spawn(async move {
            if let Err(error) = set(true) {
                tracing::warn!(%error, "the login entry couldn't be refreshed");
            }
        })
        .detach();
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Mutex;

    /// What the tests asked for.
    pub(crate) static REQUESTS: Mutex<Vec<bool>> = Mutex::new(Vec::new());

    #[test]
    fn the_entry_starts_this_program_or_its_appimage() {
        let exe = super::login_executable().unwrap();
        assert!(exe.is_absolute(), "{}", exe.display());
    }
}
