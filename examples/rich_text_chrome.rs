//! Demonstrates the `markdown` opt-in on chrome `TextElement`s. A
//! plot's title, subtitle, caption, and axis titles all render through
//! [`draw_text_element_in_rect`], so setting
//! `TextElement::markdown = Some(true)` on any of them routes that
//! slot through [`draw_rich_text`] against the theme's `rich_text`
//! sheet.
//!
//! Renders one plot whose title highlights a variable name in a
//! hex-colour span, whose subtitle italicises the metric name, and
//! whose caption emphasises the sample size in bold.
//!
//! Writes `examples/rich_text_chrome.png`.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{Composition, Patch, Span};
use hephaestus::geometry::Size;
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement};
use hephaestus::plot::theme::{Element, TextElement, Theme};
use hephaestus::plot::{scale, Plot, PlotComposition, PointGeom};
use hephaestus::scales::chrome::AxisSide;
use hephaestus::scene::SceneBuilder;
use hephaestus::Renderer;

/// A theme where every text slot opts into markdown shaping.
fn markdown_chrome_theme() -> Theme {
    fn md(slot: &Element<TextElement>) -> Element<TextElement> {
        let mut el = slot.as_set().cloned().unwrap_or_default();
        el.markdown = Some(true);
        Element::Set(el)
    }
    let mut theme = Theme::default();
    theme.plot_title = md(&theme.plot_title);
    theme.plot_subtitle = md(&theme.plot_subtitle);
    theme.plot_caption = md(&theme.plot_caption);
    // Axis titles cascade through `axis.all.title`.
    theme.axis.all.title = md(&theme.axis.all.title);
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

    let mut plot = Plot::new(&comp(), "p")
        .bind("x", "x")
        .bind("y", "y")
        .title("Trend of **{#c14b4b price}** across the day")
        .subtitle("Metric: *closing_bid* — sampled hourly")
        .caption("n = **60**, source: {.gray internal}");
    plot.add_geom(
        PointGeom::builder()
            .set("x", xs)
            .set("y", ys)
            .set("fill", rgb8(88, 106, 195))
            .set("size", 8.0_f64)
            .build(),
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
    hephaestus::png::write_png(&path, w, h, &pixels).expect("write png");
    println!("wrote {}", path.display());
}
