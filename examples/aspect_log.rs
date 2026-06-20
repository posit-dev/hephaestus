//! Aspect ratio with transformed (log10) position scales.
//!
//! Both axes use a `log10` transform and the plot asks for
//! `aspect_ratio(1.0)`. For a transformed scale, aspect is measured in
//! *transformed* units, so `1.0` means **one decade on x occupies the
//! same screen distance as one decade on y** — regardless of where each
//! axis's raw range sits.
//!
//! The data traces the power law `y = x / 1000`. In log-log space that's
//! a straight line of slope 1, so it renders at exactly 45° *only* when
//! decades are square. x spans `[10, 10000]` (3 decades) and y spans
//! `[0.01, 10]` (3 decades) — wildly different raw ranges, identical
//! decade counts — and the line still comes out at 45° in both modes.
//!
//! ```text
//!   ┌─────────────────────────┬─────────────────────────┐
//!   │ log-log, AspectMode     │ log-log, AspectMode     │
//!   │   ::Panel               │   ::Range               │
//!   │ panel locked square,    │ panel fills cell, x     │
//!   │ 45° slope-1 line        │ range expanded, still   │
//!   │                         │ 45° per-decade          │
//!   └─────────────────────────┴─────────────────────────┘
//! ```
//!
//! Writes `examples/aspect_log.png`.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{grid, Composition, Patch};
use hephaestus::geometry::Size;
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement};
use hephaestus::plot::{scale, AspectMode, LineGeom, Plot, PlotComposition, PointGeom};
use hephaestus::scales::chrome::AxisSide;
use hephaestus::scales::TransformKind;
use hephaestus::Renderer;

fn comp_shape() -> Composition {
    grid(
        1,
        2,
        vec![
            Patch::new("log_panel").into(),
            Patch::new("log_range").into(),
        ],
    )
}

fn main() {
    // Markedly wide cells (a 1×2 grid in a wide viewport) so the two
    // modes read differently: Panel locks the panel to a square with side
    // slack, Range fills the cell — both keep decades square.
    let (w, h) = (2000u32, 720u32);
    let dpi = 96.0;

    // Logarithmically spaced x over [10, 10000]; y = x / 1000 traces a
    // slope-1 power law that fills y's [0.01, 10] range exactly.
    let xs: Vec<f64> = (0..=60)
        .map(|i| 10f64.powf(1.0 + 3.0 * (i as f64) / 60.0))
        .collect();
    let ys: Vec<f64> = xs.iter().map(|x| x / 1000.0).collect();
    let line = rgb8(40, 90, 200);
    let dot = rgb8(200, 60, 40);

    let mut view = PlotComposition::new(comp_shape())
        .add_scale(
            "x",
            scale::continuous(10.0..=10000.0).with_transform(TransformKind::Log10),
        )
        .add_scale(
            "y",
            scale::continuous(0.01..=10.0).with_transform(TransformKind::Log10),
        );

    // ── Panel mode (the default).
    // Equal transformed extents (3 decades each) plus `aspect_ratio(1.0)`
    // lock the panel to a square; the slope-1 line lands at 45°.
    {
        let mut p = Plot::new(&comp_shape(), "log_panel")
            .title("log-log · AspectMode::Panel")
            .subtitle("panel locked square, one decade square on both axes")
            .bind("x", "x")
            .bind("y", "y")
            .aspect_ratio(1.0);
        p.add_geom(
            LineGeom::builder()
                .set("x", xs.clone())
                .set("y", ys.clone())
                .set("stroke", line)
                .set("linewidth", 1.5_f64)
                .build(),
        );
        p.add_geom(
            PointGeom::builder()
                .set("x", xs.clone())
                .set("y", ys.clone())
                .set("fill", dot)
                .set("size", 4.0_f64)
                .build(),
        );
        p.add_axis(Axis::rail("x", AxisPlacement::Cartesian(AxisSide::Bottom)));
        p.add_axis(Axis::rail("y", AxisPlacement::Cartesian(AxisSide::Left)));
        view.attach_plot(p);
    }

    // ── Range mode.
    // Same `aspect_ratio(1.0)`, but the panel fills the wide cell. The x
    // scale's range is expanded *in log space* to keep decades square, so
    // the slope-1 line stays at 45°; the bottom axis shows the widened
    // decade range.
    {
        let mut p = Plot::new(&comp_shape(), "log_range")
            .title("log-log · AspectMode::Range")
            .subtitle("panel fills cell, x range expanded in log space")
            .bind("x", "x")
            .bind("y", "y")
            .aspect_ratio(1.0)
            .aspect_mode(AspectMode::Range);
        p.add_geom(
            LineGeom::builder()
                .set("x", xs.clone())
                .set("y", ys.clone())
                .set("stroke", line)
                .set("linewidth", 1.5_f64)
                .build(),
        );
        p.add_geom(
            PointGeom::builder()
                .set("x", xs.clone())
                .set("y", ys.clone())
                .set("fill", dot)
                .set("size", 4.0_f64)
                .build(),
        );
        p.add_axis(Axis::rail("x", AxisPlacement::Cartesian(AxisSide::Bottom)));
        p.add_axis(Axis::rail("y", AxisPlacement::Cartesian(AxisSide::Left)));
        view.attach_plot(p);
    }

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
        .join("examples/aspect_log.png");
    hephaestus::png::write_png(&path, w, h, &pixels).expect("write png");
    println!("wrote {}", path.display());
}
