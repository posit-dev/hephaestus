//! Vello backend. Wraps `vello::Scene` to implement [`SceneBuilder`] and owns
//! the wgpu device/queue/renderer needed for headless rasterization.

use crate::backend::convert;
use crate::backend::mesh;

use std::num::NonZeroUsize;

use vello::{AaConfig, AaSupport, RenderParams, Renderer as VRenderer, RendererOptions, Scene};

use crate::backend::{BackendError, Renderer, WgpuRenderer};
use crate::blend::BlendMode;
use crate::brush::{Brush, Image, Sampling};
use crate::color::Color;
use crate::geometry::Affine;
use crate::mesh::Mesh;

use crate::path::{FillRule, Path};
use crate::pick::{PickId, PickIndexScene};
use crate::scene::{GlyphRun, SceneBuilder};
use crate::stroke::Stroke;

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
/// Ignores `pick_id`: hit testing is a CPU-side index built by
/// [`PickIndexScene`], which wraps this, so a rasteriser has nothing to do
/// with it. The parameter stays on the trait because the vector backends do
/// surface it — SVG emits `data-pick-id`.
pub struct VelloScene {
    inner: Scene,
}

impl VelloScene {
    /// Build an empty scene.
    pub fn new() -> Self {
        Self {
            inner: Scene::new(),
        }
    }

    /// Borrow the underlying `vello::Scene` (e.g. to render it).
    pub fn raw(&self) -> &Scene {
        &self.inner
    }

    /// Draw-info words the encoded scene occupies, to be compared against
    /// [`MAX_DRAW_INFO_WORDS`].
    pub fn draw_info_words(&self) -> u32 {
        draw_info_words(&self.inner)
    }

    /// True when the scene fits the backend's draw budget, so a render will
    /// not be rejected.
    pub fn fits_draw_budget(&self) -> bool {
        check_draw_budget(&self.inner).is_ok()
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
    fn clear(&mut self) {
        self.inner.reset();
    }

    fn fill(
        &mut self,
        rule: FillRule,
        transform: Affine,
        brush: &Brush,
        brush_transform: Option<Affine>,
        path: &Path,
        _pick_id: PickId,
    ) {
        let fill_rule = convert::fill_rule(rule);
        self.inner
            .fill(fill_rule, transform, brush, brush_transform, path);
    }

    fn stroke(
        &mut self,
        stroke: &Stroke,
        transform: Affine,
        brush: &Brush,
        brush_transform: Option<Affine>,
        path: &Path,
        _pick_id: PickId,
    ) {
        self.inner
            .stroke(stroke, transform, brush, brush_transform, path);
    }

    fn draw_image(
        &mut self,
        image: &Image,
        transform: Affine,
        sampling: Sampling,
        alpha: f32,
        _pick_id: PickId,
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
    }

    fn draw_glyphs(&mut self, run: &GlyphRun<'_>, _pick_id: PickId) {
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
    }

    fn draw_mesh(&mut self, mesh: &Mesh, transform: Affine, pick_id: PickId) {
        // Neither vello nor peniko has an indexed-mesh primitive, so the mesh
        // becomes fills.
        mesh::decompose(mesh, transform, pick_id, self);
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
/// Hit testing is a property of the scene, not of this renderer: the scene is
/// a [`crate::pick::PickIndexScene`], and
/// [`Self::with_picking`] is what turns its indexing on.
pub struct VelloRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: VRenderer,
    scene: PickIndexScene<VelloScene>,
    target: Option<HeadlessTarget>,
}

impl VelloRenderer {
    /// Build a renderer that does not hit-test. File-export workloads
    /// should use this form; the scene indexes nothing.
    pub fn new() -> Result<Self, BackendError> {
        pollster::block_on(Self::new_async(false))
    }

    /// Build a renderer whose scene records a hit index as it is drawn,
    /// making [`Self::pick_at`] and the other queries answerable.
    ///
    /// Indexing costs CPU per draw call, so it is off by default.
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

    /// Like [`Self::with_device`] but with hit indexing enabled — see
    /// [`Self::with_picking`].
    pub fn with_device_and_picking(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Result<Self, BackendError> {
        Self::build(device.clone(), queue.clone(), true)
    }

    async fn new_async(picking: bool) -> Result<Self, BackendError> {
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
        // GL is included alongside PRIMARY so the GLES backend compiled in
        // for unix is actually reachable: a Linux host without Vulkan falls
        // back to it rather than finding no adapter at all. `WGPU_BACKENDS`
        // overrides the choice when a host needs to pin one. On wasm the
        // flag finds nothing — vello rasterises through compute pipelines,
        // which WebGL2 has no stage for, so only WebGPU is compiled in.
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
    /// scene against an already-owned device/queue.
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

        Ok(Self {
            device,
            queue,
            renderer,
            scene: PickIndexScene::new(VelloScene::new(), picking),
            target: None,
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

    /// Reject a scene vello cannot configure, before any GPU work is queued.
    fn check_scene_budget(&self) -> Result<(), BackendError> {
        check_draw_budget(self.scene.inner().raw())
    }

    /// The hit index the scene built while it was drawn, or `None` when this
    /// renderer was not built with picking.
    ///
    /// The only pick method here, deliberately: a renderer's part in hit
    /// testing is owning the scene that recorded the index, so every query
    /// lives on [`PickIndex`](crate::pick::PickIndex) rather than being
    /// forwarded through two more layers of the same names.
    pub fn pick_index(&self) -> Option<&crate::pick::PickIndex> {
        self.scene.indexes().then(|| self.scene.index())
    }
}

impl Renderer for VelloRenderer {
    type Scene = PickIndexScene<VelloScene>;

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
        let target = self.target.as_ref().unwrap();

        self.renderer
            .render_to_texture(
                &self.device,
                &self.queue,
                self.scene.inner().raw(),
                &target.view,
                &RenderParams {
                    base_color: background,
                    width,
                    height,
                    antialiasing_method: AaConfig::Area,
                },
            )
            .map_err(|e| BackendError::Other(format!("vello render: {e}")))?;

        // Copy the rendered texture back to CPU.
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

        let display_slice = target.readback.slice(..);
        let (display_tx, display_rx) = futures_intrusive::channel::shared::oneshot_channel();
        display_slice.map_async(wgpu::MapMode::Read, move |res| {
            let _ = display_tx.send(res);
        });

        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());

        match pollster::block_on(display_rx.receive()) {
            Some(Ok(())) => {}
            Some(Err(e)) => return Err(BackendError::Readback(e.to_string())),
            None => return Err(BackendError::Readback("map_async sender dropped".into())),
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

        Ok(())
    }
}

impl WgpuRenderer for VelloRenderer {
    const REQUIRED_TARGET_USAGE: wgpu::TextureUsages = wgpu::TextureUsages::STORAGE_BINDING;

    const TARGET_IS_PREMULTIPLIED: bool = false;

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
                self.scene.inner().raw(),
                view,
                &RenderParams {
                    base_color: background,
                    width,
                    height,
                    antialiasing_method: AaConfig::Area,
                },
            )
            .map_err(|e| BackendError::Other(format!("vello render: {e}")))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::rgb8;
    use crate::geometry::Point;

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
