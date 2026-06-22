//! Backend trait and error types.

use crate::color::Color;
use crate::scene::SceneBuilder;

#[cfg(feature = "vello")]
pub mod vello;

/// Owns backend resources (GPU device, pipelines, etc.) and rasterizes a scene
/// to an RGBA8 buffer.
///
/// `SceneBuilder` is the authoring surface; this trait is the "produce output"
/// step. Split into two traits because authoring is pure CPU/infallible and
/// rasterization is fallible/resource-owning — and the recording backend only
/// needs `SceneBuilder`.
pub trait Renderer {
    /// Concrete scene type for this backend. Implements [`SceneBuilder`].
    type Scene: SceneBuilder;

    /// Mutable access to the scene being built. Issue draw calls against this.
    fn scene(&mut self) -> &mut Self::Scene;

    /// Render the current scene into `out`, which must be exactly
    /// `width * height * 4` bytes (RGBA8, premultiplied).
    fn render_to_buffer(
        &mut self,
        width: u32,
        height: u32,
        background: Color,
        out: &mut [u8],
    ) -> Result<(), BackendError>;
}

/// Optional extension for wgpu-backed renderers: rasterise directly into a
/// host-owned texture, bypassing the CPU readback that
/// [`Renderer::render_to_buffer`] uses.
///
/// This is the path for showing a scene in a window. A host that owns its own
/// wgpu device, queue, and presentation surface constructs the backend with a
/// `with_device`-style constructor so all GPU work shares a single device,
/// then calls [`render_to_texture`](Self::render_to_texture) each frame.
///
/// **Target constraints.** The supplied `view` must wrap a texture with
/// format `Rgba8Unorm` and usage including
/// `STORAGE_BINDING | COPY_SRC` — the backend writes via a compute shader
/// (Vello), so a render-attachment-only swap chain texture cannot be used
/// directly. Hosts whose presentation surface uses a different format
/// (typical for swap chains, which are usually `Bgra8UnormSrgb`) are
/// responsible for blitting from this view to the surface.
///
/// **Picking.** Picking (when enabled at construction) still rasterises the
/// parallel pick scene into the backend's own pick target and reads it back
/// to CPU, so [`pick_at`](crate::backend::vello::VelloRenderer::pick_at)
/// remains valid after a `render_to_texture` call.
#[cfg(feature = "vello")]
pub trait WgpuRenderer: Renderer {
    /// Render the current scene into `view`. See trait docs for the
    /// format / usage requirements `view` must satisfy.
    fn render_to_texture(
        &mut self,
        view: &wgpu::TextureView,
        width: u32,
        height: u32,
        background: Color,
    ) -> Result<(), BackendError>;
}

#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("output buffer is the wrong size (expected {expected} bytes, got {actual})")]
    BufferSize { expected: usize, actual: usize },

    #[error("no compatible GPU adapter available")]
    NoAdapter,

    #[error("failed to acquire GPU device: {0}")]
    DeviceRequest(String),

    #[error("GPU readback failed: {0}")]
    Readback(String),

    #[error("backend internal error: {0}")]
    Other(String),
}
