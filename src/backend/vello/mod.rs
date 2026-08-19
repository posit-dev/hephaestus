//! Vello backend. Wraps `vello::Scene` to implement [`SceneBuilder`] and owns
//! the wgpu device/queue/renderer needed for headless rasterization.

mod convert;
mod mesh;

use std::num::NonZeroUsize;

use crate::geometry::Shape as _;
use vello::{AaConfig, AaSupport, RenderParams, Renderer as VRenderer, RendererOptions, Scene};

use crate::backend::{BackendError, Renderer, WgpuRenderer};
use crate::blend::BlendMode;
use crate::brush::{Brush, Image, Sampling};
use crate::color::Color;
use crate::geometry::{Affine, Point};
use crate::mesh::Mesh;

use crate::path::{FillRule, Path};
use crate::pick::{self, PickId};
use crate::scene::{GlyphRun, SceneBuilder};
use crate::stroke::Stroke;
use mesh::{
    detect_quad_pair, detect_uniform_fan, polygon_path, quad_gradient_brush, quad_path,
    triangle_gradient_brush, triangle_path,
};

/// Minimum stroke width (in pixels) the pick pass uses, so hairline strokes
/// remain hittable even when the visual stroke is sub-pixel.
const MIN_PICK_STROKE_WIDTH: f64 = 2.0;

/// Largest number of draw-info words vello can rasterise in one pass.
///
/// Vello sizes its `bin_data` GPU buffer to a fixed `1 << 18` words and stores
/// the scene's draw-info stream at its front. A scene whose stream is longer
/// cannot be configured at all — the size arithmetic underflows before any GPU
/// work is dispatched. [`VelloScene::draw_info_words`] measures a scene against
/// this budget; the render entry points reject an over-budget scene with
/// [`BackendError::SceneTooLarge`].
///
/// A solid-brush fill or stroke costs one word, so this caps a scene at ~262k
/// flat-coloured objects. Gradient and image brushes cost more per draw.
pub const MAX_DRAW_INFO_WORDS: u32 = 1 << 18;

/// A `SceneBuilder` that writes into a `vello::Scene`.
///
/// When picking is enabled (constructed via `with_picking`), every
/// drawing call is also recorded into a parallel "pick" scene with its brush
/// replaced by a solid colour encoding the call's [`PickId`]. The renderer
/// rasterises both scenes; the pick scene is read back to a CPU u32 buffer
/// that powers hit tests.
pub struct VelloScene {
    inner: Scene,
    pick: Option<Scene>,
}

impl VelloScene {
    /// Build a scene with no picking machinery — file-export workloads should
    /// use this form (zero overhead).
    pub fn new() -> Self {
        Self {
            inner: Scene::new(),
            pick: None,
        }
    }

    /// Build a scene that records into both the display scene and a parallel
    /// pick scene. Used internally by [`VelloRenderer::with_picking`].
    pub(crate) fn with_picking() -> Self {
        Self {
            inner: Scene::new(),
            pick: Some(Scene::new()),
        }
    }

    /// Borrow the underlying `vello::Scene` (e.g. to render it).
    pub fn raw(&self) -> &Scene {
        &self.inner
    }

    /// Borrow the parallel pick scene, if picking is enabled.
    pub(crate) fn raw_pick(&self) -> Option<&Scene> {
        self.pick.as_ref()
    }

    /// Draw-info words the encoded display scene occupies, to be compared
    /// against [`MAX_DRAW_INFO_WORDS`].
    pub fn draw_info_words(&self) -> u32 {
        draw_info_words(&self.inner)
    }

    /// True when both the display scene and the pick scene fit the backend's
    /// draw budget, so a render will not be rejected.
    pub fn fits_draw_budget(&self) -> bool {
        check_draw_budget(&self.inner).is_ok()
            && self
                .pick
                .as_ref()
                .is_none_or(|p| check_draw_budget(p).is_ok())
    }
}

/// Length of a scene's draw-info stream, measured the way vello sizes it.
fn draw_info_words(scene: &Scene) -> u32 {
    scene
        .encoding()
        .draw_tags
        .iter()
        .map(|tag| tag.info_size())
        .sum()
}

fn check_draw_budget(scene: &Scene) -> Result<(), BackendError> {
    let used = draw_info_words(scene);
    if used > MAX_DRAW_INFO_WORDS {
        return Err(BackendError::SceneTooLarge {
            used,
            max: MAX_DRAW_INFO_WORDS,
        });
    }
    Ok(())
}

impl Default for VelloScene {
    fn default() -> Self {
        Self::new()
    }
}

impl SceneBuilder for VelloScene {
    /// Clears both the display scene and, when picking is enabled, the
    /// parallel pick scene.
    fn clear(&mut self) {
        self.inner.reset();
        if let Some(p) = &mut self.pick {
            p.reset();
        }
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
        let fill_rule = convert::fill_rule(rule);
        self.inner
            .fill(fill_rule, transform, brush, brush_transform, path);
        if let Some(pick) = &mut self.pick {
            if let Some(id) = pick::raw_id(pick_id) {
                let pick_brush = Brush::Solid(pick::id_to_color(id));
                pick.fill(fill_rule, transform, &pick_brush, None, path);
            }
        }
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
        self.inner
            .stroke(stroke, transform, brush, brush_transform, path);
        if let Some(pick) = &mut self.pick {
            if let Some(id) = pick::raw_id(pick_id) {
                let pick_brush = Brush::Solid(pick::id_to_color(id));
                let mut pick_stroke = stroke.clone();
                if pick_stroke.width < MIN_PICK_STROKE_WIDTH {
                    pick_stroke.width = MIN_PICK_STROKE_WIDTH;
                }
                pick.stroke(&pick_stroke, transform, &pick_brush, None, path);
            }
        }
    }

    fn draw_image(
        &mut self,
        image: &Image,
        transform: Affine,
        sampling: Sampling,
        alpha: f32,
        pick_id: PickId,
    ) {
        let sampler = peniko::ImageSampler {
            x_extend: peniko::Extend::Pad,
            y_extend: peniko::Extend::Pad,
            quality: convert::sampling_to_quality(sampling),
            alpha,
        };
        let brush = peniko::ImageBrush {
            image: image.clone(),
            sampler,
        };
        self.inner.draw_image(&brush, transform);
        if let Some(pick) = &mut self.pick {
            if let Some(id) = pick::raw_id(pick_id) {
                let pick_brush = Brush::Solid(pick::id_to_color(id));
                let bounds =
                    crate::geometry::Rect::new(0.0, 0.0, image.width as f64, image.height as f64)
                        .to_path(0.1);
                pick.fill(peniko::Fill::NonZero, transform, &pick_brush, None, &bounds);
            }
        }
    }

    fn draw_glyphs(&mut self, run: &GlyphRun<'_>, pick_id: PickId) {
        let style: peniko::StyleRef<'_> = match run.style {
            Some(stroke) => peniko::StyleRef::from(stroke),
            None => peniko::StyleRef::from(peniko::Fill::NonZero),
        };
        let builder = self
            .inner
            .draw_glyphs(run.font.data())
            .font_size(run.font_size)
            .transform(run.transform)
            .glyph_transform(run.glyph_transform)
            .brush(run.brush)
            .brush_alpha(run.brush_alpha)
            .hint(run.hint);
        builder.draw(
            style,
            run.glyphs.iter().map(|g| vello::Glyph {
                id: g.id,
                x: g.x,
                y: g.y,
            }),
        );

        if let Some(pick) = &mut self.pick {
            if let Some(id) = pick::raw_id(pick_id) {
                let pick_brush = Brush::Solid(pick::id_to_color(id));
                let pick_style: peniko::StyleRef<'_> = match run.style {
                    Some(stroke) => peniko::StyleRef::from(stroke),
                    None => peniko::StyleRef::from(peniko::Fill::NonZero),
                };
                let pick_builder = pick
                    .draw_glyphs(run.font.data())
                    .font_size(run.font_size)
                    .transform(run.transform)
                    .glyph_transform(run.glyph_transform)
                    .brush(&pick_brush)
                    .brush_alpha(1.0)
                    .hint(run.hint);
                pick_builder.draw(
                    pick_style,
                    run.glyphs.iter().map(|g| vello::Glyph {
                        id: g.id,
                        x: g.x,
                        y: g.y,
                    }),
                );
            }
        }
    }

    fn draw_mesh(&mut self, mesh: &Mesh, transform: Affine, pick_id: PickId) {
        // Vello (and peniko) has no native indexed-mesh primitive, so
        // decompose into `fill` calls. To eliminate the AA seam along
        // the shared diagonal of adjacent triangles forming a quad,
        // we detect the pattern `[A, B, C, A, C, D]` (the canonical
        // ribbon emission shape) and emit a single 4-vertex polygon
        // fill for the merged quad with one gradient brush. Triangles
        // that don't match this pattern fall back to the per-triangle
        // path, which is correct but has visible per-triangle bands
        // for general meshes with three distinct colours per
        // triangle.
        let pick_enabled = pick::raw_id(pick_id).is_some();
        let pick_brush = if pick_enabled && self.pick.is_some() {
            Some(Brush::Solid(pick::id_to_color(
                pick::raw_id(pick_id).unwrap_or(0),
            )))
        } else {
            None
        };

        let mut i = 0;
        let indices = &mesh.indices;
        while i + 3 <= indices.len() {
            // 1. Try a fan of ≥ 2 triangles all sharing a single
            //    vertex and a uniform colour. Eliminates internal
            //    fan seams (round caps, round joins).
            if let Some((boundary, advance)) = detect_uniform_fan(indices, i, &mesh.colors) {
                let pts: Vec<Point> = boundary
                    .iter()
                    .map(|&idx| mesh.vertices[idx as usize])
                    .collect();
                let path = polygon_path(&pts);
                let brush = Brush::Solid(mesh.colors[boundary[0] as usize]);
                self.inner
                    .fill(peniko::Fill::NonZero, transform, &brush, None, &path);
                if let (Some(pick), Some(pb)) = (&mut self.pick, &pick_brush) {
                    pick.fill(peniko::Fill::NonZero, transform, pb, None, &path);
                }
                i += advance;
                continue;
            }
            // 2. Try a quad of two triangles forming `[A, B, C, A, C,
            //    D]` (canonical ribbon strip emission). Handles
            //    per-vertex colour via `quad_gradient_brush`.
            let merged = if i + 6 <= indices.len() {
                detect_quad_pair(&indices[i..i + 6])
            } else {
                None
            };
            if let Some([a, b, c, d]) = merged {
                let pts = [
                    mesh.vertices[a as usize],
                    mesh.vertices[b as usize],
                    mesh.vertices[c as usize],
                    mesh.vertices[d as usize],
                ];
                let colors = [
                    mesh.colors[a as usize],
                    mesh.colors[b as usize],
                    mesh.colors[c as usize],
                    mesh.colors[d as usize],
                ];
                let path = quad_path(&pts);
                let brush = quad_gradient_brush(&pts, &colors);
                self.inner
                    .fill(peniko::Fill::NonZero, transform, &brush, None, &path);
                if let (Some(pick), Some(pb)) = (&mut self.pick, &pick_brush) {
                    pick.fill(peniko::Fill::NonZero, transform, pb, None, &path);
                }
                i += 6;
            } else {
                // 3. Single-triangle fallback.
                let tri_pts = [
                    mesh.vertices[indices[i] as usize],
                    mesh.vertices[indices[i + 1] as usize],
                    mesh.vertices[indices[i + 2] as usize],
                ];
                let tri_colors = [
                    mesh.colors[indices[i] as usize],
                    mesh.colors[indices[i + 1] as usize],
                    mesh.colors[indices[i + 2] as usize],
                ];
                let tri_path = triangle_path(&tri_pts);
                let brush = triangle_gradient_brush(&tri_pts, &tri_colors);
                self.inner
                    .fill(peniko::Fill::NonZero, transform, &brush, None, &tri_path);
                if let (Some(pick), Some(pb)) = (&mut self.pick, &pick_brush) {
                    pick.fill(peniko::Fill::NonZero, transform, pb, None, &tri_path);
                }
                i += 3;
            }
        }
    }

    fn push_layer(&mut self, blend: BlendMode, alpha: f32, transform: Affine, clip: &Path) {
        self.inner.push_layer(
            peniko::Fill::NonZero,
            convert::blend_mode(blend),
            alpha,
            transform,
            clip,
        );
        if let Some(pick) = &mut self.pick {
            // Mirror the layer's clip/transform so subsequent draws are clipped
            // identically in the pick buffer, but normalize the blend so it
            // doesn't distort id colors. Alpha = 1 prevents translucent layers
            // from fading ids into the no-hit sentinel.
            pick.push_layer(
                peniko::Fill::NonZero,
                convert::blend_mode(BlendMode::NORMAL),
                1.0,
                transform,
                clip,
            );
        }
    }

    fn pop_layer(&mut self) {
        self.inner.pop_layer();
        if let Some(pick) = &mut self.pick {
            pick.pop_layer();
        }
    }
}

// ---------- Renderer ----------

/// Headless target: storage texture + readback buffer, both sized for the
/// current frame. Recreated on size change.
struct HeadlessTarget {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    readback: wgpu::Buffer,
    width: u32,
    height: u32,
    /// Bytes per row in the readback buffer (padded to wgpu's alignment).
    padded_bytes_per_row: u32,
}

impl HeadlessTarget {
    /// Allocate a storage texture and a `width`-row-padded readback
    /// buffer at the given dimensions.
    fn new(device: &wgpu::Device, width: u32, height: u32) -> Self {
        let bytes_per_row = width * 4;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded_bytes_per_row = bytes_per_row.div_ceil(align) * align;
        let buffer_size = (padded_bytes_per_row as u64) * (height as u64);

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("hephaestus.vello.target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("hephaestus.vello.readback"),
            size: buffer_size,
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

/// Hephaestus Vello renderer: owns wgpu device/queue, the vello::Renderer, the
/// scene being built, and per-size headless targets.
///
/// When constructed via [`Self::with_picking`], the renderer also rasterises a
/// parallel "pick" scene to a second target, reads it back after each render,
/// and caches the result in a CPU-side hitmap that powers [`Self::pick_at`].
pub struct VelloRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: VRenderer,
    scene: VelloScene,
    target: Option<HeadlessTarget>,
    pick_target: Option<HeadlessTarget>,
    /// Tightly-packed RGBA8 bytes of the most-recent pick render, viewable as
    /// `&[u32]` via bytemuck. `None` until the first picking-enabled render.
    hitmap: Option<Vec<u32>>,
    hitmap_dims: Option<(u32, u32)>,
}

impl VelloRenderer {
    /// Build a renderer with no picking machinery. File-export workloads
    /// should use this form; nothing in the pick path is allocated.
    pub fn new() -> Result<Self, BackendError> {
        pollster::block_on(Self::new_async(false))
    }

    /// Build a renderer with picking enabled. Each call to
    /// [`Self::render_to_buffer`] additionally rasterises the pick scene and
    /// reads it back into an internal hitmap.
    pub fn with_picking() -> Result<Self, BackendError> {
        pollster::block_on(Self::new_async(true))
    }

    /// Build a renderer that shares an existing wgpu device and queue —
    /// e.g. the device backing a window's swap chain. Use this together
    /// with [`crate::backend::WgpuRenderer::render_to_texture`]
    /// to display the scene without a CPU readback round-trip.
    ///
    /// `device` and `queue` are handles (Arc-backed in wgpu); the host
    /// keeps its own and the renderer holds clones.
    pub fn with_device(device: &wgpu::Device, queue: &wgpu::Queue) -> Result<Self, BackendError> {
        Self::build(device.clone(), queue.clone(), false)
    }

    /// Like [`Self::with_device`] but enables picking. The pick scene is
    /// rasterised into a backend-owned headless target and read back to
    /// CPU on every render, regardless of whether the display render goes
    /// to a buffer or directly to a texture.
    pub fn with_device_and_picking(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Result<Self, BackendError> {
        Self::build(device.clone(), queue.clone(), true)
    }

    async fn new_async(picking: bool) -> Result<Self, BackendError> {
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
        // GL is included alongside PRIMARY so the GLES / WebGL backends
        // compiled in for unix and wasm are actually reachable: a browser
        // without WebGPU, or a Linux host without Vulkan, falls back to
        // GL rather than finding no adapter at all. `WGPU_BACKENDS`
        // overrides the choice when a host needs to pin one.
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

        let limits = wgpu::Limits::default();
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("hephaestus.vello.device"),
                required_features: wgpu::Features::empty(),
                required_limits: limits,
                memory_hints: wgpu::MemoryHints::default(),
                trace: wgpu::Trace::Off,
                experimental_features: wgpu::ExperimentalFeatures::default(),
            })
            .await
            .map_err(|e| BackendError::DeviceRequest(e.to_string()))?;

        Self::build(device, queue, picking)
    }

    /// Shared post-device construction: build the vello renderer and the
    /// (optionally picking) scene against an already-owned device/queue.
    fn build(
        device: wgpu::Device,
        queue: wgpu::Queue,
        picking: bool,
    ) -> Result<Self, BackendError> {
        let renderer = VRenderer::new(
            &device,
            RendererOptions {
                use_cpu: false,
                antialiasing_support: AaSupport::area_only(),
                num_init_threads: NonZeroUsize::new(1),
                pipeline_cache: None,
            },
        )
        .map_err(|e| BackendError::Other(format!("vello renderer init: {e}")))?;

        let scene = if picking {
            VelloScene::with_picking()
        } else {
            VelloScene::new()
        };

        Ok(Self {
            device,
            queue,
            renderer,
            scene,
            target: None,
            pick_target: None,
            hitmap: None,
            hitmap_dims: None,
        })
    }

    /// Re-allocate the display headless target when the requested
    /// dimensions don't match the cached ones. Only used by the
    /// [`Renderer::render_to_buffer`] path — the texture-target path
    /// writes directly into the host's view and skips this entirely.
    fn ensure_display_target(&mut self, width: u32, height: u32) {
        let need_new = match &self.target {
            None => true,
            Some(t) => t.width != width || t.height != height,
        };
        if need_new {
            self.target = Some(HeadlessTarget::new(&self.device, width, height));
        }
    }

    /// Re-allocate the pick headless target when picking is enabled and
    /// the dimensions don't match the cached ones. No-op when picking is
    /// disabled.
    fn ensure_pick_target(&mut self, width: u32, height: u32) {
        if self.scene.raw_pick().is_none() {
            return;
        }
        let need_new = match &self.pick_target {
            None => true,
            Some(t) => t.width != width || t.height != height,
        };
        if need_new {
            self.pick_target = Some(HeadlessTarget::new(&self.device, width, height));
        }
    }

    /// Reject a scene vello cannot configure, before any GPU work is queued.
    fn check_scene_budget(&self) -> Result<(), BackendError> {
        check_draw_budget(self.scene.raw())?;
        if let Some(pick) = self.scene.raw_pick() {
            check_draw_budget(pick)?;
        }
        Ok(())
    }

    /// Rasterise the pick scene into the cached pick target, copy it back
    /// to CPU, and refresh the hitmap. Assumes [`Self::ensure_pick_target`]
    /// has already been called and picking is enabled.
    fn render_pick_and_readback(&mut self, width: u32, height: u32) -> Result<(), BackendError> {
        let pick_scene = self.scene.raw_pick().expect("pick scene present");
        let pick_target = self.pick_target.as_ref().expect("pick target ensured");

        // AaConfig::Area is the only mode vello offers that our AaSupport
        // opted into, and vello has no way to turn antialiasing off — so the
        // pick scene is antialiased whether or not that suits it, and edge
        // pixels blend.
        //
        // The transparent base is what makes that survivable. Vello
        // unpremultiplies on output, so a mark's fringe over *nothing*
        // divides back out to its exact id with coverage left in alpha. An
        // opaque base would instead blend every fringe toward black and hand
        // back a plausible but wrong id at full alpha. Measured on one mark
        // tagged 200: transparent base leaves 140 stray pixels, all at alpha
        // 0 and rejected by `pick::decode`; an opaque base leaves 228, all at
        // alpha 255 and undetectable.
        //
        // What neither base fixes: a fringe over *other picked content*
        // blends two real ids and lands at full alpha. See the conflation
        // note on `crate::pick`.
        self.renderer
            .render_to_texture(
                &self.device,
                &self.queue,
                pick_scene,
                &pick_target.view,
                &RenderParams {
                    base_color: Color::new([0.0, 0.0, 0.0, 0.0]),
                    width,
                    height,
                    antialiasing_method: AaConfig::Area,
                },
            )
            .map_err(|e| BackendError::Other(format!("vello pick render: {e}")))?;

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("hephaestus.vello.pick_readback"),
            });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &pick_target.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &pick_target.readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(pick_target.padded_bytes_per_row),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit(std::iter::once(encoder.finish()));

        let pick_slice = pick_target.readback.slice(..);
        let (pick_tx, pick_rx) = futures_intrusive::channel::shared::oneshot_channel();
        pick_slice.map_async(wgpu::MapMode::Read, move |res| {
            let _ = pick_tx.send(res);
        });
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        match pollster::block_on(pick_rx.receive()) {
            Some(Ok(())) => {}
            Some(Err(e)) => return Err(BackendError::Readback(e.to_string())),
            None => {
                return Err(BackendError::Readback(
                    "map_async pick sender dropped".into(),
                ))
            }
        }

        let row_bytes = (width as usize) * 4;
        let row_px = width as usize;
        let total_px = (width as usize) * (height as usize);
        let hitmap = self.hitmap.get_or_insert_with(Vec::new);
        if hitmap.len() != total_px {
            hitmap.resize(total_px, 0);
        }
        {
            let data = pick_slice.get_mapped_range();
            let padded = pick_target.padded_bytes_per_row as usize;
            for y in 0..height as usize {
                let src = &data[y * padded..y * padded + row_bytes];
                let dst: &mut [u8] =
                    bytemuck::cast_slice_mut(&mut hitmap[y * row_px..y * row_px + row_px]);
                dst.copy_from_slice(src);
            }
        }
        pick_target.readback.unmap();
        self.hitmap_dims = Some((width, height));
        Ok(())
    }

    /// Look up the id at pixel `(x, y)` in the most-recent pick render.
    /// Returns `None` if picking is disabled, no render has been performed
    /// yet, the coordinates are out of range, or the pixel is the "no hit"
    /// sentinel (uncovered or [`PickId::Block`]).
    ///
    /// Note: picking does not respect display alpha; see the [`crate::pick`]
    /// module docs for the alpha-insensitive picking limitation.
    pub fn pick_at(&self, x: u32, y: u32) -> Option<u32> {
        let (w, h) = self.hitmap_dims?;
        if x >= w || y >= h {
            return None;
        }
        let map = self.hitmap.as_deref()?;
        pick::decode(map[(y * w + x) as usize])
    }

    /// Borrow the full hitmap as a flat `&[u32]` of `width * height` pixels
    /// laid out row-major. Useful for bulk queries (marquee selection etc.).
    /// Returns `None` if picking is disabled or no render has been performed.
    pub fn hitmap(&self) -> Option<&[u32]> {
        self.hitmap.as_deref()
    }
}

impl Renderer for VelloRenderer {
    type Scene = VelloScene;

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
        self.check_scene_budget()?;

        self.ensure_display_target(width, height);
        self.ensure_pick_target(width, height);
        let target = self.target.as_ref().unwrap();

        self.renderer
            .render_to_texture(
                &self.device,
                &self.queue,
                self.scene.raw(),
                &target.view,
                &RenderParams {
                    base_color: background,
                    width,
                    height,
                    antialiasing_method: AaConfig::Area,
                },
            )
            .map_err(|e| BackendError::Other(format!("vello render: {e}")))?;

        // If picking is enabled, render the parallel pick scene over a
        // transparent base. See `render_pick_and_readback` for why the base
        // must stay transparent.
        let picking = self.scene.raw_pick().is_some();
        if picking {
            let pick_scene = self.scene.raw_pick().unwrap();
            let pick_target = self.pick_target.as_ref().expect("pick target ensured");
            // Same AA and base-colour contract as `render_pick_and_readback`.
            self.renderer
                .render_to_texture(
                    &self.device,
                    &self.queue,
                    pick_scene,
                    &pick_target.view,
                    &RenderParams {
                        base_color: Color::new([0.0, 0.0, 0.0, 0.0]),
                        width,
                        height,
                        antialiasing_method: AaConfig::Area,
                    },
                )
                .map_err(|e| BackendError::Other(format!("vello pick render: {e}")))?;
        }

        // Encode both texture→buffer copies into one command buffer so they
        // share a single submit + map round-trip.
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("hephaestus.vello.readback"),
            });
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
        if picking {
            let pick_target = self.pick_target.as_ref().unwrap();
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture: &pick_target.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: &pick_target.readback,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(pick_target.padded_bytes_per_row),
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
        self.queue.submit(std::iter::once(encoder.finish()));

        let display_slice = target.readback.slice(..);
        let (display_tx, display_rx) = futures_intrusive::channel::shared::oneshot_channel();
        display_slice.map_async(wgpu::MapMode::Read, move |res| {
            let _ = display_tx.send(res);
        });

        let pick_rx = if picking {
            let pick_target = self.pick_target.as_ref().unwrap();
            let pick_slice = pick_target.readback.slice(..);
            let (pick_tx, pick_rx) = futures_intrusive::channel::shared::oneshot_channel();
            pick_slice.map_async(wgpu::MapMode::Read, move |res| {
                let _ = pick_tx.send(res);
            });
            Some(pick_rx)
        } else {
            None
        };

        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());

        match pollster::block_on(display_rx.receive()) {
            Some(Ok(())) => {}
            Some(Err(e)) => return Err(BackendError::Readback(e.to_string())),
            None => return Err(BackendError::Readback("map_async sender dropped".into())),
        }
        if let Some(rx) = pick_rx.as_ref() {
            match pollster::block_on(rx.receive()) {
                Some(Ok(())) => {}
                Some(Err(e)) => return Err(BackendError::Readback(e.to_string())),
                None => {
                    return Err(BackendError::Readback(
                        "map_async pick sender dropped".into(),
                    ))
                }
            }
        }

        let row_bytes = (width as usize) * 4;
        {
            let data = display_slice.get_mapped_range();
            let padded = target.padded_bytes_per_row as usize;
            for y in 0..height as usize {
                let src = &data[y * padded..y * padded + row_bytes];
                let dst = &mut out[y * row_bytes..y * row_bytes + row_bytes];
                dst.copy_from_slice(src);
            }
        }
        target.readback.unmap();

        if picking {
            let pick_target = self.pick_target.as_ref().unwrap();
            let row_px = width as usize;
            let total_px = (width as usize) * (height as usize);
            let hitmap = self.hitmap.get_or_insert_with(Vec::new);
            if hitmap.len() != total_px {
                hitmap.resize(total_px, 0);
            }
            let pick_slice = pick_target.readback.slice(..);
            {
                let data = pick_slice.get_mapped_range();
                let padded = pick_target.padded_bytes_per_row as usize;
                for y in 0..height as usize {
                    let src = &data[y * padded..y * padded + row_bytes];
                    let dst: &mut [u8] =
                        bytemuck::cast_slice_mut(&mut hitmap[y * row_px..y * row_px + row_px]);
                    dst.copy_from_slice(src);
                }
            }
            pick_target.readback.unmap();
            self.hitmap_dims = Some((width, height));
        }

        Ok(())
    }
}

impl WgpuRenderer for VelloRenderer {
    fn render_to_texture(
        &mut self,
        view: &wgpu::TextureView,
        width: u32,
        height: u32,
        background: Color,
    ) -> Result<(), BackendError> {
        self.check_scene_budget()?;
        self.renderer
            .render_to_texture(
                &self.device,
                &self.queue,
                self.scene.raw(),
                view,
                &RenderParams {
                    base_color: background,
                    width,
                    height,
                    antialiasing_method: AaConfig::Area,
                },
            )
            .map_err(|e| BackendError::Other(format!("vello render: {e}")))?;

        // Picking still goes through the backend-owned pick target +
        // CPU readback. Display has no readback to wait on, so the pick
        // submit / poll happens after the display submit returns.
        if self.scene.raw_pick().is_some() {
            self.ensure_pick_target(width, height);
            self.render_pick_and_readback(width, height)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::rgb8;

    /// Add `n` flat-coloured triangles, each its own draw object.
    fn solid_fills(scene: &mut VelloScene, n: usize) {
        let brush = Brush::Solid(rgb8(10, 20, 30));
        for i in 0..n {
            let x = (i % 256) as f64;
            let mut path = Path::new();
            path.move_to(Point::new(x, 0.0));
            path.line_to(Point::new(x + 1.0, 0.0));
            path.line_to(Point::new(x + 1.0, 1.0));
            path.close_path();
            scene.fill(
                FillRule::NonZero,
                Affine::IDENTITY,
                &brush,
                None,
                &path,
                PickId::Skip,
            );
        }
    }

    #[test]
    fn an_empty_scene_spends_no_draw_budget() {
        let scene = VelloScene::new();
        assert_eq!(scene.draw_info_words(), 0);
        assert!(scene.fits_draw_budget());
    }

    #[test]
    fn each_solid_fill_costs_one_draw_info_word() {
        let mut scene = VelloScene::new();
        solid_fills(&mut scene, 3);
        assert_eq!(scene.draw_info_words(), 3);
    }

    #[test]
    fn clearing_a_scene_returns_its_draw_budget() {
        let mut scene = VelloScene::new();
        solid_fills(&mut scene, 5);
        scene.clear();
        assert_eq!(scene.draw_info_words(), 0);
    }

    #[test]
    fn a_scene_at_the_cap_fits_but_one_draw_more_does_not() {
        let mut scene = VelloScene::new();
        solid_fills(&mut scene, MAX_DRAW_INFO_WORDS as usize);
        assert!(scene.fits_draw_budget());

        solid_fills(&mut scene, 1);
        assert!(!scene.fits_draw_budget());
        assert!(matches!(
            check_draw_budget(scene.raw()),
            Err(BackendError::SceneTooLarge { used, max })
                if used == MAX_DRAW_INFO_WORDS + 1 && max == MAX_DRAW_INFO_WORDS
        ));
    }
}
