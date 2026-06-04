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
use crate::geometry::{Affine, Point};
use crate::mesh::Mesh;
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
    ///
    /// Note: picking does not respect display alpha; see the [`crate::pick`]
    /// module docs for the v1 limitation.
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
}

// ---------- mesh helpers ----------

/// Build a closed triangle path from three vertices.
fn triangle_path(pts: &[Point; 3]) -> Path {
    let mut p = Path::new();
    p.move_to(pts[0]);
    p.line_to(pts[1]);
    p.line_to(pts[2]);
    p.close_path();
    p
}

/// Pick a brush for a triangle's three vertex colours.
///
/// - All three equal → solid brush.
/// - Exactly two equal (the ribbon-triangle case: two shoulders sharing
///   a colour + one tip with a different colour) → linear gradient
///   running from the midpoint of the matching pair to the unique tip
///   vertex, with stops `[shared, tip]`. This places both equal-colour
///   vertices at gradient fraction 0 (because they project equidistant
///   from the axis's start) and the tip at fraction 1 — so adjacent
///   ribbon segments meet seamlessly.
/// - Three distinct colours (general mesh) → linear gradient between
///   the max-colour-distance pair. The third vertex gets an
///   interpolated colour at its perpendicular-projection position,
///   which produces a small visible discontinuity along the edge
///   between the picked pair and the third vertex — a documented v1
///   limitation.
fn triangle_gradient_brush(pts: &[Point; 3], colors: &[Color; 3]) -> Brush {
    let eq01 = colors_eq(&colors[0], &colors[1]);
    let eq12 = colors_eq(&colors[1], &colors[2]);
    let eq20 = colors_eq(&colors[2], &colors[0]);
    if eq01 && eq12 {
        return Brush::Solid(colors[0]);
    }
    // Identify the "tip" vertex when exactly two colours match. For
    // `eq01 && !eq12 && !eq20` the matching pair is (0, 1) and the tip
    // is index 2; similar for the other two cases.
    let tip_idx = if eq01 {
        Some(2)
    } else if eq12 {
        Some(0)
    } else if eq20 {
        Some(1)
    } else {
        None
    };

    if let Some(t) = tip_idx {
        let (a, b) = match t {
            0 => (1, 2),
            1 => (2, 0),
            _ => (0, 1),
        };
        // The gradient axis runs perpendicular to the back-edge AB,
        // through the back-edge midpoint, to the foot of the
        // perpendicular dropped from the tip onto that axis. This
        // places A and B at gradient fraction 0 (pure shared colour)
        // and the tip at fraction 1 — Gouraud-exact across the
        // triangle, with no projection error along the AB side.
        let start_x = 0.5 * (pts[a].x + pts[b].x);
        let start_y = 0.5 * (pts[a].y + pts[b].y);
        let abx = pts[b].x - pts[a].x;
        let aby = pts[b].y - pts[a].y;
        let perp_len = (abx * abx + aby * aby).sqrt();
        if perp_len < 1e-12 {
            // Degenerate back-edge: A and B coincide. Fall back to a
            // straight A → tip gradient.
            let gradient =
                peniko::Gradient::new_linear(pts[a], pts[t]).with_stops([colors[a], colors[t]]);
            return Brush::Gradient(gradient);
        }
        // Perpendicular to AB (90° CCW). Either sign is fine — we
        // resolve direction by signed projection of (tip - start)
        // onto it.
        let perp_x = -aby / perp_len;
        let perp_y = abx / perp_len;
        let dx = pts[t].x - start_x;
        let dy = pts[t].y - start_y;
        let d_signed = dx * perp_x + dy * perp_y;
        let end_x = start_x + perp_x * d_signed;
        let end_y = start_y + perp_y * d_signed;
        let gradient =
            peniko::Gradient::new_linear(Point::new(start_x, start_y), Point::new(end_x, end_y))
                .with_stops([colors[a], colors[t]]);
        return Brush::Gradient(gradient);
    }

    // Three distinct colours: fall back to the max-distance pair.
    let d01 = color_distance_sq(&colors[0], &colors[1]);
    let d12 = color_distance_sq(&colors[1], &colors[2]);
    let d20 = color_distance_sq(&colors[2], &colors[0]);
    let (a_idx, b_idx) = if d01 >= d12 && d01 >= d20 {
        (0, 1)
    } else if d12 >= d20 {
        (1, 2)
    } else {
        (2, 0)
    };
    let gradient = peniko::Gradient::new_linear(pts[a_idx], pts[b_idx])
        .with_stops([colors[a_idx], colors[b_idx]]);
    Brush::Gradient(gradient)
}

/// Detect a fan of ≥ 2 triangles all sharing the first vertex and a
/// uniform colour: pattern `[A, B₀, B₁], [A, B₁, B₂], [A, B₂, B₃], …`.
/// All referenced vertices must have the same colour. Returns the
/// polygon boundary `[A, B₀, B₁, …, Bₖ]` (in cyclic order so the
/// polygon closes via `Bₖ → A`) along with the number of mesh-index
/// entries consumed.
///
/// Used to collapse round-cap and round-join fans into a single
/// closed-polygon fill so the internal "wedge" seams between
/// adjacent fan triangles disappear.
fn detect_uniform_fan(
    indices: &[u32],
    start: usize,
    colors: &[Color],
) -> Option<(Vec<u32>, usize)> {
    if start + 6 > indices.len() {
        return None;
    }
    let t0 = &indices[start..start + 3];
    let t1 = &indices[start + 3..start + 6];
    let a = t0[0];
    if t1[0] != a || t1[1] != t0[2] {
        return None;
    }
    let target = colors[a as usize];
    if !colors_eq(&colors[t0[1] as usize], &target)
        || !colors_eq(&colors[t0[2] as usize], &target)
        || !colors_eq(&colors[t1[2] as usize], &target)
    {
        return None;
    }
    let mut boundary = vec![a, t0[1], t0[2], t1[2]];
    let mut consumed = 6;
    while start + consumed + 3 <= indices.len() {
        let tk = &indices[start + consumed..start + consumed + 3];
        if tk[0] == a && tk[1] == *boundary.last().unwrap() {
            if !colors_eq(&colors[tk[2] as usize], &target) {
                break;
            }
            boundary.push(tk[2]);
            consumed += 3;
        } else {
            break;
        }
    }
    Some((boundary, consumed))
}

/// Build a closed polygon path from N vertices in cyclic order.
fn polygon_path(pts: &[Point]) -> Path {
    let mut p = Path::new();
    if pts.is_empty() {
        return p;
    }
    p.move_to(pts[0]);
    for v in &pts[1..] {
        p.line_to(*v);
    }
    p.close_path();
    p
}

/// Detect adjacent triangle pairs that form a quad shaped
/// `[A, B, C, A, C, D]` — the canonical ribbon-strip emission. Returns
/// the quad indices `[A, B, C, D]` in CCW cyclic order when matched.
fn detect_quad_pair(six: &[u32]) -> Option<[u32; 4]> {
    debug_assert_eq!(six.len(), 6);
    let a = six[0];
    let b = six[1];
    let c = six[2];
    let d2 = six[3];
    let e2 = six[4];
    let f2 = six[5];
    // Canonical ribbon emission: triangle 1 = (a, b, c), triangle 2 =
    // (a, c, d). Check shared vertices are a and c in that order.
    if d2 == a && e2 == c {
        Some([a, b, c, f2])
    } else {
        None
    }
}

/// Build a closed quad path from four vertices, in cyclic order.
fn quad_path(pts: &[Point; 4]) -> Path {
    let mut p = Path::new();
    p.move_to(pts[0]);
    p.line_to(pts[1]);
    p.line_to(pts[2]);
    p.line_to(pts[3]);
    p.close_path();
    p
}

/// Pick a brush for a quad's four vertex colours. The expected ribbon
/// pattern is `(ci, ci, cj, cj)` where vertices 0-1 share the start
/// colour and 2-3 share the end colour — the gradient axis then runs
/// from `midpoint(p0, p1)` to `midpoint(p2, p3)` (the segment
/// centerline) with stops `[ci, cj]`. For uniform colours the brush
/// collapses to solid. For other colour patterns, fall back to
/// emitting per-triangle (re-decomposes via the caller's loop) — but
/// since the caller already chose to merge, we use a reasonable
/// default of max-distance pair across all four vertices.
fn quad_gradient_brush(pts: &[Point; 4], colors: &[Color; 4]) -> Brush {
    let all_same = colors_eq(&colors[0], &colors[1])
        && colors_eq(&colors[1], &colors[2])
        && colors_eq(&colors[2], &colors[3]);
    if all_same {
        return Brush::Solid(colors[0]);
    }
    // Ribbon-canonical: indices 0-1 share colour ci, indices 2-3
    // share colour cj. Gradient axis = midpoint(p0,p1) → midpoint(p2,p3).
    let pair01 = colors_eq(&colors[0], &colors[1]);
    let pair23 = colors_eq(&colors[2], &colors[3]);
    if pair01 && pair23 {
        let start = Point::new(0.5 * (pts[0].x + pts[1].x), 0.5 * (pts[0].y + pts[1].y));
        let end = Point::new(0.5 * (pts[2].x + pts[3].x), 0.5 * (pts[2].y + pts[3].y));
        let gradient = peniko::Gradient::new_linear(start, end).with_stops([colors[0], colors[2]]);
        return Brush::Gradient(gradient);
    }
    // Other pairings (12, 30): rotate the gradient axis accordingly.
    let pair12 = colors_eq(&colors[1], &colors[2]);
    let pair30 = colors_eq(&colors[3], &colors[0]);
    if pair12 && pair30 {
        let start = Point::new(0.5 * (pts[1].x + pts[2].x), 0.5 * (pts[1].y + pts[2].y));
        let end = Point::new(0.5 * (pts[3].x + pts[0].x), 0.5 * (pts[3].y + pts[0].y));
        let gradient = peniko::Gradient::new_linear(start, end).with_stops([colors[1], colors[3]]);
        return Brush::Gradient(gradient);
    }
    // Fallback: pick the max-distance pair across the four vertices.
    let mut best = (0usize, 1usize, 0.0f32);
    for i in 0..4 {
        for j in (i + 1)..4 {
            let d = color_distance_sq(&colors[i], &colors[j]);
            if d > best.2 {
                best = (i, j, d);
            }
        }
    }
    let gradient = peniko::Gradient::new_linear(pts[best.0], pts[best.1])
        .with_stops([colors[best.0], colors[best.1]]);
    Brush::Gradient(gradient)
}

fn colors_eq(a: &Color, b: &Color) -> bool {
    a.components == b.components
}

fn color_distance_sq(a: &Color, b: &Color) -> f32 {
    let [ar, ag, ab, _] = a.components;
    let [br, bg, bb, _] = b.components;
    let dr = ar - br;
    let dg = ag - bg;
    let db = ab - bb;
    dr * dr + dg * dg + db * db
}
