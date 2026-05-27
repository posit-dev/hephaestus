//! The visual range a scale maps into.
//!
//! Explicit only — no palette indirection (the user supplies the actual
//! numbers / colors / strings). Variants line up with the corresponding
//! [`Value`](crate::plot::value::Value) shapes.

use std::sync::Arc;

use crate::color::Color;

/// The output range of a [`Scale`](super::Scale) — the set of visual
/// values a domain entry can map to.
///
/// For continuous scales, ordinal-with-numeric-range scales, and similar,
/// the user supplies the literal output values (px sizes in pt, colors,
/// strings). Position scales typically leave the output range unset; the
/// `Continuous` `ScaleType` returns the normalised `[0, 1]` fraction in
/// that case (the geom converts to panel pixels at draw time).
#[derive(Clone, Debug, PartialEq)]
pub enum OutputRange {
    /// Numeric outputs in **pt** for absolute sizes, or unitless for
    /// continuous-scalar mappings.
    Numbers(Vec<f64>),
    /// String outputs (e.g. categorical text labels). Stored as
    /// `Arc<str>` so columns of identical strings dedupe cheaply.
    Strings(Vec<Arc<str>>),
    /// Color outputs (fill/stroke palettes).
    Colors(Vec<Color>),
    /// Dash-pattern outputs (linetype palettes). Each entry is an
    /// even-length pt array; empty array = solid. `Arc<[f64]>` so
    /// palette entries can be shared across rows when a column maps
    /// repeatedly to the same pattern.
    Linetypes(Vec<Arc<[f64]>>),
}

impl OutputRange {
    /// Number of entries in this output range.
    pub fn len(&self) -> usize {
        match self {
            OutputRange::Numbers(v) => v.len(),
            OutputRange::Strings(v) => v.len(),
            OutputRange::Colors(v) => v.len(),
            OutputRange::Linetypes(v) => v.len(),
        }
    }

    /// `true` if the range has no entries.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_len_and_emptiness() {
        let r = OutputRange::Numbers(vec![1.0, 2.0, 3.0]);
        assert_eq!(r.len(), 3);
        assert!(!r.is_empty());
        let e = OutputRange::Numbers(vec![]);
        assert_eq!(e.len(), 0);
        assert!(e.is_empty());
    }

    #[test]
    fn strings_eq() {
        let a = OutputRange::Strings(vec![Arc::from("a"), Arc::from("b")]);
        let b = OutputRange::Strings(vec![Arc::from("a"), Arc::from("b")]);
        assert_eq!(a, b);
    }

    #[test]
    fn colors_eq() {
        let a = OutputRange::Colors(vec![Color::new([1.0, 0.0, 0.0, 1.0])]);
        let b = OutputRange::Colors(vec![Color::new([1.0, 0.0, 0.0, 1.0])]);
        assert_eq!(a, b);
    }

    #[test]
    fn distinct_variants_not_equal() {
        let n = OutputRange::Numbers(vec![1.0]);
        let s = OutputRange::Strings(vec![Arc::from("1")]);
        assert_ne!(n, s);
    }
}
