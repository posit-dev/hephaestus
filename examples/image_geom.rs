//! End-to-end visual sanity for `ImageGeom`. Three renders:
//!
//! - `image_geom_1_markers.png` — anchored mode: one image per data point
//!   at a fixed pt size, rotated by a per-row `"angle"`. The size is
//!   absolute, so it does not track the panel.
//! - `image_geom_2_categorical.png` — a discrete scale maps a category
//!   column to registry names, and each image fills its band. Band
//!   offsets are set explicitly, because `ImageGeom` defaults every edge
//!   to `0.0` rather than assuming bar-chart intent.
//! - `image_geom_3_fit.png` — the same non-square image in three
//!   identical boxes under `"stretch"`, `"contain"` and `"cover"`.
//! - `image_geom_4_markdown.png` — images inside markdown: one inline in
//!   a title, one inline in a text row, one alone in its paragraph (so it
//!   fills the column), and a location that resolves to nothing, which
//!   draws the placeholder.
//!
//! Every image here is generated in-process and round-tripped through the
//! PNG codec in `hephaestus::image`, so the example also exercises the
//! encoder and decoder against each other.

use std::sync::Arc;

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::brush::Image;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{Composition, Patch, Span};
use hephaestus::geometry::Size;
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement};
use hephaestus::plot::{
    scale, ImageGeom, ImageRegistry, Plot, PlotComposition, RectGeom, TextGeom,
};
use hephaestus::scales::chrome::AxisSide;
use hephaestus::scene::SceneBuilder;
use hephaestus::Renderer;

fn main() {
    let dpi = 96.0;
    let comp = || Composition::empty(1, 1).place(1, 1, Span::cell(), Patch::new("panel"));

    let mut renderer = VelloRenderer::new().expect("vello renderer init");
    let bg: Color = rgb8(248, 248, 252);

    // ── Render 1: images as markers at a fixed pt size ───────────────
    {
        let mut registry = ImageRegistry::new();
        registry.insert("arrow", arrow(48, 48));

        let xs = vec![1.0_f64, 2.0, 3.0, 4.0, 5.0, 6.0];
        let ys = vec![2.0_f64, 4.5, 3.0, 5.5, 4.0, 6.5];
        // A quarter turn per point, counter-clockwise.
        let angles: Vec<f64> = (0..xs.len())
            .map(|i| i as f64 * std::f64::consts::FRAC_PI_4)
            .collect();

        let mut plot = Plot::new(&comp(), "panel")
            .bind("x", "x_axis")
            .bind("y", "y_axis")
            .image_registry(registry);
        plot.add_geom(
            ImageGeom::builder()
                .set("image", "arrow")
                .set("x", xs)
                .set("y", ys)
                .set("width", 28.0_f64)
                .set("angle", angles)
                .build(),
        );
        plot.add_axis(Axis::rail(
            "x_axis",
            AxisPlacement::Cartesian(AxisSide::Bottom),
        ));
        plot.add_axis(Axis::rail(
            "y_axis",
            AxisPlacement::Cartesian(AxisSide::Left),
        ));

        let mut view = PlotComposition::new(&comp())
            .add_scale("x_axis", scale::continuous(0.0..=7.0))
            .add_scale("y_axis", scale::continuous(0.0..=8.0))
            .with_plot(plot)
            .title("Anchored: an absolute pt size at each data point");

        render_to(
            &mut renderer,
            &mut view,
            800,
            500,
            dpi,
            bg,
            "examples/image_geom_1_markers.png",
        );
    }

    // ── Render 2: one image per category, filling its band ───────────
    {
        let mut registry = ImageRegistry::new();
        registry.insert("warm", ramp(32, 32, [220, 120, 60], [250, 220, 120]));
        registry.insert("cool", ramp(32, 32, [50, 90, 180], [140, 210, 240]));
        registry.insert("neutral", ramp(32, 32, [90, 90, 100], [200, 200, 210]));

        let cats: Vec<&str> = vec!["A", "B", "C", "D", "E"];
        let heights = vec![24.0_f64, 38.0, 17.0, 45.0, 30.0];
        let kinds: Vec<&str> = vec!["warm", "cool", "neutral", "warm", "cool"];
        let n = cats.len();

        let mut plot = Plot::new(&comp(), "panel")
            .bind("x", "category")
            .bind("x2", "category")
            .bind("y", "value")
            .bind("y2", "value")
            .bind("image", "kind")
            .image_registry(registry);
        plot.add_geom(
            ImageGeom::builder()
                .set("image", kinds)
                .set("x", cats.clone())
                .set("x2", cats.clone())
                .set("y", vec![0.0_f64; n])
                .set("y2", heights)
                // ImageGeom defaults every band offset to zero, so a bar
                // that should fill its band asks for it.
                .set("x_band", -0.45_f64)
                .set("x2_band", 0.45_f64)
                .build(),
        );
        plot.add_axis(Axis::rail(
            "category",
            AxisPlacement::Cartesian(AxisSide::Bottom),
        ));
        plot.add_axis(Axis::rail(
            "value",
            AxisPlacement::Cartesian(AxisSide::Left),
        ));

        let mut view = PlotComposition::new(&comp())
            .add_scale("category", scale::ordinal(cats))
            .add_scale("value", scale::continuous(0.0..=50.0))
            .add_scale(
                "kind",
                scale::ordinal(["warm", "cool", "neutral"]).range_strings([
                    Arc::from("warm"),
                    Arc::from("cool"),
                    Arc::from("neutral"),
                ]),
            )
            .with_plot(plot)
            .title("Data-space: a scale picks each band's image");

        render_to(
            &mut renderer,
            &mut view,
            800,
            500,
            dpi,
            bg,
            "examples/image_geom_2_categorical.png",
        );
    }

    // ── Render 3: the three fit modes in identical boxes ─────────────
    {
        let mut registry = ImageRegistry::new();
        // A 3:1 image, so every fit mode has something visible to do in a
        // square box.
        registry.insert("wide", grid(96, 32));

        let modes = ["stretch", "contain", "cover"];
        let lefts = vec![0.5_f64, 3.5, 6.5];
        let rights = vec![2.5_f64, 5.5, 8.5];

        let mut plot = Plot::new(&comp(), "panel")
            .bind("x", "x_axis")
            .bind("y", "y_axis")
            .bind("x2", "x_axis")
            .bind("y2", "y_axis")
            .image_registry(registry);
        // A frame per box, so the letterboxing and the clipping are legible.
        plot.add_geom(
            RectGeom::builder()
                .set("x", lefts.clone())
                .set("x2", rights.clone())
                .set("y", vec![1.0_f64; 3])
                .set("y2", vec![3.0_f64; 3])
                .set("x_band", 0.0_f64)
                .set("x2_band", 0.0_f64)
                .set("stroke", rgb8(70, 70, 80))
                .set("linewidth", 1.0_f64)
                .build(),
        );
        plot.add_geom(
            ImageGeom::builder()
                .set("image", "wide")
                .set("x", lefts)
                .set("x2", rights)
                .set("y", vec![1.0_f64; 3])
                .set("y2", vec![3.0_f64; 3])
                .set("fit", modes.to_vec())
                .build(),
        );

        let mut view = PlotComposition::new(&comp())
            .add_scale("x_axis", scale::continuous(0.0..=9.0))
            .add_scale("y_axis", scale::continuous(0.0..=4.0))
            .with_plot(plot)
            .title("Fit: stretch, contain, cover — one 3:1 image, three square boxes");

        render_to(
            &mut renderer,
            &mut view,
            900,
            420,
            dpi,
            bg,
            "examples/image_geom_3_fit.png",
        );
    }

    // ── Render 4: images inside markdown ─────────────────────────────
    {
        let mut registry = ImageRegistry::new();
        registry.insert("arrow", arrow(48, 48));
        registry.insert("wide", grid(96, 32));

        let mut theme = hephaestus::plot::theme::Theme::default();
        // Chrome text parses markdown, so the title's tags resolve.
        theme.text.markdown = Some(true);

        let mut plot = Plot::new(&comp(), "panel")
            .bind("x", "x_axis")
            .bind("y", "y_axis")
            .image_registry(registry)
            .title("Inline ![](arrow) in a title, and a broken one ![](nope.png)");
        plot.add_geom(
            TextGeom::builder()
                .set("x", vec![2.0_f64, 5.5])
                .set("y", vec![3.0_f64, 1.5])
                .set(
                    "text",
                    vec![
                        "one em tall: ![](arrow) then more text".to_string(),
                        // A tag alone in its paragraph is a block image,
                        // so it fills the width it is broken to.
                        "![](wide)".to_string(),
                    ],
                )
                .set("markdown", true)
                .set("size", 11.0_f64)
                .set("width", 120.0_f64)
                .build(),
        );
        plot.add_axis(Axis::rail(
            "x_axis",
            AxisPlacement::Cartesian(AxisSide::Bottom),
        ));
        plot.add_axis(Axis::rail(
            "y_axis",
            AxisPlacement::Cartesian(AxisSide::Left),
        ));

        let mut view = PlotComposition::new(&comp())
            .add_scale("x_axis", scale::continuous(0.0..=9.0))
            .add_scale("y_axis", scale::continuous(0.0..=4.0))
            .theme(theme)
            .with_plot(plot);

        render_to(
            &mut renderer,
            &mut view,
            800,
            500,
            dpi,
            bg,
            "examples/image_geom_4_markdown.png",
        );
    }
}

// ─── Image sources ───────────────────────────────────────────────────────────
//
// Each helper builds RGBA8 pixels, encodes them as a PNG, and reads the PNG
// back — so what reaches the registry has been through both halves of
// `hephaestus::image`.

/// Round-trip pixels through the PNG codec on the way into an `Image`.
fn via_png(width: u32, height: u32, pixels: Vec<u8>) -> Image {
    let bytes = hephaestus::image::encode_png(width, height, &pixels, None).expect("encode");
    hephaestus::image::decode_png(&bytes).expect("decode")
}

/// An opaque triangle pointing along +x, on a transparent field. Rotation
/// is obvious on it in a way it is not on a symmetric shape.
fn arrow(width: u32, height: u32) -> Image {
    let (w, h) = (width as f64, height as f64);
    let mut px = Vec::with_capacity((width as usize) * (height as usize) * 4);
    for y in 0..height {
        for x in 0..width {
            // Inside the triangle (0, 0)-(0, h)-(w, h/2)?
            let (fx, fy) = (x as f64 / w, (y as f64 - h / 2.0).abs() / (h / 2.0));
            let inside = fy <= 1.0 - fx;
            if inside {
                px.extend_from_slice(&[210, 70, 60, 255]);
            } else {
                px.extend_from_slice(&[0, 0, 0, 0]);
            }
        }
    }
    via_png(width, height, px)
}

/// A vertical two-colour ramp.
fn ramp(width: u32, height: u32, bottom: [u8; 3], top: [u8; 3]) -> Image {
    let mut px = Vec::with_capacity((width as usize) * (height as usize) * 4);
    for y in 0..height {
        let t = y as f64 / (height - 1).max(1) as f64;
        let lerp = |a: u8, b: u8| (f64::from(a) + (f64::from(b) - f64::from(a)) * t) as u8;
        let (r, g, b) = (
            lerp(top[0], bottom[0]),
            lerp(top[1], bottom[1]),
            lerp(top[2], bottom[2]),
        );
        for _ in 0..width {
            px.extend_from_slice(&[r, g, b, 255]);
        }
    }
    via_png(width, height, px)
}

/// A checkerboard with a marked border. Distortion, letterboxing and
/// clipping are all legible on it: stretched squares stop being square,
/// a letterboxed image keeps its border inside the box, and a covering
/// one loses its border to the clip.
fn grid(width: u32, height: u32) -> Image {
    let cell = 8;
    let mut px = Vec::with_capacity((width as usize) * (height as usize) * 4);
    for y in 0..height {
        for x in 0..width {
            let edge = x < 2 || y < 2 || x + 2 >= width || y + 2 >= height;
            let on = ((x / cell) + (y / cell)) % 2 == 0;
            let rgb = if edge {
                [30, 30, 40]
            } else if on {
                [120, 170, 210]
            } else {
                [235, 240, 245]
            };
            px.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
        }
    }
    via_png(width, height, px)
}

fn render_to(
    renderer: &mut VelloRenderer,
    view: &mut PlotComposition,
    w: u32,
    h: u32,
    dpi: f64,
    bg: Color,
    out_relative: &str,
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
    hephaestus::image::write_png(&path, w, h, &pixels, Some(dpi)).expect("write png");
    println!("wrote {}", path.display());
}
