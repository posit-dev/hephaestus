# src/scales/CLAUDE.md

Scales — value mappers. Map a domain `Value` to a visual output (panel fraction, colour, pt size, dash pattern).

**Free functions, no traits.** Every algorithm in this module is a plain free function over enum tags + POD config types — no `Arc<dyn>`, no `ScaleTypeTrait` / `TransformTrait`. Dispatch is `match` on `ScaleTypeKind` / `TransformKind`. Adding a new scale type or transform means extending the enum, writing a per-kind free function, and adding the new arm to each central match. Hephaestus has a closed, well-understood set of variants; runtime polymorphism via traits was overkill and added a layer of indirection across the upcoming crate boundary.

**Leaf-module convention.** Nothing inside `src/scales/` imports from `crate::plot::*`, `crate::scene::*`, `crate::backend::*`, `crate::primitives::*`, or `crate::text::*`. It depends only on `std`, peniko (via `crate::color`), and its own siblings. The module is structured this way so it can be lifted into its own crate (`scales`) when the API settles. The lift is a `mv src/scales crates/scales/src && cargo init --lib`-style migration — no surface changes.

**No `Scale` aggregate here.** The hephaestus `Scale` struct (bundle of scale_type + transform + ranges + generation counter) lives in `src/plot/scale.rs`. Future consumers of the scales crate roll their own bundle and call the free functions directly. The Scale struct's methods are 1-line shims that match on the enum tags and delegate.

Axes and legends — *rendering* of a scale's ticks / breaks against a `SceneBuilder` — live in `src/plot/chrome/` (hephaestus-internal; feature-gated on `text`). The scale layer here defines *what* to draw; chrome draws it.

## What this module does

A `Scale` combines a `ScaleType` (Continuous / Discrete / Ordinal / Binned / Identity), an optional `Transform` (Identity in v1), an `InputRange` (domain), and an optional `OutputRange` (visual range). Mapping flow:

1. Apply the transform (continuous scales only).
2. Normalise to `[0, 1]` against the input range.
3. Interpolate through the output range, or return the fraction directly if output is unset (position scales).

Scales are *stateless mappers*: all config lives on `Scale` itself. The same scale instance is shared between plots and across renders.

## Core types

- **`ScaleTypeKind`** (`scale_type.rs`) — enum tagging the scale family: `Continuous`, `Discrete`, `Ordinal`, `Binned`, `Identity`. Pure data; algorithms are free functions matching on this tag.
- **`Transform`** (`transform.rs`) — POD struct `{ kind: TransformKind }`. Convenience methods (`forward`, `inverse`, `allowed_domain`) delegate to free functions of the same name with `_` suffix.
- **`TransformKind`** — enum tagging the transform family. Identity is wired today; Log10/Log2/Log/Sqrt/Square/Exp10/Exp2/Exp/Asinh/PseudoLog land in Phase E.1.
- **`InputRange`** (`input.rs`) — `Continuous { min: f64, max: f64 }` (closed interval; temporal data projects to f64) or `Discrete(Vec<Value>)` (explicit list, user-ordered for ordinal scales). Accessors: `extent()`, `discrete_len()`.
- **`OutputRange`** (`output.rs`) — `Numbers(Vec<f64>)` (pt for absolute sizes, unitless otherwise), `Strings(Vec<Arc<str>>)`, `Colors(Vec<Color>)`, `Linetypes(Vec<Arc<[LinetypeStep]>>)`. Position scales typically leave this unset; continuous scales then return `[0, 1]` fraction.
- **`AxisSide`** / **`LegendSide`** (`chrome.rs`) — placement enums (Left / Right / Bottom / Top, Top / Bottom / Left / Right). No logic; rendering lives in `crate::plot::chrome::{axis, legend}`.
- **`Geometry`** (`geometry.rs`) — spatial-feature enum (Point / MultiPoint / LineString / MultiLineString / Polygon / MultiPolygon / GeometryCollection / Empty). Carried by `Value::Geometry(Arc<Geometry>)` so a column of features behaves like any other typed channel. **Opaque to scales** — geometries don't enter continuous or discrete domains and cannot be mapped through `scale.map`; the consuming geom walks the geometry and routes each coordinate through the bound `x` / `y` scales itself. Optional WKT / WKB / GeoJSON constructors gate behind `geom-wkt` / `geom-wkb` / `geom-geojson` features; each parser is hand-rolled and dependency-free.
- **Per-kind free functions** (`scale_type.rs`): `continuous_map`, `discrete_map`, `ordinal_map`, `binned_map`, `identity_map`; `continuous_breaks`, `discrete_breaks`, `binned_breaks`; `discrete_band_width`, `binned_band_width`, `binned_band_width_at`.
- **Transform dispatch** (`transform.rs`): `transform_forward`, `transform_inverse`, `transform_allowed_domain` — all take `kind: TransformKind`.
- **Tick selection** (`breaks.rs`): `extended_breaks` (Wilkinson), `linear_breaks` (evenly-spaced). E.1 will add `log_pretty_breaks`, `log_minor_breaks`, `sqrt_breaks`, `symlog_breaks`, etc.

Rendering of axis and legend chrome lives in `crate::plot::chrome::{axis, legend}`, not here — that's hephaestus's own surface against `SceneBuilder`. Future `scales`-crate consumers (e.g. ggsql) supply their own rendering.

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

1. Extend the `ScaleTypeKind` enum with the new variant.
2. Add a per-kind free function pair (`my_kind_map(...)`, `my_kind_breaks(...)`, optionally `my_kind_band_width(...)` / `my_kind_band_width_at(...)`).
3. Add the new arm to each central `match` in `crate::plot::scale::Scale::{map, breaks, band_width, band_width_at}`. Rust's exhaustive-match check makes the missing arms compile errors — easy to find them all.
4. Geoms don't directly interact with scale types — they call `scale.map(&value)`. No geom changes needed unless the new type implies a new `ExpectedOutput` variant.

## Cross-references

- `src/scales/value.rs` — the `Value` enum scales map; `DataColumn`; temporal newtypes (`Date`, `DateTime`, `Time`, `Duration`); `LinetypeStep`. Co-located with scales because they're the data scales operate on.
- `src/plot/geom/resolve.rs` — the helpers geoms use to apply a scale per row. `resolve_position` (Value → panel fraction), `resolve_color_channel` (Value → Color), `resolve_linetype_channel` (Value → dash pattern).
- `src/plot/composition.rs` — `PlotComposition::add_scale` / `update_scale` are the user-facing entry points. Scale mutations through `update_scale` bump the generation and mark dependent plots dirty.
- `src/plot/chrome/{axis,legend}.rs` (gated on `text`) — axis / legend rendering. Pulls breaks / format / band info from `Scale` and draws them via `SceneBuilder` + `TextRun`.
