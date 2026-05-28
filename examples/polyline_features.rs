//! End-to-end visual demonstration of the v1.5 polyline-primitive
//! channels on `LineGeom` and `SegmentGeom`.
//!
//! Three renders:
//! - `polyline_1_corner_radius.png` — four identical zigzag polylines
//!   stacked vertically, each with a different `"corner_radius"`. Goes
//!   from sharp (`0pt`) through to heavily filleted (`24pt`).
//! - `polyline_2_corner_max_angle.png` — same mixed-angle polyline
//!   drawn twice. The top copy uses the default `max_angle = ∞` and
//!   rounds every corner; the bottom copy uses `max_angle = 60°` and
//!   leaves the obtuse corners sharp while still rounding the acute
//!   ones.
//! - `polyline_3_endpoint_clip.png` — a small node-and-edge graph.
//!   The edges are `SegmentGeom`; `clip_start_radius` /
//!   `clip_end_radius` (matched to the node's pt radius) pull each
//!   connector back to the visual node boundary so segments don't
//!   shoot through the dots.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, rgba, Color};
use hephaestus::composition::{Composition, Patch, Span};
use hephaestus::geometry::Size;
use hephaestus::plot::{LineGeom, Plot, PlotComposition, PointGeom, Raw, SegmentGeom, TextGeom};
use hephaestus::Renderer;

fn main() {
    let (w, h) = (1200u32, 500u32);
    let dpi = 96.0;

    let comp = || Composition::empty(1, 1).place(1, 1, Span::cell(), Patch::new("panel"));
    let mut renderer = VelloRenderer::new().expect("vello renderer init");
    let bg: Color = rgb8(248, 248, 252);

    // ── Render 1: corner_radius progression ──────────────────────────
    {
        // Same zigzag shape stacked at four y positions, each with a
        // different corner_radius value. Vertices live in panel-fraction
        // space — `Raw` bypasses any scale binding.
        let radii_pt = [0.0_f64, 6.0, 14.0, 24.0];
        let y_rows = [0.84_f64, 0.62, 0.40, 0.18];

        let mut plot = Plot::new(&comp(), "panel");

        for (i, &y) in y_rows.iter().enumerate() {
            let dy = 0.07;
            let xs = vec![
                0.18_f64, 0.30, 0.30, 0.46, 0.46, 0.62, 0.62, 0.78, 0.78, 0.94,
            ];
            let ys = vec![y, y, y + dy, y + dy, y - dy, y - dy, y + dy, y + dy, y, y];
            plot.add_geom(
                LineGeom::builder()
                    .keys(vec![i as i64; xs.len()])
                    .set("x", Raw(xs))
                    .set("y", Raw(ys))
                    .set("stroke", rgb8(70, 100, 180))
                    .set("linewidth", 3.0_f64)
                    .set("cap", "round")
                    .set("join", "round")
                    .set("corner_radius", radii_pt[i])
                    .build(),
            );
            plot.add_geom(
                TextGeom::builder()
                    .set("x", Raw(vec![0.02_f64]))
                    .set("y", Raw(vec![y]))
                    .set(
                        "text",
                        vec![format!("corner_radius = {}pt", radii_pt[i] as i32)],
                    )
                    .set("anchor_x", 0.0_f64)
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
            "examples/polyline_1_corner_radius.png",
        );
    }

    // ── Render 2: corner_max_angle filter ────────────────────────────
    {
        // A polyline that alternates between gentle and sharp peaks.
        // With x-step = 0.10 between adjacent peaks:
        //   y-step = 0.025 → interior angle ≈ 152° (obtuse)
        //   y-step = 0.20  → interior angle ≈ 53°  (acute)
        // The top copy rounds every corner (default `max_angle = ∞`);
        // the bottom copy only rounds the acute corners (those with
        // interior angle ≤ 60°), leaving the obtuse ones sharp.
        let mixed_shape = |y_base: f64| {
            let xs = vec![0.10_f64, 0.20, 0.30, 0.40, 0.50, 0.60, 0.70, 0.80, 0.90];
            let pattern = [0.0_f64, 0.025, 0.0, -0.20, 0.0, 0.025, 0.0, -0.20, 0.0];
            let ys = pattern.iter().map(|p| y_base + p).collect::<Vec<_>>();
            (xs, ys)
        };

        let mut plot = Plot::new(&comp(), "panel");

        let (x_top, y_top) = mixed_shape(0.75);
        plot.add_geom(
            LineGeom::builder()
                .set("x", Raw(x_top))
                .set("y", Raw(y_top))
                .set("stroke", rgb8(180, 70, 90))
                .set("linewidth", 3.0_f64)
                .set("cap", "round")
                .set("join", "round")
                .set("corner_radius", 28.0_f64)
                .build(),
        );
        plot.add_geom(
            TextGeom::builder()
                .set("x", Raw(vec![0.5_f64]))
                .set("y", Raw(vec![0.92_f64]))
                .set(
                    "text",
                    vec!["corner_max_angle = ∞ (default) — every corner rounds"],
                )
                .set("anchor_x", 0.5_f64)
                .set("anchor_y", 0.5_f64)
                .set("size", 13.0_f64)
                .set("weight", 600.0_f64)
                .set("fill", rgb8(180, 70, 90))
                .build(),
        );

        let (x_bot, y_bot) = mixed_shape(0.30);
        plot.add_geom(
            LineGeom::builder()
                .set("x", Raw(x_bot))
                .set("y", Raw(y_bot))
                .set("stroke", rgb8(60, 140, 100))
                .set("linewidth", 3.0_f64)
                .set("cap", "round")
                .set("join", "round")
                .set("corner_radius", 28.0_f64)
                .set("corner_max_angle", 60.0_f64)
                .build(),
        );
        plot.add_geom(
            TextGeom::builder()
                .set("x", Raw(vec![0.5_f64]))
                .set("y", Raw(vec![0.08_f64]))
                .set(
                    "text",
                    vec!["corner_max_angle = 60° — only acute corners round"],
                )
                .set("anchor_x", 0.5_f64)
                .set("anchor_y", 0.5_f64)
                .set("size", 13.0_f64)
                .set("weight", 600.0_f64)
                .set("fill", rgb8(60, 140, 100))
                .build(),
        );

        // Use a square panel for this render so the geometry-defined
        // interior angles match what you see — corner rounding works in
        // panel-pixel space, not panel-fraction space.
        let mut view = PlotComposition::new(comp()).with_plot(plot);
        render_to(
            &mut renderer,
            &mut view,
            600,
            600,
            dpi,
            bg,
            "examples/polyline_2_corner_max_angle.png",
        );
    }

    // ── Render 3: endpoint-clipped network edges ─────────────────────
    {
        // A tiny node-and-edge graph: five nodes drawn as PointGeom
        // circles, six edges drawn as SegmentGeoms with both endpoints
        // clipped to the node radius. Without the clip the segments
        // would run from centre to centre and dive into the dots.
        let node_xs = vec![0.15_f64, 0.40, 0.40, 0.65, 0.85];
        let node_ys = vec![0.50_f64, 0.20, 0.80, 0.50, 0.50];
        let node_radius_pt = 14.0_f64;
        let node_diameter_pt = node_radius_pt * 2.0;

        let edges: Vec<(usize, usize)> = vec![(0, 1), (0, 2), (1, 3), (2, 3), (3, 4), (0, 3)];
        let seg_x: Vec<f64> = edges.iter().map(|(a, _)| node_xs[*a]).collect();
        let seg_y: Vec<f64> = edges.iter().map(|(a, _)| node_ys[*a]).collect();
        let seg_x2: Vec<f64> = edges.iter().map(|(_, b)| node_xs[*b]).collect();
        let seg_y2: Vec<f64> = edges.iter().map(|(_, b)| node_ys[*b]).collect();

        let mut plot = Plot::new(&comp(), "panel");

        // Edges first so the nodes draw on top.
        plot.add_geom(
            SegmentGeom::builder()
                .set("x", Raw(seg_x))
                .set("y", Raw(seg_y))
                .set("x2", Raw(seg_x2))
                .set("y2", Raw(seg_y2))
                .set("x_band", 0.0_f64)
                .set("x2_band", 0.0_f64)
                .set("y_band", 0.0_f64)
                .set("y2_band", 0.0_f64)
                .set("stroke", rgb8(110, 120, 140))
                .set("linewidth", 2.0_f64)
                .set("cap", "round")
                .set("clip_start_radius", node_radius_pt)
                .set("clip_end_radius", node_radius_pt)
                .build(),
        );

        // Nodes — fill only. PointGeom's stroke is currently applied
        // after the size affine, so a stroked large glyph swallows the
        // fill; we keep the demo to fills only to avoid that
        // pre-existing issue.
        plot.add_geom(
            PointGeom::builder()
                .set("x", Raw(node_xs.clone()))
                .set("y", Raw(node_ys.clone()))
                .set("fill", rgb8(220, 100, 80))
                .set("size", node_diameter_pt)
                .build(),
        );

        // Title strip.
        plot.add_geom(
            TextGeom::builder()
                .set("x", Raw(vec![0.5_f64]))
                .set("y", Raw(vec![0.95_f64]))
                .set(
                    "text",
                    vec!["SegmentGeom: clip_*_radius trims edges to node boundary"],
                )
                .set("anchor_x", 0.5_f64)
                .set("anchor_y", 0.5_f64)
                .set("size", 14.0_f64)
                .set("weight", 600.0_f64)
                .set("fill", rgb8(40, 40, 50))
                .set("bg_fill", rgba(1.0, 1.0, 1.0, 0.85))
                .set("bg_padding", 6.0_f64)
                .set("bg_corner_radius", 4.0_f64)
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
            "examples/polyline_3_endpoint_clip.png",
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
