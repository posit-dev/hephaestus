//! Flatten a shaped [`RichTextRun`] into a single line of positioned
//! glyphs plus decoration rules, for callers that stamp each glyph
//! individually instead of drawing a laid-out box — text on a curve
//! being the case this exists for.
//!
//! Every line of every block is appended in document order, separated
//! by one space of the run's base style, so a source that produced
//! block structure (headings, list items, paragraph breaks, hard
//! breaks) still reads as one continuous string. Block geometry is
//! dropped in the process: block `y`, indents, margins, list markers,
//! span backgrounds and borders have no meaning on a curve. What
//! survives is everything that lives on a glyph — font, size, colour,
//! baseline shift — plus underline / strikethrough as
//! [`RichFlatRule`]s.

use parley::PositionedLayoutItem;

use super::draw::baseline_shift_for_range;
use super::run::RichTextRun;
use super::shape::pt_to_px;
use crate::color::Color;
use crate::scene::Font;
use crate::text::shape_common::rule_spans;
use crate::text::{TextRun, TextStyle};

/// One glyph of a flattened run, positioned along the flow axis.
///
/// `x` is the distance from the run's start along the flow direction —
/// arc length, for a caller walking a curve. `dy` is the offset from
/// the run's common baseline in screen (y-down) pixels, carrying any
/// superscript / subscript shift.
#[derive(Clone)]
pub struct RichFlatGlyph {
    /// Glyph id in `font`.
    pub id: u32,
    /// Distance from the run's start along the flow axis.
    pub x: f32,
    /// The glyph's advance — how much flow distance it occupies.
    pub advance: f32,
    /// Offset from the run's baseline, screen y-down.
    pub dy: f32,
    /// Font the glyph id belongs to.
    pub font: Font,
    /// Font size in pixels.
    pub font_size: f32,
    /// Resolved glyph colour.
    pub color: Color,
}

/// One underline or strikethrough rule over a span of the flattened
/// run.
///
/// `dy` locates the rule's centreline relative to the run's baseline
/// (screen y-down), and `thickness` is the font's own rule thickness,
/// so a caller strokes `[x0, x1]` at `dy` with width `thickness`.
#[derive(Clone, Copy)]
pub struct RichFlatRule {
    /// Start of the rule along the flow axis.
    pub x0: f32,
    /// End of the rule along the flow axis.
    pub x1: f32,
    /// Centreline offset from the run's baseline, screen y-down.
    pub dy: f32,
    /// Rule thickness in pixels, from the font's metrics.
    pub thickness: f32,
    /// Rule colour — the colour of the text it decorates.
    pub color: Color,
}

/// A rich run reduced to one line: glyphs, decoration rules, and the
/// metrics a caller needs to place the line as a whole.
#[derive(Clone, Default)]
pub struct RichFlatText {
    /// Every glyph, in flow order.
    pub glyphs: Vec<RichFlatGlyph>,
    /// Underline / strikethrough rules over spans of the flow axis.
    pub rules: Vec<RichFlatRule>,
    /// Total flow-axis extent, joining gaps included.
    pub width: f32,
    /// Largest ascent over the flattened lines.
    pub ascent: f32,
    /// Largest descent over the flattened lines.
    pub descent: f32,
}

/// Flatten `run` to a single line of glyphs and decoration rules.
///
/// Segments — one per line of one block — are joined by a space of the
/// run's base style. Returns an empty [`RichFlatText`] when the run
/// shaped to no glyphs.
pub fn flatten_rich_run(run: &RichTextRun) -> RichFlatText {
    let mut out = RichFlatText::default();
    let blocks = run.blocks.borrow();
    let mut cursor = 0.0_f32;
    // Measured on the first join only — a single-segment run (the
    // common case: one paragraph of inline markup) never pays for it.
    let mut gap: Option<f32> = None;
    for bl in blocks.iter() {
        // The asymmetric-shift companion layout only materialises on a
        // re-break at a fixed width, which a flow-axis caller never
        // asks for — the block's own layout holds every line.
        for line in bl.layout.lines() {
            let line_metrics = line.metrics();
            let baseline = line_metrics.baseline;
            // A segment that shapes to nothing (an `hr` block, say)
            // leaves the cursor — and the pending joining space —
            // alone, so it costs no gap.
            let segment_start = if out.glyphs.is_empty() {
                cursor
            } else {
                cursor + *gap.get_or_insert_with(|| space_advance(&run.base_style, run.dpi))
            };
            let mut segment_width = 0.0_f32;
            for item in line.items() {
                let PositionedLayoutItem::GlyphRun(gr) = item else {
                    continue;
                };
                let prun = gr.run();
                let font = Font::from_data(prun.font().clone());
                let font_size = prun.font_size();
                let color = gr.style().brush.0;
                // Screen y-down, so a positive typographic lift
                // (superscript) subtracts.
                let shift_px = pt_to_px(
                    baseline_shift_for_range(&bl.baseline_shifts, &prun.text_range()),
                    run.dpi,
                );
                for g in gr.positioned_glyphs() {
                    out.glyphs.push(RichFlatGlyph {
                        id: g.id,
                        x: segment_start + g.x,
                        advance: g.advance,
                        dy: (g.y - baseline) - shift_px,
                        font: font.clone(),
                        font_size,
                        color,
                    });
                }
                let x0 = segment_start + gr.offset();
                let x1 = x0 + gr.advance();
                segment_width = segment_width.max(gr.offset() + gr.advance());
                for span in rule_spans(gr.style(), prun.metrics(), x0, x1, -shift_px) {
                    out.rules.push(RichFlatRule {
                        x0: span.x0,
                        x1: span.x1,
                        dy: span.dy,
                        thickness: span.thickness,
                        color,
                    });
                }
            }
            if segment_width <= 0.0 {
                continue;
            }
            cursor = segment_start + segment_width;
            out.width = out.width.max(cursor);
            // Ascent and descent split the line box at the baseline —
            // half the leading falls on each side — so a caller
            // anchoring on `ascent + descent` reserves the same height
            // the plain shaper reports for the same string.
            let line_height = line_metrics.ascent + line_metrics.descent + line_metrics.leading;
            out.ascent = out.ascent.max(baseline);
            out.descent = out.descent.max(line_height - baseline);
        }
    }
    out
}

/// Flow-axis width of one space in `style`, measured as the difference
/// between a spaced and an unspaced pair. Parley trims a lone space,
/// so the space has to be measured in context.
fn space_advance(style: &TextStyle, dpi: f64) -> f32 {
    let spaced = TextRun::new("a a", style, dpi).natural_width();
    let tight = TextRun::new("aa", style, dpi).natural_width();
    let gap = (spaced - tight) as f32;
    if gap.is_finite() && gap > 0.0 {
        gap
    } else {
        // Nothing measurable — a quarter em is the usual space width.
        (style.size_pt as f64 * dpi / 72.0) as f32 * 0.25
    }
}
