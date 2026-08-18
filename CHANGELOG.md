# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Unreleased

### Added

- **Markdown in every chrome text slot.** Legend titles, colorbar titles, break labels (cartesian ticks, legend keys and polar rails alike) and legend text swatches now read the `markdown` flag, so a theme that turns rich text on gets it everywhere rather than only on the title band, axis titles and strip labels. Polar titles that stamp glyphs along an arc stay plain — the rich pipeline has no per-glyph placement surface.
- **`RichTextRun` answers the metrics `TextRun` does** — `baseline_offset`, `cap_height`, `ink_top_offset` and `inked_height` — so a caller can anchor either kind of run the same way. The inked band unions the glyph ink with every block paint box, so a backgrounded block measures at the size it draws.
- **`AxisTheme::resolved_with_root`** — resolves an `AxisTheme` against a root text element, the way `Theme::resolved_axis` already did for `PerAxis`.

### Changed

- **The built-in rich-text sheet no longer sets a line height on `base`.** The caller's `TextStyle::line_height` was the one field the sheet overrode, which left a chrome slot unable to reach its own theme's line height without rewriting the sheet. A plain string now measures identically through both shapers. A document that wants marquee's `1.6` leading asks for it on the style it passes.
- **`RichTextRun`'s `Measure::height_at` reports the inked band** rather than the stacked line box, matching `TextRun`. A markdown slot no longer reserves the half-leading above the first line and below the last that the run never paints into.
- **Unwrapped chrome runs are memoized across frames.** Break labels and chrome titles shape at natural width and never re-break, so they cache cleanly; only the slots the layout solver re-breaks (title band, axis titles, strip labels) stay uncached. Both the measure and the draw pass read the same memo, which also makes the two structurally unable to disagree.

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
