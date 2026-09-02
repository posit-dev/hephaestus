# CLAUDE.md

Repo-level orientation for working in `hephaestus`. Architecture, module map, and per-module specifics live under `src/CLAUDE.md` and the per-folder `CLAUDE.md` files below it.

## Project

`hephaestus` is a 2D scene renderer for data visualization. The crate exposes a backend-agnostic scene API, two Vello backends over wgpu — Vello Classic (GPU compute) and Vello Hybrid (sparse strips: path processing on the CPU, a render pipeline on the GPU) — and two vector backends that emit markup rather than pixels: SVG, aimed at editable output, and PDF, aimed at a fixed artifact with its fonts embedded. The one future planned backend is Blend2D (CPU raster). Performance for interactive / real-time updates on dense plots is the design driver. WASM must work.

The crate ships two API levels in the same source tree: a low-level scene API (`SceneBuilder` + primitives + layout) and a high-level plot API (`plot::*` — geoms, scales, and the `PlotComposition` orchestrator) built on top of it. See `src/CLAUDE.md` for the split and the rules that govern it.

## Commands

```sh
cargo build                                              # default features (vello + png)
cargo build --no-default-features                        # core types & traits only — no wgpu pulled in
cargo check --target wasm32-unknown-unknown             # wasm is a supported target; catches GL-backend and dep regressions
cargo clippy --target wasm32-unknown-unknown --no-default-features --features webgl,document-read -- -D warnings  # the WebGL2 build: no wgpu at all
cargo build --no-default-features --features vello,png   # explicit feature combination
cargo build --no-default-features --features vello-hybrid,png  # sparse strips instead of compute shaders
cargo build --features window                            # adds winit + the presentation surface
cargo +1.86 check --no-default-features --features document-write --ignore-rust-version  # renderer-free writer on the oldest supported rustc

cargo test                                               # all tests
cargo test --test smoke                                  # the GPU smoke test (requires a working wgpu adapter)
cargo test --test picking                                # picking round-trip
cargo test --test image_geom                             # raster images through PlotComposition
cargo test --no-default-features --features vello-hybrid --test hybrid  # the sparse-strips backend, end to end
cargo test --test window_blit                            # the window presentation blit, headless
cargo check --no-default-features --features window,vello-hybrid,png  # presentation with no compute-shader backend
cargo test --features document --test document_roundtrip # plot documents: reflow at unseen sizes
cargo test --features svg --test svg                     # the vector backend, with and without a codec
cargo test --features svg,png --test svg
cargo test --features pdf --test pdf                     # the fixed vector backend
cargo test --no-default-features --features pdf,png --test pdf  # …and its one use for a codec: bitmap color glyphs

cargo clippy --all-features --all-targets -- -D warnings # treat warnings as errors
cargo fmt                                                # rustfmt; always run before declaring a task done

cargo run --example hello                                # renders examples/hello.png — visual sanity check
cargo run --example svg_export --features svg            # one plot as both SVG and PNG, for side-by-side review
cargo run --example pdf_export --features pdf,vello,png  # the same plot as both PDF and PNG
cargo check --no-default-features --features document-read,svg  # renderer-free: document in, SVG out
cargo check --no-default-features --features document-read,pdf  # …and document in, PDF out
cargo run --example image_formats --features jpeg,tiff,webp  # all four raster writers
cargo run --example image_geom                           # raster images placed in a panel, and in markdown
cargo run --example document_placeholder --features vello-hybrid,document-read,png  # the static picture a page shows while the client boots
cargo run --example window --features window             # live window: resize + hover picking
cargo run --release --example window --features window -- 100000  # same scene at N points
cargo run --release --example window --features window,vello-hybrid -- 200000 hybrid  # sparse strips: no draw cap
cargo run --release --example backend_perf --features vello,vello-hybrid,png -- 100000  # per-frame cost, both backends
```

The wasm render client is a separate workspace, so it builds on its own:

```sh
cargo clippy --target wasm32-unknown-unknown --no-default-features \
  --features webgl,document-read -- -D warnings          # the client's default config
cargo clippy --target wasm32-unknown-unknown --no-default-features \
  --features vello,canvas,document-read -- -D warnings   # its `wgpu-backend` alternative
cd crates/hephaestus-wasm && ./build.sh && node verify-dist.mjs   # assemble + check dist/
cd crates/hephaestus-wasm && node bench/pixel-diff.mjs           # does a native pre-render match the client's frame?
cd crates/hephaestus-wasm && node bench/swap.mjs                 # is first paint immediate, and the swap invisible?
```

`crates/hephaestus-wasm/bench/README.md` covers the startup measurements, what
they mean and how to reproduce them. The headline: with a natively-rendered
picture in the served HTML the plot is on screen at ~57 ms instead of ~185 ms,
and the two frames are bit-identical.

**Always run `cargo fmt` after completing a coding task.** It's the last step before reporting work done, even when the diff looks cosmetically fine — rustfmt catches subtle layout drift (over-long lines, brace style, import ordering) that otherwise piles up across changes.

## Comments

This project **overrides** the usual "default to no comments" guidance. Specifically, for files under `src/`:

- **Every `pub fn` / `pub(crate) fn` gets a doc comment.** Including trivial accessors (`len`, `is_empty`, `id`) — give them one short line.
- **Trait method declarations** (`fn foo(&self);` inside `pub trait Foo`) get docs describing the contract callers can rely on.
- **Trait method implementations** inherit from the trait declaration — don't add per-impl doc comments. Same for `From` / `Default` / `Display` / `Debug` impls unless the impl does something non-obvious.
- **Private `fn`** gets a comment only when the purpose isn't obvious from the name. Lean conservative: a well-named helper is its own documentation.
- **`pub struct` fields and `pub enum` variants** get docs when they're carrying non-obvious meaning.

Style rules (apply everywhere, including comments in `tests/` and `examples/`):

- **Describe purpose, not implementation.** `/// True when the geom holds no rows.` — not `/// Returns true if `self.keys.len() == 0`.`
- **One concise sentence for most fns.** Two or three lines only when there's a non-obvious invariant or interaction with other code.
- **No backwards-facing language.** No "Now", "Previously", "Was", "Used to" (in the historical sense), "no longer", "originally", "legacy", "deprecated". Describe current behavior only.
- **No version markers** ("v1", "v1.5", "v1.6", etc.) in comments. If a planned future behavior is genuinely load-bearing it lives in an issue or planning doc, not the source.
- **No references to current task, callers, PRs, or commit history.** That belongs in the PR description / git log.
- **For builder methods that return `self`**, describe the field being set: `/// Set the patch's outer margin.` — not a restatement of the chaining pattern.
- **Use `///` for items; `//!` for module-level docs.** Inline `//` only inside function bodies for non-obvious WHYs.

`src/plot/geom/resolve.rs` is a good in-codebase template for the geom/resolve style.

## Cargo features

- **`vello`** (default) — the compute-shader GPU rasterizing backend (wgpu + vello + pollster + futures-intrusive + bytemuck).
- **`vello-hybrid`** (off by default) — the sparse-strips GPU backend (wgpu + vello_hybrid + vello_common + glifo + the same support crates). Independent of `vello`: either, both, or neither. Two things it can do that the compute-shader backend cannot — rasterize with binary coverage, which is what an id buffer needs, and size GPU buffers to actual scene content instead of fixed caps, so there is no draw-count ceiling. Measured less than half the wasm bundle size of `vello`. Implies `png` and `skrifa`: a bitmap color glyph (Apple Color Emoji and most Android emoji ship PNG strikes) is read and decoded by this crate rather than by the rasterizer, because the rasterizer's own strike path takes no rotation and paints the strike's colors into the id buffer. See `src/backend/hybrid/CLAUDE.md`.
- **`png`** (default) — PNG reader and writer (`png` crate).
- **`jpeg`**, **`tiff`**, **`webp`** (off by default) — the other raster codecs (`jpeg-encoder` + `jpeg-decoder`, `tiff`, `image-webp`). All four live in `src/image/`. Writing consumes the same RGBA8 buffer a `Renderer` produces, so a format costs only its codec — unlike `svg` / `pdf`, which need an alternative render path. Reading normalizes whatever the file holds to that same buffer and hands back a `brush::Image`, which is what `SceneBuilder::draw_image` and `plot::ImageGeom` consume. JPEG is the one format needing two crates, since `jpeg-encoder` cannot decode. All pure Rust and wasm-clean.
- **`google-fonts`** (off by default) — auto-fetch named Google Fonts families on demand. Synchronous network call on cache miss; cache hits are offline.
- **`image-url`** (off by default) — resolve an image named by an `http(s)` URL rather than a filesystem path, wherever a name is looked up: an `ImageGeom` channel, or a markdown `![](…)` tag. Fetches once per process and memoizes, like `google-fonts`; unlike it there is no on-disk cache, so it adds nothing beyond the HTTP client (`ureq`, already optional). Without it a URL is a location this build cannot read, which renders the broken-image placeholder in markdown and nothing at all for a geom row.
- **`window`** (off by default) — live window presentation: an OS window, a wgpu surface, and an event loop with resize and pointer events (`winit`). Requires a rasterizing backend — either one — and `WindowConfig::backend` chooses which. See `src/window/CLAUDE.md`.
- **`webgl`** (off by default) — the same sparse-strip rasterizer against a canvas's WebGL2 context instead of wgpu, using `vello_hybrid`'s precompiled GLSL renderer. Pulls **no wgpu at all**, and needs no WebGPU: it runs on browsers that have none, which is the point. Independent of `vello-hybrid`, which is the wgpu flavour of the same rasterizer, and implies `png` and `skrifa` for the same reason it does. Only `wasm32` compiles it — a `WebGl2RenderingContext` exists nowhere else. Brings its own presentation host, `window::WebGlHost`, since there is no surface or swap chain to manage: the canvas is the render target. See `src/backend/hybrid/CLAUDE.md`.
- **`canvas`** (off by default) — presentation onto a `<canvas>` already on a page, for a wasm build embedded in a website. Shares the `WindowApp` / `Frame` / `Event` surface and the blit path with `window`, but the page owns the event loop and feeds resize and pointer events in. Requires a rasterizing backend; pulls no winit. Only `wasm32` compiles the host, since `wgpu::SurfaceTarget::Canvas` exists nowhere else. The client built on it is `crates/hephaestus-wasm`.
- **`geom-wkt`**, **`geom-wkb`**, **`geom-geojson`** (off by default) — opt-in parsers for `crate::scales::Geometry`. Each gate enables one of `Geometry::from_wkt` / `from_wkb` / `from_geojson`. Hand-rolled and dependency-free, so toggling them only affects what constructors compile, not the dependency tree.
- **`document-read`**, **`document-write`**, **`document`** (off by default) — plot documents: capture a `PlotComposition` to a self-contained binary file (`.hep` by convention) and rebuild it elsewhere, so a wasm build on a website re-solves the layout at whatever size it has rather than scaling a frozen image. Hand-rolled and dependency-free, like the `geom-*` parsers. Split by direction because a consumer only ever reads; `document` enables both. See `src/document/CLAUDE.md`. Adding no dependency of their own, they are also the one useful configuration with no renderer at all: `--no-default-features --features document-write` builds a writer that compiles on rustc 1.86, which `vello` rules out.
- **`svg`** (off by default) — vector output: a `SceneBuilder` that emits SVG text instead of pixels, so it implements `SceneBuilder` and not `Renderer`. The point is *editable* output rather than merely vector output — text arrives as real `<text>` elements naming their font, markdown links as `<a href>`, decorations as `text-decoration`, and a filled-and-stroked mark as one `<path>` rather than two stacked ones. Needs no GPU and adds only `skrifa` (already in the tree via parley, for the glyph-outline fallback), which makes `--no-default-features --features document-read,svg` a renderer-free "document in, SVG out" build on rustc 1.86. Embedding a raster image additionally needs `png`; without it an image is reported and skipped. See `src/backend/svg/CLAUDE.md`.
- **`pdf`** (off by default) — fixed vector output: a `SceneBuilder` that emits a PDF file. Where `svg` aims at output someone can *edit*, this aims at output that looks the same everywhere — a figure going into a paper, a print pipeline or an archive. So every glyph a plot draws is embedded, always, as a subset font synthesized from the outlines actually used: a few kB rather than the 2.4 MB collection macOS resolves `sans-serif` to, and one code path that also handles CFF faces, variable-font instances and collections, none of which `svg` can embed. Adds `skrifa` (already in the tree via parley) and `flate2` (already there via `png`), so `--no-default-features --features document-read,pdf` is a renderer-free "document in, PDF out" build on rustc 1.86. Unlike `svg` it does not need `png` for raster images — PDF takes raw samples — but it does reach for it to decode a *bitmap* color glyph, which is how most emoji ship; without it those report `PdfWarning::MissingPngFeature`. Three things this expresses that `svg` cannot: real transparency groups, a native Gouraud mesh shading, and color emoji. See `src/backend/pdf/CLAUDE.md`.
- **`blend2d`** — a feature placeholder only; no backend code behind it yet. Wired so dependent crates can write `features = ["blend2d"]` once it exists.

The core types and traits compile with `--no-default-features` (no wgpu pulled in), so downstream crates can build on top of `SceneBuilder` without GPU dependencies.

## Out of scope at the crate level

The following belong in higher layers or other crates and should not land here:

- **Animation runtime** — picking emits pixel ids (see `src/CLAUDE.md`) and the `window` feature delivers them to an event handler, but tweening states and animation scheduling live in the host.
- **Filter effects** — blur, drop shadow, etc. Outside the Vello-∩-Blend2D intersection that governs the scene API.
- **Font selection / loading at the `SceneBuilder` level** — the scene API consumes already-positioned glyphs. Shaping and font discovery live in the `text` module (parley-backed); a host that wants its own shaper can replace it behind the `TextRun` / `draw_text` surface.

The `plot/` module is in-scope: it is the high-level layer inside this crate that builds on the low-level surface. Out-of-scope means "not in this crate", not "not in this layer".

## Releasing

A `v*` tag publishes to both registries from `release.yml`, a workflow that
does nothing else. Its `crate` job sends this crate to crates.io and its
`npm` job sends the wasm client to npm as `hephaestus-wasm`. Both
authenticate by OIDC — no stored tokens — and both refuse to run unless the
tag matches the version they are about to publish.

### Cutting a release

1. **Bump the version in both manifests**, to the same number: `Cargo.toml`
   and `crates/hephaestus-wasm/Cargo.toml`.
2. **Regenerate `Cargo.lock`.** It is tracked and records this crate's own
   version, so a bump leaves it stale. Any resolving command rewrites it —
   `cargo metadata --no-deps` does not, since it skips resolution.
3. **Promote `## Unreleased` in `CHANGELOG.md`** to the version number.
4. **Commit, push to `main`, and let `check.yml` finish green.** A tag
   triggers `release.yml` alone, and that runs the test suite but not clippy,
   the feature-isolation passes, or either MSRV job. Those run only on the way
   to main, so tagging an unchecked commit skips them entirely.
5. **Tag, and push the tag:**

   ```sh
   git tag v0.2.0
   git push origin v0.2.0
   ```

   A GitHub Release is not required — `push: tags` is the trigger. Creating
   one works too, since it pushes a tag.

### One version, two manifests

The crate's version and the npm package's live in separate files, and both
guards check against the tag: `v0.2.0` needs both manifests to read `0.2.0`
or the mismatched job fails. They are in lockstep deliberately, since the wasm
client is a view onto this crate rather than something with its own release
cycle.

The two publishes are otherwise independent — the client depends on this
crate by path, not by version — so ordering does not matter. It does mean
half a release is a reachable state, and not one a retry fixes: crates.io
refuses a version it already has, and an npm version cannot be reused.
Recovering means publishing the failed half by hand, or bumping both and
cutting again.

### Why releasing is its own workflow

npm and crates.io both register a trusted publisher by *workflow filename*. A
publish job inside `check.yml` would mean every change to the everyday check
workflow touches something that can publish. `release.yml` runs on nothing but
a tag, which keeps that surface as small as the trust model allows.

Each registry needs one-time setup: a GitHub environment — `crates-io` and
`npm` — and a trusted publisher naming `release.yml`.

See `crates/hephaestus-wasm/CLAUDE.md` for the npm side in detail.

## Where to look next

- **`src/backend/svg/CLAUDE.md`** — the vector backend: why text is `textLength` rather than per-glyph positions, and what degrades.
- **`src/backend/pdf/CLAUDE.md`** — the fixed vector backend: why the embedded font is synthesized rather than sliced, the pattern-matrix trap, and why this is the first backend that draws a mesh correctly.
- **`src/CLAUDE.md`** — code architecture: API levels, two-trait split, intersection-of-backends rule, picking model, module map.
- **Per-module `CLAUDE.md` files** under `src/scene/`, `src/backend/`, `src/backend/vello/`, `src/backend/hybrid/`, `src/layout/`, `src/composition/`, `src/document/`, `src/primitives/`, `src/plot/`, `src/plot/geom/`, `src/plot/theme/`, `src/scales/`, `src/image/`, `src/text/`, `src/text/rich/`, `src/window/`.
- **`crates/hephaestus-wasm/CLAUDE.md`** — the wasm render client: the Rust/JS split, why WebGPU is required, and why fonts are the thing that surprises people.

## Help / feedback

- `/help` — Claude Code help.
- File issues at https://github.com/anthropics/claude-code/issues.
