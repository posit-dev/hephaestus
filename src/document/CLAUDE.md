# src/document/CLAUDE.md

Capture a `PlotComposition` to bytes and rebuild it elsewhere. See `src/CLAUDE.md` for where this sits relative to the two API levels.

## What this module does

A plot exists only as live Rust objects, and `PlotComposition::render` turns one into draw calls for a single pixel size. A document is the durable form: the plot's **configuration**, with nothing size-dependent baked in. The reader gets a live `PlotComposition` back and calls `render` itself, so resizing reflows — axes re-lay-out, text re-wraps, ticks recompute — instead of stretching a frozen image.

The design decision that follows from that: **nothing shaped or measured is written.** Text travels as source strings plus style descriptors; `TextRun`, `RichTextRun`, `AxisMeasure`, `LegendMeasure` and every cache are rebuilt on load. Wrap width is whatever the layout solver allocates, so shaped output is size-specific — writing it would defeat the format's whole purpose.

## Read and write are separate cargo features

`document-read` and `document-write` gate the two halves; `document` enables both. A wasm consumer only ever reads, and `document-read` halves the codec it compiles.

The macro gates each generated half, so this is nearly free — but it is easy to leave a helper reachable from only one side. **Check all three combinations**, because two of them will not fail the default build:

```sh
cargo clippy --no-default-features --features document-read --lib -- -D warnings
cargo clippy --no-default-features --features document-write --lib -- -D warnings
cargo clippy --no-default-features --features document --lib -- -D warnings
```

## Building without a renderer

`--no-default-features --features document-write` is a complete writer: no `vello`, no `wgpu`, and nothing in the write path that needs pixels. That configuration is what an R package vendoring this crate for CRAN builds, so it carries a rustc constraint the rest of the crate does not — 1.86, against the crate's own 1.86 `rust-version`.

Two things follow, both easy to break:

- **`--ignore-rust-version` is required, and only because of a declaration.** `parley` and `fontique` declare 1.88 and are still compiled here, since text descriptors and shaping share one module. Their *code* builds on 1.86; cargo refuses on the declared floor alone. Splitting `crate::text` into descriptors and shaping is what would let the flag go away.
- **Anything the writer reaches must compile on 1.86.** A std API stabilised later is accepted by every other build. `rust-version = "1.86"` is what tells clippy to stop suggesting them, and the `msrv-document` CI job is what proves it.

The read half is gated the same way and checked on 1.86 too, but a reader with no renderer only round-trips — rendering a loaded document needs a backend.

## Conventions

- **Adding a field to `Theme`, `Scale`, `Plot` or the composition template means adding a line to the matching `impls_*.rs`.** Nothing catches the omission at compile time — a macro invocation lists field *names*, so a new field is silently skipped rather than rejected. `tests/document_roundtrip.rs` catches it only if the field changes pixels.
- **Discriminants are part of the file format.** Adding an enum variant takes the next free number. Renumbering an existing one silently reinterprets every document already written.
- **`impl_codec!` for types whose fields this module can name; hand-written impls for encapsulated ones.** `Scale`, `PolarProjection`, `Axis` and `Plot` are read through their accessors and rebuilt through their builders, so decoding runs the same validation as ordinary construction — a document with non-increasing bin edges is refused by `Scale::try_set_bins`, not accepted into a scale that would misplace rows.
- **There is deliberately no blanket `impl Encode for Arc<T>`.** Whether a shared value is written inline or interned is a decision per type: `Arc<str>` and `Arc<[LinetypeStep]>` are values whose sharing saves only memory, while `Arc<Geometry>` and `Arc<RichTextStyleSheet>` carry *identity* that live code compares by pointer. Omitting the blanket impl forces each new shared type to say which it is.
- **Encoding is infallible; validation is a separate pass.** `write::unsupported_items` runs first and reports everything at once. That is what lets `Encode::encode` return nothing to check, and why `Encode for Locale` can treat an unnameable locale as unreachable.

## Interning and identity

`src/document/intern.rs` holds three tables. Two motives, and it matters which applies:

- **Identity.** `Value::key_eq` compares `Arc<Geometry>` with `Arc::ptr_eq`, and `RichShapeCache` keys style sheets on `Arc::as_ptr`. Handing out one `Arc` per table entry is a correctness requirement — a fresh `Arc` per label misses the shape cache on every frame and breaks diff keys.
- **Size.** A geometry column of fifty thousand rows over two hundred country outlines is two hundred outlines. Strings are interned by *content* rather than pointer, since `Arc<str>` has no identity semantics and a grouping column built from a `Vec<String>` arrives as one allocation per row.

## Fonts

A document names families; it does not carry them by default. `WriteOptions::embed_fonts` turns embedding on, and the measurement that sets the default is stark: the four-panel plot in `tests/document_roundtrip.rs` is ~10 kB, and embedding the one family it names takes it past 2 MB, because macOS resolves `sans-serif` to a 2.4 MB Helvetica collection. A website already serves a subsetted web font an order of magnitude smaller, so the usual path is for the consumer to call `text::register_font_bytes` and `text::set_generic_family` itself.

Two traps if you touch this:

- **A generic family is an indirection, not a name.** Shipping Helvetica's bytes doesn't make `sans-serif` resolve to it — the consumer's context has to be told, which is what `fonts::GenericMapping` carries. A browser's `FontContext::new()` enumerates nothing, so without the mapping every generic falls back to nothing.
- **`font_faces_for_family` returns one entry per *file*, not per face.** A collection (TTC / OTC) holds every face of a family in one file, so asking per face hands back the same multi-megabyte file once each — that mistake made a document 14 MB instead of 2.4 MB. `register_font_bytes` registers every face in a file anyway.

## Known limitation

`GeomId` / `AxisId` / `LegendId` are not carried. They are handles for later `update_geom`-style calls, not anything drawing depends on — draw order is vector order, which is preserved — and a handle cannot outlive the process that issued it. Replaying through `add_geom` / `add_axis` / `add_legend` renumbers from zero, which differs from the original only where the original had gaps.

## Cross-references

- `src/plot/composition.rs` — `CompositionTemplate` is the layout spec this module writes, and was already measure-free before it existed. `from_document` is the constructor the read path lands in.
- `src/plot/geom/mod.rs` — `Geom::kind` supplies the tag; `GeomBuilder::from_parts` + `BuildableGeom::build_from` is the reconstruction path.
- `src/plot/scale/mod.rs` — `FormatSpec` is how a label formatter is named rather than carried.
