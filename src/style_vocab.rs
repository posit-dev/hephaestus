//! Shared styling vocabulary — the small value types that both the
//! plot theme and the text layer express visual quantities in.
//!
//! Three groups live here:
//!
//! - **[`Length`] / [`Margin`]** — a measurement that's either an
//!   absolute pt value or a multiplier against a parent's resolved
//!   length, plus the four-sided container over it. Mirrors ggplot2's
//!   `rel()` affordance: a sub-element's `size_pt` can be `Rel(1.5)`
//!   to read as "1.5× the inherited parent size" without recomputing
//!   absolute values. Resolution is one step —
//!   `length.resolve(parent_pt)`; walking the inheritance chain is
//!   the caller's job.
//! - **[`Palette`] / [`ThemeColor`]** — three semantic colour anchors
//!   (`paper` / `ink` / `accent`) and references into them. Swapping
//!   `paper` ↔ `ink` inverts every element that references them,
//!   which is how a dark theme is a one-line `invert()`.
//!   `ThemeColor::Fixed(...)` remains available for the rare case
//!   where a colour should be locked regardless of palette.
//! - **[`HAlign`] / [`VAlign`]** — alignment within a slot.
//!
//! These sit at the crate root rather than under `plot::theme` because
//! `text::rich` resolves palette colours and relative sizes while
//! shaping, and the low-level text layer must not depend on the
//! high-level plot layer. `plot::theme` re-exports every item here, so
//! plot-side code addresses them through the theme as before.

use std::sync::Arc;

use crate::color::{lerp_color, rgb, Color, ColorSpace};

// ─── Linetype steps ─────────────────────────────────────────────────────────

/// One step in a linetype pattern.
///
/// Patterns are even-length sequences where even-indexed entries are
/// `Dash` or `Marker` (something to draw at the cursor) and odd-indexed
/// entries are `Gap` (an unconditional advance). See
/// [`crate::linetype`] for constructors that enforce the
/// alternation.
///
/// `PartialEq` follows f64's IEEE semantics for `Dash` / `Gap`
/// (NaN ≠ NaN); use [`Value::key_eq`](crate::scales::value::Value::key_eq) / `key_hash` for the canonicalised
/// diff-friendly comparison.
#[derive(Clone, Debug, PartialEq)]
pub enum LinetypeStep {
    /// Stroke a segment of this length (in pt) along the line, then
    /// advance the cursor by the same amount.
    Dash(f64),
    /// Stamp the named shape at the current cursor. The marker is
    /// assumed to occupy `linewidth` pt of arc length so the next gap
    /// measures clear space starting from the marker's trailing edge.
    Marker(Arc<str>),
    /// Advance the cursor by this many pt without drawing.
    Gap(f64),
}

impl LinetypeStep {
    /// `true` if this is a `Marker` step.
    pub fn is_marker(&self) -> bool {
        matches!(self, LinetypeStep::Marker(_))
    }
}

// ─── Length ─────────────────────────────────────────────────────────────────

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

    /// Resolve against `parent_pt`, then swap the `left` and `right`
    /// components when `is_rtl` is true.
    ///
    /// Used at the callsites that treat a class-supplied `.left` /
    /// `.right` as **logical** start / end sides (block-level
    /// `padding`, `margin`, `border_width` on paragraphs, headings,
    /// blockquotes, list containers). A blockquote class that sets
    /// `border_width.left = 3` semantically means "start-side bar";
    /// under Rtl that bar has to paint on the physical right, so this
    /// helper swaps the two before downstream code reads the tuple.
    ///
    /// Directional swap is opt-in per callsite — physical primitives
    /// (`Element::Inherit`, chrome axes, geom rects) still use
    /// [`Self::resolve`].
    #[inline]
    pub fn resolve_for_direction(&self, parent_pt: f64, is_rtl: bool) -> (f64, f64, f64, f64) {
        let (t, r, b, l) = self.resolve(parent_pt);
        if is_rtl {
            (t, l, b, r)
        } else {
            (t, r, b, l)
        }
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

// ─── Palette ────────────────────────────────────────────────────────────────

/// Three semantic colour anchors that every theme element references.
///
/// - `paper` — background anchor (panel + plot backgrounds, light grids
///   in light themes / dark grids in dark themes).
/// - `ink` — foreground anchor (text, axis lines, panel borders,
///   default stroke colour for geoms).
/// - `accent` — highlight anchor (default fill colour for geoms when
///   no fill channel is bound; legend / strip accents).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    /// Background anchor.
    pub paper: Color,
    /// Foreground anchor.
    pub ink: Color,
    /// Highlight anchor.
    pub accent: Color,
}

impl Palette {
    /// Construct a palette from explicit anchors.
    #[inline]
    pub const fn new(paper: Color, ink: Color, accent: Color) -> Self {
        Self { paper, ink, accent }
    }
}

impl Default for Palette {
    /// Light palette: pure-white paper, black ink, muted-blue accent.
    /// Paper at 1.0 lets `ThemeColor::mix(Paper, Ink, t)` produce
    /// exact grey-`100*(1-t)` levels — so theme defaults can address
    /// ggplot2 anchor greys (grey92, grey85, grey30, …) directly.
    fn default() -> Self {
        Self {
            paper: rgb(1.0, 1.0, 1.0),
            ink: rgb(0.0, 0.0, 0.0),
            accent: rgb(0.20, 0.45, 0.85),
        }
    }
}

/// A colour expressed in palette terms. Resolved to a concrete `Color`
/// at draw time against the effective theme's [`Palette`].
#[derive(Debug, Clone, PartialEq)]
pub enum ThemeColor {
    /// A concrete colour locked to its literal value regardless of
    /// palette.
    Fixed(Color),
    /// The palette's `paper` anchor.
    Paper,
    /// The palette's `ink` anchor.
    Ink,
    /// The palette's `accent` anchor.
    Accent,
    /// Linear interpolation between two `ThemeColor`s through the given
    /// [`ColorSpace`]. `t = 0` returns `a`, `t = 1` returns `b`.
    /// [`ThemeColor::mix`] builds this with [`ColorSpace::Srgb`], which is
    /// what addresses palette anchors by channel fraction (`mix(Paper,
    /// Ink, 0.08)` is grey92 against the default palette).
    Mix(Box<ThemeColor>, Box<ThemeColor>, f64, ColorSpace),
    /// Same colour, modulated alpha. `Alpha(inner, a)` multiplies the
    /// resolved colour's alpha channel by `a`.
    Alpha(Box<ThemeColor>, f32),
}

impl ThemeColor {
    /// Materialize a concrete `Color` against `palette`. Cheap — a
    /// few floating-point ops at worst.
    pub fn resolve(&self, palette: &Palette) -> Color {
        match self {
            ThemeColor::Fixed(c) => *c,
            ThemeColor::Paper => palette.paper,
            ThemeColor::Ink => palette.ink,
            ThemeColor::Accent => palette.accent,
            ThemeColor::Mix(a, b, t, space) => {
                lerp_color(a.resolve(palette), b.resolve(palette), *t, *space)
            }
            ThemeColor::Alpha(inner, a) => {
                let c = inner.resolve(palette);
                let [r, g, b, alpha] = c.components;
                Color::new([r, g, b, alpha * a])
            }
        }
    }

    /// Mix two `ThemeColor`s in sRGB — the space that makes `t` a channel
    /// fraction between the two anchors, so palette greys land on their
    /// nominal levels.
    #[inline]
    pub fn mix(a: ThemeColor, b: ThemeColor, t: f64) -> Self {
        ThemeColor::Mix(Box::new(a), Box::new(b), t, ColorSpace::Srgb)
    }

    /// Mix two `ThemeColor`s in an explicit space. Use for a perceptually
    /// even blend between two saturated anchors, where [`Self::mix`]'s
    /// channel arithmetic would read as a dip in lightness.
    #[inline]
    pub fn mix_in(a: ThemeColor, b: ThemeColor, t: f64, space: ColorSpace) -> Self {
        ThemeColor::Mix(Box::new(a), Box::new(b), t, space)
    }

    /// `ThemeColor::Alpha(inner, a)` constructor without the `Box::new`
    /// noise.
    #[inline]
    pub fn alpha(inner: ThemeColor, a: f32) -> Self {
        ThemeColor::Alpha(Box::new(inner), a)
    }
}

// ─── Alignment ──────────────────────────────────────────────────────────────

/// Horizontal alignment — for text justification within a slot, and
/// for `hjust`-style anchor positioning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum HAlign {
    /// Align with the start edge (left in left-to-right scripts).
    #[default]
    Start,
    /// Centre within the slot.
    Center,
    /// Align with the end edge (right in left-to-right scripts).
    End,
    /// Stretch lines to fill the slot width.
    Justify,
}

/// Vertical alignment — for text baseline positioning within a slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum VAlign {
    /// Align with the top edge of the slot.
    Top,
    /// Centre vertically within the slot.
    #[default]
    Middle,
    /// Align with the alphabetic baseline within the slot.
    Baseline,
    /// Align with the bottom edge of the slot.
    Bottom,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mix_addresses_palette_greys_by_channel_fraction() {
        // `mix(Paper, Ink, 0.08)` has to land on grey92 against the
        // default white-paper / black-ink palette — that's how the
        // built-in themes name ggplot2's anchor greys.
        let grey =
            ThemeColor::mix(ThemeColor::Paper, ThemeColor::Ink, 0.08).resolve(&Palette::default());
        for c in &grey.components[0..3] {
            assert!((c - 0.92).abs() < 1e-5, "expected grey92, got {grey:?}");
        }
    }

    /// A palette whose three anchors are primaries, so a resolved
    /// channel names which anchor it came from.
    fn primary_palette() -> Palette {
        Palette::new(rgb(1.0, 0.0, 0.0), rgb(0.0, 0.0, 1.0), rgb(0.0, 1.0, 0.0))
    }

    #[track_caller]
    fn assert_components(got: Color, want: [f32; 4]) {
        for (i, (g, w)) in got.components.iter().zip(want.iter()).enumerate() {
            assert!(
                (g - w).abs() < 1e-6,
                "component {i}: got {got:?}, want {want:?}"
            );
        }
    }

    #[test]
    fn a_mix_nested_in_a_mix_resolves_before_the_outer_blend() {
        // Half paper / half ink, then half of that toward ink again.
        let c = ThemeColor::mix(
            ThemeColor::mix(ThemeColor::Paper, ThemeColor::Ink, 0.5),
            ThemeColor::Ink,
            0.5,
        )
        .resolve(&Palette::default());
        assert_components(c, [0.25, 0.25, 0.25, 1.0]);
    }

    #[test]
    fn mixing_two_mixes_averages_their_resolved_colors() {
        let c = ThemeColor::mix(
            ThemeColor::mix(ThemeColor::Paper, ThemeColor::Ink, 0.25),
            ThemeColor::mix(ThemeColor::Paper, ThemeColor::Ink, 0.75),
            0.5,
        )
        .resolve(&Palette::default());
        assert_components(c, [0.5, 0.5, 0.5, 1.0]);
    }

    #[test]
    fn nested_mixes_address_the_supplied_palette_anchors() {
        // Half paper (red) / half accent (green), then half toward ink
        // (blue).
        let c = ThemeColor::mix(
            ThemeColor::mix(ThemeColor::Paper, ThemeColor::Accent, 0.5),
            ThemeColor::Ink,
            0.5,
        )
        .resolve(&primary_palette());
        assert_components(c, [0.25, 0.25, 0.5, 1.0]);
    }

    #[test]
    fn nested_alphas_multiply_rather_than_replace() {
        let c = ThemeColor::alpha(ThemeColor::alpha(ThemeColor::Accent, 0.5), 0.5)
            .resolve(&primary_palette());
        assert_components(c, [0.0, 1.0, 0.0, 0.25]);
    }

    #[test]
    fn alpha_scales_a_fixed_colors_own_alpha() {
        let c = ThemeColor::alpha(ThemeColor::Fixed(Color::new([0.2, 0.4, 0.6, 0.5])), 0.5)
            .resolve(&Palette::default());
        assert_components(c, [0.2, 0.4, 0.6, 0.25]);
    }

    #[test]
    fn alpha_wrapping_a_mix_keeps_the_blended_channels() {
        let c = ThemeColor::alpha(
            ThemeColor::mix(ThemeColor::Paper, ThemeColor::Ink, 0.25),
            0.4,
        )
        .resolve(&Palette::default());
        assert_components(c, [0.75, 0.75, 0.75, 0.4]);
    }

    #[test]
    fn a_mix_interpolates_the_alpha_of_its_nested_operands() {
        let c = ThemeColor::mix(
            ThemeColor::alpha(ThemeColor::Paper, 0.0),
            ThemeColor::Paper,
            0.5,
        )
        .resolve(&Palette::default());
        assert_components(c, [1.0, 1.0, 1.0, 0.5]);
    }

    #[test]
    fn mix_defaults_to_srgb_and_mix_in_overrides_it() {
        assert!(matches!(
            ThemeColor::mix(ThemeColor::Paper, ThemeColor::Ink, 0.5),
            ThemeColor::Mix(_, _, _, ColorSpace::Srgb)
        ));
        let ok = ThemeColor::mix_in(ThemeColor::Paper, ThemeColor::Ink, 0.5, ColorSpace::Oklab);
        assert!(matches!(ok, ThemeColor::Mix(_, _, _, ColorSpace::Oklab)));
        let palette = Palette::default();
        assert_ne!(
            ok.resolve(&palette),
            ThemeColor::mix(ThemeColor::Paper, ThemeColor::Ink, 0.5).resolve(&palette)
        );
    }
}
