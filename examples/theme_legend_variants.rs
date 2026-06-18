//! Demonstrates named legend variants — a plot with two legends
//! where one opts into a `"hero"` variant registered on the theme,
//! and the other uses the default. This phase wires the API
//! surface (`Legend::theme_variant(name)` + `Theme::legend_for`);
//! full per-variant styling consumption happens in F.2 when the
//! legend renderer migrates to consume LegendTheme fields directly.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb, rgb8, Color};
use hephaestus::composition::{Composition, Patch, Span};
use hephaestus::geometry::Size;
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement};
use hephaestus::plot::chrome::legend::{Legend, LegendKeySpec};
use hephaestus::plot::theme::{Element, LegendTheme, Length, RectElement, Theme, ThemeColor};
use hephaestus::plot::{scale, Plot, PlotComposition, PointGeom};
use hephaestus::scales::chrome::{AxisSide, LegendSide};
use hephaestus::scales::value::Value;
use hephaestus::Renderer;

fn comp() -> Composition {
    Composition::empty(1, 1).place(1, 1, Span::cell(), Patch::new("panel"))
}

fn main() {
    let (w, h) = (900u32, 600u32);
    let dpi = 96.0;

    let n = 24;
    let xs: Vec<f64> = (0..n).map(|i| i as f64 * 0.4).collect();
    let ys: Vec<f64> = (0..n)
        .map(|i| ((i as f64) * 0.5).sin() * 0.4 + 0.5)
        .collect();
    let cats: [&'static str; 4] = ["A", "B", "C", "D"];
    let fill_col: Vec<&'static str> = (0..n).map(|i| cats[i % 4]).collect();

    let mut plot = Plot::new(&comp(), "panel")
        .bind("x", "x")
        .bind("y", "y")
        .bind("fill", "category_color")
        .title("Two legends, one opts into the \"hero\" theme variant");
    plot.add_geom(
        PointGeom::builder()
            .set("x", xs)
            .set("y", ys)
            .set("fill", fill_col.clone())
            .set("size", 6.0_f64)
            .build(),
    );
    plot.add_axis(Axis::rail("x", AxisPlacement::Cartesian(AxisSide::Bottom)));
    plot.add_axis(Axis::rail("y", AxisPlacement::Cartesian(AxisSide::Left)));

    // First legend — opts into the "hero" variant.
    plot.add_legend(
        Legend::new("category_color")
            .side(LegendSide::Right)
            .title("Category (hero)")
            .theme_variant("hero")
            .key(
                LegendKeySpec::point()
                    .scaled("fill", "category_color")
                    .fixed("stroke", Value::Color(rgb(0.0, 0.0, 0.0)))
                    .fixed("size", 6.0_f64),
            ),
    );
    // Second legend — uses the default LegendTheme.
    plot.add_legend(
        Legend::new("category_size")
            .side(LegendSide::Bottom)
            .title("Category (default)")
            .key(
                LegendKeySpec::point()
                    .scaled("fill", "category_color")
                    .fixed("size", 6.0_f64),
            ),
    );

    // Register a "hero" variant on the theme. Distinct background
    // tint + a denser margin to telegraph the emphasis.
    let hero = LegendTheme {
        background: Element::Set(RectElement {
            fill: Some(ThemeColor::mix(ThemeColor::Paper, ThemeColor::Accent, 0.18)),
            color: Some(ThemeColor::Accent),
            linewidth_pt: Some(Length::Abs(1.0)),
            ..RectElement::default()
        }),
        ..LegendTheme::default()
    };

    let theme = Theme::default().with_legend_variant("hero", hero);

    let mut view = PlotComposition::new(comp())
        .add_scale("x", scale::continuous(0.0..=12.0))
        .add_scale("y", scale::continuous(0.0..=1.0))
        .add_scale(
            "category_color",
            scale::discrete(cats.iter().map(|s| Value::String((*s).into()))).range_colors([
                rgb8(220, 100, 80),
                rgb8(80, 160, 100),
                rgb8(80, 130, 200),
                rgb8(180, 100, 200),
            ]),
        )
        .add_scale(
            "category_size",
            scale::discrete(cats.iter().map(|s| Value::String((*s).into())))
                .range_numbers([4.0, 6.0, 8.0, 10.0]),
        )
        .theme(theme);
    view.attach_plot(plot);

    let mut renderer = VelloRenderer::new().expect("vello renderer init");
    let bg: Color = rgb8(252, 252, 252);
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
        .join("examples/theme_legend_variants.png");
    hephaestus::png::write_png(&path, w, h, &pixels).expect("write png");
    println!("wrote {}", path.display());
}
