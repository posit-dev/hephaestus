//! Polygon boolean clipping via `clipper2-rust`.
//!
//! Two operations sit alongside the polygon-offset wrapper in
//! [`super::offset`]: an intersect of two multi-ring closed areas
//! ([`intersect_polygons`]), and an intersect of a set of open polylines
//! against a multi-ring closed area ([`clip_polylines_to_polygon`]).
//!
//! Both follow the same shape conventions as
//! [`offset_polygon`](super::offset_polygon):
//! - Inputs and outputs in path coordinates (no implicit unit
//!   conversion).
//! - Closed polygons are multi-ring `&[&[Point]]` slices interpreted
//!   under `FillRule::EvenOdd` — a point inside an odd number of rings
//!   is inside the area.
//! - Outputs are `Vec<Vec<Point>>` — one ring per output for the
//!   polygon intersect, one polyline per output for the polyline
//!   clipper. Either may be empty when the inputs don't overlap.
//!
//! Used by `Projection::Custom` to trim a user-supplied outline against
//! the visible panel rect and to clip graticule polylines against the
//! resulting drawing surface.

use crate::geometry::Point;
use clipper2_rust::{
    intersect_d, ClipType, ClipperD, FillRule, PathD, PathsD, Point as ClipperPoint,
};

/// Precision (decimal places retained internally by clipper2). Four is
/// enough for sub-pixel accuracy at every render scale hephaestus
/// produces and matches clipper2's default recommendation.
const PRECISION: i32 = 4;

/// Intersect `subject` and `clip`, each treated as a multi-ring closed
/// area under EvenOdd. Returns the boundary rings of the intersection —
/// may include both outer rings and holes; empty `Vec` when the two
/// shapes don't overlap.
///
/// Both arguments are `&[&[Point]]`: the outer slice is the list of
/// rings, the inner slices are the per-ring vertex arrays. Same shape as
/// the input to [`super::offset_polygon`] and the output of
/// [`crate::primitives::path_to_rings`].
pub fn intersect_polygons(subject: &[&[Point]], clip: &[&[Point]]) -> Vec<Vec<Point>> {
    if subject.is_empty() || clip.is_empty() {
        return Vec::new();
    }
    let subject_paths = to_paths_d(subject);
    let clip_paths = to_paths_d(clip);
    let solution = intersect_d(&subject_paths, &clip_paths, FillRule::EvenOdd, PRECISION);
    paths_d_to_rings(&solution)
}

/// Intersect each open polyline with `polygon` (multi-ring closed area,
/// EvenOdd). Each input polyline becomes zero or more output polylines —
/// a line that crosses any ring boundary splits into the in-polygon
/// segments. A polyline that lies entirely outside the polygon
/// contributes nothing; one that lies entirely inside is preserved
/// verbatim (modulo clipper2's collinear-vertex cleanup).
///
/// Returned polylines retain their open status — no closing edge is
/// inserted.
pub fn clip_polylines_to_polygon(polylines: &[&[Point]], polygon: &[&[Point]]) -> Vec<Vec<Point>> {
    if polylines.is_empty() || polygon.is_empty() {
        return Vec::new();
    }
    let subjects: PathsD = polylines
        .iter()
        .filter(|line| line.len() >= 2)
        .map(|line| points_to_path_d(line))
        .collect();
    if subjects.is_empty() {
        return Vec::new();
    }
    let clips = to_paths_d(polygon);

    let mut clipper = ClipperD::new(PRECISION);
    clipper.add_open_subject(&subjects);
    clipper.add_clip(&clips);
    let mut closed_out = PathsD::new();
    let mut open_out = PathsD::new();
    clipper.execute(
        ClipType::Intersection,
        FillRule::EvenOdd,
        &mut closed_out,
        Some(&mut open_out),
    );
    // Open inputs yield open outputs; closed_out should be empty for
    // open-only subjects but we drop it anyway.
    paths_d_to_rings(&open_out)
}

fn to_paths_d(rings: &[&[Point]]) -> PathsD {
    rings
        .iter()
        .filter(|r| !r.is_empty())
        .map(|r| points_to_path_d(r))
        .collect()
}

fn points_to_path_d(points: &[Point]) -> PathD {
    let mut path = PathD::with_capacity(points.len());
    for p in points {
        path.push(ClipperPoint::<f64>::new(p.x, p.y));
    }
    path
}

fn paths_d_to_rings(paths: &PathsD) -> Vec<Vec<Point>> {
    let mut out = Vec::with_capacity(paths.len());
    for path in paths {
        let mut ring = Vec::with_capacity(path.len());
        for pt in path {
            ring.push(Point::new(pt.x, pt.y));
        }
        if !ring.is_empty() {
            out.push(ring);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(x: f64, y: f64) -> Point {
        Point::new(x, y)
    }

    fn bbox(ring: &[Point]) -> (f64, f64, f64, f64) {
        let mut x0 = f64::INFINITY;
        let mut x1 = f64::NEG_INFINITY;
        let mut y0 = f64::INFINITY;
        let mut y1 = f64::NEG_INFINITY;
        for p in ring {
            x0 = x0.min(p.x);
            x1 = x1.max(p.x);
            y0 = y0.min(p.y);
            y1 = y1.max(p.y);
        }
        (x0, y0, x1, y1)
    }

    // ── intersect_polygons ──

    #[test]
    fn intersect_overlapping_squares_yields_intersection_rect() {
        let a = [pt(0.0, 0.0), pt(10.0, 0.0), pt(10.0, 10.0), pt(0.0, 10.0)];
        let b = [pt(5.0, 5.0), pt(15.0, 5.0), pt(15.0, 15.0), pt(5.0, 15.0)];
        let out = intersect_polygons(&[&a], &[&b]);
        assert_eq!(out.len(), 1);
        let (x0, y0, x1, y1) = bbox(&out[0]);
        assert!((x0 - 5.0).abs() < 0.01);
        assert!((y0 - 5.0).abs() < 0.01);
        assert!((x1 - 10.0).abs() < 0.01);
        assert!((y1 - 10.0).abs() < 0.01);
    }

    #[test]
    fn intersect_disjoint_polygons_returns_empty() {
        let a = [pt(0.0, 0.0), pt(1.0, 0.0), pt(1.0, 1.0), pt(0.0, 1.0)];
        let b = [pt(5.0, 5.0), pt(6.0, 5.0), pt(6.0, 6.0), pt(5.0, 6.0)];
        let out = intersect_polygons(&[&a], &[&b]);
        assert!(
            out.is_empty(),
            "non-overlapping shapes intersect to nothing"
        );
    }

    #[test]
    fn intersect_preserves_hole_when_clip_covers_it() {
        // Outer square 0..10 with a 2..8 hole. Clip with a 1..9 square →
        // the hole stays inside (and the result is the clip minus the
        // hole — two rings under EvenOdd).
        let outer = [pt(0.0, 0.0), pt(10.0, 0.0), pt(10.0, 10.0), pt(0.0, 10.0)];
        let hole = [pt(2.0, 2.0), pt(8.0, 2.0), pt(8.0, 8.0), pt(2.0, 8.0)];
        let clip = [pt(1.0, 1.0), pt(9.0, 1.0), pt(9.0, 9.0), pt(1.0, 9.0)];
        let out = intersect_polygons(&[&outer, &hole], &[&clip]);
        assert_eq!(out.len(), 2, "outer band + preserved hole");
    }

    #[test]
    fn intersect_clip_lying_entirely_outside_subject_is_empty() {
        let outer = [pt(0.0, 0.0), pt(10.0, 0.0), pt(10.0, 10.0), pt(0.0, 10.0)];
        let clip = [pt(100.0, 100.0), pt(101.0, 100.0), pt(101.0, 101.0)];
        let out = intersect_polygons(&[&outer], &[&clip]);
        assert!(out.is_empty());
    }

    // ── clip_polylines_to_polygon ──

    #[test]
    fn polyline_inside_polygon_survives_intact() {
        let polygon = [pt(0.0, 0.0), pt(10.0, 0.0), pt(10.0, 10.0), pt(0.0, 10.0)];
        let line = [pt(2.0, 5.0), pt(8.0, 5.0)];
        let out = clip_polylines_to_polygon(&[&line], &[&polygon]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].len(), 2);
    }

    #[test]
    fn polyline_outside_polygon_is_dropped() {
        let polygon = [pt(0.0, 0.0), pt(1.0, 0.0), pt(1.0, 1.0), pt(0.0, 1.0)];
        let line = [pt(5.0, 5.0), pt(6.0, 6.0)];
        let out = clip_polylines_to_polygon(&[&line], &[&polygon]);
        assert!(out.is_empty());
    }

    #[test]
    fn polyline_crossing_boundary_is_trimmed_at_intersections() {
        // A horizontal line from x=-5 to x=15 crossing a unit square at
        // x in [0, 10]. Result: one polyline from (0, 5) to (10, 5).
        let polygon = [pt(0.0, 0.0), pt(10.0, 0.0), pt(10.0, 10.0), pt(0.0, 10.0)];
        let line = [pt(-5.0, 5.0), pt(15.0, 5.0)];
        let out = clip_polylines_to_polygon(&[&line], &[&polygon]);
        assert_eq!(out.len(), 1);
        let (x0, _y0, x1, _y1) = bbox(&out[0]);
        assert!((x0 - 0.0).abs() < 0.01);
        assert!((x1 - 10.0).abs() < 0.01);
    }

    #[test]
    fn polyline_passing_through_hole_splits() {
        // Polygon: outer 0..10, hole 4..6. A horizontal line through
        // y=5 spanning x=0..10 must produce two segments: x in [0, 4]
        // and x in [6, 10].
        let outer = [pt(0.0, 0.0), pt(10.0, 0.0), pt(10.0, 10.0), pt(0.0, 10.0)];
        let hole = [pt(4.0, 4.0), pt(6.0, 4.0), pt(6.0, 6.0), pt(4.0, 6.0)];
        let line = [pt(0.0, 5.0), pt(10.0, 5.0)];
        let out = clip_polylines_to_polygon(&[&line], &[&outer, &hole]);
        assert_eq!(out.len(), 2, "line split by the hole");
    }
}
