# src/plot/CLAUDE.md

The high-level plot API: typed columnar data, named scales, geoms that consume them, and the `PlotComposition` orchestrator that wires the whole thing into the layout from `composition/`. Layered on top of the low-level `SceneBuilder` surface.

## What this module does

A **plot** is a per-patch unit of state that binds channel names (`"x"`, `"y"`, `"color"`, etc.) to scale names and holds a list of geoms. Multiple plots share a single `ScaleRegistry`, so two plots that bind their `"x"` to `"time"` use the same configured scale — change the domain once, both update.

The canonical user-facing surface is **`PlotComposition`** (in `composition.rs`). It owns the layout shape, the scale registry, and a `HashMap<String, Plot>` of attached plots. `view.render(scene, size, dpi)` is the single entry point: the orchestrator rebuilds the composition fresh on every render with each plot's chrome wired into anatomical slots, solves the layout, and drives `draw_chrome_into` + `draw_panel_into` per plot.

For tests and one-off renders, `Plot` is independently usable with a hand-built `ScaleRegistry` and a manually constructed `Composition`.

## Subdirectories

- **`geom/`** — vectorised drawing primitives (`PointGeom`, `LineGeom`, `PolygonGeom`, `RectGeom`, `EllipseGeom`, `SegmentGeom`, `WedgeGeom`, plus `TextGeom` / `TextFitGeom` / `TextPathGeom` when the `text` feature is on). See `src/plot/geom/CLAUDE.md`.
- **`scale/`** — named value mappers (Continuous / Discrete / Ordinal / Binned / Identity), axes, legends. See `src/plot/scale/CLAUDE.md`.

## Core types (this folder, not in subdirectories)

- **`PlotComposition`** (`composition.rs`) — the orchestrator. Construct with `PlotComposition::new(composition)`; register scales with `add_scale("name", scale)`; attach plots with `with_plot(plot)` / `attach_plot(plot)`. Mutations flow through closures (`view.update_scale("time", |s| ...)`, `view.update_plot("price", |p| ...)`) so dirty-tracking stays accurate. The dirty model is conservative: any mutation flips `layout_dirty` and the next `render` re-solves. Per-plot / per-scale dirty bits are plumbed but only used by v1.5+ partial-repaint heuristics.
- **`Plot`** (`plot.rs`) — bound to a patch id. Stores channel → scale-name bindings, geom list (`Vec<(GeomId, Box<dyn Geom>)>`), chrome text (title / subtitle / caption / axis titles), and a `ShapeRegistry`. Three lifecycle methods used by the orchestrator: `wire(patch, registry, dpi)` (drop chrome cells + panel into named slots; full version is `text`-gated, `wire_panel` is always available), `draw_chrome_into(scene, layout)`, `draw_panel_into(scene, layout, registry)`.
- **`GeomId`** — opaque handle returned by `Plot::add_geom`; used with `Plot::update_geom` / `remove_geom`.
- **`Value`** (`value.rs`) — runtime-typed scalar. Variants: `Number(f64)`, `String(Arc<str>)`, `Color(Color)`, `Bool(bool)`, `Linetype(...)`, plus the temporal variants `Date`, `DateTime`, `Time`, `Duration`. Equality via `key_eq` (NaN canonicalises to a single class for diffing; ±0 distinguished).
- **`DataColumn`** (`value.rs`) — typed columnar container (`Vec<T>` per variant). Geom channels use this; the hot draw loop matches the column variant once at the top, then reads typed slices directly so per-row code stays monomorphic.
- **`Date`** / **`DateTime`** / **`Time`** / **`Duration`** — `repr(transparent)` temporal newtypes. Round-trip with Arrow semantics. Project to f64 (days / microseconds) when entering a continuous scale.
- **`LinetypeStep`** — one step in a dash pattern; used by `OutputRange::Linetypes`.
- **`KeyIndex`** / **`diff_columns`** / **`diff_positional`** (`diff.rs`) — key-based columnar diff producing `(enter: Vec<usize>, update: Vec<(prev_idx, new_idx)>, exit: Vec<Value>)` for identity-preserving animation.
- **`ValidationIssue`** — issue returned by composition / plot validation.

## diff.rs — semantics

- **Variant-strict.** A `Date(1)` and a `Number(1.0)` are distinct keys even though both project to f64 `1.0`. `DataColumn::key_eq_at` / `key_hash_at` handle the variant tag.
- **Deterministic.** `enter` and `update` come back in next-iteration order; `exit` in prev-iteration order. NaN canonicalises to a single hash + equality class.
- **Each prev row matches at most one next row.** Duplicate next keys: the first occurrence pairs with the matching prev row; later duplicates fall to `enter` (D3-style "keys should be unique"; degrade gracefully).
- **Positional fast path** (`diff_positional`) is used when no user key column is supplied — matches rows by position.
- **v1 ignores the triples** (geoms snap to current state). v1.5+ will interpolate along the `update` edges for animation.

## Why two "composition" modules

There's `crate::composition` (low-level layout engine — anatomy slots, hoist, `TrackOf` chrome mirroring) and `crate::plot::composition::PlotComposition` (high-level lifecycle orchestrator — scale registry, plot map, render driver). They are not duplicates:

- `crate::composition::Composition` is library-agnostic. You could use it for non-plot composition with no scales involved.
- `crate::plot::composition::PlotComposition` *owns* a `Composition` template, captured at construction. On every render it rebuilds the composition fresh from the template, wires in each plot's chrome (`plot.wire(patch, registry, dpi)`), solves, and draws. It also owns the scale registry and the plot-by-name map.

## Conventions

- **Channel resolution flows through name binding.** A geom doesn't store its scales directly — it declares channel names and asks `GeomContext` (which carries a `ScaleResolver`) to resolve each name to a `Scale` at draw time. In production the resolver is the orchestrator's binding map + registry; in tests it's a hand-built `DirectScaleResolver`.
- **Two plots sharing a scale name share the same `Scale`.** This is by design and is the only way to share axis configuration across plots.
- **`Raw(...)` bypasses scales.** Wrapping a channel value in `Raw(...)` produces `Channel::RawConstant` / `Channel::RawData`, which the per-row resolver passes through untouched. Used when a value is already in the geom's output space (panel fraction, `Color`, pt size).
- **Temporal values project to f64 before entering a continuous scale.** Dates → days, DateTimes → microseconds, etc. Tick labels reverse the projection.
- **Chrome (axes, legends, text) is feature-gated on `text`.** The orchestrator's full `wire` and the axis / legend renderers in `scale/axis.rs` and `scale/legend.rs` require `text`; `wire_panel` is always available so the panel rect still appears in the layout for `draw_panel_into`.

## Cross-references

- `composition/` — the layout engine; `PlotComposition` owns one.
- `scene/` — `Plot::draw_panel_into` / `draw_chrome_into` issue calls against a `&mut dyn SceneBuilder`.
- `primitives/` — geoms construct paths via `primitives` before drawing.
- `shape.rs` — `Plot` carries a `ShapeRegistry` for marker / endpoint glyphs.
- `text/` (gated) — chrome rendering depends on `TextRun`.
