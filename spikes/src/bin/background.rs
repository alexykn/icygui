//! Spike: background mode (results in `docs/spikes.md`).
//!
//! Checks, on the platform it runs on:
//! 1. with `QuitMode::Explicit` the app keeps running after its last window
//!    closes (with `--quit-mode default` it should quit on Linux),
//! 2. a window can be opened again afterwards, which is what the tray's
//!    "Open" does,
//! 3. a tray / menu-bar icon with a menu runs next to GPUI's event loop and
//!    its clicks reach the app,
//! 4. a system notification with action buttons can be posted while no
//!    window is open, and its response reaches the app.
//!
//! Interactive: `cargo run -p spikes --bin background`, then close the
//! window and use the tray menu (Open / Notify / Quit).
//!
//! Scripted: `cargo run -p spikes --bin background -- --auto 20` opens a
//! window, closes it after 1s, posts a notification after 2s and quits after
//! 20s. Everything that happens is logged, so an outside harness can click
//! the tray menu or the notification in between and check the log.

use std::time::Duration;

use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui::{
    App, AppContext as _, Bounds, Context, Global, IntoElement, ParentElement as _, QuitMode,
    Render, Styled as _, SystemNotification, SystemNotificationAction, Window, WindowBounds,
    WindowOptions, div, px, rgb, size,
};
use gpui_platform::application;
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

const NOTIFICATION_TAG: &str = "spike-background";

#[derive(Clone, Copy, Debug)]
struct Args {
    /// Run the scripted timeline and quit after this many seconds.
    auto_seconds: Option<u64>,
    quit_mode: QuitMode,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        auto_seconds: None,
        quit_mode: QuitMode::Explicit,
    };
    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--auto" => {
                let seconds = iter.next().ok_or("--auto needs a number of seconds")?;
                args.auto_seconds = Some(
                    seconds
                        .parse()
                        .map_err(|_| format!("bad seconds: {seconds}"))?,
                );
            }
            "--quit-mode" => {
                args.quit_mode = match iter.next().as_deref() {
                    Some("explicit") => QuitMode::Explicit,
                    Some("default") => QuitMode::Default,
                    Some("last-window-closed") => QuitMode::LastWindowClosed,
                    other => return Err(format!("unknown quit mode: {other:?}")),
                };
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(args)
}

/// Keeps the tray icon alive for the lifetime of the app.
struct Tray(#[expect(dead_code, reason = "held only to keep the icon alive")] TrayIcon);

impl Global for Tray {}

struct SpikeWindow;

impl Render for SpikeWindow {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_2()
            .size_full()
            .p_6()
            .bg(rgb(0x1d_2125))
            .text_color(rgb(0xd6_d8da))
            .child("Background-mode spike")
            .child("Close this window. The app should keep running; use the tray menu.")
    }
}

fn main() {
    tracing_subscriber::fmt().with_target(false).init();
    let args = match parse_args() {
        Ok(args) => args,
        Err(error) => {
            tracing::error!("{error}");
            return;
        }
    };
    tracing::info!(?args, "starting");

    application()
        .with_quit_mode(args.quit_mode)
        .run(move |cx: &mut App| {
            cx.set_app_identity("local.icinga-client.spike", "Icinga Client spike");
            start_tray(cx);
            cx.on_system_notification_response(|response, cx| {
                tracing::info!(
                    tag = %response.tag,
                    action = ?response.action_id,
                    "notification response received"
                );
                if response.action_id.as_deref() == Some("open") || response.action_id.is_none() {
                    open_window(cx, "notification");
                }
            });
            cx.on_window_closed(|cx, _| {
                tracing::info!(open_windows = cx.windows().len(), "window closed");
            })
            .detach();
            open_window(cx, "startup");
            if let Some(seconds) = args.auto_seconds {
                run_timeline(cx, seconds);
            }
        });

    // Only reached on Linux; macOS terminates the process from `quit`.
    tracing::info!("event loop ended");
}

#[expect(
    clippy::disallowed_methods,
    reason = "the spike has no interface size: real pixels"
)]
fn open_window(cx: &mut App, reason: &str) {
    if !cx.windows().is_empty() {
        tracing::info!(reason, "window already open");
        return;
    }
    let bounds = Bounds::centered(None, size(px(560.), px(240.)), cx);
    let result = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..WindowOptions::default()
        },
        |_, cx| cx.new(|_| SpikeWindow),
    );
    match result {
        Ok(_) => tracing::info!(reason, "window opened"),
        Err(error) => tracing::error!(reason, error = %format!("{error:#}"), "window failed"),
    }
}

fn post_notification(cx: &App, reason: &str) {
    cx.show_system_notification(SystemNotification {
        tag: NOTIFICATION_TAG.into(),
        title: "CRITICAL · postgres-replication on db-prod-03".into(),
        body: "CRITICAL - standby lag 412s (> 300s)".into(),
        actions: vec![
            SystemNotificationAction {
                id: "ack".into(),
                label: "Acknowledge".into(),
            },
            SystemNotificationAction {
                id: "open".into(),
                label: "Open".into(),
            },
        ],
    });
    tracing::info!(reason, "notification posted");
}

fn start_tray(cx: &mut App) {
    let menu = Menu::new();
    let items = [
        MenuItem::with_id("open", "Open", true, None),
        MenuItem::with_id("notify", "Post test notification", true, None),
        MenuItem::with_id("quit", "Quit", true, None),
    ];
    for item in &items {
        if let Err(error) = menu.append(item) {
            tracing::error!(%error, "tray menu item failed");
        }
    }

    // Menu clicks arrive on a platform thread; hand them to the main thread.
    let (sender, mut receiver) = mpsc::unbounded::<MenuEvent>();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        sender.unbounded_send(event).ok();
    }));
    cx.spawn(async move |cx| {
        while let Some(event) = receiver.next().await {
            let id = event.id().0.clone();
            tracing::info!(%id, "tray menu clicked");
            cx.update(|cx| match id.as_str() {
                "open" => open_window(cx, "tray"),
                "notify" => post_notification(cx, "tray"),
                "quit" => cx.quit(),
                _ => {}
            });
        }
    })
    .detach();

    let tray = state_icon([0xe0, 0x6c, 0x6c]).and_then(|icon| {
        TrayIconBuilder::new()
            .with_tooltip("Icinga Client spike")
            .with_title("Icinga Client spike")
            .with_icon(icon)
            .with_menu(Box::new(menu))
            .build()
            .map_err(|error| error.to_string())
    });
    match tray {
        Ok(tray) => {
            tracing::info!("tray icon created");
            cx.set_global(Tray(tray));
        }
        Err(error) => tracing::error!(%error, "tray icon failed; continuing without it"),
    }
}

/// A filled circle in the given colour, the shape of the design's state dots.
fn state_icon(color: [u8; 3]) -> Result<Icon, String> {
    const SIZE: u32 = 32;
    let center = f64::from(SIZE) / 2.;
    let radius = center - 2.;
    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let dx = f64::from(x) + 0.5 - center;
            let dy = f64::from(y) + 0.5 - center;
            let inside = dx.hypot(dy) <= radius;
            rgba.extend_from_slice(&color);
            rgba.push(if inside { 0xff } else { 0x00 });
        }
    }
    Icon::from_rgba(rgba, SIZE, SIZE).map_err(|error| error.to_string())
}

fn run_timeline(cx: &App, seconds: u64) {
    cx.spawn(async move |cx| {
        let timer = |duration| cx.background_executor().timer(duration);

        timer(Duration::from_secs(1)).await;
        cx.update(|cx| {
            for window in cx.windows() {
                window
                    .update(cx, |_, window, _| window.remove_window())
                    .ok();
            }
        });

        timer(Duration::from_secs(1)).await;
        cx.update(|cx| {
            tracing::info!(
                open_windows = cx.windows().len(),
                "alive after last window closed"
            );
            post_notification(cx, "timeline");
        });

        timer(Duration::from_secs(seconds.saturating_sub(2))).await;
        cx.update(|cx| {
            tracing::info!(open_windows = cx.windows().len(), "timeline done, quitting");
            cx.quit();
        });
    })
    .detach();
}
