//! Visual smoke test for the high-level composition layer. Builds a nested
//! composition (an outer patch wrapping a row of two inner patches), each
//! patch's chrome filled with fake fixed-size content, and renders every
//! resolved rect with `VelloRenderer`.
//!
//! Writes `examples/composition_demo.png` next to this file.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{beside, Patch, Slot};
use hephaestus::layout::{Cell, Measure, WidthHint};
use hephaestus::stroke::Stroke;
use hephaestus::{Affine, Brush, FillRule, Path, PickId, Renderer, SceneBuilder};
use kurbo::Shape;

/// A fixed-size leaf for layout testing.
struct FixedSize {
    w: f64,
    h: f64,
}

impl Measure for FixedSize {
    fn width_hint(&self, _dpi: f64) -> WidthHint {
        WidthHint::Min(self.w)
    }
    fn height_at(&self, _width: f64, _dpi: f64) -> f64 {
        self.h
    }
}

fn sized(w: f64, h: f64) -> Cell {
    Cell::measured(FixedSize { w, h })
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

fn build_inner_patch(id: &str, y_axis_w: f64, axis_label_w: f64) -> Patch {
    Patch::new(id)
        .slot(Slot::AxisLeft, sized(y_axis_w, 0.0))
        .slot(Slot::AxisLeftTitle, sized(axis_label_w, 0.0))
        .slot(Slot::AxisBottom, sized(0.0, 22.0))
        .slot(Slot::AxisBottomTitle, sized(0.0, 18.0))
        .slot(Slot::Panel, Cell::empty())
}

fn main() {
    let (w, h) = (1200u32, 700u32);
    let dpi = 96.0;

    // Two inner plots with different y-axis widths to demonstrate alignment.
    let inner_a = build_inner_patch("plot_a", 24.0, 18.0);
    let inner_b = build_inner_patch("plot_b", 56.0, 18.0);

    // Outer patch wrapping the row. Adds a title spanning both inner panels
    // and a shared left axis title that merges with plot_a's left chrome.
    let composed = Patch::new("outer")
        .slot(Slot::Title, sized(0.0, 36.0))
        .slot(Slot::Subtitle, sized(0.0, 22.0))
        .slot(Slot::Caption, sized(0.0, 18.0))
        .place_in_panel(beside(inner_a, inner_b));

    let layout = composed.solve(hephaestus::Size::new(w as f64, h as f64), dpi);

    let mut renderer = VelloRenderer::new().expect("vello renderer init");
    {
        let scene = renderer.scene();
        let stroke = Stroke::new(1.5);

        // Draw all rects with a fill (semi-transparent) + an outline.
        for (_id, region, rect) in layout.iter() {
            // Skip panel — drawn last so its border is on top.
            if region == "panel" {
                continue;
            }
            let mut color = color_for_region(region);
            // 40% alpha by darkening through alpha override:
            color = Color::new([
                color.components[0],
                color.components[1],
                color.components[2],
                0.4,
            ]);
            let path: Path = rect.to_path(0.1);
            scene.fill(
                FillRule::NonZero,
                Affine::IDENTITY,
                &Brush::Solid(color),
                None,
                &path,
                PickId::Skip,
            );
            scene.stroke(
                &stroke,
                Affine::IDENTITY,
                &Brush::Solid(color_for_region(region)),
                None,
                &path,
                PickId::Skip,
            );
        }

        // Now panels on top so their fill doesn't bleed.
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
    }

    let mut pixels = vec![0u8; (w * h * 4) as usize];
    let bg: Color = rgb8(245, 245, 248);
    renderer
        .render_to_buffer(w, h, bg, &mut pixels)
        .expect("render");

    let path = std::env::current_dir()
        .unwrap()
        .join("examples/composition_demo.png");
    hephaestus::png::write_png(&path, w, h, &pixels).expect("write png");
    println!("wrote {}", path.display());

    // Print a small alignment-report to stdout for quick sanity check.
    let panel_a = layout.get("plot_a", Slot::Panel).unwrap();
    let panel_b = layout.get("plot_b", Slot::Panel).unwrap();
    let title = layout.get("outer", Slot::Title).unwrap();
    println!("plot_a.panel = {:?}", panel_a);
    println!("plot_b.panel = {:?}", panel_b);
    println!("outer.title  = {:?}", title);
    assert!(
        (panel_a.y0 - panel_b.y0).abs() < 0.5,
        "panels should share y0"
    );
    assert!(
        title.x0 <= panel_a.x0 + 0.5 && title.x1 >= panel_b.x1 - 0.5,
        "title should span across both panels"
    );
    println!("alignment invariants hold ✓");
}
