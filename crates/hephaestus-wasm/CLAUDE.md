# crates/hephaestus-wasm/CLAUDE.md

## Two backends, chosen at build time

`webgl` is the **default**: the sparse-strip rasteriser talking WebGL2 directly, with no wgpu in the bundle and no WebGPU requirement, so it runs wherever a canvas does. `wgpu-backend` swaps in the compute-shader path on `hephaestus/vello` + `canvas`, which is the more mature renderer but needs WebGPU. Naming `wgpu-backend` wins even alongside the default, so `--features wgpu-backend` is enough to switch — no `--no-default-features` needed.

`isSupported()` tests whichever applies: `navigator.gpu` on a wgpu build, an actual `webgl2` context request on the default, since a browser can expose the API and still refuse a context. The failure text in the wrapper and the demo page names both cases, because the JS cannot tell which backend it was built against.

Measured on this client, same release and `wasm-opt` settings:

| build | `.wasm` bytes |
|---|---|
| `webgl` (default) | 2,914,666 |
| `vello-hybrid` + `canvas` | 2,941,297 |
| `wgpu-backend`, i.e. `vello` + `canvas` | 3,252,703 |

Read that carefully: **the default's advantage is reach, not size.** It is ~10% smaller than the wgpu build, and almost all of that comes from swapping the rasteriser rather than from dropping wgpu — wgpu is worth only ~27 kB here, because the bundle is dominated by this crate's own plot, text and document layers. What WebGL2 buys is running at all on the browsers that have no WebGPU.

One consequence of the swap worth knowing: the WebGL2 context is created without `preserveDrawingBuffer`, so its drawing buffer is cleared once composited. `saveOnRightClick` survives that only because it re-renders **synchronously** immediately before each capture — the same thing Safari already required of it. Nothing may `await` between that render and `toDataURL`.

## Why `verify-dist.mjs` scans for undefined calls

Node has no DOM, so nothing here can exercise the canvas paths — which is how three PNG helpers stayed referenced-but-unwritten long enough to ship. The failure was silent twice over: a bare `catch` swallowed the `ReferenceError`, and an `<img>` with no `src` degrades to an ordinary element rather than erroring, so a right-click just showed the wrong menu. The static scan is crude and will flag a false positive if the wrapper ever gains an unusual declaration form, but it catches the whole class, and the alternative was finding out from a user.


The wasm render client: a page loads this, points it at a `<canvas>` and a
`.hplot` document, and gets a plot that reflows on resize and follows
light/dark.

Its own workspace, not a member of the crate above it. See the note in
`../../Cargo.toml`: cargo honours `[profile]` only at a workspace root, so a
member could not carry `opt-level = "z"` / `panic = "abort"` without those
reaching `hephaestus`'s native release builds, where throughput is the point.
The cost is a second lock file and no shared `target/`.

## The split between Rust and JS

Deliberate, and the rule is one line: **wasm bytes are expensive, JavaScript
is not.** So the Rust side is imperative and minimal — `render`, `resize`,
`setDark`, plus the two font entry points — and everything browser-shaped
lives in `js/hephaestus.js`:

| In `js/hephaestus.js` | Why not Rust |
| --- | --- |
| `ResizeObserver`, `matchMedia`, `requestAnimationFrame` | `Closure::wrap` plumbing plus the `web-sys` features, for logic that is a dozen lines of JS |
| `fetch` for fonts | same, and it keeps `web-sys` down to `HtmlCanvasElement` |
| frame coalescing | scheduling policy belongs where the event loop is |
| the font dedupe `Set` | no reason to spend wasm on a string set |

`PlotView` (JS) is the documented public API. `PlotHandle` (Rust) is the
binding underneath it, and a page can drive it directly if it wants to own
scheduling.

The seam is five names — `PlotHandle`, `isSupported`, `documentFormatVersion`,
`registerFont`, `setGenericFamily`. Renaming any of them on the Rust side
breaks the wrapper with no compile error, which is what `verify-dist.mjs`
is for.

## Fonts are the thing that surprises people

A browser enumerates **no** system fonts: fontique falls back to a dummy
backend, so the collection starts empty and `sans-serif` resolves to nothing.
A page that registers no font gets a plot with chrome and no text — no error,
no warning, just missing glyphs. Hence:

- `registerFont` **errors rather than reporting nothing registered**, so a
  blob that holds no faces is loud instead of silently textless.
- It accepts `wOF2` / `wOFF` alongside sfnt, unwrapping the container before
  the shaper sees it — fontique itself ingests sfnt only, and WOFF2 is what a
  font CDN hands a browser, so this was the likeliest input to fail. See
  "WOFF and WOFF2 are decoded" below.
- **`registerFont` returns the family names it registered**, which is what
  makes `setGenericFamily` usable: a generic is an indirection through the
  font context rather than a name, so registering Inter does not make
  `sans-serif` mean Inter — and the only place a family's name exists is
  inside the file. Deriving it from the filename looks fine until the first
  real font, where it silently resolves to nothing and every glyph vanishes.
  `text::register_font_families` is the accessor that exists for this.
- `registerFontFromUrl(url, { genericFor })` therefore chains the two: it
  registers, takes the names back, and points the generic at them.
- Registration is process-global and permanent, so it is once per page, not
  once per plot. Order matters: it must precede the first `PlotView.create`,
  since decoding a document's theme is already enough to shape.

### One file per weight and style — the trap worth knowing

The shaper selects within a family by **weight, width and style, with no notion
of CSS `unicode-range`.** A CDN meanwhile splits every face into per-script
subset files that all carry the same family name. Register several and one
without basic Latin can win the attribute match, at which point every tick
label becomes a tofu box while the bold title still renders, because a
different subset won for weight 700. Observed exactly that way before it was
fixed.

So: **exactly one file per (weight, style)**. `registerGoogleFont` enforces it
and takes `subset` to say which. Hand-registering means doing the same.

### Google Fonts needs no API key

Since WOFF2 is decoded, `registerGoogleFont` uses the keyless CSS2 endpoint
rather than the Developer API. CORS allows the `fetch`, and the WOFF2 it
returns — the only thing it will return to a page, `User-Agent` being a
forbidden header — is now something the shaper can take.

### Variable fonts work, and are the best answer for coverage

The shaper applies the `wght` axis: measured 399.93 / 403.73 / 411.24 / 420.89
px for one string at weights 400 / 500 / 700 / 900 off a single variable file.
So a variable font serves **every** weight, including interpolated ones no
static set reaches, from one registration — sidestepping both the
one-file-per-face cap and the subset choice. Italic stays a second file.

| want | route | cost |
| --- | --- | --- |
| zero setup | bundled default (4 static faces, 5 scripts) | 258 kB brotli |
| a site's own typography | `registerGoogleFont`, one subset | ~4 × 10–20 kB |
| full Unicode + all weights | variable font via `registerFontFromUrl` | Inter 365 + 403 kB; Roboto 231 + 266 kB |

Static instances stay right for the *bundle* — subsetted, they are half what
Roboto's variable pair costs for the same scripts — and variable is right for a
page that wants everything.

**There is no full-coverage file on the CSS endpoints, so `subset` has no "all"
value.** One weight returns seven `unicode-range`-split files, and the legacy
`&subset=` no longer merges them. Full coverage keyless means the raw variable
TTF in `google/fonts`, and a *general* helper for that is not buildable: the
filename embeds each family's axis list (`[wght]` vs `[opsz,wght]` vs
`[wdth,wght]`), the licence directory varies (`ofl` / `apache` / `ufl`),
jsDelivr's data API rejects subpath listing, and the GitHub contents API is
rate-limited per IP. So it stays `registerFontFromUrl(<url you supply>)`.

Getting several CSS subsets into one family would mean rewriting each file's
`name` table to a distinct family and chaining them through
`setGenericFamily`, which does accept a fallback list. Not built, and hard to
justify against fetching one variable file.

### Can we reuse the fonts the page already loaded?

Almost entirely **no**, and it is worth knowing why before anyone tries.
Probed in Chrome, not reasoned about:

- **`document.fonts` exposes descriptors only.** A `FontFace`'s whole surface
  is `family`, `style`, `weight`, `stretch`, `unicodeRange`, `variant`,
  `featureSettings`, `display`, the metrics overrides, `status`, `loaded`,
  `load`, `variationSettings`. There is no `url`, no `src`, and no accessor
  for the data — not even for a face the page itself constructed from an
  `ArrayBuffer`. Fonts the browser has loaded are unreachable by design.
- **CSSOM can recover the `src` URL, but only same-origin.** An inline or
  same-origin sheet yields `CSSFontFaceRule`s whose
  `style.getPropertyValue('src')` reads `url("./local.ttf") format("truetype")`.
  A cross-origin sheet — which is what a Google Fonts `<link>` is — throws
  `SecurityError` on `cssRules`.
- **`queryLocalFonts()` does hand back real bytes** via `FontData.blob()`, so
  it is the one route to genuine system fonts. It needs a user gesture
  (`SecurityError: User activation is required` without one) plus a permission
  prompt, and it is Chromium-desktop-only. Viable behind a button; never
  automatic.

Even with perfect discovery the format defeats it: a site's web fonts are
WOFF2, which fontique rejects. So auto-discovery would find precisely the
fonts that already work (self-hosted TTF/OTF) and skip everything else. The
blocker is WOFF2, not the discovery.

The deeper reason none of this can be fully automatic: vello needs real
outlines to shape and rasterise. A browser will happily *render* text in a
font it will not hand over, and there is no path from that to a vello scene
short of rasterising to a 2D canvas and uploading an image — which throws away
vector output and picking.

### The bundled default font

`PlotView.create` fetches Roboto when `hasFonts()` says nothing is registered,
so an embed works with no font code at all; a page that registers its own font
first never fetches it. `defaultFont: false` opts out.

**Four static instances, not one variable font.** Variable fonts look like the
obvious answer and are the wrong one here: `gvar` deltas survive charset
subsetting, so a variable roman/italic pair at this coverage is ~1 MB *raw*,
larger than the renderer. Measured, brotli, four faces:

| face | coverage | size |
| --- | --- | --- |
| Inter | latin | 151 kB |
| Inter | latin + latin-ext | 286 kB |
| **Roboto** | **latin + latin-ext + Greek + Cyrillic + Vietnamese** | **258 kB** |

Roboto gives four faces and three extra scripts for less than Inter costs at
latin-ext alone, which is why it is the default. The trade for static instances
is that a theme asking for weight 500 snaps to 400 or 700 rather than
interpolating; the built-in themes use only 400 and 700.

**All four faces are required, not a luxury.** The default theme sets a bold
plot title, and the rich-text sheet maps `weight: 700` and `italic: true`
independently, so markdown chrome reaches bold-italic through nested
`***emphasis***`. Three faces plus synthetic oblique would look visibly cruder
at label sizes.

**CJK is out of scope** — a CJK face is megabytes, and no bundling decision
closes that. It stays a bring-your-own case.

Licensing: Roboto is **OFL-1.1** (Google relicensed it from Apache-2.0; it now
lives under `ofl/roboto/` in `google/fonts`). No Reserved Font Name is
declared, so an instanced, subsetted derivative may keep the family name —
which matters, since the theme refers to it by name. `fonts/OFL-Roboto.txt`
ships with the faces, as the licence requires; it covers the fonts, not the
crate.

The faces are **committed** under `fonts/`, regenerated by `fonts/generate.sh`
when the font or coverage changes, so `build.sh` and CI need neither network
access nor fontTools. They are fetched at runtime rather than embedded in the
wasm, so a page with its own font transfers none of them.

**Google Fonts cannot work the way the `google-fonts` cargo feature does.**
That feature relies on sending *no* `User-Agent`, which is what makes the CSS2
API serve TTF instead of WOFF2 (see the comment on `http_get_string` in
`src/text/google_fonts.rs`). `User-Agent` is a forbidden header, so `fetch`
can neither remove nor override it, and a page always gets WOFF2. That is why
`registerGoogleFont` uses the Developer API v1 — whose `files` map is TTF —
and why it needs a key. A keyless route would need either a WOFF2 decoder or
a TTF mirror; neither is built.

One thing that *is* settled: `fetch`ing `fonts.googleapis.com/css2` is
**allowed by CORS** (probed, not assumed). What it returns depends on the user
agent it sees: a Chrome-131 UA gets `format('woff2')` with `.woff2` URLs, a
Safari-17 UA gets `.woff`. A real browser therefore gets WOFF2, so a keyless
path is gated purely on decoding that — not on reaching the API.

### WOFF and WOFF2 are decoded

Behind the `webfonts` feature, on by default. `registerFont` unwraps a `wOF2`
or `wOFF` container to the sfnt inside before the shaper sees it, so a URL from
a font CDN works as-is — which matters, because WOFF2 is what a CDN serves a
browser and was therefore the likeliest input to fail.

`wuff` does the work: pure Rust, no build script, and `brotli-decompressor`
(the decode-only half) rather than full `brotli`. Chosen over `woff2-patched`
(full `brotli` plus four more deps) and `woofwoof` (needs `cc`); it is also
Linebender-adjacent, same ecosystem as parley and fontique.

**Cost: +212 kB raw but +68.5 kB brotli, +8.0% over the wire** — about a
quarter of what the four bundled faces cost. An earlier note here predicted
this would be dominated by brotli's ~120 kB static dictionary; that is true of
the raw figure and misleading about transfer, because the dictionary is
text-like and compresses well itself. Reason about the compressed number.

Correctness was checked against the real thing. Decoding the Inter WOFF2 that
Google serves a browser yields exactly the 21312-byte sfnt its header promises,
fontique registers it, the outlines render, and `hmtx` advances match the
reference full TTF on **all 224 glyphs** at the same upem. That last check is
the one that matters: a bad `glyf`/`loca` reconstruction gives plausible
metrics with mangled outlines, so structural validity proves nothing.

What a WOFF2 actually demands, from that same file:

```
flavor  TrueType (glyf)   15 tables, 21312-byte sfnt from 10236 compressed
glyf    13944 bytes  TRANSFORMED
loca      450 bytes  TRANSFORMED
others               null transform (brotli only)
```

`glyf` + `loca` are 65% of the font and are genuinely transformed rather than
merely compressed — seven parallel sub-streams, triplet-packed point deltas, a
bbox bitmap, composite passthrough, `loca` rebuilt at the right offset width.
Not something to hand-roll, which is the argument for the dependency.

Borrowing brotli from the browser was investigated and rejected. The format
name is `'brotli'`, not `'br'`, and it works in Safari 26 and Firefox 154 but
**not Chrome 151**, so a bundled decoder is needed regardless; a constructed
`Response` carrying `Content-Encoding: br` decodes nothing, since content
coding is applied by the network layer.
`wuff::decompress_woff2_with_custom_brotli` takes a closure and would allow
supplying the platform decoder where present, saving those 68.5 kB on two of
three engines at the cost of an async boundary through `registerFont`. Not
built; at 68.5 kB it is hard to justify.


## Startup cost, and the placeholder trap

Measured in headless Chrome: importing the wrapper and fetching the wasm is
~17 ms, instantiating it ~6 ms, the font ~10 ms, the document ~10 ms, and
`PlotView.create` — adapter, device, and vello's compute-pipeline compilation
— dominates the rest. Ready at roughly **400 ms** from navigation, near
enough identical on a cold profile and a warm one, so the browser's shader
cache is not what is being waited on. A subsequent redraw is ~0 ms, which is
what identifies the cost as one-time pipeline setup rather than per-frame work.

(One early measurement showed ~9.7 s here. That was the GPU process spinning
up for the first time in a fresh headless Chrome, not the page: it did not
reproduce on any later run, cold profile included. Treat ~400 ms as the figure
and re-measure on real hardware.)

So the wasm module is not why a page waits, and shrinking the bundle will not
help it. The honest mitigation is to tell the viewer something is coming.

**A placeholder must not be drawn into the canvas.** Calling
`getContext('2d')` on it makes the later `getContext('webgpu')` return `null`,
permanently — a canvas commits to one context type. Verified, not assumed. Use
an overlay element (what `www/index.html` does) or a CSS background on the
canvas; never a 2D context.

`PlotView.create` resolves only after the first frame has been drawn — the
constructor's `_syncSize` renders synchronously — so a host can clear its
placeholder immediately after awaiting it, with no extra readiness signal.

## Saving a PNG needs no configuration, but does need an `<img>`

`canvas.toDataURL()` / `toBlob()` work on the WebGPU canvas as-is (verified:
78 kB of real content off a 900×420 plot). `preserveDrawingBuffer` is a WebGL
concern with no WebGPU counterpart to set here; the surface's
`RENDER_ATTACHMENT` usage and `CompositeAlphaMode::Opaque` are already what a
clean readback wants.

What does *not* come for free is the affordance. A `<canvas>` never offers
"Save image as…", "Copy image" or drag-to-save: those come from the hit-test
node being an image, and a `contextmenu` handler cannot retarget the menu. So
`saveOnRightClick` overlays a transparent, exactly-coincident `<img>` over the
canvas and keeps its `src` refreshed from the live frame — the canvas below
renders, the image above is what the menu acts on. Verified with
`elementFromPoint` at the plot's centre returning `IMG`.

Encoding is lazy, on pointer entry / right-or-ctrl mousedown / `contextmenu`,
because a PNG per frame would be pure waste. `mousedown` is in that list
deliberately: a browser gathers the menu's image URL during hit testing, which
can precede the `contextmenu` dispatch, so refreshing only on `contextmenu`
risks saving the previous frame.

**Every capture re-renders first, and Safari requires it.** Safari returns an
all-black snapshot — opaque black, `[0,0,0,255]`, not transparent — unless a
render happened recently: its drawable is consumed once composited, where
Chrome and Firefox retain the presented image. Measured across four capture
orderings in Safari via `safaridriver`:

| ordering | result |
| --- | --- |
| capture long after the last render | all black |
| render, then capture in the same task | correct |
| inside `requestAnimationFrame`, render then capture | correct |
| capture with a render having just happened | correct |

So `refresh()` calls `_renderNow()` unconditionally before `toDataURL`. Do not
"optimise" that away on the grounds that the canvas already shows the right
thing — it does, and the snapshot still comes back black.

Note what was *not* the cause, since it is the tempting guess: adding
`COPY_SRC` to the surface's `GPUCanvasConfiguration.usage` changes nothing.
Tested by A/B — black before the re-render fix with `COPY_SRC` present,
correct after it with `COPY_SRC` absent.

**The exported PNG carries its resolution.** A canvas writes no `pHYs` chunk,
so every viewer falls back to its own default (72 dpi typically) and a plot
rendered at 2× device pixels claims twice its intended physical size. The
overlay therefore splices a `pHYs` in — after `IHDR`, where the spec wants it,
replacing any existing one — from the same dpi the frame was rendered at. On a
2× display: 1504×840 pixels at 7559 ppm, which is 192 dpi and the right
physical size. **The bytes must reach the `<img>` as a `data:` URL, not a `blob:` one.** A
blob is the obvious choice once the bytes have been rewritten — it avoids
re-base64-ing ~160 kB — but Safari's image context menu degrades for `blob:`
sources: "Save Image to Downloads" / "Save Image As…" collapse to a single
"Get Picture". So the patched bytes are re-encoded to a data URL, chunked
because `String.fromCharCode(...bytes)` overflows the argument limit on
anything large. The URL scheme is load-bearing for the affordance, which is
not obvious from anything about the element itself.

`src/image/png.rs` writes no `pHYs` either, so the native writers have the
same gap. Nothing here depends on that, but a `write_png` that took a dpi
would let the two paths agree.

Two consequences worth knowing: pointer events land on the overlay rather than
the canvas, so a host doing hover picking should listen on the container (they
bubble, and `offsetX` / `offsetY` are in the same coordinate space); and the
overlay sets `position: relative` on the canvas's parent if it is `static`.

For a *different* size than the canvas, re-render rather than scaling the
export — that is the whole point of shipping a document.

## Resize must draw synchronously

Assigning `canvas.width` / `canvas.height` **clears the drawing buffer**. A
`ResizeObserver` callback runs after layout but *before* paint, so the two
orderings differ visibly:

- draw inside the callback → the new frame lands in the same paint;
- defer to `requestAnimationFrame` → the browser paints the cleared buffer
  first, then the frame arrives a paint later. That reads as a flicker on
  every step of a drag, plus a frame of latency.

So `_onObserved` calls `_renderNow()`, never `_schedule()`. `_schedule` exists
only for changes that leave the buffer intact — a theme swap — where batching
is worth a frame.

This is a scheduling problem, not a throughput one, and it is worth not
misdiagnosing: a full frame here — re-solve the layout, re-shape every string,
rasterise, blit — measured **~1.8 ms mean** on a software rasteriser in
headless Chrome. Neither WebGPU nor the intermediate-texture blit is the
bottleneck at any plausible drag rate.

The observer asks for `box: 'device-pixel-content-box'` so the backing store
matches the exact device-pixel box with no rounding drift against
`devicePixelRatio`; `observe()` throws where that box is unsupported, and the
fallback is the CSS box times the ratio. Sizes come off the observer entry
rather than `clientWidth`, which would force a layout flush inside the
callback.

## WebGPU is required

Vello rasterises through compute pipelines and WebGL2 has no compute stage,
so there is no fallback: `isSupported()` is a hard gate, not a preference.
`CanvasHost` asks for `Backends::BROWSER_WEBGPU` alone so an unsupported
browser fails at adapter selection rather than inside pipeline creation, and
the root `Cargo.toml` does not compile wgpu's `webgl` feature on wasm at all.

## Light and dark

`PlotHandle` keeps the theme **exactly as the document carried it** and
derives each mode from that clone. Inverting the live theme in place would
work once and drift on the second identical call; deriving makes
`setColorScheme` idempotent.

The canvas clear colour is keyed to `theme.palette.paper` rather than being a
separate setting, which is what makes it invert for free — the background
lives outside the theme, so anything else would need its own light/dark rule.

Known limitation, worth repeating to anyone who asks why their points stayed
blue: `Theme::invert` swaps paper and ink only. `palette.accent` is untouched
and `GeomTheme`'s colour fields default to `None`, so a geom given an explicit
`fill` keeps it. Marks adapt only when the plot expressed them as palette
references.

## Not built yet

- **`ReadContext` customisation.** Custom geoms and named formatters are not
  reachable from JS, so a document using either fails to load. Consistent with
  rendering a prepared plot rather than accepting draw commands.
- **Multiple documents per handle.** One `PlotHandle` is one document; a page
  with several plots creates several.

## Distribution

An npm package — **`hephaestus-wasm`, unscoped** — consumed either from a CDN
by exact-version URL or through a bundler. `publish = false` in `Cargo.toml`
refers to crates.io: this crate is a `cdylib` artifact, not a library anyone
depends on from Rust.

The bare name `hephaestus` on npm belongs to an unrelated project, so the
`-wasm` suffix does the work a scope otherwise would. `ggsql-wasm` in the
sibling repo carries the suffix for the same reason, and the crate, this
directory and the wasm artifacts all share the name so a consumer sees one
word in the manifest, the import and the network tab.

`./build.sh` assembles `dist/`, which is both what the demo loads and what
gets published:

```
dist/
  hephaestus.js          entry point; the wrapper
  hephaestus.d.ts        hand-written types for it
  hephaestus_wasm.js     wasm-bindgen glue
  hephaestus_wasm.d.ts   generated types
  hephaestus_wasm_bg.wasm
  package.json           from package.template.json, version from Cargo.toml
```

**One layout for development and publication, deliberately.** Keeping the
wrapper in `js/` and the glue in `pkg/` meant the wrapper imported the glue as
`../pkg/hephaestus_wasm.js` locally and would need `./hephaestus_wasm.js` once
published — a discrepancy that cannot fail until after a release. Now both are
`./`, and `www/index.html` loads `../dist/hephaestus.js`, the same entry point
npm serves.

`--no-pack` is passed to wasm-pack because its own `package.json` lists only
its three outputs and points `main` at the glue rather than at the wrapper.

`verify-dist.mjs` is the guard, and CI runs it: it loads the entry point the
way a consumer does, instantiates the wasm from the published bytes, and checks
the manifest against the directory — every `files[]` entry exists, `exports`
points at something published, the version matches the crate, the wrapper
imports the glue by a package-relative path, every public name is both exported
and declared in the `.d.ts`. Those are the failures that otherwise surface only
after publishing.

### Publishing

A `v*` tag is the trigger, and nothing else publishes. The `npm` job in
`release.yml` assembles `dist/`, runs `verify-dist.mjs`, and publishes from
that same directory, so nothing is rebuilt between the check and the upload.
It is a separate workflow from `check.yml` because npm registers a trusted
publisher by workflow filename, and a release file that does nothing else is
a narrower thing to trust. `check.yml` still assembles and verifies the
package on every push — it just stops at `npm pack --dry-run`.

A tag containing `-dev` or `-rc` publishes under the `next` dist-tag so
`npm install hephaestus-wasm` keeps resolving to the last stable release.

`--provenance` is worth its `id-token: write` here: SRI does not work for ES
module imports, so a verifiable link back to the workflow run is the only
integrity story a CDN consumer gets. The credential lives in the `npm` GitHub
environment, not a bare repo secret.

Three things pin the version together, because each alone leaves a gap.
`build.sh` reads it from `Cargo.toml`, `verify-dist.mjs` fails if the manifest
and the crate disagree, and `release.yml` fails — before it builds
anything — if the tag disagrees with `Cargo.toml`. Otherwise a mistyped tag
publishes whatever the manifest happened to hold, which on a first publish
succeeds under the wrong number.

### Pinning, and the coupling that matters

For a consumer, the version *is* the URL:

```html
<script type="module">
  import init, { PlotView } from
    'https://cdn.jsdelivr.net/npm/hephaestus-wasm@0.1.0/hephaestus.js';
</script>
```

The glue resolves its wasm through `new URL('hephaestus_wasm_bg.wasm',
import.meta.url)`, so a CDN needs no configuration and Vite handles the pattern
natively; a bundler that does not can pass `init({ module_or_path })`.

**The document format major version is a hard compatibility boundary, and
semver has to carry it.** `read_composition` refuses a major it does not know —
equality, not a floor — so a site pinned to one release can only read documents
written at the same major. `documentFormatVersion()` exposes it so a build step
can assert rather than discovering the mismatch as a plot that never appears,
and a format major bump must be a **major npm release**. Nothing at runtime can
recover from getting this wrong.

Serve the wasm as `Content-Type: application/wasm` (streaming instantiation)
with brotli — 833 kB against 2.9 MB raw. A CDN does both. Given GPU init is
already the dominant startup cost, a `<link rel="preload" as="fetch"
crossorigin>` for the wasm is worth having so its download overlaps module
parsing. Note that SRI is not broadly usable for ES module imports, so the
exact-version URL is the integrity story.

### Open, for whoever ships this

- **No fallback for a browser without WebGPU.** `isSupported()` is a hard gate,
  so a producer should emit a static image beside the `.hplot` and the embed
  snippet should swap to it.

## Running the demo

```sh
cargo run --example document_save --features document-write   # writes examples/document.hplot
cp examples/document.hplot crates/hephaestus-wasm/www/
cp /path/to/some.ttf crates/hephaestus-wasm/www/font.ttf       # neither is committed
cd crates/hephaestus-wasm
./build.sh                           # or ./build.sh --dev for panic messages
python3 -m http.server 8080          # serve the crate dir, not www/
```

Then open <http://localhost:8080/www/>. The server root has to be the crate
directory: `www/index.html` imports `../dist/hephaestus.js`, so rooting the
server at `www/` puts it out of reach.

## Cross-references

- `src/window/CLAUDE.md` — `CanvasHost`, the frame path, and why the host is
  a handle rather than a driver.
- `src/document/CLAUDE.md` — what a document carries, and the font reasoning
  this crate is the consumer for.
