//! Rasterizes `examples/document.hep` to `examples/document.png` — the
//! static picture a host shows while the wasm render client boots.
//!
//! This is the reference for a producer that links `hephaestus` natively and
//! emits its plots into a page: it ships both halves of the same composition,
//! a document that reflows and a picture that is on screen before any script
//! runs. `crates/hephaestus-wasm`'s `placeholder` option takes it from there,
//! swapping the picture out the moment the first live frame lands.
//!
//! Three things make the swap invisible, and all three are visible here:
//!
//! - **The picture comes from the document, not from the composition that
//!   wrote it.** A document carries configuration, and a few things — a custom
//!   formatter, an unnameable geom — cannot travel; `unsupported_items`
//!   reports them. Rasterizing what the *reader* rebuilds is what guarantees
//!   the two frames agree. Compiled with `document-read` and not
//!   `document-write` for exactly that reason, as `document_load` is.
//! - **The backend is `vello-hybrid`**, the same sparse-strip rasterizer the
//!   client's WebGL2 build runs. The compute-shader backend antialiases by a
//!   different algorithm, so its edges would not match.
//! - **Size, dpi and background come from one place.** The picture is rendered
//!   at the canvas's device-pixel box with dpi `96 × ratio`, which is what the
//!   client's own resize path uses, and cleared to the document's background
//!   hint so any letterbox matches the plot.
//! - **The fonts are the client's own faces.** A browser enumerates no system
//!   fonts, so the client resolves `sans-serif` to the Roboto it ships;
//!   natively the same generic reaches whatever the system has. Different
//!   faces mean different advances, which means different line breaks and
//!   different tick label widths — a reflow, not a rounding difference. So
//!   this registers the committed faces from `crates/hephaestus-wasm/fonts/`
//!   and points the generic at them, which is what a producer has to do too.
//!
//! Run `document_save` first, then this with
//! `--features document-read,vello-hybrid,png`. Optional arguments are the CSS
//! width, the CSS height and the device pixel ratio.

use hephaestus::backend::hybrid::HybridRenderer;
use hephaestus::color::Color;
use hephaestus::document::{read_composition, read_hints, ReadContext};
use hephaestus::geometry::Size;
use hephaestus::scene::SceneBuilder;
use hephaestus::text::GenericFamilyKind;
use hephaestus::Renderer;

fn main() {
    let mut args = std::env::args().skip(1);
    let css_w: f64 = arg(args.next(), 900.0, "width");
    let css_h: f64 = arg(args.next(), 420.0, "height");
    let ratio: f64 = arg(args.next(), 1.0, "ratio");

    register_client_fonts();

    let path = "examples/document.hep";
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("cannot read {path}: {e}");
            eprintln!("run `cargo run --example document_save --features document-write` first");
            std::process::exit(1);
        }
    };

    // The hints say what the writer had in mind. Only the background is used
    // here — the size is the host's to decide, and this is the host.
    let hints = read_hints(&bytes).expect("a readable document");
    let background = hints.background.unwrap_or(Color::WHITE);

    let mut view = read_composition(&bytes, &ReadContext::new()).expect("a readable document");

    // Device pixels for the buffer, points-per-inch for the theme's lengths.
    // A ratio above one is a bigger buffer at a higher dpi, which is the same
    // layout at more samples rather than a scaled one.
    let (w, h) = (
        ((css_w * ratio).round() as u32).max(1),
        ((css_h * ratio).round() as u32).max(1),
    );
    let dpi = 96.0 * ratio;

    let mut renderer = HybridRenderer::new().expect("a working wgpu adapter");
    renderer.scene().clear();
    view.render(renderer.scene(), Size::new(f64::from(w), f64::from(h)), dpi);

    let mut buf = vec![0u8; (w as usize) * (h as usize) * 4];
    renderer
        .render_to_buffer(w, h, background, &mut buf)
        .expect("render to buffer");

    // The dpi travels in the file. On the path where the renderer never starts
    // this picture is what a viewer saves, and a PNG that declares nothing is
    // read as 72 dpi — a 2x render would claim twice its physical size.
    let name = "examples/document.png";
    hephaestus::png::write_png(name, w, h, &buf, Some(dpi)).expect("write png");
    println!("wrote {name} ({w}x{h} at {dpi} dpi, {css_w}x{css_h} css at {ratio}x)");
}

/// Parse one optional positional argument, or fall back to `default`.
fn arg(value: Option<String>, default: f64, name: &str) -> f64 {
    match value {
        None => default,
        Some(s) => match s.parse::<f64>() {
            Ok(v) if v.is_finite() && v > 0.0 => v,
            _ => {
                eprintln!("{name} must be a positive number, got {s:?}");
                std::process::exit(1);
            }
        },
    }
}

/// Register the faces the wasm client ships, so a picture made here and a
/// frame drawn there shape identically.
///
/// A missing directory is a warning rather than a failure: the picture is
/// still correct, it just stops being comparable to the client's frame.
fn register_client_fonts() {
    let dir = std::path::Path::new("crates/hephaestus-wasm/fonts");
    let mut families: Vec<String> = Vec::new();
    for face in ["regular", "bold", "italic", "bolditalic"] {
        let path = dir.join(format!("roboto-{face}.ttf"));
        match std::fs::read(&path) {
            Ok(bytes) => families.extend(hephaestus::text::register_font_families(bytes)),
            Err(e) => {
                eprintln!("warning: cannot read {}: {e}", path.display());
                eprintln!("the picture will not match a frame the wasm client draws");
                return;
            }
        }
    }
    families.sort();
    families.dedup();
    // After the faces, so the names the mapping points at exist.
    hephaestus::text::set_generic_family(GenericFamilyKind::SansSerif, &families);
    println!("registered {}", families.join(", "));
}
