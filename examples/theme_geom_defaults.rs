//! Demonstrates `theme.geom.point.size_pt` — overriding a geom
//! default through the theme. Same data drawn twice: left panel uses
//! the default 5pt size, right panel sets `theme.geom.point.size_pt =
//! 2.0` so every unbound `"size"` channel reads the smaller value at
//! draw time.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{Composition, Patch, Span};
use hephaestus::geometry::Size;
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement};
use hephaestus::plot::theme::{PointDefaults, Theme};
use hephaestus::plot::{scale, Plot, PlotComposition, PointGeom};
use hephaestus::scales::chrome::AxisSide;
use hephaestus::Renderer;

fn main() {
    let (w, h) = (1200u32, 500u32);
    let dpi = 96.0;

    let comp = || {
        Composition::empty(1, 2)
            .place(1, 1, Span::cell(), Patch::new("default"))
            .place(1, 2, Span::cell(), Patch::new("small"))
    };

    let n = 60;
    let xs: Vec<f64> = (0..n).map(|i| i as f64 * 0.1).collect();
    let ys: Vec<f64> = (0..n)
        .map(|i| ((i as f64) * 0.4).sin() * 0.4 + 0.5)
        .collect();

    let mut default_plot = Plot::new(&comp(), "default")
        .bind("x", "x_scale")
        .bind("y", "y_scale")
        .title("Default size (5pt)");
    default_plot.add_geom(
        PointGeom::builder()
            .set("x", xs.clone())
            .set("y", ys.clone())
            .set("fill", rgb8(120, 170, 250))
            .build(),
    );
    default_plot.add_axis(Axis::rail(
        "x_scale",
        AxisPlacement::Cartesian(AxisSide::Bottom),
    ));
    default_plot.add_axis(Axis::rail(
        "y_scale",
        AxisPlacement::Cartesian(AxisSide::Left),
    ));

    let mut small_plot = Plot::new(&comp(), "small")
        .bind("x", "x_scale")
        .bind("y", "y_scale")
        .title("theme.geom.point.size_pt = 2.0");
    small_plot.add_geom(
        PointGeom::builder()
            .set("x", xs)
            .set("y", ys)
            .set("fill", rgb8(220, 100, 80))
            .build(),
    );
    small_plot.add_axis(Axis::rail(
        "x_scale",
        AxisPlacement::Cartesian(AxisSide::Bottom),
    ));
    small_plot.add_axis(Axis::rail(
        "y_scale",
        AxisPlacement::Cartesian(AxisSide::Left),
    ));

    // Composition theme: default everywhere. The small_plot overrides
    // `theme.geom.point.size_pt` via a per-plot ThemePart.
    let mut shrunk_geom = Theme::default().geom.clone();
    shrunk_geom.point = PointDefaults {
        size_pt: 2.0,
        ..shrunk_geom.point
    };
    let small_override = hephaestus::plot::theme::ThemePart {
        geom: Some(shrunk_geom),
        ..hephaestus::plot::theme::ThemePart::default()
    };
    small_plot.set_theme_override(Some(small_override));

    let mut view = PlotComposition::new(comp())
        .add_scale("x_scale", scale::continuous(0.0..=6.0))
        .add_scale("y_scale", scale::continuous(0.0..=1.0));
    view.attach_plot(default_plot);
    view.attach_plot(small_plot);

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
        .join("examples/theme_geom_defaults.png");
    hephaestus::png::write_png(&path, w, h, &pixels).expect("write png");
    println!("wrote {}", path.display());
}
