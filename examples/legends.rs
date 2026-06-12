//! Legend rendering — manual `Legend` API.
//!
//! Three legends, one per side:
//!
//! 1. **Right** (top stack): two keys merged into one legend via the
//!    auto-merge in `add_legend` — a `Line` key whose stroke is
//!    scaled by the category colour, and a `Point` key whose fill is
//!    scaled by the same colour scale but whose stroke is **fixed**
//!    black. The Point's stroke does NOT pick up the line's stroke
//!    scale because each key carries its own per-aesthetic bindings.
//!
//! 2. **Top**: a `Point` key whose size is scaled by `category_size`.
//!
//! 3. **Bottom**: a `Line` key whose linetype is scaled by
//!    `category_line`.
//!
//! Produces `examples/legends.png`.

use std::sync::Arc;

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb, rgb8, Color};
use hephaestus::composition::{Composition, Patch, Span};
use hephaestus::geometry::Size;
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement};
use hephaestus::plot::chrome::legend::{Legend, LegendKeySpec};
use hephaestus::plot::geom::linetype::{dashed, dotted, solid};
use hephaestus::plot::{scale, Plot, PlotComposition, PointGeom};
use hephaestus::scales::chrome::{Anchor, AxisSide, LegendSide};
use hephaestus::scales::value::Value;
use hephaestus::shape::ShapeRegistry;
use hephaestus::text::{glyph_marker, TextStyle};
use hephaestus::Renderer;

fn comp() -> Composition {
    Composition::empty(1, 1).place(1, 1, Span::cell(), Patch::new("panel"))
}

fn main() {
    let (w, h) = (900u32, 600u32);
    let dpi = 96.0;

    // ── Data ──
    let n = 24;
    let xs: Vec<f64> = (0..n).map(|i| i as f64 * 0.4).collect();
    let ys: Vec<f64> = (0..n)
        .map(|i| ((i as f64) * 0.5).sin() * 0.4 + 0.5)
        .collect();
    let cats: [&'static str; 4] = ["A", "B", "C", "D"];
    let fill_col: Vec<&'static str> = (0..n).map(|i| cats[i % 4]).collect();
    let size_col: Vec<&'static str> = (0..n).map(|i| cats[i % 4]).collect();

    // Build the plot's shape registry with the built-in vector
    // shapes plus a handful of emoji glyphs used by the binned
    // legend below. Glyph shapes share the same registry surface
    // as the vector ones (insert by name; look up by name) so the
    // legend chrome can resolve either.
    let glyph_style = TextStyle::new(16.0);
    let mut shapes = ShapeRegistry::with_builtins();
    // U+1F4A7 droplet, U+1F31E sun, U+1F525 fire, U+1F33F herb,
    // U+1F30A wave — picked to read as a "low → high" intensity
    // ramp matching the gradient_scale values.
    shapes.insert("droplet", glyph_marker("\u{1F4A7}", &glyph_style));
    shapes.insert("herb", glyph_marker("\u{1F33F}", &glyph_style));
    shapes.insert("sun", glyph_marker("\u{1F31E}", &glyph_style));
    shapes.insert("wave", glyph_marker("\u{1F30A}", &glyph_style));
    shapes.insert("fire", glyph_marker("\u{1F525}", &glyph_style));

    let mut p = Plot::new(&comp(), "panel")
        .bind("x", "x")
        .bind("y", "y")
        .bind("fill", "category_color")
        .bind("size", "category_size")
        .shape_registry(shapes);
    p.add_geom(
        PointGeom::builder()
            .set("x", xs)
            .set("y", ys)
            .set("fill", fill_col)
            .set("size", size_col)
            .build(),
    );
    p.add_axis(Axis::rail("x", AxisPlacement::Cartesian(AxisSide::Bottom)));
    p.add_axis(Axis::rail("y", AxisPlacement::Cartesian(AxisSide::Left)));

    // ── Right side, legend #1: line + point, both driven by
    // category_color. First add_legend creates the legend; second
    // add_legend matches the (domain_scale, side, title) triple and
    // merges its key.
    p.add_legend(
        Legend::new("category_color")
            .side(LegendSide::Right)
            .title("Category")
            .key(LegendKeySpec::line().scaled("stroke", "category_color")),
    );
    p.add_legend(
        Legend::new("category_color")
            .side(LegendSide::Right)
            .title("Category")
            .key(
                LegendKeySpec::point()
                    .scaled("fill", "category_color")
                    .fixed("stroke", Value::Color(rgb(0.0, 0.0, 0.0)))
                    .fixed("size", 6.0_f64),
            ),
    );

    // ── Right side, legend #2 (stacks below #1): a size legend that
    // shares the Right slot. Different `domain_scale` triple so
    // `add_legend` keeps it separate, then the per-side stacker
    // arranges both legends vertically.
    p.add_legend(
        Legend::new("category_size")
            .side(LegendSide::Right)
            .title("Size")
            .key(
                LegendKeySpec::point()
                    .scaled("size", "category_size")
                    .fixed("fill", Value::Color(rgb(0.25, 0.25, 0.25))),
            ),
    );

    // ── Right side, legend #3: a continuous gradient colorbar
    // driven by `gradient_scale`. Stacks below the discrete ones
    // via the same per-side stacker. The colorbar also pulls an
    // alpha from `gradient_alpha` — same domain → low values are
    // mostly transparent, high values fully opaque.
    p.add_legend(
        Legend::colorbar("gradient_scale")
            .side(LegendSide::Right)
            .title("Gradient")
            .scaled("alpha", "gradient_alpha"),
    );

    // ── Bottom side, legend #3: same gradient scale but rendered
    // as discrete steps at each break — useful for showing a
    // binned colour mapping or any colorbar you want to read as
    // categorical bands. Routed to the Bottom slot so it stacks
    // horizontally alongside the existing "Pattern" + "Colour"
    // legends.
    p.add_legend(
        Legend::colorbar("gradient_scale")
            .side(LegendSide::Bottom)
            .title("Steps")
            .binned()
            .open_upper(),
    );

    // ── Bottom side, legend #4: a **binned stack** that varies
    // SHAPE per bin (instead of colour like the stepped colorbar
    // next to it). Six bin boundaries from `gradient_scale` → five
    // bins, each rendered as a different emoji-glyph shape via the
    // `bin_shape` scale (resolved against the plot's shape
    // registry). Demonstrates that the same legend key wiring
    // works with vector and glyph-backed shapes.
    //
    // Emoji glyphs fill their em-bbox almost completely (Latin
    // letters and vector circles only fill ~70–80 % of the same
    // bbox), so a 12pt emoji reads at roughly the same visual
    // weight as the 16pt circle markers used by the other legends.
    p.add_legend(
        Legend::new("gradient_scale")
            .side(LegendSide::Bottom)
            .title("Bins")
            .binned()
            .equal_bins()
            .key(
                LegendKeySpec::point()
                    .scaled("shape", "bin_shape")
                    .fixed("fill", Value::Color(rgb(0.15, 0.15, 0.15)))
                    .fixed("size", 12.0_f64),
            ),
    );

    // ── Bottom side: two legends side-by-side (horizontal stack).
    p.add_legend(
        Legend::new("category_line")
            .side(LegendSide::Bottom)
            .title("Pattern")
            .key(
                LegendKeySpec::line()
                    .scaled("linetype", "category_line")
                    .fixed("linewidth", 1.5_f64),
            ),
    );
    p.add_legend(
        Legend::new("category_color")
            .side(LegendSide::Bottom)
            .title("Colour")
            .key(LegendKeySpec::rect().scaled("fill", "category_color")),
    );

    // ── In-panel overlay: a compact category legend pinned to the
    // top-right corner of the panel area. Reserves no chrome space —
    // the data marks beneath continue to occupy the full panel rect.
    p.add_legend(
        Legend::new("category_color")
            .side(LegendSide::InPanel {
                anchor: Anchor::TopRight,
                inset_pt: 8.0,
            })
            .title("Overlay")
            .key(
                LegendKeySpec::point()
                    .scaled("fill", "category_color")
                    .fixed("size", 6.0_f64),
            ),
    );

    let cat_values: Vec<Value> = cats.iter().map(|s| Value::String(Arc::from(*s))).collect();
    let line_cats: [&'static str; 3] = ["Solid", "Dashed", "Dotted"];
    let line_values: Vec<Value> = line_cats
        .iter()
        .map(|s| Value::String(Arc::from(*s)))
        .collect();

    let mut view = PlotComposition::new(comp())
        .add_scale("x", scale::continuous(0.0..=10.0))
        .add_scale("y", scale::continuous(0.0..=1.0))
        .add_scale(
            "category_color",
            scale::discrete(cat_values.clone()).range_colors([
                rgb8(220, 90, 70),
                rgb8(70, 160, 90),
                rgb8(70, 120, 220),
                rgb8(180, 120, 200),
            ]),
        )
        .add_scale(
            "category_size",
            scale::discrete(cat_values).range_numbers([4.0, 8.0, 12.0, 16.0]),
        )
        .add_scale(
            "category_line",
            scale::discrete(line_values).range_linetypes([solid(), dashed(), dotted()]),
        )
        .add_scale(
            "gradient_scale",
            scale::continuous(0.0..=100.0).range_colors([
                rgb8(20, 30, 90),
                rgb8(60, 160, 200),
                rgb8(230, 220, 100),
                rgb8(220, 60, 40),
            ]),
        )
        .add_scale(
            "gradient_alpha",
            scale::continuous(0.0..=100.0).range_numbers([0.1, 1.0]),
        )
        .add_scale(
            "bin_shape",
            scale::continuous(0.0..=100.0).range_strings([
                Arc::from("droplet"),
                Arc::from("herb"),
                Arc::from("sun"),
                Arc::from("wave"),
                Arc::from("fire"),
            ]),
        );
    view.attach_plot(p);

    let issues = view.validate();
    if !issues.is_empty() {
        panic!("validate(): {issues:?}");
    }

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
        .join("examples/legends.png");
    hephaestus::png::write_png(&path, w, h, &pixels).expect("write png");
    println!("wrote {}", path.display());
}
