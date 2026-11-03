# crates/hephaestus-svg-wasm/AGENTS.md

The wasm SVG client: a page loads this, points it at a container element and a
`.hep` document, and gets a plot that reflows on resize and follows
light/dark — with no rasteriser anywhere in the bundle.

Crate `hephaestus-svg-wasm`, npm package **`hephaestus-svg-wasm`** — the same
name, as with the sibling. `-wasm` earns its place on both sides: without it
`hephaestus-svg` reads as the root crate's `svg` feature, which is a different
thing living in `src/backend/svg/`, and a page author installing a wasm module
is better served by a name that says so. One word in the manifest, the import
and the network tab.

The filenames follow the canvas client exactly: package `hephaestus-wasm` has
its entry point at `hephaestus.js`, so package `hephaestus-svg-wasm` has its at
`hephaestus-svg.js`. wasm-pack names the glue from the crate, underscored, so
the wrapper's `import … from './hephaestus_svg_wasm.js'` is the one place the
two spellings meet — `verify-dist.mjs` check 4 is what holds it.

Its own workspace, not a member of the crate above it. See the note in
`../../Cargo.toml`: cargo honours `[profile]` only at a workspace root, so a
member could not carry `opt-level = "z"` / `panic = "abort"` without those
reaching `hephaestus`'s native release builds, where throughput is the point.

## Why this exists beside `hephaestus-wasm`

Both clients read the same documents and present the same five-name Rust seam.
The difference is that this one never rasterises: `svg` implements
`SceneBuilder` and not `Renderer`, so `PlotComposition::render` feeds it
unchanged and what comes back is a string.

Measured, both at `-Oz` with the same `wasm-opt` settings:

| build | `.wasm` raw | brotli |
|---|---|---|
| `hephaestus-wasm`, `webgl` (default) | 3,109,708 | 952,457 |
| **`hephaestus-svg-wasm`** | **2,405,263** | **799,111** |

22.7% smaller raw, 16.1% over the wire. Read that carefully: dropping the
rasteriser is a real cut but not a transformative one, because the bundle is
dominated by this crate's own plot, text and document layers — parley, ICU and
harfrust stay, and shaping is not optional even when the browser does the
drawing. The size is a reason to prefer this client, not the reason it exists.

**The reason it exists is everything it does not have to do.** Each of these is
a section of `../hephaestus-wasm/AGENTS.md` with no counterpart here:

- **No GPU requirement.** `isSupported()` has no analogue — there is no adapter
  to probe and no context to be refused. A page cannot fail to show a plot
  because a browser lacks WebGPU or a driver is blocklisted.
- **No context budget.** A browser caps live WebGL2 contexts (commonly 16) and
  drops the oldest when a page exceeds it. A docs page with twenty plots is
  fine here and is not fine on a canvas.
- **No device pixel ratio.** An SVG has no backing store whose resolution has
  to be chosen; the browser rasterises the markup for whatever display it lands
  on. `CSS_DPI` is a flat 96 and nothing multiplies it. That also removes the
  soft-mismatch row from the placeholder table.
- **No synchronous resize.** Assigning `canvas.width` clears the drawing
  buffer, which is why the canvas client draws inside the `ResizeObserver`
  callback and never defers. Replacing markup clears nothing — the previous
  plot stays on screen until the new one is ready — so every draw here is
  rAF-coalesced and there is nothing to flicker.
- **No async anywhere in the Rust.** `PlotDocument::load` is synchronous
  because there is no device to await.
- **No `web-sys`.** The Rust side returns a `String`; the DOM belongs to JS.
- **No placeholder machinery.** See "First paint" below — the mechanism is
  still there, and it costs no code.

What it costs: **one DOM element per mark.** A 100k-point scatter is 100k
`<path>` elements, which no browser will enjoy. That is the whole reason both
packages exist, and the honest advice is that a dense plot belongs on a canvas.

## The split between Rust and JS

Same rule as the canvas client, one line: **wasm bytes are expensive,
JavaScript is not.** The Rust side is imperative and minimal — load a document,
ask for markup at a size — and everything browser-shaped lives in
`js/hephaestus-svg.js`.

| In `js/hephaestus-svg.js` | Why not Rust |
| --- | --- |
| `ResizeObserver`, `matchMedia`, `requestAnimationFrame` | `Closure::wrap` plumbing plus the `web-sys` features, for a dozen lines of JS |
| `fetch` for fonts | same, and here it keeps `web-sys` out of the tree entirely |
| putting markup in the page | one `innerHTML` assignment |
| **hit testing** | the markup on screen *is* the index; see below |
| the font dedupe `Set` | no reason to spend wasm on a string set |

`PlotView` (JS) is the documented public API. `PlotDocument` (Rust) is the
binding underneath it, and a page can drive it directly if it wants to own
scheduling — or call `renderSvg` for a one-shot with no handle at all.

The seam is six names — `PlotDocument`, `documentFormatVersion`, `hasFonts`,
`registerFont`, `renderSvg`, `setGenericFamily`. Renaming any of them on the
Rust side breaks the wrapper with no compile error, which is what
`verify-dist.mjs` is for.

## Picking is the DOM

The canvas client answers `pickAt` from a CPU spatial index the scene builds as
it draws, and pays for that index on every draw call whether or not anything is
queried — hence `picking: false` by default there.

Here the attributes *are* the index. `SvgConfig::pick_ids` puts `data-pick-id`
on primitives and wraps scopes in `<g data-pick-kind>`, so a hover is
`document.elementFromPoint` plus `closest()`, with nothing crossing into wasm
and no possibility of the answer lagging the frame. It is on by default for
that reason; turn it off for a dense plot that is never hit-tested, where the
attributes are pure weight.

Three attributes, and each row is load-bearing:

| `PickId` | markup | why |
|---|---|---|
| `Id(n)` | `data-pick-id="n"` | the authoring id, reported back |
| `Block` | `data-pick-block=""` | occlusion. **Its own attribute, not `data-pick-id="0"`** — the id space is the full `u32` with nothing reserved, so `Id(0)` is an ordinary mark and one spelling for both would be ambiguous from here |
| `Skip`, in a `Group` scope | `pointer-events="none"` | items beneath stay hittable *through* it, so an unpicked gridline over a mark does not swallow the hit |
| `Skip`, in a `Target` scope | nothing | the primitive **is** the target. This is how chrome participates, having no id of its own |

That last row is the one to understand. Chrome draws with `PickId::Skip` inside
`ScopeMode::Target` frames, so writing `pointer-events="none"` on it — which
the backend used to do unconditionally — left every axis label, tick and title
unhittable in a page while the CPU index happily answered for the same scene.
`SvgScene` now tracks the scope modes and reproduces the indexing rule from
`src/AGENTS.md`.

The consequence for the JS API is that **a hit has two independent halves**,
which is why `PlotView.pick` returns both:

- an **id**, where the primitive carries one — and a geom carries one only when
  it was given a `pick_id` channel. Without one every mark is `Skip`, and the
  document the examples ship is exactly that case: it reports scopes and no
  ids.
- a **scope path** — `composition → plot → region → axis|legend|geom → part →
  item` — which is what identifies chrome, and what lets a hover say "the
  bottom axis's third tick label" rather than only "row 12".

`pickAt` and `pickPathAt` are each one half of `pick`, kept because most
callers want one.

## Fonts: registered twice, and both halves matter

A browser enumerates **no** system fonts, so everything in
`../hephaestus-wasm/AGENTS.md`'s font section applies here unchanged: register
before the first view, exactly one file per (weight, style), WOFF2 is what a
CDN serves and is unwrapped by `wuff`, a generic family is an indirection and
not a name.

**What is different, and is the one genuine trap of this client:** wasm only
*measures*. The markup names a family and the browser draws it. So a face has
to reach two places:

1. **The shaper**, via `registerFont`, so advances exist. Without this the
   layout solves as if every string were empty — collapsed chrome, no labels —
   which is a layout failure, not just a cosmetic one, because tick-label
   widths size the axis tracks.
2. **The page**, via CSS `@font-face`, so the browser can resolve the family
   the markup names. Without this it substitutes, and because every run is
   placed by one anchor plus `textLength`, the substitute is mechanically
   squeezed into a width measured from a different face. That reads as
   *plausible* and is wrong, which is worse than obviously broken.

`registerDefaultFonts` does both: it registers the four bundled Roboto faces
with the shaper and injects an `@font-face` block pointing at the same files.
A page bringing its own font has to do the same on both sides —
`registerGoogleFont` covers the shaper, and the page still needs its own
`<link>` to the same family.

`textLength` is the safety net rather than the plan: it keeps a substituted
face from reflowing the block, so the layout survives even when the face does
not match.

The faces are **not** duplicated in this crate. `build.sh` copies them out of
`../hephaestus-wasm/fonts/`, which is where `generate.sh` produces them — a
second checked-in copy would be half a megabyte of binary free to drift.

## First paint, and why there is no placeholder option

The canvas client boots in ~185 ms and hides that behind a PNG placed in the
served HTML, adopted through `PlotView.create`'s `placeholder` option and
retired in the same task as the first draw.

The same trick works here and needs **no option and no code**, because the
placeholder and the plot are the same format. A producer emits the markup
natively — `examples/document_svg.rs` is exactly that program — and puts it in
the container in the served HTML. `PlotView.create` does not touch the
container until it has markup to put there, so:

- the picture is on screen after an HTML parse, with no image decode and no
  `data:` URL inflating the document by a third;
- the swap is one `innerHTML` assignment of markup produced by the same
  backend from the same document, so nothing can differ structurally;
- there is no "rasterise the document read back, not the composition that
  wrote it" rule to get wrong, because there is no second pipeline;
- and a client that never boots — a failed fetch, a CSP refusal — leaves a
  real, complete, selectable plot on screen rather than a fallback image.

What still has to match is the CSS box and the light/dark choice, for the same
reasons as on the canvas side. The dpi row of that table is simply gone.

## Distribution

An npm package — **`hephaestus-svg-wasm`, unscoped** — consumed from a CDN by
exact-version URL or through a bundler. `publish = false` in `Cargo.toml` refers
to crates.io: this is a `cdylib` artifact, not a library anyone depends on from
Rust.

```html
<script type="module">
  import init, { PlotView } from
    'https://cdn.jsdelivr.net/npm/hephaestus-svg-wasm@0.5.0/hephaestus-svg.js';
</script>
```

The bare name `hephaestus` on npm belongs to an unrelated project, so the
suffix does the work a scope otherwise would — the same reason
`hephaestus-wasm` carries one.

`./build.sh` assembles `dist/`, which is both what the demo loads and what gets
published:

```
dist/
  hephaestus-svg.js          entry point; the wrapper
  hephaestus-svg.d.ts        hand-written types for it
  hephaestus_svg_wasm.js     wasm-bindgen glue
  hephaestus_svg_wasm.d.ts   generated types
  hephaestus_svg_wasm_bg.wasm
  fonts/                     four Roboto faces + the OFL licence
  package.json               from package.template.json, version from Cargo.toml
```

**Three manifests now carry one version**, and `release.yml` checks the tag
against each: `Cargo.toml`, `crates/hephaestus-wasm/Cargo.toml` and this one.
They are in lockstep deliberately, since both clients are views onto this crate
rather than things with their own release cycles. It does mean a release is now
three independent publishes and a third of one is a reachable state; see the
root `AGENTS.md`.

The `npm-svg` job needs a **one-time trusted-publisher registration** on npm for
the `hephaestus-svg-wasm` package naming `release.yml`, plus an `npm-svg`
GitHub environment. Without it the first tag publishes the other two and fails here.

### What `verify-dist.mjs` can check that the canvas client's cannot

Node has no DOM and no GPU, so `../hephaestus-wasm/verify-dist.mjs` can
instantiate the module and stop. Here the entire pipeline — decode, solve,
shape, emit — returns a string, so the package check renders `www/document.hep`
end to end and asserts the markup is well formed, carries real `<text>`, and
*differs* at a second size. That last one is the difference between a document
and a picture, checked rather than assumed.

It is also how the font trap above gets caught: the check registers the faces
and points `sans-serif` at them, and dropping the second half turns the
assertion on `<text>` red.

What it still cannot reach is the browser half — `ResizeObserver`, the markup
swap, `elementFromPoint`. `bench/smoke.mjs` drives the real `PlotView` in a
headless Chrome for those, and CI runs it. See `bench/README.md`; the one
assertion worth knowing about is that a `MutationObserver` watches for an empty
container across a run of resizes, because "there is nothing to flicker" is the
claim this client's whole scheduling model rests on.

## Running the demo

```sh
cargo run --example document_save --features document-write   # examples/document.hep
cp examples/document.hep crates/hephaestus-svg-wasm/www/
cd crates/hephaestus-svg-wasm
./build.sh                           # or ./build.sh --dev for panic messages
python3 -m http.server 8080          # serve the crate dir, not www/
```

Then open <http://localhost:8080/www/>. The server root has to be the crate
directory, because `www/index.html` imports `../dist/hephaestus-svg.js`.

Unlike the canvas client's demo, `python3 -m http.server` is fine: nothing here
depends on the wasm arriving as `application/wasm`. It still should in
production — streaming instantiation is worth having — but a wrong MIME type
only costs the streaming path rather than changing the boot being demonstrated.

## Not built yet

- **`ReadContext` customisation.** Custom geoms and named formatters are not
  reachable from JS, so a document using either fails to load. Same limit as
  the canvas client, and the same reasoning: this renders a prepared plot
  rather than accepting draw commands.
- **Multiple documents per handle.** One `PlotDocument` is one document. The
  argument for a `setDocument` is much weaker here than on the canvas side,
  where it would skip the GL context and the shaders — there is nothing
  expensive to keep warm but the module itself.
- **Page-supplied images.** A document that *embeds* its images renders them
  with no help, which is what `hephaestus/png` is in the dependency list for. A
  document that only *names* one has no way for the page to supply it.
- **Streaming or incremental markup.** Every draw emits the whole document.
  For a resize that is the point — the layout re-solves — but a plot changing
  one mark still replaces everything.

## Cross-references

- `../../src/backend/svg/AGENTS.md` — the backend: why text is `textLength`,
  what degrades, and the picking attributes in detail.
- `../../src/document/AGENTS.md` — what a document carries, and the font
  reasoning both clients are consumers for.
- `../hephaestus-wasm/AGENTS.md` — the canvas client. Its font, distribution
  and document sections apply here almost unchanged; its GPU, placeholder and
  synchronous-resize sections are the ones this client deletes.
- `../../examples/document_svg.rs` — the same pipeline natively, and the
  producer side of the first-paint story above.
- `../../tests/document_svg.rs` — where that pipeline is pinned, since this
  crate is a `cdylib` with no Rust tests of its own.
