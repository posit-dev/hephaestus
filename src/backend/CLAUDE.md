# src/backend/CLAUDE.md

The `Renderer` trait, the error type, and the backend implementations themselves. See `src/CLAUDE.md` for the rationale behind splitting `SceneBuilder` and `Renderer` into two traits and the intersection-of-backends rule that governs what backends have to support.

## What this module does

A `Renderer` owns backend resources (GPU device, pipelines, readback buffer) and produces an RGBA8 byte buffer from a scene authored against `SceneBuilder`. The trait is fallible (`Result<(), BackendError>`); resource ownership lets each backend cache whatever it needs across renders.

## Core types

- **`Renderer`** trait — two methods: `scene(&mut self) -> &mut Self::Scene` (issue draws against this) and `render_to_buffer(width, height, background, out)` (rasterise into `out`, which must be exactly `width * height * 4` bytes RGBA8 premultiplied).
- **`Renderer::Scene`** associated type — the backend's concrete `SceneBuilder` implementation.
- **`WgpuRenderer`** trait — optional extension, gated on `feature = "vello"`. Adds `render_to_texture(view, width, height, background)` for hosts that want to skip the CPU readback and present the result through their own wgpu surface. The view must be `Rgba8Unorm` with `STORAGE_BINDING | COPY_SRC` usage (Vello renders via a compute shader, so a render-attachment-only swap chain texture cannot be the direct target). Hosts manage their own intermediate-storage-texture → swap-chain blit. Picking still works: the pick scene continues to rasterise into the backend-owned pick target and read back to CPU.
- **`BackendError`** — `BufferSize`, `NoAdapter`, `DeviceRequest`, `Readback`, `Other`. Backends should prefer a typed variant over `Other` when possible.

## Conventions

- **No `Box<dyn Renderer>`.** The associated `Scene` type makes the trait awkward as a trait object (GAT-ish). For runtime backend selection use an enum (`AnyRenderer { Vello(VelloRenderer), Blend2d(...) }`). Dynamic dispatch on the scene side is fine: `&mut dyn SceneBuilder` is object-safe.
- **Device sharing for windowing.** GPU backends that implement `WgpuRenderer` expose a `with_device(&wgpu::Device, &wgpu::Queue)` (+ `with_device_and_picking`) constructor so the host can hand in the device backing its presentation surface. Each backend's `new()` continues to spin up its own headless device — that path stays available for file export and tests. The crate re-exports `wgpu` at `hephaestus::wgpu` so callers don't need a separate dependency at a matching version.
- **One backend per subfolder.** Each backend lives in `src/backend/<name>/` with at minimum `mod.rs` (the `SceneBuilder` and `Renderer` impls) and `convert.rs` (the enum-mapping layer).
- **`convert.rs` is where the intersection rule is enforced.** Our restricted enums (`FillRule`, `BlendMode`, `Compose`, `Mix`, `Sampling`) map into the backend's wider native enums here. When peniko exposes `Mix::Clip` and we don't, the conversion table is the only place that knows that.
- **Feature-gated.** Each backend is gated by a cargo feature of the same name (`vello`, future `blend2d`). `vello` and `png` are default-on. `blend2d`, `svg`, `pdf` are stub features (no code behind them yet) so dependent crates can write `features = ["blend2d"]` once available.

## Adding a new backend

1. Add a feature in `Cargo.toml` and the optional deps it requires.
2. Create `src/backend/<name>/{mod.rs, convert.rs}`.
3. Implement `SceneBuilder` for `<name>Scene` and `Renderer` for `<name>Renderer`.
4. In `convert.rs`, map our restricted enums (`FillRule`, `BlendMode`, `Compose`, `Mix`, `Sampling`) to the backend's native types. Use the existing `backend/vello/convert.rs` as the reference.
5. Add a cfg-gated `pub mod <name>;` line in `src/backend/mod.rs`.
6. Don't extend `SceneBuilder` to expose backend-specific features. If you need to, that's an architectural decision worth discussing first — extension trait is the fallback, not a method on `SceneBuilder`.

## Cross-references

- `scene/` — the `SceneBuilder` trait every backend implements.
- `backend/vello/` — the only rasterising backend today. See its own `CLAUDE.md` for the wgpu / vello / pollster quirks.
