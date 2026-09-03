//! A plot exported as SVG, beside the same plot as a PNG.
//!
//! Both are rendered from one composition through one `render` call
//! each, so the pair is directly comparable — which is the thing most
//! worth eyeballing on this backend. Open `svg_export.svg` in a browser
//! for fidelity, and in a vector editor to confirm the text is text:
//! every label is a `<text>` element you can click into and retype.
//!
//! The three title slots opt into markdown, so the export also
//! exercises the rich path: emphasis, strong and a `code` span become
//! styled `<tspan>`s inside a single `<text>`, and the code span's
//! rounded background is written ahead of the text it sits behind
//! rather than splitting the sentence into two objects.
//!
//! ```sh
//! cargo run --example svg_export --features svg
//! ```

use hephaestus::backend::svg::{write_svg, SvgConfig, SvgScene};
use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{beside, Patch};
use hephaestus::geometry::Size;
use hephaestus::image::PngCompression;
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement};
use hephaestus::plot::theme::{Element, TextElement, Theme};
use hephaestus::plot::{scale, Plot, PlotComposition, PointGeom};
use hephaestus::scales::chrome::AxisSide;
use hephaestus::scene::SceneBuilder;
use hephaestus::Renderer;

fn main() {
    let (w, h) = (900u32, 400u32);
    let dpi = 96.0;
    let size = Size::new(w as f64, h as f64);

    let comp = || beside(Patch::new("price"), Patch::new("volume"));
    let xs: Vec<f64> = (0..40).map(|i| i as f64 * 2.5).collect();
    let ys_price: Vec<f64> = xs
        .iter()
        .map(|x| 50.0 + 20.0 * (x * 0.06).sin() + 0.1 * x)
        .collect();
    let ys_volume: Vec<f64> = xs
        .iter()
        .map(|x| 1.0e5 + 4.0e4 * (x * 0.04 + 1.0).cos().abs())
        .collect();

    // Subtitles rather than titles on the panels: the composition owns
    // the title row, and a plot title would be hoisted into it and land
    // beside the figure's own.
    let mut plot_price = Plot::new(&comp(), "price")
        .bind("x", "time")
        .bind("y", "price_y")
        .subtitle("Price over *time*");
    plot_price.add_geom(
        PointGeom::builder()
            .set("x", xs.clone())
            .set("y", ys_price)
            .set("fill", rgb8(220, 90, 70))
            .set("size", 5.0_f64)
            .build(),
    );
    plot_price.add_axis(Axis::rail(
        "time",
        AxisPlacement::Cartesian(AxisSide::Bottom),
    ));
    plot_price.add_axis(Axis::rail(
        "price_y",
        AxisPlacement::Cartesian(AxisSide::Left),
    ));

    let mut plot_volume = Plot::new(&comp(), "volume")
        .bind("x", "time")
        .bind("y", "volume_y")
        .subtitle("Volume, in `units`");
    plot_volume.add_geom(
        PointGeom::builder()
            .set("x", xs)
            .set("y", ys_volume)
            .set("fill", rgb8(70, 120, 220))
            .set("size", 5.0_f64)
            .build(),
    );
    plot_volume.add_axis(Axis::rail(
        "time",
        AxisPlacement::Cartesian(AxisSide::Bottom),
    ));
    plot_volume.add_axis(Axis::rail(
        "volume_y",
        AxisPlacement::Cartesian(AxisSide::Left),
    ));

    // Markdown in chrome is opt-in per slot, so switch it on for the
    // three title slots only. Leaving it off elsewhere is deliberate:
    // an axis label is arbitrary data, and a value containing `*` or
    // `_` should render as itself rather than as emphasis.
    let mut theme = Theme::default();
    let markdown_slot = TextElement {
        markdown: Some(true),
        ..TextElement::default()
    };
    theme.plot_title = Element::Set(markdown_slot.clone());
    theme.plot_subtitle = Element::Set(markdown_slot.clone());
    theme.plot_caption = Element::Set(markdown_slot);

    let mut view = PlotComposition::new(&comp())
        .add_scale("time", scale::continuous(0.0..=100.0))
        .add_scale("price_y", scale::continuous(40.0..=90.0))
        .add_scale("volume_y", scale::continuous(80_000.0..=160_000.0))
        .with_plot(plot_price)
        .with_plot(plot_volume);
    view.set_theme(theme);
    view.set_title("Two panels, exported as **vector**");

    // Vector.
    let mut svg = SvgScene::with_config(size, dpi, SvgConfig::new().background(Some(Color::WHITE)));
    view.render(&mut svg, size, dpi);
    write_svg("examples/svg_export.svg", &svg).expect("write svg");
    let warnings = svg.warnings();
    if warnings.is_empty() {
        println!("examples/svg_export.svg — no degradations");
    } else {
        println!("examples/svg_export.svg — {warnings:?}");
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
        .render_to_buffer(w, h, Color::WHITE, &mut pixels)
        .expect("render");
    hephaestus::image::write_png(
        "examples/svg_export.png",
        w,
        h,
        &pixels,
        PngCompression::Balanced,
        Some(dpi),
    )
    .expect("write png");
    println!("examples/svg_export.png");
}
