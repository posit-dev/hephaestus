//! End-to-end visual sanity for `BSplineGeom` — clamped uniform-knot
//! B-spline curves, one per mark. Mirrors `examples/line.rs` and
//! `examples/ribbon_geom.rs` for layout.
//!
//! Renders, all 1200×500 with full plotting chrome:
//!
//! - `bspline_1_cubic_vs_bezier.png` — single 4-point degree-3 spline.
//!   The clamped knot vector pins the curve to the first/last control
//!   points; interior points pull without passing through. Math
//!   equivalence to a cubic Bezier is verified separately in the unit
//!   tests.
//! - `bspline_2_parallel_coords.png` — five categorical x positions
//!   threading several smoothed curves through them.
//! - `bspline_3_polar_interpolation.png` — same control polygon
//!   under polar projection, one curve in `"domain"` mode and one in
//!   `"panel"` mode; demonstrates the `"interpolation"` channel.
//! - `bspline_4_endpoint_arrows.png` — spline with arrow-closed
//!   markers at both endpoints, confirming the tangent points along
//!   the first knot interval.
//! - `bspline_5_ribbon_gradient.png` — per-row stroke gradient + per-
//!   row linewidth taper along a spline, triggering the ribbon-mode
//!   variance-detect dispatch.
//! - `bspline_6_dashed_markers.png` — dashed pattern with embedded
//!   circle markers walked along the curve via the same arc-length
//!   walker LineGeom uses.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{Composition, Patch, Span};
use hephaestus::geometry::Size;
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement, PolarRing};
use hephaestus::plot::projection::Projection;
use hephaestus::plot::{linetype, scale, BSplineGeom, Plot, PlotComposition, Value};
use hephaestus::scales::chrome::AxisSide;
use hephaestus::Renderer;

fn cell_comp() -> Composition {
    Composition::empty(1, 1).place(1, 1, Span::cell(), Patch::new("panel"))
}

fn main() {
    let (w, h) = (1200u32, 500u32);
    let dpi = 96.0;

    let mut renderer = VelloRenderer::new().expect("vello renderer init");
    let bg: Color = rgb8(248, 248, 252);

    cubic_vs_bezier(&mut renderer, w, h, dpi, bg);
    parallel_coordinates(&mut renderer, w, h, dpi, bg);
    polar_interpolation(&mut renderer, w, h, dpi, bg);
    endpoint_arrows(&mut renderer, w, h, dpi, bg);
    ribbon_gradient(&mut renderer, w, h, dpi, bg);
    dashed_markers(&mut renderer, w, h, dpi, bg);
}

// ── Render 1: clamped endpoints + interior pull ──
//
// Single 4-point degree-3 spline. The clamped knot vector forces the
// curve to pass exactly through the first and last control points
// while pulling toward — not through — the interior two. For a
// 4-point group this curve is mathematically a cubic Bezier
// (verified in the unit tests via `de_boor_4pt_cubic_matches_bezier_at_half`).
fn cubic_vs_bezier(renderer: &mut VelloRenderer, w: u32, h: u32, dpi: f64, bg: Color) {
    let xs = vec![1.0_f64, 4.0, 7.0, 9.0];
    let ys = vec![1.0_f64, 8.5, 1.0, 6.0];

    let mut plot = Plot::new(&cell_comp(), "panel")
        .title("Clamped 4-point B-spline (cubic)")
        .subtitle("First/last control points lie on the curve; interior points pull the curve toward them")
        .bind("x", "x")
        .bind("y", "y");
    plot.add_geom(
        BSplineGeom::builder()
            .set("x", xs.clone())
            .set("y", ys.clone())
            .set("stroke", rgb8(40, 60, 140))
            .set("linewidth", 3.0_f64)
            .build(),
    );
    plot.add_geom(
        hephaestus::plot::PointGeom::builder()
            .set("x", xs)
            .set("y", ys)
            .set("shape", "circle")
            .set("size", 6.0_f64)
            .set("fill", rgb8(220, 60, 90))
            .set("stroke", rgb8(40, 40, 40))
            .build(),
    );
    plot.add_axis(Axis::rail("x", AxisPlacement::Cartesian(AxisSide::Bottom)).title("x"));
    plot.add_axis(Axis::rail("y", AxisPlacement::Cartesian(AxisSide::Left)).title("y"));

    let mut view = PlotComposition::new(&cell_comp())
        .add_scale("x", scale::continuous(0.0..=10.0))
        .add_scale("y", scale::continuous(0.0..=10.0))
        .with_plot(plot);
    panic_on_issues(view.validate());
    render_to(
        renderer,
        &mut view,
        w,
        h,
        dpi,
        bg,
        "examples/bspline_1_cubic_vs_bezier.png",
    );
}

// ── Render 2: parallel coordinates ──
//
// Five categorical x positions, several smoothed curves threading
// through them. Each curve is one mark with five control points; the
// degree-3 spline pulls the curve toward each interior point without
// passing through it.
fn parallel_coordinates(renderer: &mut VelloRenderer, w: u32, h: u32, dpi: f64, bg: Color) {
    let n_lines = 6;
    let n_axes = 5;
    let xs: Vec<f64> = (0..(n_lines * n_axes))
        .map(|i| (i % n_axes) as f64)
        .collect();
    let ys: Vec<f64> = (0..(n_lines * n_axes))
        .map(|i| {
            let line = i / n_axes;
            let axis = i % n_axes;
            let phase = line as f64 * 0.45;
            0.5 + 0.4 * ((axis as f64 + phase) * 1.3).sin()
        })
        .collect();
    let keys: Vec<i64> = (0..(n_lines * n_axes))
        .map(|i| (i / n_axes) as i64)
        .collect();

    let mut plot = Plot::new(&cell_comp(), "panel")
        .title("Parallel coordinates with B-spline smoothing")
        .subtitle("Each curve is one mark; control points are the axis values")
        .bind("x", "axis")
        .bind("y", "value")
        .bind("stroke", "line_id");
    plot.add_geom(
        BSplineGeom::builder()
            .keys(keys.clone())
            .set("x", xs)
            .set("y", ys)
            .set("stroke", keys)
            .set("linewidth", 2.0_f64)
            .set("alpha", 0.85_f64)
            .build(),
    );
    plot.add_axis(Axis::rail("axis", AxisPlacement::Cartesian(AxisSide::Bottom)).title("Axis"));
    plot.add_axis(Axis::rail("value", AxisPlacement::Cartesian(AxisSide::Left)).title("Value"));

    let palette: Vec<Color> = vec![
        rgb8(220, 60, 90),
        rgb8(60, 130, 200),
        rgb8(80, 170, 100),
        rgb8(200, 130, 40),
        rgb8(140, 90, 200),
        rgb8(40, 160, 170),
    ];
    let mut view = PlotComposition::new(&cell_comp())
        .add_scale("axis", scale::continuous(0.0..=(n_axes as f64 - 1.0)))
        .add_scale("value", scale::continuous(0.0..=1.0))
        .add_scale(
            "line_id",
            scale::ordinal((0..(n_lines as i64)).collect::<Vec<_>>()).range_colors(palette),
        )
        .with_plot(plot);
    panic_on_issues(view.validate());
    render_to(
        renderer,
        &mut view,
        w,
        h,
        dpi,
        bg,
        "examples/bspline_2_parallel_coords.png",
    );
}

// ── Render 3: polar interpolation modes side by side ──
//
// Two curves sharing a control polygon: one with "domain"
// interpolation (built in channel-fraction space; polar curvature
// bends the curve between knots), one with "panel" (control points
// projected first; the curve smooths between the projected pixel
// positions). Cartesian users see no difference; this is purely a
// non-Cartesian display.
fn polar_interpolation(renderer: &mut VelloRenderer, w: u32, h: u32, dpi: f64, bg: Color) {
    let thetas = [0.1_f64, 0.25, 0.5, 0.75, 0.9];
    let radii = [0.85_f64, 0.45, 0.85, 0.45, 0.85];
    let n = thetas.len();

    // Two marks sharing one control polygon: the first carries
    // "domain" interpolation, the second "panel". The mark id picks
    // both the colour binding ("mode" scale) and the per-mark
    // "interpolation" channel.
    let mut xs: Vec<f64> = Vec::with_capacity(2 * n);
    let mut ys: Vec<f64> = Vec::with_capacity(2 * n);
    let mut keys: Vec<&'static str> = Vec::with_capacity(2 * n);
    let mut interps: Vec<&'static str> = Vec::with_capacity(2 * n);
    for (mode, _) in [("domain", 0), ("panel", 1)] {
        for i in 0..n {
            xs.push(thetas[i] * std::f64::consts::TAU);
            ys.push(radii[i]);
            keys.push(mode);
            interps.push(mode);
        }
    }

    let mut plot = Plot::new(&cell_comp(), "panel")
        .projection(Projection::polar())
        .title("Polar B-spline — interpolation modes")
        .subtitle("Red: \"domain\" (curve faithful in data space). Blue: \"panel\" (curve smooths in pixel space).")
        .bind("x", "theta")
        .bind("y", "radius")
        .bind("stroke", "mode");
    plot.add_geom(
        BSplineGeom::builder()
            .keys(keys)
            .set("x", xs.clone())
            .set("y", ys.clone())
            .set("stroke", interps.clone())
            .set("interpolation", interps)
            .set("linewidth", 2.5_f64)
            .build(),
    );
    // Mark the five control points so the curves' relationship to the
    // polygon is visible.
    let ctrl_theta: Vec<f64> = thetas.iter().map(|t| t * std::f64::consts::TAU).collect();
    let ctrl_radius: Vec<f64> = radii.to_vec();
    plot.add_geom(
        hephaestus::plot::PointGeom::builder()
            .set("x", ctrl_theta)
            .set("y", ctrl_radius)
            .set("shape", "circle")
            .set("size", 6.0_f64)
            .set("fill", rgb8(40, 40, 40))
            .set("stroke", rgb8(40, 40, 40))
            .build(),
    );
    plot.add_axis(
        Axis::rail("theta", AxisPlacement::PolarAngular(PolarRing::Outer)).title("Theta"),
    );
    plot.add_axis(
        Axis::rail("radius", AxisPlacement::PolarRadius { theta_frac: 0.0 }).title("Radius"),
    );

    let mut view = PlotComposition::new(&cell_comp())
        .add_scale("theta", scale::continuous(0.0..=std::f64::consts::TAU))
        .add_scale("radius", scale::continuous(0.0..=1.0))
        .add_scale(
            "mode",
            scale::ordinal(["domain", "panel"])
                .range_colors([rgb8(220, 60, 90), rgb8(60, 130, 200)]),
        )
        .with_plot(plot);
    panic_on_issues(view.validate());
    render_to(
        renderer,
        &mut view,
        w,
        h,
        dpi,
        bg,
        "examples/bspline_3_polar_interpolation.png",
    );
}

// ── Render 4: endpoint arrows ──
fn endpoint_arrows(renderer: &mut VelloRenderer, w: u32, h: u32, dpi: f64, bg: Color) {
    let xs = vec![1.0_f64, 2.0, 6.0, 8.0, 9.0];
    let ys = vec![1.5_f64, 8.0, 2.0, 8.5, 4.0];

    let mut plot = Plot::new(&cell_comp(), "panel")
        .title("B-spline with endpoint markers")
        .subtitle("Stroke is trimmed automatically so the arrow tips land on the data endpoints")
        .bind("x", "x")
        .bind("y", "y");
    plot.add_geom(
        BSplineGeom::builder()
            .set("x", xs.clone())
            .set("y", ys.clone())
            .set("stroke", rgb8(40, 80, 170))
            .set("linewidth", 3.0_f64)
            .set("start_marker", "arrow-closed")
            .set("end_marker", "arrow-closed")
            .set("start_marker_size", 14.0_f64)
            .set("end_marker_size", 14.0_f64)
            .build(),
    );
    plot.add_geom(
        hephaestus::plot::PointGeom::builder()
            .set("x", xs)
            .set("y", ys)
            .set("shape", "circle")
            .set("size", 5.0_f64)
            .set("fill", rgb8(180, 180, 200))
            .set("stroke", rgb8(60, 60, 80))
            .build(),
    );
    plot.add_axis(Axis::rail("x", AxisPlacement::Cartesian(AxisSide::Bottom)).title("x"));
    plot.add_axis(Axis::rail("y", AxisPlacement::Cartesian(AxisSide::Left)).title("y"));

    let mut view = PlotComposition::new(&cell_comp())
        .add_scale("x", scale::continuous(0.0..=10.0))
        .add_scale("y", scale::continuous(0.0..=10.0))
        .with_plot(plot);
    panic_on_issues(view.validate());
    render_to(
        renderer,
        &mut view,
        w,
        h,
        dpi,
        bg,
        "examples/bspline_4_endpoint_arrows.png",
    );
}

// ── Render 5: per-row gradient + linewidth taper (ribbon-mode) ──
fn ribbon_gradient(renderer: &mut VelloRenderer, w: u32, h: u32, dpi: f64, bg: Color) {
    let n = 12;
    let xs: Vec<f64> = (0..n)
        .map(|i| i as f64 * (10.0 / (n as f64 - 1.0)))
        .collect();
    let ys: Vec<f64> = (0..n)
        .map(|i| {
            let t = i as f64 / (n as f64 - 1.0);
            5.0 + 3.5 * (t * std::f64::consts::TAU).sin()
        })
        .collect();
    let widths: Vec<f64> = (0..n)
        .map(|i| {
            let t = i as f64 / (n as f64 - 1.0);
            2.0 + 12.0 * t
        })
        .collect();
    let strokes: Vec<i64> = (0..n as i64).collect();

    let mut plot = Plot::new(&cell_comp(), "panel")
        .title("Ribbon-mode B-spline — per-row colour and width")
        .subtitle("Variance-detect upgrades to polyline_ribbon_full")
        .bind("x", "x")
        .bind("y", "y")
        .bind("stroke", "row");
    plot.add_geom(
        BSplineGeom::builder()
            .set("x", xs)
            .set("y", ys)
            .set("linewidth", widths)
            .set("stroke", strokes)
            .build(),
    );
    plot.add_axis(Axis::rail("x", AxisPlacement::Cartesian(AxisSide::Bottom)).title("x"));
    plot.add_axis(Axis::rail("y", AxisPlacement::Cartesian(AxisSide::Left)).title("y"));

    let mut view = PlotComposition::new(&cell_comp())
        .add_scale("x", scale::continuous(0.0..=10.0))
        .add_scale("y", scale::continuous(0.0..=10.0))
        .add_scale(
            "row",
            scale::continuous(0.0..=(n as f64 - 1.0))
                .range_colors([rgb8(240, 180, 60), rgb8(190, 50, 130)]),
        )
        .with_plot(plot);
    panic_on_issues(view.validate());
    render_to(
        renderer,
        &mut view,
        w,
        h,
        dpi,
        bg,
        "examples/bspline_5_ribbon_gradient.png",
    );
}

// ── Render 6: dashed + marker linetype ──
fn dashed_markers(renderer: &mut VelloRenderer, w: u32, h: u32, dpi: f64, bg: Color) {
    let xs = vec![1.0_f64, 3.0, 5.0, 7.0, 9.0];
    let ys = vec![2.0_f64, 7.0, 3.5, 8.0, 4.0];

    let pat = linetype::pattern([
        linetype::dash(6.0),
        linetype::gap(3.0),
        linetype::marker("circle"),
        linetype::gap(3.0),
    ]);

    let mut plot = Plot::new(&cell_comp(), "panel")
        .title("B-spline with dashed-plus-marker linetype")
        .subtitle("Pattern walked along the arc length of the flattened curve")
        .bind("x", "x")
        .bind("y", "y");
    plot.add_geom(
        BSplineGeom::builder()
            .set("x", xs)
            .set("y", ys)
            .set("stroke", rgb8(40, 60, 140))
            .set("linewidth", 3.0_f64)
            .set("linetype", Value::Linetype(pat))
            .build(),
    );
    plot.add_axis(Axis::rail("x", AxisPlacement::Cartesian(AxisSide::Bottom)).title("x"));
    plot.add_axis(Axis::rail("y", AxisPlacement::Cartesian(AxisSide::Left)).title("y"));

    let mut view = PlotComposition::new(&cell_comp())
        .add_scale("x", scale::continuous(0.0..=10.0))
        .add_scale("y", scale::continuous(0.0..=10.0))
        .with_plot(plot);
    panic_on_issues(view.validate());
    render_to(
        renderer,
        &mut view,
        w,
        h,
        dpi,
        bg,
        "examples/bspline_6_dashed_markers.png",
    );
}

// ── Helpers ──

fn panic_on_issues<T: std::fmt::Debug>(issues: Vec<T>) {
    if !issues.is_empty() {
        panic!("validate() reported issues: {issues:?}");
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
    write_buffer(renderer, w, h, bg, out_relative);
}

fn write_buffer(renderer: &mut VelloRenderer, w: u32, h: u32, bg: Color, out_relative: &str) {
    let mut pixels = vec![0u8; (w * h * 4) as usize];
    renderer
        .render_to_buffer(w, h, bg, &mut pixels)
        .expect("render");
    let path = std::env::current_dir().unwrap().join(out_relative);
    hephaestus::png::write_png(&path, w, h, &pixels).expect("write png");
    println!("wrote {}", path.display());
}
