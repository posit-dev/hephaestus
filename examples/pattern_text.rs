//! Text-based pattern fills: bars tiled with a text stamp. Text elements
//! are shaped when the tile is built, so the PNG shows the glyphs like any
//! other text; the SVG keeps them as real `<text>` elements naming their
//! family, editable after export. The PDF backend skips text elements (a
//! tile has no font context) and reports `UnsupportedPatternElement`.
//!
//! ```sh
//! cargo run --example pattern_text --features svg
//! ```

use hephaestus::backend::svg::{write_svg, SvgConfig, SvgScene};
use hephaestus::backend::vello::VelloRenderer;
use hephaestus::brush::{Brush, FillPattern, PatternElement, PatternPaint};
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{beside, Patch};
use hephaestus::geometry::{Affine, Point, Rect, Size, Vec2};
use hephaestus::image::PngCompression;
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement};
use hephaestus::plot::{Plot, PlotComposition, Raw, RectGeom};
use hephaestus::primitives;
use hephaestus::scales::chrome::AxisSide;
use hephaestus::scene::SceneBuilder;
use hephaestus::Renderer;
use std::sync::Arc;

/// A pattern tiling one text stamp, on a white backdrop.
fn text_pattern(text: &str, color: Color) -> FillPattern {
    FillPattern::new(
        vec![
            PatternElement::Path {
                path: primitives::rect(Rect::new(0.0, 0.0, 1.0, 1.0)),
                fill: Some(PatternPaint::Fixed(Brush::Solid(Color::WHITE))),
                stroke: None,
            },
            PatternElement::Text {
                content: Arc::from(text),
                at: Point::new(0.08, 0.8),
                size: 0.7,
                family: None,
                paint: PatternPaint::Fixed(Brush::Solid(color)),
            },
        ],
        Rect::new(0.0, 0.0, 1.0, 1.0),
        // Wider than tall, so the stamp reads left to right with room
        // between repetitions.
        Vec2::new(1.6, 1.0),
    )
    .with_transform(Affine::scale(36.0))
}

fn main() {
    let (w, h) = (800u32, 400u32);
    let dpi = 96.0;
    let size = Size::new(w as f64, h as f64);

    let comp = beside(Patch::new("ab"), Patch::new("pct"));
    let xs: Vec<f64> = (0..8).map(|i| i as f64).collect();
    let ys: Vec<f64> = xs
        .iter()
        .map(|x| 2.0 + 1.5 * (x * 0.9).sin().abs())
        .collect();

    let panels = [
        ("ab", "\"ab\" stamp", text_pattern("ab", rgb8(60, 100, 200))),
        ("pct", "\"%\" stamp", text_pattern("%", rgb8(200, 90, 60))),
    ];
    let mut view = PlotComposition::new(&comp)
        .add_scale("cat", hephaestus::plot::scale::continuous(-0.5..=8.0))
        .add_scale("value", hephaestus::plot::scale::continuous(0.0..=4.0));
    for (name, title, pattern) in panels {
        let mut plot = Plot::new(&comp, name)
            .bind("x", "cat")
            .bind("x2", "cat")
            .bind("y", "value")
            .bind("y2", "value")
            .subtitle(title);
        plot.add_geom(
            RectGeom::builder()
                .set("x", xs.clone())
                .set("x2", xs.iter().map(|x| x + 0.8).collect::<Vec<_>>())
                .set("y", vec![0.0_f64; 8])
                .set("y2", ys.clone())
                .set("fill", Raw(Brush::Pattern(Arc::new(pattern))))
                .build(),
        );
        for a in [
            Axis::rail("cat", AxisPlacement::Cartesian(AxisSide::Bottom)),
            Axis::rail("value", AxisPlacement::Cartesian(AxisSide::Left)),
        ] {
            plot.add_axis(a);
        }
        view = view.with_plot(plot);
    }

    // Vector: text stays text.
    let mut svg = SvgScene::with_config(size, dpi, SvgConfig::default());
    view.render(&mut svg, size, dpi);
    write_svg("examples/pattern_text.svg", &svg).expect("write svg");
    if !svg.warnings().is_empty() {
        eprintln!("svg warnings: {:?}", svg.warnings());
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
        "examples/pattern_text.png",
        w,
        h,
        &pixels,
        PngCompression::Balanced,
        Some(dpi),
    )
    .expect("write png");
    println!("wrote examples/pattern_text.png and examples/pattern_text.svg");
}
