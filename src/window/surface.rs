//! Presentation surface: the swap chain, the intermediate texture the
//! renderer writes into, and the blit that moves one to the other.
//!
//! A backend that rasterises through a render pipeline can write the acquired
//! swap-chain texture itself, so its frames are one step: render, present. One
//! that writes through a storage binding cannot — a surface texture is never
//! one — so its frames go intermediate texture → blit → swap chain. The
//! backend decides which, and [`WindowSurface`] allocates the intermediate and
//! the blitter only for the second case.

use crate::window::{PresentMode, WindowError};

/// Usage the blit itself needs, unioned with whatever the backend requires:
/// `TEXTURE_BINDING`, because the blit samples the intermediate texture.
const BLIT_USAGE: wgpu::TextureUsages = wgpu::TextureUsages::TEXTURE_BINDING;

/// The swap chain plus everything needed to get a rendered frame onto it.
pub(crate) struct WindowSurface {
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    /// The intermediate texture and the blit that moves it onto the swap
    /// chain. `None` when the renderer writes the surface texture itself.
    indirect: Option<Indirect>,
}

/// What a backend that cannot write a surface texture needs: somewhere to
/// render, and a blit to get it onto the swap chain.
struct Indirect {
    blitter: wgpu::util::TextureBlitter,
    target: wgpu::Texture,
    target_view: wgpu::TextureView,
    /// Usage the intermediate is allocated with, kept so a resize reallocates
    /// it the same way.
    usage: wgpu::TextureUsages,
}

impl WindowSurface {
    /// Pick an adapter compatible with `surface`, open a device on it, and
    /// configure the swap chain at `width` × `height`.
    ///
    /// Blocks on both requests, so this is the desktop path's constructor;
    /// see [`Self::new_async`] for the one a browser can use.
    pub(crate) fn new(
        instance: &wgpu::Instance,
        surface: wgpu::Surface<'static>,
        width: u32,
        height: u32,
        present_mode: PresentMode,
        target_usage: Option<wgpu::TextureUsages>,
    ) -> Result<Self, WindowError> {
        pollster::block_on(Self::new_async(
            instance,
            surface,
            width,
            height,
            present_mode,
            target_usage,
        ))
    }

    /// The body of [`Self::new`], awaiting adapter and device rather than
    /// blocking on them.
    ///
    /// This is the form a browser needs: there is no thread to park on the
    /// main event loop, so both requests have to be awaited.
    pub(crate) async fn new_async(
        instance: &wgpu::Instance,
        surface: wgpu::Surface<'static>,
        width: u32,
        height: u32,
        present_mode: PresentMode,
        target_usage: Option<wgpu::TextureUsages>,
    ) -> Result<Self, WindowError> {
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .map_err(|_| WindowError::NoAdapter)?;

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("hephaestus.window.device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::default(),
                trace: wgpu::Trace::Off,
                experimental_features: wgpu::ExperimentalFeatures::default(),
            })
            .await
            .map_err(|e| WindowError::DeviceRequest(e.to_string()))?;

        let capabilities = surface.get_capabilities(&adapter);
        let format = pick_surface_format(&capabilities.formats)
            .ok_or(WindowError::UnsupportedSurfaceFormat)?;

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: width.max(1),
            height: height.max(1),
            present_mode: present_mode.to_wgpu(),
            desired_maximum_frame_latency: 2,
            // The renderer emits straight alpha; presenting opaque means the
            // alpha channel is ignored rather than misinterpreted as
            // premultiplied by the compositor.
            alpha_mode: wgpu::CompositeAlphaMode::Opaque,
            view_formats: vec![],
        };
        surface.configure(&device, &config);

        let indirect = target_usage.map(|usage| {
            let usage = usage | BLIT_USAGE;
            let (target, target_view) = create_target(&device, config.width, config.height, usage);
            Indirect {
                blitter: wgpu::util::TextureBlitter::new(&device, format),
                target,
                target_view,
                usage,
            }
        });

        Ok(Self {
            device,
            queue,
            surface,
            config,
            indirect,
        })
    }

    /// The device the swap chain lives on. Hand this to the renderer so both
    /// share one device.
    pub(crate) fn device(&self) -> &wgpu::Device {
        &self.device
    }

    /// The queue paired with [`Self::device`].
    pub(crate) fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    /// The surface format the renderer must write.
    pub(crate) fn format(&self) -> wgpu::TextureFormat {
        self.config.format
    }

    /// Current surface size in device pixels.
    pub(crate) fn size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    /// Reconfigure the swap chain and reallocate the intermediate texture.
    ///
    /// Zero in either dimension is ignored — that is what a minimised window
    /// reports, and neither a swap chain nor a texture can be sized to it.
    pub(crate) fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        if self.config.width == width && self.config.height == height {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
        if let Some(indirect) = self.indirect.as_mut() {
            let (target, view) = create_target(&self.device, width, height, indirect.usage);
            indirect.target = target;
            indirect.target_view = view;
        }
    }

    /// Acquire a frame, render into it, and present it.
    ///
    /// `render` is handed the view it must draw into: the swap-chain texture
    /// itself when the backend can write one, otherwise the intermediate that
    /// is then blitted across. Returning `Ok(false)` means the frame was not
    /// presentable and nothing was drawn — a resize or an occluded window —
    /// which is not an error.
    ///
    /// `pre_present` runs immediately before the frame is handed to the
    /// compositor, which is the point a windowing backend wants told.
    pub(crate) fn draw_frame(
        &self,
        render: impl FnOnce(&wgpu::TextureView) -> Result<(), WindowError>,
        pre_present: impl FnOnce(),
    ) -> Result<bool, WindowError> {
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            wgpu::CurrentSurfaceTexture::Timeout
            | wgpu::CurrentSurfaceTexture::Occluded
            | wgpu::CurrentSurfaceTexture::Outdated => return Ok(false),
            wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                return Ok(false);
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                return Err(WindowError::Surface(
                    "surface validation error while acquiring a frame".into(),
                ))
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        match self.indirect.as_ref() {
            Some(indirect) => {
                render(&indirect.target_view)?;
                let mut encoder =
                    self.device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("hephaestus.window.blit"),
                        });
                indirect
                    .blitter
                    .copy(&self.device, &mut encoder, &indirect.target_view, &view);
                self.queue.submit([encoder.finish()]);
            }
            // Straight into the acquired texture: no intermediate, no blit,
            // one fewer full-screen copy per frame.
            None => render(&view)?,
        }

        pre_present();
        frame.present();
        Ok(true)
    }
}

/// Allocate the intermediate texture the renderer draws into.
fn create_target(
    device: &wgpu::Device,
    width: u32,
    height: u32,
    usage: wgpu::TextureUsages,
) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("hephaestus.window.target"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}

/// Choose a swap-chain format from what the surface offers.
///
/// Only the non-sRGB 8-bit formats qualify: the intermediate texture is
/// `Rgba8Unorm` and the blit is a plain copy, so an sRGB swap chain would
/// apply the transfer function a second time and wash the image out.
fn pick_surface_format(formats: &[wgpu::TextureFormat]) -> Option<wgpu::TextureFormat> {
    formats.iter().copied().find(|f| {
        matches!(
            f,
            wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Bgra8Unorm
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wgpu::TextureFormat;

    #[test]
    fn prefers_the_non_srgb_format_the_surface_offers() {
        let formats = [TextureFormat::Bgra8UnormSrgb, TextureFormat::Bgra8Unorm];
        assert_eq!(
            pick_surface_format(&formats),
            Some(TextureFormat::Bgra8Unorm)
        );
    }

    #[test]
    fn takes_the_first_acceptable_format_in_order() {
        let formats = [TextureFormat::Rgba8Unorm, TextureFormat::Bgra8Unorm];
        assert_eq!(
            pick_surface_format(&formats),
            Some(TextureFormat::Rgba8Unorm)
        );
    }

    #[test]
    fn rejects_an_srgb_only_surface() {
        let formats = [TextureFormat::Bgra8UnormSrgb, TextureFormat::Rgba8UnormSrgb];
        assert_eq!(pick_surface_format(&formats), None);
    }
}
