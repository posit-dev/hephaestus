# src/text/rich/CLAUDE.md

Marquee-flavoured rich text: markdown source in, positioned glyph runs and block paints out. Lives alongside the rest of `src/text/`.

Modelled on the R package [marquee](https://marquee.r-lib.org). Where this module deviates, the deviation is deliberate and noted below.

## The pipeline

Four stages, each in its own file, each consuming the previous stage's output and nothing else:

1. **`parser.rs`** — markdown → `Vec<RichEvent>`. Wraps `pulldown-cmark` (strikethrough / superscript / subscript / math enabled) and layers on two extensions: a line-based pre-pass for `:::class` fenced divs, and a per-`Text`-payload post-pass for `{selector body}` inline spans.
2. **`reduce.rs`** — events → `BuiltRuns { text, inline, baseline_shifts, blocks }`. Flattens the document into one `String` plus byte-ranged side tables. This is where the style cascade happens: the reducer keeps a stack of `ResolvedStyle` and hands every inline run and every block a fully resolved style, lengths already in points.
3. **`shape.rs`** — `BuiltRuns` → `RichTextRun`. One `parley::Layout<RichBrush>` per top-level *leaf* block, shaped at its effective content width; container blocks contribute insets, spacing chains and list markers to the leaves inside them. `wrap.rs::stack_blocks` then positions the leaves vertically.
4. **`draw.rs`** / **`border.rs`** — a positioned `RichTextRun` → `SceneBuilder` calls: block paints first, then list markers, then glyph runs with their span chrome and decorations.

Supporting files: `length.rs` (the measurement vocabulary), `style.rs` (`StyleDelta` / `ResolvedStyle` / `RichTextStyleSheet` / `css_color`), `block.rs` (block paints), `anchor.rs` (`RichAnchor` positioning), `cache.rs` (`RichShapeCache`), `flat.rs` (flattening, below), `tests.rs` (shape / wrap / draw tests).

## Flattening: what survives leaving the box

`flat.rs::flatten_rich_run` is the alternative to `draw.rs` for a caller
that stamps glyphs one at a time instead of drawing a laid-out box —
`TextPathGeom` walking a curve is the case it exists for. It returns
every glyph with its advance, its colour, its font and its offset from
one common baseline, plus the underline / strikethrough rules as
`RichFlatRule` centrelines.

Every line of every block is appended in document order, separated by
one space of the base style — measured as the difference between `"a a"`
and `"aa"`, since parley trims a lone space, and measured lazily so a
single-paragraph run never pays for it. A heading followed by a
paragraph therefore reads as one string, keeping its per-span sizes.

**The block geometry is what gets dropped**: block `y`, indents,
margins, list markers, span backgrounds and borders. None of them
survive because none of them mean anything on a curve — a rect cannot
bend. Per-span `text_stroke` goes too, since attributing it needs the
`InlineRun` brush matching that only `draw.rs` does. What survives is
everything carried by a glyph: font, size, weight, slant, colour, and
the `sup` / `sub` baseline shift, which arrives as a per-glyph `dy` a
caller adds to its own perpendicular offset.

Ascent and descent split the line box at the baseline — half the
leading on each side — so `ascent + descent` matches what the plain
shaper reports for the same string, and a caller anchoring on the text's
own height gets the same box either way.

## Metrics mirror the plain shaper

`RichTextRun` answers the same questions `TextRun` does, so a caller can anchor either kind of run the same way: `baseline_offset`, `cap_height`, `ink_top_offset` (the rich name for `first_line_ascender_offset`) and `inked_height`.

The band those last two describe is the union of the glyph ink — first line's ascender top to last line's descender bottom, leading only *between* lines — and every block paint box, so a backgrounded or bordered block is measured at the size it draws rather than at its glyphs. For a single unstyled paragraph the union collapses to exactly what `TextRun` reports.

`Measure::height_at` returns that band, not the stacked line box. A slot sized off the box would reserve half-leading above the first line and below the last that the run never paints into, and a markdown slot would come out taller than the same string shaped plain.

`cap_height` reads the first glyph run of the first line, matching `TextRun`'s ladder (`cap_height` → `x_height` → `0.7 × ascent`). Spans that resolve to a different font or size don't move it, so a label centres on its tick the way the plain labels around it do.

## Layering rule

**Nothing under `src/text/` may import from `src/plot/`.** The text layer is the low-level surface; the plot layer builds on it. Shared styling vocabulary lives at the crate root:

- `crate::style_vocab` — `Length`, `Margin`, `Palette`, `ThemeColor`, `HAlign`, `VAlign`.
- `crate::linetype` — the linetype pattern vocabulary and its arc-length renderer (block borders express their dashes as linetypes).
- `crate::shape::ShapeRegistry::shared_builtins()` — the process-wide built-in shape registry that marker-bearing borders stamp from.

`plot::theme` re-exports every `style_vocab` item, so plot-side code keeps addressing them through the theme. The gate is `grep -rn "crate::plot" src/text/` — only doc links may match.

## The length model

Marquee's four-way model, in `length.rs`. Every measurement on a `StyleDelta` is a `LengthSpec`:

- `Pt(v)` — absolute points.
- `Relative(m)` — `m ×` **the parent element's value of the same field**. Multipliers compound down the tree, so a `Relative(2.25)` heading inside a `Relative(0.9)` div is `2.25 × 0.9 ×` base.
- `Em(m)` — `m ×` **the element's own resolved font size**. This is what makes `h1 { margin_top: em(1) }` reserve one *h1*-sized line.
- `Rem(m)` — `m ×` **the run's base size**, unaffected by nesting.

`size` resolves first (its `Em` is degenerate and reads as `Relative`), then every other field resolves against that new own size. `LineHeightSpec` is separate because its natural reading is "multiple of the font size" rather than "multiple of the parent's line height". `StyleDelta::skip_inherit` is a `FieldSet` of fields that read the **grandparent** instead of the parent — marquee's mechanism for keeping `sup` inside `sup` from shrinking without bound.

**Tracking is the one measurement outside this vocabulary.** `StyleDelta::tracking` is a bare `f32` in 1/1000 em rather than a `LengthSpec`, because that is marquee's unit and there is no useful absolute reading of it — letter spacing that doesn't follow the em is a bug, not a feature. `TextStyle::tracking` uses the same unit, so `ResolvedStyle::from_base` copies it rather than converting, and `shape.rs` expands it against each element's own resolved size: a heading at `Relative(2.25)` tracks 2.25× as wide as the body around it.

**Do not extend `style_vocab::Length` for this.** Its two-variant `resolve(parent_pt)` is the plot theme's contract, and the theme cascade depends on that shape.

## Inheritance: one deliberate divergence

Marquee inherits *everything* down the tree and relies on `classic_style()` carrying explicit resets on every inline tag, so a block's background doesn't reappear on the spans inside it. We get the same rendered output from `ResolvedStyle::for_inline()`, which zeroes the box-level fields (`margin` / `padding` / `background` / `border_*` / `indent` / `hanging` / `bullet` / `align`) when a block's style seeds the cascade for its own content. The mechanism differs; the pixels don't.

## The box is tight at the document edges

Vertical margins collapse per CSS, with one rule on top: **a margin that reaches the run's top or bottom edge collapses out of the box and is dropped**. `stack_blocks` implements it — the first commit in the walk and whatever is left pending after the last block are both discarded.

This is marquee's `force_body_margin`, which it turns on for `geom_marquee()` and `element_marquee()` — every label-like use. The reason is that `paragraph` carries `margin.bottom = rem(1)`: a box that absorbed it would hang a blank line under a one-paragraph label, so every caller anchoring on the box bottom would place markdown text higher than the same string shaped as plain `TextRun`. Since every consumer here is a label or a chrome slot, the rule is unconditional rather than a flag.

Marquee arrives at the same place from the other side: it wraps the document in a `body` block styled `margin = trbl(0)` and forces those authored margins to win over anything that collapsed onto them. Adding a `body` selector is the extension point if an authored document margin (or padding / background around the whole run) is ever wanted; the tree recursion marquee needs for it is not, because the chain reaching each document edge is exactly what the flat walk has pending at its first and last commit.

## Style sheets

`RichTextStyleSheet` maps selector names to `StyleDelta`s. `new()` ships marquee's `classic_style()` values against palette-relative colours; `empty()` is a blank slate. Reserved names: `base`, the inline tags (`em`, `strong`, `underline`, `del`, `code`, `sup`, `sub`, `link`, `outline`), and the block tags (`paragraph`, `h1`..`h6`, `block_quote`, `list`, `list_ordered`, `list_item`, `list_item_body`, `code_block`, `hr`).

Selector names are descriptive rather than marquee's HTML-tag abbreviations (`block_quote`, not `qb`); the *values* match marquee. Colours are `ThemeColor` references so a sheet inverts with the palette.

**`base` is empty.** Marquee's `classic_style()` sets a `1.6` line height on its root because its base style *is* the caller's style. Here the caller passes a `TextStyle` that `ResolvedStyle::from_base` already folds into the cascade, so a value on `base` would be the one field of that style the sheet overrides — and a chrome slot could not reach its own theme's line height without rewriting the sheet. A document that wants marquee's leading asks for it on the style it passes, or sets `base` itself.

This is what lets a plain string measure identically through both shapers, which is the invariant every chrome slot depends on: turning markdown on for a slot must not change the box it reserves.

**A sheet is immutable once installed.** `RichShapeCache` keys on the `Arc` identity of the sheet a run shaped against; mutating a live sheet in place would leave stale entries. Build a new sheet instead.

## Parsing never fails

`parse` returns `Vec<RichEvent>`, not a `Result`. Every input is a document, and a malformed construct degrades to the characters that spell it:

- An unrecognised or empty selector head → a literal `{`, and scanning continues.
- A span left open at end of chunk → its head replayed as text.
- A stray `:::` with no open div → body text; an unclosed `:::` → closed at end of input.

This is marquee's guarantee, and it matters here because markdown is a formatting convenience layered over arbitrary user strings — a data label that happens to contain a brace still has to render. The old fallible surface produced *silently skipped rows and chrome slots*, which is strictly worse than rendering the braces.

Related parser conventions: `_x_` is **underline** and `*x*` is italic (marquee's reading); `\{` and `{{` both yield a literal brace; raw HTML passes through as literal text; `{#name}` looks up the id namespace with the `#` included in the key, so ids and classes share one map without colliding.

## Brush-matching caveat

Parley splits `GlyphRun`s on brush changes, so `draw.rs` identifies which `InlineRun` owns a given glyph run by matching resolved colour. Two inline runs that resolve to the *same* colour are indistinguishable at that point — a background-only or outline-only span with no `color` of its own can't have its decorations attributed. Sheet entries that want span chrome therefore set `color` alongside `background` / `text_stroke`; the built-in `code` and `outline` selectors both do.

## Caching and invalidation

Two layers:

- **Per run.** `RichTextRun::set_max_width` early-returns when the requested `(width, alignment)` matches the current break, and `derived` (bounds + block paints) is invalidated only when the blocks actually move. The layout solver probes the same width repeatedly while it converges and the draw pass asks once more for the width it settled on; without the memo each of those re-broke every block.
- **Across frames.** `RichShapeCache` holds `Rc<RichTextRun>`s keyed on source, style, brush, sheet identity, palette, dpi, quantized width and alignment — everything that decides what the run looks like. `TextGeom` owns one and clears it from `invalidate_caches`.

`Rc` rather than `Arc` because a `RichTextRun` holds `RefCell`s and rendering is single-threaded; a cache is therefore not `Send`, which is why it lives on the geom rather than in shared state.

`shape.rs` also keeps parley's `LayoutContext` in a thread-local, reused across blocks. It pairs with the process-global `crate::text::font_context()` mutex, which is taken first and released after the layout is built.

**Chrome divides on whether the slot wraps.** A slot the solver re-breaks — the title band, axis titles, strip labels — can't be keyed across frames, because its key would have to include a width it doesn't know at construction and two slots sharing that key would fight over one run's break state. The per-run memo still covers the solver's repeated probing there.

Every *unwrapped* slot is cached: break labels, legend and colorbar titles, polar labels, legend text swatches. They shape at natural width and never re-break, so `RichTextWidth::Natural` is the whole of their width key. `plot::chrome::text` owns that cache and hands out `ChromeRun`s from it; see `src/plot/CLAUDE.md` for why it's thread-local rather than owned by a `Plot`.

## Known limitations

- **Images are not rendered.** `![alt](path)` drops the image; the alt text renders as plain text. There is no host image-resolver hook yet.
- **Math renders as its source text.** `$x^2$` renders the characters, not an equation.
- Marquee's seven-value alignment vocabulary (`justified-left`, …), gradient backgrounds, and explicit control over underline / strikethrough metrics are not implemented.
- Block borders stamp markers from the built-in shape registry only; a caller-supplied registry isn't threaded through.

## Cross-references

- `src/text/CLAUDE.md` — the plain-text surface this layers alongside; `shape_common.rs` holds what the two share.
- `src/CLAUDE.md` — crate architecture, including `style_vocab` and `linetype`.
- `src/plot/plot.rs` — `draw_text_element_in_rect` and `measure_for_element` route chrome slots through this module when a `TextElement` opts into markdown.
- `src/plot/geom/text.rs` — `TextGeom`'s `"markdown"` channel.
- `src/plot/chrome/text.rs` — `ChromeRun` / `RichChrome`, the unwrapped-chrome path and its cache.
- `src/plot/theme/CLAUDE.md` — `Theme::rich_text` is the default sheet every markdown-enabled slot resolves through.
