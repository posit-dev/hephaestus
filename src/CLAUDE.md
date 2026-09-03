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
- `Renderer` owns backend resources (GPU device, pipelines, readback buffer) and rasterises a built scene. Fallible, resource-owning. Its `Scene` is a `PickIndexScene`, so hit testing rides on the scene rather than on the backend.

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

## Thread-safety: `Scale` is shareable, `Plot` is not

`Scale` and `ScaleRegistry` are `Send + Sync` — `LabelFormatter` carries a `Send + Sync` bound and the generation counter is a plain `u64`. A registry can be built on a worker thread and shared.

`Plot` and `PlotComposition` are deliberately **neither**. Geoms memoize shaped text behind `RefCell` (`RichShapeCache`, and the per-run break memo inside `RichTextRun`), which is what keeps interactive redraws off the shaper. Making them `Send` would mean putting locks on that path to buy thread mobility nothing in the render model asks for — rendering is single-threaded by design, and the host owns presentation. Build plots on the thread that draws them; share configuration through the registry.

## Picking model

Picking is a **CPU-side spatial index built while a scene is drawn**, not a second rasterisation. `PickIndexScene<S>` wraps any `SceneBuilder`, forwards every call unchanged, and records each primitive's geometry as it goes past. Nothing is read back from a GPU, and the answer always describes the frame on screen rather than lagging it.

It is a property of the **scene**, not of a backend: every renderer's `Renderer::Scene` is a `PickIndexScene`, so a vector backend or a build with no renderer at all hit-tests the same way a GPU one does. `with_picking()` turns indexing on; `new()` leaves it off, because filling the index costs CPU per draw call whether or not anything is queried.

### The two things a primitive carries

**A `PickId`** — the authoring layer's handle for what a call draws. Every drawing primitive on `SceneBuilder` (`fill`, `stroke`, `draw_image`, `draw_glyphs`, `draw_mesh`) takes one; `push_layer` / `pop_layer` do not.

- `PickId::Skip` — no authoring id. See the indexing rule below for whether it is recorded.
- `PickId::Block` — occlude without reporting. A point query stops here and returns nothing, so an opaque panel can hide what is under it without being interactive. Region queries are unaffected: a marquee is a spatial query, not a ray.
- `PickId::Id(n)` — the given id, across the **full `u32` range** with nothing reserved. Occlusion is `Block`, a variant rather than a magic value; `0` was special only while ids were packed into a texture. The caller owns the namespace; nothing allocates ids.

**A `PickScope` stack** — the logical tree the drawing sits in, pushed and popped like a layer but with no visual effect and no clip. The stack in effect at a draw is that primitive's ancestor chain, so **the scope stack is the bubble path**. This is what makes chrome pickable: chrome has no id of its own, and carving a range out of a namespace the caller owns was never safe.

### The indexing rule

A primitive is recorded when:

```
pick_id != Skip   OR   the innermost scope's mode is Target
```

`ScopeMode::Group` is the default, so the safe behaviour is what you get by omission: a dense geom with no `pick_id` channel emits `Skip`, sits only in `Group` frames, and is **not recorded at all** — no entry, no leaf box, nothing to test. `ScopeMode::Target` is emitted only by `plot::pick`'s `part_scope` / `item_scope`, so chrome opts in structurally rather than at ~90 individual call sites. Measured on a two-plot composition: 500 marks without a `pick_id` channel contribute 0 entries; the 26 that exist are all chrome.

### Layering

`crate::pick` is chart-agnostic — a `PickScope` carries a `&'static str` kind plus an optional name and index, and nothing down there knows what an axis is. The vocabulary lives in `plot::pick` (`PlotPart`, the scope constructors, the typed `PlotPath` view), the same split `composition` already uses between `Slot::name` and the `Region` trait. The grammar, with only `region` and `part` always present:

```
composition → plot? → region(Slot) → [axis|legend|geom]? → part(PlotPart) → item(u32)?
```

`plot` is absent for composition-level chrome, so a hit on the figure title reports `PlotPath::plot() == None`.

### Queries

`PickIndex::hits_at(p)` returns every hit, topmost first, each carrying its scope chain — all-hits costs the same as topmost-only, since the tree descent is the cost and refinement is noise. `hits_in` / `hits_within` are rubber-band brushing (`hits_within` is the exact one: bounds inside a rect implies geometry inside it); `hits_in_path` is a lasso, centre-based because for an arbitrary polygon bbox containment implies nothing.

A renderer exposes **one** pick method, `pick_index()`. Its part in hit testing is owning the scene that recorded the index, so every query lives on `PickIndex` rather than being forwarded through two more layers of the same names.

### What it costs

Filling the index during a draw, and building the R-tree lazily on the first query after a frame. Measured at 100k marks, 900×560: **+7 ms** when marks share a marker path under a per-mark transform (what `PointGeom` does, so interning collapses the geometry to one copy), **+19 ms** when every mark's path is distinct; then **5 ms** once for the tree, and **~2.7 µs** per warm query. A frame nobody queries never builds a tree.

### Known limits

- **Dashed strokes are hittable along their gaps.** A hit target follows the path, not the dash pattern.
- **Glyph runs pick as layout boxes, not ink.** `skrifa` is optional and `pick` is unconditional, so real outlines are unreachable from a core module; the box is synthesized from `font_size`. Leading and side bearings are hittable, which is what a text target should be — but a glyph-backed marker shape is correspondingly looser than its outline.
- **Stroke ends and joins are round** whatever the cap and join say. Sub-pixel to a few pixels, and on the generous side.
- **There is no canvas.** A mark drawn partly outside the frame is hittable at coordinates outside it: the index answers about geometry, not about a framebuffer.

### Backend semantics

- **`RecordingScene`** stores both `PickId` and the scope ops faithfully. `draw_ops()` skips the scope bookkeeping, for a test asserting what got *drawn*; `scope_at(i)` reads the stack in effect at an op.
- **Rasterising backends ignore `pick_id`.** The index sits above them, so a rasteriser has nothing to do with it.
- **SVG surfaces both** — `data-pick-id` on primitives and `<g data-pick-kind=…>` for scopes, behind the one `SvgConfig::pick_ids` flag. PDF accepts and ignores.

## Core types — wrapping kurbo + peniko

Geometry (`Affine`, `Point`, `Rect`, `Size`, `Vec2`, `BezPath`) comes from kurbo. Brushes, gradients, colors, image data, fonts come from peniko / linebender_resource_handle. We re-export them through our own module paths (`hephaestus::geometry::Affine`, not `kurbo::Affine`) so a future swap is a single-line change.

Where the intersection is narrower than peniko's full surface, we define our own enum (see "intersection rule" above). Otherwise re-export directly — reimplementing affine math or gradient interpolation is not in scope.

## Module map

Folders (each with its own CLAUDE.md):

- `scene/` — `SceneBuilder` trait, glyph types, recording backend.
- `backend/` — `Renderer` trait, error type, and backend implementations.
- `layout/` — grid layout solver. Recursive grids, fr / auto tracks, `respect()`, `Measure` protocol.
- `composition/` — patchwork-style plot composition. 13-col × 16-row anatomical grid; chrome alignment across nested compositions via `Extent::TrackOf`.
- `pick/` — hit testing: `PickId`, `PickScope`, and the CPU spatial index behind them. Depends on nothing above `scene/`, which is what lets any backend — or none — answer a query.
- `primitives/` — compound 2D primitives: path constructors (rect / circle / wedge / polyline / polygon / arc), composable vertex transforms (clip / offset / round corners), arc-length sampling, ribbon tessellation.
- `plot/` — high-level plot API: `Plot`, `PlotComposition` orchestrator, key-based diff for identity-preserving animation. Geoms in `plot/geom/`; axis / legend rendering in `plot/chrome/`. Scales and values themselves live in [`crate::scales`] (see below).
- `scales/` — leaf module: `Value`, `DataColumn`, `Scale`, scale types, transforms, break / tick algorithms. Backend-agnostic and plot-agnostic; nothing inside imports from `src/plot/`, `src/scene/`, etc. Intended to be lifted into its own crate once the API settles. Hephaestus's own `Scale` bundle, `ScaleRegistry` and the ggplot-style constructors live in `plot/scale/` (which also re-exports `crate::scales::*`, so `hephaestus::plot::scale::*` reaches both); `plot/value.rs` is a pure re-export shim over `crate::scales::value`.
- `document/` — plot documents behind `document-read` / `document-write`: capture a `PlotComposition` to bytes and rebuild it elsewhere, carrying configuration rather than pixels so the reader re-solves layout at its own size. The two exceptions are opt-in payloads: font files, and the raster images a plot names — from an `ImageGeom`'s channel or a markdown image tag, including ones the writer read off disk. Marker shapes are carried as configuration — a custom one is a few Bézier subpaths, and a glyph-backed one travels as its source text. Depends on `plot/`, `scales/` and the style vocabulary; nothing depends on it.
- `image/` — raster readers and writers (PNG / JPEG / TIFF / WebP), one cargo feature per format. Reading normalises whatever the file holds to the buffer contract the writers enforce and hands back a `brush::Image`, which is what `SceneBuilder::draw_image` and `plot::ImageGeom` consume.
- `window/` — live window presentation behind the `window` feature: the `WindowApp` trait, the winit event loop, and the surface blit that puts a rendered frame on screen. Depends on `backend/` (it is a host for `WgpuRenderer`) and on nothing above it.
- `text/` — parley-backed text shaping and layout. A host crate may swap in its own shaper behind the `TextRun` / `draw_text` surface, but the parley path is the committed default. `text/rich/` layers marquee-flavoured markdown on top of it (see `src/text/rich/CLAUDE.md`).

Single-file modules (no CLAUDE.md, one-line descriptions here):

- `blend.rs` — `BlendMode` / `Compose` / `Mix` enums (intersection of Vello + Blend2D).
- `brush.rs` — `Brush`, `Image`, `Sampling` (Nearest / Bilinear).
- `color.rs` — re-exports peniko `Color`; owns `ColorSpace` (Oklab / Srgb) and `lerp_color`, the one place two colors get blended. Every blend names its space; `ColorSpace::default()` is Oklab.
- `geometry.rs` — re-exports kurbo `Affine`, `Point`, `Rect`, `Size`, `Vec2`.
- `image_registry.rs` — `ImageRegistry`: the name → image map `ImageGeom` and markdown image tags resolve against. At the crate root for the same reason `linetype.rs` is — `text::rich` resolves image tags while shaping, and `text/` must not depend on `plot/`; `plot::ImageRegistry` re-exports it. A name it does not hold is read as a location (a file path, or an `http(s)` URL behind `image-url`) and cached, which is what makes a markdown `![](logo.png)` need no setup while leaving a registered name authoritative for a build with no filesystem.
- `linetype.rs` — the named `solid` / `dashed` / `dotted` / `dashdot` constructors plus `draw_linetype_with_markers`, the arc-length walk that stamps marker shapes along a polyline. At the crate root rather than under `plot/geom/` because rich-text block borders express their strokes as linetypes too, and `text/` must not depend on `plot/`; `plot::geom::linetype` re-exports it. The `LinetypeStep` enum itself lives in `style_vocab.rs` — it's shared vocabulary like `Color`, so `scales` can carry a column of dash patterns without depending on the renderer that walks them.
- `mesh.rs` — `Mesh`: flat 2D triangle list with per-vertex colour. Used by `primitives::ribbon` and consumed by `SceneBuilder::draw_mesh`.
- `path.rs` — `Path` (kurbo `BezPath` wrapper) and `FillRule` (intersection enum).
- `png.rs` — aliases for the PNG entry points in `image/` (`png` feature).
- `shape.rs` — `Shape` / `ShapeRegistry` / `ShapeStyle`: named glyphs / paths for scatterplot markers and line endpoint terminators.
- `stroke.rs` — re-exports kurbo `Stroke`, `Cap`, `Join`. Stroke alignment and variable-width strokes are not in scope.
- `style_vocab.rs` — the styling vocabulary shared by the plot theme, the text layer and `scales`: `Length` / `Margin` (absolute-or-relative measurements), `LinetypeStep` (one step of a dash pattern), `Palette` / `ThemeColor` (semantic colour anchors and references into them), `HAlign` / `VAlign`. Lives at the crate root for the same reason `linetype.rs` does — `text::rich` resolves palette colours and relative sizes while shaping. `plot::theme` re-exports every item, so plot-side code addresses them through the theme.
