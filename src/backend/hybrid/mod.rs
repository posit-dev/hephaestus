//! Vello Hybrid backend: path processing and coverage on the CPU, a plain
//! render pipeline on the GPU.
//!
//! Two properties drive the design. First, coverage is computed CPU-side, so
//! the rasterizer can paint with *binary* coverage on request — a pixel is
//! either fully painted or not painted at all. That is what an id buffer
//! needs, and it is why picking here reports exactly one id per pixel rather
//! than a blend of two. Second, the GPU buffers are sized to the scene's
//! actual content instead of fixed caps, so there is no draw-count ceiling to
//! budget against.
//!
//! # Why the scene is recorded rather than written straight through
//!
//! `vello_hybrid::Scene` generates strips as each path arrives, which means it
//! needs the frame's pixel dimensions *before* the first draw. [`SceneBuilder`]
//! carries no size, so [`HybridScene`] records draws into a
//! [`RecordingScene`] and the renderer replays them once the size is known.
//! Replaying is also how the pick pass is produced: one recording feeds both
//! scenes, so enabling picking costs a second rasterization but not a second
//! set of recorded draws.

use std::collections::HashMap;

use vello_common::paint::{ImageSource, PaintType};
use vello_hybrid::{Resources, Scene};

use crate::backend::{convert, mesh, BackendError};
use crate::blend::BlendMode;
use crate::brush::{Brush, Image, Sampling};
use crate::geometry::Affine;
use crate::mesh::Mesh;
use crate::path::{FillRule, Path};
use crate::pick::{self, PickId};
use crate::scene::recording::RecordingScene;
use crate::scene::{GlyphRun, SceneBuilder};
use crate::stroke::Stroke;

mod glyph_bitmap;
#[cfg(all(feature = "webgl", target_arch = "wasm32"))]
mod webgl;
#[cfg(feature = "vello-hybrid")]
mod wgpu_renderer;

#[cfg(all(feature = "webgl", target_arch = "wasm32"))]
pub use webgl::HybridWebGlRenderer;
#[cfg(feature = "vello-hybrid")]
pub use wgpu_renderer::HybridRenderer;

/// Coverage a pick pixel must exceed to be painted at all.
///
/// The midpoint: a pixel belongs to whichever mark covers most of it. Any
/// value disables antialiasing; the choice only decides which side of a
/// half-covered pixel wins.
const PICK_ALIASING_THRESHOLD: u8 = 128;

/// Minimum stroke width (in pixels) the pick pass uses, so hairline strokes
/// remain hittable even when the visual stroke is sub-pixel.
///
/// Binary coverage makes this load-bearing rather than a nicety: a stroke
/// thinner than the threshold covers no pixel past
/// [`PICK_ALIASING_THRESHOLD`] and would vanish from the hitmap entirely.
const MIN_PICK_STROKE_WIDTH: f64 = 2.0;

/// Largest scene dimension the rasterizer accepts, in pixels.
///
/// `vello_hybrid::Scene` sizes itself in `u16`.
pub const MAX_DIMENSION: u32 = u16::MAX as u32;

// ---------- Scene ----------

/// A [`SceneBuilder`] that records draws for the Hybrid renderer to replay.
///
/// Recording rather than rasterizing immediately is what lets one set of draws
/// serve a frame whose size is only known at render time, and serve the
/// parallel pick pass as well. See the module docs.
#[derive(Debug, Default, Clone)]
pub struct HybridScene {
    ops: RecordingScene,
}

impl HybridScene {
    /// Build an empty scene.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of recorded draw operations.
    pub fn len(&self) -> usize {
        self.ops.ops.len()
    }

    /// True when nothing has been recorded.
    pub fn is_empty(&self) -> bool {
        self.ops.ops.is_empty()
    }
}

impl SceneBuilder for HybridScene {
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
        self.ops
            .fill(rule, transform, brush, brush_transform, path, pick_id);
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
        self.ops
            .stroke(stroke, transform, brush, brush_transform, path, pick_id);
    }

    fn draw_image(
        &mut self,
        image: &Image,
        transform: Affine,
        sampling: Sampling,
        alpha: f32,
        pick_id: PickId,
    ) {
        self.ops
            .draw_image(image, transform, sampling, alpha, pick_id);
    }

    fn draw_glyphs(&mut self, run: &GlyphRun<'_>, pick_id: PickId) {
        self.ops.draw_glyphs(run, pick_id);
    }

    fn draw_mesh(&mut self, mesh: &Mesh, transform: Affine, pick_id: PickId) {
        self.ops.draw_mesh(mesh, transform, pick_id);
    }

    fn push_layer(&mut self, blend: BlendMode, alpha: f32, transform: Affine, clip: &Path) {
        self.ops.push_layer(blend, alpha, transform, clip);
    }

    fn pop_layer(&mut self) {
        self.ops.pop_layer();
    }
}

// ---------- replay ----------

/// Which of the two scenes a replay is filling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pass {
    /// The visible frame: the caller's brushes, blend modes and antialiasing.
    Display,
    /// The id buffer: solid encoded ids, normalized blending, binary coverage.
    Pick,
}

/// Key identifying an image's pixels, so one upload serves every draw of it.
///
/// Peniko blobs carry a process-local id, which is exactly the identity an
/// atlas wants: two handles onto the same decoded pixels share it.
fn image_key(image: &Image) -> u64 {
    image.data.id()
}

/// Replays recorded draws into a `vello_hybrid::Scene`.
///
/// One writer per pass. The pick pass differs in three ways: solid ids
/// replace brushes, blending and layer alpha are normalized so ids cannot
/// fade toward the no-hit sentinel, and hairline strokes are widened.
struct Writer<'a> {
    scene: &'a mut Scene,
    resources: &'a mut Resources,
    pass: Pass,
    /// Atlas handle per image, filled in before replay — uploading needs the
    /// device, which a [`SceneBuilder`] has no access to.
    images: &'a HashMap<u64, ImageSource>,
}

impl Writer<'_> {
    /// Paint for a draw, or `None` when this pass should skip the draw.
    fn paint(&self, brush: &Brush, pick_id: PickId) -> Option<PaintType> {
        match self.pass {
            Pass::Display => match brush {
                Brush::Solid(color) => Some((*color).into()),
                Brush::Gradient(gradient) => Some(gradient.clone().into()),
                Brush::Image(image) => self.image_paint(
                    &image.image,
                    image.sampler.quality,
                    image.sampler.x_extend,
                    image.sampler.y_extend,
                ),
            },
            Pass::Pick => pick::raw_id(pick_id).map(|id| pick::id_to_color(id).into()),
        }
    }

    /// Paint sampling an already-uploaded image, or `None` if it never made
    /// it into the atlas.
    fn image_paint(
        &self,
        image: &Image,
        quality: peniko::ImageQuality,
        x_extend: peniko::Extend,
        y_extend: peniko::Extend,
    ) -> Option<PaintType> {
        let source = self.images.get(&image_key(image))?;
        Some(
            vello_common::paint::Image {
                image: source.clone(),
                sampler: peniko::ImageSampler {
                    x_extend,
                    y_extend,
                    quality,
                    // Opacity cannot ride on the sampler: the paint encoder
                    // rejects any value but 1.0. Callers' alpha becomes an
                    // opacity layer instead.
                    alpha: 1.0,
                },
            }
            .into(),
        )
    }

    /// Apply the caller's transform and brush transform to the scene state.
    fn set_placement(&mut self, transform: Affine, brush_transform: Option<Affine>) {
        self.scene.set_transform(transform);
        match brush_transform {
            Some(bt) => self.scene.set_paint_transform(bt),
            None => self.scene.reset_paint_transform(),
        }
    }
}

impl SceneBuilder for Writer<'_> {
    fn clear(&mut self) {
        self.scene.reset();
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
        let Some(paint) = self.paint(brush, pick_id) else {
            return;
        };
        self.set_placement(transform, brush_transform);
        self.scene.set_fill_rule(convert::fill_rule(rule));
        self.scene.set_paint(paint);
        self.scene.fill_path(path);
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
        let Some(paint) = self.paint(brush, pick_id) else {
            return;
        };
        let mut stroke = stroke.clone();
        if self.pass == Pass::Pick && stroke.width < MIN_PICK_STROKE_WIDTH {
            stroke.width = MIN_PICK_STROKE_WIDTH;
        }
        self.set_placement(transform, brush_transform);
        self.scene.set_stroke(stroke);
        self.scene.set_paint(paint);
        self.scene.stroke_path(path);
    }

    fn draw_image(
        &mut self,
        image: &Image,
        transform: Affine,
        sampling: Sampling,
        alpha: f32,
        pick_id: PickId,
    ) {
        let bounds = crate::geometry::Rect::new(0.0, 0.0, image.width.into(), image.height.into());
        let paint = match self.pass {
            Pass::Display => self.image_paint(
                image,
                convert::sampling_to_quality(sampling),
                peniko::Extend::Pad,
                peniko::Extend::Pad,
            ),
            Pass::Pick => pick::raw_id(pick_id).map(|id| pick::id_to_color(id).into()),
        };
        let Some(paint) = paint else {
            return;
        };
        // Image opacity has to be a layer rather than a sampler field; the
        // pick pass ignores it, since a faded id is a wrong id.
        let layered = self.pass == Pass::Display && alpha < 1.0;
        if layered {
            self.scene.push_opacity_layer(alpha);
        }
        self.set_placement(transform, None);
        self.scene.set_fill_rule(peniko::Fill::NonZero);
        self.scene.set_paint(paint);
        self.scene.fill_rect(&bounds);
        if layered {
            self.scene.pop_layer();
        }
    }

    fn draw_glyphs(&mut self, run: &GlyphRun<'_>, pick_id: PickId) {
        let Some(paint) = self.paint(run.brush, pick_id) else {
            return;
        };
        let layered = self.pass == Pass::Display && run.brush_alpha < 1.0;
        if layered {
            self.scene.push_opacity_layer(run.brush_alpha);
        }

        // Glyphs a bitmap strike serves leave the glyph pipeline and draw as
        // images instead; see the `glyph_bitmap` module docs for why.
        let split = glyph_bitmap::split(
            run.font,
            run.font_size,
            run.glyph_transform,
            glyph_bitmap::foreground(run.brush),
            run.glyphs,
        );
        let (outlines, strikes) = match &split {
            Some((outlines, strikes)) => (outlines.as_slice(), strikes.as_slice()),
            None => (run.glyphs, &[] as &[glyph_bitmap::Strike]),
        };

        if !outlines.is_empty() {
            self.scene.set_transform(run.transform);
            self.scene.reset_paint_transform();
            self.scene.set_paint(paint);

            let stroked = match (self.pass, run.style) {
                (Pass::Display, Some(stroke)) => {
                    self.scene.set_stroke(stroke.clone());
                    true
                }
                // The pick pass fills glyph outlines whatever the display
                // style: an outlined glyph should still be hittable in its
                // interior.
                _ => false,
            };

            let glyphs = outlines
                .iter()
                .map(|g| glifo::Glyph {
                    id: g.id,
                    x: g.x,
                    y: g.y,
                })
                .collect::<Vec<_>>();

            let mut builder = self
                .scene
                .glyph_run(self.resources, run.font.data())
                .font_size(run.font_size)
                .hint(run.hint);
            if let Some(gt) = run.glyph_transform {
                builder = builder.glyph_transform(gt);
            }
            if stroked {
                builder.stroke_glyphs(glyphs.into_iter());
            } else {
                builder.fill_glyphs(glyphs.into_iter());
            }
        }

        for strike in strikes {
            // Alpha is already carried by the layer around the whole run,
            // and a strike paints its own colors rather than the run brush.
            self.draw_image(
                &strike.image,
                run.transform * strike.transform,
                Sampling::Bilinear,
                1.0,
                pick_id,
            );
        }

        if layered {
            self.scene.pop_layer();
        }
    }

    fn draw_mesh(&mut self, mesh_data: &Mesh, transform: Affine, pick_id: PickId) {
        mesh::decompose(mesh_data, transform, pick_id, self);
    }

    fn push_layer(&mut self, blend: BlendMode, alpha: f32, transform: Affine, clip: &Path) {
        self.scene.set_transform(transform);
        match self.pass {
            Pass::Display => self.scene.push_layer(
                Some(clip),
                Some(convert::blend_mode(blend)),
                Some(alpha),
                None,
                None,
            ),
            // Mirror the clip so subsequent draws are clipped identically,
            // but drop the blend mode and alpha: either would distort the
            // encoded ids, and a translucent layer would fade them toward
            // the no-hit sentinel.
            Pass::Pick => self.scene.push_layer(Some(clip), None, None, None, None),
        }
    }

    fn pop_layer(&mut self) {
        self.scene.pop_layer();
    }
}

fn dimension(v: u32) -> Result<u16, BackendError> {
    u16::try_from(v).map_err(|_| {
        BackendError::Other(format!(
            "frame dimension {v} exceeds the {MAX_DIMENSION} px the hybrid backend supports"
        ))
    })
}

/// Every distinct image the recording paints with, in first-drawn order —
/// including the bitmap strike behind each color glyph, which this backend
/// draws as an image like any other.
///
/// Images arrive as CPU pixels but the rasterizer only samples handles into
/// its atlas, so each one has to be uploaded before a replay can reference it.
fn recorded_images(ops: &RecordingScene) -> Vec<Image> {
    use crate::scene::recording::Op;

    let mut keys: Vec<u64> = Vec::new();
    let mut images: Vec<Image> = Vec::new();
    let mut push = |image: Image| {
        let key = image_key(&image);
        if !keys.contains(&key) {
            keys.push(key);
            images.push(image);
        }
    };
    for op in &ops.ops {
        match op {
            Op::Fill { brush, .. } | Op::Stroke { brush, .. } => {
                if let Brush::Image(b) = brush {
                    push(b.image.clone());
                }
            }
            Op::DrawGlyphs(run) => {
                if let Brush::Image(b) = &run.brush {
                    push(b.image.clone());
                }
                // A bitmap strike is drawn as an image, so it needs the same
                // upload every other image does.
                if let Some((_, strikes)) = glyph_bitmap::split(
                    &run.font,
                    run.font_size,
                    run.glyph_transform,
                    glyph_bitmap::foreground(&run.brush),
                    &run.glyphs,
                ) {
                    for strike in strikes {
                        push(strike.image);
                    }
                }
            }
            Op::DrawImage { image, .. } => push(image.clone()),
            _ => {}
        }
    }
    images
}

/// Convert premultiplied RGBA8 in place to the straight alpha every
/// [`Renderer`](crate::backend::Renderer) hands out.
///
/// Only the wgpu path needs it: that is the one with a `render_to_buffer` to
/// hand bytes out of.
///
/// The rasterizer composites premultiplied, so without this a PNG writer
/// would darken every partially transparent pixel.
#[cfg(feature = "vello-hybrid")]
fn unpremultiply(buf: &mut [u8]) {
    for px in buf.chunks_exact_mut(4) {
        let a = px[3];
        if a == 0 || a == 255 {
            continue;
        }
        let a32 = u32::from(a);
        for c in &mut px[..3] {
            *c = ((u32::from(*c) * 255 + a32 / 2) / a32).min(255) as u8;
        }
    }
}
