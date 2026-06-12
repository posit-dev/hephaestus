//! Linetype markers on `LineGeom`: stamp a registered shape every N pt
//! of arc length along the polyline, in place of (or alongside) dashes.
//!
//! Three renders:
//! - `polyline_markers_1_arrows.png` — flow path with `arrow-closed`
//!   markers every 20 pt, rotated to the local tangent. Demonstrates
//!   tangent alignment along a curved path.
//! - `polyline_markers_2_dotted.png` — densely placed `circle` markers
//!   every 4 pt — the "alternative to a fine dash pattern" use case.
//! - `polyline_markers_3_mixed.png` — pattern mixing a dash, a gap, a
//!   marker, and another gap, repeating. Confirms dashes and marker
//!   stamps coexist in one linetype.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{Composition, Patch, Span};
use hephaestus::geometry::Size;
#[cfg(feature = "text")]
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement};
use hephaestus::plot::{linetype, scale, LineGeom, Plot, PlotComposition, Value};
#[cfg(feature = "text")]
use hephaestus::scales::chrome::AxisSide;
use hephaestus::Renderer;

fn main() {
    let (w, h) = (1200u32, 400u32);
    let dpi = 96.0;

    let comp = || Composition::empty(1, 1).place(1, 1, Span::cell(), Patch::new("panel"));

    // ── Synthetic flow path: 80 vertices on a sine wave. ────────────────
    let n = 80;
    let xs: Vec<f64> = (0..n).map(|i| i as f64 / (n - 1) as f64 * 60.0).collect();
    let ys: Vec<f64> = xs.iter().map(|x| 50.0 + 20.0 * (x * 0.18).sin()).collect();

    let bg: Color = rgb8(248, 248, 252);
    let stroke_col = rgb8(40, 90, 180);

    let mut renderer = VelloRenderer::new().expect("vello renderer init");

    // ── Render 1: arrowheads along a flow path ──────────────────────────
    let arrow_pattern = linetype::pattern([linetype::marker("arrow-closed"), linetype::gap(20.0)]);
    let mut view = build_view(
        comp(),
        xs.clone(),
        ys.clone(),
        stroke_col,
        arrow_pattern,
        /* linewidth */ 16.0,
    );
    render_to(
        &mut renderer,
        &mut view,
        w,
        h,
        dpi,
        bg,
        "examples/polyline_markers_1_arrows.png",
    );

    // ── Render 2: dotted line via tightly-spaced circle markers ────────
    let dotted_pattern = linetype::pattern([linetype::marker("circle"), linetype::gap(4.0)]);
    let mut view = build_view(
        comp(),
        xs.clone(),
        ys.clone(),
        stroke_col,
        dotted_pattern,
        /* linewidth */ 8.0,
    );
    render_to(
        &mut renderer,
        &mut view,
        w,
        h,
        dpi,
        bg,
        "examples/polyline_markers_2_dotted.png",
    );

    // ── Render 3: mixed dash + marker pattern ───────────────────────────
    let mixed_pattern = linetype::pattern([
        linetype::dash(6.0),
        linetype::gap(2.0),
        linetype::marker("circle"),
        linetype::gap(4.0),
    ]);
    let mut view = build_view(
        comp(),
        xs.clone(),
        ys.clone(),
        stroke_col,
        mixed_pattern,
        /* linewidth */ 10.0,
    );
    render_to(
        &mut renderer,
        &mut view,
        w,
        h,
        dpi,
        bg,
        "examples/polyline_markers_3_mixed.png",
    );
}

fn build_view(
    comp: Composition,
    xs: Vec<f64>,
    ys: Vec<f64>,
    stroke: Color,
    pattern: std::sync::Arc<[hephaestus::plot::LinetypeStep]>,
    linewidth_pt: f64,
) -> PlotComposition {
    let mut plot = Plot::new(&comp, "panel")
        .bind("x", "x_scale")
        .bind("y", "y_scale");
    plot.add_geom(
        LineGeom::builder()
            .set("x", xs)
            .set("y", ys)
            .set("stroke", stroke)
            .set("linewidth", linewidth_pt)
            .set("linetype", Value::Linetype(pattern))
            .build(),
    );
    #[cfg(feature = "text")]
    {
        plot.add_axis(Axis::rail(
            "x_scale",
            AxisPlacement::Cartesian(AxisSide::Bottom),
        ));
        plot.add_axis(Axis::rail(
            "y_scale",
            AxisPlacement::Cartesian(AxisSide::Left),
        ));
    }
    PlotComposition::new(comp)
        .add_scale("x_scale", scale::continuous(0.0..=60.0))
        .add_scale("y_scale", scale::continuous(0.0..=100.0))
        .with_plot(plot)
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
