# src/window/CLAUDE.md

Live window presentation, behind the off-by-default `window` feature. This is the in-crate host for `WgpuRenderer` — the counterpart to writing an RGBA8 buffer to a file.

## What this module does

`run(config, app)` opens an OS window, owns the GPU device backing its swap chain, hands that device to a `VelloRenderer` via `with_device` / `with_device_and_picking`, and pumps an event loop. The app implements `WindowApp`: `draw(&mut Frame)` per frame, `event(&mut EventCtx, Event)` for everything else.

Layering: this module sits on `backend/` and on nothing above it. It knows about `SceneBuilder` and `VelloRenderer`; it does not know about `plot/`. An app draws a `PlotComposition` by calling `view.render(scene, size, dpi)` itself — the window layer just supplies the three arguments.

## The frame path

Vello rasterises through a compute shader, so its output texture must be `Rgba8Unorm` with `STORAGE_BINDING`, which a swap-chain texture never is. Every frame is therefore three steps:

1. `VelloRenderer::render_to_texture` into an intermediate texture (`STORAGE_BINDING | TEXTURE_BINDING` — `TEXTURE_BINDING` because the blit *samples* it, not because anything copies it).
2. `wgpu::util::TextureBlitter::copy` from that texture onto the acquired swap-chain view.
3. `present()`.

`tests/window_blit.rs` pins this headlessly: same usage flags, same blitter, asserted pixel-identical to `render_to_buffer`.

## Quirks worth remembering

- **Non-sRGB swap chain, deliberately.** `pick_surface_format` accepts only `Rgba8Unorm` / `Bgra8Unorm`. The intermediate is `Rgba8Unorm` and the blit is a plain copy, so an sRGB swap chain would apply the transfer function a second time.
- **`CompositeAlphaMode::Opaque`.** The renderer emits straight (un-premultiplied) alpha. Presenting opaque means the compositor ignores the alpha channel rather than reading it as premultiplied. A translucent window would need a premultiplying blit shader; that is not built.
- **`size` is physical pixels, `dpi` is `96.0 * scale_factor`.** That pair is what makes theme lengths in pt / mm come out the right physical size on a high-density display. `BASE_DPI` in `mod.rs` is the one place 96 appears.
- **Zero-sized resizes are ignored.** A minimised window reports 0 × 0, which neither a swap chain nor a texture accepts.
- **Redraw is on demand.** A resize schedules a frame; otherwise the app asks via `EventCtx::request_redraw`, or sets `WindowConfig::continuous_redraw`.
- **Picking costs a readback per frame.** When enabled, `render_to_texture` reads the pick target back to CPU and blocks on it every frame — whether or not `pick_at` is called. Off by default for that reason.
- **`resumed` can fire more than once.** The window is built on the first one only; Android-style resume cycles hit this.
- **Errors escape through a field.** `ApplicationHandler` methods return `()`, so a failed frame stores the `WindowError` on the driver and exits the loop; `run` returns it.
- **winit is not in the public API.** `Event` / `MouseButton` / `PresentMode` are ours, so swapping the windowing backend would not be a breaking change. Keep it that way — nothing winit-shaped should appear outside `app.rs`.

## Files

- `mod.rs` — the public surface: `run`, `WindowApp`, `WindowConfig`, `PresentMode`, `Frame`, `EventCtx`, `WindowError`.
- `event.rs` — `Event` and `MouseButton`.
- `surface.rs` — `WindowSurface`: adapter / device selection against the surface, swap-chain config, the intermediate texture, the blit, and `present`.
- `app.rs` — the winit `ApplicationHandler`, window creation, and winit → `Event` translation. The only file that names winit.

## Not built yet

- **wasm entry point.** Everything here compiles for `wasm32-unknown-unknown`, but `run` is `cfg(not(target_arch = "wasm32"))`: the web path needs `EventLoopExtWebSys::spawn_app`, a canvas target, and a non-blocking return contract.
- **Keyboard and scroll events.** `Event` covers resize, cursor, and mouse buttons. Adding more is additive; the enum is not exhaustive-matched anywhere outside `app.rs`.
- **Multiple windows.** One window per `run` call.

## Cross-references

- `backend/` — `WgpuRenderer`, the trait this module hosts, and the texture contract it documents.
- `backend/vello/` — `with_device` / `with_device_and_picking`, and the per-frame pick readback.
- `examples/window.rs` — the end-to-end demo: resize re-layout plus hover picking.
