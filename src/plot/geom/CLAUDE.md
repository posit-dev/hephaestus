# src/plot/geom/CLAUDE.md

Vectorised drawing primitives — geoms. Consume per-channel data + a `Scale` registry and emit `SceneBuilder` calls.

## What this module does

A geom is a trait-erased (`Box<dyn Geom>`) drawing primitive that holds typed columnar channel data, looks up scales by channel name at draw time, and writes scene primitives. Geoms are heterogeneous in a `Plot` (one plot can hold a `PointGeom` + a `LineGeom` + a `TextGeom`), so the trait surface is the lowest common denominator: declare channels, rebuild diff, draw.

## Concrete geoms

- **`PointGeom`** — markers from `ShapeRegistry`, one mark per row.
- **`LineGeom`** — polylines, one mark per key group (multi-row-per-mark). Maintains a `Vec<MarkSlot>` cache.
- **`BSplineGeom`** — clamped uniform-knot B-spline curves, one mark per key group. Per-row `(x, y)` are control points; per-mark `degree` (default 3) selects curve order. The `"interpolation"` channel (`"domain"` / `"panel"`) picks whether the spline is built in channel-fraction space and projected (faithful) or in pixel space after projecting control points (smoothed polyline through projected vertices). Inherits LineGeom's full stroke / linetype / dash / marker channel surface including ribbon-mode variance-detect upgrade and Phase C.5 endpoint markers.
- **`SegmentGeom`** — 2-point line segments (`x0`, `y0`, `x1`, `y1`).
- **`RectGeom`** — axis-aligned rectangles.
- **`EllipseGeom`** — ellipses (centre + radii).
- **`PolygonGeom`** — closed polygons; multi-row-per-mark like `LineGeom`.
- **`RibbonGeom`** — filled band between two curves; multi-row-per-mark like `LineGeom`. Orientation is selected by which of `x2` / `y2` are supplied: only `y2` → horizontal (curve B shares `x`), only `x2` → vertical (curve B shares `y`), both → free (curve B fully independent). At least one of the two is required. Variable fill renders as a linear-gradient brush in the axis-aligned + linear-projection case (fast path); free orientation or any non-linear projection routes through `ribbon_band_mesh` for a per-vertex-coloured quad strip.
- **`WedgeGeom`** — pie / donut slices.
- **`TextGeom`**, **`TextFitGeom`**, **`TextPathGeom`** (gated on `text`) — text marks, text fitted to a box, text along a path.

## Core trait surface

- **`Geom`** trait — three responsibilities: `declared_channels` (orchestrator validates bindings and wires anatomy chrome), `rebuild_diff_against_previous` (runs lazily before `draw` when dirty), `draw` (emit scene primitives). Plus `state` / `state_mut` accessors and `as_any_mut` for downcasting in `Plot::update_geom`. `len`, `is_empty`, `mark_count`, `invalidate_caches` have sensible defaults; multi-row-per-mark geoms override `mark_count` and `invalidate_caches`.
- **`BuildableGeom`** — `fn build_from(GeomBuilder<Self>) -> Self`. Geom-specific validation, default injection, and channel-declaration computation live here. There's no per-geom builder type — `GeomBuilder<G>` is generic.
- **`GeomBuilder<G>`** — geom-agnostic builder. Holds an optional key column + a `HashMap<String, Channel>`. Methods take `&mut self` and return `&mut Self`, so the same shape works for initial construction and inside `update` closures.
- **`GeomState`** (`state.rs`) — the universal state every concrete geom holds: `keys`, `channels`, `prev_keys`, `prev_channels`, diff results (`enter` / `update` / `exit`), dirty flag, declared channel list. Per-geom impls compose this with any extra caches they need.
- **`KeysStrategy`** (`state.rs`) — drives `Keys` synthesis in `from_builder`. Positional vs explicit, plus the per-row vs one-mark variants for grouped geoms.

## Channels

- **`Channel`** enum — `Constant(Value)` (scalar applied to every row), `Data(DataColumn)` (one value per row), `RawConstant(Value)`, `RawData(DataColumn)` (scale-bypassing). `Into<Channel>` blanket handles type coercion from `f64` / `Vec<f64>` / `&str` / `Vec<&str>` / `Color` / `Vec<Color>` etc.
- **`Raw<T>`** wrapper — turns the inner value into the scale-bypassing variant via `Into<Channel>`. `Raw(0.5_f64)` → `Channel::RawConstant`; `Raw(vec![0.1, 0.5, 0.9])` → `Channel::RawData`.
- **`ChannelDecl`** — declared channel metadata (name + expected output type). Geoms declare their full channel list as a `const CHANNELS: &[(&str, ExpectedOutput)]` and filter via `filter_declared` in `build_from`.
- **`ExpectedOutput`** — what output type a channel should resolve to (Position, Color, Size, Linetype, etc.). Used by axis/legend chrome inference.
- **`GeomContext`** — passed to `Geom::draw`. Carries the `ScaleResolver` (production: orchestrator binding map + registry; tests: `DirectScaleResolver`), the panel rect, the DPI, and the shape registry.
- **`ScaleResolver`** trait — looks up `Option<&Scale>` for a given channel name.
- **`DirectScaleResolver<'a>`** — hand-built resolver for tests: `DirectScaleResolver::new().with("x", &scale_x).with("y", &scale_y)`.

## Keys

- **`Keys::Positional(n)`** — conceptual `(0..n)` key column stored as just the row count. Zero allocation. Diff matches by position.
- **`Keys::Explicit(DataColumn)`** — user-supplied key column. Diff matches by identity. Used by grouped geoms (`LineGeom`, `PolygonGeom`) where all rows sharing a key form one mark.

`Keys::empty_like()` produces the length-zero counterpart of the same variant — used to seed the first-frame `prev_keys` snapshot so diff produces all-enter on the first draw.

## Resolution helpers (`resolve.rs`)

Every geom maps the same kind of raw `(Channel, Option<&Scale>, row_idx)` triple to a typed visual output. The helpers centralise that machinery so each geom's draw loop reads as the geom-specific logic only.

- **`resolve_position(raw, scale, band_offset)`** — Value → `[0, 1]` panel fraction. Optional band-offset folded in for scales that report band width.
- **`resolve_color_channel`** — Value → `Color`.
- **`resolve_linetype_channel`** — Value → `Arc<[LinetypeStep]>` dash pattern.
- **`pt_to_px(pt, dpi)`** — convert pt to px using `pt * dpi / 72.0`. Same convention for every absolute graphical size (point diameter, stroke linewidth, dash lengths).

Principle: **scale mapping is applied to the raw `Value` before the typed extraction**, so a `"size"` column of categorical strings can flow through an ordinal scale to a numeric output, an `"x"` column of dates can flow through a continuous scale to a `[0, 1]` panel fraction.

## Hot-loop monomorphism

The hot draw loop matches on the `DataColumn` variant **once** at the top, then reads typed slices in the inner per-row body. Per-row code stays monomorphic — no per-row variant dispatch. This is performance-critical for dense plots and is the reason `DataColumn` is variant-typed rather than `Vec<Value>`.

## Adding a new geom

1. Define the struct as `pub struct MyGeom { state: GeomState }` plus any extra caches (e.g. `LineGeom` adds `Vec<MarkSlot>`).
2. Declare the channel list at the top: `const CHANNELS: &[(&str, ExpectedOutput)] = &[("x", ExpectedOutput::Position), ...]`.
3. Implement `BuildableGeom::build_from`: validate (required channels present, lengths match the row count, channel column variants are acceptable), inject defaults, then call `GeomState::from_builder(keys, channels, n, strategy, declared)` and wrap in `Self { state, ...caches }`.
4. Implement `Geom`: four required methods (`state`, `state_mut`, `draw`, `as_any_mut`). Override `mark_count` and `invalidate_caches` only if you cache per-mark layout (`LineGeom`, `PolygonGeom`). Override `rebuild_diff_against_previous` only if you need to rebuild caches after the diff (delegate to `self.state_mut().rebuild_diff_against_previous()` then rebuild the cache).
5. In `draw`: for each channel, get `ctx.scale_for("name")`, then in the per-row loop call the right `resolve_*` helper. Issue scene primitives via the `&mut dyn SceneBuilder`.

## Conventions

- **Per-row vs per-mark.** `PointGeom` uses positional one-per-row keys; `LineGeom` uses explicit one-per-mark keys so all rows sharing a key form one mark. The diff machinery respects the distinction — `LineGeom` overrides `rebuild_diff_against_previous` to rebuild its mark cache after the state-level diff.
- **Validation panics at the call site, not at draw.** Wrong column variants, missing required channels, and length mismatches panic from `build_from` so the runtime geom never has to handle them.
- **Defaults injected at `build_from` time, not at `draw` time.** A missing `"size"` channel is replaced by `Channel::Constant(default_size_pt)` during build, so the draw loop is always fed a valid channel.
- **Raw channels are output-space values.** `Raw(vec![0.1, 0.5, 0.9])` on `"x"` means "these are already panel fractions; bypass any `"x"` scale". Useful when the scale is bound elsewhere but a specific geom should use pre-scaled data.

## Cross-references

- `plot/scale/` — `Scale`, `ScaleType`, `Transform`, `InputRange`, `OutputRange`. The `ScaleResolver` returns `Option<&Scale>` for a channel name.
- `plot/value.rs` — `Value`, `DataColumn`. `DataColumn::key_eq_at` / `key_hash_at` power the diff.
- `plot/diff.rs` — `KeyIndex`, `diff_columns`, `diff_positional`. `GeomState::rebuild_diff_against_previous` calls into these.
- `primitives/` — path constructors geoms use before issuing fill / stroke calls.
- `shape.rs` — `ShapeRegistry` carried in `GeomContext` for marker / endpoint glyphs.
