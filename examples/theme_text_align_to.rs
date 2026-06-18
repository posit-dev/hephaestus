//! Compares `theme.plot_text_align_to`. Renders two side-by-side
//! variants of the same plot — one with `AlignTo::Plot` (title spans
//! the full plot interior, centered relative to legend + axes +
//! panel), one with `AlignTo::Panel` (title spans only the panel
//! column, centered above the data area regardless of side chrome).
//!
//! Each plot has a wide y-axis title + a right-side legend, so the
//! difference between the two modes is visible: under `Plot` the
//! title is offset right by half the left chrome; under `Panel` it
//! sits flush above the panel.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb, rgb8, Color};
use hephaestus::composition::{beside, Patch};
use hephaestus::geometry::Size;
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement};
use hephaestus::plot::chrome::legend::Legend;
use hephaestus::plot::theme::{AlignTo, Element, HAlign, Theme};
use hephaestus::plot::{scale, Plot, PlotComposition, PointGeom};
use hephaestus::scales::chrome::{AxisSide, LegendSide};
use hephaestus::Renderer;

fn main() {
    let (w, h) = (1400u32, 500u32);
    let dpi = 96.0;

    let comp = || beside(Patch::new("a"), Patch::new("b"));
    let xs: Vec<f64> = (0..40).map(|i| i as f64 * 0.15).collect();
    let ys: Vec<f64> = xs.iter().map(|x| (x * 0.7).sin() * 0.4 + 0.5).collect();
    let categories: Vec<&str> = xs
        .iter()
        .map(|x| match (*x as usize) % 4 {
            0 => "A",
            1 => "B",
            2 => "C",
            _ => "D",
        })
        .collect();

    let make_plot = |patch_id: &str, title: &str| {
        let mut plot = Plot::new(&comp(), patch_id)
            .bind("x", "x_scale")
            .bind("y", "y_scale")
            .bind("stroke", "category")
            .title(title);
        plot.add_geom(
            PointGeom::builder()
                .set("x", xs.clone())
                .set("y", ys.clone())
                .set("size", 6.0_f64)
                .set("fill", rgb(0.20, 0.45, 0.85))
                .set("stroke", categories.clone())
                .set("linewidth", 1.0_f64)
                .build(),
        );
        plot.add_axis(
            Axis::rail("x_scale", AxisPlacement::Cartesian(AxisSide::Bottom)).title("Time (s)"),
        );
        plot.add_axis(
            Axis::rail("y_scale", AxisPlacement::Cartesian(AxisSide::Left))
                .title("A wide y-axis title"),
        );
        plot.add_legend(
            Legend::new("category")
                .side(LegendSide::Right)
                .title("Group")
                .key(
                    hephaestus::plot::chrome::legend::LegendKeySpec::point()
                        .scaled("stroke", "category"),
                ),
        );
        plot
    };

    let category_scale = scale::discrete([
        hephaestus::scales::Value::String(std::sync::Arc::from("A")),
        hephaestus::scales::Value::String(std::sync::Arc::from("B")),
        hephaestus::scales::Value::String(std::sync::Arc::from("C")),
        hephaestus::scales::Value::String(std::sync::Arc::from("D")),
    ])
    .range_colors([
        hephaestus::color::rgb(0.20, 0.20, 0.20),
        hephaestus::color::rgb(0.70, 0.20, 0.20),
        hephaestus::color::rgb(0.20, 0.60, 0.20),
        hephaestus::color::rgb(0.20, 0.20, 0.70),
    ]);

    // Left-align the title so its left edge anchors visibly differ
    // between the two `AlignTo` modes — under `Plot` it lands at
    // the left edge of the legend / plot interior; under `Panel`
    // it lands at the left edge of the panel itself. Mutating
    // `plot_title` in place (rather than constructing a new
    // `Element::Set(...)`) preserves the existing 16pt-bold styling
    // from `Theme::default`.
    let mut theme = Theme {
        plot_text_align_to: AlignTo::Plot,
        ..Theme::default()
    };
    if let Element::Set(t) = &mut theme.plot_title {
        t.align = Some(HAlign::Start);
    }
    let mut view = PlotComposition::new(comp())
        .add_scale("x_scale", scale::continuous(0.0..=6.0))
        .add_scale("y_scale", scale::continuous(0.0..=1.0))
        .add_scale("category", category_scale)
        .theme(theme);
    view.attach_plot(make_plot(
        "a",
        "AlignTo::Plot — title left-edge aligns to plot interior",
    ));
    // Second plot uses a per-plot theme override to flip to Panel.
    view.attach_plot(
        make_plot("b", "AlignTo::Panel — title left-edge aligns to panel").theme_override(
            hephaestus::plot::theme::ThemePart {
                plot_text_align_to: Some(AlignTo::Panel),
                ..hephaestus::plot::theme::ThemePart::default()
            },
        ),
    );

    let mut renderer = VelloRenderer::new().expect("vello renderer init");
    let bg: Color = rgb8(245, 245, 245);
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
        .join("examples/theme_text_align_to.png");
    hephaestus::png::write_png(&path, w, h, &pixels).expect("write png");
    println!("wrote {}", path.display());
}
