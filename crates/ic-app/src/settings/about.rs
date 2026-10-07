//! The about dialog (the macOS app menu's *About icygui*, the palette, the
//! settings): the logo, the version, what icygui is, and where it keeps
//! its files.

use std::sync::Arc;

use gpui::{
    AnyElement, App, Image, ImageFormat, IntoElement, ParentElement as _, Styled as _, div, img, px,
};
use ic_ui_kit::{ActiveTheme as _, DialogBody, KvTable};

/// The app icon, 128 px.
const ICON: &[u8] = include_bytes!("../../../../assets/icons/icygui-128.png");

/// What the about dialog shows about where things are.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct AboutFacts {
    /// The settings file, unless in the demo.
    pub(crate) settings: Option<String>,
    /// The log directory.
    pub(crate) logs: Option<String>,
}

/// The dialog's content; `close` is its button.
pub(crate) fn render(facts: &AboutFacts, close: impl IntoElement, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let colors = theme.colors;
    let mut table = KvTable::new().row("version", env!("CARGO_PKG_VERSION"));
    if let Some(settings) = &facts.settings {
        table = table.row("settings", settings.clone());
    }
    if let Some(logs) = &facts.logs {
        table = table.row("logs", logs.clone());
    }
    DialogBody::new("About icygui")
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(16.))
                .child(
                    img(Arc::new(Image::from_bytes(ImageFormat::Png, ICON.to_vec())))
                        .size(px(56.))
                        .flex_none(),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .child(
                            div()
                                .text_size(theme.text.title)
                                .text_color(colors.text_strong)
                                .child("icygui"),
                        )
                        .child(
                            div()
                                .text_color(colors.text_muted)
                                .child("A desktop client for the Icinga 2 monitoring API."),
                        ),
                ),
        )
        .child(table)
        .child(
            div()
                .text_size(theme.text.small)
                .text_color(colors.text_faint)
                .child(
                    "Live state from Icinga's event stream; notification rules, mutes and \
                     pauses stay on this computer. MIT licensed; IBM Plex Mono under the SIL \
                     Open Font License.",
                ),
        )
        .action(close)
        .into_any_element()
}
