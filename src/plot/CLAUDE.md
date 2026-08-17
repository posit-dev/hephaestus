# src/plot/CLAUDE.md

The high-level plot API: typed columnar data, named scales, geoms that consume them, and the `PlotComposition` orchestrator that wires the whole thing into the layout from `composition/`. Layered on top of the low-level `SceneBuilder` surface.

## What this module does

A **plot** is a per-patch unit of state that binds channel names (`"x"`, `"y"`, `"color"`, etc.) to scale names and holds a list of geoms. Multiple plots share a single `ScaleRegistry`, so two plots that bind their `"x"` to `"time"` use the same configured scale — change the domain once, both update.

The canonical user-facing surface is **`PlotComposition`** (in `composition.rs`). It owns the layout shape, the scale registry, and a `HashMap<String, Plot>` of attached plots. `view.render(scene, size, dpi)` is the single entry point: the orchestrator rebuilds the composition fresh on every render with each plot's chrome wired into anatomical slots, solves the layout, and drives `draw_chrome_into` + `draw_panel_into` per plot.

For tests and one-off renders, `Plot` is independently usable with a hand-built `ScaleRegistry` and a manually constructed `Composition`.

## Subdirectories

- **`geom/`** — vectorised drawing primitives (`PointGeom`, `LineGeom`, `PolygonGeom`, `RectGeom`, `EllipseGeom`, `SegmentGeom`, `WedgeGeom`, plus `TextGeom` / `TextFitGeom` / `TextPathGeom`). See `src/plot/geom/CLAUDE.md`.
- **`chrome/`** — axis and legend rendering. The scale layer (in `crate::scales`) defines what to draw; this module draws it against `SceneBuilder`.

## Scale bundle and re-export shims

- **`scale/`** — hephaestus's own `Scale` bundle, `ScaleRegistry`, and the ggplot-style constructors (`scale::continuous(...)`, `scale::binned(...)`, …) in `scale/constructors.rs`. Hephaestus-only: none of it ships in the lift-ready scales crate. The module also re-exports `crate::scales::*`, so `crate::plot::scale::*` resolves both the bundle and the underlying enums / free functions.
  - **Reversal is one flag, uniform across families.** `with_direction(Direction::Reversed)` / `set_direction` mirror the normalised fraction (or the domain index) before the output range is consulted, so the same flag flips a position axis, a palette and a colorbar's ramp. It is not expressed as a backwards domain — see the `Direction` convention in `src/scales/CLAUDE.md` for why, and for what it means for breaks, band offsets and legend order.
  - **Break control is symmetric across majors and minors.** `BreaksSpec` (`with_breaks` / `with_breaks_labeled` / `with_interval` / `with_temporal_interval`) pins majors; `MinorBreaksSpec` (`with_minor_breaks` / `with_minor_count` / `with_minor_interval` / `with_minor_temporal_interval`) pins minors. The two are independent — pinned majors still get automatic minors and vice versa — and a spec whose variant doesn't fit the scale type falls back to the automatic algorithm rather than panicking. Automatic minors do follow a pinned major *interval* where the majors define what there is to subdivide: on a temporal scale, `with_temporal_interval` drives the minors through `derive_minor_interval`, so quarterly majors get monthly minors regardless of the tick target. Interval-derived minors skip positions that already carry a major, matching what the automatic algorithms do (log minors skip the decade powers, calendar minors skip the aligned major).
- **`value.rs`** — pure re-export of `crate::scales::value::*`.

The algorithms `Scale` delegates to live in [`crate::scales`]; see `src/scales/CLAUDE.md`.

## Core types (this folder, not in subdirectories)

- **`PlotComposition`** (`composition.rs`) — the orchestrator. Construct with `PlotComposition::new(&composition)`; register scales with `add_scale("name", scale)`; attach plots with `with_plot(plot)` / `attach_plot(plot)`. Composition-level chrome — a title / subtitle / caption, shared axis titles, and legends that serve every facet — is set directly on it (`title`, `axis_title`, `add_legend`), mirroring the same methods on `Plot`. Mutations flow through closures (`view.update_scale("time", |s| ...)`, `view.update_plot("price", |p| ...)`) so dirty-tracking stays accurate. The dirty model is conservative: any mutation flips `layout_dirty` and the next `render` re-solves. Per-plot / per-scale dirty bits are plumbed but only used by v1.5+ partial-repaint heuristics.
- **`Plot`** (`plot.rs`) — bound to a patch id. Stores channel → scale-name bindings, geom list (`Vec<(GeomId, Box<dyn Geom>)>`), chrome text (title / subtitle / caption / axis titles), and a `ShapeRegistry`. Three lifecycle methods used by the orchestrator: `wire(patch, registry, dpi)` (drop chrome cells + panel into named slots; full version is `text`-gated, `wire_panel` is always available), `draw_chrome_into(scene, layout)`, `draw_panel_into(scene, layout, registry)`.
- **`GeomId`** — opaque handle returned by `Plot::add_geom`; used with `Plot::update_geom` / `remove_geom`.
- **`KeyIndex`** / **`diff_columns`** / **`diff_positional`** (`diff.rs`) — key-based columnar diff producing `(enter: Vec<usize>, update: Vec<(prev_idx, new_idx)>, exit: Vec<Value>)` for identity-preserving animation.
- **`ValidationIssue`** — issue returned by composition / plot validation.

(`Value` / `DataColumn` / `Date` / `DateTime` / `Time` / `Duration` / `LinetypeStep` live in `crate::scales::value` — moved alongside `Scale` since they're the data the scales operate on.)

## diff.rs — semantics

- **Variant-strict.** A `Date(1)` and a `Number(1.0)` are distinct keys even though both project to f64 `1.0`. `DataColumn::key_eq_at` / `key_hash_at` handle the variant tag.
- **Deterministic.** `enter` and `update` come back in next-iteration order; `exit` in prev-iteration order. NaN canonicalises to a single hash + equality class.
- **Each prev row matches at most one next row.** Duplicate next keys: the first occurrence pairs with the matching prev row; later duplicates fall to `enter` (D3-style "keys should be unique"; degrade gracefully).
- **Positional fast path** (`diff_positional`) is used when no user key column is supplied — matches rows by position.
- **Opt-in.** `Plot::track_identity(true)` turns the per-draw rebuild on; it's off by default because nothing in the draw path consumes the triples yet (geoms snap to the current state) and building them costs a hash index plus a key-column snapshot per geom per frame. Animation will interpolate along the `update` edges.

## Why two "composition" modules

There's `crate::composition` (low-level layout engine — anatomy slots, hoist, `TrackOf` chrome mirroring) and `crate::plot::composition::PlotComposition` (high-level lifecycle orchestrator — scale registry, plot map, render driver). They are not duplicates:

- `crate::composition::Composition` is library-agnostic. You could use it for non-plot composition with no scales involved.
- `crate::plot::composition::PlotComposition` *owns* a `Composition` template, captured at construction. On every render it rebuilds the composition fresh from the template, wires in each plot's chrome (`plot.wire(patch, registry, dpi)`), solves, and draws. It also owns the scale registry and the plot-by-name map.

## Composition-level chrome

Chrome belongs to whichever thing owns the space it sits in: a patch's title goes on `Plot`, a title spanning a whole facet grid goes on `PlotComposition`. Nothing is collected or inferred across plots — a legend serving every facet is one legend attached to the composition, not a merge of the per-plot ones.

- **Storage is keyed by composition id.** `PlotComposition` holds `HashMap<String, CompositionChrome>`; the root's entry is keyed on `root_id`, which is the caller's `Composition::id` when they set one and `ROOT_COMPOSITION_ID` otherwise. The id matters because composition chrome rects resolve as `(composition_id, region)` in `CompositionLayout::get` — `BuildState::register_region` only records a region when the composition is named.
- **The public methods target the root.** `title` / `subtitle` / `caption` / `set_title` / `clear_title` / `axis_title` / `set_axis_title` / `add_legend` / `add_legend_separate` / `legends` / `clear_legends`, plus `shape_registry` for the registry backing composition legend key glyphs. A named nested composition already gets its chrome wired and drawn by the same path; only a public entry point that addresses it by id is missing (see `PLAN.md`).
- **Wiring mirrors the per-plot path exactly.** `CompositionChrome::wire` drops cells into `Composition::slot` / `place_at` using the same helpers `Plot::wire` uses (`title_band_placement`, `text_cell_for_element`, `axis_title_cell`, `legend_stack_measure`), so `theme.plot_text_align_to` and every theme text field behave identically at both levels.
- **Composition chrome draws last** (render phase 5). It shares canonical rows with the border facets' own chrome — see `build_wrapped_composition` — so the composition's wider rect paints over the band the narrower per-facet chrome sits in.
- **Two things a composition can't have.** `LegendSide::InPanel` panics: an in-panel legend anchors to a panel rect, and the composition's panel cell is filled by its facets. And `TitleLocation::Inside` is ignored for composition axis titles for the same reason — they always take the outer slot.
- **Chrome `Cell`s set on the composition before construction are dropped**, exactly as pre-attached patch chrome is. A raw `Cell` reserves space but carries nothing the orchestrator could draw, so keeping it would produce reserved-but-blank bands. `examples/nesting_faceted_title.rs` shows the low-level path, where the caller solves and draws the cells themselves.

## Conventions

- **Channel resolution flows through name binding.** A geom doesn't store its scales directly — it declares channel names and asks `GeomContext` (which carries a `ScaleResolver`) to resolve each name to a `Scale` at draw time. In production the resolver is the orchestrator's binding map + registry; in tests it's a hand-built `DirectScaleResolver`.
- **Two plots sharing a scale name share the same `Scale`.** This is by design and is the only way to share axis configuration across plots.
- **`Raw(...)` bypasses scales.** Wrapping a channel value in `Raw(...)` produces `Channel::RawConstant` / `Channel::RawData`, which the per-row resolver passes through untouched. Used when a value is already in the geom's output space (panel fraction, `Color`, pt size).
- **Temporal values project to f64 before entering a continuous scale.** Dates → days, DateTimes → microseconds, etc. Tick labels reverse the projection.
- **Chrome text is measured at the width it will be drawn at.** A measure that lays its run out unconstrained while the draw pass wraps reserves one line for an N-line block, and the surplus lines get clipped. Text that wraps at draw time either goes through `Cell::measured(run)` (the solver re-breaks via `Measure::height_at`) or re-breaks inside its own `Measure` at the same width, minus the same padding and text margin, that the draw pass uses — `chrome::strip::StripMeasure` is the worked example, including `WidthHint::NeedsHeight` for the rotated case where the thickness depends on the strip's length. Text that draws on one line (tick labels, rotated axis titles) anchors on `TextRun::content_width`, never on `Measure::width_hint` — the hint is the longest unbreakable cluster, a wrap lower bound that undershoots any label containing a space.
- **Rotated text wraps against the edge it runs along.** `plot::rotated_wrap_width` projects the target rect onto the text's advance direction, so a quarter-turned block breaks at the rect's height rather than its (narrow) screen width.
- **Legend collapse runs per render, not on attach.** `add_legend` / `add_legend_separate` (on both `Plot` and `PlotComposition`) only store the legend and hand back an id; `chrome::legend::collapse_legends` folds compatible legends together inside `wire` and `draw_chrome_into`, which must call it with the same registry + locale so measured space matches what's drawn. Compatibility compares the *scales* the two legends resolve to (`Scale::legend_equivalent_to`: same family, transform, domain, breaks and labels) rather than the scale names, so two separately configured scales trained to the same values share one legend. Colorbars collapse under a stricter condition: since the surviving bar has to stand in for both, the bodies must agree on step mode, sample count and every aesthetic binding, and the scales behind those bindings must be `Scale::visual_equivalent_to` (legend-equivalent *plus* the same output range). `add_legend_separate` sets `Legend::merge = false` to opt a legend out of being folded into an earlier one.

## Cross-references

- `composition/` — the layout engine; `PlotComposition` owns one.
- `scene/` — `Plot::draw_panel_into` / `draw_chrome_into` issue calls against a `&mut dyn SceneBuilder`.
- `primitives/` — geoms construct paths via `primitives` before drawing.
- `shape.rs` — `Plot` carries a `ShapeRegistry` for marker / endpoint glyphs.
- `text/` (gated) — chrome rendering depends on `TextRun`.
