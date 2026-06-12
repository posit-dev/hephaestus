//! End-to-end visual sanity for `TextGeom`. Four renders:
//!
//! - `text_1_labels.png` — scatter points with labels.
//! - `text_2_alignment.png` — 3×3 grid demonstrating anchor placement.
//! - `text_3_badges.png` — labels with rounded background rects
//!   ("badges" / "callouts") sitting at scatter data points.
//! - `text_4_wrapped.png` — multi-line labels that soft-wrap to fit
//!   within their categorical bands, using `width_band = 1.0`.

use std::sync::Arc;

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{Composition, Patch, Span};
use hephaestus::geometry::Size;
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement};
use hephaestus::plot::value::Value;
use hephaestus::plot::{scale, Plot, PlotComposition, PointGeom, TextGeom};
use hephaestus::scales::chrome::AxisSide;
use hephaestus::Renderer;

fn main() {
    let (w, h) = (1200u32, 500u32);
    let dpi = 96.0;

    let comp = || Composition::empty(1, 1).place(1, 1, Span::cell(), Patch::new("panel"));

    let mut renderer = VelloRenderer::new().expect("vello renderer init");
    let bg: Color = rgb8(248, 248, 252);

    // ── Render 1: scatter + labels ───────────────────────────────────
    {
        let labels: &[&str] = &["A", "B", "C", "D", "E", "F", "G", "H"];
        let n = labels.len();
        let xs: Vec<f64> = (0..n).map(|i| (i as f64) * 12.0 + 8.0).collect();
        let ys: Vec<f64> = xs.iter().map(|x| 50.0 + 20.0 * (x * 0.07).sin()).collect();

        let mut plot = Plot::new(&comp(), "panel")
            .bind("x", "x_axis")
            .bind("y", "y_axis");

        // Points first.
        plot.add_geom(
            PointGeom::builder()
                .set("x", xs.clone())
                .set("y", ys.clone())
                .set("fill", rgb8(220, 90, 70))
                .set("size", 7.0_f64)
                .build(),
        );

        // Labels above each point.
        plot.add_geom(
            TextGeom::builder()
                .set("x", xs)
                .set("y", ys)
                .set("text", labels.to_vec())
                .set("size", 13.0_f64)
                .set("weight", 600.0_f64)
                .set("fill", rgb8(30, 30, 30))
                .set("anchor_x", 0.5_f64)
                .set("anchor_y", 1.0_f64) // anchor at bottom edge of label
                .set("y_offset", 10.0_f64) // 10pt above the point
                .build(),
        );

        plot.add_axis(Axis::rail(
            "x_axis",
            AxisPlacement::Cartesian(AxisSide::Bottom),
        ));
        plot.add_axis(Axis::rail(
            "y_axis",
            AxisPlacement::Cartesian(AxisSide::Left),
        ));

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
            "examples/text_1_labels.png",
        );
    }

    // ── Render 2: anchor alignment grid ──────────────────────────────
    {
        // 3×3 grid of anchor points; each point's label demonstrates a
        // different anchor placement. The label text reads as the
        // (anchor_x, anchor_y) values, drawn relative to its red dot.
        // The label position around the dot makes the convention visible:
        //   "TL" sits below-right of its dot (anchor at top-left of label)
        //   "TR" sits below-left of its dot (anchor at top-right of label)
        //   "BL" sits above-right of its dot
        //   "BR" sits above-left of its dot
        //   etc.
        let ax_choices = [0.0_f64, 0.5, 1.0];
        let ay_choices = [0.0_f64, 0.5, 1.0];
        let labels = [["TL", "TC", "TR"], ["ML", "MC", "MR"], ["BL", "BC", "BR"]];
        let cell_xs = [25.0_f64, 50.0, 75.0];
        let cell_ys = [75.0_f64, 50.0, 25.0]; // row 0 = top of plot (y=75)

        let mut xs: Vec<f64> = Vec::new();
        let mut ys: Vec<f64> = Vec::new();
        let mut text_col: Vec<&'static str> = Vec::new();
        let mut ax_col: Vec<f64> = Vec::new();
        let mut ay_col: Vec<f64> = Vec::new();
        for (i, ay) in ay_choices.iter().enumerate() {
            for (j, ax) in ax_choices.iter().enumerate() {
                xs.push(cell_xs[j]);
                ys.push(cell_ys[i]);
                text_col.push(labels[i][j]);
                ax_col.push(*ax);
                ay_col.push(*ay);
            }
        }

        let mut plot = Plot::new(&comp(), "panel")
            .bind("x", "x_axis")
            .bind("y", "y_axis");

        // Red dots at each grid anchor point.
        plot.add_geom(
            PointGeom::builder()
                .set("x", xs.clone())
                .set("y", ys.clone())
                .set("fill", rgb8(220, 70, 70))
                .set("size", 6.0_f64)
                .build(),
        );

        // One label per dot demonstrating its anchor placement.
        plot.add_geom(
            TextGeom::builder()
                .set("x", xs)
                .set("y", ys)
                .set("text", text_col)
                .set("anchor_x", ax_col)
                .set("anchor_y", ay_col)
                .set("size", 16.0_f64)
                .set("weight", 600.0_f64)
                .set("fill", rgb8(30, 30, 40))
                .build(),
        );

        plot.add_axis(Axis::rail(
            "x_axis",
            AxisPlacement::Cartesian(AxisSide::Bottom),
        ));
        plot.add_axis(Axis::rail(
            "y_axis",
            AxisPlacement::Cartesian(AxisSide::Left),
        ));

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
            "examples/text_2_alignment.png",
        );
    }

    // ── Render 3: badges (labels with background rects) ──────────────
    {
        let labels: &[&str] = &["alpha", "beta", "gamma", "delta", "epsilon"];
        let n = labels.len();
        let xs: Vec<f64> = (0..n).map(|i| (i as f64) * 18.0 + 10.0).collect();
        let ys: Vec<f64> = xs.iter().map(|x| 50.0 + 18.0 * (x * 0.05).sin()).collect();

        // Three colour palettes — one per category in a rotation.
        let palette = [
            (rgb8(240, 210, 200), rgb8(170, 80, 70)),
            (rgb8(210, 230, 240), rgb8(70, 110, 170)),
            (rgb8(210, 240, 220), rgb8(70, 140, 90)),
            (rgb8(245, 235, 210), rgb8(170, 130, 60)),
            (rgb8(235, 215, 240), rgb8(140, 80, 170)),
        ];
        let bg_fills: Vec<Color> = (0..n).map(|i| palette[i % palette.len()].0).collect();
        let strokes: Vec<Color> = (0..n).map(|i| palette[i % palette.len()].1).collect();

        let mut plot = Plot::new(&comp(), "panel")
            .bind("x", "x_axis")
            .bind("y", "y_axis");

        // Anchor dots.
        plot.add_geom(
            PointGeom::builder()
                .set("x", xs.clone())
                .set("y", ys.clone())
                .set("fill", rgb8(40, 40, 40))
                .set("size", 5.0_f64)
                .build(),
        );

        // Badge labels sitting just above each dot.
        plot.add_geom(
            TextGeom::builder()
                .set("x", xs)
                .set("y", ys)
                .set("text", labels.to_vec())
                .set("size", 12.0_f64)
                .set("weight", 600.0_f64)
                .set("fill", strokes.clone()) // text colour matches stroke
                .set("bg_fill", bg_fills)
                .set("bg_stroke", strokes)
                .set("bg_linewidth", 1.0_f64)
                .set("bg_padding", 4.0_f64)
                .set("bg_corner_radius", 4.0_f64)
                .set("anchor_x", 0.5_f64)
                .set("anchor_y", 1.0_f64) // bottom of badge at anchor
                .set("y_offset", 10.0_f64) // 10pt above the dot
                .build(),
        );

        plot.add_axis(Axis::rail(
            "x_axis",
            AxisPlacement::Cartesian(AxisSide::Bottom),
        ));
        plot.add_axis(Axis::rail(
            "y_axis",
            AxisPlacement::Cartesian(AxisSide::Left),
        ));

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
            "examples/text_3_badges.png",
        );
    }

    // ── Render 4: wrapped labels inside categorical bands ────────────
    {
        let cats = ["one", "two", "three", "four"];
        let texts = [
            "Short text",
            "Medium-length descriptive text that needs wrapping",
            "Even longer body text demonstrating that lines break neatly at word boundaries within the cell",
            "A few words",
        ];

        let mut plot = Plot::new(&comp(), "panel")
            .bind("x", "category")
            .bind("y", "y_axis");

        // Background rect per cell using width_band = 1.0 to fill bands.
        // Padding lets us see the inset effect.
        plot.add_geom(
            TextGeom::builder()
                .set("x", cats.to_vec())
                .set("y", vec![0.5_f64; cats.len()])
                .set("text", texts.to_vec())
                .set("size", 13.0_f64)
                .set("width_band", 1.0_f64)
                .set("width", -16.0_f64) // shrink by 8pt on each side for inset
                .set("anchor_x", 0.5_f64)
                .set("anchor_y", 0.5_f64)
                .set("fill", rgb8(30, 30, 40))
                .set("bg_fill", rgb8(240, 235, 220))
                .set("bg_stroke", rgb8(180, 160, 130))
                .set("bg_linewidth", 1.0_f64)
                .set("bg_padding", 8.0_f64)
                .set("bg_corner_radius", 6.0_f64)
                .build(),
        );

        plot.add_axis(Axis::rail(
            "category",
            AxisPlacement::Cartesian(AxisSide::Bottom),
        ));
        plot.add_axis(Axis::rail(
            "y_axis",
            AxisPlacement::Cartesian(AxisSide::Left),
        ));

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
            "examples/text_4_wrapped.png",
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
