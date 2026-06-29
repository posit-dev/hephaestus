//! Endpoint markers on `LineGeom` and `SegmentGeom` (Phase C.5). Stamps
//! a registered shape at each endpoint of a polyline / segment, rotated
//! to point along the chord toward the original (pre-clip) endpoint.
//!
//! Five renders:
//! - `endpoint_arrows_1_basic.png` — a single LineGeom flow path with
//!   `end_marker = "arrow-closed"`. Canonical directed-edge case.
//! - `endpoint_arrows_2_both_ends.png` — SegmentGeom rows with
//!   `start_marker = "arrow-stealth"` and `end_marker = "arrow-closed"`,
//!   plus per-endpoint fill colours. Demonstrates outward-pointing
//!   rotation on both ends and asymmetric fill control.
//! - `endpoint_arrows_3_clipped.png` — line with `clip_end_radius` set,
//!   `end_marker = "arrow-stealth"`. The arrowhead attaches cleanly to
//!   the trimmed endpoint.
//! - `endpoint_arrows_4_sized.png` — three segments with increasing
//!   `end_marker_size` against a fixed `linewidth`. Size channel
//!   decoupled from linewidth.
//! - `endpoint_arrows_5_combined.png` — a path carrying both linetype
//!   markers along its length AND endpoint markers at its ends.
//!   Confirms the two systems compose.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{Composition, Patch, Span};
use hephaestus::geometry::Size;
#[cfg(feature = "text")]
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement};
use hephaestus::plot::{linetype, scale, LineGeom, Plot, PlotComposition, SegmentGeom, Value};
#[cfg(feature = "text")]
use hephaestus::scales::chrome::AxisSide;
use hephaestus::Renderer;

fn main() {
    let (w, h) = (1200u32, 400u32);
    let dpi = 96.0;

    let comp = || Composition::empty(1, 1).place(1, 1, Span::cell(), Patch::new("panel"));

    let bg: Color = rgb8(248, 248, 252);
    let stroke_col = rgb8(40, 90, 180);

    let mut renderer = VelloRenderer::new().expect("vello renderer init");

    // ── Render 1: a flow path with one arrowhead at the end ─────────────
    {
        let n = 80;
        let xs: Vec<f64> = (0..n).map(|i| i as f64 / (n - 1) as f64 * 55.0).collect();
        let ys: Vec<f64> = xs.iter().map(|x| 50.0 + 20.0 * (x * 0.18).sin()).collect();

        let mut plot = Plot::new(&comp(), "panel")
            .bind("x", "x_scale")
            .bind("y", "y_scale");
        plot.add_geom(
            LineGeom::builder()
                .set("x", xs)
                .set("y", ys)
                .set("stroke", stroke_col)
                .set("linewidth", 3.0_f64)
                .set("end_marker", "arrow-closed")
                .set("end_marker_size", 24.0_f64)
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
        let mut view = PlotComposition::new(&comp())
            .add_scale("x_scale", scale::continuous(0.0..=60.0))
            .add_scale("y_scale", scale::continuous(0.0..=100.0))
            .with_plot(plot);
        render_to(
            &mut renderer,
            &mut view,
            w,
            h,
            dpi,
            bg,
            "examples/endpoint_arrows_1_basic.png",
        );
    }

    // ── Render 2: both ends, asymmetric shapes + fills ──────────────────
    {
        // Three diagonal segments, all with start_marker / end_marker.
        let xs: Vec<f64> = vec![5.0, 5.0, 5.0];
        let xs2: Vec<f64> = vec![55.0, 55.0, 55.0];
        let ys: Vec<f64> = vec![80.0, 50.0, 20.0];
        let ys2: Vec<f64> = vec![20.0, 50.0, 80.0];

        let start_fill = rgb8(40, 170, 100);
        let end_fill = rgb8(220, 100, 60);

        let mut plot = Plot::new(&comp(), "panel")
            .bind("x", "x_scale")
            .bind("x2", "x_scale")
            .bind("y", "y_scale")
            .bind("y2", "y_scale");
        plot.add_geom(
            SegmentGeom::builder()
                .set("x", xs)
                .set("x2", xs2)
                .set("y", ys)
                .set("y2", ys2)
                .set("stroke", stroke_col)
                .set("linewidth", 2.0_f64)
                .set("start_marker", "arrow-stealth")
                .set("end_marker", "arrow-closed")
                .set("start_marker_size", 12.0_f64)
                .set("end_marker_size", 14.0_f64)
                .set("start_marker_fill", start_fill)
                .set("end_marker_fill", end_fill)
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
        let mut view = PlotComposition::new(&comp())
            .add_scale("x_scale", scale::continuous(0.0..=60.0))
            .add_scale("y_scale", scale::continuous(0.0..=100.0))
            .with_plot(plot);
        render_to(
            &mut renderer,
            &mut view,
            w,
            h,
            dpi,
            bg,
            "examples/endpoint_arrows_2_both_ends.png",
        );
    }

    // ── Render 3: endpoint marker attaches to a clipped end ─────────────
    {
        // Line trimmed at the end; arrowhead sits cleanly on the trim
        // and points back toward the original endpoint.
        let xs: Vec<f64> = (0..40).map(|i| i as f64 / 39.0 * 60.0).collect();
        let ys: Vec<f64> = xs.iter().map(|x| 30.0 + 25.0 * (x * 0.15).cos()).collect();

        let mut plot = Plot::new(&comp(), "panel")
            .bind("x", "x_scale")
            .bind("y", "y_scale");
        plot.add_geom(
            LineGeom::builder()
                .set("x", xs)
                .set("y", ys)
                .set("stroke", stroke_col)
                .set("linewidth", 3.0_f64)
                .set("clip_end_radius", 14.0_f64)
                .set("end_marker", "arrow-stealth")
                .set("end_marker_size", 20.0_f64)
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
        let mut view = PlotComposition::new(&comp())
            .add_scale("x_scale", scale::continuous(0.0..=60.0))
            .add_scale("y_scale", scale::continuous(0.0..=100.0))
            .with_plot(plot);
        render_to(
            &mut renderer,
            &mut view,
            w,
            h,
            dpi,
            bg,
            "examples/endpoint_arrows_3_clipped.png",
        );
    }

    // ── Render 4: end-marker size decoupled from linewidth ──────────────
    {
        // Three horizontal segments stacked, fixed linewidth, growing
        // end_marker_size.
        let xs = vec![5.0, 5.0, 5.0];
        let xs2 = vec![55.0, 55.0, 55.0];
        let ys = vec![25.0, 50.0, 75.0];
        let ys2 = ys.clone();
        let sizes = vec![8.0, 16.0, 28.0];

        let mut plot = Plot::new(&comp(), "panel")
            .bind("x", "x_scale")
            .bind("x2", "x_scale")
            .bind("y", "y_scale")
            .bind("y2", "y_scale");
        plot.add_geom(
            SegmentGeom::builder()
                .set("x", xs)
                .set("x2", xs2)
                .set("y", ys)
                .set("y2", ys2)
                .set("stroke", stroke_col)
                .set("linewidth", 2.0_f64)
                .set("end_marker", "arrow-closed")
                .set("end_marker_size", sizes)
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
        let mut view = PlotComposition::new(&comp())
            .add_scale("x_scale", scale::continuous(0.0..=60.0))
            .add_scale("y_scale", scale::continuous(0.0..=100.0))
            .with_plot(plot);
        render_to(
            &mut renderer,
            &mut view,
            w,
            h,
            dpi,
            bg,
            "examples/endpoint_arrows_4_sized.png",
        );
    }

    // ── Render 5: combined linetype markers + endpoint markers ──────────
    {
        // Dots along the line plus an arrowhead at the end. Confirms the
        // two systems compose: linetype walker fires per-period
        // alongside the endpoint-marker emitter at the line's actual end.
        let n = 80;
        let xs: Vec<f64> = (0..n).map(|i| i as f64 / (n - 1) as f64 * 55.0).collect();
        let ys: Vec<f64> = xs.iter().map(|x| 50.0 + 15.0 * (x * 0.2).sin()).collect();

        let dotted = linetype::pattern([linetype::marker("circle"), linetype::gap(8.0)]);
        let mut plot = Plot::new(&comp(), "panel")
            .bind("x", "x_scale")
            .bind("y", "y_scale");
        plot.add_geom(
            LineGeom::builder()
                .set("x", xs)
                .set("y", ys)
                .set("stroke", stroke_col)
                .set("linewidth", 4.0_f64)
                .set("linetype", Value::Linetype(dotted))
                .set("end_marker", "arrow-closed")
                .set("end_marker_size", 18.0_f64)
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
        let mut view = PlotComposition::new(&comp())
            .add_scale("x_scale", scale::continuous(0.0..=60.0))
            .add_scale("y_scale", scale::continuous(0.0..=100.0))
            .with_plot(plot);
        render_to(
            &mut renderer,
            &mut view,
            w,
            h,
            dpi,
            bg,
            "examples/endpoint_arrows_5_combined.png",
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
