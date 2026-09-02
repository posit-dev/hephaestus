//! End-to-end visual sanity for `TextPathGeom`. Three renders:
//!
//! - `text_path_1_sine.png` — three sine waves with the same label,
//!   demonstrating `justify_x = 0`, `0.5`, `1.0` packing along the path.
//! - `text_path_2_upright.png` — circular paths with `upright = false`
//!   (text upside-down on the bottom half) vs. `upright = true` (each
//!   glyph always reads right-side-up).
//! - `text_path_3_anchor_y.png` — straight horizontal paths with
//!   `anchor_y = 1`, `0.5`, `0` showing how the anchor fraction moves
//!   the text across the curve
//!   relative to the curve.
//! - `text_path_4_markdown.png` — markdown labels on sine waves:
//!   inline emphasis, a coloured span with a superscript, and an
//!   underlined span whose rule follows the curve.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{beside, Composition, Patch, Span};
use hephaestus::geometry::Size;
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement};
use hephaestus::plot::{scale, LineGeom, Plot, PlotComposition, TextPathGeom};
use hephaestus::scales::chrome::AxisSide;
use hephaestus::scene::SceneBuilder;
use hephaestus::Renderer;

fn main() {
    let (w, h) = (1200u32, 500u32);
    let dpi = 96.0;

    let mut renderer = VelloRenderer::new().expect("vello renderer init");
    let bg: Color = rgb8(248, 248, 252);

    // ── Render 1: justify_x along a sine wave ────────────────────────────
    //
    // Three stacked sine waves; each carries the same label, rendered
    // at different justify_x values to show start / centre / end packing.
    {
        let n = 80usize;
        let xs: Vec<f64> = (0..n)
            .map(|i| (i as f64) * 100.0 / (n as f64 - 1.0))
            .collect();
        // Each sine sits at a different y centre, with the wave running
        // top-to-bottom in the plot. y is in [0, 100] with 0 at the
        // bottom of the panel.
        let make_sine = |y_centre: f64, amplitude: f64| -> Vec<f64> {
            xs.iter()
                .map(|x| y_centre + amplitude * (x * 0.18).sin())
                .collect()
        };
        let sine_high = make_sine(80.0, 8.0);
        let sine_mid = make_sine(50.0, 8.0);
        let sine_low = make_sine(20.0, 8.0);

        // Per-key xs / ys / label / justify_x columns. Three marks total.
        let keys = ["start", "centre", "end"];
        let mut all_x: Vec<f64> = Vec::new();
        let mut all_y: Vec<f64> = Vec::new();
        let mut all_keys: Vec<&'static str> = Vec::new();
        let mut text_per_row: Vec<&'static str> = Vec::new();
        let mut justify_per_row: Vec<f64> = Vec::new();
        for (key, (ys, justify_x)) in
            keys.iter()
                .zip([(&sine_high, 0.0_f64), (&sine_mid, 0.5), (&sine_low, 1.0)])
        {
            for (x, y) in xs.iter().zip(ys.iter()) {
                all_x.push(*x);
                all_y.push(*y);
                all_keys.push(*key);
                text_per_row.push("hello world");
                justify_per_row.push(justify_x);
            }
        }

        let comp = || Composition::empty(1, 1).place(1, 1, Span::cell(), Patch::new("panel"));
        let mut plot = Plot::new(&comp(), "panel")
            .bind("x", "x_axis")
            .bind("y", "y_axis");

        // Reference lines — thin grey sine waves.
        plot.add_geom(
            LineGeom::builder()
                .keys(all_keys.clone())
                .set("x", all_x.clone())
                .set("y", all_y.clone())
                .set("stroke", rgb8(180, 180, 190))
                .set("linewidth", 1.0_f64)
                .build(),
        );
        // Labels following the lines.
        plot.add_geom(
            TextPathGeom::builder()
                .keys(all_keys)
                .set("x", all_x)
                .set("y", all_y)
                .set("text", text_per_row)
                .set("justify_x", justify_per_row)
                .set("size", 20.0_f64)
                .set("weight", 600.0_f64)
                .set("fill", rgb8(30, 40, 70))
                .set("anchor_y", 1.0_f64) // bottom edge on the line, text above
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

        let mut view = PlotComposition::new(&comp())
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
            "examples/text_path_1_sine.png",
        );
    }

    // ── Render 2: upright on / off on circular paths ─────────────────
    //
    // Two side-by-side panels, each containing a unit circle traced
    // CCW (math convention). The text wraps around the whole circle.
    // Left panel keeps `upright = false` (default) — glyphs follow the
    // tangent literally and read upside-down on the bottom arc. Right
    // panel sets `upright = true` — every glyph stays right-side-up,
    // though reading direction reverses where the tangent flips.
    {
        let n = 200usize;
        let cx = 50.0;
        let cy = 50.0;
        let r = 35.0;
        let xs: Vec<f64> = (0..n)
            .map(|i| {
                let t = (i as f64) / (n as f64 - 1.0) * std::f64::consts::TAU;
                cx + r * t.cos()
            })
            .collect();
        let ys: Vec<f64> = (0..n)
            .map(|i| {
                let t = (i as f64) / (n as f64 - 1.0) * std::f64::consts::TAU;
                cy + r * t.sin()
            })
            .collect();
        let label = "the quick brown fox jumps over the lazy dog";

        let make_plot = |comp_template: &Composition, patch: &str, upright: bool| -> Plot {
            let mut plot = Plot::new(comp_template, patch)
                .bind("x", "x_axis")
                .bind("y", "y_axis");
            plot.add_geom(
                LineGeom::builder()
                    .set("x", xs.clone())
                    .set("y", ys.clone())
                    .set("stroke", rgb8(180, 180, 190))
                    .set("linewidth", 1.0_f64)
                    .build(),
            );
            plot.add_geom(
                TextPathGeom::builder()
                    .set("x", xs.clone())
                    .set("y", ys.clone())
                    .set("text", label)
                    .set("size", 14.0_f64)
                    .set("weight", 600.0_f64)
                    .set("fill", rgb8(30, 40, 70))
                    .set("upright", upright)
                    .set("anchor_y", 1.0_f64)
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
            plot
        };

        let comp = || {
            beside(
                Patch::new("left").aspect(1.0, 1.0),
                Patch::new("right").aspect(1.0, 1.0),
            )
        };
        let plot_left = make_plot(&comp(), "left", false);
        let plot_right = make_plot(&comp(), "right", true);

        let mut view = PlotComposition::new(&comp())
            .add_scale("x_axis", scale::continuous(0.0..=100.0))
            .add_scale("y_axis", scale::continuous(0.0..=100.0))
            .with_plot(plot_left)
            .with_plot(plot_right);

        render_to(
            &mut renderer,
            &mut view,
            w,
            h,
            dpi,
            bg,
            "examples/text_path_2_upright.png",
        );
    }

    // ── Render 3: anchor_y across the curve ────────────────────────────────
    //
    // Three horizontal paths at distinct y values, each carrying the
    // same label at a different `anchor_y`. The fraction is of the
    // text's own height: 1 puts its bottom edge on the curve (text
    // sits above), 0.5 centres it, 0 puts its top edge on the curve.
    {
        let n = 50usize;
        let xs: Vec<f64> = (0..n)
            .map(|i| 10.0 + (i as f64) * 80.0 / (n as f64 - 1.0))
            .collect();
        let ys_top: Vec<f64> = vec![75.0; n];
        let ys_mid: Vec<f64> = vec![50.0; n];
        let ys_low: Vec<f64> = vec![25.0; n];

        let keys = ["above", "on", "below"];
        let anchor_values = [1.0_f64, 0.5, 0.0];
        let mut all_x: Vec<f64> = Vec::new();
        let mut all_y: Vec<f64> = Vec::new();
        let mut all_keys: Vec<&'static str> = Vec::new();
        let mut text_per_row: Vec<String> = Vec::new();
        let mut anchor_per_row: Vec<f64> = Vec::new();
        for ((key, ys), anchor_y) in keys
            .iter()
            .zip([&ys_top, &ys_mid, &ys_low])
            .zip(anchor_values)
        {
            for (x, y) in xs.iter().zip(ys.iter()) {
                all_x.push(*x);
                all_y.push(*y);
                all_keys.push(*key);
                text_per_row.push(format!("anchor_y = {anchor_y}"));
                anchor_per_row.push(anchor_y);
            }
        }

        let comp = || Composition::empty(1, 1).place(1, 1, Span::cell(), Patch::new("panel"));
        let mut plot = Plot::new(&comp(), "panel")
            .bind("x", "x_axis")
            .bind("y", "y_axis");

        // Reference lines.
        plot.add_geom(
            LineGeom::builder()
                .keys(all_keys.clone())
                .set("x", all_x.clone())
                .set("y", all_y.clone())
                .set("stroke", rgb8(180, 180, 190))
                .set("linewidth", 1.0_f64)
                .build(),
        );
        plot.add_geom(
            TextPathGeom::builder()
                .keys(all_keys)
                .set("x", all_x)
                .set("y", all_y)
                .set("text", text_per_row)
                .set("anchor_y", anchor_per_row)
                .set("justify_x", 0.5_f64)
                .set("size", 18.0_f64)
                .set("weight", 600.0_f64)
                .set("fill", rgb8(30, 40, 70))
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

        let mut view = PlotComposition::new(&comp())
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
            "examples/text_path_3_anchor_y.png",
        );
    }

    // ── Render 4: markdown labels ────────────────────────────────────
    //
    // The `"markdown"` channel reads each label as marquee-flavoured
    // markdown. Inline styling follows the curve — emphasis, per-span
    // colour and size, superscripts — and an underline is stroked
    // along the path rather than drawn as a straight rule. The third
    // label is two paragraphs, which flatten onto one line with a
    // space between them.
    {
        let n = 80usize;
        let xs: Vec<f64> = (0..n)
            .map(|i| (i as f64) * 100.0 / (n as f64 - 1.0))
            .collect();
        let make_sine = |y_centre: f64, amplitude: f64| -> Vec<f64> {
            xs.iter()
                .map(|x| y_centre + amplitude * (x * 0.18).sin())
                .collect()
        };
        let labels = [
            "plain, **bold**, *italic*, `code`",
            "{#c02020 warming} of 1.5 ^oC^ above",
            "first _underlined_\n\nsecond",
        ];
        let mut all_x: Vec<f64> = Vec::new();
        let mut all_y: Vec<f64> = Vec::new();
        let mut all_keys: Vec<&'static str> = Vec::new();
        let mut text_per_row: Vec<&'static str> = Vec::new();
        for (key, (label, y_centre)) in ["inline", "span", "blocks"]
            .iter()
            .zip(labels.iter().zip([80.0_f64, 50.0, 20.0]))
        {
            let ys = make_sine(y_centre, 8.0);
            for (x, y) in xs.iter().zip(ys.iter()) {
                all_x.push(*x);
                all_y.push(*y);
                all_keys.push(*key);
                text_per_row.push(label);
            }
        }

        let comp = || Composition::empty(1, 1).place(1, 1, Span::cell(), Patch::new("panel"));
        let mut plot = Plot::new(&comp(), "panel")
            .bind("x", "x_axis")
            .bind("y", "y_axis");

        plot.add_geom(
            LineGeom::builder()
                .keys(all_keys.clone())
                .set("x", all_x.clone())
                .set("y", all_y.clone())
                .set("stroke", rgb8(180, 180, 190))
                .set("linewidth", 1.0_f64)
                .build(),
        );
        plot.add_geom(
            TextPathGeom::builder()
                .keys(all_keys)
                .set("x", all_x)
                .set("y", all_y)
                .set("text", text_per_row)
                .set("markdown", true)
                .set("size", 20.0_f64)
                .set("fill", rgb8(30, 40, 70))
                .set("anchor_y", 1.0_f64)
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

        let mut view = PlotComposition::new(&comp())
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
            "examples/text_path_4_markdown.png",
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
    hephaestus::image::write_png(&path, w, h, &pixels, Some(dpi)).expect("write png");
    println!("wrote {}", path.display());
}
