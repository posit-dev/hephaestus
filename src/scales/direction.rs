//! Which way a scale's output runs across its domain.
//!
//! Reversal is a property of the *mapping*, not of the domain: the domain
//! stays in its natural order (ascending numbers, user-ordered
//! categories, increasing bin edges) and [`Direction`] decides which end
//! of the output it lands on. Break generation is therefore untouched by
//! reversal — a reversed axis emits the same tick values as a forward
//! one, positioned at the mirrored fractions.

/// Which way a scale's output runs across its domain.
///
/// Applied to the normalised `[0, 1]` fraction (or the domain index)
/// *before* the output range is consulted, so one flag covers both roles
/// a scale can play: a position scale's axis runs backwards, and a
/// material scale walks its palette from the far end.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Direction {
    /// The domain's low end maps to the low end of the output.
    #[default]
    Forward,
    /// The domain's low end maps to the high end of the output.
    Reversed,
}

impl Direction {
    /// True for [`Self::Reversed`].
    pub fn is_reversed(self) -> bool {
        matches!(self, Direction::Reversed)
    }

    /// Flip a normalised fraction about `0.5` when reversed; pass it
    /// through otherwise. Unclamped, so an extrapolating fraction
    /// mirrors past the same end it overshot.
    pub fn apply_fraction(self, t: f64) -> f64 {
        match self {
            Direction::Forward => t,
            Direction::Reversed => 1.0 - t,
        }
    }

    /// Mirror an index into an `n`-entry sequence when reversed; pass it
    /// through otherwise. Out-of-bounds indices and an empty sequence
    /// pass through untouched.
    pub fn apply_index(self, idx: usize, n: usize) -> usize {
        match self {
            Direction::Reversed if idx < n => n - 1 - idx,
            _ => idx,
        }
    }
}
