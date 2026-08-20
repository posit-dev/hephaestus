//! Backend-agnostic scene authoring.
//!
//! [`SceneBuilder`] is the surface that plot code calls. Every method is
//! self-contained — no persistent "current transform / current brush" state —
//! which makes both immediate-mode and recording backends easy to implement.

use crate::blend::BlendMode;
use crate::brush::{Brush, Image, Sampling};
use crate::geometry::Affine;
use crate::mesh::Mesh;
use crate::path::{FillRule, Path};
use crate::pick::PickId;

pub mod recording;

/// A trait for issuing draw operations against some backend.
///
/// Implementations may rasterize immediately (e.g. Vello, Blend2D) or record
/// the calls for later replay (e.g. SVG, PDF).
pub trait SceneBuilder {
    /// Drop everything recorded so far, leaving the builder ready for a
    /// new frame. Part of the frame lifecycle, so generic code written
    /// against the trait can start one.
    fn clear(&mut self);

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

    /// Draw a 2D triangle mesh with per-vertex colour. `transform`
    /// applies to vertex positions (not colours). The whole mesh
    /// shares a single `pick_id` — picking does not distinguish
    /// individual triangles.
    ///
    /// No backend currently has a native indexed-mesh primitive;
    /// every backend decomposes the mesh into its own draw ops (e.g.
    /// the Vello backend emits one `fill` per triangle with a
    /// per-triangle linear-gradient brush). See [`Self::fill`] for
    /// `pick_id` semantics.
    fn draw_mesh(&mut self, mesh: &Mesh, transform: Affine, pick_id: PickId);

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
pub struct Font(peniko::FontData);

/// Two handles are equal when they name the same face — the same font
/// bytes at the same index — however each was obtained.
///
/// Hand-written because the underlying blob compares by *identity*: it
/// carries a process-local id, so the derived impl would call two
/// handles onto one face unequal whenever the file had been loaded twice.
/// Font resolution does that in practice, which makes identity the wrong
/// question to answer here.
///
/// Comparing unequal handles reads both font files. That is a byte
/// comparison of megabytes in the worst case, so this is not something
/// to put on a hot path; the identity fast path covers the common case
/// of two handles that really do share one blob.
impl PartialEq for Font {
    fn eq(&self, other: &Self) -> bool {
        if self.0.index != other.0.index {
            return false;
        }
        self.0.data.id() == other.0.data.id() || self.0.data.as_ref() == other.0.data.as_ref()
    }
}

impl Font {
    /// Wrap an already-resolved backend font. Crate-internal: the
    /// public way in is [`Self::new`].
    pub(crate) fn from_data(data: peniko::FontData) -> Self {
        Self(data)
    }

    /// Borrow the backend font blob. Crate-internal so the handle
    /// stays opaque on the public surface.
    #[cfg_attr(not(feature = "vello"), allow(dead_code))]
    pub(crate) fn data(&self) -> &peniko::FontData {
        &self.0
    }

    /// Wrap a font blob and face index. `data` is an Arc-backed byte
    /// buffer carrying the font file; `index` selects a face within a
    /// TrueType / OpenType collection (0 for the common single-face case).
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
    /// Render style. `None` (default) fills the glyph outlines; `Some(stroke)`
    /// strokes them along the outline with the given pen. Used for outlined
    /// text — typically paired with a separate filled pass on top.
    pub style: Option<&'a crate::stroke::Stroke>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use peniko::Blob;

    /// The property the derived impl would have got wrong: font
    /// resolution can hand out two blobs for one file, and two handles
    /// onto the same face have to compare equal regardless.
    #[test]
    fn fonts_naming_the_same_face_are_equal_across_separate_blobs() {
        let bytes = vec![7u8, 8, 9, 10];
        let a = Font::new(Blob::from(bytes.clone()), 0);
        let b = Font::new(Blob::from(bytes), 0);
        assert_ne!(
            a.data().data.id(),
            b.data().data.id(),
            "the two blobs should have distinct ids, or this proves nothing"
        );
        assert_eq!(a, b);
    }

    #[test]
    fn a_font_equals_a_clone_of_itself() {
        let font = Font::new(Blob::from(vec![1u8, 2, 3]), 2);
        assert_eq!(font.clone(), font);
    }

    #[test]
    fn fonts_differing_in_face_index_are_not_equal() {
        let bytes = vec![1u8, 2, 3];
        let a = Font::new(Blob::from(bytes.clone()), 0);
        let b = Font::new(Blob::from(bytes), 1);
        assert_ne!(a, b);
    }

    #[test]
    fn fonts_with_different_bytes_are_not_equal() {
        let a = Font::new(Blob::from(vec![1u8, 2, 3]), 0);
        let b = Font::new(Blob::from(vec![1u8, 2, 4]), 0);
        assert_ne!(a, b);
    }
}
