//! Renders the logo (`assets/logo/*.svg`, the single source of truth) into
//! the app icon PNGs, the macOS `.icns` and the README banner.

use std::path::Path;
use std::sync::Arc;

use icns::{IconFamily, Image, PixelFormat};
use resvg::{tiny_skia, usvg};

/// The app icon source, relative to the workspace root.
pub(crate) const ICON_SVG: &str = "assets/logo/icygui.svg";
/// The icon sizes in an `.icns` file.
const ICNS_SIZES: [u32; 7] = [16, 32, 64, 128, 256, 512, 1024];

fn parse(svg: &[u8], fonts_dir: Option<&Path>) -> Result<usvg::Tree, String> {
    let mut options = usvg::Options::default();
    if let Some(dir) = fonts_dir {
        let mut db = usvg::fontdb::Database::new();
        db.load_fonts_dir(dir);
        options.fontdb = Arc::new(db);
    }
    usvg::Tree::from_data(svg, &options).map_err(|error| format!("parsing SVG: {error}"))
}

#[expect(
    clippy::cast_precision_loss,
    reason = "image sizes are at most a few thousand pixels"
)]
fn render(tree: &usvg::Tree, width: u32, height: u32) -> Result<tiny_skia::Pixmap, String> {
    let mut pixmap = tiny_skia::Pixmap::new(width, height)
        .ok_or_else(|| format!("bad image size {width}x{height}"))?;
    let transform = tiny_skia::Transform::from_scale(
        width as f32 / tree.size().width(),
        height as f32 / tree.size().height(),
    );
    resvg::render(tree, transform, &mut pixmap.as_mut());
    Ok(pixmap)
}

/// Renders an SVG document into a square PNG of `size` pixels.
pub(crate) fn render_svg(svg: &[u8], size: u32) -> Result<Vec<u8>, String> {
    render(&parse(svg, None)?, size, size)?
        .encode_png()
        .map_err(|error| format!("encoding PNG: {error}"))
}

/// Renders an SVG at its own aspect ratio, `width` pixels wide, using the
/// fonts in `fonts_dir` for text.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "image sizes are small positive numbers"
)]
pub(crate) fn render_svg_wide(svg: &[u8], width: u32, fonts_dir: &Path) -> Result<Vec<u8>, String> {
    let tree = parse(svg, Some(fonts_dir))?;
    let height = (tree.size().height() * width as f32 / tree.size().width()).round() as u32;
    render(&tree, width, height)?
        .encode_png()
        .map_err(|error| format!("encoding PNG: {error}"))
}

/// The icon as a macOS `.icns` file with every standard size.
pub(crate) fn icns(svg: &[u8]) -> Result<Vec<u8>, String> {
    let tree = parse(svg, None)?;
    let mut family = IconFamily::new();
    for size in ICNS_SIZES {
        let pixmap = render(&tree, size, size)?;
        // tiny-skia stores premultiplied alpha; .icns wants straight alpha.
        let rgba: Vec<u8> = pixmap
            .pixels()
            .iter()
            .flat_map(|pixel| {
                let color = pixel.demultiply();
                [color.red(), color.green(), color.blue(), color.alpha()]
            })
            .collect();
        let image = Image::from_data(PixelFormat::RGBA, size, size, rgba)
            .map_err(|error| error.to_string())?;
        family.add_icon(&image).map_err(|error| error.to_string())?;
    }
    let mut out = Vec::new();
    family.write(&mut out).map_err(|error| error.to_string())?;
    Ok(out)
}
