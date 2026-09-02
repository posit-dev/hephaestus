# src/backend/hybrid/CLAUDE.md

Vello Hybrid backend: records draws against a `RecordingScene`, replays them into a `vello_hybrid::Scene` once the frame size is known, and rasterises through a wgpu render pipeline.

## What this module does

`HybridScene` is a recorder, not a rasteriser — it delegates every `SceneBuilder` method to `crate::scene::recording::RecordingScene`. `HybridRenderer` owns the wgpu device and queue, and per frame replays the recording into one or two `vello_hybrid::Scene`s, rasterises them into `Target`s, and reads them back.

`Writer` is the replay sink: a `SceneBuilder` that writes into a `vello_hybrid::Scene`. One instance per pass, chosen by `Pass`:

- **`Pass::Display`** — the caller's brushes, blend modes, layer alpha, and antialiasing.
- **`Pass::Pick`** — solid encoded ids, blend and layer alpha dropped, hairline strokes widened, and `set_aliasing_threshold(Some(PICK_ALIASING_THRESHOLD))`.

## Why the scene is recorded rather than written straight through

`vello_hybrid::Scene::new` takes the frame's width and height, and strips are generated as each path arrives — so the size has to be known *before the first draw*. `SceneBuilder` carries no size; the size only shows up at `render_to_buffer`. Recording defers the whole scene until that point.

Two things fall out of it, both load-bearing:

- **The pick pass is a second replay, not a second recording.** The compute-shader backend maintains two `vello::Scene`s and encodes every draw twice; here one recording feeds both passes.
- **A resize replays rather than losing the frame.** `tests/hybrid.rs` pins rendering the same scene at two sizes in a row.

The cost is one extra owned copy of the geometry per frame (`Op` clones paths and brushes). Worth measuring before it is optimised away — the obvious next step is replaying directly out of the plot layer rather than through an op list.

## Quirks worth remembering

- **Aliased picking is the point of this backend.** Coverage is computed CPU-side, so the pick pass can paint with binary coverage: a pixel belongs to exactly one primitive. On two overlapping circles the compute-shader backend yields 28 ids that were never drawn; this one yields two. `tests/hybrid.rs::overlapping_picked_marks_never_blend_into_a_third_id` is that case. Note the geometry has to be *antialiased* to show the difference — axis-aligned rects on integer pixel boundaries have no fringe and both backends agree.
- **`MIN_PICK_STROKE_WIDTH` is load-bearing here, not a nicety.** With binary coverage a stroke thinner than the threshold covers no pixel at all and vanishes from the hitmap entirely, rather than merely fading.
- **The display and pick passes must not share a command buffer.** One
  `vello_hybrid::Renderer` serves both, and rasterising a scene writes that
  frame's coverage, paints and glyph atlas into renderer-owned textures *while
  the pass is being recorded*. Recording both into one encoder therefore lets
  the pick pass's uploads land before the GPU has run the display pass, and the
  display comes back reading the pick pass's binary coverage — aliased,
  glyph-less, and with the wrong paints. Every path submits the display pass
  before recording the pick one; `submit_pick_blocking` is the shared helper,
  and the deferred `submit_pick` was always separate. Two submits per frame is
  the price. `tests/hybrid.rs::picking_does_not_change_the_{buffered,textured}_display`
  pin the invariant, and fail loudly if the submits are ever merged.
- **The pick scene gets no background fill.** An uncovered pick pixel must stay at alpha 0, which is what `pick::decode` reads as a miss.
- **`RENDER_ATTACHMENT`, not `STORAGE_BINDING`.** Rasterisation goes through a render pipeline, so the target is an ordinary colour attachment — which is exactly what a swap-chain texture is, so `window` and `canvas` skip the intermediate texture and its per-frame blit entirely on this backend.
- **The target format is settable, and the pick target follows it.** `set_target_format` exists so a host presenting straight into its swap chain can name that surface's format; `render_to_buffer` ignores it and always uses `Rgba8Unorm`, since that is the byte order it hands out. One `vello_hybrid::Renderer` targets one format, so the pick target takes the display's — meaning a `Bgra8Unorm` surface needs `read_hitmap` to swap red and blue before `pick::decode` sees an id.
- **Output is premultiplied; `unpremultiply` converts on the way out.** Every `Renderer` hands out straight alpha. `tests/hybrid.rs::output_is_straight_alpha` fails loudly if the conversion goes missing.
- **The background is a draw, not a parameter.** `render` takes no base colour, so `replay` fills the frame rect first. It has to stay the first draw.
- **A resize rebuilds the renderer, the scenes, and the image atlas.** `RenderTargetConfig` and `Scene` both fix their dimensions at construction, so `Sized` holds all three together and is replaced wholesale. Uploaded images are invalidated with it.
- **Image opacity cannot ride on the sampler.** `vello_common`'s paint encoder does `unimplemented!("Applying opacity to image commands")` for any `sampler.alpha != 1.0`. `draw_image`'s alpha becomes a `push_opacity_layer` instead. `tests/hybrid.rs::a_translucent_image_fades_instead_of_panicking` pins it.
- **Images must be atlas handles.** The paint encoder matches only `ImageSource::OpaqueId` and panics on `ImageSource::Pixmap`, so every image is uploaded before replay can reference it. `ImageSource::from_peniko_image_data` does the format narrowing and premultiply; we take the pixmap back out of it and upload that.
- **Bitmap color glyphs bypass the rasteriser's own strike path**, and that is not an optimisation — `glyph_bitmap.rs` exists because the upstream path is unusable here twice over. It reaches the GPU only through the glyph atlas, and the atlas takes no rotation or skew; the fallback for anything else is a `Pixmap` paint, which is the panic in the bullet above. And the atlas path paints the strike's *own colors*, so the pick pass reads an emoji back as hundreds of ids that were never drawn — measured, on one 48 px emoji. Resolved as an image instead, a strike costs one atlas upload, survives any transform, and picks as the caller's id.
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
| display pass | 18.7 ms | 84.8 ms |
| display + pick | 31.3 ms | 145.0 ms |
| display + pick, pick pass skipped | 18.7 ms | 85.6 ms |
| compute-shader backend, display | 6.3 ms | 27.4 ms |
| compute-shader backend, display + pick | 9.1 ms | 39.4 ms |

Three things to read off it:

- **The pick pass costs what the display pass costs**, because it is a second
  strip generation over the same geometry. `set_refresh_pick(false)` recovers
  all of it, and `WindowConfig::pick_interval` is how a host throttles it.
- **Recording is a real but minor tax** — about 13 ms of the 100k frame once the
  shared path-building is subtracted. Writing straight into the rasteriser's
  scene would recover it, at the cost of needing the frame size before the
  first draw. Not done.
- **The remaining ~60 ms is strip generation**, against ~16 ms for the other
  backend's GPU equivalent, at both scales. There is no local fix: SIMD level is
  already auto-detected (`Level::try_detect`), and `vello_hybrid` exposes no
  threading feature — only `vello_cpu` does, via rayon. So this backend is
  roughly 3x slower on dense scatter and picking it is a correctness and
  footprint decision, not a speed one.

## The one remaining capacity limit

There is no draw-count ceiling — no `MAX_DRAW_INFO_WORDS` analogue — because GPU buffers are sized to actual content (`create_strips_buffer(device, total_len)`) and grow as needed (`maybe_resize_alphas_tex`). `vello_hybrid::RenderError` has no geometry-capacity variant at all.

The exception is the alpha texture, which holds per-pixel coverage for antialiased strips and is capped at `dim² × 16` bytes where `dim = min(device.max_texture_dimension_2d, 4096)`. 4096 is a vello constant, so better hardware does not raise it and a weak WebGL device reporting 2048 gets a quarter as much. Exceeding it trips an `assert!` — a **panic**, where the compute-shader backend silently blanked the frame. A pre-flight guard belongs here, and unlike the other backend's budget it can be exact: the CPU knows `alphas.len()` before it uploads. Not built yet.

## Dependency version quirks

- **`vello_hybrid` does not re-export the paint types**, so `vello_common` is a direct dependency purely to name `ImageSource` / `PaintType` when building an image paint. Keep its version matched to what `vello_hybrid` resolves.
- **Neither crate re-exports the glyph type**, so `glifo` is a direct dependency for `glifo::Glyph` alone. Same version-matching caveat. Both would be removable if upstream re-exported them.
- **`skrifa` and `png` are not optional for this backend.** Reading a face's bitmap strikes needs the first and decoding one needs the second, and a build without them draws every PNG-strike emoji as nothing. Both are already in the tree — `skrifa` via parley, `png` via this crate's own gate — so requiring them costs a compile of code the default build has anyway.
- **`default-features = false` on `vello_hybrid` drops `wgpu_default`**, which would otherwise pull in every wgpu backend and undo the per-platform tables in `Cargo.toml`. It also drops `text`, so that one is named explicitly — without it there is no glyph pipeline at all.

## Two renderers, one scene layer

`mod.rs` holds everything that needs no GPU API — `HybridScene`, `Writer`, `Pass`, the image collection and the alpha conversion — and the renderers sit beside it:

- **`wgpu_renderer.rs`** (`vello-hybrid`) — `HybridRenderer`: renders to a wgpu texture, implements `Renderer` and `WgpuRenderer`, and reads the pick target back through a mapped buffer.
- **`webgl.rs`** (`webgl`, `wasm32` only) — `HybridWebGlRenderer`: renders to a canvas's WebGL2 default framebuffer through precompiled GLSL, with no wgpu in the build.

Keeping the scene layer GPU-free is what makes the second one possible: a WebGL2 build has no wgpu types to name anywhere.

The WebGL renderer differs in three ways worth knowing:

- **No offscreen target.** Upstream draws to the default framebuffer and nothing else — the field that would redirect it is private — so there is no intermediate texture and no blit. The canvas *is* the target.
- **Picking draws the id buffer to the canvas and reads it straight back**, then overdraws it with the display. Both happen in one JS task and a canvas is not composited until the task yields, so the id frame is never seen. It does mean the pick pass has to go first, and that `readPixels` is synchronous.
- **`readPixels` reads bottom-up**, so the hitmap rows are reversed on the way in. It also implements no `Renderer`: that trait rasterises into a caller's buffer, which here would mean drawing to a visible canvas and reading it back — not what the name promises. Use the wgpu renderer for file output.

## Files

- `mod.rs` — `HybridScene`, `Writer`, `Pass`, and the shared helpers.
- `glyph_bitmap.rs` — bitmap color glyphs: which of a run's glyphs a strike serves, the strike decoded as an image, and where it sits. Placement is Skia's arithmetic, the same the compute-shader backend and `backend/pdf/` carry; `tests/hybrid.rs::a_bitmap_color_glyph_lands_where_the_other_backend_puts_it` holds the two to the same pixels.
- `wgpu_renderer.rs` — `Target`, `SizeBound`, `PendingPick`, `HybridRenderer`.
- `webgl.rs` — `HybridWebGlRenderer`.

Enum mapping lives in `backend/convert.rs` and mesh decomposition in `backend/mesh.rs`, both shared with the other rasterising backend.

## Cross-references

- `backend/` — the `Renderer` trait and the `WgpuRenderer` target contract this backend does not yet satisfy.
- `backend/vello/` — the compute-shader backend, and the picking limitations this one lifts.
- `scene/recording.rs` — `RecordingScene` and `replay`, the mechanism the whole module is built on.
