//! Micro-benchmark of geom draw hot loops: `PointGeom::draw` with 200k
//! rows against a no-op `SceneBuilder`, so the measurement is channel
//! resolution + scale mapping, not rasterisation. Reports ns/row, best
//! of 7 runs. Two cases, mirroring issue #42:
//!
//! - continuous x/y scales
//! - discrete x scale with 8 levels, continuous y
//!
//! Run with:
//!
//! ```sh
//! cargo run --release --example geom_perf
//! ```

use std::hint::black_box;
use std::time::Instant;

use hephaestus::blend::BlendMode;
use hephaestus::brush::{Brush, Image, Sampling};
use hephaestus::geometry::{Affine, Rect};
use hephaestus::mesh::Mesh;
use hephaestus::path::{FillRule, Path};
use hephaestus::pick::PickId;
use hephaestus::plot::geom::DirectScaleResolver;
use hephaestus::plot::{Geom, GeomContext, PointGeom, Scale, ScaleTypeKind, Value};
use hephaestus::scene::{GlyphRun, SceneBuilder};
use hephaestus::shape::ShapeRegistry;
use hephaestus::stroke::Stroke;

/// A `SceneBuilder` that discards everything. The `black_box` on the
/// pick id keeps the draw calls themselves from being optimised away.
struct NullScene;

impl SceneBuilder for NullScene {
    fn clear(&mut self) {}

    fn fill(
        &mut self,
        _rule: FillRule,
        _transform: Affine,
        _brush: &Brush,
        _brush_transform: Option<Affine>,
        _path: &Path,
        pick_id: PickId,
    ) {
        black_box(pick_id);
    }

    fn stroke(
        &mut self,
        _stroke: &Stroke,
        _transform: Affine,
        _brush: &Brush,
        _brush_transform: Option<Affine>,
        _path: &Path,
        pick_id: PickId,
    ) {
        black_box(pick_id);
    }

    fn draw_image(
        &mut self,
        _image: &Image,
        _transform: Affine,
        _sampling: Sampling,
        _alpha: f32,
        pick_id: PickId,
    ) {
        black_box(pick_id);
    }

    fn draw_glyphs(&mut self, _run: &GlyphRun<'_>, pick_id: PickId) {
        black_box(pick_id);
    }

    fn draw_mesh(&mut self, _mesh: &Mesh, _transform: Affine, pick_id: PickId) {
        black_box(pick_id);
    }

    fn push_layer(&mut self, _blend: BlendMode, _alpha: f32, _transform: Affine, _clip: &Path) {}

    fn pop_layer(&mut self) {}
}

const N: usize = 200_000;
const RUNS: usize = 7;

fn bench(name: &str, geom: &PointGeom, ctx: &GeomContext<'_>) {
    let mut scene = NullScene;
    // Warm-up, then best-of-N.
    geom.draw(&mut scene, ctx);
    let mut best = f64::INFINITY;
    for _ in 0..RUNS {
        let t = Instant::now();
        geom.draw(&mut scene, ctx);
        best = best.min(t.elapsed().as_secs_f64());
    }
    println!("{name:<24} {:>8.1} ns/row", best * 1e9 / N as f64);
}

fn main() {
    let shapes = ShapeRegistry::with_builtins();
    let panel = Rect::new(0.0, 0.0, 1000.0, 800.0);

    // ── Continuous x/y ──
    let x: Vec<f64> = (0..N).map(|i| (i % 1000) as f64).collect();
    let y: Vec<f64> = (0..N).map(|i| ((i * 7919) % 1000) as f64).collect();
    let geom = PointGeom::builder()
        .set("x", x)
        .set("y", y)
        .set("fill", "steelblue")
        .build();
    let x_scale = Scale::new(ScaleTypeKind::Continuous).domain_continuous(0.0, 1000.0);
    let y_scale = Scale::new(ScaleTypeKind::Continuous).domain_continuous(0.0, 1000.0);
    let scales = DirectScaleResolver::new()
        .with("x", &x_scale)
        .with("y", &y_scale);
    let ctx = GeomContext::new(panel, 96.0, &shapes, &scales);
    bench("point continuous x/y", &geom, &ctx);

    // ── Discrete x (8 levels), continuous y ──
    let levels = ["a", "b", "c", "d", "e", "f", "g", "h"];
    let x: Vec<&str> = (0..N).map(|i| levels[i % levels.len()]).collect();
    let y: Vec<f64> = (0..N).map(|i| ((i * 7919) % 1000) as f64).collect();
    let geom = PointGeom::builder()
        .set("x", x)
        .set("y", y)
        .set("fill", "steelblue")
        .build();
    let x_scale =
        Scale::new(ScaleTypeKind::Discrete).domain_discrete(levels.iter().map(|s| Value::from(*s)));
    let scales = DirectScaleResolver::new()
        .with("x", &x_scale)
        .with("y", &y_scale);
    let ctx = GeomContext::new(panel, 96.0, &shapes, &scales);
    bench("point discrete x (8 lvls)", &geom, &ctx);

    // ── Discrete x (200 levels), continuous y ──
    // Leaked for 'static strs, matching the `set` column contract.
    let levels: Vec<&'static str> = (0..200).map(|i| &*format!("cat-{i}").leak()).collect();
    let x: Vec<&str> = (0..N).map(|i| levels[i % levels.len()]).collect();
    let y: Vec<f64> = (0..N).map(|i| ((i * 7919) % 1000) as f64).collect();
    let geom = PointGeom::builder()
        .set("x", x)
        .set("y", y)
        .set("fill", "steelblue")
        .build();
    let x_scale =
        Scale::new(ScaleTypeKind::Discrete).domain_discrete(levels.iter().map(|s| Value::from(*s)));
    let scales = DirectScaleResolver::new()
        .with("x", &x_scale)
        .with("y", &y_scale);
    let ctx = GeomContext::new(panel, 96.0, &shapes, &scales);
    bench("point discrete x (200 lvls)", &geom, &ctx);

    // ── Polygon mark grouping: 10k marks × 20 rows ──
    // `build()` leaves the mark layout empty, so the first `draw`
    // builds it; time exactly that call (grouping only — the draw
    // itself follows and is included, but the layout build dominates
    // at this mark count).
    const MARKS: usize = 10_000;
    const PER_MARK: usize = 20;
    let keys: Vec<i64> = (0..(MARKS * PER_MARK) as i64)
        .map(|i| i / PER_MARK as i64)
        .collect();
    let x: Vec<f64> = (0..MARKS * PER_MARK)
        .map(|i| (i % PER_MARK) as f64)
        .collect();
    let y: Vec<f64> = (0..MARKS * PER_MARK)
        .map(|i| ((i * 31) % PER_MARK) as f64)
        .collect();
    let x_scale = Scale::new(ScaleTypeKind::Continuous).domain_continuous(0.0, 20.0);
    let y_scale = Scale::new(ScaleTypeKind::Continuous).domain_continuous(0.0, 20.0);
    let scales = DirectScaleResolver::new()
        .with("x", &x_scale)
        .with("y", &y_scale);
    let ctx = GeomContext::new(panel, 96.0, &shapes, &scales);
    let mut scene = NullScene;
    let mut best = f64::INFINITY;
    for _ in 0..RUNS {
        // Fresh build each run: the mark layout is built lazily on the
        // first draw, which is the call being timed.
        let g = hephaestus::plot::PolygonGeom::builder()
            .keys(keys.clone())
            .set("x", x.clone())
            .set("y", y.clone())
            .set("fill", "steelblue")
            .build();
        let t = Instant::now();
        g.draw(&mut scene, &ctx);
        best = best.min(t.elapsed().as_secs_f64());
    }
    let rows = (MARKS * PER_MARK) as f64;
    println!(
        "{:<24} {:>8.1} ns/row",
        "polygon 10k marks (build+draw)",
        best * 1e9 / rows
    );
}
