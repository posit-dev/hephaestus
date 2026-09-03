# src/scene/CLAUDE.md

The authoring surface every backend has to satisfy. See `src/CLAUDE.md` for the two-trait split (`SceneBuilder` vs `Renderer`) and the picking model overview.

## What this module does

`SceneBuilder` is the trait plot code calls to issue draw operations. Implementations either rasterise immediately (Vello, future Blend2D) or record the calls for later replay (`scene::recording::RecordingScene`, used by future SVG / PDF emitters).

Every method is **self-contained** — no persistent "current transform" or "current brush" state. The caller passes everything per call. This is what lets the recording backend be a one-line `match` per op and what makes both immediate-mode and replay backends trivial to implement.

## Core types

- **`SceneBuilder`** trait — `fill`, `stroke`, `draw_image`, `draw_glyphs`, `draw_mesh`, `push_layer`, `pop_layer`, `push_pick_scope`, `pop_pick_scope`. Every drawing primitive (not the layer or scope pairs) takes a `PickId`.
- **`Font`** — opaque handle wrapping `peniko::FontData` (Arc-backed font blob + index). Construct via `Font::new(blob, index)`.
- **`Glyph`** — `{ id: u32, x: f32, y: f32 }`. A single positioned glyph in run-local coordinates.
- **`GlyphRun<'a>`** — a run of glyphs sharing one font, size, transform, brush, and brush alpha. Borrows the font and glyph slice; the brush is owned by the caller and borrowed by reference.
- **`TextSource<'a>`** — optional, on `GlyphRun`: what the run was *shaped from*. Glyph ids are everything a rasteriser needs and strictly less than a backend emitting `<text>` or a PDF text object needs — those need the characters back and the font named, and shaping knew both. Carries the source substring, a `FontSpec` (the CSS-shaped *request*, since the resolved face's family lives only in its own `name` table), the run's advance, its decorations, a link destination, and a `TextGroup`. `None` when the caller positioned glyphs itself with no string to point at — a glyph-backed marker — in which case a vector backend falls back to outlines.
- **`TextGroup`** — says which runs were laid out together, so a backend can gather a block into one element instead of one per run. **Equality on `TextSource` ignores it**: ids come from a running counter, so the same drawing recorded twice carries different ones, and `RecordingScene`'s op-for-op equality exists to answer "is this the same *drawing*". The cost is that op-list equality cannot verify grouping — `tests/svg.rs` is what covers it.

The trait deliberately consumes already-positioned glyphs — shaping and line-breaking are out of scope. The optional `text` module provides the parley-backed shaper used by chrome and the text geoms; the scene API itself does not require it.

## Conventions

- **Adding a method on `SceneBuilder` requires adding an `Op` variant in `recording.rs`.** The recording backend is exhaustive over the trait surface; that exhaustiveness is what validates the trait shape (if recording is awkward, the trait is wrong) and what lets future SVG / PDF emitters be one `match` over `Op`. Skipping this step breaks the recording backend and downstream emitters.
- **Picking ids carry through every primitive.** Authoring code chooses `PickId::Skip` (no authoring id), `PickId::Block` (occlude without reporting), or `PickId::Id(n)`. See `src/CLAUDE.md` for the model.
- **`push_layer` does not take a `PickId`, and `push_pick_scope` is not a layer.** The two stacks are orthogonal: a scope has no visual effect and imposes no clip, and the two need not nest with one another. A backend that emits groups for both — SVG does — has to tag them, or an interleaved pair closes the wrong element.
- **The two scope methods have default no-op bodies, and they are the only ones that do.** Deliberate: three of the five real implementors want exactly a no-op, it keeps the trait non-breaking for a downstream implementor, and — unlike every other method here — ignoring it still produces a correct *picture*. The intersection-of-backends rule is about visual capabilities, and a scope has none.
- **`draw_mesh` shares one `pick_id` across the whole mesh.** Picking does not distinguish individual triangles. No backend currently has a native indexed-mesh primitive — each backend decomposes the mesh into its own draw ops (e.g. one fill with a per-triangle linear-gradient brush in Vello).

## Cross-references

- `backend/vello/` — one of the two rasterising `SceneBuilder` implementations. Ignores `pick_id`, like every rasteriser: the hit index sits above them.
- `backend/svg/` — the vector implementation. Consumes `TextSource` to emit real `<text>`; the reason that field exists.
- `pick/` — `PickId`, `PickScope`, and the index a scene records into.
- `text/` — produces `GlyphRun` values from shaped strings.
- `mesh.rs` — `Mesh` type consumed by `draw_mesh`.
