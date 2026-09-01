//! Building a TrueType font out of extracted outlines.
//!
//! The embedded font is *synthesized* rather than sliced out of the
//! face it came from. One code path then covers four things a
//! byte-level `glyf` subset would need four for: a CFF/OTF face has no
//! `glyf` table at all, a variable font has to be embedded at the
//! instance the plot shaped with rather than at its default, a `ttcf`
//! collection has to have one face pulled out of it, and subsetting
//! itself — only the glyphs asked for are ever drawn.
//!
//! ISO 32000-1 §9.9 (Table 126) lets a `/FontFile2` subset omit `cmap`,
//! `name` and `post`, and hinting tables are only needed when hinting
//! is present, which it is not here. So the file carries six tables and
//! nothing else: `glyf`, `head`, `hhea`, `hmtx`, `loca`, `maxp`.

use skrifa::outline::OutlinePen;

/// A point on a quadratic contour, as `glyf` stores it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GlyfPoint {
    /// X in font units.
    pub(crate) x: i16,
    /// Y in font units, in the face's own Y-up convention.
    pub(crate) y: i16,
    /// False for a quadratic control point.
    pub(crate) on_curve: bool,
}

/// One glyph's contours in font units.
#[derive(Debug, Clone, Default)]
pub(crate) struct GlyfGlyph {
    /// One entry per contour: the points making it up.
    pub(crate) contours: Vec<Vec<GlyfPoint>>,
}

impl GlyfGlyph {
    /// True when the glyph has no ink — `.notdef`, a space, or a color
    /// glyph whose monochrome layer is empty.
    pub(crate) fn is_empty(&self) -> bool {
        self.contours.iter().all(|c| c.is_empty())
    }

    /// The glyph's bounding box in font units, or all zeros when it is
    /// empty.
    fn bbox(&self) -> (i16, i16, i16, i16) {
        let mut b = (i16::MAX, i16::MAX, i16::MIN, i16::MIN);
        let mut any = false;
        for p in self.contours.iter().flatten() {
            b.0 = b.0.min(p.x);
            b.1 = b.1.min(p.y);
            b.2 = b.2.max(p.x);
            b.3 = b.3.max(p.y);
            any = true;
        }
        if any {
            b
        } else {
            (0, 0, 0, 0)
        }
    }

    /// Total point count, which `maxp` reports the largest of.
    fn point_count(&self) -> usize {
        self.contours.iter().map(Vec::len).sum()
    }
}

/// Collects skrifa outlines into `glyf` contours.
///
/// `glyf` stores quadratics, so a cubic from a CFF-flavoured face is
/// converted; a TrueType source emits quadratics already and is copied
/// through exactly. Nothing is negated here, unlike the SVG backend's
/// pen — a font file stores Y-up outlines and this is writing a font
/// file. The flip lives in the text matrix.
#[derive(Default)]
pub(crate) struct GlyfPen {
    glyph: GlyfGlyph,
    open: Vec<GlyfPoint>,
    current: kurbo::Point,
}

impl GlyfPen {
    /// The glyph collected so far.
    pub(crate) fn finish(mut self) -> GlyfGlyph {
        self.end_contour();
        self.glyph
    }

    fn end_contour(&mut self) {
        if !self.open.is_empty() {
            self.glyph.contours.push(std::mem::take(&mut self.open));
        }
    }

    fn push(&mut self, x: f64, y: f64, on_curve: bool) {
        self.open.push(GlyfPoint {
            x: round_i16(x),
            y: round_i16(y),
            on_curve,
        });
    }
}

impl OutlinePen for GlyfPen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.end_contour();
        self.push(f64::from(x), f64::from(y), true);
        self.current = kurbo::Point::new(f64::from(x), f64::from(y));
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.push(f64::from(x), f64::from(y), true);
        self.current = kurbo::Point::new(f64::from(x), f64::from(y));
    }

    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        self.push(f64::from(cx), f64::from(cy), false);
        self.push(f64::from(x), f64::from(y), true);
        self.current = kurbo::Point::new(f64::from(x), f64::from(y));
    }

    fn curve_to(&mut self, c1x: f32, c1y: f32, c2x: f32, c2y: f32, x: f32, y: f32) {
        let end = kurbo::Point::new(f64::from(x), f64::from(y));
        let cubic = kurbo::CubicBez::new(
            self.current,
            kurbo::Point::new(f64::from(c1x), f64::from(c1y)),
            kurbo::Point::new(f64::from(c2x), f64::from(c2y)),
            end,
        );
        // A tenth of a font unit at 1000–2048 upem is far below one
        // device pixel at any plot text size.
        for (_, _, q) in cubic.to_quads(0.1) {
            self.push(q.p1.x, q.p1.y, false);
            self.push(q.p2.x, q.p2.y, true);
        }
        self.current = end;
    }

    fn close(&mut self) {
        self.end_contour();
    }
}

/// The vertical metrics an `hhea` needs, in font units.
#[derive(Debug, Clone, Copy)]
pub(crate) struct VerticalMetrics {
    /// Design units per em.
    pub(crate) upem: u16,
    /// Distance from the baseline to the top of the alignment box.
    pub(crate) ascent: i16,
    /// Distance from the baseline to the bottom of the alignment box,
    /// negative as the face reports it.
    pub(crate) descent: i16,
    /// Recommended additional spacing between lines.
    pub(crate) line_gap: i16,
}

/// Assemble a TrueType file carrying exactly `glyphs`.
///
/// `glyphs[0]` is `.notdef` and is expected to be empty; `advances` has
/// one entry per glyph, in font units.
pub(crate) fn build(glyphs: &[GlyfGlyph], advances: &[u16], m: VerticalMetrics) -> Vec<u8> {
    let n = glyphs.len();
    debug_assert_eq!(n, advances.len());

    let mut glyf: Vec<u8> = Vec::new();
    let mut loca: Vec<u32> = Vec::with_capacity(n + 1);
    let mut bbox = (i16::MAX, i16::MAX, i16::MIN, i16::MIN);
    let mut lsbs: Vec<i16> = Vec::with_capacity(n);
    let mut max_points = 0usize;
    let mut max_contours = 0usize;
    for g in glyphs {
        loca.push(glyf.len() as u32);
        let (x0, y0, x1, y1) = g.bbox();
        lsbs.push(x0);
        if !g.is_empty() {
            bbox.0 = bbox.0.min(x0);
            bbox.1 = bbox.1.min(y0);
            bbox.2 = bbox.2.max(x1);
            bbox.3 = bbox.3.max(y1);
            write_simple_glyph(&mut glyf, g, (x0, y0, x1, y1));
            while glyf.len() % 4 != 0 {
                glyf.push(0);
            }
        }
        max_points = max_points.max(g.point_count());
        max_contours = max_contours.max(g.contours.len());
    }
    loca.push(glyf.len() as u32);
    if bbox.0 == i16::MAX {
        bbox = (0, 0, 0, 0);
    }

    let mut loca_bytes = Vec::with_capacity(loca.len() * 4);
    for o in &loca {
        loca_bytes.extend_from_slice(&o.to_be_bytes());
    }

    let mut head = Vec::with_capacity(54);
    head.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // version
    head.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // fontRevision
    head.extend_from_slice(&0u32.to_be_bytes()); // checkSumAdjustment, patched last
    head.extend_from_slice(&0x5F0F_3CF5u32.to_be_bytes()); // magicNumber
    head.extend_from_slice(&3u16.to_be_bytes()); // flags
    head.extend_from_slice(&m.upem.to_be_bytes());
    head.extend_from_slice(&0i64.to_be_bytes()); // created
    head.extend_from_slice(&0i64.to_be_bytes()); // modified
    head.extend_from_slice(&bbox.0.to_be_bytes());
    head.extend_from_slice(&bbox.1.to_be_bytes());
    head.extend_from_slice(&bbox.2.to_be_bytes());
    head.extend_from_slice(&bbox.3.to_be_bytes());
    head.extend_from_slice(&0u16.to_be_bytes()); // macStyle
    head.extend_from_slice(&8u16.to_be_bytes()); // lowestRecPPEM
    head.extend_from_slice(&2i16.to_be_bytes()); // fontDirectionHint
    head.extend_from_slice(&1i16.to_be_bytes()); // indexToLocFormat: long
    head.extend_from_slice(&0i16.to_be_bytes()); // glyphDataFormat

    let mut hhea = Vec::with_capacity(36);
    hhea.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    hhea.extend_from_slice(&m.ascent.to_be_bytes());
    hhea.extend_from_slice(&m.descent.to_be_bytes());
    hhea.extend_from_slice(&m.line_gap.to_be_bytes());
    hhea.extend_from_slice(&advances.iter().copied().max().unwrap_or(0).to_be_bytes());
    hhea.extend_from_slice(&lsbs.iter().copied().min().unwrap_or(0).to_be_bytes()); // minLeftSideBearing
    hhea.extend_from_slice(&0i16.to_be_bytes()); // minRightSideBearing
    hhea.extend_from_slice(&bbox.2.to_be_bytes()); // xMaxExtent
    hhea.extend_from_slice(&1i16.to_be_bytes()); // caretSlopeRise
    hhea.extend_from_slice(&0i16.to_be_bytes()); // caretSlopeRun
    hhea.extend_from_slice(&0i16.to_be_bytes()); // caretOffset
    for _ in 0..4 {
        hhea.extend_from_slice(&0i16.to_be_bytes()); // reserved
    }
    hhea.extend_from_slice(&0i16.to_be_bytes()); // metricDataFormat
    hhea.extend_from_slice(&(n as u16).to_be_bytes()); // numberOfHMetrics

    let mut hmtx = Vec::with_capacity(n * 4);
    for (advance, lsb) in advances.iter().zip(&lsbs) {
        hmtx.extend_from_slice(&advance.to_be_bytes());
        hmtx.extend_from_slice(&lsb.to_be_bytes());
    }

    let mut maxp = Vec::with_capacity(32);
    maxp.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    maxp.extend_from_slice(&(n as u16).to_be_bytes());
    maxp.extend_from_slice(&(max_points.min(0xffff) as u16).to_be_bytes());
    maxp.extend_from_slice(&(max_contours.min(0xffff) as u16).to_be_bytes());
    // Composites are zero because the pen flattens everything into
    // simple glyphs.
    maxp.extend_from_slice(&0u16.to_be_bytes()); // maxCompositePoints
    maxp.extend_from_slice(&0u16.to_be_bytes()); // maxCompositeContours
    maxp.extend_from_slice(&2u16.to_be_bytes()); // maxZones
    for _ in 0..7 {
        maxp.extend_from_slice(&0u16.to_be_bytes());
    }
    maxp.extend_from_slice(&0u16.to_be_bytes()); // maxComponentDepth

    // Records must be sorted by tag, which for these six is their
    // ASCII order.
    let tables: [(&[u8; 4], Vec<u8>); 6] = [
        (b"glyf", glyf),
        (b"head", head),
        (b"hhea", hhea),
        (b"hmtx", hmtx),
        (b"loca", loca_bytes),
        (b"maxp", maxp),
    ];
    assemble(&tables)
}

/// Lay out a table directory and its tables into one file.
fn assemble(tables: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
    let count = tables.len() as u16;
    let entry_selector = (15 - count.leading_zeros()) as u16;
    let search_range = 16u16 << entry_selector;
    let range_shift = count * 16 - search_range;

    let mut out = Vec::new();
    out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    out.extend_from_slice(&count.to_be_bytes());
    out.extend_from_slice(&search_range.to_be_bytes());
    out.extend_from_slice(&entry_selector.to_be_bytes());
    out.extend_from_slice(&range_shift.to_be_bytes());

    let directory_start = out.len();
    out.resize(directory_start + tables.len() * 16, 0);
    let mut head_offset = 0usize;
    for (i, (tag, data)) in tables.iter().enumerate() {
        while out.len() % 4 != 0 {
            out.push(0);
        }
        let offset = out.len();
        if *tag == b"head" {
            head_offset = offset;
        }
        let record = directory_start + i * 16;
        out[record..record + 4].copy_from_slice(*tag);
        out[record + 4..record + 8].copy_from_slice(&table_checksum(data).to_be_bytes());
        out[record + 8..record + 12].copy_from_slice(&(offset as u32).to_be_bytes());
        out[record + 12..record + 16].copy_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(data);
    }
    while out.len() % 4 != 0 {
        out.push(0);
    }
    // `head`'s own checksum was taken with this field still zero, which
    // is what the format requires.
    let adjustment = 0xB1B0_AFBAu32.wrapping_sub(table_checksum(&out));
    out[head_offset + 8..head_offset + 12].copy_from_slice(&adjustment.to_be_bytes());
    out
}

/// One glyph as a `glyf` simple-glyph record.
fn write_simple_glyph(out: &mut Vec<u8>, g: &GlyfGlyph, bbox: (i16, i16, i16, i16)) {
    let contours: Vec<&Vec<GlyfPoint>> = g.contours.iter().filter(|c| !c.is_empty()).collect();
    out.extend_from_slice(&(contours.len() as i16).to_be_bytes());
    out.extend_from_slice(&bbox.0.to_be_bytes());
    out.extend_from_slice(&bbox.1.to_be_bytes());
    out.extend_from_slice(&bbox.2.to_be_bytes());
    out.extend_from_slice(&bbox.3.to_be_bytes());
    let mut end = 0i32;
    for c in &contours {
        end += c.len() as i32;
        out.extend_from_slice(&((end - 1) as u16).to_be_bytes());
    }
    out.extend_from_slice(&0u16.to_be_bytes()); // instructionLength

    let points: Vec<GlyfPoint> = contours.iter().flat_map(|c| c.iter().copied()).collect();
    let xv: Vec<i16> = points.iter().map(|p| p.x).collect();
    let yv: Vec<i16> = points.iter().map(|p| p.y).collect();
    let (xs, x_flags) = deltas(&xv, 0x02, 0x10);
    let (ys, y_flags) = deltas(&yv, 0x04, 0x20);
    // No repeat compression: it saves a fraction of the glyph bytes,
    // flate absorbs the difference, and the encoder is one fewer place
    // to be subtly wrong.
    for (i, p) in points.iter().enumerate() {
        let mut flag = 0u8;
        if p.on_curve {
            flag |= 0x01;
        }
        flag |= x_flags[i];
        flag |= y_flags[i];
        out.push(flag);
    }
    out.extend_from_slice(&xs);
    out.extend_from_slice(&ys);
}

/// One coordinate axis as `glyf` deltas, with the flag bits that say
/// how each was encoded.
///
/// `short_bit` means the delta is written as one byte; `same_bit` then
/// means it is positive, and means "same as the previous point" when
/// `short_bit` is clear. X uses `0x02` / `0x10`, Y uses `0x04` / `0x20`
/// — the same two bits one place along.
fn deltas(values: &[i16], short_bit: u8, same_bit: u8) -> (Vec<u8>, Vec<u8>) {
    let mut bytes = Vec::new();
    let mut flags = Vec::with_capacity(values.len());
    let mut prev = 0i32;
    for v in values {
        let d = i32::from(*v) - prev;
        prev = i32::from(*v);
        if d == 0 {
            flags.push(same_bit);
        } else if d.abs() <= 255 {
            flags.push(short_bit | if d > 0 { same_bit } else { 0 });
            bytes.push(d.unsigned_abs() as u8);
        } else {
            flags.push(0);
            bytes.extend_from_slice(&(d as i16).to_be_bytes());
        }
    }
    (bytes, flags)
}

/// Sum of a table's big-endian `u32` words, wrapping, with the tail
/// zero-padded to a word boundary.
pub(crate) fn table_checksum(data: &[u8]) -> u32 {
    let mut sum = 0u32;
    let mut chunks = data.chunks_exact(4);
    for c in &mut chunks {
        sum = sum.wrapping_add(u32::from_be_bytes([c[0], c[1], c[2], c[3]]));
    }
    let rest = chunks.remainder();
    if !rest.is_empty() {
        let mut word = [0u8; 4];
        word[..rest.len()].copy_from_slice(rest);
        sum = sum.wrapping_add(u32::from_be_bytes(word));
    }
    sum
}

/// A coordinate rounded into the range `glyf` stores.
fn round_i16(v: f64) -> i16 {
    v.round().clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(x: i16, y: i16, on_curve: bool) -> GlyfPoint {
        GlyfPoint { x, y, on_curve }
    }

    fn square() -> GlyfGlyph {
        GlyfGlyph {
            contours: vec![vec![
                pt(0, 0, true),
                pt(500, 0, true),
                pt(500, 700, true),
                pt(0, 700, true),
            ]],
        }
    }

    #[test]
    fn a_checksum_pads_its_tail_to_a_word() {
        assert_eq!(table_checksum(&[0, 0, 0, 1]), 1);
        assert_eq!(table_checksum(&[0, 0, 0, 1, 0, 0, 0, 2]), 3);
        // Three trailing bytes are padded, not dropped.
        assert_eq!(table_checksum(&[1, 0, 0]), 0x0100_0000);
    }

    /// These three fields are pure arithmetic over the table count, and
    /// a viewer that binary-searches the directory trusts them.
    #[test]
    fn the_directory_search_fields_follow_the_table_count() {
        let file = build(&[GlyfGlyph::default()], &[0], metrics());
        let count = u16::from_be_bytes([file[4], file[5]]);
        let search_range = u16::from_be_bytes([file[6], file[7]]);
        let entry_selector = u16::from_be_bytes([file[8], file[9]]);
        let range_shift = u16::from_be_bytes([file[10], file[11]]);
        assert_eq!(count, 6);
        assert_eq!(entry_selector, 2, "floor(log2(6))");
        assert_eq!(search_range, 64, "16 * 2^2");
        assert_eq!(range_shift, 6 * 16 - 64);
    }

    fn metrics() -> VerticalMetrics {
        VerticalMetrics {
            upem: 1000,
            ascent: 800,
            descent: -200,
            line_gap: 0,
        }
    }

    #[test]
    fn an_empty_glyph_occupies_no_glyf_bytes() {
        let g = GlyfGlyph::default();
        assert!(g.is_empty());
        assert_eq!(g.bbox(), (0, 0, 0, 0));
    }

    #[test]
    fn deltas_pick_the_shortest_encoding_per_point() {
        // 0 -> 0 is "same"; 0 -> 200 is one positive byte; 200 -> -200
        // needs two.
        let (bytes, flags) = deltas(&[0, 200, -200], 0x02, 0x10);
        assert_eq!(flags, vec![0x10, 0x02 | 0x10, 0x00]);
        assert_eq!(bytes, vec![200, 0xFE, 0x70]);
    }

    #[test]
    fn a_cubic_is_flattened_into_quadratics_the_pen_can_store() {
        let mut pen = GlyfPen::default();
        pen.move_to(0.0, 0.0);
        pen.curve_to(0.0, 100.0, 100.0, 100.0, 100.0, 0.0);
        pen.close();
        let g = pen.finish();
        assert_eq!(g.contours.len(), 1);
        assert!(
            g.contours[0].iter().any(|p| !p.on_curve),
            "a flattened cubic has control points"
        );
        let last = g.contours[0].last().unwrap();
        assert_eq!((last.x, last.y), (100, 0), "it ends where the cubic did");
    }

    /// The file this builder produces has to be readable by an
    /// independent parser, or nothing downstream of it means anything.
    #[test]
    fn the_built_font_parses_back_and_draws_its_glyphs() {
        use skrifa::instance::{LocationRef, Size};
        use skrifa::outline::DrawSettings;
        use skrifa::{FontRef, MetadataProvider};

        let glyphs = vec![GlyfGlyph::default(), square()];
        let file = build(&glyphs, &[0, 600], metrics());
        let font = FontRef::from_index(&file, 0).expect("a parseable font");
        assert_eq!(
            font.metrics(Size::unscaled(), LocationRef::default())
                .units_per_em,
            1000
        );

        let outlines = font.outline_glyphs();
        let g = outlines
            .get(skrifa::GlyphId::new(1))
            .expect("glyph 1 exists");
        let mut back = GlyfPen::default();
        g.draw(
            DrawSettings::unhinted(Size::unscaled(), LocationRef::default()),
            &mut back,
        )
        .expect("draws");
        let drawn = back.finish();
        assert_eq!(drawn.contours.len(), 1);
        assert_eq!(drawn.contours[0].len(), 4);

        let metrics = font.glyph_metrics(Size::unscaled(), LocationRef::default());
        assert_eq!(metrics.advance_width(skrifa::GlyphId::new(1)), Some(600.0));
    }

    #[test]
    fn a_contour_with_control_points_round_trips() {
        use skrifa::instance::{LocationRef, Size};
        use skrifa::outline::DrawSettings;
        use skrifa::{FontRef, MetadataProvider};

        let curved = GlyfGlyph {
            contours: vec![vec![pt(0, 0, true), pt(250, 400, false), pt(500, 0, true)]],
        };
        let file = build(&[GlyfGlyph::default(), curved], &[0, 500], metrics());
        let font = FontRef::from_index(&file, 0).expect("a parseable font");
        let mut back = GlyfPen::default();
        font.outline_glyphs()
            .get(skrifa::GlyphId::new(1))
            .unwrap()
            .draw(
                DrawSettings::unhinted(Size::unscaled(), LocationRef::default()),
                &mut back,
            )
            .expect("draws");
        let drawn = back.finish();
        assert!(
            drawn.contours[0].iter().any(|p| !p.on_curve),
            "the off-curve point survived the round trip"
        );
    }
}
