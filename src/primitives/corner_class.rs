//! Join classification shared by the two corner-rounding passes.
//!
//! [`super::corner`] (piecewise-linear, Chaikin) and
//! [`super::path_corner`] (curve-aware fillets) run different
//! algorithms but answer the same question first: given the interior
//! angle at a join, is it a corner worth rounding, a collinear join to
//! walk through, or a bend too gentle to touch? The vocabulary for that
//! answer lives here so the two passes can't drift apart on where the
//! boundaries sit.

use crate::geometry::Vec2;
use crate::primitives::tolerance::{COLLINEAR_TOL_DEG, DEGENERATE_EPS};

/// What a corner-rounding pass does with a join.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Class {
    /// Eligible corner: replaced by a Bezier fillet.
    Corner,
    /// Within tolerance of 180° (or 0°) — carries no bend, so the walk
    /// that measures cut distance passes through transparently and the
    /// vertex itself is not emitted.
    Collinear,
    /// Real bend, but above `max_angle_deg` and so too gentle to round.
    /// A walk stops here under the halfway-share rule and the vertex is
    /// emitted as-is.
    NonCorner,
    /// End of an open subpath. A walk stops here using the full
    /// available distance — there is no neighbouring corner to share
    /// with.
    Endpoint,
}

/// Interior angle, in degrees, between two directions leaving a shared
/// vertex. Falls back to 180° (collinear) when either direction is too
/// short to carry one.
pub(crate) fn angle_between_deg(a: Vec2, b: Vec2) -> f64 {
    let a_len = a.hypot();
    let b_len = b.hypot();
    if a_len <= DEGENERATE_EPS || b_len <= DEGENERATE_EPS {
        return 180.0;
    }
    let cos = (a.x * b.x + a.y * b.y) / (a_len * b_len);
    cos.clamp(-1.0, 1.0).acos().to_degrees()
}

/// Classify a join from its interior angle. Angles within
/// [`COLLINEAR_TOL_DEG`] of 180° or 0° are collinear; the rest are
/// corners up to `max_angle_deg` and non-corners above it.
pub(crate) fn classify_angle(angle_deg: f64, max_angle_deg: f64) -> Class {
    if angle_deg >= 180.0 - COLLINEAR_TOL_DEG || angle_deg <= COLLINEAR_TOL_DEG {
        Class::Collinear
    } else if angle_deg <= max_angle_deg {
        Class::Corner
    } else {
        Class::NonCorner
    }
}
