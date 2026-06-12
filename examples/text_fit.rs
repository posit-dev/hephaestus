//! End-to-end visual sanity for `TextFitGeom` — text that fits itself
//! to a target rect by scaling the font size. Three renders:
//!
//! - `text_fit_1_aspect_grid.png` — the same string fitted into a grid
//!   of rects with different aspect ratios. Wide rects produce
//!   single-line large text; tall narrow rects produce multi-line
//!   wrapped text.
//!
//! - `text_fit_2_clip.png` — a deliberately-too-small rect forces the
//!   clip-on-overflow path: the geom draws at `min_font_size` with a
//!   clip rect at the target.
//!
//! - `text_fit_3_justify.png` — justification combinations:
//!   `justify_x` ∈ {start, center, end} × `justify_y` ∈ {start,
//!   center, end} demonstrated by placing the same fitted string in
//!   9 rects with each combination.

use std::sync::Arc;

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{Composition, Patch, Span};
use hephaestus::geometry::Size;
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement};
use hephaestus::plot::value::Value;
use hephaestus::plot::{scale, Plot, PlotComposition, RectGeom, TextFitGeom, TextGeom};
use hephaestus::scales::chrome::AxisSide;
use hephaestus::Renderer;

fn main() {
    let (w, h) = (1200u32, 600u32);
    let dpi = 96.0;

    let comp = || Composition::empty(1, 1).place(1, 1, Span::cell(), Patch::new("panel"));

    let mut renderer = VelloRenderer::new().expect("vello renderer init");
    let bg: Color = rgb8(248, 248, 252);

    render_aspect_grid(&mut renderer, &comp, w, h, dpi, bg);
    render_clip(&mut renderer, &comp, w, h, dpi, bg);
    render_justify(&mut renderer, &comp, w, h, dpi, bg);
}

/// Render 1: a grid of rects with varying aspect ratios; the same
/// string is fitted into each. Visualises how the binary search
/// produces different font sizes for different containers.
fn render_aspect_grid(
    renderer: &mut VelloRenderer,
    comp: &impl Fn() -> Composition,
    w: u32,
    h: u32,
    dpi: f64,
    bg: Color,
) {
    let ink = rgb8(35, 40, 60);
    let text_ink = rgb8(80, 50, 30);

    // 5 rects with different aspect ratios: very-wide, wide, square,
    // tall, very-tall. Each anchored at a different x and roughly
    // centred vertically.
    let xs0 = [0.04_f64, 0.24, 0.44, 0.62, 0.80];
    let xs1 = [0.20_f64, 0.42, 0.58, 0.74, 0.90];
    let ys0 = [0.40_f64, 0.30, 0.20, 0.10, 0.05];
    let ys1 = [0.60_f64, 0.70, 0.80, 0.90, 0.95];

    let mut plot = Plot::new(&comp(), "panel")
        .title("TextFitGeom — same text fitted into rects of varying aspect ratio")
        .bind("x", "x_axis")
        .bind("y", "y_axis");

    // Draw the target rects (outlined) so the viewer sees the bounding box.
    plot.add_geom(
        RectGeom::builder()
            .set("x", xs0.to_vec())
            .set("x2", xs1.to_vec())
            .set("y", ys0.to_vec())
            .set("y2", ys1.to_vec())
            .set("x_band", vec![0.0_f64; 5])
            .set("x2_band", vec![0.0_f64; 5])
            .set("stroke", rgb8(160, 170, 200))
            .set("linewidth", 1.0_f64)
            .build(),
    );

    plot.add_geom(
        TextFitGeom::builder()
            .set("x", xs0.to_vec())
            .set("x2", xs1.to_vec())
            .set("y", ys0.to_vec())
            .set("y2", ys1.to_vec())
            .set(
                "text",
                vec!["Fit me", "Fit me", "Fit me", "Fit me", "Fit me"],
            )
            .set("weight", 600.0_f64)
            .set("fill", text_ink)
            .set("min_font_size", 6.0_f64)
            .set("max_font_size", 72.0_f64)
            .set("justify_x", vec!["center"; 5])
            .set("justify_y", vec!["center"; 5])
            .build(),
    );
    let _ = ink;

    plot.add_axis(Axis::rail(
        "x_axis",
        AxisPlacement::Cartesian(AxisSide::Bottom),
    ));
    plot.add_axis(Axis::rail(
        "y_axis",
        AxisPlacement::Cartesian(AxisSide::Left),
    ));

    let mut view = PlotComposition::new(comp())
        .add_scale("x_axis", scale::continuous(0.0..=1.0))
        .add_scale("y_axis", scale::continuous(0.0..=1.0))
        .with_plot(plot);
    render_to(
        renderer,
        &mut view,
        w,
        h,
        dpi,
        bg,
        "examples/text_fit_1_aspect_grid.png",
    );
}

/// Render 2: clip-on-overflow demo. A deliberately-too-small rect with
/// long text; min_font_size pins the size and the geom pushes a clip
/// layer at the target rect.
fn render_clip(
    renderer: &mut VelloRenderer,
    comp: &impl Fn() -> Composition,
    w: u32,
    h: u32,
    dpi: f64,
    bg: Color,
) {
    // Two rects side by side:
    //   left:  10pt min, fits → text at fitted size.
    //   right: 14pt min on a tiny rect → doesn't fit → clipped.
    let xs0 = [0.10_f64, 0.55];
    let xs1 = [0.45_f64, 0.65];
    let ys0 = [0.35_f64, 0.35];
    let ys1 = [0.65_f64, 0.65];

    let mut plot = Plot::new(&comp(), "panel")
        .title("TextFitGeom — fit (left) vs clip-on-overflow (right)")
        .bind("x", "x_axis")
        .bind("y", "y_axis");

    plot.add_geom(
        RectGeom::builder()
            .set("x", xs0.to_vec())
            .set("x2", xs1.to_vec())
            .set("y", ys0.to_vec())
            .set("y2", ys1.to_vec())
            .set("x_band", vec![0.0_f64; 2])
            .set("x2_band", vec![0.0_f64; 2])
            .set("stroke", rgb8(180, 100, 100))
            .set("linewidth", 1.5_f64)
            .build(),
    );

    plot.add_geom(
        TextFitGeom::builder()
            .set("x", xs0.to_vec())
            .set("x2", xs1.to_vec())
            .set("y", ys0.to_vec())
            .set("y2", ys1.to_vec())
            .set(
                "text",
                vec!["This fits naturally inside the rect", "UnbreakableLongWord"],
            )
            .set("weight", 500.0_f64)
            .set("fill", rgb8(30, 30, 30))
            .set("min_font_size", vec![6.0_f64, 24.0])
            .set("max_font_size", vec![48.0_f64, 28.0])
            .set("justify_x", vec!["center"; 2])
            .set("justify_y", vec!["center"; 2])
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
        .add_scale("x_axis", scale::continuous(0.0..=1.0))
        .add_scale("y_axis", scale::continuous(0.0..=1.0))
        .with_plot(plot);
    render_to(
        renderer,
        &mut view,
        w,
        h,
        dpi,
        bg,
        "examples/text_fit_2_clip.png",
    );
}

/// Render 3: 3×3 grid of justification combinations.
fn render_justify(
    renderer: &mut VelloRenderer,
    comp: &impl Fn() -> Composition,
    w: u32,
    h: u32,
    dpi: f64,
    bg: Color,
) {
    let justify_choices = ["start", "center", "end"];

    let mut xs0: Vec<f64> = Vec::new();
    let mut xs1: Vec<f64> = Vec::new();
    let mut ys0: Vec<f64> = Vec::new();
    let mut ys1: Vec<f64> = Vec::new();
    let mut justify_x: Vec<&'static str> = Vec::new();
    let mut justify_y: Vec<&'static str> = Vec::new();
    let mut labels: Vec<String> = Vec::new();

    // 3 columns × 3 rows. Each cell ~ 0.27 wide × 0.27 tall.
    let cell_w = 0.27_f64;
    let cell_h = 0.27_f64;
    let pad_x = 0.05_f64;
    let pad_y = 0.05_f64;
    for (col, jx) in justify_choices.iter().enumerate() {
        for (row, jy) in justify_choices.iter().enumerate() {
            let x0 = pad_x + col as f64 * (cell_w + pad_x);
            let x1 = x0 + cell_w;
            let y_top = 1.0 - pad_y - row as f64 * (cell_h + pad_y);
            let y_bot = y_top - cell_h;
            xs0.push(x0);
            xs1.push(x1);
            ys0.push(y_bot);
            ys1.push(y_top);
            justify_x.push(jx);
            justify_y.push(jy);
            labels.push("Short\nlabel".to_string());
        }
    }
    let _ = Arc::<str>::from(""); // silence unused-import warning if Arc not used elsewhere
    let _ = Value::Null;

    let mut plot = Plot::new(&comp(), "panel")
        .title("TextFitGeom — justify_x × justify_y (multi-line short text)")
        .bind("x", "x_axis")
        .bind("y", "y_axis");

    plot.add_geom(
        RectGeom::builder()
            .set("x", xs0.clone())
            .set("x2", xs1.clone())
            .set("y", ys0.clone())
            .set("y2", ys1.clone())
            .set("x_band", vec![0.0_f64; 9])
            .set("x2_band", vec![0.0_f64; 9])
            .set("stroke", rgb8(180, 180, 200))
            .set("linewidth", 1.0_f64)
            .build(),
    );

    plot.add_geom(
        TextFitGeom::builder()
            .set("x", xs0.clone())
            .set("x2", xs1.clone())
            .set("y", ys0.clone())
            .set("y2", ys1.clone())
            .set("text", labels)
            .set("weight", 500.0_f64)
            .set("fill", rgb8(30, 30, 30))
            .set("max_font_size", 18.0_f64)
            .set("justify_x", justify_x.clone())
            .set("justify_y", justify_y.clone())
            .build(),
    );

    // Label each cell with its justify_x/justify_y combo (small text
    // above the cell, outside the rect).
    let combo_labels: Vec<String> = justify_x
        .iter()
        .zip(justify_y.iter())
        .map(|(jx, jy)| format!("x={}, y={}", jx, jy))
        .collect();
    plot.add_geom(
        TextGeom::builder()
            .set(
                "x",
                xs0.iter()
                    .zip(xs1.iter())
                    .map(|(a, b)| (a + b) * 0.5)
                    .collect::<Vec<_>>(),
            )
            .set("y", ys1.clone())
            .set("text", combo_labels)
            .set("size", 9.0_f64)
            .set("fill", rgb8(100, 100, 120))
            .set("anchor_x", 0.5_f64)
            .set("anchor_y", 0.0_f64)
            .set("y_offset", 3.0_f64)
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
        .add_scale("x_axis", scale::continuous(0.0..=1.0))
        .add_scale("y_axis", scale::continuous(0.0..=1.0))
        .with_plot(plot);
    render_to(
        renderer,
        &mut view,
        w,
        h,
        dpi,
        bg,
        "examples/text_fit_3_justify.png",
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
