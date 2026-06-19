//! Side-by-side demo of [`AspectMode::Panel`] vs [`AspectMode::Range`]
//! for both Cartesian and Polar projections. Same data, same
//! `aspect_ratio(1.0)` (Cartesian) / same default polar bbox (Polar) —
//! only the enforcement mode differs.
//!
//! Layout: 2×2 grid in a wide viewport so each cell is wider than tall.
//!
//! ```text
//!   ┌─────────────────────────┬─────────────────────────┐
//!   │ Cartesian, AspectMode   │ Cartesian, AspectMode   │
//!   │   ::Panel               │   ::Range               │
//!   │ panel locked 1:1, side  │ panel fills cell, x     │
//!   │ slack on the cell       │ scale range expanded    │
//!   ├─────────────────────────┼─────────────────────────┤
//!   │ Polar, AspectMode       │ Polar, AspectMode       │
//!   │   ::Panel               │   ::Range               │
//!   │ panel locked 1:1, side  │ panel fills cell, disk  │
//!   │ slack on the cell       │ centred with slack      │
//!   └─────────────────────────┴─────────────────────────┘
//! ```
//!
//! Writes `examples/aspect_mode.png`.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{grid, Composition, Patch};
use hephaestus::geometry::Size;
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement, PolarRing};
use hephaestus::plot::projection::Projection;
use hephaestus::plot::{scale, AspectMode, Plot, PlotComposition, PointGeom};
use hephaestus::scales::chrome::AxisSide;
use hephaestus::Renderer;

fn comp_shape() -> Composition {
    grid(
        2,
        2,
        vec![
            Patch::new("cart_panel").into(),
            Patch::new("cart_range").into(),
            Patch::new("polar_panel").into(),
            Patch::new("polar_range").into(),
        ],
    )
}

fn main() {
    // Wider than tall so each 2×1 cell is markedly non-square — Panel
    // mode will obviously waste space on the sides, Range mode obviously
    // fills it.
    let (w, h) = (1600u32, 800u32);
    let dpi = 96.0;

    // Sample data in the natural [0, 10] × [0, 10] box. Cartesian plots
    // bind directly; polar plots reuse the same columns as
    // (theta, radius) in [0, 10].
    let xs: Vec<f64> = (0..40).map(|i| (i as f64) * 0.25).collect();
    let ys: Vec<f64> = xs.iter().map(|x| 5.0 + 3.5 * (x * 0.9).sin()).collect();
    let dot = rgb8(40, 90, 200);

    let mut view = PlotComposition::new(comp_shape())
        .add_scale("x", scale::continuous(0.0..=10.0))
        .add_scale("y", scale::continuous(0.0..=10.0));

    // ── Cartesian, Panel mode (the default).
    // `aspect_ratio(1.0)` shrinks the panel to a square; the wide cell
    // ends up with empty margins on the left and right.
    {
        let mut p = Plot::new(&comp_shape(), "cart_panel")
            .title("Cartesian · AspectMode::Panel")
            .subtitle("panel locked 1:1, side slack on the wide cell")
            .bind("x", "x")
            .bind("y", "y")
            .aspect_ratio(1.0);
        p.add_geom(
            PointGeom::builder()
                .set("x", xs.clone())
                .set("y", ys.clone())
                .set("fill", dot)
                .set("size", 5.0_f64)
                .build(),
        );
        p.add_axis(Axis::rail("x", AxisPlacement::Cartesian(AxisSide::Bottom)));
        p.add_axis(Axis::rail("y", AxisPlacement::Cartesian(AxisSide::Left)));
        view.attach_plot(p);
    }

    // ── Cartesian, Range mode.
    // Same `aspect_ratio(1.0)` but the panel fills the wide cell.
    // To honor 1:1 the x scale range is symmetrically expanded around
    // the natural [0, 10] — the bottom axis labels show the widened
    // domain. The data still occupies the central column.
    {
        let mut p = Plot::new(&comp_shape(), "cart_range")
            .title("Cartesian · AspectMode::Range")
            .subtitle("panel fills cell, x scale range expanded")
            .bind("x", "x")
            .bind("y", "y")
            .aspect_ratio(1.0)
            .aspect_mode(AspectMode::Range);
        p.add_geom(
            PointGeom::builder()
                .set("x", xs.clone())
                .set("y", ys.clone())
                .set("fill", dot)
                .set("size", 5.0_f64)
                .build(),
        );
        p.add_axis(Axis::rail("x", AxisPlacement::Cartesian(AxisSide::Bottom)));
        p.add_axis(Axis::rail("y", AxisPlacement::Cartesian(AxisSide::Left)));
        view.attach_plot(p);
    }

    // ── Polar, Panel mode (the default).
    // Full-circle polar reports a 1:1 bbox aspect to the layout, so the
    // panel locks square inside the wide cell — same side-slack as the
    // Cartesian Panel-mode case above.
    {
        let mut p = Plot::new(&comp_shape(), "polar_panel")
            .title("Polar · AspectMode::Panel")
            .subtitle("panel locked 1:1 to bbox aspect")
            .projection(Projection::polar())
            .bind("x", "x")
            .bind("y", "y");
        p.add_geom(
            PointGeom::builder()
                .set("x", xs.clone())
                .set("y", ys.clone())
                .set("fill", dot)
                .set("size", 5.0_f64)
                .build(),
        );
        p.add_axis(Axis::rail(
            "x",
            AxisPlacement::PolarAngular(PolarRing::Outer),
        ));
        p.add_axis(Axis::rail(
            "y",
            AxisPlacement::PolarRadius { theta_frac: 0.0 },
        ));
        view.attach_plot(p);
    }

    // ── Polar, Range mode.
    // Same polar projection, but the patch isn't aspect-locked. The
    // panel fills the wide cell; the polar disk centres inside it with
    // horizontal slack on either side. Useful when you want polar plots
    // to share a row with non-polar siblings without forcing every
    // sibling into a square.
    {
        let mut p = Plot::new(&comp_shape(), "polar_range")
            .title("Polar · AspectMode::Range")
            .subtitle("panel fills cell, disk centred with slack")
            .projection(Projection::polar())
            .aspect_mode(AspectMode::Range)
            .bind("x", "x")
            .bind("y", "y");
        p.add_geom(
            PointGeom::builder()
                .set("x", xs.clone())
                .set("y", ys.clone())
                .set("fill", dot)
                .set("size", 5.0_f64)
                .build(),
        );
        p.add_axis(Axis::rail(
            "x",
            AxisPlacement::PolarAngular(PolarRing::Outer),
        ));
        p.add_axis(Axis::rail(
            "y",
            AxisPlacement::PolarRadius { theta_frac: 0.0 },
        ));
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
        .join("examples/aspect_mode.png");
    hephaestus::png::write_png(&path, w, h, &pixels).expect("write png");
    println!("wrote {}", path.display());
}
