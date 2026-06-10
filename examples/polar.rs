//! Polar projection — full-circle scatter, full-circle bar (rose
//! chart), half-disk gauge, and a non-axis-aligned partial arc
//! (-60° → 135°) with a centre hole. Exercises `Projection::polar()`,
//! `Projection::gauge()`, and a hand-rolled `PolarProjection` for the
//! asymmetric sweep — confirms the bbox-based geometry centres the
//! projection correctly when the swept region isn't aligned to a
//! cardinal axis.
//!
//! For a chord-style (radar / spider chart) variant covering the same
//! four plot kinds, see `examples/radar.rs`.
//!
//! Produces `examples/polar.png`.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{beside, Composition, Patch};
use hephaestus::geometry::Size;
use hephaestus::plot::projection::{PolarEdgeStyle, PolarProjection, Projection};
use hephaestus::plot::{scale, Plot, PlotComposition, PointGeom, RectGeom, SegmentGeom};
use hephaestus::Renderer;

fn comp_shape() -> Composition {
    beside(
        beside(
            beside(Patch::new("scatter"), Patch::new("rose")),
            Patch::new("gauge"),
        ),
        Patch::new("partial"),
    )
}

/// Non-axis-aligned partial polar — sweeps from -60° (5 o'clock) CCW
/// through 0° (3 o'clock), 90° (12 o'clock), to 135° (10:30 position).
/// Inner radius leaves a 20 % hole. The bbox-aware geometry should
/// place the polar centre off-centre in the panel — the sweep extends
/// fully to the right (+x at θ=0°) and up (+y at θ=90°) but not far
/// left or down, so the bbox is asymmetric around the math origin.
fn partial_projection() -> Projection {
    Projection::Polar(PolarProjection {
        angle_channel: "x".into(),
        radius_channel: "y".into(),
        theta_start: -std::f64::consts::PI / 3.0,
        theta_end: 3.0 * std::f64::consts::PI / 4.0,
        inner_radius_frac: 0.2,
        edge_style: PolarEdgeStyle::Geodesic,
        theta_break_fracs: Vec::new(),
    })
}

fn main() {
    let (w, h) = (2000u32, 500u32);
    let dpi = 96.0;

    // Scatter: 60 points distributed at varying theta + radius.
    let scatter_n = 60;
    let scatter_theta: Vec<f64> = (0..scatter_n)
        .map(|i| (i as f64 / scatter_n as f64) * std::f64::consts::TAU)
        .collect();
    let scatter_radius: Vec<f64> = (0..scatter_n)
        .map(|i| {
            // Mix of inner and outer points — visual spread.
            let t = (i as f64 * 0.137).sin() * 0.5 + 0.5;
            0.2 + 0.8 * t
        })
        .collect();
    let dot = rgb8(40, 100, 200);

    // Rose chart: 12 bars (one per "hour"), each is an annular
    // wedge spanning a 30° slice. Now using `RectGeom` which
    // densifies its four edges under non-linear projections so the
    // theta-axis edges curve along the projected arc.
    let rose_n = 12;
    let rose_step = std::f64::consts::TAU / rose_n as f64;
    let rose_theta_a: Vec<f64> = (0..rose_n).map(|i| (i as f64 - 0.4) * rose_step).collect();
    let rose_theta_b: Vec<f64> = (0..rose_n).map(|i| (i as f64 + 0.4) * rose_step).collect();
    let rose_inner: Vec<f64> = (0..rose_n).map(|_| 0.1_f64).collect();
    let rose_outer: Vec<f64> = (0..rose_n)
        .map(|i| {
            let t = (i as f64 / rose_n as f64) * std::f64::consts::TAU;
            0.4 + 0.4 * (t * 2.0).cos().abs()
        })
        .collect();
    let bar_color = rgb8(180, 80, 100);

    // Gauge: a single needle drawn as a segment from centre to the
    // needle position (theta = 0.7 = "70 % of the way along the
    // half-disk arc"). Plus tick marks at 0, 0.25, 0.5, 0.75, 1.0.
    let needle_theta = vec![0.7_f64];
    let needle_inner = vec![0.0_f64];
    let needle_outer = vec![1.0_f64];
    let needle_color = rgb8(60, 30, 30);

    // Asymmetric partial arc (-60° to 135°): scattered dots distributed
    // evenly along the sweep + a single SegmentGeom at theta_frac=0.5
    // (the geometric middle of the partial sweep, which in math angle
    // is the bisector of [-60°, 135°] = 37.5°). The bbox-based
    // geometry should place the polar centre such that the swept
    // region fills the panel proportionally — neither cardinal
    // direction dominates.
    let partial_n = 40;
    let partial_theta: Vec<f64> = (0..partial_n)
        .map(|i| i as f64 / (partial_n - 1) as f64)
        .collect();
    let partial_radius: Vec<f64> = (0..partial_n)
        .map(|i| {
            let t = i as f64 / (partial_n - 1) as f64;
            0.3 + 0.6 * t // spiral outward along the sweep
        })
        .collect();
    let partial_dot = rgb8(120, 60, 160);

    let mut view = PlotComposition::new(comp_shape())
        // Scatter: theta in radians [0, 2π], radius in [0, 1].
        .add_scale("theta_full", scale::continuous(0.0..=std::f64::consts::TAU))
        .add_scale("radius_unit", scale::continuous(0.0..=1.0))
        // Rose: same theta/radius scales work.
        // Gauge: theta in [0, 1] (gauge position fraction); radius
        // doesn't need its own scale.
        .add_scale("gauge_theta", scale::continuous(0.0..=1.0))
        .add_scale("gauge_radius", scale::continuous(0.0..=1.0))
        // Partial: shared [0, 1] domain for both theta and radius.
        .add_scale("partial_theta", scale::continuous(0.0..=1.0))
        .add_scale("partial_radius", scale::continuous(0.0..=1.0));

    // ── Scatter plot ──
    let mut p_scatter = Plot::new(&comp_shape(), "scatter")
        .projection(Projection::polar())
        .bind("x", "theta_full")
        .bind("y", "radius_unit");
    p_scatter.add_geom(
        PointGeom::builder()
            .set("x", scatter_theta.clone())
            .set("y", scatter_radius.clone())
            .set("fill", dot)
            .set("size", 6.0_f64)
            .build(),
    );
    view.attach_plot(p_scatter);

    // ── Rose: annular-wedge bars via RectGeom (its 4 edges densify
    // along the projected arc under polar).
    let mut p_rose = Plot::new(&comp_shape(), "rose")
        .projection(Projection::polar())
        .bind("x", "theta_full")
        .bind("y", "radius_unit");
    p_rose.add_geom(
        RectGeom::builder()
            .set("x", rose_theta_a.clone())
            .set("x2", rose_theta_b.clone())
            .set("y", rose_inner.clone())
            .set("y2", rose_outer.clone())
            .set("fill", bar_color)
            .build(),
    );
    view.attach_plot(p_rose);

    // ── Gauge ──
    let mut p_gauge = Plot::new(&comp_shape(), "gauge")
        .projection(Projection::gauge())
        .bind("x", "gauge_theta")
        .bind("y", "gauge_radius");
    p_gauge.add_geom(
        SegmentGeom::builder()
            .set("x", needle_theta.clone())
            .set("x2", needle_theta)
            .set("y", needle_inner)
            .set("y2", needle_outer)
            .set("stroke", needle_color)
            .set("linewidth", 4.0_f64)
            .build(),
    );
    view.attach_plot(p_gauge);

    // ── Partial arc (-60° → 135°) ──
    let mut p_partial = Plot::new(&comp_shape(), "partial")
        .projection(partial_projection())
        .bind("x", "partial_theta")
        .bind("y", "partial_radius");
    p_partial.add_geom(
        PointGeom::builder()
            .set("x", partial_theta.clone())
            .set("y", partial_radius.clone())
            .set("fill", partial_dot)
            .set("size", 7.0_f64)
            .build(),
    );
    view.attach_plot(p_partial);

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
    let path = std::env::current_dir().unwrap().join("examples/polar.png");
    hephaestus::png::write_png(&path, w, h, &pixels).expect("write png");
    println!("wrote {}", path.display());
}
