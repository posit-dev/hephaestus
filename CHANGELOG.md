# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Unreleased

### Added

- **A second rasterizing backend, behind the off-by-default `vello-hybrid` feature.** `backend::hybrid::{HybridScene, HybridRenderer}` rasterize through Vello Hybrid's sparse strips — path processing and coverage on the CPU, a plain render pipeline on the GPU — independently of `vello`: either, both, or neither. Picking returns only ids that were actually drawn, there is no draw-count ceiling, and the rasterizer is under half the wasm size of the compute-shader one. Parity covers fills, strokes, gradients, clip and blend layers, meshes, images and text.
- **A wasm build that needs no WebGPU, behind the off-by-default `webgl` feature.** `backend::hybrid::HybridWebGlRenderer` runs the same sparse-strip rasterizer against a canvas's WebGL2 context and pulls in no wgpu at all; `window::WebGlHost` presents it with the same `render` / `resize` / `dispatch` surface `CanvasHost` offers. It cannot rasterize offscreen and does not implement `Renderer`, since the canvas is the only render target.
- **Live window presentation, behind the off-by-default `window` feature** (`winit`). `window::run(config, app)` opens an OS window and pumps an event loop; the app implements `WindowApp`, drawing into a `Frame` — scene, physical size and dpi — and taking resize, cursor and mouse-button events. A resize renders at the new size rather than stretching the last frame. Picking is opt-in through `WindowConfig::picking`, after which `EventCtx::pick_at` answers the id under any pixel. winit stays out of the public surface. Native only; see `examples/window.rs`.
- **Canvas presentation, behind the off-by-default `canvas` feature** (wasm32 only, no winit). `window::CanvasHost` attaches to a `<canvas>` already on a page and shares `WindowApp`, `Frame` and `Event` with the desktop window, but the page keeps its own event loop and calls `render`, `resize` and `dispatch`. Picking answers from a frame or two earlier rather than parking the main thread.
- **Presentation runs on either rasterizing backend.** `WindowConfig::backend` names it, and its variants exist only for the backends compiled in. `window` and `canvas` require *a* rasterizing backend rather than `vello` specifically. On the sparse-strip backend the hosts present straight into the swap chain, skipping the intermediate texture and its per-frame full-screen blit; `HybridRenderer::set_target_format` is how a host names its surface's format.
- **The hitmap can refresh less often than the frame.** `WindowConfig::pick_interval` caps how often the pick pass runs and `set_refresh_pick` is the per-render control under it; frames in between reuse the previous hitmap, so `pick_at` may describe a slightly older frame. The pick pass rasterizes the scene a second time — at 100k marks it costs 60 ms of a 145 ms frame — so throttling it during a resize drag or an animation recovers all of that.
- **`crates/hephaestus-web`** — a wasm render client, shipped as an npm package: a page points it at a canvas and a `.hplot` document and gets a plot that re-solves its layout on resize instead of stretching. WebGL2 is the default configuration and needs no WebGPU; the mutually exclusive `wgpu-backend` feature swaps in the compute-shader path. `PlotView` exposes `render`, `resize`, `setDark`, `isSupported`, `hasFonts` and `documentFormatVersion`, plus `saveOnRightClick` — an overlaid, coincident `<img>` that gives the plot an ordinary image context menu (Save image as…, Copy image, drag-to-save) under a caller-supplied filename, with the render dpi written into the exported PNG. `./build.sh` assembles `dist/` and `verify-dist.mjs` checks it the way a consumer loads it.
- **The wasm client registers fonts once per page.** WOFF and WOFF2 decode behind the default-on `webfonts` feature, `registerGoogleFont` needs no API key, and four static Roboto faces are fetched as a fallback when the page registers nothing of its own. CJK stays a bring-your-own case.
- **Plot documents** — behind the off-by-default `document-read` and `document-write` features (`document` enables both). `document::write_composition` captures a `PlotComposition` as a self-contained byte string and `document::read_composition` rebuilds a live one, so a plot can be authored in one process and drawn in another. Nothing shaped, measured or solved is written: the consumer calls `render` at whatever size it has and the plot reflows rather than scaling a frozen image. `WriteOptions` carries the render hints, `embed_fonts`, and a `lossy` switch for the items `unsupported_items` reports as unwritable; `ReadContext` takes the host's own geom constructors and named formatters. Hand-rolled and dependency-free, so `--no-default-features --features document-write` is a complete writer with no renderer at all. See `examples/document_save.rs` and `examples/document_load.rs`.
- **`document::read_hints` and `DocumentHints`** — read a document's render hints (`background`, `size`, `dpi`) without rebuilding the composition. Decodes the head alone, so it is cheap enough to call first.
- **`document::FORMAT_VERSION_MAJOR` / `FORMAT_VERSION_MINOR`** — the document format version this build speaks. The reader requires an exact major match, so it is a hard compatibility boundary between writer and reader.
- **JPEG, TIFF and WebP writers** — behind the new `jpeg`, `tiff` and `webp` features, one pure-Rust encoder each. Every format offers the same three entry points as PNG: `write_*` to a path, `write_*_to` for any writer, `encode_*` for the bytes. TIFF and WebP are lossless and carry alpha; TIFF takes a `TiffCompression`, and `write_jpeg` takes a `quality` (1–100) and a background `Color` to composite onto.
- **`hephaestus::image`** — the home of every raster writer, including PNG. `hephaestus::png::{write_png, encode_png, write_png_to}` continue to resolve as aliases.
- **`scene::recording::RecordingScene::replay`** — issue every recorded op against any `SceneBuilder`, in order. What lets a backend defer draws until it knows the frame size, or rasterize a second scene from the same draws.
- **`RecordingScene`, `Op` and `OwnedGlyphRun` are `PartialEq`.** Equality is op-for-op, which makes a recording a test oracle over drawing rather than pixels. `Font` compares by face — the same bytes at the same index — not by blob identity.
- **`examples/backend_perf.rs`** — where a frame's time goes on each backend at a given mark count, reporting the fastest of ten runs.
- **Markdown in every chrome text slot.** Legend titles, colorbar titles, break labels and legend text swatches read the `markdown` flag, so a theme that turns rich text on gets it everywhere. Polar titles that stamp glyphs along an arc stay plain.
- **`RichTextRun` answers the metrics `TextRun` does** — `baseline_offset`, `cap_height`, `ink_top_offset` and `inked_height` — so a caller can anchor either kind of run the same way. The inked band unions glyph ink with every block paint box.
- **`RichTextStyleSheet::iter`, `len` and `is_empty`** — walk a sheet's named style deltas.
- **`AxisTheme::resolved_with_root`** — resolve an `AxisTheme` against a root text element, as `Theme::resolved_axis` already did for `PerAxis`.
- **`Geom::kind`** — a stable wire name for the concrete geom type behind a `Box<dyn Geom>`. Every geom in the crate returns `Some`; the default is `None`, so a downstream geom opts in by overriding it and registering a matching constructor with `ReadContext::with_geom`. `GeomBuilder::from_parts` is the inverse of `into_parts`.
- **`Plot::add_boxed_geom` and `Plot::geoms`** — the counterpart to `remove_geom`, and a borrowing iterator over every geom with its id in draw order. Alongside them, getters for settings that had only setters: `aspect_ratio_ref`, `is_clipped`, `tracks_identity`, `title_ref`, `subtitle_ref`, `caption_ref` and `shape_registry_ref`.
- **`Scale::with_named_format` / `set_named_format`, and `Scale::format_spec`** — a label formatter registered under a name, plus `FormatSpec` (`Default`, `Named`, `Custom`) describing which kind a scale carries. A name is what lets a scale's labels be reproduced in another process; an anonymous `with_format` closure stays `Custom`.
- **`Scale::try_with_bins` and `try_set_bins`** — fallible siblings of `with_bins`, returning `BinEdgeError` (`TooFew`, `NotFinite`, `NotIncreasing`) instead of panicking. `try_set_bins` leaves the existing ladder in place when it rejects.
- **`linetype::try_pattern` and `linetype::check_pattern`** — fallible siblings of `pattern` and `validate_pattern`, returning `PatternError` (`OddLength`, `Misaligned`).
- **`text::register_font_families`** — register a font blob and get the family names back, so a host with no system fonts can pair a blob with `set_generic_family` instead of guessing from a filename. `register_font_bytes` keeps returning the face count.
- **`text::registered_families`** — the font families the context knows, so a caller can decide whether a fallback font is needed.
- **`text::font_faces_for_family`, `generic_family_names` and `set_generic_family`** — read the font files backing a family, or a generic family's resolution order, out of the font context and reinstate them elsewhere. `font_faces_for_family` returns one entry per distinct file, since a collection holds a whole family in one.

### Changed

- **`Locale` is a tag and nothing more**: `Locale::EN_US`, `Locale::from("ar-EG")`, `locale.tag()`. Describing a locale correctly takes a CLDR-sized table, which belongs with whatever formats labels rather than with the renderer that draws them. The tag rides on `Theme`, reaches every `LabelFormatter`, and is written into a plot document. It is carried verbatim, so `ar_EG` and `ar-EG` are distinct locales.
- **The default tick formatter ignores the locale.** Numbers render with a `.` decimal and temporal values as compact `YYYY-MM-DD` / `HH:MM:SS`, whatever locale is passed. An axis that should follow a locale supplies a closure through `Scale::with_format`, which receives the tag.
- **`rust-version` is 1.86**, down from 1.88 — what this crate's own source needs. A build pulling in `vello` or `parley` still wants 1.88; the floor exists for the renderer-free `--no-default-features --features document-write` configuration, which needs `--ignore-rust-version` today because cargo refuses on `parley`'s declared floor alone.
- **wasm builds no longer compile wgpu's `webgl` backend.** On wasm, wgpu exists only to serve `vello`, which rasterizes through compute pipelines that WebGL2 has no stage for, so the GL hal only ever backed an adapter that would fail inside pipeline creation. An unsupported browser now fails at adapter selection instead.
- **The built-in rich-text sheet no longer sets a line height on `base`**, which had left a chrome slot unable to reach its own theme's line height. A plain string measures identically through both shapers; a document wanting marquee's `1.6` leading asks for it on the style it passes.
- **`RichTextRun`'s `Measure::height_at` reports the inked band** rather than the stacked line box, matching `TextRun`. A markdown slot no longer reserves half-leading the run never paints into.
- **Unwrapped chrome runs are memoized across frames.** Break labels and chrome titles shape at natural width and never re-break, so they cache; only the slots the layout solver re-breaks stay uncached. The measure and draw passes read the same memo.
- Writers reject a zero width or height with `io::ErrorKind::InvalidInput` and a message naming the dimensions, for every format.

### Removed

- **`Locale`'s descriptive fields, and the `Weekday` enum.** `decimal`, `grouping`, `month_short`, `month_long`, `day_short`, `day_long`, `am`, `pm` and `first_dow` are gone, and `Weekday` with them. `Locale` is also no longer `Copy`. A tag goes in through `Locale::from` or `Locale::from_static` and comes out through `locale.tag()`.

### Fixed

- **The theme's root `text` element reaches legend text.** The legend cascade resolved titles and break labels against the concrete defaults only, so a figure-wide font, color or `markdown` switch set on `theme.text` never landed on a legend.

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
