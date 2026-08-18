//! Demonstrates the `"markdown"` channel on [`TextGeom`]. Each label
//! carries a small snippet of marquee-flavoured markdown — inline
//! bold / italic, hex-colour spans, sup/sub, and inline code — so the
//! same geom binding switches from plain labels to richly styled ones
//! by flipping a single boolean.
//!
//! Renders two panels side-by-side:
//!
//! - Left: the labels as literal strings (plain path).
//! - Right: the same labels interpreted as markdown (rich path).
//!
//! Writes `examples/rich_text_inline.png`.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{Composition, Patch, Span};
use hephaestus::geometry::Size;
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement};
use hephaestus::plot::{scale, Plot, PlotComposition, PointGeom, TextGeom};
use hephaestus::scales::chrome::AxisSide;
use hephaestus::scene::SceneBuilder;
use hephaestus::Renderer;

fn main() {
    let (w, h) = (1200u32, 500u32);
    let dpi = 96.0;
    let bg: Color = rgb8(250, 250, 253);

    let comp = || {
        Composition::empty(1, 2)
            .place(1, 1, Span::cell(), Patch::new("plain"))
            .place(1, 2, Span::cell(), Patch::new("rich"))
    };

    // Labels that mix bold, italic, coloured spans, and inline code —
    // the vocabulary the rich-text pipeline handles.
    //
    // Note: pulldown-cmark's `~sub~` / `^sup^` grammar requires
    // whitespace around the outer markers, so `H~2~O` and `E=mc^2^`
    // don't trigger sub/sup. Use a leading marker with surrounding
    // whitespace (e.g. `x ^2^`) or the marquee-parity example for
    // that vocabulary.
    let xs: Vec<f64> = vec![15.0, 40.0, 60.0, 80.0];
    let ys: Vec<f64> = vec![75.0, 50.0, 60.0, 30.0];
    let labels: Vec<&str> = vec![
        "**bold** and *italic*",
        "colour: {.crimson red}",
        "hex: {#3369e8 blue}",
        "inline `code`",
    ];
    let point_fill: Color = rgb8(88, 106, 195);

    let mut plain = Plot::new(&comp(), "plain")
        .bind("x", "x")
        .bind("y", "y")
        .title("Plain labels");
    plain.add_geom(
        PointGeom::builder()
            .set("x", xs.clone())
            .set("y", ys.clone())
            .set("fill", point_fill)
            .set("size", 12.0_f64)
            .build(),
    );
    plain.add_geom(
        TextGeom::builder()
            .set("x", xs.clone())
            .set("y", ys.clone())
            .set("text", labels.clone())
            .set("size", 14.0_f64)
            .set("y_offset", -12.0_f64)
            .set("anchor_y", 1.0_f64)
            .build(),
    );

    let mut rich = Plot::new(&comp(), "rich")
        .bind("x", "x")
        .bind("y", "y")
        .title("markdown = true");
    rich.add_geom(
        PointGeom::builder()
            .set("x", xs.clone())
            .set("y", ys.clone())
            .set("fill", point_fill)
            .set("size", 12.0_f64)
            .build(),
    );
    rich.add_geom(
        TextGeom::builder()
            .set("x", xs)
            .set("y", ys)
            .set("text", labels)
            .set("size", 14.0_f64)
            .set("y_offset", -12.0_f64)
            .set("anchor_y", 1.0_f64)
            .set("markdown", true)
            .build(),
    );

    for p in [&mut plain, &mut rich] {
        p.add_axis(Axis::rail("x", AxisPlacement::Cartesian(AxisSide::Bottom)).title("x"));
        p.add_axis(Axis::rail("y", AxisPlacement::Cartesian(AxisSide::Left)).title("y"));
    }

    let mut view = PlotComposition::new(&comp())
        .add_scale("x", scale::continuous(0.0..=100.0))
        .add_scale("y", scale::continuous(0.0..=100.0))
        .with_plot(plain)
        .with_plot(rich);

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
        .join("examples/rich_text_inline.png");
    hephaestus::image::write_png(&path, w, h, &pixels).expect("write png");
    println!("wrote {}", path.display());
}
