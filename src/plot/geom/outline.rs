//! Per-curve outline emission helper shared across geoms that stroke
//! multiple polyline curves per mark.
//!
//! Each curve gets its own resolved channel set (stroke colour,
//! linewidth, dash pattern, cap / join, endpoint markers, endpoint
//! clipping). The helper composes the existing primitives in
//! [`crate::primitives`] and the resolution helpers in
//! [`super::resolve`] to emit:
//!
//! 1. The start endpoint marker (before the stroke, so a
//!    self-intersecting polyline's later segments draw over it).
//! 2. The stroked polyline — fast path through [`SceneBuilder::stroke`]
//!    when the linetype pattern has no markers; otherwise
//!    [`draw_linetype_with_markers`] for inline dashes + marker stamps.
//! 3. The end endpoint marker (after the stroke, so it sits on top of
//!    the termination).
//!
//! This mirrors `LineGeom`'s draw flow but factored to take a
//! pre-resolved [`OutlineSpec`] and a pre-built polyline, so multi-curve
//! geoms (`RibbonGeom`, `RibbonBSplineGeom`) can call it once per curve
//! without duplicating ~150 lines of orchestration.

use crate::brush::Brush;
use crate::color::Color;
use crate::geometry::{Affine, Point};
use crate::pick::PickId;
use crate::plot::scale::Scale;
use crate::plot::value::LinetypeStep;
use crate::primitives::{clip_polyline, polyline, EndClip, PolylineOptions, PolylineSampler};
use crate::scene::SceneBuilder;
use crate::stroke::{Cap, Join, Stroke};
use std::collections::HashMap;
use std::sync::Arc;

use super::linetype;
use super::resolve::{
    auto_endpoint_clip_pt, build_stroke_for_pattern, draw_linetype_with_markers,
    emit_endpoint_marker, endpoint_outward, override_alpha, pt_to_px, resolve_bool_channel_or,
    resolve_cap_channel, resolve_color_channel, resolve_join_channel, resolve_linetype_channel,
    resolve_number_channel, resolve_number_channel_or, resolve_str_channel_or,
};
use super::{Channel, GeomContext};

// Style defaults (linewidth, cap, join) for outlines come from the
// caller-supplied `ShapeDefaults` so RibbonGeom and RibbonBSplineGeom
// (which both call into here) can carry independent defaults under
// `theme.geom.ribbon` vs `theme.geom.ribbon_bspline`.

/// Channel handles for one curve's full LineGeom-style outline surface,
/// keyed off a suffix (`""` for curve A, `"2"` for curve B in a ribbon
/// geom).
pub(crate) struct OutlineChannels<'a> {
    pub stroke: Option<&'a Channel>,
    pub linewidth: Option<&'a Channel>,
    pub linetype: Option<&'a Channel>,
    pub dash_offset: Option<&'a Channel>,
    pub cap: Option<&'a Channel>,
    pub join: Option<&'a Channel>,
    pub clip_start: Option<&'a Channel>,
    pub clip_end: Option<&'a Channel>,
    pub start_marker: Option<&'a Channel>,
    pub end_marker: Option<&'a Channel>,
    pub start_marker_size: Option<&'a Channel>,
    pub end_marker_size: Option<&'a Channel>,
    pub start_marker_fill: Option<&'a Channel>,
    pub end_marker_fill: Option<&'a Channel>,
    pub start_marker_invert: Option<&'a Channel>,
    pub end_marker_invert: Option<&'a Channel>,
}

impl<'a> OutlineChannels<'a> {
    /// Look up each outline channel by name, appending `suffix` to the
    /// base channel name. `suffix = ""` reads curve A's channels;
    /// `suffix = "2"` reads curve B's.
    pub(crate) fn from_map(channels: &'a HashMap<String, Channel>, suffix: &str) -> Self {
        let g = |base: &str| channels.get(&format!("{base}{suffix}"));
        OutlineChannels {
            stroke: g("stroke"),
            linewidth: g("linewidth"),
            linetype: g("linetype"),
            dash_offset: g("dash_offset"),
            cap: g("cap"),
            join: g("join"),
            clip_start: g("clip_start_radius"),
            clip_end: g("clip_end_radius"),
            start_marker: g("start_marker"),
            end_marker: g("end_marker"),
            start_marker_size: g("start_marker_size"),
            end_marker_size: g("end_marker_size"),
            start_marker_fill: g("start_marker_fill"),
            end_marker_fill: g("end_marker_fill"),
            start_marker_invert: g("start_marker_invert"),
            end_marker_invert: g("end_marker_invert"),
        }
    }
}

/// Scale references for one curve's outline surface, keyed off the same
/// suffix as the matching [`OutlineChannels`].
pub(crate) struct OutlineScales<'a> {
    pub stroke: Option<&'a Scale>,
    pub linewidth: Option<&'a Scale>,
    pub linetype: Option<&'a Scale>,
    pub dash_offset: Option<&'a Scale>,
    pub cap: Option<&'a Scale>,
    pub join: Option<&'a Scale>,
    pub clip_start: Option<&'a Scale>,
    pub clip_end: Option<&'a Scale>,
    pub start_marker: Option<&'a Scale>,
    pub end_marker: Option<&'a Scale>,
    pub start_marker_size: Option<&'a Scale>,
    pub end_marker_size: Option<&'a Scale>,
    pub start_marker_fill: Option<&'a Scale>,
    pub end_marker_fill: Option<&'a Scale>,
    pub start_marker_invert: Option<&'a Scale>,
    pub end_marker_invert: Option<&'a Scale>,
}

impl<'a> OutlineScales<'a> {
    /// Look up each outline scale by channel name, appending `suffix` to
    /// the base channel name.
    pub(crate) fn from_ctx(ctx: &'a GeomContext<'_>, suffix: &str) -> Self {
        let g = |base: &str| ctx.scale_for(&format!("{base}{suffix}"));
        OutlineScales {
            stroke: g("stroke"),
            linewidth: g("linewidth"),
            linetype: g("linetype"),
            dash_offset: g("dash_offset"),
            cap: g("cap"),
            join: g("join"),
            clip_start: g("clip_start_radius"),
            clip_end: g("clip_end_radius"),
            start_marker: g("start_marker"),
            end_marker: g("end_marker"),
            start_marker_size: g("start_marker_size"),
            end_marker_size: g("end_marker_size"),
            start_marker_fill: g("start_marker_fill"),
            end_marker_fill: g("end_marker_fill"),
            start_marker_invert: g("start_marker_invert"),
            end_marker_invert: g("end_marker_invert"),
        }
    }
}

/// Resolve a curve's full outline spec from its [`OutlineChannels`] /
/// [`OutlineScales`] handles at the mark's first row.
///
/// Returns `None` when no stroke colour is bound (no outline to draw).
/// `alpha_ch` / `alpha_scale` supply the shared per-mark alpha that
/// overrides each colour channel's resolved alpha; pass `None` for both
/// when there is no shared alpha channel.
#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_outline_spec(
    ctx: &GeomContext<'_>,
    defaults: &crate::plot::theme::ShapeDefaults,
    ch: &OutlineChannels<'_>,
    sc: &OutlineScales<'_>,
    alpha_ch: Option<&Channel>,
    alpha_scale: Option<&Scale>,
    i0: usize,
    pick: PickId,
) -> Option<OutlineSpec> {
    let _ = ctx;
    let stroke_color = override_alpha(
        resolve_color_channel(ch.stroke, sc.stroke, i0),
        resolve_number_channel(alpha_ch, alpha_scale, i0),
    )?;
    let linewidth_pt =
        resolve_number_channel_or(ch.linewidth, sc.linewidth, i0, defaults.linewidth_pt);
    let dash_pattern_pt = resolve_linetype_channel(ch.linetype, sc.linetype, i0);
    let dash_offset_pt = resolve_number_channel_or(ch.dash_offset, sc.dash_offset, i0, 0.0);
    let cap = resolve_cap_channel(ch.cap, sc.cap, i0, defaults.cap);
    let join = resolve_join_channel(ch.join, sc.join, i0, defaults.join);
    let user_clip_start_pt = resolve_number_channel_or(ch.clip_start, sc.clip_start, i0, 0.0);
    let user_clip_end_pt = resolve_number_channel_or(ch.clip_end, sc.clip_end, i0, 0.0);

    let default_marker_size_pt = 3.0 * linewidth_pt;
    let start_marker_name = resolve_str_channel_or(ch.start_marker, sc.start_marker, i0, "");
    let end_marker_name = resolve_str_channel_or(ch.end_marker, sc.end_marker, i0, "");
    let start_marker_size_pt = resolve_number_channel_or(
        ch.start_marker_size,
        sc.start_marker_size,
        i0,
        default_marker_size_pt,
    );
    let end_marker_size_pt = resolve_number_channel_or(
        ch.end_marker_size,
        sc.end_marker_size,
        i0,
        default_marker_size_pt,
    );
    let start_marker_fill = resolve_color_channel(ch.start_marker_fill, sc.start_marker_fill, i0);
    let end_marker_fill = resolve_color_channel(ch.end_marker_fill, sc.end_marker_fill, i0);
    let start_marker_invert =
        resolve_bool_channel_or(ch.start_marker_invert, sc.start_marker_invert, i0, false);
    let end_marker_invert =
        resolve_bool_channel_or(ch.end_marker_invert, sc.end_marker_invert, i0, false);

    Some(OutlineSpec {
        stroke_color,
        linewidth_pt,
        dash_pattern_pt,
        dash_offset_pt,
        cap,
        join,
        // Default marker fill = stroke colour; per-endpoint override
        // happens via the `EndpointMarker::fill` field below.
        marker_fill: stroke_color,
        user_clip_start_pt,
        user_clip_end_pt,
        start_marker: EndpointMarker {
            name: start_marker_name,
            size_pt: start_marker_size_pt,
            fill: start_marker_fill,
            invert: start_marker_invert,
        },
        end_marker: EndpointMarker {
            name: end_marker_name,
            size_pt: end_marker_size_pt,
            fill: end_marker_fill,
            invert: end_marker_invert,
        },
        pick,
    })
}

/// Per-mark per-curve outline configuration.
///
/// Built once per curve from the per-mark resolved channels, then handed
/// to [`draw_curve_outline`] alongside the curve's pre-built polyline.
pub(crate) struct OutlineSpec {
    /// Resolved stroke colour (with alpha folded in).
    pub stroke_color: Color,
    /// Stroke width in pt. Pixel conversion happens inside the helper.
    pub linewidth_pt: f64,
    /// Dash pattern (`LinetypeStep` sequence). Empty = solid.
    pub dash_pattern_pt: Arc<[LinetypeStep]>,
    /// Phase shift along the dash pattern in pt.
    pub dash_offset_pt: f64,
    /// Stroke end style.
    pub cap: Cap,
    /// Stroke vertex style.
    pub join: Join,
    /// Default marker interior colour. Each endpoint's
    /// [`EndpointMarker::fill`] overrides this when set; otherwise this
    /// colour is used (typically the curve's stroke colour).
    pub marker_fill: Color,
    /// User-supplied start-side clip radius in pt. The marker's forward
    /// extent is added automatically so the marker tip lands at the
    /// user's clip boundary.
    pub user_clip_start_pt: f64,
    /// User-supplied end-side clip radius in pt.
    pub user_clip_end_pt: f64,
    pub start_marker: EndpointMarker,
    pub end_marker: EndpointMarker,
    pub pick: PickId,
}

/// Endpoint marker configuration for one side of a curve.
pub(crate) struct EndpointMarker {
    /// Shape name registered in the [`ShapeRegistry`]. Empty disables.
    pub name: String,
    /// Marker size in pt. Conventionally `3 * linewidth_pt`.
    pub size_pt: f64,
    /// Marker interior colour. `None` falls back to
    /// [`OutlineSpec::marker_fill`].
    pub fill: Option<Color>,
    /// Flip the outward direction (mirror the shape across the curve's
    /// terminal tangent). Used for asymmetric non-arrow shapes.
    pub invert: bool,
}

impl Default for EndpointMarker {
    fn default() -> Self {
        EndpointMarker {
            name: String::new(),
            size_pt: 0.0,
            fill: None,
            invert: false,
        }
    }
}

/// Stroke a pre-built polyline curve under the given outline spec, with
/// endpoint markers stamped before / after the stroke per Phase C.5
/// path-order convention.
///
/// `points` is the curve's polyline in panel pixel space, already
/// projected and densified to follow any non-linear projection's
/// geodesic. The helper applies endpoint clipping (user clip + auto
/// clip from marker geometry), builds the kurbo path, emits the start
/// marker, dispatches the stroke (fast path or dashed-with-markers
/// walker), and emits the end marker.
///
/// No-op when `points.len() < 2`, linewidth is non-finite or
/// non-positive, or the post-clip polyline has fewer than two vertices.
pub(crate) fn draw_curve_outline(
    scene: &mut dyn SceneBuilder,
    ctx: &GeomContext<'_>,
    points: &[Point],
    spec: &OutlineSpec,
) {
    if points.len() < 2 {
        return;
    }

    let linewidth_px = pt_to_px(spec.linewidth_pt, ctx.dpi);
    if !linewidth_px.is_finite() || linewidth_px <= 0.0 {
        return;
    }

    let auto_clip_start_pt = auto_endpoint_clip_pt(
        &spec.start_marker.name,
        spec.start_marker.size_pt,
        spec.start_marker.invert,
        ctx.shapes,
    );
    let auto_clip_end_pt = auto_endpoint_clip_pt(
        &spec.end_marker.name,
        spec.end_marker.size_pt,
        spec.end_marker.invert,
        ctx.shapes,
    );
    let clip_start_pt = spec.user_clip_start_pt + auto_clip_start_pt;
    let clip_end_pt = spec.user_clip_end_pt + auto_clip_end_pt;

    let clipped: Vec<Point> = if clip_start_pt > 0.0 || clip_end_pt > 0.0 {
        let start = (clip_start_pt > 0.0).then(|| EndClip::Circle {
            center: points[0],
            radius: pt_to_px(clip_start_pt, ctx.dpi),
        });
        let end = (clip_end_pt > 0.0).then(|| EndClip::Circle {
            center: *points.last().unwrap(),
            radius: pt_to_px(clip_end_pt, ctx.dpi),
        });
        clip_polyline(points, start, end)
    } else {
        points.to_vec()
    };
    if clipped.len() < 2 {
        return;
    }

    let path = polyline(&clipped, PolylineOptions::default());
    let has_markers = !linetype::is_marker_free(&spec.dash_pattern_pt);
    let marker_outline_px = linewidth_px.max(pt_to_px(0.5, ctx.dpi));
    let xform = Affine::IDENTITY;

    if !spec.start_marker.name.is_empty() {
        let size_px = pt_to_px(spec.start_marker.size_pt, ctx.dpi);
        let fill = spec.start_marker.fill.unwrap_or(spec.marker_fill);
        let outward = endpoint_outward(&clipped, points, true, clip_start_pt > 0.0);
        emit_endpoint_marker(
            scene,
            clipped[0],
            outward,
            spec.start_marker.invert,
            &spec.start_marker.name,
            size_px,
            fill,
            spec.stroke_color,
            marker_outline_px,
            xform,
            ctx.shapes,
            spec.pick,
        );
    }

    if !has_markers {
        let stroke_spec = build_stroke_for_pattern(
            linewidth_px,
            spec.cap,
            spec.join,
            &spec.dash_pattern_pt,
            spec.dash_offset_pt,
            spec.linewidth_pt,
            ctx.dpi,
        );
        scene.stroke(
            &stroke_spec,
            xform,
            &Brush::Solid(spec.stroke_color),
            None,
            &path,
            spec.pick,
        );
    } else {
        let dash_offset_px = pt_to_px(spec.dash_offset_pt, ctx.dpi);
        let linewidth_px_for_marker = pt_to_px(spec.linewidth_pt, ctx.dpi);
        let samplers = vec![PolylineSampler::from_polyline(&clipped)];
        let solid_stroke_spec = Stroke::new(linewidth_px)
            .with_caps(spec.cap)
            .with_join(spec.join);
        draw_linetype_with_markers(
            scene,
            &samplers,
            &spec.dash_pattern_pt,
            dash_offset_px,
            linewidth_px_for_marker,
            spec.marker_fill,
            spec.stroke_color,
            ctx.theme.geom.marker_outline_pt,
            &solid_stroke_spec,
            xform,
            ctx.shapes,
            ctx.dpi,
            spec.pick,
            false,
        );
    }

    if !spec.end_marker.name.is_empty() {
        let size_px = pt_to_px(spec.end_marker.size_pt, ctx.dpi);
        let fill = spec.end_marker.fill.unwrap_or(spec.marker_fill);
        let outward = endpoint_outward(&clipped, points, false, clip_end_pt > 0.0);
        let placement = *clipped.last().unwrap();
        emit_endpoint_marker(
            scene,
            placement,
            outward,
            spec.end_marker.invert,
            &spec.end_marker.name,
            size_px,
            fill,
            spec.stroke_color,
            marker_outline_px,
            xform,
            ctx.shapes,
            spec.pick,
        );
    }
}
