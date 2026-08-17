//! Anchor points for positioning a [`RichTextRun`] within a target
//! region. Mirrors marquee's `hjust` / `vjust` vocabulary 1:1 — every
//! anchor there has a direct equivalent here.
//!
//! **Horizontal anchors** ([`HAnchor`]) — where on the text's inline
//! dimension the caller-supplied `x` lands:
//! - `Left` / `Right` — the outer bounding-box edges (line-box edge,
//!   including any leading whitespace and centred-line padding).
//! - `LeftInk` / `RightInk` — the actual ink extents (skips lines
//!   that don't reach the bounding-box edge, useful for irregular
//!   ragged text).
//! - `Center` / `CenterInk` — midpoints of the two above ranges.
//! - `Fraction(f)` — arbitrary fraction, `0.0` = left, `1.0` = right.
//!
//! **Vertical anchors** ([`VAnchor`]) — where on the text's block
//! dimension the caller-supplied `y` lands:
//! - `Top` / `Bottom` — the outer bounding-box edges (line-box top of
//!   the first line, line-box bottom of the last).
//! - `TopInk` / `BottomInk` — the ink extents (first-line ascender
//!   top, last-line descender bottom).
//! - `FirstLine` / `LastLine` — the baseline of the first / last
//!   line specifically. Useful when a caller wants a rich text block
//!   to sit on the same baseline as adjacent plain text.
//! - `Center` / `CenterInk` — midpoints of the two ranges.
//! - `Fraction(f)` — arbitrary fraction, `0.0` = top, `1.0` = bottom.
//!
//! `Center` / `Top` / etc. use the parley line-box which includes
//! half-leading above the first line and below the last; the `-Ink`
//! variants hug the visible glyph column, which is what most
//! typographers want for tight vertical placement.

/// Horizontal anchor on a rich-text layout. See module docs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HAnchor {
    /// Left of the bounding box (x = 0 in layout coords).
    Left,
    /// Left of the ink — smallest `inline_min_coord` across all lines.
    LeftInk,
    /// Bounding-box centre.
    Center,
    /// Ink centre (midpoint of `LeftInk` and `RightInk`).
    CenterInk,
    /// Right of the ink — largest `inline_max_coord` across all lines.
    RightInk,
    /// Right of the bounding box (x = layout width).
    Right,
    /// Custom fraction of the bounding-box width, `0.0..=1.0`.
    Fraction(f32),
}

impl Default for HAnchor {
    /// `Left` — matches marquee's default of `hjust = 0`.
    fn default() -> Self {
        HAnchor::Left
    }
}

/// Vertical anchor on a rich-text layout. See module docs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum VAnchor {
    /// Top of the bounding box (y = 0 in layout coords).
    Top,
    /// Top of the ink — smallest `block_min_coord` across all lines.
    TopInk,
    /// Baseline of the first line.
    FirstLine,
    /// Bounding-box centre.
    Center,
    /// Ink centre (midpoint of `TopInk` and `BottomInk`).
    CenterInk,
    /// Baseline of the last line.
    LastLine,
    /// Bottom of the ink — largest `block_max_coord` across all lines.
    BottomInk,
    /// Bottom of the bounding box (y = layout height).
    Bottom,
    /// Custom fraction of the bounding-box height, `0.0..=1.0`.
    Fraction(f32),
}

impl Default for VAnchor {
    /// `Top` — matches marquee's default of `vjust = 1` inverted (top
    /// of block).
    fn default() -> Self {
        VAnchor::Top
    }
}

/// Combined horizontal + vertical anchor. `RichAnchor::default()`
/// is `Left` × `Top` — the layout box's top-left corner, matching
/// [`crate::text::draw_text`]'s implicit anchor.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct RichAnchor {
    /// Horizontal anchor.
    pub h: HAnchor,
    /// Vertical anchor.
    pub v: VAnchor,
}

impl RichAnchor {
    /// Convenience: top-left (the implicit default).
    pub fn top_left() -> Self {
        Self {
            h: HAnchor::Left,
            v: VAnchor::Top,
        }
    }

    /// Convenience: centred both ways (bounding-box centre).
    pub fn center() -> Self {
        Self {
            h: HAnchor::Center,
            v: VAnchor::Center,
        }
    }

    /// Convenience: centred both ways using ink extents.
    pub fn center_ink() -> Self {
        Self {
            h: HAnchor::CenterInk,
            v: VAnchor::CenterInk,
        }
    }

    /// Convenience: horizontally left, vertically on the first-line
    /// baseline — matches what a caller wants when placing rich text
    /// inline alongside plain text sharing the same baseline.
    pub fn first_line_baseline() -> Self {
        Self {
            h: HAnchor::Left,
            v: VAnchor::FirstLine,
        }
    }
}

/// Layout-local reference offsets a [`RichAnchor`] resolves to.
/// `(ref_x, ref_y)` is the point *within the laid-out box* that the
/// caller's `(x, y)` should coincide with. The draw code translates
/// the layout's top-left to `(x - ref_x, y - ref_y)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnchorOffsets {
    /// Layout-local x that maps to the caller's anchor x.
    pub ref_x: f32,
    /// Layout-local y that maps to the caller's anchor y.
    pub ref_y: f32,
}

/// Reference metrics a [`RichAnchor`] resolves against. Produced
/// by [`crate::text::rich::RichTextRun::layout_bounds`] by walking
/// the parley layout's per-line metrics once. Exposed publicly so
/// callers who want to implement bespoke anchoring rules can read
/// the same numbers our built-in [`RichAnchor`] uses.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayoutBounds {
    /// Bounding-box width (layout advance width).
    pub width: f32,
    /// Bounding-box height (layout height, includes leading).
    pub height: f32,
    /// Ink left edge — min `inline_min_coord` across lines.
    pub ink_left: f32,
    /// Ink right edge — max `inline_max_coord` across lines.
    pub ink_right: f32,
    /// Ink top edge — min `block_min_coord` across lines.
    pub ink_top: f32,
    /// Ink bottom edge — max `block_max_coord` across lines.
    pub ink_bottom: f32,
    /// First-line baseline in layout coords.
    pub first_baseline: f32,
    /// Last-line baseline in layout coords.
    pub last_baseline: f32,
}

impl LayoutBounds {
    /// Resolve `anchor` to a `(ref_x, ref_y)` pair inside the layout's
    /// coordinate system — the point that a caller's `(x, y)` should
    /// coincide with.
    pub fn resolve(&self, anchor: RichAnchor) -> AnchorOffsets {
        AnchorOffsets {
            ref_x: match anchor.h {
                HAnchor::Left => 0.0,
                HAnchor::LeftInk => self.ink_left,
                HAnchor::Center => self.width * 0.5,
                HAnchor::CenterInk => (self.ink_left + self.ink_right) * 0.5,
                HAnchor::RightInk => self.ink_right,
                HAnchor::Right => self.width,
                HAnchor::Fraction(f) => self.width * f,
            },
            ref_y: match anchor.v {
                VAnchor::Top => 0.0,
                VAnchor::TopInk => self.ink_top,
                VAnchor::FirstLine => self.first_baseline,
                VAnchor::Center => self.height * 0.5,
                VAnchor::CenterInk => (self.ink_top + self.ink_bottom) * 0.5,
                VAnchor::LastLine => self.last_baseline,
                VAnchor::BottomInk => self.ink_bottom,
                VAnchor::Bottom => self.height,
                VAnchor::Fraction(f) => self.height * f,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds() -> LayoutBounds {
        LayoutBounds {
            width: 200.0,
            height: 100.0,
            ink_left: 5.0,
            ink_right: 190.0,
            ink_top: 8.0,
            ink_bottom: 92.0,
            first_baseline: 20.0,
            last_baseline: 80.0,
        }
    }

    #[test]
    fn left_top_resolves_to_origin() {
        let o = bounds().resolve(RichAnchor::top_left());
        assert_eq!(o.ref_x, 0.0);
        assert_eq!(o.ref_y, 0.0);
    }

    #[test]
    fn center_resolves_to_midpoint() {
        let o = bounds().resolve(RichAnchor::center());
        assert_eq!(o.ref_x, 100.0);
        assert_eq!(o.ref_y, 50.0);
    }

    #[test]
    fn ink_center_uses_ink_extents() {
        let o = bounds().resolve(RichAnchor::center_ink());
        assert_eq!(o.ref_x, (5.0 + 190.0) * 0.5);
        assert_eq!(o.ref_y, (8.0 + 92.0) * 0.5);
    }

    #[test]
    fn first_line_baseline_maps_to_baseline_field() {
        let o = bounds().resolve(RichAnchor::first_line_baseline());
        assert_eq!(o.ref_x, 0.0);
        assert_eq!(o.ref_y, 20.0);
    }

    #[test]
    fn fraction_scales_bounds() {
        let a = RichAnchor {
            h: HAnchor::Fraction(0.25),
            v: VAnchor::Fraction(0.75),
        };
        let o = bounds().resolve(a);
        assert_eq!(o.ref_x, 50.0);
        assert_eq!(o.ref_y, 75.0);
    }

    #[test]
    fn last_line_baseline() {
        let a = RichAnchor {
            h: HAnchor::Left,
            v: VAnchor::LastLine,
        };
        let o = bounds().resolve(a);
        assert_eq!(o.ref_y, 80.0);
    }
}
