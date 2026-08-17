//! Draw a shaped [`RichTextRun`](super::run::RichTextRun) into a
//! `SceneBuilder`: block backgrounds and borders first, then list
//! markers, then the glyph runs with their span chrome and
//! decorations.

use std::ops::Range;

use parley::PositionedLayoutItem;

use super::anchor::RichAnchor;
use super::border::emit_block_paint;
use super::reduce::{BaselineRun, InlineRun};
use super::run::{BlockLayout, MarkerLayout, RichBrush, RichTextRun};
use super::shape::pt_to_px;
use crate::brush::Brush;
use crate::color::Color;
use crate::geometry::Affine;
use crate::pick::PickId;
use crate::scene::{Font, GlyphRun, SceneBuilder};
use crate::style_vocab::Palette;
use crate::text::shape_common::{emit_decoration_rect, glyphs_of_run, DecorationRect};

// ─── Draw ───────────────────────────────────────────────────────────────────

/// Emit the shaped [`RichTextRun`] into `scene` at `(x, y)`.
///
/// `anchor` picks the point on the laid-out run that coincides with
/// `(x, y)`. `transform` composes around that anchor.
///
/// Draw order per glyph run: span background/border (from any
/// overlapping [`StyleDelta`] with `background` / `border_*`) →
/// per-span outline stroke (from [`StyleDelta::text_stroke`] +
/// `text_stroke_width`) → fill glyphs → text decorations
/// (underline / strikethrough). A caller who wants an outline on
/// every glyph regardless of source styles should set `text_stroke`
/// on the sheet class that governs the block (e.g. `paragraph`, a
/// custom container class).
///
/// Block-level paints (backgrounds, borders on paragraphs /
/// containers) are emitted before any of the above via
/// [`RichTextRun::block_paints`].
#[allow(clippy::too_many_arguments)]
pub fn draw_rich_text(
    scene: &mut dyn SceneBuilder,
    run: &RichTextRun,
    x: f64,
    y: f64,
    anchor: RichAnchor,
    transform: Affine,
    pick_id: PickId,
) {
    let bounds = run.layout_bounds();
    let offsets = bounds.resolve(anchor);
    let final_transform = Affine::translate((x, y)) * transform;

    // Block backgrounds + borders first.
    let dpi = run.dpi;
    for paint in &run.block_paints() {
        emit_block_paint(scene, paint, offsets, final_transform, dpi, pick_id);
    }
    let palette = &run.palette;
    // Glyphs.
    let blocks = run.blocks.borrow();
    for bl in blocks.iter() {
        let block_x = bl.left_px;
        let block_y = bl.y_px;
        emit_marker(
            scene,
            bl,
            palette,
            run.base_brush,
            offsets,
            final_transform,
            dpi,
            pick_id,
        );
        if let Some(cont_layout) = &bl.continuation_layout {
            if let Some(first_line) = bl.layout.lines().next() {
                emit_line_glyphs(
                    scene,
                    first_line,
                    block_x,
                    block_y,
                    bl.first_line_shift_px,
                    bl.is_rtl,
                    &bl.baseline_shifts,
                    &bl.source_inlines,
                    palette,
                    run.base_brush,
                    dpi,
                    offsets,
                    final_transform,
                    pick_id,
                );
            }
            let cont_y = block_y + bl.first_line_height_px;
            for line in cont_layout.lines() {
                emit_line_glyphs(
                    scene,
                    line,
                    block_x,
                    cont_y,
                    bl.continuation_shift_px,
                    bl.is_rtl,
                    &bl.continuation_baseline_shifts,
                    &bl.continuation_inlines,
                    palette,
                    run.base_brush,
                    dpi,
                    offsets,
                    final_transform,
                    pick_id,
                );
            }
        } else {
            for (line_index, line) in bl.layout.lines().enumerate() {
                let per_line_shift = if line_index == 0 {
                    bl.first_line_shift_px
                } else {
                    bl.continuation_shift_px
                };
                emit_line_glyphs(
                    scene,
                    line,
                    block_x,
                    block_y,
                    per_line_shift,
                    bl.is_rtl,
                    &bl.baseline_shifts,
                    &bl.source_inlines,
                    palette,
                    run.base_brush,
                    dpi,
                    offsets,
                    final_transform,
                    pick_id,
                );
            }
        }
    }
}

/// Emit one parley line's glyph runs (with span backgrounds,
/// per-span or outer outlines, and decorations) into `scene`.
/// Extracted so [`draw_rich_text`] can call it twice for the
/// asymmetric-shift case (first line + continuation lines).
///
/// `is_rtl` controls how `shift_px` is applied: under Ltr it pushes
/// the glyph run rightward from the block's left edge (indent gutter
/// on the left); under Rtl the shift is not added — the block's
/// narrower shape width plus parley's right-alignment already leaves
/// the indent gutter on the right side of the block.
#[allow(clippy::too_many_arguments)]
fn emit_line_glyphs(
    scene: &mut dyn SceneBuilder,
    line: parley::layout::Line<'_, RichBrush>,
    block_x: f32,
    block_y: f32,
    shift_px: f32,
    is_rtl: bool,
    baseline_shifts: &[BaselineRun],
    inlines: &[InlineRun],
    palette: &Palette,
    base_brush: Color,
    dpi: f64,
    offsets: super::anchor::AnchorOffsets,
    final_transform: Affine,
    pick_id: PickId,
) {
    // First pass — record positions of any InlineBoxes on this line
    // so we can bracket span paint rects with the reserved padding
    // space. Keyed by id (assigned at shape time: 2*i = left,
    // 2*i + 1 = right).
    let mut inline_box_x: std::collections::HashMap<u64, (f32, f32)> =
        std::collections::HashMap::new();
    for item in line.items() {
        if let PositionedLayoutItem::InlineBox(ib) = item {
            inline_box_x.insert(ib.id, (ib.x, ib.x + ib.width));
        }
    }
    for item in line.items() {
        let PositionedLayoutItem::GlyphRun(gr) = item else {
            continue;
        };
        let prun = gr.run();
        let font = Font::from_data(prun.font().clone());
        let brush_color = gr.style().brush.0;
        let brush = Brush::Solid(brush_color);
        let run_range = prun.text_range();
        let font_size = prun.font_size();
        let dy_px = pt_to_px(baseline_shift_for_range(baseline_shifts, &run_range), dpi);
        let metrics = prun.metrics();
        let baseline = gr.baseline();
        // Under Rtl the shape width was narrowed by `shift_px` and
        // parley right-aligns into it, so the indent gutter already
        // sits on the right of the block — no positional shift needed.
        let effective_shift = if is_rtl { 0.0 } else { shift_px };
        let run_x_base = block_x + effective_shift - offsets.ref_x;
        let glyph_x0 = run_x_base + gr.offset();
        let glyph_x1 = glyph_x0 + gr.advance();
        let y_base = block_y + baseline - offsets.ref_y - dy_px;

        // ── Match GlyphRun to InlineRun. Parley's `Run::text_range`
        //    reports the **parent** run's byte range (which spans
        //    every GlyphRun in the line, since a style-driven split
        //    subdivides one Run into multiple GlyphRuns without
        //    changing its logical extent). To identify which
        //    InlineRun owns this specific GlyphRun we match on the
        //    resolved brush colour: parley splits GlyphRuns on
        //    `Brush` changes, and each InlineRun's `delta.color`
        //    (or the block's `base_brush` fallback) determines that
        //    brush. When two InlineRuns resolve to the same colour
        //    (a background-only or text_stroke-only span with no
        //    `color`) parley cannot distinguish them at all — the
        //    span's decorations are not observable and, correctly,
        //    the outline pass doesn't fire. Callers wanting an
        //    outlined span therefore set `color` alongside
        //    `text_stroke` (or fold both into a sheet entry).
        let gr_brush = gr.style().brush.0;
        // Prefer a decorated InlineRun (has bg / border / text_stroke)
        // whose resolved colour matches this GlyphRun's brush — those
        // are the InlineRuns whose decorations we can actually emit.
        // Falls back to any overlapping InlineRun with matching brush
        // so plain (colour-only) spans still resolve for the fill pass.
        let brush_matches = |r: &&InlineRun| -> bool {
            let overlaps = r.range.start < run_range.end && r.range.end > run_range.start;
            if !overlaps {
                return false;
            }
            let effective = r
                .style
                .color
                .as_ref()
                .map(|c| c.resolve(palette))
                .unwrap_or(base_brush);
            effective == gr_brush
        };
        let has_decoration = |r: &&InlineRun| -> bool {
            r.style.background.is_some()
                || (r.style.border_color.is_some()
                    && r.style.border_width_pt.iter().any(|w| *w > 0.0))
                || (r.style.text_stroke.is_some() && r.style.text_stroke_width_pt > 0.0)
        };
        let matched: Option<&InlineRun> = inlines
            .iter()
            .find(|r| brush_matches(r) && has_decoration(r))
            .or_else(|| inlines.iter().find(brush_matches));

        // ── Span background + border, per line-fragment. Bracketed
        //    by any InlineBox padding placeholders on this line.
        if let Some(inl) = matched {
            let d = &inl.style;
            let has_bg = d.background.is_some();
            let has_border = d.border_color.is_some() && d.border_width_pt.iter().any(|w| *w > 0.0);
            if has_bg || has_border {
                let idx = inlines.iter().position(|r| std::ptr::eq(r, inl));
                let left_box_x =
                    idx.and_then(|i| inline_box_x.get(&((i as u64) * 2)).map(|(x0, _)| *x0));
                let right_box_x1 =
                    idx.and_then(|i| inline_box_x.get(&((i as u64) * 2 + 1)).map(|(_, x1)| *x1));
                let starts_here = inl.range.start >= run_range.start;
                let ends_here = inl.range.end <= run_range.end;
                // Rect x: extend to include the InlineBox padding
                // space when it's on this line-fragment.
                // InlineBox positions come from parley in the same
                // layout frame as `gr.offset()` / `gr.advance()`, so
                // to get screen coords we apply the same translation
                // (`block_x + shift_px - offsets.ref_x`) as the glyph
                // positions.
                let x0_paint = if starts_here {
                    left_box_x.map(|x| x + run_x_base).unwrap_or(glyph_x0)
                } else {
                    glyph_x0
                };
                let x1_paint = if ends_here {
                    right_box_x1.map(|x| x + run_x_base).unwrap_or(glyph_x1)
                } else {
                    glyph_x1
                };
                // Vertical: inflate by padding.top / .bottom. Font
                // ascent / descent give the natural line extent.
                let t_pad = (d.padding_pt[0] * dpi / 72.0) as f32;
                let b_pad = (d.padding_pt[2] * dpi / 72.0) as f32;
                let y0_paint = y_base - metrics.ascent - t_pad;
                let y1_paint = y_base + metrics.descent + b_pad;
                if x1_paint > x0_paint && y1_paint > y0_paint {
                    let rect = crate::geometry::Rect::new(
                        x0_paint as f64,
                        y0_paint as f64,
                        x1_paint as f64,
                        y1_paint as f64,
                    );
                    let corner_radius = (d.border_radius_pt * dpi / 72.0) as f32;
                    let path = if corner_radius > 0.0 {
                        crate::primitives::rounded_rect(rect, corner_radius as f64)
                    } else {
                        crate::primitives::rect(rect)
                    };
                    if let Some(bg) = &d.background {
                        let c = bg.resolve(palette);
                        scene.fill(
                            crate::path::FillRule::NonZero,
                            final_transform,
                            &Brush::Solid(c),
                            None,
                            &path,
                            pick_id,
                        );
                    }
                    if let Some(bc) = &d.border_color {
                        let [t, r, b, l] = d.border_width_pt;
                        let uniform =
                            ((t - r).abs() < 1e-3 && (r - b).abs() < 1e-3 && (b - l).abs() < 1e-3)
                                .then_some(t);
                        // Only handle uniform-width borders on inline
                        // spans; per-side is a rare case for inline.
                        if let Some(w_pt) = uniform {
                            let w_px = (w_pt * dpi / 72.0) as f32;
                            if w_px > 0.0 {
                                let c = bc.resolve(palette);
                                let stroke = crate::stroke::Stroke::new(w_px as f64)
                                    .with_caps(crate::stroke::Cap::Butt)
                                    .with_join(crate::stroke::Join::Miter);
                                scene.stroke(
                                    &stroke,
                                    final_transform,
                                    &Brush::Solid(c),
                                    None,
                                    &path,
                                    pick_id,
                                );
                            }
                        }
                    }
                }
            }
        }

        let glyphs = glyphs_of_run(&gr, run_x_base, block_y - offsets.ref_y - dy_px);
        if glyphs.is_empty() {
            continue;
        }

        // ── Outline pass (behind fill). Sourced from
        //    `text_stroke` on any matching InlineRun —
        //    typically set on a specific span class (e.g. a
        //    `.haloed` custom class) so a caller who wants a
        //    document-wide outline just sets the field on the
        //    sheet's root `paragraph` / `heading` classes.
        let span_outline_owned: Option<(Brush, crate::stroke::Stroke)> = matched.and_then(|inl| {
            let color = inl.style.text_stroke.as_ref()?;
            let w_px = (inl.style.text_stroke_width_pt * dpi / 72.0) as f32;
            if w_px <= 0.0 {
                return None;
            }
            Some((
                Brush::Solid(color.resolve(palette)),
                crate::stroke::Stroke::new(w_px as f64),
            ))
        });
        if let Some((ob, os)) = span_outline_owned.as_ref() {
            let outline_run = GlyphRun {
                font: &font,
                font_size,
                transform: final_transform,
                glyph_transform: None,
                brush: ob,
                brush_alpha: 1.0,
                hint: false,
                glyphs: &glyphs,
                style: Some(os),
            };
            // The fill pass owns picking; the halo behind it must not
            // widen the hit area.
            scene.draw_glyphs(&outline_run, PickId::Skip);
        }

        // Fill pass.
        let glyph_run = GlyphRun {
            font: &font,
            font_size,
            transform: final_transform,
            glyph_transform: None,
            brush: &brush,
            brush_alpha: 1.0,
            hint: false,
            glyphs: &glyphs,
            style: None,
        };
        scene.draw_glyphs(&glyph_run, pick_id);

        // Decorations (over glyphs).
        let style = gr.style();
        if style.underline.is_some() || style.strikethrough.is_some() {
            if let Some(deco) = &style.underline {
                emit_decoration_rect(
                    scene,
                    DecorationRect {
                        x0: glyph_x0,
                        x1: glyph_x1,
                        top: y_base - deco.offset.unwrap_or(metrics.underline_offset),
                        thickness: deco.size.unwrap_or(metrics.underline_size).max(0.0),
                    },
                    &brush,
                    final_transform,
                    pick_id,
                );
            }
            if let Some(deco) = &style.strikethrough {
                emit_decoration_rect(
                    scene,
                    DecorationRect {
                        x0: glyph_x0,
                        x1: glyph_x1,
                        top: y_base - deco.offset.unwrap_or(metrics.strikethrough_offset),
                        thickness: deco.size.unwrap_or(metrics.strikethrough_size).max(0.0),
                    },
                    &brush,
                    final_transform,
                    pick_id,
                );
            }
        }
    }
}
/// Horizontal extent (px) a block's marker occupies, in run-local
/// coordinates. The marker sits in the list's start gutter, so under
/// Rtl it hangs off the block's right edge instead of its left.
pub(crate) fn marker_x_range(bl: &BlockLayout, marker: &MarkerLayout) -> (f32, f32) {
    if bl.is_rtl {
        let x0 = bl.left_px + bl.shape_width_px + marker.gap_px;
        (x0, x0 + marker.width_px)
    } else {
        let x1 = bl.left_px - marker.gap_px;
        (x1 - marker.width_px, x1)
    }
}

/// Draw a block's list-item marker, right-aligned into the gutter on
/// the start side and sharing the first line's baseline.
#[allow(clippy::too_many_arguments)]
fn emit_marker(
    scene: &mut dyn SceneBuilder,
    bl: &BlockLayout,
    palette: &Palette,
    base_brush: Color,
    offsets: super::anchor::AnchorOffsets,
    final_transform: Affine,
    dpi: f64,
    pick_id: PickId,
) {
    let Some(marker) = &bl.marker else { return };
    let Some(body_line) = bl.layout.lines().next() else {
        return;
    };
    let Some(marker_line) = marker.layout.lines().next() else {
        return;
    };
    let (x0, _) = marker_x_range(bl, marker);
    // Align the marker's baseline with the item's first line so a
    // bullet and its text sit on the same rule.
    let dy = bl.y_px + body_line.metrics().baseline - marker_line.metrics().baseline;
    for line in marker.layout.lines() {
        emit_line_glyphs(
            scene,
            line,
            x0,
            dy,
            0.0,
            false,
            &[],
            &[],
            palette,
            base_brush,
            dpi,
            offsets,
            final_transform,
            pick_id,
        );
    }
}
/// The accumulated baseline shift (pt) covering `run_range`. Nested
/// shifts are emitted innermost-first, so the first overlap wins.
fn baseline_shift_for_range(shifts: &[BaselineRun], run_range: &Range<usize>) -> f64 {
    for bs in shifts {
        if run_range.start < bs.range.end && bs.range.start < run_range.end {
            return bs.shift_pt;
        }
    }
    0.0
}
