//! [`Length`] — a numeric measurement that's either an absolute pt
//! value or a relative multiplier against a parent's resolved length.
//!
//! Mirrors ggplot2's `rel()` affordance: a sub-element's `size_pt` can
//! be `Rel(1.5)` to read as "1.5× the inherited parent size" without
//! recomputing absolute values. Every element-level numeric field that
//! benefits from inheritance (font size, linewidth, tick length,
//! margins) is a `Length`.
//!
//! Resolution is one step: `length.resolve(parent_pt)`. Walking the
//! inheritance chain is the caller's job — by the time `resolve` is
//! called, `parent_pt` is the parent's already-resolved pt value.

/// A measurement that's either absolute or relative to a parent.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Length {
    /// Absolute size in pt.
    Abs(f64),
    /// Multiplier against the inherited parent's resolved length.
    /// `Rel(1.5)` on a child = 1.5× the parent's resolved pt value.
    Rel(f64),
}

impl Length {
    /// Resolve against the parent's already-resolved pt value. For
    /// `Abs`, the parent is ignored. For `Rel(m)`, returns
    /// `parent_pt * m`.
    #[inline]
    pub fn resolve(self, parent_pt: f64) -> f64 {
        match self {
            Length::Abs(v) => v,
            Length::Rel(m) => parent_pt * m,
        }
    }

    /// `true` if this is an absolute length.
    #[inline]
    pub fn is_abs(self) -> bool {
        matches!(self, Length::Abs(_))
    }
}

/// Ergonomic constructor: `pt(11.0)` reads as a concrete 11 pt size.
#[inline]
pub const fn pt(v: f64) -> Length {
    Length::Abs(v)
}

/// Ergonomic constructor: `rel(1.5)` reads as "1.5× the inherited
/// parent size".
#[inline]
pub const fn rel(v: f64) -> Length {
    Length::Rel(v)
}

impl Default for Length {
    /// `Rel(1.0)` — "same as the parent's resolved length". A safe
    /// default for sub-element fields that should inherit by default.
    fn default() -> Self {
        Length::Rel(1.0)
    }
}

/// Four-sided spacing in pt, with each side an independent
/// [`Length`]. Resolves against an outer parent measurement (typically
/// the element's `size_pt` for text margins, or a fixed pt for
/// container padding).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Margin {
    /// Top edge length.
    pub top: Length,
    /// Right edge length.
    pub right: Length,
    /// Bottom edge length.
    pub bottom: Length,
    /// Left edge length.
    pub left: Length,
}

impl Margin {
    /// Construct a margin with all four sides set to `v`.
    #[inline]
    pub const fn all(v: Length) -> Self {
        Self {
            top: v,
            right: v,
            bottom: v,
            left: v,
        }
    }

    /// Construct a margin with explicit per-side values.
    #[inline]
    pub const fn new(top: Length, right: Length, bottom: Length, left: Length) -> Self {
        Self {
            top,
            right,
            bottom,
            left,
        }
    }

    /// Resolve every side against `parent_pt`, returning a fully
    /// concretized `(top, right, bottom, left)` tuple in pt.
    #[inline]
    pub fn resolve(&self, parent_pt: f64) -> (f64, f64, f64, f64) {
        (
            self.top.resolve(parent_pt),
            self.right.resolve(parent_pt),
            self.bottom.resolve(parent_pt),
            self.left.resolve(parent_pt),
        )
    }

    /// A zero-length margin on every side.
    pub const ZERO: Margin = Margin::all(Length::Abs(0.0));
}

impl Default for Margin {
    /// Zero on every side.
    fn default() -> Self {
        Self::ZERO
    }
}
