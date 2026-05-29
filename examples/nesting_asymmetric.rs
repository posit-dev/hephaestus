//! Visual stress test: nested compositions with asymmetric column counts.
//!
//! Stacks a 1×3 composition over a 1×2 composition. The two rows have
//! different inner column counts but must both tile the full viewport
//! width. Historical pain point — the old super-grid logic failed here.
//!
//! Each inner plot has visible labelled chrome so the alignment between
//! sibling plots within a row, and the row boundary between rows, is
//! easy to read off the rendered image.
//!
//! Writes `examples/nesting_asymmetric.png`.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{grid, stack, Patch, Slot};
use hephaestus::layout::Cell;
use hephaestus::text::{draw_text_in_rect, TextRun, TextStyle};
use hephaestus::{Affine, Brush, FillRule, Path, PickId, Renderer, SceneBuilder};
use kurbo::Shape;

fn text_cell(text: &str, size: f32) -> Cell {
    Cell::measured(TextRun::new(text, &TextStyle::new(size)))
}

fn plot(id: &str, title: &str, axis_left: &str, axis_bottom: &str) -> Patch {
    Patch::new(id)
        .slot(Slot::Title, text_cell(title, 16.0))
        .slot(Slot::AxisLeft, text_cell(axis_left, 11.0))
        .slot(Slot::AxisBottom, text_cell(axis_bottom, 11.0))
        .slot(Slot::Panel, Cell::empty())
}

fn color_for_region(region: &str) -> Color {
    match region {
        "panel" => rgb8(40, 60, 90),
        "title" => rgb8(220, 180, 90),
        "axis_left" | "axis_bottom" => rgb8(160, 100, 130),
        _ => rgb8(120, 120, 120),
    }
}

fn main() {
    let (w, h) = (1200u32, 700u32);
    let dpi = 96.0;

    let row_three = grid(
        1,
        3,
        vec![
            plot("a1", "Series A1", "0\n10\n20", "0  5  10").into(),
            plot("a2", "Series A2", "0\n50\n100", "0  5  10").into(),
            plot("a3", "Series A3", "0\n500\n1000", "0  5  10").into(),
        ],
    );
    let row_two = grid(
        1,
        2,
        vec![
            plot("b1", "Series B1 (wider)", "0\n100\n200\n300", "0   25   50").into(),
            plot("b2", "Series B2 (wider)", "0\n5000\n10000", "0   25   50").into(),
        ],
    );
    let composed = stack(row_three, row_two);
    let layout = composed.solve(hephaestus::Size::new(w as f64, h as f64), dpi);

    let mut renderer = VelloRenderer::new().expect("vello renderer init");
    {
        let scene = renderer.scene();
        let stroke = hephaestus::stroke::Stroke::new(1.0);
        let text_brush: Brush = rgb8(20, 20, 30).into();

        // Chrome rects, tinted background.
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

        // Panels solid.
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

        // Text labels in chrome.
        for (id, region, rect) in layout.iter() {
            if region == "panel" {
                continue;
            }
            let text = match (id, region) {
                ("a1", "title") => "Series A1",
                ("a2", "title") => "Series A2",
                ("a3", "title") => "Series A3",
                ("b1", "title") => "Series B1 (wider)",
                ("b2", "title") => "Series B2 (wider)",
                ("a1", "axis_left") => "0\n10\n20",
                ("a2", "axis_left") => "0\n50\n100",
                ("a3", "axis_left") => "0\n500\n1000",
                ("b1", "axis_left") => "0\n100\n200\n300",
                ("b2", "axis_left") => "0\n5000\n10000",
                (_, "axis_bottom") if id.starts_with('a') => "0  5  10",
                (_, "axis_bottom") if id.starts_with('b') => "0   25   50",
                _ => continue,
            };
            let size = if region == "title" { 16.0 } else { 11.0 };
            let run = TextRun::new(text, &TextStyle::new(size));
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
        .join("examples/nesting_asymmetric.png");
    hephaestus::png::write_png(&path, w, h, &pixels).expect("write png");
    println!("wrote {}", path.display());
}
