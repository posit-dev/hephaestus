//! Radar (chord-style polar) projection — the same four plot kinds as
//! `examples/polar.rs` (scatter, rose, gauge, partial arc) rendered
//! through chord-style polar projections, plus a fifth "polygon"
//! panel that demonstrates how lines spanning multiple category
//! boundaries bend at each crossed break. Categories live at evenly-
//! spaced positions around the swept arc; polygon-style grid rings
//! replace concentric circles, and polylines crossing category
//! boundaries bend at each break instead of curving along an arc.
//!
//! Each plot pairs a `scale::discrete([N category names])` for the
//! angle channel with `Projection::radar(N)` (or a hand-rolled
//! chord-style `PolarProjection` for the gauge / partial-arc
//! variants). `Projection::radar(N)` defaults its
//! `theta_break_fracs` to band centres `(i + 0.5) / N` — the same
//! positions a discrete scale's `map` returns — so polygon
//! corners, axis spokes, and data points all sit on the same
//! ring of angles.
//!
//! Produces `examples/radar.png`.

use std::sync::Arc;

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{beside, grid, Composition, Patch};
use hephaestus::geometry::Size;
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement, PolarRing};
use hephaestus::plot::projection::{PolarEdgeStyle, PolarProjection, Projection};
use hephaestus::plot::{scale, LineGeom, Plot, PlotComposition, PointGeom, RectGeom, SegmentGeom};
use hephaestus::scales::value::Value;
use hephaestus::Renderer;

fn comp_shape() -> Composition {
    let left = grid(
        4,
        1,
        vec![
            Patch::new("scatter").into(),
            Patch::new("rose").into(),
            Patch::new("gauge").into(),
            Patch::new("partial").into(),
        ],
    );
    beside(left, Patch::new("polygon"))
}

/// Half-disk radar gauge: chord-style, theta sweeps 180° → 0°
/// (left → top → right), with a 40 % centre hole. Five evenly-
/// spaced categories along the sweep at band centres.
fn radar_gauge_projection(n: usize) -> Projection {
    Projection::Polar(PolarProjection {
        theta_start: std::f64::consts::PI,
        theta_end: 0.0,
        inner_radius_frac: 0.4,
        edge_style: PolarEdgeStyle::Chord,
        theta_break_fracs: (0..n).map(|i| (i as f64 + 0.5) / n as f64).collect(),
        ..PolarProjection::full_circle()
    })
}

/// Non-axis-aligned partial radar — same -60° → 135° sweep as the
/// polar example's partial panel, but with chord-style edges and
/// band-centre break positions over `n` categories.
fn radar_partial_projection(n: usize) -> Projection {
    Projection::Polar(PolarProjection {
        theta_start: -std::f64::consts::PI / 3.0,
        theta_end: 3.0 * std::f64::consts::PI / 4.0,
        inner_radius_frac: 0.2,
        edge_style: PolarEdgeStyle::Chord,
        theta_break_fracs: (0..n).map(|i| (i as f64 + 0.5) / n as f64).collect(),
        ..PolarProjection::full_circle()
    })
}

fn cats(strings: &[&'static str]) -> Vec<Value> {
    strings
        .iter()
        .map(|s| Value::String(Arc::from(*s)))
        .collect()
}

fn main() {
    let (w, h) = (2000u32, 1000u32);
    let dpi = 96.0;

    // ── Scatter: 12 angular categories ("01" … "12", clock-like),
    // multiple radius samples per category, dots only. The grid is
    // a 12-sided polygon ring at each radius break.
    let scatter_cats: Vec<&'static str> = vec![
        "01", "02", "03", "04", "05", "06", "07", "08", "09", "10", "11", "12",
    ];
    let scatter_samples_per_cat = 5;
    let mut scatter_x: Vec<&'static str> = Vec::new();
    let mut scatter_y: Vec<f64> = Vec::new();
    for cat in &scatter_cats {
        for i in 0..scatter_samples_per_cat {
            scatter_x.push(*cat);
            // Spread radii — same shape as the polar scatter
            // (deterministic, derived from indices for reproducibility).
            let h = (cat.parse::<f64>().unwrap_or(0.0) * 0.731 + i as f64 * 0.273).sin();
            scatter_y.push(0.2 + 0.8 * (h * 0.5 + 0.5));
        }
    }
    let dot = rgb8(40, 100, 200);

    // ── Rose: 12 categories, one bar per category, with varying
    // outer radius. Each bar is a RectGeom band on the discrete
    // scale (`x_band` / `x2_band` carve out the wedge inside the
    // band). The chord-style edges between consecutive bars are
    // straight pixel-space chords — visually a star-shaped strip
    // around the centre.
    let rose_cats = scatter_cats.clone();
    let rose_n = rose_cats.len();
    let rose_x: Vec<&'static str> = rose_cats.clone();
    let rose_inner: Vec<f64> = (0..rose_n).map(|_| 0.1_f64).collect();
    let rose_outer: Vec<f64> = (0..rose_n)
        .map(|i| {
            let t = (i as f64 / rose_n as f64) * std::f64::consts::TAU;
            0.4 + 0.4 * (t * 2.0).cos().abs()
        })
        .collect();
    // Each bar occupies most of its band (0.8 wide, centred).
    let rose_x_band: Vec<f64> = (0..rose_n).map(|_| -0.4_f64).collect();
    let rose_x2_band: Vec<f64> = (0..rose_n).map(|_| 0.4_f64).collect();
    let bar_color = rgb8(180, 80, 100);

    // ── Gauge: half-disk with 5 labelled stops + a single needle
    // pointing at the "70 %" stop.
    let gauge_cats: Vec<&'static str> = vec!["0", "25", "50", "75", "100"];
    let gauge_n = gauge_cats.len();
    // Needle: at the "75" category (index 3), spanning full radius.
    let needle_cat = vec!["75"];
    let needle_inner = vec![0.0_f64];
    let needle_outer = vec![1.0_f64];
    let needle_color = rgb8(60, 30, 30);

    // ── Partial arc (-60° → 135°): 8 categories arranged along
    // the asymmetric sweep. Dots at each (category, radius)
    // combination — one point per category at a spiral-outward
    // radius.
    let partial_cats: Vec<&'static str> = vec!["a", "b", "c", "d", "e", "f", "g", "h"];
    let partial_n = partial_cats.len();
    let partial_x: Vec<&'static str> = partial_cats.clone();
    let partial_y: Vec<f64> = (0..partial_n)
        .map(|i| 0.3 + 0.6 * i as f64 / (partial_n - 1) as f64)
        .collect();
    let partial_dot = rgb8(120, 60, 160);

    // ── Polygon: demonstrates chord-style interpolation by visiting
    // *non-adjacent* categories. Each segment in this 12-category
    // radar spans 4 forward categories, so the chord-style
    // densification places one interior sample per crossed
    // category boundary (3 per segment). The line therefore
    // appears piecewise-linear with visible bends at every
    // intermediate category, with the bend radius linearly
    // interpolated between the segment endpoints.
    //
    // Markers at the data vertices (cats[0,3,6,9]) sit on top of
    // the bends-from-the-other-segments at the same theta — they
    // visually disambiguate "user-supplied data" from
    // "densification-introduced bend points".
    let polygon_cats = scatter_cats.clone();
    let polygon_x: Vec<&'static str> = vec!["01", "04", "07", "10"];
    let polygon_y: Vec<f64> = vec![0.85, 0.35, 0.75, 0.45];
    let polygon_color = rgb8(60, 130, 60);

    let mut view = PlotComposition::new(comp_shape())
        // Scatter: discrete categories for theta + continuous radius.
        .add_scale("scatter_cat", scale::discrete(cats(&scatter_cats)))
        .add_scale("radius_unit", scale::continuous(0.0..=1.0))
        // Rose: same scales as scatter for the angle, plus a 0..=1
        // outer-radius scale (matched to the data extents).
        .add_scale("rose_cat", scale::discrete(cats(&rose_cats)))
        // Gauge: 5-stop categorical sweep + a unit radius scale.
        .add_scale("gauge_cat", scale::discrete(cats(&gauge_cats)))
        .add_scale("gauge_radius", scale::continuous(0.0..=1.0))
        // Partial: 8 categories + unit radius.
        .add_scale("partial_cat", scale::discrete(cats(&partial_cats)))
        .add_scale("partial_radius", scale::continuous(0.0..=1.0))
        // Polygon: reuses the 12-category clock-like scale + unit radius.
        .add_scale("polygon_cat", scale::discrete(cats(&polygon_cats)));

    // ── Scatter plot (radar) ──
    let mut p_scatter = Plot::new(&comp_shape(), "scatter")
        .projection(Projection::radar(scatter_cats.len()))
        .bind("x", "scatter_cat")
        .bind("y", "radius_unit");
    p_scatter.add_geom(
        PointGeom::builder()
            .set("x", scatter_x)
            .set("y", scatter_y)
            .set("fill", dot)
            .set("size", 6.0_f64)
            .build(),
    );
    p_scatter.add_axis(Axis::rail(
        "scatter_cat",
        AxisPlacement::PolarAngular(PolarRing::Outer),
    ));
    p_scatter.add_axis(Axis::rail(
        "radius_unit",
        AxisPlacement::PolarRadius { theta_frac: 0.0 },
    ));
    view.attach_plot(p_scatter);

    // ── Rose (radar) ──
    let mut p_rose = Plot::new(&comp_shape(), "rose")
        .projection(Projection::radar(rose_n))
        .bind("x", "rose_cat")
        .bind("x2", "rose_cat")
        .bind("y", "radius_unit")
        .bind("y2", "radius_unit");
    p_rose.add_geom(
        RectGeom::builder()
            .set("x", rose_x.clone())
            .set("x2", rose_x)
            .set("x_band", rose_x_band)
            .set("x2_band", rose_x2_band)
            .set("y", rose_inner)
            .set("y2", rose_outer)
            .set("fill", bar_color)
            .build(),
    );
    p_rose.add_axis(Axis::rail(
        "rose_cat",
        AxisPlacement::PolarAngular(PolarRing::Outer),
    ));
    p_rose.add_axis(Axis::rail(
        "radius_unit",
        AxisPlacement::PolarRadius { theta_frac: 0.0 },
    ));
    view.attach_plot(p_rose);

    // ── Gauge (radar) ──
    let mut p_gauge = Plot::new(&comp_shape(), "gauge")
        .projection(radar_gauge_projection(gauge_n))
        .bind("x", "gauge_cat")
        .bind("x2", "gauge_cat")
        .bind("y", "gauge_radius")
        .bind("y2", "gauge_radius");
    p_gauge.add_geom(
        SegmentGeom::builder()
            .set("x", needle_cat.clone())
            .set("x2", needle_cat)
            .set("y", needle_inner)
            .set("y2", needle_outer)
            .set("stroke", needle_color)
            .set("linewidth", 4.0_f64)
            .build(),
    );
    p_gauge.add_axis(Axis::rail(
        "gauge_cat",
        AxisPlacement::PolarAngular(PolarRing::Outer),
    ));
    p_gauge.add_axis(Axis::rail(
        "gauge_cat",
        AxisPlacement::PolarAngular(PolarRing::Inner),
    ));
    p_gauge.add_axis(Axis::rail(
        "gauge_radius",
        AxisPlacement::PolarRadius { theta_frac: 0.0 },
    ));
    view.attach_plot(p_gauge);

    // ── Partial radar arc (-60° → 135°) ──
    let mut p_partial = Plot::new(&comp_shape(), "partial")
        .projection(radar_partial_projection(partial_n))
        .bind("x", "partial_cat")
        .bind("y", "partial_radius");
    p_partial.add_geom(
        PointGeom::builder()
            .set("x", partial_x)
            .set("y", partial_y)
            .set("fill", partial_dot)
            .set("size", 7.0_f64)
            .build(),
    );
    p_partial.add_axis(Axis::rail(
        "partial_cat",
        AxisPlacement::PolarAngular(PolarRing::Outer),
    ));
    p_partial.add_axis(Axis::rail(
        "partial_radius",
        AxisPlacement::PolarRadius { theta_frac: 0.0 },
    ));
    view.attach_plot(p_partial);

    // ── Polygon (radar): non-adjacent jumps to show interpolation ──
    let mut p_polygon = Plot::new(&comp_shape(), "polygon")
        .projection(Projection::radar(polygon_cats.len()))
        .bind("x", "polygon_cat")
        .bind("y", "radius_unit");
    let polygon_key: Vec<&'static str> = vec!["series"; polygon_x.len()];
    p_polygon.add_geom(
        LineGeom::builder()
            .keys(polygon_key)
            .set("x", polygon_x.clone())
            .set("y", polygon_y.clone())
            .set("stroke", polygon_color)
            .set("linewidth", 2.5_f64)
            .build(),
    );
    p_polygon.add_geom(
        PointGeom::builder()
            .set("x", polygon_x)
            .set("y", polygon_y)
            .set("fill", polygon_color)
            .set("size", 8.0_f64)
            .build(),
    );
    p_polygon.add_axis(Axis::rail(
        "polygon_cat",
        AxisPlacement::PolarAngular(PolarRing::Outer),
    ));
    p_polygon.add_axis(Axis::rail(
        "radius_unit",
        AxisPlacement::PolarRadius { theta_frac: 0.0 },
    ));
    view.attach_plot(p_polygon);

    let issues = view.validate();
    if !issues.is_empty() {
        panic!("validate() reported issues: {issues:?}");
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
    let path = std::env::current_dir().unwrap().join("examples/radar.png");
    hephaestus::png::write_png(&path, w, h, &pixels).expect("write png");
    println!("wrote {}", path.display());
}
