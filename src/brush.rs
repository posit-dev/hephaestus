//! Brushes — what fills/strokes paint with. Solid colors, gradients, and image
//! patterns. Re-exports peniko types; `Sampling` is restricted to the modes both
//! Vello and Blend2D support natively.

pub use peniko::{
    Brush, ColorStop, ColorStops, Extend, Gradient, GradientKind, ImageData as Image, ImageFormat,
};

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
