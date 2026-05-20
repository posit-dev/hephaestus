//! Draws a few primitives through the SceneBuilder abstraction to validate the
//! Vello backend end-to-end. Writes `examples/hello.png` in the project root.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::brush::Gradient;
use hephaestus::color::{rgb, rgb8, rgba, Color};
use hephaestus::stroke::{Cap, Join, Stroke};
use hephaestus::{
    Affine, BlendMode, Brush, Compose, FillRule, Mix, Path, Point, Rect, Renderer, SceneBuilder,
};
use kurbo::Shape;

fn main() {
    let mut renderer = VelloRenderer::new().expect("vello renderer init");
    let (w, h) = (512u32, 512u32);

    {
        let scene = renderer.scene();

        // 1. Filled rectangle (solid brush)
        let rect = Rect::new(40.0, 40.0, 240.0, 200.0).to_path(0.1);
        let solid: Brush = rgb8(60, 120, 200).into();
        scene.fill(FillRule::NonZero, Affine::IDENTITY, &solid, None, &rect);

        // 2. Stroked open polyline with round caps/joins
        let mut poly = Path::new();
        poly.move_to(Point::new(60.0, 300.0));
        poly.line_to(Point::new(140.0, 360.0));
        poly.line_to(Point::new(200.0, 280.0));
        poly.line_to(Point::new(260.0, 380.0));
        let stroke = Stroke::new(8.0)
            .with_caps(Cap::Round)
            .with_join(Join::Round);
        let line_brush: Brush = rgb8(220, 220, 220).into();
        scene.stroke(&stroke, Affine::IDENTITY, &line_brush, None, &poly);

        // 3. Gradient-filled circle
        let circle = kurbo::Circle::new(Point::new(380.0, 150.0), 90.0).to_path(0.1);
        let gradient = Gradient::new_linear(Point::new(290.0, 60.0), Point::new(470.0, 240.0))
            .with_stops([rgb(0.95, 0.55, 0.15), rgb(0.15, 0.05, 0.45)].as_slice());
        let grad_brush: Brush = gradient.into();
        scene.fill(
            FillRule::NonZero,
            Affine::IDENTITY,
            &grad_brush,
            None,
            &circle,
        );

        // 4. Multiply layer with a translucent square overlapping the rect
        let layer_clip = Rect::new(0.0, 0.0, w as f64, h as f64).to_path(0.1);
        scene.push_layer(
            BlendMode::new(Mix::Multiply, Compose::SrcOver),
            1.0,
            Affine::IDENTITY,
            &layer_clip,
        );
        let overlap = Rect::new(160.0, 120.0, 360.0, 320.0).to_path(0.1);
        let overlap_brush: Brush = rgba(1.0, 0.85, 0.2, 0.7).into();
        scene.fill(
            FillRule::NonZero,
            Affine::IDENTITY,
            &overlap_brush,
            None,
            &overlap,
        );
        scene.pop_layer();
    }

    let mut pixels = vec![0u8; (w * h * 4) as usize];
    let bg: Color = rgb8(20, 22, 28);
    renderer
        .render_to_buffer(w, h, bg, &mut pixels)
        .expect("render");

    let path = std::env::current_dir().unwrap().join("examples/hello.png");
    hephaestus::png::write_png(&path, w, h, &pixels).expect("write png");
    println!("wrote {}", path.display());
}
