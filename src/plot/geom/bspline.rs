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
//! - `"alpha"` — overrides alpha of `"stroke"` (per-mark).
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
//!   [`endpoint_outward`](super::resolve::endpoint_outward): when the
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
use crate::plot::diff::{diff_columns, diff_positional, KeyIndex};
use crate::plot::scale::Scale;
use crate::plot::value::DataColumn;
use crate::primitives::{clip_polyline_with_attrs, polyline_ribbon_full, EndClip, RibbonOptions};
use crate::scene::SceneBuilder;

use super::marks::{build_marks_from_column, unique_values_at_first_rows, MarkSlot};
use super::outline::{draw_curve_outline, EndpointMarker, OutlineSpec};
use super::resolve::{
    apply_per_row_offsets, auto_endpoint_clip_pt, channel_varies_across, emit_endpoint_marker,
    endpoint_outward, override_alpha, pt_to_px, resolve_bool_channel_or, resolve_cap_channel,
    resolve_color_channel, resolve_color_channel_or_theme, resolve_join_channel,
    resolve_linetype_channel, resolve_number_channel, resolve_number_channel_or, resolve_pick_id,
    resolve_position, resolve_str_channel_or, ChannelBind,
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
    ("alpha", ExpectedOutput::Numbers),
    ("linewidth", ExpectedOutput::Numbers),
    ("linetype", ExpectedOutput::Linetypes),
    ("dash_offset", ExpectedOutput::Numbers),
    ("cap", ExpectedOutput::Strings),
    ("join", ExpectedOutput::Strings),
    ("clip_start_radius", ExpectedOutput::Numbers),
    ("clip_end_radius", ExpectedOutput::Numbers),
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
    fill: ChannelBind<'a>,
    stroke: ChannelBind<'a>,
    alpha: ChannelBind<'a>,
    linewidth: ChannelBind<'a>,
    linetype: ChannelBind<'a>,
    dash_offset: ChannelBind<'a>,
    cap: ChannelBind<'a>,
    join: ChannelBind<'a>,
    clip_start_radius: ChannelBind<'a>,
    clip_end_radius: ChannelBind<'a>,
    pick_id: ChannelBind<'a>,
    start_marker: ChannelBind<'a>,
    end_marker: ChannelBind<'a>,
    start_marker_size: ChannelBind<'a>,
    end_marker_size: ChannelBind<'a>,
    start_marker_fill: ChannelBind<'a>,
    end_marker_fill: ChannelBind<'a>,
    start_marker_invert: ChannelBind<'a>,
    end_marker_invert: ChannelBind<'a>,
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
            alpha: b("alpha"),
            linewidth: b("linewidth"),
            linetype: b("linetype"),
            dash_offset: b("dash_offset"),
            cap: b("cap"),
            join: b("join"),
            clip_start_radius: b("clip_start_radius"),
            clip_end_radius: b("clip_end_radius"),
            pick_id: b("pick_id"),
            start_marker: b("start_marker"),
            end_marker: b("end_marker"),
            start_marker_size: b("start_marker_size"),
            end_marker_size: b("end_marker_size"),
            start_marker_fill: b("start_marker_fill"),
            end_marker_fill: b("end_marker_fill"),
            start_marker_invert: b("start_marker_invert"),
            end_marker_invert: b("end_marker_invert"),
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
                    "BSplineGeom",
                );
                let next_unique = unique_values_at_first_rows(
                    next_col,
                    next_marks.iter().map(|m| m.first_row),
                    "BSplineGeom",
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
        fill: ChannelBind {
            ch: fill_ch,
            scale: fill_scale,
        },
        stroke: ChannelBind {
            ch: stroke_ch,
            scale: stroke_scale,
        },
        alpha: ChannelBind {
            ch: alpha_ch,
            scale: alpha_scale,
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
        clip_start_radius:
            ChannelBind {
                ch: clip_start_radius_ch,
                scale: clip_start_radius_scale,
            },
        clip_end_radius:
            ChannelBind {
                ch: clip_end_radius_ch,
                scale: clip_end_radius_scale,
            },
        pick_id:
            ChannelBind {
                ch: pick_id_ch,
                scale: pick_id_scale,
            },
        start_marker:
            ChannelBind {
                ch: start_marker_ch,
                scale: start_marker_scale,
            },
        end_marker:
            ChannelBind {
                ch: end_marker_ch,
                scale: end_marker_scale,
            },
        start_marker_size:
            ChannelBind {
                ch: start_marker_size_ch,
                scale: start_marker_size_scale,
            },
        end_marker_size:
            ChannelBind {
                ch: end_marker_size_ch,
                scale: end_marker_size_scale,
            },
        start_marker_fill:
            ChannelBind {
                ch: start_marker_fill_ch,
                scale: start_marker_fill_scale,
            },
        end_marker_fill:
            ChannelBind {
                ch: end_marker_fill_ch,
                scale: end_marker_fill_scale,
            },
        start_marker_invert:
            ChannelBind {
                ch: start_marker_invert_ch,
                scale: start_marker_invert_scale,
            },
        end_marker_invert:
            ChannelBind {
                ch: end_marker_invert_ch,
                scale: end_marker_invert_scale,
            },
    } = dc;

    let i0 = mark.first_row;

    let stroke_color = override_alpha(
        resolve_color_channel_or_theme(
            stroke_ch,
            stroke_scale,
            i0,
            ctx.theme.geom.bspline.stroke.as_ref(),
            &ctx.theme.palette,
        ),
        resolve_number_channel(alpha_ch, alpha_scale, i0),
    );
    let stroke_color = match stroke_color {
        Some(c) => c,
        None => return,
    };

    let linewidth_pt = resolve_number_channel_or(
        linewidth_ch,
        linewidth_scale,
        i0,
        ctx.theme.geom.bspline.linewidth_pt,
    );
    let linewidth_px = pt_to_px(linewidth_pt, ctx.dpi);
    if !linewidth_px.is_finite() || linewidth_px <= 0.0 {
        return;
    }

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

    let dash_pattern_pt = resolve_linetype_channel(linetype_ch, linetype_scale, i0);
    let dash_offset_pt = resolve_number_channel_or(dash_offset_ch, dash_offset_scale, i0, 0.0);
    let cap = resolve_cap_channel(cap_ch, cap_scale, i0, ctx.theme.geom.bspline.cap);
    let join = resolve_join_channel(join_ch, join_scale, i0, ctx.theme.geom.bspline.join);
    let marker_fill = resolve_color_channel(fill_ch, fill_scale, i0).unwrap_or(stroke_color);
    let pick = resolve_pick_id(pick_id_ch, pick_id_scale, i0);

    // ── Control polygon in channel-fraction space. ──
    //
    // Non-finite rows are dropped silently (matches LineGeom /
    // PolygonGeom): the spline closes around what's left rather
    // than splitting.
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
        || channel_varies_across(alpha_ch, alpha_scale, &mark.rows);
    let ribbon_mode = dash_pattern_pt.is_empty() && (linewidth_varies || stroke_varies);

    // ── Endpoint-marker constants (per-mark). ──
    //
    // Resolved BEFORE the clip calc so the auto-clip
    // contribution can fold in below.
    let start_name = resolve_str_channel_or(start_marker_ch, start_marker_scale, i0, "");
    let end_name = resolve_str_channel_or(end_marker_ch, end_marker_scale, i0, "");
    let default_marker_size_pt = 3.0 * linewidth_pt;
    let start_marker_size_pt = resolve_number_channel_or(
        start_marker_size_ch,
        start_marker_size_scale,
        i0,
        default_marker_size_pt,
    );
    let end_marker_size_pt = resolve_number_channel_or(
        end_marker_size_ch,
        end_marker_size_scale,
        i0,
        default_marker_size_pt,
    );
    let start_invert =
        resolve_bool_channel_or(start_marker_invert_ch, start_marker_invert_scale, i0, false);
    let end_invert =
        resolve_bool_channel_or(end_marker_invert_ch, end_marker_invert_scale, i0, false);

    // ── End-clip (per-mark). ──
    //
    // User-supplied `clip_*_radius` covers the "trim to a node
    // boundary" use case (graph layouts, leaving breathing
    // room next to a data point, etc.). On top of that we
    // automatically add the forward extent of any endpoint
    // marker so the marker's tip lands at the user's clip
    // boundary (or the original endpoint when no user clip
    // is set) without the user having to compute the marker
    // geometry themselves.
    //
    // Ribbon mode threads per-vertex widths / colours through
    // `clip_polyline_with_attrs` so the synthesised
    // intersection vertex picks up lerped attrs.
    let user_clip_start_pt =
        resolve_number_channel_or(clip_start_radius_ch, clip_start_radius_scale, i0, 0.0);
    let user_clip_end_pt =
        resolve_number_channel_or(clip_end_radius_ch, clip_end_radius_scale, i0, 0.0);
    let auto_clip_start_pt =
        auto_endpoint_clip_pt(&start_name, start_marker_size_pt, start_invert, ctx.shapes);
    let auto_clip_end_pt =
        auto_endpoint_clip_pt(&end_name, end_marker_size_pt, end_invert, ctx.shapes);
    let clip_start_pt = user_clip_start_pt + auto_clip_start_pt;
    let clip_end_pt = user_clip_end_pt + auto_clip_end_pt;

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

    if ribbon_mode {
        // ── Ribbon-mode path: per-vertex tessellated mesh. Clip threads
        // widths / colors through so the post-clip mesh stays
        // attr-aligned.
        let (ribbon_colors, ribbon_half_widths) = build_ribbon_attrs(
            &samples,
            &ctrl_rows,
            stroke_color,
            linewidth_pt,
            ctx.dpi,
            stroke_ch,
            stroke_scale,
            alpha_ch,
            alpha_scale,
            linewidth_ch,
            linewidth_scale,
        );
        let (clipped_points, clipped_colors, clipped_half_widths) =
            if clip_start_pt > 0.0 || clip_end_pt > 0.0 {
                let start_clip = (clip_start_pt > 0.0).then(|| EndClip::Circle {
                    center: sample_points[0],
                    radius: pt_to_px(clip_start_pt, ctx.dpi),
                });
                let end_clip = (clip_end_pt > 0.0).then(|| EndClip::Circle {
                    center: *sample_points.last().unwrap(),
                    radius: pt_to_px(clip_end_pt, ctx.dpi),
                });
                let (p, w, c) = clip_polyline_with_attrs(
                    &sample_points,
                    &ribbon_half_widths,
                    &ribbon_colors,
                    start_clip,
                    end_clip,
                );
                (p, c, w)
            } else {
                (sample_points.clone(), ribbon_colors, ribbon_half_widths)
            };
        if clipped_points.len() < 2 {
            return;
        }

        let marker_outline_px = linewidth_px.max(pt_to_px(0.5, ctx.dpi));

        if !start_name.is_empty() {
            let size_px = pt_to_px(start_marker_size_pt, ctx.dpi);
            let fill = resolve_color_channel(start_marker_fill_ch, start_marker_fill_scale, i0)
                .unwrap_or(marker_fill);
            let outward =
                endpoint_outward(&clipped_points, &sample_points, true, clip_start_pt > 0.0);
            emit_endpoint_marker(
                scene,
                clipped_points[0],
                outward,
                start_invert,
                &start_name,
                size_px,
                fill,
                stroke_color,
                marker_outline_px,
                Affine::IDENTITY,
                ctx.shapes,
                pick,
            );
        }

        let opts = RibbonOptions {
            half_width: 0.0,
            cap,
            join,
            miter_limit: 4.0,
        };
        let mesh = polyline_ribbon_full(
            &clipped_points,
            Some(&clipped_colors),
            Some(&clipped_half_widths),
            &opts,
        );
        scene.draw_mesh(&mesh, Affine::IDENTITY, pick);

        if !end_name.is_empty() {
            let size_px = pt_to_px(end_marker_size_pt, ctx.dpi);
            let fill = resolve_color_channel(end_marker_fill_ch, end_marker_fill_scale, i0)
                .unwrap_or(marker_fill);
            let outward =
                endpoint_outward(&clipped_points, &sample_points, false, clip_end_pt > 0.0);
            let placement = *clipped_points.last().unwrap();
            emit_endpoint_marker(
                scene,
                placement,
                outward,
                end_invert,
                &end_name,
                size_px,
                fill,
                stroke_color,
                marker_outline_px,
                Affine::IDENTITY,
                ctx.shapes,
                pick,
            );
        }
        return;
    }

    // ── Non-ribbon path: build an outline spec and delegate to the
    // shared `draw_curve_outline` helper, which handles endpoint clip,
    // polyline path construction, start marker, stroke (fast path or
    // dashed-with-markers walker), and end marker.
    let start_marker_fill_resolved =
        resolve_color_channel(start_marker_fill_ch, start_marker_fill_scale, i0);
    let end_marker_fill_resolved =
        resolve_color_channel(end_marker_fill_ch, end_marker_fill_scale, i0);
    let spec = OutlineSpec {
        stroke_color,
        linewidth_pt,
        dash_pattern_pt,
        dash_offset_pt,
        cap,
        join,
        marker_fill,
        user_clip_start_pt,
        user_clip_end_pt,
        start_marker: EndpointMarker {
            name: start_name,
            size_pt: start_marker_size_pt,
            fill: start_marker_fill_resolved,
            invert: start_invert,
        },
        end_marker: EndpointMarker {
            name: end_name,
            size_pt: end_marker_size_pt,
            fill: end_marker_fill_resolved,
            invert: end_invert,
        },
        pick,
        xform: Affine::IDENTITY,
        corner_rounding: None,
    };
    draw_curve_outline(scene, ctx, &sample_points, &spec);
}

// ─── Ribbon-mode per-sample attributes ───────────────────────────────────────

/// Build per-sample (color, half-width) for ribbon-mode dispatch.
/// Each sample carries a row position `u` in `[0, n_ctrl − 1]`
/// (computed by [`build_spline_flatten`] or the polyline fallback);
/// per-row stroke / linewidth / alpha values lerp linearly between
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
    alpha_ch: Option<&Channel>,
    alpha_scale: Option<&crate::plot::scale::Scale>,
    linewidth_ch: Option<&Channel>,
    linewidth_scale: Option<&crate::plot::scale::Scale>,
) -> (Vec<Color>, Vec<f64>) {
    let n_rows = ctrl_rows.len();
    let row_color = |i: usize| -> Color {
        override_alpha(
            resolve_color_channel(stroke_ch, stroke_scale, ctrl_rows[i]),
            resolve_number_channel(alpha_ch, alpha_scale, ctrl_rows[i]),
        )
        .unwrap_or(fallback_stroke)
    };
    let row_half_width_px = |i: usize| -> f64 {
        let w_pt =
            resolve_number_channel_or(linewidth_ch, linewidth_scale, ctrl_rows[i], linewidth_pt);
        pt_to_px(w_pt, dpi) * 0.5
    };
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
        colors.push(lerp_color(c_a, c_b, frac));
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
