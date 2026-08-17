//! The numeric tolerances shared by the primitive constructors and
//! transforms.
//!
//! Every geometric approximation in this module family — curve
//! flattening, arc fans, corner classification — trades accuracy for
//! vertex count against one of the constants below. They live together
//! so the trade-offs can be compared side by side instead of drifting
//! apart in the files that consume them.
//!
//! All length-valued tolerances are in **path coordinates** (panel
//! pixels at the call sites that convert from pt).

/// Maximum deviation allowed when a curved `kurbo` shape ([`circle`],
/// [`ellipse`], [`rounded_rect`], [`arc`]) is approximated as a Bezier
/// path. Smaller values produce more vertices.
///
/// [`circle`]: super::circle
/// [`ellipse`]: super::ellipse
/// [`rounded_rect`]: super::rounded_rect
/// [`arc`]: super::arc
pub(crate) const CURVE_APPROX_TOLERANCE: f64 = 0.1;

/// Flattening tolerance the arc-length walker applies when it turns a
/// path into the polylines it measures along. Coarser than
/// [`CURVE_APPROX_TOLERANCE`] because sample *spacing* is far less
/// sensitive to chord error than a rendered outline is.
pub(crate) const SAMPLER_FLATTEN_TOLERANCE: f64 = 0.5;

/// Accuracy passed to `kurbo`'s arc-length integration and its inverse
/// when a corner fillet needs a cut distance along a curved segment.
pub(crate) const ARCLEN_ACCURACY: f64 = 1e-3;

/// Maximum chord deviation of a round cap / round join fan from the
/// true arc. Sub-pixel, so the faceting is invisible at any reasonable
/// zoom.
pub(crate) const ARC_FAN_TOLERANCE: f64 = 0.5;

/// Upper bound on the angular step of an arc fan, in radians. The
/// chord-error formula `ε = R(1 − cos(Δθ/2))` keeps the *positional*
/// deviation sub-pixel, but at small `R` (typical line half-widths of
/// 1–3 px) it picks angular steps of 60–90°, which read as visible
/// corners even though the chord error itself is sub-pixel. Capping the
/// step at 15° gives at least 12 segments per semicircle at any radius.
pub(crate) const ARC_FAN_MAX_STEP: f64 = std::f64::consts::PI / 12.0;

/// Lower bound on the angular step of an arc fan, in radians. Guards
/// the segment count against a chord step that rounds to zero at very
/// large radii.
pub(crate) const ARC_FAN_MIN_STEP: f64 = 1e-3;

/// Angular tolerance, in degrees, for calling a join collinear. A join
/// within this much of 180° (or of 0°) carries no bend worth rounding,
/// so both corner-rounding passes walk straight through it.
pub(crate) const COLLINEAR_TOL_DEG: f64 = 1e-3;

/// Length below which a vector or segment counts as having no
/// direction, so tangents fall back rather than dividing by ~zero.
///
/// Chosen at `1e-9` path units: coordinates run to a few thousand
/// pixels, where an f64 ulp is around `2e-13`, so a threshold three
/// orders of magnitude above that still catches input that accumulated
/// rounding has turned degenerate, while staying six orders below any
/// distance that could round to a distinct pixel.
pub(crate) const DEGENERATE_EPS: f64 = 1e-9;
