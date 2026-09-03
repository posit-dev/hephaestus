# src/backend/svg/CLAUDE.md

The vector backend: a `SceneBuilder` that emits SVG text. See `src/backend/CLAUDE.md` for the backend conventions this one deliberately departs from, and `src/CLAUDE.md` for the two-trait split that makes the departure legal.

## What this module does

Implements `SceneBuilder` and **not** `Renderer`. `Renderer`'s contract is to fill `width * height * 4` bytes of RGBA8; there are no pixels here. `PlotComposition::render` takes `&mut dyn SceneBuilder`, so nothing above `backend/` knows the difference.

It pulls no GPU dependency and, apart from `skrifa` (already in the tree via parley), nothing new at all. That is what makes `--no-default-features --features document-read,svg` a renderer-free "document in, SVG out" build on rustc 1.86 — the configuration an R package vendoring this crate for CRAN has to hit. CI pins it.

## The goal is editable output, not merely vector output

Everything below follows from that. A picture that happens to be made of paths is not the point; a plot someone can open in Illustrator and retype an axis label in is.

- **Text is `<text>`**, naming its font, never outlines — unless there is no source text to emit, or `SvgConfig::text` asks for outlines.
- **One object per thing.** A filled-and-stroked shape is one `<path>` carrying both; outlined text is one `<tspan>` with `paint-order`; a wrapped label is one `<text>` with a `<tspan>` per line. Two stacked elements are two objects to an editor, so editing one leaves the other behind — that is a correctness bug, not a size one.
- **Decorations are semantic.** `text-decoration-line`, not a rect that editing the text strands at the old length.
- **What every element would repeat goes on the root.** The document font, and the `xml:space` / `white-space:pre` pair that keeps white-space processing from collapsing the runs `textLength` measured. Both are inherited, so an element needing something else still says so.

**A block's own chrome is held back, not written through.** A span background or border arrives as an ordinary `fill` *between* two glyph runs of one paragraph, and letting it through would close the `<text>` and open another — so a `` `code` `` span would split its own sentence into two objects. Instead those elements buffer into a prelude and are written immediately ahead of the `<text>` they belong to. That is a reordering, and it is safe for the same reason the glyphs-then-decorations reorder in `src/text/` is: a span background sits *behind* its own glyphs, and span boxes are laid out sequentially along a line and stacked down the page, so one span's background never covers another's ink. The buffering is scoped to an open block, so nothing unrelated can be caught in it.

## Why `textLength` rather than per-glyph positions

A run is placed by one `x`/`y` plus `textLength` and `lengthAdjust`, following svglite.

Per-character positioning would assert something we have no basis for. We know what *our* shaping did; we cannot know what a fallback face on another machine ligates, how it clusters, or how it kerns. And the spec requires a renderer to break a ligature whose characters carry absolute positions, so the text would come apart in exactly the case the extra precision was meant to serve. `textLength` claims only what is true regardless of face: this run occupies this width.

## Streaming, not recording

Draw calls append to a body `String` with `<defs>` accumulating beside it; the document is assembled at write time. `<defs>` ordering is not a problem precisely because the body is a string — it gets written ahead of content it was discovered from.

Recording first would mean holding a cloned `BezPath` and `Brush` per draw, which is hundreds of megabytes for a dense scatter, to produce a string that could have been produced incrementally. `RecordingScene::replay(&mut svg_scene)` still works, so the op-list path exists for anyone who wants it.

## Invariants

- **Only leaf elements carry `transform`.** Layer `<g>`s never do: `push_layer`'s transform applies to the clip, not the contents, so it is baked into the clip geometry instead. Baking is exact for an affine and it canonicalizes the path, so two identically-clipped panels share one definition. This invariant is what makes `gradientUnits="userSpaceOnUse"` unambiguous everywhere.
- **`gradientUnits="userSpaceOnUse"` on every gradient.** The default is `objectBoundingBox`, which rescales the ramp to each shape's bbox — the most common gradient bug in a hand-rolled emitter, and it looks *almost* right.
- **`gradientTransform` is `brush_transform` alone**, never composed with the element's transform: that transform already establishes the user space `userSpaceOnUse` resolves in.
- **Ids are allocated in first-use order and emitted in id order**, never by iterating a hash map. `tests/svg.rs` asserts two renders are byte-identical, which is what catches hash-order leakage.
- **The document is always well-formed.** Unbalanced layers are closed at write time and reported; a non-finite coordinate is written as `0` rather than as `NaN`, which would make the file unparseable.

## Number formatting

Narrower than "avoid scientific notation": Rust's `Display` for `f64` never emits exponent form — only `Debug` does. **So the rule is never to `{:?}` a coordinate.** kurbo's own `BezPath::to_svg` is correct on that but writes 17 digits; `writer::num` is what to use.

**Two precisions.** Coordinates get `SvgConfig::decimals` (3). The linear part of a matrix gets 6 unconditionally: a scale rounded to three decimals is a 0.1% error, which over a 1000 px span is a visible pixel of drift, while a translation rounded the same way moves by a thousandth of a pixel.

## Degradations

Each is reported through `SvgScene::warnings()`, deduplicated by variant, and each still produces a well-formed document. Following `document`'s `UnsupportedItem` precedent: a scene is still drawable when one feature degrades.

| Scene feature | What happens |
|---|---|
| `GradientKind::Sweep` | flat fill from the middle of the ramp. SVG has no conic paint server in either version, and CSS `conic-gradient()` is a background image rather than a paint. Unreachable from `plot/`. |
| `Compose` ≠ `SrcOver` | emitted as `SrcOver`. See below. |
| asymmetric stroke caps | the start cap wins. SVG has one `stroke-linecap`. Unreachable from `plot/` — `Stroke::with_caps` sets both. |
| radial `start_radius` ≠ 0 | written as SVG 2's `fr`, which older consumers drop. |
| `Brush::Image` as paint | nothing drawn. Needs `<pattern>`; nothing in-crate constructs one. |
| image without `png` | nothing drawn. |
| glyph run with no source | outlines, or nothing if the face has no monochrome contours. |
| `embed_fonts` on a font *collection* | not embedded, and reported. `@font-face` cannot name a face inside a collection, so a `ttcf` blob in a data URL loads in no browser. macOS resolves `sans-serif` to a 2.4 MB collection, so this is the ordinary case rather than an exotic one. |

### Why `Compose` is not expressible, which is *not* "filters are out of scope"

`src/CLAUDE.md` puts filter effects outside the **authoring surface**. That says nothing about what this emitter may use internally — `<filter>` is entirely available here, the same way the vello backend is free to use compute shaders nobody authors against.

The actual blocker is narrower: **an SVG filter cannot see its backdrop.** `feComposite` and `feBlend` combine two *filter inputs*, and `push_layer` needs to composite against what was already painted beneath. SVG 1.1 specified `BackgroundImage` / `BackgroundAlpha` for exactly this, no implementation ever shipped them, and Filter Effects 1 removed them.

There *is* a way through, since we generate the whole tree: emit the backdrop and the layer as two sibling groups and combine them with an `feComposite` taking both as inputs — `over`/`in`/`out`/`atop`/`xor` natively, the rest via `operator="arithmetic"`. It costs buffering that subtree instead of streaming it, and a filtered group is rasterized by most renderers, which forfeits vector-ness precisely where someone asked for a blend. Not worth it while every in-tree `push_layer` uses `BlendMode::NORMAL` — but it is the known path, not an impossibility.

`Mix` needs no filter: its 16 variants *are* the CSS `mix-blend-mode` keyword set, and CSS Compositing does reach the backdrop. `isolation:isolate` on every layer group and on the root is what scopes that backdrop to the enclosing layer rather than to a host page the SVG is inlined into.

## Fonts: named always, delivered optionally

The document names a full family chain plus a generic tail, because whatever delivery is attempted can fail and a substituted face beats none. `textLength` is what keeps a substituted face from reflowing the block.

**Named once, on the root, and inherited.** The first text drawn claims `font-family` and `font-size` on the `<svg>` element; text agreeing with it writes neither, and text that disagrees names its own. Family and size are claimed separately, so a heading differing only in size still inherits the family. Beyond dropping a line of boilerplate from every label, this is what makes restyling a whole figure's type a one-place edit — which is the point of the format here.

**The whole request travels, not just the family.** Weight, style, width, letter spacing, OpenType features and variable-font axes are all applied at shape time, so a document that named only the family would be asking the viewer for a different face than the one the advances were measured from — and `textLength` would then squeeze whatever it resolved into the right box, which reads as plausible and is a mechanical scale of the wrong face. Width goes out as `font-stretch`, as the keyword when the ratio is one of the nine CSS steps and a percentage otherwise, since a variable font's `wdth` axis lands between them. Features and axes go in a `style` attribute rather than as presentation attributes, because SVG 1.1 names neither and an unrecognised attribute is ignored silently; nothing else on a `<text>` writes one, so there is no declaration to collide with.

The mechanism is inheritance, deliberately, and not a `<style>` rule: a presentation attribute loses to *any* CSS rule whatever its specificity, so a catch-all `text{font-family:…}` would override the `` `code` `` span that needs monospace rather than merely defaulting it. An inherited value is the opposite — weaker than the element's own attribute, so overrides keep working.

Two delivery mechanisms sit on top, neither of which guesses:

- **`@import`** for families this process actually resolved through `fetch_google_font`, recorded by `text::google_fonts::google_fetched_families`. Guessing from the name would send the font names of every plot anyone exports to Google, and would mostly 400.
- **`SvgConfig::embed_fonts`**, off by default, inlining the face bytes as `@font-face`. A face is often megabytes, so this can take a 30 kB plot past 3 MB.

A declaration's identity is family, weight, style **and** width, which is what `FaceKey` holds. Two rules with identical descriptors are not two faces to CSS — the later one wins — so leaving width out would let a document holding both the condensed and the normal cut of one family at one weight render every label in whichever came last. The descriptor and the element's `font-stretch` are written by the same helper, since a declaration that disagreed with the request would be a face the document asks for and its own `@font-face` does not answer.

The `@import` URL names `wdth` alongside `ital` and `wght`, so a condensed cut of a Google family is imported as the cut the element asks for rather than at normal width. Axis names go in alphabetical order and their tuples ascending, which is the form the API documents; an axis every face of a family agrees on is dropped, so the ordinary request stays `wght@400`. Google's own response declares `font-stretch` on the `@font-face` it serves — the same descriptor the embed path writes.

**Embedding declares the *resolved* family, not the requested one**, read back off the face's own `name` table with skrifa — a chain ending in a generic named nothing, and a named family that was missing resolved to something else. That name is also prepended to the element's chain, because an `@font-face` nothing references does nothing at all.

There is no WOFF2 anywhere in the pipeline: `google_fonts.rs` deliberately sends no User-Agent so Google serves TTF, and adding a compressor is out of scope. `sfnt_format` sniffs the four-byte version tag to pick between `truetype` and `opentype`.

## The `png` coupling

`svg` does not imply `png`. An image's original bytes are gone by the time a backend sees it — a scene holds decoded pixels — so embedding means re-encoding, and PNG is the choice for the reasons `src/document/images.rs` gives. Implying `png` would grow the renderer-free build by three crates for a capability most plots never use.

Two things `document/images.rs` gets wrong that this does not, and which are worth fixing there: `ImageFormat::Bgra8` is swizzled rather than refused, and `ImageAlphaType::AlphaPremultiplied` is un-premultiplied before encoding, since PNG is a straight-alpha format.

## Picking

Off by default — file export is the common case and the attributes are pure weight there. When on, `PickId::Id(n)` becomes `data-pick-id="n"`, `Block` becomes `"0"`, and **`Skip` becomes `pointer-events="none"`**. That last row is what makes the feature correct rather than decorative: it reproduces "items beneath remain hittable through this primitive" under `elementFromPoint`, without which a `Skip` gridline over a mark swallows the hit.

Pick **scopes** ride the same flag, as `<g data-pick-kind="…">` with
`data-pick-name` and `data-pick-index` when the scope carries them. They are
the same feature from a consumer's side, and they are the larger part of what
makes the output *editable*: a designer opening the file selects "the bottom
axis" or "this tick label" as a group, rather than a flat soup of paths. A
real plot emits `region → axis → part → item` nesting straight out of
`plot/chrome/`.

**The two group stacks must stay tagged apart.** `push_layer` and
`push_pick_scope` both emit `<g>`, but they are independent stacks — a scope
can open inside a layer and close outside it. `SvgScene` tracks
`Vec<GroupKind>` rather than a depth counter, and a pop whose kind does not
match the innermost group is refused and noted as `SvgWarning::UnbalancedScopes`
instead of emitting `</g>` against the wrong element. Sharing one counter
produces malformed XML, which
`tests/svg.rs::interleaved_layers_and_scopes_do_not_produce_malformed_xml`
would catch.

## Files

- `mod.rs` — `SvgScene`, `SvgConfig`, the `SceneBuilder` impl, the write trio.
- `writer.rs` — number formatting, XML escaping, `transform`.
- `path.rs` — `BezPath` → `d`.
- `paint.rs` — brushes → paint, gradients into `<defs>`.
- `defs.rs` — id allocation, dedup, the `<style>` slot.
- `text.rs` — glyph runs → `<text>` / `<tspan>`.
- `outline.rs` — glyph outlines → `<path>`, via `skrifa`.
- `image.rs`, `base64.rs` — raster images as data URLs (`png`).

## Cross-references

- `scene/` — `TextSource` is what carries a run's characters and font here; without it no backend could emit `<text>` at all.
- `src/text/` — populates `TextSource`, and owns the glyphs-then-decorations ordering that keeps one block's runs contiguous.
- `backend/mesh.rs` — `decompose` is shared verbatim; it needs no GPU.
- `src/image/` — the PNG encoder image embedding re-encodes through.
