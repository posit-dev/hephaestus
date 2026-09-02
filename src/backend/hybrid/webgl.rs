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
//! - **Picking costs no GPU work at all.** The scene records a CPU-side hit
//!   index as it is drawn, so there is nothing to rasterise, read back, or
//!   order against the display.
//! - **There is no `Renderer` impl.** That trait rasterises into a caller's
//!   byte buffer, which here would mean drawing to a visible canvas and reading
//!   it back — a different contract than the name promises. Use the wgpu
//!   renderer for file output.

use std::collections::HashMap;
use std::sync::Arc;

use vello_common::paint::ImageSource;
use vello_hybrid::{Pixmap, RenderSize, Resources, Scene, WebGlRenderer, WebGlTextureBindings};
use web_sys::HtmlCanvasElement;

use super::{dimension, image_key, recorded_images, HybridScene, Writer};
use crate::backend::BackendError;
use crate::color::Color;
use crate::geometry::Affine;
use crate::path::{FillRule, Path};
use crate::pick::{Hit, PickIndexScene};

/// Hephaestus WebGL2 renderer: owns the canvas's GL context, the recorded
/// scene, and the sparse-strip scenes it replays into.
pub struct HybridWebGlRenderer {
    renderer: WebGlRenderer,
    resources: Resources,
    scene: PickIndexScene<HybridScene>,
    display: Scene,
    /// Atlas handle per image, keyed as [`image_key`].
    images: HashMap<u64, ImageSource>,
    width: u32,
    height: u32,
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
            scene: PickIndexScene::new(HybridScene::new(), picking),
            display: Scene::new(w, h),
            images: HashMap::new(),
            width,
            height,
        })
    }

    /// The scene to draw into.
    pub fn scene(&mut self) -> &mut PickIndexScene<HybridScene> {
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
        self.images.clear();
        self.width = width;
        self.height = height;
        Ok(())
    }

    /// The canvas drawing-buffer size the renderer is currently built for.
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Every hit at `p`, topmost first.
    pub fn hits_at(&self, p: crate::geometry::Point) -> Vec<Hit<'_>> {
        self.scene.hits_at(p)
    }

    /// The topmost authoring id at `p`.
    pub fn pick_at(&self, p: crate::geometry::Point) -> Option<u32> {
        self.scene.pick_at(p)
    }

    /// Every hit whose bounds intersect `rect` — rubber-band brushing.
    pub fn hits_in(&self, rect: crate::geometry::Rect) -> Vec<Hit<'_>> {
        self.scene.hits_in(rect)
    }

    /// Every hit entirely inside `rect` — a selection marquee.
    pub fn hits_within(&self, rect: crate::geometry::Rect) -> Vec<Hit<'_>> {
        self.scene.hits_within(rect)
    }

    /// Lasso selection.
    pub fn hits_in_path(&self, path: &Path, rule: FillRule) -> Vec<Hit<'_>> {
        self.scene.hits_in_path(path, rule)
    }

    /// The hit index the scene built. `None` when picking is off.
    pub fn pick_index(&self) -> Option<&crate::pick::PickIndex> {
        self.scene.indexes().then(|| self.scene.index())
    }

    /// Whether this renderer's scene records a hit index.
    pub fn picks(&self) -> bool {
        self.scene.indexes()
    }

    /// Draw the recorded scene onto the canvas.
    pub fn present(&mut self, background: Color) -> Result<(), BackendError> {
        self.upload_images();
        self.replay(background);

        let size = RenderSize {
            width: self.width,
            height: self.height,
        };
        let bindings = WebGlTextureBindings::new();
        self.renderer
            .render(&self.display, &mut self.resources, &size, &bindings)
            .map_err(|e| BackendError::Other(format!("hybrid webgl render: {e}")))
    }

    /// Upload every image the recording needs that is not already resident.
    fn upload_images(&mut self) {
        for image in recorded_images(&self.scene.inner().ops) {
            let key = image_key(&image);
            if self.images.contains_key(&key) {
                continue;
            }
            let ImageSource::Pixmap(pixmap) = ImageSource::from_peniko_image_data(&image) else {
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

    /// Replay the recording into the display scene.
    fn replay(&mut self, background: Color) {
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
            images: &self.images,
        };
        self.scene.inner().ops.replay(&mut writer);
    }
}
