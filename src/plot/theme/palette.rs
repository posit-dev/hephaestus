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

use crate::color::{lerp_color, rgb, Color};

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
    /// Linear interpolation between two `ThemeColor`s. `Mix(a, b, t)`
    /// returns `lerp(a.resolve(), b.resolve(), t)`. `t = 0` returns
    /// `a`, `t = 1` returns `b`.
    Mix(Box<ThemeColor>, Box<ThemeColor>, f32),
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
            ThemeColor::Mix(a, b, t) => {
                lerp_color(a.resolve(palette), b.resolve(palette), *t as f64)
            }
            ThemeColor::Alpha(inner, a) => {
                let c = inner.resolve(palette);
                let [r, g, b, alpha] = c.components;
                Color::new([r, g, b, alpha * a])
            }
        }
    }

    /// `ThemeColor::Mix(a, b, t)` constructor without the `Box::new`
    /// noise.
    #[inline]
    pub fn mix(a: ThemeColor, b: ThemeColor, t: f32) -> Self {
        ThemeColor::Mix(Box::new(a), Box::new(b), t)
    }

    /// `ThemeColor::Alpha(inner, a)` constructor without the `Box::new`
    /// noise.
    #[inline]
    pub fn alpha(inner: ThemeColor, a: f32) -> Self {
        ThemeColor::Alpha(Box::new(inner), a)
    }
}
