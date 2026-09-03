//! SVG output: a `SceneBuilder` that emits markup instead of pixels.
//!
//! Unlike the rasterising backends this implements [`SceneBuilder`] and
//! not [`crate::Renderer`] — `Renderer`'s contract is to fill a buffer
//! of RGBA8, and there are no pixels here. It needs no GPU and no
//! optional dependency, which is what makes a renderer-free
//! `document-read` + `svg` build possible.
//!
//! The emitter streams: draw calls append to a body buffer and any
//! `<defs>` they need accumulate beside it, with the whole document
//! assembled at write time. Recording first would mean holding a cloned
//! path and brush per draw — hundreds of megabytes for a dense scatter —
//! to produce a string that could have been produced incrementally.
//! Nothing needs a second pass: `<defs>` is written ahead of the body it
//! was discovered from, because the body is a `String`.

mod base64;
mod defs;
mod fonts;
mod image;
mod outline;
mod paint;
mod path;
mod text;
mod writer;

use std::io;

use crate::blend::{BlendMode, Compose, Mix};
use crate::brush::{Brush, Image, Sampling};
use crate::color::Color;
use crate::geometry::{Affine, Size};
use crate::mesh::Mesh;
use crate::path::{FillRule, Path};
use crate::pick::{PickId, PickScope};
use crate::scene::{GlyphRun, SceneBuilder};

use defs::{DefKind, Defs};
use writer::{escape_attr, is_identity, num, transform_attr};

/// Something the scene expressed that SVG cannot, or cannot yet.
///
/// Reported rather than returned: a scene is still drawable when one
/// feature degrades, and refusing to write the other 99% of the picture
/// would help nobody. Mirrors `document`'s `UnsupportedItem`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SvgWarning {
    /// A sweep gradient was flattened to a solid color. SVG has no
    /// conic paint server in either version.
    SweepGradient,
    /// A layer asked for a Porter-Duff operator other than `SrcOver`.
    /// Expressing it needs the backdrop, which an SVG filter cannot
    /// reach.
    UnsupportedCompose,
    /// A stroke set different start and end caps. SVG has one
    /// `stroke-linecap`.
    AsymmetricCaps,
    /// A radial gradient had a non-zero focal radius, written as the
    /// SVG 2 `fr` attribute that older consumers ignore.
    RadialFocalRadius,
    /// An image brush was used as a fill or stroke paint.
    ImageBrushUnsupported,
    /// A coordinate was not finite and was written as zero.
    NonFiniteCoordinate,
    /// More layers were pushed than popped; the difference was closed
    /// at write time.
    UnbalancedLayers,
    /// A pick scope was popped where a layer was open, or popped with
    /// nothing open. The group was left alone rather than closing an
    /// element it did not open.
    UnbalancedScopes,
    /// A glyph run arrived with no source text and no outline path to
    /// fall back on.
    TextWithoutSource,
    /// Drawing an image needs a PNG encoder, which this build lacks.
    MissingPngFeature,
    /// An image's pixels were in a layout this backend cannot re-encode.
    UnembeddableImage,
    /// Embedding was asked for, but a face could not be inlined —
    /// almost always because it is a font *collection*, which
    /// `@font-face` cannot address a member of. Its text still renders
    /// in whatever the viewer resolves the family to.
    FontNotEmbeddable,
}

/// Warnings collected during a frame, deduplicated by variant.
///
/// Deduplicated because a twenty-thousand-triangle mesh full of sweep
/// brushes would otherwise report the same fact twenty thousand times.
#[derive(Default)]
pub(crate) struct Warnings(Vec<SvgWarning>);

impl Warnings {
    /// Record `w` if it has not been seen this frame.
    pub(crate) fn note(&mut self, w: SvgWarning) {
        if !self.0.contains(&w) {
            self.0.push(w);
        }
    }

    /// True when `w` was recorded.
    #[cfg(test)]
    pub(crate) fn contains(&self, w: &SvgWarning) -> bool {
        self.0.contains(w)
    }
}

/// How lengths are written on the root element.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SvgUnits {
    /// CSS pixels — the `viewBox` units, unadorned.
    #[default]
    Px,
    /// Points, so the file prints at the physical size it was rendered
    /// for. The `viewBox` is unchanged; only `width` and `height` gain
    /// a `pt` suffix.
    Pt,
}

/// Emission options.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SvgConfig {
    /// Painted behind everything as a full-canvas rect. `None` emits no
    /// rect at all — unlike `Renderer::render_to_buffer`, which always
    /// names a background color even when that color is transparent.
    pub background: Option<Color>,
    /// Decimal places for coordinates. The linear part of a matrix
    /// always uses more; see `writer`.
    pub decimals: u8,
    /// How `width` and `height` are written.
    pub units: SvgUnits,
    /// Prefix every generated id with this.
    ///
    /// Not a nicety: two documents inlined into one HTML page that both
    /// define `#lg0` will have the second's `url(#lg0)` resolve to the
    /// first's definition, in every browser.
    pub id_prefix: Option<String>,
    /// Emit `data-pick-id` for picked primitives, and
    /// `pointer-events="none"` for skipped ones. Off by default: file
    /// export is the common case and the attributes are pure weight
    /// there.
    pub pick_ids: bool,
    /// How text is written.
    pub text: TextMode,
    /// Inline the face bytes as `@font-face` rather than only naming
    /// the family.
    ///
    /// Off by default, and deliberately: a face is often megabytes —
    /// macOS resolves `sans-serif` to a 2.4 MB collection — so this can
    /// take a 30 kB plot past 3 MB. Worth it for a document that must
    /// render identically offline; not worth it by default.
    pub embed_fonts: bool,
}

/// Whether text stays text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextMode {
    /// Real `<text>` elements naming their font — editable, and the
    /// reason this backend exists.
    #[default]
    Text,
    /// Glyph outlines as `<path>`. Self-contained and identical in
    /// every viewer, at the cost of text that can no longer be
    /// selected, searched or retyped. For print, or for a consumer that
    /// cannot resolve fonts.
    Outline,
}

impl SvgConfig {
    /// Default options.
    pub fn new() -> Self {
        Self::default()
    }

    /// Paint `color` behind everything as a full-canvas rect.
    pub fn background(mut self, color: Option<Color>) -> Self {
        self.background = color;
        self
    }

    /// Set the decimal places coordinates are written to.
    pub fn decimals(mut self, decimals: u8) -> Self {
        self.decimals = decimals;
        self
    }

    /// Set how `width` and `height` are written on the root element.
    pub fn units(mut self, units: SvgUnits) -> Self {
        self.units = units;
        self
    }

    /// Prefix every generated id, so two documents can share a page.
    pub fn id_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.id_prefix = Some(prefix.into());
        self
    }

    /// Emit picking attributes.
    pub fn pick_ids(mut self, on: bool) -> Self {
        self.pick_ids = on;
        self
    }

    /// Choose whether text stays text or becomes outlines.
    pub fn text(mut self, mode: TextMode) -> Self {
        self.text = mode;
        self
    }

    /// Inline the face bytes rather than only naming the family.
    pub fn embed_fonts(mut self, on: bool) -> Self {
        self.embed_fonts = on;
        self
    }
}

impl Default for SvgConfig {
    fn default() -> Self {
        Self {
            background: None,
            decimals: 3,
            units: SvgUnits::default(),
            id_prefix: None,
            pick_ids: false,
            text: TextMode::default(),
            embed_fonts: false,
        }
    }
}

/// One pending fill, held back so a stroke of the same path can join it.
struct PendingFill {
    path: Path,
    transform: Affine,
    rule: FillRule,
    paint: paint::Paint,
    pick: PickId,
}

/// A scene that emits SVG.
///
/// Build it, draw into it exactly as into any other [`SceneBuilder`],
/// then hand it to [`write_svg`] or [`encode_svg`].
pub struct SvgScene {
    size: Size,
    dpi: f64,
    config: SvgConfig,
    body: String,
    defs: Defs,
    warnings: Warnings,
    /// Open `<g>` elements, innermost last.
    ///
    /// Tagged rather than a plain count because layers and pick scopes both
    /// emit groups and their stacks are independent — a scope opened inside
    /// a layer need not close inside it. Popping the wrong kind would emit
    /// `</g>` against the wrong element and produce malformed XML.
    groups: Vec<GroupKind>,
    pending: Option<PendingFill>,
    /// Runs accumulating toward one `<text>` element.
    block: text::TextBlock,
    /// Faces the document referenced, for the `<style>` block.
    fonts: fonts::FontRegistry,
    /// Chrome belonging to the open text block — span backgrounds and
    /// borders — held so it can be written *ahead* of the `<text>` it
    /// belongs to rather than splitting it in two.
    block_prelude: String,
    /// The font hoisted onto the root element for every `<text>` to
    /// inherit.
    root_font: text::RootFont,
}

impl SvgScene {
    /// A scene covering `size`, rendered for `dpi`.
    ///
    /// Both are what the caller is about to pass to
    /// `PlotComposition::render`, so the two lines agree by
    /// construction.
    pub fn new(size: Size, dpi: f64) -> Self {
        Self::with_config(size, dpi, SvgConfig::default())
    }

    /// As [`Self::new`], with emission options.
    pub fn with_config(size: Size, dpi: f64, config: SvgConfig) -> Self {
        Self {
            size,
            dpi,
            config,
            body: String::new(),
            defs: Defs::default(),
            warnings: Warnings::default(),
            groups: Vec::new(),
            pending: None,
            block: text::TextBlock::default(),
            fonts: fonts::FontRegistry::default(),
            block_prelude: String::new(),
            root_font: text::RootFont::default(),
        }
    }

    /// Resize between frames. Does not discard drawn content; call
    /// [`SceneBuilder::clear`] for that.
    pub fn set_size(&mut self, size: Size, dpi: f64) {
        self.size = size;
        self.dpi = dpi;
    }

    /// The canvas size.
    pub fn size(&self) -> Size {
        self.size
    }

    /// Everything the scene expressed that SVG could not, deduplicated.
    pub fn warnings(&self) -> &[SvgWarning] {
        &self.warnings.0
    }

    /// The emission options in force.
    pub fn config(&self) -> &SvgConfig {
        &self.config
    }

    /// The id prefix, or the empty string.
    fn prefix(&self) -> &str {
        self.config.id_prefix.as_deref().unwrap_or("")
    }

    /// Emit the accumulating text block, if any.
    ///
    /// Its own chrome goes first: a span background sits *behind* its
    /// span's glyphs, so writing every one of a block's backgrounds
    /// ahead of every one of its runs preserves the stacking that
    /// matters. Span boxes are laid out sequentially along a line and
    /// stacked down the page, so one span's background never covers
    /// another's ink.
    fn flush_block(&mut self) {
        if self.block.runs.is_empty() {
            // Nothing is holding the prelude open, so it is ordinary
            // body content.
            self.body.push_str(&std::mem::take(&mut self.block_prelude));
            return;
        }
        self.body.push_str(&std::mem::take(&mut self.block_prelude));
        let block = std::mem::take(&mut self.block);
        let (dec, pick) = (self.config.decimals, self.config.pick_ids);
        text::write_block(&mut self.body, &block, dec, pick, &mut self.root_font);
    }

    /// True when a text block is open, so its own chrome should be
    /// buffered rather than ending it.
    fn block_open(&self) -> bool {
        !self.block.runs.is_empty()
    }

    /// Flush any fill waiting for a stroke to join it.
    fn flush_pending(&mut self) {
        let Some(p) = self.pending.take() else { return };
        let (dec, pick) = (self.config.decimals, self.config.pick_ids);
        if self.block_open() {
            write_pending_fill(&mut self.block_prelude, &p, dec, pick);
        } else {
            write_pending_fill(&mut self.body, &p, dec, pick);
        }
    }

    /// Append `fill` and its companions.
    fn write_fill_attrs(&mut self, paint: &paint::Paint, rule: FillRule) {
        let dec = self.config.decimals;
        write_fill_attrs_to(&mut self.body, paint, rule, dec);
    }

    /// Append the stroke attributes for `stroke`, omitting SVG defaults.
    fn write_stroke_attrs(&mut self, stroke: &crate::stroke::Stroke, paint: &paint::Paint) {
        let dec = self.config.decimals;
        write_stroke_attrs_to(&mut self.body, stroke, paint, dec, &mut self.warnings);
    }

    /// Append the picking attributes, when the config asks for them.
    /// Close the innermost `<g>`, provided it is the kind being closed.
    ///
    /// A mismatch means the two stacks were interleaved rather than nested;
    /// emitting `</g>` anyway would close the wrong element, so the pop is
    /// dropped and noted instead.
    fn close_group(&mut self, kind: GroupKind, warning: SvgWarning) {
        if self.groups.last() != Some(&kind) {
            self.warnings.note(warning);
            return;
        }
        self.groups.pop();
        self.body.push_str("</g>");
    }

    fn write_pick(&mut self, pick: PickId) {
        let on = self.config.pick_ids;
        write_pick_to(&mut self.body, pick, on);
    }

    /// Draw a run as glyph outlines.
    fn write_glyph_outlines(&mut self, run: &GlyphRun<'_>, pick_id: PickId) {
        let dec = self.config.decimals;
        let prefix = self.prefix().to_string();
        let Some(d) = outline::outline_d(run, dec, &mut self.warnings) else {
            return;
        };
        let paint = paint::resolve(
            run.brush,
            None,
            &mut self.defs,
            &prefix,
            dec,
            &mut self.warnings,
        );
        self.body.push_str("<path d=\"");
        self.body.push_str(&d);
        self.body.push('"');
        match run.style {
            // An outline pass strokes the contours rather than filling.
            Some(stroke) => {
                self.body.push_str(" fill=\"none\"");
                self.write_stroke_attrs(stroke, &paint);
            }
            None => self.write_fill_attrs(&paint, FillRule::NonZero),
        }
        let t = run.transform;
        transform_attr(&mut self.body, t, dec);
        self.write_pick(pick_id);
        self.body.push_str("/>");
    }

    /// Serialize the whole document.
    fn to_svg(&self) -> String {
        let dec = self.config.decimals;
        let (w, h) = (self.size.width, self.size.height);
        // The block still open at write time can be the one that names
        // the font the root carries, so it is emitted before the header
        // that hoists it. Against a clone rather than the scene's own,
        // so serializing twice gives the same bytes twice.
        let mut root_font = self.root_font.clone();
        let mut tail = String::new();
        if !self.block.runs.is_empty() {
            text::write_block(
                &mut tail,
                &self.block,
                dec,
                self.config.pick_ids,
                &mut root_font,
            );
        }
        let mut out = String::with_capacity(self.body.len() + 512);
        // No `<?xml …?>` declaration: it is optional (UTF-8 is XML's
        // default) and omitting it lets the same bytes serve as a
        // standalone file and as an inline HTML fragment.
        out.push_str("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"");
        match self.config.units {
            SvgUnits::Px => num(&mut out, w, dec),
            SvgUnits::Pt => {
                num(&mut out, w * 72.0 / self.dpi, dec);
                out.push_str("pt");
            }
        }
        out.push_str("\" height=\"");
        match self.config.units {
            SvgUnits::Px => num(&mut out, h, dec),
            SvgUnits::Pt => {
                num(&mut out, h * 72.0 / self.dpi, dec);
                out.push_str("pt");
            }
        }
        out.push_str("\" viewBox=\"0 0 ");
        num(&mut out, w, dec);
        out.push(' ');
        num(&mut out, h, dec);
        out.push('"');
        // Named once for the whole document; the text that disagrees
        // names its own and the rest inherit these.
        if let Some(family) = root_font.family() {
            out.push_str(" font-family=\"");
            escape_attr(&mut out, family);
            out.push('"');
        }
        if let Some(size) = root_font.size() {
            out.push_str(" font-size=\"");
            num(&mut out, size as f64, dec);
            out.push('"');
        }
        // Both spellings of the same thing: SVG 1.1 renderers honour
        // `xml:space`, SVG 2 deprecates it for the CSS property and
        // browsers honour that. Without one, white-space processing
        // collapses runs of spaces and trims the ends, which changes
        // the width `textLength` is claiming. Every `<text>` needs it,
        // and both are inherited, so the root declares it for all of
        // them.
        if root_font.family().is_some() {
            out.push_str(" xml:space=\"preserve\"");
        }
        // Scopes any `mix-blend-mode` inside to this document rather
        // than to the page it may be inlined into.
        out.push_str(" style=\"isolation:isolate");
        if root_font.family().is_some() {
            out.push_str(";white-space:pre");
        }
        out.push_str("\">");
        // Derived from the whole document — which fonts it used — so
        // it is only known once drawing is done.
        let style = self.fonts.stylesheet(self.config.embed_fonts, dec);
        self.defs.write(&mut out, style.as_deref());
        if let Some(bg) = self.config.background {
            let p = paint::solid(bg);
            out.push_str("<rect width=\"");
            num(&mut out, w, dec);
            out.push_str("\" height=\"");
            num(&mut out, h, dec);
            out.push_str("\" fill=\"");
            out.push_str(&p.value);
            out.push('"');
            if let Some(a) = p.opacity {
                out.push_str(" fill-opacity=\"");
                num(&mut out, a as f64, 3);
                out.push('"');
            }
            out.push_str("/>");
        }
        out.push_str(&self.body);
        // Content still held back at write time: a text block waiting
        // for a sibling run, and a fill waiting for a stroke. Written
        // here rather than mutating, so serializing twice gives the
        // same bytes twice.
        if !self.block_prelude.is_empty() {
            out.push_str(&self.block_prelude);
        }
        out.push_str(&tail);
        if let Some(p) = &self.pending {
            write_pending_fill(&mut out, p, dec, self.config.pick_ids);
        }
        // A scene may leave layers open; closing them keeps the
        // document well-formed, which matters more than the warning.
        for _ in 0..self.groups.len() {
            out.push_str("</g>");
        }
        out.push_str("</svg>");
        out
    }
}

impl SceneBuilder for SvgScene {
    fn clear(&mut self) {
        self.block = text::TextBlock::default();
        self.block_prelude.clear();
        self.fonts.clear();
        self.body.clear();
        self.defs.clear();
        self.warnings.0.clear();
        self.root_font = text::RootFont::default();
        self.groups.clear();
        self.pending = None;
    }

    fn fill(
        &mut self,
        rule: FillRule,
        transform: Affine,
        brush: &Brush,
        brush_transform: Option<Affine>,
        path: &Path,
        pick_id: PickId,
    ) {
        // A decoration this block already declared semantically: drop
        // it rather than drawing the rule twice — and, as importantly,
        // without flushing the block, since a rule arriving between two
        // runs would otherwise split one piece of text into two
        // elements.
        if self.block_open() {
            if let Some(r) = axis_aligned_rect(path, transform) {
                if self.block.claims_rule(r) {
                    return;
                }
            }
        } else {
            self.flush_block();
        }
        self.flush_pending();
        if path.elements().is_empty() {
            return;
        }
        let dec = self.config.decimals;
        let prefix = self.prefix().to_string();
        let paint = paint::resolve(
            brush,
            brush_transform,
            &mut self.defs,
            &prefix,
            dec,
            &mut self.warnings,
        );
        // Held back so a stroke of the same path can become one element
        // rather than a second one stacked on top.
        self.pending = Some(PendingFill {
            path: path.clone(),
            transform,
            rule,
            paint,
            pick: pick_id,
        });
    }

    fn stroke(
        &mut self,
        stroke: &crate::stroke::Stroke,
        transform: Affine,
        brush: &Brush,
        brush_transform: Option<Affine>,
        path: &Path,
        pick_id: PickId,
    ) {
        if !self.block_open() {
            self.flush_block();
        }
        let dec = self.config.decimals;
        let prefix = self.prefix().to_string();
        // A fill of this very path is waiting: paint both on one
        // element. Two stacked paths would be two objects to an editor,
        // so nudging the fill would leave its border behind.
        let merged = matches!(
            &self.pending,
            Some(p) if p.transform == transform && p.path == *path
        );
        let pick_on = self.config.pick_ids;
        // Built into a local so it can land in the body or, while a
        // text block is open, in that block's prelude.
        let mut el = String::new();
        if merged {
            let p = self.pending.take().expect("checked");
            let paint = paint::resolve(
                brush,
                brush_transform,
                &mut self.defs,
                &prefix,
                dec,
                &mut self.warnings,
            );
            el.push_str("<path d=\"");
            el.push_str(&path::to_d(&p.path, dec));
            el.push('"');
            write_fill_attrs_to(&mut el, &p.paint, p.rule, dec);
            write_stroke_attrs_to(&mut el, stroke, &paint, dec, &mut self.warnings);
            transform_attr(&mut el, transform, dec);
            write_pick_to(&mut el, p.pick, pick_on);
            el.push_str("/>");
        } else {
            self.flush_pending();
            if path.elements().is_empty() {
                return;
            }
            let paint = paint::resolve(
                brush,
                brush_transform,
                &mut self.defs,
                &prefix,
                dec,
                &mut self.warnings,
            );
            el.push_str("<path d=\"");
            el.push_str(&path::to_d(path, dec));
            el.push_str("\" fill=\"none\"");
            write_stroke_attrs_to(&mut el, stroke, &paint, dec, &mut self.warnings);
            transform_attr(&mut el, transform, dec);
            write_pick_to(&mut el, pick_id, pick_on);
            el.push_str("/>");
        }
        if self.block_open() {
            self.block_prelude.push_str(&el);
        } else {
            self.body.push_str(&el);
        }
    }

    fn draw_image(
        &mut self,
        image: &Image,
        transform: Affine,
        sampling: Sampling,
        alpha: f32,
        _pick_id: PickId,
    ) {
        self.flush_block();
        self.flush_pending();
        let dec = self.config.decimals;
        let prefix = self.prefix().to_string();
        image::emit(
            &mut self.body,
            image,
            transform,
            sampling,
            alpha,
            &mut self.defs,
            &prefix,
            dec,
            &mut self.warnings,
        );
    }

    fn draw_glyphs(&mut self, run: &GlyphRun<'_>, pick_id: PickId) {
        self.flush_pending();
        let dec = self.config.decimals;
        let prefix = self.prefix().to_string();
        // No source text, or outlines asked for: draw the contours.
        // A run that can be neither is dropped with a warning rather
        // than silently, which is the one outcome a rendering backend
        // must never have.
        if run.source.is_none() || self.config.text == TextMode::Outline {
            self.flush_block();
            self.write_glyph_outlines(run, pick_id);
            return;
        }
        let Some(pending) = text::prepare(
            run,
            pick_id,
            &mut self.defs,
            &prefix,
            dec,
            &mut self.warnings,
        ) else {
            return;
        };
        if let Some(src) = run.source {
            self.fonts.note(src.font, run.font);
        }
        // An `@font-face` only takes effect if the element references
        // the family it declares, so an embedded document names the
        // resolved face ahead of the chain that was asked for.
        let mut pending = pending;
        if self.config.embed_fonts {
            if let Some(name) = fonts::resolved_family(run.font) {
                pending
                    .spec
                    .families
                    .insert(0, crate::style_vocab::FontFamilyEntry::Named(name));
            }
            if !fonts::is_embeddable(run.font) {
                self.warnings.note(SvgWarning::FontNotEmbeddable);
            }
        }
        // A run of a different block, or under a different transform,
        // starts a new element.
        if !self.block.accepts(pending.group, pending.transform) {
            self.flush_block();
        }
        self.block.push(pending);
    }

    fn draw_mesh(&mut self, mesh: &Mesh, transform: Affine, pick_id: PickId) {
        self.flush_block();
        self.flush_pending();
        // Shared with the rasterising backends: one fill per triangle,
        // with adjacent uniform triangles already merged. Working in
        // this crate's own types means it needs no GPU and no
        // SVG-specific handling.
        crate::backend::mesh::decompose(mesh, transform, pick_id, self);
    }

    fn push_layer(&mut self, blend: BlendMode, alpha: f32, transform: Affine, clip: &Path) {
        self.flush_block();
        self.flush_pending();
        let dec = self.config.decimals;
        let prefix = self.prefix().to_string();
        self.body.push_str("<g");
        if !clip.elements().is_empty() {
            // The transform applies to the clip, not to the layer's
            // contents, so it is baked into the geometry rather than
            // put on the group. Baking is exact for an affine, and it
            // canonicalizes the path so two identically-clipped panels
            // share one definition.
            let baked: Path = if is_identity(transform) {
                clip.clone()
            } else {
                transform * clip.clone()
            };
            let body = format!(
                "<clipPath clipPathUnits=\"userSpaceOnUse\"><path d=\"{}\"/></clipPath>",
                path::to_d(&baked, dec)
            );
            let id = self.defs.intern(DefKind::Clip, &body, &prefix);
            self.body.push_str(" clip-path=\"url(#");
            self.body.push_str(&id);
            self.body.push_str(")\"");
        }
        if alpha != 1.0 {
            self.body.push_str(" opacity=\"");
            num(&mut self.body, alpha as f64, dec.max(3));
            self.body.push('"');
        }
        if blend.compose != Compose::SrcOver {
            // Porter-Duff against the backdrop needs `BackgroundImage`,
            // which no renderer ever implemented and Filter Effects
            // removed. Restructuring the tree so the backdrop is a
            // filter input is the way through, and is not this.
            self.warnings.note(SvgWarning::UnsupportedCompose);
        }
        if blend.mix != Mix::Normal {
            self.body
                .push_str(" style=\"isolation:isolate;mix-blend-mode:");
            self.body.push_str(mix_keyword(blend.mix));
            self.body.push('"');
        } else {
            self.body.push_str(" style=\"isolation:isolate\"");
        }
        self.body.push('>');
        self.groups.push(GroupKind::Layer);
    }

    fn pop_layer(&mut self) {
        self.flush_block();
        self.flush_pending();
        self.close_group(GroupKind::Layer, SvgWarning::UnbalancedLayers);
    }

    fn push_pick_scope(&mut self, scope: &PickScope) {
        if !self.config.pick_ids {
            return;
        }
        self.flush_block();
        self.flush_pending();
        self.body.push_str("<g data-pick-kind=\"");
        escape_attr(&mut self.body, scope.kind());
        self.body.push('"');
        if let Some(name) = scope.name() {
            self.body.push_str(" data-pick-name=\"");
            escape_attr(&mut self.body, name);
            self.body.push('"');
        }
        if let Some(index) = scope.index() {
            self.body.push_str(" data-pick-index=\"");
            self.body.push_str(&index.to_string());
            self.body.push('"');
        }
        self.body.push('>');
        self.groups.push(GroupKind::Scope);
    }

    fn pop_pick_scope(&mut self) {
        if !self.config.pick_ids {
            return;
        }
        self.flush_block();
        self.flush_pending();
        self.close_group(GroupKind::Scope, SvgWarning::UnbalancedScopes);
    }
}

/// What an open `<g>` was opened for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GroupKind {
    Layer,
    Scope,
}

/// CSS `mix-blend-mode` keyword for a mix function.
///
/// The enum is exactly the CSS keyword set, so this is a spelling
/// change and nothing more.
fn mix_keyword(mix: Mix) -> &'static str {
    match mix {
        Mix::Normal => "normal",
        Mix::Multiply => "multiply",
        Mix::Screen => "screen",
        Mix::Overlay => "overlay",
        Mix::Darken => "darken",
        Mix::Lighten => "lighten",
        Mix::ColorDodge => "color-dodge",
        Mix::ColorBurn => "color-burn",
        Mix::HardLight => "hard-light",
        Mix::SoftLight => "soft-light",
        Mix::Difference => "difference",
        Mix::Exclusion => "exclusion",
        Mix::Hue => "hue",
        Mix::Saturation => "saturation",
        Mix::Color => "color",
        Mix::Luminosity => "luminosity",
    }
}

/// Serialize `scene` as an SVG document.
///
/// Infallible: building a string cannot fail, and a scene expressing
/// something SVG cannot produces [`SvgScene::warnings`] rather than an
/// error.
pub fn encode_svg(scene: &SvgScene) -> String {
    scene.to_svg()
}

/// Write `scene` to `w`.
pub fn write_svg_to<W: io::Write>(mut w: W, scene: &SvgScene) -> io::Result<()> {
    w.write_all(scene.to_svg().as_bytes())
}

/// Write `scene` to `path`.
pub fn write_svg(path: impl AsRef<std::path::Path>, scene: &SvgScene) -> io::Result<()> {
    let file = std::fs::File::create(path)?;
    write_svg_to(io::BufWriter::new(file), scene)
}

/// Append `fill` and its companion attributes.
fn write_fill_attrs_to(out: &mut String, paint: &paint::Paint, rule: FillRule, decimals: u8) {
    out.push_str(" fill=\"");
    out.push_str(&paint.value);
    out.push('"');
    if let Some(a) = paint.opacity {
        out.push_str(" fill-opacity=\"");
        num(out, a as f64, decimals.max(3));
        out.push('"');
    }
    if rule == FillRule::EvenOdd {
        out.push_str(" fill-rule=\"evenodd\"");
    }
}

/// Append the picking attributes, when they are switched on.
fn write_pick_to(out: &mut String, pick: PickId, on: bool) {
    if !on {
        return;
    }
    match pick {
        // Reproduces "items beneath remain hittable through this
        // primitive" under `elementFromPoint`. Without it a gridline
        // over a mark swallows the hit.
        PickId::Skip => out.push_str(" pointer-events=\"none\""),
        PickId::Block => out.push_str(" data-pick-id=\"0\""),
        PickId::Id(n) => {
            out.push_str(" data-pick-id=\"");
            out.push_str(&n.to_string());
            out.push('"');
        }
    }
}

/// Write a fill that no stroke joined.
fn write_pending_fill(out: &mut String, p: &PendingFill, decimals: u8, pick_ids: bool) {
    let d = path::to_d(&p.path, decimals);
    if d.is_empty() {
        return;
    }
    out.push_str("<path d=\"");
    out.push_str(&d);
    out.push('"');
    write_fill_attrs_to(out, &p.paint, p.rule, decimals);
    transform_attr(out, p.transform, decimals);
    write_pick_to(out, p.pick, pick_ids);
    out.push_str("/>");
}

/// The rectangle `path` describes, when it is an axis-aligned one drawn
/// under `transform`.
///
/// Only used to recognize a decoration rule the text emitter predicted.
/// Returning `None` means "draw it normally", so a false negative costs
/// a duplicated underline and never a missing graphic.
fn axis_aligned_rect(path: &Path, transform: Affine) -> Option<crate::geometry::Rect> {
    use crate::geometry::Point;
    use crate::path::PathEl;
    if !is_identity(transform) && {
        let c = transform.as_coeffs();
        c[1] != 0.0 || c[2] != 0.0
    } {
        // A rotation or skew: the rule is no longer axis-aligned and
        // the prediction was made in the pre-transform frame anyway.
        return None;
    }
    let mut pts: Vec<Point> = Vec::with_capacity(5);
    for el in path.elements() {
        match el {
            PathEl::MoveTo(p) | PathEl::LineTo(p) => pts.push(*p),
            PathEl::ClosePath => {}
            _ => return None,
        }
    }
    if pts.len() < 4 || pts.len() > 5 {
        return None;
    }
    if pts.len() == 5 && (pts[4] - pts[0]).hypot() > 1e-6 {
        return None;
    }
    let xs: Vec<f64> = pts.iter().take(4).map(|p| p.x).collect();
    let ys: Vec<f64> = pts.iter().take(4).map(|p| p.y).collect();
    let (x0, x1) = (
        xs.iter().cloned().fold(f64::INFINITY, f64::min),
        xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
    );
    let (y0, y1) = (
        ys.iter().cloned().fold(f64::INFINITY, f64::min),
        ys.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
    );
    // Every corner has to sit on the bounding box for this to be a rect
    // rather than some other quadrilateral.
    let on_edge = pts
        .iter()
        .take(4)
        .all(|p| (p.x - x0).abs() < 1e-6 || (p.x - x1).abs() < 1e-6)
        && pts
            .iter()
            .take(4)
            .all(|p| (p.y - y0).abs() < 1e-6 || (p.y - y1).abs() < 1e-6);
    if !on_edge {
        return None;
    }
    Some(crate::geometry::Rect::new(x0, y0, x1, y1))
}

/// Append the stroke attributes for `stroke`, omitting SVG defaults.
fn write_stroke_attrs_to(
    out: &mut String,
    stroke: &crate::stroke::Stroke,
    paint: &paint::Paint,
    decimals: u8,
    warnings: &mut Warnings,
) {
    use crate::stroke::{Cap, Join};
    let dec = decimals;
    out.push_str(" stroke=\"");
    out.push_str(&paint.value);
    out.push('"');
    if let Some(a) = paint.opacity {
        out.push_str(" stroke-opacity=\"");
        num(out, a as f64, dec.max(3));
        out.push('"');
    }
    if stroke.width != 1.0 {
        out.push_str(" stroke-width=\"");
        num(out, stroke.width, dec);
        out.push('"');
    }
    if stroke.start_cap != stroke.end_cap {
        warnings.note(SvgWarning::AsymmetricCaps);
    }
    let cap = match stroke.start_cap {
        Cap::Butt => None,
        Cap::Square => Some("square"),
        Cap::Round => Some("round"),
    };
    if let Some(c) = cap {
        out.push_str(" stroke-linecap=\"");
        out.push_str(c);
        out.push('"');
    }
    let join = match stroke.join {
        Join::Miter => None,
        Join::Round => Some("round"),
        Join::Bevel => Some("bevel"),
    };
    if let Some(j) = join {
        out.push_str(" stroke-linejoin=\"");
        out.push_str(j);
        out.push('"');
    }
    if stroke.join == Join::Miter && stroke.miter_limit != 4.0 {
        out.push_str(" stroke-miterlimit=\"");
        num(out, stroke.miter_limit, dec);
        out.push('"');
    }
    if !stroke.dash_pattern.is_empty() && stroke.dash_pattern.iter().sum::<f64>() > 0.0 {
        out.push_str(" stroke-dasharray=\"");
        for (i, v) in stroke.dash_pattern.iter().enumerate() {
            if i > 0 {
                out.push(' ');
            }
            num(out, *v, dec);
        }
        out.push('"');
        if stroke.dash_offset != 0.0 {
            out.push_str(" stroke-dashoffset=\"");
            num(out, stroke.dash_offset, dec);
            out.push('"');
        }
    }
}
