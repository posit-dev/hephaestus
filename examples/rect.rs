//! End-to-end visual sanity for `RectGeom`. Three renders:
//!
//! - `rect_1_bars.png` — a categorical bar chart. Demonstrates the
//!   default `x_band = -0.5, x2_band = +0.5` band-filling behaviour:
//!   the user binds both `"x"` and `"x2"` to the same category column
//!   and the bars fill their bands without further configuration.
//! - `rect_2_dodged.png` — three groups dodged side-by-side within
//!   each band, demonstrating per-row band offsets.
//! - `rect_3_heatmap.png` — a continuous-x continuous-y heatmap-style
//!   grid of coloured rectangles with `corner_radius` rounding.

use std::sync::Arc;

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{Composition, Patch, Span};
use hephaestus::geometry::Size;
use hephaestus::plot::value::Value;
use hephaestus::plot::{scale, Plot, PlotComposition, RectGeom};
use hephaestus::Renderer;

fn main() {
    let (w, h) = (1200u32, 400u32);
    let dpi = 96.0;

    let comp = || Composition::empty(1, 1).place(1, 1, Span::cell(), Patch::new("panel"));

    let mut renderer = VelloRenderer::new().expect("vello renderer init");
    let bg: Color = rgb8(248, 248, 252);

    // ── Render 1: simple categorical bars ────────────────────────────
    {
        let cats: Vec<&str> = vec!["A", "B", "C", "D", "E"];
        let heights = vec![24.0_f64, 38.0, 17.0, 45.0, 30.0];
        let n = cats.len();
        let mut plot = Plot::new(&comp(), "panel")
            .bind("x", "category")
            .bind("x2", "category")
            .bind("y", "value")
            .bind("y2", "value");
        plot.add_geom(
            RectGeom::builder()
                .set("x", cats.clone())
                .set("x2", cats.clone())
                .set("y", vec![0.0_f64; n])
                .set("y2", heights.clone())
                .set("fill", rgb8(180, 90, 70))
                .build(),
        );
        let mut view = PlotComposition::new(comp())
            .add_scale(
                "category",
                scale::discrete(cats.iter().map(|c| Value::String(Arc::from(*c)))),
            )
            .add_scale("value", scale::continuous(0.0..=50.0))
            .with_plot(plot);
        render_to(
            &mut renderer,
            &mut view,
            w,
            h,
            dpi,
            bg,
            "examples/rect_1_bars.png",
        );
    }

    // ── Render 2: dodged bars ────────────────────────────────────────
    {
        let cats = ["A", "B", "C", "D"];
        let groups = ["lo", "mid", "hi"];
        let mut x: Vec<&str> = Vec::new();
        let mut y0: Vec<f64> = Vec::new();
        let mut y1: Vec<f64> = Vec::new();
        let mut x_band: Vec<f64> = Vec::new();
        let mut x2_band: Vec<f64> = Vec::new();
        let mut group_col: Vec<&str> = Vec::new();
        for cat in &cats {
            for (gi, group) in groups.iter().enumerate() {
                // Three dodge slots per band, even split.
                let lo = -0.5 + (gi as f64) / 3.0;
                let hi = -0.5 + (gi as f64 + 1.0) / 3.0;
                x.push(cat);
                group_col.push(group);
                y0.push(0.0);
                y1.push(10.0 + (gi as f64) * 12.0 + (cat.len() as f64));
                x_band.push(lo);
                x2_band.push(hi);
            }
        }
        let mut plot = Plot::new(&comp(), "panel")
            .bind("x", "category")
            .bind("x2", "category")
            .bind("y", "value")
            .bind("y2", "value")
            .bind("fill", "group");
        plot.add_geom(
            RectGeom::builder()
                .set("x", x.clone())
                .set("x2", x)
                .set("y", y0)
                .set("y2", y1)
                .set("x_band", x_band)
                .set("x2_band", x2_band)
                .set("fill", group_col)
                .build(),
        );
        let red = Color::new([0.85, 0.4, 0.4, 1.0]);
        let green = Color::new([0.4, 0.7, 0.4, 1.0]);
        let blue = Color::new([0.4, 0.55, 0.85, 1.0]);
        let mut view = PlotComposition::new(comp())
            .add_scale(
                "category",
                scale::discrete(cats.iter().map(|c| Value::String(Arc::from(*c)))),
            )
            .add_scale("value", scale::continuous(0.0..=50.0))
            .add_scale(
                "group",
                scale::ordinal(groups).range_colors([red, green, blue]),
            )
            .with_plot(plot);
        render_to(
            &mut renderer,
            &mut view,
            w,
            h,
            dpi,
            bg,
            "examples/rect_2_dodged.png",
        );
    }

    // ── Render 3: continuous heatmap with rounded corners ────────────
    {
        let cols = 12;
        let rows = 6;
        let mut x: Vec<f64> = Vec::new();
        let mut x2: Vec<f64> = Vec::new();
        let mut y: Vec<f64> = Vec::new();
        let mut y2: Vec<f64> = Vec::new();
        let mut fill: Vec<Color> = Vec::new();
        for r in 0..rows {
            for c in 0..cols {
                let cx = c as f64;
                let cy = r as f64;
                x.push(cx + 0.05);
                x2.push(cx + 0.95);
                y.push(cy + 0.05);
                y2.push(cy + 0.95);
                let t = ((cx / cols as f64) + (cy / rows as f64)) * 0.5;
                let t = t.clamp(0.0, 1.0) as f32;
                fill.push(Color::new([1.0 - t * 0.6, 0.55, 0.4 + t * 0.55, 1.0]));
            }
        }
        let mut plot = Plot::new(&comp(), "panel")
            .bind("x", "x_axis")
            .bind("x2", "x_axis")
            .bind("y", "y_axis")
            .bind("y2", "y_axis");
        plot.add_geom(
            RectGeom::builder()
                .set("x", x)
                .set("x2", x2)
                .set("y", y)
                .set("y2", y2)
                .set("fill", fill)
                .set("corner_radius", 3.0_f64)
                .build(),
        );
        let mut view = PlotComposition::new(comp())
            .add_scale("x_axis", scale::continuous(0.0..=cols as f64))
            .add_scale("y_axis", scale::continuous(0.0..=rows as f64))
            .with_plot(plot);
        render_to(
            &mut renderer,
            &mut view,
            w,
            h,
            dpi,
            bg,
            "examples/rect_3_heatmap.png",
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
