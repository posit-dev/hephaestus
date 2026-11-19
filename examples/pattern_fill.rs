//! Vector pattern fills: bars filled with the named built-in patterns —
//! stripe, crosshatch, and dot — through the same fill channel that takes
//! solid, gradient, and image brushes.
//!
//! Exported as SVG and PNG from one composition, so the vector backend's
//! native `<pattern>` can be compared against the rasteriser's clipped
//! replay side by side.
//!
//! ```sh
//! cargo run --example pattern_fill --features svg
//! ```

use hephaestus::backend::svg::{write_svg, SvgConfig, SvgScene};
use hephaestus::backend::vello::VelloRenderer;
use hephaestus::brush::{fill_pattern, Brush};
use hephaestus::color::rgb8;
use hephaestus::composition::{beside, Patch};
use hephaestus::geometry::Size;
use hephaestus::image::PngCompression;
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement};
use hephaestus::plot::{Plot, PlotComposition, Raw, RectGeom};
use hephaestus::scales::chrome::AxisSide;
use hephaestus::scene::SceneBuilder;
use hephaestus::Renderer;

fn main() {
    let (w, h) = (1200u32, 400u32);
    let dpi = 96.0;
    let size = Size::new(w as f64, h as f64);

    let comp = || {
        beside(
            beside(Patch::new("stripe-dense"), Patch::new("crosshatch")),
            Patch::new("circle"),
        )
    };

    let xs: Vec<f64> = (0..8).map(|i| i as f64).collect();
    let ys: Vec<f64> = xs
        .iter()
        .map(|x| 2.0 + 1.5 * (x * 0.9).sin().abs())
        .collect();

    // One panel per named pattern. Built-ins are authored in a unit
    // tile; scaling the tiling transform to 24 pt scales the tile, the
    // pitch, and the line weights with it.
    let panels = [
        ("stripe-dense", "Stripe-dense fill", rgb8(60, 100, 200)),
        ("crosshatch", "Crosshatch fill", rgb8(200, 90, 60)),
        ("circle", "Circle fill", rgb8(70, 150, 90)),
    ];
    let comp = comp();
    let mut plots = Vec::new();
    for (name, title, color) in panels {
        let mut plot = Plot::new(&comp, name)
            .bind("x", "cat")
            .bind("x2", "cat")
            .bind("y", "value")
            .bind("y2", "value")
            .subtitle(title);
        let mut pattern = fill_pattern(name, Brush::Solid(color)).expect("named pattern");
        pattern.transform *= hephaestus::geometry::Affine::scale(24.0);
        plot.add_geom(
            RectGeom::builder()
                .set("x", xs.clone())
                .set("x2", xs.iter().map(|x| x + 0.8).collect::<Vec<_>>())
                .set("y", vec![0.0_f64; 8])
                .set("y2", ys.clone())
                .set("fill", Raw(Brush::Pattern(std::sync::Arc::new(pattern))))
                .build(),
        );
        for a in [
            Axis::rail("cat", AxisPlacement::Cartesian(AxisSide::Bottom)),
            Axis::rail("value", AxisPlacement::Cartesian(AxisSide::Left)),
        ] {
            plot.add_axis(a);
        }
        plots.push(plot);
    }

    let mut view = PlotComposition::new(&comp)
        .add_scale("cat", hephaestus::plot::scale::continuous(-0.5..=8.0))
        .add_scale("value", hephaestus::plot::scale::continuous(0.0..=4.0));
    for plot in plots {
        view = view.with_plot(plot);
    }

    // Vector.
    let mut svg = SvgScene::with_config(size, dpi, SvgConfig::default());
    view.render(&mut svg, size, dpi);
    write_svg("examples/pattern_fill.svg", &svg).expect("write svg");
    if !svg.warnings().is_empty() {
        eprintln!("svg warnings: {:?}", svg.warnings());
    }

    // Raster, from the same composition, for side-by-side review.
    let mut renderer = VelloRenderer::new().expect("vello renderer init");
    {
        let scene = renderer.scene();
        scene.clear();
        view.render(scene, size, dpi);
    }
    let mut pixels = vec![0u8; (w * h * 4) as usize];
    renderer
        .render_to_buffer(w, h, hephaestus::color::Color::WHITE, &mut pixels)
        .expect("render");
    hephaestus::image::write_png(
        "examples/pattern_fill.png",
        w,
        h,
        &pixels,
        PngCompression::Balanced,
        Some(dpi),
    )
    .expect("write png");
    println!("wrote examples/pattern_fill.png and examples/pattern_fill.svg");
}
