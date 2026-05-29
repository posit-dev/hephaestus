//! Visual stress test: three levels of composition nesting.
//!
//! Layout: `beside(o1, beside(m1, beside(l1, l2)))`. Four plot panels
//! across, with the rightmost two living inside a nested-of-a-nested
//! composition. The deepest plot has labelled chrome; the others are
//! plain. The bidirectional sizer pair at each of the two boundaries
//! must converge in 3 iterations of the solver's TrackOf fixed-point
//! loop (within `MAX_ITER = 5`).
//!
//! Visually: all four panels share their y range, and the axis_top
//! height contributed by the deepest plot propagates outward through
//! both nesting boundaries to push every other panel down by the same
//! amount.
//!
//! Writes `examples/nesting_deep.png`.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{beside, Patch, Slot};
use hephaestus::layout::Cell;
use hephaestus::text::{draw_text_in_rect, TextRun, TextStyle};
use hephaestus::{Affine, Brush, FillRule, Path, PickId, Renderer, SceneBuilder};
use kurbo::Shape;

fn text_cell(text: &str, size: f32) -> Cell {
    Cell::measured(TextRun::new(text, &TextStyle::new(size)))
}

fn plain(id: &str) -> Patch {
    Patch::new(id).slot(Slot::Panel, Cell::empty())
}

fn color_for_region(region: &str) -> Color {
    match region {
        "panel" => rgb8(40, 60, 90),
        "axis_top" => rgb8(200, 100, 130),
        "axis_bottom" => rgb8(160, 100, 130),
        _ => rgb8(120, 120, 120),
    }
}

fn main() {
    let (w, h) = (1400u32, 400u32);
    let dpi = 96.0;

    // Deepest level: two plots, the first carries the chrome that needs
    // to propagate all the way up to the root composition.
    let leaf = beside(
        Patch::new("leaf_l")
            .slot(Slot::AxisTop, text_cell("axis_top from deepest leaf", 14.0))
            .slot(Slot::AxisBottom, text_cell("axis_bottom from leaf", 11.0))
            .slot(Slot::Panel, Cell::empty()),
        plain("leaf_r"),
    );
    // Mid level: a plain plot beside the leaf composition.
    let mid = beside(plain("mid"), leaf);
    // Root level: a plain plot beside the mid composition.
    let composed = beside(plain("root"), mid);

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

        if let Some(rect) = layout.get("leaf_l", Slot::AxisTop) {
            let run = TextRun::new("axis_top from deepest leaf", &TextStyle::new(14.0));
            draw_text_in_rect(scene, &run, rect, &text_brush, PickId::Skip);
        }
        if let Some(rect) = layout.get("leaf_l", Slot::AxisBottom) {
            let run = TextRun::new("axis_bottom from leaf", &TextStyle::new(11.0));
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
        .join("examples/nesting_deep.png");
    hephaestus::png::write_png(&path, w, h, &pixels).expect("write png");
    println!("wrote {}", path.display());

    // Print panel y0s so the user can verify alignment across all 3 nesting levels.
    let root_panel = layout.get("root", Slot::Panel).unwrap();
    let mid_panel = layout.get("mid", Slot::Panel).unwrap();
    let leaf_l_panel = layout.get("leaf_l", Slot::Panel).unwrap();
    let leaf_r_panel = layout.get("leaf_r", Slot::Panel).unwrap();
    println!(
        "panel y0: root={}, mid={}, leaf_l={}, leaf_r={}",
        root_panel.y0, mid_panel.y0, leaf_l_panel.y0, leaf_r_panel.y0
    );
    println!("(all four should be equal — propagation across 3 nesting levels)");
}
