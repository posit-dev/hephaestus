//! Integration smoke test for `ImageGeom`: render named raster images
//! end-to-end through the full `PlotComposition` pipeline, in both sizing
//! modes, and confirm the resulting pixel buffer carries them.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::brush::Image;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{Composition, Patch, Span};
use hephaestus::geometry::Size;
use hephaestus::plot::{scale, ImageGeom, ImageRegistry, Plot, PlotComposition};
use hephaestus::scene::SceneBuilder;
use hephaestus::Renderer;

/// A solid opaque image of the given size and colour.
fn solid(width: u32, height: u32, rgb: [u8; 3]) -> Image {
    let mut px = Vec::with_capacity((width as usize) * (height as usize) * 4);
    for _ in 0..(width as usize) * (height as usize) {
        px.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
    }
    hephaestus::image::from_rgba8(width, height, px).expect("valid buffer")
}

fn panel() -> Composition {
    Composition::empty(1, 1).place(1, 1, Span::cell(), Patch::new("panel"))
}

/// The fraction of pixels carrying the red test image. Counting the
/// image's own colour rather than "anything but the background" is what
/// keeps the theme's panel fill and chrome out of the measurement.
fn red_fraction(pixels: &[u8]) -> f64 {
    let total = pixels.len() / 4;
    let hits = pixels
        .chunks_exact(4)
        .filter(|px| px[0] > 180 && px[1] < 120 && px[2] < 120)
        .count();
    hits as f64 / total as f64
}

/// Render one plot and hand back its pixels.
fn render(plot: Plot, w: u32, h: u32, bg: Color) -> Vec<u8> {
    let mut view = PlotComposition::new(&panel())
        .add_scale("x_axis", scale::continuous(0.0..=10.0))
        .add_scale("y_axis", scale::continuous(0.0..=10.0))
        .with_plot(plot);

    let mut renderer = VelloRenderer::new().expect("vello renderer init");
    {
        let scene = renderer.scene();
        scene.clear();
        view.render(scene, Size::new(w as f64, h as f64), 96.0);
    }
    let mut pixels = vec![0u8; (w * h * 4) as usize];
    renderer
        .render_to_buffer(w, h, bg, &mut pixels)
        .expect("render");
    pixels
}

fn registry() -> ImageRegistry {
    let mut r = ImageRegistry::new();
    r.insert("red", solid(8, 8, [220, 60, 60]));
    r.insert("blue", solid(8, 8, [60, 90, 220]));
    r
}

/// Anchored mode: an image per data point at an absolute pt size. Two
/// registry names bound per row, so both entries have to reach the scene.
#[test]
fn anchored_images_render_at_their_data_points() {
    let bg: Color = rgb8(248, 248, 252);
    let mut plot = Plot::new(&panel(), "panel")
        .bind("x", "x_axis")
        .bind("y", "y_axis")
        .image_registry(registry());
    plot.add_geom(
        ImageGeom::builder()
            .set("image", vec!["red", "blue"])
            .set("x", vec![2.5_f64, 7.5])
            .set("y", vec![5.0_f64, 5.0])
            .set("width", 40.0_f64)
            .set("height", 40.0_f64)
            .build(),
    );

    let pixels = render(plot, 400, 400, bg);

    // Both registry entries have to reach the scene, so both colours are
    // on screen. A 40 pt square at 96 dpi is ~53 px, so each covers a few
    // thousand of the 160 000 pixels.
    let red = red_fraction(&pixels);
    let blue = pixels
        .chunks_exact(4)
        .filter(|px| px[2] > 180 && px[0] < 120)
        .count();
    assert!(
        red > 0.005,
        "the \"red\" registry entry covered only {red} of the frame"
    );
    assert!(
        blue > 800,
        "the \"blue\" registry entry covered only {blue} pixels"
    );
}

/// Data-space mode: an image spanning a rect in data units grows with the
/// panel, where an anchored one would not.
#[test]
fn a_data_space_image_scales_with_the_panel() {
    let bg: Color = rgb8(248, 248, 252);
    let build = || {
        let mut plot = Plot::new(&panel(), "panel")
            .bind("x", "x_axis")
            .bind("y", "y_axis")
            .image_registry(registry());
        plot.add_geom(
            ImageGeom::builder()
                .set("image", "red")
                .set("x", vec![2.0_f64])
                .set("y", vec![2.0_f64])
                .set("x2", vec![8.0_f64])
                .set("y2", vec![8.0_f64])
                .build(),
        );
        plot
    };

    let small = red_fraction(&render(build(), 200, 200, bg));
    let large = red_fraction(&render(build(), 400, 400, bg));

    assert!(small > 0.0, "the small render drew nothing");
    // Chrome takes a size-dependent share of the frame, so the two
    // fractions are not equal — but a data-space image tracks its panel
    // rather than shrinking to a fixed pixel count, which an anchored one
    // in the same spot would.
    assert!(
        (large - small).abs() < 0.15,
        "coverage moved from {small} to {large}; a data-space image should stay proportional"
    );
}

/// A name the registry doesn't hold is silently skipped rather than
/// panicking or drawing a placeholder — the row simply has no image.
#[test]
fn an_unregistered_name_renders_nothing() {
    let bg: Color = rgb8(248, 248, 252);
    let mut plot = Plot::new(&panel(), "panel")
        .bind("x", "x_axis")
        .bind("y", "y_axis")
        .image_registry(ImageRegistry::new());
    plot.add_geom(
        ImageGeom::builder()
            .set("image", "red")
            .set("x", vec![5.0_f64])
            .set("y", vec![5.0_f64])
            .set("width", 40.0_f64)
            .build(),
    );

    let with_geom = render(plot, 200, 200, bg);

    // The comparison is against the same plot with no geom at all, so the
    // theme's own panel fill and chrome cancel out and what is left is
    // whether the geom contributed anything.
    let bare = Plot::new(&panel(), "panel")
        .bind("x", "x_axis")
        .bind("y", "y_axis");
    let without_geom = render(bare, 200, 200, bg);
    assert_eq!(
        with_geom, without_geom,
        "an unregistered name should draw nothing at all"
    );
}

/// The whole point of `ImageRegistry`: the image travels as a name, so a
/// scale can pick which one each row gets.
#[test]
fn a_scale_maps_categories_to_registry_names() {
    let bg: Color = rgb8(248, 248, 252);
    let mut plot = Plot::new(&panel(), "panel")
        .bind("x", "x_axis")
        .bind("y", "y_axis")
        .bind("image", "which")
        .image_registry(registry());
    plot.add_geom(
        ImageGeom::builder()
            .set("image", vec!["lo", "hi"])
            .set("x", vec![3.0_f64, 7.0])
            .set("y", vec![5.0_f64, 5.0])
            .set("width", 40.0_f64)
            .set("height", 40.0_f64)
            .build(),
    );

    let mut view = PlotComposition::new(&panel())
        .add_scale("x_axis", scale::continuous(0.0..=10.0))
        .add_scale("y_axis", scale::continuous(0.0..=10.0))
        .add_scale(
            "which",
            scale::ordinal(["lo", "hi"])
                .range_strings([std::sync::Arc::from("red"), std::sync::Arc::from("blue")]),
        )
        .with_plot(plot);

    let mut renderer = VelloRenderer::new().expect("vello renderer init");
    {
        let scene = renderer.scene();
        scene.clear();
        view.render(scene, Size::new(400.0, 400.0), 96.0);
    }
    let mut pixels = vec![0u8; 400 * 400 * 4];
    renderer
        .render_to_buffer(400, 400, bg, &mut pixels)
        .expect("render");

    let has_red = red_fraction(&pixels) > 0.005;
    let has_blue = pixels
        .chunks_exact(4)
        .filter(|px| px[2] > 180 && px[0] < 120)
        .count()
        > 800;
    assert!(has_red, "the \"lo\" category did not resolve to \"red\"");
    assert!(has_blue, "the \"hi\" category did not resolve to \"blue\"");
}
