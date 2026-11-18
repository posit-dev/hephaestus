//! Brushes — what fills/strokes paint with. Solid colors, gradients, image
//! patterns, and vector fill patterns. The brush vocabulary is zygos's
//! (`zygos::brush`), re-exported so channels and scene calls name one type;
//! backends convert to peniko's `Brush` via [`Brush::as_peniko`] for the
//! variants peniko can express and tile `Brush::Pattern` themselves.

pub use peniko::color::{ColorSpaceTag, DynamicColor, Flags, HueDirection, Missing};
pub use zygos::brush::{
    Blob, Brush, ColorStop, ColorStops, Extend, Gradient, GradientKind, ImageAlphaType, ImageBrush,
    ImageData as Image, ImageFormat, ImageQuality, ImageSampler, InterpolationAlphaSpace,
    LinearGradientPosition, RadialGradientPosition, SweepGradientPosition,
};
pub use zygos::patterns::fill::{fill_pattern, FillPattern, PatternElement, PatternPaint};

/// Bind a pattern's paint roles to concrete brushes, returning the
/// fully-colored pattern a backend tiles. `Fixed` paints pass through
/// unchanged; `Foreground` / `Background` become the supplied brushes.
/// Binding happens before the scene boundary — backends only ever see
/// bound patterns.
pub fn bind_pattern(pattern: &FillPattern, foreground: &Brush, background: &Brush) -> FillPattern {
    let bind = |paint: &PatternPaint| match paint {
        PatternPaint::Foreground => foreground.clone(),
        PatternPaint::Background => background.clone(),
        PatternPaint::Fixed(b) => b.clone(),
    };
    let mut bound = pattern.clone();
    for el in &mut bound.elements {
        match el {
            PatternElement::Path { fill, stroke, .. } => {
                if let Some(f) = fill {
                    *f = PatternPaint::Fixed(bind(f));
                }
                if let Some(s) = stroke {
                    s.1 = PatternPaint::Fixed(bind(&s.1));
                }
            }
            PatternElement::Shape { paint, .. } | PatternElement::Text { paint, .. } => {
                *paint = PatternPaint::Fixed(bind(paint));
            }
        }
    }
    bound
}

/// Image sampling mode used when an image brush is scaled or rotated.
///
/// Restricted to the intersection of Vello and Blend2D capabilities. (Peniko
/// itself exposes Low/Medium/High; we deliberately expose only the two modes
/// both backends implement natively.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Sampling {
    Nearest,
    Bilinear,
}
