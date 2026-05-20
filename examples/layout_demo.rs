//! Visual smoke test for the layout engine. Solves a small nested grid with
//! the headline-example placement (inner 2×2 in row 2, cols 3..=5 of a 5×3
//! outer, inset 1cm from the left and ending at 75% width) plus a 2:1
//! aspect-ratio cell, then draws every resolved rect with `VelloRenderer`.
//!
//! Writes `examples/layout_demo.png` next to this file.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::layout::{CellId, Grid, Inset, Length, Placement, Track};
use hephaestus::stroke::Stroke;
use hephaestus::{Affine, Brush, FillRule, Path, Renderer, SceneBuilder};
use kurbo::Shape;

fn main() {
    let (w, h) = (800u32, 600u32);
    let dpi = 96.0;

    // Outer: 5 columns × 3 rows of fr(1).
    let mut root = Grid::new(vec![Track::Fr(1.0); 5], vec![Track::Fr(1.0); 3]);

    // Tag every outer cell so we can outline the underlying grid.
    let mut outer_id = 1u64;
    for r in 1..=3u16 {
        for c in 1..=5u16 {
            root.place(Placement::at(r, c), Grid::cell().id(CellId(outer_id)));
            outer_id += 1;
        }
    }

    // Inner 2×2 in row 2, cols 3..=5, with 1cm left + 25% right insets.
    let mut inner = Grid::new(
        [Track::Fr(1.0), Track::Fr(1.0)],
        [Track::Fr(1.0), Track::Fr(1.0)],
    );
    for r in 1..=2u16 {
        for c in 1..=2u16 {
            inner.place(
                Placement::at(r, c),
                Grid::cell().id(CellId(100 + (r as u64) * 2 + c as u64)),
            );
        }
    }
    root.place(
        Placement::at(2, 3).span(1, 3).inset(
            Inset::default()
                .left(Length::cm(1.0))
                .right(Length::percent(0.25)),
        ),
        inner,
    );

    // A 2:1 cell in the top-left, expressed via respect + fr weights.
    root.place(
        Placement::at(1, 1),
        Grid::new([Track::Fr(2.0)], [Track::Fr(1.0)])
            .respect()
            .id(CellId(200)),
    );

    let layout = root.solve(hephaestus::Size::new(w as f64, h as f64), dpi);

    // Render.
    let mut renderer = VelloRenderer::new().expect("vello renderer init");
    {
        let scene = renderer.scene();
        let outer_brush: Brush = rgb8(80, 90, 110).into();
        let outer_stroke = Stroke::new(1.0);
        for id in 1..=15u64 {
            if let Some(rect) = layout.rect(CellId(id)) {
                let path: Path = rect.to_path(0.1);
                scene.stroke(&outer_stroke, Affine::IDENTITY, &outer_brush, None, &path);
            }
        }

        let inner_brush: Brush = rgb8(240, 180, 60).into();
        let inner_stroke = Stroke::new(2.0);
        for id in [103u64, 104, 105, 106] {
            if let Some(rect) = layout.rect(CellId(id)) {
                let path: Path = rect.to_path(0.1);
                scene.fill(
                    FillRule::NonZero,
                    Affine::IDENTITY,
                    &Brush::Solid(rgb8(245, 220, 160)),
                    None,
                    &path,
                );
                scene.stroke(&inner_stroke, Affine::IDENTITY, &inner_brush, None, &path);
            }
        }

        let aspect_brush: Brush = rgb8(60, 160, 220).into();
        if let Some(rect) = layout.rect(CellId(200)) {
            let path: Path = rect.to_path(0.1);
            scene.fill(
                FillRule::NonZero,
                Affine::IDENTITY,
                &aspect_brush,
                None,
                &path,
            );
        }
    }

    let mut pixels = vec![0u8; (w * h * 4) as usize];
    let bg: Color = rgb8(20, 22, 28);
    renderer
        .render_to_buffer(w, h, bg, &mut pixels)
        .expect("render");

    let path = std::env::current_dir()
        .unwrap()
        .join("examples/layout_demo.png");
    hephaestus::png::write_png(&path, w, h, &pixels).expect("write png");
    println!("wrote {}", path.display());
}
