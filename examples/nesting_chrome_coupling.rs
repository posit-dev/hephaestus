//! Visual stress test: bidirectional sizer coupling between an outer
//! composition cell and a nested composition's inner border chrome.
//!
//! Layout: outer 1×2 composition.
//! - Cell (1,1): a plain patch with NO axis_top.
//! - Cell (1,2): a nested 1×3 composition where the MIDDLE inner patch
//!   has a tall axis_top; the others don't.
//!
//! The outer-grid row band 1..16 is shared across BOTH outer blocks (they
//! sit in the same composition row). With patchwork-style sizer coupling:
//! 1. The sub-Grid's inner row 8 (AxisTop anatomical row) resolves to the
//!    max of its inner cells — so it grows to fit the middle patch's tall
//!    axis_top.
//! 2. The forward sizer in the outer points at that resolved size; outer
//!    row 8 grows to match.
//! 3. The back sizer in the sub points at outer row 8; ALL inner blocks'
//!    row-8 tracks grow to that same height.
//!
//! Visually: the plain patch's panel starts at the same y as all three
//! inner panels, even though only one of four plots actually has an
//! axis_top contributing content. The chrome row is empty on the plain
//! side (white space above its panel) — that empty space is the
//! propagated max chrome height.
//!
//! Writes `examples/nesting_chrome_coupling.png`.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{beside, grid, Patch, Slot};
use hephaestus::layout::Cell;
use hephaestus::text::{draw_text_in_rect, TextRun, TextStyle};
use hephaestus::{Affine, Brush, FillRule, Path, PickId, Renderer, SceneBuilder};
use kurbo::Shape;

fn text_cell(text: &str, size: f32) -> Cell {
    Cell::measured(TextRun::new(text, &TextStyle::new(size)))
}

fn plain_plot(id: &str) -> Patch {
    Patch::new(id).slot(Slot::Panel, Cell::empty())
}

fn plot_with_axis_top(id: &str, axis_top_text: &str) -> Patch {
    Patch::new(id)
        .slot(Slot::AxisTop, text_cell(axis_top_text, 14.0))
        .slot(Slot::Panel, Cell::empty())
}

fn color_for_region(region: &str) -> Color {
    match region {
        "panel" => rgb8(40, 60, 90),
        "axis_top" => rgb8(200, 100, 130),
        _ => rgb8(120, 120, 120),
    }
}

fn main() {
    let (w, h) = (1400u32, 400u32);
    let dpi = 96.0;

    let plain = plain_plot("plain");
    let nested = grid(
        1,
        3,
        vec![
            plain_plot("c1").into(),
            plot_with_axis_top("c2", "this is a TALL axis_top label\nspanning multiple lines\nto stress chrome propagation").into(),
            plain_plot("c3").into(),
        ],
    );
    let composed = beside(plain, nested);
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
        // Label the only chrome that has text content.
        if let Some(rect) = layout.get("c2", Slot::AxisTop) {
            let run = TextRun::new(
                "this is a TALL axis_top label\nspanning multiple lines\nto stress chrome propagation",
                &TextStyle::new(14.0),
            );
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
        .join("examples/nesting_chrome_coupling.png");
    hephaestus::png::write_png(&path, w, h, &pixels).expect("write png");
    println!("wrote {}", path.display());

    // Print the panel y-coordinates so the user can sanity-check that
    // the plain panel starts at the same y as the inner panels even
    // though it has no axis_top.
    let plain_y = layout.get("plain", Slot::Panel).unwrap();
    let c1_y = layout.get("c1", Slot::Panel).unwrap();
    let c2_y = layout.get("c2", Slot::Panel).unwrap();
    let c3_y = layout.get("c3", Slot::Panel).unwrap();
    println!(
        "panel y0: plain={}, c1={}, c2={}, c3={}",
        plain_y.y0, c1_y.y0, c2_y.y0, c3_y.y0
    );
    println!("(all four should be equal — bidirectional sizer coupling at work)");
}
