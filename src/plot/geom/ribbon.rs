//! `RibbonGeom` — filled band between two curves.
//!
//! Per-mark like [`LineGeom`](super::LineGeom): rows sharing a key value
//! form one band. Within a mark, the geom walks the rows in source order
//! to produce two curves (call them curve A and curve B), then builds a
//! closed contour — forward along curve A, back along curve B — and
//! fills it.
//!
//! Three orientations live in the same struct, selected by which
//! optional channels the user supplies:
//!
//! - **Horizontal band** (band varies along x). Channels `x`, `y`, `y2`.
//!   Curve A is `(x, y)`; curve B is `(x, y2)`. Selected when `y2` is
//!   supplied and `x2` is not.
//! - **Vertical band** (band varies along y). Channels `x`, `y`, `x2`.
//!   Curve A is `(x, y)`; curve B is `(x2, y)`. Selected when `x2` is
//!   supplied and `y2` is not.
//! - **Free band** (both edges arbitrary). Channels `x`, `y`, `x2`, `y2`.
//!   Curve A is `(x, y)`; curve B is `(x2, y2)`. Selected when both
//!   optional channels are supplied. The fill polygon closes curve A's
//!   last vertex to curve B's last vertex (and curve B's first to
//!   curve A's first) with straight segments.
//!
//! At least one of `x2` / `y2` must be supplied — a ribbon needs both
//! edges. To get a band from `(x, y)` to a baseline, supply
//! `y2 = 0.0` (or `x2 = 0.0`) explicitly.
//!
//! Channels consumed:
//!
//! - `"x"` — required; data; numeric.
//! - `"y"` — required; data; numeric.
//! - `"x2"` — optional; data; numeric.
//! - `"y2"` — optional; data; numeric.
//! - `"fill"` — band fill color (per-mark, but read at every row when
//!   the channel varies across the mark — see below). Default: none.
//! - `"alpha"` — overrides the alpha of `"fill"` (per-mark or per-row,
//!   same dispatch rule as `"fill"`). Folded in via
//!   [`override_alpha`](super::resolve::override_alpha).
//! - `"stroke"` — outline color for curve A (per-mark; first-row-of-mark).
//!   Bound → curve A is stroked; unbound → no outline on curve A.
//! - `"stroke2"` — outline color for curve B (per-mark). Independent of
//!   `"stroke"`; either, both, or neither may be bound.
//! - `"linewidth"` — width in pt of curve A's outline (per-mark;
//!   default 1.0 pt). Consulted only when `"stroke"` is bound.
//! - `"linewidth2"` — width in pt of curve B's outline (per-mark;
//!   default 1.0 pt). Consulted only when `"stroke2"` is bound.
//! - `"pick_id"` — per-mark pick id (resolved at the mark's first row).
//!
//! Variance in `"fill"` / `"alpha"` across a mark dispatches to one of
//! two render paths:
//!
//! - Axis-aligned (horizontal / vertical) **and** linear projection —
//!   linear gradient brush along the shared axis with one
//!   [`peniko::ColorStop`] per row. The fast path.
//! - Free orientation, **or** any orientation under a non-linear
//!   projection (e.g. polar) — quad-strip mesh between curve A and
//!   curve B with per-vertex colours, so the colour follows the
//!   band's actual sweep rather than a screen-aligned axis. Built via
//!   [`ribbon_band_mesh`](crate::primitives::ribbon_band_mesh).
//!
//! Uniform fill across a mark always renders as a single
//! [`Brush::Solid`] fill. Variance is detected via
//! [`channel_varies_across`](super::resolve::channel_varies_across) —
//! the same dispatcher [`LineGeom`](super::LineGeom) uses for its
//! ribbon-mesh outline upgrade.
//!
//! Non-finite rows (NaN in any required positional channel) are dropped
//! silently — the band closes around the remaining vertices, matching
//! [`PolygonGeom`](super::PolygonGeom) semantics. Rows within a mark
//! are drawn in user-supplied order; the geom does not sort.

use crate::brush::Brush;
use crate::color::Color;
use crate::geometry::{Affine, Point};
use crate::path::{FillRule, Path};
use crate::plot::diff::{diff_columns, diff_positional, KeyIndex};
use crate::plot::value::{DataColumn, Value};
use crate::scene::SceneBuilder;
use crate::stroke::Stroke;

use super::marks::{build_marks_from_column, MarkSlot};
use super::resolve::{
    channel_varies_across, override_alpha, pt_to_px, resolve_color_channel, resolve_number_channel,
    resolve_number_channel_or, resolve_pick_id, resolve_position,
};
use super::state::{
    filter_declared, require_data_column, validate_channel_lengths, validate_pick_id_channel,
    GeomState, KeysStrategy,
};
use super::{
    empty_datacolumn_like, BuildableGeom, Channel, ExpectedOutput, Geom, GeomBuilder, GeomContext,
    Keys,
};

const DEFAULT_LINEWIDTH_PT: f64 = 1.0;

/// Catalog of channels this geom recognises, with their expected scale
/// output type.
const CHANNELS: &[(&str, ExpectedOutput)] = &[
    ("x", ExpectedOutput::Numbers),
    ("y", ExpectedOutput::Numbers),
    ("x2", ExpectedOutput::Numbers),
    ("y2", ExpectedOutput::Numbers),
    ("fill", ExpectedOutput::Colors),
    ("alpha", ExpectedOutput::Numbers),
    ("stroke", ExpectedOutput::Colors),
    ("stroke2", ExpectedOutput::Colors),
    ("linewidth", ExpectedOutput::Numbers),
    ("linewidth2", ExpectedOutput::Numbers),
    ("pick_id", ExpectedOutput::Numbers),
];

/// Which optional channels supply curve B, and therefore how the
/// band relates to the panel axes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Orientation {
    /// Band sweeps along x; curve A is `(x, y)`, curve B is `(x, y2)`.
    Horizontal,
    /// Band sweeps along y; curve A is `(x, y)`, curve B is `(x2, y)`.
    Vertical,
    /// Both edges independent; curve A is `(x, y)`, curve B is
    /// `(x2, y2)`. No shared axis.
    Free,
}

/// A vectorised filled-band geom.
///
/// See the module-level docs for the channel set and the
/// horizontal-vs-vertical orientation rule.
pub struct RibbonGeom {
    pub(crate) state: GeomState,
    /// Cached mark layout — rebuilt at the start of each `draw` /
    /// `rebuild_diff_against_previous`.
    pub(crate) marks: Vec<MarkSlot>,
    /// Selected from the channel set at `build_from` time.
    pub(crate) orientation: Orientation,
}

crate::impl_geom_inherents_grouped!(RibbonGeom);

impl RibbonGeom {
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
            _ => panic!("RibbonGeom: unique-keys column variant mismatch"),
        }
    }
    template
}

// ─── BuildableGeom impl ──────────────────────────────────────────────────────

impl BuildableGeom for RibbonGeom {
    fn build_from(builder: GeomBuilder<Self>) -> Self {
        let (keys_opt, channels) = builder.into_parts();

        let n = require_data_column("x", &channels, "RibbonGeom").len();
        let y_len = require_data_column("y", &channels, "RibbonGeom").len();
        if y_len != n {
            panic!("RibbonGeom::build: \"y\" length {y_len} does not match \"x\" length {n}");
        }

        let has_x2 = channels.contains_key("x2");
        let has_y2 = channels.contains_key("y2");
        let orientation = match (has_x2, has_y2) {
            (false, false) => panic!(
                "RibbonGeom::build: needs at least one of \"x2\" or \"y2\" \
                 (use a constant baseline, e.g. y2 = 0.0, for an area-to-axis ribbon)"
            ),
            (true, false) => Orientation::Vertical,
            (false, true) => Orientation::Horizontal,
            (true, true) => Orientation::Free,
        };

        validate_channel_lengths(&channels, n, "RibbonGeom");
        validate_pick_id_channel(&channels, "RibbonGeom");

        let declared = filter_declared(&channels, CHANNELS);
        let state = GeomState::from_builder(keys_opt, channels, n, KeysStrategy::OneMark, declared);
        RibbonGeom {
            state,
            marks: Vec::new(),
            orientation,
        }
    }
}

// ─── Geom impl ───────────────────────────────────────────────────────────────

impl Geom for RibbonGeom {
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

    fn invalidate_caches(&mut self) {
        self.marks.clear();
    }

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
        let x2_scale_bound = ctx.scale_for("x2");
        let y2_scale_bound = ctx.scale_for("y2");
        let fill_scale = ctx.scale_for("fill");
        let alpha_scale = ctx.scale_for("alpha");
        let stroke_scale = ctx.scale_for("stroke");
        let stroke2_scale = ctx.scale_for("stroke2");
        let linewidth_scale = ctx.scale_for("linewidth");
        let linewidth2_scale = ctx.scale_for("linewidth2");
        let pick_id_scale = ctx.scale_for("pick_id");

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
        let alpha_ch = channels.get("alpha");
        let stroke_ch = channels.get("stroke");
        let stroke2_ch = channels.get("stroke2");
        let linewidth_ch = channels.get("linewidth");
        let linewidth2_ch = channels.get("linewidth2");
        let pick_id_ch = channels.get("pick_id");

        // Curve-B inputs. Horizontal: B = `(x, y2)`. Vertical:
        // B = `(x2, y)`. Free: B = `(x2, y2)`. The unused channel in
        // the axis-aligned cases falls back to the corresponding curve-A
        // channel for that row, so the per-row code only branches on
        // which of x2 / y2 was actually supplied.
        let x2_ch = channels.get("x2");
        let y2_ch = channels.get("y2");

        for mark in marks.iter() {
            let i0 = mark.first_row;

            // Per-mark fill colour at first row, blended with per-mark
            // alpha. Used for both the uniform `Brush::Solid` path and as
            // a fallback colour when building gradient stops if a row's
            // own fill is unresolved.
            let mark_fill = override_alpha(
                resolve_color_channel(fill_ch, fill_scale, i0),
                resolve_number_channel(alpha_ch, alpha_scale, i0),
            );
            let mark_stroke = override_alpha(
                resolve_color_channel(stroke_ch, stroke_scale, i0),
                resolve_number_channel(alpha_ch, alpha_scale, i0),
            );
            let mark_stroke2 = override_alpha(
                resolve_color_channel(stroke2_ch, stroke2_scale, i0),
                resolve_number_channel(alpha_ch, alpha_scale, i0),
            );

            // If nothing to draw (no fill, no stroke on either curve)
            // skip the whole mark.
            if mark_fill.is_none() && mark_stroke.is_none() && mark_stroke2.is_none() {
                continue;
            }

            // ── Build the two curves vertex-by-vertex. ──
            //
            // For each row we project two channel-space points to panel
            // pixels: curve-A vertex from the unprimed `(x, y)` pair, and
            // curve-B vertex from `(x2_or_x, y2_or_y)` based on which
            // optional channels were supplied. Under non-linear
            // projections (polar, future ternary) we densify each edge
            // between consecutive rows via `interpolate_segment_with_t`
            // on whichever curve has the higher chord error, then resample
            // the *other* curve at the same channel-space `t` values so
            // `curve_a_pts.len() == curve_b_pts.len()` — required by the
            // mesh dispatch and harmless to the path dispatch.
            //
            // `vertex_origins` carries a per-vertex bracketing-row /
            // interior-t tag so per-vertex colours can be lerped between
            // the bracketing rows for the mesh path.
            let is_linear = ctx.projection.is_linear();
            let mut samples_a: Vec<crate::plot::projection::InteriorSample> = Vec::new();
            let mut samples_b: Vec<crate::plot::projection::InteriorSample> = Vec::new();
            let mut merged_t: Vec<f64> = Vec::new();

            let mut curve_a_pts: Vec<Point> = Vec::with_capacity(mark.rows.len());
            let mut curve_b_pts: Vec<Point> = Vec::with_capacity(mark.rows.len());
            let mut row_for_vertex: Vec<usize> = Vec::with_capacity(mark.rows.len());
            let mut vertex_origins: Vec<VertexOrigin> = Vec::with_capacity(mark.rows.len());
            let mut prev_real: Option<(usize, [f64; 2], [f64; 2])> = None;

            for &i in &mark.rows {
                let x_frac = resolve_position(x_col.get(i), x_scale, 0.0);
                let y_frac = resolve_position(y_col.get(i), y_scale, 0.0);
                if !x_frac.is_finite() || !y_frac.is_finite() {
                    continue;
                }
                let (b_x_frac, b_y_frac) = match resolve_b_row(
                    self.orientation,
                    x2_ch,
                    y2_ch,
                    x2_scale_bound,
                    y2_scale_bound,
                    i,
                    x_frac,
                    y_frac,
                ) {
                    Some(b) => b,
                    None => continue,
                };

                let a_ch = [x_frac, y_frac];
                let b_ch = [b_x_frac, b_y_frac];

                if !is_linear {
                    if let Some((prev_row, prev_a, prev_b)) = prev_real {
                        samples_a.clear();
                        samples_b.clear();
                        ctx.projection.interpolate_segment_with_t(
                            panel,
                            &prev_a,
                            &a_ch,
                            &mut samples_a,
                        );
                        ctx.projection.interpolate_segment_with_t(
                            panel,
                            &prev_b,
                            &b_ch,
                            &mut samples_b,
                        );
                        merged_t.clear();
                        merged_t.extend(samples_a.iter().map(|s| s.t));
                        merged_t.extend(samples_b.iter().map(|s| s.t));
                        merged_t
                            .sort_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));
                        merged_t.dedup_by(|x, y| (*x - *y).abs() < 1e-9);
                        for &t in &merged_t {
                            let a_lerp = [
                                (1.0 - t) * prev_a[0] + t * a_ch[0],
                                (1.0 - t) * prev_a[1] + t * a_ch[1],
                            ];
                            let b_lerp = [
                                (1.0 - t) * prev_b[0] + t * b_ch[0],
                                (1.0 - t) * prev_b[1] + t * b_ch[1],
                            ];
                            let (apx, apy) = ctx.projection.project_to_panel_px(panel, &a_lerp);
                            let (bpx, bpy) = ctx.projection.project_to_panel_px(panel, &b_lerp);
                            curve_a_pts.push(Point::new(apx, apy));
                            curve_b_pts.push(Point::new(bpx, bpy));
                            vertex_origins.push(VertexOrigin {
                                prev_row,
                                next_row: i,
                                t,
                            });
                        }
                    }
                }

                let (apx, apy) = ctx.projection.project_to_panel_px(panel, &a_ch);
                let (bpx, bpy) = ctx.projection.project_to_panel_px(panel, &b_ch);
                curve_a_pts.push(Point::new(apx, apy));
                curve_b_pts.push(Point::new(bpx, bpy));
                row_for_vertex.push(i);
                vertex_origins.push(VertexOrigin {
                    prev_row: i,
                    next_row: i,
                    t: 1.0,
                });
                prev_real = Some((i, a_ch, b_ch));
            }

            if row_for_vertex.len() < 2 {
                // A degenerate single-row band has no area.
                continue;
            }
            debug_assert_eq!(curve_a_pts.len(), curve_b_pts.len());
            debug_assert_eq!(curve_a_pts.len(), vertex_origins.len());

            // Build the closed fill contour: forward along curve A, then
            // back along curve B (reversed), then close.
            let mut path = Path::new();
            path.move_to(curve_a_pts[0]);
            for p in &curve_a_pts[1..] {
                path.line_to(*p);
            }
            for p in curve_b_pts.iter().rev() {
                path.line_to(*p);
            }
            path.close_path();

            let pick = resolve_pick_id(pick_id_ch, pick_id_scale, i0);

            // ── Fill dispatch (variance-detect). ──
            //
            // Solid fill (or no variation) — single `Brush::Solid` over
            // the closed contour path. Variation under axis-aligned +
            // linear projection — linear gradient brush along the shared
            // axis (the fast path). Variation under Free orientation or
            // any non-linear projection — quad-strip mesh between the
            // two curves with per-vertex colours, so the gradient
            // follows the band's actual sweep instead of a screen-aligned
            // axis.
            if let Some(mark_color) = mark_fill {
                let varies = channel_varies_across(fill_ch, fill_scale, &row_for_vertex)
                    || channel_varies_across(alpha_ch, alpha_scale, &row_for_vertex);
                let axis_aligned = matches!(
                    self.orientation,
                    Orientation::Horizontal | Orientation::Vertical
                );
                let use_mesh = varies && (!axis_aligned || !is_linear);

                if use_mesh {
                    let (colors_a, colors_b) = build_per_vertex_colors(
                        &vertex_origins,
                        fill_ch,
                        fill_scale,
                        alpha_ch,
                        alpha_scale,
                        mark_color,
                    );
                    let mesh = crate::primitives::ribbon_band_mesh(
                        &curve_a_pts,
                        &curve_b_pts,
                        &colors_a,
                        &colors_b,
                    );
                    if !mesh.is_empty() {
                        scene.draw_mesh(&mesh, Affine::IDENTITY, pick);
                    }
                } else {
                    let brush = if varies {
                        build_gradient_brush(
                            self.orientation,
                            &curve_a_pts,
                            &row_for_vertex,
                            curve_a_pts.len() - row_for_vertex.len(),
                            fill_ch,
                            fill_scale,
                            alpha_ch,
                            alpha_scale,
                            mark_color,
                        )
                        .map(Brush::Gradient)
                        .unwrap_or_else(|| Brush::Solid(mark_color))
                    } else {
                        Brush::Solid(mark_color)
                    };
                    scene.fill(
                        FillRule::NonZero,
                        Affine::IDENTITY,
                        &brush,
                        None,
                        &path,
                        pick,
                    );
                }
            }

            // ── Curve A outline (independent of the fill). ──
            if let Some(sc) = mark_stroke {
                let linewidth_pt = resolve_number_channel_or(
                    linewidth_ch,
                    linewidth_scale,
                    i0,
                    DEFAULT_LINEWIDTH_PT,
                );
                let linewidth_px = pt_to_px(linewidth_pt, ctx.dpi);
                if linewidth_px.is_finite() && linewidth_px > 0.0 {
                    let mut a_path = Path::new();
                    a_path.move_to(curve_a_pts[0]);
                    for p in &curve_a_pts[1..] {
                        a_path.line_to(*p);
                    }
                    let stroke_spec = Stroke::new(linewidth_px);
                    scene.stroke(
                        &stroke_spec,
                        Affine::IDENTITY,
                        &Brush::Solid(sc),
                        None,
                        &a_path,
                        pick,
                    );
                }
            }

            // ── Curve B outline (independent of curve A's). ──
            if let Some(sc) = mark_stroke2 {
                let linewidth_pt = resolve_number_channel_or(
                    linewidth2_ch,
                    linewidth2_scale,
                    i0,
                    DEFAULT_LINEWIDTH_PT,
                );
                let linewidth_px = pt_to_px(linewidth_pt, ctx.dpi);
                if linewidth_px.is_finite() && linewidth_px > 0.0 {
                    let mut b_path = Path::new();
                    b_path.move_to(curve_b_pts[0]);
                    for p in &curve_b_pts[1..] {
                        b_path.line_to(*p);
                    }
                    let stroke_spec = Stroke::new(linewidth_px);
                    scene.stroke(
                        &stroke_spec,
                        Affine::IDENTITY,
                        &Brush::Solid(sc),
                        None,
                        &b_path,
                        pick,
                    );
                }
            }
        }
    }
}

/// Build a linear gradient brush along the band's shared axis from the
/// per-row vertex positions on curve A. Returns `None` if the gradient
/// would be degenerate (fewer than two stops with distinct offsets, or
/// zero shared-axis span).
///
/// Stops carry the per-row resolved fill (with per-row alpha folded in)
/// at offsets proportional to each vertex's projected position along
/// the shared axis. Densified interior points (added between rows under
/// polar projection) are skipped — only the real per-row vertices
/// contribute stops, since interior points have no row identity.
#[allow(clippy::too_many_arguments)]
fn build_gradient_brush(
    orientation: Orientation,
    curve_a_pts: &[Point],
    row_for_vertex: &[usize],
    _interior_count: usize,
    fill_ch: Option<&Channel>,
    fill_scale: Option<&crate::plot::scale::Scale>,
    alpha_ch: Option<&Channel>,
    alpha_scale: Option<&crate::plot::scale::Scale>,
    fallback: Color,
) -> Option<peniko::Gradient> {
    // Indices of curve_a_pts that correspond to real per-row vertices
    // (not densified interior). Walk curve_a_pts in order and pick out
    // every point whose forward position in the curve matches the next
    // expected row vertex. The simplest correct mapping: the real
    // vertices are emitted *after* each densified interior batch, so we
    // can identify them as the points at the cumulative positions that
    // correspond to where rows land. Easier: since `row_for_vertex` has
    // one entry per surviving row and curve_a_pts has those rows
    // interleaved with interior points appended in row order, the real
    // vertices are exactly the last points of each "run" — i.e. the
    // indices N - row_for_vertex.len() through N-1 are NOT correct under
    // densification. To make this robust, we scan from the back:
    // densified points are inserted *before* each successive row vertex,
    // so the last point is row N-1's vertex, the next-back is row
    // (N-2)'s vertex (after stepping back through that row's interior
    // points), and so on.
    //
    // Simpler approach: walk curve_a_pts with a parallel counter that
    // increments only when we land on a real vertex. We don't track
    // interior vs real explicitly during the build loop, so we identify
    // real vertices here by selecting the LAST point of each
    // interpolate_segment-extended row. This works because each row
    // appends (interior_batch ++ [real_vertex]) in order.
    //
    // The walk: real vertices are at indices where the cumulative
    // (interior + 1) counts roll over. We need either the interior count
    // per row (not tracked) or a per-vertex flag. Since the densified
    // points are *strictly between* two real vertices, the simplest
    // robust extraction is to assume curve_a_pts ends with the last real
    // vertex and step backwards by 1 + interior_count_for_row[i]. We
    // don't have per-row interior counts handy. Fallback: degrade
    // gradient stops to the row's projected position by walking
    // curve_a_pts and picking the LAST point per row marker.
    //
    // Implementation: emit stops only from indices that we know are real
    // vertices. To get that without tracking interior batches, we accept
    // the simplification that under linear projection (no interior
    // points) curve_a_pts.len() == row_for_vertex.len() — gradient
    // stops align trivially. Under polar, gradient brushes on bands are
    // a documented v1 limitation; we still produce a brush but stops
    // map to the last `row_for_vertex.len()` indices, which yields the
    // correct stops *for the row vertices* (other points use the
    // gradient evaluated at their projected position along the brush
    // line, which is what we want anyway).
    let n = row_for_vertex.len();
    if n < 2 {
        return None;
    }

    // Compute shared-axis range in panel pixels using only the real
    // per-row vertices. Under polar this isn't strictly an axis but the
    // gradient is still anchored screen-aligned by the band's pixel-space
    // extent along the corresponding axis. Free orientation is dispatched
    // to the mesh path elsewhere; if it ever reaches here, decline by
    // returning `None` so the caller falls back to a solid fill.
    let pick_coord = |p: &Point| match orientation {
        Orientation::Horizontal => p.x,
        Orientation::Vertical => p.y,
        Orientation::Free => 0.0,
    };
    if matches!(orientation, Orientation::Free) {
        return None;
    }
    let real_pts: Vec<Point> = if curve_a_pts.len() == n {
        curve_a_pts.to_vec()
    } else {
        // Densified path: real vertices are the last point of each row's
        // emitted batch. Each row appends interior_batch ++ [real]; the
        // interior_batch length depends on the projection. We can recover
        // real vertices by counting from the END: the final point IS the
        // last row's vertex; for earlier rows we don't have a clean
        // boundary. Practical fallback: subsample curve_a_pts uniformly
        // into n points. This is imperfect under heavy polar
        // densification but documented as a v1 limitation.
        let m = curve_a_pts.len();
        (0..n)
            .map(|k| {
                let idx = (k * (m - 1)) / (n - 1);
                curve_a_pts[idx]
            })
            .collect()
    };

    let coords: Vec<f64> = real_pts.iter().map(pick_coord).collect();
    let min_c = coords.iter().cloned().fold(f64::INFINITY, f64::min);
    let max_c = coords.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let span = max_c - min_c;
    if !span.is_finite() || span.abs() < f64::EPSILON {
        return None;
    }

    let (start, end) = match orientation {
        Orientation::Horizontal => {
            let mid_y = (real_pts[0].y + real_pts[n - 1].y) * 0.5;
            (Point::new(min_c, mid_y), Point::new(max_c, mid_y))
        }
        Orientation::Vertical => {
            let mid_x = (real_pts[0].x + real_pts[n - 1].x) * 0.5;
            (Point::new(mid_x, min_c), Point::new(mid_x, max_c))
        }
        Orientation::Free => return None,
    };

    // Build one stop per real vertex, sorted by gradient offset so peniko
    // sees a strictly-monotonic sequence. Projected coords don't follow
    // row order under cartesian (y axis is flipped) or under non-linear
    // projections in general — sort before deduping.
    let mut pairs: Vec<(f64, Color)> = Vec::with_capacity(n);
    for (k, &i) in row_for_vertex.iter().enumerate() {
        let offset = ((coords[k] - min_c) / span).clamp(0.0, 1.0);
        let row_color = override_alpha(
            resolve_color_channel(fill_ch, fill_scale, i),
            resolve_number_channel(alpha_ch, alpha_scale, i),
        )
        .unwrap_or(fallback);
        pairs.push((offset, row_color));
    }
    pairs.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    let mut stops: Vec<peniko::ColorStop> = Vec::with_capacity(pairs.len());
    let mut last_offset = f64::NEG_INFINITY;
    for (offset, color) in pairs {
        if offset <= last_offset {
            continue;
        }
        stops.push(peniko::ColorStop {
            offset: offset as f32,
            color: color.into(),
        });
        last_offset = offset;
    }
    if stops.len() < 2 {
        return None;
    }
    Some(peniko::Gradient::new_linear(start, end).with_stops(stops.as_slice()))
}

/// Bracketing-row identity for a single emitted curve vertex. Real
/// vertices set `prev_row == next_row`; densified interior vertices
/// carry the two bracketing row indices and the `t ∈ (0, 1)` fraction
/// of the segment between them.
#[derive(Clone, Copy, Debug)]
struct VertexOrigin {
    prev_row: usize,
    next_row: usize,
    /// Channel-space fraction within the bracketing segment. `1.0` for
    /// real vertices (and ignored, since `prev_row == next_row`).
    t: f64,
}

/// Resolve curve B's `(x, y)` panel-fraction for one row. Returns
/// `None` when either coordinate is non-finite (the row is dropped).
/// Falls back to the corresponding curve-A coordinate for the channel
/// that wasn't supplied in axis-aligned modes.
#[allow(clippy::too_many_arguments)]
fn resolve_b_row(
    orientation: Orientation,
    x2_ch: Option<&Channel>,
    y2_ch: Option<&Channel>,
    x2_scale_bound: Option<&crate::plot::scale::Scale>,
    y2_scale_bound: Option<&crate::plot::scale::Scale>,
    row: usize,
    x_frac: f64,
    y_frac: f64,
) -> Option<(f64, f64)> {
    let b_x = match orientation {
        Orientation::Horizontal => x_frac,
        Orientation::Vertical | Orientation::Free => {
            resolve_optional_position(x2_ch, x2_scale_bound, row)?
        }
    };
    let b_y = match orientation {
        Orientation::Vertical => y_frac,
        Orientation::Horizontal | Orientation::Free => {
            resolve_optional_position(y2_ch, y2_scale_bound, row)?
        }
    };
    Some((b_x, b_y))
}

fn resolve_optional_position(
    ch: Option<&Channel>,
    scale_bound: Option<&crate::plot::scale::Scale>,
    row: usize,
) -> Option<f64> {
    let value = match ch? {
        Channel::Constant(v) | Channel::RawConstant(v) => v.clone(),
        Channel::Data(col) | Channel::RawData(col) => col.get(row),
    };
    let scale = match ch? {
        Channel::RawConstant(_) | Channel::RawData(_) => None,
        _ => scale_bound,
    };
    let f = resolve_position(value, scale, 0.0);
    if f.is_finite() {
        Some(f)
    } else {
        None
    }
}

/// Build per-vertex colours for both curve sides of the mesh path.
/// Real vertices take the per-row resolved fill (with per-row alpha
/// folded in); densified interior vertices lerp linearly between the
/// two bracketing rows' colours along `t`. Both sides share the same
/// colour at the same vertex index — the ribbon has one fill per
/// vertex pair.
fn build_per_vertex_colors(
    vertex_origins: &[VertexOrigin],
    fill_ch: Option<&Channel>,
    fill_scale: Option<&crate::plot::scale::Scale>,
    alpha_ch: Option<&Channel>,
    alpha_scale: Option<&crate::plot::scale::Scale>,
    fallback: Color,
) -> (Vec<Color>, Vec<Color>) {
    let resolve_row = |row: usize| -> Color {
        override_alpha(
            resolve_color_channel(fill_ch, fill_scale, row),
            resolve_number_channel(alpha_ch, alpha_scale, row),
        )
        .unwrap_or(fallback)
    };
    let mut colors: Vec<Color> = Vec::with_capacity(vertex_origins.len());
    for origin in vertex_origins {
        let c = if origin.prev_row == origin.next_row {
            resolve_row(origin.prev_row)
        } else {
            let prev = resolve_row(origin.prev_row);
            let next = resolve_row(origin.next_row);
            crate::color::lerp_color(prev, next, origin.t)
        };
        colors.push(c);
    }
    // Both curve sides share the same per-row fill in the ribbon model;
    // clone once rather than re-resolving.
    let colors_b = colors.clone();
    (colors, colors_b)
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
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

    fn blue() -> Color {
        Color::new([0.0, 0.0, 1.0, 1.0])
    }

    // ── build() ──

    #[test]
    fn no_keys_synthesises_single_mark() {
        let g = RibbonGeom::builder()
            .set("x", vec![0.0_f64, 1.0, 2.0])
            .set("y", vec![1.0_f64, 2.0, 1.0])
            .set("y2", 0.0_f64)
            .build();
        assert_eq!(g.len(), 3);
        assert_eq!(g.mark_count(), 1);
    }

    #[test]
    fn explicit_keys_define_marks() {
        let g = RibbonGeom::builder()
            .keys(vec!["A", "A", "A", "B", "B", "B"])
            .set("x", vec![0.0_f64, 1.0, 2.0, 0.0, 1.0, 2.0])
            .set("y", vec![1.0_f64, 2.0, 1.0, 0.5, 1.5, 0.5])
            .set("y2", 0.0_f64)
            .build();
        assert_eq!(g.mark_count(), 2);
    }

    #[test]
    fn explicit_y2_selects_horizontal() {
        let g = RibbonGeom::builder()
            .set("x", vec![0.0_f64, 1.0, 2.0])
            .set("y", vec![1.0_f64, 2.0, 1.0])
            .set("y2", vec![0.2_f64, 0.4, 0.3])
            .build();
        assert_eq!(g.orientation, Orientation::Horizontal);
    }

    #[test]
    fn x2_selects_vertical_mode() {
        let g = RibbonGeom::builder()
            .set("x", vec![0.0_f64, 0.5, 1.0])
            .set("y", vec![0.0_f64, 0.5, 1.0])
            .set("x2", vec![0.2_f64, 0.7, 1.2])
            .build();
        assert_eq!(g.orientation, Orientation::Vertical);
    }

    #[test]
    fn both_x2_and_y2_selects_free() {
        let g = RibbonGeom::builder()
            .set("x", vec![0.0_f64, 1.0])
            .set("y", vec![0.0_f64, 1.0])
            .set("x2", vec![0.2_f64, 0.8])
            .set("y2", vec![0.2_f64, 0.8])
            .build();
        assert_eq!(g.orientation, Orientation::Free);
    }

    #[test]
    #[should_panic(expected = "needs at least one")]
    fn no_curve_b_channel_panics() {
        RibbonGeom::builder()
            .set("x", vec![0.0_f64, 1.0])
            .set("y", vec![0.0_f64, 1.0])
            .build();
    }

    #[test]
    #[should_panic(expected = "missing required channel")]
    fn missing_x_panics() {
        RibbonGeom::builder().set("y", vec![0.0_f64, 1.0]).build();
    }

    #[test]
    #[should_panic(expected = "missing required channel")]
    fn missing_y_panics() {
        RibbonGeom::builder().set("x", vec![0.0_f64, 1.0]).build();
    }

    #[test]
    #[should_panic(expected = "does not match")]
    fn length_mismatch_panics() {
        RibbonGeom::builder()
            .set("x", vec![0.0_f64, 1.0, 2.0])
            .set("y", vec![1.0_f64, 2.0])
            .build();
    }

    // ── Drawing ──

    fn draw_and_record(mut g: RibbonGeom) -> RecordingScene {
        g.rebuild_diff_against_previous();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales),
        );
        scene
    }

    #[test]
    fn constant_fill_uses_solid_brush() {
        let g = RibbonGeom::builder()
            .set("x", Raw(vec![0.1_f64, 0.5, 0.9]))
            .set("y", Raw(vec![0.5_f64, 0.7, 0.5]))
            .set("y2", Raw(0.2_f64))
            .set("fill", red())
            .build();
        let scene = draw_and_record(g);
        let solid_fills = scene
            .ops
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    Op::Fill {
                        brush: Brush::Solid(_),
                        ..
                    }
                )
            })
            .count();
        let gradient_fills = scene
            .ops
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    Op::Fill {
                        brush: Brush::Gradient(_),
                        ..
                    }
                )
            })
            .count();
        assert_eq!(solid_fills, 1);
        assert_eq!(gradient_fills, 0);
    }

    #[test]
    fn varying_fill_uses_gradient_brush_horizontal() {
        let g = RibbonGeom::builder()
            .set("x", Raw(vec![0.1_f64, 0.5, 0.9]))
            .set("y", Raw(vec![0.5_f64, 0.7, 0.5]))
            .set("y2", Raw(0.2_f64))
            .set("fill", vec![red(), blue(), red()])
            .build();
        let scene = draw_and_record(g);
        for op in &scene.ops {
            if let Op::Fill {
                brush: Brush::Gradient(g),
                ..
            } = op
            {
                // Linear horizontal gradient: start and end share a y.
                if let peniko::GradientKind::Linear(peniko::LinearGradientPosition { start, end }) =
                    g.kind
                {
                    assert!((start.y - end.y).abs() < f64::EPSILON);
                    assert!(start.x < end.x);
                } else {
                    panic!("expected linear gradient");
                }
                assert!(g.stops.len() >= 2);
                return;
            }
        }
        panic!("no gradient fill emitted");
    }

    #[test]
    fn varying_fill_uses_gradient_brush_vertical() {
        let g = RibbonGeom::builder()
            .set("x", Raw(vec![0.5_f64, 0.7, 0.5]))
            .set("y", Raw(vec![0.1_f64, 0.5, 0.9]))
            .set("x2", Raw(vec![0.3_f64, 0.3, 0.3]))
            .set("fill", vec![red(), blue(), red()])
            .build();
        let scene = draw_and_record(g);
        for op in &scene.ops {
            if let Op::Fill {
                brush: Brush::Gradient(g),
                ..
            } = op
            {
                // Linear vertical gradient: start and end share an x.
                if let peniko::GradientKind::Linear(peniko::LinearGradientPosition { start, end }) =
                    g.kind
                {
                    assert!((start.x - end.x).abs() < f64::EPSILON);
                    assert!(start.y < end.y);
                } else {
                    panic!("expected linear gradient");
                }
                return;
            }
        }
        panic!("no gradient fill emitted");
    }

    #[test]
    fn stroke_only_curve_a() {
        let g = RibbonGeom::builder()
            .set("x", Raw(vec![0.1_f64, 0.5, 0.9]))
            .set("y", Raw(vec![0.5_f64, 0.7, 0.5]))
            .set("y2", Raw(0.2_f64))
            .set("stroke", red())
            .set("linewidth", 2.0_f64)
            .build();
        let scene = draw_and_record(g);
        let strokes: Vec<&Op> = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Stroke { .. }))
            .collect();
        assert_eq!(strokes.len(), 1);
    }

    #[test]
    fn stroke_only_curve_b() {
        let g = RibbonGeom::builder()
            .set("x", Raw(vec![0.1_f64, 0.5, 0.9]))
            .set("y", Raw(vec![0.5_f64, 0.7, 0.5]))
            .set("y2", Raw(0.2_f64))
            .set("stroke2", blue())
            .set("linewidth2", 2.0_f64)
            .build();
        let scene = draw_and_record(g);
        let strokes: Vec<&Op> = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Stroke { .. }))
            .collect();
        assert_eq!(strokes.len(), 1);
    }

    #[test]
    fn stroke_both_curves() {
        let g = RibbonGeom::builder()
            .set("x", Raw(vec![0.1_f64, 0.5, 0.9]))
            .set("y", Raw(vec![0.5_f64, 0.7, 0.5]))
            .set("y2", Raw(vec![0.2_f64, 0.3, 0.2]))
            .set("stroke", red())
            .set("stroke2", blue())
            .set("linewidth", 2.0_f64)
            .set("linewidth2", 2.0_f64)
            .build();
        let scene = draw_and_record(g);
        let strokes: Vec<&Op> = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Stroke { .. }))
            .collect();
        assert_eq!(strokes.len(), 2);
    }

    #[test]
    fn no_fill_no_stroke_emits_nothing() {
        let g = RibbonGeom::builder()
            .set("x", Raw(vec![0.1_f64, 0.5, 0.9]))
            .set("y", Raw(vec![0.5_f64, 0.7, 0.5]))
            .set("y2", Raw(0.2_f64))
            .build();
        let scene = draw_and_record(g);
        assert!(scene.ops.is_empty());
    }

    #[test]
    fn nonfinite_row_dropped() {
        // A 4-row mark with one NaN row → remaining 3 vertices form
        // a valid closed band.
        let g = RibbonGeom::builder()
            .set("x", Raw(vec![0.1_f64, 0.4, 0.7, 0.9]))
            .set("y", Raw(vec![0.5_f64, f64::NAN, 0.8, 0.5]))
            .set("y2", Raw(0.2_f64))
            .set("fill", red())
            .build();
        let scene = draw_and_record(g);
        let fills = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Fill { .. }))
            .count();
        assert_eq!(fills, 1);
    }

    #[test]
    fn closed_contour_has_one_close() {
        let g = RibbonGeom::builder()
            .set("x", Raw(vec![0.1_f64, 0.5, 0.9]))
            .set("y", Raw(vec![0.5_f64, 0.7, 0.5]))
            .set("y2", Raw(0.2_f64))
            .set("fill", red())
            .build();
        let scene = draw_and_record(g);
        for op in &scene.ops {
            if let Op::Fill { path, .. } = op {
                let closes = path
                    .elements()
                    .iter()
                    .filter(|el| matches!(el, kurbo::PathEl::ClosePath))
                    .count();
                assert_eq!(closes, 1);
                return;
            }
        }
        panic!("no fill emitted");
    }

    #[test]
    fn fill_path_walks_a_forward_then_b_reversed() {
        // 3-row horizontal band with explicit y2 well below y. The fill
        // path should visit the three (x, y) vertices left-to-right then
        // the three (x, y2) vertices right-to-left.
        let g = RibbonGeom::builder()
            .set("x", Raw(vec![0.1_f64, 0.5, 0.9]))
            .set("y", Raw(vec![0.8_f64, 0.8, 0.8]))
            .set("y2", Raw(vec![0.2_f64, 0.2, 0.2]))
            .set("fill", red())
            .build();
        let scene = draw_and_record(g);
        for op in &scene.ops {
            if let Op::Fill { path, .. } = op {
                // First element is MoveTo at curve-A start. Last LineTo
                // before close is curve-B end (=row 0 in reversed order).
                let elements: Vec<_> = path.elements().iter().collect();
                if let kurbo::PathEl::MoveTo(start) = &elements[0] {
                    // y=0.8 on a 100×100 panel under default cartesian
                    // projection projects to y_px = panel.y1 - 0.8 * h =
                    // 100 - 80 = 20.
                    assert!((start.y - 20.0).abs() < 1.0);
                    assert!((start.x - 10.0).abs() < 1.0);
                } else {
                    panic!("first element not MoveTo");
                }
                return;
            }
        }
        panic!("no fill emitted");
    }

    #[test]
    fn declared_channels_alphabetical() {
        let g = RibbonGeom::builder()
            .set("x", vec![0.0_f64, 1.0, 2.0])
            .set("y", vec![1.0_f64, 2.0, 1.0])
            .set("y2", 0.0_f64)
            .set("fill", red())
            .build();
        let names: Vec<&str> = g.declared_channels().iter().map(|d| d.name).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
    }

    #[test]
    fn diff_marks_enter_on_first_draw() {
        let mut g = RibbonGeom::builder()
            .keys(vec!["A", "A", "A", "B", "B", "B"])
            .set("x", vec![0.0_f64, 1.0, 2.0, 0.0, 1.0, 2.0])
            .set("y", vec![1.0_f64, 2.0, 1.0, 0.5, 1.5, 0.5])
            .set("y2", 0.0_f64)
            .build();
        g.rebuild_diff_against_previous();
        assert_eq!(g.state.enter.len(), 2);
        assert_eq!(g.state.exit.len(), 0);
    }

    #[test]
    fn polar_band_densifies_edges() {
        use crate::plot::projection::Projection;
        let polar = Projection::polar();
        let mut g = RibbonGeom::builder()
            .set("x", Raw(vec![0.1_f64, 0.5, 0.9]))
            .set("y", Raw(vec![0.8_f64, 0.8, 0.8]))
            .set("y2", Raw(vec![0.4_f64, 0.4, 0.4]))
            .set("fill", red())
            .build();
        g.rebuild_diff_against_previous();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        let panel = Rect::new(0.0, 0.0, 200.0, 200.0);
        let ctx = GeomContext::with_projection(panel, 96.0, &shapes, &scales, &polar);
        g.draw(&mut scene, &ctx);

        // Under polar densification, each of the two angular edges
        // (curve A along outer radius, curve B along inner radius) gets
        // additional interior samples inserted by `interpolate_segment`.
        // Without densification a 3-row band would produce 6 line-to
        // elements (3 forward on A + 3 reversed on B); with curved arcs
        // we expect significantly more.
        for op in &scene.ops {
            if let Op::Fill { path, .. } = op {
                let line_count = path
                    .elements()
                    .iter()
                    .filter(|el| matches!(el, kurbo::PathEl::LineTo(_)))
                    .count();
                assert!(
                    line_count > 6,
                    "expected densified line count > 6, got {line_count}"
                );
                return;
            }
        }
        panic!("no fill emitted");
    }

    #[test]
    fn free_orientation_solid_fill_emits_path() {
        // Both x2 and y2 supplied with constant fill — still goes
        // through the closed-contour path fill, not the mesh.
        let g = RibbonGeom::builder()
            .set("x", Raw(vec![0.1_f64, 0.5, 0.9]))
            .set("y", Raw(vec![0.5_f64, 0.7, 0.5]))
            .set("x2", Raw(vec![0.1_f64, 0.5, 0.9]))
            .set("y2", Raw(vec![0.2_f64, 0.4, 0.2]))
            .set("fill", red())
            .build();
        assert_eq!(g.orientation, Orientation::Free);
        let scene = draw_and_record(g);
        let fills = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Fill { .. }))
            .count();
        let meshes = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::DrawMesh { .. }))
            .count();
        assert_eq!(fills, 1);
        assert_eq!(meshes, 0);
    }

    #[test]
    fn free_orientation_varying_fill_uses_mesh() {
        let g = RibbonGeom::builder()
            .set("x", Raw(vec![0.1_f64, 0.5, 0.9]))
            .set("y", Raw(vec![0.5_f64, 0.7, 0.5]))
            .set("x2", Raw(vec![0.1_f64, 0.5, 0.9]))
            .set("y2", Raw(vec![0.2_f64, 0.4, 0.2]))
            .set("fill", vec![red(), blue(), red()])
            .build();
        let scene = draw_and_record(g);
        let mesh_op = scene.ops.iter().find_map(|op| match op {
            Op::DrawMesh { mesh, .. } => Some(mesh),
            _ => None,
        });
        let mesh = mesh_op.expect("expected mesh draw for Free + varying fill");
        // Three rows → 2 quads → 4 triangles → 12 indices.
        assert_eq!(mesh.triangle_count(), 4);
        // Quad-pair canonical index pattern reaches the backend.
        assert_eq!(&mesh.indices[0..6], &[0, 1, 2, 0, 2, 3]);
    }

    #[test]
    fn axis_aligned_varying_fill_under_polar_uses_mesh() {
        use crate::plot::projection::Projection;
        let polar = Projection::polar();
        let mut g = RibbonGeom::builder()
            .set("x", Raw(vec![0.1_f64, 0.5, 0.9]))
            .set("y", Raw(vec![0.8_f64, 0.8, 0.8]))
            .set("y2", Raw(vec![0.4_f64, 0.4, 0.4]))
            .set("fill", vec![red(), blue(), red()])
            .build();
        g.rebuild_diff_against_previous();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        let panel = Rect::new(0.0, 0.0, 200.0, 200.0);
        let ctx = GeomContext::with_projection(panel, 96.0, &shapes, &scales, &polar);
        g.draw(&mut scene, &ctx);
        let meshes = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::DrawMesh { .. }))
            .count();
        let gradient_fills = scene
            .ops
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    Op::Fill {
                        brush: Brush::Gradient(_),
                        ..
                    }
                )
            })
            .count();
        assert_eq!(
            meshes, 1,
            "expected mesh dispatch under polar + varying fill"
        );
        assert_eq!(
            gradient_fills, 0,
            "gradient brush should not run under non-linear projection"
        );
    }

    #[test]
    fn pick_id_per_mark_resolves_from_first_row() {
        let g = RibbonGeom::builder()
            .keys(vec!["A", "A", "A", "B", "B", "B"])
            .set("x", Raw(vec![0.1_f64, 0.3, 0.5, 0.6, 0.7, 0.9]))
            .set("y", Raw(vec![0.5_f64, 0.7, 0.5, 0.4, 0.6, 0.4]))
            .set("y2", Raw(0.2_f64))
            .set("fill", red())
            .set("pick_id", vec![1001_i64, 0, 0, 2002, 0, 0])
            .build();
        let scene = draw_and_record(g);
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
        assert_eq!(picks, vec![1001, 2002]);
    }
}
