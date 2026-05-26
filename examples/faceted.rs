//! Multi-plot example showcasing scale sharing across many plots in a
//! single composition.
//!
//! Layout (a flat 2×3 grid; the summary patch spans both rows of col 3):
//!
//!   ┌────┬────┬─────────┐
//!   │ q1 │ q2 │         │
//!   ├────┼────┤ summary │
//!   │ q3 │ q4 │         │
//!   └────┴────┴─────────┘
//!
//! Five plots total — four facets plus the summary. All five bind their
//! `"x"` channel to the same scale name `"time"`, so a single
//! `view.update_scale("time", |s| ...)` between renders updates every
//! panel at once. That's the headline ergonomic of the high-level API.
//!
//! Produces:
//! - `examples/faceted_1_initial.png`
//! - `examples/faceted_2_shared_zoom.png` (same data, narrowed shared x).

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{Composition, Patch, Span};
use hephaestus::geometry::Size;
use hephaestus::plot::{scale, Plot, PlotComposition, PointGeom};
use hephaestus::Renderer;

fn main() {
    let (w, h) = (1400u32, 700u32);
    let dpi = 96.0;

    // Layout: a flat 2×3 grid. Cells (1,1)..(2,2) are the four facets;
    // cell (1,3) holds the summary plot, spanning both rows.
    //
    // Nested compositions aren't directly placeable in v1 — the solver
    // requires `Patch::place_in_panel` for true nesting. Spans on a
    // single grid give the same visual shape without that machinery,
    // so the orchestrator's plot map can route mutations by a flat
    // patch id.
    let comp_shape = || {
        Composition::empty(2, 3)
            .place(1, 1, Span::cell(), Patch::new("q1"))
            .place(1, 2, Span::cell(), Patch::new("q2"))
            .place(2, 1, Span::cell(), Patch::new("q3"))
            .place(2, 2, Span::cell(), Patch::new("q4"))
            .place(1, 3, Span::rows(2), Patch::new("summary"))
    };

    let xs: Vec<f64> = (0..50).map(|i| i as f64 * 2.0).collect();

    // Per-facet y-data with different shapes — so we can see the
    // shared x-axis change without confusing per-facet y movement.
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

    // Build the orchestrator with one shared "time" scale and a single
    // shared "y" scale (so panels are directly comparable). Every plot
    // binds "x" → "time" and "y" → "y".
    let mut view = PlotComposition::new(comp_shape())
        .add_scale("time", scale::continuous(0.0..=100.0))
        .add_scale("y", scale::continuous(0.0..=100.0));

    for (id, ys, color) in &datasets {
        let mut p = Plot::new(&comp_shape(), *id)
            .bind("x", "time")
            .bind("y", "y");
        p.add_geom(
            PointGeom::builder()
                .set("x", xs.clone())
                .set("y", ys.clone())
                .set("fill", *color)
                .set("size", 4.0_f64)
                .build(),
        );
        #[cfg(feature = "text")]
        {
            p.set_title(*id);
        }
        view.attach_plot(p);
    }

    // Surface any binding issues up front.
    let issues = view.validate();
    if !issues.is_empty() {
        panic!("validate() reported issues: {issues:?}");
    }

    let mut renderer = VelloRenderer::new().expect("vello renderer init");
    let bg: Color = rgb8(248, 248, 252);

    // ── Render 1: full x range, all five plots share it ─────────────
    render_to(
        &mut renderer,
        &mut view,
        w,
        h,
        dpi,
        bg,
        "examples/faceted_1_initial.png",
    );

    // ── Render 2: zoom the shared "time" scale once → all five
    //    panels (4 facets + summary) update from one mutation. ───────
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
