//! Integration smoke test for `GeometryGeom`: render a column carrying
//! mixed feature types end-to-end through the full `PlotComposition`
//! pipeline and confirm the resulting pixel buffer is non-empty.

#![cfg(feature = "vello")]

use std::sync::Arc;

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{Composition, Patch, Span};
use hephaestus::geometry::Size;
use hephaestus::plot::value::Value;
use hephaestus::plot::{scale, GeometryGeom, Plot, PlotComposition};
use hephaestus::scales::geometry::{Geometry, Polygon as GeoPolygon};
use hephaestus::Renderer;

#[test]
fn mixed_geometry_column_renders() {
    let (w, h) = (400u32, 400u32);
    let dpi = 96.0;
    let comp = || Composition::empty(1, 1).place(1, 1, Span::cell(), Patch::new("panel"));

    let geometries = vec![
        Geometry::Polygon(
            GeoPolygon::new(vec![
                (1.0, 1.0),
                (3.0, 1.0),
                (3.0, 3.0),
                (1.0, 3.0),
                (1.0, 1.0),
            ])
            .with_hole(vec![
                (1.5, 1.5),
                (2.5, 1.5),
                (2.5, 2.5),
                (1.5, 2.5),
                (1.5, 1.5),
            ]),
        ),
        Geometry::LineString(vec![(0.5, 5.0), (4.5, 7.5)]),
        Geometry::MultiPoint(vec![(6.0, 6.0), (7.0, 7.0)]),
    ];
    let kinds: Vec<&str> = vec!["polygon", "line", "point"];

    let mut plot = Plot::new(&comp(), "panel")
        .bind("x", "x_axis")
        .bind("y", "y_axis")
        .bind("fill", "kind_fill")
        .bind("stroke", "kind_fill");
    plot.add_geom(
        GeometryGeom::builder()
            .set("geometry", geometries)
            .set("fill", kinds.clone())
            .set("stroke", kinds)
            .set("linewidth", 1.5_f64)
            .set("size", 6.0_f64)
            .build(),
    );

    let mut view = PlotComposition::new(&comp())
        .add_scale("x_axis", scale::continuous(0.0..=10.0))
        .add_scale("y_axis", scale::continuous(0.0..=10.0))
        .add_scale(
            "kind_fill",
            scale::ordinal(
                ["polygon", "line", "point"]
                    .into_iter()
                    .map(|s| Value::String(Arc::from(s))),
            )
            .range_colors([rgb8(80, 140, 200), rgb8(200, 100, 80), rgb8(120, 80, 160)]),
        )
        .with_plot(plot);

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

    // Any non-background pixel proves the geom emitted at least one draw
    // call that survived rasterisation. We don't pixel-compare against a
    // golden — that's the example's job; this only certifies the geom
    // wires through PlotComposition end-to-end.
    let bg_rgb = [
        (bg.components[0] * 255.0) as u8,
        (bg.components[1] * 255.0) as u8,
        (bg.components[2] * 255.0) as u8,
    ];
    let any_non_bg = pixels.chunks_exact(4).any(|px| px[..3] != bg_rgb);
    assert!(
        any_non_bg,
        "every pixel matched the background — geom drew nothing"
    );
}
