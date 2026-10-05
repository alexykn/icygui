//! Bundled IBM Plex Mono (SIL Open Font License 1.1, see `fonts/OFL.txt`).

use std::borrow::Cow;

use gpui::App;

const REGULAR: &[u8] = include_bytes!("../fonts/IBMPlexMono-Regular.ttf");
const MEDIUM: &[u8] = include_bytes!("../fonts/IBMPlexMono-Medium.ttf");
const SEMI_BOLD: &[u8] = include_bytes!("../fonts/IBMPlexMono-SemiBold.ttf");

/// The platform text system rejected the bundled fonts.
#[derive(Debug, thiserror::Error)]
#[error("failed to register the bundled fonts")]
pub struct FontError(#[source] Box<dyn std::error::Error + Send + Sync>);

pub(crate) fn register(cx: &App) -> Result<(), FontError> {
    cx.text_system()
        .add_fonts(vec![
            Cow::Borrowed(REGULAR),
            Cow::Borrowed(MEDIUM),
            Cow::Borrowed(SEMI_BOLD),
        ])
        .map_err(|error| FontError(error.into()))
}
