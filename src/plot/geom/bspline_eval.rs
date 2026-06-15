//! Reusable B-spline evaluation primitives shared by spline-based geoms.
//!
//! Extracted from `BSplineGeom`'s implementation so sibling geoms (e.g.
//! `RibbonBSplineGeom`) can call the same de Boor evaluator, adaptive
//! flattener, and projection-mode switch without duplication. The
//! shapes and semantics are documented at each item; the original full
//! design rationale lives in `bspline.rs`.

use crate::geometry::{Point, Rect};

use super::GeomContext;

/// Maximum chord error in panel pixels for adaptive flattening of the
/// projected curve. Sub-pixel so the chord-approximation deviation
/// from the true curve stays below the pixel grid even at sub-pixel
/// AA precision — splines are smooth to the eye at any zoom level
/// the panel itself supports.
pub(crate) const CHORD_ERROR_PX: f64 = 0.25;

/// Maximum recursion depth in the adaptive flattener. Caps work on
/// pathological inputs (extremely tight curvature in projected space).
pub(crate) const MAX_REFINE_DEPTH: usize = 10;

/// Hard upper bound on samples per mark. Defensive cap against
/// degenerate flattening blow-ups; 4096 samples is generous for any
/// reasonable curve.
pub(crate) const MAX_SAMPLES_PER_MARK: usize = 4096;

/// Initial number of equal-parameter sub-intervals per knot span before
/// adaptive refinement kicks in. Higher values trade a flatter
/// recursion tree for more upfront samples *and* a finer-grained
/// initial chord check — which matters because the chord-error test
/// only sees curvature within the interval it's looking at. With a
/// too-coarse initial pass, an interval that wraps around a curve
/// peak can read as "flat" if the curve happens to bulge symmetrically
/// across the chord midpoint. Eight per knot span gives enough
/// resolution that the recursive refinement reliably catches the rest.
pub(crate) const INITIAL_SUBS_PER_SPAN: usize = 8;

/// Whether the spline is built in channel-fraction space and then
/// projected sample-by-sample (`Domain`), or in pixel space after
/// projecting the control points first (`Panel`). The split only
/// matters under non-Cartesian projections — under Cartesian both
/// modes produce identical curves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InterpolationSpace {
    Domain,
    Panel,
}

/// Degenerate case: groups with fewer than `degree + 1` control points
/// render as a straight polyline. Returns sample tuples
/// `(row_position, pixel_point)` ready to feed the same downstream
/// emission code the spline path uses.
pub(crate) fn build_polyline_fallback(
    ctrl_frac: &[Point],
    panel: Rect,
    ctx: &GeomContext<'_>,
) -> Vec<(f64, Point)> {
    let ctrl_px = project_ctrl_pts(ctrl_frac, panel, ctx);
    (0..ctrl_px.len()).map(|i| (i as f64, ctrl_px[i])).collect()
}

/// Build the spline samples in pixel space via de Boor + adaptive
/// chord-error refinement. Returns `(row_position, pixel_point)`
/// pairs in source order, including the clamped endpoints.
pub(crate) fn build_spline_flatten(
    ctrl_frac: &[Point],
    degree: usize,
    panel: Rect,
    ctx: &GeomContext<'_>,
    mode: InterpolationSpace,
) -> Vec<(f64, Point)> {
    let n = ctrl_frac.len();
    let t_end = (n - degree) as f64;
    let n_ctrl_minus_1 = (n - 1) as f64;

    // Resolve a curve sampler closure for the chosen mode. Both
    // branches return a pixel-space `Point` for a given parameter `t`.
    let ctrl_px = project_ctrl_pts(ctrl_frac, panel, ctx);
    let sample = |t: f64| -> Point {
        match mode {
            InterpolationSpace::Panel => de_boor(&ctrl_px, degree, t),
            InterpolationSpace::Domain => {
                let p_frac = de_boor(ctrl_frac, degree, t);
                let (px, py) = ctx
                    .projection
                    .project_to_panel_px(panel, &[p_frac.x, p_frac.y]);
                Point::new(px, py)
            }
        }
    };

    // Convert spline parameter `t ∈ [0, n − d]` to a control-point
    // position `u ∈ [0, n − 1]` for downstream lerp lookups.
    let to_u = |t: f64| -> f64 {
        if t_end > 0.0 {
            t * n_ctrl_minus_1 / t_end
        } else {
            0.0
        }
    };

    let n_spans = (t_end as usize).max(1);
    let total_initial = INITIAL_SUBS_PER_SPAN * n_spans;
    let mut samples: Vec<(f64, Point)> = Vec::with_capacity(total_initial * 2);
    samples.push((to_u(0.0), sample(0.0)));
    let mut t_prev = 0.0;
    for i in 1..=total_initial {
        let t_next = (i as f64 / total_initial as f64) * t_end;
        refine_segment(&sample, &to_u, t_prev, t_next, 0, &mut samples);
        t_prev = t_next;
        if samples.len() >= MAX_SAMPLES_PER_MARK {
            break;
        }
    }
    samples
}

/// Recursive adaptive flatten. Subdivides `[t0, t1]` until the
/// projected curve stays within [`CHORD_ERROR_PX`] panel pixels of
/// the straight chord from `p0` to `p1`. Appends accepted samples
/// to `out`. `t0`'s sample is assumed to already sit in `out`;
/// `t1`'s sample is what we're producing.
///
/// We probe three interior points (at parameter fractions 1/4, 1/2,
/// 3/4) rather than the midpoint alone. A single-midpoint check can
/// silently accept an interval whose curve bulges asymmetrically:
/// the midpoint sits near the chord while the quarter points pull
/// away. Sampling three points along the interval catches those
/// cases — at the cost of two extra `sample` calls per accepted
/// leaf, which is well worth it for visibly smooth curves.
pub(crate) fn refine_segment(
    sample: &impl Fn(f64) -> Point,
    to_u: &impl Fn(f64) -> f64,
    t0: f64,
    t1: f64,
    depth: usize,
    out: &mut Vec<(f64, Point)>,
) {
    if out.len() >= MAX_SAMPLES_PER_MARK {
        return;
    }
    let p0 = out.last().unwrap().1;
    let p1 = sample(t1);
    if depth >= MAX_REFINE_DEPTH {
        out.push((to_u(t1), p1));
        return;
    }
    let chord = p1 - p0;
    let chord_len_sq = chord.length_squared();
    let span = t1 - t0;
    let probe_t = [t0 + 0.25 * span, t0 + 0.5 * span, t0 + 0.75 * span];
    let mut max_err: f64 = 0.0;
    for &t in &probe_t {
        let p = sample(t);
        let off = p - p0;
        let err = if chord_len_sq > 1e-12 {
            // Perpendicular distance from `p` to the chord p0 → p1.
            let cross = off.x * chord.y - off.y * chord.x;
            cross.abs() / chord_len_sq.sqrt()
        } else {
            off.length()
        };
        if err > max_err {
            max_err = err;
        }
    }
    if max_err < CHORD_ERROR_PX {
        out.push((to_u(t1), p1));
    } else {
        let tm = 0.5 * (t0 + t1);
        refine_segment(sample, to_u, t0, tm, depth + 1, out);
        refine_segment(sample, to_u, tm, t1, depth + 1, out);
    }
}

/// Project every channel-fraction control point to pixel space via
/// `ctx.projection`. Used by `InterpolationSpace::Panel` (to build
/// the spline in pixel space) and by the endpoint-tangent helper.
pub(crate) fn project_ctrl_pts(
    ctrl_frac: &[Point],
    panel: Rect,
    ctx: &GeomContext<'_>,
) -> Vec<Point> {
    ctrl_frac
        .iter()
        .map(|p| {
            let (px, py) = ctx.projection.project_to_panel_px(panel, &[p.x, p.y]);
            Point::new(px, py)
        })
        .collect()
}

/// Knot value at index `j` for the clamped uniform knot vector with
/// `n` control points and degree `d`.
///
/// Knot vector layout (length `n + d + 1`):
///
/// ```text
///   [0]*(d+1) ++ [1, 2, ..., n−d−1] ++ [n−d]*(d+1)
/// ```
///
/// so `U[0..=d] = 0`, `U[n..=n+d] = n − d`, and the interior knots
/// `U[d+i]` for `i in 1..=n−d−1` are simply `i`.
#[inline]
pub(crate) fn knot(j: usize, d: usize, n_ctrl: usize) -> f64 {
    let domain_max = (n_ctrl - d) as f64;
    if j <= d {
        0.0
    } else if j >= n_ctrl {
        domain_max
    } else {
        (j - d) as f64
    }
}

/// Find the knot span `k` such that `U[k] <= t <= U[k+1]`, clamped to
/// the valid B-spline range `[d, n_ctrl - 1]`. `t == domain_max` maps
/// to the last interior span so endpoint clamping evaluates `S(t_end)
/// = P_{n−1}`.
#[inline]
pub(crate) fn find_span(t: f64, d: usize, n_ctrl: usize) -> usize {
    let domain_max = (n_ctrl - d) as f64;
    if t >= domain_max {
        return n_ctrl - 1;
    }
    if t <= 0.0 {
        return d;
    }
    let interior = t.floor() as usize;
    (interior + d).min(n_ctrl - 1)
}

/// Evaluate the clamped uniform-knot B-spline at parameter `t` via
/// de Boor's algorithm. Caller guarantees `ctrl.len() >= degree + 1`.
pub(crate) fn de_boor(ctrl: &[Point], degree: usize, t: f64) -> Point {
    let n = ctrl.len();
    let k = find_span(t, degree, n);
    let mut working: Vec<Point> = (0..=degree).map(|i| ctrl[k - degree + i]).collect();
    for r in 1..=degree {
        for i in (r..=degree).rev() {
            // Knot indices for the de Boor recursion (NURBS Book
            // §2.3): denominator is `U[j + d − r + 1] − U[j]`, where
            // `j = k − d + i`. The `d − r + 1` offset shrinks as the
            // recursion level `r` grows — at level `r` we're
            // combining degree-(d − r) basis pieces, so the support
            // interval is `d − r + 1` knots wide.
            let j = k - degree + i;
            let kn_lo = knot(j, degree, n);
            let kn_hi = knot(j + degree - r + 1, degree, n);
            let denom = kn_hi - kn_lo;
            let alpha = if denom > 0.0 {
                (t - kn_lo) / denom
            } else {
                0.0
            };
            let p_im1 = working[i - 1];
            let p_i = working[i];
            working[i] = Point::new(
                (1.0 - alpha) * p_im1.x + alpha * p_i.x,
                (1.0 - alpha) * p_im1.y + alpha * p_i.y,
            );
        }
    }
    working[degree]
}
