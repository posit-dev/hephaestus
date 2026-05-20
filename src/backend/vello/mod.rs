//! Vello backend. Wraps `vello::Scene` to implement [`SceneBuilder`] and owns
//! the wgpu device/queue/renderer needed for headless rasterization.

mod convert;

use std::num::NonZeroUsize;

use vello::{AaConfig, AaSupport, RenderParams, Renderer as VRenderer, RendererOptions, Scene};

use crate::backend::{BackendError, Renderer};
use crate::blend::BlendMode;
use crate::brush::{Brush, Image, Sampling};
use crate::color::Color;
use crate::geometry::Affine;
use crate::path::{FillRule, Path};
use crate::scene::{GlyphRun, SceneBuilder};
use crate::stroke::Stroke;

/// A `SceneBuilder` that writes into a `vello::Scene`.
pub struct VelloScene {
    inner: Scene,
}

impl VelloScene {
    pub fn new() -> Self {
        Self {
            inner: Scene::new(),
        }
    }

    /// Borrow the underlying `vello::Scene` (e.g. to render it).
    pub fn raw(&self) -> &Scene {
        &self.inner
    }

    pub fn clear(&mut self) {
        self.inner.reset();
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
    ) {
        self.inner.fill(
            convert::fill_rule(rule),
            transform,
            brush,
            brush_transform,
            path,
        );
    }

    fn stroke(
        &mut self,
        stroke: &Stroke,
        transform: Affine,
        brush: &Brush,
        brush_transform: Option<Affine>,
        path: &Path,
    ) {
        self.inner
            .stroke(stroke, transform, brush, brush_transform, path);
    }

    fn draw_image(&mut self, image: &Image, transform: Affine, sampling: Sampling, alpha: f32) {
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
    }

    fn draw_glyphs(&mut self, run: &GlyphRun<'_>) {
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
    }

    fn push_layer(&mut self, blend: BlendMode, alpha: f32, transform: Affine, clip: &Path) {
        self.inner.push_layer(
            peniko::Fill::NonZero,
            convert::blend_mode(blend),
            alpha,
            transform,
            clip,
        );
    }

    fn pop_layer(&mut self) {
        self.inner.pop_layer();
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
/// scene being built, and a per-size headless target.
pub struct VelloRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: VRenderer,
    scene: VelloScene,
    target: Option<HeadlessTarget>,
}

impl VelloRenderer {
    /// Build a new renderer using a fresh high-performance wgpu adapter.
    /// Blocks until the adapter and device are ready (no async in v1).
    pub fn new() -> Result<Self, BackendError> {
        pollster::block_on(Self::new_async())
    }

    async fn new_async() -> Result<Self, BackendError> {
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

        Ok(Self {
            device,
            queue,
            renderer,
            scene: VelloScene::new(),
            target: None,
        })
    }

    fn ensure_target(&mut self, width: u32, height: u32) {
        let need_new = match &self.target {
            None => true,
            Some(t) => t.width != width || t.height != height,
        };
        if need_new {
            self.target = Some(HeadlessTarget::new(&self.device, width, height));
        }
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

        self.ensure_target(width, height);
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

        // Copy texture → readback buffer (padded rows), submit, map, copy out.
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
        self.queue.submit(std::iter::once(encoder.finish()));

        let slice = target.readback.slice(..);
        let (sender, receiver) = futures_intrusive::channel::shared::oneshot_channel();
        slice.map_async(wgpu::MapMode::Read, move |res| {
            let _ = sender.send(res);
        });
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        let map_result = pollster::block_on(receiver.receive());
        match map_result {
            Some(Ok(())) => {}
            Some(Err(e)) => return Err(BackendError::Readback(e.to_string())),
            None => return Err(BackendError::Readback("map_async sender dropped".into())),
        }

        {
            let data = slice.get_mapped_range();
            let row_bytes = (width as usize) * 4;
            let padded = target.padded_bytes_per_row as usize;
            for y in 0..height as usize {
                let src = &data[y * padded..y * padded + row_bytes];
                let dst = &mut out[y * row_bytes..y * row_bytes + row_bytes];
                dst.copy_from_slice(src);
            }
        }
        target.readback.unmap();

        Ok(())
    }

    fn reset(&mut self) {
        self.scene.clear();
    }
}
