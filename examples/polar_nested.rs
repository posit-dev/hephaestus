//! Three polar plots sharing one panel:
//!
//! - **Inner**: full-circle scatter, sized to fill `0..0.45` of the
//!   inscribed disk's radius.
//! - **Outer top**: partial arc covering the top ~150°, sitting in
//!   the annular region `0.55..1.0`.
//! - **Outer bottom**: partial arc covering the bottom ~150°, also
//!   in `0.55..1.0`.
//!
//! Between the inner disk and the two outer arcs there's a 10 %
//! radial gap; between the two outer arcs themselves there are 30°
//! angular gaps on the left and right.
//!
//! All three plots disable bbox-based repositioning
//! (`fit_to_bbox = false`) so they share the panel's geometric
//! centre and the same maximum radius — the per-projection
//! `outer_radius_frac` / `inner_radius_frac` then partition the
//! disk between them.
//!
//! Produces `examples/polar_nested.png`.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{Composition, Patch, Span};
use hephaestus::geometry::Size;
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement, PolarRing};
use hephaestus::plot::projection::{PolarProjection, Projection};
use hephaestus::plot::{scale, Plot, PlotComposition, PointGeom, RectGeom};
use hephaestus::Renderer;

fn comp() -> Composition {
    Composition::empty(1, 1).place(1, 1, Span::cell(), Patch::new("panel"))
}

fn main() {
    let (w, h) = (700u32, 700u32);
    let dpi = 96.0;

    // Inner full-circle polar. Outer radius cap = 0.45 leaves a
    // radial gap to the outer ring.
    let inner_projection = Projection::Polar(PolarProjection {
        outer_radius_frac: Some(0.45),
        fit_to_bbox: false,
        ..PolarProjection::full_circle()
    });

    // Outer top arc: theta_start = 165° (left, near 9 o'clock) CW
    // to theta_end = 15° (right, near 3 o'clock). Sweep = -150°
    // (CW). Leaves 30° gaps on both left and right.
    let outer_top_projection = Projection::Polar(PolarProjection {
        theta_start: 165.0_f64.to_radians(),
        theta_end: 15.0_f64.to_radians(),
        inner_radius_frac: 0.55,
        fit_to_bbox: false,
        ..PolarProjection::full_circle()
    });

    // Outer bottom arc: mirror across the horizontal.
    // theta_start = -15° CW to theta_end = -165°. Sweep = -150°.
    let outer_bottom_projection = Projection::Polar(PolarProjection {
        theta_start: -15.0_f64.to_radians(),
        theta_end: -165.0_f64.to_radians(),
        inner_radius_frac: 0.55,
        fit_to_bbox: false,
        ..PolarProjection::full_circle()
    });

    // ── Data ──
    // Inner scatter: 60 points scattered around the full disk.
    let n_inner = 60;
    let inner_theta: Vec<f64> = (0..n_inner).map(|i| i as f64 / n_inner as f64).collect();
    let inner_radius: Vec<f64> = (0..n_inner)
        .map(|i| 0.35 + 0.6 * ((i as f64 * 0.137).sin() * 0.5 + 0.5))
        .collect();
    let inner_color = rgb8(60, 120, 200);

    // Outer top: 12 evenly-spaced bars across the 150° arc.
    let top_n = 12;
    let top_x_a: Vec<f64> = (0..top_n)
        .map(|i| (i as f64 - 0.4) / top_n as f64)
        .collect();
    let top_x_b: Vec<f64> = (0..top_n)
        .map(|i| (i as f64 + 0.4) / top_n as f64)
        .collect();
    let top_inner: Vec<f64> = (0..top_n).map(|_| 0.0_f64).collect();
    let top_outer: Vec<f64> = (0..top_n)
        .map(|i| 0.35 + 0.6 * ((i as f64 * 0.5).sin() * 0.5 + 0.5))
        .collect();
    let top_color = rgb8(180, 80, 100);

    // Outer bottom: scatter rather than bars to show the API
    // variety. 30 points along the arc, varying radius.
    let bot_n = 30;
    let bot_theta: Vec<f64> = (0..bot_n).map(|i| i as f64 / (bot_n - 1) as f64).collect();
    let bot_radius: Vec<f64> = (0..bot_n)
        .map(|i| 0.2 + 0.7 * ((i as f64 * 0.21).cos() * 0.5 + 0.5))
        .collect();
    let bot_color = rgb8(80, 170, 80);

    let mut view = PlotComposition::new(comp())
        .add_scale("theta_unit", scale::continuous(0.0..=1.0))
        .add_scale("radius_unit", scale::continuous(0.0..=1.0));

    // ── Inner plot ──
    let mut p_inner = Plot::new(&comp(), "panel")
        .projection(inner_projection)
        .bind("x", "theta_unit")
        .bind("y", "radius_unit");
    p_inner.add_geom(
        PointGeom::builder()
            .set("x", inner_theta)
            .set("y", inner_radius)
            .set("fill", inner_color)
            .set("size", 5.0_f64)
            .build(),
    );
    p_inner.add_axis(Axis::rail(
        "radius_unit",
        AxisPlacement::PolarRadius { theta_frac: 0.0 },
    ));
    view.attach_plot(p_inner);

    // ── Outer top arc ──
    let mut p_top = Plot::new(&comp(), "panel")
        .projection(outer_top_projection)
        .bind("x", "theta_unit")
        .bind("x2", "theta_unit")
        .bind("y", "radius_unit")
        .bind("y2", "radius_unit");
    p_top.add_geom(
        RectGeom::builder()
            .set("x", top_x_a)
            .set("x2", top_x_b)
            .set("y", top_inner)
            .set("y2", top_outer)
            .set("fill", top_color)
            .build(),
    );
    p_top.add_axis(Axis::rail(
        "theta_unit",
        AxisPlacement::PolarAngular(PolarRing::Outer),
    ));
    view.attach_plot(p_top);

    // ── Outer bottom arc ──
    let mut p_bot = Plot::new(&comp(), "panel")
        .projection(outer_bottom_projection)
        .bind("x", "theta_unit")
        .bind("y", "radius_unit");
    p_bot.add_geom(
        PointGeom::builder()
            .set("x", bot_theta)
            .set("y", bot_radius)
            .set("fill", bot_color)
            .set("size", 6.0_f64)
            .build(),
    );
    p_bot.add_axis(Axis::rail(
        "theta_unit",
        AxisPlacement::PolarAngular(PolarRing::Outer),
    ));
    view.attach_plot(p_bot);

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
    let path = std::env::current_dir()
        .unwrap()
        .join("examples/polar_nested.png");
    hephaestus::png::write_png(&path, w, h, &pixels).expect("write png");
    println!("wrote {}", path.display());
}
