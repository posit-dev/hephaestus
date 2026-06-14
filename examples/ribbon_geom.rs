//! End-to-end visual sanity for `RibbonGeom` — filled band between two
//! curves along a shared axis. Per-mark grouping (rows sharing a key
//! form one band), variance-detect fill (uniform → solid; varying →
//! linear gradient along the shared axis), and independent outlines on
//! curve A vs curve B.
//!
//! Four renders, all with full plotting chrome (title, subtitle,
//! axes with titles):
//!
//! - `ribbon_geom_1_area.png` — horizontal area chart with default
//!   `y2 = 0` baseline; only the top curve is outlined (asymmetric
//!   outline case).
//! - `ribbon_geom_2_vertical.png` — vertical orientation (`y` shared,
//!   `x2 = 0` baseline); no outlines.
//! - `ribbon_geom_3_overlap.png` — two overlapping horizontal bands
//!   with explicit `y2`, per-row varying fill (linear gradient brush),
//!   and both top + bottom curves outlined.
//! - `ribbon_geom_4_polar.png` — polar annular segment confirming
//!   projection densification on both edges.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{Composition, Patch, Span};
use hephaestus::geometry::Size;
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement, PolarRing};
use hephaestus::plot::projection::Projection;
use hephaestus::plot::{scale, Plot, PlotComposition, RibbonGeom};
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

    // ── Render 1: horizontal area, default baseline, top curve outlined ──
    {
        let n = 80;
        let xs: Vec<f64> = (0..n).map(|i| i as f64).collect();
        let ys: Vec<f64> = (0..n)
            .map(|i| {
                let t = i as f64 / (n - 1) as f64;
                40.0 + 30.0 * (t * std::f64::consts::TAU).sin().abs()
            })
            .collect();
        let mut plot = Plot::new(&cell_comp(), "panel")
            .title("Area chart — default y2 = 0 baseline")
            .subtitle("Top curve outlined; baseline implicit")
            .bind("x", "time")
            .bind("y", "value");
        plot.add_geom(
            RibbonGeom::builder()
                .set("x", xs)
                .set("y", ys)
                .set("fill", rgb8(60, 130, 200))
                .set("alpha", 0.35_f64)
                .set("stroke", rgb8(20, 60, 130))
                .set("linewidth", 1.5_f64)
                .build(),
        );
        plot.add_axis(Axis::rail("time", AxisPlacement::Cartesian(AxisSide::Bottom)).title("Time"));
        plot.add_axis(Axis::rail("value", AxisPlacement::Cartesian(AxisSide::Left)).title("Value"));

        let mut view = PlotComposition::new(cell_comp())
            .add_scale("time", scale::continuous(0.0..=(n as f64 - 1.0)))
            .add_scale("value", scale::continuous(0.0..=80.0))
            .with_plot(plot);
        panic_on_issues(view.validate());
        render_to(
            &mut renderer,
            &mut view,
            w,
            h,
            dpi,
            bg,
            "examples/ribbon_geom_1_area.png",
        );
    }

    // ── Render 2: vertical area (`y` shared, `x2 = 0` baseline) ──
    {
        let n = 80;
        let ys: Vec<f64> = (0..n).map(|i| i as f64).collect();
        let xs: Vec<f64> = (0..n)
            .map(|i| {
                let t = i as f64 / (n - 1) as f64;
                20.0 + 30.0 * (t * std::f64::consts::PI).sin()
            })
            .collect();
        let mut plot = Plot::new(&cell_comp(), "panel")
            .title("Vertical area — x2 = 0 baseline")
            .subtitle("Band sweeps along y (presence of x2 selects vertical mode)")
            .bind("x", "x_axis")
            .bind("y", "y_axis")
            .bind("x2", "x_axis");
        plot.add_geom(
            RibbonGeom::builder()
                .set("x", xs)
                .set("y", ys)
                .set("x2", 0.0_f64)
                .set("fill", rgb8(200, 120, 60))
                .set("alpha", 0.5_f64)
                .build(),
        );
        plot.add_axis(
            Axis::rail("x_axis", AxisPlacement::Cartesian(AxisSide::Bottom)).title("Value"),
        );
        plot.add_axis(
            Axis::rail("y_axis", AxisPlacement::Cartesian(AxisSide::Left)).title("Depth"),
        );

        let mut view = PlotComposition::new(cell_comp())
            .add_scale("x_axis", scale::continuous(0.0..=60.0))
            .add_scale("y_axis", scale::continuous(0.0..=(n as f64 - 1.0)))
            .with_plot(plot);
        panic_on_issues(view.validate());
        render_to(
            &mut renderer,
            &mut view,
            w,
            h,
            dpi,
            bg,
            "examples/ribbon_geom_2_vertical.png",
        );
    }

    // ── Render 3: two overlapping bands with gradient fills + dual outlines ──
    {
        let n = 80;
        let mut xs: Vec<f64> = Vec::with_capacity(2 * n);
        let mut y_top: Vec<f64> = Vec::with_capacity(2 * n);
        let mut y_bot: Vec<f64> = Vec::with_capacity(2 * n);
        let mut groups: Vec<&'static str> = Vec::with_capacity(2 * n);
        let mut fills: Vec<Color> = Vec::with_capacity(2 * n);
        for i in 0..n {
            let t = i as f64 / (n - 1) as f64;
            xs.push(i as f64);
            y_top.push(60.0 + 8.0 * (t * std::f64::consts::TAU * 1.5).sin());
            y_bot.push(50.0 + 8.0 * (t * std::f64::consts::TAU * 1.5).sin());
            groups.push("A");
            fills.push(lerp_color(rgb8(40, 90, 200), rgb8(220, 70, 70), t));
        }
        for i in 0..n {
            let t = i as f64 / (n - 1) as f64;
            xs.push(i as f64);
            y_top.push(30.0 + 8.0 * (t * std::f64::consts::TAU).cos());
            y_bot.push(20.0 + 8.0 * (t * std::f64::consts::TAU).cos());
            groups.push("B");
            fills.push(lerp_color(rgb8(230, 200, 60), rgb8(60, 170, 90), t));
        }
        let mut plot = Plot::new(&cell_comp(), "panel")
            .title("Two bands with per-row gradient fills")
            .subtitle("Per-row fill variation upgrades to a linear gradient brush along x")
            .bind("x", "time")
            .bind("y", "value");
        plot.add_geom(
            RibbonGeom::builder()
                .keys(groups)
                .set("x", xs)
                .set("y", y_top)
                .set("y2", y_bot)
                .set("fill", fills)
                .set("alpha", 0.75_f64)
                .set("stroke", rgb8(30, 30, 60))
                .set("stroke2", rgb8(30, 30, 60))
                .set("linewidth", 1.2_f64)
                .set("linewidth2", 1.2_f64)
                .build(),
        );
        plot.add_axis(Axis::rail("time", AxisPlacement::Cartesian(AxisSide::Bottom)).title("Time"));
        plot.add_axis(Axis::rail("value", AxisPlacement::Cartesian(AxisSide::Left)).title("Value"));

        let mut view = PlotComposition::new(cell_comp())
            .add_scale("time", scale::continuous(0.0..=(n as f64 - 1.0)))
            .add_scale("value", scale::continuous(0.0..=80.0))
            .with_plot(plot);
        panic_on_issues(view.validate());
        render_to(
            &mut renderer,
            &mut view,
            w,
            h,
            dpi,
            bg,
            "examples/ribbon_geom_3_overlap.png",
        );
    }

    // ── Render 4: polar annular segment (densified edges) ──
    {
        let n = 60;
        let thetas: Vec<f64> = (0..n)
            .map(|i| {
                let t = i as f64 / (n - 1) as f64;
                t * std::f64::consts::TAU
            })
            .collect();
        let outer: Vec<f64> = (0..n)
            .map(|i| {
                let t = i as f64 / (n - 1) as f64;
                0.85 + 0.10 * (t * std::f64::consts::TAU * 3.0).sin()
            })
            .collect();
        let inner: Vec<f64> = (0..n).map(|_| 0.45_f64).collect();
        let mut plot = Plot::new(&cell_comp(), "panel")
            .projection(Projection::polar())
            .title("Polar annular ribbon")
            .subtitle("interpolate_segment densifies both edges into smooth arcs")
            .bind("x", "theta")
            .bind("y", "radius");
        plot.add_geom(
            RibbonGeom::builder()
                .set("x", thetas)
                .set("y", outer)
                .set("y2", inner)
                .set("fill", rgb8(140, 90, 200))
                .set("alpha", 0.6_f64)
                .set("stroke", rgb8(70, 40, 130))
                .set("stroke2", rgb8(70, 40, 130))
                .set("linewidth", 1.5_f64)
                .set("linewidth2", 1.5_f64)
                .build(),
        );
        plot.add_axis(
            Axis::rail("theta", AxisPlacement::PolarAngular(PolarRing::Outer)).title("Theta"),
        );
        plot.add_axis(
            Axis::rail("radius", AxisPlacement::PolarRadius { theta_frac: 0.0 }).title("Radius"),
        );

        let mut view = PlotComposition::new(cell_comp())
            .add_scale("theta", scale::continuous(0.0..=std::f64::consts::TAU))
            .add_scale("radius", scale::continuous(0.0..=1.0))
            .with_plot(plot);
        panic_on_issues(view.validate());
        render_to(
            &mut renderer,
            &mut view,
            w,
            h,
            dpi,
            bg,
            "examples/ribbon_geom_4_polar.png",
        );
    }
}

fn lerp_color(a: Color, b: Color, t: f64) -> Color {
    let t = t.clamp(0.0, 1.0) as f32;
    let ac = a.components;
    let bc = b.components;
    Color::new([
        ac[0] + (bc[0] - ac[0]) * t,
        ac[1] + (bc[1] - ac[1]) * t,
        ac[2] + (bc[2] - ac[2]) * t,
        ac[3] + (bc[3] - ac[3]) * t,
    ])
}

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
    let mut pixels = vec![0u8; (w * h * 4) as usize];
    renderer
        .render_to_buffer(w, h, bg, &mut pixels)
        .expect("render");
    let path = std::env::current_dir().unwrap().join(out_relative);
    hephaestus::png::write_png(&path, w, h, &pixels).expect("write png");
    println!("wrote {}", path.display());
}
