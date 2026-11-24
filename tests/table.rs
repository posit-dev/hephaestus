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
use hephaestus::plot::{PlotComposition, Table, TableCell, TableCellKind};
use hephaestus::scene::recording::{Op, RecordingScene};

/// Render the composition and keep the recorded ops.
fn draw_calls(comp: &mut PlotComposition, w: u32, h: u32) -> Vec<Op> {
    let mut scene = RecordingScene::new();
    comp.render(&mut scene, Size::new(f64::from(w), f64::from(h)), 96.0);
    scene.ops
}

/// Shorthand for a single-track cell.
fn cell(kind: TableCellKind, row: u16, col: u16, text: &str) -> TableCell {
    TableCell::new(kind, row, col, text)
}

/// A table exercising the structural slice: a stub column, a spanner,
/// a title block and a caption — arranged by hand, as a producer
/// would: hephaestus owns no arrangement.
///
/// Grid (4 columns):
///   row 0: title (span 4)
///   row 1: subtitle (span 4)
///   row 2: spanner over cols 1–2
///   row 3: stub head + column labels
///   rows 4–7: stub + body cells
///   row 8: caption (span 4)
fn fixture() -> Vec<TableCell> {
    use TableCellKind::*;
    let mut cells = vec![
        cell(Title, 0, 0, "Ticker summary").span(1, 4),
        cell(Subtitle, 1, 0, "Four sessions").span(1, 4),
        cell(Spanner, 2, 1, "Session").span(1, 2),
        cell(StubHead, 3, 0, "Ticker"),
        cell(ColumnLabel, 3, 1, "Close"),
        cell(ColumnLabel, 3, 2, "Change"),
        cell(ColumnLabel, 3, 3, "Volume"),
    ];
    let rows = [
        ("ACME", "72.41", "+1.2%", "1.24M"),
        ("BRWN", "18.05", "-0.4%", "842K"),
        ("CRSN", "203.77", "+3.9%", "2.01M"),
        ("DLTA", "55.92", "+0.1%", "391K"),
    ];
    for (r, (ticker, close, change, volume)) in rows.iter().enumerate() {
        let grid_row = 4 + r as u16;
        cells.push(cell(Stub { row: r }, grid_row, 0, ticker));
        cells.push(cell(Body { row: r }, grid_row, 1, close));
        cells.push(cell(Body { row: r }, grid_row, 2, change));
        cells.push(cell(Body { row: r }, grid_row, 3, volume));
    }
    cells.push(cell(Caption, 8, 0, "Source: simulated").span(1, 4));
    cells
}

fn build() -> PlotComposition {
    let comp = || beside(Patch::new("summary"), Patch::new("spare"));
    let table = Table::new(&comp(), "summary")
        .cells(fixture())
        .stub_column(0);
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
    // fill too, but over `background`'s own path — distinguished
    // below by brush color.)
    let stripe_fills = ops
        .iter()
        .filter(|op| matches!(op, Op::Fill { brush: hephaestus::brush::Brush::Solid(c), .. } if *c != hephaestus::color::Color::WHITE))
        .count();
    assert_eq!(stripe_fills, 2 * 4, "one stripe fill per even-row cell");
}

/// Bounding box of the patch background — the one white fill in the
/// default theme.
fn patch_background_bounds(ops: &[Op]) -> hephaestus::geometry::Rect {
    ops.iter()
        .find_map(|op| match op {
            Op::Fill {
                brush: hephaestus::brush::Brush::Solid(c),
                path,
                ..
            } if *c == hephaestus::color::Color::WHITE => {
                Some(hephaestus::path::bounding_box(path))
            }
            _ => None,
        })
        .expect("a white patch background fill")
}

#[test]
fn the_shared_margin_applies_to_a_table_patch() {
    let mut view = build();
    view.update_theme(|theme| {
        theme.margin =
            hephaestus::plot::theme::Margin::all(hephaestus::plot::theme::Length::Abs(24.0));
    });
    let ops = draw_calls(&mut view, 700, 400);
    let bg = patch_background_bounds(&ops);

    // 24pt at 96dpi is 32px; the background sits inside the margin band.
    let px = 24.0 * 96.0 / 72.0;
    assert!((bg.x0 - px).abs() < 1e-6, "left edge at the margin: {bg:?}");
    assert!((bg.y0 - px).abs() < 1e-6, "top edge at the margin: {bg:?}");
    assert!(
        (bg.y1 - (400.0 - px)).abs() < 1e-6,
        "bottom edge inside the margin: {bg:?}"
    );
}

#[test]
fn the_table_margin_adds_to_the_shared_margin() {
    let shared_only = {
        let mut view = build();
        view.update_theme(|theme| {
            theme.margin =
                hephaestus::plot::theme::Margin::all(hephaestus::plot::theme::Length::Abs(24.0));
        });
        patch_background_bounds(&draw_calls(&mut view, 700, 400))
    };
    let mut view = build();
    view.update_theme(|theme| {
        theme.margin =
            hephaestus::plot::theme::Margin::all(hephaestus::plot::theme::Length::Abs(24.0));
        theme.table.margin =
            hephaestus::plot::theme::Margin::all(hephaestus::plot::theme::Length::Abs(12.0));
    });
    let combined = patch_background_bounds(&draw_calls(&mut view, 700, 400));

    // The table's own 12pt (16px at 96dpi) grows the margin band on
    // top of the shared one. Only the top / bottom / left edges are
    // asserted: the table's column sizes itself to content, so the
    // added right margin moves the patch boundary instead of the
    // background's right edge.
    let px = 12.0 * 96.0 / 72.0;
    assert!((combined.x0 - (shared_only.x0 + px)).abs() < 1e-6);
    assert!((combined.y0 - (shared_only.y0 + px)).abs() < 1e-6);
    assert!((combined.y1 - (shared_only.y1 - px)).abs() < 1e-6);
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
    use TableCellKind::*;
    let comp = || hephaestus::composition::grid(1, 1, vec![Patch::new("t").into()]);
    let table = Table::new(&comp(), "t").cells([
        cell(Spanner, 0, 1, "abc").span(1, 3),
        cell(Spanner, 1, 1, "ab").span(1, 2),
        cell(ColumnLabel, 2, 0, "Id"),
        cell(ColumnLabel, 2, 1, "A"),
        cell(ColumnLabel, 2, 2, "B"),
        cell(ColumnLabel, 2, 3, "C"),
        cell(Body { row: 0 }, 3, 0, "r1"),
        cell(Body { row: 0 }, 3, 1, "1"),
        cell(Body { row: 0 }, 3, 2, "2"),
        cell(Body { row: 0 }, 3, 3, "3"),
        cell(Body { row: 1 }, 4, 0, "r2"),
        cell(Body { row: 1 }, 4, 1, "4"),
        cell(Body { row: 1 }, 4, 2, "5"),
        cell(Body { row: 1 }, 4, 3, "6"),
    ]);
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
fn overlapping_cells_are_refused() {
    let comp = || hephaestus::composition::grid(1, 1, vec![Patch::new("t").into()]);
    let table = Table::new(&comp(), "t").cells([
        cell(TableCellKind::Body { row: 0 }, 0, 0, "a"),
        cell(TableCellKind::Body { row: 0 }, 0, 0, "b"),
    ]);
    let mut view = PlotComposition::new(&comp()).with_table(table);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        draw_calls(&mut view, 400, 300);
    }));
    assert!(result.is_err(), "overlapping cells must panic at wire time");
}

#[test]
fn a_fixed_fill_paints_exactly_one_background() {
    use TableCellKind::*;
    let comp = || beside(Patch::new("summary"), Patch::new("spare"));
    let accent = ThemeColor::Accent.resolve(&hephaestus::plot::theme::Palette::default());
    let table = Table::new(&comp(), "summary").cells([
        cell(ColumnLabel, 0, 0, "Id"),
        cell(ColumnLabel, 0, 1, "Value"),
        cell(Body { row: 0 }, 1, 0, "a"),
        cell(Body { row: 0 }, 1, 1, "1.0"),
        cell(Body { row: 1 }, 2, 0, "b"),
        cell(Body { row: 1 }, 2, 1, "2.5")
            .fixed(hephaestus::plot::TableScaledAesthetic::Fill, accent),
    ]);
    let mut view = PlotComposition::new(&comp()).with_table(table);
    let ops = draw_calls(&mut view, 500, 300);
    let fills = ops
        .iter()
        .filter(|op| matches!(op, Op::Fill { brush: hephaestus::brush::Brush::Solid(c), .. } if *c != hephaestus::color::Color::WHITE))
        .count();
    assert_eq!(fills, 1, "only the highlighted cell paints a fill");
}

#[test]
fn a_scaled_text_size_resizes_the_cells_text() {
    use TableCellKind::*;
    let comp = || beside(Patch::new("summary"), Patch::new("spare"));
    let table = Table::new(&comp(), "summary").cells([
        cell(Body { row: 0 }, 0, 0, "small"),
        cell(Body { row: 1 }, 1, 0, "large").scaled(
            hephaestus::plot::TableScaledAesthetic::TextSize,
            10.0_f64,
            "sizes",
        ),
    ]);
    let mut view = PlotComposition::new(&comp())
        .add_scale(
            "sizes",
            hephaestus::plot::scale::continuous(0.0..=10.0).range_numbers([8.0, 20.0]),
        )
        .with_table(table);
    let ops = draw_calls(&mut view, 500, 300);
    let sizes: Vec<f32> = ops
        .iter()
        .filter_map(|op| match op {
            Op::DrawGlyphs(run) => Some(run.font_size),
            _ => None,
        })
        .collect();
    assert_eq!(sizes.len(), 2);
    assert!(
        sizes[1] > sizes[0] * 1.5,
        "the scaled cell's text is much larger: {sizes:?}"
    );
}

#[test]
fn a_string_scale_drives_the_font_axes() {
    use hephaestus::plot::TableScaledAesthetic::FontStyle;
    use TableCellKind::*;
    let comp = || beside(Patch::new("summary"), Patch::new("spare"));
    let table = Table::new(&comp(), "summary").cells([
        cell(Body { row: 0 }, 0, 0, "plain").scaled(FontStyle, "plain", "styles"),
        cell(Body { row: 1 }, 1, 0, "fancy").scaled(FontStyle, "fancy", "styles"),
    ]);
    let mut view = PlotComposition::new(&comp())
        .add_scale(
            "styles",
            hephaestus::plot::scale::discrete([
                hephaestus::scales::Value::from("plain"),
                hephaestus::scales::Value::from("fancy"),
            ])
            .range_strings(["normal", "italic"].map(std::sync::Arc::from)),
        )
        .with_table(table);
    let ops = draw_calls(&mut view, 500, 300);
    let styles: Vec<String> = ops
        .iter()
        .filter_map(|op| match op {
            Op::DrawGlyphs(run) => Some(format!("{:?}", run.font)),
            _ => None,
        })
        .collect();
    assert_eq!(styles.len(), 2, "one run per cell");
    assert_ne!(
        styles[0], styles[1],
        "the two cells shape with different font styles"
    );
}

#[test]
fn a_string_alignment_shifts_the_text() {
    use hephaestus::plot::TableScaledAesthetic::Align;
    use TableCellKind::*;
    let comp = || beside(Patch::new("summary"), Patch::new("spare"));
    let with_align = |align: &str| {
        let mut view = PlotComposition::new(&comp()).with_table(
            Table::new(&comp(), "summary")
                // A pinned track gives the alignment slack to distribute.
                .column_width(0, hephaestus::layout::Extent::px(200.0))
                .cells([cell(Body { row: 0 }, 0, 0, "cell").fixed(Align, align.to_string())]),
        );
        let ops = draw_calls(&mut view, 500, 300);
        ops.iter()
            .find_map(|op| match op {
                Op::DrawGlyphs(run) => run.glyphs.first().map(|g| g.x),
                _ => None,
            })
            .expect("a glyph run")
    };
    let start = with_align("start");
    let end = with_align("end");
    assert!(
        end > start,
        "end-aligned text sits further right: start {start}, end {end}"
    );
}

#[test]
fn a_later_aesthetic_binding_wins() {
    use hephaestus::plot::TableScaledAesthetic::Fill;
    use TableCellKind::*;
    let comp = || beside(Patch::new("summary"), Patch::new("spare"));
    // Scaled first, then raw over it: the raw value wins.
    let table = Table::new(&comp(), "summary").cells([cell(Body { row: 0 }, 0, 0, "a")
        .scaled_fill(0.0_f64, "heat")
        .fixed(Fill, hephaestus::color::rgb8(0, 128, 0))]);
    let mut view = PlotComposition::new(&comp())
        .add_scale(
            "heat",
            hephaestus::plot::scale::continuous(0.0..=10.0).range_colors([
                hephaestus::color::rgb8(255, 0, 0),
                hephaestus::color::rgb8(0, 0, 255),
            ]),
        )
        .with_table(table);
    let ops = draw_calls(&mut view, 500, 300);
    let fills = non_white_fills(&ops);
    assert_eq!(fills, vec![hephaestus::color::rgb8(0, 128, 0)]);
}

#[test]
fn row_groups_summaries_and_footnotes_render_in_order() {
    use TableCellKind::*;
    let comp = || beside(Patch::new("summary"), Patch::new("spare"));
    let table = Table::new(&comp(), "summary").cells([
        cell(ColumnLabel, 0, 0, "Id"),
        cell(ColumnLabel, 0, 1, "Value"),
        cell(RowGroup, 1, 0, "first").span(1, 2),
        cell(Body { row: 0 }, 2, 0, "a"),
        cell(Body { row: 0 }, 2, 1, "1.0"),
        cell(Body { row: 1 }, 3, 0, "b"),
        cell(Body { row: 1 }, 3, 1, "2.0"),
        cell(RowGroup, 4, 0, "second").span(1, 2),
        cell(Body { row: 2 }, 5, 0, "c"),
        cell(Body { row: 2 }, 5, 1, "3.0"),
        cell(Summary { row: 0 }, 6, 0, "All"),
        cell(Summary { row: 0 }, 6, 1, "6.0"),
        cell(Footnote, 7, 0, "A note.").span(1, 2),
    ]);
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

/// Two body cells with a scale-driven fill: one at each end of a
/// continuous red→blue ramp.
fn scaled_fill_view() -> PlotComposition {
    use TableCellKind::*;
    let comp = || beside(Patch::new("summary"), Patch::new("spare"));
    let table = Table::new(&comp(), "summary").cells([
        cell(ColumnLabel, 0, 0, "Id"),
        cell(ColumnLabel, 0, 1, "Value"),
        cell(Body { row: 0 }, 1, 0, "a"),
        cell(Body { row: 0 }, 1, 1, "1.0").scaled_fill(0.0_f64, "heat"),
        cell(Body { row: 1 }, 2, 0, "b"),
        cell(Body { row: 1 }, 2, 1, "2.0").scaled_fill(10.0_f64, "heat"),
    ]);
    PlotComposition::new(&comp())
        .add_scale(
            "heat",
            hephaestus::plot::scale::continuous(0.0..=10.0).range_colors([
                hephaestus::color::rgb8(255, 0, 0),
                hephaestus::color::rgb8(0, 0, 255),
            ]),
        )
        .with_table(table)
}

/// Non-white solid fill colors, in draw order.
fn non_white_fills(ops: &[Op]) -> Vec<hephaestus::color::Color> {
    ops.iter()
        .filter_map(|op| match op {
            Op::Fill {
                brush: hephaestus::brush::Brush::Solid(c),
                ..
            } if *c != hephaestus::color::Color::WHITE => Some(*c),
            _ => None,
        })
        .collect()
}

#[test]
fn a_scaled_fill_maps_the_value_through_the_registry() {
    let ops = draw_calls(&mut scaled_fill_view(), 500, 300);
    let fills = non_white_fills(&ops);
    assert_eq!(
        fills,
        vec![
            hephaestus::color::rgb8(255, 0, 0),
            hephaestus::color::rgb8(0, 0, 255)
        ],
        "the two cells paint the ramp's endpoints"
    );
}

#[test]
fn a_scaled_color_recolors_the_cells_text() {
    use TableCellKind::*;
    let comp = || beside(Patch::new("summary"), Patch::new("spare"));
    let table = Table::new(&comp(), "summary").cells([
        cell(Body { row: 0 }, 0, 0, "plain"),
        cell(Body { row: 1 }, 1, 0, "scaled").scaled_color(10.0_f64, "heat"),
    ]);
    let mut view = PlotComposition::new(&comp())
        .add_scale(
            "heat",
            hephaestus::plot::scale::continuous(0.0..=10.0).range_colors([
                hephaestus::color::rgb8(255, 0, 0),
                hephaestus::color::rgb8(0, 0, 255),
            ]),
        )
        .with_table(table);
    let ops = draw_calls(&mut view, 500, 300);
    let brushes: Vec<&hephaestus::brush::Brush> = ops
        .iter()
        .filter_map(|op| match op {
            Op::DrawGlyphs(run) => Some(&run.brush),
            _ => None,
        })
        .collect();
    assert_eq!(brushes.len(), 2);
    assert!(
        matches!(brushes[0], hephaestus::brush::Brush::Solid(c) if *c == hephaestus::color::Color::BLACK),
        "the plain cell keeps the theme's ink"
    );
    assert!(
        matches!(brushes[1], hephaestus::brush::Brush::Solid(c) if *c == hephaestus::color::rgb8(0, 0, 255)),
        "the scaled cell draws in the ramp's end color"
    );
}

#[test]
fn a_raw_aesthetic_beats_the_theme_cascade() {
    use TableCellKind::*;
    let comp = || beside(Patch::new("summary"), Patch::new("spare"));
    let accent = ThemeColor::Accent.resolve(&hephaestus::plot::theme::Palette::default());
    // Body row ordinal 1 is even parity, so the themed stripe below
    // would paint this cell; the raw aesthetic wins over the cascade.
    let mut view = PlotComposition::new(&comp()).with_table(
        Table::new(&comp(), "summary").cells([cell(Body { row: 1 }, 0, 0, "a")
            .fixed(hephaestus::plot::TableScaledAesthetic::Fill, accent)]),
    );
    view.update_theme(|theme| {
        theme.table.body.even = TableCellTheme {
            background: Element::Set(RectElement {
                fill: Some(ThemeColor::mix(ThemeColor::Paper, ThemeColor::Ink, 0.05)),
                ..RectElement::default()
            }),
            ..TableCellTheme::default()
        };
    });
    let ops = draw_calls(&mut view, 500, 300);
    let fills = non_white_fills(&ops);
    assert_eq!(fills.len(), 1, "the raw aesthetic wins; only it paints");
    assert_eq!(
        fills[0], accent,
        "the fill is the raw accent, not the stripe"
    );
}

#[test]
fn an_unregistered_scaled_name_draws_nothing_and_validates() {
    use TableCellKind::*;
    let comp = || beside(Patch::new("summary"), Patch::new("spare"));
    let table = Table::new(&comp(), "summary")
        .cells([cell(Body { row: 0 }, 0, 0, "a").scaled_fill(1.0_f64, "nope")]);
    let mut view = PlotComposition::new(&comp()).with_table(table);
    let issues = view.validate();
    assert!(
        issues.iter().any(|issue| matches!(
            issue,
            hephaestus::plot::composition::ValidationIssue::TableMissingScale { scale_name, .. }
                if scale_name == "nope"
        )),
        "validate reports the unknown scale: {issues:?}"
    );
    let ops = draw_calls(&mut view, 500, 300);
    assert!(
        non_white_fills(&ops).is_empty(),
        "an unresolvable binding paints nothing"
    );
}

#[test]
fn column_parity_stripes_columns() {
    use TableCellKind::*;
    let comp = || beside(Patch::new("summary"), Patch::new("spare"));
    let mut view =
        PlotComposition::new(&comp()).with_table(Table::new(&comp(), "summary").cells([
            cell(ColumnLabel, 0, 0, "A"),
            cell(ColumnLabel, 0, 1, "B"),
            cell(ColumnLabel, 0, 2, "C"),
            cell(Body { row: 0 }, 1, 0, "1"),
            cell(Body { row: 0 }, 1, 1, "2"),
            cell(Body { row: 0 }, 1, 2, "3"),
            cell(Body { row: 1 }, 2, 0, "4"),
            cell(Body { row: 1 }, 2, 1, "5"),
            cell(Body { row: 1 }, 2, 2, "6"),
        ]));
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
    use TableCellKind::*;

    let build_mixed = || {
        let comp = || beside(Patch::new("summary"), Patch::new("points"));
        let accent = ThemeColor::Accent.resolve(&hephaestus::plot::theme::Palette::default());
        let table = Table::new(&comp(), "summary").cells([
            cell(Title, 0, 0, "Summary").span(1, 2),
            cell(Spanner, 1, 0, "all").span(1, 2),
            cell(ColumnLabel, 2, 0, "Id"),
            cell(ColumnLabel, 2, 1, "Value"),
            cell(RowGroup, 3, 0, "grp").span(1, 2),
            cell(Body { row: 0 }, 4, 0, "a"),
            // Scaled and raw aesthetics must travel too.
            cell(Body { row: 0 }, 4, 1, "1.0").scaled_fill(1.0_f64, "heat"),
            cell(Body { row: 1 }, 5, 0, "b"),
            cell(Body { row: 1 }, 5, 1, "2.5")
                .fixed(hephaestus::plot::TableScaledAesthetic::Fill, accent)
                .scaled_color(2.0_f64, "heat"),
            cell(Summary { row: 0 }, 6, 0, "All"),
            cell(Summary { row: 0 }, 6, 1, "3.5"),
            cell(Footnote, 7, 0, "Note.").span(1, 2),
        ]);
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
            .add_scale(
                "heat",
                scale::continuous(0.0..=2.0).range_colors([
                    hephaestus::color::rgb8(255, 255, 255),
                    hephaestus::color::rgb8(200, 30, 30),
                ]),
            )
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

#[test]
fn a_spanner_breaks_the_column_rules_beneath_it() {
    let mut view = build();
    view.update_theme(|theme| {
        theme.table.rules.vertical = Element::Set(LineElement {
            linewidth_pt: Some(hephaestus::plot::theme::Length::Abs(1.0)),
            ..LineElement::default()
        });
    });
    let ops = draw_calls(&mut view, 700, 400);

    // Walk the ops tracking the part scope: collect the spanner
    // label's vertical position and every stroked rule's bounds.
    let mut parts: Vec<Option<String>> = Vec::new();
    let mut spanner_y = f64::INFINITY;
    let mut rule_bounds = Vec::new();
    for op in &ops {
        match op {
            Op::PushPickScope { scope } => parts.push(scope.name().map(str::to_owned)),
            Op::PopPickScope => {
                parts.pop();
            }
            Op::DrawGlyphs(run) => {
                if parts.iter().any(|n| n.as_deref() == Some("table_spanner")) {
                    for g in &run.glyphs {
                        spanner_y = spanner_y.min(f64::from(g.y));
                    }
                }
            }
            Op::Stroke { path, .. } if parts.iter().any(|n| n.as_deref() == Some("table_rule")) => {
                rule_bounds.push(hephaestus::path::bounding_box(path));
            }
            _ => {}
        }
    }
    assert!(spanner_y.is_finite(), "a spanner label was drawn");

    // One vertical segment per interior column boundary, contiguous
    // from the stub-head row down — none crossing the spanner row.
    let vertical: Vec<_> = rule_bounds.iter().filter(|b| b.width() == 0.0).collect();
    assert_eq!(
        vertical.len(),
        3,
        "one column rule segment per interior boundary"
    );
    for b in &vertical {
        assert!(
            b.y0 > spanner_y,
            "a column rule crosses the spanner row: {b:?} vs spanner at y {spanner_y}"
        );
    }
}

/// Stroke segments recorded under a `table_rule` or cell-part scope,
/// as (bounding box, width, color, part name).
fn stroke_segments(
    ops: &[Op],
) -> Vec<(
    hephaestus::geometry::Rect,
    f64,
    hephaestus::color::Color,
    String,
)> {
    let mut parts: Vec<Option<String>> = Vec::new();
    let mut out = Vec::new();
    for op in ops {
        match op {
            Op::PushPickScope { scope } => parts.push(scope.name().map(str::to_owned)),
            Op::PopPickScope => {
                parts.pop();
            }
            Op::Stroke {
                path,
                stroke,
                brush: hephaestus::brush::Brush::Solid(c),
                ..
            } => out.push((
                hephaestus::path::bounding_box(path),
                stroke.width,
                *c,
                parts.last().cloned().flatten().unwrap_or_default(),
            )),
            _ => {}
        }
    }
    out
}

#[test]
fn a_cell_border_claims_its_four_edges() {
    use TableCellKind::*;
    let comp = || beside(Patch::new("summary"), Patch::new("spare"));
    let red = hephaestus::color::Color::from_rgb8(255, 0, 0);
    let table = Table::new(&comp(), "summary").cells([
        cell(ColumnLabel, 0, 0, "h"),
        cell(Body { row: 0 }, 1, 0, "a")
            .fixed(hephaestus::plot::TableScaledAesthetic::BorderColor, red)
            .fixed(hephaestus::plot::TableScaledAesthetic::BorderWidth, 3.0),
        cell(Body { row: 1 }, 2, 0, "b"),
    ]);
    let mut view = PlotComposition::new(&comp()).with_table(table);
    let ops = draw_calls(&mut view, 500, 300);

    // Four red segments, one per edge of the bordered cell, drawn at
    // the scaled width (3pt at 96dpi = 4px) under the cell's part.
    let red_segments: Vec<_> = stroke_segments(&ops)
        .into_iter()
        .filter(|(_, _, c, _)| *c == red)
        .collect();
    assert_eq!(red_segments.len(), 4, "one segment per cell edge");
    for (_, w, _, part) in &red_segments {
        assert_eq!(*w, 4.0);
        assert_eq!(part, "table_cell");
    }
}

#[test]
fn a_per_side_theme_border_draws_only_that_side() {
    let mut view = build();
    view.update_theme(|theme| {
        theme.table.body.all = TableCellTheme {
            background: Element::Set(RectElement {
                color: Some(ThemeColor::Ink),
                border_widths_pt: Some([
                    Some(hephaestus::plot::theme::Length::Abs(2.0)),
                    None,
                    None,
                    None,
                ]),
                ..RectElement::default()
            }),
            ..TableCellTheme::default()
        };
    });
    let ops = draw_calls(&mut view, 700, 400);

    // Every stroke is horizontal: one merged segment per body row
    // top, including the first, whose 2pt claim outbids the default
    // header rule on that edge.
    let segments = stroke_segments(&ops);
    let rule_segments: Vec<_> = segments
        .iter()
        .filter(|(_, _, _, part)| part == "table_rule")
        .collect();
    assert!(
        rule_segments.iter().all(|(bb, _, _, _)| bb.height() == 0.0),
        "no vertical strokes remain: {rule_segments:?}"
    );
    let top_borders: Vec<_> = segments
        .iter()
        .filter(|(_, w, _, part)| part == "table_cell" && *w > 2.0)
        .collect();
    assert_eq!(top_borders.len(), 4, "one merged top border per body row");
}

#[test]
fn a_narrower_cell_border_loses_the_edge_to_a_rule() {
    use TableCellKind::*;
    let comp = || beside(Patch::new("summary"), Patch::new("spare"));
    let red = hephaestus::color::Color::from_rgb8(255, 0, 0);
    // The first body row's top edge is the header rule's edge; a
    // 0.5pt cell border loses to the default 1pt header rule.
    let table = Table::new(&comp(), "summary").cells([
        cell(ColumnLabel, 0, 0, "h"),
        cell(Body { row: 0 }, 1, 0, "a")
            .fixed(hephaestus::plot::TableScaledAesthetic::BorderColor, red)
            .fixed(hephaestus::plot::TableScaledAesthetic::BorderWidth, 0.5),
    ]);
    let mut view = PlotComposition::new(&comp()).with_table(table);
    let ops = draw_calls(&mut view, 500, 300);

    let segments = stroke_segments(&ops);
    // The cell keeps its uncontested left and right edges, and loses
    // the top to the 1pt header rule and the bottom to the 1pt outer
    // rule — the wider claim wins each contested edge.
    let red_segments: Vec<_> = segments.iter().filter(|(_, _, c, _)| *c == red).collect();
    assert_eq!(red_segments.len(), 2, "only the vertical edges won");
    assert!(red_segments.iter().all(|(bb, _, _, _)| bb.width() == 0.0));
    assert!(
        segments
            .iter()
            .any(|(bb, _, _, part)| part == "table_rule" && bb.height() == 0.0),
        "the header rule still spans the contested edge"
    );
}

#[test]
fn a_per_side_cell_border_draws_only_that_side_on_that_cell() {
    use TableCellKind::*;
    let comp = || beside(Patch::new("summary"), Patch::new("spare"));
    let red = hephaestus::color::Color::from_rgb8(255, 0, 0);
    let table = Table::new(&comp(), "summary").cells([
        cell(Body { row: 0 }, 0, 0, "a")
            .fixed(hephaestus::plot::TableScaledAesthetic::BorderColor, red)
            .fixed(hephaestus::plot::TableScaledAesthetic::BorderWidthTop, 2.0),
        cell(Body { row: 1 }, 1, 0, "b"),
    ]);
    let mut view = PlotComposition::new(&comp()).with_table(table);
    let ops = draw_calls(&mut view, 500, 300);

    let red_segments: Vec<_> = stroke_segments(&ops)
        .into_iter()
        .filter(|(_, _, c, _)| *c == red)
        .collect();
    assert_eq!(red_segments.len(), 1, "only the top edge claims");
    let (bb, _, _, _) = &red_segments[0];
    assert_eq!(bb.height(), 0.0, "the claimed edge is horizontal");
}
