//! End-to-end visual sanity for the cross-geom `"angle"` channel.
//! Two renders:
//!
//! - `rotation_1_per_row.png` — every per-row geom (Point, Rect,
//!   Ellipse, Segment, Wedge, Text) drawn at four angle values per
//!   geom on a single panel. Each geom occupies one horizontal row;
//!   the four columns are angles `0`, `π/6`, `π/4`, and `π/2`.
//!   Convention: positive radians = visible counter-clockwise rotation
//!   (mathematical CCW in the user's frame; geoms flip internally for
//!   the screen y-down coordinate system).
//!
//! - `rotation_2_per_mark.png` — LineGeom and PolygonGeom, with each
//!   mark drawn at a different `"angle"` value. Demonstrates per-mark
//!   resolution (first-row-of-mark) and the centroid pivot — the
//!   shapes rotate around their own centroids, holes stay with their
//!   outer ring.

use std::f64::consts::{FRAC_PI_2, FRAC_PI_4, FRAC_PI_6, PI};
use std::sync::Arc;

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{Composition, Patch, Span};
use hephaestus::geometry::Size;
use hephaestus::plot::value::Value;
use hephaestus::plot::{
    scale, EllipseGeom, LineGeom, Plot, PlotComposition, PointGeom, PolygonGeom, RectGeom,
    SegmentGeom, TextGeom, WedgeGeom,
};
use hephaestus::Renderer;

const ANGLES: [f64; 4] = [0.0, FRAC_PI_6, FRAC_PI_4, FRAC_PI_2];
const ANGLE_LABELS: [&str; 4] = ["0", "π/6", "π/4", "π/2"];

fn main() {
    let (w, h) = (1200u32, 720u32);
    let dpi = 96.0;

    let comp = || Composition::empty(1, 1).place(1, 1, Span::cell(), Patch::new("panel"));

    let mut renderer = VelloRenderer::new().expect("vello renderer init");
    let bg: Color = rgb8(248, 248, 252);

    render_per_row(&mut renderer, &comp, w, h, dpi, bg);
    render_per_mark(&mut renderer, &comp, w, h, dpi, bg);
}

/// Render 1: every per-row geom × four angle values, one geom per row.
fn render_per_row(
    renderer: &mut VelloRenderer,
    comp: &impl Fn() -> Composition,
    w: u32,
    h: u32,
    dpi: f64,
    bg: Color,
) {
    let ink = rgb8(40, 50, 70);
    let fill_red = Color::new([0.85, 0.4, 0.4, 1.0]);
    let fill_blue = Color::new([0.4, 0.55, 0.85, 0.8]);
    let fill_gold = Color::new([0.85, 0.7, 0.3, 1.0]);

    // x positions: 4 evenly-spaced columns spanning the panel.
    let xs: Vec<f64> = (0..4).map(|i| 0.2 + 0.2 * (i as f64)).collect();
    let angles: Vec<f64> = ANGLES.to_vec();

    // y bands: 6 geoms stacked top → bottom.
    let row_ys: [(f64, &str); 6] = [
        (0.92, "Point"),
        (0.78, "Rect"),
        (0.64, "Ellipse"),
        (0.50, "Segment"),
        (0.34, "Wedge"),
        (0.16, "Text"),
    ];

    let mut plot = Plot::new(&comp(), "panel")
        .title("Cross-geom rotation — math CCW (positive = visible counter-clockwise)")
        .bind("x", "x_axis")
        .bind("y", "y_axis");

    // Row 0: PointGeom — rotate a triangle glyph so the rotation is
    // visible (a circle would look identical at every angle).
    plot.add_geom(
        PointGeom::builder()
            .set("x", xs.clone())
            .set("y", vec![row_ys[0].0; 4])
            .set("fill", fill_red)
            .set("size", 36.0_f64)
            .set("shape", "triangle-up")
            .set("angle", angles.clone())
            .build(),
    );

    // Row 1: RectGeom — corners offset by ±0.04 around (x, y_row).
    plot.add_geom(
        RectGeom::builder()
            .set("x", xs.clone())
            .set("x2", xs.clone())
            .set("y", vec![row_ys[1].0 - 0.04; 4])
            .set("y2", vec![row_ys[1].0 + 0.04; 4])
            .set("x_band", vec![0.0_f64; 4]) // overrides the default -0.5
            .set("x2_band", vec![0.0_f64; 4]) // overrides the default +0.5
            .set("x_offset", vec![-20.0_f64; 4])
            .set("x2_offset", vec![20.0_f64; 4])
            .set("fill", fill_blue)
            .set("stroke", ink)
            .set("linewidth", 1.5_f64)
            .set("angle", angles.clone())
            .build(),
    );

    // Row 2: EllipseGeom — rx = 24pt, ry = 10pt (anisotropic so rotation shows).
    plot.add_geom(
        EllipseGeom::builder()
            .set("x", xs.clone())
            .set("x2", xs.clone())
            .set("y", vec![row_ys[2].0; 4])
            .set("y2", vec![row_ys[2].0; 4])
            .set("x2_offset", vec![24.0_f64; 4])
            .set("y2_offset", vec![10.0_f64; 4])
            .set("fill", fill_gold)
            .set("stroke", ink)
            .set("linewidth", 1.5_f64)
            .set("angle", angles.clone())
            .build(),
    );

    // Row 3: SegmentGeom — horizontal segments at angle = 0; rotate.
    plot.add_geom(
        SegmentGeom::builder()
            .set("x", xs.clone())
            .set("x2", xs.clone())
            .set("y", vec![row_ys[3].0; 4])
            .set("y2", vec![row_ys[3].0; 4])
            .set("x_offset", vec![-25.0_f64; 4])
            .set("x2_offset", vec![25.0_f64; 4])
            .set("stroke", ink)
            .set("linewidth", 2.5_f64)
            .set("cap", "round")
            .set("angle", angles.clone())
            .build(),
    );

    // Row 4: WedgeGeom — quarter wedge (theta = 0, theta2 = π/2) at
    // every column; `angle` rotates the whole wedge around its centre.
    plot.add_geom(
        WedgeGeom::builder()
            .set("x", xs.clone())
            .set("y", vec![row_ys[4].0; 4])
            .set("radius", vec![30.0_f64; 4])
            .set("theta", vec![0.0_f64; 4])
            .set("theta2", vec![FRAC_PI_2; 4])
            .set("fill", fill_blue)
            .set("stroke", ink)
            .set("linewidth", 1.2_f64)
            .set("angle", angles.clone())
            .build(),
    );

    // Row 5: TextGeom — rotate "abc" labels around the alignment anchor.
    plot.add_geom(
        TextGeom::builder()
            .set("x", xs.clone())
            .set("y", vec![row_ys[5].0; 4])
            .set("text", vec!["abc", "abc", "abc", "abc"])
            .set("size", 24.0_f64)
            .set("weight", 600.0_f64)
            .set("fill", ink)
            .set("anchor_x", 0.5_f64)
            .set("anchor_y", 0.5_f64)
            .set("angle", angles.clone())
            .build(),
    );

    // Column labels along the top: angle names.
    plot.add_geom(
        TextGeom::builder()
            .set("x", xs.clone())
            .set("y", vec![0.99_f64; 4])
            .set("text", ANGLE_LABELS.to_vec())
            .set("size", 11.0_f64)
            .set("weight", 400.0_f64)
            .set("fill", rgb8(120, 130, 150))
            .set("anchor_x", 0.5_f64)
            .set("anchor_y", 1.0_f64)
            .build(),
    );

    // Row labels along the left edge: geom names.
    let row_label_x: Vec<f64> = (0..6).map(|_| 0.03).collect();
    let row_label_y: Vec<f64> = row_ys.iter().map(|(y, _)| *y).collect();
    plot.add_geom(
        TextGeom::builder()
            .set("x", row_label_x)
            .set("y", row_label_y)
            .set(
                "text",
                row_ys.iter().map(|(_, name)| *name).collect::<Vec<_>>(),
            )
            .set("size", 13.0_f64)
            .set("weight", 600.0_f64)
            .set("fill", rgb8(80, 90, 110))
            .set("anchor_x", 0.0_f64)
            .set("anchor_y", 0.5_f64)
            .build(),
    );

    let mut view = PlotComposition::new(comp())
        .add_scale("x_axis", scale::continuous(0.0..=1.0))
        .add_scale("y_axis", scale::continuous(0.0..=1.0))
        .with_plot(plot);
    render_to(
        renderer,
        &mut view,
        w,
        h,
        dpi,
        bg,
        "examples/rotation_1_per_row.png",
    );
}

/// Render 2: per-mark rotation on LineGeom (zig-zags) and PolygonGeom
/// (L-shapes with holes). Each mark gets its own angle value resolved
/// from the mark's first row.
fn render_per_mark(
    renderer: &mut VelloRenderer,
    comp: &impl Fn() -> Composition,
    w: u32,
    h: u32,
    dpi: f64,
    bg: Color,
) {
    let ink = rgb8(40, 50, 70);
    let fill_red = Color::new([0.85, 0.4, 0.4, 0.85]);
    let fill_blue = Color::new([0.4, 0.55, 0.85, 0.85]);

    // 4 line marks — each a zig-zag of 5 vertices around its centre.
    // Centres at (0.2, 0.7), (0.4, 0.7), (0.6, 0.7), (0.8, 0.7).
    // Vertices in local frame: (-1, 0), (-0.5, 0.5), (0, -0.5), (0.5, 0.5), (1, 0).
    // Scale by 0.06 panel-fraction so each zig-zag fits within ~12% of the panel width.
    let line_centres = [(0.2_f64, 0.7), (0.4, 0.7), (0.6, 0.7), (0.8, 0.7)];
    let line_angles = [0.0, FRAC_PI_6, FRAC_PI_4, PI / 2.0];

    let local_zigzag: [(f64, f64); 5] = [
        (-1.0, 0.0),
        (-0.5, 0.5),
        (0.0, -0.5),
        (0.5, 0.5),
        (1.0, 0.0),
    ];

    let mut line_xs: Vec<f64> = Vec::new();
    let mut line_ys: Vec<f64> = Vec::new();
    let mut line_keys: Vec<i64> = Vec::new();
    let mut line_angle_col: Vec<f64> = Vec::new();
    let mut line_stroke_col: Vec<Color> = Vec::new();
    let mark_colors = [
        Color::new([0.85, 0.4, 0.4, 1.0]),
        Color::new([0.4, 0.7, 0.4, 1.0]),
        Color::new([0.4, 0.55, 0.85, 1.0]),
        Color::new([0.85, 0.7, 0.3, 1.0]),
    ];
    for (m, ((cx, cy), angle)) in line_centres.iter().zip(line_angles.iter()).enumerate() {
        for (lx, ly) in &local_zigzag {
            line_xs.push(cx + lx * 0.06);
            line_ys.push(cy + ly * 0.06);
            line_keys.push(m as i64);
            line_angle_col.push(*angle);
            line_stroke_col.push(mark_colors[m]);
        }
    }

    // 4 polygon marks — each an "L" with a square hole, drawn around
    // its centre. Outer L vertices + a square hole.
    let poly_centres = [(0.2_f64, 0.3), (0.4, 0.3), (0.6, 0.3), (0.8, 0.3)];
    let poly_angles = [0.0, FRAC_PI_6, FRAC_PI_4, FRAC_PI_2];

    // L-shape outer ring (6 vertices), CCW. Local frame, half-size ~1.
    let local_outer: [(f64, f64); 6] = [
        (-1.0, -1.0),
        (1.0, -1.0),
        (1.0, 0.0),
        (0.0, 0.0),
        (0.0, 1.0),
        (-1.0, 1.0),
    ];
    // Inner hole: small square in the bottom-left of the L.
    let local_hole: [(f64, f64); 4] = [(-0.7, -0.7), (-0.3, -0.7), (-0.3, -0.3), (-0.7, -0.3)];

    let mut poly_xs: Vec<f64> = Vec::new();
    let mut poly_ys: Vec<f64> = Vec::new();
    let mut poly_keys: Vec<i64> = Vec::new();
    let mut poly_ring: Vec<i64> = Vec::new();
    let mut poly_angle_col: Vec<f64> = Vec::new();
    for (m, ((cx, cy), angle)) in poly_centres.iter().zip(poly_angles.iter()).enumerate() {
        for (lx, ly) in &local_outer {
            poly_xs.push(cx + lx * 0.045);
            poly_ys.push(cy + ly * 0.045);
            poly_keys.push(m as i64);
            poly_ring.push(0);
            poly_angle_col.push(*angle);
        }
        for (lx, ly) in &local_hole {
            poly_xs.push(cx + lx * 0.045);
            poly_ys.push(cy + ly * 0.045);
            poly_keys.push(m as i64);
            poly_ring.push(1);
            poly_angle_col.push(*angle);
        }
    }

    let mut plot = Plot::new(&comp(), "panel")
        .title("Per-mark rotation — LineGeom (top) and PolygonGeom (bottom)")
        .bind("x", "x_axis")
        .bind("y", "y_axis");

    plot.add_geom(
        LineGeom::builder()
            .keys(line_keys)
            .set("x", line_xs)
            .set("y", line_ys)
            .set("stroke", line_stroke_col)
            .set("linewidth", 2.5_f64)
            .set("cap", "round")
            .set("join", "round")
            .set("angle", line_angle_col)
            .build(),
    );

    plot.add_geom(
        PolygonGeom::builder()
            .keys(poly_keys)
            .set("x", poly_xs)
            .set("y", poly_ys)
            .set("ring", poly_ring)
            .set(
                "fill",
                vec![fill_red, fill_blue]
                    .into_iter()
                    .cycle()
                    .take(10 * 4)
                    .collect::<Vec<_>>(),
            )
            .set("stroke", ink)
            .set("linewidth", 1.0_f64)
            .set("angle", poly_angle_col)
            .build(),
    );

    // Column labels under each mark.
    plot.add_geom(
        TextGeom::builder()
            .set(
                "x",
                line_centres.iter().map(|(x, _)| *x).collect::<Vec<_>>(),
            )
            .set("y", vec![0.97_f64; 4])
            .set("text", ANGLE_LABELS.to_vec())
            .set("size", 12.0_f64)
            .set("weight", 600.0_f64)
            .set("fill", rgb8(80, 90, 110))
            .set("anchor_x", 0.5_f64)
            .set("anchor_y", 1.0_f64)
            .build(),
    );

    let mut view = PlotComposition::new(comp())
        .add_scale("x_axis", scale::continuous(0.0..=1.0))
        .add_scale("y_axis", scale::continuous(0.0..=1.0))
        .with_plot(plot);
    render_to(
        renderer,
        &mut view,
        w,
        h,
        dpi,
        bg,
        "examples/rotation_2_per_mark.png",
    );
}

fn render_to(
    renderer: &mut VelloRenderer,
    view: &mut PlotComposition,
    w: u32,
    h: u32,
    dpi: f64,
    bg: Color,
    out_relative: &str,
) {
    let _ = (Arc::<str>::from(""), Value::Null); // silence unused-import warnings
    {
        let scene = renderer.scene();
        scene.clear();
        view.render(scene, Size::new(w as f64, h as f64), dpi);
    }
    let mut pixels = vec![0u8; (w * h * 4) as usize];
    renderer
        .render_to_buffer(w, h, bg, &mut pixels)
        .expect("render");
    let path = std::env::current_dir().unwrap().join(out_relative);
    hephaestus::png::write_png(&path, w, h, &pixels).expect("write png");
    println!("wrote {}", path.display());
}
