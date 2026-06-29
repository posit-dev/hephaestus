//! End-to-end visual sanity for `LineGeom`.
//!
//! Three renders:
//! - `line_1_basic.png` — three lines sharing one stroke colour, keyed
//!   by a `category` column.
//! - `line_2_linetype_scale.png` — same data, but each line's dash
//!   pattern is driven by an ordinal scale bound to `"linetype"`. One
//!   `bind("linetype", "linetype_scale")` produces three distinct
//!   patterns from the same data column.
//! - `line_3_shared_x_zoom.png` — narrows the shared `"time"` scale via
//!   `view.update_scale("time", …)`; all three lines update from a
//!   single mutation.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{Composition, Patch, Span};
use hephaestus::geometry::Size;
#[cfg(feature = "text")]
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement};
use hephaestus::plot::{linetype, scale, LineGeom, Plot, PlotComposition};
#[cfg(feature = "text")]
use hephaestus::scales::chrome::AxisSide;
use hephaestus::Renderer;

fn main() {
    let (w, h) = (1200u32, 500u32);
    let dpi = 96.0;

    let comp = || Composition::empty(1, 1).place(1, 1, Span::cell(), Patch::new("panel"));

    // Synthetic per-row data: three categories of 60 vertices each =
    // 180 rows total. Within each category x marches 0..=60, y carries
    // a category-specific trajectory.
    let n_per = 60;
    let cats = ["alpha", "beta", "gamma"];
    let mut xs: Vec<f64> = Vec::with_capacity(n_per * cats.len());
    let mut ys: Vec<f64> = Vec::with_capacity(n_per * cats.len());
    let mut groups: Vec<&'static str> = Vec::with_capacity(n_per * cats.len());
    for (k, cat) in cats.iter().enumerate() {
        let phase = k as f64 * 1.2;
        let amp = 20.0 + k as f64 * 6.0;
        for i in 0..n_per {
            let x = i as f64;
            xs.push(x);
            ys.push(50.0 + amp * (x * 0.12 + phase).sin());
            groups.push(cat);
        }
    }

    // ── Build the plot ───────────────────────────────────────────────
    let mut plot = Plot::new(&comp(), "panel")
        .bind("x", "time")
        .bind("y", "value");
    plot.add_geom(
        LineGeom::builder()
            .keys(groups.clone())
            .set("x", xs.clone())
            .set("y", ys.clone())
            .set("stroke", rgb8(180, 70, 90))
            .set("linewidth", 2.0_f64)
            .build(),
    );
    #[cfg(feature = "text")]
    {
        plot.add_axis(Axis::rail(
            "time",
            AxisPlacement::Cartesian(AxisSide::Bottom),
        ));
        plot.add_axis(Axis::rail(
            "value",
            AxisPlacement::Cartesian(AxisSide::Left),
        ));
    }

    let mut view = PlotComposition::new(&comp())
        .add_scale("time", scale::continuous(0.0..=60.0))
        .add_scale("value", scale::continuous(0.0..=100.0))
        .with_plot(plot);

    let issues = view.validate();
    if !issues.is_empty() {
        panic!("validate() reported issues: {issues:?}");
    }

    let mut renderer = VelloRenderer::new().expect("vello renderer init");
    let bg: Color = rgb8(248, 248, 252);

    // ── Render 1: basic three-line plot ──────────────────────────────
    render_to(
        &mut renderer,
        &mut view,
        w,
        h,
        dpi,
        bg,
        "examples/line_1_basic.png",
    );

    // ── Render 2: linetype scaled by category ────────────────────────
    // Add a Linetypes-output ordinal scale and bind the LineGeom's
    // "linetype" channel through it. The same `groups` column drives both
    // the mark identity (via `.keys(...)`) and the linetype lookup (via
    // `.set("linetype", ...)`).
    view.insert_scale(
        "linetype_scale",
        scale::ordinal(cats).range_linetypes([
            linetype::solid(),
            linetype::dashed(),
            linetype::dotted(),
        ]),
    );
    view.update_plot("panel", |p| {
        p.set_binding("linetype", "linetype_scale");
        let ids: Vec<_> = p.geom_ids().collect();
        for id in ids {
            p.remove_geom(id);
        }
        p.add_geom(
            LineGeom::builder()
                .keys(groups.clone())
                .set("x", xs.clone())
                .set("y", ys.clone())
                .set("stroke", rgb8(60, 80, 180))
                .set("linewidth", 2.0_f64)
                .set("linetype", groups.clone())
                .build(),
        );
    });
    render_to(
        &mut renderer,
        &mut view,
        w,
        h,
        dpi,
        bg,
        "examples/line_2_linetype_scale.png",
    );

    // ── Render 3: zoom the shared "time" scale ───────────────────────
    view.update_scale("time", |s| s.set_domain_continuous(15.0, 35.0));
    render_to(
        &mut renderer,
        &mut view,
        w,
        h,
        dpi,
        bg,
        "examples/line_3_shared_x_zoom.png",
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
