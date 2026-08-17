//! Chrome text that outgrows the space it sits in.
//!
//! Two panels, each carrying label text wider than the panel it
//! belongs to:
//!
//! - **Tick labels containing spaces.** A discrete scale whose levels
//!   read `Bin (2.5, 5.0]` centres each label on its tick across the
//!   label's full width, spaces included.
//! - **A top strip that has to wrap.** The strip's row grows to hold
//!   every line the label breaks into, so no line is clipped by the
//!   strip background.
//! - **A right strip reading along the panel edge.** Rotated strip
//!   text breaks against the edge it runs along — the panel's height —
//!   so a multi-word label stays on one line.
//!
//! Produces `examples/long_labels.png`.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{grid, Composition, Element, Patch};
use hephaestus::geometry::Size;
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement};
use hephaestus::plot::theme::{pt, Length, Margin, RectElement, Sided, Theme, ThemeColor};
use hephaestus::plot::{scale, Plot, PlotComposition, PointGeom};
use hephaestus::scales::chrome::AxisSide;
use hephaestus::scales::value::Value;
use hephaestus::scene::SceneBuilder;
use hephaestus::Renderer;

fn comp_shape() -> Composition {
    let facets: Vec<Element> = ["low", "high"]
        .into_iter()
        .map(|id| Patch::new(id).into())
        .collect();
    grid(1, 2, facets)
}

/// Tinted strip background with a visible border, so the reserved
/// band and the label lines inside it read at a glance.
fn strip_theme() -> Theme {
    Theme {
        strip_background: Sided::new(RectElement {
            fill: Some(ThemeColor::mix(ThemeColor::Paper, ThemeColor::Ink, 0.22)),
            color: Some(ThemeColor::Ink),
            linewidth_pt: Some(Length::Abs(0.5)),
            corner_radius: Some(pt(3.0)),
            ..RectElement::default()
        }),
        strip_padding: Margin::all(pt(5.0)),
        ..Theme::default()
    }
}

fn main() {
    let (w, h) = (700u32, 420u32);
    let dpi = 96.0;

    // (patch id, x scale name, levels, top strip, right strip)
    let panels = [
        (
            "low",
            "low_bins",
            ["Bin (2.5, 5.0]", "Bin (5.0, 7.5]"],
            "Sepal width in (2.5, 5.0] millimetres of observed range",
            "First half",
        ),
        (
            "high",
            "high_bins",
            ["Bin (7.5, 10.0]", "Bin (10.0, 12.5]"],
            "Sepal width in (7.5, 12.5] millimetres of observed range",
            "Second half",
        ),
    ];

    let mut view = PlotComposition::new(&comp_shape())
        .theme(strip_theme())
        .add_scale("y", scale::continuous(0.0..=100.0));
    for (_, scale_name, levels, _, _) in &panels {
        view = view.add_scale(
            *scale_name,
            scale::discrete(levels.iter().map(|s| Value::from(*s))),
        );
    }

    for (id, scale_name, levels, top, right) in &panels {
        let mut p = Plot::new(&comp_shape(), *id)
            .bind("x", *scale_name)
            .bind("y", "y")
            .strip(AxisSide::Top, *top)
            .strip(AxisSide::Right, *right);
        p.add_geom(
            PointGeom::builder()
                .set("x", levels.to_vec())
                .set("y", vec![35.0_f64, 72.0])
                .set("fill", rgb8(70, 120, 220))
                .set("size", 6.0_f64)
                .build(),
        );
        p.add_axis(Axis::rail(
            *scale_name,
            AxisPlacement::Cartesian(AxisSide::Bottom),
        ));
        p.add_axis(Axis::rail("y", AxisPlacement::Cartesian(AxisSide::Left)));
        view.attach_plot(p);
    }

    let mut renderer = VelloRenderer::new().expect("vello renderer init");
    let bg: Color = rgb8(250, 250, 252);
    {
        let scene = renderer.scene();
        scene.clear();
        view.render(scene, Size::new(w as f64, h as f64), dpi);
    }
    let mut pixels = vec![0u8; (w * h * 4) as usize];
    renderer
        .render_to_buffer(w, h, bg, &mut pixels)
        .expect("render");
    let path = std::env::current_dir()
        .unwrap()
        .join("examples/long_labels.png");
    hephaestus::png::write_png(&path, w, h, &pixels).expect("write png");
    println!("wrote {}", path.display());
}
