//! Demonstrates `theme.plot_background` and `theme.plot_margin`.
//! `plot_margin` sizes the patch anatomy's outermost ring (rows 1,
//! 16 and cols 1, 13); `plot_background` paints into the area inside
//! the margin (the patch's `Slot::Background`).

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb, rgb8, Color};
use hephaestus::composition::{Composition, Patch, Span};
use hephaestus::geometry::Size;
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement};
use hephaestus::plot::theme::{Element, Length, Margin, RectElement, Theme, ThemeColor};
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
        .title("plot_margin + plot_background");
    plot.add_geom(
        PointGeom::builder()
            .set("x", xs)
            .set("y", ys)
            .set("size", 6.0_f64)
            .set("fill", rgb(0.20, 0.45, 0.85))
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
            color: ThemeColor::Ink,
            linewidth_pt: Length::Abs(2.0),
            linetype: std::sync::Arc::from([]),
            corner_radius: Length::Abs(0.0),
        }),
        ..Theme::default()
    };

    let mut view = PlotComposition::new(comp())
        .add_scale("x_scale", scale::continuous(0.0..=6.0))
        .add_scale("y_scale", scale::continuous(0.0..=1.0))
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
