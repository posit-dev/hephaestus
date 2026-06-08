//! Value transforms (Identity, Log, Sqrt, …) applied **inside** a scale
//! before linearisation.
//!
//! Only [`TransformKind::Identity`] is currently implemented. The other
//! variants are declared so callers can match on them exhaustively; the
//! free functions [`transform_forward`], [`transform_inverse`], and
//! [`transform_allowed_domain`] panic for any non-Identity variant until
//! it is wired in.

/// Discriminator for the family of value transforms supported by scales.
///
/// Only [`TransformKind::Identity`] is implemented; the other variants
/// exist so callers can match on them exhaustively. The dispatch
/// functions ([`transform_forward`] etc.) panic for any unwired variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TransformKind {
    /// `y = x`. The only implemented variant.
    Identity,
    /// `y = log10(x)`. Not implemented.
    Log10,
    /// `y = log2(x)`. Not implemented.
    Log2,
    /// `y = ln(x)`. Not implemented.
    Log,
    /// `y = sqrt(x)`. Not implemented.
    Sqrt,
    /// `y = x²`. Not implemented.
    Square,
    /// `y = 10^x`. Not implemented.
    Exp10,
    /// `y = 2^x`. Not implemented.
    Exp2,
    /// `y = e^x`. Not implemented.
    Exp,
    /// `y = asinh(x)`. Like log but handles zero / negatives. Not implemented.
    Asinh,
    /// Pseudo-log: linear near zero, log far away. Not implemented.
    PseudoLog,
}

/// Configured value transform. A plain POD bundle of the discriminator
/// plus future per-kind parameters. Cheap to clone (`Copy`).
///
/// Construct via [`Transform::identity`] or [`Transform::of`]. The latter
/// panics for any non-Identity variant until it's wired.
///
/// The thin convenience methods ([`Self::forward`], [`Self::inverse`])
/// delegate to the free functions [`transform_forward`] and
/// [`transform_inverse`]; downstream consumers that prefer the free-fn
/// API can ignore the methods.
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

    /// Construct from a [`TransformKind`]. Returns the identity instance
    /// for [`TransformKind::Identity`]; **panics** for any other variant
    /// — only Identity is implemented.
    pub fn of(kind: TransformKind) -> Self {
        match kind {
            TransformKind::Identity => Self::identity(),
            other => panic!("Transform::{other:?} not implemented — only Identity is wired"),
        }
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
/// boundaries is transform-specific (e.g. `Log10(0)` is `-∞`).
///
/// Currently only [`TransformKind::Identity`] is wired; any other variant
/// panics. New transforms wire in by extending this `match` and the
/// matching arms in [`transform_inverse`] / [`transform_allowed_domain`].
pub fn transform_forward(v: f64, kind: TransformKind) -> f64 {
    match kind {
        TransformKind::Identity => v,
        other => panic!("transform_forward::{other:?} not implemented"),
    }
}

/// Inverse transform: transformed value → domain value.
pub fn transform_inverse(v: f64, kind: TransformKind) -> f64 {
    match kind {
        TransformKind::Identity => v,
        other => panic!("transform_inverse::{other:?} not implemented"),
    }
}

/// The numeric domain on which this transform is defined. Identity
/// covers the full f64 range; Log10 / Log2 / Log return `(0, +∞)`; etc.
pub fn transform_allowed_domain(kind: TransformKind) -> (f64, f64) {
    match kind {
        TransformKind::Identity => (f64::NEG_INFINITY, f64::INFINITY),
        other => panic!("transform_allowed_domain::{other:?} not implemented"),
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_kind() {
        let t = Transform::identity();
        assert_eq!(t.kind(), TransformKind::Identity);
    }

    #[test]
    fn identity_round_trips_values() {
        let t = Transform::identity();
        for v in [-1e9, -1.5, -1.0, 0.0, 1.0, 42.0, 1e9] {
            assert_eq!(t.forward(v), v);
            assert_eq!(t.inverse(v), v);
        }
    }

    #[test]
    fn identity_allowed_domain_is_full() {
        let (lo, hi) = Transform::identity().allowed_domain();
        assert_eq!(lo, f64::NEG_INFINITY);
        assert_eq!(hi, f64::INFINITY);
    }

    #[test]
    fn of_identity_matches_identity_constructor() {
        let a = Transform::identity();
        let b = Transform::of(TransformKind::Identity);
        assert_eq!(a.kind(), b.kind());
    }

    #[test]
    fn default_is_identity() {
        let t = Transform::default();
        assert_eq!(t.kind(), TransformKind::Identity);
    }

    #[test]
    fn free_fn_forward_matches_method() {
        assert_eq!(transform_forward(3.5, TransformKind::Identity), 3.5);
    }

    #[test]
    fn free_fn_inverse_matches_method() {
        assert_eq!(transform_inverse(3.5, TransformKind::Identity), 3.5);
    }

    #[test]
    #[should_panic(expected = "not implemented")]
    fn of_log10_panics_when_unimplemented() {
        let _ = Transform::of(TransformKind::Log10);
    }

    #[test]
    #[should_panic(expected = "not implemented")]
    fn of_sqrt_panics_when_unimplemented() {
        let _ = Transform::of(TransformKind::Sqrt);
    }

    #[test]
    #[should_panic(expected = "not implemented")]
    fn forward_log10_panics_when_unimplemented() {
        let _ = transform_forward(1.0, TransformKind::Log10);
    }
}
