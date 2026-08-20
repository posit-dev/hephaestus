//! Reproducer: the same scene rasterises to two different images.
//!
//! Renders one unchanged `PlotComposition` at one size, repeatedly, with
//! a fresh `VelloRenderer` each time, and counts how many distinct
//! outputs come back. It should always be 1. On the machine this was
//! found on it is 2, split roughly evenly, with a single pixel differing
//! by one unit in its green and blue channels.
//!
//! Run with:
//!
//! ```sh
//! cargo run --example aa_nondeterminism
//! cargo run --release --example aa_nondeterminism
//! ```
//!
//! Notes on what has been ruled out, so nobody re-treads it:
//!
//! - **Not the scene.** Teeing the same render pass into a
//!   `RecordingScene` alongside the Vello scene gives a byte-identical op
//!   stream every time. The draw calls are deterministic; only the pixels
//!   are not.
//! - **Not renderer reuse.** Each render constructs its own
//!   `VelloRenderer`, so nothing carries over in backend state.
//! - **Not accumulated float drift.** There are exactly two outcomes,
//!   never three, however many times it runs.
//! - **Not the plot's own caches.** Two renders back to back, with no
//!   mutation between them, already disagree.
//!
//! The composition below is the smallest one found that still triggers
//! it. Both ingredients matter and neither is interesting in itself:
//! removing the composition caption stops it, and so does removing the
//! point geoms. The caption is not near the differing pixel — it only
//! shifts the layout enough to move a mark's antialiased edge onto a
//! subpixel position that triggers the bug. That suggests the real
//! trigger is a particular edge geometry rather than any of these
//! features.

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{beside, stack, Patch};
use hephaestus::geometry::Size;
use hephaestus::plot::{scale, Plot, PlotComposition, PointGeom};
use hephaestus::scene::SceneBuilder;
use hephaestus::Renderer;

/// Renders per trial. The flip rate is roughly even, so a handful would
/// do; this many just makes a clean run convincing.
const TRIALS: usize = 60;
const SIZE: (u32, u32) = (1200, 500);

fn build() -> PlotComposition {
    let comp = || {
        stack(
            beside(Patch::new("a"), Patch::new("b")),
            beside(Patch::new("c"), Patch::new("d")),
        )
    };

    let xs: Vec<f64> = (0..40).map(|i| f64::from(i) * 0.5).collect();
    let ys: Vec<f64> = xs.iter().map(|x| 10.0 + 8.0 * (x * 0.2).sin()).collect();

    let mut view = PlotComposition::new(&comp())
        // Removing this caption makes the output deterministic.
        .caption("Composition caption")
        .add_scale("t", scale::continuous(0.0..=20.0))
        .add_scale("v", scale::continuous(0.0..=20.0));

    for name in ["a", "b", "c", "d"] {
        let mut plot = Plot::new(&comp(), name).bind("x", "t").bind("y", "v");
        // Removing these geoms also makes the output deterministic.
        plot.add_geom(
            PointGeom::builder()
                .set("x", xs.clone())
                .set("y", ys.clone())
                .set("fill", rgb8(200, 30, 30))
                .set("size", 4.0_f64)
                .build(),
        );
        view = view.with_plot(plot);
    }
    view
}

fn render(comp: &mut PlotComposition, w: u32, h: u32) -> Vec<u8> {
    let mut renderer = VelloRenderer::new().expect("a working wgpu adapter");
    renderer.scene().clear();
    comp.render(
        renderer.scene(),
        Size::new(f64::from(w), f64::from(h)),
        96.0,
    );
    let mut buf = vec![0u8; (w * h * 4) as usize];
    renderer
        .render_to_buffer(w, h, Color::WHITE, &mut buf)
        .expect("render to buffer");
    buf
}

fn main() {
    let (w, h) = SIZE;
    let mut comp = build();

    // Keep one representative buffer per distinct output, so the
    // difference can be reported rather than just counted.
    let mut distinct: Vec<(Vec<u8>, usize)> = Vec::new();
    for _ in 0..TRIALS {
        let buf = render(&mut comp, w, h);
        match distinct.iter_mut().find(|(b, _)| *b == buf) {
            Some((_, count)) => *count += 1,
            None => distinct.push((buf, 1)),
        }
    }
    distinct.sort_by_key(|(_, c)| std::cmp::Reverse(*c));

    let counts: Vec<String> = distinct.iter().map(|(_, c)| c.to_string()).collect();
    println!(
        "{} distinct outputs over {TRIALS} renders of the same scene at {w}x{h} (counts {})",
        distinct.len(),
        counts.join("/")
    );

    if distinct.len() == 1 {
        println!("deterministic on this machine");
        return;
    }

    let (a, _) = &distinct[0];
    let (b, _) = &distinct[1];
    for i in 0..a.len() {
        if a[i] != b[i] {
            let px = i / 4;
            println!(
                "  pixel ({}, {}) channel {}: {} vs {}",
                px % w as usize,
                px / w as usize,
                i % 4,
                a[i],
                b[i]
            );
        }
    }
}
