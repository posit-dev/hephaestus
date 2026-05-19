# hephaestus

A backend-agnostic 2D scene renderer for data visualization, written in Rust.

**Status:** scaffolding. The crate defines a `SceneBuilder` trait and a `Renderer` trait. The initial backend is [Vello](https://github.com/linebender/vello) (GPU compute via wgpu). Future backends planned: Blend2D (CPU raster), SVG, PDF.

The public surface is the intersection of what Vello and Blend2D natively support, so plotting code written against `SceneBuilder` runs unchanged across backends.

## Quick start

```sh
cargo run --example hello
# writes examples/hello.png
```

## Features

- `vello` (default) — GPU rasterizer via wgpu.
- `png` (default) — PNG output helper.
- `blend2d`, `svg`, `pdf` — placeholders; not implemented.
