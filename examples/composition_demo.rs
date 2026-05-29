//! Visual smoke test for the high-level composition layer with real text.
//!
//! Builds an outer patch wrapping a row of two inner patches, fills each
//! anatomical slot with [`hephaestus::text::TextRun`] content (so the slots
//! actually size themselves from font metrics), then renders every resolved
//! rect plus the text into the rect.
//!
//! Writes `examples/composition_demo.png` next to this file.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{beside, Composition, Patch, Slot, Span};
use hephaestus::layout::{Cell, Track};
use hephaestus::stroke::Stroke;
use hephaestus::text::{draw_text_in_rect, TextRun, TextStyle};
use hephaestus::{Affine, Brush, FillRule, Path, PickId, Renderer, SceneBuilder};
use kurbo::Shape;

fn text_cell(text: &str, size: f32) -> Cell {
    Cell::measured(TextRun::new(text, &TextStyle::new(size)))
}

fn weighted_text_cell(text: &str, size: f32, weight: u16) -> Cell {
    Cell::measured(TextRun::new(text, &TextStyle::new(size).weight(weight)))
}

fn color_for_region(region: &str) -> Color {
    match region {
        "panel" => rgb8(40, 50, 70),
        "title" => rgb8(220, 180, 90),
        "subtitle" => rgb8(180, 150, 70),
        "caption" => rgb8(180, 130, 90),
        "axis_left" | "axis_right" | "axis_top" | "axis_bottom" => rgb8(160, 100, 130),
        "axis_left_title" | "axis_right_title" | "axis_top_title" | "axis_bottom_title" => {
            rgb8(200, 120, 160)
        }
        "strip_left" | "strip_right" | "strip_top" | "strip_bottom" => rgb8(90, 140, 180),
        "legend_left" | "legend_right" | "legend_top" | "legend_bottom" => rgb8(110, 180, 140),
        _ => rgb8(120, 120, 120),
    }
}

/// Build an inner patch with axis labels and a panel-axis title. Each
/// `TextRun` provides its own intrinsic width/height to the layout solver.
fn build_inner_patch(id: &str, y_axis_text: &str, y_axis_title: &str, x_axis_title: &str) -> Patch {
    Patch::new(id)
        .slot(Slot::AxisLeft, text_cell(y_axis_text, 12.0))
        .slot(
            Slot::AxisLeftTitle,
            weighted_text_cell(y_axis_title, 13.0, 500),
        )
        .slot(Slot::AxisBottom, text_cell("0  25  50  75  100", 12.0))
        .slot(
            Slot::AxisBottomTitle,
            weighted_text_cell(x_axis_title, 13.0, 500),
        )
        .slot(Slot::Panel, Cell::empty())
}

fn main() {
    let (w, h) = (1200u32, 700u32);
    let dpi = 96.0;

    let inner_a = build_inner_patch("plot_a", "0\n50\n100", "Pressure (kPa)", "Time (s)");
    let inner_b = build_inner_patch(
        "plot_b",
        "0\n10000\n20000\n30000",
        "Particle count",
        "Time (s)",
    );

    // Header carries title + subtitle; footer carries caption. Both have no
    // panel content — their composition rows are sized to chrome height only
    // (Fr(0.0) panel weight so the middle row absorbs all leftover height).
    let header = Patch::new("header")
        .slot(
            Slot::Title,
            weighted_text_cell("Reactor diagnostics", 28.0, 700),
        )
        .slot(
            Slot::Subtitle,
            text_cell("Pressure and particle count over the morning run", 16.0),
        );
    let footer = Patch::new("footer").slot(
        Slot::Caption,
        text_cell("Source: in-line telemetry, smoothed at 1s intervals", 11.0),
    );
    let composed = Composition::empty(3, 1)
        .heights(vec![Track::Fr(0.0), Track::Fr(1.0), Track::Fr(0.0)])
        .place(1, 1, Span::cell(), header)
        .place(2, 1, Span::cell(), beside(inner_a, inner_b))
        .place(3, 1, Span::cell(), footer);

    let layout = composed.solve(hephaestus::Size::new(w as f64, h as f64), dpi);

    let mut renderer = VelloRenderer::new().expect("vello renderer init");
    {
        let scene = renderer.scene();
        let stroke = Stroke::new(1.0);
        let text_brush: Brush = rgb8(30, 30, 40).into();

        // Background colour rectangles for chrome.
        for (_id, region, rect) in layout.iter() {
            if region == "panel" {
                continue;
            }
            let color = color_for_region(region);
            let tinted = Color::new([
                color.components[0],
                color.components[1],
                color.components[2],
                0.18,
            ]);
            let path: Path = rect.to_path(0.1);
            scene.fill(
                FillRule::NonZero,
                Affine::IDENTITY,
                &Brush::Solid(tinted),
                None,
                &path,
                PickId::Skip,
            );
            scene.stroke(
                &stroke,
                Affine::IDENTITY,
                &Brush::Solid(color),
                None,
                &path,
                PickId::Skip,
            );
        }

        // Panels on top.
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

        // Draw the actual text. We reach into the patches again to get a
        // `TextRun` reference — the composition layout doesn't hand us the
        // measure values back. For the demo, just rebuild a separate TextRun
        // for each slot keyed by the same text so we can render it.
        for (id, region, rect) in layout.iter() {
            if region == "panel" {
                continue;
            }
            let text = match (id, region) {
                ("header", "title") => "Reactor diagnostics",
                ("header", "subtitle") => "Pressure and particle count over the morning run",
                ("footer", "caption") => "Source: in-line telemetry, smoothed at 1s intervals",
                ("plot_a", "axis_left") => "0\n50\n100",
                ("plot_a", "axis_left_title") => "Pressure (kPa)",
                ("plot_a", "axis_bottom") => "0  25  50  75  100",
                ("plot_a", "axis_bottom_title") => "Time (s)",
                ("plot_b", "axis_left") => "0\n10000\n20000\n30000",
                ("plot_b", "axis_left_title") => "Particle count",
                ("plot_b", "axis_bottom") => "0  25  50  75  100",
                ("plot_b", "axis_bottom_title") => "Time (s)",
                _ => continue,
            };
            let size = match region {
                "title" => 28.0,
                "subtitle" => 16.0,
                "caption" => 11.0,
                "axis_left_title" | "axis_bottom_title" => 13.0,
                _ => 12.0,
            };
            let weight = match region {
                "title" => 700,
                "axis_left_title" | "axis_bottom_title" => 500,
                _ => 400,
            };
            let style = TextStyle::new(size).weight(weight);
            let run = TextRun::new(text, &style);
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
        .join("examples/composition_demo.png");
    hephaestus::png::write_png(&path, w, h, &pixels).expect("write png");
    println!("wrote {}", path.display());

    let panel_a = layout.get("plot_a", Slot::Panel).unwrap();
    let panel_b = layout.get("plot_b", Slot::Panel).unwrap();
    let title = layout.get("header", Slot::Title).unwrap();
    println!("plot_a.panel = {:?}", panel_a);
    println!("plot_b.panel = {:?}", panel_b);
    println!("header.title = {:?}", title);
}
