# src/plot/scale/CLAUDE.md

Scales — runtime-configurable value mappers. Map a domain `Value` to a visual output (panel fraction, colour, pt size, dash pattern). Axes and legends render the scale's tick / break structure.

## What this module does

A `Scale` combines a `ScaleType` (Continuous / Discrete / Ordinal / Binned / Identity), an optional `Transform` (Identity in v1), an `InputRange` (domain), and an optional `OutputRange` (visual range). Mapping flow:

1. Apply the transform (continuous scales only).
2. Normalise to `[0, 1]` against the input range.
3. Interpolate through the output range, or return the fraction directly if output is unset (position scales).

Scales are *stateless mappers*: all config lives on `Scale` itself. The same scale instance is shared between plots and across renders.

## Core types

- **`Scale`** (`mod.rs`) — the configurable mapper. Construct with `Scale::new(ScaleType::continuous())` or the free-function shorthands (`scale::continuous(0.0..=100.0)`, `scale::ordinal(["a", "b", "c"]).range_colors(...)`, etc.). Mutation via `set_*` methods that bump an internal `generation` counter; consumer-builder methods take `self`. Free-function constructors are scale-type-named only — output-type sugar is deliberately absent (a colour scale is `ordinal(domain).range_colors(...)`).
- **`ScaleType`** (`scale_type.rs`) — enum wrapping `Arc<dyn ScaleTypeTrait>`. Variants by kind: `Continuous`, `Discrete`, `Ordinal`, `Binned`, `Identity`.
- **`ScaleTypeKind`** — bare variant tag (no payload).
- **`ScaleTypeTrait`** — the implementer surface: `kind`, `map(&self, &Value) -> Value`, `breaks(...)`, optional `band_width` / `band_width_at`.
- **`InputRange`** (`input.rs`) — `Continuous { min: f64, max: f64 }` (closed interval; temporal data projects to f64) or `Discrete(Vec<Value>)` (explicit list, user-ordered for ordinal scales). Accessors: `extent()`, `discrete_len()`.
- **`OutputRange`** (`output.rs`) — `Numbers(Vec<f64>)` (pt for absolute sizes, unitless otherwise), `Strings(Vec<Arc<str>>)`, `Colors(Vec<Color>)`, `Linetypes(Vec<Arc<[LinetypeStep]>>)`. Position scales typically leave this unset; continuous scales then return `[0, 1]` fraction.
- **`Transform`** / **`TransformKind`** / **`TransformTrait`** (`transform.rs`) — function applied inside continuous scales before linearisation. v1 is `Identity` only; log / sqrt / etc. land here.
- **`ScaleRegistry`** (`mod.rs`) — name-keyed map owned by `PlotComposition`. Two plots that bind the same name share the registered scale.
- **`AxisSide`** / **`LegendSide`** (`chrome.rs`) — placement enums (Left / Right / Bottom / Top, Top / Bottom / Left / Right). No logic; rendering lives in `axis.rs` / `legend.rs`.
- **`extended_breaks`**, **`linear_breaks`**, **`DEFAULT_BREAK_COUNT`** (`breaks.rs`) — tick selection helpers.

Feature-gated (on `text`):

- **`axis`** module — axis rendering (ticks + labels + axis line).
- **`legend`** module — legend rendering.

## Scale types

- **Continuous** — linear interpolation over a numeric domain. Output range can be unset (→ `[0, 1]` fraction), `Numbers` (piecewise-linear across stops), or `Colors` (componentwise).
- **Discrete** — one-to-one lookup: `input[i]` → `output[i]`.
- **Ordinal** — ordered discrete domain with continuous output. Input position `idx / (n - 1)` interpolated through the output range — intermediate domain entries fall on gradient stops.
- **Binned** — continuous domain pre-binned by explicit breaks into discrete output bins.
- **Identity** — pass-through; input returned untouched.

## Conventions

- **Scales are stateless.** All configuration lives on `Scale`. No per-frame mutation.
- **Temporal data projects to f64 before entering a scale.** Date → days, DateTime → microseconds, Time → microseconds since midnight, Duration → microseconds. The domain and ticks are always f64-based; axis label formatters reverse the projection.
- **Band width is a scale-type concept.** Discrete / ordinal / binned report `1.0 / n_bins`; continuous reports 0. Geoms use `scale.map_with_offset(value, band_offset)` to fold a `[0, 1]` within-band offset into the position output. Without a scale (no binding), the band offset is ignored — band is meaningless outside a scale.
- **Generation counter is plumbed but unused in v1.** `Scale::generation` is bumped on every mutation; v1.5+ will use it to invalidate per-channel output caches without value comparison.
- **`OutputRange::Numbers` is in pt for absolute sizes**, unitless otherwise. The geom's `resolve_*` helper applies `pt_to_px` where appropriate.
- **Non-numeric endpoints panic.** `Scale::domain_continuous(String("a"), String("b"))` panics at the call site — no continuous ordering on strings or colours. Use `domain_discrete` for that.

## Adding a new scale type

1. Implement `ScaleTypeTrait`: `kind() -> ScaleTypeKind`, `map(&Value) -> Value`, `breaks(input_range, count) -> Vec<Value>`, optional `band_width() -> f64` and `band_width_at(&Value) -> f64`.
2. Wrap the impl in `Arc<dyn ScaleTypeTrait>` via `ScaleType::new_type(...)` or add a constructor on `ScaleType` for the new variant.
3. Geoms don't directly interact with scale types — they call `scale.map(&value)` which dispatches through the trait. No geom changes needed unless the new type implies a new `ExpectedOutput` variant.

## Cross-references

- `plot/value.rs` — the `Value` enum scales map.
- `plot/geom/resolve.rs` — the helpers geoms use to apply a scale per row. `resolve_position` (Value → panel fraction), `resolve_color_channel` (Value → Color), `resolve_linetype_channel` (Value → dash pattern).
- `plot/composition.rs` — `PlotComposition::add_scale` / `update_scale` are the user-facing entry points. Scale mutations through `update_scale` bump the generation and mark dependent plots dirty.
- `text/` (gated) — axis / legend rendering depends on `TextRun`. Without `text`, scales work but their chrome doesn't render.
