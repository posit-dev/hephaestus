//! Visual demo of the `primitives` module.
//!
//! - Top strip: end-clipped, Chaikin-rounded polyline between two nodes.
//! - Middle strip: polygon-with-hole, offset outward + Chaikin-rounded per
//!   output ring.
//! - Bottom strip: wedge & annular wedge with `round_path_corners` —
//!   curve-aware fillets at the line-to-arc corners.
//!
//! Renders to `examples/primitives_demo.png`.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::stroke::{Cap, Join, Stroke};
use hephaestus::{
    annular_wedge, clip_polyline, offset_polygon, polygon, round_corners, round_path_corners,
    wedge, Affine, Brush, CornerRounding, EndClip, FillRule, Path, PickId, Point, PolygonOptions,
    Rect, Renderer, SceneBuilder,
};
use kurbo::Shape;
use std::f64::consts::PI;

fn main() {
    let mut renderer = VelloRenderer::new().expect("vello renderer init");
    let (w, h) = (640u32, 800u32);

    {
        let scene = renderer.scene();
        let reference_brush: Brush = rgb8(80, 80, 80).into();
        let dotted = Stroke::new(1.0).with_dashes(0.0, vec![3.0, 4.0]);

        // ---------- strip 1: end-clipped, corner-rounded polyline ----------

        let node_a_center = Point::new(120.0, 140.0);
        let node_a_radius = 40.0;
        let node_b = Rect::new(440.0, 100.0, 580.0, 180.0);

        let node_brush: Brush = rgb8(60, 90, 140).into();
        scene.fill(
            FillRule::NonZero,
            Affine::IDENTITY,
            &node_brush,
            None,
            &kurbo::Circle::new(node_a_center, node_a_radius).to_path(0.1),
            PickId::Skip,
        );
        scene.fill(
            FillRule::NonZero,
            Affine::IDENTITY,
            &node_brush,
            None,
            &node_b.to_path(0.1),
            PickId::Skip,
        );

        let raw = [
            node_a_center,
            Point::new(220.0, 60.0),
            Point::new(340.0, 220.0),
            Point::new(420.0, 80.0),
            Point::new(node_b.center().x, node_b.center().y),
        ];
        let clipped = clip_polyline(
            &raw,
            Some(EndClip::Circle {
                center: node_a_center,
                radius: node_a_radius,
            }),
            Some(EndClip::Rect(node_b)),
        );
        let connector = round_corners(
            &clipped,
            false,
            CornerRounding {
                max_cut: 30.0,
                ..Default::default()
            },
        );
        let line_stroke = Stroke::new(4.0)
            .with_caps(Cap::Round)
            .with_join(Join::Round);
        let line_brush: Brush = rgb8(230, 200, 120).into();
        scene.stroke(
            &line_stroke,
            Affine::IDENTITY,
            &line_brush,
            None,
            &connector,
            PickId::Skip,
        );
        scene.stroke(
            &dotted,
            Affine::IDENTITY,
            &reference_brush,
            None,
            &mk_polyline(&raw),
            PickId::Skip,
        );

        // ---------- strip 2: polygon-with-hole, offset + rounded ----------

        let outer = [
            Point::new(120.0, 320.0),
            Point::new(520.0, 320.0),
            Point::new(520.0, 500.0),
            Point::new(120.0, 500.0),
        ];
        let hole = [
            Point::new(240.0, 370.0),
            Point::new(400.0, 370.0),
            Point::new(400.0, 450.0),
            Point::new(240.0, 450.0),
        ];
        let rings: [&[Point]; 2] = [&outer, &hole];

        let plain = polygon(&rings, PolygonOptions::default());
        scene.stroke(
            &dotted,
            Affine::IDENTITY,
            &reference_brush,
            None,
            &plain,
            PickId::Skip,
        );

        let inflated_rings = offset_polygon(&rings, 15.0, 4.0);
        let mut inflated = Path::new();
        for r in &inflated_rings {
            let sub = round_corners(r, true, CornerRounding::default());
            for el in sub.iter() {
                inflated.push(el);
            }
        }
        let fill_brush: Brush = rgb8(180, 70, 90).into();
        scene.fill(
            FillRule::NonZero,
            Affine::IDENTITY,
            &fill_brush,
            None,
            &inflated,
            PickId::Skip,
        );

        // ---------- strip 3: wedge & annular wedge with curve-aware rounding ----------

        // Left: wedge with rounded line-to-arc corners.
        let w_center = Point::new(170.0, 680.0);
        let w_radius = 90.0;
        let plain_wedge = wedge(w_center, w_radius, -PI / 2.0 - PI / 5.0, PI * 2.0 / 5.0);
        scene.stroke(
            &dotted,
            Affine::IDENTITY,
            &reference_brush,
            None,
            &plain_wedge,
            PickId::Skip,
        );
        let rounded_wedge = round_path_corners(
            &plain_wedge,
            CornerRounding {
                max_cut: 18.0,
                ..Default::default()
            },
        );
        let wedge_brush: Brush = rgb8(110, 160, 90).into();
        scene.fill(
            FillRule::NonZero,
            Affine::IDENTITY,
            &wedge_brush,
            None,
            &rounded_wedge,
            PickId::Skip,
        );

        // Right: annular wedge with all four corners rounded.
        let a_center = Point::new(450.0, 680.0);
        let plain_aw = annular_wedge(a_center, 35.0, 100.0, -PI / 2.0 - PI / 4.0, PI / 2.0);
        scene.stroke(
            &dotted,
            Affine::IDENTITY,
            &reference_brush,
            None,
            &plain_aw,
            PickId::Skip,
        );
        let rounded_aw = round_path_corners(
            &plain_aw,
            CornerRounding {
                max_cut: 14.0,
                ..Default::default()
            },
        );
        let aw_brush: Brush = rgb8(180, 130, 90).into();
        scene.fill(
            FillRule::NonZero,
            Affine::IDENTITY,
            &aw_brush,
            None,
            &rounded_aw,
            PickId::Skip,
        );
    }

    let mut pixels = vec![0u8; (w * h * 4) as usize];
    let bg: Color = rgb8(20, 22, 28);
    renderer
        .render_to_buffer(w, h, bg, &mut pixels)
        .expect("render");

    let path = std::env::current_dir()
        .unwrap()
        .join("examples/primitives_demo.png");
    hephaestus::png::write_png(&path, w, h, &pixels).expect("write png");
    println!("wrote {}", path.display());
}

fn mk_polyline(pts: &[Point]) -> Path {
    let mut p = Path::new();
    p.move_to(pts[0]);
    for v in &pts[1..] {
        p.line_to(*v);
    }
    p
}
