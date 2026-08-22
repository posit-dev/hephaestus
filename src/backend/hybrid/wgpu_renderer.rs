//! The wgpu-backed renderer: rasterises a replayed scene through
//! `vello_hybrid`'s render pipeline onto a wgpu texture.
//!
//! Split from the scene layer beside it because that layer needs no GPU
//! API at all, which is what lets a WebGL2 build leave wgpu out entirely.

use std::collections::HashMap;

use vello_common::paint::ImageSource;
use vello_hybrid::{
    RenderSize, RenderTargetConfig, Renderer as HRenderer, Resources, Scene, TextureBindings,
};

use super::{
    dimension, image_key, recorded_images, unpremultiply, HybridScene, Pass, Writer,
    PICK_ALIASING_THRESHOLD,
};
use crate::backend::{BackendError, Renderer, WgpuRenderer};
use crate::color::Color;
use crate::geometry::Affine;
use crate::pick;

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
    format: wgpu::TextureFormat,
}

impl Target {
    /// Allocate a render-attachment texture and a row-padded readback buffer.
    ///
    /// `RENDER_ATTACHMENT` rather than `STORAGE_BINDING`: this backend
    /// rasterises through a render pipeline, not a compute shader.
    fn new(device: &wgpu::Device, width: u32, height: u32, format: wgpu::TextureFormat) -> Self {
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
            format,
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
            format,
        }
    }
}

/// The half of the renderer bound to a frame size.
///
/// `RenderTargetConfig` fixes the dimensions a `vello_hybrid::Renderer` is
/// built for, and `Scene` its own, so a size change rebuilds both. The image
/// atlas lives here too and is therefore invalidated by a resize.
struct SizeBound {
    renderer: HRenderer,
    resources: Resources,
    display: Scene,
    pick: Option<Scene>,
    images: HashMap<u64, ImageSource>,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
}

/// A pick readback in flight: a slot the `map_async` callback fills, and the
/// dimensions it covers.
///
/// A slot rather than a future, so completion can be *checked* instead of
/// awaited. Awaiting would mean holding a borrow of the renderer across a
/// suspension point, which a browser host — where the only caller is a
/// callback that may re-enter — cannot do safely.
struct PendingPick {
    slot: std::sync::Arc<std::sync::Mutex<Option<Result<(), wgpu::BufferAsyncError>>>>,
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
    sized: Option<SizeBound>,
    target: Option<Target>,
    pick_target: Option<Target>,
    /// Decoded pick pixels of the most recent render, one `u32` per pixel.
    hitmap: Option<Vec<u32>>,
    hitmap_dims: Option<(u32, u32)>,
    pick_pending: Option<PendingPick>,
    /// Format `render_to_texture` writes. A host presenting straight into its
    /// swap chain sets this to the surface's format.
    target_format: wgpu::TextureFormat,
    /// Whether the coming render refreshes the hitmap. See
    /// [`HybridRenderer::set_refresh_pick`].
    refresh_pick: bool,
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
            pick_pending: None,
            target_format: wgpu::TextureFormat::Rgba8Unorm,
            refresh_pick: true,
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

    /// Control whether the coming render refreshes the hitmap.
    ///
    /// The pick pass costs about what the display pass does — it is a second
    /// strip generation over the same geometry, on the CPU — so a host that is
    /// resizing, animating, or otherwise redrawing faster than it queries can
    /// leave the hitmap alone and pay for it only when an answer is wanted.
    /// Measured at 100k marks: 88 ms a frame without it, 150 ms with.
    ///
    /// While it is off, [`Self::pick_at`] keeps answering from the last render
    /// that refreshed — so the ids stay readable but describe an older frame.
    /// Set it back to `true` (the default) and the next render brings the
    /// hitmap up to date.
    ///
    /// No effect when picking was not enabled at construction.
    pub fn set_refresh_pick(&mut self, refresh: bool) {
        self.refresh_pick = refresh;
    }

    /// Whether the coming render will refresh the hitmap.
    pub fn refreshes_pick(&self) -> bool {
        self.picking && self.refresh_pick
    }

    /// Set the texture format [`WgpuRenderer::render_to_texture`] writes.
    ///
    /// Defaults to `Rgba8Unorm`. A host that presents straight into its swap
    /// chain — which this backend can do, since it rasterises through a render
    /// pipeline — sets the surface's format here instead of blitting from an
    /// intermediate texture. Changing it rebuilds the size-bound state, so set
    /// it once rather than per frame.
    ///
    /// [`Renderer::render_to_buffer`] is unaffected: it owns its target and
    /// always uses `Rgba8Unorm`, since that is the byte order it hands out.
    pub fn set_target_format(&mut self, format: wgpu::TextureFormat) {
        self.target_format = format;
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
    fn ensure_sized(
        &mut self,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
    ) -> Result<(), BackendError> {
        if self
            .sized
            .as_ref()
            .is_some_and(|s| s.width == width && s.height == height && s.format == format)
        {
            return Ok(());
        }
        let (w16, h16) = (dimension(width)?, dimension(height)?);
        let (renderer, resources) = HRenderer::new(
            &self.device,
            &RenderTargetConfig {
                format,
                width,
                height,
            },
        );
        self.sized = Some(SizeBound {
            renderer,
            resources,
            display: Scene::new(w16, h16),
            pick: self.picking.then(|| Scene::new(w16, h16)),
            images: HashMap::new(),
            width,
            height,
            format,
        });
        Ok(())
    }
}

/// Narrow a pixel dimension to the `u16` the rasteriser sizes scenes in.
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
    fn replay(&mut self, background: Color, width: u32, height: u32, refresh_pick: bool) {
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

        if !refresh_pick {
            return;
        }
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

impl HybridRenderer {
    /// Allocate the pick target when the requested size differs from the
    /// cached one.
    fn ensure_pick_target(&mut self, width: u32, height: u32) {
        // The pick pass goes through the same renderer as the display, and a
        // renderer targets one format — so the pick target has to match it.
        // `read_hitmap` puts the channels back in order.
        let format = self
            .sized
            .as_ref()
            .map_or(wgpu::TextureFormat::Rgba8Unorm, |s| s.format);
        if self
            .pick_target
            .as_ref()
            .is_none_or(|t| t.width != width || t.height != height || t.format != format)
        {
            self.pick_target = Some(Target::new(&self.device, width, height, format));
        }
    }

    /// Rasterise one of the two scenes into `view`.
    fn rasterise(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        pass: Pass,
        view: &wgpu::TextureView,
        width: u32,
        height: u32,
    ) -> Result<(), BackendError> {
        let sized = self.sized.as_mut().expect("sized state ensured");
        let scene = match pass {
            Pass::Display => &sized.display,
            Pass::Pick => sized.pick.as_ref().expect("pick scene present"),
        };
        sized
            .renderer
            .render(
                scene,
                &mut sized.resources,
                &self.device,
                &self.queue,
                encoder,
                &RenderSize { width, height },
                view,
                &TextureBindings::new(),
            )
            .map_err(|e| match pass {
                Pass::Display => BackendError::Other(format!("hybrid render: {e}")),
                Pass::Pick => BackendError::Other(format!("hybrid pick render: {e}")),
            })
    }

    /// Drain the pick target into the CPU-side hitmap.
    ///
    /// Assumes the copy has been submitted and the buffer mapped.
    fn read_hitmap(&mut self, width: u32, height: u32) {
        let pick_target = self.pick_target.as_ref().expect("pick target ensured");
        let row_bytes = (width as usize) * 4;
        let row_px = width as usize;
        let hitmap = self.hitmap.get_or_insert_with(Vec::new);
        hitmap.clear();
        hitmap.resize(row_px * height as usize, 0);
        // The pick target carries the display format, because one renderer
        // targets one format. `pick::decode` reads an id out of a
        // little-endian RGBA word, so a BGRA target needs its red and blue
        // channels put back before that means anything.
        let swizzle = matches!(
            pick_target.format,
            wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb
        );
        {
            let data = pick_target.readback.slice(..).get_mapped_range();
            let padded = pick_target.padded_bytes_per_row as usize;
            for y in 0..height as usize {
                let dst: &mut [u8] =
                    bytemuck::cast_slice_mut(&mut hitmap[y * row_px..(y + 1) * row_px]);
                dst.copy_from_slice(&data[y * padded..y * padded + row_bytes]);
                if swizzle {
                    for px in dst.chunks_exact_mut(4) {
                        px.swap(0, 2);
                    }
                }
            }
        }
        pick_target.readback.unmap();
        self.hitmap_dims = Some((width, height));
    }

    /// Settle any deferred pick readback before a blocking path reuses the
    /// buffer.
    ///
    /// `map_async` on a buffer with a map already outstanding is a validation
    /// error, so a renderer that has been driven through
    /// [`Self::render_to_texture_deferring_pick`] and is then rendered
    /// blocking has to land the old readback first. Blocking is allowed on
    /// these paths, so this waits.
    fn settle_pending_pick(&mut self) -> Result<(), BackendError> {
        if self.pick_pending.is_some() {
            let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
            self.try_finish_pick()?;
        }
        Ok(())
    }

    /// Rasterise the pick scene, read it back, and refresh the hitmap.
    ///
    /// Uses an encoder of its own and submits it separately from the display
    /// pass. Both passes go through one renderer, whose per-frame coverage,
    /// paint and glyph uploads are written while a pass is being *recorded* —
    /// so sharing a command buffer would let this pass's uploads overwrite the
    /// display pass's before the GPU consumed them.
    fn submit_pick_blocking(&mut self, width: u32, height: u32) -> Result<(), BackendError> {
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("hephaestus.hybrid.pick"),
            });
        let pick_view = self
            .pick_target
            .as_ref()
            .expect("pick target ensured")
            .view
            .clone();
        self.rasterise(&mut encoder, Pass::Pick, &pick_view, width, height)?;
        {
            let pick_target = self.pick_target.as_ref().expect("pick target ensured");
            copy_to_readback(&mut encoder, pick_target, width, height);
        }
        self.queue.submit(std::iter::once(encoder.finish()));

        let pick_target = self.pick_target.as_ref().expect("pick target ensured");
        let (tx, rx) = futures_intrusive::channel::shared::oneshot_channel();
        pick_target
            .readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |res| {
                let _ = tx.send(res);
            });
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        await_map(pollster::block_on(rx.receive()))?;
        self.read_hitmap(width, height);
        Ok(())
    }

    /// Rasterise the pick scene and submit its readback without waiting.
    ///
    /// Pair with [`Self::try_finish_pick`]. Assumes the scene has already been
    /// replayed for this frame.
    fn submit_pick(&mut self, width: u32, height: u32) -> Result<(), BackendError> {
        self.ensure_pick_target(width, height);
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("hephaestus.hybrid.pick"),
            });
        let pick_view = self
            .pick_target
            .as_ref()
            .expect("pick target ensured")
            .view
            .clone();
        self.rasterise(&mut encoder, Pass::Pick, &pick_view, width, height)?;
        let pick_target = self.pick_target.as_ref().expect("pick target ensured");
        copy_to_readback(&mut encoder, pick_target, width, height);
        self.queue.submit(std::iter::once(encoder.finish()));

        let slot = std::sync::Arc::new(std::sync::Mutex::new(None));
        let sink = std::sync::Arc::clone(&slot);
        pick_target
            .readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |res| {
                if let Ok(mut guard) = sink.lock() {
                    *guard = Some(res);
                }
            });
        self.pick_pending = Some(PendingPick {
            slot,
            width,
            height,
        });
        Ok(())
    }

    /// Drain a readback submitted by [`Self::submit_pick`] into the hitmap,
    /// if it has landed.
    ///
    /// Returns whether the hitmap was refreshed: `false` means nothing was in
    /// flight, or the GPU has not finished. Never blocks, so a host that
    /// cannot park a thread calls this and accepts that the hitmap may lag
    /// the drawn frame.
    ///
    /// Only meaningful after [`Self::render_to_texture_deferring_pick`]; the
    /// blocking render paths drain their own readback before returning.
    pub fn try_finish_pick(&mut self) -> Result<bool, BackendError> {
        let Some(pending) = self.pick_pending.as_ref() else {
            return Ok(false);
        };
        let landed = pending
            .slot
            .lock()
            .map_err(|_| BackendError::Readback("pick readback slot poisoned".into()))?
            .take();
        let Some(result) = landed else {
            return Ok(false);
        };
        let PendingPick { width, height, .. } =
            self.pick_pending.take().expect("checked just above");
        result.map_err(|e| BackendError::Readback(e.to_string()))?;
        self.read_hitmap(width, height);
        Ok(true)
    }

    /// Rasterise into `view` and submit the pick pass without waiting on it.
    ///
    /// The non-blocking counterpart to
    /// [`WgpuRenderer::render_to_texture`](crate::WgpuRenderer::render_to_texture),
    /// whose pick readback parks the calling thread until the GPU is done —
    /// which a browser's main thread cannot do. Pair with
    /// [`Self::try_finish_pick`]: until that drains, [`Self::pick_at`] keeps
    /// answering from the last frame that landed.
    pub fn render_to_texture_deferring_pick(
        &mut self,
        view: &wgpu::TextureView,
        width: u32,
        height: u32,
        background: Color,
    ) -> Result<(), BackendError> {
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("hephaestus.hybrid.render_to_texture"),
            });
        let format = self.target_format;
        self.prepare(width, height, background, format, &mut encoder)?;
        self.rasterise(&mut encoder, Pass::Display, view, width, height)?;
        self.queue.submit(std::iter::once(encoder.finish()));

        if self.refreshes_pick() {
            // Drain first: that unmaps the readback buffer, and `map_async`
            // on a still-mapped buffer is a validation error. Draining also
            // has to happen before `ensure_pick_target`, which may reallocate
            // the target the in-flight readback is reading from.
            self.try_finish_pick()?;
            // Still in flight — skip this frame rather than queue a second
            // map on the same buffer. The hitmap lags until it lands, which
            // `pick_at` already documents.
            if self.pick_pending.is_none() {
                self.submit_pick(width, height)?;
            }
        }
        Ok(())
    }

    /// Shared front half of both render entry points: validate the size,
    /// rebuild size-bound state, upload images, and replay the recording.
    fn prepare(
        &mut self,
        width: u32,
        height: u32,
        background: Color,
        format: wgpu::TextureFormat,
        encoder: &mut wgpu::CommandEncoder,
    ) -> Result<(), BackendError> {
        if width == 0 || height == 0 {
            return Err(BackendError::Other(
                "cannot render a zero-sized frame".into(),
            ));
        }
        self.ensure_sized(width, height, format)?;
        self.upload_images(encoder)?;
        self.replay(background, width, height, self.refresh_pick);
        Ok(())
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

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("hephaestus.hybrid.render"),
            });
        self.prepare(
            width,
            height,
            background,
            wgpu::TextureFormat::Rgba8Unorm,
            &mut encoder,
        )?;
        if self
            .target
            .as_ref()
            .is_none_or(|t| t.width != width || t.height != height)
        {
            self.target = Some(Target::new(
                &self.device,
                width,
                height,
                wgpu::TextureFormat::Rgba8Unorm,
            ));
        }
        let picking = self.refreshes_pick();
        if picking {
            self.settle_pending_pick()?;
            self.ensure_pick_target(width, height);
        }

        let display_view = self.target.as_ref().expect("target ensured").view.clone();
        self.rasterise(&mut encoder, Pass::Display, &display_view, width, height)?;
        {
            let target = self.target.as_ref().expect("target ensured");
            copy_to_readback(&mut encoder, target, width, height);
        }
        // Submit before the pick pass is recorded, not after. Rasterising a
        // scene writes this frame's coverage, paints and glyphs into
        // renderer-owned textures, and both passes share one renderer — so
        // recording them into a single command buffer would let the pick
        // pass's uploads land before the GPU ran the display pass, and the
        // display would come out reading the pick pass's binary coverage.
        self.queue.submit(std::iter::once(encoder.finish()));
        if picking {
            self.submit_pick_blocking(width, height)?;
        }
        let target = self.target.as_ref().expect("target ensured");

        let display_slice = target.readback.slice(..);
        let (display_tx, display_rx) = futures_intrusive::channel::shared::oneshot_channel();
        display_slice.map_async(wgpu::MapMode::Read, move |res| {
            let _ = display_tx.send(res);
        });
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        await_map(pollster::block_on(display_rx.receive()))?;

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
        Ok(())
    }
}

impl WgpuRenderer for HybridRenderer {
    const REQUIRED_TARGET_USAGE: wgpu::TextureUsages = wgpu::TextureUsages::RENDER_ATTACHMENT;

    const TARGET_IS_PREMULTIPLIED: bool = true;

    fn render_to_texture(
        &mut self,
        view: &wgpu::TextureView,
        width: u32,
        height: u32,
        background: Color,
    ) -> Result<(), BackendError> {
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("hephaestus.hybrid.render_to_texture"),
            });
        let format = self.target_format;
        self.prepare(width, height, background, format, &mut encoder)?;
        self.rasterise(&mut encoder, Pass::Display, view, width, height)?;
        // Submitted before the pick pass is recorded — see
        // `submit_pick_blocking` for why they cannot share a command buffer.
        self.queue.submit(std::iter::once(encoder.finish()));

        if self.refreshes_pick() {
            self.settle_pending_pick()?;
            self.ensure_pick_target(width, height);
            self.submit_pick_blocking(width, height)?;
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
