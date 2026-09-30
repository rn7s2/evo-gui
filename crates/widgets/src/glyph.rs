//! The two small glyphs the chrome draws itself: the `×` in a tab's close button
//! and the `+` at the end of the strip.
//!
//! `TabStrip.tsx` draws them as SVG strokes — two lines through a square, with
//! `strokeLinecap: round`. gpui has no stroked-path *element*: a path is painted
//! through [`gpui_kit::canvas`] and built with [`PathBuilder::stroke`], which
//! takes lyon's options but does not re-export `LineCap`, so its strokes end
//! square. A glyph is therefore drawn as its strokes, plus a small round cap at
//! each end — the design's `round` caps, at the size the design draws them.
//!
//! The geometry is here, in the open: [`cross_lines`] and [`plus_lines`] say where
//! the strokes go, and a test holds them to the design's own coordinates.

use std::f32::consts::PI;

use gpui_kit::prelude::*;
use gpui_kit::{
    canvas, point, px, AnyElement, Hsla, IntoElement, PathBuilder, Pixels, Point, Window,
};

use crate::paint::{color, Rgb};

/// How many points a round cap is drawn with. Twelve is a circle at any size a
/// glyph is drawn at (a 10px glyph's cap is under a pixel across).
const CAP_POINTS: usize = 12;

/// A line of a glyph, in the square's own coordinates.
pub type Line = (Point<Pixels>, Point<Pixels>);

/// The `×`: two strokes corner to corner.
///
/// The design's 10px glyph runs from 1.5 to 8.5 of a 10px box — 15% to 85% — so
/// the fractions are the design's, whatever size the caller draws it at.
pub fn cross_lines(size: f32) -> Vec<Line> {
    let (from, to) = (size * 0.15, size * 0.85);
    vec![
        (point(px(from), px(from)), point(px(to), px(to))),
        (point(px(to), px(from)), point(px(from), px(to))),
    ]
}

/// The `+`: one stroke each way through the middle.
///
/// The design's path is `M7 1.5v11M1.5 7h11` in a 14-unit box, drawn 16px wide:
/// 1.5 to 12.5 of 14, which is where the fractions come from.
pub fn plus_lines(size: f32) -> Vec<Line> {
    let (from, to) = (size * (1.5 / 14.), size * (12.5 / 14.));
    let mid = size / 2.;
    vec![
        (point(px(from), px(mid)), point(px(to), px(mid))),
        (point(px(mid), px(from)), point(px(mid), px(to))),
    ]
}

/// The points of a filled circle of `radius` about `centre`, for a round cap.
pub fn cap_points(centre: Point<Pixels>, radius: f32) -> Vec<Point<Pixels>> {
    (0..CAP_POINTS)
        .map(|step| {
            let angle = 2. * PI * step as f32 / CAP_POINTS as f32;
            point(
                centre.x + px(radius * angle.cos()),
                centre.y + px(radius * angle.sin()),
            )
        })
        .collect()
}

/// Paint a glyph: its strokes, then a round cap at each of their ends.
///
/// One painter for both glyphs and for anything else a caller wants stroked —
/// the geometry is the caller's, the drawing is here.
pub fn stroked(size: f32, width: f32, lines: &[Line], colour: Hsla) -> AnyElement {
    let lines = lines.to_vec();
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let at = |p: Point<Pixels>| point(bounds.origin.x + p.x, bounds.origin.y + p.y);
            let mut builder = PathBuilder::stroke(px(width));
            for (from, to) in &lines {
                builder.move_to(at(*from));
                builder.line_to(at(*to));
            }
            if let Ok(path) = builder.build() {
                window.paint_path(path, colour);
            }
            // `strokeLinecap: round`: a disc at each end, so the glyph reads the
            // same at any size and the two strokes of a `×` meet in the middle.
            for (from, to) in &lines {
                for end in [*from, *to] {
                    let mut cap = PathBuilder::fill();
                    let points = cap_points(end, width / 2.);
                    cap.move_to(at(points[0]));
                    for point in &points[1..] {
                        cap.line_to(at(*point));
                    }
                    cap.close();
                    if let Ok(path) = cap.build() {
                        window.paint_path(path, colour);
                    }
                }
            }
        },
    )
    .flex_none()
    .size(px(size))
    .into_any_element()
}

/// The `×` in a close button: 10px with a 1.3px stroke, as the design draws it.
pub fn cross(colour: impl Into<Hsla>) -> AnyElement {
    let colour = colour.into();
    stroked(
        store::design::CLOSE_ICON,
        store::design::CLOSE_STROKE,
        &cross_lines(store::design::CLOSE_ICON),
        colour,
    )
}

/// The `+` at the end of the strip: 16px with a 1.6px stroke.
pub fn plus(colour: impl Into<Hsla>) -> AnyElement {
    let colour = colour.into();
    stroked(
        store::design::ADD_ICON,
        store::design::ADD_STROKE,
        &plus_lines(store::design::ADD_ICON),
        colour,
    )
}

/// The same two glyphs, from the mode's palette — the ink the design draws them
/// in unless a caller says otherwise.
pub fn cross_in(palette: &store::design::Palette) -> AnyElement {
    cross(color(palette.tab_ink))
}

pub fn plus_in(palette: &store::design::Palette) -> AnyElement {
    plus(color(palette.tab_ink))
}

/// A glyph's ink, for a caller that has a token rather than a colour.
pub fn ink_of(token: Rgb) -> Hsla {
    color(token)
}

/// The window a glyph is painted into, for a caller building its own.
pub type GlyphWindow = Window;

#[cfg(test)]
mod tests {
    use super::*;

    /// The `×` runs corner to corner of the design's 15%–85% box, and its two
    /// strokes cross in the middle.
    #[test]
    fn a_cross_is_two_diagonals_corner_to_corner() {
        let lines = cross_lines(10.);
        assert_eq!(lines.len(), 2);
        let (from, to) = (px(1.5), px(8.5));
        assert_eq!(lines[0], (point(from, from), point(to, to)));
        assert_eq!(lines[1], (point(to, from), point(from, to)));
        // They share the middle, which is where the design's strokes cross.
        let mid = px(5.);
        assert_eq!(lines[0].0.x + (lines[0].1.x - lines[0].0.x) / 2., mid);
        assert_eq!(lines[1].0.y + (lines[1].1.y - lines[1].0.y) / 2., mid);
    }

    /// The `+` is one stroke each way through the middle, at the design's own
    /// 1.5-to-12.5 of a 14-unit box, drawn 16px wide.
    #[test]
    fn a_plus_is_two_strokes_through_the_middle() {
        let lines = plus_lines(16.);
        assert_eq!(lines.len(), 2);
        let (from, to, mid) = (px(1.5 / 14. * 16.), px(12.5 / 14. * 16.), px(8.));
        assert_eq!(lines[0], (point(from, mid), point(to, mid)));
        assert_eq!(lines[1], (point(mid, from), point(mid, to)));
        // And it is symmetric, as the glyph is.
        assert_eq!(mid - lines[0].0.x, lines[0].1.x - mid);
    }

    /// A cap is a closed ring of `CAP_POINTS` points at the stroke's half-width,
    /// so a round cap is exactly as wide as the stroke it ends.
    #[test]
    fn a_cap_is_a_ring_the_width_of_the_stroke() {
        let points = cap_points(point(px(4.), px(4.)), 0.8);
        assert_eq!(points.len(), CAP_POINTS);
        for point in &points {
            let (dx, dy) = (f32::from(point.x) - 4., f32::from(point.y) - 4.);
            let radius = (dx * dx + dy * dy).sqrt();
            assert!((radius - 0.8).abs() < 1e-3, "{radius}");
        }
        // The design's own two widths.
        assert_eq!(store::design::CLOSE_STROKE, 1.3);
        assert_eq!(store::design::ADD_STROKE, 1.6);
        assert_eq!(store::design::CLOSE_ICON, 10.);
        assert_eq!(store::design::ADD_ICON, 16.);
    }

    /// Both glyphs build, in both modes' ink.
    #[test]
    fn both_glyphs_build() {
        let light = &store::design::LIGHT;
        let dark = &store::design::DARK;
        let _ = (
            cross_in(light),
            plus_in(light),
            cross_in(dark),
            plus_in(dark),
            cross(color(dark.fg)),
            plus(ink_of(light.fg)),
        );
    }
}
