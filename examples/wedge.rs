//! End-to-end visual sanity for `WedgeGeom`. Two renders:
//!
//! - `wedge_1_pie.png` — a single donut/pie chart centred in the panel.
//!   Five slices share a centre; `radius2 > 0` makes it a donut.
//! - `wedge_2_pies_as_glyphs.png` — the user's headline use case:
//!   small pie charts as glyphs at Cartesian data points. Each data
//!   point gets a pie chart drawn at its (x, y) with a fixed-pt radius.
//!   Demonstrates that the wedge has no special polar treatment — it's
//!   just a shape parameterised by polar dims, positioned in Cartesian
//!   space like any other geom.
//!
//! Angles use math convention: 0 is along +x (3 o'clock), positive
//! angles sweep counter-clockwise as the user sees them.

use std::f64::consts::TAU;
use std::sync::Arc;

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{Composition, Patch, Span};
use hephaestus::geometry::Size;
#[cfg(feature = "text")]
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement};
use hephaestus::plot::value::Value;
use hephaestus::plot::{scale, Plot, PlotComposition, WedgeGeom};
#[cfg(feature = "text")]
use hephaestus::scales::chrome::AxisSide;
use hephaestus::Renderer;

fn main() {
    let (w, h) = (1200u32, 500u32);
    let dpi = 96.0;

    let comp = || Composition::empty(1, 1).place(1, 1, Span::cell(), Patch::new("panel"));

    let mut renderer = VelloRenderer::new().expect("vello renderer init");
    let bg: Color = rgb8(248, 248, 252);

    // ── Render 1: a single donut chart, 5 slices ─────────────────────
    {
        let proportions: &[f64] = &[0.30, 0.22, 0.18, 0.18, 0.12];
        let labels: &[&str] = &["A", "B", "C", "D", "E"];

        // Compute theta_start / theta_end for each slice. Math
        // convention: start at PI/2 (12 o'clock) and sweep CCW (i.e.,
        // toward 9 o'clock = PI). For a "12 o'clock origin, clockwise
        // visual flow" pie, decrease the angle as we progress through
        // proportions.
        let start = std::f64::consts::PI * 0.5; // 12 o'clock
        let mut theta_start = Vec::with_capacity(proportions.len());
        let mut theta_end = Vec::with_capacity(proportions.len());
        let mut acc = 0.0_f64;
        for p in proportions {
            // Sweep CW visually means decrease in math convention.
            let s = start - acc * TAU;
            let e = start - (acc + p) * TAU;
            theta_start.push(s);
            theta_end.push(e);
            acc += p;
        }

        let palette: Vec<&str> = labels.to_vec();
        let red = Color::new([0.85, 0.4, 0.4, 1.0]);
        let blue = Color::new([0.4, 0.55, 0.85, 1.0]);
        let green = Color::new([0.4, 0.7, 0.4, 1.0]);
        let gold = Color::new([0.85, 0.7, 0.3, 1.0]);
        let purple = Color::new([0.7, 0.4, 0.7, 1.0]);

        let mut plot = Plot::new(&comp(), "panel")
            .bind("x", "x_axis")
            .bind("y", "y_axis")
            .bind("fill", "slice_color");
        plot.add_geom(
            WedgeGeom::builder()
                .set("x", vec![0.5_f64; proportions.len()])
                .set("y", vec![0.5_f64; proportions.len()])
                .set("radius", vec![100.0_f64; proportions.len()])
                .set("radius2", vec![45.0_f64; proportions.len()])
                .set("theta", theta_start)
                .set("theta2", theta_end)
                .set("fill", palette)
                .set("stroke", rgb8(30, 30, 30))
                .set("linewidth", 1.5_f64)
                .build(),
        );
        #[cfg(feature = "text")]
        {
            plot.add_axis(Axis::rail(
                "x_axis",
                AxisPlacement::Cartesian(AxisSide::Bottom),
            ));
            plot.add_axis(Axis::rail(
                "y_axis",
                AxisPlacement::Cartesian(AxisSide::Left),
            ));
        }
        let mut view = PlotComposition::new(comp())
            .add_scale("x_axis", scale::continuous(0.0..=1.0))
            .add_scale("y_axis", scale::continuous(0.0..=1.0))
            .add_scale(
                "slice_color",
                scale::ordinal(labels.iter().map(|s| Value::String(Arc::from(*s))))
                    .range_colors([red, blue, green, gold, purple]),
            )
            .with_plot(plot);
        render_to(
            &mut renderer,
            &mut view,
            w,
            h,
            dpi,
            bg,
            "examples/wedge_1_pie.png",
        );
    }

    // ── Render 2: small pies as glyphs at scatter points ─────────────
    {
        // 8 data points along a horizontal axis. Each carries a pie
        // chart with 3 slices whose proportions vary across the data.
        let n_points = 8;
        let xs_data: Vec<f64> = (0..n_points).map(|i| (i as f64) * 12.0 + 8.0).collect();
        let ys_data: Vec<f64> = xs_data
            .iter()
            .map(|x| 50.0 + 10.0 * (x * 0.05).sin())
            .collect();

        let slice_red = Color::new([0.85, 0.4, 0.4, 1.0]);
        let slice_blue = Color::new([0.4, 0.55, 0.85, 1.0]);
        let slice_green = Color::new([0.4, 0.7, 0.4, 1.0]);
        let slice_colors = [slice_red, slice_blue, slice_green];

        // Three slices per data point, three rows per glyph. We
        // replicate (x, y) three times per point.
        let mut x_col: Vec<f64> = Vec::new();
        let mut y_col: Vec<f64> = Vec::new();
        let mut radius_col: Vec<f64> = Vec::new();
        let mut theta_col: Vec<f64> = Vec::new();
        let mut theta2_col: Vec<f64> = Vec::new();
        let mut fill_col: Vec<Color> = Vec::new();
        for i in 0..n_points {
            // Per-point proportions that vary across the dataset.
            let t = (i as f64) / ((n_points - 1) as f64); // 0..=1
            let p0 = 0.20 + 0.40 * t;
            let p1 = 0.30 - 0.20 * t;
            let p2 = 1.0 - p0 - p1;
            let proportions = [p0, p1, p2];
            let start = std::f64::consts::PI * 0.5; // 12 o'clock
            let mut acc = 0.0_f64;
            for (k, p) in proportions.iter().enumerate() {
                x_col.push(xs_data[i]);
                y_col.push(ys_data[i]);
                radius_col.push(14.0); // 14pt radius
                let s = start - acc * TAU;
                let e = start - (acc + p) * TAU;
                theta_col.push(s);
                theta2_col.push(e);
                fill_col.push(slice_colors[k]);
                acc += p;
            }
        }

        let mut plot = Plot::new(&comp(), "panel")
            .bind("x", "x_axis")
            .bind("y", "y_axis");
        plot.add_geom(
            WedgeGeom::builder()
                .set("x", x_col)
                .set("y", y_col)
                .set("radius", radius_col)
                .set("theta", theta_col)
                .set("theta2", theta2_col)
                .set("fill", fill_col)
                .set("stroke", rgb8(60, 60, 60))
                .set("linewidth", 0.5_f64)
                .build(),
        );

        #[cfg(feature = "text")]
        {
            plot.add_axis(Axis::rail(
                "x_axis",
                AxisPlacement::Cartesian(AxisSide::Bottom),
            ));
            plot.add_axis(Axis::rail(
                "y_axis",
                AxisPlacement::Cartesian(AxisSide::Left),
            ));
        }

        let mut view = PlotComposition::new(comp())
            .add_scale("x_axis", scale::continuous(0.0..=100.0))
            .add_scale("y_axis", scale::continuous(0.0..=100.0))
            .with_plot(plot);
        render_to(
            &mut renderer,
            &mut view,
            w,
            h,
            dpi,
            bg,
            "examples/wedge_2_pies_as_glyphs.png",
        );
    }

    // ── Render 3: pies sized to fit their categorical bands ─────────
    {
        // Five categories on a discrete x scale; each gets a pie chart
        // sized to fit half its band (radius = 0.45 * band_width, with
        // a small pt margin to keep neighbouring pies from touching).
        // Three slices per pie, proportions varying across categories.
        let cats = ["Q1", "Q2", "Q3", "Q4", "Q5"];
        let slice_red = Color::new([0.85, 0.4, 0.4, 1.0]);
        let slice_blue = Color::new([0.4, 0.55, 0.85, 1.0]);
        let slice_green = Color::new([0.4, 0.7, 0.4, 1.0]);
        let slice_colors = [slice_red, slice_blue, slice_green];

        let mut x_col: Vec<&str> = Vec::new();
        let mut y_col: Vec<f64> = Vec::new();
        let mut radius_band_col: Vec<f64> = Vec::new();
        let mut radius_pt_col: Vec<f64> = Vec::new();
        let mut theta_col: Vec<f64> = Vec::new();
        let mut theta2_col: Vec<f64> = Vec::new();
        let mut fill_col: Vec<Color> = Vec::new();
        for (i, cat) in cats.iter().enumerate() {
            let t = (i as f64) / ((cats.len() - 1) as f64);
            let p0 = 0.20 + 0.40 * t;
            let p1 = 0.50 - 0.30 * t;
            let p2 = 1.0 - p0 - p1;
            let proportions = [p0, p1, p2];
            let start = std::f64::consts::PI * 0.5;
            let mut acc = 0.0_f64;
            for (k, p) in proportions.iter().enumerate() {
                x_col.push(cat);
                y_col.push(0.5);
                radius_band_col.push(0.45); // 45% of band → leaves margin
                radius_pt_col.push(-2.0); // pt margin to prevent touching
                let s = start - acc * TAU;
                let e = start - (acc + p) * TAU;
                theta_col.push(s);
                theta2_col.push(e);
                fill_col.push(slice_colors[k]);
                acc += p;
            }
        }

        let mut plot = Plot::new(&comp(), "panel")
            .bind("x", "category")
            .bind("y", "y_axis");
        plot.add_geom(
            WedgeGeom::builder()
                .set("x", x_col)
                .set("y", y_col)
                .set("radius", radius_pt_col)
                .set("radius_band", radius_band_col)
                .set("theta", theta_col)
                .set("theta2", theta2_col)
                .set("fill", fill_col)
                .set("stroke", rgb8(60, 60, 60))
                .set("linewidth", 0.75_f64)
                .build(),
        );

        #[cfg(feature = "text")]
        {
            plot.add_axis(Axis::rail(
                "category",
                AxisPlacement::Cartesian(AxisSide::Bottom),
            ));
            plot.add_axis(Axis::rail(
                "y_axis",
                AxisPlacement::Cartesian(AxisSide::Left),
            ));
        }

        let mut view = PlotComposition::new(comp())
            .add_scale(
                "category",
                scale::discrete(cats.iter().map(|s| Value::String(Arc::from(*s)))),
            )
            .add_scale("y_axis", scale::continuous(0.0..=1.0))
            .with_plot(plot);
        render_to(
            &mut renderer,
            &mut view,
            w,
            h,
            dpi,
            bg,
            "examples/wedge_3_band_sized.png",
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
