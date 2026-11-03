//! Value transforms applied **inside** a continuous scale before
//! linearisation.
//!
//! Identity / Log10 / Log2 / Log / Sqrt / Square / Exp10 / Exp2 / Exp /
//! Asinh / PseudoLog / PseudoLog2 / PseudoLog10 are all wired. The
//! PseudoLog family follows ggsql's convention: `asinh(x/2) / ln(base)`,
//! a symmetric "linear near zero, log far away" mapping. Square's inverse
//! takes the non-negative branch.

/// Discriminator for the family of value transforms supported by scales.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TransformKind {
    /// `y = x`.
    Identity,
    /// `y = log10(x)`. Domain `(0, +∞)`.
    Log10,
    /// `y = log2(x)`. Domain `(0, +∞)`.
    Log2,
    /// `y = ln(x)`. Domain `(0, +∞)`.
    Log,
    /// `y = sqrt(x)`. Domain `[0, +∞)`.
    Sqrt,
    /// `y = x²`. Inverse takes the non-negative branch.
    Square,
    /// `y = 10^x`.
    Exp10,
    /// `y = 2^x`.
    Exp2,
    /// `y = e^x`.
    Exp,
    /// `y = asinh(x)`. Symmetric log alternative defined on the full
    /// real line; sigma is hardcoded at 1 (no parameter).
    Asinh,
    /// Pseudo-log with natural-log base: `y = asinh(x/2) / ln(e) =
    /// asinh(x/2)`. Linear near zero, log far away. Sigma is hardcoded
    /// at 0.5 (the `/2`), matching ggsql's `pseudo_log`.
    PseudoLog,
    /// Pseudo-log with base-2: `y = asinh(x/2) / ln(2)`.
    PseudoLog2,
    /// Pseudo-log with base-10: `y = asinh(x/2) / ln(10)`.
    PseudoLog10,
}

/// Configured value transform. POD bundle of the discriminator plus
/// future per-kind parameters. Cheap to clone (`Copy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Transform {
    /// Which family this transform belongs to.
    pub kind: TransformKind,
}

impl Transform {
    /// The identity transform.
    pub const fn identity() -> Self {
        Transform {
            kind: TransformKind::Identity,
        }
    }

    /// Construct from a [`TransformKind`].
    pub const fn of(kind: TransformKind) -> Self {
        Transform { kind }
    }

    /// Discriminator.
    pub fn kind(&self) -> TransformKind {
        self.kind
    }

    /// Forward transform. Delegates to [`transform_forward`].
    pub fn forward(&self, v: f64) -> f64 {
        transform_forward(v, self.kind)
    }

    /// Inverse transform. Delegates to [`transform_inverse`].
    pub fn inverse(&self, v: f64) -> f64 {
        transform_inverse(v, self.kind)
    }

    /// The valid domain of this transform. Delegates to
    /// [`transform_allowed_domain`].
    pub fn allowed_domain(&self) -> (f64, f64) {
        transform_allowed_domain(self.kind)
    }
}

impl Default for Transform {
    fn default() -> Self {
        Transform::identity()
    }
}

// ─── Free-function dispatch ──────────────────────────────────────────────────

/// Forward transform: domain value → transformed value. Behaviour at
/// boundaries is transform-specific (e.g. `Log10(0)` returns `-∞`,
/// `Sqrt(-1)` returns `NaN`).
pub fn transform_forward(v: f64, kind: TransformKind) -> f64 {
    match kind {
        TransformKind::Identity => v,
        TransformKind::Log10 => v.log10(),
        TransformKind::Log2 => v.log2(),
        TransformKind::Log => v.ln(),
        TransformKind::Sqrt => v.sqrt(),
        TransformKind::Square => v * v,
        TransformKind::Exp10 => 10f64.powf(v),
        TransformKind::Exp2 => 2f64.powf(v),
        TransformKind::Exp => v.exp(),
        TransformKind::Asinh => v.asinh(),
        TransformKind::PseudoLog => (v * 0.5).asinh(),
        TransformKind::PseudoLog2 => (v * 0.5).asinh() / 2f64.ln(),
        TransformKind::PseudoLog10 => (v * 0.5).asinh() / 10f64.ln(),
    }
}

/// Inverse transform: transformed value → domain value. For `Square`,
/// inverse picks the non-negative branch (i.e. `sqrt(|v|)`).
pub fn transform_inverse(v: f64, kind: TransformKind) -> f64 {
    match kind {
        TransformKind::Identity => v,
        TransformKind::Log10 => 10f64.powf(v),
        TransformKind::Log2 => 2f64.powf(v),
        TransformKind::Log => v.exp(),
        TransformKind::Sqrt => v * v,
        TransformKind::Square => v.abs().sqrt(),
        TransformKind::Exp10 => v.log10(),
        TransformKind::Exp2 => v.log2(),
        TransformKind::Exp => v.ln(),
        TransformKind::Asinh => v.sinh(),
        TransformKind::PseudoLog => v.sinh() * 2.0,
        TransformKind::PseudoLog2 => (v * 2f64.ln()).sinh() * 2.0,
        TransformKind::PseudoLog10 => (v * 10f64.ln()).sinh() * 2.0,
    }
}

/// Expand a continuous domain by `mult` (a proportion of the span) plus
/// `add` (an absolute amount), applied in **transformed** space and
/// mapped back through the inverse.
///
/// Expanding before transforming breaks down for the log family: data
/// spanning decades pads the lower bound past zero, and clipping it back
/// to the transform's domain pins it at the smallest positive float —
/// a 3-decade axis becomes a 311-decade one (ggsql#512). Padding the
/// transformed range instead keeps the result inside the allowed domain
/// by construction, so the pad reads as a proportion of the visible
/// (log-space) span, matching what a reader sees on the axis.
///
/// - `mult` / `add` are in transformed units: `add = 0.1` on a `Log10`
///   scale adds a tenth of a decade at each end.
/// - A degenerate (zero-width) range is padded as if it spanned 1
///   transformed unit.
/// - If the transformed endpoints are non-finite (e.g. `min <= 0` on a
///   log scale) no expansion is possible; the input range is returned
///   unchanged. Filtering out-of-domain data is the caller's job.
///
/// Symmetric shorthand for [`expand_range_transformed_selective`].
pub fn expand_range_transformed(
    min: f64,
    max: f64,
    mult: f64,
    add: f64,
    kind: TransformKind,
) -> (f64, f64) {
    expand_range_transformed_selective(min, max, (mult, mult), (add, add), kind)
}

/// [`expand_range_transformed`] with per-end expansion: `mult` / `add`
/// are `(lower, upper)` pairs applied to the low and high end of the
/// transformed range respectively. Pass `0.0` for an end that should
/// stay put — e.g. `(0.0, 0.05)` keeps a bar chart's baseline fixed
/// while padding only the data end, the ggplot2
/// `expansion(mult = c(0, 0.05))` idiom.
pub fn expand_range_transformed_selective(
    min: f64,
    max: f64,
    mult: (f64, f64),
    add: (f64, f64),
    kind: TransformKind,
) -> (f64, f64) {
    let (lo, hi) = if min <= max { (min, max) } else { (max, min) };
    let t_lo = transform_forward(lo, kind);
    let t_hi = transform_forward(hi, kind);
    if !t_lo.is_finite() || !t_hi.is_finite() {
        return (lo, hi);
    }
    let span = t_hi - t_lo;
    let span = if span > 0.0 { span } else { 1.0 };
    let new_lo = t_lo - mult.0 * span - add.0;
    let new_hi = t_hi + mult.1 * span + add.1;
    (
        transform_inverse(new_lo, kind),
        transform_inverse(new_hi, kind),
    )
}

/// The numeric domain on which this transform is defined.
pub fn transform_allowed_domain(kind: TransformKind) -> (f64, f64) {
    match kind {
        TransformKind::Log10 | TransformKind::Log2 | TransformKind::Log => (0.0, f64::INFINITY),
        TransformKind::Sqrt => (0.0, f64::INFINITY),
        TransformKind::Identity
        | TransformKind::Square
        | TransformKind::Exp10
        | TransformKind::Exp2
        | TransformKind::Exp
        | TransformKind::Asinh
        | TransformKind::PseudoLog
        | TransformKind::PseudoLog2
        | TransformKind::PseudoLog10 => (f64::NEG_INFINITY, f64::INFINITY),
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64, tol: f64) {
        assert!((a - b).abs() < tol, "{a} ≠ {b} (tol={tol})");
    }

    #[test]
    fn identity_round_trips() {
        for v in [-1e9, -1.5, 0.0, 1.0, 42.0, 1e9] {
            approx(transform_forward(v, TransformKind::Identity), v, 1e-12);
            approx(transform_inverse(v, TransformKind::Identity), v, 1e-12);
        }
    }

    #[test]
    fn log10_round_trip() {
        for v in [0.5, 1.0, 2.0, 5.0, 10.0, 100.0, 1e6] {
            let f = transform_forward(v, TransformKind::Log10);
            let inv = transform_inverse(f, TransformKind::Log10);
            approx(inv, v, 1e-9);
        }
        approx(transform_forward(10.0, TransformKind::Log10), 1.0, 1e-12);
        approx(transform_forward(100.0, TransformKind::Log10), 2.0, 1e-12);
    }

    #[test]
    fn log2_round_trip() {
        for v in [0.5, 1.0, 2.0, 4.0, 16.0] {
            let f = transform_forward(v, TransformKind::Log2);
            let inv = transform_inverse(f, TransformKind::Log2);
            approx(inv, v, 1e-9);
        }
        approx(transform_forward(8.0, TransformKind::Log2), 3.0, 1e-12);
    }

    #[test]
    fn log_natural_round_trip() {
        for v in [0.5, 1.0, std::f64::consts::E, 100.0] {
            let f = transform_forward(v, TransformKind::Log);
            let inv = transform_inverse(f, TransformKind::Log);
            approx(inv, v, 1e-9);
        }
        approx(
            transform_forward(std::f64::consts::E, TransformKind::Log),
            1.0,
            1e-12,
        );
    }

    #[test]
    fn sqrt_round_trip() {
        for v in [0.0, 0.25, 1.0, 4.0, 100.0] {
            let f = transform_forward(v, TransformKind::Sqrt);
            let inv = transform_inverse(f, TransformKind::Sqrt);
            approx(inv, v, 1e-9);
        }
    }

    #[test]
    fn square_inverse_is_non_negative_branch() {
        // Square is non-injective; inverse takes the |v| branch.
        approx(transform_forward(3.0, TransformKind::Square), 9.0, 1e-12);
        approx(transform_inverse(9.0, TransformKind::Square), 3.0, 1e-12);
        approx(transform_inverse(-9.0, TransformKind::Square), 3.0, 1e-12);
    }

    #[test]
    fn exp_round_trips() {
        for kind in [
            TransformKind::Exp10,
            TransformKind::Exp2,
            TransformKind::Exp,
        ] {
            for v in [0.0, 1.0, 2.5, -1.0] {
                let f = transform_forward(v, kind);
                let inv = transform_inverse(f, kind);
                approx(inv, v, 1e-9);
            }
        }
    }

    #[test]
    fn asinh_round_trip() {
        for v in [-100.0, -1.0, 0.0, 1.0, 100.0] {
            let f = transform_forward(v, TransformKind::Asinh);
            let inv = transform_inverse(f, TransformKind::Asinh);
            approx(inv, v, 1e-9);
        }
    }

    #[test]
    fn pseudo_log_round_trips_all_bases() {
        for kind in [
            TransformKind::PseudoLog,
            TransformKind::PseudoLog2,
            TransformKind::PseudoLog10,
        ] {
            for v in [-1000.0, -1.0, 0.0, 1.0, 1000.0] {
                let f = transform_forward(v, kind);
                let inv = transform_inverse(f, kind);
                approx(inv, v, 1e-6);
            }
        }
    }

    #[test]
    fn pseudo_log_linear_near_zero() {
        // For small |x|, asinh(x/2) ≈ x/2 so PseudoLog ≈ x/2.
        let f = transform_forward(0.01, TransformKind::PseudoLog);
        approx(f, 0.005, 1e-4);
    }

    #[test]
    fn pseudo_log_far_from_zero_is_log_like() {
        // For large |x|, asinh(x/2) ≈ ln(x), so PseudoLog10(x) ≈ log10(x).
        let f = transform_forward(1000.0, TransformKind::PseudoLog10);
        let log10_1000 = 1000f64.log10();
        // Within ~5% — pseudo-log diverges from log10 by a constant
        // offset (ln 2 / ln 10).
        assert!(
            (f - log10_1000).abs() < 0.5,
            "pseudolog10(1000) = {f}, log10(1000) = {log10_1000}"
        );
    }

    #[test]
    fn allowed_domain_log_is_positive() {
        let (lo, hi) = transform_allowed_domain(TransformKind::Log10);
        assert_eq!(lo, 0.0);
        assert_eq!(hi, f64::INFINITY);
    }

    #[test]
    fn allowed_domain_sqrt_is_non_negative() {
        let (lo, hi) = transform_allowed_domain(TransformKind::Sqrt);
        assert_eq!(lo, 0.0);
        assert_eq!(hi, f64::INFINITY);
    }

    #[test]
    fn allowed_domain_asinh_is_full() {
        let (lo, hi) = transform_allowed_domain(TransformKind::Asinh);
        assert_eq!(lo, f64::NEG_INFINITY);
        assert_eq!(hi, f64::INFINITY);
    }

    #[test]
    fn expand_identity_matches_linear_padding() {
        // Identity transform: transformed space is data space.
        let (lo, hi) = expand_range_transformed(0.0, 100.0, 0.05, 0.0, TransformKind::Identity);
        approx(lo, -5.0, 1e-12);
        approx(hi, 105.0, 1e-12);
    }

    #[test]
    fn expand_log10_pads_decades_not_data() {
        // ggsql#512: data 1..1000 with a 5% pad must not collapse the
        // lower bound to f64::MIN_POSITIVE. In log10 space the range is
        // [0, 3], so a 5% pad is 0.15 of a decade at each end.
        let (lo, hi) = expand_range_transformed(1.0, 1000.0, 0.05, 0.0, TransformKind::Log10);
        approx(lo, 10f64.powf(-0.15), 1e-9);
        approx(hi, 10f64.powf(3.15), 1e-6);
        assert!(lo > 0.0);
    }

    #[test]
    fn expand_log10_with_additive_pad() {
        // add is in transformed units: 0.1 of a decade per end.
        let (lo, hi) = expand_range_transformed(1.0, 100.0, 0.0, 0.1, TransformKind::Log10);
        approx(lo, 10f64.powf(-0.1), 1e-9);
        approx(hi, 10f64.powf(2.1), 1e-6);
    }

    #[test]
    fn expand_sqrt_stays_on_valid_branch() {
        // sqrt domain [0, 100] → transformed [0, 10]; 5% pad inverts to
        // 0.25, not a negative lower bound.
        let (lo, hi) = expand_range_transformed(0.0, 100.0, 0.05, 0.0, TransformKind::Sqrt);
        approx(lo, 0.25, 1e-9);
        approx(hi, 110.25, 1e-6);
    }

    #[test]
    fn expand_degenerate_range_uses_unit_span() {
        // Zero-width range pads as if it spanned 1 transformed unit.
        let (lo, hi) = expand_range_transformed(10.0, 10.0, 0.05, 0.0, TransformKind::Log10);
        approx(lo, 10f64.powf(0.95), 1e-6);
        approx(hi, 10f64.powf(1.05), 1e-6);
    }

    #[test]
    fn expand_out_of_domain_input_returns_unchanged() {
        // min <= 0 on a log scale: forward is non-finite, nothing to pad.
        let (lo, hi) = expand_range_transformed(-5.0, 100.0, 0.05, 0.0, TransformKind::Log10);
        assert_eq!((lo, hi), (-5.0, 100.0));
    }

    #[test]
    fn expand_swaps_descending_endpoints() {
        let (lo, hi) = expand_range_transformed(100.0, 0.0, 0.05, 0.0, TransformKind::Identity);
        approx(lo, -5.0, 1e-12);
        approx(hi, 105.0, 1e-12);
    }

    #[test]
    fn expand_selective_pads_only_the_upper_end() {
        // The bar-chart idiom: baseline stays, data end gets the pad.
        let (lo, hi) = expand_range_transformed_selective(
            0.0,
            100.0,
            (0.0, 0.05),
            (0.0, 0.0),
            TransformKind::Identity,
        );
        approx(lo, 0.0, 1e-12);
        approx(hi, 105.0, 1e-12);
    }

    #[test]
    fn expand_selective_per_end_on_log10() {
        // Lower end padded by a tenth of a decade, upper end fixed.
        let (lo, hi) = expand_range_transformed_selective(
            1.0,
            1000.0,
            (0.0, 0.0),
            (0.1, 0.0),
            TransformKind::Log10,
        );
        approx(lo, 10f64.powf(-0.1), 1e-9);
        approx(hi, 1000.0, 1e-9);
    }

    #[test]
    fn default_is_identity() {
        let t = Transform::default();
        assert_eq!(t.kind(), TransformKind::Identity);
    }

    #[test]
    fn of_constructs_all_variants() {
        // Previously panicked for non-Identity; now all wired.
        for kind in [
            TransformKind::Identity,
            TransformKind::Log10,
            TransformKind::Log2,
            TransformKind::Log,
            TransformKind::Sqrt,
            TransformKind::Square,
            TransformKind::Exp10,
            TransformKind::Exp2,
            TransformKind::Exp,
            TransformKind::Asinh,
            TransformKind::PseudoLog,
            TransformKind::PseudoLog2,
            TransformKind::PseudoLog10,
        ] {
            let t = Transform::of(kind);
            assert_eq!(t.kind(), kind);
        }
    }
}
