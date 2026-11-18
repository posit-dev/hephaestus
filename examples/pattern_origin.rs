//! Pattern origin: scene-shared versus per-shape. The same crosshatch
//! fills three staggered hexagons twice — once with the default
//! (`brush_transform: None`), where every shape samples the one
//! scene-wide tiling and each starts mid-tile, and once with
//! `brush_transform` translating the tiling to the shape's own bounding
//! box, so each fill begins a fresh tile at its corner.
//!
//! This is a scene-level example because the plot layer passes
//! `brush_transform: None` everywhere; the hook is `SceneBuilder::fill`'s
//! own argument.
//!
//! ```sh
//! cargo run --example pattern_origin --features svg
//! ```

use hephaestus::backend::svg::{write_svg, SvgConfig, SvgScene};
use hephaestus::backend::vello::VelloRenderer;
use hephaestus::brush::{fill_pattern, Brush};
use hephaestus::color::{rgb8, Color};
use hephaestus::geometry::{Affine, Point, Size};
use hephaestus::image::PngCompression;
use hephaestus::path::{bounding_box, FillRule};
use hephaestus::pick::PickId;
use hephaestus::scene::SceneBuilder;
use hephaestus::Renderer;
use std::sync::Arc;

fn main() {
    let (w, h) = (800u32, 320u32);
    let dpi = 96.0;
    let size = Size::new(w as f64, h as f64);

    let mut pattern = fill_pattern("crosshatch", Brush::Solid(rgb8(200, 90, 60))).unwrap();
    pattern.transform *= Affine::scale(24.0);
    let brush = Brush::Pattern(Arc::new(pattern));

    // Staggered so a shared origin visibly cuts tiles at shape edges.
    let shapes: Vec<_> = [
        Point::new(100.0, 160.0),
        Point::new(225.0, 128.0),
        Point::new(335.0, 180.0),
    ]
    .iter()
    .map(|&c| hephaestus::primitives::regular_polygon(c, 60.0, 6))
    .collect();

    let ink = Brush::Solid(rgb8(60, 60, 60));
    let draw = |scene: &mut dyn SceneBuilder, per_shape: bool| {
        for (i, shape) in shapes.iter().enumerate() {
            // The right-hand panel is the same shapes shifted over.
            let xform = Affine::translate((if per_shape { 400.0 } else { 0.0 }, 0.0));
            let anchor = per_shape.then(|| {
                let b = bounding_box(shape);
                Affine::translate((b.x0, b.y0))
            });
            scene.fill(
                FillRule::NonZero,
                xform,
                &brush,
                anchor,
                shape,
                PickId::Skip,
            );
            let stroke = hephaestus::stroke::Stroke::new(1.5);
            scene.stroke(&stroke, xform, &ink, None, shape, PickId::Skip);
            let _ = i;
        }
    };

    // Raster.
    let mut renderer = VelloRenderer::new().expect("vello renderer init");
    {
        let scene = renderer.scene();
        scene.clear();
        draw(scene, false);
        draw(scene, true);
    }
    let mut pixels = vec![0u8; (w * h * 4) as usize];
    renderer
        .render_to_buffer(w, h, Color::WHITE, &mut pixels)
        .expect("render");
    hephaestus::image::write_png(
        "examples/pattern_origin.png",
        w,
        h,
        &pixels,
        PngCompression::Balanced,
        Some(dpi),
    )
    .expect("write png");

    // Vector.
    let mut svg = SvgScene::with_config(size, dpi, SvgConfig::default());
    draw(&mut svg, false);
    draw(&mut svg, true);
    write_svg("examples/pattern_origin.svg", &svg).expect("write svg");
    if !svg.warnings().is_empty() {
        eprintln!("svg warnings: {:?}", svg.warnings());
    }
    println!("wrote examples/pattern_origin.png (left: scene origin, right: per-shape) and .svg");
}
