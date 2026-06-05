//! Renders every built-in shape on a grid, plus a few demo lines showing
//! arrowheads attached at line endpoints using the anchor convention.
//! Writes `examples/shapes_demo.png`.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::shape::{builtin, ShapeKind, ShapeRegistry, ShapeStyle};
use hephaestus::stroke::{Cap, Join, Stroke};
use hephaestus::{
    Affine, Brush, FillRule, PickId, Point, Rect, Renderer, SceneBuilder, Shape, Vec2,
};
use kurbo::Shape as _;

const W: u32 = 720;
const H: u32 = 640;

const GRID_COLS: usize = 6;
const GRID_ROWS: usize = 5;
const CELL_W: f64 = 110.0;
const CELL_H: f64 = 90.0;
const GRID_LEFT: f64 = 30.0;
const GRID_TOP: f64 = 30.0;

fn cell_center(idx: usize) -> Point {
    let col = idx % GRID_COLS;
    let row = idx / GRID_COLS;
    Point::new(
        GRID_LEFT + CELL_W * (col as f64 + 0.5),
        GRID_TOP + CELL_H * (row as f64 + 0.5),
    )
}

fn draw_shape_centered(
    scene: &mut impl SceneBuilder,
    shape: &Shape,
    center: Point,
    size: f64,
    brush: &Brush,
    stroke_world_width: f64,
) {
    let xform = Affine::translate(center.to_vec2()) * Affine::scale(size);
    let (paths, style) = match shape.kind() {
        ShapeKind::Paths { paths, style } => (paths, style),
        ShapeKind::Glyph { .. } => return,
    };
    match style {
        ShapeStyle::Fill => {
            for sub in paths {
                scene.fill(FillRule::NonZero, xform, brush, None, sub, PickId::Skip);
            }
        }
        ShapeStyle::Stroke => {
            let stroke = Stroke::new(stroke_world_width / size)
                .with_caps(Cap::Round)
                .with_join(Join::Round);
            for sub in paths {
                scene.stroke(&stroke, xform, brush, None, sub, PickId::Skip);
            }
        }
    }
}

fn draw_shape_attached(
    scene: &mut impl SceneBuilder,
    shape: &Shape,
    placement: Point,
    direction: Vec2,
    size: f64,
    brush: &Brush,
    stroke_world_width: f64,
) {
    let perp = Vec2::new(-direction.y, direction.x);
    let a = shape.anchor();
    let anchor_world = direction * (a.x * size) + perp * (a.y * size);
    let origin = placement - anchor_world;
    let xform = Affine::translate(origin.to_vec2())
        * Affine::rotate(direction.atan2())
        * Affine::scale(size);
    let (paths, style) = match shape.kind() {
        ShapeKind::Paths { paths, style } => (paths, style),
        ShapeKind::Glyph { .. } => return,
    };
    match style {
        ShapeStyle::Fill => {
            for sub in paths {
                scene.fill(FillRule::NonZero, xform, brush, None, sub, PickId::Skip);
            }
        }
        ShapeStyle::Stroke => {
            let stroke = Stroke::new(stroke_world_width / size)
                .with_caps(Cap::Round)
                .with_join(Join::Round);
            for sub in paths {
                scene.stroke(&stroke, xform, brush, None, sub, PickId::Skip);
            }
        }
    }
}

fn main() {
    let mut renderer = VelloRenderer::new().expect("vello renderer init");
    let registry = ShapeRegistry::with_builtins();

    let glyph_brush: Brush = rgb8(60, 130, 220).into();
    let chrome_brush: Brush = rgb8(80, 84, 96).into();
    let line_brush: Brush = rgb8(190, 195, 205).into();

    let glyph_world_stroke = 2.0;
    let line_stroke = Stroke::new(2.5).with_caps(Cap::Butt);
    let cell_stroke = Stroke::new(1.0);

    {
        let scene = renderer.scene();

        for (i, name) in builtin::NAMES.iter().enumerate() {
            let center = cell_center(i);
            let cell = Rect::new(
                center.x - CELL_W * 0.5 + 2.0,
                center.y - CELL_H * 0.5 + 2.0,
                center.x + CELL_W * 0.5 - 2.0,
                center.y + CELL_H * 0.5 - 2.0,
            )
            .to_path(0.1);
            scene.stroke(
                &cell_stroke,
                Affine::IDENTITY,
                &chrome_brush,
                None,
                &cell,
                PickId::Skip,
            );

            let shape = registry.get(name).expect("registered");
            draw_shape_centered(scene, shape, center, 28.0, &glyph_brush, glyph_world_stroke);
        }

        let demos: &[(&str, f64)] = &[
            ("arrow-closed", 18.0),
            ("arrow-stealth", 22.0),
            ("arrow-open", 18.0),
            ("arrow-feather", 28.0),
            ("arrow-dot", 14.0),
        ];
        let demo_top = GRID_TOP + (GRID_ROWS as f64) * CELL_H + 25.0;
        let demo_spacing = 28.0;
        let x_start = 60.0;
        let x_end = (W as f64) - 60.0;

        for (i, &(name, size)) in demos.iter().enumerate() {
            let y = demo_top + (i as f64) * demo_spacing;
            let start = Point::new(x_start, y);
            let end = Point::new(x_end, y);

            let direction = (end - start).normalize();
            let shape = registry.get(name).expect("registered");

            let mut line = hephaestus::Path::new();
            line.move_to(start);
            line.line_to(end);
            scene.stroke(
                &line_stroke,
                Affine::IDENTITY,
                &line_brush,
                None,
                &line,
                PickId::Skip,
            );

            draw_shape_attached(
                scene,
                shape,
                end,
                direction,
                size,
                &glyph_brush,
                glyph_world_stroke,
            );
        }
    }

    let mut pixels = vec![0u8; (W * H * 4) as usize];
    let bg: Color = rgb8(20, 22, 28);
    renderer
        .render_to_buffer(W, H, bg, &mut pixels)
        .expect("render");

    let path = std::env::current_dir()
        .unwrap()
        .join("examples/shapes_demo.png");
    hephaestus::png::write_png(&path, W, H, &pixels).expect("write png");
    println!("wrote {}", path.display());
}
