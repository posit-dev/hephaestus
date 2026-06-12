//! End-to-end visual sanity for `EllipseGeom`. Two renders:
//!
//! - `ellipse_1_confidence.png` — 1-σ confidence ellipses around cluster
//!   centres. Demonstrates the canonical "centre at (μ_x, μ_y), far
//!   edge at (μ_x + σ_x, μ_y + σ_y)" pattern, with the underlying
//!   scatter points overlaid via PointGeom.
//! - `ellipse_2_bubbles.png` — bubble-style chart with independent
//!   x- and y-radii per row (using pt offsets on the far edge so the
//!   radius is in absolute pixel space rather than scaled units).

use std::sync::Arc;

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{Composition, Patch, Span};
use hephaestus::geometry::Size;
#[cfg(feature = "text")]
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement};
use hephaestus::plot::value::Value;
use hephaestus::plot::{scale, EllipseGeom, Plot, PlotComposition, PointGeom};
#[cfg(feature = "text")]
use hephaestus::scales::chrome::AxisSide;
use hephaestus::Renderer;

fn main() {
    let (w, h) = (1200u32, 600u32);
    let dpi = 96.0;

    let comp = || Composition::empty(1, 1).place(1, 1, Span::cell(), Patch::new("panel"));

    let mut renderer = VelloRenderer::new().expect("vello renderer init");
    let bg: Color = rgb8(248, 248, 252);

    // ── Render 1: confidence ellipses around three clusters ──────────
    {
        // Three Gaussian-like clusters of points + a confidence ellipse
        // around each centre.
        let cluster_centres: &[(f64, f64, f64, f64, &str)] = &[
            // (μ_x, μ_y, σ_x, σ_y, group)
            (30.0, 30.0, 8.0, 5.0, "A"),
            (60.0, 50.0, 10.0, 8.0, "B"),
            (75.0, 25.0, 6.0, 12.0, "C"),
        ];

        // Scatter data — synthetic, deterministic offsets.
        let mut pt_x: Vec<f64> = Vec::new();
        let mut pt_y: Vec<f64> = Vec::new();
        let mut pt_group: Vec<&str> = Vec::new();
        for (mu_x, mu_y, sigma_x, sigma_y, group) in cluster_centres {
            for k in 0..14 {
                let t = (k as f64) * std::f64::consts::TAU / 14.0;
                // Sample around (μ, σ) deterministically.
                let r1 = 1.4 * (1.7 * t + 0.3).sin();
                let r2 = 1.2 * (2.1 * t + 1.1).cos();
                pt_x.push(mu_x + sigma_x * r1);
                pt_y.push(mu_y + sigma_y * r2);
                pt_group.push(group);
            }
        }

        // Ellipses: (μ_x, μ_y, σ_x, σ_y) → centre at μ, far edge at μ+σ.
        let e_x: Vec<f64> = cluster_centres.iter().map(|c| c.0).collect();
        let e_y: Vec<f64> = cluster_centres.iter().map(|c| c.1).collect();
        let e_x2: Vec<f64> = cluster_centres.iter().map(|c| c.0 + c.2).collect();
        let e_y2: Vec<f64> = cluster_centres.iter().map(|c| c.1 + c.3).collect();
        let e_group: Vec<&str> = cluster_centres.iter().map(|c| c.4).collect();

        let red = Color::new([0.85, 0.4, 0.4, 1.0]);
        let green = Color::new([0.4, 0.7, 0.4, 1.0]);
        let blue = Color::new([0.4, 0.55, 0.85, 1.0]);

        let mut plot = Plot::new(&comp(), "panel")
            .bind("x", "x_axis")
            .bind("x2", "x_axis")
            .bind("y", "y_axis")
            .bind("y2", "y_axis")
            .bind("fill", "cluster");

        // Ellipse layer (back).
        plot.add_geom(
            EllipseGeom::builder()
                .set("x", e_x)
                .set("y", e_y)
                .set("x2", e_x2)
                .set("y2", e_y2)
                .set("fill", e_group.clone())
                .set("fill_opacity", 0.18_f64)
                .set("stroke", e_group)
                .set("linewidth", 1.5_f64)
                .build(),
        );

        // Points on top.
        plot.add_geom(
            PointGeom::builder()
                .set("x", pt_x)
                .set("y", pt_y)
                .set("fill", pt_group)
                .set("size", 5.0_f64)
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

        let mut view = PlotComposition::new(comp())
            .add_scale("x_axis", scale::continuous(0.0..=100.0))
            .add_scale("y_axis", scale::continuous(0.0..=80.0))
            .add_scale(
                "cluster",
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
            "examples/ellipse_1_confidence.png",
        );
    }

    // ── Render 2: bubbles with independent x/y radii (pt-scale) ──────
    {
        // Each row gets a circle centred at its data point with a pt
        // radius expressed via x2_offset / y2_offset. Both x2 and y2
        // equal x/y (centre + 0 in scaled space, then nudged by pt
        // offset).
        let n = 18;
        let cx: Vec<f64> = (0..n).map(|i| (i as f64) * 5.0 + 8.0).collect();
        let cy: Vec<f64> = cx.iter().map(|x| 40.0 + 18.0 * (x * 0.07).sin()).collect();
        // Per-row pt radius (in pt units).
        let r_pt: Vec<f64> = (0..n)
            .map(|i| 4.0 + 6.0 * ((i as f64) * 0.4).sin().abs())
            .collect();
        // Independent x/y radii — give an aspect-ratio variation so it
        // doesn't just look like a PointGeom + size.
        let ry_pt: Vec<f64> = r_pt.iter().map(|r| r * 0.45).collect();

        let mut plot = Plot::new(&comp(), "panel")
            .bind("x", "x_axis")
            .bind("x2", "x_axis")
            .bind("y", "y_axis")
            .bind("y2", "y_axis");

        plot.add_geom(
            EllipseGeom::builder()
                .set("x", cx.clone())
                .set("y", cy.clone())
                .set("x2", cx)
                .set("y2", cy)
                .set("x2_offset", r_pt) // pt → +rx
                .set("y2_offset", ry_pt) // pt → +ry
                .set("fill", rgb8(70, 130, 200))
                .set("fill_opacity", 0.35_f64)
                .set("stroke", rgb8(30, 80, 140))
                .set("linewidth", 1.0_f64)
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

        let mut view = PlotComposition::new(comp())
            .add_scale("x_axis", scale::continuous(0.0..=100.0))
            .add_scale("y_axis", scale::continuous(0.0..=80.0))
            .with_plot(plot);
        render_to(
            &mut renderer,
            &mut view,
            w,
            h,
            dpi,
            bg,
            "examples/ellipse_2_bubbles.png",
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
