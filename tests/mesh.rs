//! Vello smoke tests for `SceneBuilder::draw_mesh`.
//!
//! Validates the per-triangle linear-gradient decomposition: the
//! backend's `draw_mesh` impl picks the max-colour-distance pair as
//! the gradient axis and emits one `fill` per triangle. We render a
//! single colourful triangle and sample interior pixels to confirm
//! the gradient blends across the triangle.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::mesh::Mesh;
use hephaestus::{Affine, PickId, Point, Renderer, SceneBuilder};

const W: u32 = 200;
const H: u32 = 200;

#[test]
fn draw_mesh_renders_single_triangle_with_per_vertex_colors() {
    let mut r = VelloRenderer::new().expect("vello renderer init");
    let mesh = Mesh::new(
        vec![
            Point::new(20.0, 180.0),  // bottom-left
            Point::new(180.0, 180.0), // bottom-right
            Point::new(100.0, 20.0),  // top
        ],
        vec![
            Color::new([1.0, 0.0, 0.0, 1.0]), // red at bottom-left
            Color::new([0.0, 0.0, 1.0, 1.0]), // blue at bottom-right
            Color::new([0.0, 1.0, 0.0, 1.0]), // green at top
        ],
        vec![0, 1, 2],
    );
    {
        let scene = r.scene();
        scene.clear();
        scene.draw_mesh(&mesh, Affine::IDENTITY, PickId::Skip);
    }
    let mut buf = vec![0u8; (W * H * 4) as usize];
    let bg = rgb8(255, 255, 255);
    r.render_to_buffer(W, H, bg, &mut buf).expect("render");

    // Sample a pixel near the bottom-left vertex: should be dominated
    // by red (the gradient axis runs to the max-colour-distance
    // vertex; the bottom-left's colour is red so the closer the
    // sample sits to BL the redder it gets).
    let sample = |x: u32, y: u32| {
        let i = ((y * W + x) * 4) as usize;
        (buf[i], buf[i + 1], buf[i + 2], buf[i + 3])
    };
    let bg_white = (255u8, 255u8, 255u8);
    // Interior point near bottom-left (red-dominated):
    let (r1, g1, b1, _) = sample(35, 170);
    assert_ne!((r1, g1, b1), bg_white, "interior pixel must not be bg");
    assert!(
        r1 > g1 && r1 > b1,
        "near-BL: expected red-dominated, got ({r1}, {g1}, {b1})"
    );

    // Interior point near bottom-right (blue-dominated):
    let (r2, g2, b2, _) = sample(165, 170);
    assert_ne!((r2, g2, b2), bg_white);
    assert!(
        b2 > r2 && b2 > g2,
        "near-BR: expected blue-dominated, got ({r2}, {g2}, {b2})"
    );

    // Pixel outside the triangle (top corner of the image): should be
    // background.
    let (r3, g3, b3, _) = sample(10, 10);
    assert_eq!(
        (r3, g3, b3),
        bg_white,
        "outside triangle should be background"
    );
}

#[test]
fn draw_mesh_uniform_color_renders_solid() {
    // All three vertices the same colour → backend should pick the
    // solid-brush shortcut. The whole triangle should render in that
    // colour (within drift tolerance).
    let mut r = VelloRenderer::new().expect("vello renderer init");
    let red = Color::new([1.0, 0.0, 0.0, 1.0]);
    let mesh = Mesh::new(
        vec![
            Point::new(40.0, 160.0),
            Point::new(160.0, 160.0),
            Point::new(100.0, 40.0),
        ],
        vec![red; 3],
        vec![0, 1, 2],
    );
    {
        let scene = r.scene();
        scene.clear();
        scene.draw_mesh(&mesh, Affine::IDENTITY, PickId::Skip);
    }
    let mut buf = vec![0u8; (W * H * 4) as usize];
    r.render_to_buffer(W, H, rgb8(0, 0, 0), &mut buf)
        .expect("render");
    let i = ((100 * W + 100) * 4) as usize;
    let (r8, g8, b8) = (buf[i], buf[i + 1], buf[i + 2]);
    assert!(
        r8 > 240 && g8 < 16 && b8 < 16,
        "centre = ({r8}, {g8}, {b8})"
    );
}

#[test]
fn draw_mesh_pick_round_trip() {
    let mut r = VelloRenderer::with_picking().expect("vello renderer init");
    let mesh = Mesh::new(
        vec![
            Point::new(40.0, 160.0),
            Point::new(160.0, 160.0),
            Point::new(100.0, 40.0),
        ],
        vec![Color::new([1.0, 0.0, 0.0, 1.0]); 3],
        vec![0, 1, 2],
    );
    {
        let scene = r.scene();
        scene.clear();
        scene.draw_mesh(&mesh, Affine::IDENTITY, PickId::Id(42));
    }
    let mut buf = vec![0u8; (W * H * 4) as usize];
    r.render_to_buffer(W, H, rgb8(0, 0, 0), &mut buf)
        .expect("render");
    assert_eq!(r.pick_at(100, 100), Some(42));
    // Outside the triangle: no hit.
    assert_eq!(r.pick_at(5, 5), None);
}
