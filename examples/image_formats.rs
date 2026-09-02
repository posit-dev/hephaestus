//! One render, four raster formats. Requires every writer feature:
//!
//! ```sh
//! cargo run --example image_formats --features jpeg,tiff,webp
//! ```
//!
//! The plot is rendered over a *transparent* background, which is what makes
//! the formats differ:
//!
//! - `image_formats.png`, `image_formats.tiff`, `image_formats.webp` — carry
//!   the alpha channel, so the panel floats on transparency.
//! - `image_formats.jpg` — JPEG has no alpha channel, so the buffer is
//!   composited onto the background color handed to `write_jpeg`.
//!
//! Every file records the 96 dpi the plot was rendered at, each in the place
//! its format keeps one: a `pHYs` chunk, the JFIF density fields, the TIFF
//! resolution tags, and an EXIF block in the WebP container.
//!
//! The printed file sizes are the other point of the example: on plot output
//! WebP is the smallest of the three lossless formats, and JPEG is the
//! largest of the four — flat fills and hard edges are the worst case for a
//! DCT codec, and the best case for lossless entropy coding.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{Composition, Patch, Span};
use hephaestus::geometry::Size;
use hephaestus::image::{write_jpeg, write_png, write_tiff, write_webp, TiffCompression};
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement};
use hephaestus::plot::{scale, Plot, PlotComposition, PointGeom};
use hephaestus::scales::chrome::AxisSide;
use hephaestus::scene::SceneBuilder;
use hephaestus::Renderer;

/// Fully transparent: every format that carries alpha keeps it, and JPEG has
/// to composite it away.
const TRANSPARENT: Color = Color::new([0.0, 0.0, 0.0, 0.0]);

fn main() {
    let (w, h) = (900u32, 500u32);
    let dpi = 96.0;

    let comp = || Composition::empty(1, 1).place(1, 1, Span::cell(), Patch::new("panel"));

    let xs: Vec<f64> = (0..60).map(|i| i as f64 * 1.7).collect();
    let ys: Vec<f64> = xs
        .iter()
        .map(|x| 50.0 + 25.0 * (x * 0.07).sin() - 0.15 * x)
        .collect();

    let mut plot = Plot::new(&comp(), "panel").bind("x", "x").bind("y", "y");
    plot.add_geom(
        PointGeom::builder()
            .set("x", xs)
            .set("y", ys)
            .set("fill", rgb8(70, 120, 220))
            .set("size", 6.0_f64)
            .build(),
    );
    plot.set_title("One render, four formats");
    plot.add_axis(Axis::rail("x", AxisPlacement::Cartesian(AxisSide::Bottom)));
    plot.add_axis(Axis::rail("y", AxisPlacement::Cartesian(AxisSide::Left)));

    let mut view = PlotComposition::new(&comp())
        .add_scale("x", scale::continuous(0.0..=100.0))
        .add_scale("y", scale::continuous(20.0..=80.0))
        .with_plot(plot);

    let mut renderer = VelloRenderer::new().expect("vello renderer init");
    {
        let scene = renderer.scene();
        scene.clear();
        view.render(scene, Size::new(w as f64, h as f64), dpi);
    }

    let mut pixels = vec![0u8; (w * h * 4) as usize];
    renderer
        .render_to_buffer(w, h, TRANSPARENT, &mut pixels)
        .expect("render");

    let dir = std::env::current_dir().unwrap().join("examples");
    let png = dir.join("image_formats.png");
    let jpg = dir.join("image_formats.jpg");
    let tif = dir.join("image_formats.tiff");
    let webp = dir.join("image_formats.webp");

    // Each writer records the dpi the plot was rendered at, so the four files
    // agree on their physical size rather than falling back to a viewer's own
    // default.
    write_png(&png, w, h, &pixels, Some(dpi)).expect("write png");
    // Quality 90, composited onto the light background the plot theme assumes.
    write_jpeg(&jpg, w, h, &pixels, 90, rgb8(248, 248, 252), Some(dpi)).expect("write jpeg");
    write_tiff(&tif, w, h, &pixels, TiffCompression::Deflate, Some(dpi)).expect("write tiff");
    write_webp(&webp, w, h, &pixels, Some(dpi)).expect("write webp");

    for path in [&png, &jpg, &tif, &webp] {
        let bytes = std::fs::metadata(path).expect("stat").len();
        println!(
            "wrote {} ({:.1} KiB)",
            path.display(),
            bytes as f64 / 1024.0
        );
    }
}
