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
use crate::pick::{PickId, PickScope};
use crate::style_vocab::FontSpec;

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

    /// Push a pick scope. Every primitive issued until the matching
    /// [`Self::pop_pick_scope`] inherits it as an ancestor, so the stack at
    /// the time of a draw is that primitive's full ancestor chain.
    ///
    /// Orthogonal to [`Self::push_layer`]: a scope has no visual effect and
    /// imposes no clip, and the two stacks need not nest with one another.
    /// The only contract is that pushes and pops balance.
    ///
    /// Unlike every other method on this trait, ignoring this one still
    /// produces a correct picture — the intersection-of-backends rule is
    /// about visual capabilities, and a scope has none. So it defaults to a
    /// no-op and a backend that does not hit-test writes nothing.
    fn push_pick_scope(&mut self, scope: &PickScope) {
        let _ = scope;
    }

    /// Pop the most recently pushed pick scope. Defaults to a no-op, for the
    /// reason given on [`Self::push_pick_scope`].
    fn pop_pick_scope(&mut self) {}
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

/// Identifies the text block a glyph run belongs to.
///
/// Runs sharing a value were laid out together, so a backend that emits
/// text as text can gather them into one element rather than one per
/// run. Opaque and only ever compared for equality; a rasteriser ignores
/// it entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TextGroup(u64);

impl TextGroup {
    /// Mint a group id no other shaped run shares. Called once per
    /// shaped run, so a run drawn twice is two blocks.
    pub fn next() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }

    /// The group for one placement of a shaped run.
    ///
    /// Derived rather than minted so that two passes over the same run
    /// at the same place agree without having to be told — which is what
    /// lets an outline pass and the fill pass that follows it be
    /// recognised as one piece of text rather than two stacked copies.
    pub fn for_placement(&self, x: f64, y: f64, transform: crate::geometry::Affine) -> Self {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.0.hash(&mut h);
        x.to_bits().hash(&mut h);
        y.to_bits().hash(&mut h);
        for c in transform.as_coeffs() {
            c.to_bits().hash(&mut h);
        }
        Self(h.finish())
    }
}

/// One decoration rule's resolved metrics, in the run's own units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rule {
    /// Rule thickness.
    pub thickness: f32,
    /// Offset from the baseline, in font-typography convention (Y-up),
    /// which is how the face reports it.
    pub offset: f32,
}

/// The decorations a run's style asks for.
///
/// A rasterising backend ignores these — the rules arrive separately as
/// ordinary fills, which is what it wants. A backend that expresses
/// decorations semantically reads them here and suppresses the fills it
/// can account for.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Decorations {
    /// Underline rule, when the style asks for one.
    pub underline: Option<Rule>,
    /// Strikethrough rule, when the style asks for one.
    pub strikethrough: Option<Rule>,
}

impl Decorations {
    /// True when the style asks for no decoration at all.
    pub fn is_empty(&self) -> bool {
        self.underline.is_none() && self.strikethrough.is_none()
    }
}

/// What a run of glyphs was shaped from.
///
/// [`GlyphRun`] carries glyph ids, which is all a rasteriser needs and
/// strictly less than a backend emitting `<text>` or a PDF text object
/// needs — those need the characters back, and the font named. Shaping
/// knows both and used to drop them; this is that knowledge travelling
/// alongside the glyphs.
///
/// Optional because a caller may position glyphs itself with no source
/// string to point at — a glyph-backed marker shape, say. A backend that
/// needs text and finds `None` falls back to outlines.
#[derive(Debug, Clone, Copy)]
pub struct TextSource<'a> {
    /// The source substring this run covers, exactly.
    pub text: &'a str,
    /// How the face was asked for. [`GlyphRun::font`] is what it
    /// resolved to.
    pub font: &'a FontSpec,
    /// Total advance along the flow axis. What an SVG `textLength`
    /// states, and the only width claim we can make that does not
    /// assume the reader resolves the same face we did.
    pub advance: f32,
    /// True when the run reads right-to-left.
    pub rtl: bool,
    /// Decorations the run's style asks for.
    pub decorations: Decorations,
    /// Link destination covering this run, if any.
    pub link: Option<&'a str>,
    /// The text block this run belongs to.
    pub group: TextGroup,
}

/// Equality ignores [`TextSource::group`].
///
/// A group id is a label whose only meaning is *equality within one
/// scene* — it says which runs were laid out together, not which block
/// they are. Ids are minted from a running counter, so the same drawing
/// recorded twice carries different ones, and comparing across scenes
/// would report a difference where none exists. Two scenes are equal as
/// *drawing* when they draw the same glyphs from the same text; that is
/// the question `RecordingScene`'s equality exists to answer.
impl PartialEq for TextSource<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.text == other.text
            && self.font == other.font
            && self.advance == other.advance
            && self.rtl == other.rtl
            && self.decorations == other.decorations
            && self.link == other.link
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
    /// What this run was shaped from, when the caller knows. Backends
    /// that emit text as text need it; rasterisers ignore it.
    pub source: Option<TextSource<'a>>,
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
