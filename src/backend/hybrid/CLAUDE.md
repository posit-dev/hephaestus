# src/backend/hybrid/CLAUDE.md

Vello Hybrid backend: records draws against a `RecordingScene`, replays them into a `vello_hybrid::Scene` once the frame size is known, and rasterises through a wgpu render pipeline.

## What this module does

`HybridScene` is a recorder, not a rasteriser — it delegates every `SceneBuilder` method to `crate::scene::recording::RecordingScene`. `HybridRenderer` owns the wgpu device and queue, and per frame replays the recording into one or two `vello_hybrid::Scene`s, rasterises them into `Target`s, and reads them back.

`Writer` is the replay sink: a `SceneBuilder` that writes into a `vello_hybrid::Scene`, with the caller's brushes, blend modes, layer alpha and antialiasing. One instance per frame — there is no second pass, because hit testing is a CPU index built as the scene is *drawn* rather than a second thing to rasterise.

## Why the scene is recorded rather than written straight through

`vello_hybrid::Scene::new` takes the frame's width and height, and strips are generated as each path arrives — so the size has to be known *before the first draw*. `SceneBuilder` carries no size; the size only shows up at `render_to_buffer`. Recording defers the whole scene until that point.

Two things fall out of it, both load-bearing:

- **The recording feeds one replay.** It used to feed two — a display pass and an id-buffer pass — which is why the machinery for replaying it twice exists at all. Picking no longer needs it; the recording survives for the reason below it.
- **A resize replays rather than losing the frame.** `tests/hybrid.rs` pins rendering the same scene at two sizes in a row.

The cost is one extra owned copy of the geometry per frame (`Op` clones paths and brushes). Worth measuring before it is optimised away — the obvious next step is replaying directly out of the plot layer rather than through an op list.

## Quirks worth remembering

- **Image opacity cannot ride on the sampler.** `vello_common`'s paint encoder does `unimplemented!("Applying opacity to image commands")` for any `sampler.alpha != 1.0`. `draw_image`'s alpha becomes a `push_opacity_layer` instead. `tests/hybrid.rs::a_translucent_image_fades_instead_of_panicking` pins it.
- **Images must be atlas handles.** The paint encoder matches only `ImageSource::OpaqueId` and panics on `ImageSource::Pixmap`, so every image is uploaded before replay can reference it. `ImageSource::from_peniko_image_data` does the format narrowing and premultiply; we take the pixmap back out of it and upload that.
- **Bitmap color glyphs bypass the rasteriser's own strike path**, and that is not an optimisation — `glyph_bitmap.rs` exists because the upstream path is unusable here twice over. It reaches the GPU only through the glyph atlas, and the atlas takes no rotation or skew; the fallback for anything else is a `Pixmap` paint, which is the panic in the bullet above. Resolved as an image instead, a strike costs one atlas upload and survives any transform. It also stays one pick target rather than becoming one per coloured region — `tests/hybrid.rs::a_bitmap_color_glyph_picks_as_one_id` pins that.
- **Masks are unreachable, deliberately.** `Scene::push_layer` panics on a mask layer, and our `push_layer` has no mask channel, so `None` is always passed.
- **Scene dimensions are `u16`.** `MAX_DIMENSION` is the ceiling; `dimension()` reports anything past it as a `BackendError`.
- **Blend coverage is complete.** All 16 `Mix` and 14 `Compose` variants are mapped upstream, a superset of what `backend/convert.rs` exposes, so no conversion entries are missing.

## Performance shape

Coverage is computed on the CPU, single-threaded, so cost tracks total mark
perimeter in device pixels rather than being handed to the GPU. On a dense
scatter that is the whole story. Measured with
`examples/backend_perf.rs` at 900x560, minimum of ten runs:

| | 20k marks | 100k marks |
|---|---|---|
| building the paths (both backends pay) | 2.3 ms | 11.4 ms |
| recording, before any rasterisation | 4.2 ms | 24.6 ms |
| display pass | 18.7 ms | 83.3 ms |
| display + hit index | — | 102.4 ms |
| compute-shader backend, display | 6.3 ms | 26.9 ms |
| compute-shader backend, display + hit index | — | 46.0 ms |

Three things to read off it:

- **Picking is no longer this backend's problem.** It used to be a second
  strip generation over the same geometry and cost about what the display pass
  did — 145 ms against 85 ms at 100k. It is now a CPU index built while the
  scene is drawn, so both backends pay the same ~19 ms and neither rasterises
  anything twice. The rows above are the pathological case for it (every mark
  a distinct path); a shared marker path interns to ~7 ms. See the picking
  model in `src/CLAUDE.md`.
- **Recording is a real but minor tax** — about 13 ms of the 100k frame once the
  shared path-building is subtracted. Writing straight into the rasteriser's
  scene would recover it, at the cost of needing the frame size before the
  first draw. Not done.
- **The remaining ~60 ms is strip generation**, against ~16 ms for the other
  backend's GPU equivalent, at both scales. There is no local fix: SIMD level is
  already auto-detected (`Level::try_detect`), and `vello_hybrid` exposes no
  threading feature — only `vello_cpu` does, via rayon. So this backend is
  roughly 3x slower on dense scatter and picking it is a correctness and
  footprint decision, not a speed one. That trade used to be partly repaid by
  its aliased pick pass being the correct one; the CPU index makes both
  backends correct, so what remains is footprint — measured at less than half
  the wasm bundle.

## The one remaining capacity limit

There is no draw-count ceiling — no `MAX_DRAW_INFO_WORDS` analogue — because GPU buffers are sized to actual content (`create_strips_buffer(device, total_len)`) and grow as needed (`maybe_resize_alphas_tex`). `vello_hybrid::RenderError` has no geometry-capacity variant at all.

The exception is the alpha texture, which holds per-pixel coverage for antialiased strips and is capped at `dim² × 16` bytes where `dim = min(device.max_texture_dimension_2d, 4096)`. 4096 is a vello constant, so better hardware does not raise it and a weak WebGL device reporting 2048 gets a quarter as much. Exceeding it trips an `assert!` — a **panic**, where the compute-shader backend silently blanked the frame. A pre-flight guard belongs here, and unlike the other backend's budget it can be exact: the CPU knows `alphas.len()` before it uploads. Not built yet.

## Dependency version quirks

- **`vello_hybrid` does not re-export the paint types**, so `vello_common` is a direct dependency purely to name `ImageSource` / `PaintType` when building an image paint. Keep its version matched to what `vello_hybrid` resolves.
- **Neither crate re-exports the glyph type**, so `glifo` is a direct dependency for `glifo::Glyph` alone. Same version-matching caveat. Both would be removable if upstream re-exported them.
- **`skrifa` and `png` are not optional for this backend.** Reading a face's bitmap strikes needs the first and decoding one needs the second, and a build without them draws every PNG-strike emoji as nothing. Both are already in the tree — `skrifa` via parley, `png` via this crate's own gate — so requiring them costs a compile of code the default build has anyway.
- **`default-features = false` on `vello_hybrid` drops `wgpu_default`**, which would otherwise pull in every wgpu backend and undo the per-platform tables in `Cargo.toml`. It also drops `text`, so that one is named explicitly — without it there is no glyph pipeline at all.

## Two renderers, one scene layer

`mod.rs` holds everything that needs no GPU API — `HybridScene`, `Writer`, the image collection and the alpha conversion — and the renderers sit beside it:

- **`wgpu_renderer.rs`** (`vello-hybrid`) — `HybridRenderer`: renders to a wgpu texture and implements `Renderer` and `WgpuRenderer`. Its `Scene` is a `PickIndexScene<HybridScene>`, so `with_picking` is a flag on the scene rather than a second target to allocate.
- **`webgl.rs`** (`webgl`, `wasm32` only) — `HybridWebGlRenderer`: renders to a canvas's WebGL2 default framebuffer through precompiled GLSL, with no wgpu in the build.

Keeping the scene layer GPU-free is what makes the second one possible: a WebGL2 build has no wgpu types to name anywhere.

The WebGL renderer differs in three ways worth knowing:

- **No offscreen target.** Upstream draws to the default framebuffer and nothing else — the field that would redirect it is private — so there is no intermediate texture and no blit. The canvas *is* the target.
- **Picking costs no GPU work at all.** It used to draw the id buffer to the canvas, `readPixels` it synchronously, and overdraw it with the display inside one JS task — which worked only because a canvas is not composited until the task yields. The CPU index removes the whole arrangement, including the ordering constraint it imposed.
- **It implements no `Renderer`**: that trait rasterises into a caller's buffer, which here would mean drawing to a visible canvas and reading it back — not what the name promises. Use the wgpu renderer for file output.

## Files

- `mod.rs` — `HybridScene`, `Writer`, `Pass`, and the shared helpers.
- `glyph_bitmap.rs` — bitmap color glyphs: which of a run's glyphs a strike serves, the strike decoded as an image, and where it sits. Placement is Skia's arithmetic, the same the compute-shader backend and `backend/pdf/` carry; `tests/hybrid.rs::a_bitmap_color_glyph_lands_where_the_other_backend_puts_it` holds the two to the same pixels.
- `wgpu_renderer.rs` — `Target`, `SizeBound`, `HybridRenderer`.
- `webgl.rs` — `HybridWebGlRenderer`.

Enum mapping lives in `backend/convert.rs` and mesh decomposition in `backend/mesh.rs`, both shared with the other rasterising backend.

## Cross-references

- `backend/` — the `Renderer` trait and the `WgpuRenderer` target contract this backend does not yet satisfy.
- `backend/vello/` — the compute-shader backend.
- `pick/` — the hit index both backends' scenes are wrapped in.
- `scene/recording.rs` — `RecordingScene` and `replay`, the mechanism the whole module is built on.
