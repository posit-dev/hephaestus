//! End-to-end coverage for the rich-text pipeline.
//!
//! Two halves. The **structural** tests drive `parse` + `reduce` over a
//! kitchen-sink document and assert what came out — text, run
//! boundaries, resolved values, block kinds, markers. They're the
//! regression net that says the four stages still agree with each
//! other after a refactor, and they need no GPU.
//!
//! The **render** test rasterises a block through Vello and checks that
//! ink lands where the layout says it should. No golden images: the
//! repo has no reference-image infrastructure, and a band-of-ink
//! assertion catches the failures that matter (nothing drawn, drawn
//! outside its box, blocks stacked in the wrong order) without
//! breaking on a font update.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::geometry::Affine;
use hephaestus::pick::PickId;
use hephaestus::scene::SceneBuilder;
use hephaestus::style_vocab::{HAlign, Palette};
use hephaestus::text::rich::{
    draw_rich_text, parse, reduce, BlockKind, BuiltRuns, InlineRun, ResolvedStyle, RichAnchor,
    RichTextRun, RichTextStyleSheet,
};
use hephaestus::text::TextStyle;
use hephaestus::Renderer;

const BASE_PT: f32 = 12.0;

/// One document exercising every construct the pipeline handles.
const KITCHEN_SINK: &str = "\
# Heading one

## Heading two

A paragraph with *italic*, _underline_, **bold**, ~~struck~~ text,
some `inline code`, a ^script^ and a ~script~, plus a
{.17 sized span} and a {#c04030 coloured} one.

- first bullet
- second bullet
  - nested bullet
  1. nested ordered
  2. and another
- third bullet

9. nine
10. ten

> A block quote with **emphasis** inside it.

```rust
let x = 1;
```

---

:::note
A fenced div body.
:::
";

fn base() -> ResolvedStyle {
    ResolvedStyle::from_base(&TextStyle::new(BASE_PT))
}

fn build(src: &str) -> BuiltRuns {
    reduce(&parse(src), &RichTextStyleSheet::new(), &base())
}

fn run_covering<'a>(r: &'a BuiltRuns, needle: &str) -> &'a InlineRun {
    let at = r.text.find(needle).unwrap_or_else(|| {
        panic!("{needle:?} is not in the reduced text: {:?}", r.text);
    });
    r.inline
        .iter()
        .find(|run| run.range.start <= at && run.range.end > at)
        .expect("a run covering the match")
}

// ─── Structural ─────────────────────────────────────────────────────────────

#[test]
fn kitchen_sink_reduces_to_covering_non_overlapping_runs() {
    let r = build(KITCHEN_SINK);
    assert!(!r.text.is_empty());
    assert_eq!(r.inline.first().expect("a first run").range.start, 0);
    assert_eq!(
        r.inline.last().expect("a last run").range.end,
        r.text.len(),
        "the last run must reach the end of the text"
    );
    // Runs are ordered and never overlap. They need not abut: the
    // reducer inserts `\n\n` between top-level blocks so parley
    // breaks them as separate paragraphs, and those separators
    // belong to no block.
    for pair in r.inline.windows(2) {
        assert!(
            pair[0].range.end <= pair[1].range.start,
            "runs overlap: {:?} then {:?}",
            pair[0].range,
            pair[1].range
        );
    }
}

#[test]
fn kitchen_sink_emits_every_block_kind() {
    let r = build(KITCHEN_SINK);
    let has = |f: &dyn Fn(&BlockKind) -> bool| r.blocks.iter().any(|b| f(&b.kind));
    assert!(has(&|k| matches!(k, BlockKind::Heading(1))), "h1");
    assert!(has(&|k| matches!(k, BlockKind::Heading(2))), "h2");
    assert!(has(&|k| matches!(k, BlockKind::Paragraph)), "paragraph");
    assert!(has(&|k| matches!(k, BlockKind::BlockQuote)), "block quote");
    assert!(
        has(&|k| matches!(k, BlockKind::List { ordered: false, .. })),
        "unordered list"
    );
    assert!(
        has(&|k| matches!(k, BlockKind::List { ordered: true, .. })),
        "ordered list"
    );
    assert!(has(&|k| matches!(k, BlockKind::ListItem { .. })), "item");
    assert!(has(&|k| matches!(k, BlockKind::CodeBlock { .. })), "code");
    assert!(has(&|k| matches!(k, BlockKind::Rule)), "rule");
    assert!(
        has(&|k| matches!(k, BlockKind::Div { class } if class == "note")),
        "div"
    );
}

#[test]
fn inline_marks_resolve_to_their_style() {
    let r = build(KITCHEN_SINK);
    assert!(run_covering(&r, "italic").style.italic);
    assert!(run_covering(&r, "underline").style.underline);
    assert_eq!(run_covering(&r, "bold").style.weight, 700);
    assert!(run_covering(&r, "struck").style.strikethrough);
    assert_eq!(
        run_covering(&r, "inline code").style.family.as_deref(),
        Some("monospace")
    );
}

#[test]
fn sized_and_coloured_spans_resolve_to_concrete_values() {
    let r = build(KITCHEN_SINK);
    let sized = run_covering(&r, "sized span");
    assert!(
        (sized.style.size_pt - 17.0).abs() < 1e-6,
        "`{{.17 …}}` should be 17pt, got {}",
        sized.style.size_pt
    );
    assert!(run_covering(&r, "coloured").style.color.is_some());
}

#[test]
fn superscript_and_subscript_shift_in_opposite_directions() {
    let r = build(KITCHEN_SINK);
    let up = r
        .baseline_shifts
        .iter()
        .filter(|b| b.shift_pt > 0.0)
        .count();
    let down = r
        .baseline_shifts
        .iter()
        .filter(|b| b.shift_pt < 0.0)
        .count();
    assert!(up >= 1, "expected a superscript shift");
    assert!(down >= 1, "expected a subscript shift");
}

#[test]
fn headings_carry_larger_sizes_than_body_text() {
    let r = build(KITCHEN_SINK);
    let size_of = |level: u8| {
        r.blocks
            .iter()
            .find(|b| matches!(b.kind, BlockKind::Heading(l) if l == level))
            .expect("heading block")
            .style
            .size_pt
    };
    assert!(size_of(1) > size_of(2));
    assert!(size_of(2) > BASE_PT as f64);
}

#[test]
fn list_markers_ride_on_their_items() {
    let r = build(KITCHEN_SINK);
    let markers: Vec<&str> = r
        .blocks
        .iter()
        .filter_map(|b| match &b.kind {
            BlockKind::ListItem { marker, .. } => marker.as_deref(),
            _ => None,
        })
        .collect();
    assert!(markers.contains(&"•"), "unordered items keep a bullet");
    assert!(markers.contains(&"◦"), "nesting cycles the bullet set");
    assert!(
        markers.contains(&"1."),
        "ordered items number from the list start"
    );
    assert!(markers.contains(&"10."), "ordinals reach double digits");
    // Marker text must not be injected into the document body.
    assert!(
        !r.text.contains("• "),
        "markers leaked into the text: {:?}",
        r.text
    );
}

#[test]
fn code_block_body_keeps_its_source_lines() {
    let r = build(KITCHEN_SINK);
    let block = r
        .blocks
        .iter()
        .find(|b| matches!(b.kind, BlockKind::CodeBlock { .. }))
        .expect("code block");
    assert_eq!(r.text[block.range.clone()].trim(), "let x = 1;");
}

#[test]
fn escaped_braces_never_open_a_span() {
    let r = build(r"a \{not a span\} b");
    assert_eq!(r.text, "a {not a span} b");
    assert_eq!(r.inline.len(), 1, "no span means one uniform run");
}

#[test]
fn image_alt_text_survives_as_plain_text() {
    // Documented limitation: images aren't rendered, but their alt
    // text must not vanish.
    let r = build("before ![a diagram](diagram.png) after");
    assert!(
        r.text.contains("a diagram"),
        "alt text should render, got {:?}",
        r.text
    );
}

#[test]
fn a_pathological_source_still_produces_a_document() {
    for src in [
        "{",
        "}",
        "{.red unclosed",
        "{}",
        "{!!! nonsense}",
        ":::",
        ":::open\nbody",
        "~~~",
        "***",
        "",
    ] {
        let r = build(src);
        // The point is that it returns at all, with coherent ranges.
        for run in &r.inline {
            assert!(
                run.range.end <= r.text.len(),
                "{src:?} produced a bad range"
            );
        }
    }
}

// ─── Render ─────────────────────────────────────────────────────────────────

/// Column of a rendered buffer, as `(x0, y0, x1, y1)` bounds of any
/// pixel that differs from `bg`.
fn ink_bounds(buf: &[u8], w: u32, h: u32, bg: Color) -> Option<(u32, u32, u32, u32)> {
    let [br, bgc, bb, _] = bg.components;
    let target = [
        (br * 255.0).round() as i16,
        (bgc * 255.0).round() as i16,
        (bb * 255.0).round() as i16,
    ];
    let mut bounds: Option<(u32, u32, u32, u32)> = None;
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            let differs = (0..3).any(|c| (buf[i + c] as i16 - target[c]).abs() > 8);
            if !differs {
                continue;
            }
            bounds = Some(match bounds {
                None => (x, y, x, y),
                Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
            });
        }
    }
    bounds
}

#[test]
fn a_rendered_block_puts_its_ink_inside_the_box_it_reports() {
    let (w, h) = (480u32, 420u32);
    let dpi = 96.0;
    let bg = rgb8(255, 255, 255);
    let sheet = RichTextStyleSheet::new();
    let palette = Palette::default();
    let style = TextStyle::new(13.0);
    let run = RichTextRun::new(
        "# Title\n\nA paragraph that is long enough to need more than one \
         line at this column width.\n\n- alpha\n- beta",
        &style,
        rgb8(20, 20, 30),
        &sheet,
        &palette,
        dpi,
    );
    let column = 360.0_f32;
    let (origin_x, origin_y) = (48.0_f64, 32.0_f64);
    let height = run.set_max_width(column, HAlign::Start) as f64;
    assert!(height > 0.0, "a shaped block must have height");

    let mut renderer = VelloRenderer::new().expect("vello renderer init");
    {
        let scene = renderer.scene();
        scene.clear();
        draw_rich_text(
            scene,
            &run,
            origin_x,
            origin_y,
            RichAnchor::top_left(),
            Affine::IDENTITY,
            PickId::Skip,
        );
    }
    let mut buf = vec![0u8; (w * h * 4) as usize];
    renderer
        .render_to_buffer(w, h, bg, &mut buf)
        .expect("render");

    let (x0, y0, x1, y1) = ink_bounds(&buf, w, h, bg).expect("the block should have drawn ink");
    // Markers hang into the list's start gutter, so ink may sit a
    // little left of the text origin.
    assert!(
        (x0 as f64) > origin_x - 40.0,
        "ink starts too far left: {x0}"
    );
    assert!(
        (x1 as f64) <= origin_x + column as f64 + 4.0,
        "ink overflows the wrap column: {x1}"
    );
    assert!(
        (y0 as f64) >= origin_y - 4.0,
        "ink starts above the box: {y0}"
    );
    assert!(
        (y1 as f64) <= origin_y + height + 8.0,
        "ink runs past the reported height: {y1} vs {}",
        origin_y + height
    );
    // A heading, a wrapped paragraph and two list items can't fit on
    // one line.
    assert!(
        (y1 - y0) as f64 > 60.0,
        "expected several stacked lines, got {}px of ink",
        y1 - y0
    );
}

#[test]
fn wrapping_narrower_makes_the_block_taller() {
    let sheet = RichTextStyleSheet::new();
    let palette = Palette::default();
    let style = TextStyle::new(13.0);
    let run = RichTextRun::new(
        "A paragraph long enough that halving its column has to add lines.",
        &style,
        rgb8(0, 0, 0),
        &sheet,
        &palette,
        96.0,
    );
    let wide = run.set_max_width(400.0, HAlign::Start);
    let narrow = run.set_max_width(160.0, HAlign::Start);
    assert!(narrow > wide, "{narrow} should exceed {wide}");
    // And the memo hands back the same answer without re-breaking.
    assert_eq!(run.set_max_width(160.0, HAlign::Start), narrow);
}
