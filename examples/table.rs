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
use hephaestus::plot::{scale, Plot, PlotComposition, PointGeom, Table};
use hephaestus::scales::chrome::AxisSide;
use hephaestus::scene::SceneBuilder;
use hephaestus::Renderer;

fn main() {
    let (w, h) = (900u32, 420u32);
    let dpi = 96.0;
    let size = Size::new(w as f64, h as f64);

    let comp = || beside(Patch::new("summary"), Patch::new("price"));

    let table = Table::new(&comp(), "summary")
        .title("Ticker summary")
        .subtitle("Four sessions, latest close")
        .column("ticker", "Ticker")
        .column("close", "Close")
        .column("change", "Δ day")
        .column("volume", "Volume")
        .stub_column("ticker")
        .spanner("Session", 0, ["close", "change"])
        .row_group(
            "Large cap",
            [
                ["ACME", "72.41", "+1.2%", "1.24M"],
                ["BRWN", "18.05", "−0.4%", "842K"],
            ],
        )
        .row_group(
            "Mid cap",
            [
                ["CRSN", "203.77", "+3.9%", "2.01M"],
                ["DLTA", "55.92", "+0.1%", "391K"],
            ],
        )
        .summary_row(["All", "—", "+4.8%", "4.48M"])
        .footnote("Change is against the previous close.")
        // One highlighted cell: bold accent text, above every theme
        // layer.
        .cell_style(
            2,
            "change",
            hephaestus::plot::theme::TableCellTheme {
                text: Element::Set(hephaestus::plot::theme::TextElement {
                    font: hephaestus::plot::theme::FontSpec {
                        weight: Some(hephaestus::plot::theme::FontWeight::BOLD),
                        ..hephaestus::plot::theme::FontSpec::default()
                    },
                    color: Some(ThemeColor::Accent),
                    ..hephaestus::plot::theme::TextElement::default()
                }),
                ..hephaestus::plot::theme::TableCellTheme::default()
            },
        )
        .caption("Source: simulated data");

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
