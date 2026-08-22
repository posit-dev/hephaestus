# CLAUDE.md

Repo-level orientation for working in `hephaestus`. Architecture, module map, and per-module specifics live under `src/CLAUDE.md` and the per-folder `CLAUDE.md` files below it.

## Project

`hephaestus` is a 2D scene renderer for data visualization. The crate exposes a backend-agnostic scene API and two Vello backends over wgpu — Vello Classic (GPU compute) and Vello Hybrid (sparse strips: path processing on the CPU, a render pipeline on the GPU); future planned backends are Blend2D (CPU raster), SVG, and PDF. Performance for interactive / real-time updates on dense plots is the design driver. WASM must work but is not the primary target.

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
cargo test --no-default-features --features vello-hybrid --test hybrid  # the sparse-strips backend, end to end
cargo test --test window_blit                            # the window presentation blit, headless
cargo check --no-default-features --features window,vello-hybrid,png  # presentation with no compute-shader backend
cargo test --features document --test document_roundtrip # plot documents: reflow at unseen sizes

cargo clippy --all-features --all-targets -- -D warnings # treat warnings as errors
cargo fmt                                                # rustfmt; always run before declaring a task done

cargo run --example hello                                # renders examples/hello.png — visual sanity check
cargo run --example image_formats --features jpeg,tiff,webp  # all four raster writers
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
```

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

- **`vello`** (default) — the compute-shader GPU rasterising backend (wgpu + vello + pollster + futures-intrusive + bytemuck).
- **`vello-hybrid`** (off by default) — the sparse-strips GPU backend (wgpu + vello_hybrid + vello_common + glifo + the same support crates). Independent of `vello`: either, both, or neither. Two things it can do that the compute-shader backend cannot — rasterise with binary coverage, which is what an id buffer needs, and size GPU buffers to actual scene content instead of fixed caps, so there is no draw-count ceiling. Measured less than half the wasm bundle size of `vello`. See `src/backend/hybrid/CLAUDE.md`.
- **`png`** (default) — PNG writer (`png` crate).
- **`jpeg`**, **`tiff`**, **`webp`** (off by default) — the other raster writers, one encoder each (`jpeg-encoder`, `tiff`, `image-webp`). All four writers live in `src/image/` and consume the same RGBA8 buffer a `Renderer` produces, so a format costs only its encoder — unlike `svg` / `pdf`, which need an alternative render path. All pure Rust and wasm-clean.
- **`google-fonts`** (off by default) — auto-fetch named Google Fonts families on demand. Synchronous network call on cache miss; cache hits are offline.
- **`window`** (off by default) — live window presentation: an OS window, a wgpu surface, and an event loop with resize and pointer events (`winit`). Requires a rasterising backend — either one — and `WindowConfig::backend` chooses which. See `src/window/CLAUDE.md`.
- **`webgl`** (off by default) — the same sparse-strip rasteriser against a canvas's WebGL2 context instead of wgpu, using `vello_hybrid`'s precompiled GLSL renderer. Pulls **no wgpu at all**, and needs no WebGPU: it runs on browsers that have none, which is the point. Independent of `vello-hybrid`, which is the wgpu flavour of the same rasteriser. Only `wasm32` compiles it — a `WebGl2RenderingContext` exists nowhere else. Brings its own presentation host, `window::WebGlHost`, since there is no surface or swap chain to manage: the canvas is the render target. See `src/backend/hybrid/CLAUDE.md`.
- **`canvas`** (off by default) — presentation onto a `<canvas>` already on a page, for a wasm build embedded in a website. Shares the `WindowApp` / `Frame` / `Event` surface and the blit path with `window`, but the page owns the event loop and feeds resize and pointer events in. Requires a rasterising backend; pulls no winit. Only `wasm32` compiles the host, since `wgpu::SurfaceTarget::Canvas` exists nowhere else. The client built on it is `crates/hephaestus-wasm`.
- **`geom-wkt`**, **`geom-wkb`**, **`geom-geojson`** (off by default) — opt-in parsers for `crate::scales::Geometry`. Each gate enables one of `Geometry::from_wkt` / `from_wkb` / `from_geojson`. Hand-rolled and dependency-free, so toggling them only affects what constructors compile, not the dependency tree.
- **`document-read`**, **`document-write`**, **`document`** (off by default) — plot documents: capture a `PlotComposition` to a self-contained binary file and rebuild it elsewhere, so a wasm build on a website re-solves the layout at whatever size it has rather than scaling a frozen image. Hand-rolled and dependency-free, like the `geom-*` parsers. Split by direction because a consumer only ever reads; `document` enables both. See `src/document/CLAUDE.md`. Adding no dependency of their own, they are also the one useful configuration with no renderer at all: `--no-default-features --features document-write` builds a writer that compiles on rustc 1.86, which `vello` rules out.
- **`blend2d`**, **`svg`**, **`pdf`** — feature placeholders only; no backend code behind them yet. Wired so dependent crates can write `features = ["blend2d"]` once they exist.

The core types and traits compile with `--no-default-features` (no wgpu pulled in), so downstream crates can build on top of `SceneBuilder` without GPU dependencies.

## Out of scope at the crate level

The following belong in higher layers or other crates and should not land here:

- **Animation runtime** — picking emits pixel ids (see `src/CLAUDE.md`) and the `window` feature delivers them to an event handler, but tweening states and animation scheduling live in the host.
- **Filter effects** — blur, drop shadow, etc. Outside the Vello-∩-Blend2D intersection that governs the scene API.
- **Font selection / loading at the `SceneBuilder` level** — the scene API consumes already-positioned glyphs. Shaping and font discovery live in the `text` module (parley-backed); a host that wants its own shaper can replace it behind the `TextRun` / `draw_text` surface.

The `plot/` module is in-scope: it is the high-level layer inside this crate that builds on the low-level surface. Out-of-scope means "not in this crate", not "not in this layer".

## Releasing

A `v*` tag publishes to both registries, from `release.yml` — its own
workflow, which does nothing else. The `crate` job sends this crate to
crates.io; the `npm` job sends the wasm client to npm as `hephaestus-wasm`.
Both authenticate by OIDC — no stored tokens — and both refuse to run unless
the tag matches the version they are about to publish.

Releasing lives apart from `check.yml` because npm and crates.io both
register a trusted publisher by *workflow filename*. A publish job inside the
everyday check workflow would mean every change to that file touches
something that can publish.

**That last part couples two version numbers to one tag.** The crate's version
lives in `Cargo.toml` and the npm package's in
`crates/hephaestus-wasm/Cargo.toml`, and they are separate files: a `v0.2.0`
tag requires both to read `0.2.0` or the mismatched job fails. They are
deliberately in lockstep rather than independently versioned, since the wasm
client is a view onto this crate rather than a thing with its own release
cycle.

The two publishes are independent of each other — the client depends on this
crate by path, not by version — so neither ordering nor a partial failure
leaves the other registry wrong.

See `crates/hephaestus-wasm/CLAUDE.md` for the npm side in detail.

## Where to look next

- **`src/CLAUDE.md`** — code architecture: API levels, two-trait split, intersection-of-backends rule, picking model, module map.
- **Per-module `CLAUDE.md` files** under `src/scene/`, `src/backend/`, `src/backend/vello/`, `src/backend/hybrid/`, `src/layout/`, `src/composition/`, `src/document/`, `src/primitives/`, `src/plot/`, `src/plot/geom/`, `src/plot/theme/`, `src/scales/`, `src/image/`, `src/text/`, `src/text/rich/`, `src/window/`.
- **`crates/hephaestus-wasm/CLAUDE.md`** — the wasm render client: the Rust/JS split, why WebGPU is required, and why fonts are the thing that surprises people.

## Help / feedback

- `/help` — Claude Code help.
- File issues at https://github.com/anthropics/claude-code/issues.
