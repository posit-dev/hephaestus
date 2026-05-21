//! Path-level corner rounding for arbitrary `BezPath`s.
//!
//! Walks the path segment by segment, identifies each join where two segments
//! meet, and — when the join is an eligible corner — truncates the incident
//! segments and connects them with a cubic Bezier fillet whose endpoint
//! tangents match the original segments. Works on lines, quadratic Beziers,
//! and cubic Beziers, in any combination.
//!
//! The same [`CornerRounding`] semantics as [`super::round_corners`] apply:
//! the join's interior angle (angle between `-T_in` and `T_out`) classifies
//! it as `Corner` / `Collinear` / `NonCorner`; the cut distance is
//! `min(half-of-incident-arc-length, max_cut)` when the segment has a corner
//! at its other end, or `min(full-arc-length, max_cut)` at a path endpoint.
//!
//! The connecting cubic Bezier uses a `(2/3) · d` control-point magnitude —
//! the cubic degree-elevation ratio for the quadratic Chaikin limit. This
//! matches what [`super::round_corners`] produces for line-to-line corners
//! exactly, so a polyline rounded through either entry point looks
//! identical. For line-to-arc or arc-to-arc corners the result is tangent-
//! continuous but not strictly a circular fillet; upgrading to an exact-
//! circular fillet (Bezier-arc magic `(4/3) · tan(bend/4)`) is a one-line
//! swap when the visual difference matters.

use crate::geometry::{Point, Vec2};
use crate::path::{Path, PathEl};
use crate::primitives::CornerRounding;
use kurbo::{CubicBez, Line, ParamCurve, ParamCurveArclen, ParamCurveDeriv, PathSeg, QuadBez};

const COLLINEAR_TOL_DEG: f64 = 1e-3;
const ARCLEN_ACCURACY: f64 = 1e-3;
const DEGENERATE_EPS: f64 = 1e-9;

/// Round corners on an arbitrary `Path`. Unlike [`super::round_corners`],
/// this operates on the path's segments directly, so it works for shapes
/// whose edges are not piecewise-linear ([`super::wedge`],
/// [`super::annular_wedge`], etc.).
///
/// Each rounded corner becomes a single cubic Bezier whose endpoint tangents
/// match the original segments — the join is tangent-continuous in the
/// output. Joins whose interior angle is within tolerance of 180° (or 0°)
/// are treated as collinear and left alone; joins above `max_angle_deg`
/// stay sharp.
pub fn round_path_corners(path: &Path, opts: CornerRounding) -> Path {
    let subpaths = split_subpaths(path);
    let mut out = Path::new();
    for sp in &subpaths {
        round_subpath_into(sp, opts, &mut out);
    }
    out
}

struct Subpath {
    segs: Vec<PathSeg>,
    closed: bool,
}

fn split_subpaths(path: &Path) -> Vec<Subpath> {
    let mut out = Vec::new();
    let mut segs: Vec<PathSeg> = Vec::new();
    let mut start = Point::ORIGIN;
    let mut pen = Point::ORIGIN;
    let mut in_sub = false;
    let mut closed = false;
    for el in path.elements() {
        match el {
            PathEl::MoveTo(p) => {
                if in_sub {
                    out.push(Subpath {
                        segs: std::mem::take(&mut segs),
                        closed,
                    });
                }
                start = *p;
                pen = *p;
                in_sub = true;
                closed = false;
            }
            PathEl::LineTo(p) => {
                segs.push(PathSeg::Line(Line::new(pen, *p)));
                pen = *p;
            }
            PathEl::QuadTo(c, p) => {
                segs.push(PathSeg::Quad(QuadBez::new(pen, *c, *p)));
                pen = *p;
            }
            PathEl::CurveTo(c1, c2, p) => {
                segs.push(PathSeg::Cubic(CubicBez::new(pen, *c1, *c2, *p)));
                pen = *p;
            }
            PathEl::ClosePath => {
                if (pen - start).hypot() > DEGENERATE_EPS {
                    segs.push(PathSeg::Line(Line::new(pen, start)));
                    pen = start;
                }
                closed = true;
            }
        }
    }
    if in_sub {
        out.push(Subpath { segs, closed });
    }
    out
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Class {
    Corner,
    Collinear,
    NonCorner,
}

struct JoinInfo {
    t_back: f64,
    t_fwd: f64,
    c1: Point,
    c2: Point,
    p_fwd: Point,
}

fn round_subpath_into(sp: &Subpath, opts: CornerRounding, out: &mut Path) {
    let n = sp.segs.len();
    if n == 0 {
        return;
    }
    let n_joins = if sp.closed { n } else { n - 1 };
    if n_joins == 0 {
        // Single open segment — nothing to round.
        let s = sp.segs[0];
        out.move_to(s.start());
        push_seg_continuation(out, &s);
        return;
    }

    let classes: Vec<Class> = (0..n_joins)
        .map(|j| classify_join(&sp.segs[j], &sp.segs[(j + 1) % n], opts.max_angle_deg))
        .collect();

    let mut joins: Vec<Option<JoinInfo>> = (0..n_joins).map(|_| None).collect();
    for j in 0..n_joins {
        if classes[j] != Class::Corner {
            continue;
        }
        let left = sp.segs[j];
        let right = sp.segs[(j + 1) % n];
        let left_len = left.arclen(ARCLEN_ACCURACY);
        let right_len = right.arclen(ARCLEN_ACCURACY);
        if left_len < DEGENERATE_EPS || right_len < DEGENERATE_EPS {
            continue;
        }
        let back_share = if other_end_corner(j, n_joins, sp.closed, &classes, /*back=*/ true) {
            0.5
        } else {
            1.0
        };
        let fwd_share = if other_end_corner(j, n_joins, sp.closed, &classes, /*back=*/ false) {
            0.5
        } else {
            1.0
        };
        let back_dist = (left_len * back_share).min(opts.max_cut);
        let fwd_dist = (right_len * fwd_share).min(opts.max_cut);
        if back_dist < DEGENERATE_EPS || fwd_dist < DEGENERATE_EPS {
            continue;
        }
        let t_back = left.inv_arclen(left_len - back_dist, ARCLEN_ACCURACY);
        let t_fwd = right.inv_arclen(fwd_dist, ARCLEN_ACCURACY);
        let p_back = left.eval(t_back);
        let p_fwd = right.eval(t_fwd);
        let t_back_dir = unit_tangent_at(&left, t_back);
        let t_fwd_dir = unit_tangent_at(&right, t_fwd);
        // (2/3) is the cubic degree-elevation ratio for the quadratic
        // Chaikin limit — same magnitude as super::round_corners uses.
        const CUBIC_DEG_ELEV: f64 = 2.0 / 3.0;
        let c1 = p_back + t_back_dir * (back_dist * CUBIC_DEG_ELEV);
        let c2 = p_fwd - t_fwd_dir * (fwd_dist * CUBIC_DEG_ELEV);
        joins[j] = Some(JoinInfo {
            t_back,
            t_fwd,
            c1,
            c2,
            p_fwd,
        });
    }

    // Emission.
    let start_pt = if sp.closed {
        match joins[n_joins - 1].as_ref() {
            Some(j) => j.p_fwd,
            None => sp.segs[0].start(),
        }
    } else {
        sp.segs[0].start()
    };
    out.move_to(start_pt);

    for k in 0..n {
        let t_start = left_join_idx(k, n_joins, sp.closed)
            .and_then(|j| joins[j].as_ref())
            .map(|info| info.t_fwd)
            .unwrap_or(0.0);
        let t_end = right_join_idx(k, n_joins)
            .and_then(|j| joins[j].as_ref())
            .map(|info| info.t_back)
            .unwrap_or(1.0);
        if t_end > t_start + DEGENERATE_EPS {
            let sub = sp.segs[k].subsegment(t_start..t_end);
            push_seg_continuation(out, &sub);
        }
        if let Some(j) = right_join_idx(k, n_joins) {
            if let Some(info) = joins[j].as_ref() {
                out.curve_to(info.c1, info.c2, info.p_fwd);
            }
        }
    }

    if sp.closed {
        out.close_path();
    }
}

fn classify_join(left: &PathSeg, right: &PathSeg, max_angle_deg: f64) -> Class {
    let t_in = unit_tangent_at(left, 1.0);
    let t_out = unit_tangent_at(right, 0.0);
    // Interior angle = angle between (-T_in) and T_out.
    let cos = (-t_in.x) * t_out.x + (-t_in.y) * t_out.y;
    let cos = cos.clamp(-1.0, 1.0);
    let angle_deg = cos.acos().to_degrees();
    if angle_deg >= 180.0 - COLLINEAR_TOL_DEG || angle_deg <= COLLINEAR_TOL_DEG {
        Class::Collinear
    } else if angle_deg <= max_angle_deg {
        Class::Corner
    } else {
        Class::NonCorner
    }
}

fn unit_tangent_at(seg: &PathSeg, t: f64) -> Vec2 {
    let v = match seg {
        PathSeg::Line(l) => l.deriv().eval(t).to_vec2(),
        PathSeg::Quad(q) => q.deriv().eval(t).to_vec2(),
        PathSeg::Cubic(c) => c.deriv().eval(t).to_vec2(),
    };
    let len = v.hypot();
    if len < DEGENERATE_EPS {
        Vec2::new(1.0, 0.0)
    } else {
        v / len
    }
}

fn left_join_idx(k: usize, n_joins: usize, closed: bool) -> Option<usize> {
    if k == 0 {
        if closed {
            Some(n_joins - 1)
        } else {
            None
        }
    } else {
        Some(k - 1)
    }
}

fn right_join_idx(k: usize, n_joins: usize) -> Option<usize> {
    if k < n_joins {
        Some(k)
    } else {
        None
    }
}

/// For join `j`, returns whether the segment on its `back` (true → previous)
/// or forward (false → next) side has another `Corner` join at its other end.
fn other_end_corner(j: usize, n_joins: usize, closed: bool, classes: &[Class], back: bool) -> bool {
    let other = if back {
        // left segment of join j is segment j; its other end is at join (j-1)
        if j == 0 {
            if closed {
                Some(n_joins - 1)
            } else {
                None
            }
        } else {
            Some(j - 1)
        }
    } else {
        // right segment of join j is segment (j+1); its other end is at join (j+1)
        if j + 1 < n_joins {
            Some(j + 1)
        } else if closed {
            Some(0)
        } else {
            None
        }
    };
    other.map(|i| classes[i] == Class::Corner).unwrap_or(false)
}

fn push_seg_continuation(path: &mut Path, seg: &PathSeg) {
    match seg {
        PathSeg::Line(l) => path.line_to(l.p1),
        PathSeg::Quad(q) => path.quad_to(q.p1, q.p2),
        PathSeg::Cubic(c) => path.curve_to(c.p1, c.p2, c.p3),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::{annular_wedge, polygon, rounded_rect, wedge};
    use crate::Rect;
    use std::f64::consts::PI;

    fn count_curves(path: &Path) -> usize {
        path.elements()
            .iter()
            .filter(|el| matches!(el, PathEl::CurveTo(_, _, _)))
            .count()
    }

    fn count_moves(path: &Path) -> usize {
        path.elements()
            .iter()
            .filter(|el| matches!(el, PathEl::MoveTo(_)))
            .count()
    }

    #[test]
    fn empty_path_returns_empty() {
        let p = Path::new();
        let out = round_path_corners(&p, CornerRounding::default());
        assert_eq!(out.elements().len(), 0);
    }

    #[test]
    fn unit_square_rounded_yields_four_fillets() {
        // Build a 10×10 polygon via the polygon constructor (4 lines + close).
        let sq = [
            Point::new(0.0, 0.0),
            Point::new(10.0, 0.0),
            Point::new(10.0, 10.0),
            Point::new(0.0, 10.0),
        ];
        let p = polygon(&[&sq], crate::primitives::PolygonOptions::default());
        let out = round_path_corners(&p, CornerRounding::default());
        // Four corners → four cubic fillets.
        assert_eq!(count_curves(&out), 4);
        // Output should still be one closed subpath.
        assert_eq!(count_moves(&out), 1);
    }

    #[test]
    fn wedge_rounds_two_line_to_arc_corners() {
        // 90° wedge: should get two fillets (one at each radial-to-arc corner).
        let w = wedge(Point::ORIGIN, 100.0, 0.0, PI / 2.0);
        let opts = CornerRounding {
            max_cut: 20.0,
            ..Default::default()
        };
        let out = round_path_corners(&w, opts);
        // Original wedge has: center vertex (line-to-line, two radials meeting)
        // plus two line-to-arc corners. With max_angle_deg = INF, all 3 round.
        // The center is a 90° corner (radials at 0 and 90 deg from each other).
        // Expected fillets: 3.
        assert!(count_curves(&out) >= 3);
    }

    #[test]
    fn annular_wedge_rounds_four_corners() {
        let a = annular_wedge(Point::ORIGIN, 30.0, 100.0, 0.0, PI / 2.0);
        let opts = CornerRounding {
            max_cut: 10.0,
            ..Default::default()
        };
        let out = round_path_corners(&a, opts);
        // 4 line-to-arc corners → at least 4 fillets.
        let curves = count_curves(&out);
        assert!(curves >= 4, "expected at least 4 fillets, got {curves}",);
    }

    #[test]
    fn collinear_joins_are_left_alone() {
        // Square split into 8 vertices (each edge bisected). The four
        // midpoint joins are collinear; they shouldn't be rounded.
        let mut path = Path::new();
        path.move_to(Point::new(0.0, 0.0));
        path.line_to(Point::new(5.0, 0.0));
        path.line_to(Point::new(10.0, 0.0));
        path.line_to(Point::new(10.0, 5.0));
        path.line_to(Point::new(10.0, 10.0));
        path.line_to(Point::new(5.0, 10.0));
        path.line_to(Point::new(0.0, 10.0));
        path.line_to(Point::new(0.0, 5.0));
        path.close_path();
        let out = round_path_corners(&path, CornerRounding::default());
        // Only the 4 actual corners (90°) are rounded; the 4 midpoints stay.
        assert_eq!(count_curves(&out), 4);
    }

    #[test]
    fn max_angle_filter_keeps_corner_sharp() {
        let sq = [
            Point::new(0.0, 0.0),
            Point::new(10.0, 0.0),
            Point::new(10.0, 10.0),
            Point::new(0.0, 10.0),
        ];
        let p = polygon(&[&sq], crate::primitives::PolygonOptions::default());
        // 90° corners > 80° → corners get rounded.
        let out = round_path_corners(
            &p,
            CornerRounding {
                max_angle_deg: 80.0,
                ..Default::default()
            },
        );
        assert_eq!(count_curves(&out), 0, "above max_angle_deg, no rounding");
        let out2 = round_path_corners(
            &p,
            CornerRounding {
                max_angle_deg: 95.0,
                ..Default::default()
            },
        );
        assert_eq!(count_curves(&out2), 4);
    }

    #[test]
    fn rounded_rect_still_produces_valid_output() {
        // rounded_rect's corners are already arcs (no sharp joins).
        // round_path_corners should leave them alone.
        let p = rounded_rect(Rect::new(0.0, 0.0, 10.0, 10.0), 2.0);
        let _ = round_path_corners(&p, CornerRounding::default());
        // Shouldn't panic. The path's joins between arc and line are
        // tangent-continuous, so no fillets are added.
    }
}
