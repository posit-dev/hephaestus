//! Polyline ribbons rendered via `SceneBuilder::draw_mesh`.
//!
//! Four renders:
//! - `ribbon_1_gradient_along.png` — single polyline with a 5-stop
//!   colour gradient along its length.
//! - `ribbon_2_variable_width.png` — single polyline with width
//!   varying from 2 pt at the start to 20 pt at the end.
//! - `ribbon_3_full.png` — gradient + variable-width combined.
//! - `ribbon_4_caps_joins.png` — 3×3 grid: rows = cap (butt / square
//!   / round), columns = join (miter / bevel / round).

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::geometry::Affine;
use hephaestus::mesh::Mesh;
use hephaestus::pick::PickId;
use hephaestus::primitives::{
    polyline_gradient, polyline_ribbon, polyline_ribbon_full, RibbonCap, RibbonJoin, RibbonOptions,
};
use hephaestus::scene::SceneBuilder;
use hephaestus::{Point, Renderer};

fn main() {
    let dpi = 96.0;
    let bg: Color = rgb8(248, 248, 252);
    let mut renderer = VelloRenderer::new().expect("vello renderer init");

    // ── Render 1: colour gradient along the line ───────────────────
    {
        let (w, h) = (1200u32, 300u32);
        let n = 80;
        let xs: Vec<f64> = (0..n)
            .map(|i| 80.0 + i as f64 / (n - 1) as f64 * 1040.0)
            .collect();
        let ys: Vec<f64> = xs
            .iter()
            .enumerate()
            .map(|(i, _)| 150.0 + 60.0 * (i as f64 * 0.18).sin())
            .collect();
        let points: Vec<Point> = xs
            .iter()
            .zip(ys.iter())
            .map(|(x, y)| Point::new(*x, *y))
            .collect();
        // 5-stop gradient: cool blues to warm reds.
        let stops = [
            rgb8(60, 80, 200),
            rgb8(80, 160, 200),
            rgb8(220, 220, 220),
            rgb8(220, 130, 80),
            rgb8(200, 50, 60),
        ];
        let colors: Vec<Color> = (0..n)
            .map(|i| interpolate_stops(&stops, i as f64 / (n - 1) as f64))
            .collect();
        let opts = RibbonOptions {
            half_width: pt_to_px(12.0, dpi) * 0.5,
            cap: RibbonCap::Round,
            join: RibbonJoin::Round,
            miter_limit: 4.0,
        };
        let mesh = polyline_gradient(&points, &colors, &opts);
        render_mesh(
            &mut renderer,
            &mesh,
            w,
            h,
            dpi,
            bg,
            "examples/ribbon_1_gradient_along.png",
        );
    }

    // ── Render 2: variable width along the line ────────────────────
    {
        let (w, h) = (1200u32, 300u32);
        let n = 80;
        let xs: Vec<f64> = (0..n)
            .map(|i| 80.0 + i as f64 / (n - 1) as f64 * 1040.0)
            .collect();
        let ys: Vec<f64> = (0..n)
            .map(|i| 150.0 + 60.0 * (i as f64 * 0.18).sin())
            .collect();
        let points: Vec<Point> = xs
            .iter()
            .zip(ys.iter())
            .map(|(x, y)| Point::new(*x, *y))
            .collect();
        // Width ramps from 2pt to 20pt linearly.
        let half_widths: Vec<f64> = (0..n)
            .map(|i| {
                let t = i as f64 / (n - 1) as f64;
                pt_to_px(2.0 + (20.0 - 2.0) * t, dpi) * 0.5
            })
            .collect();
        let opts = RibbonOptions {
            half_width: 1.0,
            cap: RibbonCap::Round,
            join: RibbonJoin::Round,
            miter_limit: 4.0,
        };
        // `polyline_ribbon_full` with no colours uses black; pass a
        // same-colour slice for the desired stroke colour.
        let stroke = rgb8(40, 90, 180);
        let colors = vec![stroke; n];
        let mesh = polyline_ribbon_full(&points, Some(&colors), Some(&half_widths), &opts);
        render_mesh(
            &mut renderer,
            &mesh,
            w,
            h,
            dpi,
            bg,
            "examples/ribbon_2_variable_width.png",
        );
    }

    // ── Render 3: gradient + variable width on a self-intersecting
    // figure-8 (Lissajous 1:2). The polyline crosses itself once at
    // the centre — each crossing arm renders at a different width
    // and colour, and SrcOver compositing layers them in source order
    // (later vertices draw on top of earlier ones).
    {
        let (w, h) = (1200u32, 400u32);
        let n = 240;
        let cx = 600.0;
        let cy = 200.0;
        let amp_x = 500.0;
        let amp_y = 120.0;
        let xs: Vec<f64> = (0..n)
            .map(|i| {
                let t = i as f64 / (n - 1) as f64 * std::f64::consts::TAU;
                cx + amp_x * t.sin()
            })
            .collect();
        let ys: Vec<f64> = (0..n)
            .map(|i| {
                let t = i as f64 / (n - 1) as f64 * std::f64::consts::TAU;
                cy + amp_y * (2.0 * t).sin()
            })
            .collect();
        let points: Vec<Point> = xs
            .iter()
            .zip(ys.iter())
            .map(|(x, y)| Point::new(*x, *y))
            .collect();
        let stops = [
            rgb8(60, 80, 200),
            rgb8(80, 160, 200),
            rgb8(220, 220, 220),
            rgb8(220, 130, 80),
            rgb8(200, 50, 60),
        ];
        let colors: Vec<Color> = (0..n)
            .map(|i| interpolate_stops(&stops, i as f64 / (n - 1) as f64))
            .collect();
        let half_widths: Vec<f64> = (0..n)
            .map(|i| {
                let t = i as f64 / (n - 1) as f64;
                pt_to_px(2.0 + (24.0 - 2.0) * t, dpi) * 0.5
            })
            .collect();
        let opts = RibbonOptions {
            half_width: 1.0,
            cap: RibbonCap::Round,
            join: RibbonJoin::Round,
            miter_limit: 4.0,
        };
        let mesh = polyline_ribbon_full(&points, Some(&colors), Some(&half_widths), &opts);
        render_mesh(
            &mut renderer,
            &mesh,
            w,
            h,
            dpi,
            bg,
            "examples/ribbon_3_full.png",
        );
    }

    // ── Render 4: 3×3 cap/join grid ────────────────────────────────
    {
        let (w, h) = (900u32, 700u32);
        let caps = [
            ("butt", RibbonCap::Butt),
            ("square", RibbonCap::Square),
            ("round", RibbonCap::Round),
        ];
        let joins = [
            ("miter", RibbonJoin::Miter),
            ("bevel", RibbonJoin::Bevel),
            ("round", RibbonJoin::Round),
        ];
        // Build 9 ribbons in one mesh (or one render): for each
        // (row, col), zigzag polyline showing the cap and join.
        let mut all_meshes: Vec<Mesh> = Vec::new();
        let cell_w = w as f64 / 3.0;
        let cell_h = h as f64 / 3.0;
        for (r_idx, (_cap_name, cap)) in caps.iter().enumerate() {
            for (c_idx, (_join_name, join)) in joins.iter().enumerate() {
                let cx = c_idx as f64 * cell_w;
                let cy = r_idx as f64 * cell_h;
                // Zigzag polyline filling the cell.
                let pad = 30.0;
                let pts = vec![
                    Point::new(cx + pad, cy + cell_h - pad),
                    Point::new(cx + cell_w * 0.5, cy + pad),
                    Point::new(cx + cell_w - pad, cy + cell_h - pad),
                ];
                let opts = RibbonOptions {
                    half_width: pt_to_px(18.0, dpi) * 0.5,
                    cap: *cap,
                    join: *join,
                    miter_limit: 4.0,
                };
                let stroke = rgb8(40, 90, 180);
                all_meshes.push(polyline_ribbon(&pts, stroke, &opts));
            }
        }
        render_meshes(
            &mut renderer,
            &all_meshes,
            w,
            h,
            dpi,
            bg,
            "examples/ribbon_4_caps_joins.png",
        );
    }
}

fn pt_to_px(pt: f64, dpi: f64) -> f64 {
    pt * dpi / 72.0
}

fn interpolate_stops(stops: &[Color], t: f64) -> Color {
    if stops.is_empty() {
        return Color::new([0.0, 0.0, 0.0, 1.0]);
    }
    if stops.len() == 1 {
        return stops[0];
    }
    let scaled = t.clamp(0.0, 1.0) * (stops.len() - 1) as f64;
    let lo = scaled.floor() as usize;
    let hi = (lo + 1).min(stops.len() - 1);
    let frac = (scaled - lo as f64) as f32;
    let a = stops[lo].components;
    let b = stops[hi].components;
    Color::new([
        a[0] * (1.0 - frac) + b[0] * frac,
        a[1] * (1.0 - frac) + b[1] * frac,
        a[2] * (1.0 - frac) + b[2] * frac,
        a[3] * (1.0 - frac) + b[3] * frac,
    ])
}

fn render_mesh(
    renderer: &mut VelloRenderer,
    mesh: &Mesh,
    w: u32,
    h: u32,
    dpi: f64,
    bg: Color,
    out: &str,
) {
    let _ = dpi;
    {
        let scene = renderer.scene();
        scene.clear();
        scene.draw_mesh(mesh, Affine::IDENTITY, PickId::Skip);
    }
    let mut pixels = vec![0u8; (w * h * 4) as usize];
    renderer
        .render_to_buffer(w, h, bg, &mut pixels)
        .expect("render");
    let path = std::env::current_dir().unwrap().join(out);
    hephaestus::png::write_png(&path, w, h, &pixels).expect("write png");
    println!("wrote {}", path.display());
}

fn render_meshes(
    renderer: &mut VelloRenderer,
    meshes: &[Mesh],
    w: u32,
    h: u32,
    dpi: f64,
    bg: Color,
    out: &str,
) {
    let _ = dpi;
    {
        let scene = renderer.scene();
        scene.clear();
        for m in meshes {
            scene.draw_mesh(m, Affine::IDENTITY, PickId::Skip);
        }
    }
    let mut pixels = vec![0u8; (w * h * 4) as usize];
    renderer
        .render_to_buffer(w, h, bg, &mut pixels)
        .expect("render");
    let path = std::env::current_dir().unwrap().join(out);
    hephaestus::png::write_png(&path, w, h, &pixels).expect("write png");
    println!("wrote {}", path.display());
}
