//! Demonstrates glyph outlines at both API levels — the per-glyph
//! `text_stroke` + `text_linewidth` channels on `TextGeom`, and the
//! matching `text_stroke` / `text_linewidth_pt` fields on the theme's
//! `TextElement` that outline chrome text. Renders two side-by-side
//! panels:
//!
//! - Left: plain labels on a busy colored backdrop — readability
//!   suffers where labels cross strong color contrast.
//! - Right: the same labels with a contrasting outline (white stroke
//!   under black fill) so they pop against any backdrop.
//!
//! Both panels' titles and axis labels are outlined through the theme,
//! so chrome and data labels use the same specification.
//!
//! Writes `examples/text_outline.png`.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{Composition, Patch, Span};
use hephaestus::geometry::Size;
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement};
use hephaestus::plot::theme::{Element, Length, TextElement, Theme, ThemeColor};
use hephaestus::plot::{scale, Plot, PlotComposition, PointGeom, TextGeom};
use hephaestus::scales::chrome::AxisSide;
use hephaestus::scene::SceneBuilder;
use hephaestus::Renderer;

/// A theme whose titles and axis titles are drawn as knocked-out white
/// text over a dark halo — the chrome-side counterpart of the geoms'
/// `text_stroke` channel, and the reason `TextElement` carries the same
/// pair of fields.
///
/// Overlays onto whatever each slot already holds, so the built-in
/// theme's bold title and sized axis text survive.
fn outlined_chrome_theme() -> Theme {
    /// Knock the text out in white over a dark outline.
    fn haloed(slot: &Element<TextElement>, width_pt: f64) -> Element<TextElement> {
        let mut el = slot.as_set().cloned().unwrap_or_default();
        el.color = Some(ThemeColor::Fixed(rgb8(255, 255, 255)));
        el.text_stroke = Some(ThemeColor::Fixed(rgb8(25, 25, 40)));
        el.text_linewidth_pt = Some(Length::Abs(width_pt));
        Element::Set(el)
    }
    let mut theme = Theme::default();
    theme.plot_title = haloed(&theme.plot_title, 3.5);
    theme.axis.all.title = haloed(&theme.axis.all.title, 3.0);
    theme
}

fn main() {
    let (w, h) = (1200u32, 500u32);
    let dpi = 96.0;
    let bg: Color = rgb8(245, 245, 250);

    // Two-panel layout so the with/without comparison sits side-by-side.
    let comp = || {
        Composition::empty(1, 2)
            .place(1, 1, Span::cell(), Patch::new("plain"))
            .place(1, 2, Span::cell(), Patch::new("outlined"))
    };

    // A field of brightly-colored points that any overlaid label will
    // partially cross. The labels themselves sit at the same data
    // positions so they overlap the points.
    let n = 9;
    let xs: Vec<f64> = (0..n).map(|i| 10.0 + (i as f64) * 10.0).collect();
    let ys: Vec<f64> = (0..n)
        .map(|i| 50.0 + 30.0 * ((i as f64) * 0.7).sin())
        .collect();
    let labels: Vec<&str> = vec![
        "alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "theta", "iota",
    ];
    let point_fills: Vec<Color> = (0..n)
        .map(|i| {
            let t = i as f64 / (n - 1) as f64;
            // hsl-ish rainbow via rgb
            let r = (255.0 * (1.0 - t)) as u8;
            let g = (255.0 * (1.0 - (t - 0.5).abs() * 2.0).max(0.0)) as u8;
            let b = (255.0 * t) as u8;
            rgb8(r, g, b)
        })
        .collect();

    let mut plain = Plot::new(&comp(), "plain")
        .bind("x", "x")
        .bind("y", "y")
        .title("Plain text");
    plain.add_geom(
        PointGeom::builder()
            .set("x", xs.clone())
            .set("y", ys.clone())
            .set("fill", point_fills.clone())
            .set("size", 22.0_f64)
            .build(),
    );
    plain.add_geom(
        TextGeom::builder()
            .set("x", xs.clone())
            .set("y", ys.clone())
            .set("text", labels.clone())
            .set("size", 14.0_f64)
            .set("weight", 700.0_f64)
            .set("fill", rgb8(20, 20, 30))
            .build(),
    );

    let mut outlined = Plot::new(&comp(), "outlined")
        .bind("x", "x")
        .bind("y", "y")
        .title("text_stroke + text_linewidth");
    outlined.add_geom(
        PointGeom::builder()
            .set("x", xs.clone())
            .set("y", ys.clone())
            .set("fill", point_fills)
            .set("size", 22.0_f64)
            .build(),
    );
    outlined.add_geom(
        TextGeom::builder()
            .set("x", xs)
            .set("y", ys)
            .set("text", labels)
            .set("size", 14.0_f64)
            .set("weight", 700.0_f64)
            .set("fill", rgb8(20, 20, 30))
            .set("text_stroke", rgb8(255, 255, 255))
            .set("text_linewidth", 3.0_f64)
            .build(),
    );

    // Axes on both panels so the theme's outlined tick labels and axis
    // titles are visible alongside the outlined geom labels.
    for p in [&mut plain, &mut outlined] {
        p.add_axis(Axis::rail("x", AxisPlacement::Cartesian(AxisSide::Bottom)).title("x position"));
        p.add_axis(Axis::rail("y", AxisPlacement::Cartesian(AxisSide::Left)).title("y position"));
    }

    let mut view = PlotComposition::new(&comp())
        .theme(outlined_chrome_theme())
        .add_scale("x", scale::continuous(0.0..=100.0))
        .add_scale("y", scale::continuous(0.0..=100.0))
        .with_plot(plain)
        .with_plot(outlined);

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
        .join("examples/text_outline.png");
    hephaestus::png::write_png(&path, w, h, &pixels).expect("write png");
    println!("wrote {}", path.display());
}
