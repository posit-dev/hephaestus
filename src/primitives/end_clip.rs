//! Polyline-end clipping by a circle, ellipse, or axis-aligned rectangle.
//!
//! Trims the start (or end) of a polyline at the first segment that exits the
//! clip shape, *only* when the polyline's starting (or ending) point lies
//! inside the shape. A polyline that begins outside the shape is left
//! untouched — incidental crossings in the middle are not clipped.

use crate::geometry::{Point, Rect};
use crate::path::Path;

/// A shape used to clip a polyline endpoint. All variants are axis-aligned in
/// v1; rotated ellipses or arbitrary kurbo shapes are out of scope.
#[derive(Debug, Clone, Copy)]
pub enum EndClip {
    /// Closed disk centered at `center` with radius `radius`.
    Circle { center: Point, radius: f64 },
    /// Axis-aligned ellipse centered at `center` with x-radius `rx` and
    /// y-radius `ry`.
    Ellipse { center: Point, rx: f64, ry: f64 },
    /// Closed axis-aligned rectangle.
    Rect(Rect),
}

impl EndClip {
    /// Whether the closed shape contains the point.
    fn contains(&self, p: Point) -> bool {
        match *self {
            EndClip::Circle { center, radius } => (p - center).hypot() <= radius,
            EndClip::Ellipse { center, rx, ry } => {
                if rx <= 0.0 || ry <= 0.0 {
                    return false;
                }
                let dx = (p.x - center.x) / rx;
                let dy = (p.y - center.y) / ry;
                dx * dx + dy * dy <= 1.0
            }
            EndClip::Rect(r) => p.x >= r.x0 && p.x <= r.x1 && p.y >= r.y0 && p.y <= r.y1,
        }
    }

    /// For a segment from `p0` (assumed inside) to `p1`, return the smallest
    /// `t ∈ (0, 1]` such that `p0 + t·(p1-p0)` lies on the shape boundary.
    /// Returns `None` when the segment stays inside.
    fn exit_t(&self, p0: Point, p1: Point) -> Option<f64> {
        let d = p1 - p0;
        match *self {
            EndClip::Circle { center, radius } => exit_circle(p0, d, center, radius),
            EndClip::Ellipse { center, rx, ry } => {
                if rx <= 0.0 || ry <= 0.0 {
                    return None;
                }
                let u0 = Point::new((p0.x - center.x) / rx, (p0.y - center.y) / ry);
                let u1 = Point::new((p1.x - center.x) / rx, (p1.y - center.y) / ry);
                exit_circle(u0, u1 - u0, Point::ORIGIN, 1.0)
            }
            EndClip::Rect(r) => exit_rect(p0, d, r),
        }
    }
}

fn exit_circle(p0: Point, d: kurbo::Vec2, center: Point, radius: f64) -> Option<f64> {
    let f = p0 - center;
    let a = d.dot(d);
    if a == 0.0 {
        return None;
    }
    let b = 2.0 * f.dot(d);
    let c = f.dot(f) - radius * radius;
    let disc = b * b - 4.0 * a * c;
    if disc < 0.0 {
        return None;
    }
    let sqrt_disc = disc.sqrt();
    // Larger root — the exit for a segment starting inside.
    let t = (-b + sqrt_disc) / (2.0 * a);
    if t > 0.0 && t <= 1.0 {
        Some(t)
    } else {
        None
    }
}

fn exit_rect(p0: Point, d: kurbo::Vec2, r: Rect) -> Option<f64> {
    let mut t_exit = f64::INFINITY;
    if d.x > 0.0 {
        t_exit = t_exit.min((r.x1 - p0.x) / d.x);
    } else if d.x < 0.0 {
        t_exit = t_exit.min((r.x0 - p0.x) / d.x);
    }
    if d.y > 0.0 {
        t_exit = t_exit.min((r.y1 - p0.y) / d.y);
    } else if d.y < 0.0 {
        t_exit = t_exit.min((r.y0 - p0.y) / d.y);
    }
    if t_exit.is_finite() && t_exit > 0.0 && t_exit <= 1.0 {
        Some(t_exit)
    } else {
        None
    }
}

/// Apply optional clip-start and clip-end to a polyline, returning the
/// trimmed vertex list. Useful as a standalone preprocessing step before
/// piping the result into [`round_corners`](super::round_corners) or any
/// other vertex-list consumer.
///
/// `clip_start` trims only when `points[0]` is inside the shape — a polyline
/// beginning outside the clip shape is left untouched, even if its middle
/// passes through. Symmetric for `clip_end` against `points[len-1]`. May
/// return an empty `Vec` when the polyline is fully inside one of the clip
/// shapes.
pub fn clip_polyline(
    points: &[Point],
    clip_start: Option<EndClip>,
    clip_end: Option<EndClip>,
) -> Vec<Point> {
    if points.len() < 2 {
        return points.to_vec();
    }
    let mut pts = points.to_vec();
    if let Some(c) = clip_start {
        pts = trim_start(&pts, &c);
        if pts.len() < 2 {
            return pts;
        }
    }
    if let Some(c) = clip_end {
        pts.reverse();
        pts = trim_start(&pts, &c);
        pts.reverse();
    }
    pts
}

fn trim_start(points: &[Point], clip: &EndClip) -> Vec<Point> {
    let n = points.len();
    if n < 2 {
        return points.to_vec();
    }
    if !clip.contains(points[0]) {
        return points.to_vec();
    }
    for i in 0..(n - 1) {
        if let Some(t) = clip.exit_t(points[i], points[i + 1]) {
            let p_exit = points[i] + (points[i + 1] - points[i]) * t;
            let mut out = Vec::with_capacity(n - i);
            out.push(p_exit);
            out.extend_from_slice(&points[(i + 1)..]);
            return out;
        }
    }
    Vec::new()
}

/// Construct a plain `move_to` + `line_to*` path from a vertex list. Used by
/// callers that don't need corner rounding.
pub(super) fn polyline_path(points: &[Point]) -> Path {
    let mut path = Path::new();
    if points.len() < 2 {
        return path;
    }
    path.move_to(points[0]);
    for p in &points[1..] {
        path.line_to(*p);
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(x: f64, y: f64) -> Point {
        Point::new(x, y)
    }

    #[test]
    fn circle_clip_trims_at_entry() {
        // Segment from (-5, 0) into unit circle at origin.
        let pts = [pt(0.0, 0.0), pt(5.0, 0.0)];
        let clip = EndClip::Circle {
            center: Point::ORIGIN,
            radius: 1.0,
        };
        let out = clip_polyline(&pts, Some(clip), None);
        assert_eq!(out.len(), 2);
        assert!((out[0].x - 1.0).abs() < 1e-9);
        assert!(out[0].y.abs() < 1e-9);
        assert_eq!(out[1], pt(5.0, 0.0));
    }

    #[test]
    fn circle_no_clip_when_start_outside() {
        let pts = [pt(-5.0, 0.0), pt(5.0, 0.0)];
        let clip = EndClip::Circle {
            center: Point::ORIGIN,
            radius: 1.0,
        };
        let out = clip_polyline(&pts, Some(clip), None);
        assert_eq!(out, vec![pt(-5.0, 0.0), pt(5.0, 0.0)]);
    }

    #[test]
    fn ellipse_clip_trims_at_boundary() {
        // Ellipse rx=2, ry=1 at origin. Start at origin, head along +x — exit at x=2.
        let pts = [pt(0.0, 0.0), pt(5.0, 0.0)];
        let clip = EndClip::Ellipse {
            center: Point::ORIGIN,
            rx: 2.0,
            ry: 1.0,
        };
        let out = clip_polyline(&pts, Some(clip), None);
        assert_eq!(out.len(), 2);
        assert!((out[0].x - 2.0).abs() < 1e-9);
        assert!(out[0].y.abs() < 1e-9);
    }

    #[test]
    fn rect_clip_trims_at_boundary() {
        let pts = [pt(0.5, 0.5), pt(2.0, 0.5)];
        let clip = EndClip::Rect(Rect::new(0.0, 0.0, 1.0, 1.0));
        let out = clip_polyline(&pts, Some(clip), None);
        assert!((out[0].x - 1.0).abs() < 1e-9);
        assert!((out[0].y - 0.5).abs() < 1e-9);
    }

    #[test]
    fn polyline_fully_inside_returns_empty() {
        let pts = [pt(0.0, 0.0), pt(0.5, 0.0), pt(0.5, 0.5)];
        let clip = EndClip::Circle {
            center: Point::ORIGIN,
            radius: 5.0,
        };
        let out = clip_polyline(&pts, Some(clip), None);
        assert!(out.is_empty());
    }

    #[test]
    fn both_ends_clipped() {
        // Segment from inside-circle-A to inside-circle-B.
        let pts = [pt(0.0, 0.0), pt(10.0, 0.0)];
        let clip_a = EndClip::Circle {
            center: Point::ORIGIN,
            radius: 1.0,
        };
        let clip_b = EndClip::Circle {
            center: pt(10.0, 0.0),
            radius: 1.0,
        };
        let out = clip_polyline(&pts, Some(clip_a), Some(clip_b));
        assert_eq!(out.len(), 2);
        assert!((out[0].x - 1.0).abs() < 1e-9);
        assert!((out[1].x - 9.0).abs() < 1e-9);
    }

    #[test]
    fn multi_segment_polyline_clip_finds_first_exit() {
        // Start at origin (inside unit circle); zig-zag out and back in.
        let pts = [
            pt(0.0, 0.0),
            pt(0.5, 0.0),
            pt(2.0, 0.0), // exits unit circle on this segment
            pt(2.0, 1.0),
        ];
        let clip = EndClip::Circle {
            center: Point::ORIGIN,
            radius: 1.0,
        };
        let out = clip_polyline(&pts, Some(clip), None);
        // Exit on segment (0.5, 0) -> (2.0, 0) at x = 1.
        assert_eq!(out.len(), 3);
        assert!((out[0].x - 1.0).abs() < 1e-9);
        assert_eq!(out[1], pt(2.0, 0.0));
        assert_eq!(out[2], pt(2.0, 1.0));
    }
}
