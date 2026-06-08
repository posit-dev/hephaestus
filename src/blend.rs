//! Blend modes. Restricted to the intersection of Vello and Blend2D.
//!
//! Both backends support the full set of separable and non-separable Mix modes
//! listed here plus the Porter–Duff Compose operators. Blend2D supports a few
//! extras (LinearBurn, PinLight, HardMix, Modulate, etc.) which are
//! intentionally not exposed.

/// Composite operator (Porter–Duff).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Compose {
    Clear,
    Copy,
    Dest,
    SrcOver,
    DestOver,
    SrcIn,
    DestIn,
    SrcOut,
    DestOut,
    SrcAtop,
    DestAtop,
    Xor,
    Plus,
}

/// Blend (mix) function applied before the compositor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mix {
    Normal,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    HardLight,
    SoftLight,
    Difference,
    Exclusion,
    Hue,
    Saturation,
    Color,
    Luminosity,
}

/// A blend mode is a (mix, compose) pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlendMode {
    pub mix: Mix,
    pub compose: Compose,
}

impl BlendMode {
    /// Combine a [`Mix`] function with a [`Compose`] operator.
    pub const fn new(mix: Mix, compose: Compose) -> Self {
        Self { mix, compose }
    }

    /// `Mix::Normal` over `Compose::SrcOver` — the default "alpha blend".
    pub const NORMAL: Self = Self::new(Mix::Normal, Compose::SrcOver);
}

impl Default for BlendMode {
    fn default() -> Self {
        Self::NORMAL
    }
}
