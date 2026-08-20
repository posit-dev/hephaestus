# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Unreleased

### Added

- **Markdown in every chrome text slot.** Legend titles, colorbar titles, break labels (cartesian ticks, legend keys and polar rails alike) and legend text swatches now read the `markdown` flag, so a theme that turns rich text on gets it everywhere rather than only on the title band, axis titles and strip labels. Polar titles that stamp glyphs along an arc stay plain — the rich pipeline has no per-glyph placement surface.
- **`RichTextRun` answers the metrics `TextRun` does** — `baseline_offset`, `cap_height`, `ink_top_offset` and `inked_height` — so a caller can anchor either kind of run the same way. The inked band unions the glyph ink with every block paint box, so a backgrounded block measures at the size it draws.
- **`AxisTheme::resolved_with_root`** — resolves an `AxisTheme` against a root text element, the way `Theme::resolved_axis` already did for `PerAxis`.
- **JPEG, TIFF and WebP writers** — behind the new `jpeg`, `tiff` and `webp` features, one encoder each (`jpeg-encoder`, `tiff`, `image-webp`; all pure Rust and wasm-clean). Each format offers the same three entry points as PNG: `write_*` to a path, `write_*_to` for any writer, `encode_*` for the bytes in memory. TIFF and WebP are lossless and carry alpha; TIFF takes a `TiffCompression` (deflate, LZW, PackBits, or none) and declares its alpha unassociated. JPEG has no alpha channel, so `write_jpeg` takes a `quality` (1–100) and a background `Color` to composite the buffer onto.
- **`hephaestus::image`** — the home of every raster writer, including PNG. `hephaestus::png::{write_png, encode_png, write_png_to}` continue to resolve as aliases for the entries in `image`.
- **Live window presentation** — behind the new off-by-default `window` feature (`winit`, requires `vello`). `window::run(config, app)` opens an OS window, owns the GPU device backing its swap chain, hands that device to the Vello renderer, and pumps an event loop; the app implements `WindowApp`, drawing into a `Frame` per frame and taking resize, cursor and mouse-button events through `event(&mut EventCtx, Event)`. A `Frame` carries the scene alongside the physical size and the DPI to render at, so theme lengths in pt / mm come out at the right physical size on a high-density display, and a resize schedules a frame at the new size rather than stretching the last one. Picking is opt-in per window through `WindowConfig::picking`, after which `EventCtx::pick_at` answers the id under any pixel — the pick id the host needs for hover and click without a CPU hit-test. winit stays out of the public surface: `Event`, `MouseButton` and `PresentMode` are the crate's own types. `run` is native-only; the rest of the module compiles for wasm, but the web entry point is not wired up. See `examples/window.rs`.

### Changed

- **The built-in rich-text sheet no longer sets a line height on `base`.** The caller's `TextStyle::line_height` was the one field the sheet overrode, which left a chrome slot unable to reach its own theme's line height without rewriting the sheet. A plain string now measures identically through both shapers. A document that wants marquee's `1.6` leading asks for it on the style it passes.
- **`RichTextRun`'s `Measure::height_at` reports the inked band** rather than the stacked line box, matching `TextRun`. A markdown slot no longer reserves the half-leading above the first line and below the last that the run never paints into.
- **Unwrapped chrome runs are memoized across frames.** Break labels and chrome titles shape at natural width and never re-break, so they cache cleanly; only the slots the layout solver re-breaks (title band, axis titles, strip labels) stay uncached. Both the measure and the draw pass read the same memo, which also makes the two structurally unable to disagree.
- Writers reject a zero width or height with `io::ErrorKind::InvalidInput` and a message naming the dimensions. A zero-area image already failed; the error is now the same shape as the wrong-buffer-length one, for every format.
- **`Locale` is a tag and nothing more.** It named a locale *and* described one — a decimal mark, a grouping separator, four tables of month and weekday names, AM / PM markers and a first day of week — but of those nine fields exactly one, the decimal mark, was read anywhere in the crate. Describing a locale correctly takes a CLDR-sized table, and that table belongs with whatever formats labels rather than with the renderer that draws them.

  So `Locale` carries a tag and nothing else: `Locale::EN_US`, `Locale::from("ar-EG")`, `locale.tag()`. It rides on `Theme`, reaches every `LabelFormatter` beside the value being formatted, and is written into a plot document so a consumer knows which locale a plot was for. The tag is a `Cow<'static, str>`, so the built-in constants stay compile-time values that cost no allocation while a tag obtained at runtime owns itself. It is carried verbatim — canonicalizing `ar_EG` to `ar-EG` needs the same table the type exists to avoid holding, so the two are distinct `Locale`s.
- **The default tick formatter ignores the locale.** It consulted `locale.decimal`, so `Locale::DE_DE` rendered `0,5` on a numeric axis. With no rules on the type there is nothing to consult: numbers render with a `.` decimal and temporal values as compact `YYYY-MM-DD` / `HH:MM:SS`, whatever locale is passed. An axis that should follow a locale supplies a closure through `Scale::with_format`, which receives the tag.

### Removed

- **`Locale`'s descriptive fields, and the `Weekday` enum.** `decimal`, `grouping`, `month_short`, `month_long`, `day_short`, `day_long`, `am`, `pm` and `first_dow` are gone, and `Weekday` with them — it existed only as `first_dow`'s type. `Locale` is also no longer `Copy`, since it now owns a string. Neither a struct field nor an enum can be kept resolving at its old path, so constructing a `Locale` literal, reading one of its fields, or naming `Weekday` all need changing; a tag goes in through `Locale::from` or `Locale::from_static` and comes out through `locale.tag()`.

### Fixed

- **The theme's root `text` element reaches legend text.** The legend cascade resolved titles and break labels against the concrete defaults only, so a figure-wide font, colour or `markdown` switch set on `theme.text` never landed on a legend.

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
