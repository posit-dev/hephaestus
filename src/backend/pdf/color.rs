//! Color glyphs: COLR paint graphs and bitmap strikes.
//!
//! A synthesized monochrome subset font cannot carry a color glyph, so
//! these leave the text path entirely and are drawn as graphics. What
//! makes that affordable is that skrifa's
//! [`ColorPainter`](skrifa::color::ColorPainter) callbacks map
//! one-to-one onto operators this backend already emits: a transform is
//! a `cm`, a clip is `W n`, a gradient fill is a shading painted with
//! `sh`, and a composite layer is the same transparency-group form a
//! `push_layer` produces.
//!
//! Each glyph's graphics are wrapped in a marked-content span naming
//! the characters they stand for, so selection, search and text
//! extraction still work.

use skrifa::bitmap::{BitmapData, BitmapGlyph, Origin};
use skrifa::color::{Brush as ColrBrush, ColorGlyph, ColorPainter, CompositeMode, Transform};
use skrifa::instance::{NormalizedCoord, Size as SkSize};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::raw::types::BoundingBox;
use skrifa::{FontRef, GlyphId, MetadataProvider};

use super::content;
use super::paint;
use super::res::{ResKind, Resources, RES_REF};
use super::writer::{matrix, num, pdf_hex};
use super::{PdfScene, PdfWarning, Warnings};
use crate::color::Color;
use crate::geometry::{Affine, Point, Rect};
use crate::scene::{Glyph, GlyphRun};

/// Which of the three drawing paths a glyph takes.
pub(crate) enum GlyphKind<'a> {
    /// Ordinary monochrome contours, drawn as text.
    Outline,
    /// A COLR v0 or v1 paint graph.
    Color(ColorGlyph<'a>),
    /// An embedded bitmap strike.
    Bitmap(BitmapGlyph<'a>),
}

/// Decide how `gid` is drawn.
///
/// COLR first, then bitmap strikes, then outlines — the order vello
/// uses.
///
/// **`outline_glyphs().get(gid).is_none()` is not the color-glyph
/// test.** An sbix font such as Apple Color Emoji carries a `glyf`
/// table whose emoji entries are *empty*, so `get` returns `Some` and
/// `draw` succeeds having emitted no pen calls at all. Only a face with
/// no outline table of any kind returns `None`.
pub(crate) fn classify<'a>(font_ref: &FontRef<'a>, gid: u32) -> GlyphKind<'a> {
    let id = GlyphId::new(gid);
    if let Some(g) = font_ref.color_glyphs().get(id) {
        return GlyphKind::Color(g);
    }
    // An unscaled request selects the *largest* strike, which is what a
    // print artifact wants — unlike a screen rasterizer, which matches
    // the pixel size it is drawing at.
    if let Some(b) = font_ref
        .bitmap_strikes()
        .glyph_for_size(SkSize::unscaled(), id)
    {
        return GlyphKind::Bitmap(b);
    }
    GlyphKind::Outline
}

/// Draw one color or bitmap glyph as graphics.
pub(crate) fn emit(
    scene: &mut PdfScene,
    run: &GlyphRun<'_>,
    font_ref: &FontRef<'_>,
    coords: &[NormalizedCoord],
    glyph: Glyph,
    kind: GlyphKind<'_>,
) {
    let dec = scene.config.decimals;
    let body = match kind {
        GlyphKind::Outline => return,
        GlyphKind::Color(cg) => paint_colr(scene, run, font_ref, coords, glyph, &cg),
        GlyphKind::Bitmap(bg) => paint_bitmap(scene, run, font_ref, coords, glyph, &bg),
    };
    let Some(body) = body else {
        scene.warnings.note(PdfWarning::GlyphNotDrawable);
        return;
    };
    // The character the graphics stand for, so a reader can still
    // select and search them.
    let actual = super::font::coords_for(font_ref, run);
    let text = scene.fonts.actual_text(run, &actual, glyph.id).or_else(|| {
        font_ref
            .charmap()
            .mappings()
            .find(|(_, g)| g.to_u32() == glyph.id)
            .map(|(cp, _)| cp)
    });
    let mut ops = String::new();
    if let Some(cp) = text {
        ops.push_str("/Span << /ActualText ");
        // UTF-16BE with a byte-order mark, which is what a text string
        // in a marked-content property list means.
        let mut bytes = vec![0xFE, 0xFF];
        for unit in super::font::utf16(cp) {
            bytes.extend_from_slice(&unit.to_be_bytes());
        }
        pdf_hex(&mut ops, &bytes);
        ops.push_str(" >> BDC\n");
    }
    ops.push_str("q\n");
    super::writer::cm(&mut ops, run.transform, dec);
    ops.push_str(&body);
    ops.push_str("Q\n");
    if text.is_some() {
        ops.push_str("EMC\n");
    }
    scene.out().push_str(&ops);
}

/// The operators drawing one COLR glyph, in the run's own space.
fn paint_colr(
    scene: &mut PdfScene,
    run: &GlyphRun<'_>,
    font_ref: &FontRef<'_>,
    coords: &[NormalizedCoord],
    glyph: Glyph,
    cg: &ColorGlyph<'_>,
) -> Option<String> {
    let location = super::font::location_of(coords);
    let m = font_ref.metrics(SkSize::unscaled(), location);
    let upem = if m.units_per_em == 0 {
        1000.0
    } else {
        f64::from(m.units_per_em)
    };
    let size = f64::from(run.font_size);
    let face_box = m.bounds.map(|b| {
        Rect::new(
            f64::from(b.x_min),
            f64::from(b.y_min),
            f64::from(b.x_max),
            f64::from(b.y_max),
        )
    });
    let clip = cg
        .bounding_box(location, SkSize::unscaled())
        .map(|b| {
            Rect::new(
                f64::from(b.x_min),
                f64::from(b.y_min),
                f64::from(b.x_max),
                f64::from(b.y_max),
            )
        })
        .or(face_box)
        .unwrap_or(Rect::new(0.0, 0.0, upem, upem));

    let palette: Vec<Color> = font_ref
        .color_palettes()
        .get(0)
        .map(|p| {
            p.colors()
                .iter()
                .map(|c| Color::from_rgba8(c.red, c.green, c.blue, c.alpha))
                .collect()
        })
        .unwrap_or_default();
    let foreground = match run.brush {
        crate::brush::Brush::Solid(c) => *c,
        _ => Color::BLACK,
    };
    let brush_alpha = run.brush_alpha.clamp(0.0, 1.0);

    // The painter works in font units, Y-up; this is the step onto the
    // page, and the same flip the text matrix carries. Computed before
    // the painter borrows the scene, because the mask it may build has
    // to know the whole way to default user space.
    let place = Affine::translate((f64::from(glyph.x), f64::from(glyph.y)))
        * Affine::scale_non_uniform(size / upem, -size / upem);
    let base = super::base_flip(scene.size, scene.dpi) * run.transform * place;
    let page = scene.page_box();
    let mut painter = ColrPainter {
        stack: vec![String::new()],
        res: &mut scene.res,
        warnings: &mut scene.warnings,
        decimals: scene.config.decimals,
        font: font_ref.clone(),
        coords,
        palette,
        foreground,
        alpha: brush_alpha,
        page,
        base,
        ctm: Affine::IDENTITY,
        ctm_stack: Vec::new(),
        clips: vec![clip],
        failed: false,
    };
    if cg.paint(location, &mut painter).is_err() || painter.failed {
        return None;
    }
    let inner = painter.stack.pop()?;
    if inner.is_empty() {
        return None;
    }
    let mut out = String::from("q\n");
    content::write_placement(&mut out, place, scene.config.decimals);
    out.push_str(&inner);
    out.push_str("Q\n");
    Some(out)
}

/// The operators drawing one bitmap strike, in the run's own space.
fn paint_bitmap(
    scene: &mut PdfScene,
    run: &GlyphRun<'_>,
    font_ref: &FontRef<'_>,
    coords: &[NormalizedCoord],
    glyph: Glyph,
    bitmap: &BitmapGlyph<'_>,
) -> Option<String> {
    let location = super::font::location_of(coords);
    let m = font_ref.metrics(SkSize::unscaled(), location);
    let upem = if m.units_per_em == 0 {
        1000.0
    } else {
        f64::from(m.units_per_em)
    };
    let size = f64::from(run.font_size);
    let foreground = match run.brush {
        crate::brush::Brush::Solid(c) => *c,
        _ => Color::BLACK,
    };
    let rgba = decode_bitmap(bitmap, foreground, &mut scene.warnings)?;
    let data = run.font.data();
    let key = format!(
        "glyphbmp:{}:{}:{}:{}",
        data.data.id(),
        glyph.id,
        bitmap.ppem_y,
        bitmap.width
    );
    let name = super::image::intern_samples(
        &key,
        bitmap.width,
        bitmap.height,
        &rgba,
        true,
        &mut scene.res,
    );

    // Skia's arithmetic, which its own comment attributes to CoreText
    // conformance testing; vello carries the same. Derived from
    // scratch it comes out subtly wrong for sbix faces.
    let font_units_to_size = size / upem;
    let image_scale_factor = if bitmap.ppem_y > 0.0 {
        size / f64::from(bitmap.ppem_y)
    } else {
        1.0
    };
    // Apple Color Emoji reports a zero y bearing; Skia substitutes 100.
    let bearing_y = if bitmap.bearing_y == 0.0 {
        100.0
    } else {
        f64::from(bitmap.bearing_y)
    };
    let mut t = Affine::translate((f64::from(glyph.x), f64::from(glyph.y)))
        * Affine::translate((
            -f64::from(bitmap.bearing_x) * font_units_to_size,
            bearing_y * font_units_to_size,
        ))
        * Affine::scale(image_scale_factor)
        * Affine::translate((
            -f64::from(bitmap.inner_bearing_x),
            -f64::from(bitmap.inner_bearing_y),
        ));
    if bitmap.placement_origin == Origin::BottomLeft {
        t *= Affine::translate((0.0, -f64::from(bitmap.height)));
    }
    let unit = Affine::new([
        f64::from(bitmap.width),
        0.0,
        0.0,
        -f64::from(bitmap.height),
        0.0,
        f64::from(bitmap.height),
    ]);
    let mut out = String::from("q\n");
    content::write_placement(&mut out, t * unit, scene.config.decimals);
    out.push_str(&format!("/{name} Do\nQ\n"));
    Some(out)
}

/// A strike's samples as straight-alpha RGBA8.
fn decode_bitmap(
    bitmap: &BitmapGlyph<'_>,
    foreground: Color,
    warnings: &mut Warnings,
) -> Option<Vec<u8>> {
    let n = (bitmap.width as usize).checked_mul(bitmap.height as usize)?;
    match &bitmap.data {
        BitmapData::Bgra(bytes) => {
            if bytes.len() < n * 4 {
                return None;
            }
            let mut rgba = bytes[..n * 4].to_vec();
            for px in rgba.chunks_exact_mut(4) {
                px.swap(0, 2);
            }
            super::image::unpremultiply(&mut rgba);
            Some(rgba)
        }
        BitmapData::Png(bytes) => decode_png_strike(bytes, warnings),
        BitmapData::Mask(mask) => {
            let mut alpha = vec![0u8; n];
            mask.decode_to_slice(bitmap.width, bitmap.height, &mut alpha)
                .ok()?;
            let [r, g, b, a] = foreground.to_rgba8().to_u8_array();
            let mut rgba = Vec::with_capacity(n * 4);
            for v in alpha {
                rgba.extend_from_slice(&[r, g, b, ((u16::from(v) * u16::from(a)) / 255) as u8]);
            }
            Some(rgba)
        }
    }
}

/// A PNG-compressed strike's samples.
#[cfg(feature = "png")]
fn decode_png_strike(bytes: &[u8], _warnings: &mut Warnings) -> Option<Vec<u8>> {
    let image = crate::image::decode_png(bytes).ok()?;
    Some(image.data.as_ref().to_vec())
}

/// Without a PNG decoder a PNG strike cannot be embedded.
#[cfg(not(feature = "png"))]
fn decode_png_strike(_bytes: &[u8], warnings: &mut Warnings) -> Option<Vec<u8>> {
    warnings.note(PdfWarning::MissingPngFeature);
    None
}

/// Maps skrifa's COLR callbacks onto content-stream operators.
struct ColrPainter<'a, 'f> {
    /// Operator buffers, innermost last. A composite layer pushes one.
    stack: Vec<String>,
    res: &'a mut Resources,
    warnings: &'a mut Warnings,
    decimals: u8,
    font: FontRef<'f>,
    coords: &'a [NormalizedCoord],
    palette: Vec<Color>,
    /// The color `palette_index == 0xFFFF` selects.
    foreground: Color,
    /// The run's own brush alpha, multiplied into every fill.
    alpha: f32,
    /// The page rectangle in default user space, which a mask form's
    /// `/BBox` and a nested group's need.
    page: Rect,
    /// Glyph space to default user space. A soft mask is set with the
    /// CTM reset to that space, so this is what its content carries.
    base: Affine,
    /// The transform accumulated since the glyph's own space.
    ctm: Affine,
    ctm_stack: Vec<Affine>,
    /// Clip boxes in the glyph's own space, innermost last.
    clips: Vec<Rect>,
    /// Set when an operator could not be emitted, so the caller drops
    /// the glyph rather than leaving a half-written `q` stack.
    failed: bool,
}

impl ColrPainter<'_, '_> {
    fn out(&mut self) -> &mut String {
        self.stack.last_mut().expect("one buffer is always open")
    }

    /// The innermost clip, in the space operators are being written in.
    fn clip_rect(&self) -> Rect {
        let r = *self.clips.last().expect("seeded in `paint_colr`");
        let inv = self.ctm.inverse();
        let corners = [
            inv * Point::new(r.x0, r.y0),
            inv * Point::new(r.x1, r.y0),
            inv * Point::new(r.x0, r.y1),
            inv * Point::new(r.x1, r.y1),
        ];
        let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for c in corners {
            x0 = x0.min(c.x);
            y0 = y0.min(c.y);
            x1 = x1.max(c.x);
            y1 = y1.max(c.y);
        }
        Rect::new(x0, y0, x1, y1)
    }

    /// Push a clip expressed in the current space, recorded in glyph
    /// space so a later transform does not invalidate it.
    fn push_clip_rect(&mut self, r: Rect) {
        let m = self.ctm;
        let corners = [
            m * Point::new(r.x0, r.y0),
            m * Point::new(r.x1, r.y0),
            m * Point::new(r.x0, r.y1),
            m * Point::new(r.x1, r.y1),
        ];
        let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for c in corners {
            x0 = x0.min(c.x);
            y0 = y0.min(c.y);
            x1 = x1.max(c.x);
            y1 = y1.max(c.y);
        }
        let outer = *self.clips.last().expect("seeded in `paint_colr`");
        self.clips.push(outer.intersect(Rect::new(x0, y0, x1, y1)));
    }

    /// Append a glyph's contours as path-construction operators.
    fn write_outline(&mut self, gid: GlyphId) -> bool {
        let location = super::font::location_of(self.coords);
        let Some(g) = self.font.outline_glyphs().get(gid) else {
            return false;
        };
        let mut path = crate::path::Path::new();
        let mut pen = ContourPen {
            path: &mut path,
            wrote: false,
        };
        if g.draw(
            DrawSettings::unhinted(SkSize::unscaled(), location),
            &mut pen,
        )
        .is_err()
            || !pen.wrote
        {
            return false;
        }
        let dec = self.decimals;
        let mut non_finite = false;
        let out = self.out();
        content::write_path(out, &path, dec, &mut non_finite)
    }

    /// The color a palette index and alpha resolve to.
    fn color(&self, palette_index: u16, alpha: f32) -> Color {
        // 0xFFFF means "the text's own color", which is the brush the
        // run was drawn with.
        let base = if palette_index == 0xFFFF {
            self.foreground
        } else {
            self.palette
                .get(usize::from(palette_index))
                .copied()
                .unwrap_or(self.foreground)
        };
        let mut c = base;
        c.components[3] *= alpha * self.alpha;
        c
    }

    /// Resolve a COLR color line into the stop list `ramp_function`
    /// consumes.
    fn stops(&self, stops: &[skrifa::color::ColorStop]) -> Vec<(f32, Color)> {
        let mut out: Vec<(f32, Color)> = stops
            .iter()
            .map(|s| {
                (
                    s.offset.clamp(0.0, 1.0),
                    self.color(s.palette_index, s.alpha),
                )
            })
            .collect();
        if out.is_empty() {
            out.push((0.0, self.foreground));
        }
        out.sort_by(|a, b| a.0.total_cmp(&b.0));
        if out[0].0 > 0.0 {
            out.insert(0, (0.0, out[0].1));
        }
        if out[out.len() - 1].0 < 1.0 {
            out.push((1.0, out[out.len() - 1].1));
        }
        out
    }

    /// Fill the current clip area with a solid color.
    fn fill_solid(&mut self, color: Color) {
        let dec = self.decimals;
        let p = paint::solid(color);
        let gs = paint::ext_gstate(p.alpha, None, None, None).map(|body| {
            let n = self.res.intern(ResKind::ExtGState, &body);
            format!("/{n} gs\n")
        });
        let r = self.clip_rect();
        let out = self.out();
        out.push_str("q\n");
        if let Some(gs) = gs {
            out.push_str(&gs);
        }
        out.push_str(&p.fill_ops);
        content::write_rect(out, r.x0, r.y0, r.width(), r.height(), dec);
        out.push_str("f\nQ\n");
    }

    /// Paint a gradient over the current clip area with `sh`.
    ///
    /// Stops that disagree about alpha take the same luminosity soft
    /// mask an ordinary gradient does — a COLRv1 colour line carries
    /// per-stop alpha, so a glyph that fades is as ordinary here as a
    /// ribbon that does.
    fn fill_gradient(&mut self, kind: u8, coords: &str, stops: &[(f32, Color)]) {
        let dict = self.shading(kind, coords, stops, paint::Ramp::Rgb);
        let to_page = self.base * self.ctm;
        let (alpha, mask) = match paint::uniform_alpha(stops) {
            Some(a) => ((a < 1.0).then_some(a), None),
            // A singular chain has no space to set a mask in — and a
            // glyph whose matrix collapses is invisible either way, so
            // the mean costs nothing and needs no report.
            None if to_page.determinant().abs() < 1e-12 => {
                let mean = stops.iter().map(|s| s.1.components[3]).sum::<f32>()
                    / stops.len().max(1) as f32;
                ((mean < 1.0).then_some(mean), None)
            }
            None => {
                let gray = self.shading(kind, coords, stops, paint::Ramp::Alpha);
                let (page, dec) = (self.page, self.decimals);
                (
                    None,
                    Some(paint::alpha_mask(&gray, to_page, page, self.res, dec)),
                )
            }
        };
        self.fill_shading(dict, alpha, mask);
    }

    /// A shading dictionary over `coords` in whichever channel `ramp`
    /// names.
    fn shading(&self, kind: u8, coords: &str, stops: &[(f32, Color)], ramp: paint::Ramp) -> String {
        let mut out = format!(
            "<< /ShadingType {kind} /ColorSpace {} /Coords [{coords}] /Function ",
            match ramp {
                paint::Ramp::Rgb => "/DeviceRGB",
                paint::Ramp::Alpha => "/DeviceGray",
            }
        );
        paint::ramp_function(&mut out, stops, ramp);
        out.push_str(" /Extend [true true] >>");
        out
    }

    /// Paint a shading over the current clip area with `sh`.
    fn fill_shading(&mut self, dict: String, alpha: Option<f32>, mask: Option<String>) {
        let name = self.res.intern(ResKind::Shading, &dict);
        let masked = mask.is_some();
        let gs = paint::ext_gstate(alpha, None, None, mask.as_deref()).map(|body| {
            let n = self.res.intern(ResKind::ExtGState, &body);
            format!("/{n} gs\n")
        });
        let reset = self.base * self.ctm;
        let dec = self.decimals;
        let out = self.out();
        out.push_str("q\n");
        // The mask has to be set in default user space; everything else
        // is painted in the glyph's own.
        if masked {
            super::writer::cm(out, reset.inverse(), dec);
        }
        if let Some(gs) = gs {
            out.push_str(&gs);
        }
        if masked {
            super::writer::cm(out, reset, dec);
        }
        out.push_str(&format!("/{name} sh\nQ\n"));
    }
}

impl ColorPainter for ColrPainter<'_, '_> {
    fn push_transform(&mut self, transform: Transform) {
        let a = Affine::new([
            f64::from(transform.xx),
            f64::from(transform.yx),
            f64::from(transform.xy),
            f64::from(transform.yy),
            f64::from(transform.dx),
            f64::from(transform.dy),
        ]);
        self.ctm_stack.push(self.ctm);
        self.ctm *= a;
        let dec = self.decimals;
        let out = self.out();
        out.push_str("q\n");
        matrix(out, a, dec);
        out.push_str("cm\n");
    }

    fn pop_transform(&mut self) {
        if let Some(t) = self.ctm_stack.pop() {
            self.ctm = t;
        }
        self.out().push_str("Q\n");
    }

    fn push_clip_glyph(&mut self, glyph_id: GlyphId) {
        let bounds = self
            .font
            .glyph_metrics(SkSize::unscaled(), super::font::location_of(self.coords))
            .bounds(glyph_id);
        let r = bounds
            .map(|b| {
                Rect::new(
                    f64::from(b.x_min),
                    f64::from(b.y_min),
                    f64::from(b.x_max),
                    f64::from(b.y_max),
                )
            })
            .unwrap_or_else(|| self.clip_rect());
        self.push_clip_rect(r);
        self.out().push_str("q\n");
        if self.write_outline(glyph_id) {
            self.out().push_str("W n\n");
        } else {
            // Nothing to clip to: leave the area unclipped rather than
            // clipping everything away.
            self.out().push_str("n\n");
        }
    }

    fn push_clip_box(&mut self, clip_box: BoundingBox<f32>) {
        let r = Rect::new(
            f64::from(clip_box.x_min),
            f64::from(clip_box.y_min),
            f64::from(clip_box.x_max),
            f64::from(clip_box.y_max),
        );
        self.push_clip_rect(r);
        let dec = self.decimals;
        let out = self.out();
        out.push_str("q\n");
        content::write_rect(out, r.x0, r.y0, r.width(), r.height(), dec);
        out.push_str("W n\n");
    }

    fn pop_clip(&mut self) {
        if self.clips.len() > 1 {
            self.clips.pop();
        }
        self.out().push_str("Q\n");
    }

    fn fill(&mut self, brush: ColrBrush<'_>) {
        let dec = self.decimals;
        match brush {
            ColrBrush::Solid {
                palette_index,
                alpha,
            } => {
                let c = self.color(palette_index, alpha);
                self.fill_solid(c);
            }
            ColrBrush::LinearGradient {
                p0,
                p1,
                color_stops,
                ..
            } => {
                let stops = self.stops(color_stops);
                let mut coords = String::new();
                for v in [p0.x, p0.y, p1.x, p1.y] {
                    num(&mut coords, f64::from(v), dec);
                    coords.push(' ');
                }
                coords.pop();
                self.fill_gradient(2, &coords, &stops);
            }
            ColrBrush::RadialGradient {
                c0,
                r0,
                c1,
                r1,
                color_stops,
                ..
            } => {
                let mut stops = self.stops(color_stops);
                // Normalization can hand back a negative start radius;
                // truncating the color line at zero is the client's
                // job.
                let (c0x, c0y, r0) = if r0 < 0.0 && r1 > r0 {
                    let t = (-r0 / (r1 - r0)).clamp(0.0, 1.0);
                    stops = truncate(&stops, t);
                    (c0.x + (c1.x - c0.x) * t, c0.y + (c1.y - c0.y) * t, 0.0f32)
                } else {
                    (c0.x, c0.y, r0.max(0.0))
                };
                let mut coords = String::new();
                for v in [c0x, c0y, r0, c1.x, c1.y, r1] {
                    num(&mut coords, f64::from(v), dec);
                    coords.push(' ');
                }
                coords.pop();
                self.fill_gradient(3, &coords, &stops);
            }
            ColrBrush::SweepGradient { color_stops, .. } => {
                self.warnings.note(PdfWarning::SweepGradient);
                let stops = self.stops(color_stops);
                let mid = stops
                    .iter()
                    .min_by(|a, b| (a.0 - 0.5).abs().total_cmp(&(b.0 - 0.5).abs()))
                    .map(|s| s.1)
                    .unwrap_or(self.foreground);
                self.fill_solid(mid);
            }
        }
    }

    fn fill_glyph(
        &mut self,
        glyph_id: GlyphId,
        brush_transform: Option<Transform>,
        brush: ColrBrush<'_>,
    ) {
        // The COLRv0 fast path, and the common v1 one: a solid layer
        // needs no clip at all, just the outline filled. skrifa's v0
        // traversal emits nothing else, so this is the whole of v0.
        if let ColrBrush::Solid {
            palette_index,
            alpha,
        } = brush
        {
            if brush_transform.is_none() {
                let c = self.color(palette_index, alpha);
                let p = paint::solid(c);
                let gs = paint::ext_gstate(p.alpha, None, None, None).map(|body| {
                    let n = self.res.intern(ResKind::ExtGState, &body);
                    format!("/{n} gs\n")
                });
                self.out().push_str("q\n");
                if let Some(gs) = gs {
                    self.out().push_str(&gs);
                }
                let fill_ops = p.fill_ops.clone();
                self.out().push_str(&fill_ops);
                if self.write_outline(glyph_id) {
                    self.out().push_str("f\n");
                } else {
                    self.out().push_str("n\n");
                }
                self.out().push_str("Q\n");
                return;
            }
        }
        self.push_clip_glyph(glyph_id);
        match brush_transform {
            Some(t) => {
                self.push_transform(t);
                self.fill(brush);
                self.pop_transform();
            }
            None => self.fill(brush),
        }
        self.pop_clip();
    }

    fn push_layer(&mut self, _composite_mode: CompositeMode) {
        self.stack.push(String::new());
    }

    fn pop_layer_with_mode(&mut self, composite_mode: CompositeMode) {
        let Some(inner) = self.stack.pop() else {
            self.failed = true;
            return;
        };
        if self.stack.is_empty() {
            // More layers popped than pushed: the graph is malformed
            // and the glyph is dropped rather than half-drawn.
            self.stack.push(inner);
            self.failed = true;
            return;
        }
        let Some(mix) = composite_mix(composite_mode) else {
            self.warnings.note(PdfWarning::UnsupportedCompose);
            // Everything Porter-Duff composites as source-over, which
            // is what appending the layer inline does.
            self.out().push_str(&inner);
            return;
        };
        let dec = self.decimals;
        let mut dict = String::from(
            "/Type /XObject /Subtype /Form /Group \
             << /S /Transparency /CS /DeviceRGB /I true /K false >> /BBox [",
        );
        // Generous on purpose: the group's contents are already clipped
        // by whatever is in force, and a box in font units would have
        // to track the whole transform chain to be right.
        let extent = self.page.width().max(self.page.height()).max(4096.0);
        for v in [-extent, -extent, extent, extent] {
            num(&mut dict, v, dec);
            dict.push(' ');
        }
        dict.push_str("] /Resources ");
        dict.push_str(RES_REF);
        let payload = inner.into_bytes();
        let key = format!("colrlayer:{dict}|{}", super::fnv1a(&payload));
        let name = self
            .res
            .intern_stream(ResKind::XObject, &key, &dict, payload, None);
        let gs = paint::ext_gstate(None, None, Some(mix), None).map(|body| {
            let n = self.res.intern(ResKind::ExtGState, &body);
            format!("/{n} gs\n")
        });
        let out = self.out();
        out.push_str("q\n");
        if let Some(gs) = gs {
            out.push_str(&gs);
        }
        out.push_str(&format!("/{name} Do\nQ\n"));
    }
}

/// The mix function a COLR composite mode maps onto, or `None` for the
/// Porter-Duff operators PDF's imaging model cannot express.
fn composite_mix(mode: CompositeMode) -> Option<crate::blend::Mix> {
    use crate::blend::Mix;
    Some(match mode {
        CompositeMode::SrcOver => Mix::Normal,
        CompositeMode::Screen => Mix::Screen,
        CompositeMode::Overlay => Mix::Overlay,
        CompositeMode::Darken => Mix::Darken,
        CompositeMode::Lighten => Mix::Lighten,
        CompositeMode::ColorDodge => Mix::ColorDodge,
        CompositeMode::ColorBurn => Mix::ColorBurn,
        CompositeMode::HardLight => Mix::HardLight,
        CompositeMode::SoftLight => Mix::SoftLight,
        CompositeMode::Difference => Mix::Difference,
        CompositeMode::Exclusion => Mix::Exclusion,
        CompositeMode::Multiply => Mix::Multiply,
        CompositeMode::HslHue => Mix::Hue,
        CompositeMode::HslSaturation => Mix::Saturation,
        CompositeMode::HslColor => Mix::Color,
        CompositeMode::HslLuminosity => Mix::Luminosity,
        _ => return None,
    })
}

/// A color line restricted to `t..=1`, re-interpolating the color at
/// the cut.
fn truncate(stops: &[(f32, Color)], t: f32) -> Vec<(f32, Color)> {
    if t <= 0.0 {
        return stops.to_vec();
    }
    let mut out: Vec<(f32, Color)> = Vec::with_capacity(stops.len() + 1);
    let at = sample(stops, t);
    out.push((0.0, at));
    for (o, c) in stops {
        if *o > t {
            out.push((((o - t) / (1.0 - t)).clamp(0.0, 1.0), *c));
        }
    }
    if out.last().map(|s| s.0).unwrap_or(0.0) < 1.0 {
        let last = out.last().map(|s| s.1).unwrap_or(at);
        out.push((1.0, last));
    }
    out
}

/// The color a sorted color line holds at `t`.
fn sample(stops: &[(f32, Color)], t: f32) -> Color {
    if stops.is_empty() {
        return Color::BLACK;
    }
    if t <= stops[0].0 {
        return stops[0].1;
    }
    for pair in stops.windows(2) {
        if t <= pair[1].0 {
            let span = pair[1].0 - pair[0].0;
            let k = if span > 0.0 {
                (t - pair[0].0) / span
            } else {
                0.0
            };
            return crate::color::lerp_color(
                pair[0].1,
                pair[1].1,
                f64::from(k),
                crate::color::ColorSpace::Srgb,
            );
        }
    }
    stops[stops.len() - 1].1
}

/// Collects a glyph's contours into a path, undoing nothing.
struct ContourPen<'a> {
    path: &'a mut crate::path::Path,
    wrote: bool,
}

impl OutlinePen for ContourPen<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.path.move_to(Point::new(f64::from(x), f64::from(y)));
        self.wrote = true;
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.path.line_to(Point::new(f64::from(x), f64::from(y)));
    }

    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        self.path.quad_to(
            Point::new(f64::from(cx), f64::from(cy)),
            Point::new(f64::from(x), f64::from(y)),
        );
    }

    fn curve_to(&mut self, c1x: f32, c1y: f32, c2x: f32, c2y: f32, x: f32, y: f32) {
        self.path.curve_to(
            Point::new(f64::from(c1x), f64::from(c1y)),
            Point::new(f64::from(c2x), f64::from(c2y)),
            Point::new(f64::from(x), f64::from(y)),
        );
    }

    fn close(&mut self) {
        self.path.close_path();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::pdf::sfnt::{GlyfGlyph, GlyfPoint, VerticalMetrics};

    /// A minimal parseable face, so the painter can be driven with
    /// synthetic callbacks on a machine with no color font installed.
    fn face_bytes() -> Vec<u8> {
        let square = GlyfGlyph {
            contours: vec![vec![
                GlyfPoint {
                    x: 0,
                    y: 0,
                    on_curve: true,
                },
                GlyfPoint {
                    x: 500,
                    y: 0,
                    on_curve: true,
                },
                GlyfPoint {
                    x: 500,
                    y: 700,
                    on_curve: true,
                },
                GlyfPoint {
                    x: 0,
                    y: 700,
                    on_curve: true,
                },
            ]],
        };
        super::super::sfnt::build(
            &[GlyfGlyph::default(), square],
            &[0, 600],
            VerticalMetrics {
                upem: 1000,
                ascent: 800,
                descent: -200,
                line_gap: 0,
            },
        )
    }

    /// Run `body` against a painter over a throwaway face and return the
    /// operators it wrote.
    fn drive(body: impl FnOnce(&mut ColrPainter<'_, '_>)) -> (String, Warnings) {
        let bytes = face_bytes();
        let font = FontRef::from_index(&bytes, 0).expect("a parseable face");
        let mut res = Resources::default();
        let mut warnings = Warnings::default();
        let out = {
            let mut painter = ColrPainter {
                stack: vec![String::new()],
                res: &mut res,
                warnings: &mut warnings,
                decimals: 3,
                font,
                coords: &[],
                palette: vec![
                    Color::from_rgba8(255, 0, 0, 255),
                    Color::from_rgba8(0, 0, 255, 128),
                ],
                foreground: Color::BLACK,
                alpha: 1.0,
                page: Rect::new(0.0, 0.0, 450.0, 150.0),
                base: Affine::scale(0.75),
                ctm: Affine::IDENTITY,
                ctm_stack: Vec::new(),
                clips: vec![Rect::new(0.0, 0.0, 1000.0, 1000.0)],
                failed: false,
            };
            body(&mut painter);
            painter.stack.pop().expect("one buffer is always open")
        };
        (out, warnings)
    }

    /// The property that matters most: whatever the paint graph does,
    /// the operators it produces have to balance, or the whole page
    /// after the glyph is drawn in the wrong state.
    fn assert_balanced(ops: &str) {
        let mut depth = 0i32;
        for token in ops.split_whitespace() {
            match token {
                "q" => depth += 1,
                "Q" => depth -= 1,
                _ => {}
            }
            assert!(depth >= 0, "popped more than pushed:\n{ops}");
        }
        assert_eq!(depth, 0, "left {depth} levels open:\n{ops}");
    }

    #[test]
    fn a_transform_becomes_a_scoped_matrix() {
        let (ops, _) = drive(|p| {
            p.push_transform(Transform {
                xx: 2.0,
                yx: 3.0,
                xy: 5.0,
                yy: 7.0,
                dx: 11.0,
                dy: 13.0,
            });
            p.pop_transform();
        });
        assert_eq!(ops, "q\n2 3 5 7 11 13 cm\nQ\n");
        assert_balanced(&ops);
    }

    #[test]
    fn a_clip_box_becomes_a_rectangle_and_a_clip() {
        let (ops, _) = drive(|p| {
            p.push_clip_box(BoundingBox {
                x_min: 10.0,
                y_min: 20.0,
                x_max: 110.0,
                y_max: 220.0,
            });
            p.pop_clip();
        });
        assert!(ops.contains("10 20 100 200 re"), "{ops}");
        assert!(ops.contains("W n"), "{ops}");
        assert_balanced(&ops);
    }

    #[test]
    fn a_clip_glyph_becomes_the_outline_and_a_clip() {
        let (ops, _) = drive(|p| {
            p.push_clip_glyph(GlyphId::new(1));
            p.pop_clip();
        });
        assert!(ops.contains("0 0 m"), "the outline was written: {ops}");
        assert!(ops.contains("W n"), "{ops}");
        assert_balanced(&ops);
    }

    #[test]
    fn a_solid_fill_paints_the_clip_area_in_the_palette_color() {
        let (ops, _) = drive(|p| {
            p.push_clip_box(BoundingBox {
                x_min: 0.0,
                y_min: 0.0,
                x_max: 100.0,
                y_max: 100.0,
            });
            p.fill(ColrBrush::Solid {
                palette_index: 0,
                alpha: 1.0,
            });
            p.pop_clip();
        });
        assert!(ops.contains("1 0 0 rg"), "palette entry 0 is red: {ops}");
        assert_balanced(&ops);
    }

    /// `0xFFFF` means "whatever color the text is", not "entry 65535".
    #[test]
    fn the_foreground_index_resolves_to_the_runs_own_brush() {
        let (ops, _) = drive(|p| {
            p.foreground = Color::from_rgba8(0, 255, 0, 255);
            p.fill(ColrBrush::Solid {
                palette_index: 0xFFFF,
                alpha: 1.0,
            });
        });
        assert!(ops.contains("0 1 0 rg"), "{ops}");
    }

    #[test]
    fn a_solid_layer_fills_its_outline_with_no_clip_at_all() {
        let (ops, _) = drive(|p| {
            p.fill_glyph(
                GlyphId::new(1),
                None,
                ColrBrush::Solid {
                    palette_index: 0,
                    alpha: 1.0,
                },
            );
        });
        assert!(ops.contains("0 0 m"), "{ops}");
        assert!(ops.contains("\nf\n"), "{ops}");
        assert!(!ops.contains("W n"), "the fast path needs no clip: {ops}");
        assert_balanced(&ops);
    }

    #[test]
    fn a_linear_gradient_becomes_a_shading_painted_over_the_clip() {
        let stops = [
            skrifa::color::ColorStop {
                offset: 0.0,
                palette_index: 0,
                alpha: 1.0,
            },
            skrifa::color::ColorStop {
                offset: 1.0,
                palette_index: 1,
                alpha: 1.0,
            },
        ];
        let (ops, _) = drive(|p| {
            p.fill(ColrBrush::LinearGradient {
                p0: skrifa::raw::types::Point::new(0.0, 0.0),
                p1: skrifa::raw::types::Point::new(100.0, 0.0),
                color_stops: &stops,
                extend: skrifa::color::Extend::Pad,
            });
        });
        assert!(ops.contains(" sh\n"), "{ops}");
        assert_balanced(&ops);
    }

    #[test]
    fn a_sweep_gradient_degrades_to_a_flat_fill_and_says_so() {
        let stops = [
            skrifa::color::ColorStop {
                offset: 0.0,
                palette_index: 0,
                alpha: 1.0,
            },
            skrifa::color::ColorStop {
                offset: 1.0,
                palette_index: 1,
                alpha: 1.0,
            },
        ];
        let (ops, warnings) = drive(|p| {
            p.fill(ColrBrush::SweepGradient {
                c0: skrifa::raw::types::Point::new(0.0, 0.0),
                start_angle: 0.0,
                end_angle: 360.0,
                color_stops: &stops,
                extend: skrifa::color::Extend::Pad,
            });
        });
        assert!(!ops.contains(" sh\n"), "{ops}");
        assert!(ops.contains("rg\n"), "{ops}");
        assert!(warnings.contains(&PdfWarning::SweepGradient));
    }

    #[test]
    fn a_blend_layer_becomes_a_transparency_group() {
        let (ops, _) = drive(|p| {
            p.push_layer(CompositeMode::Multiply);
            p.fill(ColrBrush::Solid {
                palette_index: 0,
                alpha: 1.0,
            });
            p.pop_layer_with_mode(CompositeMode::Multiply);
        });
        assert!(
            ops.contains(" Do\n"),
            "the layer is painted as a form: {ops}"
        );
        assert_balanced(&ops);
    }

    #[test]
    fn a_porter_duff_layer_composites_inline_and_is_reported() {
        let (ops, warnings) = drive(|p| {
            p.push_layer(CompositeMode::Xor);
            p.fill(ColrBrush::Solid {
                palette_index: 0,
                alpha: 1.0,
            });
            p.pop_layer_with_mode(CompositeMode::Xor);
        });
        assert!(!ops.contains(" Do\n"), "{ops}");
        assert!(
            ops.contains("rg\n"),
            "the layer's own paint survives: {ops}"
        );
        assert!(warnings.contains(&PdfWarning::UnsupportedCompose));
        assert_balanced(&ops);
    }

    #[test]
    fn every_composite_mode_either_maps_or_is_refused() {
        assert_eq!(
            composite_mix(CompositeMode::SrcOver),
            Some(crate::blend::Mix::Normal)
        );
        assert_eq!(
            composite_mix(CompositeMode::HslLuminosity),
            Some(crate::blend::Mix::Luminosity)
        );
        for mode in [
            CompositeMode::Clear,
            CompositeMode::Src,
            CompositeMode::SrcIn,
            CompositeMode::Xor,
            CompositeMode::Plus,
        ] {
            assert_eq!(
                composite_mix(mode),
                None,
                "{mode:?} is not a blend function"
            );
        }
    }

    /// skrifa's radial normalization can hand back a negative start
    /// radius, and truncating the color line at zero is the client's
    /// job.
    #[test]
    fn a_truncated_color_line_starts_at_the_interpolated_color() {
        let stops = [
            (0.0f32, Color::from_rgba8(0, 0, 0, 255)),
            (1.0f32, Color::from_rgba8(255, 255, 255, 255)),
        ];
        let cut = truncate(&stops, 0.5);
        assert_eq!(cut.first().unwrap().0, 0.0);
        assert_eq!(cut.last().unwrap().0, 1.0);
        let mid = cut[0].1.to_rgba8().to_u8_array();
        assert!(
            (100..=160).contains(&mid[0]),
            "the cut lands halfway up the ramp: {mid:?}"
        );
    }

    #[test]
    fn an_untruncated_color_line_is_left_alone() {
        let stops = [(0.0f32, Color::BLACK), (1.0f32, Color::WHITE)];
        assert_eq!(truncate(&stops, 0.0).len(), 2);
    }
}
