//! Exercises every sheet selector in a single rendered block: headings
//! `h1..h6`, `em` / `strong` / `del` / `code`, `sup` / `sub`, inline
//! `link`, custom colour spans, fenced code block, blockquote, ordered
//! and unordered lists (nested), a horizontal rule, and a fenced div.
//!
//! Renders one rich-text block directly (no plot) so the styling is the
//! only thing to inspect. A dashed guide-box shows the wrap column.
//!
//! Writes `examples/rich_text_marquee_parity.png`.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::brush::Brush;
use hephaestus::color::{rgb8, Color};
use hephaestus::geometry::{Affine, Rect};
use hephaestus::pick::PickId;
use hephaestus::plot::theme::{HAlign, Palette, ThemeColor};
use hephaestus::primitives::rect as rect_path;
use hephaestus::scales::value::LinetypeStep;
use hephaestus::stroke::Stroke;
use hephaestus::text::rich::{
    draw_rich_text, pt, relative, Direction, RichAnchor, RichMargin, RichTextRun,
    RichTextStyleSheet, RichTextWidth, StyleDelta,
};
use hephaestus::text::{FontFeatureSetting, TextStyle};
use hephaestus::{Renderer, SceneBuilder};
use std::sync::Arc;

const SOURCE: &str = "# Rich text at a glance

A one-block tour of the marquee-flavoured styling vocabulary.

## Inline formatting

Plain text can carry **strong emphasis**, *soft emphasis*, _underline_,
~~struck through~~ fragments, and inline `code` spans. Underscore emphasis
underlines rather than slanting, matching marquee. Braces that are meant
literally get escaped: \\{like so\\}, or doubled as {{like so}}. Named or hex
colour spans work anywhere:
{.crimson red}, {#3369e8 hex-blue}. Combine styles by **nesting**: {.royalblue *slanted*}.
Superscript ^like this^ and subscript ~like this~ require whitespace around the
outer markers (pulldown-cmark's grammar) — chemistry / physics notation with
tightly-glued markers falls back to literal.

Per-glyph outlines paint through the sheet's `text_stroke` field. A
{.haloed HALOED} span sets a coloured outline behind its fill; combine it
with a distinct `color` on the same class so parley splits the run at the
span boundary (the outline scope is defined by the resulting glyph run).

OpenType features are per-span too: {.smcp small caps here} route through
the `smcp` feature via a sheet class; {.tnum 0123456789} pushes `tnum` for
tabular numerals so digits share a fixed advance.

## Lists

- Unordered items get bullets from the sheet's cycle.
- Second item, showing that tight lists stack tighter than loose lists.
  - Nested items indent under their parent's continuation position.
  - The nested marker cycles to `◦`.

Ordered lists number themselves, and multi-digit ordinals right-align on
their period because markers live in the list's start gutter rather than
in the item's own text flow:

1. First step
2. Second step
3. Third step
4. Fourth
5. Fifth
6. Sixth
7. Seventh
8. Eighth
9. Ninth
10. Tenth — note the right-aligned period

## Blockquotes and code

> Blockquote content indents past a left-edge bar. Multiple lines fit into the
> wrapped column just like regular paragraphs.

```rust
fn code_block() {
    println!(\"backgrounded and monospaced\");
}
```

---

A horizontal rule sits above this paragraph, drawn as the rule block's\nbottom-only border.

:::note
Fenced divs pick up a custom class; users can theme them via the sheet.
:::

## Custom indent + hanging + justification

:::first-line-indent
Sheet-defined class `.first-line-indent` sets a `Rel(2)` first-line
indent — this paragraph's first line steps in by two em, while continuation
lines start flush with the block's left. Try wrapping a few sentences at
this width and see the effect apply to each paragraph inside the div.
:::

:::hanging-block
Sheet-defined class `.hanging-block` sets a `Rel(2.5)` hanging indent —
the first line stays flush at the left while every wrapped continuation
line steps in. Handy for a definition-list-style layout where each
paragraph's first word acts as the term.
:::

:::justified
Sheet-defined class `.justified` sets `align: HAlign::Justify` — parley
distributes trailing whitespace across each line so both the left and
the right edge align. Effect is most visible on paragraphs that wrap
across multiple lines with mixed word lengths.
:::

:::dashed-note
`border_type` sets a dash pattern on any block border — this div has
a dashed rectangle around it. Adjacent same-width sides share one
polyline stroke so the corner is a single mitred join, not two abutting
segments meeting at the same point.
:::

:::l-shape
`.l-shape` sets top + left borders only. They collapse into one
polyline through the top-left corner rather than rendering as two
independent segments.
:::

:::stamped-note
`border_type` also accepts `LinetypeStep::Marker` — small shape stamps
spaced along the border. This div's pattern alternates dashes with
`circle` marker stamps, walked by the same primitive `LineGeom` uses.
:::

:::rtl-quote
> نص عربي مع اقتباس — the blockquote bar flips to the right edge under
> `Direction::Rtl`, first-line indent applies from the right, and
> `HAlign::Start` right-aligns text. Latin words inside still shape
> left-to-right (parley's UBA is untouched).
:::";

fn main() {
    let (w, h) = (960u32, 1600u32);
    let dpi = 96.0;
    let bg: Color = rgb8(252, 252, 254);
    // Sheet with three custom div classes — each demonstrates a
    // different block-level styling axis.
    let mut sheet = RichTextStyleSheet::new();
    sheet.set(
        "first-line-indent",
        StyleDelta {
            indent: Some(relative(2.0)),
            ..StyleDelta::empty()
        },
    );
    sheet.set(
        "hanging-block",
        StyleDelta {
            hanging: Some(relative(2.5)),
            ..StyleDelta::empty()
        },
    );
    sheet.set(
        "justified",
        StyleDelta {
            align: Some(HAlign::Justify),
            ..StyleDelta::empty()
        },
    );
    // Bright fill + contrasting halo. `color` forces parley to split
    // the run at the span's edges so the outline stays contained.
    sheet.set(
        "haloed",
        StyleDelta {
            weight: Some(700),
            color: Some(ThemeColor::Fixed(rgb8(230, 60, 60))),
            text_stroke: Some(ThemeColor::Fixed(rgb8(255, 235, 205))),
            text_stroke_width: Some(pt(2.0)),
            ..StyleDelta::empty()
        },
    );
    sheet.set(
        "smcp",
        StyleDelta {
            features: Some(vec![FontFeatureSetting {
                tag: *b"smcp",
                value: 1,
            }]),
            ..StyleDelta::empty()
        },
    );
    sheet.set(
        "tnum",
        StyleDelta {
            features: Some(vec![FontFeatureSetting {
                tag: *b"tnum",
                value: 1,
            }]),
            ..StyleDelta::empty()
        },
    );
    sheet.set(
        "dashed-note",
        StyleDelta {
            border_color: Some(ThemeColor::Fixed(rgb8(160, 90, 40))),
            border_width: Some(RichMargin::all(pt(1.5))),
            border_type: Some(Arc::from(vec![
                LinetypeStep::Dash(6.0),
                LinetypeStep::Gap(3.0),
            ])),
            border_radius: Some(pt(4.0)),
            padding: Some(RichMargin::all(pt(8.0))),
            margin: Some(RichMargin {
                top: pt(6.0),
                right: pt(0.0),
                bottom: pt(6.0),
                left: pt(0.0),
            }),
            ..StyleDelta::empty()
        },
    );
    sheet.set(
        "stamped-note",
        StyleDelta {
            border_color: Some(ThemeColor::Fixed(rgb8(80, 130, 90))),
            border_width: Some(RichMargin::all(pt(1.0))),
            border_type: Some(Arc::from(vec![
                LinetypeStep::Dash(6.0),
                LinetypeStep::Gap(3.0),
                LinetypeStep::Marker(Arc::from("circle")),
                LinetypeStep::Gap(3.0),
            ])),
            padding: Some(RichMargin::all(pt(8.0))),
            margin: Some(RichMargin {
                top: pt(6.0),
                right: pt(0.0),
                bottom: pt(6.0),
                left: pt(0.0),
            }),
            ..StyleDelta::empty()
        },
    );
    sheet.set(
        "rtl-quote",
        StyleDelta {
            text_direction: Some(Direction::Rtl),
            padding: Some(RichMargin::all(pt(6.0))),
            margin: Some(RichMargin {
                top: pt(6.0),
                right: pt(0.0),
                bottom: pt(6.0),
                left: pt(0.0),
            }),
            ..StyleDelta::empty()
        },
    );
    sheet.set(
        "l-shape",
        StyleDelta {
            border_color: Some(ThemeColor::Fixed(rgb8(60, 100, 160))),
            // Top + left only. Same width on both so they collapse
            // into one polyline through the top-left corner.
            border_width: Some(RichMargin {
                top: pt(2.0),
                right: pt(0.0),
                bottom: pt(0.0),
                left: pt(2.0),
            }),
            padding: Some(RichMargin::all(pt(8.0))),
            margin: Some(RichMargin {
                top: pt(6.0),
                right: pt(0.0),
                bottom: pt(6.0),
                left: pt(0.0),
            }),
            ..StyleDelta::empty()
        },
    );
    let palette = Palette::default();
    let base_style = TextStyle::new(13.0);
    let base_brush: Color = rgb8(24, 24, 30);
    // Column width for wrapping. Leave a 40px gutter on each side.
    let column = (w as f32) - 80.0;
    let run = RichTextRun::new_with_width(
        SOURCE,
        &base_style,
        base_brush,
        &sheet,
        &palette,
        dpi,
        RichTextWidth::Fixed(column),
    );
    let mut renderer = VelloRenderer::new().expect("vello renderer init");
    {
        let scene = renderer.scene();
        scene.clear();
        // Faint dashed guide box showing the column bounds.
        let guide_rect = Rect::new(
            40.0,
            40.0,
            40.0 + column as f64,
            40.0 + run.current_height() + 8.0,
        );
        let guide_path = rect_path(guide_rect);
        let guide_stroke = Stroke::new(1.0);
        scene.stroke(
            &guide_stroke,
            Affine::IDENTITY,
            &Brush::Solid(rgb8(220, 220, 230)),
            None,
            &guide_path,
            PickId::Skip,
        );
        // The block itself.
        draw_rich_text(
            scene,
            &run,
            40.0,
            48.0,
            RichAnchor::top_left(),
            Affine::IDENTITY,
            PickId::Skip,
        );
    }
    let mut pixels = vec![0u8; (w * h * 4) as usize];
    renderer
        .render_to_buffer(w, h, bg, &mut pixels)
        .expect("render");
    let path = std::env::current_dir()
        .unwrap()
        .join("examples/rich_text_marquee_parity.png");
    hephaestus::image::write_png(&path, w, h, &pixels).expect("write png");
    println!("wrote {}", path.display());
}
