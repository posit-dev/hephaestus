//! Live window presentation.
//!
//! Opens an OS window, presents rendered frames through a wgpu surface, and
//! drives an event loop. This is the interactive counterpart to
//! [`Renderer::render_to_buffer`](crate::Renderer::render_to_buffer): instead
//! of an RGBA8 slab bound for a file, frames go straight to the screen and the
//! app gets resize and pointer events back.
//!
//! ```no_run
//! use hephaestus::window::{run, Frame, WindowApp, WindowConfig};
//!
//! struct Demo {
//!     view: hephaestus::plot::PlotComposition,
//! }
//!
//! impl WindowApp for Demo {
//!     fn draw(&mut self, frame: &mut Frame<'_>) {
//!         let (scene, size, dpi) = frame.parts();
//!         self.view.render(scene, size, dpi);
//!     }
//! }
//!
//! # fn demo(app: Demo) {
//! run(WindowConfig::new("demo"), app).unwrap();
//! # }
//! ```
//!
//! The window layer owns the GPU device and hands it to the renderer through
//! [`VelloRenderer::with_device`](crate::backend::vello::VelloRenderer::with_device),
//! so rasterisation and presentation share one device.
//!
//! Frames are drawn on demand. A resize schedules one; anything else the app
//! wants redrawn it asks for with [`EventCtx::request_redraw`], or it sets
//! [`WindowConfig::continuous_redraw`] and gets a frame as fast as the present
//! mode allows.

// `run` and its winit driver are desktop-only; the browser is served by
// `CanvasHost` instead. The surface and the driver still compile for wasm —
// proving the dependency set builds for the target is the point of keeping
// them in — so on wasm under `window` alone they have no caller.
#![cfg_attr(target_arch = "wasm32", allow(dead_code))]

#[cfg(feature = "window")]
mod app;
// `wgpu::SurfaceTarget::Canvas` exists only on wasm's web targets, so the
// host compiles there and the feature is inert anywhere else.
#[cfg(all(feature = "canvas", target_arch = "wasm32"))]
mod canvas;
mod event;
// Presentation needs something to rasterise with, and there is no sensible
// default to fall back on: the two backends differ in what the target texture
// must be. Naming one is the caller's choice, so say so rather than failing
// somewhere further in.
#[cfg(all(
    any(feature = "window", feature = "canvas"),
    not(any(feature = "vello", feature = "vello-hybrid"))
))]
compile_error!(
    "the `window` and `canvas` features need a wgpu rasterising backend: \
     enable `vello` (compute shaders) or `vello-hybrid` (sparse strips). \
     For a WebGL2 build with no wgpu at all, use `webgl` instead."
);

#[cfg(any(feature = "vello", feature = "vello-hybrid"))]
mod renderer;
#[cfg(any(feature = "vello", feature = "vello-hybrid"))]
mod surface;
// The WebGL2 host needs no surface and no wgpu: the canvas is the target.
#[cfg(all(feature = "webgl", target_arch = "wasm32"))]
mod webgl_host;

#[cfg(all(feature = "canvas", target_arch = "wasm32"))]
pub use canvas::CanvasHost;
pub use event::{Event, MouseButton};
#[cfg(any(feature = "vello", feature = "vello-hybrid"))]
pub use renderer::Backend;
#[cfg(all(feature = "webgl", target_arch = "wasm32"))]
pub use webgl_host::WebGlHost;

use std::cell::Cell;

use crate::color::Color;
use crate::geometry::{Point, Size};
use crate::scene::SceneBuilder;

/// Dots per inch a scale factor of 1.0 corresponds to.
const BASE_DPI: f64 = 96.0;

/// Application driven by the window event loop.
///
/// Implement this, hand it to [`run`], and the loop calls back for each frame
/// and each event until the app exits.
pub trait WindowApp {
    /// Draw one frame.
    ///
    /// The scene is already cleared. Called for every redraw, including the
    /// one that follows a resize.
    fn draw(&mut self, frame: &mut Frame<'_>);

    /// React to a window event. Ignores everything by default.
    fn event(&mut self, ctx: &mut EventCtx<'_>, event: Event) {
        let _ = (ctx, event);
    }
}

/// How finished frames are handed to the compositor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PresentMode {
    /// Wait for vertical blank. Steady frame pacing, no tearing.
    #[default]
    Vsync,
    /// Present as soon as a frame is ready. Lowest latency, may tear.
    NoVsync,
}

impl PresentMode {
    /// The wgpu present mode this maps to. Both choices are the `Auto`
    /// variants, which every backend supports.
    #[cfg(any(feature = "vello", feature = "vello-hybrid"))]
    fn to_wgpu(self) -> wgpu::PresentMode {
        match self {
            PresentMode::Vsync => wgpu::PresentMode::AutoVsync,
            PresentMode::NoVsync => wgpu::PresentMode::AutoNoVsync,
        }
    }
}

/// How the window is set up before it opens.
#[derive(Debug, Clone)]
pub struct WindowConfig {
    title: String,
    width: u32,
    height: u32,
    background: Color,
    picking: bool,
    continuous_redraw: bool,
    present_mode: PresentMode,
    #[cfg(any(feature = "vello", feature = "vello-hybrid"))]
    backend: Backend,
}

impl WindowConfig {
    /// A window with the given title, at the default size of 800 × 600.
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            width: 800,
            height: 600,
            background: Color::WHITE,
            picking: false,
            continuous_redraw: false,
            present_mode: PresentMode::default(),
            #[cfg(any(feature = "vello", feature = "vello-hybrid"))]
            backend: Backend::default(),
        }
    }

    /// Set the initial window size in logical pixels.
    pub fn size(mut self, width: u32, height: u32) -> Self {
        self.width = width;
        self.height = height;
        self
    }

    /// Set the color the scene is rasterised over.
    pub fn background(mut self, background: Color) -> Self {
        self.background = background;
        self
    }

    /// Enable hit testing, making [`EventCtx::pick_at`] and the other
    /// queries answer.
    ///
    /// The scene records a spatial index as it is drawn, which costs CPU per
    /// draw call whether or not anything is queried, so it stays off unless
    /// asked for. Nothing is read back from the GPU either way.
    pub fn picking(mut self, picking: bool) -> Self {
        self.picking = picking;
        self
    }

    /// Choose which rasterising backend draws the window.
    #[cfg(any(feature = "vello", feature = "vello-hybrid"))]
    ///
    /// Worth setting when picking matters: the sparse-strip backend reports
    /// exactly one id per pixel, where the compute-shader one can blend two
    /// overlapping marks' ids into a third.
    pub fn backend(mut self, backend: Backend) -> Self {
        self.backend = backend;
        self
    }

    /// The rasterising backend this window draws through.
    #[cfg(any(feature = "vello", feature = "vello-hybrid"))]
    pub fn selected_backend(&self) -> Backend {
        self.backend
    }

    /// Draw a new frame continuously rather than only when one is requested.
    pub fn continuous_redraw(mut self, continuous: bool) -> Self {
        self.continuous_redraw = continuous;
        self
    }

    /// Set how finished frames are handed to the compositor.
    pub fn present_mode(mut self, mode: PresentMode) -> Self {
        self.present_mode = mode;
        self
    }

    /// The window title.
    pub fn title(&self) -> &str {
        &self.title
    }
}

/// One frame's drawing context.
pub struct Frame<'a> {
    scene: &'a mut dyn SceneBuilder,
    size: Size,
    dpi: f64,
}

impl Frame<'_> {
    /// The scene to draw into, already cleared for this frame.
    pub fn scene(&mut self) -> &mut dyn SceneBuilder {
        self.scene
    }

    /// The scene together with the size and dpi to draw it at.
    ///
    /// Borrowing the scene borrows the whole frame, so this hands out all
    /// three at once for the usual `render(scene, size, dpi)` call.
    pub fn parts(&mut self) -> (&mut dyn SceneBuilder, Size, f64) {
        (self.scene, self.size, self.dpi)
    }

    /// Drawing surface size in device pixels.
    pub fn size(&self) -> Size {
        self.size
    }

    /// Dots per inch for this frame, accounting for the window's scale factor.
    ///
    /// Pass this alongside [`Self::size`] to
    /// [`PlotComposition::render`](crate::plot::PlotComposition::render) and
    /// physical units come out the right size on a high-density display.
    pub fn dpi(&self) -> f64 {
        self.dpi
    }
}

/// What an event handler can inspect and ask for.
pub struct EventCtx<'a> {
    index: Option<&'a crate::pick::PickIndex>,
    // A flag rather than a direct call into the windowing backend: it keeps
    // winit out of everything but `app.rs`, and lets the canvas host share
    // this type. The host acts on it once the handler returns.
    redraw: &'a Cell<bool>,
    cursor: Option<Point>,
    size: Size,
    dpi: f64,
    exit: &'a mut bool,
}

impl EventCtx<'_> {
    /// The topmost pick id at a device-pixel coordinate of the last drawn
    /// frame.
    ///
    /// Always `None` unless [`WindowConfig::picking`] was enabled. Answers
    /// from a CPU-side index, so calling it per pointer event is cheap and
    /// never lags the frame.
    pub fn pick_at(&self, x: f64, y: f64) -> Option<u32> {
        self.index?.pick_at(Point::new(x, y))
    }

    /// Every hit at a device-pixel coordinate, topmost first, each carrying
    /// the scope chain it was drawn inside — the path an event bubbles along.
    ///
    /// Chrome participates: a hover over an axis tick label reports a target
    /// even though chrome carries no authoring id.
    pub fn hits_at(&self, x: f64, y: f64) -> Vec<crate::pick::Hit<'_>> {
        self.index
            .map(|ix| ix.hits_at(Point::new(x, y)))
            .unwrap_or_default()
    }

    /// The hit index for the last drawn frame, for rectangle and lasso
    /// queries. `None` unless [`WindowConfig::picking`] was enabled.
    pub fn pick_index(&self) -> Option<&crate::pick::PickIndex> {
        self.index
    }

    /// The last known cursor position in device pixels, if it is over the
    /// window.
    pub fn cursor(&self) -> Option<Point> {
        self.cursor
    }

    /// Ask for another frame to be drawn.
    ///
    /// The frame is scheduled once the event handler returns, not during it.
    pub fn request_redraw(&self) {
        self.redraw.set(true);
    }

    /// Close the window and end the event loop.
    pub fn exit(&mut self) {
        *self.exit = true;
    }

    /// Drawing surface size in device pixels.
    pub fn size(&self) -> Size {
        self.size
    }

    /// Dots per inch for the surface, accounting for the scale factor.
    pub fn dpi(&self) -> f64 {
        self.dpi
    }
}

/// Anything that can go wrong opening or driving a window.
#[derive(Debug, thiserror::Error)]
pub enum WindowError {
    /// The renderer failed while rasterising a frame.
    #[error(transparent)]
    Backend(#[from] crate::BackendError),

    /// The event loop could not be created or ended abnormally.
    #[error("event loop failed: {0}")]
    EventLoop(String),

    /// The OS window could not be created.
    #[error("failed to create window: {0}")]
    Window(String),

    /// The presentation surface could not be created or used.
    #[error("surface failed: {0}")]
    Surface(String),

    /// No GPU adapter can drive the window's surface.
    #[error("no GPU adapter compatible with the window surface")]
    NoAdapter,

    /// The surface offers no format the blit can present into.
    #[error("surface offers no non-sRGB 8-bit format")]
    UnsupportedSurfaceFormat,

    /// Acquiring the GPU device failed.
    #[error("failed to acquire GPU device: {0}")]
    DeviceRequest(String),
}

/// Open a window and run the event loop until the app exits.
///
/// Blocks until the window closes. Must be called from the main thread — the
/// platform event loops require it, and [`Plot`](crate::plot::Plot) and
/// [`PlotComposition`](crate::plot::PlotComposition) are single-threaded by
/// design anyway.
#[cfg(all(feature = "window", not(target_arch = "wasm32")))]
pub fn run<A: WindowApp>(config: WindowConfig, app: A) -> Result<(), WindowError> {
    app::run(config, app)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_to_an_800_by_600_opaque_window_without_picking() {
        let config = WindowConfig::new("demo");
        assert_eq!(config.title(), "demo");
        assert_eq!((config.width, config.height), (800, 600));
        assert_eq!(config.background, Color::WHITE);
        assert!(!config.picking);
        assert!(!config.continuous_redraw);
        assert_eq!(config.present_mode, PresentMode::Vsync);
    }

    #[test]
    fn config_builders_chain() {
        let config = WindowConfig::new("demo")
            .size(320, 240)
            .picking(true)
            .continuous_redraw(true)
            .present_mode(PresentMode::NoVsync);
        assert_eq!((config.width, config.height), (320, 240));
        assert!(config.picking);
        assert!(config.continuous_redraw);
        assert_eq!(config.present_mode, PresentMode::NoVsync);
    }
}
