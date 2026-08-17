# src/scales/CLAUDE.md

Scales — value mappers. Map a domain `Value` to a visual output (panel fraction, colour, pt size, dash pattern).

**Free functions, no traits.** Every algorithm in this module is a plain free function over enum tags + POD config types — no `Arc<dyn>`, no `ScaleTypeTrait` / `TransformTrait`. Dispatch is `match` on `ScaleTypeKind` / `TransformKind`. Adding a new scale type or transform means extending the enum, writing a per-kind free function, and adding the new arm to each central match. Hephaestus has a closed, well-understood set of variants; runtime polymorphism via traits was overkill and added a layer of indirection across the upcoming crate boundary.

**Leaf-module convention.** Nothing inside `src/scales/` imports from `crate::plot::*`, `crate::scene::*`, `crate::backend::*`, `crate::primitives::*`, or `crate::text::*`. It depends only on `std`, peniko (via `crate::color`), and its own siblings. The module is structured this way so it can be lifted into its own crate (`scales`) when the API settles. The lift is a `mv src/scales crates/scales/src && cargo init --lib`-style migration — no surface changes.

**No `Scale` aggregate here.** The hephaestus `Scale` struct (bundle of scale_type + transform + direction + ranges + bin edges + break / minor-break specs + formatter + generation counter) lives in `src/plot/scale/mod.rs`, with its named constructors in `src/plot/scale/constructors.rs`. Future consumers of the scales crate roll their own bundle and call the free functions directly. The Scale struct's methods are 1-line shims that match on the enum tags and delegate.

Axes and legends — *rendering* of a scale's ticks / breaks against a `SceneBuilder` — live in `src/plot/chrome/` (hephaestus-internal). The scale layer here defines *what* to draw; chrome draws it.

**Locale, not formatter, crosses the boundary.** `Locale` lives here because break labels are a scale-layer concern, but the `LabelFormatter` that overrides them hangs off hephaestus's `Scale`. A future crate consumer supplies its own formatter and reuses `Locale` as-is.

## What this module does

A `Scale` combines a `ScaleType` (Continuous / Discrete / Ordinal / Binned / Identity / Temporal), an optional `Transform`, an `InputRange` (domain), an optional `OutputRange` (visual range), and — for `Binned` — a bin-edge list. Mapping flow:

1. Apply the transform (continuous scales only).
2. Normalise to `[0, 1]` against the input range.
3. Mirror the fraction (or the domain index) if the `Direction` is `Reversed`.
4. Interpolate through the output range, or return the fraction directly if output is unset (position scales).

Scales are *stateless mappers*: all config lives on `Scale` itself. The same scale instance is shared between plots and across renders.

## Core types

- **`ScaleTypeKind`** (`scale_type.rs`) — enum tagging the scale family: `Continuous`, `Discrete`, `Ordinal`, `Binned`, `Identity`, `Temporal(TemporalUnit)`. Pure data; algorithms are free functions matching on this tag.
- **`Direction`** (`direction.rs`) — `Forward` / `Reversed`. Applied to the normalised fraction (`apply_fraction`) or the domain index (`apply_index`) *before* the output range is consulted, so the one flag reverses a position axis and a palette alike. Not a transform and not a domain shape: the domain keeps its natural order, so break generation is untouched.
- **`Transform`** (`transform.rs`) — POD struct `{ kind: TransformKind }`. Convenience methods (`forward`, `inverse`, `allowed_domain`) delegate to free functions of the same name with `_` suffix.
- **`TransformKind`** — enum tagging the transform family. Thirteen kinds are wired: `Identity`, `Log10`, `Log2`, `Log`, `Sqrt`, `Square`, `Exp10`, `Exp2`, `Exp`, `Asinh`, `PseudoLog`, `PseudoLog2`, `PseudoLog10`. Each has a forward, inverse and allowed-domain arm; `allowed_domain` is what keeps a log scale's domain off zero.
- **`TemporalUnit`** (`scale_type.rs`) — `Date` / `DateTime` / `Time` / `Duration`, the calendar quantity a `Temporal` scale's f64 domain stands for. Decides which calendar-alignment family generates breaks and how tick values are wrapped back into typed `Value`s.
- **`Locale`** (`locale.rs`) — number and date formatting rules (decimal / grouping separators, month and weekday names, first day of week). Threaded into label generation so tick text is locale-correct without the scale layer owning a formatter.
- **`InputRange`** (`input.rs`) — `Continuous { min: f64, max: f64 }` (closed interval; temporal data projects to f64) or `Discrete(Vec<Value>)` (explicit list, user-ordered for ordinal scales). Accessors: `extent()`, `discrete_len()`.
- **`OutputRange`** (`output.rs`) — `Numbers(Vec<f64>)` (pt for absolute sizes, unitless otherwise), `Strings(Vec<Arc<str>>)`, `Colors(Vec<Color>)`, `Linetypes(Vec<Arc<[LinetypeStep]>>)`. Position scales typically leave this unset; continuous scales then return `[0, 1]` fraction. Never doubles as configuration — a binned scale's edges are a separate argument, so any family can carry any palette. A `Colors` range interpolates through the [`ColorSpace`](crate::color::ColorSpace) the mapping function is handed — `Oklab` by default, so a two-stop ramp reads as an even perceptual progression instead of dipping dark through the middle.
- **`AxisSide`** / **`LegendSide`** (`chrome.rs`) — placement enums (Left / Right / Bottom / Top, Top / Bottom / Left / Right). No logic; rendering lives in `crate::plot::chrome::{axis, legend}`.
- **`Geometry`** (`geometry.rs`) — spatial-feature enum (Point / MultiPoint / LineString / MultiLineString / Polygon / MultiPolygon / GeometryCollection / Empty). Carried by `Value::Geometry(Arc<Geometry>)` so a column of features behaves like any other typed channel. **Opaque to scales** — geometries don't enter continuous or discrete domains and cannot be mapped through `scale.map`; the consuming geom walks the geometry and routes each coordinate through the bound `x` / `y` scales itself. Optional WKT / WKB / GeoJSON constructors gate behind `geom-wkt` / `geom-wkb` / `geom-geojson` features; each parser is hand-rolled and dependency-free.
- **Per-kind free functions** (`scale_type.rs`): `continuous_map`, `discrete_map`, `ordinal_map`, `binned_map`, `identity_map`; `continuous_breaks`, `continuous_minor_breaks`, `discrete_breaks`, `binned_breaks`, `temporal_breaks`, `temporal_breaks_with_interval`, `temporal_minor_breaks`, `temporal_minor_breaks_with_interval`; `binned_map_break`; `wrap_temporal_value` (raw f64 position → its calendar `Value`); `discrete_band_width`, `binned_band_width`, `binned_band_width_at`.
- **Transform dispatch** (`transform.rs`): `transform_forward`, `transform_inverse`, `transform_allowed_domain` — all take `kind: TransformKind`. Break generation dispatches on the same tag through the private `transform_breaks` / `transform_minor_breaks` in `scale_type.rs`, reached via `continuous_breaks` / `continuous_minor_breaks` — a continuous scale's ticks follow its transform without the caller choosing an algorithm.
- **Tick selection** (`breaks.rs`): `extended_breaks` (Wilkinson) and `linear_breaks` (evenly-spaced) for linear domains; `log_pretty_breaks` / `log_minor_breaks` for the log family; `sqrt_breaks`; `symlog_breaks` / `symlog_minor_breaks` for Asinh and the PseudoLog family; `linear_minor_breaks_between` for subdividing majors.
- **Calendar arithmetic** (`breaks.rs`): `pick_temporal_interval` sizes a `TemporalInterval` to a target tick count; `derive_minor_interval` picks the sub-interval. Per-type `align_*`, `advance_*`, `retreat_*`, `temporal_breaks_*` and `temporal_minor_breaks_*` families cover `Date` (days), `DateTime` (µs) and `Time` (ns), with `temporal_breaks_from_f64` / `temporal_minor_breaks_from_f64` as the projected-f64 entry points. `temporal_minor_breaks_from_f64_with_interval` takes the major interval instead of deriving it from a tick target, so minors under a pinned major interval subdivide that interval.

Rendering of axis and legend chrome lives in `crate::plot::chrome::{axis, legend}`, not here — that's hephaestus's own surface against `SceneBuilder`. Future `scales`-crate consumers (e.g. ggsql) supply their own rendering.

## Scale types

- **Continuous** — linear interpolation over a numeric domain. Output range can be unset (→ `[0, 1]` fraction), `Numbers` (piecewise-linear across stops), or `Colors` (componentwise).
- **Discrete** — one-to-one lookup: `input[i]` → `output[i]`.
- **Ordinal** — ordered discrete domain with continuous output. Input position `idx / (n - 1)` interpolated through the output range — intermediate domain entries fall on gradient stops.
- **Binned** — continuous domain pre-binned by explicit edges. With no output range it positions data at bin centres; with one, the bin index picks a palette entry the ordinal way (N entries over N bins is one-to-one, fewer interpolate). The edges travel in their own argument (`bins: Option<&[f64]>` on the free functions, a `bins` field on hephaestus's `Scale`), so `OutputRange` means the same thing here as for every other family.
- **Identity** — pass-through; input returned untouched.
- **Temporal** — a continuous domain carrying a calendar quantity, tagged by `TemporalUnit`. Maps linearly exactly as Continuous does; the difference is break generation, which lands on calendar boundaries (year / quarter / month / week / day / hour / minute / second) instead of Wilkinson "nice numbers", and tick values come back as typed `Value::Date` / `DateTime` / `Time` / `Duration` so formatters can reverse the f64 projection.

## Conventions

- **Scales are stateless.** All configuration lives on `Scale`. No per-frame mutation.
- **Temporal data projects to f64 before entering a scale.** Date → days, DateTime → microseconds, Time → nanoseconds since midnight, Duration → microseconds. The domain and ticks are always f64-based; axis label formatters reverse the projection.
- **Break placement can differ from the data mapping.** Chrome positions ticks and gridlines with `Scale::map_break`, not `Scale::map`. The two agree for every family except Binned, whose `map` sends data to the centre of its bin while its breaks are the bin edges — those must land on their own domain fraction. A new scale type only needs a `map_break` arm when the same distinction applies.
- **Colour interpolation names its space.** `continuous_map` / `ordinal_map` / `binned_map` take a `ColorSpace` argument, threaded from `Scale::color_space` (`with_color_space` / `set_color_space` configure it; the default is `Oklab`). `discrete_map` doesn't — a discrete palette is a one-to-one lookup with nothing to interpolate. Exact stops come back untouched in either space, so a palette's endpoints survive the round trip.
- **Band width is a scale-type concept.** Discrete / ordinal / binned report `1.0 / n_bins`; continuous reports 0. Geoms use `scale.map_with_offset(value, band_offset)` to fold a `[0, 1]` within-band offset into the position output. Without a scale (no binding), the band offset is ignored — band is meaningless outside a scale. Band offsets are anchored to the domain, so a `Reversed` scale negates them: flipping an axis mirrors within-band placement along with everything else drawn on it.
- **Reversal is a `Direction`, never a backwards domain.** Reading the domain end-to-start only works for a linear continuous scale by coincidence; a binned scale rejects everything outside `min..=max` and every tick algorithm past the linear one requires `lo < hi`. So `Direction::Reversed` mirrors the mapping and the domain stays ascending. Two consequences worth knowing: `breaks()` returns the same values either way (only where they land changes), and a legend lists its domain in domain order either way — a reversed material scale shows the same rows against a mirrored palette, matching Vega-Lite's `scale.reverse`. Descending endpoints are still ordered on the way into break generation (`ordered` in `scale_type.rs`) rather than silently emitting no ticks.
- **Generation counter is plumbed but unused in v1.** `Scale::generation` is bumped on every mutation; v1.5+ will use it to invalidate per-channel output caches without value comparison.
- **`OutputRange::Numbers` is in pt for absolute sizes**, unitless otherwise. The geom's `resolve_*` helper applies `pt_to_px` where appropriate.
- **Non-numeric endpoints have no continuous ordering.** `Scale::domain_continuous(String("a"), String("b"))` panics at the call site; `try_domain_continuous` / `try_set_domain_continuous` hand the offending `Value` back instead, for endpoints that come from data rather than literals. Use `domain_discrete` for categories.

## Adding a new scale type

1. Extend the `ScaleTypeKind` enum with the new variant.
2. Add a per-kind free function pair (`my_kind_map(...)`, `my_kind_breaks(...)`, optionally `my_kind_band_width(...)` / `my_kind_band_width_at(...)`). The map function takes a `Direction` and applies it to whatever it normalises — a fraction via `apply_fraction`, an index via `apply_index` — unless the kind normalises nothing (`identity_map`).
3. Add the new arm to each central `match` in `crate::plot::scale::Scale::{map, breaks, band_width, band_width_at}`. Rust's exhaustive-match check makes the missing arms compile errors — easy to find them all.
4. Geoms don't directly interact with scale types — they call `scale.map(&value)`. No geom changes needed unless the new type implies a new `ExpectedOutput` variant.

## Cross-references

- `src/scales/value.rs` — the `Value` enum scales map; `DataColumn`; temporal newtypes (`Date`, `DateTime`, `Time`, `Duration`); `LinetypeStep`. Co-located with scales because they're the data scales operate on.
- `src/plot/geom/resolve.rs` — the helpers geoms use to apply a scale per row. `resolve_position` (Value → panel fraction), `resolve_color_channel` (Value → Color), `resolve_linetype_channel` (Value → dash pattern).
- `src/plot/composition.rs` — `PlotComposition::add_scale` / `update_scale` are the user-facing entry points. Scale mutations through `update_scale` bump the generation and mark dependent plots dirty.
- `src/plot/chrome/` — axis / legend rendering: `axis.rs`, `linear_axis.rs`, `polar.rs`, `strip.rs`, `panel.rs`, and `legend/`. Pulls breaks / format / band info from `Scale` and draws them via `SceneBuilder` + `TextRun`.
