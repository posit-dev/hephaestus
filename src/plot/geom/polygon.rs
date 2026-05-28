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

use crate::brush::Brush;
use crate::geometry::{Affine, Point};
use crate::path::{FillRule, Path};
use crate::plot::diff::{diff_columns, diff_positional, KeyIndex};
use crate::plot::value::{DataColumn, Value};
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
        Some(Channel::Data(col)) => Some(col),
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

        let x_scale = ctx.scale_for("x");
        let y_scale = ctx.scale_for("y");
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

        let channels = &self.state.channels;
        let x_col = match channels.get("x") {
            Some(Channel::Data(c)) => c,
            _ => return,
        };
        let y_col = match channels.get("y") {
            Some(Channel::Data(c)) => c,
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

            // Build the combined path: one closed sub-path per ring.
            let mut path = Path::new();
            let mut any_rings_emitted = false;
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
                if points.len() < 3 {
                    continue;
                }
                path.move_to(points[0]);
                for p in &points[1..] {
                    path.line_to(*p);
                }
                path.close_path();
                any_rings_emitted = true;
            }
            if !any_rings_emitted {
                continue;
            }

            let pick = resolve_pick_id(pick_id_ch, pick_id_scale, i0);

            if let Some(fc) = fill_color {
                scene.fill(
                    FillRule::EvenOdd,
                    Affine::IDENTITY,
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
                    let stroke_spec = build_stroke(
                        linewidth_px,
                        cap,
                        join,
                        &dash_pattern_pt,
                        dash_offset_pt,
                        ctx.dpi,
                    );
                    scene.stroke(
                        &stroke_spec,
                        Affine::IDENTITY,
                        &Brush::Solid(sc),
                        None,
                        &path,
                        pick,
                    );
                }
            }
        }
    }
}

// ─── Stroke construction ─────────────────────────────────────────────────────

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
    use crate::plot::geom::DirectScaleResolver;
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
}
