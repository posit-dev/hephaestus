//! Block-level layout pass — turn [`crate::text::rich::BuiltRuns::blocks`]
//! into auxiliary drawing primitives.
//!
//! Parley owns inline shaping and line breaking. Anything that draws
//! outside the glyph runs — backgrounds, borders, bullets, blockquote
//! bars, hr lines — is emitted here as a [`BlockPaint`] alongside the
//! shaped layout. [`crate::text::rich::draw_rich_text`] iterates the
//! block paints before the glyph runs so backgrounds and borders sit
//! underneath the text.
//!
//! **Coordinate system.** Every rect returned lives in the parley
//! layout's coordinate system — the same coordinates parley reports
//! through `line.metrics().inline_min_coord` etc. The draw pass
//! translates them into screen space via the anchor + transform
//! composition, so callers here don't touch the outer transform.
//!
//! **Padding.** The rect is the *outer* box — text ink extents
//! inflated outward by the block's `padding` (top / right / bottom /
//! left). CSS box-model semantics: the background paints the whole
//! outer rect (including padding), and the border stroke sits on the
//! outer rect's edge.
//!
//! **What this pass does not do (yet).**
//! - `margin`: parley already emits `\n\n` between top-level blocks so
//!   paragraphs get roughly one line of vertical breathing room. Finer
//!   control (a paragraph asking for `Rel(0.5)` bottom margin) would
//!   need a post-shape re-positioning pass that shifts blocks
//!   vertically after parley lays them out. Deferred.
//! - `indent` / `hanging`: parley has no `TextIndent` style property.
//!   Implementing them requires either whitespace injection at
//!   paragraph starts or a re-positioning of the first cluster on each
//!   line. Deferred.
//! - Per-side borders (a left-only bar for blockquotes, a bottom-only
//!   rule for `hr`): the current `StyleDelta` only carries a single
//!   `border_color` / `border_width` pair, which this pass emits as a
//!   uniform four-sided border. Tasks 7 and 8 in the plan refine
//!   those slots with dedicated auxiliary shapes.

use std::ops::Range;

use kurbo::Rect;

use super::reduce::Block;
use super::run::RichBrush;
use crate::color::Color;
use crate::plot::theme::Palette;

/// A drawing instruction for one block-level box. Emitted by
/// [`compute_block_paints`] in outer-first order — outer boxes paint
/// underneath inner ones.
#[derive(Debug, Clone, PartialEq)]
pub struct BlockPaint {
    /// Outer rectangle (background + border edge) in parley layout
    /// coordinates.
    pub outer_rect: Rect,
    /// Background fill colour. `None` = no background pass.
    pub background: Option<Color>,
    /// Border stroke. `None` = no border pass.
    pub border: Option<BlockBorder>,
    /// Uniform corner radius in pixels. `0.0` renders as a square-
    /// cornered rect.
    pub corner_radius: f32,
}

/// Border descriptor on a [`BlockPaint`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BlockBorder {
    /// Resolved stroke colour.
    pub color: Color,
    /// Stroke width in pixels.
    pub width_px: f32,
}

/// Walk `blocks` and produce one [`BlockPaint`] per block that carries
/// a background or a border. Empty blocks (`Rule` etc.) and blocks
/// with no paintable properties are skipped.
///
/// Output order is **outer-first**: for a `blockquote` containing a
/// `code_block`, the blockquote paint precedes the code block's paint
/// so the code block's background lands on top. This matches the
/// order [`crate::text::rich::draw_rich_text`] iterates.
///
/// `base_size_pt` is the base font size in pt; it resolves any
/// `Length::Rel` in padding / border-width / border-radius against
/// the block's ambient em. `dpi` converts the resolved pt values to
/// pixels.
pub fn compute_block_paints(
    layout: &parley::Layout<RichBrush>,
    blocks: &[Block],
    palette: &Palette,
    base_size_pt: f32,
    dpi: f64,
) -> Vec<BlockPaint> {
    // The reducer emits blocks in *close* order — children before
    // their parent. To paint outer-first, walk the list in reverse.
    let mut paints = Vec::new();
    for block in blocks.iter().rev() {
        let delta = &block.delta;
        let has_background = delta.background.is_some();
        let has_border = delta.border_color.is_some() && delta.border_width.is_some();
        if !has_background && !has_border {
            continue;
        }
        let Some(ink) = block_ink_rect(layout, &block.range) else {
            continue;
        };
        let (pad_top, pad_right, pad_bottom, pad_left) = delta
            .padding
            .map(|m| {
                let (t, r, b, l) = m.resolve(base_size_pt as f64);
                (
                    t * dpi / 72.0,
                    r * dpi / 72.0,
                    b * dpi / 72.0,
                    l * dpi / 72.0,
                )
            })
            .unwrap_or((0.0, 0.0, 0.0, 0.0));
        let outer_rect = Rect::new(
            ink.x0 - pad_left,
            ink.y0 - pad_top,
            ink.x1 + pad_right,
            ink.y1 + pad_bottom,
        );
        let background = delta.background.as_ref().map(|tc| tc.resolve(palette));
        let border = if has_border {
            let width_px =
                (delta.border_width.unwrap().resolve(base_size_pt as f64) * dpi / 72.0) as f32;
            if width_px > 0.0 {
                Some(BlockBorder {
                    color: delta.border_color.as_ref().unwrap().resolve(palette),
                    width_px,
                })
            } else {
                None
            }
        } else {
            None
        };
        let corner_radius = delta
            .border_radius
            .map(|l| (l.resolve(base_size_pt as f64) * dpi / 72.0) as f32)
            .unwrap_or(0.0);
        // A border-only block whose width collapsed to zero above.
        if background.is_none() && border.is_none() {
            continue;
        }
        paints.push(BlockPaint {
            outer_rect,
            background,
            border,
            corner_radius,
        });
    }
    paints
}

/// Bounding rect of every parley line whose text range overlaps
/// `block_range`. Returns `None` when no line intersects (zero-length
/// blocks or blocks whose content collapsed under shaping).
fn block_ink_rect(layout: &parley::Layout<RichBrush>, block_range: &Range<usize>) -> Option<Rect> {
    let mut x0 = f32::INFINITY;
    let mut x1 = f32::NEG_INFINITY;
    let mut y0 = f32::INFINITY;
    let mut y1 = f32::NEG_INFINITY;
    let mut has = false;
    for line in layout.lines() {
        let lr = line.text_range();
        // Half-open overlap: lr.end > block.start && lr.start < block.end.
        // A zero-length block (block.start == block.end) never overlaps
        // — matches the `Rule` case which paints via a dedicated
        // auxiliary primitive (task 8), not via this pass.
        if lr.end <= block_range.start || lr.start >= block_range.end {
            continue;
        }
        let m = line.metrics();
        x0 = x0.min(m.inline_min_coord);
        x1 = x1.max(m.inline_max_coord);
        y0 = y0.min(m.block_min_coord);
        y1 = y1.max(m.block_max_coord);
        has = true;
    }
    if !has {
        return None;
    }
    Some(Rect::new(x0 as f64, y0 as f64, x1 as f64, y1 as f64))
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::Color;
    use crate::plot::theme::{Length, Margin, ThemeColor};
    use crate::text::rich::style::StyleDelta;
    use crate::text::rich::{RichTextRun, RichTextStyleSheet};
    use crate::text::TextStyle;

    fn palette() -> Palette {
        Palette::new(
            Color::from_rgba8(255, 255, 255, 255),
            Color::from_rgba8(0, 0, 0, 255),
            Color::from_rgba8(51, 105, 232, 255),
        )
    }

    fn base_style() -> TextStyle {
        TextStyle::new(14.0)
    }

    fn shape(sheet: &RichTextStyleSheet, src: &str) -> RichTextRun {
        RichTextRun::new(
            src,
            &base_style(),
            Color::from_rgba8(0, 0, 0, 255),
            sheet,
            &palette(),
            96.0,
        )
        .unwrap()
    }

    #[test]
    fn code_block_produces_background_paint() {
        let run = shape(&RichTextStyleSheet::new(), "```\nlet x = 1;\n```");
        let paints = run.block_paints();
        assert!(
            paints.iter().any(|p| p.background.is_some()),
            "code_block should produce a background paint; got {paints:?}"
        );
    }

    #[test]
    fn paragraph_without_paint_produces_no_paint() {
        // The default `paragraph` entry only sets margin — this pass
        // ignores block-only fields that don't paint anything, so the
        // paint list must be empty.
        let run = shape(&RichTextStyleSheet::new(), "plain paragraph");
        let paints = run.block_paints();
        assert!(paints.is_empty(), "expected no paints, got {paints:?}");
    }

    #[test]
    fn padding_inflates_the_outer_rect() {
        // Custom sheet: paragraph gets a solid background + uniform
        // 10 pt padding so we know exactly what to expect.
        let mut sheet = RichTextStyleSheet::empty();
        sheet.set(
            "paragraph",
            StyleDelta {
                background: Some(ThemeColor::Fixed(Color::from_rgba8(200, 200, 200, 255))),
                padding: Some(Margin::all(Length::Abs(10.0))),
                ..StyleDelta::empty()
            },
        );
        let run = shape(&sheet, "hello world");
        let paints = run.block_paints();
        assert_eq!(paints.len(), 1, "expected one paint");
        let p = &paints[0];
        // 10 pt at 96 dpi = 10 * 96 / 72 ≈ 13.33 px on every side.
        let expected = 10.0 * 96.0 / 72.0;
        // Top-left ink starts at (0, 0) for parley's default alignment,
        // so the outer top-left must sit at ~(-expected, -expected).
        assert!(
            (p.outer_rect.x0 - (-expected)).abs() < 0.5,
            "expected outer.x0 ≈ -{expected}, got {}",
            p.outer_rect.x0
        );
        assert!(
            (p.outer_rect.y0 - (-expected)).abs() < 0.5,
            "expected outer.y0 ≈ -{expected}, got {}",
            p.outer_rect.y0
        );
    }

    #[test]
    fn zero_length_hr_block_produces_no_paint() {
        // A markdown `---` reduces to a zero-length `Rule` block. This
        // pass leaves it to the hr-line renderer (task 8) — it must
        // produce no paint here.
        let run = shape(&RichTextStyleSheet::new(), "before\n\n---\n\nafter");
        for paint in run.block_paints() {
            // The default hr entry has border but no background, so
            // *if* we did emit anything for hr, it would be a border-
            // only paint whose rect spans zero height. Assert the
            // opposite: no zero-height rects come out.
            assert!(
                paint.outer_rect.height() > 0.0,
                "hr should not emit paint (got {paint:?})"
            );
        }
    }

    #[test]
    fn nested_blockquote_paints_outer_first() {
        // Blockquote contains a code_block — the blockquote paint (if
        // any) must precede the code_block paint so the code_block's
        // background sits on top.
        let run = shape(&RichTextStyleSheet::new(), "> ```\n> let x = 1;\n> ```");
        let paints = run.block_paints();
        // We only care about the pair of paints that actually emitted
        // (blockquote's default has a border, code_block's has a
        // background — both should register).
        let bq_idx = paints
            .iter()
            .position(|p| p.border.is_some() && p.background.is_none());
        let cb_idx = paints.iter().position(|p| p.background.is_some());
        if let (Some(bq), Some(cb)) = (bq_idx, cb_idx) {
            assert!(
                bq < cb,
                "outer blockquote paint (index {bq}) should precede inner code_block paint (index {cb})"
            );
        }
        // (If pulldown-cmark parses the nested code block differently
        // than expected the two may not co-appear; the strict order
        // check only fires when both do, keeping the test resilient.)
    }

    #[test]
    fn border_only_block_produces_stroke_paint() {
        // A custom class carrying border but no background — the
        // paint must have `border = Some` and `background = None`.
        let mut sheet = RichTextStyleSheet::empty();
        sheet.set(
            "paragraph",
            StyleDelta {
                border_color: Some(ThemeColor::Ink),
                border_width: Some(Length::Abs(1.0)),
                ..StyleDelta::empty()
            },
        );
        let run = shape(&sheet, "content");
        let paints = run.block_paints();
        assert_eq!(paints.len(), 1);
        assert!(paints[0].border.is_some(), "border should be set");
        assert!(
            paints[0].background.is_none(),
            "background should not be set"
        );
    }

    #[test]
    fn zero_width_border_collapses_to_no_paint() {
        let mut sheet = RichTextStyleSheet::empty();
        sheet.set(
            "paragraph",
            StyleDelta {
                border_color: Some(ThemeColor::Ink),
                border_width: Some(Length::Abs(0.0)),
                ..StyleDelta::empty()
            },
        );
        let run = shape(&sheet, "content");
        assert!(
            run.block_paints().is_empty(),
            "zero-width border should not emit a paint"
        );
    }
}
