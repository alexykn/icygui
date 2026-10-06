//! The tray / menu-bar icon (BG-01, BG-02, REL-07) through
//! `ic_platform::tray`: the logo's mark tinted with the active
//! environment's worst unhandled state (grey while not connected), a
//! tooltip with the environment, its connection and its counts, and the
//! menu: open, pause notifications (30 minutes, an hour, until 08:00) or
//! resume, switch environment, quit.
//!
//! It exists while the settings say to keep running in the tray
//! (`General::close_to_tray`, on by default) and follows the state: every
//! change of the state updates it (the platform tray sends only what
//! changed). [`TrayView`] is what it shows, computed purely from the
//! state.

use std::time::{Duration, Instant};

use futures::StreamExt as _;
use gpui::{App, Entity, Global, Subscription, Task};
use ic_model::Timestamp;
use ic_platform::tray::{Tray, TrayCommand, TrayTone};

use crate::app_state::AppState;
use crate::notifications::when;

/// What the tray shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TrayView {
    /// The icon's tint: the worst unhandled state while connected, else
    /// grey (`None`).
    pub(crate) tone: Option<TrayTone>,
    /// `prod-cluster · connected`, the counts, and a pause.
    pub(crate) tooltip: String,
    /// The environments to switch to, `(id, name)`.
    pub(crate) environments: Vec<(String, String)>,
    /// The active one.
    pub(crate) active: Option<String>,
    /// Until when notifications are paused, as the menu says it.
    pub(crate) paused: Option<String>,
}

/// What the tray shows for `state` at `now`.
pub(crate) fn tray_view(state: &AppState, now: Timestamp) -> TrayView {
    let connection = state.connection();
    let connected = connection.is_connected();
    let overall = &state.snapshot().overall;
    let tone = if connected {
        TrayTone::for_worst_unhandled(overall.worst_unhandled)
    } else {
        None
    };
    let mut lines = Vec::new();
    match state.environment() {
        Some(environment) => {
            let demo = if state.is_demo_environment() {
                " (demo)"
            } else {
                ""
            };
            lines.push(format!(
                "{}{demo} · {}",
                environment.name,
                connection.short_state()
            ));
            if connected {
                let counts: Vec<String> = [
                    (overall.down, "down"),
                    (overall.unreachable, "unreachable"),
                    (overall.critical, "critical"),
                    (overall.warning, "warning"),
                    (overall.unknown, "unknown"),
                ]
                .into_iter()
                .filter(|(count, _)| *count > 0)
                .map(|(count, word)| format!("{count} {word}"))
                .collect();
                if !counts.is_empty() {
                    lines.push(counts.join(" · "));
                }
                lines.push(match overall.unhandled {
                    0 => "nothing unhandled".to_owned(),
                    1 => "1 unhandled problem".to_owned(),
                    count => format!("{count} unhandled problems"),
                });
            }
        }
        None => lines.push("no environment".to_owned()),
    }
    let paused = state
        .paused_until()
        .filter(|until| *until > now)
        .map(|until| when(until, now));
    if let Some(until) = &paused {
        lines.push(format!("notifications paused until {until}"));
    }
    TrayView {
        tone,
        tooltip: lines.join("\n"),
        environments: state
            .environments()
            .iter()
            .map(|environment| (environment.id.clone(), environment.name.clone()))
            .collect(),
        active: state.active_environment_id().map(str::to_owned),
        paused,
    }
}

/// The tray while it exists, and what it shows.
struct Shown {
    tray: Tray,
    view: Option<TrayView>,
    _commands: Task<()>,
}

/// The tray's bridge to the app: a global.
struct TrayBridge {
    shown: Option<Shown>,
    /// The tray couldn't be created (no session bus yet, at login): when
    /// to try again, and whether that was said already.
    retry_at: Option<Instant>,
    failures: u32,
    _observe: Subscription,
}

/// How long to wait before trying to create the tray again.
const RETRY_AFTER: Duration = Duration::from_mins(1);

impl Global for TrayBridge {}

/// Starts following `state`: creates the tray when the settings want one,
/// and keeps it current.
pub(crate) fn install(state: &Entity<AppState>, cx: &mut App) {
    let observe = cx.observe(state, |state, cx| sync(&state, cx));
    cx.set_global(TrayBridge {
        shown: None,
        retry_at: None,
        failures: 0,
        _observe: observe,
    });
    sync(state, cx);
    // Pauses end by themselves: the menu and tooltip follow every minute.
    let weak = state.downgrade();
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor()
                .timer(Duration::from_secs(30))
                .await;
            let Some(state) = weak.upgrade() else {
                break;
            };
            cx.update(|cx| sync(&state, cx));
        }
    })
    .detach();
}

/// Whether the tray icon is there.
pub(crate) fn is_shown(cx: &App) -> bool {
    cx.try_global::<TrayBridge>()
        .is_some_and(|bridge| bridge.shown.is_some())
}

/// Creates or drops the tray as the settings say, and updates it.
fn sync(state: &Entity<AppState>, cx: &mut App) {
    if !cx.has_global::<TrayBridge>() {
        return;
    }
    let wanted = state.read(cx).config().general.close_to_tray;
    let present = is_shown(cx);
    if wanted && !present {
        let waiting = cx
            .global::<TrayBridge>()
            .retry_at
            .is_some_and(|at| Instant::now() < at);
        if waiting {
            return;
        }
        match Tray::new(crate::APP_NAME) {
            Ok(tray) => {
                tracing::info!("tray icon shown");
                let commands = spawn_commands(&tray, state, cx);
                let bridge = cx.global_mut::<TrayBridge>();
                bridge.retry_at = None;
                bridge.failures = 0;
                bridge.shown = Some(Shown {
                    tray,
                    view: None,
                    _commands: commands,
                });
            }
            Err(error) => {
                let bridge = cx.global_mut::<TrayBridge>();
                if bridge.failures == 0 {
                    tracing::warn!(%error, "no tray icon (trying again every minute); closing the window will quit");
                } else {
                    tracing::debug!(%error, "still no tray icon");
                }
                bridge.failures = bridge.failures.saturating_add(1);
                bridge.retry_at = Some(Instant::now() + RETRY_AFTER);
                return;
            }
        }
    } else if !wanted {
        let bridge = cx.global_mut::<TrayBridge>();
        bridge.retry_at = None;
        if present {
            tracing::info!("the tray icon goes (switched off in the settings)");
            bridge.shown = None;
        }
        return;
    }
    let view = tray_view(state.read(cx), Timestamp::now());
    let bridge = cx.global_mut::<TrayBridge>();
    let Some(shown) = bridge.shown.as_mut() else {
        return;
    };
    if shown.view.as_ref() == Some(&view) {
        return;
    }
    shown.tray.set_state(view.tone, &view.tooltip);
    shown
        .tray
        .set_environments(&view.environments, view.active.as_deref());
    shown.tray.set_paused(view.paused.clone());
    shown.view = Some(view);
}

/// Carries out what is chosen in the tray, on the UI thread.
fn spawn_commands(tray: &Tray, state: &Entity<AppState>, cx: &mut App) -> Task<()> {
    let mut commands = tray.commands();
    let state = state.downgrade();
    cx.spawn(async move |cx| {
        while let Some(command) = commands.next().await {
            let Some(state) = state.upgrade() else {
                break;
            };
            cx.update(|cx| run(command, &state, cx));
        }
    })
}

/// One tray command.
fn run(command: TrayCommand, state: &Entity<AppState>, cx: &mut App) {
    tracing::info!(?command, "tray");
    match command {
        TrayCommand::Open => {
            super::window::show(cx);
        }
        TrayCommand::PauseFor(duration) => state.update(cx, |state, cx| {
            let until = Timestamp::from_unix_seconds(
                Timestamp::now().as_unix_seconds() + duration.as_secs_f64(),
            );
            state.pause_notifications(Some(until));
            cx.notify();
        }),
        TrayCommand::Resume => state.update(cx, |state, cx| {
            state.pause_notifications(None);
            cx.notify();
        }),
        TrayCommand::SwitchEnvironment(id) => {
            if let Some(session) = crate::live::session(cx) {
                session.update(cx, |session, cx| {
                    session.switch_environment(&id, cx);
                });
            }
        }
        TrayCommand::Quit => cx.quit(),
    }
}

#[cfg(test)]
mod tests {
    use ic_core::ConnectionState;

    use super::*;

    fn now() -> Timestamp {
        Timestamp::from_unix_seconds(1_790_000_000.)
    }

    #[test]
    fn the_tray_shows_the_worst_unhandled_state_and_the_counts() {
        let mut state = AppState::fixture(now());
        let view = tray_view(&state, now());
        // The design's sample data: unhandled criticals.
        assert_eq!(view.tone, Some(TrayTone::Critical));
        let lines: Vec<&str> = view.tooltip.lines().collect();
        assert_eq!(lines[0], "prod-cluster (demo) · connected");
        assert!(lines[1].contains("critical"), "{lines:?}");
        assert!(lines[2].ends_with("unhandled problems"), "{lines:?}");
        assert_eq!(view.active.as_deref(), state.active_environment_id());
        assert_eq!(view.environments.len(), state.environments().len());
        assert_eq!(view.paused, None);

        // Paused: the menu and the tooltip say until when.
        let until = Timestamp::from_unix_seconds(now().as_unix_seconds() + 1800.);
        state.apply(ic_core::CoreEvent::NotificationsPaused(Some(until)));
        let view = tray_view(&state, now());
        assert!(view.paused.is_some());
        assert!(view.tooltip.ends_with(&format!(
            "notifications paused until {}",
            view.paused.clone().unwrap()
        )));

        // Not connected: grey, and why.
        state.set_connection_lost();
        let view = tray_view(&state, now());
        assert_eq!(view.tone, None);
        assert_eq!(
            view.tooltip.lines().next(),
            Some("prod-cluster (demo) · reconnecting")
        );
        state.apply(ic_core::CoreEvent::Connection(
            ConnectionState::AuthFailed {
                message: "401".to_owned(),
            },
        ));
        assert!(tray_view(&state, now()).tooltip.contains("login refused"));
    }

    #[test]
    fn without_an_environment_the_tray_is_grey() {
        let view = tray_view(&AppState::empty(), now());
        assert_eq!(view.tone, None);
        assert_eq!(view.tooltip, "no environment");
        assert!(view.environments.is_empty());
    }
}
