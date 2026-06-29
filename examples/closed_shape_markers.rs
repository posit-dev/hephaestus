//! Linetype markers on closed-shape geoms: polygon, rect, ellipse, wedge.
//!
//! Closed shapes use `TrailingPolicy::Distribute` internally — gap
//! lengths are scaled so an integer number of pattern repeats fits the
//! perimeter exactly, eliminating the seam at the closing edge.
//!
//! Markers stamp the named shape around the closed boundary and use
//! the resolved **stroke colour** as their fill (the geom's `"fill"`
//! channel still fills the shape interior, not the markers).
//!
//! One render: 2×2 grid of closed shapes (rect / ellipse / wedge /
//! polygon) each outlined with circle markers around the perimeter.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{Composition, Patch, Span};
use hephaestus::geometry::Size;
#[cfg(feature = "text")]
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement};
use hephaestus::plot::{
    linetype, scale, EllipseGeom, Plot, PlotComposition, PolygonGeom, Raw, RectGeom, Value,
    WedgeGeom,
};
#[cfg(feature = "text")]
use hephaestus::scales::chrome::AxisSide;
use hephaestus::Renderer;

fn main() {
    let (w, h) = (900u32, 700u32);
    let dpi = 96.0;

    // 2×2 grid of cells.
    let comp = || {
        Composition::empty(2, 2)
            .place(1, 1, Span::cell(), Patch::new("rect"))
            .place(1, 2, Span::cell(), Patch::new("ellipse"))
            .place(2, 1, Span::cell(), Patch::new("wedge"))
            .place(2, 2, Span::cell(), Patch::new("polygon"))
    };

    let stroke_col = rgb8(40, 90, 180);
    let fill_col: Color = rgb8(220, 230, 245);
    let pat = linetype::pattern([linetype::marker("circle"), linetype::gap(8.0)]);
    let linewidth = 10.0_f64;

    // Rect: square centred in its cell.
    let mut rect_plot = Plot::new(&comp(), "rect")
        .bind("x", "xs")
        .bind("y", "ys")
        .bind("x2", "xs")
        .bind("y2", "ys");
    rect_plot.add_geom(
        RectGeom::builder()
            .set("x", Raw(vec![0.2_f64]))
            .set("y", Raw(vec![0.2_f64]))
            .set("x2", Raw(vec![0.8_f64]))
            .set("y2", Raw(vec![0.8_f64]))
            .set("fill", fill_col)
            .set("stroke", stroke_col)
            .set("linewidth", linewidth)
            .set("linetype", Value::Linetype(pat.clone()))
            .build(),
    );
    #[cfg(feature = "text")]
    {
        rect_plot.add_axis(Axis::rail("xs", AxisPlacement::Cartesian(AxisSide::Bottom)));
        rect_plot.add_axis(Axis::rail("ys", AxisPlacement::Cartesian(AxisSide::Left)));
    }

    // Ellipse: oval centred in its cell.
    let mut ellipse_plot = Plot::new(&comp(), "ellipse")
        .bind("x", "xs")
        .bind("y", "ys")
        .bind("x2", "xs")
        .bind("y2", "ys");
    ellipse_plot.add_geom(
        EllipseGeom::builder()
            .set("x", Raw(vec![0.5_f64]))
            .set("y", Raw(vec![0.5_f64]))
            .set("x2", Raw(vec![0.9_f64]))
            .set("y2", Raw(vec![0.85_f64]))
            .set("fill", fill_col)
            .set("stroke", stroke_col)
            .set("linewidth", linewidth)
            .set("linetype", Value::Linetype(pat.clone()))
            .build(),
    );
    #[cfg(feature = "text")]
    {
        ellipse_plot.add_axis(Axis::rail("xs", AxisPlacement::Cartesian(AxisSide::Bottom)));
        ellipse_plot.add_axis(Axis::rail("ys", AxisPlacement::Cartesian(AxisSide::Left)));
    }

    // Wedge: pie slice (3/4 of a circle), centre at cell middle.
    let mut wedge_plot = Plot::new(&comp(), "wedge").bind("x", "xs").bind("y", "ys");
    wedge_plot.add_geom(
        WedgeGeom::builder()
            .set("x", Raw(vec![0.5_f64]))
            .set("y", Raw(vec![0.5_f64]))
            .set("radius", 80.0_f64)
            .set("theta", 0.4_f64)
            .set("theta2", std::f64::consts::TAU * 0.85)
            .set("fill", fill_col)
            .set("stroke", stroke_col)
            .set("linewidth", linewidth)
            .set("linetype", Value::Linetype(pat.clone()))
            .build(),
    );
    #[cfg(feature = "text")]
    {
        wedge_plot.add_axis(Axis::rail("xs", AxisPlacement::Cartesian(AxisSide::Bottom)));
        wedge_plot.add_axis(Axis::rail("ys", AxisPlacement::Cartesian(AxisSide::Left)));
    }

    // Polygon: irregular hexagon-ish in panel-fraction coords.
    let xs: Vec<f64> = vec![0.5, 0.85, 0.85, 0.5, 0.15, 0.15];
    let ys: Vec<f64> = vec![0.15, 0.35, 0.7, 0.85, 0.7, 0.35];
    let mut poly_plot = Plot::new(&comp(), "polygon")
        .bind("x", "xs")
        .bind("y", "ys");
    poly_plot.add_geom(
        PolygonGeom::builder()
            .set("x", Raw(xs))
            .set("y", Raw(ys))
            .set("fill", fill_col)
            .set("stroke", stroke_col)
            .set("linewidth", linewidth)
            .set("linetype", Value::Linetype(pat))
            .build(),
    );
    #[cfg(feature = "text")]
    {
        poly_plot.add_axis(Axis::rail("xs", AxisPlacement::Cartesian(AxisSide::Bottom)));
        poly_plot.add_axis(Axis::rail("ys", AxisPlacement::Cartesian(AxisSide::Left)));
    }

    let mut view = PlotComposition::new(&comp())
        .add_scale("xs", scale::continuous(0.0..=1.0))
        .add_scale("ys", scale::continuous(0.0..=1.0))
        .with_plot(rect_plot)
        .with_plot(ellipse_plot)
        .with_plot(wedge_plot)
        .with_plot(poly_plot);

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
        .join("examples/closed_shape_markers.png");
    hephaestus::png::write_png(&path, w, h, &pixels).expect("write png");
    println!("wrote {}", path.display());
}
