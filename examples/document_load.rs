//! Reads `examples/document.hep` and renders it at two sizes.
//!
//! Deliberately built with `document-read` and **not** `document-write`:
//! a consumer compiles only the half it uses, and this example is the
//! shape a wasm build would take. It holds no data, no plot code and no
//! scale definitions — everything comes out of the file.
//!
//! The two output sizes are chosen to differ in aspect. Their axes,
//! titles and tick spacing are laid out independently for each, because
//! the document carries the plot's configuration rather than a frozen
//! frame; a scaled image would stretch all three.
//!
//! Run `document_save` first, then this with
//! `--features document-read,png`.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::Color;
use hephaestus::document::{read_composition, ReadContext};
use hephaestus::geometry::Size;
use hephaestus::scene::SceneBuilder;
use hephaestus::Renderer;

fn main() {
    let path = "examples/document.hep";
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("cannot read {path}: {e}");
            eprintln!("run `cargo run --example document_save --features document-write` first");
            std::process::exit(1);
        }
    };
    println!("read {path} ({} bytes)", bytes.len());

    // Everything this consumer knows about the plot arrives in the file.
    // The context supplies only what a document can't carry: geom
    // constructors (all builtins by default) and named formatters (none
    // here, since the saved plot uses default labels).
    let ctx = ReadContext::new();

    let mut renderer = VelloRenderer::new().expect("a working wgpu adapter");

    for (w, h, name) in [
        (900u32, 420u32, "examples/document_1_wide.png"),
        (520u32, 700u32, "examples/document_2_tall.png"),
    ] {
        // Rebuilt per size only so each PNG starts from an untouched
        // composition; one composition can serve every size.
        let mut view = read_composition(&bytes, &ctx).expect("a readable document");

        renderer.scene().clear();
        view.render(
            renderer.scene(),
            Size::new(f64::from(w), f64::from(h)),
            96.0,
        );

        let mut buf = vec![0u8; (w * h * 4) as usize];
        renderer
            .render_to_buffer(w, h, Color::WHITE, &mut buf)
            .expect("render to buffer");
        hephaestus::png::write_png(name, w, h, &buf, Some(96.0)).expect("write png");
        println!("wrote {name} ({w}x{h})");
    }
}
