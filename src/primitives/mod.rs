//! Compound 2D primitives that sit at the boundary between the low- and
//! high-level scene APIs.
//!
//! Both functions in this module produce a [`crate::path::Path`] — drawing is
//! the caller's responsibility (same pattern as [`crate::shape`]). The caller
//! issues a [`SceneBuilder::fill`](crate::scene::SceneBuilder::fill) or
//! [`SceneBuilder::stroke`](crate::scene::SceneBuilder::stroke) with the
//! returned path, its own brush, transform, and `PickId`.
//!
//! Every constructor takes geometric inputs and returns a `Path`. Geometry
//! transforms ([`round_corners`], [`clip_polyline`], [`offset_polygon`]) are
//! separate, composable functions — the constructors don't bake them in.
//!
//! **Path-emitting constructors:**
//! - [`polyline`] — open polyline from a point list, with optional end
//!   trimming via [`EndClip`].
//! - [`polygon`] — closed polygon from one outer ring plus zero or more
//!   holes, with optional signed offset.
//! - [`rect`], [`rounded_rect`], [`circle`], [`ellipse`] — thin wrappers
//!   over the equivalent `kurbo` shapes that hide the path-approximation
//!   tolerance and the `kurbo::Shape` import.
//! - [`segment`] — 2-point line shorthand.
//! - [`regular_polygon`] — n-sided regular polygon. Use
//!   [`regular_polygon_vertices`] when you want the raw vertices for further
//!   composition.
//! - [`arc`] — open circular arc.
//! - [`wedge`], [`annular_wedge`] — closed pie / donut slices.
//!
//! **Vertex transforms** (compose by feeding their output to a constructor):
//! - [`clip_polyline`] — trim a polyline's start/end against a shape.
//!   Returns `Vec<Point>`.
//! - [`offset_polygon`] — inflate/deflate a polygon's rings via Clipper2.
//!   Returns `Vec<Vec<Point>>`.
//! - [`path_to_rings`] — flatten any `Path` (lines + quads + cubics) into
//!   piecewise-linear ring vertices. Lets you pipe curved primitives
//!   ([`wedge`], [`circle`], etc.) through [`offset_polygon`].
//! - [`round_corners`] — Chaikin-style adaptive corner cutting on a vertex
//!   sequence. Returns a `Path` with one cubic Bezier per rounded corner;
//!   the cubic's control points are placed along the **local segment
//!   tangents** at each cut point, which makes the join tangent-continuous
//!   even when the cut walked through near-collinear intermediate vertices.
//!   In the strict-collinear case the two controls collapse to the original
//!   corner vertex (recovering the classical Chaikin quadratic limit
//!   curve). Piecewise-linear input only.
//! - [`round_path_corners`] — the curve-aware variant: takes any `Path` and
//!   replaces each eligible join with a cubic Bezier fillet whose endpoint
//!   tangents match the original segments. Use this for `wedge`,
//!   `annular_wedge`, or any path whose edges include quads / cubics.
//!
//! **Path sampling:**
//! - [`ArcLengthWalker`] — yields position + tangent samples at fixed
//!   arc-length intervals along a path. Used by LineGeom's linetype
//!   marker emission and by future text-on-path geoms.
//!
//! Corner rounding is an adaptive port of the algorithm from the
//! [`boundaries`](https://github.com/thomasp85/boundaries) R package. Each
//! eligible corner is replaced with a tangent-aware cubic Bezier — see
//! [`round_corners`] for the math. Polygon offset is delegated to
//! [`clipper2-rust`].

use crate::geometry::{Point, Rect, Vec2};
use crate::path::{Path, PathEl};
use kurbo::Shape;

mod arc_length;
mod corner;
mod end_clip;
mod offset;
mod path_corner;
mod ribbon;

pub use arc_length::{ArcLengthWalker, ArcSample, PolylineSampler, TrailingPolicy};
pub use corner::round_corners;
pub use end_clip::{clip_polyline, EndClip};
pub use offset::offset_polygon;
pub use path_corner::round_path_corners;
pub use ribbon::{
    polyline_gradient, polyline_ribbon, polyline_ribbon_full, RibbonCap, RibbonJoin, RibbonOptions,
};

/// Path-approximation tolerance for curved primitives ([`circle`],
/// [`ellipse`], [`rounded_rect`]). Smaller values produce more vertices.
const CURVE_TOLERANCE: f64 = 0.1;

/// Configuration for the adaptive corner-rounding pass.
///
/// `max_angle_deg` does double duty:
/// - Interior angle greater than this → the vertex is *not* rounded.
/// - Interior angle within tolerance of 180° (or 0°) → the vertex is collinear
///   and the back/forward walks for adjacent corners pass through it.
///
/// A cut never extends past the next eligible corner; when both ends of a
/// straight run are eligible corners, each side gets at most half the run so
/// the two corners share the available space. At a polyline endpoint there's
/// no neighbour to share with, so the cut can use the full distance.
#[derive(Debug, Clone, Copy)]
pub struct CornerRounding {
    /// Skip corners whose interior angle (in degrees) is larger than this.
    /// `f64::INFINITY` (the default) rounds every non-collinear corner.
    pub max_angle_deg: f64,
    /// Maximum cut distance from a corner vertex, in path coordinates.
    /// `f64::INFINITY` (the default) lets the half-share rule decide.
    pub max_cut: f64,
}

impl Default for CornerRounding {
    fn default() -> Self {
        Self {
            max_angle_deg: f64::INFINITY,
            max_cut: f64::INFINITY,
        }
    }
}

/// Options for [`polyline`].
#[derive(Debug, Clone, Copy, Default)]
pub struct PolylineOptions {
    /// Clip the start of the polyline against this shape. The polyline is
    /// trimmed at the first segment that exits the shape, but only when
    /// `points[0]` lies inside the shape.
    pub clip_start: Option<EndClip>,
    /// Same as `clip_start`, applied at the end of the polyline.
    pub clip_end: Option<EndClip>,
}

/// Options for [`polygon`].
#[derive(Debug, Clone, Copy)]
pub struct PolygonOptions {
    /// Signed offset distance. `> 0` expands outward, `< 0` contracts inward.
    /// Holes are offset in the opposite direction automatically.
    pub offset: f64,
    /// Miter clamp ratio passed to Clipper2. Defaults to `4.0` (matches SVG's
    /// default `stroke-miterlimit`). Only applies when `offset != 0.0`.
    pub miter_limit: f64,
}

impl Default for PolygonOptions {
    fn default() -> Self {
        Self {
            offset: 0.0,
            miter_limit: 4.0,
        }
    }
}

/// Build a polyline path from a sequence of points, applying optional end
/// clipping. To round corners on the result, feed the clipped vertices
/// through [`round_corners`] explicitly — pull the vertices from
/// [`clip_polyline`] rather than re-clipping a `Path`.
///
/// Returns an empty path when `points.len() < 2` or when both end clips
/// remove every segment.
pub fn polyline(points: &[Point], opts: PolylineOptions) -> Path {
    if points.len() < 2 {
        return Path::new();
    }
    let pts = clip_polyline(points, opts.clip_start, opts.clip_end);
    if pts.len() < 2 {
        return Path::new();
    }
    end_clip::polyline_path(&pts)
}

/// Build a polygon path from one outer ring and zero or more holes.
///
/// `rings[0]` is the outer boundary; `rings[1..]` are holes. Winding doesn't
/// matter — the wrapper normalises before handing to Clipper2. An empty path
/// is returned when `rings` is empty, when the outer ring has fewer than 3
/// vertices, or when an inward offset collapses the polygon entirely.
///
/// When `opts.offset != 0.0`, polygon offset is delegated to `clipper2-rust`
/// with `JoinType::Miter`. Output may contain more rings than input (an inset
/// can split a "dumbbell") or fewer (a hole may collapse). Each output ring
/// becomes one subpath in the resulting `Path`.
///
/// To round the corners of an offset polygon, call [`offset_polygon`]
/// directly to get the rings, then pipe each ring through [`round_corners`]
/// before assembling the path.
pub fn polygon(rings: &[&[Point]], opts: PolygonOptions) -> Path {
    if rings.is_empty() || rings[0].len() < 3 {
        return Path::new();
    }
    let offset_rings: Vec<Vec<Point>> = if opts.offset == 0.0 {
        rings
            .iter()
            .filter(|r| r.len() >= 3)
            .map(|r| r.to_vec())
            .collect()
    } else {
        offset_polygon(rings, opts.offset, opts.miter_limit)
    };

    let mut path = Path::new();
    for ring in &offset_rings {
        if ring.len() < 3 {
            continue;
        }
        let sub = plain_polygon_path(ring);
        for el in sub.iter() {
            path.push(el);
        }
    }
    path
}

fn plain_polygon_path(ring: &[Point]) -> Path {
    let mut p = Path::new();
    p.move_to(ring[0]);
    for v in &ring[1..] {
        p.line_to(*v);
    }
    p.close_path();
    p
}

/// Flatten any `Path` into one `Vec<Point>` per subpath, replacing every
/// quadratic and cubic Bezier with line segments small enough that no segment
/// deviates from the original curve by more than `tolerance` units.
///
/// Returns one inner `Vec` per subpath: a single open polyline contributes
/// one ring; a closed polygon contributes one ring; a polygon with holes
/// contributes one ring per `MoveTo` / `ClosePath` cycle. The `ClosePath`
/// itself is *not* emitted as a duplicate vertex — the first and last
/// vertices of a closed ring are guaranteed distinct (subject to numerical
/// precision).
///
/// Useful as glue between curved constructors ([`wedge`], [`circle`],
/// [`rounded_rect`], etc.) and vertex-only transforms like
/// [`offset_polygon`] or [`round_corners`].
pub fn path_to_rings(path: &Path, tolerance: f64) -> Vec<Vec<Point>> {
    let mut rings: Vec<Vec<Point>> = Vec::new();
    let mut cur: Vec<Point> = Vec::new();
    kurbo::flatten(path.iter(), tolerance, |el| match el {
        PathEl::MoveTo(p) if !cur.is_empty() => {
            rings.push(std::mem::take(&mut cur));
            cur.push(p);
        }
        PathEl::MoveTo(p) => cur.push(p),
        PathEl::LineTo(p) => cur.push(p),
        PathEl::ClosePath if !cur.is_empty() => {
            rings.push(std::mem::take(&mut cur));
        }
        // kurbo::flatten only emits MoveTo / LineTo / ClosePath.
        _ => {}
    });
    if !cur.is_empty() {
        rings.push(cur);
    }
    rings
}

/// Build a closed axis-aligned rectangle path. Pure line segments — no
/// curve approximation.
pub fn rect(r: Rect) -> Path {
    r.to_path(CURVE_TOLERANCE)
}

/// Build a closed rectangle with uniformly-rounded corners (SVG-style
/// quarter-circle arcs at each corner). `radius` is clamped by `kurbo` to at
/// most half the shorter rectangle side.
pub fn rounded_rect(r: Rect, radius: f64) -> Path {
    kurbo::RoundedRect::from_rect(r, radius).to_path(CURVE_TOLERANCE)
}

/// Build a closed circle path approximated by cubic Beziers.
pub fn circle(center: Point, radius: f64) -> Path {
    kurbo::Circle::new(center, radius).to_path(CURVE_TOLERANCE)
}

/// Build a closed axis-aligned ellipse path. `radii.x` is the x-half-axis,
/// `radii.y` the y-half-axis.
pub fn ellipse(center: Point, radii: Vec2) -> Path {
    kurbo::Ellipse::new(center, radii, 0.0).to_path(CURVE_TOLERANCE)
}

/// A single line segment from `a` to `b`. Same as a 2-point [`polyline`] with
/// no options.
pub fn segment(a: Point, b: Point) -> Path {
    let mut p = Path::new();
    p.move_to(a);
    p.line_to(b);
    p
}

/// Build a closed regular polygon centred at `center` with the given
/// circumradius and number of sides. The first vertex sits at
/// `(center.x + circumradius, center.y)`; for a different orientation, either
/// post-multiply by a rotation [`Affine`](crate::geometry::Affine) when
/// drawing or build vertices via [`regular_polygon_vertices`] and rotate them
/// before handing to [`polygon`].
///
/// A regular polygon is one ring by definition — there's no "hole" parameter
/// here. If you want a regular outer ring with one or more holes, call
/// [`regular_polygon_vertices`] for the outer and feed the result into
/// [`polygon`] alongside your hole rings:
///
/// ```ignore
/// let outer = regular_polygon_vertices(Point::ORIGIN, 100.0, 6);
/// let hole  = [/* ... */];
/// let path  = polygon(&[&outer, &hole], PolygonOptions::default());
/// ```
///
/// Returns an empty path when `n_sides < 3`.
pub fn regular_polygon(center: Point, circumradius: f64, n_sides: usize) -> Path {
    let verts = regular_polygon_vertices(center, circumradius, n_sides);
    if verts.is_empty() {
        return Path::new();
    }
    plain_polygon_path(&verts)
}

/// Vertices of a regular n-sided polygon, in CCW order starting at the
/// `+x` direction from `center`. Useful when you want to compose with
/// [`polygon`] for holes, offset, or corner rounding.
///
/// Returns an empty `Vec` when `n_sides < 3`.
pub fn regular_polygon_vertices(center: Point, circumradius: f64, n_sides: usize) -> Vec<Point> {
    if n_sides < 3 {
        return Vec::new();
    }
    let mut v = Vec::with_capacity(n_sides);
    let step = std::f64::consts::TAU / n_sides as f64;
    for i in 0..n_sides {
        let a = step * i as f64;
        v.push(Point::new(
            center.x + circumradius * a.cos(),
            center.y + circumradius * a.sin(),
        ));
    }
    v
}

fn point_on_circle(center: Point, radius: f64, angle: f64) -> Point {
    Point::new(
        center.x + radius * angle.cos(),
        center.y + radius * angle.sin(),
    )
}

/// An **open** circular arc centred at `center`, with the given `radius`,
/// spanning from `start_angle` through `sweep_angle` radians.
///
/// Angles use the mathematical convention: `0` is along `+x`, increasing
/// angle is counter-clockwise in math space (which appears clockwise in
/// screen coordinates with Y pointing down). Negative `sweep_angle` reverses
/// the arc direction.
pub fn arc(center: Point, radius: f64, start_angle: f64, sweep_angle: f64) -> Path {
    let arc = kurbo::Arc::new(
        center,
        Vec2::new(radius, radius),
        start_angle,
        sweep_angle,
        0.0,
    );
    arc.to_path(CURVE_TOLERANCE)
}

/// A **closed** circular wedge (pie slice): two radial segments from `center`
/// to the arc endpoints, joined by an arc of `radius` running from
/// `start_angle` through `sweep_angle` radians. See [`arc`] for the angle
/// convention.
pub fn wedge(center: Point, radius: f64, start_angle: f64, sweep_angle: f64) -> Path {
    let arc_start = point_on_circle(center, radius, start_angle);
    let mut path = Path::new();
    path.move_to(center);
    path.line_to(arc_start);
    let arc = kurbo::Arc::new(
        center,
        Vec2::new(radius, radius),
        start_angle,
        sweep_angle,
        0.0,
    );
    for el in arc.append_iter(CURVE_TOLERANCE) {
        path.push(el);
    }
    path.close_path();
    path
}

/// A **closed** annular wedge (donut slice): two radial segments at
/// `start_angle` and `start_angle + sweep_angle`, joining an outer arc at
/// `outer_radius` and an inner arc at `inner_radius`. See [`arc`] for the
/// angle convention. Use `inner_radius == 0.0` to recover a [`wedge`].
pub fn annular_wedge(
    center: Point,
    inner_radius: f64,
    outer_radius: f64,
    start_angle: f64,
    sweep_angle: f64,
) -> Path {
    let outer_start = point_on_circle(center, outer_radius, start_angle);
    let inner_end = point_on_circle(center, inner_radius, start_angle + sweep_angle);
    let mut path = Path::new();
    path.move_to(outer_start);
    let outer_arc = kurbo::Arc::new(
        center,
        Vec2::new(outer_radius, outer_radius),
        start_angle,
        sweep_angle,
        0.0,
    );
    for el in outer_arc.append_iter(CURVE_TOLERANCE) {
        path.push(el);
    }
    path.line_to(inner_end);
    let inner_arc = kurbo::Arc::new(
        center,
        Vec2::new(inner_radius, inner_radius),
        start_angle + sweep_angle,
        -sweep_angle,
        0.0,
    );
    for el in inner_arc.append_iter(CURVE_TOLERANCE) {
        path.push(el);
    }
    path.close_path();
    path
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Rect;
    use crate::path::PathEl;

    fn pt(x: f64, y: f64) -> Point {
        Point::new(x, y)
    }

    #[test]
    fn polyline_end_clipped() {
        // Connector from inside circle A to inside rect B, with a bend in the middle.
        let pts = [pt(0.0, 0.0), pt(5.0, 0.0), pt(5.0, 5.0), pt(10.0, 5.0)];
        let opts = PolylineOptions {
            clip_start: Some(EndClip::Circle {
                center: Point::ORIGIN,
                radius: 1.0,
            }),
            clip_end: Some(EndClip::Rect(Rect::new(9.0, 4.0, 11.0, 6.0))),
        };
        let path = polyline(&pts, opts);
        let mut last_pen: Option<Point> = None;
        for el in path.elements() {
            match el {
                PathEl::MoveTo(p) | PathEl::LineTo(p) => last_pen = Some(*p),
                _ => {}
            }
        }
        let first = path
            .elements()
            .iter()
            .find_map(|el| {
                if let PathEl::MoveTo(p) = el {
                    Some(*p)
                } else {
                    None
                }
            })
            .expect("move_to");
        assert!((first - Point::ORIGIN).hypot() <= 1.0 + 1e-6);
        let last = last_pen.expect("last point");
        assert!((last.x - 9.0).abs() < 1e-6);
    }

    #[test]
    fn polyline_then_round_corners_composes() {
        // Same setup as above, but compose: clip with clip_polyline, then round.
        let pts = [pt(0.0, 0.0), pt(5.0, 0.0), pt(5.0, 5.0), pt(10.0, 5.0)];
        let clipped = clip_polyline(
            &pts,
            Some(EndClip::Circle {
                center: Point::ORIGIN,
                radius: 1.0,
            }),
            Some(EndClip::Rect(Rect::new(9.0, 4.0, 11.0, 6.0))),
        );
        let path = round_corners(&clipped, false, CornerRounding::default());
        let has_curve = path
            .elements()
            .iter()
            .any(|el| matches!(el, PathEl::CurveTo(_, _, _)));
        assert!(has_curve);
    }

    #[test]
    fn polygon_with_hole_and_offset() {
        let outer = [pt(0.0, 0.0), pt(10.0, 0.0), pt(10.0, 10.0), pt(0.0, 10.0)];
        let hole = [pt(3.0, 3.0), pt(7.0, 3.0), pt(7.0, 7.0), pt(3.0, 7.0)];
        let rings: [&[Point]; 2] = [&outer, &hole];
        let opts = PolygonOptions {
            offset: 1.0,
            ..PolygonOptions::default()
        };
        let path = polygon(&rings, opts);
        let move_count = path
            .elements()
            .iter()
            .filter(|el| matches!(el, PathEl::MoveTo(_)))
            .count();
        let close_count = path
            .elements()
            .iter()
            .filter(|el| matches!(el, PathEl::ClosePath))
            .count();
        assert_eq!(move_count, 2, "outer + hole");
        assert_eq!(close_count, 2);
    }

    #[test]
    fn polygon_no_offset_returns_input_geometry() {
        let sq = [pt(0.0, 0.0), pt(1.0, 0.0), pt(1.0, 1.0), pt(0.0, 1.0)];
        let path = polygon(&[&sq], PolygonOptions::default());
        // 1 move + 3 lines + 1 close.
        let (m, l, q, c) = count_elements(&path);
        assert_eq!(m, 1);
        assert_eq!(l, 3);
        assert_eq!(q, 0);
        assert_eq!(c, 1);
    }

    #[test]
    fn offset_then_round_composition() {
        let sq = [pt(0.0, 0.0), pt(10.0, 0.0), pt(10.0, 10.0), pt(0.0, 10.0)];
        let rings = offset_polygon(&[&sq], 2.0, 4.0);
        let mut path = Path::new();
        for r in &rings {
            let sub = round_corners(r, true, CornerRounding::default());
            for el in sub.iter() {
                path.push(el);
            }
        }
        let curves = path
            .elements()
            .iter()
            .filter(|el| matches!(el, PathEl::CurveTo(_, _, _)))
            .count();
        assert!(
            curves >= 4,
            "rounded inflated square should have at least 4 cubics"
        );
    }

    #[test]
    fn polyline_returns_empty_when_under_two_points() {
        assert_eq!(
            polyline(&[], PolylineOptions::default()).elements().len(),
            0
        );
        assert_eq!(
            polyline(&[pt(0.0, 0.0)], PolylineOptions::default())
                .elements()
                .len(),
            0,
        );
    }

    #[test]
    fn polygon_returns_empty_when_outer_too_small() {
        let r: [&[Point]; 1] = [&[pt(0.0, 0.0), pt(1.0, 0.0)]];
        assert_eq!(polygon(&r, PolygonOptions::default()).elements().len(), 0);
    }

    #[test]
    fn shape_constructors_produce_non_empty_paths() {
        let r = rect(Rect::new(0.0, 0.0, 10.0, 5.0));
        let rr = rounded_rect(Rect::new(0.0, 0.0, 10.0, 5.0), 1.0);
        let c = circle(Point::new(0.0, 0.0), 5.0);
        let e = ellipse(Point::new(0.0, 0.0), Vec2::new(5.0, 3.0));
        for p in [&r, &rr, &c, &e] {
            assert!(matches!(p.elements().first(), Some(PathEl::MoveTo(_))));
            assert!(
                p.elements().len() > 1,
                "primitive should have drawing elements past the move_to",
            );
        }
    }

    #[test]
    fn rect_bounds_match_input() {
        let r = Rect::new(1.0, 2.0, 10.0, 8.0);
        let p = rect(r);
        let b = kurbo::Shape::bounding_box(&p);
        assert!((b.x0 - 1.0).abs() < 1e-9);
        assert!((b.y0 - 2.0).abs() < 1e-9);
        assert!((b.x1 - 10.0).abs() < 1e-9);
        assert!((b.y1 - 8.0).abs() < 1e-9);
    }

    #[test]
    fn circle_bounds_match_input() {
        let p = circle(Point::new(10.0, 20.0), 5.0);
        let b = kurbo::Shape::bounding_box(&p);
        assert!((b.x0 - 5.0).abs() < 0.5);
        assert!((b.y0 - 15.0).abs() < 0.5);
        assert!((b.x1 - 15.0).abs() < 0.5);
        assert!((b.y1 - 25.0).abs() < 0.5);
    }

    #[test]
    fn segment_is_move_then_line() {
        let p = segment(Point::new(1.0, 2.0), Point::new(3.0, 4.0));
        let els: Vec<_> = p.elements().to_vec();
        assert_eq!(els.len(), 2);
        assert!(matches!(els[0], PathEl::MoveTo(p) if p == Point::new(1.0, 2.0)));
        assert!(matches!(els[1], PathEl::LineTo(p) if p == Point::new(3.0, 4.0)));
    }

    #[test]
    fn regular_polygon_vertex_count_matches_n_sides() {
        for n in [3, 4, 5, 6, 8, 12] {
            let v = regular_polygon_vertices(Point::ORIGIN, 1.0, n);
            assert_eq!(v.len(), n);
        }
    }

    #[test]
    fn regular_polygon_first_vertex_at_plus_x() {
        let v = regular_polygon_vertices(Point::new(10.0, 20.0), 5.0, 6);
        assert!((v[0].x - 15.0).abs() < 1e-9);
        assert!((v[0].y - 20.0).abs() < 1e-9);
    }

    #[test]
    fn regular_polygon_empty_below_three_sides() {
        assert!(regular_polygon_vertices(Point::ORIGIN, 1.0, 2).is_empty());
        assert_eq!(regular_polygon(Point::ORIGIN, 1.0, 1).elements().len(), 0);
    }

    #[test]
    fn arc_starts_at_expected_point() {
        let path = arc(Point::new(10.0, 0.0), 5.0, 0.0, std::f64::consts::PI);
        let first = path
            .elements()
            .iter()
            .find_map(|el| {
                if let PathEl::MoveTo(p) = el {
                    Some(*p)
                } else {
                    None
                }
            })
            .expect("move_to");
        // start_angle = 0 → start at center + (radius, 0) = (15, 0).
        assert!((first.x - 15.0).abs() < 1e-9 && first.y.abs() < 1e-9);
    }

    #[test]
    fn wedge_starts_at_center() {
        let path = wedge(Point::new(2.0, 3.0), 4.0, 0.0, std::f64::consts::PI / 2.0);
        let first = path
            .elements()
            .iter()
            .find_map(|el| {
                if let PathEl::MoveTo(p) = el {
                    Some(*p)
                } else {
                    None
                }
            })
            .expect("move_to");
        assert_eq!(first, Point::new(2.0, 3.0));
        assert!(path
            .elements()
            .iter()
            .any(|el| matches!(el, PathEl::ClosePath)));
    }

    #[test]
    fn annular_wedge_zero_inner_matches_wedge_bounds() {
        let aw = annular_wedge(Point::ORIGIN, 0.0, 5.0, 0.0, std::f64::consts::PI / 2.0);
        let w = wedge(Point::ORIGIN, 5.0, 0.0, std::f64::consts::PI / 2.0);
        let a = kurbo::Shape::bounding_box(&aw);
        let b = kurbo::Shape::bounding_box(&w);
        assert!((a.x0 - b.x0).abs() < 0.1);
        assert!((a.y0 - b.y0).abs() < 0.1);
        assert!((a.x1 - b.x1).abs() < 0.1);
        assert!((a.y1 - b.y1).abs() < 0.1);
    }

    #[test]
    fn full_arc_bounds_match_circle() {
        let a = arc(Point::new(0.0, 0.0), 5.0, 0.0, std::f64::consts::TAU);
        let b = kurbo::Shape::bounding_box(&a);
        assert!((b.x0 - (-5.0)).abs() < 0.5);
        assert!((b.x1 - 5.0).abs() < 0.5);
        assert!((b.y0 - (-5.0)).abs() < 0.5);
        assert!((b.y1 - 5.0).abs() < 0.5);
    }

    #[test]
    fn path_to_rings_flattens_polygon_unchanged() {
        let sq = [pt(0.0, 0.0), pt(1.0, 0.0), pt(1.0, 1.0), pt(0.0, 1.0)];
        let p = polygon(&[&sq], PolygonOptions::default());
        let rings = path_to_rings(&p, 0.1);
        assert_eq!(rings.len(), 1);
        assert_eq!(rings[0].len(), 4);
        for (a, b) in rings[0].iter().zip(sq.iter()) {
            assert!((a.x - b.x).abs() < 1e-9 && (a.y - b.y).abs() < 1e-9);
        }
    }

    #[test]
    fn path_to_rings_flattens_circle_into_many_points() {
        let c = circle(Point::ORIGIN, 10.0);
        let rings = path_to_rings(&c, 0.1);
        assert_eq!(rings.len(), 1);
        assert!(rings[0].len() > 8, "circle should flatten to many vertices");
    }

    #[test]
    fn path_to_rings_separates_multi_subpath() {
        let outer = [pt(0.0, 0.0), pt(10.0, 0.0), pt(10.0, 10.0), pt(0.0, 10.0)];
        let hole = [pt(3.0, 3.0), pt(7.0, 3.0), pt(7.0, 7.0), pt(3.0, 7.0)];
        let p = polygon(&[&outer, &hole], PolygonOptions::default());
        let rings = path_to_rings(&p, 0.1);
        assert_eq!(rings.len(), 2);
    }

    #[test]
    fn path_to_rings_handles_open_polyline() {
        let mut p = Path::new();
        p.move_to(pt(0.0, 0.0));
        p.line_to(pt(1.0, 0.0));
        p.line_to(pt(1.0, 1.0));
        // no close_path
        let rings = path_to_rings(&p, 0.1);
        assert_eq!(rings.len(), 1);
        assert_eq!(rings[0].len(), 3);
    }

    #[test]
    fn path_to_rings_then_offset_inflates_a_circle() {
        // Curved primitive in → polygon offset → curved result back as polygon.
        let c = circle(Point::ORIGIN, 10.0);
        let rings = path_to_rings(&c, 0.5);
        let refs: Vec<&[Point]> = rings.iter().map(Vec::as_slice).collect();
        let out = offset_polygon(&refs, 5.0, 4.0);
        assert_eq!(out.len(), 1);
        // Inflated circle has radius ≈ 15. Verify by bounding box.
        let (mut x0, mut x1, mut y0, mut y1) = (
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
        );
        for p in &out[0] {
            x0 = x0.min(p.x);
            x1 = x1.max(p.x);
            y0 = y0.min(p.y);
            y1 = y1.max(p.y);
        }
        assert!((x0 - (-15.0)).abs() < 0.5);
        assert!((x1 - 15.0).abs() < 0.5);
        assert!((y0 - (-15.0)).abs() < 0.5);
        assert!((y1 - 15.0).abs() < 0.5);
    }

    #[test]
    fn round_corners_works_on_externally_built_vertex_sequence() {
        let verts = regular_polygon_vertices(Point::ORIGIN, 10.0, 6);
        let path = round_corners(&verts, true, CornerRounding::default());
        let curves = path
            .elements()
            .iter()
            .filter(|el| matches!(el, PathEl::CurveTo(_, _, _)))
            .count();
        assert_eq!(curves, 6, "one cubic per corner of the hexagon");
    }

    #[test]
    fn regular_polygon_composes_with_polygon_for_holes() {
        // Hexagonal annulus: regular hexagon outer + smaller hexagon hole.
        let outer = regular_polygon_vertices(Point::ORIGIN, 10.0, 6);
        let hole = regular_polygon_vertices(Point::ORIGIN, 4.0, 6);
        let path = polygon(&[&outer, &hole], PolygonOptions::default());
        let move_count = path
            .elements()
            .iter()
            .filter(|el| matches!(el, PathEl::MoveTo(_)))
            .count();
        assert_eq!(move_count, 2, "outer + hole");
    }

    fn count_elements(path: &Path) -> (usize, usize, usize, usize) {
        let mut m = 0;
        let mut l = 0;
        let mut q = 0;
        let mut c = 0;
        for el in path.elements() {
            match el {
                PathEl::MoveTo(_) => m += 1,
                PathEl::LineTo(_) => l += 1,
                PathEl::QuadTo(_, _) => q += 1,
                PathEl::ClosePath => c += 1,
                _ => {}
            }
        }
        (m, l, q, c)
    }
}
