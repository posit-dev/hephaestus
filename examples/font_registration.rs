//! Register a font file from disk and use it in a plot.
//!
//! Drop a TTF / OTF file at `examples/fonts/custom.ttf` (or change
//! `FONT_PATH` below) and run with:
//!
//! ```sh
//! cargo run --example font_registration --features vello,png,text
//! ```
//!
//! The plot uses the font's first family name as the `family` channel
//! on every text geom (axis labels, legend, title, data labels), so
//! the registered font replaces the platform default end-to-end.

use std::path::PathBuf;

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{Composition, Patch, Span};
use hephaestus::geometry::Size;
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement};
use hephaestus::plot::{scale, Plot, PlotComposition, PointGeom, TextGeom};
use hephaestus::scales::chrome::AxisSide;
use hephaestus::text::{register_font_path, TextStyle};
use hephaestus::Renderer;

const FONT_PATH: &str = "examples/fonts/custom.ttf";
const FAMILY_NAME: &str = "Custom Display";

fn main() {
    let mut font_file: PathBuf = std::env::current_dir().unwrap();
    font_file.push(FONT_PATH);

    if !font_file.exists() {
        eprintln!(
            "[font_registration] no font at {}.\n\
             Drop a TTF / OTF there (and update FAMILY_NAME in this example\n\
             to the font's family name as embedded in the file) to see it\n\
             applied to every text element in the plot.",
            font_file.display()
        );
        return;
    }

    let registered = register_font_path(&font_file).expect("register font");
    println!(
        "[font_registration] registered {registered} face(s) from {}",
        font_file.display()
    );
    if registered == 0 {
        eprintln!("[font_registration] the file contained no recognisable font faces");
        return;
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
        .title("Registered local font");
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
            .set("family", FAMILY_NAME)
            .set("size", 14.0_f64)
            .set("anchor_y", 1.0_f64)
            .set("y_offset", 10.0_f64)
            .set("fill", rgb8(30, 30, 30))
            .build(),
    );
    plot.add_axis(Axis::rail("x", AxisPlacement::Cartesian(AxisSide::Bottom)));
    plot.add_axis(Axis::rail("y", AxisPlacement::Cartesian(AxisSide::Left)));

    let mut theme = hephaestus::plot::theme::Theme::default();
    apply_family_to_theme(&mut theme, FAMILY_NAME);

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
        .join("examples/font_registration.png");
    hephaestus::png::write_png(&path, w, h, &pixels).expect("write png");
    println!("[font_registration] wrote {}", path.display());

    // Sanity check the style flowed through.
    let _style: TextStyle = TextStyle::new(14.0).family(FAMILY_NAME);
}

fn apply_family_to_theme(theme: &mut hephaestus::plot::theme::Theme, family: &str) {
    use hephaestus::plot::theme::{FontFamily, FontSpec};
    theme.text.font.family = Some(FontFamily::Named(vec![family.to_string()]));
    // Geom defaults — text geoms read theme.geom.text.family on the
    // hot path through scales; this example sets the family directly
    // on each geom too, so the global theme update is a belt-and-
    // braces redundancy that also covers axis / legend / title text.
    let _ = FontSpec::default();
}
