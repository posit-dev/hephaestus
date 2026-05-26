//! End-to-end visual sanity for the high-level plot API.
//!
//! Builds a two-panel composition driven by `PlotComposition`. Both
//! panels share a single `"time"` scale, so mutating that scale once
//! updates both plots. Produces three PNGs in `examples/`:
//!
//! - `point_1_initial.png` — initial render of both plots.
//! - `point_2_shared_scale.png` — same data; the shared `"time"` domain
//!   has been narrowed via `view.update_scale(...)`. Both panels'
//!   x extents update from the single mutation.
//! - `point_3_data_replaced.png` — same shape; the price plot's geom has
//!   been replaced with a different dataset via `view.update_plot(...)`.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{beside, Patch};
use hephaestus::geometry::Size;
use hephaestus::plot::{scale, Plot, PlotComposition, PointGeom};
use hephaestus::Renderer;

fn main() {
    let (w, h) = (1200u32, 500u32);
    let dpi = 96.0;

    // Layout shape: two named patches side-by-side.
    let comp = || beside(Patch::new("price"), Patch::new("volume"));

    // Synthetic data.
    let xs: Vec<f64> = (0..40).map(|i| i as f64 * 2.5).collect();
    let ys_price: Vec<f64> = xs
        .iter()
        .map(|x| 50.0 + 20.0 * (x * 0.06).sin() + 0.1 * x)
        .collect();
    let ys_volume: Vec<f64> = xs
        .iter()
        .map(|x| 1.0e5 + 4.0e4 * (x * 0.04 + 1.0).cos().abs())
        .collect();

    // Two plots, both binding their "x" channel to the same scale name
    // — that's what makes the shared-scale mutation in render #2 update
    // both panels from a single call site.
    let mut plot_price = Plot::new(&comp(), "price")
        .bind("x", "time")
        .bind("y", "price_y");
    plot_price.add_geom(
        PointGeom::builder()
            .set("x", xs.clone())
            .set("y", ys_price.clone())
            .set("fill", rgb8(220, 90, 70))
            .set("size", 5.0_f64)
            .build(),
    );
    #[cfg(feature = "text")]
    {
        plot_price.set_title("Price");
    }

    let mut plot_volume = Plot::new(&comp(), "volume")
        .bind("x", "time")
        .bind("y", "volume_y");
    plot_volume.add_geom(
        PointGeom::builder()
            .set("x", xs.clone())
            .set("y", ys_volume.clone())
            .set("fill", rgb8(70, 120, 220))
            .set("size", 5.0_f64)
            .build(),
    );
    #[cfg(feature = "text")]
    {
        plot_volume.set_title("Volume");
    }

    let mut view = PlotComposition::new(comp())
        .add_scale("time", scale::continuous(0.0..=100.0))
        .add_scale("price_y", scale::continuous(40.0..=90.0))
        .add_scale("volume_y", scale::continuous(80_000.0..=160_000.0))
        .with_plot(plot_price)
        .with_plot(plot_volume);

    // Surface validation issues early — the plan calls these out as the
    // intended way to find binding mistakes.
    let issues = view.validate();
    if !issues.is_empty() {
        panic!("validate() reported issues: {issues:?}");
    }

    let mut renderer = VelloRenderer::new().expect("vello renderer init");
    let bg: Color = rgb8(248, 248, 252);

    // ── Render 1: initial ────────────────────────────────────────────
    render_to(
        &mut renderer,
        &mut view,
        w,
        h,
        dpi,
        bg,
        "examples/point_1_initial.png",
    );

    // ── Render 2: narrow the shared "time" scale ─────────────────────
    // Both plots see the change because both bind "x" → "time".
    view.update_scale("time", |s| s.set_domain_continuous(0.0, 50.0));
    render_to(
        &mut renderer,
        &mut view,
        w,
        h,
        dpi,
        bg,
        "examples/point_2_shared_scale.png",
    );

    // ── Render 3: replace the price plot's data ──────────────────────
    // Different y-values, same row count. Note: we wrap in update_plot
    // so the orchestrator's dirty bits are flipped accurately.
    let new_price: Vec<f64> = xs
        .iter()
        .map(|x| 50.0 + 30.0 * (x * 0.12).cos() - 0.05 * x)
        .collect();
    view.update_plot("price", |p| {
        // Replace the single geom by removing it and adding a new one.
        // (PointGeom::update lets you mutate in place; here we just
        // demonstrate the orchestrator's mutation closure.)
        let ids: Vec<_> = p.geom_ids().collect();
        for id in ids {
            p.remove_geom(id);
        }
        p.add_geom(
            PointGeom::builder()
                .set("x", xs.clone())
                .set("y", new_price.clone())
                .set("fill", rgb8(160, 70, 200))
                .set("size", 5.0_f64)
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
        "examples/point_3_data_replaced.png",
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
