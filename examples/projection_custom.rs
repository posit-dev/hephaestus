//! End-to-end visual sanity for `Projection::Custom`. Three renders:
//!
//! - `projection_custom_1.png` — an irregular hexagon-shaped drawing
//!   surface, a graticule grid of four x-major + four y-major + thin
//!   minor lines, and a `GeometryGeom` polygon overlaid on top.
//!   Confirms that the outline shapes the panel, the graticules end
//!   cleanly at the boundary (clipper2 pre-clip), and the geom is
//!   clipped to the same outline via `plot.clip(true)`.
//! - `projection_custom_2.png` — same plot with the x-scale narrowed,
//!   so the outline trims against the visible panel rect at draw
//!   time. The hexagon's right edge gets sliced off; graticules and
//!   geom both respect the trimmed boundary.
//! - `projection_custom_3.png` — outline polygon with a square hole
//!   in the middle. Demonstrates the EvenOdd handling: the hole is a
//!   non-drawing region; both the panel background fill and the geom
//!   clip respect it.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{Composition, Patch, Span};
use hephaestus::geometry::Size;
use hephaestus::plot::{scale, CustomProjection, GeometryGeom, Plot, PlotComposition, Projection};
use hephaestus::scales::geometry::{Geometry, Polygon as GeoPolygon};
use hephaestus::Renderer;

fn main() {
    let (w, h) = (800u32, 700u32);
    let dpi = 96.0;
    let comp = || Composition::empty(1, 1).place(1, 1, Span::cell(), Patch::new("panel"));

    let mut renderer = VelloRenderer::new().expect("vello renderer init");
    let bg: Color = rgb8(248, 248, 252);

    // Hexagonal drawing surface — six vertices in data space.
    let hex = GeoPolygon::new(vec![
        (3.0, 0.5),
        (7.0, 0.5),
        (9.0, 4.0),
        (7.0, 7.5),
        (3.0, 7.5),
        (1.0, 4.0),
        (3.0, 0.5),
    ]);
    // Four major vertical (x) graticule lines and four major horizontal
    // (y) ones at the visible integer positions inside the hexagon, plus
    // four denser minor lines per axis.
    let x_major: Vec<Vec<(f64, f64)>> = (2..=8)
        .step_by(2)
        .map(|x| vec![(x as f64, 0.0), (x as f64, 8.0)])
        .collect();
    let y_major: Vec<Vec<(f64, f64)>> = (1..=7)
        .step_by(2)
        .map(|y| vec![(0.0, y as f64), (10.0, y as f64)])
        .collect();
    let x_minor: Vec<Vec<(f64, f64)>> = (1..=9)
        .step_by(2)
        .map(|x| vec![(x as f64, 0.0), (x as f64, 8.0)])
        .collect();
    let y_minor: Vec<Vec<(f64, f64)>> = (0..=8)
        .step_by(2)
        .map(|y| vec![(0.0, y as f64), (10.0, y as f64)])
        .collect();

    // A diamond-shaped GeometryGeom polygon that pokes outside the
    // hexagon — the clip layer should trim it to the hexagon.
    let diamond = Geometry::Polygon(GeoPolygon::new(vec![
        (5.0, 0.0),
        (10.0, 4.0),
        (5.0, 8.0),
        (0.0, 4.0),
        (5.0, 0.0),
    ]));

    // ── Render 1: full extent. ───────────────────────────────────────
    render_scene(
        &mut renderer,
        w,
        h,
        dpi,
        bg,
        "examples/projection_custom_1.png",
        build_view(
            &comp,
            CustomProjection::new(hex.clone())
                .x_major(x_major.clone())
                .x_minor(x_minor.clone())
                .y_major(y_major.clone())
                .y_minor(y_minor.clone()),
            (0.0, 10.0),
            (0.0, 8.0),
            diamond.clone(),
        ),
    );

    // ── Render 2: x-domain narrowed (zoomed in past the right side
    //     of the hexagon). ─────────────────────────────────────────────
    render_scene(
        &mut renderer,
        w,
        h,
        dpi,
        bg,
        "examples/projection_custom_2.png",
        build_view(
            &comp,
            CustomProjection::new(hex.clone())
                .x_major(x_major.clone())
                .x_minor(x_minor.clone())
                .y_major(y_major.clone())
                .y_minor(y_minor.clone()),
            (1.5, 6.5),
            (0.0, 8.0),
            diamond.clone(),
        ),
    );

    // ── Render 3: outline with a hole (EvenOdd). ─────────────────────
    let hex_with_hole = hex.clone().with_hole(vec![
        (4.0, 3.0),
        (6.0, 3.0),
        (6.0, 5.0),
        (4.0, 5.0),
        (4.0, 3.0),
    ]);
    render_scene(
        &mut renderer,
        w,
        h,
        dpi,
        bg,
        "examples/projection_custom_3.png",
        build_view(
            &comp,
            CustomProjection::new(hex_with_hole)
                .x_major(x_major)
                .x_minor(x_minor)
                .y_major(y_major)
                .y_minor(y_minor),
            (0.0, 10.0),
            (0.0, 8.0),
            diamond,
        ),
    );
}

fn build_view<F>(
    comp: &F,
    proj: CustomProjection,
    x_domain: (f64, f64),
    y_domain: (f64, f64),
    geom: Geometry,
) -> PlotComposition
where
    F: Fn() -> Composition,
{
    let mut plot = Plot::new(&comp(), "panel")
        .bind("x", "x_axis")
        .bind("y", "y_axis")
        .projection(Projection::Custom(proj))
        .clip(true);

    plot.add_geom(
        GeometryGeom::builder()
            .set("geometry", vec![geom])
            .set("fill", rgb8(60, 130, 200))
            .set("stroke", rgb8(20, 50, 90))
            .set("linewidth", 1.5_f64)
            .build(),
    );
    // Custom projections use `ChromeStrategy::InsidePanel`; perimeter
    // axes (Cartesian-style) are rejected by `Plot::add_axis` for this
    // projection, so we don't attach any.
    PlotComposition::new(&comp())
        .add_scale("x_axis", scale::continuous(x_domain.0..=x_domain.1))
        .add_scale("y_axis", scale::continuous(y_domain.0..=y_domain.1))
        .with_plot(plot)
}

fn render_scene(
    renderer: &mut VelloRenderer,
    w: u32,
    h: u32,
    dpi: f64,
    bg: Color,
    out_relative: &str,
    mut view: PlotComposition,
) {
    {
        let scene = renderer.scene();
        scene.clear();
        view.render(scene, Size::new(w as f64, h as f64), dpi);
    }
    let mut pixels = vec![0u8; (w * h * 4) as usize];
    renderer
        .render_to_buffer(w, h, bg, &mut pixels)
        .expect("render");
    let path = std::env::current_dir().unwrap().join(out_relative);
    hephaestus::png::write_png(&path, w, h, &pixels).expect("write png");
    println!("wrote {}", path.display());
}
