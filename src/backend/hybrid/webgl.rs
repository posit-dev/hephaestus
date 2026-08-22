//! The WebGL2-backed renderer: the same sparse-strip rasteriser as the wgpu
//! path, talking to a canvas's `WebGl2RenderingContext` through precompiled
//! GLSL.
//!
//! Only `wasm32` compiles this, since the context type exists nowhere else.
//!
//! # How it differs from the wgpu path
//!
//! There is no offscreen target. `vello_hybrid`'s WebGL renderer draws to the
//! canvas's default framebuffer and nothing else — the field that would
//! redirect it is private upstream — so:
//!
//! - **Presentation is one step.** No intermediate texture, no blit, no swap
//!   chain. The canvas *is* the target.
//! - **Picking draws the id buffer to the canvas and reads it straight back**,
//!   then draws the display over the top. Both happen inside one JS task, and a
//!   canvas is not composited until the task yields, so the id frame is never
//!   seen. It does mean the pick pass must come first.
//! - **There is no `Renderer` impl.** That trait rasterises into a caller's
//!   byte buffer, which here would mean drawing to a visible canvas and reading
//!   it back — a different contract than the name promises. Use the wgpu
//!   renderer for file output.

use std::collections::HashMap;
use std::sync::Arc;

use vello_common::paint::ImageSource;
use vello_hybrid::{Pixmap, RenderSize, Resources, Scene, WebGlRenderer, WebGlTextureBindings};
use web_sys::{HtmlCanvasElement, WebGl2RenderingContext};

use super::{
    dimension, image_key, recorded_images, HybridScene, Pass, Writer, PICK_ALIASING_THRESHOLD,
};
use crate::backend::BackendError;
use crate::color::Color;
use crate::geometry::Affine;
use crate::pick;

/// Hephaestus WebGL2 renderer: owns the canvas's GL context, the recorded
/// scene, and the sparse-strip scenes it replays into.
pub struct HybridWebGlRenderer {
    renderer: WebGlRenderer,
    resources: Resources,
    scene: HybridScene,
    display: Scene,
    pick: Option<Scene>,
    /// Atlas handle per image, keyed as [`image_key`].
    images: HashMap<u64, ImageSource>,
    width: u32,
    height: u32,
    hitmap: Option<Vec<u32>>,
    hitmap_dims: Option<(u32, u32)>,
    refresh_pick: bool,
}

impl HybridWebGlRenderer {
    /// Build a renderer against `canvas`, acquiring its WebGL2 context.
    ///
    /// `width` and `height` are the canvas's drawing-buffer size in device
    /// pixels; the renderer requires them to keep matching, so a host that
    /// resizes the canvas calls [`Self::resize`].
    pub fn new(
        canvas: &HtmlCanvasElement,
        width: u32,
        height: u32,
        picking: bool,
    ) -> Result<Self, BackendError> {
        let (w, h) = (dimension(width)?, dimension(height)?);
        let (renderer, resources) = WebGlRenderer::new(canvas);
        Ok(Self {
            renderer,
            resources,
            scene: HybridScene::new(),
            display: Scene::new(w, h),
            pick: picking.then(|| Scene::new(w, h)),
            images: HashMap::new(),
            width,
            height,
            hitmap: None,
            hitmap_dims: None,
            refresh_pick: true,
        })
    }

    /// The scene to draw into.
    pub fn scene(&mut self) -> &mut HybridScene {
        &mut self.scene
    }

    /// Match the renderer to a new canvas drawing-buffer size.
    ///
    /// The atlas is invalidated with the scenes, since image handles belong to
    /// the rasteriser state being rebuilt.
    pub fn resize(&mut self, width: u32, height: u32) -> Result<(), BackendError> {
        if (width, height) == (self.width, self.height) {
            return Ok(());
        }
        let (w, h) = (dimension(width)?, dimension(height)?);
        self.display.reset_and_resize(w, h);
        if let Some(pick) = self.pick.as_mut() {
            pick.reset_and_resize(w, h);
        }
        self.images.clear();
        self.width = width;
        self.height = height;
        Ok(())
    }

    /// Control whether the coming frame refreshes the hitmap.
    ///
    /// Costs the same as it does on the wgpu path — a second strip generation
    /// over the same geometry, plus a synchronous `readPixels` — so a host
    /// redrawing faster than it queries should throttle it.
    pub fn set_refresh_pick(&mut self, refresh: bool) {
        self.refresh_pick = refresh;
    }

    /// Whether the coming frame will refresh the hitmap.
    pub fn refreshes_pick(&self) -> bool {
        self.pick.is_some() && self.refresh_pick
    }

    /// Id recorded at the given pixel of the last refreshed hitmap.
    pub fn pick_at(&self, x: u32, y: u32) -> Option<u32> {
        let (w, h) = self.hitmap_dims?;
        if x >= w || y >= h {
            return None;
        }
        pick::decode(self.hitmap.as_ref()?[(y * w + x) as usize])
    }

    /// Current drawing-buffer size in device pixels.
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Raw pick pixels of the last refreshed hitmap, for bulk queries.
    pub fn hitmap(&self) -> Option<&[u32]> {
        self.hitmap.as_deref()
    }

    /// Draw the recorded scene onto the canvas.
    ///
    /// When picking is on and due, the id buffer is drawn and read back first,
    /// then overdrawn by the display — invisible, because the canvas is not
    /// composited until this task yields.
    pub fn present(&mut self, background: Color) -> Result<(), BackendError> {
        self.upload_images();
        let refresh = self.refreshes_pick();
        self.replay(background, refresh);

        let size = RenderSize {
            width: self.width,
            height: self.height,
        };
        let bindings = WebGlTextureBindings::new();

        if refresh {
            let pick = self.pick.as_ref().expect("pick scene present");
            self.renderer
                .render(pick, &mut self.resources, &size, &bindings)
                .map_err(|e| BackendError::Other(format!("hybrid webgl pick render: {e}")))?;
            self.read_hitmap();
        }
        self.renderer
            .render(&self.display, &mut self.resources, &size, &bindings)
            .map_err(|e| BackendError::Other(format!("hybrid webgl render: {e}")))
    }

    /// Upload every image the recording needs that is not already resident.
    fn upload_images(&mut self) {
        for image in recorded_images(&self.scene.ops) {
            let key = image_key(image);
            if self.images.contains_key(&key) {
                continue;
            }
            let ImageSource::Pixmap(pixmap) = ImageSource::from_peniko_image_data(image) else {
                continue;
            };
            let transparency = pixmap.may_have_transparency();
            let id = self
                .renderer
                .upload_image::<Arc<Pixmap>>(&mut self.resources, &pixmap);
            self.images.insert(
                key,
                ImageSource::opaque_id_with_transparency_hint(id, transparency),
            );
        }
    }

    /// Replay the recording into the display scene, and the pick scene when due.
    fn replay(&mut self, background: Color, refresh_pick: bool) {
        let frame = crate::geometry::Rect::new(0.0, 0.0, self.width.into(), self.height.into());

        self.display.reset();
        self.display.set_aliasing_threshold(None);
        // No base-colour parameter, so the background is the first draw.
        self.display.set_transform(Affine::IDENTITY);
        self.display.set_paint(background);
        self.display.fill_rect(&frame);
        let mut writer = Writer {
            scene: &mut self.display,
            resources: &mut self.resources,
            pass: Pass::Display,
            images: &self.images,
        };
        self.scene.ops.replay(&mut writer);

        if !refresh_pick {
            return;
        }
        let pick = self.pick.as_mut().expect("pick scene present");
        pick.reset();
        // Binary coverage: one primitive owns each pick pixel, so an edge
        // reports a real id rather than a blend of the two either side.
        pick.set_aliasing_threshold(Some(PICK_ALIASING_THRESHOLD));
        let mut writer = Writer {
            scene: pick,
            resources: &mut self.resources,
            pass: Pass::Pick,
            images: &self.images,
        };
        self.scene.ops.replay(&mut writer);
    }

    /// Read the just-drawn id buffer out of the default framebuffer.
    fn read_hitmap(&mut self) {
        let (w, h) = (self.width as usize, self.height as usize);
        let mut raw = vec![0u8; w * h * 4];
        let gl = self.renderer.gl_context();
        // Errors here leave the hitmap as it was rather than corrupting it: a
        // stale answer beats a wrong one.
        if gl
            .read_pixels_with_opt_u8_array(
                0,
                0,
                self.width as i32,
                self.height as i32,
                WebGl2RenderingContext::RGBA,
                WebGl2RenderingContext::UNSIGNED_BYTE,
                Some(&mut raw),
            )
            .is_err()
        {
            return;
        }

        let hitmap = self.hitmap.get_or_insert_with(Vec::new);
        hitmap.clear();
        hitmap.resize(w * h, 0);
        // GL reads bottom-up while the hitmap is indexed top-down, so the rows
        // go back in reverse.
        for y in 0..h {
            let src = (h - 1 - y) * w * 4;
            let dst: &mut [u8] = bytemuck_cast(&mut hitmap[y * w..(y + 1) * w]);
            dst.copy_from_slice(&raw[src..src + w * 4]);
        }
        self.hitmap_dims = Some((self.width, self.height));
    }
}

/// View a `u32` row as the bytes behind it.
///
/// Hand-rolled rather than pulling `bytemuck` in: this build exists to be
/// small, and one cast does not justify a dependency.
fn bytemuck_cast(row: &mut [u32]) -> &mut [u8] {
    // Safety: `u32` has no invalid bit patterns and no padding, so any byte
    // sequence of the same length is a valid `[u32]` and vice versa. The
    // lifetime and length are both derived from the input.
    unsafe { core::slice::from_raw_parts_mut(row.as_mut_ptr().cast::<u8>(), row.len() * 4) }
}
