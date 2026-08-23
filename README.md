# hephaestus

[![Crates.io](https://img.shields.io/crates/v/hephaestus.svg)](https://crates.io/crates/hephaestus)
[![Docs.rs](https://docs.rs/hephaestus/badge.svg)](https://docs.rs/hephaestus)
[![Check](https://github.com/posit-dev/hephaestus/actions/workflows/check.yml/badge.svg)](https://github.com/posit-dev/hephaestus/actions/workflows/check.yml)
[![MSRV](https://img.shields.io/badge/rustc-1.86+-blue.svg)](https://github.com/posit-dev/hephaestus)
[![npm](https://img.shields.io/npm/v/hephaestus-wasm)](https://www.npmjs.com/package/hephaestus-wasm)

A backend-agnostic, high performant, 2D scene renderer for data visualization, written in Rust.

Hephaestus is written to make it easy and convenient to create high-level visualization APIs without having to worry about the actual display of data. It provides a vectorized high-level API along with a low-level api for full control. Both APIs are composable so it is not a question of either or. In general, the high-level API is still flexible and "low-level" enough to serve almost all needs while still providing tangible benefits for the user.

### Features

* A rich vectorised set of different geometries
* A Layout system that supports easy plot composition with easy resizing
* Markdown parsing and rendering for rich text support
* Native wasm support for embedding on websites
* Scales and projections
* Render to png, jpeg, tiff, webp, canvas, or a window buffer
* Theming system

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
