//! Polyline-end clipping by a circle, ellipse, or axis-aligned rectangle.
//!
//! Trims the start (or end) of a polyline at the first segment that exits the
//! clip shape, *only* when the polyline's starting (or ending) point lies
//! inside the shape. A polyline that begins outside the shape is left
//! untouched — incidental crossings in the middle are not clipped.

use crate::color::{lerp_color, Color, ColorSpace};
use crate::geometry::{Point, Rect};

/// A shape used to clip a polyline endpoint. All variants are
/// axis-aligned; rotated ellipses or arbitrary kurbo shapes are out of
/// scope.
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

fn exit_circle(p0: Point, d: crate::geometry::Vec2, center: Point, radius: f64) -> Option<f64> {
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

fn exit_rect(p0: Point, d: crate::geometry::Vec2, r: Rect) -> Option<f64> {
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

/// Apply optional clip-start and clip-end to a polyline carrying parallel
/// per-vertex `widths` and `colors` arrays. Synthesised intersection
/// vertices receive width and colour linearly interpolated from the two
/// bracketing data vertices at the intersection parameter `t`, the colour
/// through `space`. Used by ribbon-mode geoms so the clipped polyline
/// keeps its per-vertex attributes aligned with the points list.
///
/// `widths` and `colors` must have the same length as `points`.
pub fn clip_polyline_with_attrs(
    points: &[Point],
    widths: &[f64],
    colors: &[Color],
    clip_start: Option<EndClip>,
    clip_end: Option<EndClip>,
    space: ColorSpace,
) -> (Vec<Point>, Vec<f64>, Vec<Color>) {
    debug_assert_eq!(points.len(), widths.len());
    debug_assert_eq!(points.len(), colors.len());
    if points.len() < 2 {
        return (points.to_vec(), widths.to_vec(), colors.to_vec());
    }
    let mut pts = points.to_vec();
    let mut ws = widths.to_vec();
    let mut cs = colors.to_vec();
    if let Some(c) = clip_start {
        let (np, nw, nc) = trim_start_with_attrs(&pts, &ws, &cs, &c, space);
        pts = np;
        ws = nw;
        cs = nc;
        if pts.len() < 2 {
            return (pts, ws, cs);
        }
    }
    if let Some(c) = clip_end {
        pts.reverse();
        ws.reverse();
        cs.reverse();
        let (np, nw, nc) = trim_start_with_attrs(&pts, &ws, &cs, &c, space);
        pts = np;
        ws = nw;
        cs = nc;
        pts.reverse();
        ws.reverse();
        cs.reverse();
    }
    (pts, ws, cs)
}

fn trim_start_with_attrs(
    points: &[Point],
    widths: &[f64],
    colors: &[Color],
    clip: &EndClip,
    space: ColorSpace,
) -> (Vec<Point>, Vec<f64>, Vec<Color>) {
    let n = points.len();
    if n < 2 {
        return (points.to_vec(), widths.to_vec(), colors.to_vec());
    }
    if !clip.contains(points[0]) {
        return (points.to_vec(), widths.to_vec(), colors.to_vec());
    }
    for i in 0..(n - 1) {
        if let Some(t) = clip.exit_t(points[i], points[i + 1]) {
            let p_exit = points[i] + (points[i + 1] - points[i]) * t;
            let w_exit = widths[i] + t * (widths[i + 1] - widths[i]);
            let c_exit = lerp_color(colors[i], colors[i + 1], t, space);
            let mut out_p = Vec::with_capacity(n - i);
            let mut out_w = Vec::with_capacity(n - i);
            let mut out_c = Vec::with_capacity(n - i);
            out_p.push(p_exit);
            out_w.push(w_exit);
            out_c.push(c_exit);
            out_p.extend_from_slice(&points[(i + 1)..]);
            out_w.extend_from_slice(&widths[(i + 1)..]);
            out_c.extend_from_slice(&colors[(i + 1)..]);
            return (out_p, out_w, out_c);
        }
    }
    (Vec::new(), Vec::new(), Vec::new())
}

// ─── Interior trimming ───────────────────────────────────────────────────────

/// One node of a trimmed sub-run: either a cut boundary at an arc-length
/// position along the original path, or an original vertex passed through
/// untouched.
enum TrimNode {
    /// Boundary interpolated at an arc length measured along the path
    /// (including the closing edge for a ring).
    Cut(f64),
    /// Original vertex by index.
    Vert(usize),
}

/// Split a polyline into sub-runs by cutting an arc-length neighbourhood
/// out around selected vertices.
///
/// `cuts` is a list of `(vertex index, radius)` pairs: the path is
/// trimmed `radius` px of arc length on both sides of that vertex. Cut
/// neighbourhoods that overlap merge into one — the path between two
/// buffered vertices disappears entirely once their radii reach each
/// other. A cut centred on the first or last vertex of an open polyline
/// trims only the one side that exists. When `closed` is set the path is
/// treated as a ring: arc length includes the closing edge from the last
/// vertex back to the first, so a cut on vertex 0 trims across the seam,
/// and the surviving runs are returned open (no synthetic closure is
/// added across a cut).
///
/// Returns the surviving sub-runs in path order. Empty when the cuts
/// consume the whole path.
pub fn clip_polyline_interior(
    points: &[Point],
    cuts: &[(usize, f64)],
    closed: bool,
) -> Vec<Vec<Point>> {
    let n = points.len();
    if n < 2 {
        return vec![points.to_vec()];
    }
    let closing = closing_len(points, closed);
    interior_trim_nodes(points, closing, cuts, closed)
        .into_iter()
        .map(|run| {
            run.iter()
                .map(|node| match *node {
                    TrimNode::Cut(a) => point_at(points, closing, a),
                    TrimNode::Vert(i) => points[i],
                })
                .collect()
        })
        .collect()
}

/// [`clip_polyline_interior`] for a polyline carrying parallel per-vertex
/// `widths` and `colors` arrays. Cut boundaries receive attributes
/// interpolated from the bracketing vertices at the cut parameter, the
/// colour through `space` — the same rule
/// [`clip_polyline_with_attrs`] uses for endpoint cuts.
pub fn clip_polyline_interior_with_attrs(
    points: &[Point],
    widths: &[f64],
    colors: &[Color],
    cuts: &[(usize, f64)],
    closed: bool,
    space: ColorSpace,
) -> Vec<(Vec<Point>, Vec<f64>, Vec<Color>)> {
    debug_assert_eq!(points.len(), widths.len());
    debug_assert_eq!(points.len(), colors.len());
    let n = points.len();
    if n < 2 {
        return vec![(points.to_vec(), widths.to_vec(), colors.to_vec())];
    }
    let closing = closing_len(points, closed);
    interior_trim_nodes(points, closing, cuts, closed)
        .into_iter()
        .map(|run| {
            let mut ps = Vec::with_capacity(run.len());
            let mut ws = Vec::with_capacity(run.len());
            let mut cs = Vec::with_capacity(run.len());
            for node in run {
                match node {
                    TrimNode::Cut(a) => {
                        let (p, w, c) =
                            point_at_with_attrs(points, widths, colors, closing, a, space);
                        ps.push(p);
                        ws.push(w);
                        cs.push(c);
                    }
                    TrimNode::Vert(i) => {
                        ps.push(points[i]);
                        ws.push(widths[i]);
                        cs.push(colors[i]);
                    }
                }
            }
            (ps, ws, cs)
        })
        .collect()
}

/// Arc length of the closing edge — zero for an open path.
fn closing_len(points: &[Point], closed: bool) -> f64 {
    if closed && points.len() >= 2 {
        (points[0] - points[points.len() - 1]).hypot()
    } else {
        0.0
    }
}

/// Compute the sub-runs surviving the cuts as lists of [`TrimNode`]s.
/// Shared core of the plain and attributed interior trims.
fn interior_trim_nodes(
    points: &[Point],
    closing: f64,
    cuts: &[(usize, f64)],
    closed: bool,
) -> Vec<Vec<TrimNode>> {
    let n = points.len();
    // Cumulative arc length at each vertex; `total` spans the whole
    // path, closing edge included for a ring.
    let mut s = vec![0.0; n];
    for i in 1..n {
        s[i] = s[i - 1] + (points[i] - points[i - 1]).hypot();
    }
    let total = s[n - 1] + closing;
    if total <= 0.0 {
        return vec![Vec::new()];
    }

    // Raw cut intervals in arc space. Ring intervals are normalised
    // into [0, total) and may wrap past the end; wrap splits them into
    // two linear intervals.
    let mut ivs: Vec<(f64, f64)> = Vec::new();
    for &(idx, r) in cuts {
        if idx >= n || !r.is_finite() || r <= 0.0 {
            continue;
        }
        let (a, b) = (s[idx] - r, s[idx] + r);
        if closed {
            let len = (b - a).min(total);
            if len >= total {
                return Vec::new(); // one cut swallows the whole ring
            }
            let a = a.rem_euclid(total);
            let b = a + len;
            if b > total {
                ivs.push((a, total));
                ivs.push((0.0, b - total));
            } else {
                ivs.push((a, b));
            }
        } else {
            let (a, b) = (a.max(0.0), b.min(total));
            if a < b {
                ivs.push((a, b));
            }
        }
    }
    if ivs.is_empty() {
        return vec![(0..n).map(TrimNode::Vert).collect()];
    }

    // Merge overlapping intervals.
    ivs.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut merged: Vec<(f64, f64)> = Vec::with_capacity(ivs.len());
    for (a, b) in ivs {
        match merged.last_mut() {
            Some(last) if a <= last.1 => last.1 = last.1.max(b),
            _ => merged.push((a, b)),
        }
    }
    // On a ring, intervals touching arc 0 and arc `total` are the same
    // circular interval — join them into one that extends past `total`.
    if closed && merged.len() > 1 && merged[0].0 == 0.0 && merged[merged.len() - 1].1 == total {
        let head = merged.remove(0);
        merged.last_mut().unwrap().1 = head.1 + total;
    }

    // Complement arcs — the gaps between merged cut intervals. On a
    // ring the arc from the last interval's end around the seam to the
    // first interval's start is a gap too, expressed past `total` so a
    // run crossing the seam extracts as one piece.
    let mut gaps: Vec<(f64, f64)> = Vec::with_capacity(merged.len() + 1);
    for w in merged.windows(2) {
        gaps.push((w[0].1, w[1].0));
    }
    if closed {
        let wrap = (merged[merged.len() - 1].1, merged[0].0 + total);
        if wrap.0 < wrap.1 {
            gaps.push(wrap);
        }
    } else {
        if merged[0].0 > 0.0 {
            gaps.insert(0, (0.0, merged[0].0));
        }
        let last_b = merged[merged.len() - 1].1;
        if last_b < total {
            gaps.push((last_b, total));
        }
    }

    let eps = 1e-9 * total.max(1.0);
    let mut runs = Vec::with_capacity(gaps.len());
    for &(ga, gb) in gaps.iter() {
        if gb - ga <= eps {
            continue;
        }
        let mut run = vec![TrimNode::Cut(ga)];
        // Original vertices strictly inside the gap, in path order.
        // Vertices before `ga` lift past the seam (+total) for a wrap
        // gap, so they walk in a second pass to keep the run ordered.
        for (i, &si) in s.iter().enumerate() {
            if si > ga + eps && si < gb - eps {
                run.push(TrimNode::Vert(i));
            }
        }
        for (i, &si) in s.iter().enumerate() {
            let lifted = si + total;
            if si < ga && lifted > ga + eps && lifted < gb - eps {
                run.push(TrimNode::Vert(i));
            }
        }
        run.push(TrimNode::Cut(gb));
        runs.push(run);
    }
    runs
}

/// Locate an arc-length position on the path as a segment index and
/// parameter. Positions past the last vertex's cumulative length sit on
/// the closing edge (last → first), which exists only for a ring.
fn locate(s: &[f64], closing: f64, a: f64) -> (usize, f64) {
    let n = s.len();
    if a >= s[n - 1] {
        let t = if closing > 0.0 {
            (a - s[n - 1]) / closing
        } else {
            0.0
        };
        return (n - 1, t.clamp(0.0, 1.0));
    }
    // Largest j with s[j] <= a; degenerate (zero-length) edges get t=0.
    let j = match s.binary_search_by(|v| v.total_cmp(&a)) {
        Ok(j) => j,
        Err(j) => j - 1,
    };
    let len = s[j + 1] - s[j];
    let t = if len > 0.0 { (a - s[j]) / len } else { 0.0 };
    (j, t)
}

/// Interpolate the point at arc length `a` along the path. Positions
/// past the total length wrap around the seam of a ring.
fn point_at(points: &[Point], closing: f64, a: f64) -> Point {
    let mut s = Vec::with_capacity(points.len());
    s.push(0.0);
    for i in 1..points.len() {
        s.push(s[i - 1] + (points[i] - points[i - 1]).hypot());
    }
    let total = s[points.len() - 1] + closing;
    // A ring wraps positions past the seam; an open path clamps.
    let a = if closing > 0.0 && total > 0.0 {
        a.rem_euclid(total)
    } else {
        a.clamp(0.0, total)
    };
    let (j, t) = locate(&s, closing, a);
    let p0 = points[j];
    let p1 = if j + 1 < points.len() {
        points[j + 1]
    } else {
        points[0] // closing edge of a ring
    };
    p0 + (p1 - p0) * t
}

/// [`point_at`], additionally lerping the per-vertex attributes at the
/// cut parameter.
fn point_at_with_attrs(
    points: &[Point],
    widths: &[f64],
    colors: &[Color],
    closing: f64,
    a: f64,
    space: ColorSpace,
) -> (Point, f64, Color) {
    let mut s = Vec::with_capacity(points.len());
    s.push(0.0);
    for i in 1..points.len() {
        s.push(s[i - 1] + (points[i] - points[i - 1]).hypot());
    }
    let total = s[points.len() - 1] + closing;
    let a = if closing > 0.0 && total > 0.0 {
        a.rem_euclid(total)
    } else {
        a.clamp(0.0, total)
    };
    let (j, t) = locate(&s, closing, a);
    let k = if j + 1 < points.len() { j + 1 } else { 0 };
    (
        points[j] + (points[k] - points[j]) * t,
        widths[j] + t * (widths[k] - widths[j]),
        lerp_color(colors[j], colors[k], t, space),
    )
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
    fn attrs_clip_lerps_widths_and_colors_at_cut() {
        // Segment from (0, 0) to (5, 0) — exits unit circle at x = 1, t = 0.2.
        let pts = [pt(0.0, 0.0), pt(5.0, 0.0)];
        let widths = [2.0_f64, 12.0];
        let colors = [
            Color::new([0.0, 0.0, 0.0, 1.0]),
            Color::new([1.0, 1.0, 1.0, 1.0]),
        ];
        let clip = EndClip::Circle {
            center: Point::ORIGIN,
            radius: 1.0,
        };
        let (out_p, out_w, out_c) =
            clip_polyline_with_attrs(&pts, &widths, &colors, Some(clip), None, ColorSpace::Srgb);
        assert_eq!(out_p.len(), 2);
        assert_eq!(out_w.len(), 2);
        assert_eq!(out_c.len(), 2);
        // Cut at t = 1/5.
        assert!((out_p[0].x - 1.0).abs() < 1e-9);
        // width lerp: 2.0 + 0.2 * (12.0 - 2.0) = 4.0
        assert!((out_w[0] - 4.0).abs() < 1e-9);
        // color lerp componentwise at t=0.2.
        assert!((out_c[0].components[0] - 0.2).abs() < 1e-5);
        assert!((out_c[0].components[1] - 0.2).abs() < 1e-5);
        // Original second vertex passes through.
        assert_eq!(out_p[1], pt(5.0, 0.0));
        assert!((out_w[1] - 12.0).abs() < 1e-9);
    }

    #[test]
    fn attrs_clip_end_lerps_at_reverse_walk() {
        // Segment from (-10, 0) to (10, 0) — clip end against unit circle at origin.
        let pts = [pt(-10.0, 0.0), pt(10.0, 0.0)];
        let widths = [2.0_f64, 12.0];
        let colors = [
            Color::new([0.0, 0.0, 0.0, 1.0]),
            Color::new([1.0, 1.0, 1.0, 1.0]),
        ];
        let clip = EndClip::Circle {
            center: Point::ORIGIN,
            radius: 1.0,
        };
        let (out_p, out_w, _out_c) =
            clip_polyline_with_attrs(&pts, &widths, &colors, None, Some(clip), ColorSpace::Srgb);
        // Endpoint at (10, 0) is inside? It's *outside* the unit circle (distance 10),
        // so this should be a no-op. Let me adjust the test.
        assert_eq!(out_p.len(), 2);
        assert_eq!(out_p[0], pt(-10.0, 0.0));
        assert_eq!(out_p[1], pt(10.0, 0.0));
        // unchanged widths.
        assert!((out_w[0] - 2.0).abs() < 1e-9);
        assert!((out_w[1] - 12.0).abs() < 1e-9);
    }

    #[test]
    fn attrs_clip_both_ends() {
        // Polyline from inside-A to inside-B; both clips fire and both
        // synthesised endpoints carry lerped attrs.
        let pts = [pt(0.0, 0.0), pt(10.0, 0.0)];
        let widths = [0.0_f64, 10.0];
        let colors = [
            Color::new([0.0, 0.0, 0.0, 1.0]),
            Color::new([1.0, 0.0, 0.0, 1.0]),
        ];
        let clip_a = EndClip::Circle {
            center: Point::ORIGIN,
            radius: 1.0,
        };
        let clip_b = EndClip::Circle {
            center: pt(10.0, 0.0),
            radius: 1.0,
        };
        let (out_p, out_w, _out_c) = clip_polyline_with_attrs(
            &pts,
            &widths,
            &colors,
            Some(clip_a),
            Some(clip_b),
            ColorSpace::Srgb,
        );
        assert_eq!(out_p.len(), 2);
        // Start cut at t = 0.1 → width = 1.0.
        assert!((out_p[0].x - 1.0).abs() < 1e-9);
        assert!((out_w[0] - 1.0).abs() < 1e-9);
        // End cut at t = 0.9 → width = 9.0.
        assert!((out_p[1].x - 9.0).abs() < 1e-9);
        assert!((out_w[1] - 9.0).abs() < 1e-9);
    }

    #[test]
    fn attrs_clip_no_op_when_start_outside() {
        let pts = [pt(-5.0, 0.0), pt(5.0, 0.0)];
        let widths = [1.0_f64, 2.0];
        let colors = [
            Color::new([0.0, 0.0, 0.0, 1.0]),
            Color::new([1.0, 0.0, 0.0, 1.0]),
        ];
        let clip = EndClip::Circle {
            center: Point::ORIGIN,
            radius: 1.0,
        };
        let (out_p, out_w, _out_c) =
            clip_polyline_with_attrs(&pts, &widths, &colors, Some(clip), None, ColorSpace::Srgb);
        assert_eq!(out_p, vec![pt(-5.0, 0.0), pt(5.0, 0.0)]);
        assert_eq!(out_w, vec![1.0, 2.0]);
    }

    #[test]
    fn interior_trim_cuts_around_middle_vertex() {
        // Straight 10px line; cut radius 1 around the vertex at x=5.
        let pts = [pt(0.0, 0.0), pt(5.0, 0.0), pt(10.0, 0.0)];
        let runs = clip_polyline_interior(&pts, &[(1, 1.0)], false);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0], vec![pt(0.0, 0.0), pt(4.0, 0.0)]);
        assert_eq!(runs[1], vec![pt(6.0, 0.0), pt(10.0, 0.0)]);
    }

    #[test]
    fn interior_trim_no_cuts_returns_input() {
        let pts = [pt(0.0, 0.0), pt(5.0, 0.0), pt(10.0, 0.0)];
        let runs = clip_polyline_interior(&pts, &[], false);
        assert_eq!(runs, vec![pts.to_vec()]);
    }

    #[test]
    fn interior_trim_at_open_endpoint_trims_one_side() {
        let pts = [pt(0.0, 0.0), pt(10.0, 0.0)];
        let runs = clip_polyline_interior(&pts, &[(0, 2.0)], false);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0], vec![pt(2.0, 0.0), pt(10.0, 0.0)]);
    }

    #[test]
    fn interior_trim_overlapping_radii_merge() {
        // Two buffered vertices whose radii touch: the path between
        // them disappears entirely rather than leaving a sliver.
        let pts = [pt(0.0, 0.0), pt(4.0, 0.0), pt(6.0, 0.0), pt(10.0, 0.0)];
        let runs = clip_polyline_interior(&pts, &[(1, 2.0), (2, 2.0)], false);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0], vec![pt(0.0, 0.0), pt(2.0, 0.0)]);
        assert_eq!(runs[1], vec![pt(8.0, 0.0), pt(10.0, 0.0)]);
    }

    #[test]
    fn interior_trim_consuming_whole_path_returns_empty() {
        let pts = [pt(0.0, 0.0), pt(10.0, 0.0)];
        let runs = clip_polyline_interior(&pts, &[(0, 6.0), (1, 6.0)], false);
        assert!(runs.is_empty());
    }

    #[test]
    fn interior_trim_ring_cut_on_vertex_zero_crosses_seam() {
        // Unit square, total arc 4. A radius-0.5 cut on vertex 0 trims
        // 0.5 into the last edge and 0.5 into the first edge; the one
        // surviving run crosses no seam and holds vertices 1 and 2.
        let pts = [pt(0.0, 0.0), pt(1.0, 0.0), pt(1.0, 1.0), pt(0.0, 1.0)];
        let runs = clip_polyline_interior(&pts, &[(0, 0.5)], true);
        assert_eq!(runs.len(), 1);
        assert_eq!(
            runs[0],
            vec![
                pt(0.5, 0.0),
                pt(1.0, 0.0),
                pt(1.0, 1.0),
                pt(0.0, 1.0),
                pt(0.0, 0.5)
            ]
        );
    }

    #[test]
    fn interior_trim_ring_two_cuts_give_two_open_runs() {
        let pts = [pt(0.0, 0.0), pt(1.0, 0.0), pt(1.0, 1.0), pt(0.0, 1.0)];
        let runs = clip_polyline_interior(&pts, &[(0, 0.25), (2, 0.25)], true);
        assert_eq!(runs.len(), 2);
        // Run 1: from the cut past vertex 2 (0.75, 1) through vertex 3
        // to the cut before the seam (0, 0.25).
        assert_eq!(runs[0], vec![pt(0.75, 1.0), pt(0.0, 1.0), pt(0.0, 0.25)]);
        // Run 2 wraps the seam: from the cut past the seam (0.25, 0)
        // through vertex 1 to the cut before vertex 2 (1, 0.75).
        assert_eq!(runs[1], vec![pt(0.25, 0.0), pt(1.0, 0.0), pt(1.0, 0.75)]);
    }

    #[test]
    fn interior_trim_with_attrs_lerps_at_cuts() {
        let pts = [pt(0.0, 0.0), pt(10.0, 0.0)];
        let widths = [0.0_f64, 10.0];
        let colors = [
            Color::new([0.0, 0.0, 0.0, 1.0]),
            Color::new([1.0, 1.0, 1.0, 1.0]),
        ];
        let runs = clip_polyline_interior_with_attrs(
            &pts,
            &widths,
            &colors,
            &[(0, 2.0)],
            false,
            ColorSpace::Srgb,
        );
        assert_eq!(runs.len(), 1);
        let (p, w, c) = &runs[0];
        assert_eq!(p.len(), 2);
        // Cut at t = 0.2 along the single segment.
        assert!((p[0].x - 2.0).abs() < 1e-9);
        assert!((w[0] - 2.0).abs() < 1e-9);
        assert!((c[0].components[0] - 0.2).abs() < 1e-5);
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
