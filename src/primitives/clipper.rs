//! Conversion between hephaestus point rings and the `clipper2-rust`
//! path types.
//!
//! Both Clipper2-backed transforms — the polygon offset in
//! [`super::offset`] and the boolean clips in [`super::clip`] — go
//! through these helpers so a ring makes the round trip at one
//! precision, whichever operation carries it.

use crate::geometry::Point;
use clipper2_rust::{PathD, PathsD, Point as ClipperPoint};

/// Decimal places Clipper2 retains internally. Four is enough for
/// sub-pixel accuracy at every render scale hephaestus produces and
/// matches clipper2's default recommendation.
pub(crate) const PRECISION: i32 = 4;

/// Convert one ring / polyline, optionally reversing its winding.
pub(crate) fn to_path_d(points: &[Point], reverse: bool) -> PathD {
    let mut path = PathD::with_capacity(points.len());
    if reverse {
        for p in points.iter().rev() {
            path.push(ClipperPoint::<f64>::new(p.x, p.y));
        }
    } else {
        for p in points {
            path.push(ClipperPoint::<f64>::new(p.x, p.y));
        }
    }
    path
}

/// Convert a multi-ring shape, preserving each ring's winding. Empty
/// rings are dropped.
pub(crate) fn to_paths_d(rings: &[&[Point]]) -> PathsD {
    rings
        .iter()
        .filter(|r| !r.is_empty())
        .map(|r| to_path_d(r, false))
        .collect()
}

/// Convert a Clipper2 result back to point rings, dropping empty ones.
pub(crate) fn from_paths_d(paths: &PathsD) -> Vec<Vec<Point>> {
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
