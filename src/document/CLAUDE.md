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

## Registries: shapes always, images on request

Two of the things a plot carries are name-keyed registries rather than
configuration: `ShapeRegistry` (marker glyphs) and `ImageRegistry` (raster
images). The geoms resolve *names* against them, so a document that carried
neither would load fine and silently draw nothing for those rows. Both are
carried in their own chunks, and the two are treated differently because
their costs differ by three orders of magnitude.

- **`shps` — always written, and usually empty.** Every reader rebuilds the
  built-ins itself, so only the *delta* travels: an entry whose name is not a
  built-in, or one that replaces a built-in with something else. The delta is
  computed by comparing shapes, not names, which is what makes overriding
  `"circle"` travel — hence `Shape: PartialEq`, added for this. A custom shape
  is a handful of Bézier subpaths, so there is no flag to weigh.
- **`imgs` — off by default, behind `WriteOptions::embed_images`.** Pixels are
  payload, not configuration, which is the line the whole format draws; the
  reasoning is the same one that keeps `embed_fonts` off. Images travel
  PNG-encoded: measured 66x–96x smaller than the raw buffer on rendered plot
  content, and a synthetic 256x256 gradient embeds in 874 bytes against
  262,144 raw. Photographic content compresses far less, and is the case where
  naming stays the better answer.

  Two things beyond the registered entries end up in it. **Names read from a
  location** — a markdown `![](logo.png)`, or an `"image"` channel holding a
  path — are cached in the register they resolved through, and `carried_names`
  unions them in, which is the whole reason a figure whose title holds a
  picture survives being rebuilt on a page with no filesystem. And the
  **composition's own register**, for chrome that belongs to the composition
  rather than to a plot: it rides a `composition` field of its own, exactly as
  `EmbeddedShapes` does, rather than a plot address it would have to fake.

**Both need `png`, and neither half pulls it in.** `document-read` and
`document-write` stay dependency-free, so a build without `png` reports every
image through `UnsupportedItem::UnembeddableImage` on write and skips the
chunk on read. `crates/hephaestus-wasm` enables it deliberately (+46 kB
brotli); a renderer-free writer on rustc 1.86 does not, and still compiles.

**A glyph-backed shape travels as text, not as a glyph id.** `Shape::glyph`
holds a resolved face and glyph id because the draw path must not shape per
frame, but neither survives a trip: an id means nothing outside its own face,
and a family resolves differently on different machines, so a carried id would
index some arbitrary other glyph — a failure that renders something plausible.
So `glyph_marker` records a `GlyphSource { text, style }` and the reader re-runs
`try_glyph_marker` on it. This is not a workaround; it is the rule stated at the
top of this file applied to shapes. `try_glyph_marker` exists because the
panicking original would take down a document load when the reader's fonts
happen not to ligate a sequence. A shape built straight through `Shape::glyph`
has no source and is refused as `UnsupportedItem::UnnameableShape`.

**Registry entries are addressed by `(patch id, ordinal)`.** A patch can hold
several plots, so the position within the patch is part of the address —
`update_plot_at` is what the read side uses. Both chunks apply *after*
`from_document`, inserting into registries that already hold what the reader
rebuilt, so `shape_registry_mut` / `image_registry_mut` rather than the
replacing setters.

## Reading

`read_document` is the entry point: it hands back the composition **and** the
hints from one pass. `read_composition` and `read_hints` are each that with one
half discarded, and they still exist because a consumer often wants only one —
`read_hints` decodes the head alone, which is what lets a caller choose a size
before rebuilding anything. Calling both, though, decodes the head twice, so a
renderer seeding a surface from the hints before it draws should call
`read_document`.

`ReadContext::builtin()` is the shared context; `ReadContext::new` builds a
fifteen-entry geom factory table per call, so it is for adding a formatter or a
geom of your own rather than for the default case. A host reading document after
document wants the shared one.

## Conventions

- **Adding a field to `Theme`, `Scale`, `Plot` or the composition template means adding a line to the matching `impls_*.rs`, and the compiler says so.** Every `impl_codec!` form emits a completeness check — a destructuring for structs and records, an exhaustive `match` for enums — outside both feature gates, so a field or variant the invocation doesn't list fails the build. The gate placement is the point: a read-enabled build already caught a missing struct field through the decode-side struct literal, but `--features document-write` alone did not, and neither half caught a hand-written impl or a new enum variant.
- **Discriminants are part of the file format.** Adding an enum variant takes the next free number. Renumbering an existing one silently reinterprets every document already written. There is deliberately no graceful-degradation slot on any enum: an unknown variant is refused, because a plot rendered from an approximated value is worse than one that refuses to load.
- **What is a minor bump, and what is a major one.** Minor: a **trailing field on a `record`**, a new section at a **chunk body's tail**, a new **lowercase** chunk. Major: a reordered or removed field, a renumbered discriminant, a new **uppercase** chunk, a changed container flag. Readers never inspect the minor — the three mechanisms are what make it additive, and `wire`'s module docs hold the rules. Note what framing does *not* buy: growth is at a tail only, and a reader cannot tell it skipped a tail it didn't understand, which is why criticality is on the tag.
- **`record` for an aggregate that could gain a field; `struct` for one that cannot.** A record is length-prefixed and its tail is skippable; a struct is a bare concatenation. Reach for `struct` only where the arity is fixed by what the type means (`Point`, `Margin`, `Span`) or where the type appears once per data element and a length byte would be per-row overhead. Measured cost of the split on the four-panel test document: 6915 → 7090 bytes, about 2.5% — of which framing is ~2% and the rest is the container flags word and the head's writer version. Hand-written impls frame themselves through `codec::write_record` / `codec::read_record`.
- **An optional chunk is written unconditionally and read tolerantly.** The writer emits an empty table when the payload is off, so byte layout never depends on a flag; the reader treats absence as empty rather than as `MissingChunk`. `font` set the pattern and `shps` / `imgs` follow it — and all three are lowercase, which is the same fact stated in the tag.
- **`impl_codec!` for types whose fields this module can name; hand-written impls for encapsulated ones.** `Scale`, `PolarProjection`, `Axis` and `Plot` are read through their accessors and rebuilt through their builders, so decoding runs the same validation as ordinary construction — a document with non-increasing bin edges is refused by `Scale::try_set_bins`, not accepted into a scale that would misplace rows.
- **There is deliberately no blanket `impl Encode for Arc<T>`.** Whether a shared value is written inline or interned is a decision per type: `Arc<str>` and `Arc<[LinetypeStep]>` are values whose sharing saves only memory, while `Arc<Geometry>` and `Arc<RichTextStyleSheet>` carry *identity* that live code compares by pointer. Omitting the blanket impl forces each new shared type to say which it is.
- **Encoding is infallible; validation is a separate pass.** `write::unsupported_items_for` runs first and reports everything at once. That is what lets `Encode::encode` return nothing to check. It exists beside the options-free `unsupported_items` because one problem — an image the caller asked to embed and this build cannot — depends on the `WriteOptions`, and that public signature predates it.

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
