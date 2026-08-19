//! `BSplineGeom` — clamped uniform B-spline curve, one curve per mark.
//!
//! Per-mark like [`LineGeom`](super::LineGeom): rows sharing a key value
//! form one curve. The rows' `(x, y)` positions are the control polygon
//! (in source order). The knot vector is clamped uniform — the first and
//! last control points sit exactly on the curve; interior control points
//! pull the curve toward themselves without forcing it through. A
//! 4-point degree-3 group collapses to a cubic Bezier; longer groups
//! generalise without an API change.
//!
//! Channels consumed:
//!
//! - `"x"` — control-point x (required; data; numeric).
//! - `"y"` — control-point y (required; data; numeric).
//! - `"x_offset"` / `"y_offset"` — per-row absolute pt offsets applied
//!   to the projected spline samples, lerped between bracketing
//!   control points by the spline parameter. Positive `y_offset`
//!   shifts the sample up (decrements pixel y).
//! - `"x_band"` / `"y_band"` — per-row band-fraction offset folded
//!   into the scale's `map_with_offset` for each control point. No
//!   effect on continuous scales.
//! - `"degree"` — curve degree (per-mark; default 3). Effective degree
//!   is clamped to `min(degree, n_ctrl - 1)`. Groups with fewer than
//!   `degree + 1` control points degrade to a straight polyline through
//!   the available points.
//! - `"interpolation"` — `"domain"` (default) or `"panel"`. Under
//!   non-Cartesian projections selects whether the spline is built in
//!   channel-fraction space and then projected sample-by-sample
//!   (`"domain"` — faithful in data space), or whether control points
//!   are projected first and the spline is built in pixel space
//!   (`"panel"` — smoothed polyline through the projected vertices).
//!   Cartesian projections collapse the two modes to the same result.
//! - `"stroke"` — outline color (per-mark). Also used as the marker
//!   stroke color for any markers in the linetype.
//! - `"stroke_opacity"` — overrides alpha of `"stroke"` (per-mark).
//! - `"fill"` — marker interior color for linetype markers (per-mark;
//!   defaults to the resolved stroke color when unset). The curve
//!   itself is stroked, not filled — `"fill"` only affects marker
//!   interiors and endpoint markers.
//! - `"linewidth"` — stroke width in pt (per-mark; default 1.0 pt).
//! - `"linetype"` — [`crate::plot::value::LinetypeStep`] pattern
//!   (per-mark; default solid). A pure-dash pattern renders via the
//!   kurbo stroke fast path. A pattern containing markers walks the
//!   flattened curve in arc length and stamps each marker rotated to
//!   the local tangent.
//! - `"dash_offset"` — phase shift along the dash pattern in pt
//!   (per-mark). No effect on solid lines.
//! - `"cap"` / `"join"` — cap and join style (per-mark; defaults
//!   `"butt"` / `"miter"`).
//! - `"clip_start_radius"` / `"clip_end_radius"` — circle clip radius
//!   in pt at the spline's first / last sample (per-mark; default
//!   `0.0` — no clip). When non-zero, the flattened curve is trimmed
//!   where it exits a circle of that radius centred on the first /
//!   last sample. Use to make room for an arrowhead at the endpoint
//!   so the arrow tip lands at the original endpoint rather than
//!   extending past it.
//! - `"start_marker"` / `"end_marker"` — registered shape name stamped
//!   at the post-clip endpoint (per-mark). Outward direction follows
//!   `endpoint_outward`: when the
//!   endpoint was clipped, the chord from the clipped endpoint toward
//!   the *original* endpoint (the direction the curve would have
//!   continued in); otherwise the terminal edge of the flattened
//!   polyline. Same convention as LineGeom.
//! - `"start_marker_size"` / `"end_marker_size"` — marker size in pt
//!   (per-mark; default `3 × linewidth`).
//! - `"start_marker_fill"` / `"end_marker_fill"` — marker interior
//!   colour (per-mark; defaults to the linetype-marker fill which
//!   itself defaults to the stroke colour).
//! - `"start_marker_invert"` / `"end_marker_invert"` — flip the
//!   outward direction (per-mark; default `false`).
//! - `"angle"` — rotation in **radians** around the curve's centroid
//!   (mean of the flattened sample positions in panel space),
//!   mathematical CCW. Per-mark; default `0.0`. Applied after the
//!   spline is flattened, so clipping and endpoint markers are
//!   resolved in the unrotated frame and the whole curve then turns as
//!   a rigid body. Same convention as LineGeom.
//! - `"pick_id"` — per-mark pick id (resolved at the mark's first row).
//!
//! Per-mark channels resolve once per curve (first-row-of-mark, like
//! every other multi-row-per-mark geom). When `"stroke"` or
//! `"linewidth"` varies across the rows of a mark and the linetype is
//! solid, the geom upgrades to a per-vertex tessellated mesh via
//! [`polyline_ribbon_full`](crate::primitives::polyline_ribbon_full).
//! Per-sample colour and half-width are linearly interpolated between
//! adjacent control points' values, indexed by the spline parameter
//! rescaled to a row position — same convention LineGeom uses for its
//! per-segment lerp, generalised to the spline parameter space.

use crate::color::{lerp_color, Color};
use crate::geometry::{Affine, Point, Rect};
use crate::plot::scale::Scale;
use crate::plot::value::DataColumn;
use crate::scene::SceneBuilder;

use super::marks::{build_marks_from_column, MarkSlot};
use super::outline::{
    draw_curve_outline, draw_ribbon_mode_curve, resolve_outline_spec, OutlineChannels,
    OutlineScales,
};
use super::resolve::{
    apply_per_row_offsets, channel_color_space, channel_varies_across, override_alpha, pt_to_px,
    resolve_angle_channel, resolve_color_channel, resolve_number_channel,
    resolve_number_channel_or, resolve_pick_id, resolve_position, resolve_str_channel_or,
    ChannelBind,
};
use super::state::{finalize_state, require_x_and_siblings, GeomState, KeysStrategy};
use super::{BuildableGeom, Channel, ExpectedOutput, Geom, GeomBuilder, GeomContext, Keys};

// Style defaults (linewidth, cap, join) live on `theme.geom.bspline`.
// DEGREE is a semantic default — the curve's order — and stays as a
// per-geom constant.
const DEFAULT_DEGREE: usize = 3;

use super::bspline_eval::{build_polyline_fallback, build_spline_flatten, InterpolationSpace};
// `de_boor` and `CHORD_ERROR_PX` are referenced only inside the test
// module below.
#[cfg(test)]
use super::bspline_eval::{de_boor, CHORD_ERROR_PX};

/// Catalog of channels this geom recognises, with their expected scale
/// output type.
const CHANNELS: &[(&str, ExpectedOutput)] = &[
    ("x", ExpectedOutput::Numbers),
    ("y", ExpectedOutput::Numbers),
    ("x_offset", ExpectedOutput::Numbers),
    ("y_offset", ExpectedOutput::Numbers),
    ("x_band", ExpectedOutput::Numbers),
    ("y_band", ExpectedOutput::Numbers),
    ("degree", ExpectedOutput::Numbers),
    ("interpolation", ExpectedOutput::Strings),
    ("fill", ExpectedOutput::Colors),
    ("stroke", ExpectedOutput::Colors),
    ("stroke_opacity", ExpectedOutput::Numbers),
    ("linewidth", ExpectedOutput::Numbers),
    ("linetype", ExpectedOutput::Linetypes),
    ("dash_offset", ExpectedOutput::Numbers),
    ("cap", ExpectedOutput::Strings),
    ("join", ExpectedOutput::Strings),
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

// ─── BSplineGeom ─────────────────────────────────────────────────────────────

/// A vectorised B-spline geom. Non-generic; all channel data flows
/// through [`DataColumn`].
pub struct BSplineGeom {
    pub(crate) state: GeomState,
    /// Cached mark layout — rebuilt at the start of each `draw` /
    /// `rebuild_diff_against_previous`. One entry per unique key value
    /// in first-appearance order.
    pub(crate) marks: Vec<MarkSlot>,
}

crate::impl_geom_inherents_grouped!(BSplineGeom);

impl BSplineGeom {
    /// Build the mark layout from the current keys column.
    pub(crate) fn build_marks(&self) -> Vec<MarkSlot> {
        super::marks::build_marks(&self.state.keys)
    }
}

// ─── BuildableGeom impl ──────────────────────────────────────────────────────

impl BuildableGeom for BSplineGeom {
    fn build_from(builder: GeomBuilder<Self>) -> Self {
        let (keys_opt, channels) = builder.into_parts();
        let n = require_x_and_siblings(&channels, &["y"], "BSplineGeom");
        let state = finalize_state(
            keys_opt,
            channels,
            n,
            KeysStrategy::OneMark,
            CHANNELS,
            "BSplineGeom",
        );
        BSplineGeom {
            state,
            marks: Vec::new(),
        }
    }
}

// ─── Draw-time channel/scale bundle ──────────────────────────────────────────

/// Channel + scale references for one `BSplineGeom::draw` call. Built
/// once at the top of `draw`, then threaded into [`draw_one_bspline_mark`].
#[derive(Clone, Copy)]
struct BSplineDrawCtx<'a> {
    x_col: &'a DataColumn,
    y_col: &'a DataColumn,
    x_scale: Option<&'a Scale>,
    y_scale: Option<&'a Scale>,
    x_offset: ChannelBind<'a>,
    y_offset: ChannelBind<'a>,
    x_band: ChannelBind<'a>,
    y_band: ChannelBind<'a>,
    degree: ChannelBind<'a>,
    interpolation: ChannelBind<'a>,
    /// Marker interior colour override, handed to
    /// [`resolve_outline_spec`] as the outline's marker fill.
    fill: ChannelBind<'a>,
    stroke: ChannelBind<'a>,
    stroke_opacity: ChannelBind<'a>,
    linewidth: ChannelBind<'a>,
    angle: ChannelBind<'a>,
    pick_id: ChannelBind<'a>,
    /// The stroke / linetype / cap / clip / endpoint-marker surface,
    /// resolved as one outline spec per mark.
    outline_ch: OutlineChannels<'a>,
    outline_sc: OutlineScales<'a>,
}

impl<'a> BSplineDrawCtx<'a> {
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
            degree: b("degree"),
            interpolation: b("interpolation"),
            fill: b("fill"),
            stroke: b("stroke"),
            stroke_opacity: b("stroke_opacity"),
            linewidth: b("linewidth"),
            angle: b("angle"),
            pick_id: b("pick_id"),
            outline_ch: OutlineChannels::from_map(channels, ""),
            outline_sc: OutlineScales::from_ctx(ctx, ""),
        })
    }
}

// ─── Geom impl ───────────────────────────────────────────────────────────────

impl Geom for BSplineGeom {
    fn state(&self) -> &GeomState {
        &self.state
    }

    fn state_mut(&mut self) -> &mut GeomState {
        &mut self.state
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn kind(&self) -> Option<&'static str> {
        Some("bspline")
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
        let first_rows =
            |ms: &[MarkSlot]| -> Vec<usize> { ms.iter().map(|m| m.first_row).collect() };
        self.state.rebuild_grouped_diff(
            &first_rows(&prev_marks),
            &first_rows(&next_marks),
            "BSplineGeom",
        );
        self.marks = next_marks;
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

        let dc = match BSplineDrawCtx::build(&self.state.channels, ctx) {
            Some(dc) => dc,
            None => return,
        };

        for mark in marks.iter() {
            draw_one_bspline_mark(scene, ctx, panel, dc, mark);
        }
    }
}

/// Render a single B-spline mark — channel resolution, control-point
/// projection, spline flattening (faithful in domain space or smoothed
/// in pixel space), clip + ribbon-mode dispatch, stroke + endpoint
/// markers. Each mark is independent; the caller iterates.
fn draw_one_bspline_mark(
    scene: &mut dyn SceneBuilder,
    ctx: &GeomContext<'_>,
    panel: Rect,
    dc: BSplineDrawCtx<'_>,
    mark: &MarkSlot,
) {
    let BSplineDrawCtx {
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
        degree: ChannelBind {
            ch: degree_ch,
            scale: degree_scale,
        },
        interpolation:
            ChannelBind {
                ch: interpolation_ch,
                scale: interpolation_scale,
            },
        fill,
        stroke: ChannelBind {
            ch: stroke_ch,
            scale: stroke_scale,
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
        angle: ChannelBind {
            ch: angle_ch,
            scale: angle_scale,
        },
        pick_id:
            ChannelBind {
                ch: pick_id_ch,
                scale: pick_id_scale,
            },
        outline_ch,
        outline_sc,
    } = dc;

    // ── Per-mark channel resolution (first row of mark). ──
    //
    // The whole stroke / linetype / cap / endpoint-clip /
    // endpoint-marker surface resolves in one go; `xform` lands on the
    // spec further down, once the flattened curve it pivots around
    // exists.
    let i0 = mark.first_row;
    let pick = resolve_pick_id(pick_id_ch, pick_id_scale, i0);
    let Some(mut spec) = resolve_outline_spec(
        ctx,
        (&ctx.theme.geom.bspline).into(),
        &outline_ch,
        &outline_sc,
        fill,
        i0,
        pick,
    ) else {
        return;
    };
    let stroke_color = spec.stroke_color;
    let linewidth_pt = spec.linewidth_pt;
    let linewidth_px = pt_to_px(linewidth_pt, ctx.dpi);

    let degree_raw = resolve_number_channel_or(degree_ch, degree_scale, i0, DEFAULT_DEGREE as f64);
    let degree_req = if degree_raw.is_finite() && degree_raw >= 1.0 {
        degree_raw.round() as usize
    } else {
        DEFAULT_DEGREE
    };

    let interpolation_mode = match resolve_str_channel_or(
        interpolation_ch,
        interpolation_scale,
        i0,
        "domain",
    )
    .as_str()
    {
        "panel" => InterpolationSpace::Panel,
        _ => InterpolationSpace::Domain,
    };

    // ── Control polygon in channel-fraction space. ──
    //
    // Non-finite rows are dropped silently (matches PolygonGeom, and
    // unlike LineGeom, which splits the mark): the spline is fitted to
    // what's left rather than breaking at the gap.
    let mut ctrl_frac: Vec<Point> = Vec::with_capacity(mark.rows.len());
    let mut ctrl_rows: Vec<usize> = Vec::with_capacity(mark.rows.len());
    for &i in &mark.rows {
        let x_band = resolve_number_channel_or(x_band_ch, x_band_scale, i, 0.0);
        let y_band = resolve_number_channel_or(y_band_ch, y_band_scale, i, 0.0);
        let xf = resolve_position(x_col.get(i), x_scale, x_band);
        let yf = resolve_position(y_col.get(i), y_scale, y_band);
        if !xf.is_finite() || !yf.is_finite() {
            continue;
        }
        ctrl_frac.push(Point::new(xf, yf));
        ctrl_rows.push(i);
    }
    if ctrl_frac.len() < 2 {
        return;
    }

    // Effective degree: standard clamped B-spline requires
    // `n >= degree + 1`. Below that we degrade to a straight
    // polyline through whatever control points exist — same
    // semantics the masterplan documents.
    let degenerate = ctrl_frac.len() < degree_req + 1;

    // ── Build the flattened curve in pixel space. ──
    //
    // Two paths, branchless at the call site (each branch
    // returns `Vec<(row_position, pixel_point)>`):
    //
    // - Polyline / degenerate fallback: straight segments
    //   through control points; row position equals control
    //   point index.
    // - Spline: de Boor + adaptive chord-error refinement.
    //   Row position is `u = t × (n − 1) / (n − d)`, the
    //   piecewise-linear lerp index into `ctrl_rows`.
    let samples: Vec<(f64, Point)> = if degenerate {
        build_polyline_fallback(&ctrl_frac, panel, ctx)
    } else {
        build_spline_flatten(&ctrl_frac, degree_req, panel, ctx, interpolation_mode)
    };
    if samples.len() < 2 {
        return;
    }

    // ── Ribbon-mode decision. ──
    //
    // Same dispatch as LineGeom (lines 425): per-vertex
    // tessellated mesh when stroke or linewidth varies within
    // the mark, gated to solid linetype.
    let linewidth_varies = channel_varies_across(linewidth_ch, linewidth_scale, &mark.rows);
    let stroke_varies = channel_varies_across(stroke_ch, stroke_scale, &mark.rows)
        || channel_varies_across(stroke_opacity_ch, stroke_opacity_scale, &mark.rows);
    let ribbon_mode = spec.dash_pattern_pt.is_empty() && (linewidth_varies || stroke_varies);

    // A non-positive width means "nothing to stroke" only when a
    // single width governs the whole mark. In ribbon mode each sample
    // carries its own width, so a zero at the first control point is a
    // pinch point on an otherwise visible ribbon.
    if !ribbon_mode && (!linewidth_px.is_finite() || linewidth_px <= 0.0) {
        return;
    }

    let mut sample_points: Vec<Point> = samples.iter().map(|(_, p)| *p).collect();
    let sample_us: Vec<f64> = samples.iter().map(|(u, _)| *u).collect();
    apply_per_row_offsets(
        &mut sample_points,
        &sample_us,
        &ctrl_rows,
        x_offset_ch,
        x_offset_scale,
        y_offset_ch,
        y_offset_scale,
        ctx.dpi,
    );

    // Per-mark rotation about the flattened curve's centroid. Resolved
    // after flattening and offsets so clipping and endpoint markers are
    // computed in the unrotated frame; the whole curve then turns as a
    // rigid body, matching LineGeom.
    let angle = resolve_angle_channel(angle_ch, angle_scale, i0);
    spec.xform = if angle == 0.0 || sample_points.is_empty() {
        Affine::IDENTITY
    } else {
        let n = sample_points.len() as f64;
        let cx = sample_points.iter().map(|p| p.x).sum::<f64>() / n;
        let cy = sample_points.iter().map(|p| p.y).sum::<f64>() / n;
        Affine::rotate_about(-angle, Point::new(cx, cy))
    };

    if ribbon_mode {
        // ── Ribbon-mode path: per-vertex tessellated mesh. The shared
        // helper threads the per-sample widths / colours through the
        // endpoint clip so the post-clip mesh stays attr-aligned.
        let (ribbon_colors, ribbon_half_widths) = build_ribbon_attrs(
            &samples,
            &ctrl_rows,
            stroke_color,
            linewidth_pt,
            ctx.dpi,
            stroke_ch,
            stroke_scale,
            stroke_opacity_ch,
            stroke_opacity_scale,
            linewidth_ch,
            linewidth_scale,
        );
        draw_ribbon_mode_curve(
            scene,
            ctx.shapes,
            ctx.dpi,
            &sample_points,
            &ribbon_half_widths,
            &ribbon_colors,
            channel_color_space(stroke_scale),
            &spec,
            spec.xform,
        );
        return;
    }

    // ── Non-ribbon path: delegate to the shared `draw_curve_outline`
    // helper, which handles endpoint clip, polyline path construction,
    // start marker, stroke (fast path or dashed-with-markers walker),
    // and end marker.
    draw_curve_outline(
        scene,
        ctx.shapes,
        ctx.dpi,
        ctx.theme.geom.marker_outline_pt,
        &sample_points,
        &spec,
    );
}

// ─── Ribbon-mode per-sample attributes ───────────────────────────────────────

/// Build per-sample (color, half-width) for ribbon-mode dispatch.
/// Each sample carries a row position `u` in `[0, n_ctrl − 1]`
/// (computed by [`build_spline_flatten`] or the polyline fallback);
/// per-row stroke / linewidth / stroke-opacity values lerp linearly between
/// `ctrl_rows[⌊u⌋]` and `ctrl_rows[⌈u⌉]`. This matches LineGeom's
/// per-segment lerp convention, generalised to spline parameter space.
#[allow(clippy::too_many_arguments)]
fn build_ribbon_attrs(
    samples: &[(f64, Point)],
    ctrl_rows: &[usize],
    fallback_stroke: Color,
    linewidth_pt: f64,
    dpi: f64,
    stroke_ch: Option<&Channel>,
    stroke_scale: Option<&crate::plot::scale::Scale>,
    stroke_opacity_ch: Option<&Channel>,
    stroke_opacity_scale: Option<&crate::plot::scale::Scale>,
    linewidth_ch: Option<&Channel>,
    linewidth_scale: Option<&crate::plot::scale::Scale>,
) -> (Vec<Color>, Vec<f64>) {
    let n_rows = ctrl_rows.len();
    let row_color = |i: usize| -> Color {
        override_alpha(
            resolve_color_channel(stroke_ch, stroke_scale, ctrl_rows[i]),
            resolve_number_channel(stroke_opacity_ch, stroke_opacity_scale, ctrl_rows[i]),
        )
        .unwrap_or(fallback_stroke)
    };
    let row_half_width_px = |i: usize| -> f64 {
        let w_pt =
            resolve_number_channel_or(linewidth_ch, linewidth_scale, ctrl_rows[i], linewidth_pt);
        // Clamp so a scale range reaching below zero pinches the ribbon
        // shut instead of flipping its shoulders.
        (pt_to_px(w_pt, dpi) * 0.5).max(0.0)
    };
    let stroke_space = channel_color_space(stroke_scale);
    let last = n_rows - 1;
    let mut colors = Vec::with_capacity(samples.len());
    let mut half_widths = Vec::with_capacity(samples.len());
    for (u, _) in samples {
        let u_clamped = u.clamp(0.0, last as f64);
        let i_a = u_clamped.floor() as usize;
        let i_b = (i_a + 1).min(last);
        let frac = u_clamped - i_a as f64;
        let c_a = row_color(i_a);
        let c_b = row_color(i_b);
        let w_a = row_half_width_px(i_a);
        let w_b = row_half_width_px(i_b);
        colors.push(lerp_color(c_a, c_b, frac, stroke_space));
        half_widths.push(w_a + frac * (w_b - w_a));
    }
    (colors, half_widths)
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::Color;
    use crate::geometry::{Point, Rect};
    use crate::plot::geom::{DirectScaleResolver, Raw};
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

    fn red() -> Color {
        Color::new([1.0, 0.0, 0.0, 1.0])
    }

    // ── de Boor evaluator ──

    #[test]
    fn de_boor_endpoint_clamping_4pt_cubic() {
        let ctrl = [
            Point::new(0.0, 0.0),
            Point::new(1.0, 2.0),
            Point::new(2.0, -1.0),
            Point::new(3.0, 1.0),
        ];
        let s0 = de_boor(&ctrl, 3, 0.0);
        let s1 = de_boor(&ctrl, 3, 1.0);
        assert!(
            (s0.x - 0.0).abs() < 1e-12 && (s0.y - 0.0).abs() < 1e-12,
            "S(0) != P_0"
        );
        assert!(
            (s1.x - 3.0).abs() < 1e-12 && (s1.y - 1.0).abs() < 1e-12,
            "S(1) != P_3"
        );
    }

    #[test]
    fn de_boor_4pt_cubic_matches_bezier_at_half() {
        // For n=4, d=3 the knot vector is [0,0,0,0,1,1,1,1] — the
        // spline is a cubic Bezier on (P_0, P_1, P_2, P_3). At t=0.5
        // the Bernstein basis evaluates to (1/8, 3/8, 3/8, 1/8).
        let ctrl = [
            Point::new(-2.0, 5.0),
            Point::new(7.0, 11.0),
            Point::new(13.0, -3.0),
            Point::new(4.0, 8.0),
        ];
        let s = de_boor(&ctrl, 3, 0.5);
        let exp_x = 0.125 * ctrl[0].x + 0.375 * ctrl[1].x + 0.375 * ctrl[2].x + 0.125 * ctrl[3].x;
        let exp_y = 0.125 * ctrl[0].y + 0.375 * ctrl[1].y + 0.375 * ctrl[2].y + 0.125 * ctrl[3].y;
        assert!(
            (s.x - exp_x).abs() < 1e-10,
            "S(0.5).x = {} vs {}",
            s.x,
            exp_x
        );
        assert!(
            (s.y - exp_y).abs() < 1e-10,
            "S(0.5).y = {} vs {}",
            s.y,
            exp_y
        );
    }

    #[test]
    fn de_boor_5pt_cubic_endpoint_clamping() {
        // n=5, d=3 → domain [0, 2]. Endpoints clamp to P_0 / P_4.
        let ctrl = [
            Point::new(0.0, 0.0),
            Point::new(1.0, 5.0),
            Point::new(2.0, -3.0),
            Point::new(3.0, 2.0),
            Point::new(4.0, 0.0),
        ];
        let s0 = de_boor(&ctrl, 3, 0.0);
        let s_end = de_boor(&ctrl, 3, 2.0);
        assert!(
            (s0.x - 0.0).abs() < 1e-12 && (s0.y - 0.0).abs() < 1e-12,
            "S(0) != P_0"
        );
        assert!(
            (s_end.x - 4.0).abs() < 1e-12 && (s_end.y - 0.0).abs() < 1e-12,
            "S(2) != P_4"
        );
    }

    // ── Adaptive flatten ──

    #[test]
    fn flatten_chord_error_stays_within_tolerance() {
        // A 6-control-point cubic that bends through high-curvature
        // regions in pixel space. After adaptive flatten every
        // intermediate point on the true curve at parameter values
        // between the produced samples must sit within
        // `CHORD_ERROR_PX` of the polyline approximation. We probe
        // 4 interior parameter values per output segment and check
        // perpendicular distance to that segment's chord.
        let ctrl_frac = [
            Point::new(0.05, 0.10),
            Point::new(0.20, 0.95),
            Point::new(0.40, 0.05),
            Point::new(0.60, 0.95),
            Point::new(0.80, 0.05),
            Point::new(0.95, 0.90),
        ];
        let panel = Rect::new(0.0, 0.0, 1000.0, 600.0);
        let resolver = DirectScaleResolver::new();
        let shapes = registry();
        let ctx = GeomContext::new(panel, 96.0, &shapes, &resolver);
        let samples = build_spline_flatten(&ctrl_frac, 3, panel, &ctx, InterpolationSpace::Domain);
        assert!(
            samples.len() >= 16,
            "expected adaptive flatten to produce >= 16 samples for a wiggly cubic, got {}",
            samples.len()
        );

        // Reconstruct the same parameter→pixel sampler used by the
        // flattener so we can probe interior points.
        let sample = |t: f64| -> Point {
            let p_frac = de_boor(&ctrl_frac, 3, t);
            let (px, py) = ctx
                .projection
                .project_to_panel_px(panel, &[p_frac.x, p_frac.y]);
            Point::new(px, py)
        };
        // Map a row position u back to spline parameter t. Inverse
        // of `to_u` in `build_spline_flatten`.
        let t_end = (ctrl_frac.len() - 3) as f64;
        let n_minus_1 = (ctrl_frac.len() - 1) as f64;
        let to_t = |u: f64| -> f64 { u * t_end / n_minus_1 };

        let mut max_err: f64 = 0.0;
        for window in samples.windows(2) {
            let (u0, p0) = window[0];
            let (u1, p1) = window[1];
            let t0 = to_t(u0);
            let t1 = to_t(u1);
            let chord = p1 - p0;
            let chord_len = chord.hypot();
            if chord_len < 1e-9 {
                continue;
            }
            for k in 1..5 {
                let t = t0 + (k as f64 / 5.0) * (t1 - t0);
                let p = sample(t);
                let off = p - p0;
                let cross = off.x * chord.y - off.y * chord.x;
                let err = cross.abs() / chord_len;
                if err > max_err {
                    max_err = err;
                }
            }
        }
        assert!(
            max_err < CHORD_ERROR_PX * 2.0,
            "max chord error {max_err} exceeds 2× tolerance ({})",
            CHORD_ERROR_PX * 2.0
        );
    }

    // ── build() validation ──

    #[test]
    #[should_panic(expected = "missing required channel")]
    fn builder_missing_x_panics() {
        BSplineGeom::builder()
            .set("y", vec![1.0_f64, 2.0, 3.0])
            .build();
    }

    #[test]
    #[should_panic(expected = "must be data, not constant")]
    fn builder_x_constant_panics() {
        BSplineGeom::builder()
            .set("x", 5.0)
            .set("y", vec![1.0_f64])
            .build();
    }

    #[test]
    #[should_panic(expected = "does not match")]
    fn builder_mismatched_lengths_panic() {
        BSplineGeom::builder()
            .set("x", vec![0.0_f64, 1.0, 2.0])
            .set("y", vec![0.0_f64, 1.0])
            .build();
    }

    #[test]
    fn builder_no_keys_synthesises_single_mark() {
        let g = BSplineGeom::builder()
            .set("x", vec![0.0_f64, 1.0, 2.0, 3.0])
            .set("y", vec![0.0_f64, 1.0, -1.0, 0.0])
            .build();
        assert_eq!(g.len(), 4);
        assert_eq!(g.mark_count(), 1);
    }

    #[test]
    fn builder_explicit_keys_define_marks() {
        let g = BSplineGeom::builder()
            .keys(vec!["A", "A", "A", "A", "B", "B", "B", "B"])
            .set("x", vec![0.0_f64, 1.0, 2.0, 3.0, 0.0, 1.0, 2.0, 3.0])
            .set("y", vec![0.0_f64, 1.0, -1.0, 0.0, 1.0, 2.0, 0.0, 1.0])
            .build();
        assert_eq!(g.len(), 8);
        assert_eq!(g.mark_count(), 2);
    }

    // ── Draw output ──

    #[test]
    fn draw_4pt_emits_one_stroke_op() {
        let mut g = BSplineGeom::builder()
            .set("x", Raw(vec![0.1_f64, 0.3, 0.7, 0.9]))
            .set("y", Raw(vec![0.5_f64, 0.9, 0.1, 0.5]))
            .set("stroke", red())
            .build();
        g.rebuild_diff_against_previous();
        let shapes = registry();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 200.0, 200.0), &shapes, &scales),
        );
        let strokes = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Stroke { .. }))
            .count();
        assert_eq!(strokes, 1);
    }

    #[test]
    fn stroke_opacity_overrides_the_stroke_alpha() {
        let panel = Rect::new(0.0, 0.0, 200.0, 200.0);
        let mut g = BSplineGeom::builder()
            .set("x", Raw(vec![0.1_f64, 0.3, 0.7, 0.9]))
            .set("y", Raw(vec![0.2_f64, 0.8, 0.4, 0.6]))
            .set("stroke", red())
            .set("stroke_opacity", 0.4_f64)
            .build();
        g.rebuild_diff_against_previous();
        let shapes = registry();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &ctx(panel, &shapes, &scales));
        let alphas: Vec<f32> = scene
            .ops
            .iter()
            .filter_map(|op| match op {
                Op::Stroke {
                    brush: crate::brush::Brush::Solid(c),
                    ..
                } => Some(c.components[3]),
                _ => None,
            })
            .collect();
        assert!(!alphas.is_empty(), "expected a stroked curve");
        for a in alphas {
            assert!((a - 0.4).abs() < 1e-6, "stroke alpha {a}");
        }
    }

    #[test]
    fn draw_passes_through_clamped_endpoints() {
        // With Raw channels (no scaling) and an identity Cartesian
        // projection, the first and last samples should land exactly
        // at the first and last control points in pixel space.
        let panel = Rect::new(0.0, 0.0, 200.0, 200.0);
        let xs = vec![0.1_f64, 0.3, 0.7, 0.9, 0.5];
        let ys = vec![0.2_f64, 0.8, 0.4, 0.6, 0.3];
        let mut g = BSplineGeom::builder()
            .set("x", Raw(xs.clone()))
            .set("y", Raw(ys.clone()))
            .set("stroke", red())
            .build();
        g.rebuild_diff_against_previous();
        let shapes = registry();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(&mut scene, &ctx(panel, &shapes, &scales));
        // Extract the path elements of the single stroke op and pull
        // the first MoveTo + the last LineTo target.
        let path = scene
            .ops
            .iter()
            .find_map(|op| match op {
                Op::Stroke { path, .. } => Some(path.clone()),
                _ => None,
            })
            .expect("stroke op");
        let els: Vec<_> = path.elements().to_vec();
        let first = match els.first() {
            Some(crate::path::PathEl::MoveTo(p)) => *p,
            _ => panic!("expected MoveTo"),
        };
        let last = els
            .iter()
            .rev()
            .find_map(|el| match el {
                crate::path::PathEl::LineTo(p) | crate::path::PathEl::MoveTo(p) => Some(*p),
                _ => None,
            })
            .expect("expected at least one LineTo");
        // P_0 in pixel space: (x_frac × 200, 200 − y_frac × 200).
        let exp_first = Point::new(xs[0] * 200.0, 200.0 - ys[0] * 200.0);
        let exp_last = Point::new(
            *xs.last().unwrap() * 200.0,
            200.0 - *ys.last().unwrap() * 200.0,
        );
        assert!(
            (first.x - exp_first.x).abs() < 1e-6 && (first.y - exp_first.y).abs() < 1e-6,
            "first sample {:?} != P_0 {:?}",
            first,
            exp_first
        );
        assert!(
            (last.x - exp_last.x).abs() < 1e-6 && (last.y - exp_last.y).abs() < 1e-6,
            "last sample {:?} != P_{{n-1}} {:?}",
            last,
            exp_last
        );
    }

    #[test]
    fn ribbon_mode_endpoint_markers_turn_with_the_curve() {
        // `angle` rotates the mark as a rigid body, so a marker stamped
        // at a terminal has to carry the same transform the mesh does —
        // otherwise the curve turns and its arrowheads stay put.
        let build = |angle: f64| {
            let mut g = BSplineGeom::builder()
                .keys(vec!["A"; 4])
                .set("x", Raw(vec![0.1_f64, 0.3, 0.7, 0.9]))
                .set("y", Raw(vec![0.5_f64, 0.8, 0.2, 0.5]))
                // Varying linewidth is what selects the mesh path.
                .set("linewidth", vec![4.0_f64, 8.0, 12.0, 6.0])
                .set("stroke", red())
                .set("start_marker", "circle")
                .set("angle", angle)
                .build();
            g.rebuild_diff_against_previous();
            let shapes = registry();
            let scales = DirectScaleResolver::new();
            let mut scene = RecordingScene::default();
            g.draw(
                &mut scene,
                &ctx(Rect::new(0.0, 0.0, 200.0, 200.0), &shapes, &scales),
            );
            scene
                .ops
                .iter()
                .find_map(|op| match op {
                    Op::Fill { transform, .. } => Some(transform.translation()),
                    _ => None,
                })
                .expect("the start marker is filled")
        };
        let upright = build(0.0);
        let turned = build(std::f64::consts::FRAC_PI_2);
        let moved = (turned.x - upright.x).hypot(turned.y - upright.y);
        assert!(
            moved > 1.0,
            "a quarter-turn should move the start marker; it sat at {upright:?} either way"
        );
    }

    #[test]
    fn draw_per_vertex_linewidth_upgrades_to_mesh() {
        let mut g = BSplineGeom::builder()
            .keys(vec!["A"; 4])
            .set("x", Raw(vec![0.1_f64, 0.3, 0.7, 0.9]))
            .set("y", Raw(vec![0.5_f64, 0.8, 0.2, 0.5]))
            .set("linewidth", vec![4.0_f64, 8.0, 12.0, 6.0])
            .set("stroke", red())
            .build();
        g.rebuild_diff_against_previous();
        let shapes = registry();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 200.0, 200.0), &shapes, &scales),
        );
        let strokes = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Stroke { .. }))
            .count();
        let meshes = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::DrawMesh { .. }))
            .count();
        assert_eq!(strokes, 0, "ribbon-mode upgrade bypasses Op::Stroke");
        assert_eq!(meshes, 1, "expected one mesh op");
    }

    #[test]
    fn zero_linewidth_at_first_row_still_renders_ribbon() {
        // A `linewidth` range starting at 0 puts a zero on the mark's
        // first control point. The rest of the mark is wide, so the
        // ribbon renders with a pinch at the start rather than
        // vanishing.
        let mut g = BSplineGeom::builder()
            .keys(vec!["A"; 4])
            .set("x", Raw(vec![0.1_f64, 0.3, 0.7, 0.9]))
            .set("y", Raw(vec![0.5_f64, 0.8, 0.2, 0.5]))
            .set("linewidth", vec![0.0_f64, 10.0, 20.0, 30.0])
            .set("stroke", red())
            .build();
        g.rebuild_diff_against_previous();
        let shapes = registry();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 200.0, 200.0), &shapes, &scales),
        );
        let meshes = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::DrawMesh { .. }))
            .count();
        assert_eq!(meshes, 1, "expected one mesh op");
    }

    #[test]
    fn all_zero_linewidth_draws_nothing() {
        // No within-mark variance and a non-positive width → nothing to
        // stroke, and no ribbon upgrade to reinterpret it.
        let mut g = BSplineGeom::builder()
            .keys(vec!["A"; 4])
            .set("x", Raw(vec![0.1_f64, 0.3, 0.7, 0.9]))
            .set("y", Raw(vec![0.5_f64, 0.8, 0.2, 0.5]))
            .set("linewidth", 0.0_f64)
            .set("stroke", red())
            .build();
        g.rebuild_diff_against_previous();
        let shapes = registry();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 200.0, 200.0), &shapes, &scales),
        );
        assert!(scene.ops.is_empty(), "expected no ops, got {:?}", scene.ops);
    }

    #[test]
    fn draw_two_control_points_renders_as_segment() {
        // n_ctrl = 2 < degree + 1 = 4 → polyline fallback. Should
        // still emit one stroke op (a straight line).
        let mut g = BSplineGeom::builder()
            .set("x", Raw(vec![0.1_f64, 0.9]))
            .set("y", Raw(vec![0.5_f64, 0.5]))
            .set("stroke", red())
            .build();
        g.rebuild_diff_against_previous();
        let shapes = registry();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 200.0, 200.0), &shapes, &scales),
        );
        let strokes = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Stroke { .. }))
            .count();
        assert_eq!(strokes, 1);
    }

    #[test]
    fn draw_single_control_point_skips() {
        let mut g = BSplineGeom::builder()
            .set("x", Raw(vec![0.5_f64]))
            .set("y", Raw(vec![0.5_f64]))
            .set("stroke", red())
            .build();
        g.rebuild_diff_against_previous();
        let shapes = registry();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 200.0, 200.0), &shapes, &scales),
        );
        let strokes = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Stroke { .. }))
            .count();
        assert_eq!(strokes, 0);
    }

    #[test]
    fn draw_emits_end_marker_after_stroke() {
        // start_marker BEFORE stroke; end_marker AFTER stroke — same
        // path order as LineGeom. We check by op index in the
        // recording scene.
        let mut g = BSplineGeom::builder()
            .set("x", Raw(vec![0.1_f64, 0.3, 0.7, 0.9]))
            .set("y", Raw(vec![0.5_f64, 0.5, 0.5, 0.5]))
            .set("stroke", red())
            .set("start_marker", "circle")
            .set("end_marker", "circle")
            .build();
        g.rebuild_diff_against_previous();
        let shapes = registry();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 200.0, 200.0), &shapes, &scales),
        );
        let stroke_idx = scene
            .ops
            .iter()
            .position(|op| matches!(op, Op::Stroke { .. }))
            .expect("stroke op");
        let first_fill_idx = scene
            .ops
            .iter()
            .position(|op| matches!(op, Op::Fill { .. }))
            .expect("first marker fill");
        let last_fill_idx = scene
            .ops
            .iter()
            .rposition(|op| matches!(op, Op::Fill { .. }))
            .expect("last marker fill");
        assert!(
            first_fill_idx < stroke_idx,
            "start marker should precede stroke"
        );
        assert!(
            last_fill_idx > stroke_idx,
            "end marker should follow stroke"
        );
    }
}
