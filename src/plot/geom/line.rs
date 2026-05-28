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
//! - `"stroke"` — outline color (per-mark; first-row-of-mark).
//! - `"stroke_opacity"` — overrides alpha of `"stroke"` (per-mark).
//! - `"linewidth"` — stroke width in pt (per-mark; default 1.0 pt).
//! - `"linetype"` — `Value::Linetype` dash pattern (per-mark; default
//!   solid). Pt array of alternating dash/gap. Use the
//!   [`crate::plot::geom::linetype`] helpers for the named patterns.
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
};
use crate::scene::SceneBuilder;
use crate::stroke::{Cap, Join, Stroke};

use super::resolve::{
    override_alpha, pt_to_px, resolve_cap_channel, resolve_color_channel, resolve_join_channel,
    resolve_linetype_channel, resolve_number_channel, resolve_number_channel_or, resolve_pick_id,
    resolve_position,
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
    ("pick_id", ExpectedOutput::Numbers),
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

/// One mark in the geom — a logical line composed of N rows.
#[derive(Clone, Debug)]
pub(crate) struct MarkSlot {
    /// Source-order row index of the first appearance of this mark's key.
    /// Used to resolve per-mark channels.
    pub(crate) first_row: usize,
    /// Row indices that make up this mark's polyline, in source order.
    pub(crate) rows: Vec<usize>,
}

crate::impl_geom_inherents_grouped!(LineGeom);

impl LineGeom {
    /// Build the mark layout from the current keys column.
    pub(crate) fn build_marks(&self) -> Vec<MarkSlot> {
        match &self.state.keys {
            // No-keys default (after `build_from` rewriting) should never
            // reach this branch — the rewriter replaces Positional with an
            // Explicit single-value column. But if a user-driven Positional
            // somehow slips through, fall back to "every row is its own
            // mark" — matches PointGeom semantics for the diff path.
            Keys::Positional(n) => (0..*n)
                .map(|i| MarkSlot {
                    first_row: i,
                    rows: vec![i],
                })
                .collect(),
            Keys::Explicit(col) => build_marks_from_column(col),
        }
    }
}

/// Walk `col` and produce one [`MarkSlot`] per unique key value, in
/// first-appearance order. Each slot's `rows` are in source order.
fn build_marks_from_column(col: &DataColumn) -> Vec<MarkSlot> {
    let n = col.len();
    let mut order: Vec<MarkSlot> = Vec::new();
    // For small mark counts (typical: K << N) a linear scan over `order`
    // is cheaper than maintaining a HashMap.
    for i in 0..n {
        let key_i = col.get(i);
        let mut found = false;
        for slot in order.iter_mut() {
            if col.get(slot.first_row).key_eq(&key_i) {
                slot.rows.push(i);
                found = true;
                break;
            }
        }
        if !found {
            order.push(MarkSlot {
                first_row: i,
                rows: vec![i],
            });
        }
    }
    order
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
            if p.len() % 2 != 0 {
                panic!(
                    "LineGeom::build: \"linetype\" array has odd length {}; \
                     dash patterns must alternate dash/gap (even length, or empty for solid)",
                    p.len()
                );
            }
        }
        Channel::Constant(_) | Channel::RawConstant(_) => {} // non-Linetype constant — resolved at draw time
        Channel::Data(DataColumn::Linetype(v)) | Channel::RawData(DataColumn::Linetype(v)) => {
            for (i, p) in v.iter().enumerate() {
                if p.len() % 2 != 0 {
                    panic!(
                        "LineGeom::build: \"linetype\"[{i}] array has odd length {}; \
                         dash patterns must alternate dash/gap (even length, or empty for solid)",
                        p.len()
                    );
                }
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
        let x_offset_ch = channels.get("x_offset");
        let y_offset_ch = channels.get("y_offset");
        let x_band_ch = channels.get("x_band");
        let y_band_ch = channels.get("y_band");

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

            let stroke_spec = build_stroke(
                linewidth_px,
                cap,
                join,
                &dash_pattern_pt,
                dash_offset_pt,
                ctx.dpi,
            );

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
                points
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
            scene.stroke(
                &stroke_spec,
                Affine::IDENTITY,
                &Brush::Solid(stroke_color),
                None,
                &path,
                pick,
            );
        }
    }
}

// ─── Stroke construction ─────────────────────────────────────────────────────

/// Construct a [`Stroke`] with the pt dash pattern + offset converted
/// to px using `dpi`. An empty pattern leaves the stroke un-dashed
/// (solid).
fn build_stroke(
    width_px: f64,
    cap: Cap,
    join: Join,
    pattern_pt: &[f64],
    offset_pt: f64,
    dpi: f64,
) -> Stroke {
    let mut s = Stroke::new(width_px).with_caps(cap).with_join(join);
    if !pattern_pt.is_empty() {
        let pattern_px: Vec<f64> = pattern_pt.iter().map(|p| pt_to_px(*p, dpi)).collect();
        let offset_px = pt_to_px(offset_pt, dpi);
        s = s.with_dashes(offset_px, pattern_px);
    }
    s
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
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
    #[should_panic(expected = "odd length")]
    fn builder_odd_length_linetype_constant_panics() {
        LineGeom::builder()
            .set("x", vec![0.0_f64, 1.0])
            .set("y", vec![0.0_f64, 1.0])
            .set("linetype", Value::Linetype(Arc::from(vec![1.0, 2.0, 3.0])))
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
