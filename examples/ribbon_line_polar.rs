//! Phase E.5 — ribbon-style rendering for `LineGeom` under
//! cartesian and polar projections.
//!
//! Three renders demonstrate that per-row variation in `linewidth`
//! or `stroke` upgrades the kurbo-stroke path to a per-vertex
//! tessellated mesh, and that under non-cartesian projections the
//! ribbon densifies along the projected geodesic with interpolated
//! width / colour at every interior sample.
//!
//! - `ribbon_line_polar_1_cartesian.png` — Cartesian baseline.
//!   Three lines: constant (existing stroke path); varying-width
//!   only; varying-width + colour gradient.
//! - `ribbon_line_polar_2_polar.png` — same data under
//!   `Projection::polar()`. Ribbons follow the polar geodesic; the
//!   colour gradient is smooth across each densified interior. A
//!   fourth line shows a marker-bearing dashed pattern under polar
//!   — markers space along the projected arc, not the chord.
//! - `ribbon_line_polar_3_clipped.png` — Cartesian variable-width
//!   line with `clip_end_radius` > 0. Exercises the
//!   `clip_polyline_with_attrs` path: the synthesised clip endpoint
//!   takes a half-width and colour lerped at the segment-`t`.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{beside, Composition, Patch};
use hephaestus::geometry::Size;
use hephaestus::plot::projection::Projection;
use hephaestus::plot::value::Value;
use hephaestus::plot::{linetype, scale, LineGeom, Plot, PlotComposition};
use hephaestus::Renderer;

fn cart_shape() -> Composition {
    beside(
        beside(Patch::new("constant"), Patch::new("varwidth")),
        Patch::new("gradient"),
    )
}

fn polar_shape() -> Composition {
    beside(
        beside(Patch::new("p_constant"), Patch::new("p_varwidth")),
        beside(Patch::new("p_gradient"), Patch::new("p_dashed")),
    )
}

fn clip_shape() -> Composition {
    Composition::empty(1, 1).place(
        1,
        1,
        hephaestus::composition::Span::cell(),
        Patch::new("clip"),
    )
}

fn main() {
    let (w, h) = (1500u32, 500u32);
    let dpi = 96.0;

    // Single sine wave shared across all three plots — 40 vertices.
    let n = 40;
    let xs: Vec<f64> = (0..n).map(|i| i as f64 / (n as f64 - 1.0)).collect();
    let ys: Vec<f64> = xs
        .iter()
        .map(|x| 0.5 + 0.35 * (x * std::f64::consts::TAU).sin())
        .collect();

    // Per-vertex widths: 1 pt at the ends, swelling to 8 pt at the
    // middle.
    let widths: Vec<f64> = xs
        .iter()
        .map(|x| {
            let bell = 1.0 - (2.0 * (x - 0.5)).powi(2);
            1.0 + 7.0 * bell.max(0.0)
        })
        .collect();

    // Colour gradient: red at left, blue at right, smooth through the
    // middle.
    let red = Color::new([0.85, 0.20, 0.30, 1.0]);
    let blue = Color::new([0.10, 0.40, 0.95, 1.0]);
    let colors: Vec<Color> = xs
        .iter()
        .map(|x| {
            Color::new([
                red.components[0] + (blue.components[0] - red.components[0]) * *x as f32,
                red.components[1] + (blue.components[1] - red.components[1]) * *x as f32,
                red.components[2] + (blue.components[2] - red.components[2]) * *x as f32,
                1.0,
            ])
        })
        .collect();

    let mut renderer = VelloRenderer::new().expect("vello renderer init");
    let bg: Color = rgb8(248, 248, 252);

    // ── Render 1: Cartesian ──────────────────────────────────────────
    {
        let mut view = PlotComposition::new(cart_shape())
            .add_scale("x_unit", scale::continuous(0.0..=1.0))
            .add_scale("y_unit", scale::continuous(0.0..=1.0));

        let mut p_const = Plot::new(&cart_shape(), "constant")
            .bind("x", "x_unit")
            .bind("y", "y_unit");
        p_const.add_geom(
            LineGeom::builder()
                .set("x", xs.clone())
                .set("y", ys.clone())
                .set("stroke", rgb8(60, 90, 180))
                .set("linewidth", 3.0_f64)
                .build(),
        );
        view.attach_plot(p_const);

        let mut p_var = Plot::new(&cart_shape(), "varwidth")
            .bind("x", "x_unit")
            .bind("y", "y_unit");
        p_var.add_geom(
            LineGeom::builder()
                .set("x", xs.clone())
                .set("y", ys.clone())
                .set("stroke", rgb8(60, 90, 180))
                .set("linewidth", widths.clone())
                .build(),
        );
        view.attach_plot(p_var);

        let mut p_grad = Plot::new(&cart_shape(), "gradient")
            .bind("x", "x_unit")
            .bind("y", "y_unit");
        p_grad.add_geom(
            LineGeom::builder()
                .set("x", xs.clone())
                .set("y", ys.clone())
                .set("stroke", colors.clone())
                .set("linewidth", widths.clone())
                .build(),
        );
        view.attach_plot(p_grad);

        render_to(
            &mut renderer,
            &mut view,
            w,
            h,
            dpi,
            bg,
            "examples/ribbon_line_polar_1_cartesian.png",
        );
    }

    // ── Render 2: Polar (full circle) ────────────────────────────────
    //
    // Theta channel = xs (period [0, 1] maps to full circle), radius
    // channel = ys. Under polar, each segment between consecutive
    // (theta, radius) data vertices follows a geodesic arc; the
    // ribbon path densifies via `interpolate_segment_with_t` and
    // lerps width / colour at every interior sample.
    {
        let mut view = PlotComposition::new(polar_shape())
            .add_scale("theta", scale::continuous(0.0..=1.0))
            .add_scale("r", scale::continuous(0.0..=1.0));

        // Radial profile for the polar plots — a smooth bump avoids
        // a closed loop hitting the centre and tests the geodesic
        // densification across a wide theta range per segment.
        let polar_r: Vec<f64> = xs.iter().map(|x| 0.45 + 0.35 * x).collect();

        let mut p_pc = Plot::new(&polar_shape(), "p_constant")
            .projection(Projection::polar())
            .bind("x", "theta")
            .bind("y", "r");
        p_pc.add_geom(
            LineGeom::builder()
                .set("x", xs.clone())
                .set("y", polar_r.clone())
                .set("stroke", rgb8(60, 90, 180))
                .set("linewidth", 3.0_f64)
                .build(),
        );
        view.attach_plot(p_pc);

        let mut p_pv = Plot::new(&polar_shape(), "p_varwidth")
            .projection(Projection::polar())
            .bind("x", "theta")
            .bind("y", "r");
        p_pv.add_geom(
            LineGeom::builder()
                .set("x", xs.clone())
                .set("y", polar_r.clone())
                .set("stroke", rgb8(60, 90, 180))
                .set("linewidth", widths.clone())
                .build(),
        );
        view.attach_plot(p_pv);

        let mut p_pg = Plot::new(&polar_shape(), "p_gradient")
            .projection(Projection::polar())
            .bind("x", "theta")
            .bind("y", "r");
        p_pg.add_geom(
            LineGeom::builder()
                .set("x", xs.clone())
                .set("y", polar_r.clone())
                .set("stroke", colors.clone())
                .set("linewidth", widths.clone())
                .build(),
        );
        view.attach_plot(p_pg);

        let mut p_pd = Plot::new(&polar_shape(), "p_dashed")
            .projection(Projection::polar())
            .bind("x", "theta")
            .bind("y", "r");
        p_pd.add_geom(
            LineGeom::builder()
                .set("x", xs.clone())
                .set("y", polar_r.clone())
                .set("stroke", rgb8(20, 20, 20))
                .set("linewidth", 2.0_f64)
                .set("linetype", Value::Linetype(linetype::dashed()))
                .build(),
        );
        view.attach_plot(p_pd);

        render_to(
            &mut renderer,
            &mut view,
            w,
            h,
            dpi,
            bg,
            "examples/ribbon_line_polar_2_polar.png",
        );
    }

    // ── Render 3: Cartesian + end clip ───────────────────────────────
    //
    // Variable-width line with `clip_end_radius > 0`. The synthesised
    // clip endpoint sits between the last two data vertices at some
    // segment fraction `t`; `clip_polyline_with_attrs` lerps the
    // half-width and colour at that `t` so the ribbon terminates
    // smoothly without a width discontinuity.
    {
        let mut view = PlotComposition::new(clip_shape())
            .add_scale("x_unit", scale::continuous(0.0..=1.0))
            .add_scale("y_unit", scale::continuous(0.0..=1.0));
        let mut p = Plot::new(&clip_shape(), "clip")
            .bind("x", "x_unit")
            .bind("y", "y_unit");
        p.add_geom(
            LineGeom::builder()
                .set("x", xs.clone())
                .set("y", ys.clone())
                .set("stroke", colors.clone())
                .set("linewidth", widths.clone())
                .set("clip_end_radius", 35.0_f64)
                .build(),
        );
        view.attach_plot(p);

        render_to(
            &mut renderer,
            &mut view,
            (w as f64 / 3.0) as u32,
            h,
            dpi,
            bg,
            "examples/ribbon_line_polar_3_clipped.png",
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
