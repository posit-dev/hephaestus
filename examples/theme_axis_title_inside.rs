//! Demonstrates `AxisTheme::title_location = Inside` — axis titles
//! draw inside the panel instead of in the outer chrome slot, freeing
//! up plot real estate for the panel itself.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb, rgb8, Color};
use hephaestus::composition::{Composition, Patch, Span};
use hephaestus::geometry::Size;
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement};
use hephaestus::plot::theme::{
    AxisTheme, Element, HAlign, Rotation, TextElement, Theme, TitleLocation, VAlign,
};
use hephaestus::plot::{scale, Plot, PlotComposition, PointGeom};
use hephaestus::scales::chrome::AxisSide;
use hephaestus::Renderer;

fn main() {
    let (w, h) = (900u32, 600u32);
    let dpi = 96.0;

    let comp = || Composition::empty(1, 1).place(1, 1, Span::cell(), Patch::new("p"));
    let xs: Vec<f64> = (0..40).map(|i| i as f64 * 0.15).collect();
    let ys: Vec<f64> = xs.iter().map(|x| (x * 0.7).sin() * 0.4 + 0.5).collect();

    let mut plot = Plot::new(&comp(), "p")
        .bind("x", "x_scale")
        .bind("y", "y_scale")
        .title("Inside axis titles");
    plot.add_geom(
        PointGeom::builder()
            .set("x", xs)
            .set("y", ys)
            .set("size", 6.0_f64)
            .set("fill", rgb(0.20, 0.45, 0.85))
            .build(),
    );
    plot.add_axis(
        Axis::rail("x_scale", AxisPlacement::Cartesian(AxisSide::Bottom)).title("Time (s)"),
    );
    plot.add_axis(
        Axis::rail("y_scale", AxisPlacement::Cartesian(AxisSide::Left)).title("Amplitude"),
    );

    // Common in-panel idiom: titles sit at the **end** of their
    // respective axes, horizontal (no rotation) — like the publication
    // convention "x-axis label flush right under the axis end, y-axis
    // label flush left above the axis end." Achieved with three
    // overrides on each axis title:
    //
    //   - `title_location: Inside`        — anchor against the panel.
    //   - `angle: Rotation::Degrees(0.0)` — never rotate.
    //   - `align` / `valign`              — push the text to the
    //                                       axis-end corner.
    //
    // Bottom axis (channel 0, side 0): right-end = bottom-right corner
    // → `align: End`, `valign: Bottom`.
    // Left axis (channel 1, side 0): top-end = top-left corner
    // → `align: Start`, `valign: Top`.
    //
    // Every other TextElement field (color, size_pt, font weight,
    // lineheight, margin) cascades through `AxisTheme.all.title` and
    // ultimately `theme.text` — no need to re-state them here.
    let mut theme = Theme::default();
    theme.axis.by_channel_side[0][0] = AxisTheme {
        title_location: Some(TitleLocation::Inside),
        title: Element::Set(TextElement {
            angle: Some(Rotation::Degrees(0.0)),
            align: Some(HAlign::End),
            valign: Some(VAlign::Bottom),
            ..TextElement::default()
        }),
        ..AxisTheme::default()
    };
    theme.axis.by_channel_side[1][0] = AxisTheme {
        title_location: Some(TitleLocation::Inside),
        title: Element::Set(TextElement {
            angle: Some(Rotation::Degrees(0.0)),
            align: Some(HAlign::Start),
            valign: Some(VAlign::Top),
            ..TextElement::default()
        }),
        ..AxisTheme::default()
    };

    let mut view = PlotComposition::new(comp())
        .add_scale("x_scale", scale::continuous(0.0..=6.0))
        .add_scale("y_scale", scale::continuous(0.0..=1.0))
        .theme(theme);
    view.attach_plot(plot);

    let mut renderer = VelloRenderer::new().expect("vello renderer init");
    let bg: Color = rgb8(248, 248, 248);
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
        .join("examples/theme_axis_title_inside.png");
    hephaestus::png::write_png(&path, w, h, &pixels).expect("write png");
    println!("wrote {}", path.display());
}
