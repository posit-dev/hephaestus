//! Vello Hybrid backend: path processing and coverage on the CPU, a plain
//! render pipeline on the GPU.
//!
//! Two properties drive the design. First, coverage is computed CPU-side, so
//! the rasteriser can paint with *binary* coverage on request — a pixel is
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
//! scenes, so enabling picking costs a second rasterisation but not a second
//! set of recorded draws.

use std::collections::HashMap;

use vello_common::paint::{ImageSource, PaintType};
use vello_hybrid::{
    RenderSize, RenderTargetConfig, Renderer as HRenderer, Resources, Scene, TextureBindings,
};

use crate::backend::{convert, mesh, BackendError, Renderer};
use crate::blend::BlendMode;
use crate::brush::{Brush, Image, Sampling};
use crate::color::Color;
use crate::geometry::Affine;
use crate::mesh::Mesh;
use crate::path::{FillRule, Path};
use crate::pick::{self, PickId};
use crate::scene::recording::RecordingScene;
use crate::scene::{GlyphRun, SceneBuilder};
use crate::stroke::Stroke;

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

/// Largest scene dimension the rasteriser accepts, in pixels.
///
/// `vello_hybrid::Scene` sizes itself in `u16`.
pub const MAX_DIMENSION: u32 = u16::MAX as u32;

// ---------- Scene ----------

/// A [`SceneBuilder`] that records draws for the Hybrid renderer to replay.
///
/// Recording rather than rasterising immediately is what lets one set of draws
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
    /// The id buffer: solid encoded ids, normalised blending, binary coverage.
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
/// replace brushes, blending and layer alpha are normalised so ids cannot
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
        self.scene.set_transform(run.transform);
        self.scene.reset_paint_transform();
        self.scene.set_paint(paint);

        let stroked = match (self.pass, run.style) {
            (Pass::Display, Some(stroke)) => {
                self.scene.set_stroke(stroke.clone());
                true
            }
            // The pick pass fills glyph outlines whatever the display style:
            // an outlined glyph should still be hittable in its interior.
            _ => false,
        };

        let glyphs = run
            .glyphs
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

// ---------- Renderer ----------

/// Render target plus the readback buffer that drains it, sized for the
/// current frame. Recreated on size change.
struct Target {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    readback: wgpu::Buffer,
    width: u32,
    height: u32,
    /// Bytes per row in the readback buffer (padded to wgpu's alignment).
    padded_bytes_per_row: u32,
}

impl Target {
    /// Allocate a render-attachment texture and a row-padded readback buffer.
    ///
    /// `RENDER_ATTACHMENT` rather than `STORAGE_BINDING`: this backend
    /// rasterises through a render pipeline, not a compute shader.
    fn new(device: &wgpu::Device, width: u32, height: u32) -> Self {
        let bytes_per_row = width * 4;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded_bytes_per_row = bytes_per_row.div_ceil(align) * align;

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("hephaestus.hybrid.target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("hephaestus.hybrid.readback"),
            size: u64::from(padded_bytes_per_row) * u64::from(height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        Self {
            texture,
            view,
            readback,
            width,
            height,
            padded_bytes_per_row,
        }
    }
}

/// The half of the renderer bound to a frame size.
///
/// `RenderTargetConfig` fixes the dimensions a `vello_hybrid::Renderer` is
/// built for, and `Scene` its own, so a size change rebuilds both. The image
/// atlas lives here too and is therefore invalidated by a resize.
struct Sized {
    renderer: HRenderer,
    resources: Resources,
    display: Scene,
    pick: Option<Scene>,
    images: HashMap<u64, ImageSource>,
    width: u32,
    height: u32,
}

/// Hephaestus Hybrid renderer: owns the wgpu device and queue, the recorded
/// scene, and the per-size rasterisation state.
///
/// When constructed via [`Self::with_picking`], every render also replays the
/// recording into a pick scene rasterised with binary coverage, reads it back,
/// and caches it as the hitmap behind [`Self::pick_at`].
pub struct HybridRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    scene: HybridScene,
    picking: bool,
    sized: Option<Sized>,
    target: Option<Target>,
    pick_target: Option<Target>,
    /// Decoded pick pixels of the most recent render, one `u32` per pixel.
    hitmap: Option<Vec<u32>>,
    hitmap_dims: Option<(u32, u32)>,
}

impl HybridRenderer {
    /// Build a renderer with no picking machinery. File-export workloads
    /// should use this form; nothing in the pick path is allocated.
    pub fn new() -> Result<Self, BackendError> {
        pollster::block_on(Self::new_async(false))
    }

    /// Build a renderer with picking enabled. Each render additionally
    /// rasterises the pick scene with binary coverage and reads it back.
    pub fn with_picking() -> Result<Self, BackendError> {
        pollster::block_on(Self::new_async(true))
    }

    /// Build a renderer that shares an existing wgpu device and queue — e.g.
    /// the device backing a window's swap chain.
    ///
    /// `device` and `queue` are handles (Arc-backed in wgpu); the host keeps
    /// its own and the renderer holds clones.
    pub fn with_device(device: &wgpu::Device, queue: &wgpu::Queue) -> Result<Self, BackendError> {
        Ok(Self::build(device.clone(), queue.clone(), false))
    }

    /// Like [`Self::with_device`] but enables picking.
    pub fn with_device_and_picking(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Result<Self, BackendError> {
        Ok(Self::build(device.clone(), queue.clone(), true))
    }

    async fn new_async(picking: bool) -> Result<Self, BackendError> {
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
        // GL sits alongside PRIMARY so a Linux host without Vulkan reaches
        // the GLES backend rather than finding no adapter. Unlike the
        // compute-shader backend this one has no stage WebGL2 lacks, so the
        // flag is meaningful on more targets.
        desc.backends =
            wgpu::Backends::from_env().unwrap_or(wgpu::Backends::PRIMARY | wgpu::Backends::GL);
        let instance = wgpu::Instance::new(desc);
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await
            .map_err(|_| BackendError::NoAdapter)?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("hephaestus.hybrid.device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::default(),
                trace: wgpu::Trace::Off,
                experimental_features: wgpu::ExperimentalFeatures::default(),
            })
            .await
            .map_err(|e| BackendError::DeviceRequest(e.to_string()))?;
        Ok(Self::build(device, queue, picking))
    }

    fn build(device: wgpu::Device, queue: wgpu::Queue, picking: bool) -> Self {
        Self {
            device,
            queue,
            scene: HybridScene::new(),
            picking,
            sized: None,
            target: None,
            pick_target: None,
            hitmap: None,
            hitmap_dims: None,
        }
    }

    /// Id recorded at the given pixel, or `None` for a miss.
    ///
    /// Returns `None` when picking is disabled, nothing has been rendered
    /// yet, the coordinates fall outside the last render, or nothing
    /// pickable covered the pixel. Binary coverage means the answer is the
    /// id of exactly one primitive — never a blend of two.
    pub fn pick_at(&self, x: u32, y: u32) -> Option<u32> {
        let (w, h) = self.hitmap_dims?;
        if x >= w || y >= h {
            return None;
        }
        let hitmap = self.hitmap.as_ref()?;
        pick::decode(hitmap[(y * w + x) as usize])
    }

    /// Raw pick pixels of the most recent render, for bulk queries.
    ///
    /// Row-major, `width * height` entries. Interpret each with
    /// [`pick::decode`].
    pub fn hitmap(&self) -> Option<&[u32]> {
        self.hitmap.as_deref()
    }

    /// Rebuild the size-bound state when the requested frame size differs
    /// from what it was built for.
    fn ensure_sized(&mut self, width: u32, height: u32) -> Result<(), BackendError> {
        if self
            .sized
            .as_ref()
            .is_some_and(|s| s.width == width && s.height == height)
        {
            return Ok(());
        }
        let (w16, h16) = (dimension(width)?, dimension(height)?);
        let (renderer, resources) = HRenderer::new(
            &self.device,
            &RenderTargetConfig {
                format: wgpu::TextureFormat::Rgba8Unorm,
                width,
                height,
            },
        );
        self.sized = Some(Sized {
            renderer,
            resources,
            display: Scene::new(w16, h16),
            pick: self.picking.then(|| Scene::new(w16, h16)),
            images: HashMap::new(),
            width,
            height,
        });
        Ok(())
    }
}

/// Narrow a pixel dimension to the `u16` the rasteriser sizes scenes in.
fn dimension(v: u32) -> Result<u16, BackendError> {
    u16::try_from(v).map_err(|_| {
        BackendError::Other(format!(
            "frame dimension {v} exceeds the {MAX_DIMENSION} px the hybrid backend supports"
        ))
    })
}

/// Every distinct image the recording paints with, in first-drawn order.
///
/// Images arrive as CPU pixels but the rasteriser only samples handles into
/// its atlas, so each one has to be uploaded before a replay can reference it.
fn recorded_images(ops: &RecordingScene) -> Vec<&Image> {
    use crate::scene::recording::Op;

    let mut keys: Vec<u64> = Vec::new();
    let mut images: Vec<&Image> = Vec::new();
    for op in &ops.ops {
        let image = match op {
            Op::Fill { brush, .. } | Op::Stroke { brush, .. } => match brush {
                Brush::Image(b) => Some(&b.image),
                _ => None,
            },
            Op::DrawGlyphs(run) => match &run.brush {
                Brush::Image(b) => Some(&b.image),
                _ => None,
            },
            Op::DrawImage { image, .. } => Some(image),
            _ => None,
        };
        if let Some(image) = image {
            let key = image_key(image);
            if !keys.contains(&key) {
                keys.push(key);
                images.push(image);
            }
        }
    }
    images
}

/// Convert premultiplied RGBA8 in place to the straight alpha every
/// [`Renderer`] hands out.
///
/// The rasteriser composites premultiplied, so without this a PNG writer
/// would darken every partially transparent pixel.
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

impl HybridRenderer {
    /// Upload every image the recording needs, reusing atlas handles already
    /// held for this size.
    fn upload_images(&mut self, encoder: &mut wgpu::CommandEncoder) -> Result<(), BackendError> {
        let images = recorded_images(&self.scene.ops);
        if images.is_empty() {
            return Ok(());
        }
        let sized = self.sized.as_mut().expect("sized state ensured");
        for image in images {
            let key = image_key(image);
            if sized.images.contains_key(&key) {
                continue;
            }
            // Their conversion handles both the format narrowing and the
            // premultiply; we only need the pixmap back out of it to upload.
            let ImageSource::Pixmap(pixmap) = ImageSource::from_peniko_image_data(image) else {
                return Err(BackendError::Other(
                    "image conversion did not yield pixel data".into(),
                ));
            };
            let transparency = pixmap.may_have_transparency();
            let id = sized.renderer.upload_image(
                &mut sized.resources,
                &self.device,
                &self.queue,
                encoder,
                &pixmap,
            );
            sized.images.insert(
                key,
                ImageSource::opaque_id_with_transparency_hint(id, transparency),
            );
        }
        Ok(())
    }

    /// Replay the recording into the display scene, and into the pick scene
    /// when picking is on.
    fn replay(&mut self, background: Color, width: u32, height: u32) {
        let sized = self.sized.as_mut().expect("sized state ensured");
        let frame = crate::geometry::Rect::new(0.0, 0.0, width.into(), height.into());

        sized.display.reset();
        sized.display.set_aliasing_threshold(None);
        // No base-colour parameter here, so the background is a draw. It has
        // to be the first one.
        sized.display.set_transform(Affine::IDENTITY);
        sized.display.set_paint(background);
        sized.display.fill_rect(&frame);

        let mut writer = Writer {
            scene: &mut sized.display,
            resources: &mut sized.resources,
            pass: Pass::Display,
            images: &sized.images,
        };
        self.scene.ops.replay(&mut writer);

        if let Some(pick) = sized.pick.as_mut() {
            pick.reset();
            // The one line the whole backend exists for: a pick pixel is
            // painted by exactly one primitive, so an edge reports a real id
            // instead of a blend of the two ids either side of it.
            pick.set_aliasing_threshold(Some(PICK_ALIASING_THRESHOLD));
            // No background: an uncovered pick pixel must stay at alpha 0,
            // which is what `pick::decode` reads as "no hit".
            let mut writer = Writer {
                scene: pick,
                resources: &mut sized.resources,
                pass: Pass::Pick,
                images: &sized.images,
            };
            self.scene.ops.replay(&mut writer);
        }
    }
}

impl Renderer for HybridRenderer {
    type Scene = HybridScene;

    fn scene(&mut self) -> &mut Self::Scene {
        &mut self.scene
    }

    fn render_to_buffer(
        &mut self,
        width: u32,
        height: u32,
        background: Color,
        out: &mut [u8],
    ) -> Result<(), BackendError> {
        let expected = (width as usize) * (height as usize) * 4;
        if out.len() != expected {
            return Err(BackendError::BufferSize {
                expected,
                actual: out.len(),
            });
        }
        if width == 0 || height == 0 {
            return Err(BackendError::Other(
                "cannot render a zero-sized frame".into(),
            ));
        }
        self.ensure_sized(width, height)?;
        if self
            .target
            .as_ref()
            .is_none_or(|t| t.width != width || t.height != height)
        {
            self.target = Some(Target::new(&self.device, width, height));
        }
        if self.picking
            && self
                .pick_target
                .as_ref()
                .is_none_or(|t| t.width != width || t.height != height)
        {
            self.pick_target = Some(Target::new(&self.device, width, height));
        }

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("hephaestus.hybrid.render"),
            });
        self.upload_images(&mut encoder)?;
        self.replay(background, width, height);

        let render_size = RenderSize { width, height };
        let bindings = TextureBindings::new();
        {
            let sized = self.sized.as_mut().expect("sized state ensured");
            let target = self.target.as_ref().expect("target ensured");
            sized
                .renderer
                .render(
                    &sized.display,
                    &mut sized.resources,
                    &self.device,
                    &self.queue,
                    &mut encoder,
                    &render_size,
                    &target.view,
                    &bindings,
                )
                .map_err(|e| BackendError::Other(format!("hybrid render: {e}")))?;
            if let (Some(pick), Some(pick_target)) =
                (sized.pick.as_ref(), self.pick_target.as_ref())
            {
                sized
                    .renderer
                    .render(
                        pick,
                        &mut sized.resources,
                        &self.device,
                        &self.queue,
                        &mut encoder,
                        &render_size,
                        &pick_target.view,
                        &bindings,
                    )
                    .map_err(|e| BackendError::Other(format!("hybrid pick render: {e}")))?;
            }
        }

        // One submit for both passes and both copies, matching the display +
        // pick round-trip the other backend does.
        let target = self.target.as_ref().expect("target ensured");
        copy_to_readback(&mut encoder, target, width, height);
        let picking = self.picking && self.pick_target.is_some();
        if picking {
            let pick_target = self.pick_target.as_ref().expect("pick target ensured");
            copy_to_readback(&mut encoder, pick_target, width, height);
        }
        self.queue.submit(std::iter::once(encoder.finish()));

        let display_slice = target.readback.slice(..);
        let (display_tx, display_rx) = futures_intrusive::channel::shared::oneshot_channel();
        display_slice.map_async(wgpu::MapMode::Read, move |res| {
            let _ = display_tx.send(res);
        });
        let pick_rx = if picking {
            let pick_target = self.pick_target.as_ref().expect("pick target ensured");
            let (tx, rx) = futures_intrusive::channel::shared::oneshot_channel();
            pick_target
                .readback
                .slice(..)
                .map_async(wgpu::MapMode::Read, move |res| {
                    let _ = tx.send(res);
                });
            Some(rx)
        } else {
            None
        };

        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        await_map(pollster::block_on(display_rx.receive()))?;
        if let Some(rx) = pick_rx.as_ref() {
            await_map(pollster::block_on(rx.receive()))?;
        }

        let row_bytes = (width as usize) * 4;
        {
            let data = display_slice.get_mapped_range();
            let padded = target.padded_bytes_per_row as usize;
            for y in 0..height as usize {
                out[y * row_bytes..(y + 1) * row_bytes]
                    .copy_from_slice(&data[y * padded..y * padded + row_bytes]);
            }
        }
        target.readback.unmap();
        // The rasteriser composites premultiplied; every `Renderer` hands out
        // straight alpha.
        unpremultiply(out);

        if picking {
            let pick_target = self.pick_target.as_ref().expect("pick target ensured");
            let total_px = (width as usize) * (height as usize);
            let hitmap = self.hitmap.get_or_insert_with(Vec::new);
            hitmap.clear();
            hitmap.resize(total_px, 0);
            let row_px = width as usize;
            {
                let data = pick_target.readback.slice(..).get_mapped_range();
                let padded = pick_target.padded_bytes_per_row as usize;
                for y in 0..height as usize {
                    let dst: &mut [u8] =
                        bytemuck::cast_slice_mut(&mut hitmap[y * row_px..(y + 1) * row_px]);
                    dst.copy_from_slice(&data[y * padded..y * padded + row_bytes]);
                }
            }
            pick_target.readback.unmap();
            self.hitmap_dims = Some((width, height));
        }
        Ok(())
    }
}

/// Queue the texture-to-buffer copy that drains a target to CPU.
fn copy_to_readback(encoder: &mut wgpu::CommandEncoder, target: &Target, width: u32, height: u32) {
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &target.texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &target.readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(target.padded_bytes_per_row),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
}

/// Turn a `map_async` completion into a backend error.
fn await_map(res: Option<Result<(), wgpu::BufferAsyncError>>) -> Result<(), BackendError> {
    match res {
        Some(Ok(())) => Ok(()),
        Some(Err(e)) => Err(BackendError::Readback(e.to_string())),
        None => Err(BackendError::Readback("map_async sender dropped".into())),
    }
}
