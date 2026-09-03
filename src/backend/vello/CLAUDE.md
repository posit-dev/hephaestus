# src/backend/vello/CLAUDE.md

Vello backend: implements `SceneBuilder` against a `vello::Scene` and `Renderer` against an `wgpu` device that rasterises headlessly to an RGBA8 buffer.

## What this module does

`VelloScene` (in `mod.rs`) wraps a `vello::Scene` and translates our restricted enums to peniko's wider set via `../convert.rs`. `VelloRenderer` owns the wgpu device, queue, and the cached `HeadlessTarget` (storage texture + readback buffer) needed to render headlessly. Its `Renderer::Scene` is a `PickIndexScene<VelloScene>`, so `with_picking()` turns on a CPU hit index built as the scene is drawn rather than allocating anything in the rasteriser. `VelloScene` itself ignores `pick_id`.

Two output paths share one scene:

- **`Renderer::render_to_buffer`** — the headless path. Renders into the backend-owned display `HeadlessTarget`, copies it back to CPU, and writes into the caller's RGBA8 slab.
- **`WgpuRenderer::render_to_texture`** — the windowing path. Renders straight into a host-owned `wgpu::TextureView` (must be `Rgba8Unorm` storage; the host blits to its swap chain). The display `HeadlessTarget` is never allocated on this path. Picking is unaffected by which path is taken, since the index is built before either.

`VelloRenderer::new()` / `with_picking()` spin up a private wgpu device. `with_device(&Device, &Queue)` / `with_device_and_picking(&Device, &Queue)` share an existing host device so the rendered texture is on the same device as the host's surface.

## Quirks worth remembering

- **Sync construction via `pollster::block_on`.** Public API is sync. If async init becomes needed, add a `with_device(device, queue)` constructor — don't make `new()` async.
- **`HeadlessTarget` cached per `(width, height)`.** Recreated on size change.
- **`Rgba8Unorm`, not `Rgba8UnormSrgb`.** Vello requires storage texture; sRGB is not storable on the path Vello uses. Storage flags: `STORAGE_BINDING | COPY_SRC`.
- **Vello unpremultiplies on output.** Its fine shader writes straight alpha into the target texture, and our readback is a plain row-by-row memcpy, so both output paths hand out un-premultiplied RGBA8. This is vello's behavior, not ours — re-check it on every vello bump; `tests/alpha_format.rs` fails loudly if it flips.
- **Readback honours wgpu's 256-byte row alignment** (`COPY_BYTES_PER_ROW_ALIGNMENT`). The readback buffer has padded rows; the copy-out strips padding into the caller's tight RGBA8 buffer.
- **GPU drain pattern after `queue.submit`** — `device.poll(PollType::wait_indefinitely())` then await `map_async` via a `futures_intrusive` oneshot. Non-obvious; preserve this sequence.
- **Draw budget is a hard cap, not a soft one.** Vello sizes its `bin_data` buffer to a fixed `1 << 18` words and stores the scene's draw-info stream at its front; `RenderConfig::new` subtracts the stream length from that size, so an over-long stream underflows and panics before any GPU work is queued (and a panic raised inside winit's macOS draw callback aborts rather than unwinds). `MAX_DRAW_INFO_WORDS` mirrors the buffer size and both render entry points reject an over-budget scene with `BackendError::SceneTooLarge`. Solid brushes cost one word per fill / stroke, so ~262k flat-coloured objects; gradients and images cost more. Re-check the constant against `vello_encoding::BufferSizes::new` on every vello bump.
- **Bump-buffer exhaustion fails silently and blanks the frame.** Flatten, path-count and coarse bump-allocate from fixed buffers — `lines` / `tile` / `seg_counts` / `segments` at `1 << 21` each, `ptcl` at `1 << 23`. When one runs out the stage sets a bit in `bump.failed`, `path_tiling_setup` writes `ptcl[0] = ~0u`, `coarse` returns early and `fine` skips every tile, leaving the target texture untouched — an all-zero image, which presents as a black window rather than partial output. `Renderer::render_to_texture` reads the bump counters back only under vello's `debug_layers` feature, so nothing surfaces this by default; to diagnose it, enable `debug_layers` + `bump_estimate` on the vello dep and call the deprecated `render_to_texture_async`, which returns the `BumpAllocators`. `seg_counts` (line-segment × tile intersections) binds first for dense mark geoms — measured ~20 per 7px circle and ~42 per 28px circle, so a 14px-diameter scatter blanks around 76k marks. It tracks mark count × mark perimeter in device pixels, so DPI moves it; overlap lands in `ptcl` instead, which has far more headroom.
- **This backend's picking used to be the wrong one.** It cannot disable antialiasing, so an id buffer's edge pixels were a coverage blend of the two marks either side — two overlapping circles yielded 28 ids that were never drawn. The failure went away along with the pick pass itself: the index tests geometry, so every id it reports is one that was drawn.

## Dependency version quirks

Linebender / wgpu move fast and broke surface between recent versions. Notes for future bumps:

- **peniko 0.6** renamed `Image` → `ImageData`, introduced `ImageBrush` (= `ImageData` + `ImageSampler`), and removed `peniko::Font` — fonts are now `peniko::FontData` (re-exported from `linebender_resource_handle`). `Color` is a type alias for `color::AlphaColor<Srgb>`.
- **peniko 0.6 `Gradient`** struct fields include `interpolation_alpha_space` (not `..._cs`); construct via `Gradient::new_linear(start, end).with_stops(&[Color, Color])` rather than struct literals.
- **kurbo 0.13** `Rect::to_path(tolerance)` requires `use kurbo::Shape` in scope.
- **wgpu 29** `InstanceDescriptor` does not implement `Default`; use `InstanceDescriptor::new_without_display_handle()` and mutate fields. `wgpu::Instance::new` takes an owned descriptor, not a reference. `DeviceDescriptor` requires an `experimental_features` field. `PollType::Wait` is a struct variant — use `PollType::wait_indefinitely()`.
- **vello 0.9** `Renderer::render_to_texture` takes `&TextureView`, not `&Texture`. `AaSupport::area_only()` is the cheapest init (matches the `AaConfig::Area` we use in `RenderParams`).

## Files

- `mod.rs` — `VelloScene`, `VelloRenderer`, `HeadlessTarget`.
- `../convert.rs` — the enum-mapping layer: `FillRule`, `BlendMode`, `Compose`, `Mix`, `Sampling` → peniko's native types.
