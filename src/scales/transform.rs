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
