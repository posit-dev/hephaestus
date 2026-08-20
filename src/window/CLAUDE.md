# src/window/CLAUDE.md

Live presentation, behind the off-by-default `window` and `canvas` features. This is the in-crate host for `WgpuRenderer` — the counterpart to writing an RGBA8 buffer to a file.

## What this module does

Two hosts over one frame path. Both hand the GPU device backing their swap chain to a `VelloRenderer` via `with_device` / `with_device_and_picking`, and both call the same `WindowApp`: `draw(&mut Frame)` per frame, `event(&mut EventCtx, Event)` for everything else.

- **`run(config, app)`** (`window`) opens an OS window and pumps a winit event loop, calling the app back until it exits. Desktop only.
- **`CanvasHost`** (`canvas`, wasm only) attaches to a `<canvas>` already on the page. It is a *handle*, not a driver: the page owns the event loop, so it calls `render` when it wants a frame, `resize` when the element changes size, and `dispatch` to forward an `Event`. `dispatch` returns whether the app asked for a redraw, leaving the page to decide whether to schedule one.

The inversion is the whole difference. A desktop app hands over control; a page keeps it.

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
- **winit is not in the public API.** `Event` / `MouseButton` / `PresentMode` are ours, so swapping the windowing backend would not be a breaking change. Keep it that way — nothing winit-shaped should appear outside `app.rs`. `EventCtx` carries a `&Cell<bool>` rather than calling `request_redraw` directly for exactly this reason: it is what lets the canvas host share the type.
- **The canvas host requests `BROWSER_WEBGPU` only.** WebGL2 has no compute stage and vello rasterises through compute pipelines, so a GL adapter would be found and then fail deep inside pipeline creation. Asking only for WebGPU turns an unsupported browser into an honest `NoAdapter`. `Cargo.toml` doesn't compile the `webgl` wgpu feature on wasm at all, for the same reason.
- **Device acquisition is async on the web.** `WindowSurface::new_async` is the real constructor; `new` is a `pollster::block_on` wrapper over it for the desktop path. A browser main thread has nothing to park.
- **Picking never blocks, and so can lag.** `WgpuRenderer::render_to_texture` parks the calling thread on the pick readback, which a browser main thread cannot do. `CanvasHost` uses `VelloRenderer::render_to_texture_deferring_pick` instead: it submits the readback and moves on, and `try_finish_pick` drains it when it lands. The hitmap can therefore describe a frame or two behind what is on screen. A frame whose predecessor is still in flight skips its own pick submit rather than queueing a second `map_async` on a buffer that is still mapped, which would be a validation error.

## Files

- `mod.rs` — the public surface: `run`, `WindowApp`, `WindowConfig`, `PresentMode`, `Frame`, `EventCtx`, `WindowError`.
- `event.rs` — `Event` and `MouseButton`.
- `surface.rs` — `WindowSurface`: adapter / device selection against the surface, swap-chain config, the intermediate texture, the blit, and `present`.
- `app.rs` — the winit `ApplicationHandler`, window creation, and winit → `Event` translation. The only file that names winit.
- `canvas.rs` — `CanvasHost`: the browser host. The only file that names `web_sys`. Compiled only for `wasm32`, since `wgpu::SurfaceTarget::Canvas` exists nowhere else.

## Not built yet

- **A winit-driven web path.** `CanvasHost` deliberately skips winit: it keeps winit out of a wasm bundle, and winit's web backend wants to own canvas sizing, which a page embedding a plot does not want. Nothing needs `EventLoopExtWebSys::spawn_app` as a result.
- **Keyboard and scroll events.** `Event` covers resize, cursor, and mouse buttons. Adding more is additive; the enum is not exhaustive-matched anywhere outside `app.rs`.
- **Multiple windows.** One window per `run` call.

## Cross-references

- `backend/` — `WgpuRenderer`, the trait this module hosts, and the texture contract it documents.
- `backend/vello/` — `with_device` / `with_device_and_picking`, and the per-frame pick readback.
- `examples/window.rs` — the end-to-end demo: resize re-layout plus hover picking.
- `crates/hephaestus-web/` — the wasm render client built on `CanvasHost`; the page-facing API and the resize / light-dark wiring live there, not here.
