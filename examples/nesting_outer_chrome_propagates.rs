//! Visual stress test: top-level chrome propagating INTO a nested
//! composition's chrome cell.
//!
//! Mirror of `nesting_chrome_coupling.rs`: this time the chrome lives on
//! the OUTER plain sibling, and we verify that the nested composition's
//! inner chrome row grows to match.
//!
//! Layout: outer 1×2 composition.
//! - Cell (1,1): a plain patch with a TALL axis_top (multi-line text).
//! - Cell (1,2): a nested 1×3 composition where ALL three inner patches
//!   are plain (no axis_top of their own).
//!
//! Propagation chain:
//! 1. The outer Auto row 8 (axis_top anatomical row) is shared across
//!    both outer blocks in the same composition row. The plain patch's
//!    `Slot::AxisTop` cell contributes its content height; the row
//!    resolves to that height.
//! 2. The BACK sizer in the nested sub-Grid (at every inner top-row
//!    block's anatomical row 8) reads outer row 8 via
//!    `Length::track_of(outer_id, Axis::Height, ...)` and forces the
//!    sub-Grid's inner row 8 to that same height.
//! 3. The inner panels in the nested composition all sit below that
//!    grown chrome row — so even though the inner patches have no
//!    axis_top content of their own, their panels are pushed down by
//!    the outer plain plot's axis_top height.
//!
//! Visually: the outer plain patch has its axis_top label rendered in
//! the chrome row; the nested side has empty (white) space above its
//! three panels exactly matching that chrome row's height.
//!
//! Writes `examples/nesting_outer_chrome_propagates.png`.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{beside, grid, Patch, Slot};
use hephaestus::layout::Cell;
use hephaestus::text::{draw_text_in_rect, TextRun, TextStyle};
use hephaestus::{Affine, Brush, FillRule, Path, PickId, Renderer, SceneBuilder};
use kurbo::Shape;

fn text_cell(text: &str, size: f32) -> Cell {
    Cell::measured(TextRun::new(text, &TextStyle::new(size), 96.0))
}

fn plain_plot(id: &str) -> Patch {
    Patch::new(id).slot(Slot::Panel, Cell::empty())
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

    // Top-level chrome lives on the LEFT sibling. The right side is a
    // nested composition with three plain inner patches.
    let outer_with_chrome = Patch::new("outer_plain")
        .slot(
            Slot::AxisTop,
            text_cell(
                "axis_top on the outer SIBLING patch\nspans multiple lines\nto propagate INTO the nested composition",
                14.0,
            ),
        )
        .slot(Slot::Panel, Cell::empty());
    let nested = grid(
        1,
        3,
        vec![
            plain_plot("c1").into(),
            plain_plot("c2").into(),
            plain_plot("c3").into(),
        ],
    );
    let composed = beside(outer_with_chrome, nested);
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

        if let Some(rect) = layout.get("outer_plain", Slot::AxisTop) {
            let run = TextRun::new(
                "axis_top on the outer SIBLING patch\nspans multiple lines\nto propagate INTO the nested composition",
                &TextStyle::new(14.0),
                96.0,
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
        .join("examples/nesting_outer_chrome_propagates.png");
    hephaestus::png::write_png(&path, w, h, &pixels).expect("write png");
    println!("wrote {}", path.display());

    // Sanity check: all four panels share y0, even though only the outer
    // sibling carries the axis_top content. The back sizer drives the
    // nested inner row 8 from the outer-resolved row 8.
    let outer_y = layout.get("outer_plain", Slot::Panel).unwrap();
    let c1_y = layout.get("c1", Slot::Panel).unwrap();
    let c2_y = layout.get("c2", Slot::Panel).unwrap();
    let c3_y = layout.get("c3", Slot::Panel).unwrap();
    println!(
        "panel y0: outer_plain={}, c1={}, c2={}, c3={}",
        outer_y.y0, c1_y.y0, c2_y.y0, c3_y.y0
    );
    println!("(all four should be equal — outer chrome propagated INTO the nested composition via back sizers)");
}
