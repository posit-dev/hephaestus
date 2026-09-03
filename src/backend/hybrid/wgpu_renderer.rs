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

use super::{dimension, image_key, recorded_images, unpremultiply, HybridScene, Writer};
use crate::backend::{BackendError, Renderer, WgpuRenderer};
use crate::color::Color;
use crate::geometry::Affine;
use crate::pick::PickIndexScene;

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
    images: HashMap<u64, ImageSource>,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
}

/// Hephaestus Hybrid renderer: owns the wgpu device and queue, the recorded
/// scene, and the per-size rasterisation state.
///
/// Hit testing is a property of the scene, not of this renderer: the scene is
/// a [`crate::pick::PickIndexScene`], and
/// [`Self::with_picking`] is what turns its indexing on.
pub struct HybridRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    scene: PickIndexScene<HybridScene>,
    sized: Option<SizeBound>,
    target: Option<Target>,
    /// Format `render_to_texture` writes. A host presenting straight into its
    /// swap chain sets this to the surface's format.
    target_format: wgpu::TextureFormat,
}

impl HybridRenderer {
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

    /// Build a renderer that shares an existing wgpu device and queue — e.g.
    /// the device backing a window's swap chain.
    ///
    /// `device` and `queue` are handles (Arc-backed in wgpu); the host keeps
    /// its own and the renderer holds clones.
    pub fn with_device(device: &wgpu::Device, queue: &wgpu::Queue) -> Result<Self, BackendError> {
        Ok(Self::build(device.clone(), queue.clone(), false))
    }

    /// Like [`Self::with_device`] but with hit indexing enabled — see
    /// [`Self::with_picking`].
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
            scene: PickIndexScene::new(HybridScene::new(), picking),
            sized: None,
            target: None,
            target_format: wgpu::TextureFormat::Rgba8Unorm,
        }
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
        let images = recorded_images(&self.scene.inner().ops);
        if images.is_empty() {
            return Ok(());
        }
        let sized = self.sized.as_mut().expect("sized state ensured");
        for image in images {
            let key = image_key(&image);
            if sized.images.contains_key(&key) {
                continue;
            }
            // Their conversion handles both the format narrowing and the
            // premultiply; we only need the pixmap back out of it to upload.
            let ImageSource::Pixmap(pixmap) = ImageSource::from_peniko_image_data(&image) else {
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

    /// Replay the recording into the display scene.
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
            images: &sized.images,
        };
        self.scene.inner().ops.replay(&mut writer);
    }
}

impl HybridRenderer {
    /// Rasterise the display scene into `view`.
    fn rasterise(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        width: u32,
        height: u32,
    ) -> Result<(), BackendError> {
        let sized = self.sized.as_mut().expect("sized state ensured");
        let scene = &sized.display;
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
            .map_err(|e| BackendError::Other(format!("hybrid render: {e}")))
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
        self.replay(background, width, height);
        Ok(())
    }
}

impl Renderer for HybridRenderer {
    type Scene = PickIndexScene<HybridScene>;

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
        let display_view = self.target.as_ref().expect("target ensured").view.clone();
        self.rasterise(&mut encoder, &display_view, width, height)?;
        {
            let target = self.target.as_ref().expect("target ensured");
            copy_to_readback(&mut encoder, target, width, height);
        }
        self.queue.submit(std::iter::once(encoder.finish()));
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
        self.rasterise(&mut encoder, view, width, height)?;
        self.queue.submit(std::iter::once(encoder.finish()));
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
