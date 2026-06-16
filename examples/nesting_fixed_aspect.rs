//! Visual demo: fixed-aspect plots composed with deeply-nested flex
//! plots, all sharing the same outer composition row.
//!
//! Layout: a 1×5 outer composition with `fixed_l` and `fixed_r` at the
//! two ends (both `.aspect(1, 1)`), and three flex plots in the middle
//! arranged in a 3-level-deep beside chain
//! (`beside(flex_a, beside(flex_b, flex_c))`).
//!
//! ```text
//!   ┌─────────┬─────────┬──────┬──────┬─────────┐
//!   │ fixed_l │ flex_a  │flex_b│flex_c│ fixed_r │
//!   └─────────┴─────────┴──────┴──────┴─────────┘
//! ```
//!
//! Both fixed-aspect plots and all flex plots share the **same outer
//! row**, so all five panel tops/bottoms align. Selective respect on
//! the outer grid couples `fixed_l` and `fixed_r`'s panel cells at
//! 1:1; the three flex panel columns (one direct child plus a 2-deep
//! nested composition) absorb the horizontal slack at their respective
//! nesting levels.
//!
//! Compare against `nesting_deep.png` — same horizontal layout style,
//! but with two aspect-locked anchors and chrome on the locked panels.
//!
//! Note on alignment: fixed-aspect plots align with their siblings
//! when they share the outermost composition row (this example).
//! Putting one fixed plot at the outer level and another deep inside a
//! nested composition currently produces different panel heights —
//! the forward/back sizer chain couples chrome rows across nesting but
//! not the panel row. Best practice for v1.5: keep fixed-aspect plots
//! at the same composition level so they share the same panel row.
//!
//! Writes `examples/nesting_fixed_aspect.png`.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{beside, Patch, Slot};
use hephaestus::layout::Cell;
use hephaestus::text::{draw_text_in_rect, TextRun, TextStyle};
use hephaestus::{Affine, Brush, FillRule, Path, PickId, Renderer, SceneBuilder};
use kurbo::Shape;

fn text_cell(text: &str, size: f32) -> Cell {
    Cell::measured(TextRun::new(text, &TextStyle::new(size), 96.0))
}

fn plain(id: &str) -> Patch {
    Patch::new(id).slot(Slot::Panel, Cell::empty())
}

fn fixed_square(id: &str, label: &str) -> Patch {
    // Chrome on a fixed patch is fine — the solver's second iteration
    // picks up the resolved Auto-row heights from iter 0 and reshapes
    // the respected fr distribution to honour the lock anyway. The
    // axis_top here proves it: panels still report ratio = 1.000.
    Patch::new(id)
        .aspect(1.0, 1.0)
        .slot(Slot::AxisTop, text_cell(label, 12.0))
        .slot(Slot::Panel, Cell::empty())
}

fn color_for(id: &str, region: &str) -> Color {
    match (id, region) {
        (_, "panel") if id.starts_with("fixed") => rgb8(80, 140, 80), // green for locked
        (_, "panel") => rgb8(40, 60, 90),                             // blue for flex
        (_, "axis_top") => rgb8(200, 100, 130),
        _ => rgb8(120, 120, 120),
    }
}

fn main() {
    // Wide viewport so the lock-vs-flex difference is obvious.
    let (w, h) = (1600u32, 400u32);
    let dpi = 96.0;

    // Three flex plots in a 2-level beside chain — flex_a is at the
    // outer level beside the deeper composition; flex_b and flex_c sit
    // one level deeper.
    let flex_chain = beside(plain("flex_a"), beside(plain("flex_b"), plain("flex_c")));
    // Both fixed-aspect plots sit in the SAME outermost composition row
    // as the flex chain. `Composition::beside` extends an existing 1-row
    // composition by appending a cell — all five end up as direct
    // siblings of the same outer grid, sharing one panel row.
    let composed =
        beside(fixed_square("fixed_l", "1:1"), flex_chain).beside(fixed_square("fixed_r", "1:1"));

    let layout = composed.solve(hephaestus::Size::new(w as f64, h as f64), dpi);

    let mut renderer = VelloRenderer::new().expect("vello renderer init");
    {
        let scene = renderer.scene();
        let stroke = hephaestus::stroke::Stroke::new(1.0);
        let text_brush: Brush = rgb8(20, 20, 30).into();

        // Chrome first (axes / etc.).
        for (id, region, rect) in layout.iter() {
            if region == "panel" {
                continue;
            }
            let c = color_for(id, region);
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
        // Panels (with green for locked, blue for flex).
        for (id, region, rect) in layout.iter() {
            if region != "panel" {
                continue;
            }
            let path: Path = rect.to_path(0.1);
            scene.fill(
                FillRule::NonZero,
                Affine::IDENTITY,
                &Brush::Solid(color_for(id, region)),
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
        // Panel-centre labels noting locked vs flex + actual width:height.
        for (id, region, rect) in layout.iter() {
            if region != "panel" {
                continue;
            }
            let w = rect.x1 - rect.x0;
            let h = rect.y1 - rect.y0;
            let label = if id.starts_with("fixed") {
                format!("{id}\nlocked 1:1\n{w:.0}×{h:.0}")
            } else {
                format!("{id}\nflex\n{w:.0}×{h:.0}")
            };
            let run = TextRun::new(&label, &TextStyle::new(13.0).weight(500), 96.0);
            let brush: Brush = rgb8(255, 255, 255).into();
            draw_text_in_rect(scene, &run, rect, &brush, PickId::Skip);
        }
        // Axis-top labels on the two fixed patches.
        for id in &["fixed_l", "fixed_r"] {
            if let Some(rect) = layout.get(id, Slot::AxisTop) {
                let run = TextRun::new(&format!("{id} 1:1"), &TextStyle::new(12.0), 96.0);
                draw_text_in_rect(scene, &run, rect, &text_brush, PickId::Skip);
            }
        }
    }

    let mut pixels = vec![0u8; (w * h * 4) as usize];
    let bg: Color = rgb8(248, 248, 252);
    renderer
        .render_to_buffer(w, h, bg, &mut pixels)
        .expect("render");

    let path = std::env::current_dir()
        .unwrap()
        .join("examples/nesting_fixed_aspect.png");
    hephaestus::png::write_png(&path, w, h, &pixels).expect("write png");
    println!("wrote {}", path.display());

    // Print per-panel widths/heights so the aspect locks are verifiable
    // from a terminal too. All five panels should report the same
    // height (alignment across the outer row); fixed_l and fixed_r
    // additionally have width == height (locked 1:1).
    for id in &["fixed_l", "flex_a", "flex_b", "flex_c", "fixed_r"] {
        let p = layout.get(id, Slot::Panel).unwrap();
        let pw = p.x1 - p.x0;
        let ph = p.y1 - p.y0;
        let ratio = pw / ph;
        let tag = if id.starts_with("fixed") {
            "locked 1:1"
        } else {
            "flex"
        };
        println!("{id:>8} ({tag:>10}):  w={pw:>5.1}  h={ph:>5.1}  ratio={ratio:.3}");
    }
    println!("(all five panels share one outer row → all heights equal;");
    println!(" fixed_l and fixed_r additionally report ratio ≈ 1.000.)");
}
