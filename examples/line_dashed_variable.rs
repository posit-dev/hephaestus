//! Variable width and colour on non-solid linetypes.
//!
//! One render, `line_dashed_variable.png` — three lines, each keyed to
//! its own mark:
//! - a dashed line whose width tapers 2pt → 18pt; the dashes lengthen
//!   as the line thickens (step lengths are multiples of the local
//!   width) and each dash is its own ribbon mesh.
//! - a dashdot line with a stroke-colour gradient; the dash cut points
//!   carry the interpolated colour.
//! - a marker-bearing pattern (`circle` + gap) on a ramping width; the
//!   stamps size and colour themselves from the local line.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{Composition, Patch, Span};
use hephaestus::geometry::Size;
use hephaestus::image::PngCompression;
use hephaestus::plot::{linetype, scale, LineGeom, Plot, PlotComposition, Value};
use hephaestus::scene::SceneBuilder;
use hephaestus::Renderer;

fn main() {
    let (w, h) = (1200u32, 500u32);
    let dpi = 96.0;

    let comp = || Composition::empty(1, 1).place(1, 1, Span::cell(), Patch::new("panel"));

    let n = 60;
    let xs: Vec<f64> = (0..n).map(|i| i as f64).collect();
    let blue = rgb8(60, 80, 200);
    let red = rgb8(200, 50, 60);

    // ── Dashed, tapering width ──────────────────────────────────────
    let taper: Vec<f64> = (0..n)
        .map(|i| 2.0 + (18.0 - 2.0) * i as f64 / (n - 1) as f64)
        .collect();
    // ── Marker pattern on a ramping width ───────────────────────────
    let ramp: Vec<f64> = (0..n)
        .map(|i| 2.0 + (14.0 - 2.0) * i as f64 / (n - 1) as f64)
        .collect();
    // ── Dashdot with a colour gradient ──────────────────────────────
    let gradient: Vec<Color> = (0..n)
        .map(|i| {
            let t = i as f32 / (n - 1) as f32;
            let a = blue.components;
            let b = red.components;
            Color::new([
                a[0] + (b[0] - a[0]) * t,
                a[1] + (b[1] - a[1]) * t,
                a[2] + (b[2] - a[2]) * t,
                1.0,
            ])
        })
        .collect();

    let mut plot = Plot::new(&comp(), "panel")
        .bind("x", "time")
        .bind("y", "value");
    plot.add_geom(
        LineGeom::builder()
            .keys(vec!["taper"; n])
            .set("x", xs.clone())
            .set(
                "y",
                (0..n)
                    .map(|i| 80.0 + 6.0 * (i as f64 * 0.3).sin())
                    .collect::<Vec<_>>(),
            )
            .set("stroke", rgb8(40, 90, 180))
            .set("linewidth", taper)
            .set("linetype", Value::Linetype(linetype::dashed()))
            .build(),
    );
    plot.add_geom(
        LineGeom::builder()
            .keys(vec!["gradient"; n])
            .set("x", xs.clone())
            .set(
                "y",
                (0..n)
                    .map(|i| 50.0 + 6.0 * (i as f64 * 0.3).cos())
                    .collect::<Vec<_>>(),
            )
            .set("stroke", gradient)
            .set("linewidth", 4.0_f64)
            .set("linetype", Value::Linetype(linetype::dashdot()))
            .build(),
    );
    plot.add_geom(
        LineGeom::builder()
            .keys(vec!["markers"; n])
            .set("x", xs.clone())
            .set(
                "y",
                (0..n)
                    .map(|i| 20.0 + 4.0 * (i as f64 * 0.25).sin())
                    .collect::<Vec<_>>(),
            )
            .set("stroke", rgb8(90, 140, 70))
            .set("linewidth", ramp)
            .set(
                "linetype",
                Value::Linetype(linetype::pattern([
                    linetype::marker("circle"),
                    linetype::gap(5.0),
                ])),
            )
            .build(),
    );

    let mut view = PlotComposition::new(&comp())
        .add_scale("time", scale::continuous(0.0..=60.0))
        .add_scale("value", scale::continuous(0.0..=100.0))
        .with_plot(plot);

    let issues = view.validate();
    if !issues.is_empty() {
        eprintln!("validation issues: {issues:?}");
        std::process::exit(1);
    }

    let mut renderer = VelloRenderer::new().expect("vello renderer init");
    {
        let scene = renderer.scene();
        scene.clear();
        view.render(scene, Size::new(w as f64, h as f64), dpi);
    }
    let mut pixels = vec![0u8; (w * h * 4) as usize];
    renderer
        .render_to_buffer(w, h, rgb8(248, 248, 252), &mut pixels)
        .expect("render");
    let path = std::env::current_dir()
        .unwrap()
        .join("examples/line_dashed_variable.png");
    hephaestus::image::write_png(&path, w, h, &pixels, PngCompression::Balanced, Some(dpi))
        .expect("write png");
    println!("wrote {}", path.display());
}
