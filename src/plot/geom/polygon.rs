//! `PolygonGeom` — vectorised closed polygons drawn at scaled `(x, y)`
//! vertices. Multi-row-per-mark, like LineGeom.
//!
//! The `Keys` column identifies marks: rows sharing a key value belong
//! to the same polygon. Within a mark, the per-row `"ring"` channel
//! buckets rows into separate rings — outer ring + 0+ holes — each
//! emitted as a closed sub-path. The fill rule is **EvenOdd**, so the
//! visual difference between outer and holes falls out of the
//! sub-path arrangement: a sub-path enclosed by another is naturally
//! treated as a hole.
//!
//! Vertices within a ring are connected in source order. Polygons close
//! automatically. Non-finite vertices are skipped (the polygon's
//! remaining vertices still close); rings with fewer than 3 finite
//! vertices are dropped.
//!
//! Channels consumed:
//!
//! - `"x"`, `"y"` — vertex position (required; data; numeric).
//! - `"x_offset"`, `"y_offset"`, `"x_band"`, `"y_band"` — per-vertex
//!   offsets / band fractions, applied uniformly to every vertex of
//!   every ring in the mark. The whole polygon translates together;
//!   per-vertex band variation isn't a real use case here.
//! - `"ring"` — per-row ring identifier (any DataColumn variant). Rows
//!   with the same `(key, ring)` value belong to the same ring within
//!   their mark. If unset, every row is in the same ring.
//! - `"fill"`, `"fill_opacity"`, `"stroke"`, `"stroke_opacity"`,
//!   `"linewidth"`, `"linetype"`, `"dash_offset"`, `"cap"`, `"join"` —
//!   per-mark styling, resolved at the mark's first row.
//! - `"expand"` — signed pt offset applied to every ring of the mark
//!   (per-mark; default `0.0`). Positive grows outward, negative
//!   contracts inward; holes are offset in the opposite direction
//!   automatically. Backed by `clipper2`'s Miter-join offset with a
//!   default miter clamp of 4.0. Output may contain more rings than
//!   input (an inward offset can split a "dumbbell") or fewer (a hole
//!   may collapse).
//! - `"corner_radius"` — fillet size in pt applied at each vertex of
//!   every ring after `"expand"` (per-mark; default `0.0`). Maps to
//!   [`CornerRounding::max_cut`](crate::primitives::CornerRounding);
//!   the cut is clamped to half the shorter adjacent edge.
//! - `"angle"` — rotation in **radians** around the mark's centroid
//!   (mean of finite outer-ring vertex positions in panel space),
//!   mathematical CCW. Per-mark; default `0.0`. Holes rotate together
//!   with the outer ring as a rigid body. Applies after expand +
//!   corner rounding (the constructed path is rotated whole).
//!
//! Order matters: `"expand"` is applied **before** `"corner_radius"`.
//! Offsetting an already-filleted polygon would treat the existing
//! arcs as polylines and bake them into the new outline; rounding the
//! offset result is what users typically want ("inset by 4pt, then
//! round the result").

use crate::brush::Brush;
use crate::geometry::{Affine, Point};
use crate::path::{FillRule, Path};
use crate::plot::diff::{diff_columns, diff_positional, KeyIndex};
use crate::plot::value::{DataColumn, Value};
use crate::primitives::{offset_polygon, round_corners, CornerRounding, PolylineSampler};
use crate::scene::SceneBuilder;
use crate::stroke::{Cap, Join, Stroke};

use super::linetype;
use super::resolve::{
    build_stroke_for_pattern, draw_linetype_with_markers, override_alpha, pt_to_px,
    resolve_angle_channel, resolve_cap_channel, resolve_color_channel, resolve_join_channel,
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
/// Miter clamp ratio passed to Clipper2 for `"expand"` offsets. Matches
/// SVG's default `stroke-miterlimit`. Not user-configurable in v1.5; drop
/// to `primitives::offset_polygon` directly if a different clamp is
/// needed.
const MITER_LIMIT: f64 = 4.0;

const CHANNELS: &[(&str, ExpectedOutput)] = &[
    ("x", ExpectedOutput::Numbers),
    ("y", ExpectedOutput::Numbers),
    ("x_offset", ExpectedOutput::Numbers),
    ("y_offset", ExpectedOutput::Numbers),
    ("x_band", ExpectedOutput::Numbers),
    ("y_band", ExpectedOutput::Numbers),
    ("ring", ExpectedOutput::Any),
    ("fill", ExpectedOutput::Colors),
    ("stroke", ExpectedOutput::Colors),
    ("fill_opacity", ExpectedOutput::Numbers),
    ("stroke_opacity", ExpectedOutput::Numbers),
    ("linewidth", ExpectedOutput::Numbers),
    ("linetype", ExpectedOutput::Linetypes),
    ("dash_offset", ExpectedOutput::Numbers),
    ("cap", ExpectedOutput::Strings),
    ("join", ExpectedOutput::Strings),
    ("expand", ExpectedOutput::Numbers),
    ("corner_radius", ExpectedOutput::Numbers),
    ("corner_max_angle", ExpectedOutput::Numbers),
    ("angle", ExpectedOutput::Numbers),
    ("pick_id", ExpectedOutput::Numbers),
];

// ─── PolygonGeom ─────────────────────────────────────────────────────────────

/// A vectorised polygon geom. Multi-row-per-mark; supports holes via
/// the `"ring"` channel.
pub struct PolygonGeom {
    pub(crate) state: GeomState,
    /// Cached mark layout — rebuilt by `rebuild_diff_against_previous`
    /// or lazily inside `draw` if no diff has been triggered yet.
    pub(crate) marks: Vec<PolygonMarkSlot>,
}

/// One mark in the geom — a polygon composed of N rings, each composed
/// of M rows. Sub-paths within a mark are fill-rule-combined (EvenOdd).
#[derive(Clone, Debug)]
pub(crate) struct PolygonMarkSlot {
    /// First-appearance row index of this mark's key. Used to resolve
    /// per-mark channels.
    pub(crate) first_row: usize,
    /// One entry per ring, in first-appearance order of ring value.
    pub(crate) rings: Vec<Vec<usize>>,
}

crate::impl_geom_inherents_grouped!(PolygonGeom);

impl PolygonGeom {
    /// Build the mark layout from the current keys + ring columns.
    pub(crate) fn build_marks(&self) -> Vec<PolygonMarkSlot> {
        let ring_ch = self.state.channels.get("ring");
        match &self.state.keys {
            Keys::Positional(n) => (0..*n)
                .map(|i| PolygonMarkSlot {
                    first_row: i,
                    rings: vec![vec![i]],
                })
                .collect(),
            Keys::Explicit(col) => build_marks_from_columns(col, ring_ch),
        }
    }
}

/// Bucket rows first by mark (key value), then within each mark bucket
/// by ring value. Order is first-appearance order at both levels.
fn build_marks_from_columns(keys: &DataColumn, ring_ch: Option<&Channel>) -> Vec<PolygonMarkSlot> {
    let n = keys.len();
    // First pass: collect row indices per mark in first-appearance order.
    struct MarkBucket {
        first_row: usize,
        rows: Vec<usize>,
    }
    let mut marks: Vec<MarkBucket> = Vec::new();
    for i in 0..n {
        let key_i = keys.get(i);
        let mut found = false;
        for bucket in marks.iter_mut() {
            if keys.get(bucket.first_row).key_eq(&key_i) {
                bucket.rows.push(i);
                found = true;
                break;
            }
        }
        if !found {
            marks.push(MarkBucket {
                first_row: i,
                rows: vec![i],
            });
        }
    }

    // Second pass: within each mark, bucket rows by ring value.
    let ring_data = match ring_ch {
        Some(Channel::Data(col)) | Some(Channel::RawData(col)) => Some(col),
        _ => None, // unset OR constant → all rows in one ring
    };

    marks
        .into_iter()
        .map(|bucket| {
            let rings = bucket_rows_by_ring(&bucket.rows, ring_data);
            PolygonMarkSlot {
                first_row: bucket.first_row,
                rings,
            }
        })
        .collect()
}

/// Bucket a mark's rows by ring value, in first-appearance order.
/// When `ring_col` is None, returns a single ring containing all rows.
fn bucket_rows_by_ring(rows: &[usize], ring_col: Option<&DataColumn>) -> Vec<Vec<usize>> {
    let col = match ring_col {
        None => return vec![rows.to_vec()],
        Some(c) => c,
    };
    // Local "first row index of each ring value" tracker.
    struct RingBucket {
        first_row: usize,
        rows: Vec<usize>,
    }
    let mut buckets: Vec<RingBucket> = Vec::new();
    for &i in rows {
        let r_i = col.get(i);
        let mut found = false;
        for b in buckets.iter_mut() {
            if col.get(b.first_row).key_eq(&r_i) {
                b.rows.push(i);
                found = true;
                break;
            }
        }
        if !found {
            buckets.push(RingBucket {
                first_row: i,
                rows: vec![i],
            });
        }
    }
    buckets.into_iter().map(|b| b.rows).collect()
}

/// Build a column of one entry per mark — the key value of each mark's
/// first row. Used to feed `diff_columns` at mark granularity. Same
/// idea as LineGeom's `unique_keys_column`.
fn unique_keys_column(col: &DataColumn, marks: &[PolygonMarkSlot]) -> DataColumn {
    let template = empty_datacolumn_like(col);
    push_values_into(template, marks.iter().map(|m| col.get(m.first_row)))
}

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
            _ => panic!("PolygonGeom: unique-keys column variant mismatch"),
        }
    }
    template
}

// ─── BuildableGeom impl ──────────────────────────────────────────────────────

impl BuildableGeom for PolygonGeom {
    fn build_from(builder: GeomBuilder<Self>) -> Self {
        let (keys_opt, channels) = builder.into_parts();

        let n = require_data_column("x", &channels, "PolygonGeom").len();
        let y_len = require_data_column("y", &channels, "PolygonGeom").len();
        if y_len != n {
            panic!("PolygonGeom::build: \"y\" length {y_len} does not match \"x\" length {n}");
        }
        validate_channel_lengths(&channels, n, "PolygonGeom");
        validate_pick_id_channel(&channels, "PolygonGeom");

        let declared = filter_declared(&channels, CHANNELS);
        let state = GeomState::from_builder(keys_opt, channels, n, KeysStrategy::OneMark, declared);
        PolygonGeom {
            state,
            marks: Vec::new(),
        }
    }
}

// ─── Geom impl ───────────────────────────────────────────────────────────────

impl Geom for PolygonGeom {
    fn state(&self) -> &GeomState {
        &self.state
    }

    fn state_mut(&mut self) -> &mut GeomState {
        &mut self.state
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn mark_count(&self) -> usize {
        if self.marks.is_empty() && !self.is_empty() {
            return self.build_marks().len();
        }
        self.marks.len()
    }

    /// Override: drop the cached mark layout when state rotates.
    fn invalidate_caches(&mut self) {
        self.marks.clear();
    }

    fn rebuild_diff_against_previous(&mut self) {
        if !self.state.dirty {
            return;
        }
        let next_marks = self.build_marks();
        let prev_marks = match &self.state.prev_keys {
            Keys::Explicit(col) if !col.is_empty() => {
                let prev_ring = self.state.prev_channels.get("ring");
                build_marks_from_columns(col, prev_ring)
            }
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

        let owned_marks;
        let marks: &[PolygonMarkSlot] = if self.marks.is_empty() && !self.is_empty() {
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
        let x_offset_scale = ctx.scale_for("x_offset");
        let y_offset_scale = ctx.scale_for("y_offset");
        let x_band_scale = ctx.scale_for("x_band");
        let y_band_scale = ctx.scale_for("y_band");
        let fill_scale = ctx.scale_for("fill");
        let stroke_scale = ctx.scale_for("stroke");
        let fill_opacity_scale = ctx.scale_for("fill_opacity");
        let stroke_opacity_scale = ctx.scale_for("stroke_opacity");
        let linewidth_scale = ctx.scale_for("linewidth");
        let linetype_scale = ctx.scale_for("linetype");
        let dash_offset_scale = ctx.scale_for("dash_offset");
        let cap_scale = ctx.scale_for("cap");
        let join_scale = ctx.scale_for("join");
        let pick_id_scale = ctx.scale_for("pick_id");
        let expand_scale = ctx.scale_for("expand");
        let corner_radius_scale = ctx.scale_for("corner_radius");
        let corner_max_angle_scale = ctx.scale_for("corner_max_angle");
        let angle_scale = ctx.scale_for("angle");

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
        let x_offset_ch = channels.get("x_offset");
        let y_offset_ch = channels.get("y_offset");
        let x_band_ch = channels.get("x_band");
        let y_band_ch = channels.get("y_band");
        let fill_ch = channels.get("fill");
        let stroke_ch = channels.get("stroke");
        let fill_opacity_ch = channels.get("fill_opacity");
        let stroke_opacity_ch = channels.get("stroke_opacity");
        let linewidth_ch = channels.get("linewidth");
        let linetype_ch = channels.get("linetype");
        let dash_offset_ch = channels.get("dash_offset");
        let cap_ch = channels.get("cap");
        let join_ch = channels.get("join");
        let pick_id_ch = channels.get("pick_id");
        let expand_ch = channels.get("expand");
        let corner_radius_ch = channels.get("corner_radius");
        let corner_max_angle_ch = channels.get("corner_max_angle");
        let angle_ch = channels.get("angle");

        for mark in marks.iter() {
            let i0 = mark.first_row;

            let fill_color = override_alpha(
                resolve_color_channel(fill_ch, fill_scale, i0),
                resolve_number_channel(fill_opacity_ch, fill_opacity_scale, i0),
            );
            let stroke_color = override_alpha(
                resolve_color_channel(stroke_ch, stroke_scale, i0),
                resolve_number_channel(stroke_opacity_ch, stroke_opacity_scale, i0),
            );
            if fill_color.is_none() && stroke_color.is_none() {
                continue;
            }

            // Resolve per-mark expand + corner_radius once.
            let expand_pt = resolve_number_channel_or(expand_ch, expand_scale, i0, 0.0);
            let expand_px = pt_to_px(expand_pt, ctx.dpi);
            let corner_radius_pt =
                resolve_number_channel_or(corner_radius_ch, corner_radius_scale, i0, 0.0);
            let corner_radius_px = pt_to_px(corner_radius_pt, ctx.dpi);
            let corner_max_angle_deg = resolve_number_channel_or(
                corner_max_angle_ch,
                corner_max_angle_scale,
                i0,
                f64::INFINITY,
            );

            // First pass: build vertex sequences for every ring.
            let mut rings_pts: Vec<Vec<Point>> = Vec::with_capacity(mark.rings.len());
            for ring in &mark.rings {
                let mut points: Vec<Point> = Vec::with_capacity(ring.len());
                for &i in ring {
                    let x_band = resolve_number_channel_or(x_band_ch, x_band_scale, i, 0.0);
                    let y_band = resolve_number_channel_or(y_band_ch, y_band_scale, i, 0.0);
                    let x_frac = resolve_position(x_col.get(i), x_scale, x_band);
                    let y_frac = resolve_position(y_col.get(i), y_scale, y_band);
                    if !x_frac.is_finite() || !y_frac.is_finite() {
                        continue;
                    }
                    let mut px = panel.x0 + x_frac * panel_w;
                    let mut py = panel.y1 - y_frac * panel_h;
                    if let Some(off) = resolve_number_channel(x_offset_ch, x_offset_scale, i) {
                        px += pt_to_px(off, ctx.dpi);
                    }
                    if let Some(off) = resolve_number_channel(y_offset_ch, y_offset_scale, i) {
                        py -= pt_to_px(off, ctx.dpi);
                    }
                    points.push(Point::new(px, py));
                }
                if points.len() >= 3 {
                    rings_pts.push(points);
                }
            }
            if rings_pts.is_empty() {
                continue;
            }

            // Rotation pivot: outer-ring centroid in panel space, computed
            // from raw vertex positions before `expand` / corner rounding
            // so the pivot tracks the user-supplied data even when the
            // outline is deformed by the offset pass.
            let angle = resolve_angle_channel(angle_ch, angle_scale, i0);
            let xform = if angle == 0.0 || rings_pts.is_empty() {
                Affine::IDENTITY
            } else {
                let outer = &rings_pts[0];
                let n_pts = outer.len() as f64;
                let cx = outer.iter().map(|p| p.x).sum::<f64>() / n_pts;
                let cy = outer.iter().map(|p| p.y).sum::<f64>() / n_pts;
                Affine::rotate_about(-angle, Point::new(cx, cy))
            };

            // Order is fixed: expand first, then corner rounding. Insetting
            // a polygon with already-filleted corners produces an inset
            // path whose old fillets are now lines plus a fresh inset
            // shape with sharp corners — visually wrong. Offsetting first
            // and rounding the offset rings gives the intuitive result.
            let offset_rings: Vec<Vec<Point>> = if expand_px != 0.0 && expand_px.is_finite() {
                let refs: Vec<&[Point]> = rings_pts.iter().map(|r| r.as_slice()).collect();
                offset_polygon(&refs, expand_px, MITER_LIMIT)
            } else {
                rings_pts
            };

            let mut path = Path::new();
            let mut any_rings_emitted = false;
            for ring in &offset_rings {
                if ring.len() < 3 {
                    continue;
                }
                if corner_radius_px > 0.0 {
                    let opts = CornerRounding {
                        max_cut: corner_radius_px,
                        max_angle_deg: corner_max_angle_deg,
                    };
                    let sub = round_corners(ring, true, opts);
                    for el in sub.iter() {
                        path.push(el);
                    }
                } else {
                    path.move_to(ring[0]);
                    for p in &ring[1..] {
                        path.line_to(*p);
                    }
                    path.close_path();
                }
                any_rings_emitted = true;
            }
            if !any_rings_emitted {
                continue;
            }

            let pick = resolve_pick_id(pick_id_ch, pick_id_scale, i0);

            if let Some(fc) = fill_color {
                scene.fill(
                    FillRule::EvenOdd,
                    xform,
                    &Brush::Solid(fc),
                    None,
                    &path,
                    pick,
                );
            }
            if let Some(sc) = stroke_color {
                let linewidth_pt = resolve_number_channel_or(
                    linewidth_ch,
                    linewidth_scale,
                    i0,
                    DEFAULT_LINEWIDTH_PT,
                );
                let linewidth_px = pt_to_px(linewidth_pt, ctx.dpi);
                if linewidth_px.is_finite() && linewidth_px > 0.0 {
                    let dash_pattern_pt = resolve_linetype_channel(linetype_ch, linetype_scale, i0);
                    let dash_offset_pt =
                        resolve_number_channel_or(dash_offset_ch, dash_offset_scale, i0, 0.0);
                    let cap = resolve_cap_channel(cap_ch, cap_scale, i0, DEFAULT_CAP);
                    let join = resolve_join_channel(join_ch, join_scale, i0, DEFAULT_JOIN);
                    if linetype::is_marker_free(&dash_pattern_pt) {
                        let stroke_spec = build_stroke_for_pattern(
                            linewidth_px,
                            cap,
                            join,
                            &dash_pattern_pt,
                            dash_offset_pt,
                            linewidth_pt,
                            ctx.dpi,
                        );
                        scene.stroke(&stroke_spec, xform, &Brush::Solid(sc), None, &path, pick);
                    } else {
                        // Markers: walk the closed perimeter, scale
                        // gaps so the pattern fits seamlessly, fill
                        // every marker with the stroke colour.
                        let samplers = PolylineSampler::from_closed_path(&path, 0.5);
                        let solid_stroke_spec =
                            Stroke::new(linewidth_px).with_caps(cap).with_join(join);
                        let dash_offset_px = pt_to_px(dash_offset_pt, ctx.dpi);
                        draw_linetype_with_markers(
                            scene,
                            &samplers,
                            &dash_pattern_pt,
                            dash_offset_px,
                            linewidth_px,
                            sc,
                            sc,
                            &solid_stroke_spec,
                            xform,
                            ctx.shapes,
                            ctx.dpi,
                            pick,
                            /* distribute */ true,
                        );
                    }
                }
            }
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::color::Color;
    use crate::geometry::Rect;
    use crate::plot::geom::{DirectScaleResolver, Raw};
    use crate::scene::recording::{Op, RecordingScene};

    fn shapes() -> crate::shape::ShapeRegistry {
        crate::shape::ShapeRegistry::with_builtins()
    }

    fn ctx<'a>(
        panel: Rect,
        registry: &'a crate::shape::ShapeRegistry,
        scales: &'a DirectScaleResolver<'a>,
    ) -> GeomContext<'a> {
        GeomContext::new(panel, 96.0, registry, scales)
    }

    fn red() -> Color {
        Color::new([1.0, 0.0, 0.0, 1.0])
    }

    // ── build() ──

    #[test]
    fn no_keys_synthesises_single_mark() {
        let g = PolygonGeom::builder()
            .set("x", vec![0.0_f64, 1.0, 1.0, 0.0])
            .set("y", vec![0.0_f64, 0.0, 1.0, 1.0])
            .build();
        assert_eq!(g.len(), 4);
        assert_eq!(g.mark_count(), 1);
    }

    #[test]
    fn explicit_keys_define_marks() {
        let g = PolygonGeom::builder()
            .keys(vec!["A", "A", "A", "B", "B", "B"])
            .set("x", vec![0.0_f64, 1.0, 0.0, 2.0, 3.0, 2.0])
            .set("y", vec![0.0_f64, 0.0, 1.0, 0.0, 0.0, 1.0])
            .build();
        assert_eq!(g.mark_count(), 2);
    }

    #[test]
    fn ring_channel_buckets_within_mark() {
        // Single mark with two rings: outer (4 vertices, ring=0) +
        // inner hole (4 vertices, ring=1).
        let g = PolygonGeom::builder()
            .set("x", vec![0.0_f64, 1.0, 1.0, 0.0, 0.25, 0.75, 0.75, 0.25])
            .set("y", vec![0.0_f64, 0.0, 1.0, 1.0, 0.25, 0.25, 0.75, 0.75])
            .set("ring", vec![0_i32, 0, 0, 0, 1, 1, 1, 1])
            .build();
        let marks = g.build_marks();
        assert_eq!(marks.len(), 1);
        assert_eq!(marks[0].rings.len(), 2);
        assert_eq!(marks[0].rings[0], vec![0, 1, 2, 3]);
        assert_eq!(marks[0].rings[1], vec![4, 5, 6, 7]);
    }

    #[test]
    fn unset_ring_means_single_ring_per_mark() {
        let g = PolygonGeom::builder()
            .keys(vec!["A", "A", "A", "B", "B", "B"])
            .set("x", vec![0.0_f64, 1.0, 0.0, 2.0, 3.0, 2.0])
            .set("y", vec![0.0_f64, 0.0, 1.0, 0.0, 0.0, 1.0])
            .build();
        let marks = g.build_marks();
        assert_eq!(marks.len(), 2);
        for m in &marks {
            assert_eq!(
                m.rings.len(),
                1,
                "mark should have one ring when ring is unset"
            );
        }
    }

    #[test]
    #[should_panic(expected = "missing required channel")]
    fn missing_x_panics() {
        PolygonGeom::builder()
            .set("y", vec![0.0_f64, 1.0, 0.0])
            .build();
    }

    #[test]
    #[should_panic(expected = "does not match")]
    fn length_mismatch_panics() {
        PolygonGeom::builder()
            .set("x", vec![0.0_f64, 1.0, 0.0])
            .set("y", vec![0.0_f64, 1.0])
            .build();
    }

    // ── Drawing ──

    #[test]
    fn fills_one_subpath_when_one_ring() {
        let mut g = PolygonGeom::builder()
            .set("x", vec![0.1_f64, 0.5, 0.9, 0.5])
            .set("y", vec![0.5_f64, 0.1, 0.5, 0.9])
            .set("fill", red())
            .build();
        g.rebuild_diff_against_previous();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales),
        );
        let fills = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Fill { .. }))
            .count();
        assert_eq!(fills, 1);
    }

    #[test]
    fn fills_with_even_odd_rule() {
        let mut g = PolygonGeom::builder()
            .set("x", vec![0.1_f64, 0.9, 0.9, 0.1])
            .set("y", vec![0.1_f64, 0.1, 0.9, 0.9])
            .set("fill", red())
            .build();
        g.rebuild_diff_against_previous();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales),
        );
        let rule = scene
            .ops
            .iter()
            .find_map(|op| match op {
                Op::Fill { rule, .. } => Some(*rule),
                _ => None,
            })
            .expect("fill");
        assert!(matches!(rule, FillRule::EvenOdd));
    }

    #[test]
    fn polygon_with_hole_produces_two_closed_subpaths() {
        let mut g = PolygonGeom::builder()
            .set("x", vec![0.1_f64, 0.9, 0.9, 0.1, 0.35, 0.65, 0.65, 0.35])
            .set("y", vec![0.1_f64, 0.1, 0.9, 0.9, 0.35, 0.35, 0.65, 0.65])
            .set("ring", vec![0_i32, 0, 0, 0, 1, 1, 1, 1])
            .set("fill", red())
            .build();
        g.rebuild_diff_against_previous();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales),
        );
        for op in &scene.ops {
            if let Op::Fill { path, .. } = op {
                // Two ClosePath elements = two sub-paths.
                let close_count = path
                    .elements()
                    .iter()
                    .filter(|el| matches!(el, kurbo::PathEl::ClosePath))
                    .count();
                assert_eq!(close_count, 2);
                let move_count = path
                    .elements()
                    .iter()
                    .filter(|el| matches!(el, kurbo::PathEl::MoveTo(_)))
                    .count();
                assert_eq!(move_count, 2);
                return;
            }
        }
        panic!("no fill op emitted");
    }

    #[test]
    fn nonfinite_vertex_skipped_within_ring() {
        // A square with one bad vertex — the remaining 3 close into a triangle.
        let mut g = PolygonGeom::builder()
            .set("x", vec![0.1_f64, f64::NAN, 0.9, 0.1])
            .set("y", vec![0.1_f64, 0.1, 0.9, 0.9])
            .set("fill", red())
            .build();
        g.rebuild_diff_against_previous();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales),
        );
        let fills = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Fill { .. }))
            .count();
        assert_eq!(fills, 1);
    }

    #[test]
    fn ring_with_under_three_finite_vertices_dropped() {
        // Outer ring has 4 vertices; hole ring has only 2 (degenerate)
        // — hole should be silently dropped but outer renders.
        let mut g = PolygonGeom::builder()
            .set("x", vec![0.1_f64, 0.9, 0.9, 0.1, 0.4, 0.6])
            .set("y", vec![0.1_f64, 0.1, 0.9, 0.9, 0.5, 0.5])
            .set("ring", vec![0_i32, 0, 0, 0, 1, 1])
            .set("fill", red())
            .build();
        g.rebuild_diff_against_previous();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales),
        );
        for op in &scene.ops {
            if let Op::Fill { path, .. } = op {
                let close_count = path
                    .elements()
                    .iter()
                    .filter(|el| matches!(el, kurbo::PathEl::ClosePath))
                    .count();
                assert_eq!(close_count, 1, "only outer ring should render");
                return;
            }
        }
        panic!("no fill op emitted");
    }

    #[test]
    fn no_fill_no_stroke_emits_nothing() {
        let mut g = PolygonGeom::builder()
            .set("x", vec![0.1_f64, 0.9, 0.5])
            .set("y", vec![0.1_f64, 0.1, 0.9])
            .build();
        g.rebuild_diff_against_previous();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales),
        );
        assert!(scene.ops.is_empty());
    }

    #[test]
    fn stroke_only_traces_outline() {
        let mut g = PolygonGeom::builder()
            .set("x", vec![0.1_f64, 0.9, 0.5])
            .set("y", vec![0.1_f64, 0.1, 0.9])
            .set("stroke", red())
            .set("linewidth", 2.0_f64)
            .build();
        g.rebuild_diff_against_previous();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales),
        );
        let (fills, strokes) = scene.ops.iter().fold((0, 0), |(f, s), op| match op {
            Op::Fill { .. } => (f + 1, s),
            Op::Stroke { .. } => (f, s + 1),
            _ => (f, s),
        });
        assert_eq!(fills, 0);
        assert_eq!(strokes, 1);
    }

    #[test]
    fn within_mark_fill_divergence_uses_first_row() {
        // Two-vertex polygon (degenerate to test channel resolution).
        // Use 3+ vertices so the ring isn't dropped.
        let red_solid = Color::new([1.0, 0.0, 0.0, 1.0]);
        let blue_solid = Color::new([0.0, 0.0, 1.0, 1.0]);
        let mut g = PolygonGeom::builder()
            .keys(vec!["A", "A", "A"])
            .set("x", vec![0.1_f64, 0.9, 0.5])
            .set("y", vec![0.1_f64, 0.1, 0.9])
            .set("fill", vec![red_solid, blue_solid, blue_solid])
            .build();
        g.rebuild_diff_against_previous();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales),
        );
        for op in &scene.ops {
            if let Op::Fill {
                brush: crate::brush::Brush::Solid(c),
                ..
            } = op
            {
                // First-row fill: red.
                assert_eq!(*c, red_solid);
                return;
            }
        }
        panic!("no fill op emitted");
    }

    #[test]
    fn diff_marks_enter_on_first_draw() {
        let mut g = PolygonGeom::builder()
            .keys(vec!["A", "A", "A", "B", "B", "B"])
            .set("x", vec![0.0_f64, 1.0, 0.0, 2.0, 3.0, 2.0])
            .set("y", vec![0.0_f64, 0.0, 1.0, 0.0, 0.0, 1.0])
            .build();
        g.rebuild_diff_against_previous();
        assert_eq!(g.state.enter.len(), 2);
        assert_eq!(g.state.exit.len(), 0);
    }

    #[test]
    fn diff_mark_exits_when_removed() {
        let mut g = PolygonGeom::builder()
            .keys(vec!["A", "A", "A", "B", "B", "B"])
            .set("x", vec![0.0_f64, 1.0, 0.0, 2.0, 3.0, 2.0])
            .set("y", vec![0.0_f64, 0.0, 1.0, 0.0, 0.0, 1.0])
            .build();
        g.rebuild_diff_against_previous();
        g.update(|b| {
            b.keys(vec!["A", "A", "A"]);
            b.set("x", vec![0.0_f64, 1.0, 0.0]);
            b.set("y", vec![0.0_f64, 0.0, 1.0]);
        });
        g.rebuild_diff_against_previous();
        assert_eq!(g.state.exit.len(), 1);
        assert!(g.state.exit[0].key_eq(&Value::String(Arc::from("B"))));
    }

    #[test]
    fn pick_id_per_mark_resolves_from_first_row() {
        // Per-mark pick id: each mark gets the pick_id value from its
        // first row. Within-mark variation is silently ignored.
        let mut g = PolygonGeom::builder()
            .keys(vec!["A", "A", "A", "B", "B", "B", "C", "C", "C"])
            .set("x", vec![0.0_f64, 0.2, 0.1, 0.4, 0.6, 0.5, 0.7, 0.9, 0.8])
            .set("y", vec![0.0_f64, 0.0, 0.2, 0.0, 0.0, 0.2, 0.0, 0.0, 0.2])
            .set("fill", red())
            .set("pick_id", vec![1001_i64, 0, 0, 2002, 0, 0, 3003, 0, 0])
            .build();
        g.rebuild_diff_against_previous();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales),
        );
        let picks: Vec<u32> = scene
            .ops
            .iter()
            .filter_map(|op| match op {
                Op::Fill {
                    pick_id: crate::pick::PickId::Id(n),
                    ..
                } => Some(*n),
                _ => None,
            })
            .collect();
        assert_eq!(picks, vec![1001, 2002, 3003]);
    }

    #[test]
    fn declared_channels_alphabetical() {
        let g = PolygonGeom::builder()
            .set("x", vec![0.0_f64, 1.0, 0.0])
            .set("y", vec![0.0_f64, 0.0, 1.0])
            .set("fill", red())
            .build();
        let names: Vec<&str> = g.declared_channels().iter().map(|d| d.name).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
    }

    // ── corner_radius / expand ──

    fn fill_path(scene: &RecordingScene) -> Option<crate::path::Path> {
        scene.ops.iter().find_map(|op| match op {
            Op::Fill { path, .. } => Some(path.clone()),
            _ => None,
        })
    }

    #[test]
    fn corner_radius_produces_fillets_per_corner() {
        // Unit square (4 corners) drawn via Raw fractions, with
        // corner_radius set → 4 fillets in the resulting path.
        let mut g = PolygonGeom::builder()
            .set("x", Raw(vec![0.2_f64, 0.8, 0.8, 0.2]))
            .set("y", Raw(vec![0.2_f64, 0.2, 0.8, 0.8]))
            .set("fill", red())
            .set("corner_radius", 5.0_f64)
            .build();
        g.rebuild_diff_against_previous();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales),
        );
        let path = fill_path(&scene).expect("fill");
        let curves = path
            .elements()
            .iter()
            .filter(|el| matches!(el, kurbo::PathEl::CurveTo(_, _, _)))
            .count();
        assert_eq!(curves, 4);
    }

    #[test]
    fn expand_grows_polygon_bbox() {
        // Square 60×60 at panel fractions 0.2..0.8 → 20..80 px on a
        // 100-px panel, so bbox is 60 wide. expand = 5pt (≈ 6.67px at
        // 96 dpi) grows each side outward → bbox 60 + 2*6.67 ≈ 73.33.
        use kurbo::Shape;
        let mut g = PolygonGeom::builder()
            .set("x", Raw(vec![0.2_f64, 0.8, 0.8, 0.2]))
            .set("y", Raw(vec![0.2_f64, 0.2, 0.8, 0.8]))
            .set("fill", red())
            .set("expand", 5.0_f64)
            .build();
        g.rebuild_diff_against_previous();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales),
        );
        let path = fill_path(&scene).expect("fill");
        let bb = path.bounding_box();
        let expected_width = 60.0 + 2.0 * 5.0 * 96.0 / 72.0;
        assert!(
            (bb.width() - expected_width).abs() < 0.5,
            "width = {}, expected ~{}",
            bb.width(),
            expected_width
        );
    }

    #[test]
    fn expand_then_corner_radius_order_is_fixed() {
        // expand first → corner_radius applied to the expanded
        // outline. The test just verifies the combination doesn't
        // panic and the output has curves (from rounding) plus a
        // bbox at least as large as the un-expanded original.
        use kurbo::Shape;
        let mut g = PolygonGeom::builder()
            .set("x", Raw(vec![0.2_f64, 0.8, 0.8, 0.2]))
            .set("y", Raw(vec![0.2_f64, 0.2, 0.8, 0.8]))
            .set("fill", red())
            .set("expand", 5.0_f64)
            .set("corner_radius", 3.0_f64)
            .build();
        g.rebuild_diff_against_previous();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales),
        );
        let path = fill_path(&scene).expect("fill");
        let curves = path
            .elements()
            .iter()
            .filter(|el| matches!(el, kurbo::PathEl::CurveTo(_, _, _)))
            .count();
        assert!(
            curves >= 4,
            "expected ≥ 4 curves after expand+round, got {curves}"
        );
        let bb = path.bounding_box();
        // Outer bbox should at least cover the expanded edges.
        assert!(bb.width() > 65.0, "width = {}", bb.width());
    }

    #[test]
    fn linetype_marker_stamps_around_closed_perimeter() {
        // Polygon with a marker-only linetype. Closed-shape semantics:
        // - markers are stamped along the perimeter via fill ops;
        // - marker fill colour is the stroke colour (does NOT consult
        //   the `"fill"` channel — that fills the polygon interior);
        // - gaps are distributed so the pattern wraps seamlessly,
        //   meaning we get an integer number of markers around the
        //   loop with no visible seam.
        use crate::plot::geom::linetype;
        let pat = linetype::pattern([linetype::marker("circle"), linetype::gap(5.0)]);
        let blue = Color::new([0.0, 0.0, 1.0, 1.0]);
        let mut g = PolygonGeom::builder()
            // Unit square in Raw [0, 1] fractions; on a 100×100 panel
            // this paints a 100×100 square (perimeter = 400 px).
            .set("x", Raw(vec![0.0_f64, 1.0, 1.0, 0.0]))
            .set("y", Raw(vec![0.0_f64, 0.0, 1.0, 1.0]))
            .set("stroke", red())
            // Polygon interior fill — markers ignore this and use the
            // stroke colour.
            .set("fill", blue)
            .set("linewidth", 4.0_f64)
            .set("linetype", Value::Linetype(pat))
            .build();
        g.rebuild_diff_against_previous();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales),
        );
        // Marker-only pattern emits no dash sub-strokes.
        let strokes = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Stroke { .. }))
            .count();
        assert_eq!(strokes, 0, "marker-only pattern emits no Dash strokes");

        // Fills: 1 polygon-interior fill + N marker stamps. The first
        // fill is the polygon interior in `blue`; the rest are
        // markers in the stroke colour.
        let fill_colors: Vec<crate::color::Color> = scene
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
        assert!(fill_colors.len() >= 2, "expected interior + markers");
        assert_eq!(fill_colors[0], blue, "first fill = polygon interior");
        for c in &fill_colors[1..] {
            assert_eq!(
                *c,
                red(),
                "marker fill defaults to stroke colour on closed shapes"
            );
        }

        // Distributed walk: period = linewidth_px + gap_px ≈ 12 px;
        // perimeter = 400 px → round(400/12) = 33 markers.
        assert_eq!(
            fill_colors.len() - 1,
            33,
            "got {} markers around perimeter",
            fill_colors.len() - 1
        );
    }
}
