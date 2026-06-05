# src/backend/vello/CLAUDE.md

Vello backend: implements `SceneBuilder` against a `vello::Scene` and `Renderer` against an `wgpu` device that rasterises headlessly to an RGBA8 buffer.

## What this module does

`VelloScene` (in `mod.rs`) wraps a `vello::Scene` and translates our restricted enums to peniko's wider set via `convert.rs`. `VelloRenderer` owns the wgpu device, queue, and the cached `HeadlessTarget` (storage texture + readback buffer) needed to render headlessly. When picking is enabled (`VelloRenderer::with_picking()`), every draw call is also recorded into a parallel pick `vello::Scene`, rasterised into a second target, and read back to power `pick_at(x, y) -> Option<u32>`.

## Quirks worth remembering

- **Sync construction via `pollster::block_on`.** Public API is sync. If async init becomes needed, add a `with_device(device, queue)` constructor — don't make `new()` async.
- **`HeadlessTarget` cached per `(width, height)`.** Recreated on size change.
- **`Rgba8Unorm`, not `Rgba8UnormSrgb`.** Vello requires storage texture; sRGB is not storable on the path Vello uses. Storage flags: `STORAGE_BINDING | COPY_SRC`.
- **Readback honours wgpu's 256-byte row alignment** (`COPY_BYTES_PER_ROW_ALIGNMENT`). The readback buffer has padded rows; the copy-out strips padding into the caller's tight RGBA8 buffer.
- **GPU drain pattern after `queue.submit`** — `device.poll(PollType::wait_indefinitely())` then await `map_async` via a `futures_intrusive` oneshot. Non-obvious; preserve this sequence.
- **Pick scene composition normalises blend.** Inside `push_layer` the pick scene uses `NORMAL` blend with `alpha = 1.0` so encoded ids don't fade toward the no-hit sentinel through alpha attenuation. Display scene keeps the caller's `BlendMode` and `alpha`.
- **Pick scene AA: `AaConfig::Area`** with `base_color` transparent — same AA the display scene uses; matches `AaSupport::area_only()` at renderer init.
- **One submit per render.** Display and pick texture-→-buffer copies share a single command buffer / submit / poll round-trip; don't fan them out.
- **Minimum pick stroke width.** Hairline strokes (< `MIN_PICK_STROKE_WIDTH = 2.0` px) are widened in the pick scene so sub-pixel strokes remain hittable even when visually invisible.

## Dependency version quirks

Linebender / wgpu move fast and broke surface between recent versions. Notes for future bumps:

- **peniko 0.6** renamed `Image` → `ImageData`, introduced `ImageBrush` (= `ImageData` + `ImageSampler`), and removed `peniko::Font` — fonts are now `peniko::FontData` (re-exported from `linebender_resource_handle`). `Color` is a type alias for `color::AlphaColor<Srgb>`.
- **peniko 0.6 `Gradient`** struct fields include `interpolation_alpha_space` (not `..._cs`); construct via `Gradient::new_linear(start, end).with_stops(&[Color, Color])` rather than struct literals.
- **kurbo 0.13** `Rect::to_path(tolerance)` requires `use kurbo::Shape` in scope.
- **wgpu 29** `InstanceDescriptor` does not implement `Default`; use `InstanceDescriptor::new_without_display_handle()` and mutate fields. `wgpu::Instance::new` takes an owned descriptor, not a reference. `DeviceDescriptor` requires an `experimental_features` field. `PollType::Wait` is a struct variant — use `PollType::wait_indefinitely()`.
- **vello 0.9** `Renderer::render_to_texture` takes `&TextureView`, not `&Texture`. `AaSupport::area_only()` is the cheapest init (matches the `AaConfig::Area` we use in `RenderParams`).

## Files

- `mod.rs` — `VelloScene`, `VelloRenderer`, `HeadlessTarget`, the pick scene rasterisation path.
- `convert.rs` — the enum-mapping layer: `FillRule`, `BlendMode`, `Compose`, `Mix`, `Sampling` → peniko's native types.
