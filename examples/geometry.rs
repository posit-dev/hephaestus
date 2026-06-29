//! End-to-end visual sanity for `GeometryGeom`. One render
//! (`geometry_1_mixed.png`) shows a column with mixed feature types — a
//! polygon (with hole), a multi-polygon, a line string, and a multi-point
//! — all rendered as one geom call with one fill / stroke per row.

use std::sync::Arc;

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{Composition, Patch, Span};
use hephaestus::geometry::Size;
#[cfg(feature = "text")]
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement};
use hephaestus::plot::value::Value;
use hephaestus::plot::{scale, GeometryGeom, Plot, PlotComposition};
#[cfg(feature = "text")]
use hephaestus::scales::chrome::AxisSide;
use hephaestus::scales::geometry::{Geometry, Polygon as GeoPolygon};
use hephaestus::Renderer;

fn main() {
    let (w, h) = (900u32, 700u32);
    let dpi = 96.0;

    let comp = || Composition::empty(1, 1).place(1, 1, Span::cell(), Patch::new("panel"));

    let mut renderer = VelloRenderer::new().expect("vello renderer init");
    let bg: Color = rgb8(248, 248, 252);

    // Four features sharing one column. The `kind` column drives the
    // fill scale so each row gets a distinct colour.
    let geometries = vec![
        // 0: a polygon with a hole
        Geometry::Polygon(
            GeoPolygon::new(vec![
                (1.0, 1.0),
                (4.0, 1.0),
                (4.0, 4.0),
                (1.0, 4.0),
                (1.0, 1.0),
            ])
            .with_hole(vec![
                (2.0, 2.0),
                (3.0, 2.0),
                (3.0, 3.0),
                (2.0, 3.0),
                (2.0, 2.0),
            ]),
        ),
        // 1: a multi-polygon (two triangles)
        Geometry::MultiPolygon(vec![
            GeoPolygon::new(vec![(5.0, 1.0), (7.0, 1.0), (6.0, 3.0), (5.0, 1.0)]),
            GeoPolygon::new(vec![(7.5, 1.5), (9.0, 1.5), (8.25, 3.5), (7.5, 1.5)]),
        ]),
        // 2: a line string winding across the plot
        Geometry::LineString(vec![
            (1.0, 5.5),
            (2.5, 6.5),
            (4.0, 5.5),
            (5.5, 6.5),
            (7.0, 5.5),
            (8.5, 6.5),
        ]),
        // 3: a multi-point cluster
        Geometry::MultiPoint(vec![
            (2.0, 8.5),
            (3.0, 8.5),
            (4.0, 8.5),
            (5.0, 8.5),
            (6.0, 8.5),
            (7.0, 8.5),
        ]),
    ];
    let kinds: Vec<&str> = vec!["polygon", "multipolygon", "linestring", "multipoint"];

    let polygon_fill: Color = rgb8(120, 170, 220);
    let multipoly_fill: Color = rgb8(220, 150, 110);
    let line_color: Color = rgb8(60, 110, 180);
    let point_fill: Color = rgb8(160, 80, 130);

    let mut plot = Plot::new(&comp(), "panel")
        .bind("x", "x_axis")
        .bind("y", "y_axis")
        .bind("fill", "kind_fill")
        .bind("stroke", "kind_stroke");
    plot.add_geom(
        GeometryGeom::builder()
            .set("geometry", geometries)
            .set("fill", kinds.clone())
            .set("stroke", kinds.clone())
            .set("linewidth", 1.6_f64)
            .set("size", 7.0_f64)
            .build(),
    );
    #[cfg(feature = "text")]
    {
        plot.add_axis(Axis::rail(
            "x_axis",
            AxisPlacement::Cartesian(AxisSide::Bottom),
        ));
        plot.add_axis(Axis::rail(
            "y_axis",
            AxisPlacement::Cartesian(AxisSide::Left),
        ));
    }

    let mut view = PlotComposition::new(&comp())
        .add_scale("x_axis", scale::continuous(0.0..=10.0))
        .add_scale("y_axis", scale::continuous(0.0..=10.0))
        .add_scale(
            "kind_fill",
            scale::ordinal(
                ["polygon", "multipolygon", "linestring", "multipoint"]
                    .into_iter()
                    .map(|s| Value::String(Arc::from(s))),
            )
            .range_colors([polygon_fill, multipoly_fill, line_color, point_fill]),
        )
        .add_scale(
            "kind_stroke",
            scale::ordinal(
                ["polygon", "multipolygon", "linestring", "multipoint"]
                    .into_iter()
                    .map(|s| Value::String(Arc::from(s))),
            )
            .range_colors([
                rgb8(40, 60, 90),
                rgb8(90, 60, 40),
                line_color,
                rgb8(80, 40, 60),
            ]),
        )
        .with_plot(plot);

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
        .join("examples/geometry_1_mixed.png");
    hephaestus::png::write_png(&path, w, h, &pixels).expect("write png");
    println!("wrote {}", path.display());
}
