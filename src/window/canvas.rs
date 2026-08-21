//! Presentation onto an HTML canvas already on the page.
//!
//! The browser counterpart to [`app`](super::app)'s winit driver, and the only
//! file here that names `web_sys`. The split in responsibilities is different:
//! a winit driver owns the event loop and calls the app, whereas a page owns
//! the event loop and calls this. So [`CanvasHost`] is a handle rather than a
//! driver — the host asks for a frame, reports a resize, and forwards pointer
//! events, and gets the same [`WindowApp`] callbacks either way.
//!
//! Everything below the handle is shared with the desktop path: the same
//! [`WindowSurface`] blit, the same [`Frame`], the same [`Event`]. Only the
//! device acquisition differs, because a browser has no thread to park.

use std::cell::Cell;

use crate::color::Color;
use crate::geometry::{Point, Size};
use crate::window::renderer::HostRenderer;
use crate::window::surface::WindowSurface;
use crate::window::BASE_DPI;
use crate::window::{Event, EventCtx, Frame, WindowApp, WindowConfig, WindowError};

/// A canvas set up to present rendered frames, driven by the page.
///
/// Construct one with [`CanvasHost::new`], then call [`Self::render`] whenever
/// a frame is wanted and [`Self::resize`] whenever the canvas changes size.
pub struct CanvasHost {
    surface: WindowSurface,
    renderer: HostRenderer,
    background: Color,
    dpi: f64,
    cursor: Option<Point>,
}

impl CanvasHost {
    /// Attach to `canvas` and open a GPU device against it.
    ///
    /// The surface is sized from the canvas's current backing store
    /// (`canvas.width` / `canvas.height`); when either is zero the config's
    /// size is used and written back to the element. `dpi` starts from the
    /// page's `devicePixelRatio` — pass subsequent changes to
    /// [`Self::resize`].
    ///
    /// Only `Backends::BROWSER_WEBGPU` is requested. WebGL2 has no compute
    /// stage and the Vello backend rasterises through compute pipelines, so a
    /// GL adapter would be found and then fail deep inside pipeline creation;
    /// asking only for WebGPU turns that into an honest
    /// [`WindowError::NoAdapter`] on a browser that cannot run this at all.
    pub async fn new(
        canvas: web_sys::HtmlCanvasElement,
        config: WindowConfig,
    ) -> Result<Self, WindowError> {
        let (width, height) = match (canvas.width(), canvas.height()) {
            (0, _) | (_, 0) => {
                canvas.set_width(config.width);
                canvas.set_height(config.height);
                (config.width, config.height)
            }
            wh => wh,
        };

        let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
        descriptor.backends = wgpu::Backends::BROWSER_WEBGPU;
        let instance = wgpu::Instance::new(descriptor);
        let target = instance
            .create_surface(wgpu::SurfaceTarget::Canvas(canvas))
            .map_err(|e| WindowError::Surface(e.to_string()))?;

        let surface = WindowSurface::new_async(
            &instance,
            target,
            width,
            height,
            config.present_mode,
            (!config.backend.can_present_directly()).then(|| config.backend.target_usage()),
        )
        .await?;
        let mut renderer = HostRenderer::new(
            config.backend,
            surface.device(),
            surface.queue(),
            config.picking,
        )?;
        if config.backend.can_present_directly() {
            renderer.set_target_format(surface.format());
        }

        let dpi = BASE_DPI * device_pixel_ratio();
        Ok(Self {
            surface,
            renderer,
            background: config.background,
            dpi,
            cursor: None,
        })
    }

    /// Draw, rasterise and present one frame.
    ///
    /// The same three steps the desktop path takes — render into the
    /// intermediate texture, blit it onto the swap chain, present — so the
    /// contract `tests/window_blit.rs` pins covers this path too.
    pub fn render<A: WindowApp>(&mut self, app: &mut A) -> Result<(), WindowError> {
        let (width, height) = self.surface.size();
        let size = Size::new(width as f64, height as f64);

        self.renderer.scene().clear();
        {
            let mut frame = Frame {
                scene: self.renderer.scene(),
                size,
                dpi: self.dpi,
            };
            app.draw(&mut frame);
        }

        // Deferring rather than `render_to_texture`: the trait method blocks
        // on the pick readback, which a browser main thread cannot do.
        let renderer = &mut self.renderer;
        let background = self.background;
        self.surface.draw_frame(
            |view| {
                renderer
                    .render_to_texture_deferring_pick(view, width, height, background)
                    .map_err(WindowError::from)
            },
            || {},
        )?;
        Ok(())
    }

    /// Hand one event to the app, reporting whether it asked for a redraw.
    ///
    /// The page decides what to do with that: schedule an animation frame,
    /// render immediately, or coalesce with other pending work.
    pub fn dispatch<A: WindowApp>(&mut self, app: &mut A, event: Event) -> bool {
        match event {
            Event::CursorMoved { position } => self.cursor = Some(position),
            Event::CursorLeft => self.cursor = None,
            _ => {}
        }

        // Drain before borrowing the renderer for the context: `EventCtx`
        // holds it immutably, so the app cannot drain from inside a handler.
        let _ = self.renderer.try_finish_pick();

        let (width, height) = self.surface.size();
        let redraw = Cell::new(false);
        // Nothing on a canvas host can honour an exit request — the page owns
        // the element's lifetime — so the flag is accepted and dropped.
        let mut exit = false;
        let mut ctx = EventCtx {
            renderer: &self.renderer,
            redraw: &redraw,
            cursor: self.cursor,
            size: Size::new(width as f64, height as f64),
            dpi: self.dpi,
            exit: &mut exit,
        };
        app.event(&mut ctx, event);
        redraw.get()
    }

    /// Resize the presentation surface and set the dpi for later frames.
    ///
    /// `width` and `height` are device pixels — a CSS box multiplied by the
    /// device pixel ratio — and `dpi` is `96.0 * ratio`. Zero in either
    /// dimension is ignored, as is a size that hasn't changed.
    pub fn resize(&mut self, width: u32, height: u32, dpi: f64) {
        self.surface.resize(width, height);
        self.dpi = dpi;
    }

    /// The pick id at a device-pixel coordinate.
    ///
    /// Always `None` unless [`WindowConfig::picking`] was enabled. Drains a
    /// landed readback first, which is why this takes `&mut self`.
    ///
    /// The readback is never waited on, so the hitmap can describe a frame or
    /// two behind what is on screen — invisible for hover, and the price of
    /// not blocking. Coordinates are device pixels, matching [`Self::size`].
    pub fn pick_at(&mut self, x: u32, y: u32) -> Option<u32> {
        // A failed drain means the readback errored, not that the pixel has
        // no id; either way there is nothing to report for this query.
        let _ = self.renderer.try_finish_pick();
        self.renderer.pick_at(x, y)
    }

    /// Drawing surface size in device pixels.
    pub fn size(&self) -> Size {
        let (width, height) = self.surface.size();
        Size::new(width as f64, height as f64)
    }

    /// Dots per inch frames are drawn at.
    pub fn dpi(&self) -> f64 {
        self.dpi
    }

    /// Set the color the scene is rasterised over.
    pub fn set_background(&mut self, background: Color) {
        self.background = background;
    }
}

/// The page's device pixel ratio, or 1.0 outside a browsing context.
fn device_pixel_ratio() -> f64 {
    web_sys::window()
        .map(|w| w.device_pixel_ratio())
        .filter(|r| *r > 0.0)
        .unwrap_or(1.0)
}
