//! Marquee's four-way length model for rich text.
//!
//! Every measurement in a [`StyleDelta`](super::style::StyleDelta) is a
//! [`LengthSpec`], and the variant says what it measures against:
//!
//! - `Pt(v)` — an absolute point value.
//! - `Relative(m)` — `m ×` the **parent element's value of the same
//!   field**. A heading whose `size` is `Relative(2.25)` inside a
//!   `Relative(0.9)` div is `2.25 × 0.9 ×` the base size; the
//!   multipliers compound down the tree.
//! - `Em(m)` — `m ×` the **element's own resolved font size**. This is
//!   what makes `h1 { margin_top: em(1) }` reserve one h1-sized line
//!   rather than one base-sized line.
//! - `Rem(m)` — `m ×` the **base font size** of the whole run,
//!   unaffected by nesting.
//!
//! Resolution order matters: `size` resolves first (its `Em` is
//! degenerate — an element's size relative to its own size is just
//! `Relative`), and every other field then resolves against that new
//! own size.
//!
//! Line height gets its own [`LineHeightSpec`] because its natural
//! `Relative`-to-nothing reading is "multiple of the font size", which
//! is a different default from the other fields.

use crate::style_vocab::Length;

/// A length in marquee's four-way model. See the module docs for what
/// each variant measures against.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LengthSpec {
    /// Absolute points.
    Pt(f64),
    /// Multiplier on the parent element's value of the same field.
    Relative(f64),
    /// Multiplier on this element's own resolved font size.
    Em(f64),
    /// Multiplier on the run's base font size.
    Rem(f64),
}

impl LengthSpec {
    /// Resolve to points.
    ///
    /// `parent_pt` is the parent element's resolved value of the same
    /// field, `own_size_pt` this element's resolved font size, and
    /// `base_size_pt` the run's base font size.
    #[inline]
    pub fn resolve(self, parent_pt: f64, own_size_pt: f64, base_size_pt: f64) -> f64 {
        match self {
            LengthSpec::Pt(v) => v,
            LengthSpec::Relative(m) => parent_pt * m,
            LengthSpec::Em(m) => own_size_pt * m,
            LengthSpec::Rem(m) => base_size_pt * m,
        }
    }

    /// `true` when the value doesn't depend on any inherited context.
    #[inline]
    pub fn is_absolute(self) -> bool {
        matches!(self, LengthSpec::Pt(_))
    }
}

impl Default for LengthSpec {
    /// `Relative(1.0)` — "whatever the parent has".
    fn default() -> Self {
        LengthSpec::Relative(1.0)
    }
}

impl From<Length> for LengthSpec {
    /// Bridge from the theme's two-variant length: `Abs` is `Pt`, and
    /// `Rel` reads as a multiplier on the parent's same field.
    fn from(l: Length) -> Self {
        match l {
            Length::Abs(v) => LengthSpec::Pt(v),
            Length::Rel(m) => LengthSpec::Relative(m),
        }
    }
}

/// Absolute points.
#[inline]
pub const fn pt(v: f64) -> LengthSpec {
    LengthSpec::Pt(v)
}

/// Multiplier on the parent element's value of the same field.
#[inline]
pub const fn relative(m: f64) -> LengthSpec {
    LengthSpec::Relative(m)
}

/// Multiplier on this element's own resolved font size.
#[inline]
pub const fn em(m: f64) -> LengthSpec {
    LengthSpec::Em(m)
}

/// Multiplier on the run's base font size.
#[inline]
pub const fn rem(m: f64) -> LengthSpec {
    LengthSpec::Rem(m)
}

/// Four-sided spacing, each side an independent [`LengthSpec`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RichMargin {
    /// Top edge.
    pub top: LengthSpec,
    /// Right edge — a logical end side on block-level spacing, swapped
    /// to the physical left under Rtl.
    pub right: LengthSpec,
    /// Bottom edge.
    pub bottom: LengthSpec,
    /// Left edge — a logical start side on block-level spacing.
    pub left: LengthSpec,
}

impl RichMargin {
    /// All four sides the same.
    #[inline]
    pub const fn all(v: LengthSpec) -> Self {
        Self {
            top: v,
            right: v,
            bottom: v,
            left: v,
        }
    }

    /// Explicit per-side values, in `top, right, bottom, left` order.
    #[inline]
    pub const fn new(
        top: LengthSpec,
        right: LengthSpec,
        bottom: LengthSpec,
        left: LengthSpec,
    ) -> Self {
        Self {
            top,
            right,
            bottom,
            left,
        }
    }

    /// Resolve every side to points against the parent's already-
    /// resolved four sides.
    #[inline]
    pub fn resolve(&self, parent: [f64; 4], own_size_pt: f64, base_size_pt: f64) -> [f64; 4] {
        [
            self.top.resolve(parent[0], own_size_pt, base_size_pt),
            self.right.resolve(parent[1], own_size_pt, base_size_pt),
            self.bottom.resolve(parent[2], own_size_pt, base_size_pt),
            self.left.resolve(parent[3], own_size_pt, base_size_pt),
        ]
    }

    /// Zero on every side.
    pub const ZERO: RichMargin = RichMargin::all(LengthSpec::Pt(0.0));
}

impl Default for RichMargin {
    /// Zero on every side.
    fn default() -> Self {
        Self::ZERO
    }
}

/// Swap the left and right components of a resolved `[top, right,
/// bottom, left]` tuple when the block axis runs right-to-left.
///
/// Block-level `padding` / `margin` / `border_width` name **logical**
/// start and end sides: a blockquote that sets `left = 3` means
/// "start-side bar", which paints on the physical right under Rtl.
#[inline]
pub fn swap_lr(sides: [f64; 4], is_rtl: bool) -> [f64; 4] {
    if is_rtl {
        [sides[0], sides[3], sides[2], sides[1]]
    } else {
        sides
    }
}

/// Line height, which measures against the font size by default rather
/// than against the parent's line height.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LineHeightSpec {
    /// Multiple of the element's own font size — the CSS
    /// `line-height: 1.6` reading.
    Mult(f64),
    /// Multiplier on the parent element's resolved line height, in the
    /// same units the parent carried.
    Relative(f64),
    /// Absolute points.
    Pt(f64),
}

impl LineHeightSpec {
    /// Resolve against the parent's line height. A `Relative` child of
    /// a `Mult` parent stays a multiple; a `Relative` child of a `Pt`
    /// parent stays absolute.
    #[inline]
    pub fn resolve(self, parent: LineHeightSpec) -> LineHeightSpec {
        match self {
            LineHeightSpec::Relative(m) => match parent {
                LineHeightSpec::Mult(p) => LineHeightSpec::Mult(p * m),
                LineHeightSpec::Relative(p) => LineHeightSpec::Mult(p * m),
                LineHeightSpec::Pt(p) => LineHeightSpec::Pt(p * m),
            },
            other => other,
        }
    }
}

impl Default for LineHeightSpec {
    /// `Mult(1.0)` — single-spaced.
    fn default() -> Self {
        LineHeightSpec::Mult(1.0)
    }
}

/// One inheritable style field, as addressed by
/// [`StyleDelta::skip_inherit`](super::style::StyleDelta::skip_inherit).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StyleField {
    /// Font family.
    Family,
    /// Font weight.
    Weight,
    /// Italic flag.
    Italic,
    /// Font width ratio.
    Width,
    /// Font size.
    Size,
    /// Text colour.
    Color,
    /// Letter spacing.
    Tracking,
    /// Underline flag.
    Underline,
    /// Strikethrough flag.
    Strikethrough,
    /// Baseline shift.
    Baseline,
    /// Glyph outline colour.
    TextStroke,
    /// Glyph outline width.
    TextStrokeWidth,
    /// Line height.
    LineHeight,
    /// Horizontal alignment.
    Align,
    /// First-line indent.
    Indent,
    /// Continuation-line indent.
    Hanging,
    /// Outer spacing.
    Margin,
    /// Inner spacing.
    Padding,
    /// Block background.
    Background,
    /// Block border colour.
    BorderColor,
    /// Block border widths.
    BorderWidth,
    /// Block border corner radius.
    BorderRadius,
    /// Block bullet markers.
    Bullet,
}

impl StyleField {
    /// Every addressable field, for iteration.
    pub const ALL: [StyleField; 23] = [
        StyleField::Family,
        StyleField::Weight,
        StyleField::Italic,
        StyleField::Width,
        StyleField::Size,
        StyleField::Color,
        StyleField::Tracking,
        StyleField::Underline,
        StyleField::Strikethrough,
        StyleField::Baseline,
        StyleField::TextStroke,
        StyleField::TextStrokeWidth,
        StyleField::LineHeight,
        StyleField::Align,
        StyleField::Indent,
        StyleField::Hanging,
        StyleField::Margin,
        StyleField::Padding,
        StyleField::Background,
        StyleField::BorderColor,
        StyleField::BorderWidth,
        StyleField::BorderRadius,
        StyleField::Bullet,
    ];

    #[inline]
    fn bit(self) -> u32 {
        self as u32
    }
}

/// Set of [`StyleField`]s, held as a bitset.
///
/// A field in the set inherits from its **grandparent** instead of its
/// parent — marquee's `skip_inherit`, which is how `sup` inside `sup`
/// stops shrinking without number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FieldSet(u32);

impl FieldSet {
    /// The empty set — everything inherits normally.
    pub const NONE: FieldSet = FieldSet(0);

    /// Build a set from a list of fields.
    pub fn of(fields: &[StyleField]) -> Self {
        let mut bits = 0u32;
        for f in fields {
            bits |= 1 << f.bit();
        }
        FieldSet(bits)
    }

    /// Whether `field` is in the set.
    #[inline]
    pub fn contains(self, field: StyleField) -> bool {
        self.0 & (1 << field.bit()) != 0
    }

    /// Whether the set is empty.
    #[inline]
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Union of two sets.
    #[inline]
    pub fn union(self, other: FieldSet) -> FieldSet {
        FieldSet(self.0 | other.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_variant_measures_against_its_own_anchor() {
        // parent = 20pt, own size = 30pt, base = 10pt.
        assert_eq!(pt(7.0).resolve(20.0, 30.0, 10.0), 7.0);
        assert_eq!(relative(2.0).resolve(20.0, 30.0, 10.0), 40.0);
        assert_eq!(em(2.0).resolve(20.0, 30.0, 10.0), 60.0);
        assert_eq!(rem(2.0).resolve(20.0, 30.0, 10.0), 20.0);
    }

    #[test]
    fn margin_resolves_each_side_against_the_matching_parent_side() {
        let m = RichMargin::new(relative(2.0), pt(1.0), em(0.5), rem(0.25));
        let out = m.resolve([3.0, 4.0, 5.0, 6.0], 10.0, 8.0);
        assert_eq!(out, [6.0, 1.0, 5.0, 2.0]);
    }

    #[test]
    fn swap_lr_flips_only_under_rtl() {
        assert_eq!(swap_lr([1.0, 2.0, 3.0, 4.0], false), [1.0, 2.0, 3.0, 4.0]);
        assert_eq!(swap_lr([1.0, 2.0, 3.0, 4.0], true), [1.0, 4.0, 3.0, 2.0]);
    }

    #[test]
    fn relative_lineheight_compounds_within_the_parent_kind() {
        assert_eq!(
            LineHeightSpec::Relative(0.5).resolve(LineHeightSpec::Mult(1.6)),
            LineHeightSpec::Mult(0.8)
        );
        assert_eq!(
            LineHeightSpec::Relative(0.5).resolve(LineHeightSpec::Pt(20.0)),
            LineHeightSpec::Pt(10.0)
        );
        assert_eq!(
            LineHeightSpec::Mult(2.0).resolve(LineHeightSpec::Pt(20.0)),
            LineHeightSpec::Mult(2.0)
        );
    }

    #[test]
    fn field_set_membership_is_per_field() {
        let s = FieldSet::of(&[StyleField::Size, StyleField::Baseline]);
        assert!(s.contains(StyleField::Size));
        assert!(s.contains(StyleField::Baseline));
        assert!(!s.contains(StyleField::Margin));
        assert!(FieldSet::NONE.is_empty());
        // Every field must occupy a distinct bit.
        for f in StyleField::ALL {
            let only = FieldSet::of(&[f]);
            for g in StyleField::ALL {
                assert_eq!(only.contains(g), f == g, "{f:?} vs {g:?}");
            }
        }
    }
}
