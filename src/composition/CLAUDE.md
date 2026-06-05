# src/composition/CLAUDE.md

Patchwork-style plot composition. Stacks on top of `layout/`: every plot is the same anatomical grid, composed plots automatically align by anatomical position.

## What this module does

Every plot is laid out into a **shared 13 columns × 16 rows anatomical grid** (`anatomy::TABLE_COLS` × `anatomy::TABLE_ROWS`). Named [`Slot`]s (Panel, AxisTop, Title, LegendLeft, etc.) drop content into fixed positions inside that grid. Plots compose into hierarchical compositions via [`beside`] / [`stack`] / [`grid`] (or `Composition::place` directly). Nested compositions automatically align by anatomical position through the unified hoist + `Length::TrackOf` chrome-mirroring mechanism documented below.

Construction is id-addressed: every [`Patch`] is created with a string id, and resolved rects are looked up via `CompositionLayout::get(id, region)` — flat across any nesting depth.

## Core types

- **`Patch`** — a single plot's content in the 13×16 anatomy. Build with `Patch::new(id)`, drop content into named slots with `Patch::slot(Slot::Panel, cell)`, or into raw positions with `Patch::place_at(region, row, col, span, cell)`. Lock the panel to an aspect ratio with `Patch::aspect(w, h)`. Configure outer `Patch::margin(inset)` and inner `Patch::padding(inset)`.
- **`Composition`** — a `rows × cols` grid of [`Element`]s. Build with `Composition::empty(rows, cols)`, place elements with `Composition::place(row, col, span, element)`. Optional composition-level chrome via `Composition::slot(Slot::Title, cell)` — wraps the facets in a canonical 13×16 anatomical block so chrome (title, axis titles, caption) spans across all facets. Mirrors patchwork's `plot_annotation()`.
- **`Element`** — `Patch(Patch)` or `Composition(Composition)`. `impl From<Patch>` and `impl From<Composition>`, so callers pass either kind transparently.
- **`Slot`** — 21 named anatomical positions: Panel, Background, AxisTop / Bottom / Left / Right, AxisTopTitle / etc., StripTop / etc., LegendTop / Bottom / Left / Right, Title, Subtitle, Caption. Each has a fixed `(row, col, row_span, col_span)`; see `Slot::placement`. The mapping is total — for positions outside this fixed anatomy use `Patch::place_at`.
- **`Span`** — `Span::cell()` (1×1), `Span::rows(n)`, `Span::cols(n)`, `Span::rc(r, c)`. Used by `Patch::place_at` and `Composition::place`.
- **`CompositionLayout`** — resolved rects. Query with `layout.get(patch_id, slot.name())` or `layout.get(patch_id, "custom_region")` for `place_at` regions.
- **`CompositionError`** — solve-time error (e.g. duplicate patch ids).

Module-level constants: `TABLE_COLS = 13`, `TABLE_ROWS = 16`, plus the anatomical landmark constants (`PANEL_ROW = 9`, `PANEL_COL = 7`, `PLOT_LEFT / RIGHT / TOP / BOTTOM`, `MARGIN_*`, `PADDING_*`).

## Anatomy

13 columns × 16 rows, symmetric in all four directions through the legend ring. The horizontal cross-section:

```
margin | padding | legend | strip | axis-title | axis | PANEL | axis | axis-title | strip | legend | padding | margin
```

Beyond the legend, top / bottom carry title / subtitle / caption; left / right carry nothing additional.

- **Outermost tracks** (row 1, row 16, col 1, col 13): *margin*. The [`Slot::Background`] does **not** extend into these tracks — they are the gap between adjacent patches' backgrounds when patches are composed side by side. Two adjacent patches' backgrounds are separated by `margin_a + margin_b` of empty composition space.
- **Second-from-outermost tracks** (row 2, row 15, col 2, col 12): *padding*. Sits inside the background; chrome (title, legends, axes) sits inside the padding. Padding is the breathing room between the background's edge and the start of chrome.

All slot positions are 1-indexed to match `Placement`.

## Unified hoist + `Length::TrackOf` chrome mirroring

When a composition is nested inside another composition's cell, the build pipeline emits **forward sizers** in the outer block pointing at the sub-grid's inner border-block chrome tracks via `Length::TrackOf(sub_id, axis, track, span)`. Simultaneously, **back sizers** in the sub-grid point at the parent's outer chrome tracks. The layout solver iterates over `TrackOf` references using fixed-point arithmetic (see `layout/CLAUDE.md` on iteration), converging the Auto tracks at both sides of the boundary to their pointwise maximum in two or three iterations per nesting level.

The effect: inner-composition chrome (axis titles on facet borders, etc.) couples to outer-composition canonical chrome positions, enabling shared Title rows / cols across nested compositions without the caller threading the alignment by hand.

## Conventions

- **Patch ids must be unique across the entire reachable element tree.** Duplicate ids return `CompositionError` from `try_solve` (and panic from `solve`).
- **`Composition::aspect` cascades** down to immediate children without their own aspect; a child with its own aspect blocks further propagation.
- **Panel cells span across all spanned outer blocks**; chrome cells anchor to the start block (left / top) or end block (right / bottom) of the span, enabling asymmetric multi-block spans.
- **`Slot::Background` excludes the margin tracks.** Background spans padding + chrome area (rows 2–15, cols 2–12). The margin tracks are composition-level glue between patches, not part of a plot's background.
- **`Composition::slot` panics on `Slot::Panel`** — the composition's facets fill the panel cell; there is no "panel" at the composition level to populate.
- **`Patch::place_at` and `Composition::place_at` are escape hatches** for content that doesn't fit a named slot. The region string becomes the lookup key in `CompositionLayout::get`.
- **Anonymous patches** (via `spacer()`) have no id and aren't addressable in `CompositionLayout::get`. They exist to fill grid cells that should be empty.
- **`wrap(id, cell)` is shorthand** for `Patch::new(id).slot(Slot::Panel, cell)` — the most common pattern.

## Cross-references

- `layout/` — the underlying solver. `Length::TrackOf` (defined in `layout/`) is the mechanism that makes chrome mirroring work across nesting. The fixed-point iteration loop in `layout/solver.rs` is what converges them.
- `plot::composition` (`src/plot/composition.rs`) — the high-level **orchestrator** that owns a `Composition` template, the scale registry, and attached plots. It rebuilds the composition on every render with each plot's chrome wired into named slots. See `src/plot/CLAUDE.md` for the "why are there two composition modules" answer.
- `text/` — `TextRun` implements `Measure` and drops directly into a `Patch::slot(Slot::Title, Cell::measured(run))`.
