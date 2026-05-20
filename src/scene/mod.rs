//! Backend-agnostic scene authoring.
//!
//! [`SceneBuilder`] is the surface that plot code calls. Every method is
//! self-contained — no persistent "current transform / current brush" state —
//! which makes both immediate-mode and recording backends easy to implement.

use crate::blend::BlendMode;
use crate::brush::{Brush, Image, Sampling};
use crate::geometry::Affine;
use crate::path::{FillRule, Path};
use crate::pick::PickId;

pub mod recording;

/// A trait for issuing draw operations against some backend.
///
/// Implementations may rasterize immediately (e.g. Vello, Blend2D) or record
/// the calls for later replay (e.g. SVG, PDF).
pub trait SceneBuilder {
    /// Fill `path` with `brush`. `transform` applies to the path; `brush_transform`
    /// optionally transforms the brush coordinates (e.g. to rotate a gradient).
    ///
    /// `pick_id` controls how (or whether) this primitive appears in the
    /// hitmap when picking is enabled on the backend. Pass [`PickId::Skip`]
    /// for purely decorative content.
    fn fill(
        &mut self,
        rule: FillRule,
        transform: Affine,
        brush: &Brush,
        brush_transform: Option<Affine>,
        path: &Path,
        pick_id: PickId,
    );

    /// Stroke `path` with `brush`. See [`Self::fill`] for `pick_id` semantics.
    fn stroke(
        &mut self,
        stroke: &crate::stroke::Stroke,
        transform: Affine,
        brush: &Brush,
        brush_transform: Option<Affine>,
        path: &Path,
        pick_id: PickId,
    );

    /// Blit an image with the given transform. `alpha` is multiplied with the
    /// image's own alpha (0..=1). See [`Self::fill`] for `pick_id` semantics.
    fn draw_image(
        &mut self,
        image: &Image,
        transform: Affine,
        sampling: Sampling,
        alpha: f32,
        pick_id: PickId,
    );

    /// Draw a run of positioned glyphs. Shaping/layout is the caller's
    /// responsibility — this crate consumes already-placed glyphs.
    /// See [`Self::fill`] for `pick_id` semantics.
    fn draw_glyphs(&mut self, run: &GlyphRun<'_>, pick_id: PickId);

    /// Push a layer. Subsequent draws are clipped to `clip` (transformed by
    /// `transform`) and composited into the parent layer using `blend` and
    /// `alpha`. Must be matched by [`Self::pop_layer`].
    fn push_layer(&mut self, blend: BlendMode, alpha: f32, transform: Affine, clip: &Path);

    /// Pop the most recently pushed layer.
    fn pop_layer(&mut self);
}

// ---------- glyph types ----------

/// Opaque font handle. Wraps `peniko::FontData` (an Arc-backed font blob + index).
#[derive(Debug, Clone)]
pub struct Font(pub peniko::FontData);

impl Font {
    pub fn new(data: peniko::Blob<u8>, index: u32) -> Self {
        Self(peniko::FontData::new(data, index))
    }
}

/// A single positioned glyph.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Glyph {
    pub id: u32,
    pub x: f32,
    pub y: f32,
}

/// A run of glyphs sharing the same font, size, transform, and brush.
#[derive(Debug, Clone, Copy)]
pub struct GlyphRun<'a> {
    pub font: &'a Font,
    pub font_size: f32,
    pub transform: Affine,
    /// Optional per-glyph transform (skew, etc.) applied in glyph space.
    pub glyph_transform: Option<Affine>,
    pub brush: &'a Brush,
    /// Brush alpha multiplier (0..=1).
    pub brush_alpha: f32,
    /// If true, the backend may apply hinting where supported.
    pub hint: bool,
    pub glyphs: &'a [Glyph],
}
