//! Fetch a Google Fonts family by name and use it in a plot.
//!
//! Run with:
//!
//! ```sh
//! cargo run --example google_fonts --features vello,png,text-google-fonts
//! ```
//!
//! Cold cache → hits `fonts.googleapis.com` once per family, writes
//! the downloaded TTFs to the platform cache dir, registers them.
//! Warm cache → registers directly from disk, no network.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{Composition, Patch, Span};
use hephaestus::geometry::Size;
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement};
use hephaestus::plot::theme::{FontFamily, Theme};
use hephaestus::plot::{scale, Plot, PlotComposition, PointGeom, TextGeom};
use hephaestus::scales::chrome::AxisSide;
use hephaestus::text::fetch_google_font;
use hephaestus::Renderer;

const FAMILIES: &[&str] = &["Inter", "Lobster"];

fn main() {
    for family in FAMILIES {
        let n = match fetch_google_font(family) {
            Ok(n) => n,
            Err(e) => {
                eprintln!("[google_fonts] {family}: {e}");
                return;
            }
        };
        println!("[google_fonts] {family}: registered {n} face(s)");
    }

    let (w, h) = (800u32, 500u32);
    let dpi = 96.0;
    let bg: Color = rgb8(248, 248, 252);
    let comp = || Composition::empty(1, 1).place(1, 1, Span::cell(), Patch::new("panel"));

    let labels: &[&str] = &["alpha", "beta", "gamma", "delta", "epsilon"];
    let xs: Vec<f64> = (0..labels.len())
        .map(|i| (i as f64) * 18.0 + 12.0)
        .collect();
    let ys: Vec<f64> = xs.iter().map(|x| 50.0 + 18.0 * (x * 0.06).sin()).collect();

    let mut plot = Plot::new(&comp(), "panel")
        .bind("x", "x")
        .bind("y", "y")
        .title("Google Fonts — Inter chrome, Lobster labels");
    plot.add_geom(
        PointGeom::builder()
            .set("x", xs.clone())
            .set("y", ys.clone())
            .set("fill", rgb8(220, 90, 70))
            .set("size", 8.0_f64)
            .build(),
    );
    plot.add_geom(
        TextGeom::builder()
            .set("x", xs)
            .set("y", ys)
            .set("text", labels.to_vec())
            .set("family", "Lobster")
            .set("size", 22.0_f64)
            .set("anchor_y", 1.0_f64)
            .set("y_offset", 12.0_f64)
            .set("fill", rgb8(30, 30, 30))
            .build(),
    );
    plot.add_axis(Axis::rail("x", AxisPlacement::Cartesian(AxisSide::Bottom)));
    plot.add_axis(Axis::rail("y", AxisPlacement::Cartesian(AxisSide::Left)));

    let mut theme = Theme::default();
    theme.text.font.family = Some(FontFamily::Named(vec!["Inter".to_string()]));

    let mut view = PlotComposition::new(comp())
        .add_scale("x", scale::continuous(0.0..=100.0))
        .add_scale("y", scale::continuous(0.0..=100.0))
        .with_plot(plot);
    view.set_theme(theme);

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
        .join("examples/google_fonts.png");
    hephaestus::png::write_png(&path, w, h, &pixels).expect("write png");
    println!("[google_fonts] wrote {}", path.display());
}
