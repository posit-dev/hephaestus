//! Block-level layout pass — resolve each [`crate::text::rich::RichTextRun`]'s
//! block layouts + container blocks into auxiliary drawing primitives.
//!
//! Per-block parley layouts (built by `run.rs`) already know their
//! screen-space geometry: `left_px`, `y_px`, `shape_width_px`,
//! `height_px`, plus own padding. This pass converts each block's
//! outer rect into a paint instruction — a background fill and/or a
//! border stroke — that [`crate::text::rich::draw_rich_text`] emits
//! before the glyph runs.
//!
//! **Container paints.** A non-leaf container (BlockQuote / Div /
//! List) paints over the *union* of its contained leaves' rects.
//! Container padding contributes to the block leaves' `left_px` +
//! `right_inset_px`, so the union rect (inflated by the container's
//! own padding.top / .bottom) already includes the visual "box" the
//! container reserves.
//!
//! **Order.** Outer-first: containers paint before their leaves, so
//! a leaf's background lands on top of its enclosing container's.
//! Emitted in outermost → innermost order.

use kurbo::Rect;

use super::run::{ancestors_of_range, BlockLayout, RichTextRun};
use crate::color::Color;
use crate::plot::theme::Palette;
use resolve_helpers::StyleDeltaResolveHelpersImpl as StyleDeltaResolveHelpers;

/// A drawing instruction for one block-level box. Emitted by
/// [`compute_block_paints`] in outer-first order.
#[derive(Debug, Clone, PartialEq)]
pub struct BlockPaint {
    /// Outer rectangle (background + border edge) in RichTextRun-
    /// local coordinates.
    pub outer_rect: Rect,
    /// Background fill colour. `None` = no background pass.
    pub background: Option<Color>,
    /// Border stroke. `None` = no border pass.
    pub border: Option<BlockBorder>,
    /// Uniform corner radius in pixels. `0.0` = square corners.
    pub corner_radius: f32,
}

/// Border descriptor on a [`BlockPaint`]. Per-side widths let a
/// blockquote express its left-edge bar as `[0, 0, 0, 3]` rather
/// than a full rectangular stroke.
#[derive(Debug, Clone, PartialEq)]
pub struct BlockBorder {
    /// Resolved stroke colour (single colour for all sides in v1).
    pub color: Color,
    /// Per-side widths in pixels: `[top, right, bottom, left]`. A
    /// side with width `0.0` is skipped at draw time.
    pub widths_px: [f32; 4],
    /// Optional dash / marker pattern from
    /// [`crate::text::rich::StyleDelta::border_type`], carried in pt
    /// (raw form). The draw pass routes marker-free patterns through
    /// kurbo's `with_dashes` fast path and marker-bearing patterns
    /// through [`crate::plot::geom::resolve::draw_linetype_with_markers`]
    /// (per-polyline chain). `None` = solid stroke.
    pub linetype_pt: Option<std::sync::Arc<[crate::scales::value::LinetypeStep]>>,
}

impl BlockBorder {
    /// True when every side has the same width — the draw pass can
    /// then emit a single rectangular stroke (which cooperates with
    /// `corner_radius`) instead of four independent line segments.
    pub fn is_uniform(&self) -> bool {
        let w0 = self.widths_px[0];
        self.widths_px.iter().all(|&w| (w - w0).abs() < 1e-3)
    }

    /// Max width across the four sides — the value to use for a
    /// uniform stroke (or to gate paint emission when everything is
    /// zero).
    pub fn max_width(&self) -> f32 {
        self.widths_px.iter().cloned().fold(0.0_f32, f32::max)
    }
}

/// Walk the run's leaf layouts + non-leaf containers and produce one
/// [`BlockPaint`] per block that carries a background or border.
/// Outer-first ordering: containers come before any leaf they wrap.
pub fn compute_block_paints(run: &RichTextRun) -> Vec<BlockPaint> {
    let blocks = run.blocks.borrow();
    let mut paints: Vec<BlockPaint> = Vec::new();
    let palette = &run.palette;
    let base_pt = run.base_size_pt as f64;
    let dpi = run.dpi;

    // Containers first (outermost → innermost). Deeper containers
    // (larger range = outer) paint before smaller ones. We identify
    // outer-vs-inner by range size.
    let mut containers = run.containers.clone();
    containers.sort_by(|a, b| {
        let a_size = a.range.end - a.range.start;
        let b_size = b.range.end - b.range.start;
        b_size.cmp(&a_size)
    });
    for container in &containers {
        if !has_paint(&container.delta.background, &container.delta.border_color) {
            continue;
        }
        // Union rect over every leaf whose range is contained in this
        // container's range.
        let leaves: Vec<&BlockLayout> = blocks
            .iter()
            .filter(|bl| {
                bl.text_range.start >= container.range.start
                    && bl.text_range.end <= container.range.end
            })
            .collect();
        if leaves.is_empty() {
            continue;
        }
        let mut x0 = f32::INFINITY;
        let mut y0 = f32::INFINITY;
        let mut x1 = f32::NEG_INFINITY;
        let mut y1 = f32::NEG_INFINITY;
        for bl in &leaves {
            let r = bl.outer_rect();
            x0 = x0.min(r.x0 as f32);
            y0 = y0.min(r.y0 as f32);
            x1 = x1.max(r.x1 as f32);
            y1 = y1.max(r.y1 as f32);
        }
        // Inflate outward by the container's own padding.
        let (t_pt, r_pt, b_pt, l_pt) = container.delta.padding.resolve_or_zero(base_pt);
        let px = |pt: f64| (pt * dpi / 72.0) as f32;
        x0 -= px(l_pt);
        x1 += px(r_pt);
        y0 -= px(t_pt);
        y1 += px(b_pt);
        let outer_rect = Rect::new(x0 as f64, y0 as f64, x1 as f64, y1 as f64);
        let bg = container
            .delta
            .background
            .as_ref()
            .map(|c| c.resolve(palette));
        let border = border_for(
            &container.delta.border_color,
            container.delta.border_width,
            container.delta.border_type.as_deref(),
            palette,
            base_pt,
            dpi,
        );
        let corner_radius = container
            .delta
            .border_radius
            .map(|l| (l.resolve(base_pt) * dpi / 72.0) as f32)
            .unwrap_or(0.0);
        if bg.is_none() && border.is_none() {
            continue;
        }
        paints.push(BlockPaint {
            outer_rect,
            background: bg,
            border,
            corner_radius,
        });
    }

    // Then leaves.
    for bl in blocks.iter() {
        let d = &bl.delta;
        let has_bg = d.background.is_some();
        let has_border = d.border_color.is_some() && d.border_width.is_some();
        if !has_bg && !has_border {
            continue;
        }
        let outer_rect = bl.outer_rect();
        let bg = d.background.as_ref().map(|c| c.resolve(palette));
        let border = border_for(
            &d.border_color,
            d.border_width,
            d.border_type.as_deref(),
            palette,
            base_pt,
            dpi,
        );
        let corner_radius = d
            .border_radius
            .map(|l| (l.resolve(base_pt) * dpi / 72.0) as f32)
            .unwrap_or(0.0);
        if bg.is_none() && border.is_none() {
            continue;
        }
        paints.push(BlockPaint {
            outer_rect,
            background: bg,
            border,
            corner_radius,
        });
    }

    // Suppress unused warning on ancestors_of_range (kept exported
    // for future container-level introspection).
    let _ = ancestors_of_range;

    paints
}

fn has_paint(
    bg: &Option<crate::plot::theme::ThemeColor>,
    border: &Option<crate::plot::theme::ThemeColor>,
) -> bool {
    bg.is_some() || border.is_some()
}

fn border_for(
    color: &Option<crate::plot::theme::ThemeColor>,
    width: Option<crate::plot::theme::Margin>,
    dash_pattern: Option<&[crate::scales::value::LinetypeStep]>,
    palette: &Palette,
    base_pt: f64,
    dpi: f64,
) -> Option<BlockBorder> {
    let (c, w) = (color.as_ref()?, width?);
    let (t, r, b, l) = w.resolve(base_pt);
    let widths_px = [
        (t * dpi / 72.0) as f32,
        (r * dpi / 72.0) as f32,
        (b * dpi / 72.0) as f32,
        (l * dpi / 72.0) as f32,
    ];
    if widths_px.iter().all(|&w| w <= 0.0) {
        return None;
    }
    let linetype_pt = dash_pattern.map(|steps| std::sync::Arc::from(steps.to_vec()));
    Some(BlockBorder {
        color: c.resolve(palette),
        widths_px,
        linetype_pt,
    })
}

// ─── Small resolve helpers for StyleDelta / Margin ──────────────────────────

pub(crate) mod resolve_helpers {
    use crate::plot::theme::Margin;

    pub trait StyleDeltaResolveHelpersImpl {
        fn resolve_or_zero(&self, base_pt: f64) -> (f64, f64, f64, f64);
    }

    impl StyleDeltaResolveHelpersImpl for Option<Margin> {
        fn resolve_or_zero(&self, base_pt: f64) -> (f64, f64, f64, f64) {
            match self {
                Some(m) => m.resolve(base_pt),
                None => (0.0, 0.0, 0.0, 0.0),
            }
        }
    }
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
            "code_block should produce a background paint"
        );
    }

    #[test]
    fn paragraph_without_paint_produces_no_paint() {
        let run = shape(&RichTextStyleSheet::new(), "plain paragraph");
        let paints = run.block_paints();
        assert!(paints.is_empty(), "got {paints:?}");
    }

    #[test]
    fn container_paint_excludes_own_margin_includes_padding() {
        // A custom container with both padding and margin — the
        // paint rect should extend by `padding` beyond the child
        // but the container's `margin` should sit outside the paint
        // (CSS: bg / border paint on the border-box, not the
        // margin-box).
        let mut sheet = RichTextStyleSheet::empty();
        sheet.set(
            "block_quote",
            StyleDelta {
                padding: Some(Margin::all(Length::Abs(10.0))),
                margin: Some(Margin::all(Length::Abs(20.0))),
                background: Some(ThemeColor::Fixed(Color::from_rgba8(200, 200, 200, 255))),
                ..StyleDelta::empty()
            },
        );
        let run = shape(&sheet, "> hello world");
        let paints = run.block_paints();
        let bq = paints
            .iter()
            .find(|p| p.background.is_some())
            .expect("expected bordered/filled blockquote paint");
        // 10pt at 96dpi = 13.33px on every side.
        // Paint should be inflated by ~13.33 relative to inner text,
        // but the run's total height should include ANOTHER 20pt on
        // each of top / bottom (margin) OUTSIDE the paint.
        let paint_h = bq.outer_rect.height();
        let total_h = run.natural_height();
        // total_h - paint_h ≈ 2 * 20pt = 26.67pt at 96dpi.
        // (Non-collapsing since blockquote has padding.top+bottom > 0.)
        let expected_margin_gap = 2.0 * 20.0 * 96.0 / 72.0;
        assert!(
            (total_h - paint_h) >= expected_margin_gap * 0.9,
            "run should be taller than paint by ~2×margin (paint={paint_h}, total={total_h}, expected gap ≈ {expected_margin_gap})",
        );
    }

    #[test]
    fn blockquote_paint_wraps_its_content() {
        // Default block_quote entry has a border (Accent alpha) — its
        // paint should exist and its outer rect should be wider than
        // an equivalent plain paragraph's ink rect.
        let quoted = shape(&RichTextStyleSheet::new(), "> hello world");
        let paints = quoted.block_paints();
        let bq = paints.iter().find(|p| p.border.is_some());
        assert!(bq.is_some(), "blockquote should produce a bordered paint");
    }

    #[test]
    fn border_only_block_produces_stroke_paint() {
        let mut sheet = RichTextStyleSheet::empty();
        sheet.set(
            "paragraph",
            StyleDelta {
                border_color: Some(ThemeColor::Ink),
                border_width: Some(Margin::all(Length::Abs(1.0))),
                ..StyleDelta::empty()
            },
        );
        let run = shape(&sheet, "content");
        let paints = run.block_paints();
        assert_eq!(paints.len(), 1);
        assert!(paints[0].border.is_some());
        assert!(paints[0].background.is_none());
    }

    #[test]
    fn zero_width_border_collapses_to_no_paint() {
        let mut sheet = RichTextStyleSheet::empty();
        sheet.set(
            "paragraph",
            StyleDelta {
                border_color: Some(ThemeColor::Ink),
                border_width: Some(Margin::all(Length::Abs(0.0))),
                ..StyleDelta::empty()
            },
        );
        let run = shape(&sheet, "content");
        assert!(run.block_paints().is_empty());
    }

    #[test]
    fn left_only_border_produces_left_edge_paint() {
        // Only the left side has non-zero width — verify the paint's
        // BlockBorder records widths_px with the left slot populated
        // and the others at zero.
        let mut sheet = RichTextStyleSheet::empty();
        sheet.set(
            "paragraph",
            StyleDelta {
                border_color: Some(ThemeColor::Ink),
                border_width: Some(Margin::new(
                    Length::Abs(0.0),
                    Length::Abs(0.0),
                    Length::Abs(0.0),
                    Length::Abs(4.0),
                )),
                ..StyleDelta::empty()
            },
        );
        let run = shape(&sheet, "content");
        let paints = run.block_paints();
        assert_eq!(paints.len(), 1);
        let border = paints[0].border.as_ref().expect("expected a border");
        assert!(
            !border.is_uniform(),
            "border should be per-side, not uniform"
        );
        assert!(border.widths_px[0].abs() < 0.1, "top should be 0");
        assert!(border.widths_px[1].abs() < 0.1, "right should be 0");
        assert!(border.widths_px[2].abs() < 0.1, "bottom should be 0");
        assert!(border.widths_px[3] > 3.0, "left should be ~4pt in px");
    }

    #[test]
    fn own_padding_inflates_outer_rect() {
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
        assert_eq!(paints.len(), 1);
        // 10pt at 96dpi ≈ 13.33px on every side. The paragraph's
        // shaped content sits at (padding_left, padding_top) = (13.33,
        // 13.33). Its outer rect must therefore reach back to (0, 0)
        // on the top-left.
        let p = &paints[0];
        assert!(
            p.outer_rect.x0.abs() < 0.5,
            "expected outer.x0 ≈ 0 (padding pulls rect back to origin), got {}",
            p.outer_rect.x0
        );
        assert!(
            p.outer_rect.y0.abs() < 0.5,
            "expected outer.y0 ≈ 0, got {}",
            p.outer_rect.y0
        );
    }
}
