//! Demonstrates the `markdown` opt-in on chrome `TextElement`s.
//!
//! Setting `markdown = Some(true)` once on the theme's root `text`
//! element reaches every text slot: the title band, axis titles, the
//! legend title, and the break labels on both the axis and the legend.
//!
//! Renders one plot whose title highlights a variable name in a
//! hex-colour span, whose subtitle italicises the metric name, whose
//! caption emphasises the sample size in bold, and whose legend
//! labels each carry their own inline styling.
//!
//! Writes `examples/rich_text_chrome.png`.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{Composition, Patch, Span};
use hephaestus::geometry::Size;
use hephaestus::image::PngCompression;
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement};
use hephaestus::plot::chrome::legend::{Legend, LegendKeySpec};
use hephaestus::plot::theme::Theme;
use hephaestus::plot::{scale, Plot, PlotComposition, PointGeom};
use hephaestus::scales::value::Value;
use std::sync::Arc;

use hephaestus::scales::chrome::AxisSide;
use hephaestus::scene::SceneBuilder;
use hephaestus::Renderer;

/// A theme where every text slot opts into markdown shaping. One
/// field on the root element — the cascade carries it to the title
/// band, the axis titles and labels, and the legend.
fn markdown_chrome_theme() -> Theme {
    let mut theme = Theme::default();
    theme.text.markdown = Some(true);
    theme
}

fn main() {
    let (w, h) = (900u32, 560u32);
    let dpi = 96.0;
    let bg: Color = rgb8(250, 250, 253);

    let comp = || Composition::empty(1, 1).place(1, 1, Span::cell(), Patch::new("p"));

    let n = 60;
    let xs: Vec<f64> = (0..n).map(|i| i as f64 / (n - 1) as f64 * 10.0).collect();
    let ys: Vec<f64> = xs
        .iter()
        .map(|x| 2.5 + 1.2 * (x * 0.7).sin() + 0.4 * (x * 2.1).cos())
        .collect();

    // Break labels are data-derived, so a category that spells
    // markdown gets parsed like any other string.
    let bands: [&'static str; 3] = ["*low*", "**mid**", "{.red high}"];
    let groups: Vec<&'static str> = ys
        .iter()
        .map(|y| match *y {
            v if v < 2.0 => bands[0],
            v if v < 3.0 => bands[1],
            _ => bands[2],
        })
        .collect();

    let mut plot = Plot::new(&comp(), "p")
        .bind("x", "x")
        .bind("y", "y")
        .bind("fill", "band")
        .title("Trend of **{#c14b4b price}** across the day")
        .subtitle("Metric: *closing_bid* — sampled hourly")
        .caption("n = **60**, source: {.gray internal}");
    plot.add_geom(
        PointGeom::builder()
            .set("x", xs)
            .set("y", ys)
            .set("fill", groups)
            .set("size", 8.0_f64)
            .build(),
    );
    plot.add_legend(
        Legend::new("band")
            .title("**band** of the *close*")
            .key(LegendKeySpec::point().scaled("fill", "band")),
    );
    plot.add_axis(
        Axis::rail("x", AxisPlacement::Cartesian(AxisSide::Bottom))
            .title("hour of day, {.gray *UTC*}"),
    );
    plot.add_axis(
        Axis::rail("y", AxisPlacement::Cartesian(AxisSide::Left)).title("**price** (USD)"),
    );

    let mut view = PlotComposition::new(&comp())
        .theme(markdown_chrome_theme())
        .add_scale("x", scale::continuous(0.0..=10.0))
        .add_scale("y", scale::continuous(0.0..=5.0))
        .add_scale(
            "band",
            scale::discrete(bands.iter().map(|b| Value::String(Arc::from(*b)))).range_colors([
                rgb8(88, 106, 195),
                rgb8(214, 146, 60),
                rgb8(193, 75, 75),
            ]),
        )
        .with_plot(plot);

    let mut renderer = VelloRenderer::new().expect("vello renderer init");
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
        .join("examples/rich_text_chrome.png");
    hephaestus::image::write_png(&path, w, h, &pixels, PngCompression::Balanced, Some(dpi))
        .expect("write png");
    println!("wrote {}", path.display());
}
