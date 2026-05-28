//! End-to-end visual demonstration of the v1.5 polygon-primitive
//! channels: `corner_radius`, `corner_max_angle`, and the `expand`
//! channel (also shared by RectGeom / EllipseGeom / WedgeGeom).
//!
//! Three renders:
//! - `polygon_1_corner_radius.png` — same 5-pointed star repeated
//!   across four columns at increasing `corner_radius` values, going
//!   from sharp points to fully filleted lobes.
//! - `polygon_2_expand.png` — same hexagon repeated across four
//!   columns with `expand` values `-8pt`, `0`, `+8pt`, `+16pt` to show
//!   the inward / outward offset.
//! - `polygon_3_all_geoms_expand.png` — `RectGeom` / `EllipseGeom` /
//!   `WedgeGeom` / `PolygonGeom` each drawn twice in the same row: a
//!   semi-transparent "halo" copy with `expand = +10pt` behind the
//!   solid base shape. Demonstrates that the same `expand` channel
//!   works uniformly across geom types.

use std::f64::consts::{PI, TAU};

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, rgba, Color};
use hephaestus::composition::{Composition, Patch, Span};
use hephaestus::geometry::Size;
use hephaestus::plot::value::Value;
use hephaestus::plot::{
    EllipseGeom, Plot, PlotComposition, PolygonGeom, Raw, RectGeom, TextGeom, WedgeGeom,
};
use hephaestus::Renderer;

/// Generate `n`-pointed star vertices in panel-fraction space, anchored
/// at `(cx, cy)`. `r_outer` is the tip radius, `r_inner` the valley
/// radius (set them equal for a regular polygon).
fn star_vertices(
    cx: f64,
    cy: f64,
    r_outer: f64,
    r_inner: f64,
    n_points: usize,
) -> (Vec<f64>, Vec<f64>) {
    let total = n_points * 2;
    let mut xs = Vec::with_capacity(total);
    let mut ys = Vec::with_capacity(total);
    for i in 0..total {
        // Start at the top (12 o'clock) so the star looks upright.
        let theta = -std::f64::consts::FRAC_PI_2 + (i as f64) * PI / (n_points as f64);
        let r = if i % 2 == 0 { r_outer } else { r_inner };
        xs.push(cx + r * theta.cos());
        ys.push(cy + r * theta.sin());
    }
    (xs, ys)
}

fn regular_polygon(cx: f64, cy: f64, r: f64, n_sides: usize) -> (Vec<f64>, Vec<f64>) {
    let mut xs = Vec::with_capacity(n_sides);
    let mut ys = Vec::with_capacity(n_sides);
    for i in 0..n_sides {
        let theta = -std::f64::consts::FRAC_PI_2 + (i as f64) * TAU / (n_sides as f64);
        xs.push(cx + r * theta.cos());
        ys.push(cy + r * theta.sin());
    }
    (xs, ys)
}

fn main() {
    let (w, h) = (1200u32, 500u32);
    let dpi = 96.0;

    let comp = || Composition::empty(1, 1).place(1, 1, Span::cell(), Patch::new("panel"));
    let mut renderer = VelloRenderer::new().expect("vello renderer init");
    let bg: Color = rgb8(248, 248, 252);

    // ── Render 1: corner_radius progression on a star polygon ────────
    {
        let radii_pt = [0.0_f64, 5.0, 12.0, 22.0];
        let cx_positions = [0.15_f64, 0.38, 0.62, 0.85];
        let cy = 0.55;

        let mut plot = Plot::new(&comp(), "panel");

        for (i, &r_pt) in radii_pt.iter().enumerate() {
            let cx = cx_positions[i];
            let (xs, ys) = star_vertices(cx, cy, 0.085, 0.038, 5);
            let n = xs.len();
            plot.add_geom(
                PolygonGeom::builder()
                    .keys(vec![i as i64; n])
                    .set("x", Raw(xs))
                    .set("y", Raw(ys))
                    .set("fill", rgb8(180, 70, 110))
                    .set("stroke", rgb8(50, 30, 50))
                    .set("linewidth", 1.5_f64)
                    .set("corner_radius", r_pt)
                    .build(),
            );
            plot.add_geom(
                TextGeom::builder()
                    .set("x", Raw(vec![cx]))
                    .set("y", Raw(vec![0.12_f64]))
                    .set("text", vec![format!("corner_radius = {}pt", r_pt as i32)])
                    .set("anchor_x", 0.5_f64)
                    .set("anchor_y", 0.5_f64)
                    .set("size", 13.0_f64)
                    .set("fill", rgb8(40, 40, 50))
                    .build(),
            );
        }

        let mut view = PlotComposition::new(comp()).with_plot(plot);
        render_to(
            &mut renderer,
            &mut view,
            w,
            h,
            dpi,
            bg,
            "examples/polygon_1_corner_radius.png",
        );
    }

    // ── Render 2: expand on a hexagon ────────────────────────────────
    {
        let expand_pt = [-8.0_f64, 0.0, 8.0, 16.0];
        let cx_positions = [0.15_f64, 0.38, 0.62, 0.85];
        let cy = 0.55;
        let r = 0.10;

        let mut plot = Plot::new(&comp(), "panel");

        for (i, &e) in expand_pt.iter().enumerate() {
            let cx = cx_positions[i];
            // Outline of the *original* hexagon for reference.
            let (xs0, ys0) = regular_polygon(cx, cy, r, 6);
            let n = xs0.len();
            plot.add_geom(
                PolygonGeom::builder()
                    .keys(vec![(i * 10) as i64; n])
                    .set("x", Raw(xs0.clone()))
                    .set("y", Raw(ys0.clone()))
                    .set("stroke", rgba(0.3, 0.3, 0.4, 0.5))
                    .set("linewidth", 1.0_f64)
                    .set(
                        "linetype",
                        Value::Linetype(hephaestus::plot::linetype::dashed()),
                    )
                    .build(),
            );
            // Expanded shape on top.
            plot.add_geom(
                PolygonGeom::builder()
                    .keys(vec![(i * 10 + 1) as i64; n])
                    .set("x", Raw(xs0))
                    .set("y", Raw(ys0))
                    .set("fill", rgba(0.30, 0.55, 0.78, 0.55))
                    .set("stroke", rgb8(70, 140, 200))
                    .set("linewidth", 1.8_f64)
                    .set("expand", e)
                    .build(),
            );
            plot.add_geom(
                TextGeom::builder()
                    .set("x", Raw(vec![cx]))
                    .set("y", Raw(vec![0.12_f64]))
                    .set("text", vec![format!("expand = {:+}pt", e as i32)])
                    .set("anchor_x", 0.5_f64)
                    .set("anchor_y", 0.5_f64)
                    .set("size", 13.0_f64)
                    .set("fill", rgb8(40, 40, 50))
                    .build(),
            );
        }

        // Caption.
        plot.add_geom(
            TextGeom::builder()
                .set("x", Raw(vec![0.5_f64]))
                .set("y", Raw(vec![0.94_f64]))
                .set(
                    "text",
                    vec!["Dashed outline = original hexagon. Filled = after expand."],
                )
                .set("anchor_x", 0.5_f64)
                .set("anchor_y", 0.5_f64)
                .set("size", 12.0_f64)
                .set("fill", rgb8(60, 60, 70))
                .build(),
        );

        let mut view = PlotComposition::new(comp()).with_plot(plot);
        render_to(
            &mut renderer,
            &mut view,
            w,
            h,
            dpi,
            bg,
            "examples/polygon_2_expand.png",
        );
    }

    // ── Render 3: expand applied to every geom that supports it ──────
    {
        // Four shapes in a row: Rect, Ellipse, Wedge, Polygon. Each is
        // drawn twice — a semi-transparent "halo" with expand = +10pt
        // behind the solid base shape.
        let cx_positions = [0.15_f64, 0.38, 0.62, 0.85];
        let cy = 0.50;
        let halo_alpha = 0.30_f32;
        let halo_expand = 10.0_f64;

        let mut plot = Plot::new(&comp(), "panel");

        // ── Rect ──
        let rect_color = rgb8(220, 90, 90);
        plot.add_geom(
            RectGeom::builder()
                .set(
                    "x",
                    Raw(vec![cx_positions[0] - 0.07, cx_positions[0] - 0.07]),
                )
                .set("y", Raw(vec![cy - 0.10, cy - 0.10]))
                .set(
                    "x2",
                    Raw(vec![cx_positions[0] + 0.07, cx_positions[0] + 0.07]),
                )
                .set("y2", Raw(vec![cy + 0.10, cy + 0.10]))
                .set("x_band", 0.0_f64)
                .set("x2_band", 0.0_f64)
                .set(
                    "fill",
                    vec![
                        rgba(
                            rect_color.components[0],
                            rect_color.components[1],
                            rect_color.components[2],
                            halo_alpha,
                        ),
                        rect_color,
                    ],
                )
                .set("expand", vec![halo_expand, 0.0_f64])
                .build(),
        );

        // ── Ellipse ──
        let ellipse_color = rgb8(70, 140, 100);
        plot.add_geom(
            EllipseGeom::builder()
                .set("x", Raw(vec![cx_positions[1]; 2]))
                .set("y", Raw(vec![cy; 2]))
                .set("x2", Raw(vec![cx_positions[1] + 0.08; 2]))
                .set("y2", Raw(vec![cy + 0.11; 2]))
                .set(
                    "fill",
                    vec![
                        rgba(
                            ellipse_color.components[0],
                            ellipse_color.components[1],
                            ellipse_color.components[2],
                            halo_alpha,
                        ),
                        ellipse_color,
                    ],
                )
                .set("expand", vec![halo_expand, 0.0_f64])
                .build(),
        );

        // ── Wedge ──
        let wedge_color = rgb8(180, 130, 60);
        plot.add_geom(
            WedgeGeom::builder()
                .set("x", Raw(vec![cx_positions[2]; 2]))
                .set("y", Raw(vec![cy; 2]))
                .set("radius", vec![38.0_f64, 38.0])
                .set("theta", vec![PI * 0.15_f64, PI * 0.15])
                .set("theta2", vec![PI * 1.55_f64, PI * 1.55])
                .set(
                    "fill",
                    vec![
                        rgba(
                            wedge_color.components[0],
                            wedge_color.components[1],
                            wedge_color.components[2],
                            halo_alpha,
                        ),
                        wedge_color,
                    ],
                )
                .set("expand", vec![halo_expand, 0.0_f64])
                .build(),
        );

        // ── Polygon (5-pointed star) ──
        let star_color = rgb8(150, 100, 200);
        let (sx, sy) = star_vertices(cx_positions[3], cy, 0.085, 0.038, 5);
        let n = sx.len();
        // Halo first.
        plot.add_geom(
            PolygonGeom::builder()
                .keys(vec![100_i64; n])
                .set("x", Raw(sx.clone()))
                .set("y", Raw(sy.clone()))
                .set(
                    "fill",
                    rgba(
                        star_color.components[0],
                        star_color.components[1],
                        star_color.components[2],
                        halo_alpha,
                    ),
                )
                .set("expand", halo_expand)
                .build(),
        );
        // Base star on top.
        plot.add_geom(
            PolygonGeom::builder()
                .keys(vec![101_i64; n])
                .set("x", Raw(sx))
                .set("y", Raw(sy))
                .set("fill", star_color)
                .build(),
        );

        // Labels.
        let label_y = 0.10;
        let labels = ["RectGeom", "EllipseGeom", "WedgeGeom", "PolygonGeom"];
        plot.add_geom(
            TextGeom::builder()
                .set("x", Raw(cx_positions.to_vec()))
                .set("y", Raw(vec![label_y; 4]))
                .set(
                    "text",
                    labels.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                )
                .set("anchor_x", 0.5_f64)
                .set("anchor_y", 0.5_f64)
                .set("size", 13.0_f64)
                .set("weight", 600.0_f64)
                .set("fill", rgb8(40, 40, 50))
                .build(),
        );
        plot.add_geom(
            TextGeom::builder()
                .set("x", Raw(vec![0.5_f64]))
                .set("y", Raw(vec![0.94_f64]))
                .set(
                    "text",
                    vec!["Same `expand = +10pt` halo on every geom that supports it"],
                )
                .set("anchor_x", 0.5_f64)
                .set("anchor_y", 0.5_f64)
                .set("size", 14.0_f64)
                .set("fill", rgb8(40, 40, 50))
                .build(),
        );

        let mut view = PlotComposition::new(comp()).with_plot(plot);
        render_to(
            &mut renderer,
            &mut view,
            w,
            h,
            dpi,
            bg,
            "examples/polygon_3_all_geoms_expand.png",
        );
    }
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
