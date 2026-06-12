# src/layout/CLAUDE.md

Grid-only, zero-dependency layout solver. Compose n×m grids recursively, solve to a flat `HashMap<CellId, Rect>`.

## What this module does

The public API covers: n × m grids, content leaves with an intrinsic-size protocol, recursive nesting with row / column placement and spans, per-edge insets in physical or relative units, the `respect` flag (from R grid's `grid.layout(respect = TRUE)`) for cross-axis fr coupling, opt-in iteration for content whose width depends on its height, and `Length::TrackOf` cross-grid references (used by `composition/` to mirror chrome tracks across nested compositions).

Layout is unconditional — built into every cargo profile, no feature flag.

## Core types

- **`Grid`** — n × m grid. Built with `Grid::new(widths, heights)`. Place children with `Grid::place(row, col, span_rows, span_cols, child)` where the child is `impl Into<Node>` (either a `Cell` or another `Grid`). `Grid::cell()` is shorthand for `Cell::empty()`. `Grid::respect()` enables cross-axis fr coupling.
- **`Track`** — per-row / per-column sizing. `Track::Fr(f64)` (fraction unit), `Track::Auto` (size to content), or `Track::Fixed(Length)`.
- **`Length`** — `Sum { px, inches, percent }` (the common linear combination), plus `Min(Box, Box)` / `Max(Box, Box)` for deferred pointwise resolution, plus `TrackOf { grid, axis, track, span }` for cross-grid references that resolve at solve time.
- **`Inset`** — per-edge inset within a cell (left / right / top / bottom). Reference frame: the parent grid cell area (the union of tracks the child spans). "1 cm left + 25% right" means "1 cm from the cell's left, 75% of the cell width."
- **`Cell`** — a leaf. `Cell::empty()` is a zero-measure leaf; `Cell::measured(impl Measure + 'static)` carries arbitrary content (text shaper, chart, image).
- **`Node`** — internal enum: either a `GridNode` or a `Cell`. `Grid::place` accepts `impl Into<Node>` so callers pass either kind transparently.
- **`Placement`** — `Placement::at(row, col).span(rs, cs)`. 1-indexed; placements outside the grid clamp to a zero-area rect rather than panicking.
- **`Measure` trait** — leaf protocol: `width_hint(dpi) -> WidthHint`, `height_at(width, dpi) -> f64`, optional `width_at(height, dpi) -> f64` for iteration.
- **`WidthHint`** — `Min { seed }` or `NeedsHeight { seed }`. The latter opts the cell into the iteration loop.
- **`Layout`** — solve result: `HashMap<CellId, Rect>` plus per-grid track metadata.
- **`CellId`** — opaque id assigned to every tagged grid / cell, used as the layout key.
- **`Axis`** — `Width` / `Height`. Selects which axis of a target grid a `Length::TrackOf` references.

## Solver — width-major two-pass

1. **Pass 1** walks the tree resolving every column track. Auto columns use child `min_width` recursively, with `WidthHint::Min` / `seeds[path]` for leaves. Records per-grid `(col_sizes, x_range)` in side tables keyed by `Vec<usize>` tree path.
2. **Pass 2** walks the tree again with all widths known, resolving rows. Auto rows query each child with `height_at(child_width)`.
3. After the two passes, a separate `emit_rects` walk produces the flat `HashMap<CellId, Rect>`.

`Track::Auto` resolves in pass 1 for cols (from `min_width` queries: cells use `WidthHint::Min { seed }`; grids recurse) and in pass 2 for rows (from `height_at(known_width)` queries). Pass 1 skips multi-span children entirely (the v1 deliberate simplification); **pass 2 does not** — because widths are known when row Autos resolve, a multi-span child contributes to its (single) Auto row using `height_at(sum_of_spanned_widths)`.

`respect()` implements a variant of R grid's `allocateRespected`: per-fr-w and per-fr-h are clamped to the smaller, with each axis's per-fr divided into `total_fr = respected + unrespected` (rather than just the respected portion) so unrespected sibling tracks retain their share before the aspect lock claims the rest of the axis. A 2×2 grid with one square-locked cell and three flex cells therefore resolves cleanly — the locked cell gets the min-axis fr share, the flex siblings absorb the leftover on each axis. Width-pass uses a *provisional* per-fr-h that treats auto rows as 0 (their content-driven contribution isn't known yet); pass 2 re-clamps using the actual per-fr-h. With no Auto rows the provisional matches the actual exactly. With Auto rows that grow from content, the grid is allowed to **exceed** respect's prediction in height — content height wins; respect becomes a best-effort coupling, not an inviolable invariant.

## Iteration

Iteration kicks in when any cell returns `WidthHint::NeedsHeight { seed }` **or** when any `Length::TrackOf` reference appears in the tree.

The solver loops the width + height passes up to `MAX_ITER = 5` times. Between iterations it queries each iterative cell's `width_at(resolved_height)` and damps with factor `DAMPING = 0.5` (`new = 0.5·proposed + 0.5·prev`) to kill the rotated-wrap 2-cycle. Convergence (`|new - old| < EPSILON = 0.5 px` per path) breaks the loop early.

`Length::TrackOf` references resolve to 0 on iteration 0; on later iterations they pick up the resolved track size from the previous iteration. Forward references converge in 1–2 iterations; cycles will not converge and exhaust `MAX_ITER`.

The cap is a **safety valve, not a correctness guarantee** — rotated wrapped text genuinely oscillates and the solver accepts the last damped state. Iteration scope is the whole tree (re-solve from root); parent-scoped iteration is a follow-up if needed.

## Conventions

- **`Length`, `Track`, `Inset` are `Clone` but not `Copy`** because of the `Box`es in `Min`/`Max`. The solver pattern-matches by reference and uses `Option::as_ref()` on `Inset` fields.
- **Constructors `px` / `mm` / `cm` / `inch` / `pt` / `percent`** produce a `Sum`. `Length::min` / `Length::max` produce `Min` / `Max`. `Length::track_of` / `Length::tracks_of` produce `TrackOf`.
- **Arithmetic** (`+`, `-`, unary `-`, `*f64`, `/f64`) reduces field-wise on `Sum + Sum`; addition **distributes** through `Min` / `Max` exactly (`min(a, b) + c = min(a+c, b+c)`); negation and negative-scalar multiplication flip `Min` ↔ `Max`. `TrackOf` is opaque to arithmetic — it composes with `Min` / `Max` transparently but `+ / - / *` on a tree containing `TrackOf` panics. Use `tracks_of(..., span)` to express a multi-track sum.
- **Resolution helpers** — `length_to_px(&Length, dpi, axis)` walks the tree; `length_to_px_abs(&Length, dpi)` treats the percent term as 0 (used for intrinsic-min computation).
- **1-indexed placements clamp on out-of-range.** A row 99 in a 5-row grid resolves to a zero-area rect, not a panic.
- **No external dependencies.** Cross-axis `respect`, the iteration loop, and `TrackOf` references don't fit CSS Grid's independent-axis fr distribution; once fr resolution is hand-rolled, the rest is small enough that adding a dep costs more than it saves. **Don't add taffy or another layout engine.**
- **Grid-only public API.** Don't add flex / flow / float concepts — same intersection-rule discipline that governs the scene API. If a layout shape doesn't fit a grid, it probably belongs in `composition/` (which composes grids) rather than as a new primitive here.

## Cross-references

- `composition/` — builds on top of `layout`. The patchwork chrome-mirroring mechanism is exactly `Length::TrackOf` references between nested `Grid`s.
- `text/` — `TextRun` implements `Measure`, so a shaped string drops into a `Cell::measured` and participates in Auto sizing and iteration.
