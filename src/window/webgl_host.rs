//! Presentation onto a canvas through WebGL2, with no wgpu involved.
//!
//! The browser counterpart to [`CanvasHost`](super::CanvasHost) and shaped the
//! same way — the page owns the event loop and calls `render`, `resize` and
//! `dispatch` — but there is no surface, no swap chain and no blit, because the
//! canvas's default framebuffer *is* the render target.
//!
//! What it buys is reach and size. WebGL2 is available essentially everywhere,
//! where WebGPU is not, and leaving wgpu out of the build takes a bundle well
//! below either wgpu-backed configuration.

use std::cell::Cell;

use crate::backend::hybrid::HybridWebGlRenderer;
use crate::color::Color;
use crate::geometry::{Point, Size};
use crate::scene::SceneBuilder as _;
use crate::window::{Event, EventCtx, Frame, PickSource, WindowApp, WindowConfig, WindowError};

/// Presents a scene onto an existing `<canvas>` through WebGL2.
pub struct WebGlHost {
    renderer: HybridWebGlRenderer,
    background: Color,
    dpi: f64,
    cursor: Option<Point>,
}

impl WebGlHost {
    /// Attach to `canvas`, acquiring its WebGL2 context.
    ///
    /// Unlike the wgpu host this needs no device request, so there is nothing
    /// to await. A canvas with no drawing-buffer size yet is given the one from
    /// `config`.
    pub fn new(
        canvas: &web_sys::HtmlCanvasElement,
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
        let renderer = HybridWebGlRenderer::new(canvas, width, height, config.picking)?;
        Ok(Self {
            renderer,
            background: config.background,
            dpi: super::BASE_DPI,
            cursor: None,
        })
    }

    /// Draw one frame onto the canvas.
    pub fn render<A: WindowApp>(&mut self, app: &mut A) -> Result<(), WindowError> {
        let size = self.size();
        self.renderer.scene().clear();
        {
            let mut frame = Frame {
                scene: self.renderer.scene(),
                size,
                dpi: self.dpi,
            };
            app.draw(&mut frame);
        }
        self.renderer.present(self.background)?;
        Ok(())
    }

    /// Hand one event to the app, returning whether it asked for a redraw.
    pub fn dispatch<A: WindowApp>(&mut self, app: &mut A, event: Event) -> bool {
        match event {
            Event::CursorMoved { position } => self.cursor = Some(position),
            Event::CursorLeft => self.cursor = None,
            _ => {}
        }
        let size = self.size();
        let redraw = Cell::new(false);
        // A page owns its canvas's lifetime, so an exit request has no meaning
        // here: accepted and dropped, as on the wgpu canvas host.
        let mut exit = false;
        let mut ctx = EventCtx {
            renderer: &self.renderer,
            redraw: &redraw,
            cursor: self.cursor,
            size,
            dpi: self.dpi,
            exit: &mut exit,
        };
        app.event(&mut ctx, event);
        redraw.get()
    }

    /// Match the renderer to a new canvas drawing-buffer size, and set the dpi
    /// for later frames.
    ///
    /// `width` and `height` are device pixels; `dpi` is `96.0 * ratio`. Zero in
    /// either dimension is ignored — that is what a hidden element reports.
    pub fn resize(&mut self, width: u32, height: u32, dpi: f64) -> Result<(), WindowError> {
        if width == 0 || height == 0 {
            return Ok(());
        }
        self.renderer.resize(width, height)?;
        self.dpi = dpi;
        Ok(())
    }

    /// Id at a device-pixel coordinate of the last refreshed hitmap.
    ///
    /// Always `None` unless [`WindowConfig::picking`] was enabled. Reads a
    /// CPU-side hitmap, so calling it per pointer event is cheap.
    pub fn pick_at(&self, x: u32, y: u32) -> Option<u32> {
        self.renderer.pick_at(x, y)
    }

    /// Control whether the coming frame refreshes the hitmap.
    ///
    /// The pick pass rasterises the scene a second time and reads it back
    /// synchronously, so a page redrawing faster than it queries — mid-resize,
    /// or while animating — should leave it alone for a frame or two.
    pub fn set_refresh_pick(&mut self, refresh: bool) {
        self.renderer.set_refresh_pick(refresh);
    }

    /// Drawing surface size in device pixels.
    pub fn size(&self) -> Size {
        let (w, h) = self.renderer.size();
        Size::new(w as f64, h as f64)
    }

    /// Dots per inch frames are drawn at.
    pub fn dpi(&self) -> f64 {
        self.dpi
    }

    /// Set the colour the scene is rasterised over for later frames.
    pub fn set_background(&mut self, background: Color) {
        self.background = background;
    }
}

impl PickSource for HybridWebGlRenderer {
    fn pick_at(&self, x: u32, y: u32) -> Option<u32> {
        HybridWebGlRenderer::pick_at(self, x, y)
    }
}
