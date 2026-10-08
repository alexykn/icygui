//! The stat tiles' trend line (topic 06): the last 30 minutes of status
//! polls in the faint colour, the latest segment and point in the accent
//! (or the tile's warning or critical colour).

use gpui::{Bounds, Hsla, IntoElement, PathBuilder, Pixels, Styled as _, canvas, point};
use ic_ui_kit::px;

/// The line's size, as drawn in the mock-up.
pub(crate) const WIDTH: f32 = 132.;
pub(crate) const HEIGHT: f32 = 22.;

/// Where `values` go in a `width` × `height` box: x spread evenly with a
/// 2 px margin, y scaled between the smallest and largest value with 3 px
/// above and below (a flat line sits at the bottom). Fewer than two values
/// give none.
pub(crate) fn layout(values: &[f64], width: f32, height: f32) -> Vec<(f32, f32)> {
    if values.len() < 2 {
        return Vec::new();
    }
    let finite = || values.iter().copied().filter(|value| value.is_finite());
    let min = finite().fold(f64::INFINITY, f64::min);
    let max = finite().fold(f64::NEG_INFINITY, f64::max);
    let span = if max > min { max - min } else { 1.0 };
    #[expect(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        reason = "a few hundred points; screen coordinates"
    )]
    let points = values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let x = index as f32 / (values.len() - 1) as f32 * (width - 4.) + 2.;
            let value = if value.is_finite() { *value } else { min };
            let share = ((value - min) / span) as f32;
            (x, height - 3. - share * (height - 6.))
        })
        .collect();
    points
}

/// The trend line of `values` (none for fewer than two), `line` for the
/// trend, `last` for its latest segment and point.
pub(crate) fn sparkline(values: &[f64], line: Hsla, last: Hsla) -> impl IntoElement {
    let points = layout(values, WIDTH, HEIGHT);
    canvas(
        move |_, _, _| points,
        move |bounds: Bounds<Pixels>, points, window, _| {
            let at = |(x, y): (f32, f32)| point(bounds.origin.x + px(x), bounds.origin.y + px(y));
            let Some(((first, rest), end)) = points.split_first().zip(points.last()) else {
                return;
            };
            let mut trend = PathBuilder::stroke(px(1.5));
            trend.move_to(at(*first));
            for point in rest {
                trend.line_to(at(*point));
            }
            if let Ok(path) = trend.build() {
                window.paint_path(path, line);
            }
            if let [.., before, _] = points.as_slice() {
                let mut latest = PathBuilder::stroke(px(2.));
                latest.move_to(at(*before));
                latest.line_to(at(*end));
                if let Ok(path) = latest.build() {
                    window.paint_path(path, last);
                }
            }
            let radius = px(2.5);
            let center = at(*end);
            window.paint_quad(
                gpui::fill(
                    Bounds::new(
                        point(center.x - radius, center.y - radius),
                        gpui::size(radius * 2., radius * 2.),
                    ),
                    last,
                )
                .corner_radii(radius),
            );
        },
    )
    .w(px(WIDTH))
    .h(px(HEIGHT))
    .flex_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn points_fill_the_box() {
        let points = layout(&[0.0, 5.0, 10.0], 132., 22.);
        assert_eq!(points.len(), 3);
        assert!((points[0].0 - 2.).abs() < 1e-4);
        assert!((points[2].0 - 130.).abs() < 1e-4);
        assert!(
            (points[0].1 - 19.).abs() < 1e-4,
            "the smallest at the bottom"
        );
        assert!((points[2].1 - 3.).abs() < 1e-4, "the largest at the top");
        assert!((points[1].1 - 11.).abs() < 1e-4);
    }

    #[test]
    fn a_flat_trend_lies_at_the_bottom_and_one_point_draws_nothing() {
        let flat = layout(&[4.0, 4.0, 4.0], 132., 22.);
        assert!(flat.iter().all(|(_, y)| (*y - 19.).abs() < 1e-4));
        assert!(layout(&[4.0], 132., 22.).is_empty());
        assert!(layout(&[], 132., 22.).is_empty());
    }

    #[test]
    fn odd_values_stay_inside() {
        let points = layout(&[1.0, f64::NAN, 3.0], 132., 22.);
        assert!(points.iter().all(|(_, y)| (3.0..=19.0).contains(y)));
    }
}
