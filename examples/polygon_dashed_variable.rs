//! Dashed linetypes on polygon outlines with variable width / colour.
//!
//! One render, `polygon_dashed_variable.png` — three closed rings:
//! - `dashed`, width ramping around the ring: dashes lengthen where
//!   the outline is thick, and the seam sits at the thinnest vertex.
//! - `dashdot`, hue gradient around the ring: dash cut points carry
//!   the interpolated colour.
//! - a marker-bearing pattern on a pulsing width: stamps size
//!   themselves from the ring's local width.

use std::f64::consts::TAU;

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{Composition, Patch, Span};
use hephaestus::geometry::Size;
use hephaestus::image::PngCompression;
use hephaestus::plot::value::Value;
use hephaestus::plot::{linetype, scale, Plot, PlotComposition, PolygonGeom, Raw};
use hephaestus::scene::SceneBuilder;
use hephaestus::Renderer;

/// A wobbly closed ring of `n` vertices centred on `(cx, cy)` in
/// panel-fraction space, with separate x / y radii.
fn blob(cx: f64, cy: f64, rx: f64, ry: f64, n: usize) -> (Vec<f64>, Vec<f64>) {
    let mut xs = Vec::with_capacity(n);
    let mut ys = Vec::with_capacity(n);
    for i in 0..n {
        let theta = i as f64 * TAU / n as f64;
        let wobble = 1.0 + 0.12 * (3.0 * theta).sin();
        xs.push(cx + rx * wobble * theta.cos());
        ys.push(cy + ry * wobble * theta.sin());
    }
    (xs, ys)
}

fn hsv_to_color(h: f64, s: f64, v: f64) -> Color {
    let h6 = h.rem_euclid(1.0) * 6.0;
    let c = v * s;
    let x = c * (1.0 - ((h6 % 2.0) - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match h6 as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    Color::new([(r + m) as f32, (g + m) as f32, (b + m) as f32, 1.0])
}

fn main() {
    let (w, h) = (1400u32, 520u32);
    let dpi = 96.0;
    let n = 72;

    let comp = || Composition::empty(1, 1).place(1, 1, Span::cell(), Patch::new("panel"));
    let mut plot = Plot::new(&comp(), "panel");

    // ── Dashed, width ramping 2pt → 16pt around the ring ────────────
    {
        let (xs, ys) = blob(0.17, 0.52, 0.115, 0.36, n);
        let widths: Vec<f64> = (0..n)
            .map(|i| 2.0 + 14.0 * i as f64 / (n - 1) as f64)
            .collect();
        plot.add_geom(
            PolygonGeom::builder()
                .keys(vec![0; n])
                .set("x", Raw(xs))
                .set("y", Raw(ys))
                .set("fill", rgb8(232, 238, 248))
                .set("stroke", rgb8(40, 90, 180))
                .set("linewidth", widths)
                .set("linetype", Value::Linetype(linetype::dashed()))
                .build(),
        );
    }

    // ── Dashdot, hue gradient around the ring ────────────────────────
    {
        let (xs, ys) = blob(0.50, 0.52, 0.115, 0.36, n);
        let colors: Vec<Color> = (0..n)
            .map(|i| hsv_to_color(i as f64 / n as f64, 0.8, 0.9))
            .collect();
        plot.add_geom(
            PolygonGeom::builder()
                .keys(vec![1; n])
                .set("x", Raw(xs))
                .set("y", Raw(ys))
                .set("fill", rgb8(244, 240, 248))
                .set("stroke", colors)
                .set("linewidth", 4.0_f64)
                .set("linetype", Value::Linetype(linetype::dashdot()))
                .build(),
        );
    }

    // ── Marker pattern, width pulsing around the ring ────────────────
    {
        let (xs, ys) = blob(0.83, 0.52, 0.115, 0.36, n);
        let widths: Vec<f64> = (0..n)
            .map(|i| {
                let theta = i as f64 * TAU / n as f64;
                8.0 + 6.0 * (4.0 * theta).sin()
            })
            .collect();
        plot.add_geom(
            PolygonGeom::builder()
                .keys(vec![2; n])
                .set("x", Raw(xs))
                .set("y", Raw(ys))
                .set("fill", rgb8(238, 246, 236))
                .set("stroke", rgb8(90, 140, 70))
                .set("linewidth", widths)
                .set(
                    "linetype",
                    Value::Linetype(linetype::pattern([
                        linetype::dash(4.0),
                        linetype::gap(2.0),
                        linetype::marker("circle"),
                        linetype::gap(4.0),
                    ])),
                )
                .build(),
        );
    }

    let mut view = PlotComposition::new(&comp())
        .add_scale("x", scale::continuous(0.0..=1.0))
        .add_scale("y", scale::continuous(0.0..=1.0))
        .with_plot(plot);
    let issues = view.validate();
    if !issues.is_empty() {
        panic!("validate() reported issues: {issues:?}");
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
        .join("examples/polygon_dashed_variable.png");
    hephaestus::image::write_png(&path, w, h, &pixels, PngCompression::Balanced, Some(dpi))
        .expect("write png");
    println!("wrote {}", path.display());
}
