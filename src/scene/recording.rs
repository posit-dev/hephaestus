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
#[derive(Debug, Clone, PartialEq)]
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
#[derive(Debug, Clone, PartialEq)]
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
///
/// Equality is op-for-op, which is what lets two scenes be compared as
/// *drawing* rather than as pixels — useful when the rasteriser is the
/// variable you want to hold still.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct RecordingScene {
    pub ops: Vec<Op>,
}

impl RecordingScene {
    /// Construct an empty recording scene.
    pub fn new() -> Self {
        Self::default()
    }
}

impl SceneBuilder for RecordingScene {
    fn clear(&mut self) {
        self.ops.clear();
    }

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
    use crate::blend::{Compose, Mix};
    use crate::brush::{Blob, ImageAlphaType, ImageFormat};
    use crate::color::Color;
    use crate::geometry::Point;
    use crate::mesh::Mesh;
    use crate::scene::{Font, Glyph, GlyphRun};

    fn solid(r: f32, g: f32, b: f32) -> Brush {
        Brush::Solid(Color::new([r, g, b, 1.0]))
    }

    /// A unit square, enough to tell one recorded path from another.
    fn square(side: f64) -> Path {
        let mut p = Path::new();
        p.move_to(Point::new(0.0, 0.0));
        p.line_to(Point::new(side, 0.0));
        p.line_to(Point::new(side, side));
        p.close_path();
        p
    }

    /// A 2×1 RGBA8 image — the payload is never decoded, only carried.
    fn test_image() -> Image {
        Image {
            data: Blob::from(vec![1u8, 2, 3, 4, 5, 6, 7, 8]),
            format: ImageFormat::Rgba8,
            alpha_type: ImageAlphaType::Alpha,
            width: 2,
            height: 1,
        }
    }

    #[test]
    fn records_fill_with_its_arguments_intact() {
        let mut scene = RecordingScene::new();
        let brush = solid(1.0, 0.0, 0.0);
        let path = square(3.0);
        scene.fill(
            FillRule::EvenOdd,
            Affine::translate((5.0, 6.0)),
            &brush,
            Some(Affine::scale(2.0)),
            &path,
            PickId::Id(11),
        );
        match &scene.ops[..] {
            [Op::Fill {
                rule,
                transform,
                brush: b,
                brush_transform,
                path: p,
                pick_id,
            }] => {
                assert_eq!(*rule, FillRule::EvenOdd);
                assert_eq!(*transform, Affine::translate((5.0, 6.0)));
                assert_eq!(*b, brush);
                assert_eq!(*brush_transform, Some(Affine::scale(2.0)));
                assert_eq!(p.elements(), path.elements());
                assert_eq!(*pick_id, PickId::Id(11));
            }
            other => panic!("expected a single Fill, got {other:?}"),
        }
    }

    #[test]
    fn records_stroke_with_its_pen_and_pick_id() {
        let mut scene = RecordingScene::new();
        let brush = solid(0.0, 0.0, 1.0);
        let path = square(4.0);
        let stroke = Stroke::new(2.5).with_caps(crate::stroke::Cap::Square);
        scene.stroke(
            &stroke,
            Affine::IDENTITY,
            &brush,
            None,
            &path,
            PickId::Block,
        );
        match &scene.ops[..] {
            [Op::Stroke {
                stroke: s,
                brush: b,
                brush_transform,
                path: p,
                pick_id,
                ..
            }] => {
                assert_eq!(s.width, 2.5);
                assert_eq!(s.start_cap, crate::stroke::Cap::Square);
                assert_eq!(*b, brush);
                assert!(brush_transform.is_none());
                assert_eq!(p.elements(), path.elements());
                assert_eq!(*pick_id, PickId::Block);
            }
            other => panic!("expected a single Stroke, got {other:?}"),
        }
    }

    #[test]
    fn records_draw_image_with_its_sampling_and_alpha() {
        let mut scene = RecordingScene::new();
        let image = test_image();
        scene.draw_image(
            &image,
            Affine::scale(3.0),
            Sampling::Nearest,
            0.25,
            PickId::Id(9),
        );
        match &scene.ops[..] {
            [Op::DrawImage {
                image: i,
                transform,
                sampling,
                alpha,
                pick_id,
            }] => {
                assert_eq!(i.width, 2);
                assert_eq!(i.height, 1);
                assert_eq!(i.data.as_ref(), image.data.as_ref());
                assert_eq!(*transform, Affine::scale(3.0));
                assert_eq!(*sampling, Sampling::Nearest);
                assert_eq!(*alpha, 0.25);
                assert_eq!(*pick_id, PickId::Id(9));
            }
            other => panic!("expected a single DrawImage, got {other:?}"),
        }
    }

    #[test]
    fn records_draw_glyphs_as_an_owned_run() {
        let font = Font::new(Blob::from(vec![0u8; 4]), 1);
        let brush = solid(0.0, 1.0, 0.0);
        let glyphs = [
            Glyph {
                id: 3,
                x: 0.0,
                y: 1.0,
            },
            Glyph {
                id: 4,
                x: 10.0,
                y: 1.0,
            },
        ];
        let stroke = Stroke::new(0.5);
        let run = GlyphRun {
            font: &font,
            font_size: 14.0,
            transform: Affine::translate((2.0, 3.0)),
            glyph_transform: Some(Affine::scale(0.5)),
            brush: &brush,
            brush_alpha: 0.75,
            hint: true,
            glyphs: &glyphs,
            style: Some(&stroke),
        };
        let mut scene = RecordingScene::new();
        scene.draw_glyphs(&run, PickId::Id(21));

        match &scene.ops[..] {
            [Op::DrawGlyphs(owned)] => {
                assert_eq!(owned.font_size, 14.0);
                assert_eq!(owned.transform, Affine::translate((2.0, 3.0)));
                assert_eq!(owned.glyph_transform, Some(Affine::scale(0.5)));
                assert_eq!(owned.brush, brush);
                assert_eq!(owned.brush_alpha, 0.75);
                assert!(owned.hint);
                assert_eq!(owned.glyphs.len(), 2);
                assert_eq!(owned.glyphs[1].id, 4);
                assert_eq!(owned.glyphs[1].x, 10.0);
                assert_eq!(owned.style.as_ref().map(|s| s.width), Some(0.5));
                assert_eq!(owned.pick_id, PickId::Id(21));
            }
            other => panic!("expected a single DrawGlyphs, got {other:?}"),
        }
    }

    #[test]
    fn records_glyph_runs_that_fill_without_a_stroke_style() {
        let font = Font::new(Blob::from(vec![0u8; 4]), 0);
        let brush = solid(0.0, 0.0, 0.0);
        let run = GlyphRun {
            font: &font,
            font_size: 10.0,
            transform: Affine::IDENTITY,
            glyph_transform: None,
            brush: &brush,
            brush_alpha: 1.0,
            hint: false,
            glyphs: &[],
            style: None,
        };
        let mut scene = RecordingScene::new();
        scene.draw_glyphs(&run, PickId::Skip);
        match &scene.ops[..] {
            [Op::DrawGlyphs(owned)] => {
                assert!(owned.style.is_none());
                assert!(owned.glyphs.is_empty());
                assert_eq!(owned.pick_id, PickId::Skip);
            }
            other => panic!("expected a single DrawGlyphs, got {other:?}"),
        }
    }

    #[test]
    fn records_layer_pushes_and_pops_in_order() {
        let mut scene = RecordingScene::new();
        let clip = square(8.0);
        let blend = BlendMode::new(Mix::Multiply, Compose::SrcOver);
        scene.push_layer(blend, 0.5, Affine::translate((1.0, 2.0)), &clip);
        scene.pop_layer();
        match &scene.ops[..] {
            [Op::PushLayer {
                blend: b,
                alpha,
                transform,
                clip: c,
            }, Op::PopLayer] => {
                assert_eq!(*b, blend);
                assert_eq!(*alpha, 0.5);
                assert_eq!(*transform, Affine::translate((1.0, 2.0)));
                assert_eq!(c.elements(), clip.elements());
            }
            other => panic!("expected PushLayer then PopLayer, got {other:?}"),
        }
    }

    #[test]
    fn clear_drops_everything_recorded_so_far() {
        let mut scene = RecordingScene::new();
        let brush = solid(1.0, 1.0, 1.0);
        scene.fill(
            FillRule::NonZero,
            Affine::IDENTITY,
            &brush,
            None,
            &square(1.0),
            PickId::Skip,
        );
        scene.pop_layer();
        assert_eq!(scene.ops.len(), 2);
        scene.clear();
        assert!(scene.ops.is_empty());
        // Still usable for the next frame.
        scene.pop_layer();
        assert_eq!(scene.ops.len(), 1);
    }

    #[test]
    fn ops_accumulate_in_call_order() {
        let mut scene = RecordingScene::new();
        let brush = solid(1.0, 1.0, 1.0);
        scene.push_layer(BlendMode::NORMAL, 1.0, Affine::IDENTITY, &square(1.0));
        scene.fill(
            FillRule::NonZero,
            Affine::IDENTITY,
            &brush,
            None,
            &square(1.0),
            PickId::Skip,
        );
        scene.pop_layer();
        let kinds: Vec<&str> = scene
            .ops
            .iter()
            .map(|op| match op {
                Op::PushLayer { .. } => "push",
                Op::Fill { .. } => "fill",
                Op::PopLayer => "pop",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, ["push", "fill", "pop"]);
    }

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
