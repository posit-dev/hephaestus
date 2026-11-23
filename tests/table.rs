//! Tables through `PlotComposition`: structure, theming, and — behind
//! the `document` feature — a document round trip at sizes the writer
//! never saw.
//!
//! Assertions are on recorded draw calls, not pixels: a table's draw
//! calls are deterministic, and they are everything downstream of the
//! theme and layout code is responsible for.

use hephaestus::composition::{beside, Patch};
use hephaestus::geometry::Size;
use hephaestus::plot::theme::{Element, LineElement, RectElement, TableCellTheme, ThemeColor};
use hephaestus::plot::{PlotComposition, Table};
use hephaestus::scene::recording::{Op, RecordingScene};

/// Render the composition and keep the recorded ops.
fn draw_calls(comp: &mut PlotComposition, w: u32, h: u32) -> Vec<Op> {
    let mut scene = RecordingScene::new();
    comp.render(&mut scene, Size::new(f64::from(w), f64::from(h)), 96.0);
    scene.ops
}

/// A table exercising the structural slice: a stub column, a spanner,
/// a title block and a caption.
fn build() -> PlotComposition {
    let comp = || beside(Patch::new("summary"), Patch::new("spare"));
    let table = Table::new(&comp(), "summary")
        .title("Ticker summary")
        .subtitle("Four sessions")
        .column("ticker", "Ticker")
        .column("close", "Close")
        .column("change", "Change")
        .column("volume", "Volume")
        .stub_column("ticker")
        .spanner("Session", 0, ["close", "change"])
        .row(["ACME", "72.41", "+1.2%", "1.24M"])
        .row(["BRWN", "18.05", "-0.4%", "842K"])
        .row(["CRSN", "203.77", "+3.9%", "2.01M"])
        .row(["DLTA", "55.92", "+0.1%", "391K"])
        .caption("Source: simulated");
    PlotComposition::new(&comp()).with_table(table)
}

/// One glyph run per cell: title, subtitle, spanner, four heading
/// labels, sixteen body cells, caption.
const EXPECTED_TEXT_RUNS: usize = 1 + 1 + 1 + 4 + 16 + 1;

#[test]
fn a_table_renders_every_cell_and_the_default_rules() {
    let ops = draw_calls(&mut build(), 700, 400);

    let text_runs = ops
        .iter()
        .filter(|op| matches!(op, Op::DrawGlyphs(_)))
        .count();
    assert_eq!(
        text_runs, EXPECTED_TEXT_RUNS,
        "expected one glyph run per cell"
    );

    // The default table theme draws three ink rules: the outer top and
    // bottom edges and the heading/body separator.
    let rules = ops
        .iter()
        .filter(|op| matches!(op, Op::Stroke { .. }))
        .count();
    assert_eq!(rules, 3, "outer top/bottom plus the header rule");
}

#[test]
fn a_themed_stripe_paints_one_background_per_even_row_cell() {
    let mut view = build();
    view.update_theme(|theme| {
        theme.table.body.even = TableCellTheme {
            background: Element::Set(RectElement {
                fill: Some(ThemeColor::mix(ThemeColor::Paper, ThemeColor::Ink, 0.05)),
                ..RectElement::default()
            }),
            ..TableCellTheme::default()
        };
    });
    let ops = draw_calls(&mut view, 700, 400);

    // Two even rows of four cells. (The patch background is a rect
    // fill too, but over `plot_background`'s own path — distinguished
    // below by brush color.)
    let stripe_fills = ops
        .iter()
        .filter(|op| matches!(op, Op::Fill { brush: hephaestus::brush::Brush::Solid(c), .. } if *c != hephaestus::color::Color::WHITE))
        .count();
    assert_eq!(stripe_fills, 2 * 4, "one stripe fill per even-row cell");
}

#[test]
fn blanking_a_rule_removes_its_stroke() {
    let mut view = build();
    view.update_theme(|theme| {
        theme.table.rules.outer = Element::Blank;
        theme.table.rules.header = Element::Set(LineElement {
            linewidth_pt: Some(hephaestus::plot::theme::Length::Abs(2.0)),
            ..LineElement::default()
        });
    });
    let ops = draw_calls(&mut view, 700, 400);
    let rules = ops
        .iter()
        .filter(|op| matches!(op, Op::Stroke { .. }))
        .count();
    assert_eq!(rules, 1, "only the header rule survives");
}

#[test]
fn multi_level_spanners_render_without_overlap_panics() {
    let comp = || hephaestus::composition::grid(1, 1, vec![Patch::new("t").into()]);
    let table = Table::new(&comp(), "t")
        .column("id", "Id")
        .column("a", "A")
        .column("b", "B")
        .column("c", "C")
        .spanner("ab", 0, ["a", "b"])
        .spanner("abc", 1, ["a", "b", "c"])
        .row(["r1", "1", "2", "3"])
        .row(["r2", "4", "5", "6"]);
    let mut view = PlotComposition::new(&comp()).with_table(table);
    let ops = draw_calls(&mut view, 400, 300);
    // Two spanners + four labels + eight body cells.
    let text_runs = ops
        .iter()
        .filter(|op| matches!(op, Op::DrawGlyphs(_)))
        .count();
    assert_eq!(text_runs, 2 + 4 + 8);
}

#[test]
fn a_row_of_the_wrong_width_is_refused() {
    let comp = || hephaestus::composition::grid(1, 1, vec![Patch::new("t").into()]);
    let result = std::panic::catch_unwind(|| {
        Table::new(&comp(), "t").column("a", "A").row(["1", "2"]);
    });
    assert!(result.is_err());
}

#[test]
fn a_cell_style_paints_exactly_one_background() {
    let comp = || beside(Patch::new("summary"), Patch::new("spare"));
    let highlight = TableCellTheme {
        background: Element::Set(RectElement {
            fill: Some(ThemeColor::Accent),
            ..RectElement::default()
        }),
        ..TableCellTheme::default()
    };
    let table = Table::new(&comp(), "summary")
        .column("id", "Id")
        .column("value", "Value")
        .row(["a", "1.0"])
        .row(["b", "2.5"])
        .cell_style(1, "value", highlight.clone());
    let mut view = PlotComposition::new(&comp()).with_table(table);
    let ops = draw_calls(&mut view, 500, 300);
    let fills = ops
        .iter()
        .filter(|op| matches!(op, Op::Fill { brush: hephaestus::brush::Brush::Solid(c), .. } if *c != hephaestus::color::Color::WHITE))
        .count();
    assert_eq!(fills, 1, "only the highlighted cell paints a fill");

    // A row style paints the whole row (both cells), and the cell
    // override beats it field-for-field — here it replaces the row's
    // fill with its own, so the row contributes one fill, not two.
    let table = Table::new(&comp(), "summary")
        .column("id", "Id")
        .column("value", "Value")
        .row(["a", "1.0"])
        .row(["b", "2.5"])
        .row_style(1, highlight.clone())
        .cell_style(1, "id", TableCellTheme::default());
    let mut view = PlotComposition::new(&comp()).with_table(table);
    let ops = draw_calls(&mut view, 500, 300);
    let fills = ops
        .iter()
        .filter(|op| matches!(op, Op::Fill { brush: hephaestus::brush::Brush::Solid(c), .. } if *c != hephaestus::color::Color::WHITE))
        .count();
    assert_eq!(fills, 2, "the row style fills its row's cells");
}

#[test]
fn a_cell_style_can_hide_text() {
    let comp = || beside(Patch::new("summary"), Patch::new("spare"));
    let table = Table::new(&comp(), "summary")
        .column("id", "Id")
        .row(["a"])
        .cell_style(
            0,
            "id",
            TableCellTheme {
                text: Element::Blank,
                ..TableCellTheme::default()
            },
        );
    let mut view = PlotComposition::new(&comp()).with_table(table);
    let ops = draw_calls(&mut view, 500, 300);
    let runs = ops
        .iter()
        .filter(|op| matches!(op, Op::DrawGlyphs(_)))
        .count();
    assert_eq!(runs, 1, "the column label draws; the blanked cell does not");
}

#[test]
fn row_groups_summaries_and_footnotes_render_in_order() {
    let comp = || beside(Patch::new("summary"), Patch::new("spare"));
    let table = Table::new(&comp(), "summary")
        .column("id", "Id")
        .column("value", "Value")
        .row_group("first", [["a", "1.0"], ["b", "2.0"]])
        .row_group("second", [["c", "3.0"]])
        .summary_row(["All", "6.0"])
        .footnote("A note.");
    let mut view = PlotComposition::new(&comp()).with_table(table);
    let ops = draw_calls(&mut view, 500, 400);
    // One label row (2) + two group headers + six body cells + two
    // summary cells + one footnote.
    let runs = ops
        .iter()
        .filter(|op| matches!(op, Op::DrawGlyphs(_)))
        .count();
    assert_eq!(runs, 2 + 2 + 6 + 2 + 1);
}

#[test]
fn column_parity_stripes_columns() {
    let comp = || beside(Patch::new("summary"), Patch::new("spare"));
    let mut view = PlotComposition::new(&comp()).with_table(
        Table::new(&comp(), "summary")
            .column("a", "A")
            .column("b", "B")
            .column("c", "C")
            .row(["1", "2", "3"])
            .row(["4", "5", "6"]),
    );
    view.update_theme(|theme| {
        theme.table.columns.even = TableCellTheme {
            background: Element::Set(RectElement {
                fill: Some(ThemeColor::Accent),
                ..RectElement::default()
            }),
            ..TableCellTheme::default()
        };
    });
    let ops = draw_calls(&mut view, 600, 300);
    // One even column (index 1) times two body rows.
    let fills = ops
        .iter()
        .filter(|op| matches!(op, Op::Fill { brush: hephaestus::brush::Brush::Solid(c), .. } if *c != hephaestus::color::Color::WHITE))
        .count();
    assert_eq!(fills, 2, "one fill per even-column body cell");
}

/// The load-bearing document test: a composition mixing a plot and a
/// table round-trips, and the reloaded composition emits exactly the
/// same draw calls at sizes the writer never saw — the table's cells
/// re-solve at the new width like every other layout.
#[cfg(feature = "document")]
#[test]
fn a_mixed_composition_round_trips_through_a_document() {
    use hephaestus::document::{read_composition, write_composition, ReadContext, WriteOptions};
    use hephaestus::plot::{scale, Plot, PointGeom};

    let build_mixed = || {
        let comp = || beside(Patch::new("summary"), Patch::new("points"));
        let table = Table::new(&comp(), "summary")
            .title("Summary")
            .column("id", "Id")
            .column("value", "Value")
            .spanner("all", 0, ["id", "value"])
            .row_group("grp", [["a", "1.0"]])
            .row(["b", "2.5"])
            .summary_row(["All", "3.5"])
            .footnote("Note.")
            // Per-row and per-cell overrides must travel too.
            .row_style(
                1,
                TableCellTheme {
                    background: Element::Set(RectElement {
                        fill: Some(ThemeColor::Accent),
                        ..RectElement::default()
                    }),
                    ..TableCellTheme::default()
                },
            )
            .cell_style(
                0,
                "value",
                TableCellTheme {
                    text: Element::Set(hephaestus::plot::theme::TextElement {
                        font: hephaestus::plot::theme::FontSpec {
                            weight: Some(hephaestus::plot::theme::FontWeight::BOLD),
                            ..hephaestus::plot::theme::FontSpec::default()
                        },
                        ..hephaestus::plot::theme::TextElement::default()
                    }),
                    ..TableCellTheme::default()
                },
            );
        let mut plot = Plot::new(&comp(), "points").bind("x", "t").bind("y", "v");
        plot.add_geom(
            PointGeom::builder()
                .set("x", vec![0.0, 1.0, 2.0])
                .set("y", vec![1.0, 2.0, 0.5])
                .set("fill", hephaestus::color::rgb8(200, 30, 30))
                .set("size", 4.0_f64)
                .build(),
        );
        PlotComposition::new(&comp())
            .add_scale("t", scale::continuous(0.0..=2.0))
            .add_scale("v", scale::continuous(0.0..=3.0))
            .with_table(table)
            .with_plot(plot)
    };

    let source = build_mixed();
    let bytes = write_composition(&source, &WriteOptions::new()).expect("a writable composition");
    for (w, h) in [(400u32, 300u32), (900, 500)] {
        let mut original = build_mixed();
        let mut reloaded =
            read_composition(&bytes, &ReadContext::new()).expect("a readable document");
        let want = draw_calls(&mut original, w, h);
        let got = draw_calls(&mut reloaded, w, h);
        assert_eq!(
            want.len(),
            got.len(),
            "at {w}x{h} the reloaded composition emitted the wrong number of draw calls"
        );
        for (i, (a, b)) in want.iter().zip(&got).enumerate() {
            assert!(
                a == b,
                "at {w}x{h} draw call {i} differs:\n  original: {a:?}\n  reloaded: {b:?}"
            );
        }
    }
}
