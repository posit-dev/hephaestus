# bench/ — what the client actually costs, and what the placeholder buys

Three scripts, no dependencies. Node's own `WebSocket` drives the Chrome
DevTools Protocol, and `node:zlib` is enough of a PNG codec for the frames
under test.

Set `HEPHAESTUS_CHROME` if Chrome is not at the macOS default path.

## Setup

Everything here reads the same document and the same picture of it, so
generate both from the repository root:

```sh
cargo run --example document_save --features document-write
cargo run --example document_placeholder --features vello-hybrid,document-read,png
cp examples/document.hplot examples/document.png crates/hephaestus-wasm/www/
cd crates/hephaestus-wasm && ./build.sh
```

Neither artifact is committed — `examples/*.png` and `examples/*.hplot` are
ignored — so a fresh checkout runs those four lines first.

## `pixel-diff.mjs` — is a native pre-render the same picture?

Rasterises nothing itself: it compares `examples/document.png`, written
natively through `vello-hybrid` on wgpu, against the frame the wasm client
draws through the same rasteriser on WebGL2.

This is the measurement the whole placeholder story rests on. A host that
shows a natively-rendered PNG and swaps in the live frame is betting the two
agree.

```
$ node bench/pixel-diff.mjs
live   900x420  (wasm, WebGL2 sparse strips)
native 900x420  (vello-hybrid, wgpu)

differing  0 / 378000 px (0.000%)
```

**Bit-identical**, at 900x420 on macOS arm64. That is a stronger result than
the design needed and it settles a real open question: the two composite
through different shaders — a WGSL render pipeline against precompiled GLSL —
and the wasm build generates strips scalar (`Level::Fallback`, no `+simd128`)
where a native arm64 build uses Neon. Neither difference reaches the output.

Read the report by *distribution*, not by count. Differences confined to
antialiased fringes mean the geometry agrees and the coverage math rounds
differently, which nobody can see. A difference in a *region* — a shifted
tick, a re-wrapped label — means the recipe is wrong: the size, the dpi, the
theme or the fonts. `fully surrounded` is the discriminator, and a deliberate
mismatch shows what one looks like:

```
$ # the same page captured in dark, against the light reference
differing  366048 / 378000 px (96.838%)
  fully surrounded  355239 / 366048
```

Writes `examples/document_live.png` (the captured frame) and
`examples/document_diff.png` (differing pixels in red over a dimmed
reference).

## `swap.mjs` — is first paint immediate, and is the swap invisible?

Drives `bench/swap.html`, which holds nothing but the plot. That matters: the
harness hashes whole composited frames, so a status line that changes when the
renderer finishes would be indistinguishable from the plot changing. `www/` is
the demo; `bench/swap.html` is the instrument.

```
$ node bench/swap.mjs
first contentful paint    68.0 ms (median of 5)
largest contentful paint  68.0 ms on <img>
cumulative layout shift   0, 0, 0, 0, 0

time to plot pixels on screen
  with the picture     57.4 ms
  without it          184.8 ms

3 composited frames in 2 runs of identical pixels:
  6d72d977b0c3  x1   10864 bytes  at 30.0 ms
  3b34049104d8  x2   61107 bytes  at 57.4 ms
```

Two things to read there. **57 ms against 185 ms** is the feature: with the
picture in the served HTML the plot is on screen after an HTML parse and a PNG
decode, instead of after a 3 MB wasm compile. And the picture frame and the
live frame **hash identically** — `3b34049104d8` appearing twice is the swap,
and it is invisible in the composited output as well as in the buffer.

`185 ms` is also the honest cold-boot figure for the `webgl` build, which the
crate had never had: the ~400 ms in `CLAUDE.md` was the `wgpu-backend` path,
where vello compiles ~24 compute pipelines.

The screencast is the only ground truth for when a plot appears, because **a
canvas draw is not a contentful paint** — with no picture the page has no LCP
candidate for the plot at all.

The script also asserts the degradation case: with `?nowasm=1` the picture is
still in the DOM, still opaque, actually loaded, and still the topmost thing at
the plot's centre, so a browser that cannot run the renderer is left with a
real image and its real context menu.

## `server.mjs` — the static server, and why not `python3 -m http.server`

Two of its switches are the experiment rather than a convenience:

- The wasm must arrive as `application/wasm`. Without it the glue falls back
  from `WebAssembly.instantiateStreaming` to `arrayBuffer()` plus
  `instantiate`, and logs a warning — capture the console, it is free signal.
- Whether the transport compresses decides later questions, notably whether
  shipping WOFF2 faces saves anything at all. WOFF2 *is* brotli, so against a
  brotli-serving origin the wire saving is nil and the decode is pure cost.

```sh
node bench/server.mjs --port 8080 [--brotli] [--no-cache] [--wrong-wasm-mime]
```

Serves the crate directory, so `/www/`, `/bench/` and `/dist/` all resolve the
way they do in the published layout.

## Traps worth knowing before measuring anything here

- **`strip = true` in `[profile.release]` removes the wasm name section**, so
  every wasm frame in a CPU profile reads `wasm-function[NNNN]`. Build with
  `strip = false` to profile; it changes no generated code, so the timings stay
  honest.
- **Discard the first run against a fresh headless Chrome.** The GPU process
  spinning up for the first time once measured ~9.7 s here and did not
  reproduce.
- **A frame hash covers the whole viewport.** Anything else on the page that
  changes at the same moment as the plot will read as the plot changing.
