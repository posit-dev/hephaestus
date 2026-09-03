//! Render a hephaestus plot document onto a canvas in the browser.
//!
//! A document carries a plot's *configuration*, not a picture of it, so the
//! page re-solves the layout at whatever size it has. Resizing reflows —
//! axes re-lay-out, ticks recompute, text re-wraps — instead of stretching.
//!
//! This crate is the binding layer, deliberately thin. It owns the document,
//! the composition and the GPU surface, and exposes imperative calls:
//! [`PlotHandle::render`], [`PlotHandle::resize`], [`PlotHandle::set_dark`].
//! Everything browser-shaped — `ResizeObserver`, `matchMedia`,
//! `requestAnimationFrame`, `fetch` — lives in `js/hephaestus.js`, which
//! wraps this in the `PlotView` class a page actually uses. JavaScript is
//! free; wasm bytes are not.
//!
//! ```js
//! import init, { PlotView, registerFontFromUrl } from './hephaestus.js';
//!
//! await init();
//! await registerFontFromUrl('/fonts/inter.ttf', { genericFor: 'sans-serif' });
//! const view = await PlotView.create(canvas, docBytes, { colorScheme: 'auto' });
//! ```

use wasm_bindgen::prelude::*;

use hephaestus::document::{read_document, ReadContext};
use hephaestus::plot::theme::Theme;
use hephaestus::plot::PlotComposition;
use hephaestus::text::GenericFamilyKind;
use hephaestus::window::{Frame, WindowApp, WindowConfig};

// The two hosts present the same surface; which one is compiled decides
// whether the bundle carries wgpu.
#[cfg(feature = "wgpu-backend")]
use hephaestus::window::CanvasHost as Host;
#[cfg(all(feature = "webgl", not(feature = "wgpu-backend")))]
use hephaestus::window::WebGlHost as Host;

/// Whether this browser can run the renderer at all.
///
/// What it tests depends on which backend was built. The wgpu build rasterises
/// through compute pipelines, so WebGPU is a hard requirement rather than a
/// preference. The WebGL2 build needs only a WebGL2 context, which is available
/// essentially everywhere — so it is very nearly always `true`, and the check
/// is kept so a page can use one code path for either bundle.
///
/// A page should check this before creating a view and fall back to a static
/// image when it is `false`.
#[wasm_bindgen(js_name = isSupported)]
pub fn is_supported() -> bool {
    #[cfg(feature = "wgpu-backend")]
    {
        js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("navigator"))
            .ok()
            .and_then(|nav| js_sys::Reflect::get(&nav, &JsValue::from_str("gpu")).ok())
            .is_some_and(|gpu| !gpu.is_undefined() && !gpu.is_null())
    }
    // Probe by asking a throwaway canvas for the context, which is the only
    // answer that accounts for a browser that has the API but cannot give one
    // out — a blocklisted driver, or too many live contexts.
    #[cfg(all(feature = "webgl", not(feature = "wgpu-backend")))]
    {
        web_sys::window()
            .and_then(|w| w.document())
            .and_then(|d| d.create_element("canvas").ok())
            .and_then(|el| el.dyn_into::<web_sys::HtmlCanvasElement>().ok())
            .and_then(|c| c.get_context("webgl2").ok().flatten())
            .is_some()
    }
}

/// Major version of the plot-document format this build reads.
///
/// A document whose major differs is refused outright — the check is equality,
/// not a floor — so a site and whatever writes its documents have to agree on
/// this number. Publish it alongside the bundle and assert on it in a build
/// step: nothing at runtime can recover from a mismatch, and the failure is a
/// plot that never appears.
#[wasm_bindgen(js_name = documentFormatVersion)]
pub fn document_format_version() -> u16 {
    hephaestus::document::FORMAT_VERSION_MAJOR
}

/// Register every font face in `bytes`, returning the family names they
/// landed under.
///
/// A browser starts with no fonts at all — nothing enumerates a system font
/// set — so a page that registers none renders chrome with no text. Call
/// this before creating any view: the first thing a document decodes is its
/// theme, and that is enough to shape.
///
/// Registration is process-global and permanent, so one call serves every
/// view on the page for the lifetime of the module.
///
/// The family names are the point of the return value: [`set_generic_family`]
/// takes names, and the only place a family's name exists is inside the file,
/// so a caller cannot pair the two without being told. Guessing from a
/// filename does not survive contact with a real font.
///
/// Accepts TTF, OTF, TTC, OTC and — with the `webfonts` feature, on by
/// default — WOFF and WOFF2, which are unwrapped to the sfnt inside first.
/// A blob holding no recognisable face is an error rather than an empty
/// list, since registering nothing silently would render a textless plot
/// with no indication why.
#[wasm_bindgen(js_name = registerFont)]
pub fn register_font(bytes: Vec<u8>) -> Result<Vec<String>, JsError> {
    let owned = decode_webfont(bytes)?;
    let families = hephaestus::text::register_font_families(owned);
    if families.is_empty() {
        return Err(JsError::new(
            "no font faces found; the bytes are not a TTF, OTF, TTC or OTC file",
        ));
    }
    Ok(families)
}

/// Whether any font family is available to shape with.
///
/// A browser starts with none, so this answers "does this page still need a
/// font?" — and it answers it after a document has been read, so a document
/// carrying embedded faces counts. That is what lets a fallback be fetched
/// only when it is genuinely needed rather than on a guess.
#[wasm_bindgen(js_name = hasFonts)]
pub fn has_fonts() -> bool {
    !hephaestus::text::registered_families().is_empty()
}

/// Unwrap a WOFF / WOFF2 container to the sfnt inside, or pass bytes through.
///
/// The shaper ingests sfnt only (TTF / OTF / TTC / OTC), and a font CDN serves
/// a browser WOFF2 — so without this the single most likely input is the one
/// that fails.
///
/// Takes the buffer by value so an sfnt — the common case, and every face the
/// client bundles — moves straight through to the shaper. wasm-bindgen has
/// already copied the `Uint8Array` into linear memory to hand us this, and
/// `register_font_families` wants it owned, so a borrow here would mean
/// copying half a megabyte of boot-path font twice over.
#[cfg(feature = "webfonts")]
fn decode_webfont(bytes: Vec<u8>) -> Result<Vec<u8>, JsError> {
    match bytes.get(..4) {
        Some(b"wOF2") => wuff::decompress_woff2(&bytes)
            .map_err(|e| JsError::new(&format!("could not decode the WOFF2 font: {e:?}"))),
        Some(b"wOFF") => wuff::decompress_woff1(&bytes)
            .map_err(|e| JsError::new(&format!("could not decode the WOFF font: {e:?}"))),
        _ => Ok(bytes),
    }
}

/// Without the `webfonts` feature the containers are refused by name, rather
/// than reaching the shaper and registering nothing.
#[cfg(not(feature = "webfonts"))]
fn decode_webfont(bytes: Vec<u8>) -> Result<Vec<u8>, JsError> {
    match bytes.get(..4) {
        Some(b"wOF2") | Some(b"wOFF") => Err(JsError::new(
            "this build cannot decode WOFF or WOFF2; use TTF, OTF, TTC or OTC, \
             or rebuild with the `webfonts` feature",
        )),
        _ => Ok(bytes),
    }
}

/// Point a generic family at concrete families already registered.
///
/// `kind` is one of `serif`, `sans-serif`, `monospace`, `cursive`,
/// `fantasy`, `system-ui`. A generic is an indirection through the font
/// context rather than a name, so registering a font is not enough on its
/// own — a theme asking for `sans-serif` resolves to nothing until this
/// says what `sans-serif` means here. Call it after [`register_font`], since
/// names that aren't registered are skipped.
#[wasm_bindgen(js_name = setGenericFamily)]
pub fn set_generic_family(kind: &str, families: Vec<String>) -> Result<(), JsError> {
    let kind = match kind {
        "serif" => GenericFamilyKind::Serif,
        "sans-serif" => GenericFamilyKind::SansSerif,
        "monospace" | "mono" => GenericFamilyKind::Mono,
        "cursive" => GenericFamilyKind::Cursive,
        "fantasy" => GenericFamilyKind::Fantasy,
        "system-ui" => GenericFamilyKind::SystemUi,
        other => {
            return Err(JsError::new(&format!(
                "unknown generic family {other:?}; expected one of serif, \
                 sans-serif, monospace, cursive, fantasy, system-ui"
            )))
        }
    };
    hephaestus::text::set_generic_family(kind, &families);
    Ok(())
}

/// The composition being drawn, plus what it takes to re-theme it.
struct DocumentApp {
    view: PlotComposition,
    /// The theme exactly as the document carried it. Never mutated, so
    /// deriving from it makes toggling light and dark idempotent — the
    /// alternative, inverting the live theme in place, drifts if a caller
    /// sets the same mode twice.
    base: Theme,
    dark: bool,
}

impl WindowApp for DocumentApp {
    fn draw(&mut self, frame: &mut Frame<'_>) {
        let (scene, size, dpi) = frame.parts();
        self.view.render(scene, size, dpi);
    }
}

impl DocumentApp {
    /// Re-derive the theme for the current mode and report the color the
    /// canvas should be cleared to.
    fn apply_theme(&mut self) -> hephaestus::Color {
        let theme = if self.dark {
            self.base.clone().invert()
        } else {
            self.base.clone()
        };
        // The canvas background lives outside the theme, so key it to the
        // paper anchor: that is the palette's background, and it inverts
        // along with everything that references it.
        let paper = theme.palette.paper;
        self.view.set_theme(theme);
        paper
    }
}

/// One document bound to one canvas.
///
/// The low-level binding. A page normally uses the `PlotView` class in
/// `js/hephaestus.js`, which owns the resize and color-scheme wiring and
/// calls through to this.
#[wasm_bindgen]
pub struct PlotHandle {
    host: Host,
    app: DocumentApp,
    hint_size: Option<(f64, f64)>,
    hint_dpi: Option<f64>,
}

#[wasm_bindgen]
impl PlotHandle {
    /// Read `doc` and attach it to `canvas`.
    ///
    /// Awaits GPU adapter and device acquisition, which a browser cannot do
    /// synchronously. Fails if the document is unreadable or if no WebGPU
    /// adapter is available — see [`is_supported`].
    ///
    /// `picking` makes the scene record a spatial hit index as it is drawn.
    /// That costs CPU per draw call whether or not anything is queried, so it
    /// stays off unless [`Self::pick_at`] is going to be called. Nothing is
    /// read back from the GPU either way.
    #[wasm_bindgen(js_name = create)]
    pub async fn create(
        canvas: web_sys::HtmlCanvasElement,
        doc: Vec<u8>,
        picking: bool,
    ) -> Result<PlotHandle, JsError> {
        #[cfg(feature = "debug-panics")]
        console_error_panic_hook::set_once();

        // One pass, and the shared context: `read_hints` beside
        // `read_composition` decodes the head twice, and `ReadContext::new`
        // builds a geom factory table per view.
        let doc = read_document(&doc, ReadContext::builtin()).map_err(to_js)?;
        let (view, hints) = (doc.composition, doc.hints);
        let base = view.theme_ref().clone();

        let mut app = DocumentApp {
            view,
            base,
            dark: false,
        };
        let background = app.apply_theme();

        // The document's size hint seeds the surface, so a page that hasn't
        // sized its canvas yet still gets the aspect the plot was built for.
        let (w, h) = hints.size.unwrap_or((800.0, 600.0));
        let config = WindowConfig::new("hephaestus")
            .size(w.max(1.0) as u32, h.max(1.0) as u32)
            .background(background)
            .picking(picking);

        #[cfg(feature = "wgpu-backend")]
        let host = Host::new(canvas, config).await.map_err(to_js)?;
        // WebGL2 needs no device request, so there is nothing to await.
        #[cfg(all(feature = "webgl", not(feature = "wgpu-backend")))]
        let host = Host::new(&canvas, config).map_err(to_js)?;
        Ok(PlotHandle {
            host,
            app,
            hint_size: hints.size,
            hint_dpi: hints.dpi,
        })
    }

    /// Draw one frame at the current size.
    pub fn render(&mut self) -> Result<(), JsError> {
        self.host.render(&mut self.app).map_err(to_js)
    }

    /// Resize the drawing surface.
    ///
    /// `width` and `height` are device pixels — a CSS box times the device
    /// pixel ratio — and `ratio` is that same ratio, which sets the dpi so
    /// theme lengths in points come out the right physical size. Does not
    /// draw; call [`Self::render`] after.
    pub fn resize(&mut self, width: u32, height: u32, ratio: f64) {
        let ratio = if ratio > 0.0 { ratio } else { 1.0 };
        #[cfg(feature = "wgpu-backend")]
        self.host.resize(width, height, 96.0 * ratio);
        #[cfg(all(feature = "webgl", not(feature = "wgpu-backend")))]
        let _ = self.host.resize(width, height, 96.0 * ratio);
    }

    /// Switch between the document's theme and its inverted form.
    ///
    /// Inversion swaps the palette's paper and ink anchors, which every
    /// chrome element references, so gridlines, axis text and titles all
    /// follow. A geom given an explicit color keeps it — marks adapt only
    /// when the plot expressed them as palette references.
    ///
    /// Does not draw; call [`Self::render`] after.
    #[wasm_bindgen(js_name = setDark)]
    pub fn set_dark(&mut self, dark: bool) {
        if self.app.dark == dark {
            return;
        }
        self.app.dark = dark;
        let background = self.app.apply_theme();
        self.host.set_background(background);
    }

    /// Whether the inverted theme is in use.
    #[wasm_bindgen(js_name = isDark)]
    pub fn is_dark(&self) -> bool {
        self.app.dark
    }

    /// The row id under a point, or `undefined` for empty space.
    ///
    /// Coordinates are device pixels — the canvas backing store, not CSS
    /// pixels, so scale a pointer event by the device pixel ratio. Always
    /// `undefined` unless `picking` was passed to [`Self::create`].
    ///
    /// Answers from the index the scene built while it was drawn, so it
    /// always describes the frame on screen.
    #[wasm_bindgen(js_name = pickAt)]
    pub fn pick_at(&self, x: f64, y: f64) -> Option<u32> {
        self.host.pick_at(x, y)
    }

    /// Width, in points, the document's writer rendered at, if it recorded one.
    ///
    /// Advisory — a document can be drawn at any size. Useful as an aspect
    /// ratio for a container that has to be sized before anything is laid out.
    #[wasm_bindgen(js_name = hintWidth)]
    pub fn hint_width(&self) -> Option<f64> {
        self.hint_size.map(|(w, _)| w)
    }

    /// Height, in points, the document's writer rendered at, if it recorded one.
    #[wasm_bindgen(js_name = hintHeight)]
    pub fn hint_height(&self) -> Option<f64> {
        self.hint_size.map(|(_, h)| h)
    }

    /// Dots per inch the document's writer rendered at, if it recorded one.
    #[wasm_bindgen(js_name = hintDpi)]
    pub fn hint_dpi(&self) -> Option<f64> {
        self.hint_dpi
    }
}

/// Carry a crate error across to JS with its `Display` text intact.
fn to_js<E: std::fmt::Display>(e: E) -> JsError {
    JsError::new(&e.to_string())
}
