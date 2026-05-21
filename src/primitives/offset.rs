//! Polygon offset (inflate / deflate) via a thin wrapper over `clipper2-rust`.
//!
//! Clipper2 handles topology natively: holes (resolved by ring winding),
//! self-intersection cleanup, and the case where an inset collapses or splits
//! the polygon into multiple output rings. The wrapper:
//!
//! 1. Pre-normalises winding so the outer ring has positive signed area and
//!    every hole has negative signed area — Clipper2's convention for "outer
//!    vs hole" detection. Users may pass either winding.
//! 2. Calls `inflate_paths_d` with `JoinType::Miter` and `EndType::Polygon`.
//!    Corners are rounded by our own Chaikin pass downstream (`super::corner`)
//!    rather than by Clipper2's `Round` joiner — this keeps the rounding
//!    consistent across the polyline and polygon primitives.
//! 3. Returns one `Vec<Point>` per output ring. Each ring's winding follows
//!    Clipper2 (positive area = outer, negative = hole), which renders
//!    correctly under both `NonZero` and `EvenOdd` fill rules.

use crate::geometry::Point;
use clipper2_rust::{inflate_paths_d, EndType, JoinType, PathD, PathsD, Point as ClipperPoint};

/// Inflate / deflate a polygon's rings via Clipper2, returning the new ring
/// vertices. `rings[0]` is the outer boundary; `rings[1..]` are holes.
/// Winding doesn't matter — the wrapper normalises before handing to
/// Clipper2.
///
/// Output may contain more rings than input (an inset can split a
/// "dumbbell") or fewer (a hole may collapse, or the whole polygon may
/// vanish). Each output ring follows Clipper2's convention: positive signed
/// area for outers, negative for holes — which renders correctly under both
/// `NonZero` and `EvenOdd` fill rules.
///
/// Use this when you want to compose with [`round_corners`](super::round_corners)
/// per ring; for a plain inflated path, just call [`polygon`](super::polygon)
/// with a non-zero `offset` instead.
pub fn offset_polygon(rings: &[&[Point]], offset: f64, miter_limit: f64) -> Vec<Vec<Point>> {
    if rings.is_empty() || rings[0].len() < 3 {
        return Vec::new();
    }
    let outer_area = signed_area(rings[0]);
    let outer_pos = outer_area >= 0.0;

    let mut paths: PathsD = PathsD::new();
    paths.push(to_clipper(rings[0], !outer_pos));
    for hole in &rings[1..] {
        if hole.len() < 3 {
            continue;
        }
        let hole_pos = signed_area(hole) >= 0.0;
        paths.push(to_clipper(hole, hole_pos));
    }

    let inflated = inflate_paths_d(
        &paths,
        offset,
        JoinType::Miter,
        EndType::Polygon,
        miter_limit,
        2,
        0.0,
    );

    let mut out_rings = Vec::with_capacity(inflated.len());
    for ring in &inflated {
        let mut converted = Vec::with_capacity(ring.len());
        for pt in ring {
            converted.push(Point::new(pt.x, pt.y));
        }
        if converted.len() >= 3 {
            out_rings.push(converted);
        }
    }
    out_rings
}

fn to_clipper(ring: &[Point], reverse: bool) -> PathD {
    let mut path: PathD = PathD::with_capacity(ring.len());
    if reverse {
        for p in ring.iter().rev() {
            path.push(ClipperPoint::<f64>::new(p.x, p.y));
        }
    } else {
        for p in ring {
            path.push(ClipperPoint::<f64>::new(p.x, p.y));
        }
    }
    path
}

fn signed_area(ring: &[Point]) -> f64 {
    let n = ring.len();
    if n < 3 {
        return 0.0;
    }
    let mut sum = 0.0;
    for i in 0..n {
        let j = (i + 1) % n;
        sum += ring[i].x * ring[j].y - ring[j].x * ring[i].y;
    }
    sum * 0.5
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

    #[test]
    fn square_outward_offset() {
        let sq = [pt(0.0, 0.0), pt(1.0, 0.0), pt(1.0, 1.0), pt(0.0, 1.0)];
        let out = offset_polygon(&[&sq], 0.5, 4.0);
        assert_eq!(out.len(), 1);
        let (x0, y0, x1, y1) = bbox(&out[0]);
        assert!((x0 - (-0.5)).abs() < 0.01);
        assert!((y0 - (-0.5)).abs() < 0.01);
        assert!((x1 - 1.5).abs() < 0.01);
        assert!((y1 - 1.5).abs() < 0.01);
    }

    #[test]
    fn square_inward_offset() {
        let sq = [pt(0.0, 0.0), pt(1.0, 0.0), pt(1.0, 1.0), pt(0.0, 1.0)];
        let out = offset_polygon(&[&sq], -0.25, 4.0);
        assert_eq!(out.len(), 1);
        let (x0, y0, x1, y1) = bbox(&out[0]);
        assert!((x0 - 0.25).abs() < 0.01);
        assert!((y0 - 0.25).abs() < 0.01);
        assert!((x1 - 0.75).abs() < 0.01);
        assert!((y1 - 0.75).abs() < 0.01);
    }

    #[test]
    fn winding_normalised() {
        let ccw = [pt(0.0, 0.0), pt(1.0, 0.0), pt(1.0, 1.0), pt(0.0, 1.0)];
        let mut cw = ccw;
        cw.reverse();
        let from_ccw = offset_polygon(&[&ccw], 0.5, 4.0);
        let from_cw = offset_polygon(&[&cw], 0.5, 4.0);
        assert_eq!(from_ccw.len(), 1);
        assert_eq!(from_cw.len(), 1);
        let a = bbox(&from_ccw[0]);
        let b = bbox(&from_cw[0]);
        assert!((a.0 - b.0).abs() < 0.01);
        assert!((a.1 - b.1).abs() < 0.01);
        assert!((a.2 - b.2).abs() < 0.01);
        assert!((a.3 - b.3).abs() < 0.01);
    }

    #[test]
    fn square_with_hole_outward_offset_grows_outer_shrinks_hole() {
        let outer = [pt(0.0, 0.0), pt(10.0, 0.0), pt(10.0, 10.0), pt(0.0, 10.0)];
        let hole = [pt(3.0, 3.0), pt(7.0, 3.0), pt(7.0, 7.0), pt(3.0, 7.0)];
        let rings: [&[Point]; 2] = [&outer, &hole];
        let out = offset_polygon(&rings, 1.0, 4.0);
        assert_eq!(out.len(), 2, "outer + hole");
        // Pick the larger by bounding-box area.
        let (outer_out, hole_out) = if area_bbox(&out[0]) > area_bbox(&out[1]) {
            (&out[0], &out[1])
        } else {
            (&out[1], &out[0])
        };
        let (ox0, oy0, ox1, oy1) = bbox(outer_out);
        assert!((ox0 - (-1.0)).abs() < 0.01);
        assert!((oy0 - (-1.0)).abs() < 0.01);
        assert!((ox1 - 11.0).abs() < 0.01);
        assert!((oy1 - 11.0).abs() < 0.01);
        let (hx0, hy0, hx1, hy1) = bbox(hole_out);
        assert!((hx0 - 4.0).abs() < 0.01);
        assert!((hy0 - 4.0).abs() < 0.01);
        assert!((hx1 - 6.0).abs() < 0.01);
        assert!((hy1 - 6.0).abs() < 0.01);
    }

    #[test]
    fn inset_makes_outer_shrink_and_hole_grow_until_polygon_vanishes() {
        let outer = [pt(0.0, 0.0), pt(10.0, 0.0), pt(10.0, 10.0), pt(0.0, 10.0)];
        let hole = [pt(4.0, 4.0), pt(6.0, 4.0), pt(6.0, 6.0), pt(4.0, 6.0)];
        // Polygon contraction by 2: outer 10x10 → 6x6 (2..8); hole 2x2 → 6x6 (2..8).
        // Bounds meet exactly so the polygon has zero area.
        let rings: [&[Point]; 2] = [&outer, &hole];
        let out = offset_polygon(&rings, -2.0, 4.0);
        assert!(out.is_empty(), "polygon collapses to zero area");
    }

    #[test]
    fn inset_with_hole_still_inside_keeps_both_rings() {
        let outer = [pt(0.0, 0.0), pt(10.0, 0.0), pt(10.0, 10.0), pt(0.0, 10.0)];
        let hole = [pt(4.0, 4.0), pt(6.0, 4.0), pt(6.0, 6.0), pt(4.0, 6.0)];
        // Polygon contraction by 1: outer → 8x8 (1..9); hole grows to 4x4 (3..7).
        // Hole still fits inside the inset outer → two rings.
        let rings: [&[Point]; 2] = [&outer, &hole];
        let out = offset_polygon(&rings, -1.0, 4.0);
        assert_eq!(out.len(), 2);
    }

    fn area_bbox(ring: &[Point]) -> f64 {
        let (x0, y0, x1, y1) = bbox(ring);
        (x1 - x0) * (y1 - y0)
    }
}
