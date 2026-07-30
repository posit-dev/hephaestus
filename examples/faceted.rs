//! Multi-plot example: scale sharing across many plots in a single
//! composition, layered on top of real nesting.
//!
//! Layout: an outer composition wraps a 2×2 inner facet grid `beside` a
//! column-spanning summary patch.
//!
//! ```text
//!   ┌─────────────┬─────────────┐
//!   │   q1    q2  │             │
//!   │             │   summary   │
//!   │   q3    q4  │             │
//!   └─────────────┴─────────────┘
//! ```
//!
//! Five plots total — four facets plus the summary. All five bind their
//! `"x"` channel to the same scale name `"time"`, so a single
//! `view.update_scale("time", |s| ...)` between renders updates every
//! panel at once.
//!
//! Produces:
//! - `examples/faceted_1_initial.png` — full x range, all five panels.
//! - `examples/faceted_2_shared_zoom.png` — shared x narrowed once;
//!   propagates to every plot.
//! - `examples/faceted_3_aspect_locked.png` — outer composition gains
//!   `.aspect(1, 1)`; selective-respect under nesting locks each leaf
//!   panel to 1:1 while the surrounding row/col tracks absorb slack.
//!
//! Renders 1 & 2 also carry **composition-level chrome** — a title,
//! subtitle, caption, one shared bottom axis title, and a single legend
//! — set directly on `PlotComposition`. Each spans the whole five-panel
//! layout rather than repeating per patch.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{beside, grid, Composition, Element, Patch};
use hephaestus::geometry::Size;
#[cfg(feature = "text")]
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement};
#[cfg(feature = "text")]
use hephaestus::plot::chrome::legend::{Legend, LegendKeySpec};
use hephaestus::plot::{scale, Plot, PlotComposition, PointGeom};
#[cfg(feature = "text")]
use hephaestus::scales::chrome::{AxisSide, LegendSide};
#[cfg(feature = "text")]
use hephaestus::scales::value::Value;
use hephaestus::Renderer;

fn comp_shape(aspect: Option<(f32, f32)>) -> Composition {
    let facets: Vec<Element> = ["q1", "q2", "q3", "q4"]
        .into_iter()
        .map(|id| Patch::new(id).into())
        .collect();
    let inner_2x2 = grid(2, 2, facets);
    let outer = beside(inner_2x2, Patch::new("summary"));
    match aspect {
        Some((aw, ah)) => outer.aspect(aw, ah),
        None => outer,
    }
}

fn main() {
    let (w, h) = (1400u32, 700u32);
    let dpi = 96.0;

    let xs: Vec<f64> = (0..50).map(|i| i as f64 * 2.0).collect();
    let make = |phase: f64, amp: f64| -> Vec<f64> {
        xs.iter()
            .map(|x| 50.0 + amp * (x * 0.05 + phase).sin())
            .collect()
    };

    let datasets = [
        ("q1", make(0.0, 25.0), rgb8(220, 90, 70)),
        ("q2", make(1.5, 18.0), rgb8(70, 120, 220)),
        ("q3", make(3.0, 22.0), rgb8(70, 180, 120)),
        ("q4", make(4.5, 28.0), rgb8(180, 130, 80)),
        ("summary", make(0.0, 35.0), rgb8(130, 80, 180)),
    ];

    let mut renderer = VelloRenderer::new().expect("vello renderer init");
    let bg: Color = rgb8(248, 248, 252);

    // ── Renders 1 & 2: shared "time" scale across the unlocked layout
    {
        #[allow(unused_mut)]
        let mut view = PlotComposition::new(&comp_shape(None))
            .add_scale("time", scale::continuous(0.0..=100.0))
            .add_scale("y", scale::continuous(0.0..=100.0))
            .title("Sensor array")
            .subtitle("Four quadrants and a summary, sharing one time scale")
            .caption("Composition-level chrome spans every panel");
        attach_all(&mut view, &xs, &datasets);

        // One axis title and one legend for the whole grid, set on the
        // composition rather than on any single plot. The legend reads
        // its rows from the "series" scale's domain; no plot needs to
        // bind that scale for the legend to resolve it.
        #[cfg(feature = "text")]
        let mut view = {
            let series: Vec<Value> = datasets.iter().map(|(id, _, _)| Value::from(*id)).collect();
            let colors: Vec<Color> = datasets.iter().map(|(_, _, c)| *c).collect();
            let mut view = view
                .add_scale("series", scale::discrete(series).range_colors(colors))
                .axis_title(AxisSide::Bottom, "Time (s)");
            view.add_legend(
                Legend::new("series")
                    .side(LegendSide::Right)
                    .title("Series")
                    .key(
                        LegendKeySpec::point()
                            .scaled("fill", "series")
                            .fixed("size", 6.0_f64),
                    ),
            );
            view
        };

        let issues = view.validate();
        if !issues.is_empty() {
            panic!("validate() reported issues: {issues:?}");
        }

        render_to(
            &mut renderer,
            &mut view,
            w,
            h,
            dpi,
            bg,
            "examples/faceted_1_initial.png",
        );

        view.update_scale("time", |s| s.set_domain_continuous(20.0, 60.0));
        render_to(
            &mut renderer,
            &mut view,
            w,
            h,
            dpi,
            bg,
            "examples/faceted_2_shared_zoom.png",
        );
    }

    // ── Render 3: aspect-locked. Outer `.aspect(1, 1)` propagates to
    //    every leaf panel; selective respect on the layout solver
    //    couples panel col/row at the locked ratio and lets unmarked
    //    fr tracks absorb slack. Wider viewport (1800×600) makes the
    //    lock visually obvious — without it, the 1×2 outer would give
    //    half the width to each side; with it, the leaf panels land
    //    at the locked ratio and the surrounding tracks soak up the
    //    horizontal slack.
    {
        let (lw, lh) = (1800u32, 600u32);
        let mut view = PlotComposition::new(&comp_shape(Some((1.0, 1.0))))
            .add_scale("time", scale::continuous(20.0..=60.0))
            .add_scale("y", scale::continuous(0.0..=100.0));
        attach_all(&mut view, &xs, &datasets);
        render_to(
            &mut renderer,
            &mut view,
            lw,
            lh,
            dpi,
            bg,
            "examples/faceted_3_aspect_locked.png",
        );
    }
}

fn attach_all(view: &mut PlotComposition, xs: &[f64], datasets: &[(&str, Vec<f64>, Color)]) {
    // `Plot::new` reads the composition only to check that the patch id
    // exists, so one shape serves every plot here. A plot's own aspect
    // lock would come from `Plot::aspect_ratio`; render 3 instead locks
    // the outer composition and lets it cascade to the leaves.
    let shape = comp_shape(None);
    for (id, ys, color) in datasets {
        let mut p = Plot::new(&shape, *id).bind("x", "time").bind("y", "y");
        p.add_geom(
            PointGeom::builder()
                .set("x", xs.to_vec())
                .set("y", ys.clone())
                .set("fill", *color)
                .set("size", 4.0_f64)
                .build(),
        );
        #[cfg(feature = "text")]
        {
            p.add_axis(Axis::rail(
                "time",
                AxisPlacement::Cartesian(AxisSide::Bottom),
            ));
            p.add_axis(Axis::rail("y", AxisPlacement::Cartesian(AxisSide::Left)));
        }
        view.attach_plot(p);
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
