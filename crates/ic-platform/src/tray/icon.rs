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

use super::{TrayLook, TrayTone};

/// Width and height in pixels. macOS draws menu-bar icons at most 22 pt
/// tall (so this is crisp up to 3x); Linux trays scale it to the panel.
pub(crate) const SIZE: u32 = 64;

/// Bytes in one icon (RGBA).
const BYTES: usize = 4 * 64 * 64;
const _: () = assert!(BYTES == 4 * (SIZE as usize) * (SIZE as usize));

/// The colour for "no state" (not connected, no environment): a mid grey
/// between the design's muted and faint text colours. The design's pending
/// grey (`#3a3f43`) is made for dots on the dark window and all but
/// vanishes on dark panels and menu bars, which would make "not connected"
/// look like a missing icon; this one keeps at least 3:1 contrast on light
/// and dark ones alike.
pub(crate) const NO_STATE: [u8; 3] = [0x7d, 0x84, 0x8a];

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

/// The tray icon for a look: a state's tint (`None` is the grey "no
/// state" icon), or the blind one.
///
/// # Errors
///
/// Never in practice: the buffer always matches [`SIZE`].
pub(crate) fn tray_icon(look: TrayLook) -> Result<Icon, BadIcon> {
    let rgba = match look {
        TrayLook::State(tone) => render(tone),
        TrayLook::Blind => render_blind(),
    };
    Icon::from_rgba(rgba, SIZE, SIZE)
}

/// How many dashes the blind look's orbit has, and the part of each
/// period they fill.
const DASHES: f64 = 10.0;
const DASH_FILL: f64 = 0.6;
/// The blind look's hollow core: its radius and half width (logo units).
const HOLLOW_RADIUS: f64 = 130.0;
const HOLLOW_HALF_WIDTH: f64 = 32.0;

/// The blind look (16e): the orbit dashed and the core hollow, both grey,
/// the node in the warning colour. Straight-alpha RGBA like [`render`].
pub(crate) fn render_blind() -> Vec<u8> {
    let mark = Mark::new();
    let centre = f64::from(SIZE) / 2.0;
    let node = TrayTone::Warning.rgb();
    let period = 2.0 * PI / DASHES;
    let mut rgba = Vec::with_capacity(BYTES);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let px = (f64::from(x) + 0.5 - centre) / mark.scale;
            let py = (f64::from(y) + 0.5 - centre) / mark.scale;
            // The orbit, cut into dashes: outside a dash, the distance to
            // its nearest end along the circle.
            let angle = py.atan2(px).rem_euclid(period);
            let dash = period * DASH_FILL;
            let outside = if angle < dash {
                0.0
            } else {
                (angle - dash).min(period - angle) * ORBIT_RADIUS
            };
            let orbit = mark.orbit_distance(px, py).max(outside - ORBIT_HALF_WIDTH);
            let hollow = (px.hypot(py) - HOLLOW_RADIUS).abs() - HOLLOW_HALF_WIDTH;
            let (distance, colour) = [
                (orbit, NO_STATE),
                (hollow, NO_STATE),
                (mark.node_distance(px, py), node),
            ]
            .into_iter()
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .unwrap_or((f64::INFINITY, NO_STATE));
            rgba.extend_from_slice(&colour);
            rgba.push(coverage_to_alpha(0.5 - distance * mark.scale));
        }
    }
    rgba
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
        assert!(tray_icon(TrayLook::State(Some(TrayTone::Critical))).is_ok());
        assert!(tray_icon(TrayLook::Blind).is_ok());
        assert_eq!(render_blind().len(), (SIZE * SIZE * 4) as usize);
    }

    #[test]
    fn the_design_state_colours() {
        assert_eq!(TrayTone::Critical.rgb(), [0xe0, 0x6c, 0x6c]);
        assert_eq!(TrayTone::Warning.rgb(), [0xe5, 0xb0, 0x4a]);
        assert_eq!(TrayTone::Unknown.rgb(), [0xa9, 0x7f, 0xdb]);
        assert_eq!(TrayTone::Ok.rgb(), [0x56, 0xb8, 0x70]);
        assert_eq!(NO_STATE, [0x7d, 0x84, 0x8a]);
    }

    /// WCAG 2 relative luminance of an sRGB colour.
    fn luminance(rgb: [u8; 3]) -> f64 {
        let linear = |channel: u8| {
            let c = f64::from(channel) / 255.0;
            if c <= 0.040_45 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(rgb[0]) + 0.7152 * linear(rgb[1]) + 0.0722 * linear(rgb[2])
    }

    /// WCAG 2 contrast ratio, from 1 (none) to 21.
    fn contrast(a: [u8; 3], b: [u8; 3]) -> f64 {
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    #[test]
    fn the_no_state_grey_reads_on_light_and_dark_panels() {
        // WCAG asks for 3:1 for graphical objects.
        for (panel, rgb) in [
            ("white", [0xff, 0xff, 0xff]),
            ("macOS light menu bar", [0xf6, 0xf6, 0xf6]),
            ("KDE Breeze light panel", [0xef, 0xf0, 0xf1]),
            ("GNOME top bar", [0x00, 0x00, 0x00]),
            ("Yaru dark panel", [0x1d, 0x1d, 0x1d]),
            ("macOS dark menu bar", [0x2c, 0x2c, 0x2e]),
            ("KDE Breeze dark panel", [0x31, 0x36, 0x3b]),
        ] {
            let ratio = contrast(NO_STATE, rgb);
            assert!(ratio >= 3.0, "{panel}: {ratio:.2}:1");
        }
        // The design's pending grey, the colour this replaces, doesn't.
        assert!(contrast([0x3a, 0x3f, 0x43], [0x2c, 0x2c, 0x2e]) < 1.5);
        // Sanity checks of the formula.
        assert!((contrast([0, 0, 0], [0xff, 0xff, 0xff]) - 21.0).abs() < 1e-9);
        assert!((contrast(NO_STATE, NO_STATE) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn the_no_state_grey_is_a_neutral_grey() {
        // No hue, so it can't be mistaken for a state colour.
        let [r, g, b] = NO_STATE;
        assert!(r.max(g).max(b) - r.min(g).min(b) <= 0x10, "{NO_STATE:x?}");
        for tone in TONES.into_iter().flatten() {
            assert_ne!(NO_STATE, tone.rgb());
        }
        assert_ne!(NO_STATE, ACCENT);
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

    /// REL-07: the shapes drawn here are the logo's mark
    /// (`assets/logo/icygui-mark.svg`): the same orbit radius and opening,
    /// core and node position. Only the orbit and the node are bolder.
    #[test]
    fn geometry_follows_the_logo() {
        const SVG: &str = include_str!("../../../../assets/logo/icygui-mark.svg");
        const CENTRE: f64 = 512.0;
        let elements: Vec<&str> = SVG.split('<').collect();
        let attribute = |element: &str, name: &str| -> f64 {
            let key = format!(" {name}=\"");
            let start = element.find(&key).unwrap() + key.len();
            let len = element[start..].find('"').unwrap();
            element[start..start + len].parse().unwrap()
        };
        let close = |a: f64, b: f64, what: &str| assert!((a - b).abs() < 0.1, "{what}: {a} ≠ {b}");
        let angle = |x: f64, y: f64| (y - CENTRE).atan2(x - CENTRE).to_degrees();

        let path = elements.iter().find(|e| e.starts_with("path ")).unwrap();
        let start = path.find(" d=\"").unwrap() + 4;
        let d: Vec<f64> = path[start..start + path[start..].find('"').unwrap()]
            .split(|c: char| c == ' ' || c.is_ascii_alphabetic())
            .filter(|token| !token.is_empty())
            .map(|token| token.parse().unwrap())
            .collect();
        // M x0 y0 A rx ry rotation large-arc sweep x1 y1
        let [x0, y0, rx, ry, _, _, _, x1, y1] = d[..] else {
            panic!("unexpected orbit path {d:?}");
        };
        close(rx, ORBIT_RADIUS, "orbit radius");
        close(ry, ORBIT_RADIUS, "orbit radius");
        close(angle(x0, y0), GAP_TO_DEGREES, "orbit opening end");
        close(angle(x1, y1), GAP_FROM_DEGREES, "orbit opening start");
        assert!(attribute(path, "stroke-width") / 2.0 <= ORBIT_HALF_WIDTH);

        let circles: Vec<&&str> = elements
            .iter()
            .filter(|e| e.starts_with("circle "))
            .collect();
        let [core, node] = circles[..] else {
            panic!("expected the core and the node, got {circles:?}");
        };
        close(attribute(core, "cx"), CENTRE, "core x");
        close(attribute(core, "cy"), CENTRE, "core y");
        close(attribute(core, "r"), CORE_RADIUS, "core radius");
        let (nx, ny) = (attribute(node, "cx"), attribute(node, "cy"));
        close(angle(nx, ny), NODE_DEGREES, "node angle");
        assert!(((nx - CENTRE).hypot(ny - CENTRE) - ORBIT_RADIUS).abs() < 0.5);
        assert!(attribute(node, "r") <= NODE_RADIUS);
        assert!(node.contains("fill=\"#74ade8\""), "the node is accent blue");
        let [r, g, b] = ACCENT;
        assert!(node.contains(&format!("#{r:02x}{g:02x}{b:02x}")));
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

    #[test]
    fn the_blind_look_is_grey_with_a_hollow_core_and_a_yellow_node() {
        let rgba = render_blind();
        // The core's centre is empty; its ring is grey.
        assert_eq!(pixel_at(&rgba, 0.0, 0.0)[3], 0, "hollow");
        assert_eq!(pixel_at(&rgba, 90.0, HOLLOW_RADIUS), opaque(NO_STATE));
        // The node keeps its place, in the warning colour.
        assert_eq!(
            pixel_at(&rgba, NODE_DEGREES, ORBIT_RADIUS),
            opaque(TrayTone::Warning.rgb())
        );
        // The orbit is dashed: on it, some points are drawn and some not.
        let on_orbit: Vec<u8> = (0..72)
            .map(|step| pixel_at(&rgba, f64::from(step) * 5.0 + 2.5, ORBIT_RADIUS)[3])
            .collect();
        assert!(
            on_orbit.contains(&255) && on_orbit.contains(&0),
            "{on_orbit:?}"
        );
        assert_ne!(rgba, render(None));
    }
}
