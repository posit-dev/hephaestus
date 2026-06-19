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

use crate::color::{lerp_color, Color};
use crate::geometry::{Affine, Point, Rect};
#[cfg(test)]
use crate::path::FillRule;
use crate::plot::diff::{diff_columns, diff_positional, KeyIndex};
use crate::plot::projection::InteriorSample;
use crate::plot::scale::Scale;
use crate::plot::value::DataColumn;
use crate::primitives::{polygon_ribbon_full, CornerRounding, RibbonOptions};
use crate::scene::SceneBuilder;

use super::marks::unique_values_at_first_rows;
use super::outline::{draw_polygon_fill_and_stroke, expand_polygons, PolygonSpec};
use super::resolve::{
    channel_varies_across, override_alpha, pt_to_px, resolve_angle_channel, resolve_cap_channel,
    resolve_color_channel, resolve_color_channel_or_theme, resolve_join_channel,
    resolve_linetype_channel, resolve_number_channel, resolve_number_channel_or, resolve_pick_id,
    resolve_position, ChannelBind,
};
use super::state::{finalize_state, require_x_and_siblings, GeomState, KeysStrategy};
use super::{BuildableGeom, Channel, ExpectedOutput, Geom, GeomBuilder, GeomContext, Keys};

// ─── Defaults ────────────────────────────────────────────────────────────────

// Style defaults (linewidth, cap, join) live on `theme.geom.polygon`.
/// Miter clamp ratio passed to Clipper2 for `"expand"` offsets. Matches
/// SVG's default `stroke-miterlimit`. Not user-configurable; drop to
/// `primitives::offset_polygon` directly if a different clamp is needed.
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

// ─── BuildableGeom impl ──────────────────────────────────────────────────────

impl BuildableGeom for PolygonGeom {
    fn build_from(builder: GeomBuilder<Self>) -> Self {
        let (keys_opt, channels) = builder.into_parts();
        let n = require_x_and_siblings(&channels, &["y"], "PolygonGeom");
        let state = finalize_state(
            keys_opt,
            channels,
            n,
            KeysStrategy::OneMark,
            CHANNELS,
            "PolygonGeom",
        );
        PolygonGeom {
            state,
            marks: Vec::new(),
        }
    }
}

// ─── Draw-time channel/scale bundle ──────────────────────────────────────────

/// Channel + scale references for one `PolygonGeom::draw` call. Built
/// once at the top of `draw`, then threaded into [`draw_one_polygon_mark`].
#[derive(Clone, Copy)]
struct PolygonDrawCtx<'a> {
    x_col: &'a DataColumn,
    y_col: &'a DataColumn,
    x_scale: Option<&'a Scale>,
    y_scale: Option<&'a Scale>,
    x_offset: ChannelBind<'a>,
    y_offset: ChannelBind<'a>,
    x_band: ChannelBind<'a>,
    y_band: ChannelBind<'a>,
    fill: ChannelBind<'a>,
    stroke: ChannelBind<'a>,
    fill_opacity: ChannelBind<'a>,
    stroke_opacity: ChannelBind<'a>,
    linewidth: ChannelBind<'a>,
    linetype: ChannelBind<'a>,
    dash_offset: ChannelBind<'a>,
    cap: ChannelBind<'a>,
    join: ChannelBind<'a>,
    pick_id: ChannelBind<'a>,
    expand: ChannelBind<'a>,
    corner_radius: ChannelBind<'a>,
    corner_max_angle: ChannelBind<'a>,
    angle: ChannelBind<'a>,
}

impl<'a> PolygonDrawCtx<'a> {
    /// Resolve `x`/`y` columns + scales and look up every per-mark
    /// channel by name. Returns `None` when `x` or `y` is missing or
    /// non-positional.
    fn build(
        channels: &'a std::collections::HashMap<String, Channel>,
        ctx: &'a GeomContext<'a>,
    ) -> Option<Self> {
        let (x_col, x_scale) = match channels.get("x")? {
            Channel::Data(c) => (c, ctx.scale_for("x")),
            Channel::RawData(c) => (c, None),
            _ => return None,
        };
        let (y_col, y_scale) = match channels.get("y")? {
            Channel::Data(c) => (c, ctx.scale_for("y")),
            Channel::RawData(c) => (c, None),
            _ => return None,
        };
        let b = |name: &str| ChannelBind::from_ctx(channels, ctx, name);
        Some(Self {
            x_col,
            y_col,
            x_scale,
            y_scale,
            x_offset: b("x_offset"),
            y_offset: b("y_offset"),
            x_band: b("x_band"),
            y_band: b("y_band"),
            fill: b("fill"),
            stroke: b("stroke"),
            fill_opacity: b("fill_opacity"),
            stroke_opacity: b("stroke_opacity"),
            linewidth: b("linewidth"),
            linetype: b("linetype"),
            dash_offset: b("dash_offset"),
            cap: b("cap"),
            join: b("join"),
            pick_id: b("pick_id"),
            expand: b("expand"),
            corner_radius: b("corner_radius"),
            corner_max_angle: b("corner_max_angle"),
            angle: b("angle"),
        })
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
                let prev_unique = unique_values_at_first_rows(
                    prev_col,
                    prev_marks.iter().map(|m| m.first_row),
                    "PolygonGeom",
                );
                let next_unique = unique_values_at_first_rows(
                    next_col,
                    next_marks.iter().map(|m| m.first_row),
                    "PolygonGeom",
                );
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

        let dc = match PolygonDrawCtx::build(&self.state.channels, ctx) {
            Some(dc) => dc,
            None => return,
        };

        for mark in marks.iter() {
            draw_one_polygon_mark(scene, ctx, panel, dc, mark);
        }
    }
}

/// Render a single polygon mark — multi-ring contour assembly with
/// per-vertex projection, optional inset / corner rounding, fill +
/// stroke + optional gradient mesh. Each mark is independent; the
/// caller iterates.
fn draw_one_polygon_mark(
    scene: &mut dyn SceneBuilder,
    ctx: &GeomContext<'_>,
    panel: Rect,
    dc: PolygonDrawCtx<'_>,
    mark: &PolygonMarkSlot,
) {
    let PolygonDrawCtx {
        x_col,
        y_col,
        x_scale,
        y_scale,
        x_offset:
            ChannelBind {
                ch: x_offset_ch,
                scale: x_offset_scale,
            },
        y_offset:
            ChannelBind {
                ch: y_offset_ch,
                scale: y_offset_scale,
            },
        x_band: ChannelBind {
            ch: x_band_ch,
            scale: x_band_scale,
        },
        y_band: ChannelBind {
            ch: y_band_ch,
            scale: y_band_scale,
        },
        fill: ChannelBind {
            ch: fill_ch,
            scale: fill_scale,
        },
        stroke: ChannelBind {
            ch: stroke_ch,
            scale: stroke_scale,
        },
        fill_opacity:
            ChannelBind {
                ch: fill_opacity_ch,
                scale: fill_opacity_scale,
            },
        stroke_opacity:
            ChannelBind {
                ch: stroke_opacity_ch,
                scale: stroke_opacity_scale,
            },
        linewidth:
            ChannelBind {
                ch: linewidth_ch,
                scale: linewidth_scale,
            },
        linetype:
            ChannelBind {
                ch: linetype_ch,
                scale: linetype_scale,
            },
        dash_offset:
            ChannelBind {
                ch: dash_offset_ch,
                scale: dash_offset_scale,
            },
        cap: ChannelBind {
            ch: cap_ch,
            scale: cap_scale,
        },
        join: ChannelBind {
            ch: join_ch,
            scale: join_scale,
        },
        pick_id:
            ChannelBind {
                ch: pick_id_ch,
                scale: pick_id_scale,
            },
        expand: ChannelBind {
            ch: expand_ch,
            scale: expand_scale,
        },
        corner_radius:
            ChannelBind {
                ch: corner_radius_ch,
                scale: corner_radius_scale,
            },
        corner_max_angle:
            ChannelBind {
                ch: corner_max_angle_ch,
                scale: corner_max_angle_scale,
            },
        angle: ChannelBind {
            ch: angle_ch,
            scale: angle_scale,
        },
    } = dc;

    let i0 = mark.first_row;

    let fill_color = override_alpha(
        resolve_color_channel_or_theme(
            fill_ch,
            fill_scale,
            i0,
            ctx.theme.geom.polygon.fill.as_ref(),
            &ctx.theme.palette,
        ),
        resolve_number_channel(fill_opacity_ch, fill_opacity_scale, i0),
    );
    let stroke_color = override_alpha(
        resolve_color_channel_or_theme(
            stroke_ch,
            stroke_scale,
            i0,
            ctx.theme.geom.polygon.stroke.as_ref(),
            &ctx.theme.palette,
        ),
        resolve_number_channel(stroke_opacity_ch, stroke_opacity_scale, i0),
    );
    if fill_color.is_none() && stroke_color.is_none() {
        return;
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

    // ── Ribbon-mode decision (Phase E.5). Upgrade the outline
    // stroke to a per-vertex tessellated mesh per ring when
    // `linewidth` or `stroke` varies within the mark. Gated to
    // solid linetype + no `expand` + no corner rounding, since
    // those produce non-polyline outputs that don't fit the
    // ribbon primitive. The fill emission is unaffected.
    let dash_pattern_pt = resolve_linetype_channel(linetype_ch, linetype_scale, i0);
    let all_rows: Vec<usize> = mark.rings.iter().flatten().copied().collect();
    let linewidth_varies = channel_varies_across(linewidth_ch, linewidth_scale, &all_rows);
    let stroke_varies = channel_varies_across(stroke_ch, stroke_scale, &all_rows)
        || channel_varies_across(stroke_opacity_ch, stroke_opacity_scale, &all_rows);
    let ribbon_mode = stroke_color.is_some()
        && dash_pattern_pt.is_empty()
        && expand_pt == 0.0
        && corner_radius_pt == 0.0
        && (linewidth_varies || stroke_varies);

    // First pass: build vertex sequences for every ring.
    // Under non-linear projections, edges between consecutive
    // vertices are densified so polygon outlines follow the
    // projected geodesic. Closes the ring too (last → first
    // edge gets densified just like the others).
    //
    // In ribbon mode, co-build per-ring `widths` and `colors`
    // alongside `points`, lerping each interior-sample attr at
    // the channel-space `t` returned by
    // `interpolate_segment_with_t`.
    let is_linear = ctx.projection.is_linear();
    let mut interior: Vec<(f64, f64)> = Vec::new();
    let mut interior_t: Vec<InteriorSample> = Vec::new();
    let mut rings_pts: Vec<Vec<Point>> = Vec::with_capacity(mark.rings.len());
    let mut rings_widths: Vec<Vec<f64>> = if ribbon_mode {
        Vec::with_capacity(mark.rings.len())
    } else {
        Vec::new()
    };
    let mut rings_colors: Vec<Vec<Color>> = if ribbon_mode {
        Vec::with_capacity(mark.rings.len())
    } else {
        Vec::new()
    };
    let fallback_stroke = stroke_color.unwrap_or_else(|| Color::new([0.0, 0.0, 0.0, 1.0]));
    for ring in &mark.rings {
        let mut points: Vec<Point> = Vec::with_capacity(ring.len());
        let mut widths: Vec<f64> = if ribbon_mode {
            Vec::with_capacity(ring.len())
        } else {
            Vec::new()
        };
        let mut colors: Vec<Color> = if ribbon_mode {
            Vec::with_capacity(ring.len())
        } else {
            Vec::new()
        };
        let mut prev_channels: Option<[f64; 2]> = None;
        let mut first_channels: Option<[f64; 2]> = None;
        let mut prev_w: Option<f64> = None;
        let mut prev_c: Option<Color> = None;
        let mut first_w: Option<f64> = None;
        let mut first_c: Option<Color> = None;
        for &i in ring {
            let x_band = resolve_number_channel_or(x_band_ch, x_band_scale, i, 0.0);
            let y_band = resolve_number_channel_or(y_band_ch, y_band_scale, i, 0.0);
            let x_frac = resolve_position(x_col.get(i), x_scale, x_band);
            let y_frac = resolve_position(y_col.get(i), y_scale, y_band);
            if !x_frac.is_finite() || !y_frac.is_finite() {
                continue;
            }
            let curr_channels = [x_frac, y_frac];

            let (curr_w, curr_c) = if ribbon_mode {
                let w_pt = resolve_number_channel_or(
                    linewidth_ch,
                    linewidth_scale,
                    i,
                    ctx.theme.geom.polygon.linewidth_pt,
                );
                let w_half_px = pt_to_px(w_pt, ctx.dpi) * 0.5;
                let c = override_alpha(
                    resolve_color_channel(stroke_ch, stroke_scale, i),
                    resolve_number_channel(stroke_opacity_ch, stroke_opacity_scale, i),
                )
                .unwrap_or(fallback_stroke);
                (w_half_px, c)
            } else {
                (0.0, fallback_stroke)
            };

            if !is_linear {
                if let Some(prev) = prev_channels {
                    if ribbon_mode {
                        interior_t.clear();
                        ctx.projection.interpolate_segment_with_t(
                            panel,
                            &prev,
                            &curr_channels,
                            &mut interior_t,
                        );
                        let pw = prev_w.unwrap();
                        let pc = prev_c.unwrap();
                        for s in &interior_t {
                            points.push(Point::new(s.px, s.py));
                            widths.push(pw + s.t * (curr_w - pw));
                            colors.push(lerp_color(pc, curr_c, s.t));
                        }
                    } else {
                        interior.clear();
                        ctx.projection.interpolate_segment(
                            panel,
                            &prev,
                            &curr_channels,
                            &mut interior,
                        );
                        for (ipx, ipy) in &interior {
                            points.push(Point::new(*ipx, *ipy));
                        }
                    }
                }
            }
            let (px0, py0) = ctx.projection.project_to_panel_px(panel, &curr_channels);
            let mut px = px0;
            let mut py = py0;
            if let Some(off) = resolve_number_channel(x_offset_ch, x_offset_scale, i) {
                px += pt_to_px(off, ctx.dpi);
            }
            if let Some(off) = resolve_number_channel(y_offset_ch, y_offset_scale, i) {
                py -= pt_to_px(off, ctx.dpi);
            }
            points.push(Point::new(px, py));
            if ribbon_mode {
                widths.push(curr_w);
                colors.push(curr_c);
                prev_w = Some(curr_w);
                prev_c = Some(curr_c);
                if first_w.is_none() {
                    first_w = Some(curr_w);
                    first_c = Some(curr_c);
                }
            }
            if first_channels.is_none() {
                first_channels = Some(curr_channels);
            }
            prev_channels = Some(curr_channels);
        }
        // Densify the closing edge (last vertex back to first).
        if !is_linear {
            if let (Some(prev), Some(first)) = (prev_channels, first_channels) {
                if prev != first {
                    if ribbon_mode {
                        interior_t.clear();
                        ctx.projection.interpolate_segment_with_t(
                            panel,
                            &prev,
                            &first,
                            &mut interior_t,
                        );
                        let pw = prev_w.unwrap();
                        let pc = prev_c.unwrap();
                        let fw = first_w.unwrap();
                        let fc = first_c.unwrap();
                        for s in &interior_t {
                            points.push(Point::new(s.px, s.py));
                            widths.push(pw + s.t * (fw - pw));
                            colors.push(lerp_color(pc, fc, s.t));
                        }
                    } else {
                        interior.clear();
                        ctx.projection
                            .interpolate_segment(panel, &prev, &first, &mut interior);
                        for (ipx, ipy) in &interior {
                            points.push(Point::new(*ipx, *ipy));
                        }
                    }
                }
            }
        }
        if points.len() >= 3 {
            rings_pts.push(points);
            if ribbon_mode {
                rings_widths.push(widths);
                rings_colors.push(colors);
            }
        }
    }
    if rings_pts.is_empty() {
        return;
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
    let offset_rings = expand_polygons(rings_pts, None, expand_px, MITER_LIMIT);
    if offset_rings.iter().all(|r| r.len() < 3) {
        return;
    }

    let pick = resolve_pick_id(pick_id_ch, pick_id_scale, i0);
    let linewidth_pt = resolve_number_channel_or(
        linewidth_ch,
        linewidth_scale,
        i0,
        ctx.theme.geom.polygon.linewidth_pt,
    );
    let dash_offset_pt = resolve_number_channel_or(dash_offset_ch, dash_offset_scale, i0, 0.0);
    let cap = resolve_cap_channel(cap_ch, cap_scale, i0, ctx.theme.geom.polygon.cap);
    let join = resolve_join_channel(join_ch, join_scale, i0, ctx.theme.geom.polygon.join);
    let corner_rounding = (corner_radius_px > 0.0).then_some(CornerRounding {
        max_cut: corner_radius_px,
        max_angle_deg: corner_max_angle_deg,
    });

    // The shared helper emits the EvenOdd fill (when bound) and the
    // closed stroke (when bound). Ribbon mode replaces the closed
    // stroke with a per-ring mesh, so we suppress the helper's stroke
    // in that case and emit the mesh below.
    let spec = PolygonSpec {
        fill_color,
        stroke_color: if ribbon_mode { None } else { stroke_color },
        linewidth_pt,
        dash_pattern_pt: dash_pattern_pt.clone(),
        dash_offset_pt,
        cap,
        join,
        corner_rounding,
        marker_fill: stroke_color.unwrap_or(Color::new([0.0, 0.0, 0.0, 1.0])),
        xform,
        pick,
    };
    draw_polygon_fill_and_stroke(scene, ctx, &offset_rings, &spec);

    if ribbon_mode {
        if let Some(_sc) = stroke_color {
            let linewidth_px = pt_to_px(linewidth_pt, ctx.dpi);
            if linewidth_px.is_finite() && linewidth_px > 0.0 {
                let opts = RibbonOptions {
                    half_width: 0.0,
                    cap,
                    join,
                    miter_limit: MITER_LIMIT,
                };
                for (r, ring) in offset_rings.iter().enumerate() {
                    if ring.len() < 3 {
                        continue;
                    }
                    let widths = &rings_widths[r];
                    let colors = &rings_colors[r];
                    let mesh = polygon_ribbon_full(ring, Some(colors), Some(widths), &opts);
                    scene.draw_mesh(&mesh, xform, pick);
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
    use crate::plot::value::Value;
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

    // ─── Phase E.5: ribbon-mode outline tests ─────────────────────────────

    fn stroke_count(scene: &RecordingScene) -> usize {
        scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Stroke { .. }))
            .count()
    }

    fn mesh_count(scene: &RecordingScene) -> usize {
        scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::DrawMesh { .. }))
            .count()
    }

    #[test]
    fn polygon_constant_outline_stays_on_stroke_path() {
        let mut g = PolygonGeom::builder()
            .set("x", Raw(vec![0.0_f64, 1.0, 1.0, 0.0]))
            .set("y", Raw(vec![0.0_f64, 0.0, 1.0, 1.0]))
            .set("fill", red())
            .set("stroke", red())
            .set("linewidth", 2.0_f64)
            .build();
        g.rebuild_diff_against_previous();
        let r = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &r, &scales),
        );
        // One fill (interior) + one stroke (outline) → no mesh.
        assert_eq!(mesh_count(&scene), 0);
        assert_eq!(stroke_count(&scene), 1);
    }

    #[test]
    fn polygon_varying_linewidth_upgrades_outline_to_mesh() {
        let mut g = PolygonGeom::builder()
            .set("x", Raw(vec![0.0_f64, 1.0, 1.0, 0.0]))
            .set("y", Raw(vec![0.0_f64, 0.0, 1.0, 1.0]))
            .set("fill", red())
            .set("stroke", red())
            .set("linewidth", vec![2.0_f64, 6.0, 2.0, 6.0])
            .build();
        g.rebuild_diff_against_previous();
        let r = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &r, &scales),
        );
        // Fill stays as Op::Fill; outline upgraded to mesh.
        assert_eq!(stroke_count(&scene), 0);
        assert_eq!(mesh_count(&scene), 1);
    }

    #[test]
    fn polygon_varying_stroke_color_upgrades_outline() {
        let red_c = Color::new([1.0, 0.0, 0.0, 1.0]);
        let blue_c = Color::new([0.0, 0.0, 1.0, 1.0]);
        let mut g = PolygonGeom::builder()
            .set("x", Raw(vec![0.0_f64, 1.0, 1.0, 0.0]))
            .set("y", Raw(vec![0.0_f64, 0.0, 1.0, 1.0]))
            .set("fill", red_c)
            .set("stroke", vec![red_c, blue_c, red_c, blue_c])
            .set("linewidth", 2.0_f64)
            .build();
        g.rebuild_diff_against_previous();
        let r = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &r, &scales),
        );
        assert_eq!(stroke_count(&scene), 0);
        assert_eq!(mesh_count(&scene), 1);
    }

    #[test]
    fn polygon_expand_blocks_ribbon_upgrade() {
        let mut g = PolygonGeom::builder()
            .set("x", Raw(vec![0.0_f64, 1.0, 1.0, 0.0]))
            .set("y", Raw(vec![0.0_f64, 0.0, 1.0, 1.0]))
            .set("fill", red())
            .set("stroke", red())
            .set("linewidth", vec![2.0_f64, 6.0, 2.0, 6.0])
            .set("expand", 2.0_f64)
            .build();
        g.rebuild_diff_against_previous();
        let r = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &r, &scales),
        );
        assert_eq!(stroke_count(&scene), 1);
        assert_eq!(mesh_count(&scene), 0);
    }

    #[test]
    fn polygon_ribbon_does_not_affect_fill() {
        let mut g = PolygonGeom::builder()
            .set("x", Raw(vec![0.0_f64, 1.0, 1.0, 0.0]))
            .set("y", Raw(vec![0.0_f64, 0.0, 1.0, 1.0]))
            .set("fill", red())
            .set("stroke", red())
            .set("linewidth", vec![2.0_f64, 6.0, 2.0, 6.0])
            .build();
        g.rebuild_diff_against_previous();
        let r = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &r, &scales),
        );
        // Exactly one fill op (the interior).
        let fills = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Fill { .. }))
            .count();
        assert_eq!(fills, 1);
    }
}
