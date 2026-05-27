//! End-to-end visual sanity for `PolygonGeom`. Two renders:
//!
//! - `polygon_1_donut.png` — one mark with two rings: an outer square
//!   and an inner square hole. Demonstrates that EvenOdd fill rule
//!   treats the inner closed sub-path as a hole automatically.
//! - `polygon_2_multi.png` — three triangles in one geom call, each
//!   identified by a key. Demonstrates the multi-row-per-mark + per-mark
//!   styling channels.

use std::sync::Arc;

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{Composition, Patch, Span};
use hephaestus::geometry::Size;
use hephaestus::plot::value::Value;
use hephaestus::plot::{scale, Plot, PlotComposition, PolygonGeom};
use hephaestus::Renderer;

fn main() {
    let (w, h) = (1200u32, 500u32);
    let dpi = 96.0;

    let comp = || Composition::empty(1, 1).place(1, 1, Span::cell(), Patch::new("panel"));

    let mut renderer = VelloRenderer::new().expect("vello renderer init");
    let bg: Color = rgb8(248, 248, 252);

    // ── Render 1: donut (square with square hole) ────────────────────
    {
        // Single mark, two rings. Outer: 4 corners of a big square.
        // Inner: 4 corners of a small square (hole).
        let x = vec![0.1_f64, 0.9, 0.9, 0.1, 0.35, 0.65, 0.65, 0.35];
        let y = vec![0.1_f64, 0.1, 0.9, 0.9, 0.35, 0.35, 0.65, 0.65];
        let ring = vec![0_i32, 0, 0, 0, 1, 1, 1, 1];

        let mut plot = Plot::new(&comp(), "panel")
            .bind("x", "x_axis")
            .bind("y", "y_axis");
        plot.add_geom(
            PolygonGeom::builder()
                .set("x", x)
                .set("y", y)
                .set("ring", ring)
                .set("fill", rgb8(180, 90, 70))
                .set("stroke", rgb8(80, 30, 20))
                .set("linewidth", 1.5_f64)
                .build(),
        );

        let mut view = PlotComposition::new(comp())
            .add_scale("x_axis", scale::continuous(0.0..=1.0))
            .add_scale("y_axis", scale::continuous(0.0..=1.0))
            .with_plot(plot);
        render_to(
            &mut renderer,
            &mut view,
            w,
            h,
            dpi,
            bg,
            "examples/polygon_1_donut.png",
        );
    }

    // ── Render 2: three triangles, one geom call ─────────────────────
    {
        // Three marks (A, B, C), three vertices each.
        let keys = vec!["A", "A", "A", "B", "B", "B", "C", "C", "C"];
        let xs = vec![
            0.1_f64, 0.30, 0.20, // A
            0.40, 0.60, 0.50, // B
            0.70, 0.90, 0.80, // C
        ];
        let ys = vec![
            0.20, 0.20, 0.80, // A: upward triangle
            0.80, 0.80, 0.20, // B: downward triangle
            0.20, 0.20, 0.80, // C: upward
        ];
        // Per-row data; per-mark resolution reads the first row's value.
        let cat = vec!["A", "A", "A", "B", "B", "B", "C", "C", "C"];

        let red = Color::new([0.85, 0.4, 0.4, 1.0]);
        let green = Color::new([0.4, 0.7, 0.4, 1.0]);
        let blue = Color::new([0.4, 0.55, 0.85, 1.0]);

        let mut plot = Plot::new(&comp(), "panel")
            .bind("x", "x_axis")
            .bind("y", "y_axis")
            .bind("fill", "cat_fill");
        plot.add_geom(
            PolygonGeom::builder()
                .keys(keys)
                .set("x", xs)
                .set("y", ys)
                .set("fill", cat)
                .set("stroke", rgb8(40, 40, 40))
                .set("linewidth", 1.5_f64)
                .build(),
        );

        let mut view = PlotComposition::new(comp())
            .add_scale("x_axis", scale::continuous(0.0..=1.0))
            .add_scale("y_axis", scale::continuous(0.0..=1.0))
            .add_scale(
                "cat_fill",
                scale::ordinal(
                    ["A", "B", "C"]
                        .into_iter()
                        .map(|s| Value::String(Arc::from(s))),
                )
                .range_colors([red, green, blue]),
            )
            .with_plot(plot);
        render_to(
            &mut renderer,
            &mut view,
            w,
            h,
            dpi,
            bg,
            "examples/polygon_2_multi.png",
        );
    }
}

fn render_to(
    renderer: &mut VelloRenderer,
    view: &mut PlotComposition,
    w: u32,
    h: u32,
    dpi: f64,
    bg: Color,
    out_relative: &str,
) {
    {
        let scene = renderer.scene();
        scene.clear();
        view.render(scene, Size::new(w as f64, h as f64), dpi);
    }
    let mut pixels = vec![0u8; (w * h * 4) as usize];
    renderer
        .render_to_buffer(w, h, bg, &mut pixels)
        .expect("render");
    let path = std::env::current_dir().unwrap().join(out_relative);
    hephaestus::png::write_png(&path, w, h, &pixels).expect("write png");
    println!("wrote {}", path.display());
}
