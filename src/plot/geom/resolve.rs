//! Per-row resolution helpers shared across geom impls.
//!
//! Every geom maps the same kind of raw `(Channel, Option<&Scale>, row_idx)`
//! triple to a typed visual output (color, pt size, dash pattern, etc.).
//! These helpers centralise that machinery so each geom's draw loop reads
//! as the geom-specific logic only.
//!
//! The helpers all share one principle: scale mapping is applied to the
//! raw `Value` *before* the typed extraction, so a `"size"` column of
//! categorical strings can flow through an ordinal scale to a numeric
//! output, an `"x"` column of dates can flow through a continuous scale
//! to a `[0, 1]` panel fraction, etc.

use std::collections::HashMap;
use std::sync::Arc;

use crate::brush::Brush;
use crate::color::Color;
use crate::geometry::{Affine, Point, Vec2};
use crate::path::{FillRule, Path};
use crate::pick::PickId;
use crate::plot::scale::Scale;
use crate::plot::value::{LinetypeStep, Value};
use crate::primitives::PolylineSampler;
use crate::scene::{Glyph, GlyphRun, SceneBuilder};
use crate::shape::{Shape, ShapeKind, ShapeRegistry, ShapeStyle};
use crate::stroke::{Cap, Join, Stroke};

use super::{Channel, GeomContext};

const MARKER_EPSILON: f64 = 1e-9;

/// Maximum valid pick id — the 24-bit `PickId` encoding budget.
pub(crate) const MAX_PICK_ID: u32 = 0xFF_FFFF;

/// A `(channel, scale)` reference pair carried through draw-time
/// channel bundles. Bundling halves the field count of per-geom
/// `*DrawCtx` structs and gives the resolver helpers a natural pair to
/// receive.
#[derive(Clone, Copy)]
pub(crate) struct ChannelBind<'a> {
    pub ch: Option<&'a Channel>,
    pub scale: Option<&'a Scale>,
}

impl<'a> ChannelBind<'a> {
    /// Look up `name` in `channels` for the [`Channel`] handle and in
    /// `ctx` for the matching [`Scale`] handle, bundling them into one
    /// `ChannelBind`. Both lookups are independent — either may return
    /// `None`.
    pub(crate) fn from_ctx(
        channels: &'a HashMap<String, Channel>,
        ctx: &'a GeomContext<'_>,
        name: &str,
    ) -> Self {
        Self {
            ch: channels.get(name),
            scale: ctx.scale_for(name),
        }
    }
}

/// Convert pt to px at the given dpi. The same convention is used for
/// every absolute graphical size (point diameter, stroke linewidth,
/// dash lengths, dash offset).
#[inline]
pub(crate) fn pt_to_px(pt: f64, dpi: f64) -> f64 {
    pt * dpi / 72.0
}

/// Project a row's raw `Value` through an optional position scale to a
/// `[0, 1]` panel fraction, with an optional band-fraction offset folded
/// in. With no scale the input must itself project to a finite f64
/// (numeric or temporal); other variants return `NaN` so the caller
/// skips the row. Without a scale, the band offset is ignored — "band"
/// is a scale-defined concept.
pub(crate) fn resolve_position(raw: Value, scale: Option<&Scale>, band_offset: f64) -> f64 {
    let mapped = match scale {
        Some(s) => s.map_with_offset(&raw, band_offset),
        None => raw,
    };
    mapped.as_number().unwrap_or(f64::NAN)
}

/// Read the raw `Value` at row `i` from a channel and run it through an
/// optional scale. Returns `None` if `channel` itself is `None` (channel
/// unset) — distinct from the scale producing `Value::Null`.
///
/// `Channel::Raw*` variants bypass the scale: the wrapped value flows
/// through as-is, regardless of whether a scale is bound to the
/// channel name. This lets callers draw with pre-computed output-unit
/// values (panel fractions, colours, pt sizes) on a plot whose
/// channels otherwise use scales.
fn resolve_value(channel: Option<&Channel>, scale: Option<&Scale>, i: usize) -> Option<Value> {
    let (raw, bypass_scale) = match channel? {
        Channel::Constant(v) => (v.clone(), false),
        Channel::Data(col) => (col.get(i), false),
        Channel::RawConstant(v) => (v.clone(), true),
        Channel::RawData(col) => (col.get(i), true),
    };
    Some(match (bypass_scale, scale) {
        (true, _) | (false, None) => raw,
        (false, Some(s)) => s.map(&raw),
    })
}

/// Resolve a colour channel. Returns `None` when unset or when the
/// resolved value isn't a colour. Used for `"fill"` / `"stroke"`.
pub(crate) fn resolve_color_channel(
    channel: Option<&Channel>,
    scale: Option<&Scale>,
    i: usize,
) -> Option<Color> {
    resolve_value(channel, scale, i)?.as_color()
}

/// Like [`resolve_color_channel`] but falls back to a theme-provided
/// default when the channel resolves to `None`. The fallback is an
/// `Option<&ThemeColor>` so the geom can pass
/// `ctx.theme.geom.<geom>.fill.as_ref()` directly — `None` keeps the
/// pre-theme "channel-or-nothing" semantic.
pub(crate) fn resolve_color_channel_or_theme(
    channel: Option<&Channel>,
    scale: Option<&Scale>,
    i: usize,
    theme_default: Option<&crate::plot::theme::ThemeColor>,
    palette: &crate::plot::theme::Palette,
) -> Option<Color> {
    resolve_color_channel(channel, scale, i).or_else(|| theme_default.map(|tc| tc.resolve(palette)))
}

/// Resolve an optional numeric channel. Returns `None` when the channel
/// is unset or the resolved value isn't numeric; the caller decides
/// what absence means.
pub(crate) fn resolve_number_channel(
    channel: Option<&Channel>,
    scale: Option<&Scale>,
    i: usize,
) -> Option<f64> {
    resolve_value(channel, scale, i)?.as_number()
}

/// True when the channel resolves to a different value across any pair
/// of rows in `rows`. Used by ribbon-mode dispatch in `LineGeom` /
/// `PolygonGeom` to upgrade from `Op::Stroke` to a per-vertex
/// tessellated mesh only when there is actual within-mark variation.
/// Returns `false` for `Channel::Constant`, unset channels, and data
/// channels whose rows all map to the same value (compared via
/// [`Value::key_eq`] — variant-aware, NaN-canonicalised, same
/// equality the diff machinery uses).
pub(crate) fn channel_varies_across(
    channel: Option<&Channel>,
    scale: Option<&Scale>,
    rows: &[usize],
) -> bool {
    let Some(channel) = channel else { return false };
    if matches!(channel, Channel::Constant(_)) {
        return false;
    }
    let mut first: Option<Value> = None;
    for &i in rows {
        let v = resolve_value(Some(channel), scale, i);
        match (&first, &v) {
            (None, Some(_)) => first = v,
            (Some(a), Some(b)) if !a.key_eq(b) => return true,
            _ => {}
        }
    }
    false
}

/// Resolve a numeric channel with a fallback default. Equivalent to
/// `resolve_number_channel(...).unwrap_or(default)`.
pub(crate) fn resolve_number_channel_or(
    channel: Option<&Channel>,
    scale: Option<&Scale>,
    i: usize,
    default: f64,
) -> f64 {
    resolve_number_channel(channel, scale, i).unwrap_or(default)
}

/// Resolve a boolean channel with a fallback default. Reads
/// `Value::Bool`; any other resolved value (including numeric)
/// falls back to `default` rather than coercing — keeps the channel
/// strictly boolean so a misbound numeric scale doesn't silently
/// flip behaviour.
pub(crate) fn resolve_bool_channel_or(
    channel: Option<&Channel>,
    scale: Option<&Scale>,
    i: usize,
    default: bool,
) -> bool {
    match resolve_value(channel, scale, i) {
        Some(Value::Bool(b)) => b,
        _ => default,
    }
}

/// Resolve a rotation angle channel. Radians, mathematical CCW (positive
/// rotates +x toward +y in math coords; geoms flip internally when
/// emitting to the y-down render space). Returns `0.0` (no rotation)
/// when the channel is unset or the resolved value isn't numeric.
pub(crate) fn resolve_angle_channel(
    channel: Option<&Channel>,
    scale: Option<&Scale>,
    i: usize,
) -> f64 {
    resolve_number_channel(channel, scale, i).unwrap_or(0.0)
}

/// Resolve a linetype channel to a `LinetypeStep` pattern. Falls back
/// to solid (empty array) when the channel is unset or the resolved
/// value isn't a `Value::Linetype`.
pub(crate) fn resolve_linetype_channel(
    channel: Option<&Channel>,
    scale: Option<&Scale>,
    i: usize,
) -> Arc<[LinetypeStep]> {
    match resolve_value(channel, scale, i) {
        Some(Value::Linetype(p)) => p,
        _ => Arc::from(Vec::<LinetypeStep>::new()),
    }
}

/// Resolve a string channel with a fallback default. Used by
/// shape-name lookups; returns a freshly-allocated `String` for
/// matched names. The fallback accepts any `&str` so callers can
/// pass either a `'static` literal or a runtime-owned string
/// (e.g. `&theme.geom.point.shape`).
pub(crate) fn resolve_str_channel_or(
    channel: Option<&Channel>,
    scale: Option<&Scale>,
    i: usize,
    default: &str,
) -> String {
    match resolve_value(channel, scale, i).and_then(|v| v.as_str().map(str::to_owned)) {
        Some(s) => s,
        None => default.to_string(),
    }
}

/// Resolve a cap channel from a string-named value. Recognises `"butt"`
/// / `"round"` / `"square"`; falls back to `default` otherwise.
pub(crate) fn resolve_cap_channel(
    channel: Option<&Channel>,
    scale: Option<&Scale>,
    i: usize,
    default: Cap,
) -> Cap {
    let v = match resolve_value(channel, scale, i) {
        Some(v) => v,
        None => return default,
    };
    match v.as_str() {
        Some("butt") => Cap::Butt,
        Some("round") => Cap::Round,
        Some("square") => Cap::Square,
        _ => default,
    }
}

/// Resolve a join channel from a string-named value. Recognises
/// `"miter"` / `"round"` / `"bevel"`; falls back to `default` otherwise.
pub(crate) fn resolve_join_channel(
    channel: Option<&Channel>,
    scale: Option<&Scale>,
    i: usize,
    default: Join,
) -> Join {
    let v = match resolve_value(channel, scale, i) {
        Some(v) => v,
        None => return default,
    };
    match v.as_str() {
        Some("miter") => Join::Miter,
        Some("round") => Join::Round,
        Some("bevel") => Join::Bevel,
        _ => default,
    }
}

/// Build a kurbo [`Stroke`] from the resolved per-mark channels.
///
/// `pattern` is the resolved `LinetypeStep` slice. When the pattern
/// contains [`LinetypeStep::Marker`] entries, the markers are silently
/// treated as `Gap(linewidth_pt)` here — LineGeom is the only geom
/// that **also** stamps the marker shapes; other stroked geoms use
/// the dashing portion only via this helper. Empty pattern → solid.
///
/// `linewidth_pt` is used both for the stroke width (after pt→px
/// conversion) and as the arc-length contribution per `Marker` step.
pub(crate) fn build_stroke_for_pattern(
    width_px: f64,
    cap: Cap,
    join: Join,
    pattern: &[LinetypeStep],
    offset_pt: f64,
    linewidth_pt: f64,
    dpi: f64,
) -> Stroke {
    let mut s = Stroke::new(width_px).with_caps(cap).with_join(join);
    if !pattern.is_empty() {
        let pattern_px: Vec<f64> = pattern
            .iter()
            .map(|step| match step {
                LinetypeStep::Dash(p) | LinetypeStep::Gap(p) => pt_to_px(*p, dpi),
                LinetypeStep::Marker(_) => pt_to_px(linewidth_pt, dpi),
            })
            .collect();
        let offset_px = pt_to_px(offset_pt, dpi);
        s = s.with_dashes(offset_px, pattern_px);
    }
    s
}

/// Override the alpha channel of `color` with `alpha` (in `0..=1`).
/// `None` color → `None`; `None` alpha → color unchanged.
pub(crate) fn override_alpha(color: Option<Color>, alpha: Option<f64>) -> Option<Color> {
    let c = color?;
    match alpha {
        None => Some(c),
        Some(a) => {
            let [r, g, b, _] = c.components;
            Some(Color::new([r, g, b, a as f32]))
        }
    }
}

/// Look up the band width (in `[0, 1]` panel fraction) for `raw` on
/// `scale`. Continuous scales return 0 (no bands → no contribution).
/// Discrete / Ordinal / Binned return the band width at the value.
/// Used by geoms that scale a dimension by band fraction (e.g.
/// WedgeGeom's `radius_x_band`).
pub(crate) fn band_width_at(scale: Option<&Scale>, raw: &Value) -> f64 {
    match scale {
        Some(s) => s.band_width_at(raw),
        None => 0.0,
    }
}

/// Resolve a `"pick_id"` channel to a [`PickId`] for row `i`.
///
/// - `channel == None` → `PickId::Skip` (picking opt-out — the channel is
///   unset, so this geom doesn't participate in the hitmap).
/// - The raw value (Constant or `Data[i]`, run through `scale` if any)
///   must be a finite non-negative integer ≤ `MAX_PICK_ID`. Otherwise
///   the row reports `PickId::Skip` — same convention as `is_finite`
///   skips elsewhere. Non-integer values are also rejected at draw
///   time (an ordinal scale producing a fractional output would be a
///   bug; loudly skipping is more discoverable than silently
///   truncating).
/// - Value `0` → `PickId::Block` (occlude without reporting). Documented
///   contract so callers whose row indices start at 0 shift to 1+ if
///   they want their rows pickable.
///
/// Grouped geoms (LineGeom / PolygonGeom) call this with the mark's
/// `first_row` index so each mark gets one pick id from its first
/// row's value — matching the "first-row-of-mark" convention used for
/// every other non-position channel on grouped geoms.
pub(crate) fn resolve_pick_id(
    channel: Option<&Channel>,
    scale: Option<&Scale>,
    i: usize,
) -> PickId {
    let n = match resolve_number_channel(channel, scale, i) {
        Some(n) => n,
        None => return PickId::Skip,
    };
    if !n.is_finite() || n < 0.0 || n > MAX_PICK_ID as f64 || n.trunc() != n {
        return PickId::Skip;
    }
    let id = n as u32;
    if id == 0 {
        PickId::Block
    } else {
        PickId::Id(id)
    }
}

/// Walk one or more [`PolylineSampler`]s through a linetype pattern,
/// emitting `scene.stroke` for each `Dash` segment and
/// `scene.fill` / `scene.stroke` for each `Marker` stamp. Advances the
/// arc-length cursor by Dash / Marker (= `linewidth_px`) / Gap as the
/// pattern dictates; the pattern loops when the cursor wraps.
///
/// **Mode**:
/// - `distribute = false` — open polyline. Pattern starts at cursor
///   `-dash_offset_px` and runs until the cursor reaches the end of
///   each sampler. The trailing partial pattern run is silently
///   truncated.
/// - `distribute = true` — closed perimeter. Scale every `Gap` in the
///   pattern by a uniform factor so an integer number of pattern runs
///   exactly fits the sampler's total length. Dashes and marker widths
///   are left untouched. The seam at distance 0 == total_length is
///   invisible: the pattern wraps continuously. A pattern with zero
///   total Gap length cannot stretch — the call falls back to the
///   non-distribute walk.
///
/// `marker_fill` and `marker_stroke` are passed to filled / stroked
/// subpaths of each marker shape respectively (mirroring PointGeom's
/// emission convention). `solid_stroke_spec` is reused for every Dash
/// sub-stroke and for any marker shape whose style is
/// [`ShapeStyle::Stroke`] — the geom is responsible for setting
/// `width / cap / join` correctly.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_linetype_with_markers(
    scene: &mut dyn SceneBuilder,
    samplers: &[PolylineSampler],
    pattern_pt: &[LinetypeStep],
    dash_offset_px: f64,
    linewidth_px: f64,
    marker_fill: Color,
    marker_stroke: Color,
    marker_outline_pt: f64,
    solid_stroke_spec: &Stroke,
    xform: Affine,
    shapes: &ShapeRegistry,
    dpi: f64,
    pick: PickId,
    distribute: bool,
) {
    debug_assert!(
        !pattern_pt.is_empty(),
        "draw_linetype_with_markers: empty pattern"
    );

    for sampler in samplers {
        let total = sampler.total_length();
        if total <= 0.0 {
            continue;
        }

        // Resolve a per-sampler pattern: when `distribute` is set,
        // scale gaps to fit `total` exactly; otherwise use the pattern
        // as-is.
        let pattern_px = resolve_pattern_px(pattern_pt, linewidth_px, dpi, total, distribute);

        let n_steps = pattern_px.len();
        let mut cursor = if distribute { 0.0 } else { -dash_offset_px };
        let mut step_idx = 0usize;
        let mut safety = 0usize;
        // Safety cap proportional to total / linewidth to catch
        // malformed zero-advance patterns.
        let max_iters: usize = (total / linewidth_px.max(1e-3))
            .ceil()
            .clamp(64.0, 1_000_000.0) as usize
            * n_steps
            + n_steps * 4;
        while cursor < total - MARKER_EPSILON && safety < max_iters {
            safety += 1;
            let step = &pattern_px[step_idx];
            step_idx = (step_idx + 1) % n_steps;
            match step {
                ResolvedStep::Dash(len_px) => {
                    let len = *len_px;
                    if len <= 0.0 {
                        continue;
                    }
                    let start = cursor.max(0.0);
                    let end = (cursor + len).min(total);
                    if end > start + MARKER_EPSILON {
                        let path = build_sub_polyline(sampler, start, end);
                        if path.elements().len() >= 2 {
                            scene.stroke(
                                solid_stroke_spec,
                                xform,
                                &Brush::Solid(marker_stroke),
                                None,
                                &path,
                                pick,
                            );
                        }
                    }
                    cursor += len;
                }
                ResolvedStep::Marker(name) => {
                    let mid = cursor + 0.5 * linewidth_px;
                    if mid >= 0.0 && mid <= total + MARKER_EPSILON {
                        if let Some(shape) = shapes.get(name.as_ref()) {
                            if let Some(sample) = sampler.sample_at(mid) {
                                let bbox = shape.bounding_box();
                                let local_h = bbox.height();
                                let scale_factor = if local_h > 0.0 {
                                    linewidth_px / local_h
                                } else {
                                    linewidth_px
                                };
                                let marker_xform_unscaled = xform
                                    * Affine::translate(sample.point.to_vec2())
                                    * Affine::rotate(sample.tangent.atan2());
                                emit_marker_shape(
                                    scene,
                                    shape,
                                    marker_xform_unscaled,
                                    scale_factor,
                                    marker_fill,
                                    marker_stroke,
                                    pt_to_px(marker_outline_pt, dpi),
                                    pick,
                                );
                            }
                        }
                    }
                    cursor += linewidth_px;
                }
                ResolvedStep::Gap(len_px) => {
                    cursor += *len_px;
                }
            }
        }
    }
}

/// Stroke `path` honouring the full linetype contract — marker-free
/// patterns flow through a plain `Stroke::with_dashes`; patterns
/// containing `Marker(...)` steps route through
/// [`draw_linetype_with_markers`], which walks arc-length and stamps
/// shapes at the right cursor positions.
///
/// `closed` selects the sampler constructor: `true` →
/// [`PolylineSampler::from_closed_path`] (closing edge included), `false`
/// → [`PolylineSampler::from_path`]. `distribute` controls whether the
/// marker walk scales gaps to fit the perimeter exactly — `true` for
/// closed paths (no visible seam at the join), `false` for open lines.
///
/// The geom layer is responsible for resolving the per-row colours,
/// pattern, linewidth, cap, join, and pick id. This helper just plumbs
/// them through the dispatch.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_stroke_with_linetype(
    scene: &mut dyn SceneBuilder,
    path: &Path,
    closed: bool,
    stroke_color: Color,
    marker_fill: Color,
    linewidth_px: f64,
    linewidth_pt: f64,
    cap: Cap,
    join: Join,
    dash_pattern_pt: &[LinetypeStep],
    dash_offset_pt: f64,
    xform: Affine,
    pick: PickId,
    shapes: &ShapeRegistry,
    marker_outline_pt: f64,
    dpi: f64,
) {
    if !linewidth_px.is_finite() || linewidth_px <= 0.0 {
        return;
    }
    if super::linetype::is_marker_free(dash_pattern_pt) {
        let stroke_spec = build_stroke_for_pattern(
            linewidth_px,
            cap,
            join,
            dash_pattern_pt,
            dash_offset_pt,
            linewidth_pt,
            dpi,
        );
        scene.stroke(
            &stroke_spec,
            xform,
            &Brush::Solid(stroke_color),
            None,
            path,
            pick,
        );
        return;
    }
    let samplers = if closed {
        PolylineSampler::from_closed_path(path, 0.5)
    } else {
        PolylineSampler::from_path(path, 0.5)
    };
    let solid_stroke_spec = Stroke::new(linewidth_px).with_caps(cap).with_join(join);
    let dash_offset_px = pt_to_px(dash_offset_pt, dpi);
    draw_linetype_with_markers(
        scene,
        &samplers,
        dash_pattern_pt,
        dash_offset_px,
        linewidth_px,
        marker_fill,
        stroke_color,
        marker_outline_pt,
        &solid_stroke_spec,
        xform,
        shapes,
        dpi,
        pick,
        /* distribute */ closed,
    );
}

/// Resolved pattern entry — pt converted to px and (for distribute
/// mode) gaps scaled to fit the polyline total length.
enum ResolvedStep {
    Dash(f64),
    Marker(Arc<str>),
    Gap(f64),
}

/// Convert a pattern from pt → px, optionally distributing gaps so an
/// integer number of pattern runs fits `total_px` exactly.
fn resolve_pattern_px(
    pattern_pt: &[LinetypeStep],
    linewidth_px: f64,
    dpi: f64,
    total_px: f64,
    distribute: bool,
) -> Vec<ResolvedStep> {
    // Pre-compute fixed and gap contributions per pattern run.
    let mut fixed_px = 0.0;
    let mut gap_px = 0.0;
    for step in pattern_pt {
        match step {
            LinetypeStep::Dash(p) => fixed_px += pt_to_px(*p, dpi),
            LinetypeStep::Marker(_) => fixed_px += linewidth_px,
            LinetypeStep::Gap(p) => gap_px += pt_to_px(*p, dpi),
        }
    }
    let period_px = fixed_px + gap_px;

    let gap_scale = if distribute && gap_px > MARKER_EPSILON && period_px > MARKER_EPSILON {
        let n = (total_px / period_px).round().max(1.0);
        let target_gap = (total_px - n * fixed_px) / n;
        // Disallow negative scale (happens when fixed >> total: the
        // pattern's non-gap content alone already exceeds the
        // perimeter). Fall back to 1.0 — the pattern will overflow
        // visually, which matches what an unfittable closed pattern
        // does anyway.
        (target_gap / gap_px).max(0.0)
    } else {
        1.0
    };

    pattern_pt
        .iter()
        .map(|step| match step {
            LinetypeStep::Dash(p) => ResolvedStep::Dash(pt_to_px(*p, dpi)),
            LinetypeStep::Marker(name) => ResolvedStep::Marker(name.clone()),
            LinetypeStep::Gap(p) => ResolvedStep::Gap(pt_to_px(*p, dpi) * gap_scale),
        })
        .collect()
}

/// Forward extent (in pt) that an endpoint-marker shape occupies in
/// front of its anchor, taking `invert` into account. Used by line-
/// and curve-like geoms to extend the user-supplied `clip_*_radius`
/// just enough to make room for the marker so its tip lands at the
/// user-supplied clip boundary (or the original endpoint when no
/// user clip is set).
///
/// The shape's local +x axis is aligned with the outward direction
/// at draw time ([`emit_endpoint_marker`] mode-B). Forward extent
/// from the anchor along that axis is `bbox.x1 - anchor.x` in shape
/// units; when `invert` flips the marker, the new forward direction
/// is the old -x, so the rendered forward extent becomes
/// `anchor.x - bbox.x0`. Multiplied by `size_pt` since
/// [`emit_marker_shape`] scales shape coordinates by `size_px =
/// pt_to_px(size_pt, dpi)`.
///
/// Returns `0.0` when the marker is unset, unknown to the registry,
/// non-positive in size, or has no forward extent. Callers add the
/// result to any user-supplied `clip_*_radius` channel value before
/// trimming.
pub(crate) fn auto_endpoint_clip_pt(
    marker_name: &str,
    size_pt: f64,
    invert: bool,
    shapes: &ShapeRegistry,
) -> f64 {
    if marker_name.is_empty() || !size_pt.is_finite() || size_pt <= 0.0 {
        return 0.0;
    }
    let Some(shape) = shapes.get(marker_name) else {
        return 0.0;
    };
    let bbox = shape.bounding_box();
    let anchor = shape.anchor();
    let extent_units = if invert {
        anchor.x - bbox.x0
    } else {
        bbox.x1 - anchor.x
    };
    extent_units.max(0.0) * size_pt
}

/// Compute the outward direction for an endpoint marker.
///
/// The rule, per Phase C.5: the arrowhead's local +x axis points along
/// the chord from the post-clip endpoint toward the *original* endpoint
/// (i.e. the direction the line "would have continued" if it hadn't
/// been trimmed). When the endpoint wasn't trimmed (`was_clipped =
/// false`), falls back to the terminal polyline edge direction —
/// identical to the chord in that limit.
///
/// Returns a normalised [`Vec2`]; degenerate inputs (single-vertex
/// polyline, coincident neighbour, etc.) return [`Vec2::ZERO`] and the
/// downstream [`emit_endpoint_marker`] no-ops on zero-length vectors.
pub(crate) fn endpoint_outward(
    clipped: &[Point],
    original: &[Point],
    at_start: bool,
    was_clipped: bool,
) -> Vec2 {
    if clipped.len() < 2 {
        return Vec2::ZERO;
    }
    let dir = if was_clipped && !original.is_empty() {
        let (clip_pt, orig_pt) = if at_start {
            (clipped[0], original[0])
        } else {
            (clipped[clipped.len() - 1], original[original.len() - 1])
        };
        orig_pt - clip_pt
    } else if at_start {
        clipped[0] - clipped[1]
    } else {
        let n = clipped.len();
        clipped[n - 1] - clipped[n - 2]
    };
    let len_sq = dir.length_squared();
    if len_sq < 1e-24 {
        Vec2::ZERO
    } else {
        dir / len_sq.sqrt()
    }
}

/// Stamp a registered shape at a polyline endpoint. Mode-B placement:
/// the shape's `anchor()` lands on `placement`; the shape is rotated so
/// its local +x axis aligns with `outward`. `invert` flips the outward
/// direction. No-op if `marker_name` is empty, unknown to the registry,
/// or `outward` is the zero vector.
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_endpoint_marker(
    scene: &mut dyn SceneBuilder,
    placement: Point,
    outward: Vec2,
    invert: bool,
    marker_name: &str,
    size_px: f64,
    marker_fill: Color,
    marker_stroke: Color,
    stroke_width_px: f64,
    xform: Affine,
    shapes: &ShapeRegistry,
    pick: PickId,
) {
    if marker_name.is_empty() {
        return;
    }
    let Some(shape) = shapes.get(marker_name) else {
        return;
    };
    let dir = if invert { -outward } else { outward };
    if dir.length_squared() < 1e-12 {
        return;
    }
    let theta = dir.atan2();
    let rot = Affine::rotate(theta);
    let scaled_anchor = shape.anchor().to_vec2() * size_px;
    let (sn, cs) = theta.sin_cos();
    let anchor_world = Vec2::new(
        cs * scaled_anchor.x - sn * scaled_anchor.y,
        sn * scaled_anchor.x + cs * scaled_anchor.y,
    );
    let origin = placement.to_vec2() - anchor_world;
    let local_unscaled = Affine::translate(origin) * rot;
    emit_marker_shape(
        scene,
        shape,
        xform * local_unscaled,
        size_px,
        marker_fill,
        marker_stroke,
        stroke_width_px,
        pick,
    );
}

/// Stamp one shape at `xform`. Mirrors PointGeom's emission loop.
///
/// - Path-backed shapes: fill subpaths take `marker_fill`; stroke subpaths
///   take `marker_stroke`.
/// - Glyph-backed shapes: a single `GlyphRun` is emitted with
///   `brush = marker_fill`; `marker_stroke` is ignored (glyph markers
///   are fill-only). The em-space shift `em_origin - em_bbox.center()` is
///   composed inside the caller's `xform` so the glyph's visual centre
///   lands at the placement point. The caller's `xform` is expected to
///   already carry the desired translate/rotate/scale; the scale factor
///   becomes the glyph's effective font size in pixels.
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_marker_shape(
    scene: &mut dyn SceneBuilder,
    shape: &Shape,
    xform_unscaled: Affine,
    scale_factor: f64,
    marker_fill: Color,
    marker_stroke: Color,
    outline_px: f64,
    pick: PickId,
) {
    match shape.kind() {
        ShapeKind::Paths { paths, style } => {
            let xform = xform_unscaled * Affine::scale(scale_factor);
            let outline_spec = Stroke::new(outline_px);
            for sub in paths {
                match style {
                    ShapeStyle::Fill => {
                        scene.fill(
                            FillRule::NonZero,
                            xform,
                            &Brush::Solid(marker_fill),
                            None,
                            sub,
                            pick,
                        );
                    }
                    ShapeStyle::Stroke => {
                        scene.stroke(
                            &outline_spec,
                            xform,
                            &Brush::Solid(marker_stroke),
                            None,
                            sub,
                            pick,
                        );
                    }
                }
            }
        }
        ShapeKind::Glyph {
            font,
            glyph_id,
            em_bbox,
            em_origin,
        } => {
            // Glyph linetype markers / arrow terminators: scale up so
            // the visible ink approximately fills the surrounding
            // linewidth track. The bbox height is the typographic
            // ascender, but the visible ink of most glyphs only fills
            // ~85% of that height (typical emoji padding within their
            // em-square; Latin cap-to-baseline within ascender).
            // PointGeom does *not* apply this boost — its sizing is
            // anchored to the GLYPH_BBOX_REFERENCE convention, which
            // already targets the vector-shape visual extent.
            //
            // The effective scale (incl. INK_COVERAGE_BOOST) is baked
            // into `font_size` rather than the transform so vello
            // picks the matching bitmap strike for colour-emoji
            // fonts — `font_size: 1.0` with a transform scale would
            // pick the smallest strike and upscale (= fuzzy).
            const INK_COVERAGE_BOOST: f64 = 1.0 / 0.85;
            let effective_font_size = scale_factor * INK_COVERAGE_BOOST;
            // Centring is in em-space; convert to pixel space at the
            // effective font size, then apply the unscaled outer
            // transform (which carries the rotation + translation).
            let centring_em = em_origin.to_vec2() - em_bbox.center().to_vec2();
            let glyphs = [Glyph {
                id: glyph_id,
                x: 0.0,
                y: 0.0,
            }];
            let brush = Brush::Solid(marker_fill);
            let run = GlyphRun {
                font,
                font_size: effective_font_size as f32,
                transform: xform_unscaled * Affine::translate(centring_em * effective_font_size),
                glyph_transform: None,
                brush: &brush,
                brush_alpha: 1.0,
                hint: false,
                glyphs: &glyphs,
            };
            scene.draw_glyphs(&run, pick);
        }
    }
}

/// Build a sub-polyline from `sampler` spanning arc-length `[start,
/// end]`. Includes interior original vertices that fall strictly
/// between start and end, so straight runs stay one LineTo each.
fn build_sub_polyline(sampler: &PolylineSampler, start: f64, end: f64) -> Path {
    let mut path = Path::new();
    let start = start.max(0.0);
    let end = end.min(sampler.total_length());
    if end <= start + MARKER_EPSILON {
        return path;
    }
    let head = match sampler.sample_at(start) {
        Some(s) => s.point,
        None => return path,
    };
    path.move_to(head);
    for d in sampler.segment_boundaries_between(start, end) {
        if let Some(s) = sampler.sample_at(d) {
            path.line_to(s.point);
        }
    }
    if let Some(tail) = sampler.sample_at(end) {
        path.line_to(tail.point);
    }
    path
}

/// Return the smallest non-zero value among two non-negative inputs.
/// Treats 0 as "this axis isn't banded" — picks the other axis. If both
/// are 0 (both continuous), returns 0.
///
/// Shared by geoms whose `*_band` channel scales a single dimension
/// against whichever discrete axis offers a band — `WedgeGeom::radius_band`
/// and `PointGeom::size_band`. The semantics match: both-discrete picks
/// the smaller band so the geom fits the cell on both axes;
/// single-discrete uses that axis's band; both-continuous drops the
/// band contribution.
#[inline]
pub(crate) fn smallest_nonzero(a: f64, b: f64) -> f64 {
    match (a > 0.0, b > 0.0) {
        (true, true) => a.min(b),
        (true, false) => a,
        (false, true) => b,
        (false, false) => 0.0,
    }
}
