//! The `vertex_marker` channel family: a shape stamped at every vertex
//! of a line or polygon, with an arc-length buffer trimming the stroke
//! around it — the interior analogue of the endpoint markers and clip
//! radii. Writes `examples/vertex_markers.png`.
//!
//! Three panels:
//!
//! 1. A zigzag line with a `"circle"` marker at every vertex and no
//!    explicit buffer — each vertex's buffer falls back to the marker's
//!    own forward extent, so the stroke breaks cleanly around every
//!    stamp, graph-layout style.
//! 2. The same line with per-row `vertex_marker_size` and
//!    `vertex_marker_fill` — the channels resolve per vertex, so they
//!    can be data-mapped.
//! 3. A polygon whose vertex markers trim the **stroke ring only**: the
//!    fill still emits from the untrimmed ring.
//!
//! ```sh
//! cargo run --example vertex_markers
//! ```

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, rgba, Color};
use hephaestus::composition::{grid, Patch};
use hephaestus::geometry::Size;
use hephaestus::image::PngCompression;
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement};
use hephaestus::plot::{scale, LineGeom, Plot, PlotComposition, PolygonGeom};
use hephaestus::scales::chrome::AxisSide;
use hephaestus::{Renderer, SceneBuilder};

fn main() {
    let (w, h) = (1200u32, 420u32);
    let dpi = 96.0;
    let size = Size::new(w as f64, h as f64);

    let comp = || {
        grid(
            1,
            3,
            vec![
                Patch::new("buffered").into(),
                Patch::new("mapped").into(),
                Patch::new("polygon").into(),
            ],
        )
    };

    let ink = rgb8(40, 44, 52);
    let accent = rgb8(60, 120, 200);

    // A shared zigzag for the first two panels.
    let xs: Vec<f64> = (0..7).map(|i| i as f64).collect();
    let ys: Vec<f64> = vec![1.0, 3.0, 1.5, 4.0, 2.0, 3.5, 1.0];

    // 1. Open (hollow) markers: a fully transparent `vertex_marker_fill`
    // leaves just the outline ring, whose colour and thickness follow
    // the vertex's stroke and linewidth. No `vertex_marker_buffer` set,
    // so each marker trims the stroke by its own extent.
    let mut plot_buffered = Plot::new(&comp(), "buffered")
        .bind("x", "x")
        .bind("y", "y")
        .subtitle("Open circles, auto-buffered");
    plot_buffered.add_geom(
        LineGeom::builder()
            .set("x", xs.clone())
            .set("y", ys.clone())
            .set("stroke", ink)
            .set("linewidth", 2.0_f64)
            .set("vertex_marker", "circle")
            .set("vertex_marker_fill", rgba(0.0, 0.0, 0.0, 0.0))
            .build(),
    );

    // 2. Data-mapped markers: size and fill vary per vertex, and an
    // explicit buffer clears more room around every stamp.
    let sizes: Vec<f64> = (0..7).map(|i| 3.0 + i as f64).collect();
    let fills: Vec<Color> = (0..7)
        .map(|i| {
            let t = i as f64 / 6.0;
            rgb8(
                (60.0 + 160.0 * t) as u8,
                (120.0 - 60.0 * t) as u8,
                (200.0 - 140.0 * t) as u8,
            )
        })
        .collect();
    let mut plot_mapped = Plot::new(&comp(), "mapped")
        .bind("x", "x")
        .bind("y", "y")
        .subtitle("Data-mapped size & fill");
    plot_mapped.add_geom(
        LineGeom::builder()
            .set("x", xs)
            .set("y", ys)
            .set("stroke", ink)
            .set("linewidth", 2.0_f64)
            .set("vertex_marker", "circle")
            .set("vertex_marker_size", sizes)
            .set("vertex_marker_fill", fills)
            .set("vertex_marker_buffer", 6.0_f64)
            .build(),
    );

    // 3. A hexagon whose markers trim the stroke ring at each vertex;
    // the fill comes from the untrimmed ring.
    let hex: Vec<(f64, f64)> = (0..6)
        .map(|i| {
            let a = std::f64::consts::FRAC_PI_2 + i as f64 * std::f64::consts::FRAC_PI_3;
            (3.0 + 2.0 * a.cos(), 2.5 + 2.0 * a.sin())
        })
        .collect();
    let mut plot_polygon = Plot::new(&comp(), "polygon")
        .bind("x", "x")
        .bind("y", "y")
        .subtitle("Ring trimmed, fill untrimmed");
    plot_polygon.add_geom(
        PolygonGeom::builder()
            .set("x", hex.iter().map(|p| p.0).collect::<Vec<_>>())
            .set("y", hex.iter().map(|p| p.1).collect::<Vec<_>>())
            .set("fill", rgb8(225, 235, 248))
            .set("stroke", ink)
            .set("linewidth", 2.5_f64)
            .set("vertex_marker", "circle")
            .set("vertex_marker_fill", accent)
            .build(),
    );

    for plot in [&mut plot_buffered, &mut plot_mapped, &mut plot_polygon] {
        plot.add_axis(Axis::rail("x", AxisPlacement::Cartesian(AxisSide::Bottom)));
        plot.add_axis(Axis::rail("y", AxisPlacement::Cartesian(AxisSide::Left)));
    }

    let mut view = PlotComposition::new(&comp())
        .add_scale("x", scale::continuous(-0.5..=6.5))
        .add_scale("y", scale::continuous(0.25..=5.0))
        .with_plot(plot_buffered)
        .with_plot(plot_mapped)
        .with_plot(plot_polygon);
    view.set_title("Vertex markers with stroke buffering");

    let mut renderer = VelloRenderer::new().expect("vello renderer init");
    {
        let scene = renderer.scene();
        scene.clear();
        view.render(scene, size, dpi);
    }
    let mut pixels = vec![0u8; (w * h * 4) as usize];
    renderer
        .render_to_buffer(w, h, Color::WHITE, &mut pixels)
        .expect("render");

    let path = std::env::current_dir()
        .unwrap()
        .join("examples/vertex_markers.png");
    hephaestus::image::write_png(&path, w, h, &pixels, PngCompression::Balanced, Some(dpi))
        .expect("write png");
    println!("wrote {}", path.display());
}
