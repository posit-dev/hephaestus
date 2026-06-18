//! Facet-strip demo — labels on each side of a faceted layout.
//!
//! Four panels arranged in a 2×2 grid, each carrying labelled strips:
//! the column variable on top, the row variable on the right. Strip
//! styling (background fill, border, text size, rotation, padding)
//! flows from the [`Theme`]'s `strip_background` / `strip_text` /
//! `strip_padding` slots — the example tweaks the default to give the
//! strips a more obviously visible background.
//!
//! Produces `examples/facet_strips.png`.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{grid, Composition, Element, Patch};
use hephaestus::geometry::Size;
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement};
use hephaestus::plot::theme::{pt, Length, Margin, RectElement, Sided, Theme, ThemeColor};
use hephaestus::plot::{scale, Plot, PlotComposition, PointGeom};
use hephaestus::scales::chrome::AxisSide;
use hephaestus::Renderer;

fn comp_shape() -> Composition {
    let facets: Vec<Element> = ["q1", "q2", "q3", "q4"]
        .into_iter()
        .map(|id| Patch::new(id).into())
        .collect();
    grid(2, 2, facets)
}

fn brandy_theme() -> Theme {
    // Tinted strip background so the example reads at a glance.
    Theme {
        strip_background: Sided::new(RectElement {
            fill: Some(ThemeColor::mix(ThemeColor::Paper, ThemeColor::Ink, 0.22)),
            color: Some(ThemeColor::Ink),
            linewidth_pt: Some(Length::Abs(0.5)),
            corner_radius: Some(pt(3.0)),
            ..RectElement::default()
        }),
        strip_padding: Margin::all(pt(6.0)),
        ..Theme::default()
    }
}

fn main() {
    let (w, h) = (900u32, 700u32);
    let dpi = 96.0;

    let xs: Vec<f64> = (0..40).map(|i| i as f64 * 2.5).collect();
    let make = |phase: f64, amp: f64| -> Vec<f64> {
        xs.iter()
            .map(|x| 50.0 + amp * (x * 0.05 + phase).sin())
            .collect()
    };

    // (patch id, label_top, label_right, ys, color)
    let panels = [
        ("q1", "Setosa", "Spring", make(0.0, 22.0), rgb8(220, 90, 70)),
        (
            "q2",
            "Versicolor",
            "Spring",
            make(1.0, 18.0),
            rgb8(70, 120, 220),
        ),
        (
            "q3",
            "Setosa",
            "Summer",
            make(2.0, 25.0),
            rgb8(70, 180, 120),
        ),
        (
            "q4",
            "Versicolor",
            "Summer",
            make(3.0, 28.0),
            rgb8(180, 130, 80),
        ),
    ];

    let mut view = PlotComposition::new(comp_shape())
        .theme(brandy_theme())
        .add_scale("time", scale::continuous(0.0..=100.0))
        .add_scale("y", scale::continuous(0.0..=100.0));

    for (id, top, right, ys, color) in &panels {
        let mut p = Plot::new(&comp_shape(), *id)
            .bind("x", "time")
            .bind("y", "y")
            .strip(AxisSide::Top, *top)
            .strip(AxisSide::Right, *right);
        p.add_geom(
            PointGeom::builder()
                .set("x", xs.clone())
                .set("y", ys.clone())
                .set("fill", *color)
                .set("size", 4.0_f64)
                .build(),
        );
        p.add_axis(Axis::rail(
            "time",
            AxisPlacement::Cartesian(AxisSide::Bottom),
        ));
        p.add_axis(Axis::rail("y", AxisPlacement::Cartesian(AxisSide::Left)));
        view.attach_plot(p);
    }

    let mut renderer = VelloRenderer::new().expect("vello renderer init");
    let bg: Color = rgb8(248, 248, 252);
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
        .join("examples/facet_strips.png");
    hephaestus::png::write_png(&path, w, h, &pixels).expect("write png");
    println!("wrote {}", path.display());
}
