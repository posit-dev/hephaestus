# src/text/CLAUDE.md

**Scaffolding** text shaping / layout backed by `parley`. Gated behind the `text` cargo feature.

## What this module does

Provides just enough text infrastructure to render axis labels, legends, plot titles, and the `TextGeom` / `TextFitGeom` / `TextPathGeom` plot geoms. Three exposed types:

- **`TextStyle`** — minimal style descriptor (`size_px`, optional `family`, CSS-style `weight`, `italic`). Build with `TextStyle::new(size).family("Helvetica").weight(700).italic(true)`.
- **`TextRun`** — shaped string + cached parley `Layout`. Implements `crate::layout::Measure`, so it drops directly into a `Cell::measured(run)` and participates in Auto-track sizing in `layout/`. `set_max_width(px)` re-breaks lines cheaply (parley keeps the shaping result; only line breaking re-runs).
- **`draw_text`** — bridge from a positioned `TextRun` to `SceneBuilder::draw_glyphs`.

Plus `Alignment` (re-exported from parley) for line justification — geom-facing string aliases (`"start"`, `"center"`, `"end"`, `"justify"`) parse through the `justify_x` channel.

## Why this exists (and why it's marked scaffolding)

The top-level `CLAUDE.md` lists font selection / loading at the scene level as out of scope; the host crate is expected to bring its own shaper. This module is a stopgap so chrome rendering and text geoms work today.

When the host's shaper lands, this module's job is to disappear — `TextRun`'s `Measure` impl and `draw_text`'s glyph-emission contract are the surface to preserve. Anything inside (parley layout, FontContext caching) is replaceable.

## Conventions

- **`TextStyle` is deliberately minimal.** Letter spacing, line height, decorations, OpenType features — none of those are here. Add a property only if a composition test or chrome path actually needs it before a real shaper lands. Resist API growth; the module is meant to be replaced.
- **`FontContext` is a process-global `Mutex<FontContext>`** lazily initialised on first use. Shaping is serialised but cheap relative to per-frame work, so the simple Mutex suffices. Don't add per-call font contexts.
- **Font discovery uses parley's defaults** — enumerates system fonts on construction. On machines without common families the layout still works but the rendered glyphs depend on what fontique finds. Acceptable for a scaffolding module.
- **Brush type is `()`.** Parley's brush generic parameter is fixed to `()` here; real brushes are passed to `draw_text` at draw time, not embedded in the layout.

## Cross-references

- `scene/` — `draw_text` issues `SceneBuilder::draw_glyphs` calls with `GlyphRun` values.
- `layout/` — `TextRun` implements `Measure` (`width_hint`, `height_at`, `width_at`) so it participates in Auto sizing and the iteration loop.
- `composition/` — text drops into anatomical slots via `Cell::measured(run)`.
- `plot/scale/axis.rs`, `plot/scale/legend.rs` — chrome rendering depends on this module (and is the main reason it exists at this stage).
- `plot/geom/text.rs`, `plot/geom/text_fit.rs`, `plot/geom/text_path.rs` — text-based geoms.
- `shape.rs` — glyph-backed shape markers also use `SceneBuilder::draw_glyphs` (via a different path).
