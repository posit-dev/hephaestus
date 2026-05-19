# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

`hephaestus` is a low-level data-visualization renderer. The crate exposes a backend-agnostic 2D scene API and an initial Vello (GPU compute via wgpu) backend. Future planned backends: Blend2D (CPU raster), SVG, PDF. Performance for interactive/real-time updates on dense plots is the design driver. WASM must work but is not the primary target.

The crate is at scaffolding stage (~1000 LOC). Chart primitives, text shaping, and layout solving are explicitly **not** part of this crate.

## Commands

```sh
cargo build                                              # default features (vello + png)
cargo build --no-default-features                        # core types & traits only — no wgpu pulled in
cargo build --no-default-features --features vello,png   # explicit feature combination

cargo test                                               # all tests
cargo test --test smoke                                  # the GPU smoke test (requires a working wgpu adapter)

cargo clippy --all-features --all-targets -- -D warnings # treat warnings as errors

cargo run --example hello                                # renders examples/hello.png — visual sanity check
```

## Architecture

### Two traits, intentionally split

- **`SceneBuilder`** (`src/scene/mod.rs`): the authoring surface. Every method is self-contained — no persistent "current transform" or "current brush" state. Plot code calls this. Pure CPU, infallible.
- **`Renderer`** (`src/backend/mod.rs`): owns backend resources (GPU device, pipelines, readback buffer) and rasterizes a built scene to an RGBA8 buffer. Fallible.

These are split because (a) `SceneBuilder` is the only thing recording/vector backends (SVG, PDF) need — they shouldn't have to satisfy GPU concerns — and (b) it mirrors Vello's own `Scene` vs `Renderer` split so wrapping is zero-cost.

`Renderer` has `type Scene: SceneBuilder` as an associated type. For runtime backend selection use an enum (`AnyRenderer`-style), not `Box<dyn Renderer>` — the GAT makes object-safety awkward. Dynamic dispatch belongs at the `&mut dyn SceneBuilder` callsite.

### The "intersection of backends" rule

The public surface is deliberately the **intersection** of what Vello and Blend2D both support natively. This is what keeps the same plot code running across backends without escape hatches.

Concretely:
- `Sampling` (in `src/brush.rs`) exposes only `Nearest` / `Bilinear` — peniko has more.
- `BlendMode` / `Compose` / `Mix` (in `src/blend.rs`) are our own enums restricted to the intersection — peniko has more (e.g., `Mix::Clip`, `Compose::PlusLighter`) which we deliberately don't expose.
- `FillRule` (in `src/path.rs`) is our own enum.
- No conic Beziers (kurbo's `BezPath` already excludes them).
- No stroke inside/outside alignment.
- No filter effects (blur, drop shadow, etc.).
- No hit testing on the trait — do it CPU-side in scene/plot code regardless of backend.

`src/backend/vello/convert.rs` is where this restriction is enforced: it maps our restricted enums to peniko's wider set. When adding a new backend, the analogous `convert.rs` is the only place the mapping lives.

When tempted to add a feature only one backend supports: don't. If it's genuinely necessary, the alternative is a backend-specific extension trait — not a feature on `SceneBuilder`.

### Backend organization

- Each backend is `src/backend/<name>/` with `mod.rs` (Scene + Renderer impls) and `convert.rs` (enum mapping).
- Gated by a cargo feature of the same name (`vello`, future `blend2d`).
- `vello` and `png` are default-on. `blend2d`, `svg`, `pdf` are stub features (no code behind them yet) — used so dependent crates can write `features = ["blend2d"]` once available.
- The core types and traits compile with `--no-default-features` (no wgpu pulled in), so downstream can build on top of `SceneBuilder` without GPU dependencies.

### Recording backend

`src/scene/recording.rs` implements `SceneBuilder` by appending each call to an owned `Op` enum. This:
1. Validates the trait shape (if recording is awkward, the trait is wrong).
2. Will be consumed by future SVG and PDF emitters (`fn write_svg(scene: &RecordingScene, w: &mut impl Write)` etc.).

If you add a method to `SceneBuilder`, you must add a corresponding `Op` variant. The recording backend should be exhaustive over the trait surface.

### Core types — wrapping kurbo + peniko

Geometry (`Affine`, `Point`, `Rect`, `BezPath`) comes from kurbo. Brushes, gradients, colors, image data, fonts come from peniko / linebender_resource_handle. We re-export them through our own module paths (`hephaestus::geometry::Affine`, not `kurbo::Affine`) so a future swap is a single-line change.

Where the intersection is narrower than peniko's full surface, we define our own enum (see "intersection rule" above). Otherwise re-export directly — reimplementing affine math or gradient interpolation is not in scope.

### Vello backend specifics

`VelloRenderer` (in `src/backend/vello/mod.rs`):
- Sync construction via `pollster::block_on` internally. Public API is sync. If async init becomes needed, add a `with_device(device, queue)` constructor — don't make `new()` async.
- Caches a `HeadlessTarget` (storage texture + readback buffer) per (width, height). Recreated on size change.
- Headless texture is `Rgba8Unorm` with `STORAGE_BINDING | COPY_SRC` — Vello requires storage texture, not sRGB.
- Readback honors wgpu's 256-byte row alignment (`COPY_BYTES_PER_ROW_ALIGNMENT`): the buffer has padded rows; the copy-out strips padding into the caller's tight RGBA8 buffer.
- After `queue.submit`, drains the GPU with `device.poll(PollType::wait_indefinitely())` then awaits `map_async` via a `futures_intrusive` oneshot. This pattern is non-obvious — preserve it.

### Dependency version quirks

The Linebender crates move fast and broke surface between recent versions. Notable points to remember when bumping:

- **peniko 0.6** renamed `Image` → `ImageData`, introduced `ImageBrush` (= `ImageData` + `ImageSampler`), and removed `peniko::Font` — fonts are now `peniko::FontData` (re-exported from `linebender_resource_handle`). `Color` is a type alias for `color::AlphaColor<Srgb>`.
- **peniko 0.6** `Gradient` struct fields include `interpolation_alpha_space` (not `..._cs`); construct gradients via `Gradient::new_linear(start, end).with_stops(&[Color, Color])` rather than struct literals.
- **kurbo 0.13** `Rect::to_path(tolerance)` requires `use kurbo::Shape` in scope.
- **wgpu 29** `InstanceDescriptor` does not implement `Default`; use `InstanceDescriptor::new_without_display_handle()` and mutate fields. `wgpu::Instance::new` takes an owned descriptor, not a reference. `DeviceDescriptor` requires an `experimental_features` field. `PollType::Wait` is a struct variant — use `PollType::wait_indefinitely()`.
- **vello 0.9** `Renderer::render_to_texture` takes `&TextureView`, not `&Texture`. `AaSupport::area_only()` is the cheapest init (matches the `AaConfig::Area` we use in `RenderParams`).

## Adding a new backend

1. Add a feature in `Cargo.toml` and the optional deps it requires.
2. Create `src/backend/<name>/{mod.rs, convert.rs}`.
3. Implement `SceneBuilder` for `<name>Scene` and `Renderer` for `<name>Renderer`.
4. In `convert.rs`, map our restricted enums (`FillRule`, `BlendMode`, `Compose`, `Mix`, `Sampling`) to the backend's native types.
5. Add a cfg-gated `pub mod <name>;` line in `src/backend/mod.rs`.
6. Don't extend `SceneBuilder` to expose backend-specific features. If you need to, that's an architectural decision worth discussing first.

## Out of scope (do not add)

Chart primitives, axes, scales, marks, text shaping/layout, font selection or loading, layout solving, surface presentation (no winit), interaction, animation, hit testing in the trait, filter effects. These belong in higher layers or other crates.
