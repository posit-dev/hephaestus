//! Axis titles inside the panel, parallel to their axis and anchored
//! to its end — the "report" style: the y title runs horizontally
//! above the y axis at the panel's top-left, and the x title runs
//! horizontally at the panel's bottom-right. Both reserve no chrome
//! band, so the panel keeps the space an outside title would take.
//!
//! The pieces, all per-(channel, side) theme fields:
//! - `title_location = Inside` moves the title into a strip flush
//!   against the panel edge,
//! - `angle = Degrees(0)` overrides the default `Along` rotation, and
//! - `valign = Top` / `align = End` anchor the title within that
//!   strip, since the strip spans the full panel height (Left) or
//!   width (Bottom).
//!
//! ```sh
//! cargo run --example axis_title_inside
//! ```

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{Composition, Patch, Span};
use hephaestus::geometry::Size;
use hephaestus::image::PngCompression;
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement};
use hephaestus::plot::theme::{
    Element, HAlign, Rotation, TextElement, Theme, TitleLocation, VAlign,
};
use hephaestus::plot::{scale, Plot, PlotComposition, PointGeom};
use hephaestus::scales::chrome::AxisSide;
use hephaestus::Renderer;

fn main() {
    let comp = Composition::empty(1, 1).place(1, 1, Span::cell(), Patch::new("p"));

    let xs: Vec<f64> = (0..60).map(|i| i as f64 * 0.1).collect();
    let ys: Vec<f64> = xs
        .iter()
        .map(|x| 2.0 + x * 0.35 + (x * 1.7).sin() * 0.6)
        .collect();

    let mut plot = Plot::new(&comp, "p").bind("x", "x").bind("y", "y");
    plot.add_geom(
        PointGeom::builder()
            .set("x", xs)
            .set("y", ys)
            .set("fill", rgb8(40, 90, 160))
            .set("size", 5.0_f64)
            .build(),
    );
    plot.add_axis(
        Axis::rail("x", AxisPlacement::Cartesian(AxisSide::Bottom))
            .title("Engine displacement (l)"),
    );
    plot.add_axis(
        Axis::rail("y", AxisPlacement::Cartesian(AxisSide::Left))
            .title("Highway fuel economy (mpg)"),
    );

    let mut theme = Theme::default();
    // y title: horizontal, top of the left edge.
    theme.axis.by_channel_side[1][0].title = Element::Set(TextElement {
        angle: Some(Rotation::Degrees(0.0)),
        valign: Some(VAlign::Top),
        ..TextElement::default()
    });
    theme.axis.by_channel_side[1][0].title_location = Some(TitleLocation::Inside);
    // x title: horizontal, end of the bottom edge.
    theme.axis.by_channel_side[0][0].title = Element::Set(TextElement {
        angle: Some(Rotation::Degrees(0.0)),
        align: Some(HAlign::End),
        ..TextElement::default()
    });
    theme.axis.by_channel_side[0][0].title_location = Some(TitleLocation::Inside);

    let mut view = PlotComposition::new(&comp).with_plot(plot);
    view.insert_scale("x", scale::continuous(0.0..=6.0));
    view.insert_scale("y", scale::continuous(0.0..=5.0));
    view.set_theme(theme);

    let (w, h) = (640u32, 420u32);
    let size = Size::new(w as f64, h as f64);
    let mut renderer = VelloRenderer::new().expect("vello renderer init");
    {
        let scene = renderer.scene();
        view.render(scene, size, 96.0);
    }
    let mut pixels = vec![0u8; (w * h * 4) as usize];
    renderer
        .render_to_buffer(w, h, Color::WHITE, &mut pixels)
        .expect("render");

    let path = std::env::current_dir()
        .unwrap()
        .join("examples/axis_title_inside.png");
    hephaestus::image::write_png(&path, w, h, &pixels, PngCompression::Balanced, None)
        .expect("write png");
    println!("wrote {}", path.display());
}
