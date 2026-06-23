# src/CLAUDE.md

Architectural rules and module map for everything under `src/`. Repo-level commands and project pitch live in the top-level `CLAUDE.md`; module-specific details live in each subfolder's `CLAUDE.md`.

## API levels

`hephaestus` exposes **two API levels** in the same crate:

- **Low level** — `scene::SceneBuilder` plus the small modules around it (`brush`, `path`, `blend`, `pick`, `geometry`, `mesh`, `stroke`, `shape`). Direct draw calls, hand-built layouts, raw access to brushes / transforms / blend modes. The plot author owns batching and styling.
- **High level** — `plot::*`. Vectorised geoms that consume columnar channel data, named scales, and a `PlotComposition` orchestrator that wires plots into the patchwork-anatomy layout in `composition`. Built **on top of** the low-level surface — same crate, no new traits, no leakage downward.

The two levels are layered, not parallel. The low-level surface must remain independently usable: anything the high level needs that doesn't exist at the low level should either be added at the low level (if generally useful) or live entirely inside the high-level module (if plot-specific). Resist letting plot/chart concepts (axes, scales, marks, "panels") creep into the low-level surface.

## Two-trait split

`SceneBuilder` (in `scene/`) and `Renderer` (in `backend/`) are split intentionally:

- `SceneBuilder` is the authoring surface. Pure CPU, infallible, no persistent "current transform / current brush" state.
- `Renderer` owns backend resources (GPU device, pipelines, readback buffer) and rasterises a built scene. Fallible, resource-owning.

This split lets recording and vector backends (SVG, PDF) implement `SceneBuilder` without satisfying GPU concerns, and mirrors Vello's own `Scene` / `Renderer` split so wrapping is zero-cost.

`Renderer` has `type Scene: SceneBuilder` as an associated type. For runtime backend selection use an enum (`AnyRenderer`-style), not `Box<dyn Renderer>` — the GAT makes object-safety awkward. Dynamic dispatch belongs at the `&mut dyn SceneBuilder` callsite.

## Intersection-of-backends rule

The public surface is the **intersection** of what Vello and Blend2D both support natively. This is what keeps the same plot code running across backends without escape hatches.

Concretely:

- `Sampling` (in `brush.rs`) exposes only `Nearest` / `Bilinear` — peniko has more.
- `BlendMode` / `Compose` / `Mix` (in `blend.rs`) are our own enums restricted to the intersection — peniko has more (e.g. `Mix::Clip`, `Compose::PlusLighter`).
- `FillRule` (in `path.rs`) is our own enum.
- No conic Beziers (kurbo's `BezPath` already excludes them).
- No stroke inside/outside alignment.
- No filter effects (blur, drop shadow, etc.).

`backend/<name>/convert.rs` is where the restriction is enforced: it maps our restricted enums to the backend's native types. When adding a new backend, the analogous `convert.rs` is the only place the mapping lives.

When tempted to add a feature only one backend supports: don't. If it's genuinely necessary, the alternative is a backend-specific extension trait — not a method on `SceneBuilder`.

## Picking model

Picking is **opt-in per renderer**, not a CPU-side post-pass. `VelloRenderer::with_picking()` enables it; `VelloRenderer::new()` allocates nothing in the pick path. When enabled, the renderer rasterises a parallel "pick scene" into a second `HeadlessTarget`, reads it back to CPU once per render, and answers point queries via `pick_at(x, y) -> Option<u32>`.

Every drawing primitive on `SceneBuilder` (`fill`, `stroke`, `draw_image`, `draw_glyphs`, `draw_mesh`) carries a `PickId`. `push_layer` / `pop_layer` do **not** take a `PickId`. Authoring code picks one of three:

- `PickId::Skip` — don't record into the hitmap. Items beneath remain hittable through this primitive. Default for decorative chrome (gridlines, axis ticks, background fills).
- `PickId::Block` — record with id 0. Occludes whatever is beneath in the hitmap but is itself reported as "no hit". Use for opaque panels that should block picks without being interactive.
- `PickId::Id(n)` — record with the given id. `n` is a 24-bit caller-managed value (typically a row / item index). Ids above `0xFF_FFFF` are truncated; `Id(0)` is treated identically to `Block`.

Encoding lives in `pick.rs` (authoritative): ids pack into the RGB channels of an `Rgba8Unorm` pick texture with alpha forced to 255, which round-trips cleanly through default SrcOver compositing without per-draw blend-mode plumbing.

**v1 limitation: alpha-insensitive picking.** Picking ignores display alpha. A semi-transparent layer or image fully occludes picks of content beneath it, even though that content remains visible in the rasterised image. Documented on `crate::pick` and `VelloRenderer::pick_at`.

Backend semantics:

- **Recording backend (`scene::recording::RecordingScene`)** stores `PickId` in each `Op` faithfully. Future SVG / PDF emitters may surface it or ignore it — both are valid.
- **Non-rasterising backends** are free to ignore `pick_id` entirely. The trait parameter is unconditional; its effect is backend-defined.

## Core types — wrapping kurbo + peniko

Geometry (`Affine`, `Point`, `Rect`, `Size`, `Vec2`, `BezPath`) comes from kurbo. Brushes, gradients, colors, image data, fonts come from peniko / linebender_resource_handle. We re-export them through our own module paths (`hephaestus::geometry::Affine`, not `kurbo::Affine`) so a future swap is a single-line change.

Where the intersection is narrower than peniko's full surface, we define our own enum (see "intersection rule" above). Otherwise re-export directly — reimplementing affine math or gradient interpolation is not in scope.

## Module map

Folders (each with its own CLAUDE.md):

- `scene/` — `SceneBuilder` trait, glyph types, recording backend.
- `backend/` — `Renderer` trait, error type, and backend implementations.
- `layout/` — grid layout solver. Recursive grids, fr / auto tracks, `respect()`, `Measure` protocol.
- `composition/` — patchwork-style plot composition. 13-col × 16-row anatomical grid; chrome alignment across nested compositions via `Length::TrackOf`.
- `primitives/` — compound 2D primitives: path constructors (rect / circle / wedge / polyline / polygon / arc), composable vertex transforms (clip / offset / round corners), arc-length sampling, ribbon tessellation.
- `plot/` — high-level plot API: `Plot`, `PlotComposition` orchestrator, key-based diff for identity-preserving animation. Geoms in `plot/geom/`; axis / legend rendering in `plot/chrome/`. Scales and values themselves live in [`crate::scales`] (see below).
- `scales/` — leaf module: `Value`, `DataColumn`, `Scale`, scale types, transforms, break / tick algorithms. Backend-agnostic and plot-agnostic; nothing inside imports from `src/plot/`, `src/scene/`, etc. Intended to be lifted into its own crate once the API settles. Hephaestus's `plot/scale.rs` and `plot/value.rs` are thin re-export shims that preserve the historical `hephaestus::plot::scale::*` / `hephaestus::plot::value::*` paths.
- `text/` — parley-backed text shaping and layout. Gated on the `text` feature. A host crate may swap in its own shaper behind the `TextRun` / `draw_text` surface, but the parley path is the committed default.

Single-file modules (no CLAUDE.md, one-line descriptions here):

- `blend.rs` — `BlendMode` / `Compose` / `Mix` enums (intersection of Vello + Blend2D).
- `brush.rs` — `Brush`, `Image`, `Sampling` (Nearest / Bilinear).
- `color.rs` — re-exports peniko `Color`.
- `geometry.rs` — re-exports kurbo `Affine`, `Point`, `Rect`, `Size`, `Vec2`.
- `mesh.rs` — `Mesh`: flat 2D triangle list with per-vertex colour. Used by `primitives::ribbon` and consumed by `SceneBuilder::draw_mesh`.
- `path.rs` — `Path` (kurbo `BezPath` wrapper) and `FillRule` (intersection enum).
- `pick.rs` — `PickId` and the authoritative encoding into `Rgba8Unorm` RGB.
- `png.rs` — gated PNG writer (`png` feature).
- `shape.rs` — `Shape` / `ShapeRegistry` / `ShapeStyle`: named glyphs / paths for scatterplot markers and line endpoint terminators.
- `stroke.rs` — re-exports kurbo `Stroke`, `Cap`, `Join`. Stroke alignment and variable-width strokes are not in scope.
