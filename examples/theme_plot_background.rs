//! Demonstrates `theme.plot_background` and `theme.plot_margin`.
//! `plot_margin` sizes the patch anatomy's outermost ring (rows 1,
//! 16 and cols 1, 13); `plot_background` paints into the area inside
//! the margin (the patch's `Slot::Background`).

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{Composition, Patch, Span};
use hephaestus::geometry::Size;
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement};
use hephaestus::plot::chrome::legend::{Legend, LegendKeySpec};
use hephaestus::plot::theme::{Element, Length, Margin, RectElement, Theme, ThemeColor};
use hephaestus::plot::{scale, Plot, PlotComposition, PointGeom};
use hephaestus::scales::chrome::{AxisSide, LegendSide};
use hephaestus::Renderer;

fn main() {
    let (w, h) = (900u32, 600u32);
    let dpi = 96.0;

    let comp = || Composition::empty(1, 1).place(1, 1, Span::cell(), Patch::new("p"));
    let xs: Vec<f64> = (0..40).map(|i| i as f64 * 0.15).collect();
    let ys: Vec<f64> = xs.iter().map(|x| (x * 0.7).sin() * 0.4 + 0.5).collect();

    // Use the y values themselves as the colour-mapped channel so a
    // colorbar legend makes sense.
    let colours: Vec<f64> = ys.clone();
    // A second, discrete category column (assigned by x bucket),
    // used to drive the categorical stroke colour and a discrete
    // legend on the right next to the colorbar.
    let categories: Vec<&str> = xs
        .iter()
        .map(|x| match (*x as usize) % 4 {
            0 => "A",
            1 => "B",
            2 => "C",
            _ => "D",
        })
        .collect();
    let mut plot = Plot::new(&comp(), "p")
        .bind("x", "x_scale")
        .bind("y", "y_scale")
        .bind("fill", "fill_scale")
        .bind("stroke", "stroke_scale")
        .title("Rounded corners — plot bg, panel bg, frames");
    plot.add_geom(
        PointGeom::builder()
            .set("x", xs)
            .set("y", ys)
            .set("size", 8.0_f64)
            .set("fill", colours)
            .set("stroke", categories)
            .set("linewidth", 1.0_f64)
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
    plot.add_legend(
        Legend::colorbar("fill_scale")
            .side(LegendSide::Right)
            .title("Amplitude"),
    );
    plot.add_legend(
        Legend::new("stroke_scale")
            .side(LegendSide::Right)
            .title("Group")
            .key(LegendKeySpec::point().scaled("stroke", "stroke_scale")),
    );

    // Two outer bands: 24pt `plot_margin` sits outside the
    // background; 18pt `plot_padding` sits inside it. Both feed the
    // anatomical ring tracks, so chrome lands in the correct rhythm
    // automatically.
    let theme = Theme {
        plot_margin: Margin::all(Length::Abs(24.0)),
        plot_padding: Margin::all(Length::Abs(18.0)),
        plot_background: Element::Set(RectElement {
            fill: Some(ThemeColor::Mix(
                Box::new(ThemeColor::Paper),
                Box::new(ThemeColor::Accent),
                0.3,
            )),
            color: Some(ThemeColor::Ink),
            linewidth_pt: Some(Length::Abs(2.0)),
            corner_radius: Some(Length::Abs(12.0)),
            ..RectElement::default()
        }),
        // Round the panel corners too — the geom clip mask uses the
        // same rounded path so data points crop cleanly.
        panel_background: Element::Set(RectElement {
            corner_radius: Some(Length::Abs(8.0)),
            ..Theme::default().panel_background.as_set().unwrap().clone()
        }),
        // Colorbar bar + discrete key frames share `RectElement`
        // semantics: fill paints under the inner content (gradient
        // for the colorbar, marker for the key) so transparent
        // colours show the frame fill; stroke + corner_radius paint
        // on top.
        legend: hephaestus::plot::theme::LegendTheme {
            bar: hephaestus::plot::theme::BarTheme {
                frame: Element::Set(RectElement {
                    fill: Some(ThemeColor::Mix(
                        Box::new(ThemeColor::Paper),
                        Box::new(ThemeColor::Ink),
                        0.08,
                    )),
                    color: Some(ThemeColor::Ink),
                    linewidth_pt: Some(Length::Abs(1.5)),
                    corner_radius: Some(Length::Abs(6.0)),
                    ..RectElement::default()
                }),
                ..hephaestus::plot::theme::BarTheme::default()
            },
            key: hephaestus::plot::theme::KeyTheme {
                width: Length::Abs(20.0),
                height: Length::Abs(20.0),
                frame: Element::Set(RectElement {
                    fill: Some(ThemeColor::Mix(
                        Box::new(ThemeColor::Paper),
                        Box::new(ThemeColor::Ink),
                        0.08,
                    )),
                    color: Some(ThemeColor::Ink),
                    linewidth_pt: Some(Length::Abs(0.75)),
                    corner_radius: Some(Length::Abs(4.0)),
                    ..RectElement::default()
                }),
                ..hephaestus::plot::theme::KeyTheme::default()
            },
            ..Theme::default().legend
        },
        ..Theme::default()
    };

    let fill_scale = scale::continuous(0.0..=1.0).range_colors([
        hephaestus::color::rgb(0.2, 0.3, 0.6),
        hephaestus::color::rgb(0.85, 0.45, 0.2),
    ]);
    let stroke_scale = scale::discrete([
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
    let mut view = PlotComposition::new(comp())
        .add_scale("x_scale", scale::continuous(0.0..=6.0))
        .add_scale("y_scale", scale::continuous(0.0..=1.0))
        .add_scale("fill_scale", fill_scale)
        .add_scale("stroke_scale", stroke_scale)
        .theme(theme);
    view.attach_plot(plot);

    let mut renderer = VelloRenderer::new().expect("vello renderer init");
    // Contrasting canvas bg makes the plot_margin band visible
    // outside the (warm-cream) plot_background.
    let bg: Color = rgb8(60, 70, 90);
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
        .join("examples/theme_plot_background.png");
    hephaestus::png::write_png(&path, w, h, &pixels).expect("write png");
    println!("wrote {}", path.display());
}
