//! Semantic colour palette + [`ThemeColor`] references.
//!
//! Every chrome colour in a `Theme` is a `ThemeColor` — a reference
//! into the theme's `Palette` (`paper` / `ink` / `accent`) or a mix of
//! palette anchors. Swapping `paper` ↔ `ink` inverts every element
//! that references them, which is how `Theme::dark()` is implemented as
//! a one-line `default().invert()`.
//!
//! `ThemeColor::Fixed(...)` remains available for the rare case where
//! an element should be locked to a specific colour regardless of
//! palette (e.g. a red error annotation).

use crate::color::{lerp_color, rgb, Color, ColorSpace};

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
    Mix(Box<ThemeColor>, Box<ThemeColor>, f32, ColorSpace),
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
                lerp_color(a.resolve(palette), b.resolve(palette), *t as f64, *space)
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
    pub fn mix(a: ThemeColor, b: ThemeColor, t: f32) -> Self {
        ThemeColor::Mix(Box::new(a), Box::new(b), t, ColorSpace::Srgb)
    }

    /// Mix two `ThemeColor`s in an explicit space. Use for a perceptually
    /// even blend between two saturated anchors, where [`Self::mix`]'s
    /// channel arithmetic would read as a dip in lightness.
    #[inline]
    pub fn mix_in(a: ThemeColor, b: ThemeColor, t: f32, space: ColorSpace) -> Self {
        ThemeColor::Mix(Box::new(a), Box::new(b), t, space)
    }

    /// `ThemeColor::Alpha(inner, a)` constructor without the `Box::new`
    /// noise.
    #[inline]
    pub fn alpha(inner: ThemeColor, a: f32) -> Self {
        ThemeColor::Alpha(Box::new(inner), a)
    }
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
