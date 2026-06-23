//! A `SceneBuilder` that records every call into an owned op list.
//!
//! Used to replay scenes into vector backends (SVG, PDF) that don't fit the
//! "render to RGBA8 buffer" shape. The op enum is intentionally exhaustive —
//! adding a new variant means SVG/PDF emitters need to handle it.

use super::{Glyph, GlyphRun, SceneBuilder};
use crate::blend::BlendMode;
use crate::brush::{Brush, Image, Sampling};
use crate::geometry::Affine;
use crate::mesh::Mesh;
use crate::path::{FillRule, Path};
use crate::pick::PickId;
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
        pick_id: PickId,
    },
    Stroke {
        stroke: Stroke,
        transform: Affine,
        brush: Brush,
        brush_transform: Option<Affine>,
        path: Path,
        pick_id: PickId,
    },
    DrawImage {
        image: Image,
        transform: Affine,
        sampling: Sampling,
        alpha: f32,
        pick_id: PickId,
    },
    DrawGlyphs(OwnedGlyphRun),
    DrawMesh {
        mesh: Mesh,
        transform: Affine,
        pick_id: PickId,
    },
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
    /// `None` means fill the glyph outlines; `Some(stroke)` means
    /// stroke them.
    pub style: Option<crate::stroke::Stroke>,
    pub pick_id: PickId,
}

/// Recording scene: appends every call to an op list.
#[derive(Debug, Default, Clone)]
pub struct RecordingScene {
    pub ops: Vec<Op>,
}

impl RecordingScene {
    /// Construct an empty recording scene.
    pub fn new() -> Self {
        Self::default()
    }

    /// Drop every recorded op.
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
        pick_id: PickId,
    ) {
        self.ops.push(Op::Fill {
            rule,
            transform,
            brush: brush.clone(),
            brush_transform,
            path: path.clone(),
            pick_id,
        });
    }

    fn stroke(
        &mut self,
        stroke: &Stroke,
        transform: Affine,
        brush: &Brush,
        brush_transform: Option<Affine>,
        path: &Path,
        pick_id: PickId,
    ) {
        self.ops.push(Op::Stroke {
            stroke: stroke.clone(),
            transform,
            brush: brush.clone(),
            brush_transform,
            path: path.clone(),
            pick_id,
        });
    }

    fn draw_image(
        &mut self,
        image: &Image,
        transform: Affine,
        sampling: Sampling,
        alpha: f32,
        pick_id: PickId,
    ) {
        self.ops.push(Op::DrawImage {
            image: image.clone(),
            transform,
            sampling,
            alpha,
            pick_id,
        });
    }

    fn draw_glyphs(&mut self, run: &GlyphRun<'_>, pick_id: PickId) {
        self.ops.push(Op::DrawGlyphs(OwnedGlyphRun {
            font: run.font.clone(),
            font_size: run.font_size,
            transform: run.transform,
            glyph_transform: run.glyph_transform,
            brush: run.brush.clone(),
            brush_alpha: run.brush_alpha,
            hint: run.hint,
            glyphs: run.glyphs.to_vec(),
            style: run.style.cloned(),
            pick_id,
        }));
    }

    fn draw_mesh(&mut self, mesh: &Mesh, transform: Affine, pick_id: PickId) {
        self.ops.push(Op::DrawMesh {
            mesh: mesh.clone(),
            transform,
            pick_id,
        });
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::Color;
    use crate::geometry::Point;
    use crate::mesh::Mesh;

    #[test]
    fn records_draw_mesh() {
        let mesh = Mesh::new(
            vec![
                Point::new(0.0, 0.0),
                Point::new(10.0, 0.0),
                Point::new(0.0, 10.0),
            ],
            vec![
                Color::new([1.0, 0.0, 0.0, 1.0]),
                Color::new([0.0, 1.0, 0.0, 1.0]),
                Color::new([0.0, 0.0, 1.0, 1.0]),
            ],
            vec![0, 1, 2],
        );
        let mut scene = RecordingScene::default();
        scene.draw_mesh(&mesh, Affine::IDENTITY, PickId::Id(42));
        assert_eq!(scene.ops.len(), 1);
        match &scene.ops[0] {
            Op::DrawMesh {
                mesh: m,
                transform,
                pick_id,
            } => {
                assert_eq!(m.vertex_count(), 3);
                assert_eq!(m.triangle_count(), 1);
                assert_eq!(*transform, Affine::IDENTITY);
                assert!(matches!(pick_id, PickId::Id(42)));
            }
            other => panic!("expected DrawMesh, got {other:?}"),
        }
    }
}
