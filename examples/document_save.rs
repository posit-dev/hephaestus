//! Writes a plot to `examples/document.hplot`.
//!
//! The point of the pairing with `document_load` is that this binary
//! needs the data, the fonts and the plot code, while the loader needs
//! none of them — it only needs the file. That is the split a website
//! wants: run the plot once wherever the data lives, serve the document,
//! and let a wasm build re-solve the layout for whatever viewport it
//! finds.
//!
//! Run with `--features document-write`, then run `document_load`.

use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{beside, Patch};
use hephaestus::document::{unsupported_items, write_composition, WriteOptions};
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement};
use hephaestus::plot::theme::Theme;
use hephaestus::plot::{scale, LineGeom, Plot, PlotComposition, PointGeom};
use hephaestus::scales::chrome::AxisSide;
use hephaestus::scales::value::Value;

fn main() {
    let comp = || beside(Patch::new("scatter"), Patch::new("trend"));

    // Three contiguous runs of 60 rows. `LineGeom` groups *consecutive*
    // rows sharing a key into one mark, so the runs have to be blocked
    // rather than interleaved.
    let bands = ["low", "mid", "high"];
    let mut xs: Vec<f64> = Vec::new();
    let mut ys: Vec<f64> = Vec::new();
    let mut groups: Vec<&str> = Vec::new();
    for (k, band) in bands.iter().enumerate() {
        for i in 0..60 {
            let x = f64::from(i) * 0.53;
            xs.push(x);
            ys.push(45.0 + (8.0 + 4.0 * k as f64) * (x * 0.25 + k as f64 * 0.9).sin() + 0.5 * x);
            groups.push(band);
        }
    }

    let mut scatter = Plot::new(&comp(), "scatter")
        .bind("x", "t")
        .bind("y", "value")
        .bind("fill", "band")
        .title("Observations")
        .caption("one mark per reading");
    scatter.add_geom(
        PointGeom::builder()
            .set("x", xs.clone())
            .set("y", ys.clone())
            .set("fill", groups.clone())
            .set("size", 5.0_f64)
            .build(),
    );
    scatter.add_axis(Axis::rail("t", AxisPlacement::Cartesian(AxisSide::Bottom)).title("hours"));
    scatter
        .add_axis(Axis::rail("value", AxisPlacement::Cartesian(AxisSide::Left)).title("reading"));

    let mut trend = Plot::new(&comp(), "trend")
        .bind("x", "t")
        .bind("y", "value")
        .title("Trend");
    trend.add_geom(
        LineGeom::builder()
            .keys(groups)
            .set("x", xs)
            .set("y", ys)
            .set("stroke", rgb8(70, 90, 160))
            .set("linewidth", 1.75_f64)
            .set("linetype", Value::Linetype(hephaestus::linetype::dashed()))
            .build(),
    );
    trend.add_axis(Axis::rail("t", AxisPlacement::Cartesian(AxisSide::Bottom)).title("hours"));

    let view = PlotComposition::new(&comp())
        .theme(Theme::minimal())
        .caption("saved once by examples/document_save.rs, laid out per size by document_load")
        .with_plot(scatter)
        .with_plot(trend)
        .add_scale("t", scale::continuous(0.0..=32.0))
        .add_scale("value", scale::continuous(30.0..=80.0))
        .add_scale(
            "band",
            scale::discrete(["low", "mid", "high"].map(Value::from)).range_colors(vec![
                rgb8(80, 130, 210),
                rgb8(90, 175, 120),
                rgb8(215, 95, 85),
            ]),
        );

    for issue in view.validate() {
        eprintln!("validate: {issue:?}");
    }

    // Worth checking before writing: anything a document can't carry is
    // named here rather than discovered as a surprise later.
    let problems = unsupported_items(&view);
    if !problems.is_empty() {
        for p in &problems {
            eprintln!("warning: {p}");
        }
    }

    let opts = WriteOptions::new()
        .background(Color::WHITE)
        .size_hint(900.0, 420.0)
        .dpi_hint(96.0);
    let bytes = write_composition(&view, &opts).expect("this plot is writable");

    let path = "examples/document.hplot";
    std::fs::write(path, &bytes).expect("write the document");
    println!("wrote {path} ({} bytes)", bytes.len());
    println!("now run: cargo run --example document_load --features document-read,png");
}
