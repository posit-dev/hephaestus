# src/backend/CLAUDE.md

The `Renderer` trait, the error type, and the backend implementations themselves. See `src/CLAUDE.md` for the rationale behind splitting `SceneBuilder` and `Renderer` into two traits and the intersection-of-backends rule that governs what backends have to support.

## What this module does

A `Renderer` owns backend resources (GPU device, pipelines, readback buffer) and produces an RGBA8 byte buffer from a scene authored against `SceneBuilder`. The trait is fallible (`Result<(), BackendError>`); resource ownership lets each backend cache whatever it needs across renders.

## Core types

- **`Renderer`** trait — two methods: `scene(&mut self) -> &mut Self::Scene` (issue draws against this) and `render_to_buffer(width, height, background, out)` (rasterise into `out`, which must be exactly `width * height * 4` bytes RGBA8 with straight, un-premultiplied alpha).
- **`Renderer::Scene`** associated type — the backend's concrete `SceneBuilder` implementation.
- **`WgpuRenderer`** trait — optional extension, available whenever a rasterising backend is. Carries two associated constants the host reads instead of assuming: `REQUIRED_TARGET_USAGE` (`STORAGE_BINDING` for the compute-shader backend, `RENDER_ATTACHMENT` for sparse strips) and `TARGET_IS_PREMULTIPLIED` (`false` / `true` respectively — `render_to_buffer` normalises to straight alpha on both paths, `render_to_texture` does not). Adds `render_to_texture(view, width, height, background)` for hosts that want to skip the CPU readback and present the result through their own wgpu surface. The view must be `Rgba8Unorm` with `STORAGE_BINDING` usage (the compute-shader backend writes through a storage binding, so a render-attachment-only swap chain texture cannot be the direct target; the sparse-strips backend wants `RENDER_ATTACHMENT` instead, so this contract has to become backend-supplied before it can implement the trait), plus whatever the host's own consumption needs — `TEXTURE_BINDING` to blit onto a surface, `COPY_SRC` to copy back. Hosts manage their own intermediate-storage-texture → swap-chain blit, and must premultiply during that blit if they present translucent content (the view holds straight alpha, same as `render_to_buffer`). `src/window/` is the in-crate host built on this trait. Picking still works: the pick scene continues to rasterise into the backend-owned pick target and read back to CPU.
- **`BackendError`** — `BufferSize`, `NoAdapter`, `DeviceRequest`, `Readback`, `SceneTooLarge`, `Other`. Backends should prefer a typed variant over `Other` when possible.

## Conventions

- **Straight alpha on output.** Every `Renderer` writes RGBA8 with un-premultiplied alpha, so a buffer drops straight into a PNG or an `image::RgbaImage` with no conversion. A new backend whose rasteriser is natively premultiplied (most are) unpremultiplies on the way out. `tests/alpha_format.rs` pins the convention.
- **No `Box<dyn Renderer>`.** The associated `Scene` type makes the trait awkward as a trait object (GAT-ish). For runtime backend selection use an enum (`AnyRenderer { Vello(VelloRenderer), Blend2d(...) }`). Dynamic dispatch on the scene side is fine: `&mut dyn SceneBuilder` is object-safe.
- **Device sharing for windowing.** GPU backends that implement `WgpuRenderer` expose a `with_device(&wgpu::Device, &wgpu::Queue)` (+ `with_device_and_picking`) constructor so the host can hand in the device backing its presentation surface. Each backend's `new()` continues to spin up its own headless device — that path stays available for file export and tests. The crate re-exports `wgpu` at `hephaestus::wgpu` so callers don't need a separate dependency at a matching version.
- **One backend per subfolder.** Each backend lives in `src/backend/<name>/` with at minimum `mod.rs`. A *rasterising* backend implements `SceneBuilder` and `Renderer`; a *vector* backend implements `SceneBuilder` alone and exposes its own emit entry points, because `Renderer`'s contract is to fill a buffer of RGBA8 and there are no pixels to fill. `backend/svg/` and `backend/pdf/` are the worked examples, and they differ in what they aim at rather than in shape: editable output against fixed output.
- **`mesh.rs` is shared by every backend that needs it, including the ones with no GPU.** Mesh decomposition works purely in this crate's own types and emits plain `SceneBuilder::fill` calls, so `backend/svg/` reuses it verbatim; its cfg gate names `svg` alongside the rasterisers for that reason. `convert.rs` does not, because it maps onto *peniko's* enums. **`backend/pdf/` is the one backend absent from that gate**: PDF has a native Gouraud triangle shading, so it emits the mesh as one object rather than decomposing it into fills. Everything in `mesh.rs` is a workaround for the absence of such a primitive, and a backend that has one should not pay for it.
- **`convert.rs` and `mesh.rs` sit beside the backends, not inside them.** Both rasterising backends consume peniko, so the enum mapping is identical and lives once in `backend/convert.rs`. Mesh decomposition works purely in this crate's own types and emits plain `SceneBuilder::fill` calls, so it lives once in `backend/mesh.rs` — and because each backend's own `fill` already handles its pick pass, sharing it costs nothing. A backend whose native types are *not* peniko's should add its own `convert.rs` in its own folder rather than bending the shared one.
- **`backend/convert.rs` is where the intersection rule is enforced.** Our restricted enums (`FillRule`, `BlendMode`, `Compose`, `Mix`, `Sampling`) map into the wider native enums here. When peniko exposes `Mix::Clip` and we don't, the conversion table is the only place that knows that.
- **Feature-gated.** Each backend is gated by a cargo feature of the same name (`vello`, `svg`, `pdf`, future `blend2d`). `vello` and `png` are default-on; `jpeg` / `tiff` / `webp` gate the other writers in `src/image/`. `blend2d` is a stub feature (no code behind it yet) so dependent crates can write `features = ["blend2d"]` once available.

## Adding a new backend

1. Add a feature in `Cargo.toml` and the optional deps it requires.
2. Create `src/backend/<name>/mod.rs`. Reuse `backend/convert.rs` if the backend speaks peniko; add a local `convert.rs` if it does not.
3. Implement `SceneBuilder` for `<name>Scene`. A rasterising backend also implements `Renderer` for `<name>Renderer`; a vector backend does not — it exposes `write_<name>` / `encode_<name>` free functions instead, shaped like `src/image/`'s writers.
4. Map our restricted enums (`FillRule`, `BlendMode`, `Compose`, `Mix`, `Sampling`) to the backend's native types. `backend/convert.rs` is the reference and, for a peniko backend, the implementation — but only for a peniko backend: SVG's native types are attribute strings, so its mapping lives in its own folder, as the Conventions above require.
5. Add a cfg-gated `pub mod <name>;` line in `src/backend/mod.rs`.
6. Don't extend `SceneBuilder` to expose backend-specific features. If you need to, that's an architectural decision worth discussing first — extension trait is the fallback, not a method on `SceneBuilder`.

## Cross-references

- `scene/` — the `SceneBuilder` trait every backend implements.
- `backend/vello/` — the compute-shader backend. See its own `CLAUDE.md` for the wgpu / vello / pollster quirks.
- `backend/hybrid/` — the sparse-strips backend: coverage on the CPU, so it can rasterise a pick pass with binary coverage and carries no draw-count ceiling. See its own `CLAUDE.md`.
- `backend/svg/` — the vector backend: emits markup, implements `SceneBuilder` only, and needs no GPU. See its own `CLAUDE.md`.
- `backend/pdf/` — the fixed vector backend: emits a PDF file with every glyph embedded, so it looks the same on a machine with none of the fonts. Implements `SceneBuilder` only, needs no GPU, and is the one backend that does not use `backend/mesh.rs`. See its own `CLAUDE.md`.
- `backend/href.rs` — the link-destination allow-list `svg` and `pdf` share.
