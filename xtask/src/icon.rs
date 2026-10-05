//! The app icon, drawn in code: a dark rounded square with three rows of
//! the design's problem list (state circle + text line) in critical,
//! warning and ok colours.

use icns::{IconFamily, Image, PixelFormat};

const BACKGROUND: [u8; 3] = [0x1d, 0x21, 0x25];
const BORDER: [u8; 3] = [0x33, 0x38, 0x3c];
const ROWS: [([u8; 3], [u8; 3], f32); 3] = [
    ([0xe0, 0x6c, 0x6c], [0xec, 0xee, 0xef], 0.78),
    ([0xe5, 0xb0, 0x4a], [0xb5, 0xb9, 0xbc], 0.70),
    ([0x56, 0xb8, 0x70], [0x8b, 0x90, 0x94], 0.74),
];
/// Samples per pixel along each axis (16 per pixel) for anti-aliasing.
const SUPERSAMPLE: u32 = 4;

/// RGBA pixels for a square icon of `size` pixels.
#[expect(
    clippy::cast_precision_loss,
    reason = "icon sizes and sample indices are at most a few thousand"
)]
pub(crate) fn rgba(size: u32) -> Vec<u8> {
    let mut pixels = Vec::with_capacity((size * size * 4) as usize);
    let size_f = size as f32;
    for y in 0..size {
        for x in 0..size {
            let mut acc = [0.0_f32; 4];
            for sy in 0..SUPERSAMPLE {
                for sx in 0..SUPERSAMPLE {
                    let px = (x as f32 + (sx as f32 + 0.5) / SUPERSAMPLE as f32) / size_f;
                    let py = (y as f32 + (sy as f32 + 0.5) / SUPERSAMPLE as f32) / size_f;
                    let [r, g, b, a] = sample(px, py);
                    acc[0] += r * a;
                    acc[1] += g * a;
                    acc[2] += b * a;
                    acc[3] += a;
                }
            }
            let samples = (SUPERSAMPLE * SUPERSAMPLE) as f32;
            let alpha = acc[3] / samples;
            let channel = |sum: f32| {
                if acc[3] > 0.0 {
                    to_byte(sum / acc[3])
                } else {
                    0
                }
            };
            pixels.extend_from_slice(&[
                channel(acc[0]),
                channel(acc[1]),
                channel(acc[2]),
                to_byte(alpha),
            ]);
        }
    }
    pixels
}

/// Colour (0–1 per channel) and coverage at a point in unit coordinates.
#[expect(clippy::cast_precision_loss, reason = "the row index is 0..3")]
fn sample(x: f32, y: f32) -> [f32; 4] {
    // macOS icon grid: content inset ~10%, corner radius ~22% of the content.
    let inset = 0.098;
    let radius = 0.18;
    if !in_rounded_rect(x, y, inset, inset, 1.0 - inset, 1.0 - inset, radius) {
        return [0.0; 4];
    }
    let border = 0.012;
    let mut color = if in_rounded_rect(
        x,
        y,
        inset + border,
        inset + border,
        1.0 - inset - border,
        1.0 - inset - border,
        radius - border,
    ) {
        BACKGROUND
    } else {
        BORDER
    };
    for (index, (circle, line, length)) in ROWS.iter().enumerate() {
        let cy = 0.335 + 0.165 * index as f32;
        let cx = 0.315;
        let r = 0.062;
        if (x - cx).hypot(y - cy) <= r {
            color = *circle;
        }
        let half_height = 0.026;
        if in_rounded_rect(
            x,
            y,
            0.425,
            cy - half_height,
            *length,
            cy + half_height,
            half_height,
        ) {
            color = *line;
        }
    }
    [
        f32::from(color[0]) / 255.0,
        f32::from(color[1]) / 255.0,
        f32::from(color[2]) / 255.0,
        1.0,
    ]
}

fn in_rounded_rect(
    x: f32,
    y: f32,
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
    radius: f32,
) -> bool {
    if x < left || x > right || y < top || y > bottom {
        return false;
    }
    let cx = x.clamp(left + radius, right - radius);
    let cy = y.clamp(top + radius, bottom - radius);
    (x - cx).hypot(y - cy) <= radius
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the value is clamped to 0..=255 first"
)]
fn to_byte(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// The icon as a PNG file.
pub(crate) fn png(size: u32) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, size, size);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|error| error.to_string())?;
    writer
        .write_image_data(&rgba(size))
        .map_err(|error| error.to_string())?;
    writer.finish().map_err(|error| error.to_string())?;
    Ok(out)
}

/// The icon as a macOS `.icns` file with every standard size.
pub(crate) fn icns() -> Result<Vec<u8>, String> {
    let mut family = IconFamily::new();
    for size in [16, 32, 64, 128, 256, 512, 1024] {
        let image = Image::from_data(PixelFormat::RGBA, size, size, rgba(size))
            .map_err(|error| error.to_string())?;
        family.add_icon(&image).map_err(|error| error.to_string())?;
    }
    let mut out = Vec::new();
    family.write(&mut out).map_err(|error| error.to_string())?;
    Ok(out)
}
