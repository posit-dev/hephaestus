//! A `SceneBuilder` that records every call into an owned op list.
//!
//! Used to replay scenes into vector backends (SVG, PDF) that don't fit the
//! "render to RGBA8 buffer" shape. The op enum is intentionally exhaustive —
//! adding a new variant means SVG/PDF emitters need to handle it.

use super::{Glyph, GlyphRun, SceneBuilder};
use crate::blend::BlendMode;
use crate::brush::{Brush, Image, Sampling};
use crate::geometry::Affine;
use crate::path::{FillRule, Path};
use crate::stroke::Stroke;

/// One captured draw operation.
#[derive(Debug, Clone)]
pub enum Op {
    Fill {
        rule: FillRule,
        transform: Affine,
        brush: Brush,
        brush_transform: Option<Affine>,
        path: Path,
    },
    Stroke {
        stroke: Stroke,
        transform: Affine,
        brush: Brush,
        brush_transform: Option<Affine>,
        path: Path,
    },
    DrawImage {
        image: Image,
        transform: Affine,
        sampling: Sampling,
        alpha: f32,
    },
    DrawGlyphs(OwnedGlyphRun),
    PushLayer {
        blend: BlendMode,
        alpha: f32,
        transform: Affine,
        clip: Path,
    },
    PopLayer,
}

/// Owned counterpart of `GlyphRun<'_>` for storage in `Op::DrawGlyphs`.
#[derive(Debug, Clone)]
pub struct OwnedGlyphRun {
    pub font: super::Font,
    pub font_size: f32,
    pub transform: Affine,
    pub glyph_transform: Option<Affine>,
    pub brush: Brush,
    pub brush_alpha: f32,
    pub hint: bool,
    pub glyphs: Vec<Glyph>,
}

/// Recording scene: appends every call to an op list.
#[derive(Debug, Default, Clone)]
pub struct RecordingScene {
    pub ops: Vec<Op>,
}

impl RecordingScene {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&mut self) {
        self.ops.clear();
    }
}

impl SceneBuilder for RecordingScene {
    fn fill(
        &mut self,
        rule: FillRule,
        transform: Affine,
        brush: &Brush,
        brush_transform: Option<Affine>,
        path: &Path,
    ) {
        self.ops.push(Op::Fill {
            rule,
            transform,
            brush: brush.clone(),
            brush_transform,
            path: path.clone(),
        });
    }

    fn stroke(
        &mut self,
        stroke: &Stroke,
        transform: Affine,
        brush: &Brush,
        brush_transform: Option<Affine>,
        path: &Path,
    ) {
        self.ops.push(Op::Stroke {
            stroke: stroke.clone(),
            transform,
            brush: brush.clone(),
            brush_transform,
            path: path.clone(),
        });
    }

    fn draw_image(&mut self, image: &Image, transform: Affine, sampling: Sampling, alpha: f32) {
        self.ops.push(Op::DrawImage {
            image: image.clone(),
            transform,
            sampling,
            alpha,
        });
    }

    fn draw_glyphs(&mut self, run: &GlyphRun<'_>) {
        self.ops.push(Op::DrawGlyphs(OwnedGlyphRun {
            font: run.font.clone(),
            font_size: run.font_size,
            transform: run.transform,
            glyph_transform: run.glyph_transform,
            brush: run.brush.clone(),
            brush_alpha: run.brush_alpha,
            hint: run.hint,
            glyphs: run.glyphs.to_vec(),
        }));
    }

    fn push_layer(&mut self, blend: BlendMode, alpha: f32, transform: Affine, clip: &Path) {
        self.ops.push(Op::PushLayer {
            blend,
            alpha,
            transform,
            clip: clip.clone(),
        });
    }

    fn pop_layer(&mut self) {
        self.ops.push(Op::PopLayer);
    }
}
