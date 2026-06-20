//! Range-mode aspect expansion at a transform's domain boundary.
//!
//! Both panels share the same linear y scale, the same `aspect_ratio(1.0)`,
//! and `AspectMode::Range` in a wide cell — so each needs its x range
//! widened to keep the data-unit aspect honest. The only difference is the
//! x transform:
//!
//! - **Linear x** has no domain boundary, so the expansion is symmetric
//!   around the natural `[0, 10]` — the bottom axis dips **negative**.
//! - **Sqrt x** is only defined on `[0, ∞)`. A symmetric expansion in
//!   transformed space would push the low end below 0, so it **clamps at
//!   0** and redistributes the slack onto the high end. The bottom axis
//!   starts at exactly 0 and stretches further up instead.
//!
//! ```text
//!   ┌─────────────────────────┬─────────────────────────┐
//!   │ linear x · Range        │ sqrt x · Range          │
//!   │ symmetric expand →      │ clamps at 0 → all slack │
//!   │ axis goes negative      │ on the high end         │
//!   └─────────────────────────┴─────────────────────────┘
//! ```
//!
//! Writes `examples/aspect_sqrt.png`.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{grid, Composition, Patch};
use hephaestus::geometry::Size;
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement};
use hephaestus::plot::{scale, AspectMode, Plot, PlotComposition, PointGeom};
use hephaestus::scales::chrome::AxisSide;
use hephaestus::scales::TransformKind;
use hephaestus::Renderer;

fn comp_shape() -> Composition {
    grid(
        1,
        2,
        vec![Patch::new("lin").into(), Patch::new("sqrt").into()],
    )
}

fn main() {
    // Wide cells so Range mode has to widen each x range substantially —
    // the boundary clamp on the sqrt axis then reads clearly.
    let (w, h) = (2200u32, 600u32);
    let dpi = 96.0;

    let dot = rgb8(40, 90, 200);

    // Shared linear y; the x scales differ only in their transform. Both
    // start with the same transformed extent so they expand by the same
    // factor — only the boundary handling diverges.
    let mut view = PlotComposition::new(comp_shape())
        .add_scale("y", scale::continuous(0.0..=10.0))
        .add_scale("x_lin", scale::continuous(0.0..=10.0))
        .add_scale(
            "x_sqrt",
            scale::continuous(0.0..=100.0).with_transform(TransformKind::Sqrt),
        );

    // ── Linear x: no domain floor, symmetric expansion goes negative.
    {
        let lin_x: Vec<f64> = (0..=10).map(|i| i as f64).collect();
        let lin_y: Vec<f64> = lin_x.clone();
        let mut p = Plot::new(&comp_shape(), "lin")
            .title("linear x · AspectMode::Range")
            .subtitle("symmetric expansion — axis dips negative")
            .bind("x", "x_lin")
            .bind("y", "y")
            .aspect_ratio(1.0)
            .aspect_mode(AspectMode::Range);
        p.add_geom(
            PointGeom::builder()
                .set("x", lin_x)
                .set("y", lin_y)
                .set("fill", dot)
                .set("size", 5.0_f64)
                .build(),
        );
        p.add_axis(Axis::rail(
            "x_lin",
            AxisPlacement::Cartesian(AxisSide::Bottom),
        ));
        p.add_axis(Axis::rail("y", AxisPlacement::Cartesian(AxisSide::Left)));
        view.attach_plot(p);
    }

    // ── Sqrt x: domain is [0, ∞), so the low end clamps at 0 and the
    // slack lands on the high end instead.
    {
        let sqrt_x: Vec<f64> = (0..=10).map(|i| (i as f64) * 10.0).collect();
        let sqrt_y: Vec<f64> = (0..=10).map(|i| i as f64).collect();
        let mut p = Plot::new(&comp_shape(), "sqrt")
            .title("sqrt x · AspectMode::Range")
            .subtitle("expansion clamps at 0 — slack moves to the high end")
            .bind("x", "x_sqrt")
            .bind("y", "y")
            .aspect_ratio(1.0)
            .aspect_mode(AspectMode::Range);
        p.add_geom(
            PointGeom::builder()
                .set("x", sqrt_x)
                .set("y", sqrt_y)
                .set("fill", dot)
                .set("size", 5.0_f64)
                .build(),
        );
        p.add_axis(Axis::rail(
            "x_sqrt",
            AxisPlacement::Cartesian(AxisSide::Bottom),
        ));
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
        .join("examples/aspect_sqrt.png");
    hephaestus::png::write_png(&path, w, h, &pixels).expect("write png");
    println!("wrote {}", path.display());
}
