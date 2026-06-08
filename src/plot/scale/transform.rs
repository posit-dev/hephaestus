//! Value transforms (Identity, Log, Sqrt, …) applied **inside** a scale
//! before linearisation.
//!
//! Only [`TransformKind::Identity`] is currently implemented. The other
//! variants are declared so additional [`TransformTrait`] impls can drop
//! in without touching the `Scale` struct or its mapping code;
//! constructing them via [`Transform::of`] panics until they're wired.

use std::fmt::Debug;
use std::sync::Arc;

/// Discriminator for the family of value transforms supported by scales.
///
/// Only [`TransformKind::Identity`] is implemented; the other variants
/// exist so the type compiles against all callers, but constructing them
/// via [`Transform::of`] panics until they're wired.
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

/// Behaviour of a value transform. Forward map + inverse + a hint about
/// the domain it's defined on (used by scales to validate domain
/// configuration).
pub trait TransformTrait: Debug + Send + Sync {
    /// Discriminator. Lets callers match on the family without
    /// downcasting.
    fn kind(&self) -> TransformKind;

    /// The numeric domain on which the transform is defined. Identity
    /// returns the full f64 range; Log10 returns `(0.0, +∞)`; etc.
    fn allowed_domain(&self) -> (f64, f64);

    /// Forward transform: domain value → transformed value. Behaviour at
    /// boundaries is transform-specific (Log10(0) is `-∞`, etc.).
    fn transform(&self, v: f64) -> f64;

    /// Inverse transform: transformed value → domain value.
    fn inverse(&self, v: f64) -> f64;
}

/// Type-erased transform. Wraps an `Arc<dyn TransformTrait>` so it's
/// cheap to clone and share across scales / cached chrome cells.
#[derive(Clone)]
pub struct Transform(Arc<dyn TransformTrait>);

impl Transform {
    /// The identity transform.
    pub fn identity() -> Self {
        Transform(Arc::new(Identity))
    }

    /// Construct from a [`TransformKind`]. Returns the [`Identity`]
    /// instance for [`TransformKind::Identity`]; **panics** for any other
    /// variant — only Identity is implemented.
    pub fn of(kind: TransformKind) -> Self {
        match kind {
            TransformKind::Identity => Self::identity(),
            other => panic!("Transform::{other:?} not implemented — only Identity is wired"),
        }
    }

    /// Discriminator.
    pub fn kind(&self) -> TransformKind {
        self.0.kind()
    }

    /// Forward transform.
    pub fn transform(&self, v: f64) -> f64 {
        self.0.transform(v)
    }

    /// Inverse transform.
    pub fn inverse(&self, v: f64) -> f64 {
        self.0.inverse(v)
    }

    /// The valid domain of this transform.
    pub fn allowed_domain(&self) -> (f64, f64) {
        self.0.allowed_domain()
    }
}

impl Debug for Transform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Transform").field(&self.kind()).finish()
    }
}

impl Default for Transform {
    fn default() -> Self {
        Transform::identity()
    }
}

// ─── Identity ────────────────────────────────────────────────────────────────

/// The identity transform — `y = x`, allowed on the full f64 range.
#[derive(Debug)]
pub(crate) struct Identity;

impl TransformTrait for Identity {
    fn kind(&self) -> TransformKind {
        TransformKind::Identity
    }

    fn allowed_domain(&self) -> (f64, f64) {
        (f64::NEG_INFINITY, f64::INFINITY)
    }

    fn transform(&self, v: f64) -> f64 {
        v
    }

    fn inverse(&self, v: f64) -> f64 {
        v
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
            assert_eq!(t.transform(v), v);
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
    fn transform_clones_cheaply() {
        let a = Transform::identity();
        let b = a.clone();
        // Both Arcs point at the same Identity — pointer-level identity
        // is what we want.
        assert_eq!(a.kind(), b.kind());
    }
}
