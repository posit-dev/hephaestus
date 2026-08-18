# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Unreleased

### Added

- **JPEG, TIFF and WebP writers** — behind the new `jpeg`, `tiff` and `webp` features, one encoder each (`jpeg-encoder`, `tiff`, `image-webp`; all pure Rust and wasm-clean). Each format offers the same three entry points as PNG: `write_*` to a path, `write_*_to` for any writer, `encode_*` for the bytes in memory. TIFF and WebP are lossless and carry alpha; TIFF takes a `TiffCompression` (deflate, LZW, PackBits, or none) and declares its alpha unassociated. JPEG has no alpha channel, so `write_jpeg` takes a `quality` (1–100) and a background `Color` to composite the buffer onto.
- **`hephaestus::image`** — the home of every raster writer, including PNG. `hephaestus::png::{write_png, encode_png, write_png_to}` continue to resolve as aliases for the entries in `image`.

### Changed

- Writers reject a zero width or height with `io::ErrorKind::InvalidInput` and a message naming the dimensions. A zero-area image already failed; the error is now the same shape as the wrong-buffer-length one, for every format.

## 0.1.0

First public release.

### Added

- **Scene API** — the backend-agnostic drawing surface: `SceneBuilder` (fill, stroke, images, glyph runs, meshes, layers) and `Renderer`, split so recording and vector backends need not satisfy GPU concerns. The public surface is the intersection of what Vello and Blend2D natively support.
- **Vello backend** — GPU rasterizer via wgpu, with headless render-to-buffer and an opt-in picking path that answers point queries from a parallel pick texture.
- **Layout engine** — recursive grids with `fr` / `auto` tracks, aspect-ratio `respect()`, and the `Measure` protocol for content-driven sizing.
- **Composition** — patchwork-style plot composition over a 13×16 anatomical grid, with chrome alignment across nested compositions via `Extent::TrackOf`.
- **Plot API** — `Plot` and the `PlotComposition` orchestrator, name-bound scales shared across panels, and key-based columnar diffing for identity-preserving animation.
- **Geoms** — point, line, segment, rect, ellipse, polygon, wedge, ribbon, B-spline, ribbon-B-spline, geometry, and three text geoms (mark, fit-to-box, along-path).
- **Scales** — continuous, discrete, binned, temporal, and identity families; log / sqrt / custom transforms; symmetric break control across majors and minors; locale-aware label formatting.
- **Projections** — cartesian and polar plus a `CustomProjection` hook, with edge densification so straight lines in data space curve correctly under non-linear projections.
- **Chrome and themes** — axes, legends, colorbars, facet strips, titles and captions, every element theme-driven with no hardcoded constants in the render path.
- **Text** — parley-backed shaping and layout, plus a marquee-flavoured markdown subset with inline styling, block borders, and backgrounds.
- **Optional features** — `google-fonts` for on-demand font fetching, `chrono` / `time` / `jiff` datetime interop, and `geom-wkt` / `geom-wkb` / `geom-geojson` spatial parsers.

### Notes

- `kurbo`, `peniko`, and `wgpu` types appear in the public API, so their major versions are part of this crate's semver contract. `parley` does not — text alignment is expressed with the crate's own `HAlign`.
- `blend2d`, `svg`, and `pdf` are feature placeholders with no backend code behind them yet.
