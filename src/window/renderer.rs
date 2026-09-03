//! Backend selection for the presentation hosts.
//!
//! Either rasterising backend can drive a window, and they disagree about the
//! target texture — usage flags and alpha convention both — so the host asks
//! the backend rather than hardcoding what one of them happens to need.
//!
//! Selection is an enum rather than a trait object because `Renderer`'s
//! associated scene type makes it awkward as one; the scene is handed out as
//! `&mut dyn SceneBuilder`, which is object-safe. See `backend/CLAUDE.md`.

use crate::backend::{BackendError, Renderer as _, WgpuRenderer as _};
use crate::color::Color;
use crate::scene::SceneBuilder;

#[cfg(feature = "vello-hybrid")]
use crate::backend::hybrid::HybridRenderer;
#[cfg(feature = "vello")]
use crate::backend::vello::VelloRenderer;

/// Which rasterising backend a presentation host draws through.
///
/// Variants exist only for the backends compiled in, so an unavailable choice
/// is a compile error rather than a runtime one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Backend {
    /// Compute-shader rasterisation. Antialiases the pick pass, so
    /// overlapping picked marks can report an id that was never drawn.
    #[cfg(feature = "vello")]
    Vello,
    /// Sparse-strip rasterisation: coverage on the CPU, a render pipeline on
    /// the GPU. Picks without conflating ids and has no draw-count ceiling.
    #[cfg(feature = "vello-hybrid")]
    Hybrid,
}

impl Default for Backend {
    /// The compute-shader backend where it is available, since it is the
    /// default feature; otherwise whichever one is.
    fn default() -> Self {
        #[cfg(feature = "vello")]
        {
            Self::Vello
        }
        #[cfg(all(not(feature = "vello"), feature = "vello-hybrid"))]
        {
            Self::Hybrid
        }
    }
}

impl Backend {
    /// Usage flags the intermediate texture must carry for this backend.
    ///
    /// Read before any renderer exists: a host configures its surface, and
    /// the texture the renderer will write into, before it can hand the
    /// device over.
    pub(crate) fn target_usage(self) -> wgpu::TextureUsages {
        match self {
            #[cfg(feature = "vello")]
            Self::Vello => VelloRenderer::REQUIRED_TARGET_USAGE,
            #[cfg(feature = "vello-hybrid")]
            Self::Hybrid => HybridRenderer::REQUIRED_TARGET_USAGE,
        }
    }
}

impl Backend {
    /// Whether this backend can rasterise straight into a swap-chain texture.
    ///
    /// True when everything it asks of a target is something a surface texture
    /// already is — a colour attachment and nothing more. A backend needing a
    /// storage binding cannot, so its frames go through an intermediate
    /// texture and a blit.
    pub(crate) fn can_present_directly(self) -> bool {
        self.target_usage() == wgpu::TextureUsages::RENDER_ATTACHMENT
    }
}

/// The renderer a presentation host owns, resolved to one backend.
///
/// Boxed because the two renderers differ substantially in size and a host
/// holds exactly one for its lifetime, so the indirection costs a single
/// allocation and keeps the enum from carrying the larger variant's footprint
/// everywhere it is moved.
pub(crate) enum HostRenderer {
    #[cfg(feature = "vello")]
    Vello(Box<VelloRenderer>),
    #[cfg(feature = "vello-hybrid")]
    Hybrid(Box<HybridRenderer>),
}

impl HostRenderer {
    /// Build the chosen backend against a host-owned device and queue.
    pub(crate) fn new(
        backend: Backend,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        picking: bool,
    ) -> Result<Self, BackendError> {
        match backend {
            #[cfg(feature = "vello")]
            Backend::Vello => Ok(Self::Vello(Box::new(if picking {
                VelloRenderer::with_device_and_picking(device, queue)?
            } else {
                VelloRenderer::with_device(device, queue)?
            }))),
            #[cfg(feature = "vello-hybrid")]
            Backend::Hybrid => Ok(Self::Hybrid(Box::new(if picking {
                HybridRenderer::with_device_and_picking(device, queue)?
            } else {
                HybridRenderer::with_device(device, queue)?
            }))),
        }
    }

    /// Tell the renderer which format it will be writing.
    ///
    /// Only meaningful for a backend that presents directly; the blit path
    /// always hands over an `Rgba8Unorm` intermediate, which is the default.
    pub(crate) fn set_target_format(&mut self, format: wgpu::TextureFormat) {
        match self {
            #[cfg(feature = "vello")]
            Self::Vello(_) => {
                // Writes through a storage binding, which is `Rgba8Unorm` or
                // nothing, so there is no format to choose.
                let _ = format;
            }
            #[cfg(feature = "vello-hybrid")]
            Self::Hybrid(r) => r.set_target_format(format),
        }
    }

    /// The hit index the scene built while it was drawn, when this renderer
    /// was built with picking.
    pub(crate) fn pick_index(&self) -> Option<&crate::pick::PickIndex> {
        match self {
            #[cfg(feature = "vello")]
            Self::Vello(r) => r.pick_index(),
            #[cfg(feature = "vello-hybrid")]
            Self::Hybrid(r) => r.pick_index(),
        }
    }

    /// The scene to draw into.
    pub(crate) fn scene(&mut self) -> &mut dyn SceneBuilder {
        match self {
            #[cfg(feature = "vello")]
            Self::Vello(r) => r.scene(),
            #[cfg(feature = "vello-hybrid")]
            Self::Hybrid(r) => r.scene(),
        }
    }

    /// Rasterise the current scene into `view`.
    pub(crate) fn render_to_texture(
        &mut self,
        view: &wgpu::TextureView,
        width: u32,
        height: u32,
        background: Color,
    ) -> Result<(), BackendError> {
        match self {
            #[cfg(feature = "vello")]
            Self::Vello(r) => r.render_to_texture(view, width, height, background),
            #[cfg(feature = "vello-hybrid")]
            Self::Hybrid(r) => r.render_to_texture(view, width, height, background),
        }
    }
}
