//! End-to-end visual sanity for `RibbonBSplineGeom` — filled band whose
//! two boundary curves are clamped uniform-knot B-splines. Per-mark
//! grouping (rows sharing a key form one band), per-mark `degree` /
//! `interpolation` configuration, variance-detect fill (uniform → solid
//! brush; varying → quad-strip mesh), independent outlines on curve A
//! vs curve B with the full LineGeom-style stroke surface (dashes,
//! markers, clipping), and terminal-cap densification under polar.
//!
//! Three renders, all with full plotting chrome:
//!
//! - `ribbon_bspline_1_horizontal.png` — horizontal smoothed band over
//!   a 6-point control polygon, constant baseline. Curve A outlined.
//! - `ribbon_bspline_2_freeform.png` — free-form band with two
//!   independent spline boundaries and per-row varying fill (mesh
//!   path). Both curves outlined with different dash patterns.
//! - `ribbon_bspline_3_polar.png` — polar Free band rendered in both
//!   `"domain"` and `"panel"` interpolation modes side by side. The
//!   closing radial edges curve visibly in both modes — the test for
//!   terminal-cap densification under panel mode (where the spline
//!   portions are pixel-space chords).

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{beside, Composition, Patch, Span};
use hephaestus::geometry::Size;
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement, PolarRing};
use hephaestus::plot::projection::Projection;
use hephaestus::plot::value::LinetypeStep;
use hephaestus::plot::{scale, Plot, PlotComposition, RibbonBSplineGeom};
use hephaestus::scales::chrome::AxisSide;
use hephaestus::Renderer;
use std::sync::Arc;

fn cell_comp() -> Composition {
    Composition::empty(1, 1).place(1, 1, Span::cell(), Patch::new("panel"))
}

fn main() {
    let (w, h) = (1200u32, 500u32);
    let dpi = 96.0;

    let mut renderer = VelloRenderer::new().expect("vello renderer init");
    let bg: Color = rgb8(248, 248, 252);

    // ── Render 1: horizontal smoothed band ──
    {
        let xs: Vec<f64> = vec![0.0, 1.0, 2.0, 3.0, 4.0, 5.0];
        let ys: Vec<f64> = vec![30.0, 70.0, 50.0, 80.0, 40.0, 60.0];
        let mut plot = Plot::new(&cell_comp(), "panel")
            .title("B-spline ribbon — horizontal area")
            .subtitle("Six control points, cubic spline, constant baseline")
            .bind("x", "time")
            .bind("y", "value");
        plot.add_geom(
            RibbonBSplineGeom::builder()
                .set("x", xs)
                .set("y", ys)
                .set("y2", 0.0_f64)
                .set("fill", rgb8(80, 150, 220))
                .set("alpha", 0.35_f64)
                .set("stroke", rgb8(20, 60, 130))
                .set("linewidth", 2.0_f64)
                .build(),
        );
        plot.add_axis(Axis::rail("time", AxisPlacement::Cartesian(AxisSide::Bottom)).title("Time"));
        plot.add_axis(Axis::rail("value", AxisPlacement::Cartesian(AxisSide::Left)).title("Value"));

        let mut view = PlotComposition::new(cell_comp())
            .add_scale("time", scale::continuous(0.0..=5.0))
            .add_scale("value", scale::continuous(0.0..=100.0))
            .with_plot(plot);
        panic_on_issues(view.validate());
        render_to(
            &mut renderer,
            &mut view,
            w,
            h,
            dpi,
            bg,
            "examples/ribbon_bspline_1_horizontal.png",
        );
    }

    // ── Render 2: free-form band with per-row fill + dashed outlines ──
    {
        // Curve A is the upper boundary (ys_a stays above ys_b everywhere
        // so the band doesn't fold over itself). Both curves use
        // independent control polygons in (x, y) so the ribbon is genuinely
        // free-form, not just a horizontal band.
        let n = 9usize;
        let xs_a: Vec<f64> = (0..n).map(|i| 1.0 + i as f64 * 1.25).collect();
        let ys_a: Vec<f64> = vec![70.0, 90.0, 80.0, 95.0, 75.0, 92.0, 78.0, 88.0, 72.0];
        let xs_b: Vec<f64> = (0..n).map(|i| 0.6 + i as f64 * 1.3).collect();
        let ys_b: Vec<f64> = vec![20.0, 12.0, 25.0, 8.0, 22.0, 14.0, 28.0, 16.0, 24.0];
        let fills: Vec<Color> = (0..n)
            .map(|i| {
                let t = i as f64 / (n - 1) as f64;
                lerp_color(rgb8(220, 80, 80), rgb8(60, 80, 220), t)
            })
            .collect();
        let dashed_a: Arc<[LinetypeStep]> =
            Arc::from(vec![LinetypeStep::Dash(8.0), LinetypeStep::Gap(4.0)]);
        let dashed_b: Arc<[LinetypeStep]> =
            Arc::from(vec![LinetypeStep::Dash(3.0), LinetypeStep::Gap(3.0)]);

        let mut plot = Plot::new(&cell_comp(), "panel")
            .title("Free-form B-spline ribbon")
            .subtitle("Two independent control polygons, per-row fill, mismatched dashes")
            .bind("x", "time")
            .bind("y", "value")
            .bind("x2", "time")
            .bind("y2", "value");
        plot.add_geom(
            RibbonBSplineGeom::builder()
                .set("x", xs_a)
                .set("y", ys_a)
                .set("x2", xs_b)
                .set("y2", ys_b)
                .set("fill", fills)
                .set("alpha", 0.55_f64)
                .set("stroke", rgb8(120, 30, 30))
                .set("linewidth", 2.0_f64)
                .set(
                    "linetype",
                    hephaestus::plot::value::Value::Linetype(dashed_a),
                )
                .set("stroke2", rgb8(30, 30, 120))
                .set("linewidth2", 2.0_f64)
                .set(
                    "linetype2",
                    hephaestus::plot::value::Value::Linetype(dashed_b),
                )
                .build(),
        );
        plot.add_axis(Axis::rail("time", AxisPlacement::Cartesian(AxisSide::Bottom)).title("Time"));
        plot.add_axis(Axis::rail("value", AxisPlacement::Cartesian(AxisSide::Left)).title("Value"));

        let mut view = PlotComposition::new(cell_comp())
            .add_scale("time", scale::continuous(0.0..=12.0))
            .add_scale("value", scale::continuous(0.0..=100.0))
            .with_plot(plot);
        panic_on_issues(view.validate());
        render_to(
            &mut renderer,
            &mut view,
            w,
            h,
            dpi,
            bg,
            "examples/ribbon_bspline_2_freeform.png",
        );
    }

    // ── Render 3: polar Free band, domain vs panel interpolation ──
    {
        // Shared control polygons for both panels: curve A at outer
        // radius with varying theta, curve B at inner radius, also
        // varying theta. The cap connects different (theta, r) at the
        // start and end — visibly curved in polar.
        let n = 6usize;
        let xs_a: Vec<f64> = (0..n).map(|i| 0.05 + 0.18 * i as f64).collect();
        let ys_a: Vec<f64> = vec![0.85, 0.95, 0.80, 0.95, 0.85, 0.95];
        let xs_b: Vec<f64> = (0..n).map(|i| 0.10 + 0.18 * i as f64).collect();
        let ys_b: Vec<f64> = vec![0.45, 0.35, 0.50, 0.35, 0.45, 0.35];

        let comp_shape = beside(Patch::new("dom"), Patch::new("pan"));

        let mut plot_dom = Plot::new(&comp_shape, "dom")
            .title("interpolation = \"domain\"")
            .subtitle("spline samples projected one-by-one")
            .bind("x", "theta")
            .bind("y", "radius")
            .bind("x2", "theta")
            .bind("y2", "radius")
            .projection(Projection::polar());
        plot_dom.add_geom(
            RibbonBSplineGeom::builder()
                .set("x", xs_a.clone())
                .set("y", ys_a.clone())
                .set("x2", xs_b.clone())
                .set("y2", ys_b.clone())
                .set("fill", rgb8(220, 100, 60))
                .set("alpha", 0.55_f64)
                .set("stroke", rgb8(140, 40, 20))
                .set("linewidth", 1.5_f64)
                .set("stroke2", rgb8(140, 40, 20))
                .set("linewidth2", 1.5_f64)
                .build(),
        );
        plot_dom.add_axis(
            Axis::rail("theta", AxisPlacement::PolarAngular(PolarRing::Outer)).title("theta"),
        );
        plot_dom.add_axis(
            Axis::rail("radius", AxisPlacement::PolarRadius { theta_frac: 0.0 }).title("r"),
        );

        let mut plot_pan = Plot::new(&comp_shape, "pan")
            .title("interpolation = \"panel\"")
            .subtitle("spline in pixel space, caps still polar")
            .bind("x", "theta")
            .bind("y", "radius")
            .bind("x2", "theta")
            .bind("y2", "radius")
            .projection(Projection::polar());
        plot_pan.add_geom(
            RibbonBSplineGeom::builder()
                .set("x", xs_a)
                .set("y", ys_a)
                .set("x2", xs_b)
                .set("y2", ys_b)
                .set("interpolation", "panel")
                .set("fill", rgb8(60, 120, 220))
                .set("alpha", 0.55_f64)
                .set("stroke", rgb8(20, 60, 140))
                .set("linewidth", 1.5_f64)
                .set("stroke2", rgb8(20, 60, 140))
                .set("linewidth2", 1.5_f64)
                .build(),
        );
        plot_pan.add_axis(
            Axis::rail("theta", AxisPlacement::PolarAngular(PolarRing::Outer)).title("theta"),
        );
        plot_pan.add_axis(
            Axis::rail("radius", AxisPlacement::PolarRadius { theta_frac: 0.0 }).title("r"),
        );

        let mut view = PlotComposition::new(comp_shape)
            .add_scale("theta", scale::continuous(0.0..=1.0))
            .add_scale("radius", scale::continuous(0.0..=1.0))
            .with_plot(plot_dom)
            .with_plot(plot_pan);
        panic_on_issues(view.validate());
        render_to(
            &mut renderer,
            &mut view,
            w,
            h,
            dpi,
            bg,
            "examples/ribbon_bspline_3_polar.png",
        );
    }
}

fn lerp_color(a: Color, b: Color, t: f64) -> Color {
    let t = t.clamp(0.0, 1.0) as f32;
    let ac = a.components;
    let bc = b.components;
    Color::new([
        ac[0] + (bc[0] - ac[0]) * t,
        ac[1] + (bc[1] - ac[1]) * t,
        ac[2] + (bc[2] - ac[2]) * t,
        ac[3] + (bc[3] - ac[3]) * t,
    ])
}

fn panic_on_issues<T: std::fmt::Debug>(issues: Vec<T>) {
    if !issues.is_empty() {
        panic!("validate() reported issues: {issues:?}");
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
