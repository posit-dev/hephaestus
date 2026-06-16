//! Demonstrates `Theme::default().with_palette(...)` — a custom
//! brand palette propagates through every palette-referencing
//! element automatically.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb, rgb8, Color};
use hephaestus::composition::{Composition, Patch, Span};
use hephaestus::geometry::Size;
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement};
use hephaestus::plot::theme::{Palette, Theme};
use hephaestus::plot::{scale, Plot, PlotComposition, PointGeom};
use hephaestus::scales::chrome::AxisSide;
use hephaestus::Renderer;

fn main() {
    let (w, h) = (900u32, 600u32);
    let dpi = 96.0;

    let comp = || Composition::empty(1, 1).place(1, 1, Span::cell(), Patch::new("p"));

    let n = 60;
    let xs: Vec<f64> = (0..n).map(|i| i as f64 * 0.1).collect();
    let ys: Vec<f64> = (0..n)
        .map(|i| ((i as f64) * 0.4).cos() * 0.4 + 0.5)
        .collect();

    let mut plot = Plot::new(&comp(), "p")
        .bind("x", "x_scale")
        .bind("y", "y_scale")
        .title("Brand palette: warm paper, deep-blue ink");
    plot.add_geom(
        PointGeom::builder()
            .set("x", xs)
            .set("y", ys)
            .set("fill", rgb8(220, 100, 80))
            .set("size", 8.0_f64)
            .build(),
    );
    plot.add_axis(Axis::rail(
        "x_scale",
        AxisPlacement::Cartesian(AxisSide::Bottom),
    ));
    plot.add_axis(Axis::rail(
        "y_scale",
        AxisPlacement::Cartesian(AxisSide::Left),
    ));

    // Warm cream paper, deep navy ink — every grid line, axis
    // baseline, tick mark, label, and panel border re-derives from
    // these anchors.
    let brand = Palette {
        paper: rgb(0.98, 0.95, 0.88),
        ink: rgb(0.10, 0.18, 0.35),
        accent: rgb(0.86, 0.40, 0.31),
    };
    let mut view = PlotComposition::new(comp())
        .add_scale("x_scale", scale::continuous(0.0..=6.0))
        .add_scale("y_scale", scale::continuous(0.0..=1.0))
        .theme(Theme::default().with_palette(brand));
    view.attach_plot(plot);

    let mut renderer = VelloRenderer::new().expect("vello renderer init");
    let bg: Color = rgb8(252, 252, 252);
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
        .join("examples/theme_brand.png");
    hephaestus::png::write_png(&path, w, h, &pixels).expect("write png");
    println!("wrote {}", path.display());
}
