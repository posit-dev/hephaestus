//! A table beside a plot, exported as both PNG and SVG.
//!
//! The table occupies its own patch and styles through
//! `theme.table` — here the default gt-like look plus a themed
//! even-row stripe — while the plot beside it is an ordinary
//! `PointGeom` panel. Both travel in the same `PlotComposition`, so
//! the pair is also the mixed-content smoke test for documents.
//!
//! ```sh
//! cargo run --example table --features svg
//! ```

use hephaestus::backend::svg::{write_svg, SvgConfig, SvgScene};
use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::rgb8;
use hephaestus::composition::{beside, Patch};
use hephaestus::geometry::Size;
use hephaestus::image::PngCompression;
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement};
use hephaestus::plot::theme::{Element, RectElement, TableCellTheme, Theme, ThemeColor};
use hephaestus::plot::{scale, Plot, PlotComposition, PointGeom, Table, TableCell, TableCellKind};
use hephaestus::scales::chrome::AxisSide;
use hephaestus::scene::SceneBuilder;
use hephaestus::Renderer;

fn main() {
    let (w, h) = (900u32, 420u32);
    let dpi = 96.0;
    let size = Size::new(w as f64, h as f64);

    let comp = || beside(Patch::new("summary"), Patch::new("price"));

    // The table is a grid of positioned cells — arrangement (stub
    // first, spanner runs, group headers interleaved) is the
    // producer's job. Grid, 4 columns (0 = stub):
    //   0 title · 1 subtitle · 2 spanner · 3 labels
    //   4 group header · 5–6 body · 7 group header · 8–9 body
    //   10 summary · 11 caption · 12 footnote
    use TableCellKind::*;
    let cell = |kind, row, col, text: &str| TableCell::new(kind, row, col, text);
    let groups = [
        (
            "Large cap",
            [
                ["ACME", "72.41", "+1.2%", "1.24M"],
                ["BRWN", "18.05", "−0.4%", "842K"],
            ],
        ),
        (
            "Mid cap",
            [
                ["CRSN", "203.77", "+3.9%", "2.01M"],
                ["DLTA", "55.92", "+0.1%", "391K"],
            ],
        ),
    ];
    let mut cells = vec![
        cell(Title, 0, 0, "Ticker summary").span(1, 4),
        cell(Subtitle, 1, 0, "Four sessions, latest close").span(1, 4),
        cell(Spanner, 2, 1, "Session").span(1, 2),
        cell(StubHead, 3, 0, "Ticker"),
        cell(ColumnLabel, 3, 1, "Close"),
        cell(ColumnLabel, 3, 2, "Δ day"),
        cell(ColumnLabel, 3, 3, "Volume"),
    ];
    let mut grid_row = 4u16;
    let mut body_row = 0usize;
    let accent = ThemeColor::Accent.resolve(&hephaestus::plot::theme::Palette::default());
    for (label, rows) in &groups {
        cells.push(cell(RowGroup, grid_row, 0, label).span(1, 4));
        grid_row += 1;
        for row in rows {
            cells.push(cell(Stub { row: body_row }, grid_row, 0, row[0]));
            for (c, value) in row[1..].iter().enumerate() {
                let body_cell = cell(Body { row: body_row }, grid_row, 1 + c as u16, value);
                // One highlighted cell: fixed accent text, above every
                // theme layer — body row 2, grid column 2.
                let body_cell = if body_row == 2 && c == 1 {
                    body_cell.fixed(hephaestus::plot::TableScaledAesthetic::Color, accent)
                } else {
                    body_cell
                };
                cells.push(body_cell);
            }
            grid_row += 1;
            body_row += 1;
        }
    }
    cells.push(cell(Summary { row: 0 }, grid_row, 0, "All"));
    for (c, value) in ["—", "+4.8%", "4.48M"].iter().enumerate() {
        cells.push(cell(Summary { row: 0 }, grid_row, 1 + c as u16, value));
    }
    grid_row += 1;
    cells.push(cell(Caption, grid_row, 0, "Source: simulated data").span(1, 4));
    cells.push(
        cell(
            Footnote,
            grid_row + 1,
            0,
            "Change is against the previous close.",
        )
        .span(1, 4),
    );

    let table = Table::new(&comp(), "summary").cells(cells).stub_column(0);

    let xs: Vec<f64> = (0..40).map(|i| i as f64 * 2.5).collect();
    let ys: Vec<f64> = xs
        .iter()
        .map(|x| 50.0 + 20.0 * (x * 0.06).sin() + 0.1 * x)
        .collect();
    let mut plot = Plot::new(&comp(), "price")
        .bind("x", "time")
        .bind("y", "price")
        .subtitle("ACME over time");
    plot.add_geom(
        PointGeom::builder()
            .set("x", xs)
            .set("y", ys)
            .set("fill", rgb8(70, 120, 220))
            .set("size", 5.0_f64)
            .build(),
    );
    plot.add_axis(Axis::rail(
        "time",
        AxisPlacement::Cartesian(AxisSide::Bottom),
    ));
    plot.add_axis(Axis::rail(
        "price",
        AxisPlacement::Cartesian(AxisSide::Left),
    ));

    // A themed stripe: every even body row picks up a faint ink wash,
    // everything else stays at the gt-like default.
    let mut theme = Theme::default();
    theme.table.body.even = TableCellTheme {
        background: Element::Set(RectElement {
            fill: Some(ThemeColor::mix(ThemeColor::Paper, ThemeColor::Ink, 0.05)),
            ..RectElement::default()
        }),
        ..TableCellTheme::default()
    };

    let mut view = PlotComposition::new(&comp())
        .theme(theme)
        .add_scale("time", scale::continuous(0.0..=100.0))
        .add_scale("price", scale::continuous(20.0..=90.0))
        .with_table(table)
        .with_plot(plot);

    // Vector.
    let mut svg = SvgScene::with_config(size, dpi, SvgConfig::new());
    view.render(&mut svg, size, dpi);
    write_svg("examples/table.svg", &svg).expect("write svg");

    // Raster, from the same composition, for side-by-side review.
    let mut renderer = VelloRenderer::new().expect("vello renderer init");
    {
        let scene = renderer.scene();
        scene.clear();
        view.render(scene, size, dpi);
    }
    let mut pixels = vec![0u8; (w * h * 4) as usize];
    renderer
        .render_to_buffer(w, h, hephaestus::color::Color::WHITE, &mut pixels)
        .expect("render");
    hephaestus::image::write_png(
        "examples/table.png",
        w,
        h,
        &pixels,
        PngCompression::Balanced,
        Some(dpi),
    )
    .expect("png write");

    println!("wrote examples/table.png and examples/table.svg");
}
