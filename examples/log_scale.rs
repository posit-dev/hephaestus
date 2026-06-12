//! Side-by-side comparison of linear / log10 / sqrt x scales on the
//! same data. Shows how each transform redistributes density: a cluster
//! of low-x points spreads out under log; high-x values compress under
//! sqrt.
//!
//! Also exercises Phase E.1's transform-aware tick selection (decade
//! powers + 1-2-5 sub-decade stops for log) and minor-tick rendering
//! (geometric 2..9 between decade powers for log; midpoints between
//! majors for linear / sqrt).
//!
//! Produces `examples/log_scale.png`.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{beside, Composition, Patch};
use hephaestus::geometry::Size;
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement};
use hephaestus::plot::scale::TransformKind;
use hephaestus::plot::{scale, Plot, PlotComposition, PointGeom};
use hephaestus::scales::chrome::AxisSide;
use hephaestus::Renderer;

fn comp_shape() -> Composition {
    beside(
        beside(Patch::new("linear"), Patch::new("log10")),
        Patch::new("sqrt"),
    )
}

fn main() {
    let (w, h) = (1500u32, 500u32);
    let dpi = 96.0;

    // Logarithmically-distributed x values spanning three decades.
    // Linear render: most points crowd into the right edge.
    // Log render: points distribute evenly across the panel.
    // Sqrt render: somewhere in between.
    let xs: Vec<f64> = vec![
        1.0, 2.0, 3.0, 5.0, 7.0, 10.0, 15.0, 20.0, 30.0, 50.0, 70.0, 100.0, 150.0, 200.0, 300.0,
        500.0, 700.0, 1000.0,
    ];
    let ys: Vec<f64> = xs.iter().map(|x| x.ln() * 10.0).collect();
    let dot = rgb8(40, 80, 200);

    let mut view = PlotComposition::new(comp_shape())
        .add_scale("x_linear", scale::continuous(1.0..=1000.0))
        .add_scale(
            "x_log",
            scale::continuous(1.0..=1000.0).with_transform(TransformKind::Log10),
        )
        .add_scale(
            "x_sqrt",
            scale::continuous(1.0..=1000.0).with_transform(TransformKind::Sqrt),
        )
        .add_scale("y", scale::continuous(0.0..=80.0));

    for (id, x_scale_name, title) in [
        ("linear", "x_linear", "Linear"),
        ("log10", "x_log", "Log10 — 1-2-5 pattern, 2..9 minors"),
        ("sqrt", "x_sqrt", "Sqrt — squared back to data space"),
    ] {
        let mut p = Plot::new(&comp_shape(), id)
            .title(title)
            .bind("x", x_scale_name)
            .bind("y", "y");
        p.add_geom(
            PointGeom::builder()
                .set("x", xs.clone())
                .set("y", ys.clone())
                .set("fill", dot)
                .set("size", 6.0_f64)
                .build(),
        );
        p.add_axis(Axis::rail(
            x_scale_name,
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
        .join("examples/log_scale.png");
    hephaestus::png::write_png(&path, w, h, &pixels).expect("write png");
    println!("wrote {}", path.display());
}
