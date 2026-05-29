//! Faceted-plot idiom: a grid of plots with a full-width title,
//! subtitle, and caption that span the entire width of the facet grid.
//!
//! Uses `Composition`'s chrome API — `.id(...)` plus `.slot(Slot::Title,
//! ...)` etc. directly on the composition. The composition is then
//! treated as a "simplified plot" wrapping its facets in the canonical
//! 13×16 anatomical block; chrome slots sit at canonical positions
//! around the panel band that holds the facets.
//!
//! Mirrors patchwork's `simplify_gt.gtable_patchwork` + `plot_annotation`
//! use case in one move:
//!
//! ```text
//!   grid(rows, cols, [...])
//!       .id("plot")
//!       .slot(Slot::Title, ...)
//!       .slot(Slot::Subtitle, ...)
//!       .slot(Slot::AxisLeftTitle, ...)
//!       .slot(Slot::Caption, ...)
//! ```
//!
//! Writes `examples/nesting_faceted_title.png`.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{grid, Patch, Slot};
use hephaestus::layout::Cell;
use hephaestus::text::{draw_text_in_rect, TextRun, TextStyle};
use hephaestus::{Affine, Brush, FillRule, Path, PickId, Renderer, SceneBuilder};
use kurbo::Shape;

fn text_cell(text: &str, size: f32) -> Cell {
    Cell::measured(TextRun::new(text, &TextStyle::new(size)))
}

fn weighted_text_cell(text: &str, size: f32, weight: u16) -> Cell {
    Cell::measured(TextRun::new(text, &TextStyle::new(size).weight(weight)))
}

fn facet(id: &str, axis_left: &str, axis_bottom: &str) -> Patch {
    Patch::new(id)
        .slot(Slot::AxisLeft, text_cell(axis_left, 11.0))
        .slot(Slot::AxisBottom, text_cell(axis_bottom, 11.0))
        .slot(Slot::Panel, Cell::empty())
}

fn color_for_region(region: &str) -> Color {
    match region {
        "panel" => rgb8(40, 60, 90),
        "title" => rgb8(220, 180, 90),
        "subtitle" => rgb8(180, 150, 70),
        "caption" => rgb8(180, 130, 90),
        "axis_left" | "axis_bottom" => rgb8(160, 100, 130),
        _ => rgb8(120, 120, 120),
    }
}

fn main() {
    let (w, h) = (1400u32, 800u32);
    let dpi = 96.0;

    // A 2×3 grid of facets, each with its own axis chrome. The
    // composition is then annotated with shared chrome via `.slot(...)`
    // directly — no wrapper composition, no manual span calculations.
    let mut facet_cells = Vec::new();
    for r in 1..=2 {
        for c in 1..=3 {
            let id = format!("f{}_{}", r, c);
            facet_cells.push(facet(&id, "0\n50\n100", "0  25  50").into());
        }
    }
    let composed = grid(2, 3, facet_cells)
        .id("plot")
        .slot(
            Slot::Title,
            weighted_text_cell("Reactor diagnostics across runs", 26.0, 700),
        )
        .slot(
            Slot::Subtitle,
            text_cell(
                "Each panel: a separate 60-minute run; same scales throughout",
                15.0,
            ),
        )
        .slot(
            Slot::AxisLeftTitle,
            weighted_text_cell("Pressure (kPa)", 14.0, 500),
        )
        .slot(
            Slot::AxisBottomTitle,
            weighted_text_cell("Elapsed time (minutes)", 14.0, 500),
        )
        .slot(
            Slot::Caption,
            text_cell(
                "Source: in-line telemetry, smoothed at 1s; runs ordered chronologically.",
                11.0,
            ),
        );

    let layout = composed.solve(hephaestus::Size::new(w as f64, h as f64), dpi);

    let mut renderer = VelloRenderer::new().expect("vello renderer init");
    {
        let scene = renderer.scene();
        let stroke = hephaestus::stroke::Stroke::new(1.0);
        let text_brush: Brush = rgb8(20, 20, 30).into();

        for (_id, region, rect) in layout.iter() {
            if region == "panel" {
                continue;
            }
            let c = color_for_region(region);
            let tint = Color::new([c.components[0], c.components[1], c.components[2], 0.20]);
            let path: Path = rect.to_path(0.1);
            scene.fill(
                FillRule::NonZero,
                Affine::IDENTITY,
                &Brush::Solid(tint),
                None,
                &path,
                PickId::Skip,
            );
            scene.stroke(
                &stroke,
                Affine::IDENTITY,
                &Brush::Solid(c),
                None,
                &path,
                PickId::Skip,
            );
        }
        for (_id, region, rect) in layout.iter() {
            if region != "panel" {
                continue;
            }
            let path: Path = rect.to_path(0.1);
            scene.fill(
                FillRule::NonZero,
                Affine::IDENTITY,
                &Brush::Solid(color_for_region(region)),
                None,
                &path,
                PickId::Skip,
            );
            scene.stroke(
                &stroke,
                Affine::IDENTITY,
                &Brush::Solid(rgb8(255, 255, 255)),
                None,
                &path,
                PickId::Skip,
            );
        }

        for (id, region, rect) in layout.iter() {
            let (text, size, weight) = match (id, region) {
                ("plot", "title") => ("Reactor diagnostics across runs", 26.0, 700),
                ("plot", "subtitle") => (
                    "Each panel: a separate 60-minute run; same scales throughout",
                    15.0,
                    400,
                ),
                ("plot", "caption") => (
                    "Source: in-line telemetry, smoothed at 1s; runs ordered chronologically.",
                    11.0,
                    400,
                ),
                ("plot", "axis_left_title") => ("Pressure (kPa)", 14.0, 500),
                ("plot", "axis_bottom_title") => ("Elapsed time (minutes)", 14.0, 500),
                (_, "axis_left") => ("0\n50\n100", 11.0, 400),
                (_, "axis_bottom") => ("0  25  50", 11.0, 400),
                _ => continue,
            };
            let run = TextRun::new(text, &TextStyle::new(size).weight(weight));
            draw_text_in_rect(scene, &run, rect, &text_brush, PickId::Skip);
        }
    }

    let mut pixels = vec![0u8; (w * h * 4) as usize];
    let bg: Color = rgb8(248, 248, 252);
    renderer
        .render_to_buffer(w, h, bg, &mut pixels)
        .expect("render");

    let path = std::env::current_dir()
        .unwrap()
        .join("examples/nesting_faceted_title.png");
    hephaestus::png::write_png(&path, w, h, &pixels).expect("write png");
    println!("wrote {}", path.display());

    // Sanity check: chrome rects line up with the facet panel band.
    let title = layout.get("plot", Slot::Title).unwrap();
    let caption = layout.get("plot", Slot::Caption).unwrap();
    let axis_left_title = layout.get("plot", Slot::AxisLeftTitle).unwrap();
    let axis_bottom_title = layout.get("plot", Slot::AxisBottomTitle).unwrap();
    let f1_1_panel = layout.get("f1_1", Slot::Panel).unwrap();
    let f1_3_panel = layout.get("f1_3", Slot::Panel).unwrap();
    let f2_3_panel = layout.get("f2_3", Slot::Panel).unwrap();
    println!(
        "plot.title:             x0={:>5.0}  x1={:>5.0}  y0={:>5.0}  y1={:>5.0}",
        title.x0, title.x1, title.y0, title.y1
    );
    println!(
        "plot.caption:           x0={:>5.0}  x1={:>5.0}  y0={:>5.0}  y1={:>5.0}",
        caption.x0, caption.x1, caption.y0, caption.y1
    );
    println!(
        "plot.axis_left_title:   x0={:>5.0}  x1={:>5.0}  y0={:>5.0}  y1={:>5.0}",
        axis_left_title.x0, axis_left_title.x1, axis_left_title.y0, axis_left_title.y1
    );
    println!(
        "plot.axis_bottom_title: x0={:>5.0}  x1={:>5.0}  y0={:>5.0}  y1={:>5.0}",
        axis_bottom_title.x0, axis_bottom_title.x1, axis_bottom_title.y0, axis_bottom_title.y1
    );
    println!(
        "facets panel band:      x0={:>5.0}  x1={:>5.0}  y0={:>5.0}  y1={:>5.0}",
        f1_1_panel.x0, f1_3_panel.x1, f1_1_panel.y0, f2_3_panel.y1
    );
}
