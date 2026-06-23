# src/text/CLAUDE.md

Text shaping / layout backed by `parley`. Gated behind the `text` cargo feature. The committed text stack for chrome rendering and the text geoms.

## What this module does

Provides the text infrastructure that chrome (axis labels, legends, plot titles) and the `TextGeom` / `TextFitGeom` / `TextPathGeom` plot geoms render through. Three primary types:

- **`TextStyle`** — style descriptor covering size (pt, DPI-independent), family chain, CSS-style weight / width, italic / oblique, OpenType features, variable-font variations. Build with `TextStyle::new(size_pt).family("Helvetica").weight(700).italic(true)`.
- **`TextRun`** — shaped string + cached parley `Layout`. Implements `crate::layout::Measure`, so it drops directly into a `Cell::measured(run)` and participates in Auto-track sizing in `layout/`. Constructed via `TextRun::new(text, &style, dpi)` — the DPI converts the style's `size_pt` to pixels before shaping. `set_max_width(px)` re-breaks lines cheaply (parley keeps the shaping result; only line breaking re-runs).
- **`draw_text`** — bridge from a positioned `TextRun` to `SceneBuilder::draw_glyphs`.

Plus `Alignment` (re-exported from parley) for line justification — geom-facing string aliases (`"start"`, `"center"`, `"end"`, `"justify"`) parse through the `justify_x` channel.

## Host-supplied shaper (optional extension)

A host crate that wants to plug in its own shaper can do so by preserving `TextRun`'s `Measure` impl and `draw_text`'s glyph-emission contract — those are the stable surface. Anything inside (parley layout, `FontContext` caching) is implementation detail. This is an opt-in extension, not the planned trajectory.

## Conventions

- **`TextStyle` grows on demand.** Add a property when a chrome path or a geom actually needs it — the same bar that applies to any other public surface in the crate. Letter spacing, line height, OpenType features, variable-font variations are here because chrome and geoms exercise them.
- **`FontContext` is a process-global `Mutex<FontContext>`** lazily initialised on first use. Shaping is serialised but cheap relative to per-frame work, so the simple Mutex suffices. Don't add per-call font contexts.
- **Font discovery uses parley's defaults** — enumerates system fonts on construction. Hosts can extend the resolvable set via `register_font_bytes` / `register_font_path` / `register_font_dir`; missing families fall back to the resolved generic family. The optional `text-google-fonts` feature adds `fetch_google_font(family)` for on-demand Google Fonts lookup with on-disk caching.
- **Brush type is `()`.** Parley's brush generic parameter is fixed to `()` here; real brushes are passed to `draw_text` at draw time, not embedded in the layout.

## Cross-references

- `scene/` — `draw_text` issues `SceneBuilder::draw_glyphs` calls with `GlyphRun` values.
- `layout/` — `TextRun` implements `Measure` (`width_hint`, `height_at`, `width_at`) so it participates in Auto sizing and the iteration loop.
- `composition/` — text drops into anatomical slots via `Cell::measured(run)`.
- `plot/scale/axis.rs`, `plot/scale/legend.rs` — chrome rendering depends on this module.
- `plot/geom/text.rs`, `plot/geom/text_fit.rs`, `plot/geom/text_path.rs` — text-based geoms.
- `shape.rs` — glyph-backed shape markers also use `SceneBuilder::draw_glyphs` (via a different path).
