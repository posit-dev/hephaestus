//! Tick labels that don't fit their rail are culled rather than
//! overplotted.
//!
//! Two panels whose x rails carry far more breaks than the pixels
//! allow:
//!
//! - **Long discrete categories.** Twelve bin labels, each wider than
//!   its slot — the survivors are an even subset, anchored on the
//!   first and last category.
//! - **Dense continuous breaks.** A break every 4 units — culling
//!   keeps the coarsest roughly-even subset that fits, always
//!   including the range endpoints 0 and 100.
//!
//! Tick marks draw for every break regardless; only labels drop.
//!
//! Produces `examples/axis_overlap.png`.
//!
//! ```sh
//! cargo run --example axis_overlap
//! ```

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{grid, Composition, Element, Patch};
use hephaestus::geometry::Size;
use hephaestus::image::PngCompression;
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement};
use hephaestus::plot::{scale, Plot, PlotComposition, PointGeom};
use hephaestus::scales::chrome::AxisSide;
use hephaestus::scales::value::Value;
use hephaestus::scene::SceneBuilder;
use hephaestus::Renderer;

fn comp_shape() -> Composition {
    let panels: Vec<Element> = ["categories", "dense"]
        .into_iter()
        .map(|id| Patch::new(id).into())
        .collect();
    grid(1, 2, panels)
}

fn main() {
    let (w, h) = (760u32, 360u32);
    let dpi = 96.0;

    let bins: Vec<String> = (0..12)
        .map(|i| format!("Bin ({lo}, {hi}]", lo = i * 5, hi = i * 5 + 5))
        .collect();
    let dense_breaks: Vec<Value> = (0..=25).map(|i| Value::Number(i as f64 * 4.0)).collect();

    let mut view = PlotComposition::new(&comp_shape())
        .add_scale(
            "bins",
            scale::discrete(bins.iter().cloned().map(Value::from)),
        )
        .add_scale(
            "dense_x",
            scale::continuous(0.0..=100.0).with_breaks(dense_breaks),
        )
        .add_scale("y", scale::continuous(0.0..=100.0));

    let mut p_cat = Plot::new(&comp_shape(), "categories")
        .bind("x", "bins")
        .bind("y", "y")
        .subtitle("Discrete, long labels");
    p_cat.add_geom(
        PointGeom::builder()
            .set("x", bins.clone())
            .set(
                "y",
                bins.iter()
                    .enumerate()
                    .map(|(i, _)| 20.0 + 60.0 * ((i as f64) / 11.0))
                    .collect::<Vec<f64>>(),
            )
            .set("fill", rgb8(70, 120, 220))
            .set("size", 5.0_f64)
            .build(),
    );
    p_cat.add_axis(Axis::rail(
        "bins",
        AxisPlacement::Cartesian(AxisSide::Bottom),
    ));
    p_cat.add_axis(Axis::rail("y", AxisPlacement::Cartesian(AxisSide::Left)));
    view.attach_plot(p_cat);

    let mut p_dense = Plot::new(&comp_shape(), "dense")
        .bind("x", "dense_x")
        .bind("y", "y")
        .subtitle("Continuous, a break every 4");
    p_dense.add_geom(
        PointGeom::builder()
            .set("x", (0..=20).map(|i| i as f64 * 5.0).collect::<Vec<f64>>())
            .set(
                "y",
                (0..=20)
                    .map(|i| 50.0 + 30.0 * (i as f64 * 0.5).sin())
                    .collect::<Vec<f64>>(),
            )
            .set("fill", rgb8(220, 90, 70))
            .set("size", 5.0_f64)
            .build(),
    );
    p_dense.add_axis(Axis::rail(
        "dense_x",
        AxisPlacement::Cartesian(AxisSide::Bottom),
    ));
    p_dense.add_axis(Axis::rail("y", AxisPlacement::Cartesian(AxisSide::Left)));
    view.attach_plot(p_dense);

    let mut renderer = VelloRenderer::new().expect("vello renderer init");
    let bg: Color = rgb8(250, 250, 252);
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
        .join("examples/axis_overlap.png");
    hephaestus::image::write_png(&path, w, h, &pixels, PngCompression::Balanced, Some(dpi))
        .expect("write png");
    println!("wrote {}", path.display());
}
