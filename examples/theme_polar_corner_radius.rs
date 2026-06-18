//! Verifies that `theme.panel_background.corner_radius` rounds the
//! corners of a polar panel's wedge (line-to-arc joins) and that the
//! geom clip mask uses the same rounded boundary.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb, rgb8, Color};
use hephaestus::composition::{Composition, Patch, Span};
use hephaestus::geometry::Size;
use hephaestus::plot::projection::{PolarProjection, Projection};
use hephaestus::plot::theme::{Element, Length, RectElement, Theme, ThemeColor};
use hephaestus::plot::{scale, Plot, PlotComposition, PointGeom};
use hephaestus::Renderer;

fn main() {
    let (w, h) = (700u32, 700u32);
    let dpi = 96.0;

    let comp = || Composition::empty(1, 1).place(1, 1, Span::cell(), Patch::new("p"));

    // Quarter-arc projection: a 90° wedge with a center hole. Four
    // line-to-arc corners — perfect for showcasing corner_radius.
    let projection = Projection::Polar(PolarProjection {
        theta_start: 0.0,
        theta_end: std::f64::consts::FRAC_PI_2,
        inner_radius_frac: 0.3,
        ..PolarProjection::full_circle()
    });

    let n = 80;
    let theta: Vec<f64> = (0..n).map(|i| i as f64 / (n - 1) as f64).collect();
    // Vary radius sinusoidally so points spread across the annular
    // panel and bump against the rounded corners.
    let radius: Vec<f64> = (0..n)
        .map(|i| 0.5 + 0.45 * (i as f64 / 8.0).sin())
        .collect();
    let mut plot = Plot::new(&comp(), "p")
        .projection(projection)
        .bind("x", "theta")
        .bind("y", "radius")
        .title("Polar panel + corner_radius");
    plot.add_geom(
        PointGeom::builder()
            .set("x", theta)
            .set("y", radius)
            .set("size", 8.0_f64)
            .set("fill", rgb(0.85, 0.45, 0.2))
            .set("stroke", rgb(0.2, 0.2, 0.2))
            .set("linewidth", 1.0_f64)
            .build(),
    );

    // Theme override: 16pt corner radius on the panel background. The
    // panel border (1pt) follows the same path; the geom clip mask
    // does too — points near the (rounded) corner are visibly
    // clipped to the smoothed boundary rather than the sharp wedge.
    let theme = Theme {
        panel_background: Element::Set(RectElement {
            fill: Some(ThemeColor::Mix(
                Box::new(ThemeColor::Paper),
                Box::new(ThemeColor::Accent),
                0.12,
            )),
            color: Some(ThemeColor::Ink),
            linewidth_pt: Some(Length::Abs(1.0)),
            corner_radius: Some(Length::Abs(16.0)),
            ..RectElement::default()
        }),
        ..Theme::default()
    };

    let mut view = PlotComposition::new(comp())
        .add_scale("theta", scale::continuous(0.0..=1.0))
        .add_scale("radius", scale::continuous(0.0..=1.0))
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
        .join("examples/theme_polar_corner_radius.png");
    hephaestus::png::write_png(&path, w, h, &pixels).expect("write png");
    println!("wrote {}", path.display());
}
