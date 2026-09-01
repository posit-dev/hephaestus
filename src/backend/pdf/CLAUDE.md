# src/backend/pdf/CLAUDE.md

The fixed vector backend: a `SceneBuilder` that emits a PDF file. See `src/backend/CLAUDE.md` for the backend conventions this one departs from, `src/CLAUDE.md` for the two-trait split that makes the departure legal, and `src/backend/svg/CLAUDE.md` for the sibling that inverts this one's premise.

## What this module does

Implements `SceneBuilder` and **not** `Renderer`. `Renderer`'s contract is to fill `width * height * 4` bytes of RGBA8; there are no pixels here. `PlotComposition::render` takes `&mut dyn SceneBuilder`, so nothing above `backend/` knows the difference.

It pulls no GPU dependency and nothing new: `skrifa` arrives with parley and `flate2` with `png`. `--no-default-features --features document-read,pdf` is therefore a renderer-free "document in, PDF out" build on rustc 1.86, the same configuration `svg` supports. CI pins it, and the wasm target too.

## The goal is a fixed artifact, not an editable one

This is the inversion of the SVG backend's premise, and everything below follows from it. That backend aims at a plot someone opens in Illustrator and retypes an axis label in. This one aims at a figure going into a paper, a print pipeline or an archive: **nothing may depend on what the reader has installed.**

- **Glyphs are embedded, always**, as a subset font built from the outlines actually drawn. Not opt-in, unlike `SvgConfig::embed_fonts`, because a file that names a face and hopes is exactly the failure mode this backend exists to remove.
- **There is no `textLength`.** It exists in SVG to survive a substituted face; here the face travels with the file, so there is nothing to survive.
- **Per-glyph positioning is defensible here** for the same reason. The SVG backend refuses it because it cannot know what a fallback face ligates or kerns; this file carries the face whose advances the layout was solved against, so the positions it writes are the positions that will be drawn. A `TJ` array records only the departures from the embedded advances, which is usually none of them.
- **Decorations are drawn, not declared.** PDF has no semantic underline, so `TextSource::decorations` is ignored entirely and the rects `src/text/` emits pass through as ordinary fills. The field's presence invites the SVG treatment; resist it.
- **Text is still text.** Every embedded face carries a `/ToUnicode` CMap, so selection, search and extraction recover the original characters — the one editability property worth keeping, and the one a print pipeline also needs.

## Why a synthesized font rather than a subset of the original

`sfnt.rs` reads outlines through skrifa and writes a fresh TrueType. One code path then covers four things a byte-level `glyf` subset needs four for:

- **CFF/OTF faces** have no `glyf` table at all. Skrifa reports their outlines as cubics, which convert to quadratics cleanly.
- **Variable fonts** must be embedded at the *instance* the plot shaped with. Copying `glyf` bytes would embed the default instance — the wrong weight or width — because the deltas live in `gvar`.
- **Font collections** (`ttcf`) are the ordinary case on macOS, where `sans-serif` resolves to a 2.4 MB collection. Building a fresh sfnt extracts one face for free. This is the limitation that makes the SVG backend refuse to embed at all.
- **Subsetting** is not a separate step: only the glyphs asked for are ever drawn.

The cost is that hinting is dropped, which at plot sizes with any modern grayscale rasterizer is invisible, and that quantizing to integer font units introduces at most half a unit of error — 0.0025 pt on a 2048-upem face at 10 pt.

The file carries six tables and nothing else: `glyf`, `head`, `hhea`, `hmtx`, `loca`, `maxp`. ISO 32000-1 §9.9 (Table 126) lets a `/FontFile2` subset omit `cmap`, `name` and `post`, and hinting tables are only needed when hinting is present.

`/CIDToGIDMap /Identity` means the CID a content stream writes *is* an index into that table, so **subset glyph id `k` must be `glyphs[k]`**, with index 0 reserved for `.notdef`. Getting that off by one produces a page full of real letters that are the wrong letters — plausible enough to survive a glance, which is why `tests/pdf.rs` reads the embedded program back with skrifa and checks each advance against its `/W` entry.

`/BaseFont` carries a six-letter subset tag derived from the family name, face index, variation coordinates and the sorted source glyph ids — never from a blob id, which is process-local, and never from a counter, which would change if draw order did.

## Color glyphs leave the text path

A monochrome subset font cannot carry a color glyph, so `color.rs` draws those as graphics instead. A run splits into maximal spans of outline glyphs, each its own `BT` … `ET`, with a color or bitmap glyph drawn between spans in glyph order.

Dispatch is COLR, then bitmap strikes, then outlines — the order vello uses. **`outline_glyphs().get(gid).is_none()` is not the color-glyph test.** An sbix font such as Apple Color Emoji carries a `glyf` table whose emoji entries are *empty*, so `get` returns `Some` and `draw` succeeds having emitted no pen calls at all. Only a face with no outline table of any kind returns `None`.

skrifa's `ColorPainter` callbacks map one-to-one onto operators this backend already emits, which is why the whole thing is small: a transform is a `cm`, a clip is `W n`, a solid fill is the clip rectangle filled, a gradient fill is a shading painted with `sh` — which fills the current clip region, exactly the callback's contract — and a composite layer is the same transparency-group form `push_layer` produces. The painter works in font units, Y-up, so the glyph's own space is set up once with a `cm` before `paint` and everything inside is written unmodified.

Clip boxes are tracked in *glyph* space rather than in whatever space is current, and converted back through the accumulated transform when a bare `fill` needs a rectangle. The PDF clip is in force either way, so the rectangle only has to be a superset.

Each glyph's graphics are wrapped in `/Span << /ActualText <FEFF…> >> BDC` … `EMC`, which is what keeps them copyable and searchable — the character comes from the same reverse charmap the `/ToUnicode` CMap is built from.

This makes PDF the second backend to render bitmap emoji: vello does, and the hybrid backend silently drops them because `glifo`'s `png` feature is off in this crate's dependency table.

## The `png` coupling

Unlike `svg`, embedding an ordinary raster image needs no codec at all — PDF takes raw samples, so a scene's already-decoded pixels go straight into a `FlateDecode`d stream with alpha in a separate `/SMask`.

There is exactly one place `png` is reached for: a *bitmap* color glyph. Apple Color Emoji and most Android emoji ship PNG strikes, which have to be decoded before they can be re-encoded as an image XObject. Without `png` those glyphs report `PdfWarning::MissingPngFeature` and everything else still renders. CI checks both configurations.

## The coordinate flip

One `cm` at the top of the page's content stream:

```
q
0.75 0 0 -0.75 0 450 cm      %  s 0 0 -s 0 h_pt, with s = 72/dpi
```

After it, **PDF user space is scene space**: one unit is one scene pixel, y increases downward, the origin is the top-left corner. Path coordinates, stroke widths and font sizes all go out unmodified. The `/MediaBox` is the same size converted to points, so the page prints at the physical size it was rendered for.

kurbo's `Affine::as_coeffs()` is already PDF's `a b c d e f` order — both mean `x' = ax + cy + e`, `y' = bx + dy + f`. No transposition, no reordering; `writer.rs` carries a test that says so, because getting it wrong makes every transformed primitive wrong in a way that looks plausible.

Glyph space is Y-up, so the text matrix has to flip back: `Tm` is `translate(x, y) * scale(1, -1)`. A `glyph_transform` acts in Y-up glyph space and PDF applies the font size *before* `Tm`, which is what the `scale(size, -size) * g * scale(1/size)` conjugation undoes.

## The pattern-matrix trap

A shading pattern's `/Matrix` maps pattern space to the **default space of the content stream it is used in** — *not* to the CTM in effect when the paint happens. So the `cm` an ordinary fill emits does not move its own gradient, and the matrix has to carry that transform itself. This is PDF's counterpart to SVG's `gradientUnits="objectBoundingBox"` default, and it looks *almost* right when you get it wrong.

> **Invariant.** Every `Target` carries a `pattern_base`: the page flip for the page stream, the identity inside a Form XObject. A pattern's matrix is `pattern_base * transform * brush_transform`. A gradient used under two different transforms interns as two patterns, which is correct.

A form's `pattern_base` is the identity because the form is painted under the already-flipped page CTM with its `/Matrix` left implicit-identity, so scene coordinates written inside it need no further mapping.

## Meshes are native, and what that leaves behind

`draw_mesh` becomes a `ShadingType 4` free-form Gouraud triangle shading painted with `sh`. `sh` paints in the *current* user space, so there is no matrix trap here — the CTM does the work.

**This backend does not call `backend::mesh::decompose`**, and is not in that module's cfg gate. `decompose` exists because no rasterizing backend has an indexed-mesh primitive, and everything in it is a workaround for that: quad-pair detection, uniform-fan detection, and a per-triangle linear-gradient approximation whose own doc comment concedes that a triangle with three distinct colors leaves "a small visible discontinuity". A Type 4 shading needs none of it — adjacent triangles interpolate inside one object with no antialiased edge between them, and a three-color triangle is exactly what Gouraud shading is. **This is the first backend that renders `draw_mesh` correctly**, and its mesh output may legitimately differ from — and be better than — the raster backends'. That is not a bug to reconcile.

> **Emit triangles in `mesh.indices` order and never reorder them.** Overlapping triangles in a Type 4 shading paint in stream order, and `src/primitives/ribbon.rs` relies on that: a self-intersecting polyline's tail occludes its head where they cross.

One vertex record is twelve bytes at these bit widths — a flag, two 32-bit coordinates and three color bytes — all byte-aligned. Coordinates map through `/Decode`, whose range comes from the mesh's bounding box; a degenerate axis gets `dmin`, `dmin + 1` rather than a zero-width range to divide by.

### The seam bleed it inherits

`src/primitives/ribbon.rs` physically displaces vertices so adjacent quads overlap, hiding the antialiased seam a consumer leaves when it paints each quad as an independent fill. That is baked into `Mesh.vertices` before any backend sees it, so a native-mesh consumer inherits an overlap it has no seam to hide. `RibbonOptions::seam_bleed` and `ribbon_band_mesh_with_bleed` exist to turn it off; both default to today's values, so PDF output is unchanged until a caller sets the knob, and nothing in `plot/` does — a geom builds its mesh before it knows what sink it is drawing into, and `SceneBuilder` has no "do you paint meshes natively" query to add without breaking the intersection-of-backends rule. Wiring it per-backend needs a render hint on `PlotComposition`; that is a `PLAN.md` item, not something to solve here.

Meanwhile the inherited bleed is benign in a Type 4 shading: at an interior joint, segment *i*'s far vertices and segment *i+1*'s near vertices carry the *same* boundary color, so the overlap band is painted twice in nearly the same color and the last-wins result differs by at most 0.75 px worth of one segment's color delta.

## Invariants

- **Nothing is deferred to write time.** Glyph outlines are extracted, color-glyph paint graphs are walked, image pixels are converted and warnings are recorded during the `&mut self` draw calls. `to_pdf(&self)` only serializes what is already there. That is why `warnings()` is complete after the last draw call, and it is what lets `encode_pdf(&s) == encode_pdf(&s)` hold.
- **Every drawing operator re-emits its own graphics state.** `SceneBuilder` has no current-transform or current-brush state, so the emitter has none: `w`, `J`, `j`, `M`, `d`, `rg`/`RG` are written before every op that uses them. It costs a few dozen redundant bytes per primitive, which flate removes almost entirely, and it buys immunity from every `q`/`Q`-nesting bug a state cache would introduce. Do not add a "last emitted value" cache.
- **Layers never carry a `cm`.** `push_layer`'s transform applies to the clip, not to the contents, so it is baked into the clip geometry — which is what makes the pattern-space rule a single sentence.
- **A pending fill holds serialized operators, not a path.** It has to be written either way; doing it at record time avoids cloning the geometry, and it is what lets a non-finite coordinate be reported while the scene is still `&mut`. The merge test is "same transform, same serialized geometry", which is the question it was always asking.
- **Resource names are allocated in first-use order and emitted in name order**, never by iterating a hash map. One counter is shared across kinds, so names run `GS0`, `P1`, `X2` and an allocation-order bug shows up in the output. `tests/pdf.rs` asserts two encodes are byte-identical, which is what catches hash-order leakage.
- **No `/ID` and no `/Info`.** Both would carry a timestamp or a random value. Their absence is what makes that byte-identity free.
- **The file is always structurally valid.** Layers left open are closed at write time, `/Length` is always a direct integer computed from a payload already in hand, and a non-finite coordinate is written as `0` rather than as `NaN`. An xref entry is exactly 20 bytes; a viewer seeks by multiplying, so a byte either way makes the file unreadable.

## Number formatting

Same rule as the SVG backend and a harder constraint: PDF real numbers may not use exponent notation *at all* (ISO 32000-1 §7.3.3). Rust's `Display` for `f64` never emits it — only `Debug` does — so **never `{:?}` a coordinate**; use `writer::num`.

Three precisions. Coordinates get `PdfConfig::decimals` (3). The linear part of a matrix gets 6: a scale rounded to three decimals is a 0.1% error, which over a 1000 px span is a visible pixel of drift, while a translation rounded the same way moves by a thousandth of a pixel. A `DeviceRGB` component gets 4, finer than the eight bits every rasterizer in the crate produces.

Content streams are pure ASCII and built as a `String`; only object payloads — font programs, image samples, mesh vertices — are `Vec<u8>`.

## Degradations

Each is reported through `PdfScene::warnings()`, deduplicated by variant, and each still produces a structurally valid file.

| Scene feature | What happens |
|---|---|
| `GradientKind::Sweep` | flat fill from the middle of the ramp. A `ShadingType 1` shading with a `FunctionType 4` calculator computing `atan2` would express it, and is not worth a PostScript interpreter's worth of output. Unreachable from `plot/`. |
| `Extend::Repeat` / `Reflect` | padded. PDF has no repeating shading in any version — a degradation SVG does *not* have. |
| `Compose` ≠ `SrcOver` | composited as `SrcOver`. PDF's imaging model fixes source-over; `/BM` selects a blend function, not a Porter-Duff operator. This one is genuinely inexpressible, unlike SVG's, where a restructured filter tree is a known path. |
| asymmetric stroke caps | the start cap wins. PDF has one line-cap setting. Unreachable from `plot/`. |
| `Brush::Image` as paint | nothing drawn. Needs a tiling pattern; nothing in-crate constructs one. |
| PNG bitmap strike without `png` | that glyph is not drawn. |
| glyph with neither outlines nor a color form | not drawn, and reported. |

**`SvgWarning::RadialFocalRadius` has no counterpart.** `ShadingType 3` takes a non-zero start radius natively, so nothing degrades.

## Varying alpha: the soft-mask path

A shading function produces color, not alpha, so a gradient whose stops
disagree about alpha — or a mesh whose vertices do — cannot be expressed
by the shading alone. It is carried by a **luminosity soft mask**
instead: an `/SMask << /S /Luminosity /G <form> /BC [0] >>` on the
primitive's ExtGState, where the form paints the *same geometry* through
a `DeviceGray` ramp whose value is the alpha. For a mesh that is
literally the same Type 4 vertex stream with one gray byte per vertex
instead of three color ones.

This matters because it is reachable from `plot/`, which the rest of
this backend's degradations are not: a `RibbonGeom` whose
`"fill_opacity"` channel is bound to a column resolves per-row colors
with differing alpha through `override_alpha`, and those become gradient
stops on the cartesian path and mesh vertices on the projected one. A
confidence band that fades along its length is the ordinary case.

> **The mask must be set while the CTM is the identity.** A soft-mask
> group is evaluated in the coordinate system in force when the `gs`
> operator runs, and a renderer composites it into a buffer it sizes
> from that same system — CoreGraphics clips the group to `CTM ×
> MediaBox`. Set the mask under the page flip and the mask is silently
> clipped part way across the page: the shape fades correctly and then
> stops dead, which reads as a geometry bug rather than a mask one.
>
> So a masked primitive resets to default user space, sets the mask, and
> restores — two extra `cm` operators, and only on the primitives that
> carry a mask:
>
> ```
> q
> 1.333 0 0 -1.333 0 200 cm    % undo the page flip: CTM is now the identity
> /GS3 gs                      % the mask's space is default user space
> 0.75 0 0 -0.75 0 150 cm      % back to scene space, times the primitive's own
> /Pattern cs /P0 scn
> … f
> Q
> ```
>
> The mask form's content therefore carries the whole way from the
> shading's coordinates to default user space — the same composition a
> shading pattern's `/Matrix` carries — and its `/BBox` is the page in
> points. `tests/pdf.rs` asserts the reset composes with the page flip
> to the identity, which is the claim stated as an equation.

`/BBox` may be generous: `/Extend [true true]` pads the ramp with its
terminal stop's alpha, exactly as the color shading pads, so covering
more than the shape changes nothing. Covering *less* would, because
`/BC [0]` makes everything outside the box fully transparent.

**One graphics state carries one `/SMask`, and it applies to every
painting operator that follows** — so a masked fill cannot merge with
its stroke into a single `B`. `stroke` breaks the merge when either side
carries a mask.

Cost is zero when the alphas agree: no mask, no extra objects, a plain
`/ca`.

## What PDF expresses that SVG cannot

- **Real transparency groups.** A layer with alpha < 1 or a non-`Normal` mix mode becomes a Form XObject with `/Group << /S /Transparency … >>`, which composites the layer's contents together *first* and applies the alpha once — which is what `push_layer` means. Setting `/ca` on each primitive instead would let two overlapping shapes inside show through each other.
- **A gradient or mesh with varying alpha**, through the soft-mask path above. SVG expresses this natively with `stop-opacity`; PDF needs the mask, but the result is the same picture rather than a degraded one.
- **A non-zero radial focal radius**, natively.
- **Native Gouraud meshes**, per the section above.
- **Color emoji**, both COLR and bitmap strikes.
- **Embedding a face out of a `ttcf` collection**, which `@font-face` cannot address at all.

## Files

- `mod.rs` — `PdfScene`, `PdfConfig`, `PdfWarning`, the `SceneBuilder` impl, `to_pdf`, the write trio.
- `writer.rs` — number formatting, string and hex escaping, `Objects` (offsets, xref, trailer), `deflate`.
- `content.rs` — paths and graphics state as operators, the `Target` stack.
- `paint.rs` — brushes to color operators and shading patterns; the stitching-function builder, shared with color glyphs.
- `res.rs` — named resources: interning, deterministic names, the `/Resources` dictionary, and the reference tokens a body needs when it must name another resource by object number rather than by name.
- `mesh.rs` — `Mesh` to a `ShadingType 4` vertex stream.
- `image.rs` — images to XObjects and soft masks.
- `font.rs` — face identity, glyph registration, the CID font dictionaries, the `/ToUnicode` CMap, run emission.
- `sfnt.rs` — the TrueType builder: the outline pen, the six tables, checksums.
- `color.rs` — COLR paint graphs and bitmap strikes.

## Cross-references

- `scene/` — `TextSource` carries a run's characters, font and link destination; without it there would be no `/ToUnicode` CMap and no link annotations.
- `src/text/` — populates `TextSource`, and emits the halo pass and the fill pass as two runs, which stack correctly here as two text objects.
- `src/backend/svg/` — the sibling vector backend, and the shape this one follows: a `SceneBuilder` with an `encode` / `write_to` / `write` trio and a deduplicated warning list.
- `backend/href.rs` — the link-safety allow-list both vector backends apply.
- `src/primitives/ribbon.rs` — the seam bleed, and the `seam_bleed` knob that turns it off.
- `src/image/` — the PNG *decoder* a bitmap color glyph goes through.
