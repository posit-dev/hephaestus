# src/plot/theme/CLAUDE.md

The theming system for the high-level plot API. Defines visuals for every chrome surface (plot / panel / axes / legends / strips) and every geom's per-channel defaults, all resolved through one cascade with one set of concrete fallbacks.

## What this module does

A **theme** is the visual specification a `PlotComposition` applies to every attached plot at render. It bundles:

- a semantic colour palette (paper / ink / accent),
- root text / line / rect styling that every typed sub-element cascades from,
- chrome slots (titles, backgrounds, grid lines, axes, legends, strips),
- per-geom default styles, and
- a `Locale` threaded into tick formatting.

The orchestrator owns one `Arc<Theme>` (`SharedTheme`) and applies it to every plot. Each `Plot` may carry an optional sparse `ThemePart` override, merged on top of the composition theme at render time so per-plot tweaks don't fork the whole theme tree.

## Layered design

The theme system has **four mechanically independent layers** that combine at resolve time:

1. **Semantic palette** — `Palette { paper, ink, accent }`. Every chrome colour is a [`ThemeColor`] reference into the palette (or a `Mix` / `Alpha` of palette anchors), so `Theme::invert()` swaps paper ↔ ink to flip a light theme into a dark one in one line.
2. **Reusable element types** — `TextElement` / `LineElement` / `RectElement`. Each field is `Option<T>` and cascades per-field through the parent chain; after cascading, any remaining `None` falls through to the per-type `*_concrete_defaults()` safety net.
3. **`Element<T>` wrapper** — every chrome slot is `Element::Inherit` (walk up), `Element::Blank` (skip the draw call), or `Element::Set(T)` (override). `Blank` short-circuits to `None`; it never falls through.
4. **Cascade containers** — `PerChannel<T>` (per-projection-channel) and `Sided<T>` (per-(channel, side)) for chrome that varies by axis / facet side, plus `PerAxis` for the richer per-field axis cascade.

Mixing these — palette references + Option-per-field cascade + Inherit/Blank/Set + per-side containers — is what lets one `Theme` express both "thicker tick marks everywhere" and "hide just the bottom-axis title" without per-slot copy-paste.

## Module map

- **`palette.rs`** — `Palette` (paper / ink / accent) and `ThemeColor` (`Fixed` / `Paper` / `Ink` / `Accent` / `Mix` / `Alpha`).
- **`length.rs`** — `Length::Abs(pt)` / `Length::Rel(multiplier)` plus the `Margin` 4-sided container. `Length::resolve(parent_pt)` is one step; walking the inheritance chain is the caller's job.
- **`element.rs`** — `Element<T>` (the Inherit / Blank / Set wrapper), the three element types (`TextElement` / `LineElement` / `RectElement`), the `*_concrete_defaults()` safety-net constructors, and the alignment / rotation enums (`HAlign`, `VAlign`, `AlignTo`, `Rotation`).
- **`font.rs`** — `FontSpec` (sparse modern font surface: family / weight / width / style / features / variations) with per-field cascade and per-tag list merge.
- **`cascade.rs`** — `PerChannel<T>` and `Sided<T>` — the wholesale-cascade containers used for grid lines and strips.
- **`axis.rs`** — `AxisTheme`, `PerAxis` (three-layer per-field cascade), `ResolvedAxis` (the bundle returned by `PerAxis::resolve`), and `axis_concrete_defaults()`.
- **`legend.rs`** — `LegendTheme` with `KeyTheme` / `BarTheme` sub-structs and `Direction` (`Auto` / `Horizontal` / `Vertical`).
- **`geom.rs`** — `GeomTheme` and the per-geom default sub-structs (`PointDefaults`, `LineDefaults`, `ShapeDefaults`, `TextDefaults`, `TextFitDefaults`).
- **`theme.rs`** — the top-level `Theme` struct, its sparse `ThemePart` mirror, and `SharedTheme = Arc<Theme>`.
- **`builtin.rs`** — pre-built variants: `Theme::default()` / `dark()` / `minimal()` / `classic()` / `bw()` / `void()`.

## The cascade — how a field resolves

For a chrome field at draw time, the resolver walks **most-specific → least-specific** and stops at the first concrete value, per-field:

1. **Per-plot override** — `Plot::theme_override(ThemePart)` merged on top of the composition theme by `Theme::merge`.
2. **Most-specific container slot** — for axes: `by_channel_side[ch][side]` → `by_channel[ch]` → `all`. For grids: `by_channel[ch]` → `all`. For strips: `by_channel_side[ch][side]` → `by_channel[ch]` → `all`.
3. **`Element<T>` semantics** — `Set(v)` wins; `Blank` short-circuits to `None` (the chrome renderer skips the draw call); `Inherit` skips that layer and continues.
4. **Per-field merge** — within an `Element::Set(T)`, each field of `T` is `Option<...>` and cascades independently against the parent layer's resolved `T`.
5. **Root element** — chrome that has a sibling root (axis title / axis text cascade through `theme.axis.all.text`, every text-shaped slot ultimately through `theme.text`) merges through it before the safety net.
6. **Per-type concrete defaults** — `text_concrete_defaults()` / `line_concrete_defaults()` / `rect_concrete_defaults()` / `axis_concrete_defaults()`. Any `Option` still `None` after the chain picks up its absolute fallback here. These constants are also the bottom-of-cascade *parent* values that `Length::Rel` resolves against (e.g. axis tick label `Rel(0.8)` ultimately reads against `DEFAULT_TEXT_SIZE_PT = 11.0`).

The `cascade_*` helpers in `axis.rs` implement steps 3–5 for the axis layers; `Element::cascade` and the per-element `TElement::cascade(&self, parent)` methods do the per-field work elsewhere.

**`Blank` is not `None`.** `Element::Blank` means "the user said hide this"; it does not walk further up. `Element::Inherit` means "no opinion at this layer"; it falls through.

## Length and `rel()`

Every numeric measurement that benefits from inheritance is a `Length`:

- `Length::Abs(pt)` — absolute pt value. Parent ignored.
- `Length::Rel(m)` — multiplier against the parent's already-resolved pt value. `Rel(1.5)` = 1.5× the parent.

Resolution is one hop: `length.resolve(parent_pt)`. The caller is responsible for assembling the parent chain. The `DEFAULT_*_PT` constants exported from each submodule (`DEFAULT_TEXT_SIZE_PT`, `DEFAULT_LINEWIDTH_PT`, `DEFAULT_TICK_LENGTH_PT`, etc.) are the bottom-of-chain parent values — chrome sites that need to resolve a `Rel(_)` without a deeper parent use those constants directly.

`Length::default() = Rel(1.0)` — "same as the parent". A safe default for sub-element fields that should inherit unless explicitly overridden.

## Palette mechanics

Three semantic anchors:

- `paper` — background (panel + plot backgrounds, light grids in light themes).
- `ink` — foreground (text, axis lines, panel borders, default stroke for geoms).
- `accent` — highlight (default fill for geoms with no fill scale; legend / strip accents).

`ThemeColor::Mix(a, b, t)` and `ThemeColor::Alpha(inner, a)` build derived shades. Defaults use mixes of paper/ink to reproduce ggplot2's grey anchors — `ThemeColor::mix(Paper, Ink, 0.08)` resolves to grey92 against the default white-paper / black-ink palette, and to the symmetric dark shade after `invert()`. That's the whole reason chrome doesn't hardcode RGB constants: a single `invert()` should round-trip cleanly.

Use `ThemeColor::Fixed(Color::rgb(...))` only for things that must lock to a specific colour regardless of palette (a red error annotation, etc.).

## Axis cascade specifics

`AxisTheme` carries seven slot fields plus five typed `Option<Length>` / `Option<TitleLocation>` fields. The axis-specific cascade in `PerAxis::resolve(ch, side)` does **per-field merging**, not whole-element override — a user who sets `by_channel_side[0][0].tick_length` without touching anything else gets just that one field changed, with every other field walking up through `by_channel[0]` → `all` → defaults.

The `ResolvedAxis` returned is a concrete bundle ready to draw. `Length` fields stay unresolved at this stage so the chrome renderer can apply them against the right per-call parent (text size against base text, tick length against the line root).

Legends reuse `AxisTheme` for their tick-labels-and-ticks component (`LegendTheme.axis`); `axis.title` is ignored on legends — the legend's overall title sits on `LegendTheme.title`. This sharing is what lets "thicker tick marks everywhere" propagate to legend bar ticks too.

## Rotation and curved baselines

`Rotation::Along` / `Across` are surface-aware:

- **Straight baselines** (Cartesian axes, colorbar rails) — `Along` rotates the text as a single string parallel to the baseline (0° on Top / Bottom, 90° on Left / Right); `Across` is perpendicular.
- **Curved baselines** (polar angular axes — title and tick labels) — `Along` lays the text out **along the arc** via text-on-path so each glyph sits at its own tangent; `Across` orients each character radially. The chrome renderer picks the text-on-path path when the surface is curved.

`Rotation::Degrees(d)` is unconditional — always a single straight rotation regardless of surface.

## Geom defaults

`GeomTheme` mirrors every geom's old hardcoded `DEFAULT_*` constants. Geoms read defaults at draw time from `ctx.theme.geom.<geom>.<field>` via the `resolve_color_channel_or_theme` / `resolve_cap_channel` / etc. helpers in `plot/geom/resolve.rs`. The pattern:

- A bound channel wins (channel value resolved through scale).
- No channel bound → theme default applies.
- Theme default is `None` (colours) → the geom emits nothing for that aesthetic (the pre-theme "channel-or-nothing" semantic).

The defaults are intentionally minimal — only style-related values a theme might reasonably override. Geometric / semantic constants (rect band offsets, B-spline degree, partial-wedge sweep) stay as constants in each geom because they describe meaning, not appearance.

`Theme::default()`'s built-in `GeomTheme` leaves colour fields at `None` to preserve historic semantics; a populated palette-anchored theme is the role of the ggplot2-style defaults extension.

## Integration points

- **`PlotComposition`** (`plot/composition.rs`) — holds `theme: Arc<Theme>`. `theme()` / `set_theme()` / `update_theme(closure)` mutate it; `update_theme` clones via `Arc::make_mut`. `effective_theme_for(plot)` produces the per-plot merged theme used at draw.
- **`Plot::theme_override`** (`plot/plot.rs`) — installs a `ThemePart` on a single plot. Merged on top of the composition theme each render via `Theme::merge`.
- **`GeomContext`** (`plot/geom/mod.rs`) — carries `theme: &Theme`. The orchestrator threads the resolved per-plot theme through via `GeomContext::with_theme`. Standalone test callers get a static `Theme::default()` reference.
- **Chrome rendering** (`plot/chrome/`) — axes, legends, panel, strips read from the theme's element trees and palette. `theme.legend_for(variant)` resolves a per-legend variant name to its `LegendTheme`, falling back to `theme.legend` when unregistered.

## Built-in variants

- **`Theme::default()`** — mirrors ggplot2's `theme_gray()`: 11pt base text, white paper / black ink, grey92 panel fill with white grid lines, no axis baseline, grey20 ticks, bold left-aligned title, etc.
- **`Theme::dark()`** — `Theme::default().invert()`. Palette-driven, no element edits.
- **`Theme::minimal()`** — paper background, no panel border, only major grid (minor grid set to `Blank`).
- **`Theme::classic()`** — paper background, no panel border, no grid (both grids `Blank`).
- **`Theme::bw()`** — explicit grey palette, grid colours re-derived against the new palette.
- **`Theme::void()`** — every panel and axis element set to `Blank`; only data marks render.

## Conventions

- **American spelling.** `color`, `gray`, `behavior` (the core `Color` type sets the convention; prose and identifiers follow).
- **Every chrome field must be consumed by chrome.** Hardcoded constants in chrome that bypass the theme are bugs — the theme is the single source of truth at draw time. The only chrome-facing constants are the `*_concrete_defaults()` safety-net values and the `DEFAULT_*_PT` parent values that resolve `Length::Rel` at the bottom of the cascade.
- **`ThemePart` is sparse.** Every field is `Option<>` or `Element::Inherit`; only set what you mean to override. The merge is wholesale per top-level field — a `Some(PerChannel { ... })` on `panel_grid_major` replaces the whole `PerChannel`, not its individual slots. Use `Element::Inherit` inside the `PerChannel` to skip a slot.
- **Palette-relative is the default.** New chrome colours should be `ThemeColor::Paper` / `Ink` / `Accent` / `Mix(...)`, not `Fixed(...)`. Fixed colours don't invert.
- **No backwards-facing language in element docs.** Element docs describe current behaviour only; the cascade semantics live here and in module-level docs, not on individual field docstrings.

## Cross-references

- `plot/composition.rs` — `PlotComposition` ownership of `Arc<Theme>` and the `effective_theme_for(plot)` merge.
- `plot/plot.rs` — `Plot::theme_override` per-plot override surface.
- `plot/geom/` — `GeomContext::theme` and the `resolve_*_or_theme` helpers in `plot/geom/resolve.rs`.
- `plot/chrome/` — axis / legend / strip / panel renderers consuming the resolved theme.
- `scales::Locale` — threaded into tick formatting via `scale.format(&value, &theme.locale)`.
