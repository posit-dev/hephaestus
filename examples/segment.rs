//! End-to-end visual sanity for `SegmentGeom`. Two renders:
//!
//! - `segment_1_errorbars.png` — points with vertical error bars. The
//!   underlying data has y values + symmetric σ; the segment goes from
//!   `(x, y - σ)` to `(x, y + σ)`. A second SegmentGeom renders the
//!   little horizontal caps at each endpoint.
//! - `segment_2_network.png` — a sparse network: PointGeom for nodes
//!   plus SegmentGeom for edges. Demonstrates that the segment endpoints
//!   are independently scaled.
//!
//! Both renders reuse PointGeom for the dots — geoms composes inside a
//! single plot panel.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{Composition, Patch, Span};
use hephaestus::geometry::Size;
#[cfg(feature = "text")]
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement};
use hephaestus::plot::{scale, Plot, PlotComposition, PointGeom, SegmentGeom};
#[cfg(feature = "text")]
use hephaestus::scales::chrome::AxisSide;
use hephaestus::Renderer;

fn main() {
    let (w, h) = (1200u32, 500u32);
    let dpi = 96.0;

    let comp = || Composition::empty(1, 1).place(1, 1, Span::cell(), Patch::new("panel"));

    let mut renderer = VelloRenderer::new().expect("vello renderer init");
    let bg: Color = rgb8(248, 248, 252);

    // ── Render 1: points with vertical error bars ────────────────────
    {
        let xs: Vec<f64> = (0..12).map(|i| i as f64).collect();
        let ys: Vec<f64> = xs.iter().map(|x| 50.0 + 15.0 * (x * 0.7).sin()).collect();
        let sigmas: Vec<f64> = xs
            .iter()
            .map(|x| 3.0 + 2.0 * (x * 0.3).cos().abs())
            .collect();
        let y_lo: Vec<f64> = ys.iter().zip(&sigmas).map(|(y, s)| y - s).collect();
        let y_hi: Vec<f64> = ys.iter().zip(&sigmas).map(|(y, s)| y + s).collect();

        let cap_color = rgb8(80, 100, 130);
        let bar_color = rgb8(120, 140, 170);
        let pt_color = rgb8(220, 90, 70);

        let mut plot = Plot::new(&comp(), "panel")
            .bind("x", "x_axis")
            .bind("x2", "x_axis")
            .bind("y", "y_axis")
            .bind("y2", "y_axis");

        // Vertical bars: x == x2 (same x), y from y_lo to y_hi.
        plot.add_geom(
            SegmentGeom::builder()
                .set("x", xs.clone())
                .set("x2", xs.clone())
                .set("y", y_lo.clone())
                .set("y2", y_hi.clone())
                .set("stroke", bar_color)
                .set("linewidth", 1.5_f64)
                .build(),
        );

        // Caps: tiny horizontal segments at each endpoint, expressed via
        // pt offsets on x/x2 (±4 pt of cap half-width).
        let cap_half_pt = 4.0;
        let xs_double: Vec<f64> = xs.iter().chain(xs.iter()).copied().collect();
        let y_caps: Vec<f64> = y_lo.iter().chain(y_hi.iter()).copied().collect();
        plot.add_geom(
            SegmentGeom::builder()
                .set("x", xs_double.clone())
                .set("x2", xs_double.clone())
                .set("y", y_caps.clone())
                .set("y2", y_caps)
                .set("x_offset", vec![-cap_half_pt; xs_double.len()])
                .set("x2_offset", vec![cap_half_pt; xs_double.len()])
                .set("stroke", cap_color)
                .set("linewidth", 1.5_f64)
                .build(),
        );

        // Centre dots.
        plot.add_geom(
            PointGeom::builder()
                .set("x", xs)
                .set("y", ys)
                .set("fill", pt_color)
                .set("size", 6.0_f64)
                .build(),
        );

        #[cfg(feature = "text")]
        {
            plot.add_axis(Axis::rail(
                "x_axis",
                AxisPlacement::Cartesian(AxisSide::Bottom),
            ));
            plot.add_axis(Axis::rail(
                "y_axis",
                AxisPlacement::Cartesian(AxisSide::Left),
            ));
        }

        let mut view = PlotComposition::new(&comp())
            .add_scale("x_axis", scale::continuous(-1.0..=12.0))
            .add_scale("y_axis", scale::continuous(20.0..=80.0))
            .with_plot(plot);

        render_to(
            &mut renderer,
            &mut view,
            w,
            h,
            dpi,
            bg,
            "examples/segment_1_errorbars.png",
        );
    }

    // ── Render 2: sparse network (nodes + edges) ─────────────────────
    {
        // Six nodes on a circle. Edges connect a small set of pairs.
        let n_nodes = 6;
        let nodes_x: Vec<f64> = (0..n_nodes)
            .map(|i| {
                let theta = (i as f64) / (n_nodes as f64) * std::f64::consts::TAU;
                50.0 + 30.0 * theta.cos()
            })
            .collect();
        let nodes_y: Vec<f64> = (0..n_nodes)
            .map(|i| {
                let theta = (i as f64) / (n_nodes as f64) * std::f64::consts::TAU;
                50.0 + 30.0 * theta.sin()
            })
            .collect();
        let edges: &[(usize, usize)] = &[(0, 1), (0, 3), (1, 2), (2, 4), (3, 4), (4, 5), (5, 0)];
        let edge_x: Vec<f64> = edges.iter().map(|(a, _)| nodes_x[*a]).collect();
        let edge_y: Vec<f64> = edges.iter().map(|(a, _)| nodes_y[*a]).collect();
        let edge_x2: Vec<f64> = edges.iter().map(|(_, b)| nodes_x[*b]).collect();
        let edge_y2: Vec<f64> = edges.iter().map(|(_, b)| nodes_y[*b]).collect();

        let edge_color = rgb8(120, 130, 160);
        let node_color = rgb8(70, 110, 200);

        let mut plot = Plot::new(&comp(), "panel")
            .bind("x", "axis")
            .bind("x2", "axis")
            .bind("y", "axis")
            .bind("y2", "axis");

        plot.add_geom(
            SegmentGeom::builder()
                .set("x", edge_x)
                .set("x2", edge_x2)
                .set("y", edge_y)
                .set("y2", edge_y2)
                .set("stroke", edge_color)
                .set("linewidth", 1.5_f64)
                .build(),
        );
        plot.add_geom(
            PointGeom::builder()
                .set("x", nodes_x)
                .set("y", nodes_y)
                .set("fill", node_color)
                .set("size", 10.0_f64)
                .build(),
        );

        #[cfg(feature = "text")]
        {
            plot.add_axis(Axis::rail(
                "axis",
                AxisPlacement::Cartesian(AxisSide::Bottom),
            ));
            plot.add_axis(Axis::rail("axis", AxisPlacement::Cartesian(AxisSide::Left)));
        }

        let mut view = PlotComposition::new(&comp())
            .add_scale("axis", scale::continuous(0.0..=100.0))
            .with_plot(plot);

        render_to(
            &mut renderer,
            &mut view,
            w,
            h,
            dpi,
            bg,
            "examples/segment_2_network.png",
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
