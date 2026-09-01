//! PDF output: a `SceneBuilder` that emits a PDF file instead of
//! pixels.
//!
//! Like [`svg`](crate::backend::svg) this implements [`SceneBuilder`]
//! and not [`crate::Renderer`] — `Renderer`'s contract is to fill a
//! buffer of RGBA8, and there are no pixels here.
//!
//! Where the SVG backend aims at output someone can *edit*, this aims
//! at output that looks the same everywhere: a figure going into a
//! paper, a print pipeline or an archive. Nothing may depend on what
//! the reader has installed, so the glyphs a plot draws are embedded,
//! always — as a synthesized subset font a few kB in size rather than
//! the megabyte face they came from. See `CLAUDE.md` beside this file
//! for the design, and `font.rs` for why the font is synthesized rather
//! than sliced out of the original.
//!
//! The emitter streams: draw calls append operators to the open content
//! stream and the resources they need accumulate beside them, with the
//! file assembled at write time. Nothing is deferred — glyph outlines
//! are extracted, color-glyph paint graphs are walked and image pixels
//! are converted during the `&mut self` draw calls, which is what lets
//! [`encode_pdf`] take `&PdfScene` and produce the same bytes twice.

mod color;
mod content;
mod font;
mod image;
mod mesh;
mod paint;
mod res;
mod sfnt;
mod writer;

use std::io;

use crate::blend::{BlendMode, Compose, Mix};
use crate::brush::{Brush, Image, Sampling};
use crate::color::Color;
use crate::geometry::{Affine, Rect, Size};
use crate::mesh::Mesh;
use crate::path::{FillRule, Path};
use crate::pick::PickId;
use crate::scene::{GlyphRun, SceneBuilder};

use content::{LayerFrame, Target};
use res::{ResKind, Resources, RES_REF};
use writer::{cm, is_identity, num, pdf_string, Objects};

/// Something the scene expressed that PDF cannot, or cannot yet.
///
/// Reported rather than returned: a scene is still drawable when one
/// feature degrades, and refusing to write the other 99% of the picture
/// would help nobody. Mirrors
/// [`SvgWarning`](crate::backend::svg::SvgWarning).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PdfWarning {
    /// A sweep gradient was flattened to a solid color. PDF has no
    /// conic shading short of a PostScript calculator function.
    SweepGradient,
    /// A gradient asked to repeat or reflect; PDF shadings only pad.
    UnsupportedExtend,
    /// A layer, or a color glyph's paint graph, asked for a compositing
    /// operator other than source-over. PDF's imaging model fixes
    /// source-over; `/BM` selects a blend function, not a Porter-Duff
    /// operator.
    UnsupportedCompose,
    /// A stroke set different start and end caps. PDF has one line-cap
    /// setting.
    AsymmetricCaps,
    /// An image brush was used as a fill or stroke paint.
    ImageBrushUnsupported,
    /// An image's pixels were in a layout this backend cannot embed.
    UnembeddableImage,
    /// A color glyph's bitmap strike is PNG-compressed, which this
    /// build cannot decode.
    MissingPngFeature,
    /// A glyph had neither outlines nor a color form this backend can
    /// render, and did not appear.
    GlyphNotDrawable,
    /// A coordinate was not finite and was written as zero.
    NonFiniteCoordinate,
    /// More layers were popped than were pushed.
    UnbalancedLayers,
}

/// Warnings collected during a frame, deduplicated by variant.
///
/// Deduplicated because a twenty-thousand-triangle mesh full of sweep
/// brushes would otherwise report the same fact twenty thousand times.
#[derive(Default, Clone)]
pub(crate) struct Warnings(Vec<PdfWarning>);

impl Warnings {
    /// Record `w` if it has not been seen this frame.
    pub(crate) fn note(&mut self, w: PdfWarning) {
        if !self.0.contains(&w) {
            self.0.push(w);
        }
    }

    /// True when `w` was recorded.
    #[cfg(test)]
    pub(crate) fn contains(&self, w: &PdfWarning) -> bool {
        self.0.contains(w)
    }
}

/// Emission options.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct PdfConfig {
    /// Painted behind everything as a full-page rect. `None` emits no
    /// rect at all, leaving the page transparent where nothing was
    /// drawn.
    pub background: Option<Color>,
    /// Decimal places for coordinates. The linear part of a matrix
    /// always uses more; see `writer`.
    pub decimals: u8,
    /// Compress streams with `/Filter /FlateDecode`.
    pub compress: bool,
    /// Emit `/Link` annotations for runs carrying a link destination.
    pub links: bool,
}

impl PdfConfig {
    /// Default options.
    pub fn new() -> Self {
        Self::default()
    }

    /// Paint `color` behind everything as a full-page rect.
    pub fn background(mut self, color: Option<Color>) -> Self {
        self.background = color;
        self
    }

    /// Set the decimal places coordinates are written to.
    pub fn decimals(mut self, decimals: u8) -> Self {
        self.decimals = decimals;
        self
    }

    /// Compress streams, or leave them readable.
    pub fn compress(mut self, on: bool) -> Self {
        self.compress = on;
        self
    }

    /// Emit link annotations for linked text.
    pub fn links(mut self, on: bool) -> Self {
        self.links = on;
        self
    }
}

impl Default for PdfConfig {
    fn default() -> Self {
        Self {
            background: None,
            decimals: 3,
            compress: true,
            links: true,
        }
    }
}

/// One pending fill, held back so a stroke of the same path can join
/// it.
///
/// The path arrives already serialized: it has to be written either
/// way, doing it at record time avoids cloning the geometry, and it is
/// what lets a non-finite coordinate be reported while the scene is
/// still `&mut`.
#[derive(Clone)]
struct PendingFill {
    body: String,
    transform: Affine,
    rule: FillRule,
    paint: paint::Paint,
}

/// One link destination and the area that reaches it.
#[derive(Clone)]
struct Annot {
    url: String,
    /// The union of every run's box, in scene space.
    rect: Rect,
    /// The text block the runs belonged to, so two links to one URL in
    /// different sentences stay two annotations.
    group: Option<crate::scene::TextGroup>,
}

/// A scene that emits PDF.
///
/// Build it, draw into it exactly as into any other [`SceneBuilder`],
/// then hand it to [`write_pdf`] or [`encode_pdf`].
pub struct PdfScene {
    size: Size,
    dpi: f64,
    config: PdfConfig,
    /// Content streams under construction. `targets[0]` is the page;
    /// a transparency group pushes another.
    targets: Vec<Target>,
    /// One entry per open layer, innermost last.
    frames: Vec<LayerFrame>,
    res: Resources,
    fonts: font::FontRegistry,
    annots: Vec<Annot>,
    pending: Option<PendingFill>,
    warnings: Warnings,
}

impl PdfScene {
    /// A page covering `size`, rendered for `dpi`.
    ///
    /// Both are what the caller is about to pass to
    /// `PlotComposition::render`, so the two lines agree by
    /// construction. `size` is in pixels; the page's `/MediaBox` is
    /// that size converted to points.
    pub fn new(size: Size, dpi: f64) -> Self {
        Self::with_config(size, dpi, PdfConfig::default())
    }

    /// As [`Self::new`], with emission options.
    pub fn with_config(size: Size, dpi: f64, config: PdfConfig) -> Self {
        let base = base_flip(size, dpi);
        Self {
            size,
            dpi,
            config,
            targets: vec![Target::new(base)],
            frames: Vec::new(),
            res: Resources::default(),
            fonts: font::FontRegistry::default(),
            annots: Vec::new(),
            pending: None,
            warnings: Warnings::default(),
        }
    }

    /// Resize between frames. Does not discard drawn content; call
    /// [`SceneBuilder::clear`] for that.
    pub fn set_size(&mut self, size: Size, dpi: f64) {
        self.size = size;
        self.dpi = dpi;
        let base = base_flip(size, dpi);
        if let Some(page) = self.targets.first_mut() {
            page.pattern_base = base;
        }
    }

    /// The page size, in pixels.
    pub fn size(&self) -> Size {
        self.size
    }

    /// Everything the scene expressed that PDF could not, deduplicated.
    pub fn warnings(&self) -> &[PdfWarning] {
        &self.warnings.0
    }

    /// The emission options in force.
    pub fn config(&self) -> &PdfConfig {
        &self.config
    }

    /// The stream draw calls currently append to.
    fn out(&mut self) -> &mut String {
        &mut self
            .targets
            .last_mut()
            .expect("the page target is never popped")
            .content
    }

    /// The default space of the open stream, which a pattern `/Matrix`
    /// resolves against.
    fn pattern_base(&self) -> Affine {
        self.targets
            .last()
            .expect("the page target is never popped")
            .pattern_base
    }

    /// Resolve a brush against the open stream.
    fn paint(
        &mut self,
        brush: &Brush,
        brush_transform: Option<Affine>,
        transform: Affine,
    ) -> paint::Paint {
        let space = paint::PaintSpace {
            transform,
            pattern_base: self.pattern_base(),
            page_base: base_flip(self.size, self.dpi),
            page: self.page_box(),
        };
        let dec = self.config.decimals;
        paint::resolve(
            brush,
            brush_transform,
            space,
            &mut self.res,
            dec,
            &mut self.warnings,
        )
    }

    /// The page rectangle in default user space — points, y-up.
    fn page_box(&self) -> Rect {
        let s = 72.0 / self.dpi;
        Rect::new(0.0, 0.0, self.size.width * s, self.size.height * s)
    }

    /// Open a primitive's `q` block, placing `gs` where a soft mask
    /// needs it and leaving the CTM at `transform` either way.
    ///
    /// A soft mask is evaluated in the coordinate system in force when
    /// `gs` runs, so a masked primitive resets to default user space
    /// first and restores afterwards. Two extra operators, and only on
    /// the primitives that carry a mask.
    fn open_block(&self, transform: Affine, gs: &str, masked: bool) -> String {
        let dec = self.config.decimals;
        let mut ops = String::from("q\n");
        if masked {
            let base = base_flip(self.size, self.dpi);
            cm(&mut ops, base.inverse(), dec);
            ops.push_str(gs);
            cm(&mut ops, base * transform, dec);
        } else {
            cm(&mut ops, transform, dec);
            ops.push_str(gs);
        }
        ops
    }

    /// Intern an `/ExtGState` and return the `gs` operator, or the
    /// empty string when the primitive needs no graphics state.
    fn gs_op(
        &mut self,
        fill_alpha: Option<f32>,
        stroke_alpha: Option<f32>,
        blend: Option<Mix>,
        mask: Option<&str>,
    ) -> String {
        match paint::ext_gstate(fill_alpha, stroke_alpha, blend, mask) {
            Some(body) => {
                let name = self.res.intern(ResKind::ExtGState, &body);
                format!("/{name} gs\n")
            }
            None => String::new(),
        }
    }

    /// Serialize `path`, reporting a coordinate that had to be written
    /// as zero.
    fn path_body(&mut self, path: &Path) -> Option<String> {
        let dec = self.config.decimals;
        let mut body = String::new();
        let mut non_finite = false;
        let wrote = content::write_path(&mut body, path, dec, &mut non_finite);
        if non_finite {
            self.warnings.note(PdfWarning::NonFiniteCoordinate);
        }
        wrote.then_some(body)
    }

    /// Flush any fill waiting for a stroke to join it.
    fn flush_pending(&mut self) {
        let Some(p) = self.pending.take() else { return };
        let gs = self.gs_op(p.paint.alpha, None, None, p.paint.mask.as_deref());
        let masked = p.paint.mask.is_some();
        let wrap = !gs.is_empty() || !is_identity(p.transform);
        let mut ops = if wrap {
            self.open_block(p.transform, &gs, masked)
        } else {
            String::new()
        };
        ops.push_str(&p.paint.fill_ops);
        ops.push_str(&p.body);
        ops.push_str(content::fill_op(p.rule));
        if wrap {
            ops.push_str("Q\n");
        }
        self.out().push_str(&ops);
    }

    /// Record a link covering `rect` in scene space.
    fn note_link(&mut self, url: &str, rect: Rect, group: Option<crate::scene::TextGroup>) {
        // A markdown link spanning a bold word arrives as several runs
        // sharing one group, and one annotation over the union is what
        // a reader expects.
        if let Some(a) = self
            .annots
            .iter_mut()
            .find(|a| a.url == url && a.group == group)
        {
            a.rect = a.rect.union(rect);
            return;
        }
        self.annots.push(Annot {
            url: url.to_string(),
            rect,
            group,
        });
    }

    /// Close an open layer, appending to whichever stream it belongs
    /// to.
    ///
    /// Shared by [`SceneBuilder::pop_layer`] and the write-time close
    /// of layers a scene left open, which is why it works on borrowed
    /// state rather than on `self`.
    fn close_layer(
        frame: LayerFrame,
        targets: &mut Vec<Target>,
        res: &mut Resources,
        size: Size,
        decimals: u8,
    ) {
        match frame {
            LayerFrame::Simple => {
                if let Some(t) = targets.last_mut() {
                    t.content.push_str("Q\n");
                }
            }
            LayerFrame::Group { blend, alpha } => {
                let mut group = match targets.pop() {
                    Some(t) => t,
                    None => return,
                };
                group.content.push_str("Q\n");
                let mut dict = String::from(
                    "/Type /XObject /Subtype /Form /Group \
                     << /S /Transparency /CS /DeviceRGB /I true /K false >> /BBox [0 0 ",
                );
                num(&mut dict, size.width, decimals);
                dict.push(' ');
                num(&mut dict, size.height, decimals);
                dict.push_str("] /Resources ");
                dict.push_str(RES_REF);
                let payload = group.content.into_bytes();
                let key = format!("form:{dict}|{}", fnv1a(&payload));
                let name = res.intern_stream(ResKind::XObject, &key, &dict, payload, None);
                let alpha = (alpha < 1.0).then_some(alpha);
                let gs = paint::ext_gstate(alpha, alpha, Some(blend.mix), None)
                    .map(|body| {
                        let n = res.intern(ResKind::ExtGState, &body);
                        format!("/{n} gs\n")
                    })
                    .unwrap_or_default();
                if let Some(parent) = targets.last_mut() {
                    parent.content.push_str("q\n");
                    parent.content.push_str(&gs);
                    parent.content.push_str(&format!("/{name} Do\nQ\n"));
                }
            }
        }
    }

    /// Serialize the whole document.
    ///
    /// Works on clones throughout: the scene is `&self`, and a layer
    /// left open or a fill still pending has to be closed without
    /// mutating it, so that serializing twice gives the same bytes
    /// twice.
    fn to_pdf(&self) -> Vec<u8> {
        let dec = self.config.decimals;
        let mut res = self.res.clone();
        let mut targets = self.targets.clone();
        let mut frames = self.frames.clone();

        if self.pending.is_some() {
            let mut scratch = PdfScene {
                size: self.size,
                dpi: self.dpi,
                config: self.config.clone(),
                targets: std::mem::take(&mut targets),
                frames: Vec::new(),
                res,
                fonts: font::FontRegistry::default(),
                annots: Vec::new(),
                pending: self.pending.clone(),
                warnings: Warnings::default(),
            };
            scratch.flush_pending();
            targets = scratch.targets;
            res = scratch.res;
        }

        // A scene may leave layers open; closing them keeps the file
        // structurally valid, which matters more than the warning.
        while let Some(frame) = frames.pop() {
            Self::close_layer(frame, &mut targets, &mut res, self.size, dec);
        }

        let base = base_flip(self.size, self.dpi);
        let mut content = String::with_capacity(targets[0].content.len() + 256);
        content.push_str("q\n");
        cm(&mut content, base, dec);
        if let Some(bg) = self.config.background {
            let p = paint::solid(bg);
            let gs = paint::ext_gstate(p.alpha, None, None, None).map(|body| {
                let n = res.intern(ResKind::ExtGState, &body);
                format!("/{n} gs\n")
            });
            if let Some(gs) = &gs {
                content.push_str("q\n");
                content.push_str(gs);
            }
            content.push_str(&p.fill_ops);
            content::write_rect(
                &mut content,
                0.0,
                0.0,
                self.size.width,
                self.size.height,
                dec,
            );
            content.push_str("f\n");
            if gs.is_some() {
                content.push_str("Q\n");
            }
        }
        content.push_str(&targets[0].content);
        content.push_str("Q\n");

        let mut objects = Objects::new();
        let catalog = objects.alloc();
        let pages = objects.alloc();
        let res_ref = objects.alloc();
        let fonts_dict = self.fonts.write(&mut objects, self.config.compress);
        let res_body = res.write(&mut objects, self.config.compress, res_ref, &fonts_dict);
        objects.object(res_ref, &format!("<< {res_body}>>"));

        let annot_refs: Vec<String> = if self.config.links {
            self.annots
                .iter()
                .map(|a| {
                    let r = objects.alloc();
                    objects.object(r, &self.annot_body(a, base, dec));
                    r.to_ref_string()
                })
                .collect()
        } else {
            Vec::new()
        };

        let contents = objects.alloc();
        objects.stream(contents, "", content.as_bytes(), self.config.compress);

        let s = 72.0 / self.dpi;
        let mut page = String::from("<< /Type /Page /Parent ");
        page.push_str(&pages.to_ref_string());
        page.push_str(" /MediaBox [0 0 ");
        num(&mut page, self.size.width * s, dec);
        page.push(' ');
        num(&mut page, self.size.height * s, dec);
        page.push_str("] /Resources ");
        page.push_str(&res_ref.to_ref_string());
        page.push_str(" /Contents ");
        page.push_str(&contents.to_ref_string());
        if !annot_refs.is_empty() {
            page.push_str(" /Annots [");
            page.push_str(&annot_refs.join(" "));
            page.push(']');
        }
        page.push_str(" >>");
        let page_ref = objects.alloc();
        objects.object(page_ref, &page);

        objects.object(
            pages,
            &format!(
                "<< /Type /Pages /Kids [{}] /Count 1 >>",
                page_ref.to_ref_string()
            ),
        );
        objects.object(
            catalog,
            &format!("<< /Type /Catalog /Pages {} >>", pages.to_ref_string()),
        );
        objects.finish(catalog)
    }

    /// One `/Link` annotation's dictionary.
    ///
    /// `/Rect` is in the page's own default space — points, y-up — so
    /// the rectangle's corners go through the page flip on the way out.
    fn annot_body(&self, a: &Annot, base: Affine, decimals: u8) -> String {
        let corners = [
            base * crate::geometry::Point::new(a.rect.x0, a.rect.y0),
            base * crate::geometry::Point::new(a.rect.x1, a.rect.y1),
        ];
        let (x0, x1) = (
            corners[0].x.min(corners[1].x),
            corners[0].x.max(corners[1].x),
        );
        let (y0, y1) = (
            corners[0].y.min(corners[1].y),
            corners[0].y.max(corners[1].y),
        );
        let mut body = String::from("<< /Type /Annot /Subtype /Link /Rect [");
        for v in [x0, y0, x1, y1] {
            num(&mut body, v, decimals);
            body.push(' ');
        }
        // Suppresses the ugly default box a viewer would otherwise draw.
        body.push_str("] /Border [0 0 0] /A << /S /URI /URI ");
        pdf_string(&mut body, &a.url);
        body.push_str(" >> >>");
        body
    }
}

impl SceneBuilder for PdfScene {
    fn clear(&mut self) {
        let base = base_flip(self.size, self.dpi);
        self.targets = vec![Target::new(base)];
        self.frames.clear();
        self.res.clear();
        self.fonts.clear();
        self.annots.clear();
        self.pending = None;
        self.warnings.0.clear();
    }

    fn fill(
        &mut self,
        rule: FillRule,
        transform: Affine,
        brush: &Brush,
        brush_transform: Option<Affine>,
        path: &Path,
        _pick_id: PickId,
    ) {
        self.flush_pending();
        let Some(body) = self.path_body(path) else {
            return;
        };
        let paint = self.paint(brush, brush_transform, transform);
        // Held back so a stroke of the same path becomes one `B`
        // operator rather than a second copy of the path data. A
        // scatter plot's marker is a fill and a stroke of one circle,
        // so this halves the path bytes for every row.
        self.pending = Some(PendingFill {
            body,
            transform,
            rule,
            paint,
        });
    }

    fn stroke(
        &mut self,
        stroke: &crate::stroke::Stroke,
        transform: Affine,
        brush: &Brush,
        brush_transform: Option<Affine>,
        path: &Path,
        _pick_id: PickId,
    ) {
        let Some(body) = self.path_body(path) else {
            self.flush_pending();
            return;
        };
        let dec = self.config.decimals;
        let paint = self.paint(brush, brush_transform, transform);
        // Serialized geometry under the same transform is the same
        // path, which is the question the merge actually asks. A soft
        // mask blocks it either way: an `/SMask` in an ExtGState
        // applies to every painting operator that follows, so one
        // graphics state cannot carry the fill's mask and leave the
        // stroke unmasked.
        let merged = matches!(
            &self.pending,
            Some(p) if p.transform == transform
                && p.body == body
                && p.paint.mask.is_none()
                && paint.mask.is_none()
        );
        let (fill_ops, rule, fill_alpha) = if merged {
            let p = self.pending.take().expect("checked");
            (Some(p.paint.fill_ops), p.rule, p.paint.alpha)
        } else {
            self.flush_pending();
            (None, FillRule::NonZero, None)
        };
        let mut state = String::new();
        if content::write_stroke_state(&mut state, stroke, dec) {
            self.warnings.note(PdfWarning::AsymmetricCaps);
        }
        let gs = self.gs_op(fill_alpha, paint.alpha, None, paint.mask.as_deref());
        let mut ops = self.open_block(transform, &gs, paint.mask.is_some());
        ops.push_str(&state);
        if let Some(f) = &fill_ops {
            ops.push_str(f);
        }
        ops.push_str(&paint.stroke_ops);
        ops.push_str(&body);
        ops.push_str(if fill_ops.is_some() {
            content::fill_stroke_op(rule)
        } else {
            "S\n"
        });
        ops.push_str("Q\n");
        self.out().push_str(&ops);
    }

    fn draw_image(
        &mut self,
        img: &Image,
        transform: Affine,
        sampling: Sampling,
        alpha: f32,
        _pick_id: PickId,
    ) {
        self.flush_pending();
        let Some(name) = image::intern(img, sampling, &mut self.res, &mut self.warnings) else {
            return;
        };
        let dec = self.config.decimals;
        let gs = self.gs_op((alpha < 1.0).then_some(alpha), None, None, None);
        let placement = transform * image::unit_to_pixels(img);
        let mut ops = String::from("q\n");
        ops.push_str(&gs);
        content::write_placement(&mut ops, placement, dec);
        ops.push_str(&format!("/{name} Do\nQ\n"));
        self.out().push_str(&ops);
    }

    fn draw_glyphs(&mut self, run: &GlyphRun<'_>, _pick_id: PickId) {
        self.flush_pending();
        font::emit_run(self, run);
    }

    fn draw_mesh(&mut self, m: &Mesh, transform: Affine, _pick_id: PickId) {
        self.flush_pending();
        // Deliberately not `backend::mesh::decompose`: a
        // `ShadingType 4` shading interpolates adjacent triangles
        // inside one object with no antialiased edge between them, and
        // a three-color triangle is exactly what Gouraud shading is.
        let dec = self.config.decimals;
        let Some(name) = mesh::intern(m, &mut self.res) else {
            return;
        };
        let (alpha, mask) = mesh::alpha(
            m,
            base_flip(self.size, self.dpi) * transform,
            self.page_box(),
            &mut self.res,
            dec,
        );
        let gs = self.gs_op(alpha, None, None, mask.as_deref());
        let mut ops = self.open_block(transform, &gs, mask.is_some());
        ops.push_str(&format!("/{name} sh\nQ\n"));
        self.out().push_str(&ops);
    }

    fn push_layer(&mut self, blend: BlendMode, alpha: f32, transform: Affine, clip: &Path) {
        self.flush_pending();
        if blend.compose != Compose::SrcOver {
            // PDF's imaging model fixes source-over compositing; `/BM`
            // selects a blend function, not a Porter-Duff operator.
            self.warnings.note(PdfWarning::UnsupportedCompose);
        }
        let group = alpha != 1.0 || blend.mix != Mix::Normal;
        if group {
            // A group composites the layer's contents together first
            // and applies the alpha once, which is what `push_layer`
            // means; `/ca` on each primitive would let two overlapping
            // shapes inside show through each other.
            self.targets.push(Target::new(Affine::IDENTITY));
            self.frames.push(LayerFrame::Group { blend, alpha });
        } else {
            self.frames.push(LayerFrame::Simple);
        }
        let dec = self.config.decimals;
        let mut ops = String::from("q\n");
        if !clip.elements().is_empty() {
            // The transform applies to the clip, not to the layer's
            // contents, so it is baked into the geometry rather than
            // emitted as a `cm`. Baking is exact for an affine, and it
            // keeps the invariant that a layer never carries a `cm` —
            // which is what makes the pattern-space rule a single
            // sentence.
            let baked: Path = if is_identity(transform) {
                clip.clone()
            } else {
                transform * clip.clone()
            };
            let mut non_finite = false;
            if content::write_path(&mut ops, &baked, dec, &mut non_finite) {
                ops.push_str(content::clip_op(FillRule::NonZero));
            }
            if non_finite {
                self.warnings.note(PdfWarning::NonFiniteCoordinate);
            }
        }
        self.out().push_str(&ops);
    }

    fn pop_layer(&mut self) {
        self.flush_pending();
        let Some(frame) = self.frames.pop() else {
            self.warnings.note(PdfWarning::UnbalancedLayers);
            return;
        };
        let dec = self.config.decimals;
        Self::close_layer(frame, &mut self.targets, &mut self.res, self.size, dec);
    }
}

/// Maps scene space onto PDF user space for a page of this size.
///
/// The scene is y-down in pixels with the origin top-left; PDF user
/// space is y-up in points with the origin bottom-left. Emitted once at
/// the top of the page's content stream, after which one unit *is* one
/// scene pixel and every coordinate in the file goes out unmodified.
fn base_flip(size: Size, dpi: f64) -> Affine {
    let s = 72.0 / dpi;
    Affine::new([s, 0.0, 0.0, -s, 0.0, size.height * s])
}

/// FNV-1a over a payload, for deduplicating a stream by its content.
pub(crate) fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    h
}

/// Serialize `scene` as a PDF file.
///
/// Infallible: building a byte buffer cannot fail, and a scene
/// expressing something PDF cannot produces [`PdfScene::warnings`]
/// rather than an error.
pub fn encode_pdf(scene: &PdfScene) -> Vec<u8> {
    scene.to_pdf()
}

/// Write `scene` to `w`.
pub fn write_pdf_to<W: io::Write>(mut w: W, scene: &PdfScene) -> io::Result<()> {
    w.write_all(&scene.to_pdf())
}

/// Write `scene` to `path`.
pub fn write_pdf(path: impl AsRef<std::path::Path>, scene: &PdfScene) -> io::Result<()> {
    let file = std::fs::File::create(path)?;
    write_pdf_to(io::BufWriter::new(file), scene)
}
