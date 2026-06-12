//! Glyph markers: use shaped characters (letters, emoji, symbol-font
//! glyphs) as scatter markers and as linetype pattern stamps.
//!
//! Glyph-backed shapes register in the same [`ShapeRegistry`] as the
//! built-in vector shapes. The `"shape"` channel resolves names exactly
//! as before, so per-row variation and linetype-marker stamping fall out
//! for free.
//!
//! Renders `examples/glyph_markers.png` with two panels:
//!
//! - Left: a scatter where rows alternate between vector `"circle"` and
//!   two glyph shapes (the letter `A` and a heart symbol).
//! - Right: a line with a dash + glyph-marker linetype pattern, stamping
//!   a smiley face emoji 😀 along the polyline at the tangent of each
//!   step (a rotationally-symmetric glyph makes vertical centring
//!   easy to verify by eye).

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{beside, Patch};
use hephaestus::geometry::Size;
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement};
use hephaestus::plot::{linetype, scale, LineGeom, Plot, PlotComposition, PointGeom, Value};
use hephaestus::scales::chrome::AxisSide;
use hephaestus::shape::ShapeRegistry;
use hephaestus::text::{glyph_marker, TextStyle};
use hephaestus::Renderer;

fn main() {
    let (w, h) = (1200u32, 500u32);
    let dpi = 96.0;

    let comp = || beside(Patch::new("scatter"), Patch::new("line"));

    // ── Build the shared glyph registry ──
    let style = TextStyle::new(16.0);
    let mut shapes = ShapeRegistry::with_builtins();
    shapes.insert("letter-a", glyph_marker("A", &style));
    // U+2665 = heart symbol ♥.
    shapes.insert("heart", glyph_marker("\u{2665}", &style));
    // U+1F600 = grinning face 😀 — rotationally symmetric, useful for
    // verifying that glyph markers are centred correctly on the line.
    shapes.insert("smiley", glyph_marker("\u{1F600}", &style));

    // ── Scatter panel: mixed vector + glyph markers per row ──
    let n = 16;
    let xs: Vec<f64> = (0..n).map(|i| i as f64).collect();
    let ys: Vec<f64> = xs.iter().map(|x| 4.0 + 1.8 * (x * 0.55).sin()).collect();
    let shape_col: Vec<&str> = (0..n)
        .map(|i| match i % 3 {
            0 => "circle",
            1 => "letter-a",
            _ => "heart",
        })
        .collect();

    let mut scatter = Plot::new(&comp(), "scatter")
        .bind("x", "sx")
        .bind("y", "sy")
        .shape_registry(shapes.clone());
    scatter.add_geom(
        PointGeom::builder()
            .set("x", xs.clone())
            .set("y", ys.clone())
            .set("shape", shape_col)
            .set("fill", rgb8(50, 100, 200))
            .set("size", 18.0_f64)
            .build(),
    );
    scatter.add_axis(Axis::rail("sx", AxisPlacement::Cartesian(AxisSide::Bottom)));
    scatter.add_axis(Axis::rail("sy", AxisPlacement::Cartesian(AxisSide::Left)));

    // ── Line panel: glyph-shape linetype marker along a wavy line ──
    let line_xs: Vec<f64> = (0..80).map(|i| i as f64 / 10.0).collect();
    let line_ys: Vec<f64> = line_xs
        .iter()
        .map(|x| 4.0 + 2.0 * (x * 0.7).sin())
        .collect();
    let pat = linetype::pattern([
        linetype::dash(6.0),
        linetype::gap(4.0),
        linetype::marker("smiley"),
        linetype::gap(4.0),
    ]);

    let mut line_plot = Plot::new(&comp(), "line")
        .bind("x", "lx")
        .bind("y", "ly")
        .shape_registry(shapes);
    line_plot.add_geom(
        LineGeom::builder()
            .set("x", line_xs)
            .set("y", line_ys)
            .set("stroke", rgb8(200, 80, 50))
            .set("linewidth", 14.0_f64)
            .set("linetype", Value::Linetype(pat))
            .build(),
    );
    line_plot.add_axis(Axis::rail("lx", AxisPlacement::Cartesian(AxisSide::Bottom)));
    line_plot.add_axis(Axis::rail("ly", AxisPlacement::Cartesian(AxisSide::Left)));

    let mut view = PlotComposition::new(comp())
        .add_scale("sx", scale::continuous(-1.0..=(n as f64)))
        .add_scale("sy", scale::continuous(0.0..=8.0))
        .add_scale("lx", scale::continuous(0.0..=8.0))
        .add_scale("ly", scale::continuous(0.0..=8.0))
        .with_plot(scatter)
        .with_plot(line_plot);

    let issues = view.validate();
    if !issues.is_empty() {
        panic!("validate() reported issues: {issues:?}");
    }

    let mut renderer = VelloRenderer::new().expect("vello renderer init");
    let bg: Color = rgb8(248, 248, 252);
    {
        let scene = renderer.scene();
        scene.clear();
        view.render(scene, Size::new(w as f64, h as f64), dpi);
    }
    let mut pixels = vec![0u8; (w * h * 4) as usize];
    renderer
        .render_to_buffer(w, h, bg, &mut pixels)
        .expect("render");
    let path = std::env::current_dir()
        .unwrap()
        .join("examples/glyph_markers.png");
    hephaestus::png::write_png(&path, w, h, &pixels).expect("write png");
    println!("wrote {}", path.display());
}
