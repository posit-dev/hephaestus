# src/window/CLAUDE.md

Live presentation, behind the off-by-default `window` and `canvas` features. This is the in-crate host for `WgpuRenderer` — the counterpart to writing an RGBA8 buffer to a file.

## What this module does

Two hosts over one frame path. Both hand the GPU device backing their swap chain to a `VelloRenderer` via `with_device` / `with_device_and_picking`, and both call the same `WindowApp`: `draw(&mut Frame)` per frame, `event(&mut EventCtx, Event)` for everything else.

- **`run(config, app)`** (`window`) opens an OS window and pumps a winit event loop, calling the app back until it exits. Desktop only.
- **`CanvasHost`** (`canvas`, wasm only) attaches to a `<canvas>` already on the page. It is a *handle*, not a driver: the page owns the event loop, so it calls `render` when it wants a frame, `resize` when the element changes size, and `dispatch` to forward an `Event`. `dispatch` returns whether the app asked for a redraw, leaving the page to decide whether to schedule one.

The inversion is the whole difference. A desktop app hands over control; a page keeps it.

Layering: this module sits on `backend/` and on nothing above it. It knows about `SceneBuilder` and `VelloRenderer`; it does not know about `plot/`. An app draws a `PlotComposition` by calling `view.render(scene, size, dpi)` itself — the window layer just supplies the three arguments.

## The frame path

How many steps a frame takes depends on the backend, and `Backend::can_present_directly` decides: true when everything the backend asks of a target is something a surface texture already is, which means `RENDER_ATTACHMENT` and nothing more.

**Sparse strips — two steps.** It rasterises through a render pipeline, so it writes the acquired swap-chain texture itself. `WindowSurface` allocates no intermediate and no blitter (`indirect: None`), and the host calls `set_target_format(surface.format())` once so the renderer is built for the surface's format rather than `Rgba8Unorm`. Render, present. One fewer full-screen copy per frame.

**Compute shaders — three steps**, because a surface texture is never a storage binding:

1. `render_to_texture` into an intermediate texture, allocated with `Backend::target_usage() | TEXTURE_BINDING` — the backend's own requirement plus `TEXTURE_BINDING` because the blit *samples* it, not because anything copies it.
2. `wgpu::util::TextureBlitter::copy` from that texture onto the acquired swap-chain view.
3. `present()`.

Both shapes live behind `WindowSurface::draw_frame`, which acquires the frame, hands the renderer whichever view it should draw into, blits if there is an intermediate, and presents. Acquiring *before* rendering is what direct presentation requires, and the blit path does not mind.

`tests/window_blit.rs` pins the indirect path headlessly: same usage flags, same blitter, asserted pixel-identical to `render_to_buffer`. `tests/hybrid.rs::presenting_directly_matches_the_intermediate_format` pins the direct one.

## Quirks worth remembering

- **Non-sRGB swap chain, deliberately.** `pick_surface_format` accepts only `Rgba8Unorm` / `Bgra8Unorm`. The intermediate is `Rgba8Unorm` and the blit is a plain copy, so an sRGB swap chain would apply the transfer function a second time.
- **The target contract is the backend's to state, not this module's.** `WgpuRenderer::REQUIRED_TARGET_USAGE` and `TARGET_IS_PREMULTIPLIED` both differ between backends. `surface.rs` takes the usage as a constructor argument and remembers it so a resize reallocates the same way; it hardcodes only `BLIT_USAGE`, which is its own need.
- **Alpha convention differs by backend and the blit ignores it.** The compute-shader backend writes straight alpha, the sparse-strip one premultiplied. Presenting `CompositeAlphaMode::Opaque` makes that irrelevant — the conventions coincide at alpha 255 — which is the only reason one blit serves both. A translucent window would have to consult `TARGET_IS_PREMULTIPLIED` and convert.
- **`CompositeAlphaMode::Opaque`.** The renderer emits straight (un-premultiplied) alpha. Presenting opaque means the compositor ignores the alpha channel rather than reading it as premultiplied. A translucent window would need a premultiplying blit shader; that is not built.
- **`size` is physical pixels, `dpi` is `96.0 * scale_factor`.** That pair is what makes theme lengths in pt / mm come out the right physical size on a high-density display. `BASE_DPI` in `mod.rs` is the one place 96 appears.
- **Zero-sized resizes are ignored.** A minimised window reports 0 × 0, which neither a swap chain nor a texture accepts.
- **Redraw is on demand.** A resize schedules a frame; otherwise the app asks via `EventCtx::request_redraw`, or sets `WindowConfig::continuous_redraw`.
- **Picking costs CPU while drawing, not a second rasterisation.** `WindowConfig::picking` makes the scene record a hit index as the frame is authored — measured at ~7 ms per 100k marks, against the ~59 ms the old second rasterisation cost. It is still off by default, because filling the index is paid whether or not `pick_at` is ever called. There is no throttle any more and none is needed: the R-tree is built lazily on the first query after a frame, so a window redrawing faster than it is queried never builds one.
- **`resumed` can fire more than once.** The window is built on the first one only; Android-style resume cycles hit this.
- **Errors escape through a field.** `ApplicationHandler` methods return `()`, so a failed frame stores the `WindowError` on the driver and exits the loop; `run` returns it.
- **winit is not in the public API.** `Event` / `MouseButton` / `PresentMode` are ours, so swapping the windowing backend would not be a breaking change. Keep it that way — nothing winit-shaped should appear outside `app.rs`. `EventCtx` carries a `&Cell<bool>` rather than calling `request_redraw` directly for exactly this reason: it is what lets the canvas host share the type.
- **Two browser hosts, and which one a bundle carries is a feature choice.** `CanvasHost` goes through wgpu and therefore needs WebGPU; `WebGlHost` talks WebGL2 directly and needs neither. `EventCtx` holds a `&PickIndex` rather than a renderer, which is what lets both build the same event context — a WebGL2 build has no wgpu types to name, and the index is the same type either way.
- **The canvas host requests `BROWSER_WEBGPU` only, on either wgpu backend.** For the compute-shader one it is forced: WebGL2 has no compute stage, so a GL adapter would be found and then fail deep inside pipeline creation. The sparse-strip backend has no such constraint — it rasterises through a render pipeline — but `Cargo.toml` still does not compile wgpu's `webgl` feature on wasm, so there is no GL adapter to find either way. Enabling wgpu's `webgl` feature would work but costs +1.2 MB, since it drags in shader translation. Going below WebGPU is instead `WebGlHost`'s job, which talks WebGL2 directly and carries no wgpu at all.
- **Device acquisition is async on the web.** `WindowSurface::new_async` is the real constructor; `new` is a `pollster::block_on` wrapper over it for the desktop path. A browser main thread has nothing to park.
- **Picking never blocks and never lags.** It used to do both: the readback parked the calling thread, which a browser main thread cannot do, so `CanvasHost` submitted it and drained it a frame or two later — and the answers described an older frame. With the index there is nothing to read back, so `render_to_texture` is the same call on every host and a query always describes what is on screen.

## Files

- `mod.rs` — the public surface: `run`, `WindowApp`, `WindowConfig`, `PresentMode`, `Frame`, `EventCtx`, `WindowError`. Also the `compile_error!` that fires when a presentation feature is enabled with no backend behind it.
- `renderer.rs` — `Backend` (public selection) and `HostRenderer` (the boxed per-backend enum). Compiled only where a wgpu backend is.
- `webgl_host.rs` — `WebGlHost`: the same `render` / `resize` / `dispatch` surface as `CanvasHost`, against a canvas's WebGL2 context. No surface, no swap chain, no blit — the canvas's default framebuffer is the target — and no wgpu in the build at all.
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
- `backend/vello/` and `backend/hybrid/` — `with_device` / `with_device_and_picking`, which is the flag on each renderer's `PickIndexScene`.
- `pick/` — the index `EventCtx` queries.
- `examples/window.rs` — the end-to-end demo: resize re-layout plus hover picking.
- `crates/hephaestus-wasm/` — the wasm render client built on `CanvasHost`; the page-facing API and the resize / light-dark wiring live there, not here.
