//! `RibbonBSplineGeom` — filled band between two clamped uniform-knot
//! B-spline curves.
//!
//! Per-mark like [`LineGeom`](super::LineGeom): rows sharing a key value
//! form one band. Within a mark the geom builds two control polygons
//! (curve A from `(x, y)`; curve B from one of `(x, y2)` / `(x2, y)` /
//! `(x2, y2)` depending on which of `x2` / `y2` are supplied) and runs
//! each through the same de Boor evaluator + adaptive chord-error
//! flattener that [`BSplineGeom`](super::BSplineGeom) uses, then
//! assembles a closed contour — forward along curve A, back along
//! curve B — and fills it.
//!
//! Three orientations live in the same struct, selected by which
//! optional channels the user supplies. The selection logic is
//! identical to [`RibbonGeom`](super::RibbonGeom):
//!
//! - **Horizontal band**: channels `x`, `y`, `y2`. Curve A is
//!   `(x, y)`; curve B is `(x, y2)`. Selected when `y2` is supplied
//!   and `x2` is not.
//! - **Vertical band**: channels `x`, `y`, `x2`. Curve A is `(x, y)`;
//!   curve B is `(x2, y)`. Selected when `x2` is supplied and `y2`
//!   is not.
//! - **Free band**: channels `x`, `y`, `x2`, `y2`. Curve A is
//!   `(x, y)`; curve B is `(x2, y2)`. Selected when both optional
//!   channels are supplied.
//!
//! The per-mark `degree` channel (default 3) and `interpolation`
//! channel (`"domain"` / `"panel"`; default `"domain"`) are shared
//! between both curves — both rows index the same per-mark
//! configuration, and the geom's clamped-knot construction puts
//! curve A's first/last data points and curve B's first/last data
//! points exactly on their respective curves regardless of orientation.
//!
//! ### Projection switch behaviour
//!
//! In `"domain"` mode each spline sample is projected individually,
//! so the curve follows the projection's geodesic faithfully (an arc
//! in polar, a chord in Cartesian). In `"panel"` mode the control
//! points are projected once and the spline is built in pixel space
//! between them — straight-chord interpolation between projected
//! vertices, no further projection densification along the spline.
//!
//! **Terminal connections are always densified.** The closing edges
//! that join curve A's first vertex to curve B's first vertex (and
//! curve A's last to curve B's last) are data-space connections,
//! independent of the spline interpolation mode. They run through
//! [`Projection::interpolate_segment_with_t`](crate::plot::projection::Projection::interpolate_segment_with_t)
//! so under polar with a cap that spans a non-trivial arc, the closing
//! edges follow the polar geodesic (visibly angular) even when the
//! spline portions chose `"panel"` mode and are pixel-space chords.
//! Under Cartesian the cap densification call is a no-op.
//!
//! ### Channels
//!
//! Positional and fill:
//!
//! - `"x"`, `"y"` — required; data; numeric. Curve A's control points.
//! - `"x2"`, `"y2"` — optional; data; numeric. At least one is
//!   required.
//! - `"degree"` — per-mark; numeric; default 3. Clamped to
//!   `min(degree, n_ctrl - 1)`. Groups with fewer than `degree + 1`
//!   control points degrade to straight polylines per curve.
//! - `"interpolation"` — per-mark; string; default `"domain"`.
//!   `"domain"` or `"panel"` (see above).
//! - `"fill"` — band fill colour (per-row or per-mark; varying drives
//!   mesh dispatch).
//! - `"alpha"` — overrides `"fill"` / outline-stroke alphas.
//! - `"pick_id"` — per-mark pick id (resolved at the mark's first row).
//!
//! Per-curve outlines (every channel exists in both unsuffixed (curve A)
//! and `2`-suffixed (curve B) form):
//!
//! - `"stroke"` / `"stroke2"` — outline colour. Bound → that curve is
//!   stroked.
//! - `"linewidth"` / `"linewidth2"` — width in pt (default 1.0).
//! - `"linetype"` / `"linetype2"` — dash pattern (`LinetypeStep`
//!   sequence; default solid).
//! - `"dash_offset"` / `"dash_offset2"` — pt phase shift.
//! - `"cap"` / `"cap2"`, `"join"` / `"join2"` — stroke style strings.
//! - `"clip_start_radius"` / `"clip_start_radius2"`,
//!   `"clip_end_radius"` / `"clip_end_radius2"` — endpoint clip in pt.
//! - `"start_marker"` / `"start_marker2"`, `"end_marker"` /
//!   `"end_marker2"` — shape names (Phase C.5).
//! - `"start_marker_size"` / `"start_marker_size2"`,
//!   `"end_marker_size"` / `"end_marker_size2"` — marker size in pt
//!   (default `3 × linewidth`).
//! - `"start_marker_fill"` / `"start_marker_fill2"`,
//!   `"end_marker_fill"` / `"end_marker_fill2"` — marker interior.
//! - `"start_marker_invert"` / `"start_marker_invert2"`,
//!   `"end_marker_invert"` / `"end_marker_invert2"` — flip outward.
//!
//! ### Fill dispatch
//!
//! - Uniform `"fill"` across a mark → single `Brush::Solid` over the
//!   closed-contour path (fast path).
//! - Varying `"fill"` (or `"alpha"`) → per-vertex quad-strip mesh via
//!   [`ribbon_band_mesh`](crate::primitives::ribbon_band_mesh). Unlike
//!   [`RibbonGeom`], B-spline ribbons always use the mesh for varying
//!   fill — the gradient-brush fast path needs row-aligned stops, which
//!   doesn't map cleanly onto interpolated spline samples.
//!
//! Non-finite rows (NaN in any required positional channel) are dropped
//! silently. Marks with fewer than two finite rows render nothing.

use std::cmp::Ordering;

use crate::brush::Brush;
use crate::color::Color;
use crate::geometry::{Affine, Point, Rect};
use crate::path::{FillRule, Path};
use crate::plot::diff::{diff_columns, diff_positional, KeyIndex};
use crate::plot::scale::Scale;
use crate::plot::value::DataColumn;
use crate::scene::SceneBuilder;

use super::bspline_eval::{
    build_polyline_fallback, build_spline_flatten, de_boor, project_ctrl_pts, InterpolationSpace,
};
use super::marks::{build_marks_from_column, unique_values_at_first_rows, MarkSlot};
use super::outline::{draw_curve_outline, resolve_outline_spec, OutlineChannels, OutlineScales};
use super::resolve::{
    override_alpha, resolve_color_channel, resolve_color_channel_or_theme, resolve_number_channel,
    resolve_number_channel_or, resolve_pick_id, resolve_position, resolve_str_channel_or,
    ChannelBind,
};
use super::ribbon::{append_cap_fan_to_mesh, resolve_b_row, CapDirection, Orientation};
use super::state::{finalize_state, require_x_and_siblings, GeomState, KeysStrategy};
use super::{BuildableGeom, Channel, ExpectedOutput, Geom, GeomBuilder, GeomContext, Keys};

const DEFAULT_DEGREE: usize = 3;

/// Catalog of channels this geom recognises, with their expected scale
/// output type. The per-curve outline channels mirror
/// `BSplineGeom`/`LineGeom`'s full surface, doubled (`<name>` for curve
/// A, `<name>2` for curve B).
const CHANNELS: &[(&str, ExpectedOutput)] = &[
    ("x", ExpectedOutput::Numbers),
    ("y", ExpectedOutput::Numbers),
    ("x2", ExpectedOutput::Numbers),
    ("y2", ExpectedOutput::Numbers),
    ("degree", ExpectedOutput::Numbers),
    ("interpolation", ExpectedOutput::Strings),
    ("fill", ExpectedOutput::Colors),
    ("alpha", ExpectedOutput::Numbers),
    ("pick_id", ExpectedOutput::Numbers),
    // Curve A outline.
    ("stroke", ExpectedOutput::Colors),
    ("linewidth", ExpectedOutput::Numbers),
    ("linetype", ExpectedOutput::Linetypes),
    ("dash_offset", ExpectedOutput::Numbers),
    ("cap", ExpectedOutput::Strings),
    ("join", ExpectedOutput::Strings),
    ("clip_start_radius", ExpectedOutput::Numbers),
    ("clip_end_radius", ExpectedOutput::Numbers),
    ("start_marker", ExpectedOutput::Strings),
    ("end_marker", ExpectedOutput::Strings),
    ("start_marker_size", ExpectedOutput::Numbers),
    ("end_marker_size", ExpectedOutput::Numbers),
    ("start_marker_fill", ExpectedOutput::Colors),
    ("end_marker_fill", ExpectedOutput::Colors),
    ("start_marker_invert", ExpectedOutput::Any),
    ("end_marker_invert", ExpectedOutput::Any),
    // Curve B outline.
    ("stroke2", ExpectedOutput::Colors),
    ("linewidth2", ExpectedOutput::Numbers),
    ("linetype2", ExpectedOutput::Linetypes),
    ("dash_offset2", ExpectedOutput::Numbers),
    ("cap2", ExpectedOutput::Strings),
    ("join2", ExpectedOutput::Strings),
    ("clip_start_radius2", ExpectedOutput::Numbers),
    ("clip_end_radius2", ExpectedOutput::Numbers),
    ("start_marker2", ExpectedOutput::Strings),
    ("end_marker2", ExpectedOutput::Strings),
    ("start_marker_size2", ExpectedOutput::Numbers),
    ("end_marker_size2", ExpectedOutput::Numbers),
    ("start_marker_fill2", ExpectedOutput::Colors),
    ("end_marker_fill2", ExpectedOutput::Colors),
    ("start_marker_invert2", ExpectedOutput::Any),
    ("end_marker_invert2", ExpectedOutput::Any),
];

/// A vectorised B-spline filled-band geom.
pub struct RibbonBSplineGeom {
    pub(crate) state: GeomState,
    pub(crate) marks: Vec<MarkSlot>,
    pub(crate) orientation: Orientation,
}

crate::impl_geom_inherents_grouped!(RibbonBSplineGeom);

impl RibbonBSplineGeom {
    /// Build the mark layout from the current keys column.
    pub(crate) fn build_marks(&self) -> Vec<MarkSlot> {
        super::marks::build_marks(&self.state.keys)
    }
}

impl BuildableGeom for RibbonBSplineGeom {
    fn build_from(builder: GeomBuilder<Self>) -> Self {
        let (keys_opt, channels) = builder.into_parts();
        let n = require_x_and_siblings(&channels, &["y"], "RibbonBSplineGeom");

        let has_x2 = channels.contains_key("x2");
        let has_y2 = channels.contains_key("y2");
        let orientation = match (has_x2, has_y2) {
            (false, false) => panic!(
                "RibbonBSplineGeom::build: needs at least one of \"x2\" or \"y2\" \
                 (use a constant baseline, e.g. y2 = 0.0, for an area-to-axis ribbon)"
            ),
            (true, false) => Orientation::Vertical,
            (false, true) => Orientation::Horizontal,
            (true, true) => Orientation::Free,
        };

        let state = finalize_state(
            keys_opt,
            channels,
            n,
            KeysStrategy::OneMark,
            CHANNELS,
            "RibbonBSplineGeom",
        );
        RibbonBSplineGeom {
            state,
            marks: Vec::new(),
            orientation,
        }
    }
}

// ─── Draw-time channel/scale bundle ──────────────────────────────────────────

/// Channel + scale references and orientation handed to
/// [`draw_one_ribbon_bspline_mark`] for one draw call. Bundles curve-A/-B
/// outline handles (already aggregated by [`OutlineChannels`] /
/// [`OutlineScales`]) with the fill, alpha, pick-id, and B-spline
/// configuration channels plus the x/x2/y/y2 positional inputs.
#[derive(Clone, Copy)]
struct RibbonBSplineDrawCtx<'a> {
    orientation: Orientation,
    x_col: &'a DataColumn,
    y_col: &'a DataColumn,
    x_scale: Option<&'a Scale>,
    y_scale: Option<&'a Scale>,
    x2: ChannelBind<'a>,
    y2: ChannelBind<'a>,
    fill: ChannelBind<'a>,
    alpha: ChannelBind<'a>,
    degree: ChannelBind<'a>,
    interpolation: ChannelBind<'a>,
    pick_id: ChannelBind<'a>,
    outline_a_ch: OutlineChannels<'a>,
    outline_b_ch: OutlineChannels<'a>,
    outline_a_scales: OutlineScales<'a>,
    outline_b_scales: OutlineScales<'a>,
}

impl<'a> RibbonBSplineDrawCtx<'a> {
    /// Resolve x/y columns + scales and look up every per-mark channel
    /// by name. Returns `None` when `x` or `y` is missing or
    /// non-positional.
    fn build(
        channels: &'a std::collections::HashMap<String, Channel>,
        ctx: &'a GeomContext<'a>,
        orientation: Orientation,
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
            orientation,
            x_col,
            y_col,
            x_scale,
            y_scale,
            x2: b("x2"),
            y2: b("y2"),
            fill: b("fill"),
            alpha: b("alpha"),
            degree: b("degree"),
            interpolation: b("interpolation"),
            pick_id: b("pick_id"),
            outline_a_ch: OutlineChannels::from_map(channels, ""),
            outline_b_ch: OutlineChannels::from_map(channels, "2"),
            outline_a_scales: OutlineScales::from_ctx(ctx, ""),
            outline_b_scales: OutlineScales::from_ctx(ctx, "2"),
        })
    }
}

impl Geom for RibbonBSplineGeom {
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
                let prev_unique = unique_values_at_first_rows(
                    prev_col,
                    prev_marks.iter().map(|m| m.first_row),
                    "RibbonBSplineGeom",
                );
                let next_unique = unique_values_at_first_rows(
                    next_col,
                    next_marks.iter().map(|m| m.first_row),
                    "RibbonBSplineGeom",
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
        let marks: &[MarkSlot] = if self.marks.is_empty() && !self.is_empty() {
            owned_marks = self.build_marks();
            &owned_marks
        } else {
            &self.marks
        };
        if marks.is_empty() {
            return;
        }

        let dc = match RibbonBSplineDrawCtx::build(&self.state.channels, ctx, self.orientation) {
            Some(dc) => dc,
            None => return,
        };

        for mark in marks.iter() {
            draw_one_ribbon_bspline_mark(scene, ctx, panel, dc, mark);
        }
    }
}

/// Render a single B-spline ribbon mark — both boundary curves
/// evaluated and densified, fill contour built, optional gradient brush
/// or per-vertex mesh, per-curve outlines. Each mark is independent;
/// the caller iterates.
fn draw_one_ribbon_bspline_mark(
    scene: &mut dyn SceneBuilder,
    ctx: &GeomContext<'_>,
    panel: Rect,
    dc: RibbonBSplineDrawCtx<'_>,
    mark: &MarkSlot,
) {
    let RibbonBSplineDrawCtx {
        orientation,
        x_col,
        y_col,
        x_scale,
        y_scale,
        x2: ChannelBind {
            ch: x2_ch,
            scale: x2_scale_bound,
        },
        y2: ChannelBind {
            ch: y2_ch,
            scale: y2_scale_bound,
        },
        fill: ChannelBind {
            ch: fill_ch,
            scale: fill_scale,
        },
        alpha: ChannelBind {
            ch: alpha_ch,
            scale: alpha_scale,
        },
        degree: ChannelBind {
            ch: degree_ch,
            scale: degree_scale,
        },
        interpolation:
            ChannelBind {
                ch: interpolation_ch,
                scale: interpolation_scale,
            },
        pick_id:
            ChannelBind {
                ch: pick_id_ch,
                scale: pick_id_scale,
            },
        outline_a_ch,
        outline_b_ch,
        outline_a_scales,
        outline_b_scales,
    } = dc;

    let i0 = mark.first_row;

    let mark_fill = override_alpha(
        resolve_color_channel_or_theme(
            fill_ch,
            fill_scale,
            i0,
            ctx.theme.geom.ribbon_bspline.fill.as_ref(),
            &ctx.theme.palette,
        ),
        resolve_number_channel(alpha_ch, alpha_scale, i0),
    );
    let pick = resolve_pick_id(pick_id_ch, pick_id_scale, i0);
    let outline_a_spec = resolve_outline_spec(
        ctx,
        &ctx.theme.geom.ribbon_bspline,
        &outline_a_ch,
        &outline_a_scales,
        alpha_ch,
        alpha_scale,
        i0,
        pick,
    );
    let outline_b_spec = resolve_outline_spec(
        ctx,
        &ctx.theme.geom.ribbon_bspline,
        &outline_b_ch,
        &outline_b_scales,
        alpha_ch,
        alpha_scale,
        i0,
        pick,
    );

    if mark_fill.is_none() && outline_a_spec.is_none() && outline_b_spec.is_none() {
        return;
    }

    // Resolve per-mark spline configuration.
    let degree_req =
        resolve_number_channel_or(degree_ch, degree_scale, i0, DEFAULT_DEGREE as f64) as usize;
    let interp_mode_str =
        resolve_str_channel_or(interpolation_ch, interpolation_scale, i0, "domain");
    let interpolation_mode = match interp_mode_str.as_str() {
        "panel" => InterpolationSpace::Panel,
        _ => InterpolationSpace::Domain,
    };

    // Build the two control polygons in channel-fraction space.
    // Rows where either curve has a non-finite control point
    // drop out together so the two polygons stay length-aligned.
    let mut ctrl_a: Vec<Point> = Vec::with_capacity(mark.rows.len());
    let mut ctrl_b: Vec<Point> = Vec::with_capacity(mark.rows.len());
    let mut row_for_ctrl: Vec<usize> = Vec::with_capacity(mark.rows.len());
    for &i in &mark.rows {
        let x_frac = resolve_position(x_col.get(i), x_scale, 0.0);
        let y_frac = resolve_position(y_col.get(i), y_scale, 0.0);
        if !x_frac.is_finite() || !y_frac.is_finite() {
            continue;
        }
        let (b_x_frac, b_y_frac) = match resolve_b_row(
            orientation,
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
        ctrl_a.push(Point::new(x_frac, y_frac));
        ctrl_b.push(Point::new(b_x_frac, b_y_frac));
        row_for_ctrl.push(i);
    }

    let n_ctrl = ctrl_a.len();
    if n_ctrl < 2 {
        return;
    }
    // Polyline-fallback gating mirrors `BSplineGeom`: when the
    // user's requested degree exceeds what the control polygon
    // can support (`n_ctrl < degree + 1`) both curves degrade to
    // straight polylines through their control points.
    let degenerate = n_ctrl < degree_req.max(1) + 1;
    let samples_a = if degenerate {
        build_polyline_fallback(&ctrl_a, panel, ctx)
    } else {
        build_spline_flatten(&ctrl_a, degree_req, panel, ctx, interpolation_mode)
    };
    let samples_b = if degenerate {
        build_polyline_fallback(&ctrl_b, panel, ctx)
    } else {
        build_spline_flatten(&ctrl_b, degree_req, panel, ctx, interpolation_mode)
    };
    if samples_a.len() < 2 || samples_b.len() < 2 {
        return;
    }
    let curve_a_pts: Vec<Point> = samples_a.iter().map(|(_, p)| *p).collect();
    let curve_b_pts: Vec<Point> = samples_b.iter().map(|(_, p)| *p).collect();

    // Densify the two terminal caps under non-linear projections.
    // Even in `"panel"` mode the closing edges run through
    // `interpolate_segment_with_t` so under polar the caps follow
    // the projection's geodesic, not a pixel-space chord. Under
    // Cartesian both calls return zero interior samples.
    let is_linear = ctx.projection.is_linear();
    let mut start_cap_samples: Vec<crate::plot::projection::InteriorSample> = Vec::new();
    let mut end_cap_samples: Vec<crate::plot::projection::InteriorSample> = Vec::new();
    if !is_linear {
        let first_a = [ctrl_a[0].x, ctrl_a[0].y];
        let first_b = [ctrl_b[0].x, ctrl_b[0].y];
        let last_a = [ctrl_a[n_ctrl - 1].x, ctrl_a[n_ctrl - 1].y];
        let last_b = [ctrl_b[n_ctrl - 1].x, ctrl_b[n_ctrl - 1].y];
        ctx.projection
            .interpolate_segment_with_t(panel, &last_a, &last_b, &mut end_cap_samples);
        ctx.projection.interpolate_segment_with_t(
            panel,
            &first_b,
            &first_a,
            &mut start_cap_samples,
        );
    }

    // Build the closed fill contour: forward along curve A, end
    // cap samples, reversed curve B, start cap samples, close.
    let mut path = Path::new();
    path.move_to(curve_a_pts[0]);
    for p in &curve_a_pts[1..] {
        path.line_to(*p);
    }
    for s in &end_cap_samples {
        path.line_to(Point::new(s.px, s.py));
    }
    for p in curve_b_pts.iter().rev() {
        path.line_to(*p);
    }
    for s in &start_cap_samples {
        path.line_to(Point::new(s.px, s.py));
    }
    path.close_path();

    // Fill dispatch: solid colour → single brush over the closed
    // contour; varying fill → per-vertex mesh on a merged-u grid
    // so paired (A, B) vertices align across the two curves.
    if let Some(mark_color) = mark_fill {
        let varies = super::resolve::channel_varies_across(fill_ch, fill_scale, &row_for_ctrl)
            || super::resolve::channel_varies_across(alpha_ch, alpha_scale, &row_for_ctrl);

        if varies {
            let (curve_a_merged, curve_b_merged, merged_u) = build_merged_grid(
                &samples_a,
                &samples_b,
                &ctrl_a,
                &ctrl_b,
                degree_req,
                n_ctrl,
                degenerate,
                panel,
                ctx,
                interpolation_mode,
            );
            if curve_a_merged.len() >= 2 {
                let colors = build_per_vertex_colors(
                    &merged_u,
                    &row_for_ctrl,
                    fill_ch,
                    fill_scale,
                    alpha_ch,
                    alpha_scale,
                    mark_color,
                );
                let mut mesh = crate::primitives::ribbon_band_mesh(
                    &curve_a_merged,
                    &curve_b_merged,
                    &colors,
                    &colors,
                );
                if !mesh.vertices.is_empty() {
                    // Cap-fan + clip together symmetrise the
                    // cap handling. The fan adds mesh
                    // triangles in outward crescents so the
                    // outer cap arc has fill behind it; the
                    // clip carves inward overshoots so the
                    // strip's straight cap chord doesn't
                    // extend past the inward-bulging arc.
                    // append_cap_fan_to_mesh's
                    // bulge-direction check still skips the
                    // fan when the cap bulges into the strip
                    // (avoiding wasted triangles that'd just
                    // be clipped away).
                    let last = curve_a_merged.len() - 1;
                    let start_neighbor = Point::new(
                        (curve_a_merged[1].x + curve_b_merged[1].x) * 0.5,
                        (curve_a_merged[1].y + curve_b_merged[1].y) * 0.5,
                    );
                    let end_neighbor = Point::new(
                        (curve_a_merged[last - 1].x + curve_b_merged[last - 1].x) * 0.5,
                        (curve_a_merged[last - 1].y + curve_b_merged[last - 1].y) * 0.5,
                    );
                    append_cap_fan_to_mesh(
                        &mut mesh,
                        curve_a_merged[0],
                        curve_b_merged[0],
                        start_neighbor,
                        &start_cap_samples,
                        colors[0],
                        CapDirection::Start,
                    );
                    append_cap_fan_to_mesh(
                        &mut mesh,
                        curve_a_merged[last],
                        curve_b_merged[last],
                        end_neighbor,
                        &end_cap_samples,
                        *colors.last().unwrap(),
                        CapDirection::End,
                    );
                    scene.push_layer(
                        crate::blend::BlendMode::NORMAL,
                        1.0,
                        Affine::IDENTITY,
                        &path,
                    );
                    scene.draw_mesh(&mesh, Affine::IDENTITY, pick);
                    scene.pop_layer();
                }
            }
        } else {
            scene.fill(
                FillRule::NonZero,
                Affine::IDENTITY,
                &Brush::Solid(mark_color),
                None,
                &path,
                pick,
            );
        }
    }

    // Per-curve outlines.
    if let Some(ref spec) = outline_a_spec {
        draw_curve_outline(scene, ctx, &curve_a_pts, spec);
    }
    if let Some(ref spec) = outline_b_spec {
        draw_curve_outline(scene, ctx, &curve_b_pts, spec);
    }
}

/// Build a merged-`u` resampling of both splines so paired `(A, B)`
/// vertices share the same row-position fraction. The union of the two
/// independent flatten passes' `u` values is deduped and used to
/// re-evaluate both control polygons at each sample.
///
/// Returns `(curve_a_pixels, curve_b_pixels, merged_u_grid)`. The
/// `merged_u_grid` is the row-position fraction at each paired sample,
/// used downstream for per-vertex colour lerp.
#[allow(clippy::too_many_arguments)]
fn build_merged_grid(
    samples_a: &[(f64, Point)],
    samples_b: &[(f64, Point)],
    ctrl_a: &[Point],
    ctrl_b: &[Point],
    degree_req: usize,
    n_ctrl: usize,
    degenerate: bool,
    panel: crate::geometry::Rect,
    ctx: &GeomContext<'_>,
    mode: InterpolationSpace,
) -> (Vec<Point>, Vec<Point>, Vec<f64>) {
    // Polyline fallback: row positions are integer indices 0..n_ctrl-1,
    // shared between both curves. Skip the spline-eval branch.
    if degenerate {
        let a_pts: Vec<Point> = samples_a.iter().map(|(_, p)| *p).collect();
        let b_pts: Vec<Point> = samples_b.iter().map(|(_, p)| *p).collect();
        let merged_u: Vec<f64> = (0..n_ctrl).map(|i| i as f64).collect();
        return (a_pts, b_pts, merged_u);
    }

    let mut merged_u: Vec<f64> = Vec::with_capacity(samples_a.len() + samples_b.len());
    merged_u.extend(samples_a.iter().map(|(u, _)| *u));
    merged_u.extend(samples_b.iter().map(|(u, _)| *u));
    merged_u.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    merged_u.dedup_by(|a, b| (*a - *b).abs() < 1e-9);

    // u ∈ [0, n_ctrl − 1] → t ∈ [0, n_ctrl − degree]
    let n_ctrl_minus_1 = (n_ctrl - 1) as f64;
    let t_end = (n_ctrl - degree_req) as f64;
    let to_t = |u: f64| -> f64 {
        if n_ctrl_minus_1 > 0.0 {
            u * t_end / n_ctrl_minus_1
        } else {
            0.0
        }
    };

    let mut curve_a_merged = Vec::with_capacity(merged_u.len());
    let mut curve_b_merged = Vec::with_capacity(merged_u.len());
    match mode {
        InterpolationSpace::Panel => {
            let ctrl_a_px = project_ctrl_pts(ctrl_a, panel, ctx);
            let ctrl_b_px = project_ctrl_pts(ctrl_b, panel, ctx);
            for &u in &merged_u {
                let t = to_t(u);
                curve_a_merged.push(de_boor(&ctrl_a_px, degree_req, t));
                curve_b_merged.push(de_boor(&ctrl_b_px, degree_req, t));
            }
        }
        InterpolationSpace::Domain => {
            for &u in &merged_u {
                let t = to_t(u);
                let p_a = de_boor(ctrl_a, degree_req, t);
                let p_b = de_boor(ctrl_b, degree_req, t);
                let (apx, apy) = ctx.projection.project_to_panel_px(panel, &[p_a.x, p_a.y]);
                let (bpx, bpy) = ctx.projection.project_to_panel_px(panel, &[p_b.x, p_b.y]);
                curve_a_merged.push(Point::new(apx, apy));
                curve_b_merged.push(Point::new(bpx, bpy));
            }
        }
    }
    (curve_a_merged, curve_b_merged, merged_u)
}

/// Build per-vertex colours for the mesh path. Each sample's `u`
/// position lerps the bracketing rows' resolved `(fill, alpha)`.
#[allow(clippy::too_many_arguments)]
fn build_per_vertex_colors(
    merged_u: &[f64],
    row_for_ctrl: &[usize],
    fill_ch: Option<&Channel>,
    fill_scale: Option<&crate::plot::scale::Scale>,
    alpha_ch: Option<&Channel>,
    alpha_scale: Option<&crate::plot::scale::Scale>,
    fallback: Color,
) -> Vec<Color> {
    let resolve_at = |row: usize| -> Color {
        override_alpha(
            resolve_color_channel(fill_ch, fill_scale, row),
            resolve_number_channel(alpha_ch, alpha_scale, row),
        )
        .unwrap_or(fallback)
    };
    let n_rows = row_for_ctrl.len();
    merged_u
        .iter()
        .map(|&u| {
            // u indexes into `row_for_ctrl`, which maps back to source
            // row indices. The bracketing source rows are
            // `row_for_ctrl[lo]` / `row_for_ctrl[hi]`.
            let u_clamped = u.clamp(0.0, (n_rows - 1) as f64);
            let lo = u_clamped.floor() as usize;
            let hi = (lo + 1).min(n_rows - 1);
            let t = u_clamped - lo as f64;
            if lo == hi || t.abs() < 1e-9 {
                resolve_at(row_for_ctrl[lo])
            } else {
                let c0 = resolve_at(row_for_ctrl[lo]);
                let c1 = resolve_at(row_for_ctrl[hi]);
                crate::color::lerp_color(c0, c1, t)
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
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

    fn draw_and_record(mut g: RibbonBSplineGeom) -> RecordingScene {
        g.rebuild_diff_against_previous();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 200.0, 200.0), &shapes, &scales),
        );
        scene
    }

    // ── Builder & orientation selection ──

    #[test]
    fn explicit_y2_selects_horizontal() {
        let g = RibbonBSplineGeom::builder()
            .set("x", vec![0.0_f64, 0.25, 0.5, 0.75, 1.0])
            .set("y", vec![0.8_f64, 0.7, 0.9, 0.6, 0.8])
            .set("y2", 0.2_f64)
            .build();
        assert_eq!(g.orientation, Orientation::Horizontal);
    }

    #[test]
    fn x2_selects_vertical_mode() {
        let g = RibbonBSplineGeom::builder()
            .set("x", vec![0.5_f64, 0.6, 0.5, 0.4, 0.5])
            .set("y", vec![0.0_f64, 0.25, 0.5, 0.75, 1.0])
            .set("x2", 0.2_f64)
            .build();
        assert_eq!(g.orientation, Orientation::Vertical);
    }

    #[test]
    fn both_x2_and_y2_selects_free() {
        let g = RibbonBSplineGeom::builder()
            .set("x", vec![0.0_f64, 0.5, 1.0])
            .set("y", vec![0.0_f64, 1.0, 0.0])
            .set("x2", vec![0.2_f64, 0.5, 0.8])
            .set("y2", vec![0.2_f64, 0.7, 0.2])
            .build();
        assert_eq!(g.orientation, Orientation::Free);
    }

    #[test]
    #[should_panic(expected = "needs at least one")]
    fn no_curve_b_channel_panics() {
        RibbonBSplineGeom::builder()
            .set("x", vec![0.0_f64, 1.0])
            .set("y", vec![0.0_f64, 1.0])
            .build();
    }

    // ── Drawing ──

    #[test]
    fn solid_fill_emits_one_path_fill() {
        let g = RibbonBSplineGeom::builder()
            .set("x", Raw(vec![0.1_f64, 0.3, 0.5, 0.7, 0.9]))
            .set("y", Raw(vec![0.6_f64, 0.8, 0.9, 0.7, 0.6]))
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
        let meshes = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::DrawMesh { .. }))
            .count();
        assert_eq!(solid_fills, 1);
        assert_eq!(meshes, 0);
    }

    #[test]
    fn varying_fill_uses_mesh() {
        let g = RibbonBSplineGeom::builder()
            .set("x", Raw(vec![0.1_f64, 0.3, 0.5, 0.7, 0.9]))
            .set("y", Raw(vec![0.6_f64, 0.8, 0.9, 0.7, 0.6]))
            .set("y2", Raw(0.2_f64))
            .set("fill", vec![red(), blue(), red(), blue(), red()])
            .build();
        let scene = draw_and_record(g);
        let meshes = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::DrawMesh { .. }))
            .count();
        let fills = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Fill { .. }))
            .count();
        assert_eq!(meshes, 1, "expected mesh dispatch for varying fill");
        assert_eq!(fills, 0, "fill should not be emitted alongside mesh");
    }

    #[test]
    fn four_point_cubic_passes_through_endpoints() {
        // Clamped knots → the spline starts at the first control point
        // and ends at the last for both curves. The fill path's MoveTo
        // should land at curve A's first control point in pixel space.
        let g = RibbonBSplineGeom::builder()
            .set("x", Raw(vec![0.1_f64, 0.3, 0.7, 0.9]))
            .set("y", Raw(vec![0.8_f64, 0.9, 0.9, 0.8]))
            .set("y2", Raw(vec![0.2_f64, 0.1, 0.1, 0.2]))
            .set("fill", red())
            .build();
        let scene = draw_and_record(g);
        for op in &scene.ops {
            if let Op::Fill { path, .. } = op {
                if let Some(kurbo::PathEl::MoveTo(start)) = path.elements().first() {
                    // y=0.8 on a 200×200 panel under Cartesian projects
                    // to y_px = panel.y1 - 0.8 * h = 200 - 160 = 40.
                    assert!((start.x - 20.0).abs() < 1.0);
                    assert!((start.y - 40.0).abs() < 1.0);
                    return;
                }
            }
        }
        panic!("no fill emitted");
    }

    #[test]
    fn two_control_points_per_curve_renders_as_quad() {
        // n_ctrl = 2 < degree + 1 = 4 → polyline fallback per curve,
        // producing a quad with 4 vertices. The fill path should have
        // exactly one MoveTo and 3 LineTos (forward A: 2 pts, reversed
        // B: 2 pts, the seam at curve_b_pts[last] is the MoveTo's
        // predecessor via close).
        let g = RibbonBSplineGeom::builder()
            .set("x", Raw(vec![0.1_f64, 0.9]))
            .set("y", Raw(vec![0.8_f64, 0.7]))
            .set("y2", Raw(vec![0.2_f64, 0.3]))
            .set("fill", red())
            .build();
        let scene = draw_and_record(g);
        for op in &scene.ops {
            if let Op::Fill { path, .. } = op {
                let lines = path
                    .elements()
                    .iter()
                    .filter(|el| matches!(el, kurbo::PathEl::LineTo(_)))
                    .count();
                // Each curve contributes 2 points → 1 LineTo from
                // MoveTo (curve A: 1 line-to) + curve B (2 line-tos in
                // reverse). Total 3.
                assert_eq!(lines, 3);
                return;
            }
        }
        panic!("no fill emitted");
    }

    #[test]
    fn single_row_emits_nothing() {
        let g = RibbonBSplineGeom::builder()
            .set("x", Raw(vec![0.5_f64]))
            .set("y", Raw(vec![0.5_f64]))
            .set("y2", Raw(0.2_f64))
            .set("fill", red())
            .build();
        let scene = draw_and_record(g);
        assert!(scene.ops.is_empty());
    }

    #[test]
    fn no_fill_no_stroke_emits_nothing() {
        let g = RibbonBSplineGeom::builder()
            .set("x", Raw(vec![0.1_f64, 0.5, 0.9]))
            .set("y", Raw(vec![0.5_f64, 0.7, 0.5]))
            .set("y2", Raw(0.2_f64))
            .build();
        let scene = draw_and_record(g);
        assert!(scene.ops.is_empty());
    }

    #[test]
    fn stroke_only_curve_a_emits_one_stroke() {
        let g = RibbonBSplineGeom::builder()
            .set("x", Raw(vec![0.1_f64, 0.3, 0.7, 0.9]))
            .set("y", Raw(vec![0.6_f64, 0.8, 0.9, 0.7]))
            .set("y2", Raw(0.2_f64))
            .set("stroke", red())
            .set("linewidth", 2.0_f64)
            .build();
        let scene = draw_and_record(g);
        let strokes = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Stroke { .. }))
            .count();
        assert_eq!(strokes, 1);
    }

    #[test]
    fn stroke_both_curves_emits_two_strokes() {
        let g = RibbonBSplineGeom::builder()
            .set("x", Raw(vec![0.1_f64, 0.3, 0.7, 0.9]))
            .set("y", Raw(vec![0.6_f64, 0.8, 0.9, 0.7]))
            .set("y2", Raw(vec![0.2_f64, 0.3, 0.3, 0.2]))
            .set("stroke", red())
            .set("stroke2", blue())
            .set("linewidth", 2.0_f64)
            .set("linewidth2", 2.0_f64)
            .build();
        let scene = draw_and_record(g);
        let strokes = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Stroke { .. }))
            .count();
        assert_eq!(strokes, 2);
    }

    #[test]
    fn polar_band_caps_are_densified_in_panel_mode() {
        // Free orientation under polar, `"panel"` interpolation mode.
        // The spline portions are pixel-space chords between projected
        // control points, but the closing caps must still curve along
        // the polar arc.
        use crate::plot::projection::Projection;
        let polar = Projection::polar();
        let mut g = RibbonBSplineGeom::builder()
            .set("x", Raw(vec![0.1_f64, 0.5, 0.9]))
            .set("y", Raw(vec![0.8_f64, 0.85, 0.8]))
            .set("x2", Raw(vec![0.2_f64, 0.5, 0.8]))
            .set("y2", Raw(vec![0.4_f64, 0.45, 0.4]))
            .set("interpolation", "panel")
            .set("fill", red())
            .build();
        g.rebuild_diff_against_previous();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        let panel = Rect::new(0.0, 0.0, 200.0, 200.0);
        let ctx = GeomContext::with_projection(panel, 96.0, &shapes, &scales, &polar);
        g.draw(&mut scene, &ctx);

        // Find the fill path and look for at least one off-chord
        // (curved) vertex — that's the cap densification samples.
        for op in &scene.ops {
            if let Op::Fill { path, .. } = op {
                let mut all_pts: Vec<kurbo::Point> = Vec::new();
                for el in path.elements().iter() {
                    match el {
                        kurbo::PathEl::MoveTo(p) | kurbo::PathEl::LineTo(p) => all_pts.push(*p),
                        _ => {}
                    }
                }
                let mut off_chord = 0usize;
                for w in all_pts.windows(3) {
                    let (a, b, c) = (w[0], w[1], w[2]);
                    let area2 = (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x);
                    if area2.abs() > 0.5 {
                        off_chord += 1;
                    }
                }
                assert!(
                    off_chord > 0,
                    "expected at least one off-chord vertex in the polar cap"
                );
                return;
            }
        }
        panic!("no fill emitted");
    }

    #[test]
    fn polar_mesh_path_appends_cap_fan_triangles_for_outward_bulge() {
        // Chord-diagram-style data: both curves' endpoints sit at the
        // outer ring and the middle control points dip toward the
        // polar centre. Caps connect two outer-ring points at constant
        // high r, so the polar geodesic between them bulges outward
        // (away from the polar centre) while the strip sweeps inward.
        // Bulge is opposite to strip-sweep direction → cap fan fires.
        use crate::plot::projection::Projection;
        let polar = Projection::polar();
        let mut g = RibbonBSplineGeom::builder()
            .set("x", Raw(vec![0.1_f64, 0.1, 0.9, 0.9]))
            .set("y", Raw(vec![0.9_f64, 0.0, 0.0, 0.9]))
            .set("x2", Raw(vec![0.2_f64, 0.2, 0.8, 0.8]))
            .set("y2", Raw(vec![0.9_f64, 0.0, 0.0, 0.9]))
            .set("fill", vec![red(), red(), blue(), blue()])
            .build();
        g.rebuild_diff_against_previous();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        let panel = crate::geometry::Rect::new(0.0, 0.0, 200.0, 200.0);
        let ctx = GeomContext::with_projection(panel, 96.0, &shapes, &scales, &polar);
        g.draw(&mut scene, &ctx);

        let mesh = scene
            .ops
            .iter()
            .find_map(|op| match op {
                Op::DrawMesh { mesh, .. } => Some(mesh),
                _ => None,
            })
            .expect("no mesh emitted");
        // Bare strip with N merged-u pairs has 2 * (N - 1) triangles.
        // Cap fans add at least a few more on outward-bulging caps.
        // With the chord-style geometry both caps fire, so we expect
        // more triangles than the strip alone.
        let strip_n_pairs = mesh.vertices.len() / 4; // 4 vertices per quad in ribbon_band_mesh
        let strip_triangles = 2 * strip_n_pairs.saturating_sub(1);
        assert!(
            mesh.triangle_count() > strip_triangles,
            "expected cap fan to add triangles beyond the bare strip (got {} triangles, {} from strip)",
            mesh.triangle_count(),
            strip_triangles
        );
    }

    #[test]
    fn polar_mesh_path_wraps_in_clip_layer() {
        // The mesh dispatch under non-linear projection wraps the
        // draw_mesh call in a push_layer / pop_layer pair whose clip
        // is the polygon-fill path (with densified caps). This is
        // what lets the cap handling stay symmetric across bulge
        // directions: outward crescents are filled by cap fans
        // inside the polygon; inward overshoots are carved off the
        // strip's straight chord by the clip.
        use crate::plot::projection::Projection;
        let polar = Projection::polar();
        let mut g = RibbonBSplineGeom::builder()
            .set("x", Raw(vec![0.1_f64, 0.5, 0.9]))
            .set("y", Raw(vec![0.8_f64, 0.85, 0.8]))
            .set("x2", Raw(vec![0.2_f64, 0.5, 0.8]))
            .set("y2", Raw(vec![0.4_f64, 0.45, 0.4]))
            .set("fill", vec![red(), blue(), red()])
            .build();
        g.rebuild_diff_against_previous();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        let panel = crate::geometry::Rect::new(0.0, 0.0, 200.0, 200.0);
        let ctx = GeomContext::with_projection(panel, 96.0, &shapes, &scales, &polar);
        g.draw(&mut scene, &ctx);

        let mut saw_push = false;
        let mut saw_mesh_after_push = false;
        let mut saw_pop_after_mesh = false;
        for op in &scene.ops {
            match op {
                Op::PushLayer { .. } => saw_push = true,
                Op::DrawMesh { .. } if saw_push => saw_mesh_after_push = true,
                Op::PopLayer if saw_mesh_after_push => saw_pop_after_mesh = true,
                _ => {}
            }
        }
        assert!(
            saw_push && saw_mesh_after_push && saw_pop_after_mesh,
            "expected push_layer → draw_mesh → pop_layer sequence under polar"
        );
    }

    #[test]
    fn polar_mesh_path_skips_cap_fan_when_cap_bulges_into_strip() {
        // Annular-band data: curve A at high r, curve B at low r,
        // both walking the same angular range. At the caps the cap
        // arc bulges toward the strip's interior (the sweep direction
        // and the cap bulge share a positive dot product), so the cap
        // fan would double-fill the strip — append_cap_fan_to_mesh
        // detects this and skips the fan. The mesh ends up with just
        // the bare strip's quads, no extra fan triangles.
        use crate::plot::projection::Projection;
        let polar = Projection::polar();
        let mut g = RibbonBSplineGeom::builder()
            .set("x", Raw(vec![0.1_f64, 0.5, 0.9]))
            .set("y", Raw(vec![0.8_f64, 0.85, 0.8]))
            .set("x2", Raw(vec![0.2_f64, 0.5, 0.8]))
            .set("y2", Raw(vec![0.4_f64, 0.45, 0.4]))
            .set("fill", vec![red(), blue(), red()])
            .build();
        g.rebuild_diff_against_previous();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        let panel = crate::geometry::Rect::new(0.0, 0.0, 200.0, 200.0);
        let ctx = GeomContext::with_projection(panel, 96.0, &shapes, &scales, &polar);
        g.draw(&mut scene, &ctx);

        let mesh = scene
            .ops
            .iter()
            .find_map(|op| match op {
                Op::DrawMesh { mesh, .. } => Some(mesh),
                _ => None,
            })
            .expect("no mesh emitted");
        // With both caps skipped, every quad contributes 4 vertices /
        // 6 indices and the vertex count is exactly 4 * n_quads.
        assert_eq!(
            mesh.vertices.len() % 4,
            0,
            "skip case should leave only quad-pair vertices, got {}",
            mesh.vertices.len()
        );
        assert_eq!(
            mesh.indices.len() % 6,
            0,
            "skip case should leave only quad-pair indices, got {}",
            mesh.indices.len()
        );
    }

    #[test]
    fn declared_channels_alphabetical() {
        let g = RibbonBSplineGeom::builder()
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
}
