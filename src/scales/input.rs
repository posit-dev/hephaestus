//! The data domain a scale maps from.
//!
//! Two variants: continuous (closed numeric interval) and discrete (an
//! explicit list of [`Value`]s, typically string categories).
//!
//! Temporal data flows through `Continuous { min, max }` after projection
//! to f64 (Date → days, DateTime/Time/Duration → microseconds — see the
//! [`Value`](crate::scales::value::Value) docs). The `Scale` exposes
//! ergonomic constructors that build the f64 domain from
//! [`Date`](crate::scales::value::Date) /
//! [`DateTime`](crate::scales::value::DateTime) inputs so user code stays
//! calendar-native.

use crate::scales::value::Value;

/// The input range of a [`Scale`](super::Scale).
#[derive(Clone, Debug)]
pub enum InputRange {
    /// Continuous numeric domain, inclusive of both endpoints. Temporal
    /// data lands here after the f64 projection.
    Continuous { min: f64, max: f64 },
    /// Discrete domain — an explicit list of values, in user-defined order
    /// (used by Ordinal scales for category-to-output mapping and band
    /// generation).
    Discrete(Vec<Value>),
}

impl InputRange {
    /// Width of a continuous range, or `None` for a discrete range.
    pub fn extent(&self) -> Option<f64> {
        match self {
            InputRange::Continuous { min, max } => Some(max - min),
            InputRange::Discrete(_) => None,
        }
    }

    /// Number of entries in a discrete range, or `None` for a continuous
    /// range. Continuous ranges return `None` rather than 0 to disambiguate.
    pub fn discrete_len(&self) -> Option<usize> {
        match self {
            InputRange::Continuous { .. } => None,
            InputRange::Discrete(v) => Some(v.len()),
        }
    }
}

/// Deterministic equality for input ranges. We can't derive `PartialEq`
/// directly because [`Value`] doesn't implement it (NaN semantics). Two
/// discrete ranges are equal when their value vectors are pairwise
/// [`Value::key_eq`].
impl PartialEq for InputRange {
    fn eq(&self, other: &InputRange) -> bool {
        match (self, other) {
            (
                InputRange::Continuous { min: a0, max: a1 },
                InputRange::Continuous { min: b0, max: b1 },
            ) => a0 == b0 && a1 == b1,
            (InputRange::Discrete(a), InputRange::Discrete(b)) => {
                a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.key_eq(y))
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continuous_extent() {
        let r = InputRange::Continuous {
            min: 0.0,
            max: 10.0,
        };
        assert_eq!(r.extent(), Some(10.0));
        assert_eq!(r.discrete_len(), None);
    }

    #[test]
    fn discrete_len() {
        let r = InputRange::Discrete(vec![
            Value::Number(1.0),
            Value::Number(2.0),
            Value::Number(3.0),
        ]);
        assert_eq!(r.discrete_len(), Some(3));
        assert_eq!(r.extent(), None);
    }

    #[test]
    fn continuous_equality() {
        let a = InputRange::Continuous { min: 0.0, max: 1.0 };
        let b = InputRange::Continuous { min: 0.0, max: 1.0 };
        let c = InputRange::Continuous { min: 0.0, max: 2.0 };
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn discrete_equality_via_key_eq() {
        let a = InputRange::Discrete(vec![Value::Number(1.0), Value::Number(2.0)]);
        let b = InputRange::Discrete(vec![Value::Number(1.0), Value::Number(2.0)]);
        assert_eq!(a, b);
    }

    #[test]
    fn discrete_equality_distinguishes_variants() {
        let a = InputRange::Discrete(vec![Value::Number(1.0)]);
        let b = InputRange::Discrete(vec![Value::Date(1)]);
        // Both project to 1.0 numerically, but the variant differs —
        // diff keys must distinguish.
        assert_ne!(a, b);
    }

    #[test]
    fn continuous_and_discrete_not_equal() {
        let a = InputRange::Continuous { min: 0.0, max: 1.0 };
        let b = InputRange::Discrete(vec![Value::Number(0.0), Value::Number(1.0)]);
        assert_ne!(a, b);
    }
}
