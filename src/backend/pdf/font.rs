//! Embedded fonts: outline extraction at draw time, the CID font
//! dictionaries at write time, and the text objects in between.
//!
//! Every glyph a plot draws is embedded, which is the whole point of
//! this backend — a reader with none of the plot's fonts installed sees
//! the same picture. Embedding is affordable because what gets embedded
//! is a *subset* built from the outlines actually drawn (see
//! [`sfnt`](super::sfnt)), typically a few kB against the megabytes of
//! the face it came from.
//!
//! A run is emitted as `Identity-H`-encoded subset glyph ids, so the
//! content stream carries positions rather than characters, and a
//! `/ToUnicode` CMap carries the characters back for selection and
//! search.

use std::collections::BTreeMap;

use skrifa::instance::{LocationRef, NormalizedCoord, Size as SkSize};
use skrifa::outline::DrawSettings;
use skrifa::{FontRef, MetadataProvider};

use super::sfnt::{GlyfGlyph, GlyfPen, VerticalMetrics};
use super::writer::{cm, matrix, num, Objects};
use super::{content, PdfScene, PdfWarning};
use crate::geometry::{Affine, Rect};
use crate::scene::{Glyph, GlyphRun};

/// One embedded face: a font file, a face inside it, and a
/// variable-font instance.
///
/// Two runs differing in any of the three need two embedded subsets,
/// because a variable font at two weights is two sets of outlines.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct FaceKey {
    blob_id: u64,
    index: u32,
    /// Normalized variation coordinates, as skrifa reports them.
    coords: Vec<i16>,
}

/// The glyphs one face contributes, and the outlines behind them.
struct FaceSubset {
    /// Source glyph id to subset glyph id. Subset ids start at 1; 0 is
    /// reserved for `.notdef`.
    gids: BTreeMap<u32, u16>,
    /// Subset glyph id to its contours in font units. Index 0 is
    /// `.notdef`, so a glyph id indexes this directly — which is what
    /// `/CIDToGIDMap /Identity` promises a reader.
    glyphs: Vec<GlyfGlyph>,
    /// Subset glyph id to its advance in font units.
    advances: Vec<u16>,
    /// Subset glyph id to the Unicode scalar it maps back to.
    to_unicode: BTreeMap<u16, u32>,
    /// Source glyph id to the lowest codepoint that maps to it, for
    /// the whole face. Shared with the color-glyph path, which needs it
    /// for `/ActualText`.
    reverse: BTreeMap<u32, u32>,
    metrics: VerticalMetrics,
    cap_height: i16,
    bbox: (i16, i16, i16, i16),
    family: String,
    res_name: String,
}

/// The faces a document embeds.
#[derive(Default)]
pub(crate) struct FontRegistry {
    faces: BTreeMap<FaceKey, FaceSubset>,
    /// Allocation order, so resource names are first-use ordered.
    order: Vec<FaceKey>,
}

/// What [`FontRegistry::note`] hands back for one span of glyphs.
pub(crate) struct Noted {
    /// The `/Font` resource name the text object selects.
    pub(crate) res_name: String,
    /// Subset glyph id per requested glyph.
    pub(crate) gids: Vec<u16>,
    /// Advance per requested glyph, in thousandths of an em.
    pub(crate) widths: Vec<f64>,
}

impl FontRegistry {
    /// Register `gids` from `run`'s face, extracting any outline not
    /// seen before, and report what a text object needs to draw them.
    ///
    /// Returns `None` when the face yields no usable outlines at all.
    pub(crate) fn note(
        &mut self,
        run: &GlyphRun<'_>,
        font_ref: &FontRef<'_>,
        coords: &[NormalizedCoord],
        gids: &[u32],
    ) -> Option<Noted> {
        let data = run.font.data();
        let key = FaceKey {
            blob_id: data.data.id(),
            index: data.index,
            coords: coords.iter().map(|c| c.to_bits()).collect(),
        };
        let location = location_of(coords);
        if !self.faces.contains_key(&key) {
            let subset = FaceSubset::new(font_ref, location, self.order.len())?;
            self.order.push(key.clone());
            self.faces.insert(key.clone(), subset);
        }
        let subset = self.faces.get_mut(&key)?;
        let outlines = font_ref.outline_glyphs();
        let glyph_metrics = font_ref.glyph_metrics(SkSize::unscaled(), location);
        let upem = f64::from(subset.metrics.upem).max(1.0);

        let mut out_gids = Vec::with_capacity(gids.len());
        let mut widths = Vec::with_capacity(gids.len());
        for source in gids {
            let id = skrifa::GlyphId::new(*source);
            let subset_gid = match subset.gids.get(source) {
                Some(g) => *g,
                None => {
                    let mut pen = GlyfPen::default();
                    if let Some(g) = outlines.get(id) {
                        let _ = g.draw(
                            DrawSettings::unhinted(SkSize::unscaled(), location),
                            &mut pen,
                        );
                    }
                    let advance = glyph_metrics.advance_width(id).unwrap_or(0.0);
                    let next = subset.glyphs.len() as u16;
                    subset.glyphs.push(pen.finish());
                    subset
                        .advances
                        .push(advance.round().clamp(0.0, 65535.0) as u16);
                    if let Some(cp) = subset.reverse.get(source) {
                        subset.to_unicode.insert(next, *cp);
                    }
                    subset.gids.insert(*source, next);
                    next
                }
            };
            out_gids.push(subset_gid);
            let advance = f64::from(subset.advances[subset_gid as usize]);
            widths.push(advance * 1000.0 / upem);
        }
        Some(Noted {
            res_name: subset.res_name.clone(),
            gids: out_gids,
            widths,
        })
    }

    /// The lowest codepoint mapping to `source` in `run`'s face, for a
    /// glyph drawn as graphics rather than as text.
    pub(crate) fn actual_text(
        &self,
        run: &GlyphRun<'_>,
        coords: &[NormalizedCoord],
        source: u32,
    ) -> Option<u32> {
        let data = run.font.data();
        let key = FaceKey {
            blob_id: data.data.id(),
            index: data.index,
            coords: coords.iter().map(|c| c.to_bits()).collect(),
        };
        self.faces.get(&key)?.reverse.get(&source).copied()
    }

    /// Write every embedded face and return the `/Font` sub-dictionary
    /// entries for a `/Resources` dictionary.
    pub(crate) fn write(&self, objects: &mut Objects, compress: bool) -> String {
        let mut out = String::new();
        if self.order.is_empty() {
            return out;
        }
        out.push_str("/Font << ");
        for key in &self.order {
            let Some(subset) = self.faces.get(key) else {
                continue;
            };
            let r = subset.write(objects, compress, key);
            out.push('/');
            out.push_str(&subset.res_name);
            out.push(' ');
            out.push_str(&r);
            out.push(' ');
        }
        out.push_str(">> ");
        out
    }

    /// Forget every face, for a new frame.
    pub(crate) fn clear(&mut self) {
        self.faces.clear();
        self.order.clear();
    }
}

impl FaceSubset {
    /// A subset of `font_ref` at `location`, with nothing in it yet.
    fn new(font_ref: &FontRef<'_>, location: LocationRef<'_>, index: usize) -> Option<Self> {
        let m = font_ref.metrics(SkSize::unscaled(), location);
        let upem = if m.units_per_em == 0 {
            1000
        } else {
            m.units_per_em
        };
        let bounds = m.bounds.unwrap_or(skrifa::metrics::BoundingBox {
            x_min: 0.0,
            y_min: 0.0,
            x_max: f32::from(upem),
            y_max: f32::from(upem),
        });
        // The lowest codepoint per glyph, so the choice is
        // deterministic. Glyphs produced by ligature or other
        // substitution have no entry at all and are simply absent from
        // the CMap: they render correctly and are not searchable.
        let mut reverse: BTreeMap<u32, u32> = BTreeMap::new();
        for (cp, gid) in font_ref.charmap().mappings() {
            reverse.entry(gid.to_u32()).or_insert(cp);
        }
        let family = font_ref
            .localized_strings(skrifa::string::StringId::FAMILY_NAME)
            .english_or_first()
            .map(|s| s.chars().collect::<String>())
            .unwrap_or_default();
        Some(Self {
            gids: BTreeMap::new(),
            glyphs: vec![GlyfGlyph::default()],
            advances: vec![0],
            to_unicode: BTreeMap::new(),
            reverse,
            metrics: VerticalMetrics {
                upem,
                ascent: round_i16(m.ascent),
                descent: round_i16(m.descent),
                line_gap: round_i16(m.leading),
            },
            cap_height: round_i16(m.cap_height.unwrap_or(m.ascent)),
            bbox: (
                round_i16(bounds.x_min),
                round_i16(bounds.y_min),
                round_i16(bounds.x_max),
                round_i16(bounds.y_max),
            ),
            family: sanitize_family(&family),
            res_name: format!("F{index}"),
        })
    }

    /// Write this face's five objects and return the `Type0` font's
    /// reference.
    fn write(&self, objects: &mut Objects, compress: bool, key: &FaceKey) -> String {
        // Glyph 0 is `.notdef` and always present; the rest follow in
        // subset order, which is first-draw order.
        let program = super::sfnt::build(&self.glyphs, &self.advances, self.metrics);
        let file_ref = objects.alloc();
        objects.stream(
            file_ref,
            &format!("/Length1 {}", program.len()),
            &program,
            compress,
        );

        let base = format!(
            "{}+{}",
            subset_tag(key, &self.family, &self.gids),
            self.family
        );
        let upem = f64::from(self.metrics.upem).max(1.0);
        let thousandths = |v: i16| (f64::from(v) * 1000.0 / upem).round() as i32;

        let descriptor = objects.alloc();
        objects.object(
            descriptor,
            &format!(
                "<< /Type /FontDescriptor /FontName /{base} /Flags 4 \
                 /FontBBox [{} {} {} {}] /ItalicAngle 0 /Ascent {} /Descent {} \
                 /CapHeight {} /StemV 80 /FontFile2 {} >>",
                thousandths(self.bbox.0),
                thousandths(self.bbox.1),
                thousandths(self.bbox.2),
                thousandths(self.bbox.3),
                thousandths(self.metrics.ascent),
                thousandths(self.metrics.descent),
                thousandths(self.cap_height),
                file_ref.to_ref_string(),
            ),
        );

        let mut widths = String::from("[ 1 [");
        for a in self.advances.iter().skip(1) {
            widths.push(' ');
            widths.push_str(&((f64::from(*a) * 1000.0 / upem).round() as i32).to_string());
        }
        widths.push_str(" ] ]");

        let descendant = objects.alloc();
        objects.object(
            descendant,
            &format!(
                "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /{base} \
                 /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> \
                 /FontDescriptor {} /DW 1000 /W {widths} /CIDToGIDMap /Identity >>",
                descriptor.to_ref_string()
            ),
        );

        let to_unicode = objects.alloc();
        objects.stream(
            to_unicode,
            "",
            to_unicode_cmap(&self.to_unicode).as_bytes(),
            compress,
        );

        let type0 = objects.alloc();
        objects.object(
            type0,
            &format!(
                "<< /Type /Font /Subtype /Type0 /BaseFont /{base} /Encoding /Identity-H \
                 /DescendantFonts [{}] /ToUnicode {} >>",
                descendant.to_ref_string(),
                to_unicode.to_ref_string()
            ),
        );
        type0.to_ref_string()
    }
}

/// The six-letter subset tag `/BaseFont` carries.
///
/// Derived from the face's identity and the glyphs drawn — never from a
/// blob id, which is process-local, and never from a counter, which
/// would change if draw order did.
fn subset_tag(key: &FaceKey, family: &str, gids: &BTreeMap<u32, u16>) -> String {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(family.as_bytes());
    bytes.extend_from_slice(&key.index.to_be_bytes());
    for c in &key.coords {
        bytes.extend_from_slice(&c.to_be_bytes());
    }
    for source in gids.keys() {
        bytes.extend_from_slice(&source.to_be_bytes());
    }
    let h = super::fnv1a(&bytes);
    let mut tag = String::with_capacity(6);
    for i in 0..6 {
        let slice = ((h >> (i * 5)) & 0x1f) as u8;
        tag.push((b'A' + slice % 26) as char);
    }
    tag
}

/// A family name reduced to what a PDF name may carry unescaped.
fn sanitize_family(family: &str) -> String {
    let cleaned: String = family
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    if cleaned.is_empty() {
        "Font".to_string()
    } else {
        cleaned
    }
}

/// The `/ToUnicode` CMap mapping subset glyph ids back to characters.
fn to_unicode_cmap(map: &BTreeMap<u16, u32>) -> String {
    let mut out = String::from(
        "/CIDInit /ProcSet findresource begin\n\
         12 dict begin\n\
         begincmap\n\
         /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
         /CMapName /Adobe-Identity-UCS def\n\
         /CMapType 2 def\n\
         1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
    );
    let entries: Vec<(&u16, &u32)> = map.iter().collect();
    // A `bfchar` block may hold at most 100 entries.
    for chunk in entries.chunks(100) {
        out.push_str(&format!("{} beginbfchar\n", chunk.len()));
        for (gid, cp) in chunk {
            out.push_str(&format!("<{:04X}> <", gid));
            for unit in utf16(**cp) {
                out.push_str(&format!("{unit:04X}"));
            }
            out.push_str(">\n");
        }
        out.push_str("endbfchar\n");
    }
    out.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    out
}

/// A scalar as its UTF-16 code units.
pub(crate) fn utf16(cp: u32) -> Vec<u16> {
    match char::from_u32(cp) {
        Some(c) => {
            let mut buf = [0u16; 2];
            c.encode_utf16(&mut buf).to_vec()
        }
        None => vec![0xFFFD],
    }
}

/// The normalized variation coordinates `run` asks for.
///
/// A variable font would otherwise embed its default instance — the
/// wrong weight or width — because the deltas live in `gvar`. The axis
/// values travel on the run's `FontSpec` precisely for this.
pub(crate) fn coords_for(font_ref: &FontRef<'_>, run: &GlyphRun<'_>) -> Vec<NormalizedCoord> {
    run.source
        .map(|src| {
            let settings: Vec<(skrifa::Tag, f32)> = src
                .font
                .variations
                .iter()
                .map(|v| (skrifa::Tag::new(&v.tag), v.value))
                .collect();
            font_ref.axes().location(settings).coords().to_vec()
        })
        .unwrap_or_default()
}

/// A location for `coords`, or the default when there are none.
pub(crate) fn location_of(coords: &[NormalizedCoord]) -> LocationRef<'_> {
    if coords.is_empty() {
        LocationRef::default()
    } else {
        LocationRef::new(coords)
    }
}

/// Draw `run`, splitting it into text objects and color-glyph
/// graphics.
///
/// A run becomes maximal spans of outline glyphs, each emitted as its
/// own `BT` … `ET`, with a color or bitmap glyph drawn as graphics
/// between spans and in glyph order.
pub(crate) fn emit_run(scene: &mut PdfScene, run: &GlyphRun<'_>) {
    let data = run.font.data();
    let Ok(font_ref) = FontRef::from_index(data.data.as_ref(), data.index) else {
        return;
    };
    let coords = coords_for(&font_ref, run);
    let mut span: Vec<Glyph> = Vec::with_capacity(run.glyphs.len());
    for glyph in run.glyphs {
        match super::color::classify(&font_ref, glyph.id) {
            super::color::GlyphKind::Outline => span.push(*glyph),
            kind => {
                emit_span(scene, run, &font_ref, &coords, &span);
                span.clear();
                super::color::emit(scene, run, &font_ref, &coords, *glyph, kind);
            }
        }
    }
    emit_span(scene, run, &font_ref, &coords, &span);
    note_link(scene, run, &font_ref, &coords);
}

/// Emit one contiguous span of outline glyphs as a text object.
fn emit_span(
    scene: &mut PdfScene,
    run: &GlyphRun<'_>,
    font_ref: &FontRef<'_>,
    coords: &[NormalizedCoord],
    glyphs: &[Glyph],
) {
    if glyphs.is_empty() {
        return;
    }
    let ids: Vec<u32> = glyphs.iter().map(|g| g.id).collect();
    let Some(noted) = scene.fonts.note(run, font_ref, coords, &ids) else {
        scene.warnings.note(PdfWarning::GlyphNotDrawable);
        return;
    };
    let dec = scene.config.decimals;
    let size = f64::from(run.font_size);
    let brush_alpha = run.brush_alpha.clamp(0.0, 1.0);
    let mut paint = scene.paint(run.brush, None, run.transform);
    if brush_alpha < 1.0 {
        paint.alpha = Some(paint.alpha.unwrap_or(1.0) * brush_alpha);
    }
    let stroking = run.style.is_some();
    let gs = if stroking {
        scene.gs_op(None, paint.alpha, None, paint.mask.as_deref())
    } else {
        scene.gs_op(paint.alpha, None, None, paint.mask.as_deref())
    };

    let mut ops = String::from("q\n");
    cm(&mut ops, run.transform, dec);
    ops.push_str(&gs);
    if let Some(stroke) = run.style {
        if content::write_stroke_state(&mut ops, stroke, dec) {
            scene.warnings.note(PdfWarning::AsymmetricCaps);
        }
    }
    ops.push_str("BT\n");
    // A halo pass and the fill pass that follows it arrive as two
    // separate runs, and two text objects in that order stack
    // correctly — the stroke lands behind the fill, which is what the
    // SVG backend spells `paint-order="stroke fill"`.
    if stroking {
        ops.push_str(&paint.stroke_ops);
        ops.push_str("1 Tr\n");
    } else {
        ops.push_str(&paint.fill_ops);
        ops.push_str("0 Tr\n");
    }
    ops.push_str(&format!("/{} ", noted.res_name));
    num(&mut ops, size, dec);
    ops.push_str(" Tf\n");

    // A per-glyph matrix, or a run that changes baseline, needs one
    // `Tm` per glyph; everything else shares a baseline and batches
    // into one `TJ`.
    let batched = run.glyph_transform.is_none()
        && glyphs.iter().all(|g| g.y == glyphs[0].y)
        && !glyphs.is_empty();
    if batched {
        write_tm(&mut ops, run, glyphs[0], size, dec);
        ops.push('[');
        let mut open = false;
        for (k, glyph) in glyphs.iter().enumerate() {
            if k > 0 {
                let expected = f64::from(glyphs[k - 1].x) + noted.widths[k - 1] / 1000.0 * size;
                let adjust = if size > 0.0 {
                    -(f64::from(glyph.x) - expected) * 1000.0 / size
                } else {
                    0.0
                };
                // Half a thousandth of an em is below any device
                // resolution.
                if adjust.abs() >= 0.5 {
                    if open {
                        ops.push('>');
                        open = false;
                    }
                    num(&mut ops, adjust, 2);
                }
            }
            if !open {
                ops.push('<');
                open = true;
            }
            ops.push_str(&format!("{:04X}", noted.gids[k]));
        }
        if open {
            ops.push('>');
        }
        ops.push_str("] TJ\n");
    } else {
        for (k, glyph) in glyphs.iter().enumerate() {
            write_tm(&mut ops, run, *glyph, size, dec);
            ops.push_str(&format!("<{:04X}> Tj\n", noted.gids[k]));
        }
    }
    ops.push_str("ET\nQ\n");
    scene.out().push_str(&ops);
}

/// Append the text matrix placing `glyph` on the page.
///
/// Glyph space is Y-up and the page transform is already flipped so
/// scene coordinates work, so `Tm` has to flip back. A
/// `glyph_transform` acts in Y-up glyph space and PDF applies the font
/// size before `Tm`, which is what the conjugation undoes.
fn write_tm(out: &mut String, run: &GlyphRun<'_>, glyph: Glyph, size: f64, decimals: u8) {
    let at = Affine::translate((f64::from(glyph.x), f64::from(glyph.y)));
    let tm = match run.glyph_transform {
        None => at * Affine::scale_non_uniform(1.0, -1.0),
        Some(g) if size != 0.0 => {
            at * Affine::scale_non_uniform(size, -size) * g * Affine::scale(1.0 / size)
        }
        Some(g) => at * g,
    };
    matrix(out, tm, decimals);
    out.push_str("Tm\n");
}

/// Record a link annotation for a run that carries a safe destination.
fn note_link(
    scene: &mut PdfScene,
    run: &GlyphRun<'_>,
    font_ref: &FontRef<'_>,
    coords: &[NormalizedCoord],
) {
    if !scene.config.links {
        return;
    }
    let Some(src) = run.source else { return };
    let Some(url) = src.link.filter(|u| crate::backend::href::safe_href(u)) else {
        return;
    };
    let Some(first) = run.glyphs.first() else {
        return;
    };
    let m = font_ref.metrics(SkSize::unscaled(), location_of(coords));
    let upem = if m.units_per_em == 0 {
        1000.0
    } else {
        f64::from(m.units_per_em)
    };
    let size = f64::from(run.font_size);
    let ascent = f64::from(m.ascent) * size / upem;
    // skrifa reports this negative, so subtracting it moves down the
    // page.
    let descent = f64::from(m.descent) * size / upem;
    let (x, y) = (f64::from(first.x), f64::from(first.y));
    let local = Rect::new(x, y - ascent, x + f64::from(src.advance), y - descent);
    let corners = [
        run.transform * crate::geometry::Point::new(local.x0, local.y0),
        run.transform * crate::geometry::Point::new(local.x1, local.y0),
        run.transform * crate::geometry::Point::new(local.x0, local.y1),
        run.transform * crate::geometry::Point::new(local.x1, local.y1),
    ];
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for c in corners {
        x0 = x0.min(c.x);
        y0 = y0.min(c.y);
        x1 = x1.max(c.x);
        y1 = y1.max(c.y);
    }
    scene.note_link(url, Rect::new(x0, y0, x1, y1), Some(src.group));
}

/// A metric rounded into the range a font table stores.
fn round_i16(v: f32) -> i16 {
    v.round().clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_family_name_is_reduced_to_what_a_pdf_name_carries() {
        assert_eq!(sanitize_family("Open Sans"), "OpenSans");
        assert_eq!(sanitize_family("Inter-Regular"), "Inter-Regular");
        assert_eq!(sanitize_family(""), "Font");
        assert_eq!(sanitize_family("源ノ角ゴシック"), "Font");
    }

    #[test]
    fn a_subset_tag_is_six_uppercase_letters() {
        let key = FaceKey {
            blob_id: 7,
            index: 0,
            coords: vec![],
        };
        let mut gids = BTreeMap::new();
        gids.insert(36u32, 1u16);
        let tag = subset_tag(&key, "Inter", &gids);
        assert_eq!(tag.len(), 6);
        assert!(tag.chars().all(|c| c.is_ascii_uppercase()), "{tag}");
        // Deterministic: the same inputs give the same tag, and the
        // blob id — which is process-local — is not among them.
        let other = FaceKey {
            blob_id: 99,
            index: 0,
            coords: vec![],
        };
        assert_eq!(subset_tag(&other, "Inter", &gids), tag);
    }

    #[test]
    fn a_different_glyph_set_earns_a_different_tag() {
        let key = FaceKey {
            blob_id: 1,
            index: 0,
            coords: vec![],
        };
        let mut a = BTreeMap::new();
        a.insert(36u32, 1u16);
        let mut b = BTreeMap::new();
        b.insert(36u32, 1u16);
        b.insert(37u32, 2u16);
        assert_ne!(subset_tag(&key, "Inter", &a), subset_tag(&key, "Inter", &b));
    }

    #[test]
    fn the_cmap_names_every_glyph_it_was_given() {
        let mut map = BTreeMap::new();
        map.insert(1u16, 0x41u32);
        map.insert(2u16, 0x1F600u32);
        let cmap = to_unicode_cmap(&map);
        assert!(cmap.contains("2 beginbfchar"), "{cmap}");
        assert!(cmap.contains("<0001> <0041>"), "{cmap}");
        // Astral scalars go out as a surrogate pair.
        assert!(cmap.contains("<0002> <D83DDE00>"), "{cmap}");
    }

    #[test]
    fn astral_scalars_become_surrogate_pairs() {
        assert_eq!(utf16(0x41), vec![0x0041]);
        assert_eq!(utf16(0x1F600), vec![0xD83D, 0xDE00]);
    }
}
