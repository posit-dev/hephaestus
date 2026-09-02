//! Where a frame's time goes on each backend, at a scale that hurts.
//!
//! A dense scatter is the case that separates them: the sparse-strip backend
//! computes coverage on the CPU, so its cost tracks total mark perimeter, while
//! the compute-shader backend hands that to the GPU. Run it before reaching for
//! an optimisation, so the thing being optimised is the thing that costs.
//!
//! ```sh
//! cargo run --release --example backend_perf --features vello,vello-hybrid,png -- 100000
//! ```
//!
//! The last two rows are what picking costs: filling the index as the scene
//! is drawn, and querying it. Neither is a rasterisation.
//!
//! The first row is the geometry every backend has to build regardless, so
//! subtract it before comparing the rest — otherwise the shared cost reads as
//! though it belonged to whichever backend is listed first.
use hephaestus::backend::hybrid::HybridRenderer;
use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::rgb8;
use hephaestus::geometry::Point;
use hephaestus::{Affine, Brush, FillRule, PickId, Renderer, SceneBuilder};
use kurbo::Shape;
use std::time::Instant;

const W: u32 = 900;
const H: u32 = 560;

fn draw(scene: &mut dyn SceneBuilder, n: usize) {
    let mut s = 0x2545_f491_4f6c_dd1du64;
    let mut unit = || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        (s >> 11) as f64 / (1u64 << 53) as f64
    };
    for i in 0..n {
        let x = unit() * W as f64;
        let y = unit() * H as f64;
        scene.fill(
            FillRule::NonZero,
            Affine::IDENTITY,
            &Brush::Solid(rgb8(70, 120, 220)),
            None,
            &kurbo::Circle::new((x, y), 2.0).to_path(0.2),
            PickId::Id(i as u32 + 1),
        );
    }
}

/// The same scatter, drawn the way the plot layer draws it: one marker path
/// shared by every mark, placed by a per-mark transform.
///
/// The difference matters only to picking. `draw` builds a fresh path per
/// mark in absolute coordinates, which is the worst case for the hit index —
/// nothing can be shared, so every mark's geometry is stored. This is the
/// case `plot::PointGeom` actually produces, where one stored path serves
/// them all.
fn draw_shared_marker(scene: &mut dyn SceneBuilder, n: usize) {
    let marker = kurbo::Circle::new((0.0, 0.0), 2.0).to_path(0.2);
    let mut s = 0x2545_f491_4f6c_dd1du64;
    let mut unit = || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        (s >> 11) as f64 / (1u64 << 53) as f64
    };
    for i in 0..n {
        let x = unit() * W as f64;
        let y = unit() * H as f64;
        scene.fill(
            FillRule::NonZero,
            Affine::translate((x, y)),
            &Brush::Solid(rgb8(70, 120, 220)),
            None,
            &marker,
            PickId::Id(i as u32 + 1),
        );
    }
}

/// Report the fastest of `iters` runs, after warming up.
///
/// The minimum rather than the mean: GPU clocks ramp, pipelines warm, and the
/// OS steals time, all of which only ever make a run slower. The fastest run is
/// the one least polluted by things that are not the code being measured.
fn time<T>(label: &str, iters: u32, mut f: impl FnMut() -> T) {
    for _ in 0..3 {
        f();
    }
    let mut best = f64::INFINITY;
    for _ in 0..iters {
        let t = Instant::now();
        f();
        best = best.min(t.elapsed().as_secs_f64() * 1000.0);
    }
    println!("{label:<46} {best:8.2} ms");
}

fn main() {
    let n: usize = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(100_000);
    println!("=== {n} marks, {W}x{H} ===");
    let bg = rgb8(248, 248, 252);
    let mut out = vec![0u8; (W * H * 4) as usize];

    // Baseline: the geometry every backend has to build regardless.
    time("baseline: build paths only, no scene", 10, || {
        let mut s = 0x2545_f491_4f6c_dd1du64;
        let mut unit = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 11) as f64 / (1u64 << 53) as f64
        };
        let mut acc = 0usize;
        for _ in 0..n {
            let x = unit() * W as f64;
            let y = unit() * H as f64;
            acc += kurbo::Circle::new((x, y), 2.0)
                .to_path(0.2)
                .elements()
                .len();
        }
        acc
    });

    // How long does just *recording* take (the op list we then replay)?
    time("hybrid: record only (no render)", 10, || {
        let mut sc = hephaestus::scene::recording::RecordingScene::new();
        draw(&mut sc, n);
        sc.ops.len()
    });

    let mut hy = HybridRenderer::new().unwrap();
    time("hybrid: record + render, no picking", 10, || {
        hy.scene().clear();
        draw(hy.scene(), n);
        hy.render_to_buffer(W, H, bg, &mut out).unwrap();
    });

    let mut hyp = HybridRenderer::with_picking().unwrap();
    time("hybrid: record + render, WITH picking", 10, || {
        hyp.scene().clear();
        draw(hyp.scene(), n);
        hyp.render_to_buffer(W, H, bg, &mut out).unwrap();
    });

    let mut ve = VelloRenderer::new().unwrap();
    time("classic: encode + render, no picking", 10, || {
        ve.scene().clear();
        draw(ve.scene(), n);
        let _ = ve.render_to_buffer(W, H, bg, &mut out);
    });

    let mut vep = VelloRenderer::with_picking().unwrap();
    time("classic: encode + render, WITH picking", 10, || {
        vep.scene().clear();
        draw(vep.scene(), n);
        let _ = vep.render_to_buffer(W, H, bg, &mut out);
    });

    let mut hys = HybridRenderer::with_picking().unwrap();
    time("hybrid: shared marker, WITH picking", 10, || {
        hys.scene().clear();
        draw_shared_marker(hys.scene(), n);
        hys.render_to_buffer(W, H, bg, &mut out).unwrap();
    });

    let mut hysn = HybridRenderer::new().unwrap();
    time("hybrid: shared marker, no picking", 10, || {
        hysn.scene().clear();
        draw_shared_marker(hysn.scene(), n);
        hysn.render_to_buffer(W, H, bg, &mut out).unwrap();
    });

    // What picking actually costs now: filling the index while drawing, then
    // the first query building the tree. Both are CPU-side and neither is a
    // rasterisation, which is the point of the whole arrangement.
    let mut ix = HybridRenderer::with_picking().unwrap();
    ix.scene().clear();
    draw(ix.scene(), n);
    ix.render_to_buffer(W, H, bg, &mut out).unwrap();
    let index = ix.pick_index().expect("picking enabled");
    // The tree is built lazily, so the first query after a frame pays for it.
    let t = std::time::Instant::now();
    let _ = std::hint::black_box(index.pick_at(Point::new(450.0, 280.0)));
    println!(
        "{:<46} {:8.2} ms",
        "pick: first query (builds the tree)",
        t.elapsed().as_secs_f64() * 1000.0
    );
    let mut s = 0x2545_f491_4f6c_dd1du64;
    let mut unit = || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        (s >> 11) as f64 / (1u64 << 53) as f64
    };
    let qs: Vec<Point> = (0..10_000)
        .map(|_| Point::new(unit() * W as f64, unit() * H as f64))
        .collect();
    let t = std::time::Instant::now();
    let mut found = 0usize;
    for &q in &qs {
        found += index.pick_at(q).is_some() as usize;
    }
    let us = t.elapsed().as_secs_f64() * 1e6 / qs.len() as f64;
    println!(
        "{:<46} {us:8.3} us/query   ({found}/{} hit)",
        "pick: warm query",
        qs.len()
    );
}
