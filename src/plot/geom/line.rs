//! `LineGeom` — vectorised polylines drawn at scaled `(x, y)` positions.
//!
//! The model for **multi-row-per-mark geoms**. The `Keys` column carries
//! mark identity: rows sharing a key value belong to the same line.
//! Unique key values, in first-appearance order, define the marks.
//! Within a mark, rows are connected in source order; the geom does not
//! sort. Non-finite vertices (NaN x or y) are skipped — no segment break.
//!
//! Channels consumed:
//!
//! - `"x"` — vertex x (required; data; numeric).
//! - `"y"` — vertex y (required; data; numeric).
//! - `"x_offset"` / `"y_offset"` — absolute pt offset added per vertex.
//! - `"x_band"` / `"y_band"` — band-fraction offset folded into the
//!   scale's `map_with_offset` (per vertex).
//! - `"stroke"` — outline color (per-mark; first-row-of-mark). Also
//!   used as the stroke color for any markers in the linetype.
//! - `"stroke_opacity"` — overrides alpha of `"stroke"` (per-mark).
//! - `"fill"` — fill color for markers in the linetype (per-mark;
//!   defaults to the resolved stroke color when unset). The line
//!   itself is stroked, not filled — this channel only affects marker
//!   interiors.
//! - `"linewidth"` — stroke width in pt (per-mark; default 1.0 pt).
//!   Also dictates the marker size (markers are sized to one
//!   `linewidth` of arc length, rotated to the local tangent).
//! - `"linetype"` — [`LinetypeStep`] pattern (per-mark; default
//!   solid). Even-length, alternating Dash | Marker and Gap. A pure
//!   dashed pattern (no Marker entries) renders via the kurbo stroke
//!   fast path; patterns containing Marker entries walk the polyline
//!   in arc length and emit per-step strokes + shape stamps. Use the
//!   [`crate::plot::geom::linetype`] helpers (`dash` / `gap` /
//!   `marker` / `pattern` / canonical patterns).
//! - `"dash_offset"` — phase shift along the dash pattern in pt
//!   (per-mark). Has no effect on solid lines.
//! - `"cap"` — line cap style: `"butt"` / `"round"` / `"square"`
//!   (per-mark; default `"butt"`).
//! - `"join"` — line join style: `"miter"` / `"round"` / `"bevel"`
//!   (per-mark; default `"miter"`).
//! - `"corner_radius"` — fillet size in pt at each vertex of the
//!   polyline (per-mark; default `0.0` — sharp corners). Maps to
//!   [`CornerRounding::max_cut`](crate::primitives::CornerRounding); the
//!   actual fillet is clamped to half the shorter adjacent segment.
//! - `"clip_start_radius"` / `"clip_end_radius"` — circle clip radius
//!   in pt at the polyline's first / last vertex (per-mark; default
//!   `0.0` — no clip). When non-zero, the polyline is trimmed where it
//!   exits a circle of that radius centred on the first / last vertex.
//!   Use for arrowhead attachment or for trimming edges to node
//!   boundaries in graph layouts.
//! - `"angle"` — rotation in **radians** around the mark's centroid
//!   (mean of finite vertex positions in panel space), mathematical
//!   CCW. Per-mark; default `0.0`. Applies after vertex resolution
//!   (clip + corner rounding happen in the unrotated frame; the final
//!   constructed line is rotated as a rigid body around its centroid).
//!
//! Per-mark channels resolve once per line: the geom takes the value at
//! the *first row of the mark* (first row in source order whose key
//! equals the mark's key value) and uses it for the whole line. If the
//! channel's column varies within a mark, the divergence is silently
//! ignored — no averaging.
//!
//! Picking: per-mark via the `"pick_id"` channel. Resolved from the
//! mark's first row like every other per-mark channel — so a column
//! whose values are constant within a mark gives one pick id per
//! line; variation within a mark is silently ignored. Unset channel
//! → marks are non-pickable.

use crate::brush::Brush;
use crate::geometry::{Affine, Point};
use crate::plot::diff::{diff_columns, diff_positional, KeyIndex};
use crate::plot::value::{DataColumn, Value};
use crate::primitives::{
    clip_polyline, polyline, round_corners, CornerRounding, EndClip, PolylineOptions,
    PolylineSampler,
};
use crate::scene::SceneBuilder;
use crate::stroke::{Cap, Join, Stroke};

use super::linetype;
use super::marks::{build_marks_from_column, MarkSlot};
use super::resolve::{
    build_stroke_for_pattern, draw_linetype_with_markers, emit_endpoint_marker, endpoint_outward,
    override_alpha, pt_to_px, resolve_angle_channel, resolve_bool_channel_or, resolve_cap_channel,
    resolve_color_channel, resolve_join_channel, resolve_linetype_channel, resolve_number_channel,
    resolve_number_channel_or, resolve_pick_id, resolve_position, resolve_str_channel_or,
};
use super::state::{
    filter_declared, require_data_column, validate_channel_lengths, validate_pick_id_channel,
    GeomState, KeysStrategy,
};
use super::{
    empty_datacolumn_like, BuildableGeom, Channel, ExpectedOutput, Geom, GeomBuilder, GeomContext,
    Keys,
};

// ─── Defaults ────────────────────────────────────────────────────────────────

const DEFAULT_LINEWIDTH_PT: f64 = 1.0;
const DEFAULT_CAP: Cap = Cap::Butt;
const DEFAULT_JOIN: Join = Join::Miter;

/// Catalog of channels this geom recognises, with their expected scale
/// output type.
const CHANNELS: &[(&str, ExpectedOutput)] = &[
    ("x", ExpectedOutput::Numbers),
    ("y", ExpectedOutput::Numbers),
    ("x_offset", ExpectedOutput::Numbers),
    ("y_offset", ExpectedOutput::Numbers),
    ("x_band", ExpectedOutput::Numbers),
    ("y_band", ExpectedOutput::Numbers),
    ("fill", ExpectedOutput::Colors),
    ("stroke", ExpectedOutput::Colors),
    ("stroke_opacity", ExpectedOutput::Numbers),
    ("linewidth", ExpectedOutput::Numbers),
    ("linetype", ExpectedOutput::Linetypes),
    ("dash_offset", ExpectedOutput::Numbers),
    ("cap", ExpectedOutput::Strings),
    ("join", ExpectedOutput::Strings),
    ("corner_radius", ExpectedOutput::Numbers),
    ("corner_max_angle", ExpectedOutput::Numbers),
    ("clip_start_radius", ExpectedOutput::Numbers),
    ("clip_end_radius", ExpectedOutput::Numbers),
    ("angle", ExpectedOutput::Numbers),
    ("pick_id", ExpectedOutput::Numbers),
    ("start_marker", ExpectedOutput::Strings),
    ("end_marker", ExpectedOutput::Strings),
    ("start_marker_size", ExpectedOutput::Numbers),
    ("end_marker_size", ExpectedOutput::Numbers),
    ("start_marker_fill", ExpectedOutput::Colors),
    ("end_marker_fill", ExpectedOutput::Colors),
    ("start_marker_invert", ExpectedOutput::Any),
    ("end_marker_invert", ExpectedOutput::Any),
];

// ─── LineGeom ────────────────────────────────────────────────────────────────

/// A vectorised line geom. Non-generic; all channel data flows through
/// [`DataColumn`].
pub struct LineGeom {
    pub(crate) state: GeomState,
    /// Cached mark layout — rebuilt at the start of each `draw` /
    /// `rebuild_diff_against_previous`. One entry per unique key value
    /// in first-appearance order.
    pub(crate) marks: Vec<MarkSlot>,
}

crate::impl_geom_inherents_grouped!(LineGeom);

impl LineGeom {
    /// Build the mark layout from the current keys column.
    pub(crate) fn build_marks(&self) -> Vec<MarkSlot> {
        super::marks::build_marks(&self.state.keys)
    }
}

/// Build a column holding one entry per mark — the key value of each
/// mark's first row. Used to feed `diff_columns` at mark granularity.
fn unique_keys_column(col: &DataColumn, marks: &[MarkSlot]) -> DataColumn {
    let template = empty_datacolumn_like(col);
    push_values_into(template, marks.iter().map(|m| col.get(m.first_row)))
}

/// Append a sequence of Values into a fresh column of `template`'s
/// variant. Panics if a value's variant doesn't match the template.
fn push_values_into(
    mut template: DataColumn,
    values: impl IntoIterator<Item = Value>,
) -> DataColumn {
    for v in values {
        match (&mut template, v) {
            (DataColumn::F64(vec), Value::Number(n)) => vec.push(n),
            (DataColumn::F32(vec), Value::Number(n)) => vec.push(n as f32),
            (DataColumn::I32(vec), Value::Number(n)) => vec.push(n as i32),
            (DataColumn::I64(vec), Value::Number(n)) => vec.push(n as i64),
            (DataColumn::Bool(vec), Value::Bool(b)) => vec.push(b),
            (DataColumn::String(vec), Value::String(s)) => vec.push(s),
            (DataColumn::Color(vec), Value::Color(c)) => vec.push(c),
            (DataColumn::Date(vec), Value::Date(d)) => vec.push(d),
            (DataColumn::DateTime(vec), Value::DateTime(us)) => vec.push(us),
            (DataColumn::Time(vec), Value::Time(us)) => vec.push(us),
            (DataColumn::Duration(vec), Value::Duration(us)) => vec.push(us),
            (DataColumn::Linetype(vec), Value::Linetype(p)) => vec.push(p),
            _ => panic!("LineGeom: unique-keys column variant mismatch"),
        }
    }
    template
}

// ─── BuildableGeom impl ──────────────────────────────────────────────────────

impl BuildableGeom for LineGeom {
    fn build_from(builder: GeomBuilder<Self>) -> Self {
        let (keys_opt, channels) = builder.into_parts();

        let n = require_data_column("x", &channels, "LineGeom").len();
        let y_len = require_data_column("y", &channels, "LineGeom").len();
        if y_len != n {
            panic!("LineGeom::build: \"y\" length {y_len} does not match \"x\" length {n}");
        }
        validate_channel_lengths(&channels, n, "LineGeom");
        validate_pick_id_channel(&channels, "LineGeom");

        // Validate any user-supplied or constant linetype value to have
        // even length. Structural error caught at build time rather than
        // silently producing garbled dashes at draw.
        if let Some(ch) = channels.get("linetype") {
            validate_linetype_channel(ch);
        }

        let declared = filter_declared(&channels, CHANNELS);
        let state = GeomState::from_builder(keys_opt, channels, n, KeysStrategy::OneMark, declared);
        LineGeom {
            state,
            marks: Vec::new(),
        }
    }
}

fn validate_linetype_channel(ch: &Channel) {
    match ch {
        Channel::Constant(Value::Linetype(p)) | Channel::RawConstant(Value::Linetype(p)) => {
            linetype::validate_pattern(p);
        }
        Channel::Constant(_) | Channel::RawConstant(_) => {} // non-Linetype constant — resolved at draw time
        Channel::Data(DataColumn::Linetype(v)) | Channel::RawData(DataColumn::Linetype(v)) => {
            for p in v.iter() {
                linetype::validate_pattern(p);
            }
        }
        Channel::Data(_) | Channel::RawData(_) => {} // non-Linetype column — resolved at draw time
    }
}

// ─── Geom impl ───────────────────────────────────────────────────────────────

impl Geom for LineGeom {
    fn state(&self) -> &GeomState {
        &self.state
    }

    fn state_mut(&mut self) -> &mut GeomState {
        &mut self.state
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    /// Override: number of unique key values, not row count.
    fn mark_count(&self) -> usize {
        if self.marks.is_empty() && !self.is_empty() {
            return self.build_marks().len();
        }
        self.marks.len()
    }

    /// Override: drop the cached mark layout when state rotates. The
    /// next `draw` / `rebuild_diff_against_previous` rebuilds it from
    /// the current keys column.
    fn invalidate_caches(&mut self) {
        self.marks.clear();
    }

    /// Override: rebuild diff at mark granularity (against derived
    /// unique-key columns) and refresh the cached mark layout.
    fn rebuild_diff_against_previous(&mut self) {
        if !self.state.dirty {
            return;
        }
        let next_marks = self.build_marks();
        let prev_marks = match &self.state.prev_keys {
            Keys::Explicit(col) if !col.is_empty() => build_marks_from_column(col),
            _ => Vec::new(),
        };
        let (enter, update, exit) = match (&self.state.prev_keys, &self.state.keys) {
            (Keys::Explicit(prev_col), Keys::Explicit(next_col)) => {
                let prev_unique = unique_keys_column(prev_col, &prev_marks);
                let next_unique = unique_keys_column(next_col, &next_marks);
                let idx = KeyIndex::build(&prev_unique);
                diff_columns(&prev_unique, &idx, &next_unique)
            }
            _ => diff_positional(prev_marks.len(), next_marks.len()),
        };
        self.state.enter = enter;
        self.state.update = update;
        self.state.exit = exit;
        self.marks = next_marks;
        self.state.prev_keys = self.state.keys.clone();
        self.state.prev_channels = self.state.channels.clone();
        self.state.dirty = false;
    }

    fn draw(&self, scene: &mut dyn SceneBuilder, ctx: &GeomContext<'_>) {
        let panel = ctx.panel_rect;
        let panel_w = panel.x1 - panel.x0;
        let panel_h = panel.y1 - panel.y0;
        if panel_w <= 0.0 || panel_h <= 0.0 {
            return;
        }

        // If no marks have been cached yet (geom built but never drew /
        // had its diff rebuilt), build them on the fly. Cheap and keeps
        // `draw` callable without the orchestrator's dirty plumbing.
        let owned_marks;
        let marks: &[MarkSlot] = if self.marks.is_empty() && !self.is_empty() {
            owned_marks = self.build_marks();
            &owned_marks
        } else {
            &self.marks
        };
        if marks.is_empty() {
            return;
        }

        let x_scale_bound = ctx.scale_for("x");
        let y_scale_bound = ctx.scale_for("y");
        let fill_scale = ctx.scale_for("fill");
        let stroke_scale = ctx.scale_for("stroke");
        let stroke_opacity_scale = ctx.scale_for("stroke_opacity");
        let linewidth_scale = ctx.scale_for("linewidth");
        let linetype_scale = ctx.scale_for("linetype");
        let dash_offset_scale = ctx.scale_for("dash_offset");
        let cap_scale = ctx.scale_for("cap");
        let join_scale = ctx.scale_for("join");
        let pick_id_scale = ctx.scale_for("pick_id");
        let corner_radius_scale = ctx.scale_for("corner_radius");
        let corner_max_angle_scale = ctx.scale_for("corner_max_angle");
        let clip_start_radius_scale = ctx.scale_for("clip_start_radius");
        let clip_end_radius_scale = ctx.scale_for("clip_end_radius");
        let angle_scale = ctx.scale_for("angle");
        let x_offset_scale = ctx.scale_for("x_offset");
        let y_offset_scale = ctx.scale_for("y_offset");
        let x_band_scale = ctx.scale_for("x_band");
        let y_band_scale = ctx.scale_for("y_band");

        let channels = &self.state.channels;
        let (x_col, x_scale) = match channels.get("x") {
            Some(Channel::Data(c)) => (c, x_scale_bound),
            Some(Channel::RawData(c)) => (c, None),
            _ => return,
        };
        let (y_col, y_scale) = match channels.get("y") {
            Some(Channel::Data(c)) => (c, y_scale_bound),
            Some(Channel::RawData(c)) => (c, None),
            _ => return,
        };

        let fill_ch = channels.get("fill");
        let stroke_ch = channels.get("stroke");
        let stroke_opacity_ch = channels.get("stroke_opacity");
        let linewidth_ch = channels.get("linewidth");
        let linetype_ch = channels.get("linetype");
        let dash_offset_ch = channels.get("dash_offset");
        let cap_ch = channels.get("cap");
        let join_ch = channels.get("join");
        let pick_id_ch = channels.get("pick_id");
        let corner_radius_ch = channels.get("corner_radius");
        let corner_max_angle_ch = channels.get("corner_max_angle");
        let clip_start_radius_ch = channels.get("clip_start_radius");
        let clip_end_radius_ch = channels.get("clip_end_radius");
        let angle_ch = channels.get("angle");
        let x_offset_ch = channels.get("x_offset");
        let y_offset_ch = channels.get("y_offset");
        let x_band_ch = channels.get("x_band");
        let y_band_ch = channels.get("y_band");
        let start_marker_ch = channels.get("start_marker");
        let end_marker_ch = channels.get("end_marker");
        let start_marker_size_ch = channels.get("start_marker_size");
        let end_marker_size_ch = channels.get("end_marker_size");
        let start_marker_fill_ch = channels.get("start_marker_fill");
        let end_marker_fill_ch = channels.get("end_marker_fill");
        let start_marker_invert_ch = channels.get("start_marker_invert");
        let end_marker_invert_ch = channels.get("end_marker_invert");
        let start_marker_size_scale = ctx.scale_for("start_marker_size");
        let end_marker_size_scale = ctx.scale_for("end_marker_size");
        let start_marker_fill_scale = ctx.scale_for("start_marker_fill");
        let end_marker_fill_scale = ctx.scale_for("end_marker_fill");
        let start_marker_invert_scale = ctx.scale_for("start_marker_invert");
        let end_marker_invert_scale = ctx.scale_for("end_marker_invert");
        let start_marker_scale = ctx.scale_for("start_marker");
        let end_marker_scale = ctx.scale_for("end_marker");

        for mark in marks.iter() {
            // ── Per-mark channel resolution (first row of mark). ──
            let i0 = mark.first_row;
            let stroke_color = override_alpha(
                resolve_color_channel(stroke_ch, stroke_scale, i0),
                resolve_number_channel(stroke_opacity_ch, stroke_opacity_scale, i0),
            );
            let stroke_color = match stroke_color {
                Some(c) => c,
                None => continue, // no stroke → no line to draw
            };

            let linewidth_pt =
                resolve_number_channel_or(linewidth_ch, linewidth_scale, i0, DEFAULT_LINEWIDTH_PT);
            let linewidth_px = pt_to_px(linewidth_pt, ctx.dpi);
            if !linewidth_px.is_finite() || linewidth_px <= 0.0 {
                continue;
            }

            let dash_pattern_pt = resolve_linetype_channel(linetype_ch, linetype_scale, i0);
            let dash_offset_pt =
                resolve_number_channel_or(dash_offset_ch, dash_offset_scale, i0, 0.0);
            let cap = resolve_cap_channel(cap_ch, cap_scale, i0, DEFAULT_CAP);
            let join = resolve_join_channel(join_ch, join_scale, i0, DEFAULT_JOIN);

            // Marker fill defaults to the resolved stroke color.
            let marker_fill =
                resolve_color_channel(fill_ch, fill_scale, i0).unwrap_or(stroke_color);
            let has_markers = !linetype::is_marker_free(&dash_pattern_pt);

            // ── Per-vertex positions for this mark. ──
            let mut points: Vec<Point> = Vec::with_capacity(mark.rows.len());
            for &i in &mark.rows {
                let x_band = resolve_number_channel_or(x_band_ch, x_band_scale, i, 0.0);
                let y_band = resolve_number_channel_or(y_band_ch, y_band_scale, i, 0.0);
                let px_frac = resolve_position(x_col.get(i), x_scale, x_band);
                let py_frac = resolve_position(y_col.get(i), y_scale, y_band);
                if !px_frac.is_finite() || !py_frac.is_finite() {
                    continue;
                }
                let mut px = panel.x0 + px_frac * panel_w;
                let mut py = panel.y1 - py_frac * panel_h;
                if let Some(off) = resolve_number_channel(x_offset_ch, x_offset_scale, i) {
                    px += pt_to_px(off, ctx.dpi);
                }
                if let Some(off) = resolve_number_channel(y_offset_ch, y_offset_scale, i) {
                    py -= pt_to_px(off, ctx.dpi);
                }
                points.push(Point::new(px, py));
            }
            if points.len() < 2 {
                continue;
            }

            // ── Rotation: per-mark angle around the centroid of finite
            // vertex positions. Computed from `points` (after band +
            // offset resolution, before clip / round) so clip and corner
            // rounding still happen in the unrotated frame and the rigid
            // line is then rotated as a whole around the centroid.
            let angle = resolve_angle_channel(angle_ch, angle_scale, i0);
            let xform = if angle == 0.0 {
                Affine::IDENTITY
            } else {
                let n_pts = points.len() as f64;
                let cx = points.iter().map(|p| p.x).sum::<f64>() / n_pts;
                let cy = points.iter().map(|p| p.y).sum::<f64>() / n_pts;
                Affine::rotate_about(-angle, Point::new(cx, cy))
            };

            // ── End clip + corner rounding (per-mark, first row). ──
            let clip_start_pt =
                resolve_number_channel_or(clip_start_radius_ch, clip_start_radius_scale, i0, 0.0);
            let clip_end_pt =
                resolve_number_channel_or(clip_end_radius_ch, clip_end_radius_scale, i0, 0.0);
            let clipped: Vec<Point> = if clip_start_pt > 0.0 || clip_end_pt > 0.0 {
                let start = (clip_start_pt > 0.0).then(|| EndClip::Circle {
                    center: points[0],
                    radius: pt_to_px(clip_start_pt, ctx.dpi),
                });
                let end = (clip_end_pt > 0.0).then(|| EndClip::Circle {
                    center: *points.last().unwrap(),
                    radius: pt_to_px(clip_end_pt, ctx.dpi),
                });
                clip_polyline(&points, start, end)
            } else {
                points.clone()
            };
            if clipped.len() < 2 {
                continue;
            }

            let corner_radius_pt =
                resolve_number_channel_or(corner_radius_ch, corner_radius_scale, i0, 0.0);
            let path = if corner_radius_pt > 0.0 {
                let max_angle_deg = resolve_number_channel_or(
                    corner_max_angle_ch,
                    corner_max_angle_scale,
                    i0,
                    f64::INFINITY,
                );
                let opts = CornerRounding {
                    max_cut: pt_to_px(corner_radius_pt, ctx.dpi),
                    max_angle_deg,
                };
                round_corners(&clipped, false, opts)
            } else {
                polyline(&clipped, PolylineOptions::default())
            };
            let pick = resolve_pick_id(pick_id_ch, pick_id_scale, i0);

            // Resolve endpoint-marker channels up front so we can emit
            // the start marker *before* the stroke (so a self-
            // intersecting polyline's later segments draw over the start
            // marker — see Phase C.5's path-order convention).
            let start_name = resolve_str_channel_or(start_marker_ch, start_marker_scale, i0, "");
            let end_name = resolve_str_channel_or(end_marker_ch, end_marker_scale, i0, "");
            let default_marker_size_pt = 3.0 * linewidth_pt;
            let marker_outline_px = linewidth_px.max(pt_to_px(0.5, ctx.dpi));

            if !start_name.is_empty() {
                let size_pt = resolve_number_channel_or(
                    start_marker_size_ch,
                    start_marker_size_scale,
                    i0,
                    default_marker_size_pt,
                );
                let size_px = pt_to_px(size_pt, ctx.dpi);
                let fill = resolve_color_channel(start_marker_fill_ch, start_marker_fill_scale, i0)
                    .unwrap_or(marker_fill);
                let invert = resolve_bool_channel_or(
                    start_marker_invert_ch,
                    start_marker_invert_scale,
                    i0,
                    false,
                );
                let outward = endpoint_outward(&clipped, &points, true, clip_start_pt > 0.0);
                emit_endpoint_marker(
                    scene,
                    clipped[0],
                    outward,
                    invert,
                    &start_name,
                    size_px,
                    fill,
                    stroke_color,
                    marker_outline_px,
                    xform,
                    ctx.shapes,
                    pick,
                );
            }

            if !has_markers {
                // Fast path: pure-dash linetype (or solid). One stroke
                // op carries the kurbo dash pattern.
                let stroke_spec = build_stroke_for_pattern(
                    linewidth_px,
                    cap,
                    join,
                    &dash_pattern_pt,
                    dash_offset_pt,
                    linewidth_pt,
                    ctx.dpi,
                );
                scene.stroke(
                    &stroke_spec,
                    xform,
                    &Brush::Solid(stroke_color),
                    None,
                    &path,
                    pick,
                );
            } else {
                // Marker path: walk arc length through the linetype
                // pattern, emitting dash sub-strokes and marker stamps
                // independently.
                let dash_offset_px = pt_to_px(dash_offset_pt, ctx.dpi);
                let linewidth_px_for_marker = pt_to_px(linewidth_pt, ctx.dpi);
                let samplers = if corner_radius_pt > 0.0 {
                    PolylineSampler::from_path(&path, 0.5)
                } else {
                    // No rounded corners → walk the polyline directly.
                    vec![PolylineSampler::from_polyline(&clipped)]
                };
                let solid_stroke_spec = Stroke::new(linewidth_px).with_caps(cap).with_join(join);
                draw_linetype_with_markers(
                    scene,
                    &samplers,
                    &dash_pattern_pt,
                    dash_offset_px,
                    linewidth_px_for_marker,
                    marker_fill,
                    stroke_color,
                    &solid_stroke_spec,
                    xform,
                    ctx.shapes,
                    ctx.dpi,
                    pick,
                    /* distribute */ false,
                );
            }

            // End marker (Phase C.5). Emitted *after* the stroke so it
            // sits on top of the line termination — matching the
            // path-order convention (start cap → segments → end cap).
            if !end_name.is_empty() {
                let size_pt = resolve_number_channel_or(
                    end_marker_size_ch,
                    end_marker_size_scale,
                    i0,
                    default_marker_size_pt,
                );
                let size_px = pt_to_px(size_pt, ctx.dpi);
                let fill = resolve_color_channel(end_marker_fill_ch, end_marker_fill_scale, i0)
                    .unwrap_or(marker_fill);
                let invert = resolve_bool_channel_or(
                    end_marker_invert_ch,
                    end_marker_invert_scale,
                    i0,
                    false,
                );
                let outward = endpoint_outward(&clipped, &points, false, clip_end_pt > 0.0);
                let placement = *clipped.last().unwrap();
                emit_endpoint_marker(
                    scene,
                    placement,
                    outward,
                    invert,
                    &end_name,
                    size_px,
                    fill,
                    stroke_color,
                    marker_outline_px,
                    xform,
                    ctx.shapes,
                    pick,
                );
            }
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plot::value::LinetypeStep;
    use std::sync::Arc;

    use crate::color::Color;
    use crate::geometry::Rect;
    use crate::plot::geom::{linetype, DirectScaleResolver, Raw};
    use crate::plot::scale;
    use crate::scene::recording::{Op, RecordingScene};

    fn registry() -> crate::shape::ShapeRegistry {
        crate::shape::ShapeRegistry::with_builtins()
    }

    fn ctx<'a>(
        panel: Rect,
        shapes: &'a crate::shape::ShapeRegistry,
        scales: &'a DirectScaleResolver<'a>,
    ) -> GeomContext<'a> {
        GeomContext::new(panel, 96.0, shapes, scales)
    }

    // ── build() validation ──

    #[test]
    fn angle_pivots_around_mark_centroid() {
        // Three vertices forming a triangle. Their centroid is (1, 1).
        // After a math-CCW rotation of π by the geom, every vertex
        // should map to its 180° reflection across the centroid.
        use std::f64::consts::PI;
        let mut g = LineGeom::builder()
            .set("x", vec![0.0_f64, 0.02, 0.0])
            .set("y", vec![0.0_f64, 0.0, 0.02])
            .set("stroke", Color::new([1.0, 0.0, 0.0, 1.0]))
            .set("angle", PI)
            .build();
        g.rebuild_diff_against_previous();
        let panel = Rect::new(0.0, 0.0, 100.0, 100.0);
        let shapes = registry();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &ctx(panel, &shapes, &scales));
        let xform = scene
            .ops
            .iter()
            .find_map(|op| match op {
                Op::Stroke { transform, .. } => Some(*transform),
                _ => None,
            })
            .expect("stroke op");
        // Vertex positions in panel space: x in [0, 0.02] → [0, 2] px;
        // y_panel = 100 - y_frac * 100 → y in [98, 100]. Centroid:
        // ((0+2+0)/3, (100+100+98)/3) = (0.667, 99.333).
        let pivot = crate::geometry::Point::new(2.0 / 3.0, (100.0 + 100.0 + 98.0) / 3.0);
        let mapped = xform * pivot;
        // The pivot of an Affine::rotate_about(theta, pivot) maps to itself.
        assert!((mapped.x - pivot.x).abs() < 1e-6, "pivot.x = {}", mapped.x);
        assert!((mapped.y - pivot.y).abs() < 1e-6, "pivot.y = {}", mapped.y);
        // Vertex (0, 100) → after PI rotation about pivot → (2*px - 0,
        // 2*py - 100) = (1.333, 98.667).
        let v0 = xform * crate::geometry::Point::new(0.0, 100.0);
        assert!((v0.x - 2.0 * pivot.x).abs() < 1e-6, "v0.x = {}", v0.x);
        assert!(
            (v0.y - (2.0 * pivot.y - 100.0)).abs() < 1e-6,
            "v0.y = {}",
            v0.y
        );
    }

    #[test]
    fn angle_zero_produces_identity_xform() {
        let mut g = LineGeom::builder()
            .set("x", vec![0.0_f64, 0.5, 1.0])
            .set("y", vec![0.0_f64, 0.5, 0.0])
            .set("stroke", Color::new([1.0, 0.0, 0.0, 1.0]))
            .set("angle", 0.0_f64)
            .build();
        g.rebuild_diff_against_previous();
        let panel = Rect::new(0.0, 0.0, 100.0, 100.0);
        let shapes = registry();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &ctx(panel, &shapes, &scales));
        let xform = scene
            .ops
            .iter()
            .find_map(|op| match op {
                Op::Stroke { transform, .. } => Some(*transform),
                _ => None,
            })
            .expect("stroke op");
        assert_eq!(xform.as_coeffs(), Affine::IDENTITY.as_coeffs());
    }

    #[test]
    fn builder_no_keys_synthesises_single_mark() {
        let g = LineGeom::builder()
            .set("x", vec![0.0_f64, 1.0, 2.0])
            .set("y", vec![0.0_f64, 1.0, 4.0])
            .build();
        assert_eq!(g.len(), 3);
        assert_eq!(g.mark_count(), 1);
    }

    #[test]
    fn builder_explicit_keys_define_marks() {
        let g = LineGeom::builder()
            .keys(vec!["A", "A", "B", "B"])
            .set("x", vec![0.0_f64, 1.0, 0.0, 1.0])
            .set("y", vec![0.0_f64, 1.0, 2.0, 3.0])
            .build();
        assert_eq!(g.len(), 4);
        assert_eq!(g.mark_count(), 2);
    }

    #[test]
    fn builder_non_contiguous_keys_bucket_correctly() {
        let g = LineGeom::builder()
            .keys(vec!["A", "B", "A", "C", "B"])
            .set("x", vec![0.0_f64, 1.0, 2.0, 3.0, 4.0])
            .set("y", vec![0.0_f64, 1.0, 2.0, 3.0, 4.0])
            .build();
        let marks = g.build_marks();
        assert_eq!(marks.len(), 3);
        assert_eq!(marks[0].first_row, 0);
        assert_eq!(marks[0].rows, vec![0, 2]);
        assert_eq!(marks[1].first_row, 1);
        assert_eq!(marks[1].rows, vec![1, 4]);
        assert_eq!(marks[2].first_row, 3);
        assert_eq!(marks[2].rows, vec![3]);
    }

    #[test]
    #[should_panic(expected = "missing required channel")]
    fn builder_missing_x_panics() {
        LineGeom::builder()
            .set("y", vec![1.0_f64, 2.0, 3.0])
            .build();
    }

    #[test]
    #[should_panic(expected = "must be data, not constant")]
    fn builder_x_constant_panics() {
        LineGeom::builder()
            .set("x", 5.0)
            .set("y", vec![1.0_f64])
            .build();
    }

    #[test]
    #[should_panic(expected = "does not match")]
    fn builder_mismatched_lengths_panic() {
        LineGeom::builder()
            .set("x", vec![0.0_f64, 1.0, 2.0])
            .set("y", vec![0.0_f64, 1.0])
            .build();
    }

    #[test]
    #[should_panic(expected = "even length")]
    fn builder_odd_length_linetype_constant_panics() {
        LineGeom::builder()
            .set("x", vec![0.0_f64, 1.0])
            .set("y", vec![0.0_f64, 1.0])
            .set(
                "linetype",
                Value::Linetype(Arc::from(vec![
                    LinetypeStep::Dash(1.0),
                    LinetypeStep::Gap(2.0),
                    LinetypeStep::Dash(3.0),
                ])),
            )
            .build();
    }

    #[test]
    fn builder_empty_linetype_is_solid_no_panic() {
        let _g = LineGeom::builder()
            .set("x", vec![0.0_f64, 1.0])
            .set("y", vec![0.0_f64, 1.0])
            .set("linetype", Value::Linetype(linetype::solid()))
            .build();
    }

    // ── Draw output ──

    fn no_scales<'a>() -> DirectScaleResolver<'a> {
        DirectScaleResolver::new()
    }

    fn red_solid() -> Color {
        Color::new([1.0, 0.0, 0.0, 1.0])
    }

    #[test]
    fn draw_one_mark_emits_one_stroke_op() {
        let mut g = LineGeom::builder()
            .set("x", vec![0.0_f64, 0.5, 1.0])
            .set("y", vec![0.0_f64, 1.0, 0.0])
            .set("stroke", red_solid())
            .build();
        g.rebuild_diff_against_previous();
        let shapes = registry();
        let scales = no_scales();
        let c = ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales);
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &c);
        let strokes = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Stroke { .. }))
            .count();
        assert_eq!(strokes, 1);
    }

    #[test]
    fn draw_three_marks_emit_three_strokes_with_first_row_pick_ids() {
        // Per-mark pick id resolution: each mark gets the pick_id value
        // from its first row. Within-mark variation is silently ignored
        // (matches every other per-mark channel).
        let mut g = LineGeom::builder()
            .keys(vec!["A", "A", "B", "B", "C", "C"])
            .set("x", vec![0.0_f64, 1.0, 0.0, 1.0, 0.0, 1.0])
            .set("y", vec![0.0_f64, 1.0, 2.0, 3.0, 4.0, 5.0])
            .set("stroke", red_solid())
            .set("pick_id", vec![100_i64, 0, 200, 0, 300, 0])
            .build();
        g.rebuild_diff_against_previous();
        assert_eq!(g.mark_count(), 3);

        let shapes = registry();
        let scales = no_scales();
        let c = ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales);
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &c);
        let stroke_picks: Vec<u32> = scene
            .ops
            .iter()
            .filter_map(|op| match op {
                Op::Stroke {
                    pick_id: crate::pick::PickId::Id(n),
                    ..
                } => Some(*n),
                _ => None,
            })
            .collect();
        assert_eq!(stroke_picks, vec![100, 200, 300]);
    }

    #[test]
    fn constant_linewidth_converts_pt_to_px() {
        let mut g = LineGeom::builder()
            .set("x", vec![0.0_f64, 1.0])
            .set("y", vec![0.0_f64, 1.0])
            .set("stroke", red_solid())
            .set("linewidth", 2.0_f64)
            .build();
        g.rebuild_diff_against_previous();
        let shapes = registry();
        let scales = no_scales();
        let c = ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales);
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &c);
        let widths: Vec<f64> = scene
            .ops
            .iter()
            .filter_map(|op| match op {
                Op::Stroke { stroke, .. } => Some(stroke.width),
                _ => None,
            })
            .collect();
        assert_eq!(widths.len(), 1);
        assert!((widths[0] - 2.0 * 96.0 / 72.0).abs() < 1e-9);
    }

    #[test]
    fn within_mark_linewidth_divergence_uses_first_row() {
        let mut g = LineGeom::builder()
            .keys(vec!["A", "A"])
            .set("x", vec![0.0_f64, 1.0])
            .set("y", vec![0.0_f64, 1.0])
            .set("linewidth", vec![4.0_f64, 12.0])
            .set("stroke", red_solid())
            .build();
        g.rebuild_diff_against_previous();
        let shapes = registry();
        let scales = no_scales();
        let c = ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales);
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &c);
        let widths: Vec<f64> = scene
            .ops
            .iter()
            .filter_map(|op| match op {
                Op::Stroke { stroke, .. } => Some(stroke.width),
                _ => None,
            })
            .collect();
        assert_eq!(widths.len(), 1);
        assert!((widths[0] - 4.0 * 96.0 / 72.0).abs() < 1e-9);
    }

    #[test]
    fn linetype_constant_produces_dash_pattern() {
        let mut g = LineGeom::builder()
            .set("x", vec![0.0_f64, 1.0])
            .set("y", vec![0.0_f64, 1.0])
            .set("stroke", red_solid())
            .set("linetype", Value::Linetype(linetype::dashed()))
            .build();
        g.rebuild_diff_against_previous();
        let shapes = registry();
        let scales = no_scales();
        let c = ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales);
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &c);
        for op in &scene.ops {
            if let Op::Stroke { stroke, .. } = op {
                let dashes = &stroke.dash_pattern;
                assert!(!dashes.is_empty(), "expected dashes set");
                let expected_a = 8.0 * 96.0 / 72.0;
                let expected_b = 4.0 * 96.0 / 72.0;
                assert!((dashes[0] - expected_a).abs() < 1e-9);
                assert!((dashes[1] - expected_b).abs() < 1e-9);
                return;
            }
        }
        panic!("no stroke op emitted");
    }

    #[test]
    fn dash_offset_shifts_phase() {
        let mut g = LineGeom::builder()
            .set("x", vec![0.0_f64, 1.0])
            .set("y", vec![0.0_f64, 1.0])
            .set("stroke", red_solid())
            .set("linetype", Value::Linetype(linetype::dashed()))
            .set("dash_offset", 3.0_f64)
            .build();
        g.rebuild_diff_against_previous();
        let shapes = registry();
        let scales = no_scales();
        let c = ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales);
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &c);
        for op in &scene.ops {
            if let Op::Stroke { stroke, .. } = op {
                let expected_offset = 3.0 * 96.0 / 72.0;
                assert!((stroke.dash_offset - expected_offset).abs() < 1e-9);
                return;
            }
        }
        panic!("no stroke op emitted");
    }

    #[test]
    fn dash_offset_no_effect_when_solid() {
        let mut g = LineGeom::builder()
            .set("x", vec![0.0_f64, 1.0])
            .set("y", vec![0.0_f64, 1.0])
            .set("stroke", red_solid())
            .set("dash_offset", 5.0_f64)
            .build();
        g.rebuild_diff_against_previous();
        let shapes = registry();
        let scales = no_scales();
        let c = ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales);
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &c);
        for op in &scene.ops {
            if let Op::Stroke { stroke, .. } = op {
                assert!(stroke.dash_pattern.is_empty());
                return;
            }
        }
        panic!("no stroke op emitted");
    }

    #[test]
    fn linetype_scale_per_mark_resolution() {
        let s = scale::ordinal(["A", "B"]).range_linetypes([linetype::solid(), linetype::dashed()]);
        let resolver = DirectScaleResolver::new().with("linetype", &s);

        let mut g = LineGeom::builder()
            .keys(vec!["A", "A", "B", "B"])
            .set("x", vec![0.0_f64, 1.0, 0.0, 1.0])
            .set("y", vec![0.0_f64, 1.0, 2.0, 3.0])
            .set("linetype", vec!["A", "A", "B", "B"])
            .set("stroke", red_solid())
            .build();
        g.rebuild_diff_against_previous();
        let shapes = registry();
        let c = ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &resolver);
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &c);

        let dash_patterns: Vec<Vec<f64>> = scene
            .ops
            .iter()
            .filter_map(|op| match op {
                Op::Stroke { stroke, .. } => Some(stroke.dash_pattern.iter().copied().collect()),
                _ => None,
            })
            .collect();
        assert_eq!(dash_patterns.len(), 2);
        assert!(dash_patterns[0].is_empty());
        let expected_a = 8.0 * 96.0 / 72.0;
        let expected_b = 4.0 * 96.0 / 72.0;
        assert!((dash_patterns[1][0] - expected_a).abs() < 1e-9);
        assert!((dash_patterns[1][1] - expected_b).abs() < 1e-9);
    }

    // ── Marker linetype (C.2) ──

    /// Build a marker-only linetype: stamp the named shape, advance by
    /// `gap_pt`, repeat.
    fn marker_pattern(name: &str, gap_pt: f64) -> std::sync::Arc<[LinetypeStep]> {
        linetype::pattern([linetype::marker(name), linetype::gap(gap_pt)])
    }

    #[test]
    fn linetype_solid_emits_one_stroke_op_no_markers() {
        let mut g = LineGeom::builder()
            .set("x", Raw(vec![0.0_f64, 1.0]))
            .set("y", Raw(vec![0.5_f64, 0.5]))
            .set("stroke", red_solid())
            .build();
        g.rebuild_diff_against_previous();
        let shapes = registry();
        let scales = no_scales();
        let c = ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales);
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &c);
        let strokes = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Stroke { .. }))
            .count();
        let fills = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Fill { .. }))
            .count();
        assert_eq!(strokes, 1);
        assert_eq!(fills, 0);
    }

    #[test]
    fn linetype_pure_dashes_uses_fast_path_one_stroke_op() {
        let mut g = LineGeom::builder()
            .set("x", Raw(vec![0.0_f64, 1.0]))
            .set("y", Raw(vec![0.5_f64, 0.5]))
            .set("stroke", red_solid())
            .set("linetype", Value::Linetype(linetype::dashed()))
            .build();
        g.rebuild_diff_against_previous();
        let shapes = registry();
        let scales = no_scales();
        let c = ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales);
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &c);
        let strokes: Vec<_> = scene
            .ops
            .iter()
            .filter_map(|op| match op {
                Op::Stroke { stroke, .. } => Some(stroke.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(strokes.len(), 1, "fast path: one stroke op");
        assert!(
            !strokes[0].dash_pattern.is_empty(),
            "kurbo dash pattern set"
        );
    }

    #[test]
    fn linetype_marker_pattern_emits_marker_fills() {
        // 100-px polyline, linewidth 4 pt = 4*96/72 ≈ 5.333 px, gap 5 pt
        // = 5*96/72 ≈ 6.667 px → period ≈ 12.0 px → ~8 markers fit.
        let mut g = LineGeom::builder()
            .set("x", Raw(vec![0.0_f64, 100.0]))
            .set("y", Raw(vec![50.0, 50.0]))
            .set("stroke", red_solid())
            .set("linewidth", 4.0_f64)
            .set("linetype", Value::Linetype(marker_pattern("circle", 5.0)))
            .build();
        g.rebuild_diff_against_previous();
        let shapes = registry();
        let scales = no_scales();
        let c = ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales);
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &c);
        let fills = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Fill { .. }))
            .count();
        let strokes: Vec<_> = scene
            .ops
            .iter()
            .filter_map(|op| match op {
                Op::Stroke { stroke, .. } => Some(stroke.clone()),
                _ => None,
            })
            .collect();
        // The line itself emits no stroke ops on the marker-only
        // path (no Dash entries). Every stroke op present comes from
        // the marker's own outline (the "circle" shape registers as
        // Fill-style, so no marker outlines unless we add a default
        // thin stroke — which we don't in v1).
        assert!(fills >= 6, "expected several marker fills, got {fills}");
        // No kurbo dash pattern in any emitted stroke (we walked
        // the pattern manually).
        for s in &strokes {
            assert!(
                s.dash_pattern.is_empty(),
                "marker walker should not emit kurbo dashes"
            );
        }
    }

    #[test]
    fn linetype_marker_consumes_linewidth_of_arc() {
        // With linewidth = 4 pt and gap = 5 pt at 96 dpi:
        //   linewidth_px = 4 * 96/72 ≈ 5.333
        //   gap_px       = 5 * 96/72 ≈ 6.667
        //   period_px ≈ 12.0
        // Markers should be ~12 px apart (center-to-center) along the
        // line — measured between consecutive fill ops' translation
        // components.
        let mut g = LineGeom::builder()
            .set("x", Raw(vec![0.0_f64, 100.0]))
            .set("y", Raw(vec![50.0, 50.0]))
            .set("stroke", red_solid())
            .set("linewidth", 4.0_f64)
            .set("linetype", Value::Linetype(marker_pattern("circle", 5.0)))
            .build();
        g.rebuild_diff_against_previous();
        let shapes = registry();
        let scales = no_scales();
        let c = ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales);
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &c);
        let translations: Vec<f64> = scene
            .ops
            .iter()
            .filter_map(|op| match op {
                Op::Fill { transform, .. } => Some(transform.translation().x),
                _ => None,
            })
            .collect();
        assert!(translations.len() >= 3);
        // The period should match `(linewidth + gap) * 96/72`.
        let expected_period = (4.0 + 5.0) * 96.0 / 72.0;
        for w in translations.windows(2) {
            let delta = w[1] - w[0];
            assert!(
                (delta - expected_period).abs() < 1e-6,
                "marker spacing {delta} ≠ expected {expected_period}",
            );
        }
    }

    #[test]
    fn linetype_marker_rotates_to_tangent() {
        // Polyline along +y at panel-x = 50 px → tangent direction
        // (0, +y). The Affine should carry a 90° rotation in screen
        // frame (sin = 1, cos = 0).
        let mut g = LineGeom::builder()
            .set("x", Raw(vec![50.0, 50.0]))
            .set("y", Raw(vec![100.0, 0.0])) // (y_pixel 100 → 0 means line goes down→up in panel; we just want a +y direction)
            .set("stroke", red_solid())
            .set("linewidth", 4.0_f64)
            .set("linetype", Value::Linetype(marker_pattern("circle", 5.0)))
            .build();
        g.rebuild_diff_against_previous();
        let shapes = registry();
        let scales = no_scales();
        let c = ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales);
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &c);
        let coeffs = scene
            .ops
            .iter()
            .find_map(|op| match op {
                Op::Fill { transform, .. } => Some(transform.as_coeffs()),
                _ => None,
            })
            .expect("at least one marker fill");
        // Expected linear part: translate * rotate(theta) *
        // scale(linewidth_px / shape.bounding_box().height()). The
        // per-shape height division ensures the marker's local y-extent
        // matches linewidth exactly. For the builtin circle the local
        // bbox is 1.6 × 1.6 (radius 0.8). For raw input the line in
        // panel space runs (50, 0) -> (50, 100) — tangent (0, +y) screen
        // -down. rotate(atan2(1, 0)) = rotate(π/2). Linear part = R(π/2)
        // * scale(s). coeffs are [a, b, c, d, e, f] where matrix is
        // [[a, c], [b, d]] (kurbo convention). R(π/2) * scale(s) =
        // [[0, -s], [s, 0]] → a=0, b=s, c=-s, d=0.
        let linewidth_px = 4.0 * 96.0 / 72.0;
        let circle_bbox_h = 1.6;
        let s = linewidth_px / circle_bbox_h;
        assert!(coeffs[0].abs() < 1e-9, "a ≈ 0, got {}", coeffs[0]);
        assert!((coeffs[1] - s).abs() < 1e-9, "b ≈ s, got {}", coeffs[1]);
        assert!((coeffs[2] + s).abs() < 1e-9, "c ≈ -s, got {}", coeffs[2]);
        assert!(coeffs[3].abs() < 1e-9, "d ≈ 0, got {}", coeffs[3]);
    }

    #[test]
    fn linetype_marker_fill_defaults_to_stroke_color() {
        let stroke = red_solid();
        let mut g = LineGeom::builder()
            .set("x", Raw(vec![0.0_f64, 100.0]))
            .set("y", Raw(vec![50.0, 50.0]))
            .set("stroke", stroke)
            .set("linewidth", 4.0_f64)
            .set("linetype", Value::Linetype(marker_pattern("circle", 5.0)))
            .build();
        g.rebuild_diff_against_previous();
        let shapes = registry();
        let scales = no_scales();
        let c = ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales);
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &c);
        let fill_colors: Vec<_> = scene
            .ops
            .iter()
            .filter_map(|op| match op {
                Op::Fill {
                    brush: crate::brush::Brush::Solid(c),
                    ..
                } => Some(*c),
                _ => None,
            })
            .collect();
        assert!(!fill_colors.is_empty());
        for c in fill_colors {
            assert_eq!(c, stroke, "marker fill defaults to stroke color");
        }
    }

    #[test]
    fn linetype_fill_channel_overrides_marker_color() {
        let stroke = red_solid();
        let blue = Color::new([0.0, 0.0, 1.0, 1.0]);
        let mut g = LineGeom::builder()
            .set("x", Raw(vec![0.0_f64, 100.0]))
            .set("y", Raw(vec![50.0, 50.0]))
            .set("stroke", stroke)
            .set("fill", blue)
            .set("linewidth", 4.0_f64)
            .set("linetype", Value::Linetype(marker_pattern("circle", 5.0)))
            .build();
        g.rebuild_diff_against_previous();
        let shapes = registry();
        let scales = no_scales();
        let c = ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales);
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &c);
        let fill_color = scene
            .ops
            .iter()
            .find_map(|op| match op {
                Op::Fill {
                    brush: crate::brush::Brush::Solid(c),
                    ..
                } => Some(*c),
                _ => None,
            })
            .expect("at least one marker fill");
        assert_eq!(fill_color, blue);
    }

    #[test]
    fn linetype_mixed_dash_and_marker() {
        // Pattern: 5pt dash, 2pt gap, circle, 4pt gap; linewidth 2pt.
        // Both stroke and fill ops should appear.
        let pat = linetype::pattern([
            linetype::dash(5.0),
            linetype::gap(2.0),
            linetype::marker("circle"),
            linetype::gap(4.0),
        ]);
        let mut g = LineGeom::builder()
            .set("x", Raw(vec![0.0_f64, 200.0]))
            .set("y", Raw(vec![50.0, 50.0]))
            .set("stroke", red_solid())
            .set("linewidth", 2.0_f64)
            .set("linetype", Value::Linetype(pat))
            .build();
        g.rebuild_diff_against_previous();
        let shapes = registry();
        let scales = no_scales();
        let c = ctx(Rect::new(0.0, 0.0, 200.0, 100.0), &shapes, &scales);
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &c);
        let fills = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Fill { .. }))
            .count();
        let strokes = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Stroke { .. }))
            .count();
        assert!(fills > 0, "markers emit fills");
        assert!(strokes > 0, "dashes emit strokes");
    }

    #[test]
    fn linetype_marker_on_rounded_corner_follows_path() {
        // L-shape with corner rounding → markers walk the rounded
        // curve. We can't assert exact positions but we can check that
        // marker count for a rounded path is greater than 0 (i.e., the
        // walker successfully sampled the curved geometry).
        let mut g = LineGeom::builder()
            .set("x", Raw(vec![0.0_f64, 50.0, 50.0]))
            .set("y", Raw(vec![50.0, 50.0, 100.0]))
            .set("stroke", red_solid())
            .set("linewidth", 3.0_f64)
            .set("corner_radius", 10.0_f64)
            .set("linetype", Value::Linetype(marker_pattern("circle", 4.0)))
            .build();
        g.rebuild_diff_against_previous();
        let shapes = registry();
        let scales = no_scales();
        let c = ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales);
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &c);
        let fills = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Fill { .. }))
            .count();
        assert!(fills > 0, "marker walker handled the rounded path");
    }

    #[test]
    fn cap_join_constants() {
        let mut g = LineGeom::builder()
            .set("x", vec![0.0_f64, 1.0])
            .set("y", vec![0.0_f64, 1.0])
            .set("stroke", red_solid())
            .set("cap", Value::String(Arc::from("round")))
            .set("join", Value::String(Arc::from("bevel")))
            .build();
        g.rebuild_diff_against_previous();
        let shapes = registry();
        let scales = no_scales();
        let c = ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales);
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &c);
        for op in &scene.ops {
            if let Op::Stroke { stroke, .. } = op {
                assert!(matches!(stroke.start_cap, Cap::Round));
                assert!(matches!(stroke.end_cap, Cap::Round));
                assert!(matches!(stroke.join, Join::Bevel));
                return;
            }
        }
        panic!("no stroke op emitted");
    }

    #[test]
    fn no_stroke_channel_means_no_draw() {
        let mut g = LineGeom::builder()
            .set("x", vec![0.0_f64, 1.0])
            .set("y", vec![0.0_f64, 1.0])
            .build();
        g.rebuild_diff_against_previous();
        let shapes = registry();
        let scales = no_scales();
        let c = ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales);
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &c);
        let strokes = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Stroke { .. }))
            .count();
        assert_eq!(strokes, 0);
    }

    #[test]
    fn nonfinite_vertex_skipped() {
        let mut g = LineGeom::builder()
            .set("x", vec![0.0_f64, 0.5, 1.0])
            .set("y", vec![0.0_f64, f64::NAN, 1.0])
            .set("stroke", red_solid())
            .build();
        g.rebuild_diff_against_previous();
        let shapes = registry();
        let scales = no_scales();
        let c = ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales);
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &c);
        let strokes = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Stroke { .. }))
            .count();
        assert_eq!(strokes, 1);
    }

    // ── Diff at mark level ──

    #[test]
    fn diff_marks_enter_on_first_draw() {
        let mut g = LineGeom::builder()
            .keys(vec!["A", "A", "B", "B"])
            .set("x", vec![0.0_f64, 1.0, 0.0, 1.0])
            .set("y", vec![0.0_f64, 1.0, 2.0, 3.0])
            .build();
        g.rebuild_diff_against_previous();
        assert_eq!(g.state.enter.len(), 2);
        assert_eq!(g.state.exit.len(), 0);
    }

    #[test]
    fn diff_marks_exit_when_mark_removed() {
        let mut g = LineGeom::builder()
            .keys(vec!["A", "A", "B", "B"])
            .set("x", vec![0.0_f64, 1.0, 0.0, 1.0])
            .set("y", vec![0.0_f64, 1.0, 2.0, 3.0])
            .build();
        g.rebuild_diff_against_previous();
        g.update(|b| {
            b.keys(vec!["A", "A"]);
            b.set("x", vec![0.0_f64, 1.0]);
            b.set("y", vec![0.0_f64, 1.0]);
        });
        g.rebuild_diff_against_previous();
        assert_eq!(
            g.state.exit.len(),
            1,
            "expected 1 exit (B), got {:?}",
            g.state.exit
        );
        assert!(g.state.exit[0].key_eq(&Value::String(Arc::from("B"))));
    }

    // ── Primitive enhancement channels ──

    fn stroke_path(scene: &RecordingScene) -> Option<crate::path::Path> {
        scene.ops.iter().find_map(|op| match op {
            Op::Stroke { path, .. } => Some(path.clone()),
            _ => None,
        })
    }

    fn count_curves(path: &crate::path::Path) -> usize {
        path.elements()
            .iter()
            .filter(|el| matches!(el, kurbo::PathEl::CurveTo(_, _, _)))
            .count()
    }

    #[test]
    fn corner_radius_produces_curves_at_each_vertex() {
        // A 4-vertex zigzag has 2 interior joins → 2 fillets.
        let mut g = LineGeom::builder()
            .set("x", vec![0.0_f64, 0.3, 0.6, 0.9])
            .set("y", vec![0.0_f64, 0.5, 0.0, 0.5])
            .set("stroke", red_solid())
            .set("corner_radius", 5.0_f64)
            .build();
        g.rebuild_diff_against_previous();
        let shapes = registry();
        let scales = no_scales();
        let c = ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales);
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &c);
        let path = stroke_path(&scene).expect("stroke");
        assert_eq!(count_curves(&path), 2);
    }

    #[test]
    fn corner_radius_zero_keeps_path_polyline() {
        let mut g = LineGeom::builder()
            .set("x", vec![0.0_f64, 0.3, 0.6])
            .set("y", vec![0.0_f64, 0.5, 0.0])
            .set("stroke", red_solid())
            .build();
        g.rebuild_diff_against_previous();
        let shapes = registry();
        let scales = no_scales();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales),
        );
        let path = stroke_path(&scene).expect("stroke");
        assert_eq!(count_curves(&path), 0);
    }

    #[test]
    fn corner_max_angle_below_interior_skips_rounding() {
        // L-shape with one 90° corner. max_angle_deg = 80 → corner is
        // above threshold → not rounded. max_angle_deg = 95 → rounded.
        let pts_x: Vec<f64> = vec![0.0, 0.5, 0.5];
        let pts_y: Vec<f64> = vec![0.5, 0.5, 1.0];
        let mut g = LineGeom::builder()
            .set("x", Raw(pts_x.clone()))
            .set("y", Raw(pts_y.clone()))
            .set("stroke", red_solid())
            .set("corner_radius", 5.0_f64)
            .set("corner_max_angle", 80.0_f64)
            .build();
        g.rebuild_diff_against_previous();
        let shapes = registry();
        let scales = no_scales();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales),
        );
        let path = stroke_path(&scene).expect("stroke");
        assert_eq!(
            count_curves(&path),
            0,
            "80° threshold should reject 90° corners"
        );

        // Same shape with looser threshold rounds.
        let mut g2 = LineGeom::builder()
            .set("x", Raw(pts_x))
            .set("y", Raw(pts_y))
            .set("stroke", red_solid())
            .set("corner_radius", 5.0_f64)
            .set("corner_max_angle", 95.0_f64)
            .build();
        g2.rebuild_diff_against_previous();
        let mut scene2 = RecordingScene::default();
        g2.draw(
            &mut scene2,
            &ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales),
        );
        let path2 = stroke_path(&scene2).expect("stroke");
        assert_eq!(
            count_curves(&path2),
            1,
            "95° threshold should accept 90° corners"
        );
    }

    #[test]
    fn clip_start_radius_trims_first_segment() {
        // Polyline from (0,0) to (100,0). clip_start_radius = 20 (pt) =
        // 20*96/72 ≈ 26.67 px. The trimmed polyline should start at
        // (26.67, 0).
        let mut g = LineGeom::builder()
            .set("x", Raw(vec![0.0_f64, 1.0]))
            .set("y", Raw(vec![0.5_f64, 0.5]))
            .set("stroke", red_solid())
            .set("clip_start_radius", 20.0_f64)
            .build();
        g.rebuild_diff_against_previous();
        let shapes = registry();
        let scales = no_scales();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales),
        );
        let path = stroke_path(&scene).expect("stroke");
        // First element is MoveTo at the trim point.
        match path.elements().first() {
            Some(kurbo::PathEl::MoveTo(p)) => {
                let expected = 20.0 * 96.0 / 72.0;
                assert!((p.x - expected).abs() < 1e-6, "start.x = {}", p.x);
            }
            other => panic!("expected MoveTo, got {other:?}"),
        }
    }

    // ── Endpoint markers (Phase C.5) ──

    fn horizontal_line_with(
        extra: impl FnOnce(&mut crate::plot::geom::GeomBuilder<LineGeom>),
    ) -> LineGeom {
        let mut b = LineGeom::builder();
        b.set("x", Raw(vec![0.0_f64, 1.0]))
            .set("y", Raw(vec![0.5_f64, 0.5]))
            .set("stroke", red_solid())
            .set("linewidth", 2.0_f64);
        extra(&mut b);
        let mut g = b.build();
        g.rebuild_diff_against_previous();
        g
    }

    fn draw_into(g: &LineGeom) -> RecordingScene {
        let shapes = registry();
        let scales = no_scales();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales),
        );
        scene
    }

    fn fills_before_stroke(scene: &RecordingScene) -> Vec<Affine> {
        let mut out = Vec::new();
        for op in &scene.ops {
            match op {
                Op::Fill { transform, .. } => out.push(*transform),
                Op::Stroke { .. } => break,
                _ => {}
            }
        }
        out
    }

    fn fills_after_stroke(scene: &RecordingScene) -> Vec<Affine> {
        let mut seen_stroke = false;
        let mut out = Vec::new();
        for op in &scene.ops {
            match op {
                Op::Stroke { .. } => seen_stroke = true,
                Op::Fill { transform, .. } if seen_stroke => out.push(*transform),
                _ => {}
            }
        }
        out
    }

    #[test]
    fn line_endpoint_marker_unset_emits_no_extra_ops() {
        let g = horizontal_line_with(|_| {});
        let scene = draw_into(&g);
        let fills = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Fill { .. }))
            .count();
        assert_eq!(fills, 0);
    }

    #[test]
    fn line_end_marker_anchor_lands_at_last_vertex() {
        let g = horizontal_line_with(|b| {
            b.set("end_marker", "arrow-closed")
                .set("end_marker_size", 10.0_f64);
        });
        let scene = draw_into(&g);
        let xforms = fills_after_stroke(&scene);
        assert!(!xforms.is_empty());
        let anchor = xforms[0] * Point::new(-1.0, 0.0);
        assert!((anchor.x - 100.0).abs() < 1e-6);
        assert!((anchor.y - 50.0).abs() < 1e-6);
    }

    #[test]
    fn line_start_marker_emitted_before_stroke() {
        // For self-intersecting polylines the start marker must sit
        // under later segments — verify it appears before the stroke
        // op in the recording.
        let g = horizontal_line_with(|b| {
            b.set("start_marker", "arrow-closed")
                .set("start_marker_size", 10.0_f64);
        });
        let scene = draw_into(&g);
        let stroke_idx = scene
            .ops
            .iter()
            .position(|op| matches!(op, Op::Stroke { .. }))
            .expect("stroke op");
        let fill_idx = scene
            .ops
            .iter()
            .position(|op| matches!(op, Op::Fill { .. }))
            .expect("start-marker fill");
        assert!(
            fill_idx < stroke_idx,
            "start marker fill (idx {fill_idx}) must precede stroke (idx {stroke_idx})"
        );
        // Anchor lands at (0, 50).
        let xf = fills_before_stroke(&scene)[0];
        let anchor = xf * Point::new(-1.0, 0.0);
        assert!((anchor.x - 0.0).abs() < 1e-6);
        assert!((anchor.y - 50.0).abs() < 1e-6);
    }

    #[test]
    fn line_marker_size_default_is_three_times_linewidth() {
        // linewidth = 2pt → marker default = 6pt = 8px at 96 dpi.
        let g = horizontal_line_with(|b| {
            b.set("end_marker", "arrow-closed");
        });
        let scene = draw_into(&g);
        let xf = fills_after_stroke(&scene)[0];
        let coeffs = xf.as_coeffs();
        let det = coeffs[0] * coeffs[3] - coeffs[1] * coeffs[2];
        let expected = 6.0 * 96.0 / 72.0;
        assert!((det.abs() - expected * expected).abs() < 1e-6);
    }

    #[test]
    fn line_marker_direction_is_chord_toward_original_endpoint() {
        // L-shaped polyline in Raw fractions:
        //   Raw (0, 1) → panel (0, 0)   first
        //   Raw (1, 1) → panel (100, 0) middle
        //   Raw (1, 0) → panel (100, 100) last
        // (Raw y is flipped: panel.y = panel.y1 - y_frac * panel_h, so
        // Raw y=1 → panel y=0; Raw y=0 → panel y=100.)
        //
        // Choose clip_end_radius such that the trim eats the entire
        // last segment (length 100 px) and bites into the first edge.
        // clip_end_radius_pt = 100 → 133.33 px. Walking back from
        // (100, 100), the segment to (100, 0) (length 100) is fully
        // consumed; we continue into the first edge from (100, 0)
        // toward (0, 0). The exit point on a circle of radius
        // 133.33 centred on (100, 100) lies at (100 - t, 0) where
        // sqrt(t² + 100²) = 133.33 → t ≈ 88.19. So clipped last ≈
        // (11.81, 0) and the chord toward original last (100, 100)
        // has direction ≈ (88.19, 100), length 133.33, unit
        // (0.6614, 0.7500).
        let g = horizontal_line_with(|b| {
            b.set("x", Raw(vec![0.0_f64, 1.0, 1.0]))
                .set("y", Raw(vec![1.0_f64, 1.0, 0.0]))
                .set("end_marker", "arrow-closed")
                .set("end_marker_size", 10.0_f64)
                .set("clip_end_radius", 100.0_f64);
        });
        let scene = draw_into(&g);
        let xforms = fills_after_stroke(&scene);
        assert!(!xforms.is_empty(), "end marker emitted");
        let xf = xforms[0];
        let anchor = xf * Point::new(-1.0, 0.0);
        let tip = xf * Point::new(0.0, 0.0);
        let outward_x = tip.x - anchor.x;
        let outward_y = tip.y - anchor.y;
        let size_px = 10.0 * 96.0 / 72.0;

        // Expected unit chord toward the original last vertex in
        // panel coords (100, 100). Re-derive from the *actual*
        // recorded anchor so the test isn't fragile to primitive
        // precision in the trim calculation.
        let original_last = Point::new(100.0, 100.0);
        let clipped_last = anchor;
        let dx = original_last.x - clipped_last.x;
        let dy = original_last.y - clipped_last.y;
        let len = (dx * dx + dy * dy).sqrt();
        let exp_ox = dx / len * size_px;
        let exp_oy = dy / len * size_px;
        assert!(
            (outward_x - exp_ox).abs() < 1e-3,
            "outward.x mismatch: got {outward_x}, expected {exp_ox}"
        );
        assert!(
            (outward_y - exp_oy).abs() < 1e-3,
            "outward.y mismatch: got {outward_y}, expected {exp_oy}"
        );
        // Sanity: chord direction has a non-trivial y component. If
        // we'd used the local tangent along the first edge instead,
        // outward would be (±1, 0) and outward_y would be ~0.
        assert!(
            outward_y.abs() > 0.1 * size_px,
            "chord must carry a y component (was: {outward_y})"
        );
    }

    #[test]
    fn line_marker_respects_clip_start_radius() {
        // Horizontal line clipped at start. The chord from clipped
        // start to original first vertex points back along -x, so
        // the start marker rotates the same way as the no-clip case
        // — but the anchor lands at the clipped start.
        let g = horizontal_line_with(|b| {
            b.set("start_marker", "arrow-closed")
                .set("start_marker_size", 10.0_f64)
                .set("clip_start_radius", 15.0_f64);
        });
        let scene = draw_into(&g);
        let xf = fills_before_stroke(&scene)[0];
        let anchor = xf * Point::new(-1.0, 0.0);
        let expected_x = 15.0 * 96.0 / 72.0;
        assert!((anchor.x - expected_x).abs() < 1e-6);
    }

    #[test]
    fn line_marker_invert_flips_rotation() {
        let g = horizontal_line_with(|b| {
            b.set("end_marker", "arrow-closed")
                .set("end_marker_size", 10.0_f64)
                .set("end_marker_invert", true);
        });
        let scene = draw_into(&g);
        let xf = fills_after_stroke(&scene)[0];
        let tip = xf * Point::new(0.0, 0.0);
        let size_px = 10.0 * 96.0 / 72.0;
        assert!(
            (tip.x - (100.0 - size_px)).abs() < 1e-6,
            "tip.x = {} (expected {})",
            tip.x,
            100.0 - size_px
        );
    }

    #[test]
    fn line_marker_unknown_name_is_silent_no_op() {
        let g = horizontal_line_with(|b| {
            b.set("end_marker", "no-such-shape");
        });
        let scene = draw_into(&g);
        let fills = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Fill { .. }))
            .count();
        assert_eq!(fills, 0);
    }

    #[test]
    fn line_marker_fill_chain() {
        let blue = Color::new([0.0, 0.0, 1.0, 1.0]);
        let green = Color::new([0.0, 1.0, 0.0, 1.0]);

        // No fill, no end_marker_fill → marker uses stroke (red).
        let g = horizontal_line_with(|b| {
            b.set("end_marker", "arrow-closed");
        });
        let scene = draw_into(&g);
        let c = scene
            .ops
            .iter()
            .find_map(|op| match op {
                Op::Fill {
                    brush: crate::brush::Brush::Solid(c),
                    ..
                } => Some(*c),
                _ => None,
            })
            .unwrap();
        assert_eq!(c, red_solid());

        // fill=blue → marker uses blue.
        let g = horizontal_line_with(|b| {
            b.set("end_marker", "arrow-closed").set("fill", blue);
        });
        let scene = draw_into(&g);
        let c = scene
            .ops
            .iter()
            .find_map(|op| match op {
                Op::Fill {
                    brush: crate::brush::Brush::Solid(c),
                    ..
                } => Some(*c),
                _ => None,
            })
            .unwrap();
        assert_eq!(c, blue);

        // end_marker_fill=green overrides fill for the end marker.
        let g = horizontal_line_with(|b| {
            b.set("end_marker", "arrow-closed")
                .set("fill", blue)
                .set("end_marker_fill", green);
        });
        let scene = draw_into(&g);
        let c = scene
            .ops
            .iter()
            .find_map(|op| match op {
                Op::Fill {
                    brush: crate::brush::Brush::Solid(c),
                    ..
                } => Some(*c),
                _ => None,
            })
            .unwrap();
        assert_eq!(c, green);
    }

    #[test]
    fn clip_radii_too_large_skip_the_line() {
        // Polyline spans only 100 px; clip radii together exceed it →
        // no segments emitted.
        let mut g = LineGeom::builder()
            .set("x", Raw(vec![0.0_f64, 1.0]))
            .set("y", Raw(vec![0.5_f64, 0.5]))
            .set("stroke", red_solid())
            .set("clip_start_radius", 100.0_f64)
            .set("clip_end_radius", 100.0_f64)
            .build();
        g.rebuild_diff_against_previous();
        let shapes = registry();
        let scales = no_scales();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales),
        );
        let strokes = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Stroke { .. }))
            .count();
        assert_eq!(strokes, 0);
    }
}
