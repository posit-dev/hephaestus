# CLAUDE.md

Repo-level orientation for working in `hephaestus`. Architecture, module map, and per-module specifics live under `src/CLAUDE.md` and the per-folder `CLAUDE.md` files below it.

## Project

`hephaestus` is a 2D scene renderer for data visualization. The crate exposes a backend-agnostic scene API and an initial Vello (GPU compute via wgpu) backend; future planned backends are Blend2D (CPU raster), SVG, and PDF. Performance for interactive / real-time updates on dense plots is the design driver. WASM must work but is not the primary target.

The crate ships two API levels in the same source tree: a low-level scene API (`SceneBuilder` + primitives + layout) and a high-level plot API (`plot::*` — geoms, scales, and the `PlotComposition` orchestrator) built on top of it. See `src/CLAUDE.md` for the split and the rules that govern it.

## Commands

```sh
cargo build                                              # default features (vello + png)
cargo build --no-default-features                        # core types & traits only — no wgpu pulled in
cargo build --no-default-features --features vello,png   # explicit feature combination
cargo build --features vello,png,text                    # add the scaffolding text shaper (needed by chrome + text geoms)

cargo test                                               # all tests
cargo test --test smoke                                  # the GPU smoke test (requires a working wgpu adapter)
cargo test --test picking                                # picking round-trip

cargo clippy --all-features --all-targets -- -D warnings # treat warnings as errors
cargo fmt                                                # rustfmt; always run before declaring a task done

cargo run --example hello                                # renders examples/hello.png — visual sanity check
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

- **`vello`** (default) — the GPU rasterising backend (wgpu + vello + pollster + futures-intrusive + bytemuck).
- **`png`** (default) — PNG writer (`png` crate). Used by examples and tests.
- **`text`** (off by default) — scaffolding text shaping / layout via parley. Needed by the chrome on plot scales (axes, legends, titles) and by `TextGeom` / `TextFitGeom` / `TextPathGeom`. The host crate is intended to bring its own shaper eventually; see `src/text/CLAUDE.md`.
- **`geom-wkt`**, **`geom-wkb`**, **`geom-geojson`** (off by default) — opt-in parsers for `crate::scales::Geometry`. Each gate enables one of `Geometry::from_wkt` / `from_wkb` / `from_geojson`. Hand-rolled and dependency-free, so toggling them only affects what constructors compile, not the dependency tree.
- **`blend2d`**, **`svg`**, **`pdf`** — feature placeholders only; no backend code behind them yet. Wired so dependent crates can write `features = ["blend2d"]` once they exist.

The core types and traits compile with `--no-default-features` (no wgpu pulled in), so downstream crates can build on top of `SceneBuilder` without GPU dependencies.

## Out of scope at the crate level

The following belong in higher layers or other crates and should not land here:

- **Surface presentation** — no winit, no event loop. The renderer produces RGBA8 buffers; presentation is the caller's problem.
- **Interaction model and animation runtime** — picking emits pixel ids (see `src/CLAUDE.md`), but routing those to event handlers, tweening states, and animation scheduling all live in the host.
- **Filter effects** — blur, drop shadow, etc. Outside the Vello-∩-Blend2D intersection that governs the scene API.
- **Font selection / loading** — not handled at the scene level. The `text` feature provides a parley-backed scaffolding shaper and is explicitly meant to be replaced by the host. The `SceneBuilder` glyph-drawing surface consumes already-positioned glyphs.

The `plot/` module is in-scope: it is the high-level layer inside this crate that builds on the low-level surface. Out-of-scope means "not in this crate", not "not in this layer".

## Where to look next

- **`src/CLAUDE.md`** — code architecture: API levels, two-trait split, intersection-of-backends rule, picking model, module map.
- **Per-module `CLAUDE.md` files** under `src/scene/`, `src/backend/`, `src/backend/vello/`, `src/layout/`, `src/composition/`, `src/primitives/`, `src/plot/`, `src/plot/geom/`, `src/plot/scale/`, `src/text/`.

## Help / feedback

- `/help` — Claude Code help.
- File issues at https://github.com/anthropics/claude-code/issues.
