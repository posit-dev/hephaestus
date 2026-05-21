//! Adaptive corner rounding port of [`boundaries::poly_corner_cutting`].
//!
//! Each rounded corner is emitted as a single cubic Bezier. The two control
//! points sit at distance `(2/3) · cut_dist` from each endpoint, along the
//! **local polyline-forward tangent** at the cut point. This makes the
//! cubic tangent to the actual incoming/outgoing edges at the cut points —
//! and stays correct when the back/forward walk has crossed multiple
//! near-collinear intermediate vertices whose accumulated bend would shift
//! the cut point away from the line that passes through the original corner
//! vertex.
//!
//! The `(2/3)` factor is the **degree-elevation** ratio: in the strict
//! straight-edge case both controls land at `P + (2/3)(V − P)`, i.e. 2/3 of
//! the way from each cut point to the original corner vertex, which is
//! exactly the cubic representation of the quadratic Chaikin limit curve
//! (a cubic with both controls at `V` is a different, more-peaked curve).
//!
//! Vertices classified as collinear (within a small tolerance of 180° or 0°)
//! are transparent to the back/forward walk that determines the cut
//! distance — a cut can therefore extend across several collinear edges,
//! capped at half the distance to the next eligible corner (or the full
//! distance to a polyline endpoint).
//!
//! [`boundaries::poly_corner_cutting`]: https://github.com/thomasp85/boundaries/blob/main/src/corner_clip.cpp

use crate::geometry::{Point, Vec2};
use crate::path::Path;
use crate::primitives::CornerRounding;

const COLLINEAR_TOL_DEG: f64 = 1e-3;
const DEGENERATE_EPS: f64 = 1e-12;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Class {
    /// Eligible corner: will be replaced by a `quad_to`.
    Corner,
    /// Within tolerance of 180° (or 0°) — walk passes through transparently
    /// and the vertex is not emitted.
    Collinear,
    /// Real bend that's above `max_angle_deg` (too gentle to round). Walk
    /// stops here using the halfway-share rule; vertex is emitted as a
    /// `line_to`.
    Other,
    /// Polyline endpoint (only used for open polylines). Walk stops here
    /// using full available distance (no neighbour to share with); vertex is
    /// emitted as a `line_to` (the move_to / final line_to in emission).
    Endpoint,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StopKind {
    Corner,
    Other,
    Endpoint,
}

#[derive(Clone, Copy)]
struct Cut {
    p_back: Point,
    c1: Point,
    c2: Point,
    p_fwd: Point,
    degenerate: bool,
}

/// Round corners on a sequence of vertices using the adaptive Chaikin
/// algorithm from the [`boundaries`](https://github.com/thomasp85/boundaries)
/// R package. Each eligible corner is emitted as one quadratic Bezier whose
/// control point is the original vertex — the exact Chaikin limit curve,
/// reproduced in a single curve segment per corner.
///
/// `closed = true` treats the input as a polygon (vertex 0 is itself a
/// corner between the closing and opening edges); `closed = false` treats it
/// as an open polyline (the endpoints are never rounded, but a corner
/// adjacent to an endpoint may cut all the way to the endpoint, controlled
/// by `opts.max_cut`).
///
/// This function only works on **piecewise-linear** input. Shapes whose
/// edges include curves (e.g. [`super::wedge`], [`super::annular_wedge`],
/// [`super::rounded_rect`]) can't be rounded through this entry point — a
/// line-to-arc corner needs a tangent-aware fillet algorithm, not Chaikin.
pub fn round_corners(vertices: &[Point], closed: bool, opts: CornerRounding) -> Path {
    let n = vertices.len();
    if n < 2 || (closed && n < 3) {
        return Path::new();
    }

    let classes = classify(vertices, closed, opts.max_angle_deg);
    let cuts = compute_cuts(vertices, &classes, closed, opts.max_cut);
    emit(vertices, &classes, &cuts, closed)
}

fn classify(verts: &[Point], closed: bool, max_angle_deg: f64) -> Vec<Class> {
    let n = verts.len();
    (0..n)
        .map(|i| {
            if !closed && (i == 0 || i == n - 1) {
                return Class::Endpoint;
            }
            let prev = verts[if i == 0 { n - 1 } else { i - 1 }];
            let curr = verts[i];
            let next = verts[if i + 1 == n { 0 } else { i + 1 }];
            let angle = interior_angle_deg(prev, curr, next);
            if angle >= 180.0 - COLLINEAR_TOL_DEG || angle <= COLLINEAR_TOL_DEG {
                Class::Collinear
            } else if angle <= max_angle_deg {
                Class::Corner
            } else {
                Class::Other
            }
        })
        .collect()
}

fn interior_angle_deg(prev: Point, curr: Point, next: Point) -> f64 {
    let a = prev - curr;
    let b = next - curr;
    let a_len = a.hypot();
    let b_len = b.hypot();
    if a_len <= DEGENERATE_EPS || b_len <= DEGENERATE_EPS {
        return 180.0;
    }
    let cos = (a.x * b.x + a.y * b.y) / (a_len * b_len);
    cos.clamp(-1.0, 1.0).acos().to_degrees()
}

fn compute_cuts(
    verts: &[Point],
    classes: &[Class],
    closed: bool,
    max_cut: f64,
) -> Vec<Option<Cut>> {
    let n = verts.len();
    let mut cuts = vec![None; n];
    for i in 0..n {
        if classes[i] != Class::Corner {
            continue;
        }
        let (avail_back, back_stop) =
            walk_available(verts, classes, i, closed, /*back=*/ true);
        let (avail_fwd, fwd_stop) = walk_available(verts, classes, i, closed, /*back=*/ false);
        let back_share = if back_stop == StopKind::Corner {
            0.5
        } else {
            1.0
        };
        let fwd_share = if fwd_stop == StopKind::Corner {
            0.5
        } else {
            1.0
        };
        let back_dist = (avail_back * back_share).min(max_cut).max(0.0);
        let fwd_dist = (avail_fwd * fwd_share).min(max_cut).max(0.0);
        let (p_back, t_back) = walk_distance(verts, i, back_dist, closed, /*back=*/ true);
        let (p_fwd, t_fwd) = walk_distance(verts, i, fwd_dist, closed, /*back=*/ false);
        // Control points sit (2/3) · cut_dist from each endpoint along the
        // local polyline-forward tangent. The 2/3 factor is the cubic
        // degree-elevation ratio for the quadratic Chaikin limit — in the
        // straight-edge case it places each control 2/3 of the way from the
        // cut point to the original corner vertex.
        const CUBIC_DEG_ELEV: f64 = 2.0 / 3.0;
        let c1 = p_back + t_back * (back_dist * CUBIC_DEG_ELEV);
        let c2 = p_fwd - t_fwd * (fwd_dist * CUBIC_DEG_ELEV);
        let degenerate = back_dist <= DEGENERATE_EPS && fwd_dist <= DEGENERATE_EPS;
        cuts[i] = Some(Cut {
            p_back,
            c1,
            c2,
            p_fwd,
            degenerate,
        });
    }
    cuts
}

/// Walk back (or forward) from `start`, accumulating edge length and passing
/// through `Collinear` vertices. Stops at the first `Corner` or `Other`
/// vertex, or — for open polylines — at the polyline endpoint.
fn walk_available(
    verts: &[Point],
    classes: &[Class],
    start: usize,
    closed: bool,
    back: bool,
) -> (f64, StopKind) {
    let n = verts.len();
    let mut total = 0.0;
    let mut cur = start;
    loop {
        let Some(nxt) = step(cur, n, closed, back) else {
            return (total, StopKind::Endpoint);
        };
        total += (verts[nxt] - verts[cur]).hypot();
        cur = nxt;
        match classes[cur] {
            Class::Collinear => continue,
            Class::Corner => return (total, StopKind::Corner),
            Class::Other => return (total, StopKind::Other),
            Class::Endpoint => return (total, StopKind::Endpoint),
        }
    }
}

/// Walk back (or forward) from `start` by exactly `distance` units along the
/// polyline, returning the interpolated point together with the unit
/// polyline-forward tangent at that point (the natural travel direction
/// along the edge, regardless of which way the walk is going).
///
/// If the walk would run past an open-polyline endpoint, returns the
/// endpoint vertex together with the tangent of its one adjacent edge.
fn walk_distance(
    verts: &[Point],
    start: usize,
    distance: f64,
    closed: bool,
    back: bool,
) -> (Point, Vec2) {
    let n = verts.len();
    if distance <= 0.0 {
        return (
            verts[start],
            tangent_at_vertex(verts, start, n, closed, back),
        );
    }
    let mut remaining = distance;
    let mut cur = start;
    loop {
        let Some(nxt) = step(cur, n, closed, back) else {
            // Hit an open-polyline endpoint without consuming the full
            // distance — return the endpoint and its sole adjacent edge's
            // tangent.
            return (verts[cur], tangent_at_endpoint(verts, cur, n));
        };
        let seg = verts[nxt] - verts[cur];
        let seg_len = seg.hypot();
        if seg_len >= remaining {
            let t = if seg_len > 0.0 {
                remaining / seg_len
            } else {
                0.0
            };
            let p = verts[cur] + seg * t;
            // Polyline-forward direction on this edge: for a back walk we
            // stepped against polyline order, so forward = -seg; for a
            // forward walk, forward = +seg.
            let forward = if back { -seg } else { seg };
            let unit = unit_vec(forward);
            return (p, unit);
        }
        remaining -= seg_len;
        cur = nxt;
    }
}

fn tangent_at_vertex(verts: &[Point], i: usize, n: usize, closed: bool, back: bool) -> Vec2 {
    // The polyline-forward tangent at vertex `i`. Prefer the edge in the
    // direction we'd be about to walk; fall back to the other if absent.
    let forward_edge = if i + 1 < n {
        Some(verts[i + 1] - verts[i])
    } else if closed {
        Some(verts[0] - verts[i])
    } else {
        None
    };
    let backward_edge = if i > 0 {
        Some(verts[i] - verts[i - 1])
    } else if closed {
        Some(verts[i] - verts[n - 1])
    } else {
        None
    };
    let pick = if back {
        backward_edge.or(forward_edge)
    } else {
        forward_edge.or(backward_edge)
    };
    pick.map(unit_vec).unwrap_or(Vec2::new(1.0, 0.0))
}

fn tangent_at_endpoint(verts: &[Point], i: usize, n: usize) -> Vec2 {
    // Open polyline endpoint: vertex `i` has one adjacent edge.
    let edge = if i == 0 {
        verts[1] - verts[0]
    } else {
        verts[i] - verts[i - 1]
    };
    let _ = n;
    unit_vec(edge)
}

fn unit_vec(v: Vec2) -> Vec2 {
    let len = v.hypot();
    if len < DEGENERATE_EPS {
        Vec2::new(1.0, 0.0)
    } else {
        v / len
    }
}

fn step(cur: usize, n: usize, closed: bool, back: bool) -> Option<usize> {
    if back {
        if cur == 0 {
            if closed {
                Some(n - 1)
            } else {
                None
            }
        } else {
            Some(cur - 1)
        }
    } else if cur + 1 == n {
        if closed {
            Some(0)
        } else {
            None
        }
    } else {
        Some(cur + 1)
    }
}

fn emit(verts: &[Point], classes: &[Class], cuts: &[Option<Cut>], closed: bool) -> Path {
    let n = verts.len();
    let mut path = Path::new();
    if !closed {
        path.move_to(verts[0]);
        for i in 1..n {
            emit_one(&mut path, verts, classes, cuts, i);
        }
        return path;
    }
    let start = pick_start(classes);
    let start_class = classes[start];
    match start_class {
        Class::Corner => {
            let c = cuts[start].expect("Corner without cut");
            if c.degenerate {
                path.move_to(verts[start]);
            } else {
                path.move_to(c.p_fwd);
            }
        }
        _ => path.move_to(verts[start]),
    }
    for shift in 1..n {
        let i = (start + shift) % n;
        emit_one(&mut path, verts, classes, cuts, i);
    }
    if start_class == Class::Corner {
        let c = cuts[start].expect("Corner without cut");
        if c.degenerate {
            path.line_to(verts[start]);
        } else {
            path.line_to(c.p_back);
            path.curve_to(c.c1, c.c2, c.p_fwd);
        }
    }
    path.close_path();
    path
}

fn emit_one(path: &mut Path, verts: &[Point], classes: &[Class], cuts: &[Option<Cut>], i: usize) {
    match classes[i] {
        Class::Other | Class::Endpoint => {
            path.line_to(verts[i]);
        }
        Class::Collinear => {}
        Class::Corner => {
            let c = cuts[i].expect("Corner without cut");
            if c.degenerate {
                path.line_to(verts[i]);
            } else {
                path.line_to(c.p_back);
                path.curve_to(c.c1, c.c2, c.p_fwd);
            }
        }
    }
}

fn pick_start(classes: &[Class]) -> usize {
    if let Some(i) = classes.iter().position(|c| *c == Class::Other) {
        return i;
    }
    if let Some(i) = classes.iter().position(|c| *c == Class::Corner) {
        return i;
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path::PathEl;

    fn p(x: f64, y: f64) -> Point {
        Point::new(x, y)
    }

    fn count_els(path: &Path) -> (usize, usize, usize, usize) {
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

    fn default_opts() -> CornerRounding {
        CornerRounding::default()
    }

    #[test]
    fn empty_when_too_few_vertices() {
        assert_eq!(
            round_corners(&[], false, default_opts()).elements().len(),
            0
        );
        assert_eq!(
            round_corners(&[p(0.0, 0.0)], false, default_opts())
                .elements()
                .len(),
            0,
        );
        assert_eq!(
            round_corners(&[p(0.0, 0.0), p(1.0, 0.0)], true, default_opts())
                .elements()
                .len(),
            0,
        );
    }

    #[test]
    fn unit_square_rounds_all_four_corners() {
        let sq = [p(0.0, 0.0), p(1.0, 0.0), p(1.0, 1.0), p(0.0, 1.0)];
        let path = round_corners(&sq, true, default_opts());
        let (m, l, _, c) = count_els(&path);
        let curves = count_curves(&path);
        assert_eq!(m, 1, "one move_to");
        assert_eq!(curves, 4, "four curve_to (one per corner)");
        assert_eq!(c, 1, "one close_path");
        // Four "line_to(P_back)" emissions, one per corner.
        assert_eq!(l, 4);
    }

    #[test]
    fn classical_chaikin_case_controls_at_two_thirds_to_corner() {
        // For straight incident edges with no walk-through, both cubic
        // control points should land 2/3 of the way from each cut point to
        // the original corner vertex — the degree-elevation form of the
        // classical Chaikin quadratic limit curve.
        let pts = [p(0.0, 0.0), p(10.0, 0.0), p(10.0, 10.0)];
        let path = round_corners(&pts, false, default_opts());
        let mut found = false;
        let v = p(10.0, 0.0);
        let mut pen = Point::ORIGIN;
        for el in path.elements() {
            match el {
                PathEl::MoveTo(pt) | PathEl::LineTo(pt) => pen = *pt,
                PathEl::CurveTo(c1, c2, end) => {
                    let expected_c1 = pen + (v - pen) * (2.0 / 3.0);
                    let expected_c2 = *end + (v - *end) * (2.0 / 3.0);
                    assert!(
                        (*c1 - expected_c1).hypot() < 1e-9,
                        "C1 should be at 2/3 from P_back to V; got {c1:?} expected {expected_c1:?}",
                    );
                    assert!(
                        (*c2 - expected_c2).hypot() < 1e-9,
                        "C2 should be at 2/3 from P_fwd to V; got {c2:?} expected {expected_c2:?}",
                    );
                    found = true;
                    pen = *end;
                }
                _ => {}
            }
        }
        assert!(found);
    }

    #[test]
    fn near_straight_polyline_not_rounded() {
        let pts = [p(0.0, 0.0), p(5.0, 0.01), p(10.0, 0.0)];
        let opts = CornerRounding {
            max_angle_deg: 170.0,
            ..Default::default()
        };
        let path = round_corners(&pts, false, opts);
        let (m, l, _, _) = count_els(&path);
        assert_eq!(m, 1);
        assert_eq!(
            count_curves(&path),
            0,
            "no rounding when angle exceeds max_angle_deg"
        );
        assert!(l >= 2, "endpoints emitted as line_to");
    }

    #[test]
    fn max_cut_caps_cut_distance() {
        let pts = [p(0.0, 0.0), p(10.0, 0.0), p(10.0, 10.0)];
        let opts = CornerRounding {
            max_cut: 0.5,
            ..Default::default()
        };
        let path = round_corners(&pts, false, opts);
        let mut found = false;
        let corner = p(10.0, 0.0);
        let mut pen = Point::ORIGIN;
        for el in path.elements() {
            match el {
                PathEl::MoveTo(pt) | PathEl::LineTo(pt) => pen = *pt,
                PathEl::CurveTo(_, _, end) => {
                    // Classical-Chaikin case: tangents point at the corner.
                    assert!((pen - corner).hypot() <= 0.5 + 1e-9);
                    assert!((*end - corner).hypot() <= 0.5 + 1e-9);
                    pen = *end;
                    found = true;
                }
                _ => {}
            }
        }
        assert!(found, "should have produced a cubic");
    }

    #[test]
    fn collinear_walk_stays_in_back_edge_when_max_cut_short() {
        // |AB|=2, |BC|=4. max_cut=1. Cut walks 1 back from C=(6,0): lands at (5, 0).
        let a = p(0.0, 0.0);
        let b = p(2.0, 0.0);
        let c = p(6.0, 0.0);
        let d = p(6.0, 10.0);
        let opts = CornerRounding {
            max_cut: 1.0,
            ..Default::default()
        };
        let path = round_corners(&[a, b, c, d], false, opts);
        let qs = first_curve_start(&path);
        assert!(
            (qs.x - 5.0).abs() < 1e-9 && qs.y.abs() < 1e-9,
            "P_back should be at (5, 0); got {qs:?}",
        );
    }

    #[test]
    fn collinear_walk_past_subdivision_vertex() {
        // |AB|=10, |BC|=1. max_cut=5. Cut walks 5 back from C=(11,0) to (6, 0) on A-B.
        let a = p(0.0, 0.0);
        let b = p(10.0, 0.0);
        let c = p(11.0, 0.0);
        let d = p(11.0, 10.0);
        let opts = CornerRounding {
            max_cut: 5.0,
            ..Default::default()
        };
        let path = round_corners(&[a, b, c, d], false, opts);
        let qs = first_curve_start(&path);
        assert!(
            (qs.x - 6.0).abs() < 1e-9 && qs.y.abs() < 1e-9,
            "P_back should be at (6, 0); got {qs:?}",
        );
        let line_to_count = path
            .elements()
            .iter()
            .filter(|el| matches!(el, PathEl::LineTo(_)))
            .count();
        assert_eq!(line_to_count, 2, "B should be absorbed");
    }

    #[test]
    fn endpoint_walk_uses_full_available_distance() {
        let a = p(0.0, 0.0);
        let b = p(2.0, 0.0);
        let c = p(6.0, 0.0);
        let d = p(6.0, 10.0);
        let path = round_corners(&[a, b, c, d], false, default_opts());
        let qs = first_curve_start(&path);
        assert!((qs - a).hypot() < 1e-9, "P_back should be at A; got {qs:?}",);
    }

    fn first_curve_start(path: &Path) -> Point {
        let mut last_pen = Point::ORIGIN;
        for el in path.elements() {
            match el {
                PathEl::MoveTo(pt) | PathEl::LineTo(pt) => last_pen = *pt,
                PathEl::CurveTo(_, _, _) => return last_pen,
                _ => {}
            }
        }
        panic!("no curve_to in path");
    }

    #[test]
    fn adjacent_corners_share_edge_halfway() {
        // 4-point closed rectangle. Each corner's cut = 5; cubic controls
        // collapse to the corner vertex.
        let r = [p(0.0, 0.0), p(10.0, 0.0), p(10.0, 10.0), p(0.0, 10.0)];
        let path = round_corners(&r, true, default_opts());
        let mut curves = vec![];
        let mut last_pen: Option<Point> = None;
        for el in path.elements() {
            match el {
                PathEl::MoveTo(pt) | PathEl::LineTo(pt) => last_pen = Some(*pt),
                PathEl::CurveTo(c1, c2, end) => {
                    curves.push((last_pen.unwrap(), *c1, *c2, *end));
                    last_pen = Some(*end);
                }
                _ => {}
            }
        }
        assert_eq!(curves.len(), 4);
        // For straight-edge corners the cubic represents the quadratic
        // Chaikin limit; C1 sits 2/3 of the way from P_back to V, C2 the
        // same on the forward side. So |start - c1| = (2/3) * 5 and
        // |end - c2| = (2/3) * 5.
        let expected = 5.0 * 2.0 / 3.0;
        for &(start, c1, c2, end) in &curves {
            assert!(((start - c1).hypot() - expected).abs() < 1e-9);
            assert!(((end - c2).hypot() - expected).abs() < 1e-9);
        }
    }

    #[test]
    fn open_polyline_keeps_endpoints() {
        let pts = [p(0.0, 0.0), p(5.0, 0.0), p(5.0, 5.0)];
        let path = round_corners(&pts, false, default_opts());
        if let Some(PathEl::MoveTo(first)) = path.elements().first() {
            assert_eq!(*first, pts[0]);
        } else {
            panic!("expected move_to first");
        }
        let mut last_point: Option<Point> = None;
        for el in path.elements() {
            match el {
                PathEl::LineTo(pt) => last_point = Some(*pt),
                PathEl::CurveTo(_, _, end) => last_point = Some(*end),
                _ => {}
            }
        }
        assert_eq!(last_point.unwrap(), pts[2]);
    }

    #[test]
    fn endpoint_adjacent_corner_uses_full_edge() {
        // 3-point open polyline becomes a single curve from A through B to C
        // with both controls at B (straight incident edges).
        let pts = [p(0.0, 0.0), p(4.0, 0.0), p(4.0, 4.0)];
        let path = round_corners(&pts, false, default_opts());
        let mut found = None;
        let mut last_pen = pts[0];
        for el in path.elements() {
            match el {
                PathEl::MoveTo(pt) | PathEl::LineTo(pt) => last_pen = *pt,
                PathEl::CurveTo(c1, c2, end) => {
                    found = Some((last_pen, *c1, *c2, *end));
                }
                _ => {}
            }
        }
        let (start, c1, c2, end) = found.expect("should have a curve");
        assert!((start - pts[0]).hypot() < 1e-9);
        // C1 sits 2/3 from start to V; C2 sits 2/3 from end to V.
        let expected_c1 = start + (pts[1] - start) * (2.0 / 3.0);
        let expected_c2 = end + (pts[1] - end) * (2.0 / 3.0);
        assert!((c1 - expected_c1).hypot() < 1e-9);
        assert!((c2 - expected_c2).hypot() < 1e-9);
        assert!((end - pts[2]).hypot() < 1e-9);
    }

    #[test]
    fn collinear_walk_through_non_straight_run_keeps_tangent_local() {
        // A polyline whose "collinear" run has a tiny inflection — when the
        // back walk crosses this inflection point, the cut endpoint sits on
        // a segment whose direction does NOT pass through the corner V. The
        // cubic control must lie along the local segment tangent, not on
        // the (P_back → V) line.
        //
        // Layout: A--B--C(corner, 90°)--D, where B is exactly collinear
        // (within tolerance) but offset slightly perpendicular so the
        // tangent at points on A-B differs from points on B-C.
        let a = p(0.0, 0.0);
        let b = p(10.0, 1e-5); // ~collinear with A-C
        let c = p(20.0, 0.0);
        let d = p(20.0, 10.0);
        let opts = CornerRounding {
            max_cut: 15.0, // forces back cut to walk through B onto A-B
            ..Default::default()
        };
        let path = round_corners(&[a, b, c, d], false, opts);
        let mut last_pen = Point::ORIGIN;
        let mut c1_observed: Option<Point> = None;
        let mut p_back_observed: Option<Point> = None;
        for el in path.elements() {
            match el {
                PathEl::MoveTo(pt) | PathEl::LineTo(pt) => last_pen = *pt,
                PathEl::CurveTo(c1, _, _) => {
                    p_back_observed = Some(last_pen);
                    c1_observed = Some(*c1);
                    break;
                }
                _ => {}
            }
        }
        let pb = p_back_observed.expect("p_back");
        let c1 = c1_observed.expect("c1");
        // P_back lies on segment A-B (x ∈ [0, 10]).
        assert!(pb.x >= 0.0 && pb.x <= 10.0 + 1e-9, "P_back on A-B");
        // Local tangent on A-B points from A toward B: (10, 1e-5).normalize().
        // C1 - P_back should be along this direction, NOT toward C at (20, 0).
        let dir_to_c = (c - pb).normalize();
        let dir_along_ab = (b - a).normalize();
        let c1_dir = (c1 - pb).normalize();
        let dot_local = c1_dir.x * dir_along_ab.x + c1_dir.y * dir_along_ab.y;
        let dot_to_c = c1_dir.x * dir_to_c.x + c1_dir.y * dir_to_c.y;
        assert!(
            dot_local > 0.9999999,
            "C1 - P_back should align with A-B tangent (dot = {dot_local}), not the line to C (dot = {dot_to_c})"
        );
    }

    fn count_curves(path: &Path) -> usize {
        path.elements()
            .iter()
            .filter(|el| matches!(el, PathEl::CurveTo(_, _, _)))
            .count()
    }
}
