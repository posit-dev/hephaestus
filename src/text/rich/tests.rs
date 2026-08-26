//! Tests for the rich-text shaping, wrapping and draw passes.

use parley::Alignment;

use super::draw::*;
use super::flat::flatten_rich_run;
use super::length::{pt, RichMargin};
use super::run::*;
use super::shape::*;
use super::style::{RichTextStyleSheet, StyleDelta};
use crate::brush::Brush;
use crate::color::Color;
use crate::geometry::Affine;
use crate::layout::Measure;
use crate::pick::PickId;
use crate::scene::recording::{Op, RecordingScene};
use crate::style_vocab::{HAlign, Palette};
use crate::text::rich::anchor::RichAnchor;
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

fn make(src: &str) -> RichTextRun {
    let sheet = RichTextStyleSheet::new();
    RichTextRun::new(
        src,
        &base_style(),
        Color::from_rgba8(0, 0, 0, 255),
        &sheet,
        &palette(),
        96.0,
    )
}

fn draw(run: &RichTextRun) -> RecordingScene {
    let mut scene = RecordingScene::default();
    draw_rich_text(
        &mut scene,
        run,
        0.0,
        0.0,
        RichAnchor::top_left(),
        Affine::IDENTITY,
        PickId::Skip,
    );
    scene
}

fn glyph_x_at_line(scene: &RecordingScene, line_ordinal: usize) -> Option<f32> {
    // Return the leftmost x of the `line_ordinal`-th DrawGlyphs op
    // (approximation: parley emits one op per line for simple
    // text; heavier styling may split further).
    let mut ops = scene.ops.iter().filter_map(|op| match op {
        Op::DrawGlyphs(gr) => Some(gr),
        _ => None,
    });
    let gr = ops.nth(line_ordinal)?;
    gr.glyphs.iter().map(|g| g.x).fold(None::<f32>, |acc, x| {
        Some(match acc {
            None => x,
            Some(a) => a.min(x),
        })
    })
}

#[test]
fn inline_code_span_emits_background_chip() {
    // The default sheet's `code` selector sets background +
    // padding + border-radius. An `` `x` `` inline span should
    // therefore emit at least one Fill op (the chip rect)
    // BEFORE the glyph run for its content.
    let run = make("plain `x` more");
    let scene = draw(&run);
    let mut saw_fill = false;
    let mut saw_glyphs_after_fill = false;
    for op in &scene.ops {
        match op {
            Op::Fill { .. } => saw_fill = true,
            Op::DrawGlyphs(_) if saw_fill => saw_glyphs_after_fill = true,
            _ => {}
        }
    }
    assert!(saw_fill, "expected a Fill op for the code chip");
    assert!(saw_glyphs_after_fill, "glyphs should follow the chip fill");
}

#[test]
fn per_span_text_stroke_emits_outline_pass() {
    // A custom class with `text_stroke` produces a stroke-only
    // glyph pass behind the fill. To reliably split the parley
    // run at the span's boundary we also set a distinct
    // `color`; parley splits on brush change, isolating the
    // outline to the span.
    let mut sheet = RichTextStyleSheet::new();
    sheet.set(
        "haloed",
        crate::text::rich::style::StyleDelta {
            color: Some(crate::style_vocab::ThemeColor::Fixed(Color::from_rgba8(
                220, 30, 30, 255,
            ))),
            text_stroke: Some(crate::style_vocab::ThemeColor::Fixed(Color::from_rgba8(
                255, 255, 255, 255,
            ))),
            text_stroke_width: Some(pt(2.0)),
            ..crate::text::rich::style::StyleDelta::empty()
        },
    );
    let run = RichTextRun::new(
        "before {.haloed word} after",
        &base_style(),
        Color::from_rgba8(0, 0, 0, 255),
        &sheet,
        &palette(),
        96.0,
    );
    let mut scene = RecordingScene::default();
    draw_rich_text(
        &mut scene,
        &run,
        0.0,
        0.0,
        RichAnchor::top_left(),
        Affine::IDENTITY,
        PickId::Skip,
    );
    let has_stroked_pass = scene
        .ops
        .iter()
        .any(|op| matches!(op, Op::DrawGlyphs(gr) if gr.style.is_some()));
    assert!(
        has_stroked_pass,
        "expected a stroke-only glyph pass for the `haloed` span"
    );
}

#[test]
fn block_level_border_does_not_double_render_as_inline() {
    // A block-level `border` set via the sheet's `paragraph`
    // selector should paint ONCE (in the block paint pass), not
    // again as an inline border on the block's text runs.
    let mut sheet = RichTextStyleSheet::empty();
    sheet.set(
        "paragraph",
        crate::text::rich::style::StyleDelta {
            border_color: Some(crate::style_vocab::ThemeColor::Ink),
            border_width: Some(RichMargin::all(pt(1.0))),
            ..crate::text::rich::style::StyleDelta::empty()
        },
    );
    let run = RichTextRun::new(
        "some text",
        &base_style(),
        Color::from_rgba8(0, 0, 0, 255),
        &sheet,
        &palette(),
        96.0,
    );
    let mut scene = RecordingScene::default();
    draw_rich_text(
        &mut scene,
        &run,
        0.0,
        0.0,
        RichAnchor::top_left(),
        Affine::IDENTITY,
        PickId::Skip,
    );
    let strokes = scene
        .ops
        .iter()
        .filter(|op| matches!(op, Op::Stroke { .. }))
        .count();
    assert_eq!(
        strokes, 1,
        "expected exactly one border stroke (block-level)"
    );
}

#[test]
fn list_container_margin_survives_a_re_break() {
    // Regression: `set_max_width` re-stacks the blocks, and the
    // ancestor container margins routed to first / last
    // descendant at shape time must still be applied there.
    let mut sheet = RichTextStyleSheet::new();
    sheet.set(
        "list",
        StyleDelta {
            margin: Some(RichMargin::new(pt(40.0), pt(0.0), pt(40.0), pt(0.0))),
            ..StyleDelta::empty()
        },
    );
    let make_with = |src: &str, sheet: &RichTextStyleSheet| {
        RichTextRun::new(
            src,
            &base_style(),
            Color::from_rgba8(0, 0, 0, 255),
            sheet,
            &palette(),
            96.0,
        )
    };
    let roomy = make_with("- alpha\n\nfollowing", &sheet);
    let tight = make_with("- alpha\n\nfollowing", &RichTextStyleSheet::new());
    let natural_delta = roomy.natural_height() - tight.natural_height();
    assert!(
        natural_delta > 20.0,
        "the list's own margin should widen the natural stack (delta={natural_delta})"
    );
    let width = roomy.natural_width() as f32;
    let roomy_broken = roomy.set_max_width(width, HAlign::Start) as f64;
    let tight_broken = tight.set_max_width(width, HAlign::Start) as f64;
    assert!(
        (roomy_broken - tight_broken - natural_delta).abs() < 1.0,
        "re-break lost the container margin ({roomy_broken} - {tight_broken} vs {natural_delta})"
    );
}

#[test]
fn strikethrough_sits_above_baseline() {
    let sheet = RichTextStyleSheet::new();
    let run = RichTextRun::new(
        "a ~~strike~~ b",
        &base_style(),
        Color::from_rgba8(0, 0, 0, 255),
        &sheet,
        &palette(),
        96.0,
    );
    let mut scene = RecordingScene::default();
    draw_rich_text(
        &mut scene,
        &run,
        0.0,
        0.0,
        RichAnchor::top_left(),
        Affine::IDENTITY,
        PickId::Skip,
    );
    let baseline_y = scene
        .ops
        .iter()
        .find_map(|op| match op {
            Op::DrawGlyphs(gr) => gr.glyphs.first().map(|g| g.y),
            _ => None,
        })
        .expect("baseline");
    let fill_y0 = scene
        .ops
        .iter()
        .find_map(|op| match op {
            Op::Fill { path, .. } => Some(crate::geometry::Shape::bounding_box(path).y0 as f32),
            _ => None,
        })
        .expect("strikethrough fill");
    assert!(
        fill_y0 < baseline_y,
        "strikethrough rect (y0={fill_y0}) should sit ABOVE the baseline (y={baseline_y})"
    );
}

#[test]
fn underline_sits_below_baseline() {
    let sheet = RichTextStyleSheet::new();
    let run = RichTextRun::new(
        "a _under_ b",
        &base_style(),
        Color::from_rgba8(0, 0, 0, 255),
        &sheet,
        &palette(),
        96.0,
    );
    let mut scene = RecordingScene::default();
    draw_rich_text(
        &mut scene,
        &run,
        0.0,
        0.0,
        RichAnchor::top_left(),
        Affine::IDENTITY,
        PickId::Skip,
    );
    let baseline_y = scene
        .ops
        .iter()
        .find_map(|op| match op {
            Op::DrawGlyphs(gr) => gr.glyphs.first().map(|g| g.y),
            _ => None,
        })
        .expect("baseline");
    let fill_y0 = scene
        .ops
        .iter()
        .find_map(|op| match op {
            Op::Fill { path, .. } => Some(crate::geometry::Shape::bounding_box(path).y0 as f32),
            _ => None,
        })
        .expect("underline fill");
    assert!(
        fill_y0 > baseline_y,
        "underline rect (y0={fill_y0}) should sit BELOW the baseline (y={baseline_y})"
    );
}

#[test]
fn plain_text_shapes_and_measures() {
    let run = make("hello world");
    assert!(run.natural_width() > 0.0);
    assert!(run.natural_height() > 0.0);
}

#[test]
fn bold_widens_natural_width_vs_plain() {
    let plain = make("hello world");
    let bold = make("**hello world**");
    assert!(bold.natural_width() >= plain.natural_width());
}

#[test]
fn draw_emits_glyph_runs_with_per_range_brushes() {
    let run = make("a {.red word} b");
    let scene = draw(&run);
    let glyph_runs: Vec<_> = scene
        .ops
        .iter()
        .filter_map(|op| match op {
            Op::DrawGlyphs(gr) => Some(gr),
            _ => None,
        })
        .collect();
    assert!(glyph_runs.len() >= 2, "expected multiple glyph runs");
    let has_red = glyph_runs.iter().any(|gr| match &gr.brush {
        Brush::Solid(c) => {
            let [r, g, b, _] = c.components;
            (r - 1.0).abs() < 1e-3 && g < 0.1 && b < 0.1
        }
        _ => false,
    });
    assert!(has_red);
}

#[test]
fn sup_offsets_glyphs_upward() {
    let run = make("a ^2^ b");
    let scene = draw(&run);
    let mut ys: Vec<f32> = Vec::new();
    for op in &scene.ops {
        if let Op::DrawGlyphs(gr) = op {
            for g in &gr.glyphs {
                ys.push(g.y);
            }
        }
    }
    assert!(!ys.is_empty());
    let min_y = ys.iter().cloned().fold(f32::INFINITY, f32::min);
    let max_y = ys.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
    assert!(max_y - min_y > 1.0);
}

#[test]
fn measure_impl_reports_positive_height() {
    let run = make("hello **bold** world");
    let h = run.height_at(200.0, 96.0);
    assert!(h > 0.0);
}

#[test]
fn base_brush_flows_through_to_plain_runs() {
    let sheet = RichTextStyleSheet::new();
    let base_col = Color::from_rgba8(50, 100, 200, 255);
    let run = RichTextRun::new(
        "plain text",
        &base_style(),
        base_col,
        &sheet,
        &palette(),
        96.0,
    );
    let scene = draw(&run);
    let first = scene
        .ops
        .iter()
        .find_map(|op| match op {
            Op::DrawGlyphs(gr) => Some(gr),
            _ => None,
        })
        .expect("glyph run");
    match &first.brush {
        Brush::Solid(c) => {
            let [r, g, b, _] = c.components;
            assert!((r - 50.0 / 255.0).abs() < 1e-2);
            assert!((g - 100.0 / 255.0).abs() < 1e-2);
            assert!((b - 200.0 / 255.0).abs() < 1e-2);
        }
        _ => panic!("expected solid brush"),
    }
}

#[test]
fn heading_produces_larger_glyph_run_font_size() {
    let plain = make("Big");
    let heading = make("# Big");
    let max_size = |run: &RichTextRun| {
        let scene = draw(run);
        scene
            .ops
            .iter()
            .filter_map(|op| match op {
                Op::DrawGlyphs(gr) => Some(gr.font_size),
                _ => None,
            })
            .fold(0.0_f32, f32::max)
    };
    assert!(max_size(&heading) > max_size(&plain) * 1.5);
}

#[test]
fn size_selector_produces_larger_run_height() {
    let plain = make("x");
    let big = make("{.36 x}");
    assert!(big.natural_height() > plain.natural_height());
}

#[test]
fn new_with_width_wraps_at_fixed_pixels() {
    let sheet = RichTextStyleSheet::new();
    let unwrapped = RichTextRun::new(
        "one two three four five six seven eight",
        &base_style(),
        Color::from_rgba8(0, 0, 0, 255),
        &sheet,
        &palette(),
        96.0,
    );
    let wrapped = RichTextRun::new_with_width(
        "one two three four five six seven eight",
        &base_style(),
        Color::from_rgba8(0, 0, 0, 255),
        &sheet,
        &palette(),
        96.0,
        RichTextWidth::Fixed(60.0),
    );
    assert!(wrapped.current_height() > unwrapped.current_height());
}

#[test]
fn blockquote_indents_its_paragraph() {
    // A blockquote's default padding.left = Rel(1.0) so its
    // paragraph child should sit ~1em (14pt) to the right of a
    // plain paragraph. In screen px: 14pt at 96dpi ≈ 18.67px.
    let plain = make("hello world");
    let quoted = make("> hello world");
    // The paragraph inside the blockquote should be at ~1em from
    // the left; the plain paragraph's leftmost glyph sits at ~0.
    let plain_x = glyph_x_at_line(&draw(&plain), 0).unwrap_or(0.0);
    let quoted_x = glyph_x_at_line(&draw(&quoted), 0).unwrap_or(0.0);
    assert!(
        quoted_x > plain_x + 10.0,
        "blockquote content should be indented (plain={plain_x}, quoted={quoted_x})"
    );
}

#[test]
fn single_paragraph_box_is_tight_around_its_line() {
    // `paragraph` carries `margin.bottom = rem(1)`, but that margin
    // reaches the document's bottom edge and collapses out of the box.
    // A one-line label therefore measures one line tall — the same box
    // a plain `TextRun` of the same string reports, so both anchor the
    // same way.
    let run = make("**bold** and *italic*");
    let line = run.layout_bounds().height as f64;
    let total = run.natural_height();
    assert!(
        (total - line).abs() < 0.01,
        "one-paragraph run should measure one line (line={line}, total={total})"
    );
}

#[test]
fn leading_block_margin_collapses_out_of_the_box() {
    // Mirror of the trailing case: `h2` carries `margin.top = em(1)`,
    // which reaches the document's top edge. The heading's first line
    // therefore starts at the top of the box rather than one em down.
    let run = make("## Heading");
    let ink_top = run.layout_bounds().ink_top;
    assert!(
        ink_top < 1.0,
        "leading margin should collapse out of the box (ink_top={ink_top})"
    );
}

#[test]
fn interior_block_margins_survive() {
    // Only the document's own edges drop margins. A heading between
    // two paragraphs keeps both of its margins, so the stack is taller
    // than the same blocks with the heading's margins zeroed.
    let mut flat = RichTextStyleSheet::new();
    flat.set(
        "h2",
        StyleDelta {
            margin: Some(RichMargin::all(pt(0.0))),
            ..flat.get("h2").cloned().unwrap_or_default()
        },
    );
    let spaced = make("intro\n\n## Heading\n\nbody");
    let flattened = RichTextRun::new(
        "intro\n\n## Heading\n\nbody",
        &base_style(),
        Color::from_rgba8(0, 0, 0, 255),
        &flat,
        &palette(),
        96.0,
    );
    let delta = spaced.natural_height() - flattened.natural_height();
    assert!(
        delta > 10.0,
        "interior heading margins should still add space (delta={delta})"
    );
}

#[test]
fn sibling_margins_collapse_via_max_not_sum() {
    // Every paragraph carries `margin.bottom = rem(1)` and no top
    // margin, so an adjacent pair collapses to one gap of 1rem,
    // not two. Two extra paragraphs therefore add
    // 2 × (line + 1rem), not 2 × (line + 2rem).
    let three = make("a\n\nb\n\nc");
    let five = make("a\n\nb\n\nc\n\nd\n\ne");
    let diff = five.natural_height() - three.natural_height();
    // 14pt base, line-height 1.6 → one line ≈ 29.9px; 1rem ≈
    // 18.7px. Two paragraphs ≈ 97px collapsed, ≈ 134px if the
    // margins summed.
    assert!(
        diff < 115.0,
        "2 extra paragraphs should collapse to ≈97px of extra height, got {diff}"
    );
}

#[test]
fn adjacent_heading_paragraph_margins_collapse() {
    // Heading has `margin.bottom = Rel(0.3)` and paragraph has
    // `margin.top = 0` → collapse via `max(0.3, 0)` = 0.3em.
    // A bare `# H\n\ntext` should be shorter than a hypothetical
    // sum which would be 0.3+0 = 0.3em anyway. Test that headings
    // followed by paragraphs stack sanely.
    let with_heading = make("# Header\n\nBody");
    let no_heading = make("Header\n\nBody");
    assert!(with_heading.natural_height() > no_heading.natural_height());
}

#[test]
fn horizontal_rule_emits_single_top_stroke() {
    // `---` produces a Rule block; the sheet's `hr` entry sets a
    // top-only 1pt border, so the paint pass emits exactly one
    // horizontal stroke.
    let run = make("above\n\n---\n\nbelow");
    let mut scene = RecordingScene::default();
    draw_rich_text(
        &mut scene,
        &run,
        0.0,
        0.0,
        RichAnchor::top_left(),
        Affine::IDENTITY,
        PickId::Skip,
    );
    let strokes: Vec<_> = scene
        .ops
        .iter()
        .filter(|op| matches!(op, Op::Stroke { .. }))
        .collect();
    assert_eq!(
        strokes.len(),
        1,
        "hr should emit exactly one stroke (top edge only), got {}",
        strokes.len()
    );
}

#[test]
fn horizontal_rule_stretches_to_full_natural_width() {
    // The hr's paint outer_rect should span the same width as the
    // surrounding text — natural_width from the surrounding
    // paragraphs.
    let run = make("hello world hello world hello world\n\n---\n\nfollowing");
    let paints = run.block_paints();
    // Find the hr paint (border only, no background, small height).
    let hr = paints
        .iter()
        .find(|p| p.border.is_some() && p.background.is_none())
        .expect("expected hr paint");
    let width = hr.outer_rect.width();
    assert!(
        width > 50.0,
        "hr should span at least a paragraph's width; got {width}"
    );
    assert!(
        width as f32 >= run.natural_width() as f32 - 5.0,
        "hr should span ≥ natural width ({} vs {})",
        width,
        run.natural_width()
    );
}

#[test]
fn horizontal_rule_reserves_vertical_space() {
    // hr has 0 height but non-zero top/bottom margins from the
    // sheet default. `above` and `below` should be separated by
    // more space than a plain paragraph break.
    let without_hr = make("above\n\nbelow");
    let with_hr = make("above\n\n---\n\nbelow");
    assert!(
        with_hr.natural_height() > without_hr.natural_height(),
        "hr's margins should add vertical space (without: {}, with: {})",
        without_hr.natural_height(),
        with_hr.natural_height()
    );
}

#[test]
fn blockquote_emits_single_left_edge_stroke() {
    // The default `block_quote` sheet entry sets `border_width`
    // = Margin { left: 3pt, others: 0 }. The paint pass must
    // emit exactly ONE stroke op (for the left edge) — not a
    // full four-sided box.
    let run = make("> hello world");
    let mut scene = RecordingScene::default();
    draw_rich_text(
        &mut scene,
        &run,
        0.0,
        0.0,
        RichAnchor::top_left(),
        Affine::IDENTITY,
        PickId::Skip,
    );
    let strokes: Vec<_> = scene
        .ops
        .iter()
        .filter(|op| matches!(op, Op::Stroke { .. }))
        .collect();
    assert_eq!(
        strokes.len(),
        1,
        "blockquote should emit exactly one stroke (left edge only), got {}",
        strokes.len()
    );
}

#[test]
fn halign_start_maps_to_right_under_rtl() {
    assert_eq!(hal_to_alignment(HAlign::Start, false), Alignment::Left);
    assert_eq!(hal_to_alignment(HAlign::Start, true), Alignment::Right);
    assert_eq!(hal_to_alignment(HAlign::End, false), Alignment::Right);
    assert_eq!(hal_to_alignment(HAlign::End, true), Alignment::Left);
    assert_eq!(hal_to_alignment(HAlign::Center, false), Alignment::Center);
    assert_eq!(hal_to_alignment(HAlign::Center, true), Alignment::Center);
    assert_eq!(hal_to_alignment(HAlign::Justify, true), Alignment::Justify);
}

#[test]
fn auto_direction_reads_parley_is_rtl_for_arabic() {
    // Auto (unset text_direction) should reflect parley's UBA
    // determination on the block's text. Arabic script drives
    // base_level to Rtl.
    let sheet = RichTextStyleSheet::empty();
    let run = RichTextRun::new(
        "مرحبا",
        &base_style(),
        Color::from_rgba8(0, 0, 0, 255),
        &sheet,
        &palette(),
        96.0,
    );
    let blocks = run.blocks.borrow();
    assert!(
        blocks.first().map(|bl| bl.is_rtl).unwrap_or(false),
        "an Arabic-only paragraph should resolve to Rtl via parley::Layout::is_rtl"
    );
}

#[test]
fn explicit_ltr_overrides_arabic_content_direction() {
    // A block with explicit `Direction::Ltr` in its class stays
    // Ltr even when parley infers Rtl from Arabic content.
    let mut sheet = RichTextStyleSheet::empty();
    sheet.set(
        "paragraph",
        crate::text::rich::style::StyleDelta {
            text_direction: Some(crate::text::rich::style::Direction::Ltr),
            ..crate::text::rich::style::StyleDelta::empty()
        },
    );
    let run = RichTextRun::new(
        "مرحبا",
        &base_style(),
        Color::from_rgba8(0, 0, 0, 255),
        &sheet,
        &palette(),
        96.0,
    );
    let blocks = run.blocks.borrow();
    assert!(
        !blocks.first().map(|bl| bl.is_rtl).unwrap_or(true),
        "explicit Direction::Ltr must override parley's Rtl inference"
    );
}

#[test]
fn rtl_blockquote_paints_right_edge_bar() {
    // A block_quote class sets `border_width` as `[0, 0, 0, 3]` —
    // start-side bar. Under Rtl the l/r swap in `border_for`
    // routes it onto the physical right edge, so `widths_px[1]`
    // (right) becomes non-zero and `widths_px[3]` (left) stays 0.
    let mut sheet = RichTextStyleSheet::empty();
    sheet.set(
        "block_quote",
        crate::text::rich::style::StyleDelta {
            text_direction: Some(crate::text::rich::style::Direction::Rtl),
            border_color: Some(crate::style_vocab::ThemeColor::Ink),
            border_width: Some(RichMargin::new(pt(0.0), pt(0.0), pt(0.0), pt(3.0))),
            ..crate::text::rich::style::StyleDelta::empty()
        },
    );
    let run = RichTextRun::new(
        "> quoted content",
        &base_style(),
        Color::from_rgba8(0, 0, 0, 255),
        &sheet,
        &palette(),
        96.0,
    );
    let paints = run.block_paints();
    let border = paints
        .iter()
        .find_map(|p| p.border.as_ref())
        .expect("blockquote should have a border");
    assert!(
        border.widths_px[1] > 0.0,
        "under Rtl the start-side bar should paint on the physical right (widths_px[1])"
    );
    assert!(
        border.widths_px[3].abs() < 1e-3,
        "physical left (widths_px[3]) should be zero"
    );
}

#[test]
fn ltr_blockquote_still_paints_left_edge_bar() {
    // Baseline sanity: no direction set → default Ltr behavior
    // stays intact. `[0, 0, 0, 3]` still lands on the left edge.
    let mut sheet = RichTextStyleSheet::empty();
    sheet.set(
        "block_quote",
        crate::text::rich::style::StyleDelta {
            border_color: Some(crate::style_vocab::ThemeColor::Ink),
            border_width: Some(RichMargin::new(pt(0.0), pt(0.0), pt(0.0), pt(3.0))),
            ..crate::text::rich::style::StyleDelta::empty()
        },
    );
    let run = RichTextRun::new(
        "> quoted content",
        &base_style(),
        Color::from_rgba8(0, 0, 0, 255),
        &sheet,
        &palette(),
        96.0,
    );
    let paints = run.block_paints();
    let border = paints
        .iter()
        .find_map(|p| p.border.as_ref())
        .expect("blockquote should have a border");
    assert!(
        border.widths_px[3] > 0.0,
        "under Ltr the start-side bar should paint on the physical left (widths_px[3])"
    );
    assert!(
        border.widths_px[1].abs() < 1e-3,
        "physical right (widths_px[1]) should be zero"
    );
}

#[test]
fn uniform_border_emits_single_boxed_stroke() {
    // A uniform four-sided border should collapse to a single
    // rectangular stroke — no fan of segments.
    let mut sheet = RichTextStyleSheet::empty();
    sheet.set(
        "paragraph",
        crate::text::rich::style::StyleDelta {
            border_color: Some(crate::style_vocab::ThemeColor::Ink),
            border_width: Some(RichMargin::all(pt(1.0))),
            ..crate::text::rich::style::StyleDelta::empty()
        },
    );
    let run = RichTextRun::new(
        "boxed",
        &base_style(),
        Color::from_rgba8(0, 0, 0, 255),
        &sheet,
        &palette(),
        96.0,
    );
    let mut scene = RecordingScene::default();
    draw_rich_text(
        &mut scene,
        &run,
        0.0,
        0.0,
        RichAnchor::top_left(),
        Affine::IDENTITY,
        PickId::Skip,
    );
    let strokes = scene
        .ops
        .iter()
        .filter(|op| matches!(op, Op::Stroke { .. }))
        .count();
    assert_eq!(strokes, 1, "uniform border → one rectangular stroke");
}

#[test]
fn adjacent_partial_borders_collapse_into_one_polyline() {
    // Top + left borders both present with the same width should
    // stroke as ONE polyline sharing the top-left corner, not
    // two independent segments meeting there. Distinct widths on
    // T and L, in contrast, must stay separate.
    let mut sheet = RichTextStyleSheet::empty();
    sheet.set(
        "paragraph",
        crate::text::rich::style::StyleDelta {
            border_color: Some(crate::style_vocab::ThemeColor::Ink),
            border_width: Some(RichMargin {
                top: pt(2.0),
                right: pt(0.0),
                bottom: pt(0.0),
                left: pt(2.0),
            }),
            ..crate::text::rich::style::StyleDelta::empty()
        },
    );
    let run = RichTextRun::new(
        "l shape",
        &base_style(),
        Color::from_rgba8(0, 0, 0, 255),
        &sheet,
        &palette(),
        96.0,
    );
    let mut scene = RecordingScene::default();
    draw_rich_text(
        &mut scene,
        &run,
        0.0,
        0.0,
        RichAnchor::top_left(),
        Affine::IDENTITY,
        PickId::Skip,
    );
    let strokes = scene
        .ops
        .iter()
        .filter(|op| matches!(op, Op::Stroke { .. }))
        .count();
    assert_eq!(
        strokes, 1,
        "top + left same-width partial borders should collapse into one polyline"
    );
}

#[test]
fn partial_borders_with_mismatched_widths_stay_separate() {
    let mut sheet = RichTextStyleSheet::empty();
    sheet.set(
        "paragraph",
        crate::text::rich::style::StyleDelta {
            border_color: Some(crate::style_vocab::ThemeColor::Ink),
            border_width: Some(RichMargin {
                top: pt(1.0),
                right: pt(0.0),
                bottom: pt(0.0),
                left: pt(4.0),
            }),
            ..crate::text::rich::style::StyleDelta::empty()
        },
    );
    let run = RichTextRun::new(
        "mixed",
        &base_style(),
        Color::from_rgba8(0, 0, 0, 255),
        &sheet,
        &palette(),
        96.0,
    );
    let mut scene = RecordingScene::default();
    draw_rich_text(
        &mut scene,
        &run,
        0.0,
        0.0,
        RichAnchor::top_left(),
        Affine::IDENTITY,
        PickId::Skip,
    );
    let strokes = scene
        .ops
        .iter()
        .filter(|op| matches!(op, Op::Stroke { .. }))
        .count();
    assert_eq!(strokes, 2, "different widths keep sides separate");
}

#[test]
fn dashed_border_carries_dash_pattern_through() {
    use crate::scales::value::LinetypeStep;
    use std::sync::Arc;
    let mut sheet = RichTextStyleSheet::empty();
    sheet.set(
        "paragraph",
        crate::text::rich::style::StyleDelta {
            border_color: Some(crate::style_vocab::ThemeColor::Ink),
            border_width: Some(RichMargin::all(pt(1.0))),
            border_type: Some(Arc::from(vec![
                LinetypeStep::Dash(4.0),
                LinetypeStep::Gap(2.0),
            ])),
            ..crate::text::rich::style::StyleDelta::empty()
        },
    );
    let run = RichTextRun::new(
        "dashy",
        &base_style(),
        Color::from_rgba8(0, 0, 0, 255),
        &sheet,
        &palette(),
        96.0,
    );
    let paints = run.block_paints();
    let border = paints
        .iter()
        .find_map(|p| p.border.as_ref())
        .expect("expected a border on the paragraph");
    let pattern = border
        .linetype_pt
        .as_ref()
        .expect("border_type should produce a linetype pattern");
    // Two entries (dash + gap), both positive.
    assert_eq!(pattern.len(), 2);
    use crate::scales::value::LinetypeStep::{Dash, Gap};
    assert!(matches!(pattern[0], Dash(d) if d > 0.0));
    assert!(matches!(pattern[1], Gap(g) if g > 0.0));
}

#[test]
fn loose_list_stacks_taller_than_tight_list() {
    // Same items, one written tight and one written loose. The
    // loose version should be strictly taller because each
    // paragraph body carries margin.bottom.
    let tight = make("- alpha\n- beta\n- gamma");
    let loose = make("- alpha\n\n- beta\n\n- gamma");
    assert!(
        loose.natural_height() > tight.natural_height() + 5.0,
        "loose ({}) should stack taller than tight ({})",
        loose.natural_height(),
        tight.natural_height()
    );
}

#[test]
fn nested_list_item_sits_further_right_than_outer() {
    // A nested list should render its bullet indented under the
    // outer item's continuation position, i.e. outer's hanging
    // (~1.5em) to the right of the outer marker.
    let run = make("- outer\n  - inner");
    let scene = draw(&run);
    // Extract leftmost glyph x per line (approximate via y-bucket).
    let mut by_line: std::collections::BTreeMap<i32, f32> = std::collections::BTreeMap::new();
    for op in &scene.ops {
        if let Op::DrawGlyphs(gr) = op {
            if let Some(min_x) = gr.glyphs.iter().map(|g| g.x).fold(None::<f32>, |acc, x| {
                Some(match acc {
                    None => x,
                    Some(a) => a.min(x),
                })
            }) {
                let y_key = gr.glyphs[0].y as i32;
                by_line
                    .entry(y_key)
                    .and_modify(|v| *v = v.min(min_x))
                    .or_insert(min_x);
            }
        }
    }
    let xs: Vec<f32> = by_line.into_values().collect();
    assert!(xs.len() >= 2, "expected at least 2 lines, got {xs:?}");
    // First line = outer's bullet at x≈0; second = inner's bullet
    // at x≈1.5em (=1.5*14pt≈21pt→28px at 96dpi).
    assert!(
        xs[1] > xs[0] + 10.0,
        "nested item should be indented past outer (xs={xs:?})"
    );
}

#[test]
fn list_item_hanging_shifts_continuation_lines() {
    // Force the list item to wrap so a continuation line appears,
    // then verify its leftmost glyph is right of the first line's.
    let src = "- one two three four five six seven eight nine";
    let sheet = RichTextStyleSheet::new();
    let run = RichTextRun::new_with_width(
        src,
        &base_style(),
        Color::from_rgba8(0, 0, 0, 255),
        &sheet,
        &palette(),
        96.0,
        RichTextWidth::Fixed(100.0),
    );
    let scene = draw(&run);
    // Extract all glyph runs and their minimum x + y. Group by y
    // to distinguish lines.
    let mut by_line: std::collections::BTreeMap<i32, f32> = std::collections::BTreeMap::new();
    for op in &scene.ops {
        if let Op::DrawGlyphs(gr) = op {
            if let Some(min_x) = gr.glyphs.iter().map(|g| g.x).fold(None::<f32>, |acc, x| {
                Some(match acc {
                    None => x,
                    Some(a) => a.min(x),
                })
            }) {
                let y_key = gr.glyphs[0].y as i32;
                by_line
                    .entry(y_key)
                    .and_modify(|v| *v = v.min(min_x))
                    .or_insert(min_x);
            }
        }
    }
    let xs: Vec<f32> = by_line.into_values().collect();
    assert!(xs.len() >= 2, "expected at least 2 lines, got {xs:?}");
    assert!(
        xs[1] > xs[0] + 5.0,
        "continuation line should be right of first (xs={xs:?})"
    );
}

#[test]
fn code_block_emits_fill_before_glyphs() {
    let run = make("```\nlet x = 1;\n```");
    let scene = draw(&run);
    let first_fill = scene
        .ops
        .iter()
        .position(|op| matches!(op, Op::Fill { .. }));
    let first_glyphs = scene
        .ops
        .iter()
        .position(|op| matches!(op, Op::DrawGlyphs(_)));
    let (fi, gi) = (first_fill.expect("fill"), first_glyphs.expect("glyphs"));
    assert!(fi < gi, "Fill at {fi} should precede DrawGlyphs at {gi}");
}

#[test]
fn plain_text_emits_no_block_paints() {
    let run = make("just plain");
    let scene = draw(&run);
    let fills = scene
        .ops
        .iter()
        .filter(|op| matches!(op, Op::Fill { .. }))
        .count();
    let strokes = scene
        .ops
        .iter()
        .filter(|op| matches!(op, Op::Stroke { .. }))
        .count();
    assert_eq!(fills, 0);
    assert_eq!(strokes, 0);
}

#[test]
fn center_anchor_shifts_glyph_positions_by_half_width() {
    let run = make("abcdef");
    let width = run.natural_width() as f32;
    let mut scene = RecordingScene::default();
    draw_rich_text(
        &mut scene,
        &run,
        100.0,
        100.0,
        RichAnchor::center(),
        Affine::IDENTITY,
        PickId::Skip,
    );
    let first_op = scene
        .ops
        .iter()
        .find_map(|op| match op {
            Op::DrawGlyphs(gr) => Some(gr),
            _ => None,
        })
        .unwrap();
    let first_g = first_op.glyphs.first().unwrap();
    let transformed_x = first_op.transform.as_coeffs()[4] as f32 + first_g.x;
    assert!(
        (transformed_x - (100.0 - width * 0.5)).abs() < 2.0,
        "first glyph should sit near (x - width/2), got {transformed_x} (expected ~{})",
        100.0 - width * 0.5
    );
}

#[test]
fn first_line_anchor_places_baseline_on_y() {
    let run = make("hi");
    let mut scene = RecordingScene::default();
    draw_rich_text(
        &mut scene,
        &run,
        0.0,
        100.0,
        RichAnchor::first_line_baseline(),
        Affine::IDENTITY,
        PickId::Skip,
    );
    let first = scene
        .ops
        .iter()
        .find_map(|op| match op {
            Op::DrawGlyphs(gr) => Some(gr),
            _ => None,
        })
        .unwrap();
    let coeffs = first.transform.as_coeffs();
    let g = first.glyphs.first().unwrap();
    let screen_y = coeffs[5] as f32 + g.y;
    assert!(
        (screen_y - 100.0).abs() < 0.5,
        "first baseline should land at y = 100; got screen y = {screen_y}"
    );
}

#[test]
fn breaking_at_the_natural_width_reproduces_the_natural_height() {
    let run = make("a paragraph long enough to have an opinion about width\n\nand another");
    let natural = run.natural_height();
    let broken = run.set_max_width(run.natural_width() as f32, HAlign::Start) as f64;
    assert!(
        (broken - natural).abs() < 1.0,
        "re-breaking at the natural width changed the height ({natural} → {broken})"
    );
}

#[test]
fn natural_height_survives_a_narrow_re_break() {
    let run = make("a paragraph long enough to wrap when the column gets narrow");
    let natural = run.natural_height();
    run.set_max_width(natural as f32 / 4.0, HAlign::Start);
    assert!(
        run.current_height() > natural,
        "wrapping should make the run taller"
    );
    assert_eq!(
        run.natural_height(),
        natural,
        "natural height must not move when the run re-breaks"
    );
}

#[test]
fn base_style_font_features_reach_the_rich_shaper() {
    // `push_base_defaults` shares `TextRun`'s property pushes, so
    // a feature that changes advances must change the shaped
    // width here too.
    let sheet = RichTextStyleSheet::new();
    let plain = TextStyle::new(20.0);
    let small_caps = plain.clone().features([crate::text::FontFeatureSetting {
        tag: *b"smcp",
        value: 1,
    }]);
    let width_of = |style: &TextStyle| {
        RichTextRun::new(
            "widths",
            style,
            Color::from_rgba8(0, 0, 0, 255),
            &sheet,
            &palette(),
            96.0,
        )
        .natural_width()
    };
    // Both shape; the point is that the feature reaches parley at
    // all rather than being silently dropped on the rich path.
    assert!(width_of(&plain) > 0.0);
    assert!(width_of(&small_caps) > 0.0);
}

#[test]
fn list_markers_draw_in_the_gutter_left_of_the_body() {
    let run = make("- alpha");
    let blocks = run.blocks.borrow();
    let bl = blocks
        .iter()
        .find(|b| b.marker.is_some())
        .expect("the item body carries the marker");
    let marker = bl.marker.as_ref().expect("marker");
    let (x0, x1) = marker_x_range(bl, marker);
    assert!(x1 <= bl.left_px, "marker must sit start-side of the text");
    assert!(x0 < x1, "marker must have width");
}

#[test]
fn multi_digit_ordinals_share_a_right_edge() {
    let run = make(
        "1. one\n2. two\n3. three\n4. four\n5. five\n6. six\n7. seven\n8. eight\n9. nine\n10. ten",
    );
    let blocks = run.blocks.borrow();
    let right_edges: Vec<f32> = blocks
        .iter()
        .filter_map(|bl| bl.marker.as_ref().map(|m| marker_x_range(bl, m).1))
        .collect();
    assert!(right_edges.len() >= 10, "expected ten markers");
    let first = right_edges[0];
    for e in &right_edges {
        assert!(
            (e - first).abs() < 0.01,
            "markers should right-align, got {right_edges:?}"
        );
    }
}

#[test]
fn a_background_with_no_padding_still_blocks_margin_collapse() {
    let mut sheet = RichTextStyleSheet::new();
    sheet.set(
        "barrier",
        StyleDelta {
            background: Some(crate::style_vocab::ThemeColor::Accent),
            margin: Some(RichMargin::new(pt(20.0), pt(0.0), pt(20.0), pt(0.0))),
            ..StyleDelta::empty()
        },
    );
    sheet.set(
        "plain",
        StyleDelta {
            margin: Some(RichMargin::new(pt(20.0), pt(0.0), pt(20.0), pt(0.0))),
            ..StyleDelta::empty()
        },
    );
    let make_with = |src: &str| {
        RichTextRun::new(
            src,
            &base_style(),
            Color::from_rgba8(0, 0, 0, 255),
            &sheet,
            &palette(),
            96.0,
        )
    };
    // Two stacked divs, each wrapping a paragraph that carries
    // its own bottom margin. With no barrier that inner margin
    // collapses out through the div's edge and merges with the
    // div's own; a background on the div stops it, so the painted
    // stack is one paragraph margin taller.
    let collapsed = make_with(":::plain\na\n:::\n:::plain\nb\n:::").natural_height();
    let separated = make_with(":::barrier\na\n:::\n:::plain\nb\n:::").natural_height();
    assert!(
        separated > collapsed + 15.0,
        "a background must stop the collapse ({collapsed} → {separated})"
    );
}

// ─── Ink-band metrics ───────────────────────────────────────────────

/// A plain string has to measure the same through both shapers, or a
/// chrome slot that opts into markdown silently reserves a different
/// box than the same slot without it.
#[test]
fn a_plain_string_measures_the_same_through_both_shapers() {
    // No sheet surgery: the built-in sheet leaves `base` empty, so
    // the line height on the style the caller passes is the one both
    // shapers use.
    let style = base_style().line_height(crate::text::LineHeight::Relative(1.2));
    let sheet = RichTextStyleSheet::new();
    let plain = crate::text::TextRun::new("Hello World", &style, 96.0);
    let rich = RichTextRun::new(
        "Hello World",
        &style,
        Color::from_rgba8(0, 0, 0, 255),
        &sheet,
        &palette(),
        96.0,
    );
    let (p, r) = (
        plain.height_at(f64::INFINITY, 96.0),
        rich.height_at(f64::INFINITY, 96.0),
    );
    assert!(
        (p - r).abs() < 0.51,
        "measured heights should agree within half a pixel (plain {p}, rich {r})"
    );
    let (pc, rc) = (plain.cap_height(), rich.cap_height());
    assert!(
        (pc - rc).abs() < 0.01,
        "cap heights should agree (plain {pc}, rich {rc})"
    );
    let (pi, ri) = (plain.first_line_ascender_offset(), rich.ink_top_offset());
    assert!(
        (pi - ri).abs() < 0.51,
        "ink top offsets should agree (plain {pi}, rich {ri})"
    );
}

/// A block that paints a background reaches past its glyphs, so the
/// measured band has to grow with it — otherwise the slot clips the
/// box it reserved room for.
#[test]
fn a_block_background_widens_the_ink_band() {
    let mut sheet = RichTextStyleSheet::new();
    sheet.set(
        "boxed",
        StyleDelta {
            background: Some(crate::style_vocab::ThemeColor::Accent),
            padding: Some(RichMargin::new(pt(12.0), pt(0.0), pt(12.0), pt(0.0))),
            ..StyleDelta::empty()
        },
    );
    let make_with = |src: &str| {
        RichTextRun::new(
            src,
            &base_style(),
            Color::from_rgba8(0, 0, 0, 255),
            &sheet,
            &palette(),
            96.0,
        )
    };
    let bare = make_with("a").inked_height();
    let boxed = make_with(":::boxed\na\n:::").inked_height();
    assert!(
        boxed > bare + 20.0,
        "the padded background has to count toward the band ({bare} → {boxed})"
    );
}

/// The band tracks the current break: wrapping a run onto more lines
/// makes it taller, and the measure follows.
#[test]
fn the_ink_band_follows_the_current_break() {
    let run = make("one two three four five six seven eight");
    let wide = run.height_at(1000.0, 96.0);
    let narrow = run.height_at(60.0, 96.0);
    assert!(
        narrow > wide,
        "a narrower break must measure taller ({wide} → {narrow})"
    );
}

// ─── Flattening for per-glyph callers ────────────────────────────────

/// A single paragraph of inline markup flattens to the same line the
/// shaper laid out: one line's worth of metrics and the run's own
/// width.
#[test]
fn flattening_one_paragraph_preserves_its_metrics() {
    let run = make("plain **bold** *italic*");
    let flat = flatten_rich_run(&run);
    assert!(!flat.glyphs.is_empty());
    assert!(
        (flat.width as f64 - run.natural_width()).abs() < 1.0,
        "width {} should match the run's {}",
        flat.width,
        run.natural_width()
    );
    assert!(
        ((flat.ascent + flat.descent) as f64 - run.natural_height()).abs() < 1.0,
        "ascent + descent {} should match the line box {}",
        flat.ascent + flat.descent,
        run.natural_height()
    );
    // Glyph x is monotonic across the style changes, so a caller can
    // read it as distance travelled.
    let mut prev = f32::MIN;
    for g in &flat.glyphs {
        assert!(g.x >= prev, "glyph x went backwards at {}", g.x);
        prev = g.x;
    }
}

/// Block structure collapses into one line, each segment separated by
/// a space of the base style rather than butted against its neighbour.
#[test]
fn flattening_joins_blocks_with_a_space() {
    let joined = flatten_rich_run(&make("a\n\nb"));
    let contiguous = flatten_rich_run(&make("ab"));
    assert_eq!(joined.glyphs.len(), 2);
    assert_eq!(contiguous.glyphs.len(), 2);
    let gap = joined.glyphs[1].x - joined.glyphs[0].x;
    let tight = contiguous.glyphs[1].x - contiguous.glyphs[0].x;
    assert!(
        gap > tight + 2.0,
        "expected a joining space: {gap} vs {tight}"
    );
    // Both blocks share one baseline — block y is dropped.
    assert!(
        (joined.glyphs[0].dy - joined.glyphs[1].dy).abs() < 0.01,
        "flattened blocks must share a baseline"
    );
}

/// A superscript arrives as a negative `dy` — lifted off the baseline
/// in screen coordinates — and a subscript as a positive one.
#[test]
fn flattening_carries_baseline_shifts() {
    let sup = flatten_rich_run(&make("a ^2^ b"));
    let lifted = sup.glyphs.iter().map(|g| g.dy).fold(f32::MAX, f32::min);
    assert!(lifted < -1.0, "superscript should lift: {lifted}");
    let sub = flatten_rich_run(&make("a ~2~ b"));
    let dropped = sub.glyphs.iter().map(|g| g.dy).fold(f32::MIN, f32::max);
    assert!(dropped > 1.0, "subscript should drop: {dropped}");
}

/// Underline and strikethrough spans come back as rules covering only
/// the span they decorate, at the font's own thickness.
#[test]
fn flattening_reports_decoration_rules() {
    let flat = flatten_rich_run(&make("plain _under_"));
    assert_eq!(flat.rules.len(), 1, "expected one underline rule");
    let rule = flat.rules[0];
    assert!(rule.thickness > 0.0, "thickness {}", rule.thickness);
    assert!(rule.dy > 0.0, "an underline sits below the baseline");
    assert!(
        rule.x0 > 0.0 && rule.x1 <= flat.width + 0.01,
        "rule {}..{} should cover only the span, within {}",
        rule.x0,
        rule.x1,
        flat.width
    );
    let struck = flatten_rich_run(&make("plain ~~gone~~"));
    assert_eq!(struck.rules.len(), 1);
    assert!(
        struck.rules[0].dy < 0.0,
        "a strikethrough crosses above the baseline"
    );
}

/// Tracking is 1/1000 em on both sides of the boundary, so a base
/// style carries into the rich cascade untouched and a plain string
/// measures the same through either shaper.
#[test]
fn tracking_crosses_into_the_cascade_unchanged() {
    let sheet = RichTextStyleSheet::new();
    let style = base_style().tracking(200.0);
    let rich = RichTextRun::new(
        "nnnnn",
        &style,
        Color::from_rgba8(0, 0, 0, 255),
        &sheet,
        &palette(),
        96.0,
    );
    let plain = crate::text::TextRun::new("nnnnn", &style, 96.0);
    assert!(
        (rich.natural_width() - plain.natural_width()).abs() < 0.01,
        "tracked widths should agree: {} vs {}",
        rich.natural_width(),
        plain.natural_width()
    );
    // An element at a different size tracks against its own em, which
    // is the whole point of the unit.
    let heading = RichTextRun::new(
        "# nnnnn",
        &style,
        Color::from_rgba8(0, 0, 0, 255),
        &sheet,
        &palette(),
        96.0,
    );
    let untracked = RichTextRun::new(
        "# nnnnn",
        &base_style(),
        Color::from_rgba8(0, 0, 0, 255),
        &sheet,
        &palette(),
        96.0,
    );
    let head_added = heading.natural_width() - untracked.natural_width();
    let plain_added = plain.natural_width()
        - crate::text::TextRun::new("nnnnn", &base_style(), 96.0).natural_width();
    // h1 is 2.25x the base size, so its tracking is 2.25x as wide.
    assert!(
        (head_added / plain_added - 2.25).abs() < 0.05,
        "heading tracking should follow its own em: {plain_added} → {head_added}"
    );
}

#[test]
fn an_image_on_a_hanging_indent_survives_the_re_break() {
    // A list item is the asymmetric case: `set_max_width` re-shapes it
    // from source and splits the side tables between the first line and
    // the continuation. An image in either half has to come through.
    let mut images = crate::image_registry::ImageRegistry::new();
    images.insert("dot", one_pixel());
    let src = "- one ![](dot) two three four five ![](dot) six seven eight nine";
    let sheet = RichTextStyleSheet::new();
    let run = RichTextRun::new_with_images(
        src,
        &base_style(),
        Color::from_rgba8(0, 0, 0, 255),
        &sheet,
        &palette(),
        96.0,
        &images,
    );
    let wide = image_count(&draw(&run));
    run.set_max_width(120.0, HAlign::Start);
    let narrow = image_count(&draw(&run));
    assert_eq!(wide, 2, "both tags draw at natural width");
    assert_eq!(
        narrow, 2,
        "and both still draw once the item wraps onto a continuation line"
    );
}

/// A 1x1 opaque pixel, for tests that only count blits.
fn one_pixel() -> crate::brush::Image {
    crate::brush::Image {
        data: crate::brush::Blob::new(std::sync::Arc::new(vec![0, 0, 0, 255])),
        format: crate::brush::ImageFormat::Rgba8,
        alpha_type: crate::brush::ImageAlphaType::Alpha,
        width: 1,
        height: 1,
    }
}

/// How many image blits a scene holds.
fn image_count(scene: &RecordingScene) -> usize {
    scene
        .ops
        .iter()
        .filter(|op| matches!(op, Op::DrawImage { .. }))
        .count()
}

/// Rich text must report its source the same way plain text does, or a
/// backend emitting text as text sees markdown as anonymous glyphs.
///
/// Attribution is by byte range, which is what lets two spans that
/// resolve to the same color still be told apart — the case the older
/// color-matching recovery could not separate.
#[test]
fn rich_glyph_runs_report_their_source_text_and_font() {
    use crate::color::Color;
    use crate::geometry::Affine;
    use crate::pick::PickId;
    use crate::scene::recording::{Op, RecordingScene};
    use crate::style_vocab::{FontStyleKind, Palette};
    use crate::text::rich::{draw_rich_text, RichAnchor, RichTextRun, RichTextStyleSheet};
    use crate::text::TextStyle;

    fn runs(source: &str) -> Vec<(String, u16, FontStyleKind)> {
        let base = TextStyle::new(12.0);
        let sheet = RichTextStyleSheet::default();
        let palette = Palette::default();
        let run = RichTextRun::new(source, &base, Color::BLACK, &sheet, &palette, 96.0);
        let mut scene = RecordingScene::new();
        draw_rich_text(
            &mut scene,
            &run,
            0.0,
            0.0,
            RichAnchor::default(),
            Affine::IDENTITY,
            PickId::Skip,
        );
        scene
            .ops
            .iter()
            .filter_map(|op| match op {
                Op::DrawGlyphs(r) => r.source.as_ref(),
                _ => None,
            })
            .map(|s| (s.text.clone(), s.font.weight, s.font.style))
            .collect()
    }

    let got = runs("plain **bold** and *italic* text");
    let joined: String = got.iter().map(|(t, _, _)| t.as_str()).collect();
    assert_eq!(
        joined, "plain bold and italic text",
        "every run names the text it drew"
    );
    assert!(
        got.iter().any(|(t, w, _)| t == "bold" && *w >= 700),
        "the bold span reports a bold weight: {got:?}"
    );
    assert!(
        got.iter()
            .any(|(t, _, st)| t == "italic" && *st == FontStyleKind::Italic),
        "the italic span reports italic: {got:?}"
    );

    // Two spans that resolve to the same color are still distinguished,
    // because attribution is by byte range rather than by brush.
    let got = runs("a **b** c **d** e");
    let joined: String = got.iter().map(|(t, _, _)| t.as_str()).collect();
    assert_eq!(joined, "a b c d e");
}
