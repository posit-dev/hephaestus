//! Demonstrates locale-aware tick labels.
//!
//! Side-by-side renders of the same plot with `Locale::EN_US`
//! (default — decimal point) and `Locale::DE_DE` (decimal comma).
//! The tick labels are the only visual difference; the layout
//! engine and chrome rendering are identical.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{beside, Patch};
use hephaestus::geometry::Size;
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement};
use hephaestus::plot::theme::{Locale, Theme};
use hephaestus::plot::{scale, Plot, PlotComposition, PointGeom};
use hephaestus::scales::chrome::AxisSide;
use hephaestus::Renderer;

fn main() {
    let (w, h) = (1200u32, 500u32);
    let dpi = 96.0;

    // Fractional ticks make the decimal-mark swap visible.
    let xs: Vec<f64> = (0..30).map(|i| 0.1 * i as f64).collect();
    let ys: Vec<f64> = xs.iter().map(|x| (x * 1.7).sin() * 0.45 + 0.5).collect();

    let template = || beside(Patch::new("us"), Patch::new("de"));

    let theme = Theme::default().with_locale(Locale::DE_DE);
    let mut view = PlotComposition::new(template())
        .add_scale("x", scale::continuous(0.0..=3.0))
        .add_scale("y", scale::continuous(0.0..=1.0))
        .theme(theme);

    // Plot 1 — overrides locale back to en_US to demonstrate
    // per-plot override.
    let mut us = Plot::new(&template(), "us")
        .bind("x", "x")
        .bind("y", "y")
        .title("Locale: en_US (decimal '.')")
        .theme_override(hephaestus::plot::theme::ThemePart {
            locale: Some(Locale::EN_US),
            ..Default::default()
        });
    us.add_geom(
        PointGeom::builder()
            .set("x", xs.clone())
            .set("y", ys.clone())
            .set("fill", rgb8(70, 120, 220))
            .set("size", 4.0_f64)
            .build(),
    );
    us.add_axis(Axis::rail("x", AxisPlacement::Cartesian(AxisSide::Bottom)));
    us.add_axis(Axis::rail("y", AxisPlacement::Cartesian(AxisSide::Left)));

    // Plot 2 — inherits the composition's DE_DE locale.
    let mut de = Plot::new(&template(), "de")
        .bind("x", "x")
        .bind("y", "y")
        .title("Locale: de_DE (Dezimalkomma)");
    de.add_geom(
        PointGeom::builder()
            .set("x", xs)
            .set("y", ys)
            .set("fill", rgb8(220, 90, 70))
            .set("size", 4.0_f64)
            .build(),
    );
    de.add_axis(Axis::rail("x", AxisPlacement::Cartesian(AxisSide::Bottom)));
    de.add_axis(Axis::rail("y", AxisPlacement::Cartesian(AxisSide::Left)));

    view.attach_plot(us);
    view.attach_plot(de);

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
        .join("examples/locale_de.png");
    hephaestus::png::write_png(&path, w, h, &pixels).expect("write png");
    println!("wrote {}", path.display());
}
