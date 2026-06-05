# src/scene/CLAUDE.md

The authoring surface every backend has to satisfy. See `src/CLAUDE.md` for the two-trait split (`SceneBuilder` vs `Renderer`) and the picking model overview.

## What this module does

`SceneBuilder` is the trait plot code calls to issue draw operations. Implementations either rasterise immediately (Vello, future Blend2D) or record the calls for later replay (`scene::recording::RecordingScene`, used by future SVG / PDF emitters).

Every method is **self-contained** — no persistent "current transform" or "current brush" state. The caller passes everything per call. This is what lets the recording backend be a one-line `match` per op and what makes both immediate-mode and replay backends trivial to implement.

## Core types

- **`SceneBuilder`** trait — `fill`, `stroke`, `draw_image`, `draw_glyphs`, `draw_mesh`, `push_layer`, `pop_layer`. Every drawing primitive (not `push_layer` / `pop_layer`) takes a `PickId`.
- **`Font`** — opaque handle wrapping `peniko::FontData` (Arc-backed font blob + index). Construct via `Font::new(blob, index)`.
- **`Glyph`** — `{ id: u32, x: f32, y: f32 }`. A single positioned glyph in run-local coordinates.
- **`GlyphRun<'a>`** — a run of glyphs sharing one font, size, transform, brush, and brush alpha. Borrows the font and glyph slice; the brush is owned by the caller and borrowed by reference.

The trait deliberately consumes already-positioned glyphs — shaping and line-breaking are out of scope (the optional `text` module provides scaffolding, but the scene API doesn't require it).

## Conventions

- **Adding a method on `SceneBuilder` requires adding an `Op` variant in `recording.rs`.** The recording backend is exhaustive over the trait surface; that exhaustiveness is what validates the trait shape (if recording is awkward, the trait is wrong) and what lets future SVG / PDF emitters be one `match` over `Op`. Skipping this step breaks the recording backend and downstream emitters.
- **Picking ids carry through every primitive.** Authoring code chooses `PickId::Skip` (most decorative chrome), `PickId::Block` (opaque backgrounds), or `PickId::Id(n)`. See `src/CLAUDE.md` for the model; `pick.rs` for the encoding.
- **`push_layer` does not take a `PickId`.** The Vello backend normalises blend to `NORMAL` and alpha to `1.0` inside the pick scene's `push_layer` so encoded ids inside the layer don't fade toward the no-hit sentinel.
- **`draw_mesh` shares one `pick_id` across the whole mesh.** Picking does not distinguish individual triangles. No backend currently has a native indexed-mesh primitive — each backend decomposes the mesh into its own draw ops (e.g. one fill with a per-triangle linear-gradient brush in Vello).

## Cross-references

- `backend/vello/` — the only `SceneBuilder` implementation that rasterises today. Pick scene is a parallel `vello::Scene` recorded alongside the display scene.
- `pick.rs` — `PickId` variants and the RGB-channel encoding.
- `text/` — produces `GlyphRun` values from shaped strings (gated on `text` feature).
- `mesh.rs` — `Mesh` type consumed by `draw_mesh`.
