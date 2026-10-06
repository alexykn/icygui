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

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::{APP_NAME, ICON_SIZES, root};

    fn logo(name: &str) -> String {
        fs::read_to_string(root().join("assets/logo").join(name)).unwrap()
    }

    /// The mark's drawing: the orange gradient, the orbit, the core and the
    /// node, one trimmed line each.
    fn mark_lines(svg: &str) -> Vec<&str> {
        let mut lines = Vec::new();
        let mut in_orange = false;
        for line in svg.lines().map(str::trim) {
            if line.starts_with(r#"<linearGradient id="orange""#) {
                in_orange = true;
            }
            if in_orange || line.starts_with("<path ") || line.starts_with("<circle ") {
                lines.push(line);
            }
            if line == "</linearGradient>" {
                in_orange = false;
            }
        }
        lines
    }

    /// REL-01: the app icon, the bare mark and the banner draw the same
    /// mark, so `icygui.svg` stays the single source of its shape.
    #[test]
    fn every_logo_file_draws_the_same_mark() {
        let icon = logo("icygui.svg");
        let reference = mark_lines(&icon);
        assert_eq!(reference.len(), 7, "{reference:#?}");
        for name in ["icygui-mark.svg", "banner.svg"] {
            assert_eq!(mark_lines(&logo(name)), reference, "{name}");
        }
    }

    /// `committed` and `rendered` are the same image. Rounding in
    /// tiny-skia's SIMD code differs a little between CPU architectures,
    /// so pixels may differ by a few levels.
    fn assert_same_image(committed: &[u8], rendered: &[u8], what: &str) {
        if committed == rendered {
            return;
        }
        let committed = tiny_skia::Pixmap::decode_png(committed).unwrap();
        let rendered = tiny_skia::Pixmap::decode_png(rendered).unwrap();
        assert_eq!(
            (committed.width(), committed.height()),
            (rendered.width(), rendered.height()),
            "{what} is stale: run `cargo xtask icons`"
        );
        let worst = committed
            .data()
            .iter()
            .zip(rendered.data())
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap_or(0);
        assert!(
            worst <= 4,
            "{what} is stale (pixels differ by up to {worst}): run `cargo xtask icons`"
        );
    }

    /// The committed icons and banner are what `cargo xtask icons` renders
    /// from the SVGs today (rerun it after editing `assets/logo`).
    #[test]
    fn committed_images_are_rendered_from_the_svgs() {
        let icon = logo("icygui.svg");
        let icons = root().join("assets/icons");
        for size in ICON_SIZES {
            let name = format!("{APP_NAME}-{size}.png");
            assert_same_image(
                &fs::read(icons.join(&name)).unwrap(),
                &render_svg(icon.as_bytes(), size).unwrap(),
                &format!("assets/icons/{name}"),
            );
        }
        // The `.icns` is generated on x86_64 (the development machine);
        // elsewhere the PNG checks above stand in for it.
        if cfg!(target_arch = "x86_64") {
            let committed = fs::read(icons.join(format!("{APP_NAME}.icns"))).unwrap();
            assert!(
                committed == icns(icon.as_bytes()).unwrap(),
                "assets/icons/{APP_NAME}.icns is stale: run `cargo xtask icons`"
            );
        }
        let banner = render_svg_wide(
            logo("banner.svg").as_bytes(),
            1280,
            &root().join("crates/ic-ui-kit/fonts"),
        )
        .unwrap();
        assert_same_image(
            &fs::read(root().join("assets/logo/banner.png")).unwrap(),
            &banner,
            "assets/logo/banner.png",
        );
    }
}
