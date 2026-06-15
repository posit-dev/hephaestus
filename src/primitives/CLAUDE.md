# src/primitives/CLAUDE.md

Compound 2D primitives — path constructors, composable vertex transforms, arc-length sampling, ribbon tessellation. Sits at the boundary between the low-level scene API and the high-level plot geoms.

## What this module does

Every constructor takes geometric inputs and returns a `Path` (or `Mesh`, for ribbons). **Drawing is the caller's responsibility** — the caller issues `SceneBuilder::fill` / `stroke` / `draw_mesh` with the returned path or mesh, supplying their own brush, transform, and `PickId`. Same pattern as `shape.rs`.

Geometry transforms (`round_corners`, `clip_polyline`, `offset_polygon`, `round_path_corners`) are separate, composable functions — the constructors don't bake them in.

## Path-emitting constructors

- **`polyline`** — open polyline from a point list, with optional end trimming via `EndClip`.
- **`polygon`** — closed polygon from one outer ring plus zero or more holes, with optional signed offset.
- **`rect`**, **`rounded_rect`**, **`circle`**, **`ellipse`** — thin wrappers over the equivalent `kurbo` shapes that hide the path-approximation tolerance and the `kurbo::Shape` import.
- **`segment`** — 2-point line shorthand.
- **`regular_polygon`** — n-sided regular polygon. Use **`regular_polygon_vertices`** for the raw vertices when you want to feed them into another transform.
- **`arc`** — open circular arc.
- **`wedge`**, **`annular_wedge`** — closed pie / donut slices.

## Vertex transforms

- **`clip_polyline`** — trim a polyline's start / end against a shape (circle / ellipse / rect). Returns `Vec<Point>`. Compose by feeding the output into `polyline`.
- **`offset_polygon`** — inflate / deflate a polygon's rings via Clipper2 (the `clipper2-rust` dep). Returns `Vec<Vec<Point>>` — a single ring can split into multiple (a dumbbell inset) or collapse entirely.
- **`path_to_rings`** — flatten any `Path` (lines + quads + cubics) into piecewise-linear ring vertices. Lets you pipe curved primitives (`wedge`, `circle`, etc.) through `offset_polygon`.
- **`round_corners`** — Chaikin-style adaptive corner cutting on a vertex sequence. Returns a `Path` with one cubic Bezier per rounded corner; the cubic's control points are placed along the **local segment tangents** at each cut point. **Piecewise-linear input only.**
- **`round_path_corners`** — the curve-aware variant: takes any `Path` and replaces each eligible join with a cubic Bezier fillet whose endpoint tangents match the original segments. Use this for `wedge`, `annular_wedge`, or any path whose edges include quads / cubics.

## Path sampling

- **`ArcLengthWalker`** — yields position + tangent samples at fixed arc-length intervals along a path. Treats each subpath independently; zero-length segments fall back to the last valid tangent. Used by `LineGeom` for linetype marker placement and (planned) text-on-path geoms.
- **`PolylineSampler`** — same protocol over a polyline vertex list.
- **`ArcSample`**, **`TrailingPolicy`** — sample type and the policy for handling the trailing partial interval.

## Ribbon tessellation

`ribbon.rs` emits `Mesh` (not `Path`) for stroked polylines and polygons with per-vertex colour (Gouraud shading). Composed with `SceneBuilder::draw_mesh`, not fill / stroke.

- **`polyline_ribbon`**, **`polyline_ribbon_full`** — open ribbons.
- **`polygon_ribbon`**, **`polygon_ribbon_full`** — closed ribbons.
- **`polyline_gradient`**, **`polygon_gradient`** — colour-along helpers.
- **`ribbon_band_mesh`** — quad-strip mesh between two co-indexed polylines (curve A and curve B). Used by `RibbonGeom`'s mesh path for free-form bands and for axis-aligned bands under non-linear projections, where a screen-aligned linear-gradient brush would misrepresent the sweep.
- **`RibbonOptions`** — configuration. Reuses [`crate::stroke::Cap`] / [`crate::stroke::Join`] (same three variants each — no need for ribbon-specific enums).

If every vertex would share the same colour, a plain `stroke` / `fill` with a solid brush is cheaper than a ribbon mesh — the per-vertex colour is the whole point.

## Conventions

- **All distances are in path coordinates** (typically panel pixels after pt → px conversion at the call site).
- **Composable by design**: clip a polyline, round its corners, or offset a polygon then round per-ring independently. The vertex transforms return raw data so the caller can pipe outputs together before handing the final result to a path constructor.
- **`round_corners` vs `round_path_corners`** — pick `round_corners` only for piecewise-linear inputs (polyline outputs). For Bezier-containing paths (wedge, annular_wedge, circle), `round_path_corners` is the only correct choice.
- **`offset_polygon` can return zero rings.** Check the length of the result before using it. An inset that exceeds half the polygon's width collapses it entirely.

## Cross-references

- `plot/geom/` — every plot geom that draws shapes (line, polygon, wedge, rect, ellipse) uses `primitives` to construct paths before handing them to `SceneBuilder`. `LineGeom` specifically uses `ArcLengthWalker` for linetype marker placement.
- `mesh.rs` — the `Mesh` type ribbons emit.
- `shape.rs` — same calling pattern (constructs data; caller draws).
