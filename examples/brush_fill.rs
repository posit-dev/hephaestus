//! Fill channels that are more than a color: a bar series filled with a
//! linear gradient, one filled per category with an image pattern, and a
//! scatter filled with a radial gradient.
//!
//! Exported as SVG and PNG from one composition, so the vector pattern /
//! gradient definitions can be compared against the rasteriser's output
//! side by side.
//!
//! ```sh
//! cargo run --example brush_fill --features svg
//! ```

use hephaestus::backend::svg::{write_svg, SvgConfig, SvgScene};
use hephaestus::backend::vello::VelloRenderer;
use hephaestus::brush::{Brush, Extend, Gradient, ImageBrush};
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{beside, Patch};
use hephaestus::geometry::Size;
use hephaestus::image::{from_rgba8, PngCompression};
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement};
use hephaestus::plot::{scale, Plot, PlotComposition, PointGeom, Raw, RectGeom};
use hephaestus::scales::chrome::AxisSide;
use hephaestus::scene::SceneBuilder;
use hephaestus::Renderer;

/// A 4×4 two-color checkerboard, tiled.
fn checker(a: [u8; 3], b: [u8; 3]) -> ImageBrush {
    let (w, h) = (4u32, 4u32);
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let c = if (x + y) % 2 == 0 { a } else { b };
            px.extend_from_slice(&[c[0], c[1], c[2], 255]);
        }
    }
    ImageBrush::new(from_rgba8(w, h, px).expect("image")).with_extend(Extend::Repeat)
}

fn main() {
    let (w, h) = (1200u32, 400u32);
    let dpi = 96.0;
    let size = Size::new(w as f64, h as f64);

    let comp = || {
        beside(
            beside(Patch::new("gradient"), Patch::new("pattern")),
            Patch::new("points"),
        )
    };
    let axis_pair = |x: &str, y: &str| {
        [
            Axis::rail(x, AxisPlacement::Cartesian(AxisSide::Bottom)),
            Axis::rail(y, AxisPlacement::Cartesian(AxisSide::Left)),
        ]
    };

    // Bars filled with one constant gradient. The gradient's coordinate
    // space is the scene's, so the ramp runs across each bar's height.
    let xs: Vec<f64> = (0..8).map(|i| i as f64).collect();
    let ys: Vec<f64> = xs
        .iter()
        .map(|x| 2.0 + 1.5 * (x * 0.9).sin().abs())
        .collect();
    let mut plot_gradient = Plot::new(&comp(), "gradient")
        .bind("x", "cat")
        .bind("x2", "cat")
        .bind("y", "value")
        .bind("y2", "value")
        .subtitle("Constant gradient fill");
    plot_gradient.add_geom(
        RectGeom::builder()
            .set("x", xs.clone())
            .set("x2", xs.iter().map(|x| x + 0.8).collect::<Vec<_>>())
            .set("y", vec![0.0_f64; 8])
            .set("y2", ys.clone())
            .set(
                "fill",
                Gradient::new_linear((0.0, 340.0), (0.0, 60.0))
                    .with_stops([rgb8(60, 100, 200), rgb8(150, 210, 240)]),
            )
            .build(),
    );
    for a in axis_pair("cat", "value") {
        plot_gradient.add_axis(a);
    }

    // Bars filled per category with an image pattern, through a raw
    // per-row brush column: no scale involved, one tile per category.
    let mut plot_pattern = Plot::new(&comp(), "pattern")
        .bind("x", "cat")
        .bind("x2", "cat")
        .bind("y", "value")
        .bind("y2", "value")
        .subtitle("Per-category pattern fills");
    let tiles = [
        Brush::Image(checker([220, 90, 70], [250, 220, 210])),
        Brush::Image(checker([70, 120, 220], [205, 225, 250])),
    ];
    plot_pattern.add_geom(
        RectGeom::builder()
            .set("x", xs.clone())
            .set("x2", xs.iter().map(|x| x + 0.8).collect::<Vec<_>>())
            .set("y", vec![0.0_f64; 8])
            .set("y2", ys.clone())
            .set(
                "fill",
                Raw(xs
                    .iter()
                    .map(|x| tiles[(*x as usize) % 2].clone())
                    .collect::<Vec<_>>()),
            )
            .build(),
    );
    for a in axis_pair("cat", "value") {
        plot_pattern.add_axis(a);
    }

    // Points filled with a radial gradient — the fill channel on marker
    // shapes accepts brushes too, glyph markers included.
    let px: Vec<f64> = (0..30).map(|i| i as f64 * 0.25).collect();
    let py: Vec<f64> = px.iter().map(|x| 2.0 + 1.2 * (x * 1.3).cos()).collect();
    let mut plot_points = Plot::new(&comp(), "points")
        .bind("x", "t")
        .bind("y", "v")
        .subtitle("Radial gradient markers");
    plot_points.add_geom(
        PointGeom::builder()
            .set("x", px)
            .set("y", py)
            .set("size", 10.0_f64)
            .set(
                "fill",
                Gradient::new_two_point_radial((0.0, 0.0), 0.0, (0.0, 0.0), 8.0)
                    .with_stops([rgb8(255, 240, 200), rgb8(210, 90, 60)]),
            )
            .build(),
    );
    for a in axis_pair("t", "v") {
        plot_points.add_axis(a);
    }

    let mut view = PlotComposition::new(&comp())
        .add_scale("cat", scale::continuous(-0.5..=8.0))
        .add_scale("value", scale::continuous(0.0..=4.0))
        .add_scale("t", scale::continuous(0.0..=7.5))
        .add_scale("v", scale::continuous(0.0..=3.5))
        .with_plot(plot_gradient)
        .with_plot(plot_pattern)
        .with_plot(plot_points);

    // Vector.
    let mut svg = SvgScene::with_config(size, dpi, SvgConfig::new().background(Some(Color::WHITE)));
    view.render(&mut svg, size, dpi);
    write_svg("examples/brush_fill.svg", &svg).expect("write svg");
    let warnings = svg.warnings();
    if warnings.is_empty() {
        println!("examples/brush_fill.svg — no degradations");
    } else {
        println!("examples/brush_fill.svg — {warnings:?}");
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
        "examples/brush_fill.png",
        w,
        h,
        &pixels,
        PngCompression::Balanced,
        Some(dpi),
    )
    .expect("write png");
}
