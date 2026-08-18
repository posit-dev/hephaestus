# hephaestus

A backend-agnostic 2D scene renderer for data visualization, written in Rust.

`hephaestus` ships two API levels in one crate. The **scene API** (`SceneBuilder` + primitives + layout) is a backend-neutral drawing surface. The **plot API** (`plot::*` — geoms, scales, themes, and the `PlotComposition` orchestrator) is a grammar-of-graphics layer built on top of it. Use whichever level fits; they are layered, not alternatives.

The public surface is deliberately the *intersection* of what Vello and Blend2D natively support, so drawing code runs unchanged as backends are added. The initial backend is [Vello](https://github.com/linebender/vello) (GPU compute via wgpu); Blend2D (CPU raster), SVG, and PDF are planned.

Performance on dense, interactively-updated plots is the design driver. WASM is supported but is not the primary target.

## Install

```sh
cargo add hephaestus
```

## Quick start — the plot API

```rust
use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{grid, Patch};
use hephaestus::geometry::Size;
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement};
use hephaestus::plot::{scale, Plot, PlotComposition, PointGeom};
use hephaestus::scales::chrome::AxisSide;
use hephaestus::scene::SceneBuilder;
use hephaestus::Renderer;

let comp = || grid(1, 1, vec![Patch::new("main").into()]);
let xs: Vec<f64> = (0..60).map(|i| i as f64).collect();
let ys: Vec<f64> = xs.iter().map(|x| (x * 0.1).sin()).collect();

let mut plot = Plot::new(&comp(), "main").bind("x", "t").bind("y", "value");
plot.add_geom(
    PointGeom::builder()
        .set("x", xs)
        .set("y", ys)
        .set("fill", rgb8(220, 90, 70))
        .set("size", 4.0_f64)
        .build(),
);
plot.set_title("A sine wave");
plot.add_axis(Axis::rail("t", AxisPlacement::Cartesian(AxisSide::Bottom)));
plot.add_axis(Axis::rail("value", AxisPlacement::Cartesian(AxisSide::Left)));

let mut view = PlotComposition::new(&comp())
    .add_scale("t", scale::continuous(0.0..=60.0))
    .add_scale("value", scale::continuous(-1.0..=1.0))
    .with_plot(plot);

let (w, h) = (800u32, 500u32);
let mut renderer = VelloRenderer::new().expect("vello init");
{
    let scene = renderer.scene();
    scene.clear();
    view.render(scene, Size::new(w as f64, h as f64), 96.0);
}
let mut pixels = vec![0u8; (w * h * 4) as usize];
let bg: Color = rgb8(255, 255, 255);
renderer.render_to_buffer(w, h, bg, &mut pixels).expect("render");
```

Scales are shared by name: two plots that both `bind("x", "t")` resolve to the same `Scale`, so `view.update_scale("t", ...)` moves both panels from one call. Compositions nest — `beside`, `stack`, and friends build patchwork layouts whose axes and titles align across panels.

## Quick start — the scene API

```rust
use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::rgb8;
use hephaestus::{Affine, Brush, FillRule, PickId, Rect, Renderer, SceneBuilder};
use hephaestus::geometry::Shape;

let mut renderer = VelloRenderer::new().expect("vello init");
{
    let scene = renderer.scene();
    let path = Rect::new(40.0, 40.0, 240.0, 200.0).to_path(0.1);
    let brush: Brush = rgb8(60, 120, 200).into();
    scene.fill(FillRule::NonZero, Affine::IDENTITY, &brush, None, &path, PickId::Skip);
}
```

The renderer produces RGBA8 buffers for file output. The optional `window` feature presents those frames live instead — an OS window, resize, and pointer events wired to picking. Animation scheduling stays the host's problem by design.

## What's in the box

- **Geoms** — point, line, segment, rect, ellipse, polygon, wedge, ribbon, B-spline, ribbon-B-spline, geometry (WKT / WKB / GeoJSON), and three text geoms (mark, fit-to-box, along-path).
- **Scales** — continuous, discrete, binned, temporal, identity; log / sqrt / custom transforms; automatic and pinned breaks for both majors and minors; locale-aware label formatting.
- **Projections** — cartesian and polar, plus a `CustomProjection` hook. Non-linear projections densify edges so straight lines in data space curve correctly.
- **Chrome** — axes, legends, colorbars, facet strips, titles, captions, all theme-driven.
- **Themes** — every chrome element is a themed element; no hardcoded constants in the render path.
- **Rich text** — a marquee-flavoured markdown subset with inline styling, block borders, and backgrounds.
- **Picking** — opt-in per renderer; emits pixel ids for hit-testing without a CPU post-pass.
- **Live window** — optional `window` feature: presents frames on screen, re-lays-out on resize, and reports the pick id under the cursor.

## Features

| Feature | Default | What it does |
|---|---|---|
| `vello` | ✅ | GPU rasterizer via wgpu. |
| `png` | ✅ | PNG writer. Lossless, alpha preserved. |
| `jpeg` | | JPEG writer. Lossy, and the format has no alpha channel, so the buffer is composited onto a background color. |
| `tiff` | | TIFF writer. Lossless, alpha preserved, choice of compressor. |
| `webp` | | WebP writer. Lossless, alpha preserved, and smaller than PNG on most plots. |
| `window` | | Live window presentation: OS window, wgpu surface, event loop with resize and pointer events (`winit`). Requires `vello`. |
| `google-fonts` | | Fetch named Google Fonts families on demand. Network call on cache miss; cache hits are offline. |
| `chrono` / `time` / `jiff` | | `From` impls between the temporal newtypes and the matching datetime library. Pick whichever your code already uses. |
| `geom-wkt` / `geom-wkb` / `geom-geojson` | | Parsers for `scales::Geometry`. Hand-rolled and dependency-free, so toggling them only changes what constructors compile. |
| `blend2d` / `svg` / `pdf` | | Placeholders. Wired through cargo so dependent crates can name them; no backend code behind them yet. |

The core types and traits build with `--no-default-features`, pulling in no wgpu, so downstream crates can target `SceneBuilder` without GPU dependencies.

The four writer features are encoders over the RGBA8 buffer a renderer already produces, so each one costs only its own encoder — all four are pure Rust and build for wasm. They live together in `hephaestus::image`.

## Examples

Around 60 runnable examples live in `examples/`, each writing its output next to itself:

```sh
cargo run --example hello         # scene API sanity check
cargo run --example point         # plot API, shared scales across panels
cargo run --example polar         # polar projection
cargo run --example legends       # legend and colorbar variants
cargo run --example theme_dark    # theming

cargo run --example image_formats --features jpeg,tiff,webp  # all four raster writers
```

One example opens a window instead of writing a file:

```sh
cargo run --example window --features window   # live plot: resize + hover picking
```

## Status

Pre-1.0 and pre-stability: the API is expected to change as the remaining backends land. `kurbo`, `peniko`, and `wgpu` types appear in the public surface, so their major versions are part of this crate's semver contract.

## License

MIT — see [LICENSE.md](LICENSE.md).
