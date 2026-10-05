//! Icinga 2 desktop client.

mod workspace;

use gpui::{
    App, AppContext as _, Bounds, Size, TitlebarOptions, WindowBounds, WindowOptions, point, px,
    size,
};
use gpui_platform::application;
use tracing_subscriber::EnvFilter;

use crate::workspace::Workspace;

/// Reverse-DNS application id (Wayland `app_id`, notification identity).
/// Placeholder until the project has a domain to publish under.
const APP_ID: &str = "local.icinga-client";
const APP_NAME: &str = "Icinga Client";

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    application().run(|cx: &mut App| {
        cx.set_app_identity(APP_ID, APP_NAME);
        if let Err(error) = ic_ui_kit::init(cx) {
            tracing::error!(%error, "cannot start without the bundled fonts");
            cx.quit();
            return;
        }
        if let Err(error) = open_main_window(cx) {
            tracing::error!(error = %format!("{error:#}"), "failed to open the main window");
            cx.quit();
        }
    });
}

fn open_main_window(cx: &mut App) -> anyhow::Result<()> {
    let bounds = Bounds::centered(None, size(px(1440.), px(900.)), cx);
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(Size {
                width: px(900.),
                height: px(560.),
            }),
            titlebar: Some(TitlebarOptions {
                title: Some(APP_NAME.into()),
                // macOS draws its traffic lights inside our sidebar header.
                appears_transparent: true,
                traffic_light_position: Some(point(px(12.), px(13.))),
            }),
            app_id: Some(APP_ID.to_owned()),
            ..WindowOptions::default()
        },
        |_window, cx| cx.new(|_| Workspace),
    )?;
    cx.activate(true);
    Ok(())
}
