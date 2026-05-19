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

    /// Clear the scene so the next frame can be authored.
    fn reset(&mut self);
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
