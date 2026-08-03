//! Render-path integration test for binned material scales.
//!
//! A binned scale carries its bin edges independently of its output
//! range, so the same scale kind that positions data at bin centres can
//! also drive fill / stroke / size / linetype from a per-bin palette.
//! This drives the full orchestrator → geom → scene path and asserts
//! each bin's colour reaches both the panel marks and the legend keys.

use hephaestus::brush::Brush;
use hephaestus::color::{rgb, Color};
use hephaestus::composition::{beside, Composition, Patch};
use hephaestus::geometry::Size;
use hephaestus::plot::chrome::legend::{Legend, LegendKeySpec};
use hephaestus::plot::{scale, Plot, PlotComposition, PointGeom};
use hephaestus::scene::recording::{Op, RecordingScene};

const W: f64 = 400.0;
const H: f64 = 300.0;
const DPI: f64 = 96.0;

fn solid_color(brush: &Brush) -> Option<Color> {
    match brush {
        Brush::Solid(c) => Some(*c),
        _ => None,
    }
}

fn color_eq(a: Color, b: Color) -> bool {
    a.components
        .iter()
        .zip(b.components.iter())
        .all(|(x, y)| (x - y).abs() <= 1.0 / 255.0)
}

fn count_fills_with_color(ops: &[Op], target: Color) -> usize {
    ops.iter()
        .filter(|op| match op {
            Op::Fill { brush, .. } => solid_color(brush)
                .map(|c| color_eq(c, target))
                .unwrap_or(false),
            _ => false,
        })
        .count()
}

#[test]
fn binned_fill_scale_paints_one_palette_entry_per_bin() {
    let red = rgb(1.0, 0.0, 0.0);
    let green = rgb(0.0, 1.0, 0.0);
    let blue = rgb(0.0, 0.0, 1.0);

    let template = beside(Patch::new("p"), Patch::new("__pad"));
    let mut view = PlotComposition::new(&template)
        .add_scale("x", scale::continuous(0.0..=1.0))
        .add_scale("y", scale::continuous(0.0..=1.0))
        .add_scale(
            "bin_fill",
            scale::binned(0.0..=30.0, vec![0.0, 10.0, 20.0, 30.0]).range_colors([red, green, blue]),
        );

    let dummy: Composition = beside(Patch::new("p"), Patch::new("__pad"));
    let mut p = Plot::new(&dummy, "p")
        .bind("x", "x")
        .bind("y", "y")
        .bind("fill", "bin_fill");
    p.add_geom(
        PointGeom::builder()
            .set("x", vec![0.2_f64, 0.5, 0.8])
            .set("y", vec![0.5_f64, 0.5, 0.5])
            .set("fill", vec![5.0_f64, 15.0, 25.0])
            .set("size", vec![10.0_f64, 10.0, 10.0])
            .build(),
    );
    // One legend row per bin, each key filled through the same scale.
    p.add_legend(
        Legend::new("bin_fill")
            .binned()
            .key(LegendKeySpec::rect().scaled("fill", "bin_fill")),
    );
    view.attach_plot(p);

    let mut scene = RecordingScene::default();
    view.render(&mut scene, Size::new(W, H), DPI);

    // Each palette entry paints its own mark in the panel plus its own
    // legend key — two fills apiece, and never zero (the failure mode
    // when the bin definition and the palette compete for one slot).
    for (name, color) in [("red", red), ("green", green), ("blue", blue)] {
        assert!(
            count_fills_with_color(&scene.ops, color) >= 2,
            "expected a panel mark and a legend key filled {name}, got {}",
            count_fills_with_color(&scene.ops, color)
        );
    }
}
