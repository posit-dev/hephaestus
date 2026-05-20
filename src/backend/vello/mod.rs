//! Vello backend. Wraps `vello::Scene` to implement [`SceneBuilder`] and owns
//! the wgpu device/queue/renderer needed for headless rasterization.

mod convert;

use std::num::NonZeroUsize;

use kurbo::Shape;
use vello::{AaConfig, AaSupport, RenderParams, Renderer as VRenderer, RendererOptions, Scene};

use crate::backend::{BackendError, Renderer};
use crate::blend::BlendMode;
use crate::brush::{Brush, Image, Sampling};
use crate::color::Color;
use crate::geometry::Affine;
use crate::path::{FillRule, Path};
use crate::pick::{self, PickId};
use crate::scene::{GlyphRun, SceneBuilder};
use crate::stroke::Stroke;

/// Minimum stroke width (in pixels) the pick pass uses, so hairline strokes
/// remain hittable even when the visual stroke is sub-pixel.
const MIN_PICK_STROKE_WIDTH: f64 = 2.0;

/// A `SceneBuilder` that writes into a `vello::Scene`.
///
/// When picking is enabled (constructed via [`Self::with_picking`]), every
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

    /// Clear both the display scene and (if present) the pick scene.
    pub fn clear(&mut self) {
        self.inner.reset();
        if let Some(p) = &mut self.pick {
            p.reset();
        }
    }
}

impl Default for VelloScene {
    fn default() -> Self {
        Self::new()
    }
}

impl SceneBuilder for VelloScene {
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
                let bounds = kurbo::Rect::new(0.0, 0.0, image.width as f64, image.height as f64)
                    .to_path(0.1);
                pick.fill(peniko::Fill::NonZero, transform, &pick_brush, None, &bounds);
            }
        }
    }

    fn draw_glyphs(&mut self, run: &GlyphRun<'_>, pick_id: PickId) {
        let mut builder = self
            .inner
            .draw_glyphs(&run.font.0)
            .font_size(run.font_size)
            .transform(run.transform)
            .glyph_transform(run.glyph_transform)
            .brush(run.brush)
            .brush_alpha(run.brush_alpha)
            .hint(run.hint);
        let _ = &mut builder; // silence unused if hint() ever returns ()
        builder.draw(
            peniko::Fill::NonZero,
            run.glyphs.iter().map(|g| vello::Glyph {
                id: g.id,
                x: g.x,
                y: g.y,
            }),
        );

        if let Some(pick) = &mut self.pick {
            if let Some(id) = pick::raw_id(pick_id) {
                let pick_brush = Brush::Solid(pick::id_to_color(id));
                let mut pick_builder = pick
                    .draw_glyphs(&run.font.0)
                    .font_size(run.font_size)
                    .transform(run.transform)
                    .glyph_transform(run.glyph_transform)
                    .brush(&pick_brush)
                    .brush_alpha(1.0)
                    .hint(run.hint);
                let _ = &mut pick_builder;
                pick_builder.draw(
                    peniko::Fill::NonZero,
                    run.glyphs.iter().map(|g| vello::Glyph {
                        id: g.id,
                        x: g.x,
                        y: g.y,
                    }),
                );
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
    hitmap: Option<Vec<u8>>,
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

    async fn new_async(picking: bool) -> Result<Self, BackendError> {
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
        desc.backends = wgpu::Backends::PRIMARY;
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

    fn ensure_targets(&mut self, width: u32, height: u32) {
        let need_new = match &self.target {
            None => true,
            Some(t) => t.width != width || t.height != height,
        };
        if need_new {
            self.target = Some(HeadlessTarget::new(&self.device, width, height));
            if self.scene.raw_pick().is_some() {
                self.pick_target = Some(HeadlessTarget::new(&self.device, width, height));
            }
        }
    }

    /// Look up the id at pixel `(x, y)` in the most-recent pick render.
    /// Returns `None` if picking is disabled, no render has been performed
    /// yet, the coordinates are out of range, or the pixel is the "no hit"
    /// sentinel (uncovered or [`PickId::Block`]).
    pub fn pick_at(&self, x: u32, y: u32) -> Option<u32> {
        let (w, h) = self.hitmap_dims?;
        if x >= w || y >= h {
            return None;
        }
        let bytes = self.hitmap.as_deref()?;
        let map: &[u32] = bytemuck::cast_slice(bytes);
        pick::decode(map[(y * w + x) as usize])
    }

    /// Borrow the full hitmap as a flat `&[u32]` of `width * height` pixels
    /// laid out row-major. Useful for bulk queries (marquee selection etc.).
    /// Returns `None` if picking is disabled or no render has been performed.
    pub fn hitmap(&self) -> Option<&[u32]> {
        self.hitmap.as_deref().map(bytemuck::cast_slice)
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

        self.ensure_targets(width, height);
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

        // If picking is enabled, render the parallel pick scene with AA off
        // and a transparent base so uncovered pixels decode as id 0.
        let picking = self.scene.raw_pick().is_some();
        if picking {
            let pick_scene = self.scene.raw_pick().unwrap();
            let pick_target = self.pick_target.as_ref().expect("pick target ensured");
            // Use AaConfig::Area (the only mode our AaSupport opted into).
            // Edge pixels of solid fills will be partially blended toward the
            // background — fine for picking interior regions, with a small
            // ambiguity zone at borders that v1 does not try to eliminate.
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
            let total_bytes = (width as usize) * (height as usize) * 4;
            let hitmap = self.hitmap.get_or_insert_with(Vec::new);
            if hitmap.len() != total_bytes {
                hitmap.resize(total_bytes, 0);
            }
            let pick_slice = pick_target.readback.slice(..);
            {
                let data = pick_slice.get_mapped_range();
                let padded = pick_target.padded_bytes_per_row as usize;
                for y in 0..height as usize {
                    let src = &data[y * padded..y * padded + row_bytes];
                    let dst = &mut hitmap[y * row_bytes..y * row_bytes + row_bytes];
                    dst.copy_from_slice(src);
                }
            }
            pick_target.readback.unmap();
            self.hitmap_dims = Some((width, height));
        }

        Ok(())
    }

    fn reset(&mut self) {
        self.scene.clear();
    }
}
