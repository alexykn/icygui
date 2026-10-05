//! The tray icon, drawn in code: the logo's mark (an orbit ring, the core
//! disc and the blue accent node in the orbit's gap, see
//! `assets/logo/icygui-mark.svg`) with the orbit and the core tinted in the
//! state colour, so the core reads as the design's state dot (REL-07).
//!
//! Shapes are rendered from their signed distance with a one-pixel
//! anti-aliasing ramp. Each pixel takes the colour of the nearest shape, so
//! edges blend into transparency without dark fringes even in hosts that
//! scale the image without premultiplying.

use std::f64::consts::PI;

use tray_icon::{BadIcon, Icon};

use super::TrayTone;

/// Width and height in pixels. macOS draws menu-bar icons at most 22 pt
/// tall (so this is crisp up to 3x); Linux trays scale it to the panel.
pub(crate) const SIZE: u32 = 64;

/// Bytes in one icon (RGBA).
const BYTES: usize = 4 * 64 * 64;
const _: () = assert!(BYTES == 4 * (SIZE as usize) * (SIZE as usize));

/// The colour for "no state" (not connected, no environment): the design's
/// pending grey.
pub(crate) const NO_STATE: [u8; 3] = [0x3a, 0x3f, 0x43];

/// The design's accent blue, for the node.
pub(crate) const ACCENT: [u8; 3] = [0x74, 0xad, 0xe8];

// Geometry in the logo's units, relative to the mark's centre (512, 512),
// y pointing down. The orbit and the node are a little bolder than in the
// logo so they survive being drawn 16 to 22 pixels tall.
const ORBIT_RADIUS: f64 = 292.0;
const ORBIT_HALF_WIDTH: f64 = 20.0; // logo: 15
const CORE_RADIUS: f64 = 178.0;
const NODE_RADIUS: f64 = 50.0; // logo: 44
/// The orbit is open between these angles (degrees, clockwise from 3
/// o'clock): the top-right gap that holds the node.
const GAP_FROM_DEGREES: f64 = -77.0;
const GAP_TO_DEGREES: f64 = -13.0;
const NODE_DEGREES: f64 = -45.0;
/// How many pixels the whole mark spans; the rest is padding, which keeps
/// the glyph near the 16 pt macOS recommends inside the 22 pt slot.
const MARK_PIXELS: f64 = 50.0;

/// The tray icon for a state; `None` is the grey "no state" icon.
///
/// # Errors
///
/// Never in practice: the buffer always matches [`SIZE`].
pub(crate) fn tray_icon(tone: Option<TrayTone>) -> Result<Icon, BadIcon> {
    Icon::from_rgba(render(tone), SIZE, SIZE)
}

/// Straight-alpha RGBA pixels, row by row, [`SIZE`] × [`SIZE`].
pub(crate) fn render(tone: Option<TrayTone>) -> Vec<u8> {
    let state = tone.map_or(NO_STATE, TrayTone::rgb);
    let mark = Mark::new();
    let centre = f64::from(SIZE) / 2.0;
    let mut rgba = Vec::with_capacity(BYTES);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let px = (f64::from(x) + 0.5 - centre) / mark.scale;
            let py = (f64::from(y) + 0.5 - centre) / mark.scale;
            let (distance, colour) = [
                (mark.orbit_distance(px, py), state),
                (core_distance(px, py), state),
                (mark.node_distance(px, py), ACCENT),
            ]
            .into_iter()
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .unwrap_or((f64::INFINITY, state));
            rgba.extend_from_slice(&colour);
            rgba.push(coverage_to_alpha(0.5 - distance * mark.scale));
        }
    }
    rgba
}

/// The mark's shapes, with signed distances in logo units (negative
/// inside).
struct Mark {
    /// Pixels per logo unit.
    scale: f64,
    gap_from: f64,
    gap_to: f64,
    gap_ends: [(f64, f64); 2],
    node: (f64, f64),
}

impl Mark {
    fn new() -> Self {
        let polar = |degrees: f64| {
            let radians = degrees * PI / 180.0;
            (ORBIT_RADIUS * radians.cos(), ORBIT_RADIUS * radians.sin())
        };
        Self {
            scale: MARK_PIXELS / (2.0 * (ORBIT_RADIUS + ORBIT_HALF_WIDTH)),
            gap_from: GAP_FROM_DEGREES * PI / 180.0,
            gap_to: GAP_TO_DEGREES * PI / 180.0,
            gap_ends: [polar(GAP_FROM_DEGREES), polar(GAP_TO_DEGREES)],
            node: polar(NODE_DEGREES),
        }
    }

    /// The ring with round caps at both ends of the gap.
    fn orbit_distance(&self, x: f64, y: f64) -> f64 {
        let angle = y.atan2(x);
        let to_centre_line = if angle > self.gap_from && angle < self.gap_to {
            // In the gap: the nearest point is one of the arc's ends.
            self.gap_ends
                .iter()
                .map(|&(ex, ey)| (x - ex).hypot(y - ey))
                .fold(f64::INFINITY, f64::min)
        } else {
            (x.hypot(y) - ORBIT_RADIUS).abs()
        };
        to_centre_line - ORBIT_HALF_WIDTH
    }

    fn node_distance(&self, x: f64, y: f64) -> f64 {
        (x - self.node.0).hypot(y - self.node.1) - NODE_RADIUS
    }
}

fn core_distance(x: f64, y: f64) -> f64 {
    x.hypot(y) - CORE_RADIUS
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the value is clamped to 0..=255 before the cast"
)]
fn coverage_to_alpha(coverage: f64) -> u8 {
    (coverage.clamp(0.0, 1.0) * 255.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    const TONES: [Option<TrayTone>; 5] = [
        None,
        Some(TrayTone::Critical),
        Some(TrayTone::Warning),
        Some(TrayTone::Unknown),
        Some(TrayTone::Ok),
    ];

    /// The pixel nearest to a point given in logo units around the centre.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the points tested lie inside the 64 px canvas"
    )]
    fn pixel_at(rgba: &[u8], degrees: f64, radius: f64) -> [u8; 4] {
        let scale = MARK_PIXELS / (2.0 * (ORBIT_RADIUS + ORBIT_HALF_WIDTH));
        let radians = degrees * PI / 180.0;
        let centre = f64::from(SIZE) / 2.0;
        let x = (centre + radius * scale * radians.cos()).floor();
        let y = (centre + radius * scale * radians.sin()).floor();
        let index = (y as usize * SIZE as usize + x as usize) * 4;
        rgba[index..index + 4].try_into().unwrap()
    }

    fn colour(tone: Option<TrayTone>) -> [u8; 3] {
        tone.map_or(NO_STATE, TrayTone::rgb)
    }

    fn opaque(rgb: [u8; 3]) -> [u8; 4] {
        [rgb[0], rgb[1], rgb[2], 255]
    }

    #[test]
    fn size_and_layout() {
        for tone in TONES {
            assert_eq!(render(tone).len(), (SIZE * SIZE * 4) as usize);
        }
        assert!(tray_icon(Some(TrayTone::Critical)).is_ok());
    }

    #[test]
    fn the_design_state_colours() {
        assert_eq!(TrayTone::Critical.rgb(), [0xe0, 0x6c, 0x6c]);
        assert_eq!(TrayTone::Warning.rgb(), [0xe5, 0xb0, 0x4a]);
        assert_eq!(TrayTone::Unknown.rgb(), [0xa9, 0x7f, 0xdb]);
        assert_eq!(TrayTone::Ok.rgb(), [0x56, 0xb8, 0x70]);
        assert_eq!(NO_STATE, [0x3a, 0x3f, 0x43]);
    }

    #[test]
    fn the_core_and_the_orbit_carry_the_state_colour() {
        for tone in TONES {
            let rgba = render(tone);
            let state = opaque(colour(tone));
            assert_eq!(pixel_at(&rgba, 0.0, 0.0), state, "core centre, {tone:?}");
            assert_eq!(
                pixel_at(&rgba, 135.0, 100.0),
                state,
                "inside the core, {tone:?}"
            );
            for degrees in [0.0, 45.0, 90.0, 180.0, 270.0] {
                assert_eq!(
                    pixel_at(&rgba, degrees, ORBIT_RADIUS),
                    state,
                    "orbit at {degrees}°, {tone:?}"
                );
            }
        }
    }

    #[test]
    fn the_node_is_accent_blue() {
        for tone in TONES {
            let rgba = render(tone);
            assert_eq!(pixel_at(&rgba, NODE_DEGREES, ORBIT_RADIUS), opaque(ACCENT));
        }
    }

    #[test]
    fn background_gaps_and_corners_are_transparent() {
        let rgba = render(Some(TrayTone::Critical));
        for (x, y) in [(0, 0), (SIZE - 1, 0), (0, SIZE - 1), (SIZE - 1, SIZE - 1)] {
            let index = ((y * SIZE + x) * 4 + 3) as usize;
            assert_eq!(rgba[index], 0, "corner ({x}, {y})");
        }
        // Between the core and the orbit.
        for degrees in [0.0, 90.0, 200.0] {
            assert_eq!(
                pixel_at(&rgba, degrees, 235.0)[3],
                0,
                "ring gap at {degrees}°"
            );
        }
        // The orbit's opening on both sides of the node.
        assert_eq!(pixel_at(&rgba, -26.0, ORBIT_RADIUS)[3], 0);
        assert_eq!(pixel_at(&rgba, -64.0, ORBIT_RADIUS)[3], 0);
        // Outside the mark.
        assert_eq!(pixel_at(&rgba, 90.0, 360.0)[3], 0);
    }

    #[test]
    fn edges_are_anti_aliased_without_foreign_colours() {
        for tone in TONES {
            let rgba = render(tone);
            let state = colour(tone);
            let mut partial = 0;
            for pixel in rgba.chunks_exact(4) {
                let rgb = [pixel[0], pixel[1], pixel[2]];
                if pixel[3] > 0 {
                    assert!(rgb == state || rgb == ACCENT, "{pixel:?} in {tone:?}");
                }
                if pixel[3] > 0 && pixel[3] < 255 {
                    partial += 1;
                }
            }
            assert!(partial > 100, "only {partial} soft edge pixels");
        }
    }

    #[test]
    fn every_tone_has_the_same_shape() {
        let alpha = |tone| -> Vec<u8> { render(tone).chunks_exact(4).map(|p| p[3]).collect() };
        let reference = alpha(None);
        for tone in TONES {
            assert_eq!(alpha(tone), reference, "{tone:?}");
        }
        // The mark covers a sensible share of the canvas: a dot, not a
        // speck and not a filled square.
        let covered = reference.iter().filter(|&&a| a > 127).count();
        assert!((800..2000).contains(&covered), "{covered} of 4096 pixels");
    }

    #[test]
    fn tones_are_distinguishable() {
        let icons: Vec<Vec<u8>> = TONES.iter().map(|&tone| render(tone)).collect();
        for (i, a) in icons.iter().enumerate() {
            for b in &icons[i + 1..] {
                assert_ne!(a, b);
            }
        }
        assert_eq!(render(Some(TrayTone::Ok)), render(Some(TrayTone::Ok)));
    }
}
