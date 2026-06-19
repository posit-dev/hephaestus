//! Integration smoke test for `Projection::Custom`. Renders a plot with
//! an irregular outline + graticules + a clipped geom through the full
//! `PlotComposition` pipeline and confirms the pixel buffer is non-empty.
//! The detailed visual checks live in `examples/projection_custom.rs`;
//! this test only asserts that the integration points line up at all.

#![cfg(all(feature = "vello", feature = "text"))]

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{Composition, Patch, Span};
use hephaestus::geometry::Size;
use hephaestus::plot::{scale, CustomProjection, GeometryGeom, Plot, PlotComposition, Projection};
use hephaestus::scales::geometry::{Geometry, Polygon as GeoPolygon};
use hephaestus::Renderer;

#[test]
fn custom_projection_renders_outline_graticules_and_geom() {
    let (w, h) = (400u32, 400u32);
    let dpi = 96.0;
    let comp = || Composition::empty(1, 1).place(1, 1, Span::cell(), Patch::new("panel"));

    // Pentagon outline + a couple of graticules in data space.
    let outline = GeoPolygon::new(vec![
        (5.0, 1.0),
        (9.0, 4.0),
        (7.5, 8.5),
        (2.5, 8.5),
        (1.0, 4.0),
        (5.0, 1.0),
    ]);
    let proj = CustomProjection::new(outline)
        .x_major(vec![vec![(5.0, 0.0), (5.0, 10.0)]])
        .y_major(vec![vec![(0.0, 4.5), (10.0, 4.5)]]);

    let mut plot = Plot::new(&comp(), "panel")
        .bind("x", "x_axis")
        .bind("y", "y_axis")
        .projection(Projection::Custom(proj))
        .clip(true);
    plot.add_geom(
        GeometryGeom::builder()
            .set(
                "geometry",
                vec![Geometry::Polygon(GeoPolygon::new(vec![
                    (3.0, 3.0),
                    (8.0, 3.0),
                    (8.0, 7.0),
                    (3.0, 7.0),
                    (3.0, 3.0),
                ]))],
            )
            .set("fill", rgb8(100, 150, 200))
            .set("stroke", rgb8(20, 40, 80))
            .set("linewidth", 1.0_f64)
            .build(),
    );

    let mut view = PlotComposition::new(comp())
        .add_scale("x_axis", scale::continuous(0.0..=10.0))
        .add_scale("y_axis", scale::continuous(0.0..=10.0))
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
    let bg_rgb = [
        (bg.components[0] * 255.0) as u8,
        (bg.components[1] * 255.0) as u8,
        (bg.components[2] * 255.0) as u8,
    ];
    let any_non_bg = pixels.chunks_exact(4).any(|px| px[..3] != bg_rgb);
    assert!(
        any_non_bg,
        "Custom projection rendered an entirely background-coloured frame"
    );
}

#[test]
fn zoomed_in_scale_trims_outline_against_panel_rect() {
    use hephaestus::plot::projection::CustomProjection;
    use hephaestus::plot::scale::Scale;
    use hephaestus::scales::ScaleTypeKind;

    let outline = GeoPolygon::new(vec![(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)]);
    let proj = CustomProjection::new(outline);
    // x-scale narrowed to [3, 7]: the outline's [0, 10] data span should
    // trim to fractions covering only the [0, 1] visible range, which is
    // a smaller rect than the original.
    let x_scale = Scale::new(ScaleTypeKind::Continuous).domain_continuous(3.0, 7.0);
    let y_scale = Scale::new(ScaleTypeKind::Continuous).domain_continuous(0.0, 10.0);
    let rings = proj.resolved_outline_fracs(Some(&x_scale), Some(&y_scale));
    assert_eq!(rings.len(), 1, "single ring expected after trim");
    let xs: Vec<f64> = rings[0].iter().map(|(x, _)| *x).collect();
    let x_min = xs.iter().cloned().fold(f64::INFINITY, f64::min);
    let x_max = xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    assert!(
        (x_min - 0.0).abs() < 1e-6 && (x_max - 1.0).abs() < 1e-6,
        "trimmed x range expected in [0, 1], got [{x_min}, {x_max}]"
    );
}
