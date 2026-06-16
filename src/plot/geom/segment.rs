//! `SegmentGeom` — vectorised line segments drawn between two scaled
//! `(x, y) – (x2, y2)` endpoints.
//!
//! One segment per row (PointGeom-style: row == mark). Stroke only; no
//! fill, no shape primitive lookup. Used for error bars, network edges,
//! connector lines, leader lines, dropped-Y indicators.
//!
//! Channels consumed:
//!
//! - `"x"`, `"y"` — segment start (required; data; numeric).
//! - `"x2"`, `"y2"` — segment end (required; data; numeric).
//! - `"x_offset"`, `"y_offset"`, `"x2_offset"`, `"y2_offset"` — per-edge
//!   absolute pt offsets after scale resolution.
//! - `"x_band"`, `"y_band"`, `"x2_band"`, `"y2_band"` — per-edge
//!   band-fraction offsets. All default to `0.0`.
//! - `"stroke"`, `"stroke_opacity"`, `"linewidth"`, `"linetype"`,
//!   `"dash_offset"`, `"cap"`, `"join"` — same styling set as
//!   LineGeom / RectGeom.
//! - `"fill"` — fill colour for markers in the linetype (per-row;
//!   defaults to the resolved stroke colour when unset). The segment
//!   itself is stroked, not filled — this channel only affects marker
//!   interiors when the linetype contains `Marker` steps.
//! - `"angle"` — rotation in **radians** around the segment midpoint,
//!   mathematical CCW (positive rotates the segment counter-clockwise
//!   in the rendered image). Default `0.0` (no rotation).

use crate::brush::Brush;
use crate::geometry::{Affine, Point};
use crate::primitives::{
    clip_polyline, polyline as polyline_path, segment as segment_path, EndClip, PolylineOptions,
    PolylineSampler,
};
use crate::scene::SceneBuilder;
use crate::stroke::Stroke;

use super::linetype;
use super::resolve::{
    auto_endpoint_clip_pt, build_stroke_for_pattern, draw_linetype_with_markers,
    emit_endpoint_marker, endpoint_outward, override_alpha, pt_to_px, resolve_angle_channel,
    resolve_bool_channel_or, resolve_cap_channel, resolve_color_channel,
    resolve_color_channel_or_theme, resolve_join_channel, resolve_linetype_channel,
    resolve_number_channel, resolve_number_channel_or, resolve_pick_id, resolve_position,
    resolve_str_channel_or,
};
use super::state::{
    filter_declared, require_data_column, validate_channel_lengths, validate_pick_id_channel,
    GeomState, KeysStrategy,
};
use super::{BuildableGeom, Channel, ExpectedOutput, Geom, GeomBuilder, GeomContext};

// ─── Defaults ────────────────────────────────────────────────────────────────

// Style defaults (linewidth, cap, join) live on `theme.geom.segment`.

const CHANNELS: &[(&str, ExpectedOutput)] = &[
    ("x", ExpectedOutput::Numbers),
    ("y", ExpectedOutput::Numbers),
    ("x2", ExpectedOutput::Numbers),
    ("y2", ExpectedOutput::Numbers),
    ("x_offset", ExpectedOutput::Numbers),
    ("y_offset", ExpectedOutput::Numbers),
    ("x2_offset", ExpectedOutput::Numbers),
    ("y2_offset", ExpectedOutput::Numbers),
    ("x_band", ExpectedOutput::Numbers),
    ("y_band", ExpectedOutput::Numbers),
    ("x2_band", ExpectedOutput::Numbers),
    ("y2_band", ExpectedOutput::Numbers),
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

// ─── SegmentGeom ─────────────────────────────────────────────────────────────

/// A vectorised line-segment geom. One segment per row.
pub struct SegmentGeom {
    pub(crate) state: GeomState,
}

crate::impl_geom_inherents!(SegmentGeom);

// ─── BuildableGeom impl ──────────────────────────────────────────────────────

impl BuildableGeom for SegmentGeom {
    fn build_from(builder: GeomBuilder<Self>) -> Self {
        let (keys_opt, channels) = builder.into_parts();

        let n = require_data_column("x", &channels, "SegmentGeom").len();
        for name in ["y", "x2", "y2"] {
            let len = require_data_column(name, &channels, "SegmentGeom").len();
            if len != n {
                panic!(
                    "SegmentGeom::build: \"{name}\" length {len} does not match \"x\" length {n}"
                );
            }
        }
        validate_channel_lengths(&channels, n, "SegmentGeom");
        validate_pick_id_channel(&channels, "SegmentGeom");

        let declared = filter_declared(&channels, CHANNELS);
        let state = GeomState::from_builder(keys_opt, channels, n, KeysStrategy::PerRow, declared);
        SegmentGeom { state }
    }
}

// ─── Geom impl ───────────────────────────────────────────────────────────────

impl Geom for SegmentGeom {
    fn state(&self) -> &GeomState {
        &self.state
    }

    fn state_mut(&mut self) -> &mut GeomState {
        &mut self.state
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn draw(&self, scene: &mut dyn SceneBuilder, ctx: &GeomContext<'_>) {
        let panel = ctx.panel_rect;
        let panel_w = panel.x1 - panel.x0;
        let panel_h = panel.y1 - panel.y0;
        if panel_w <= 0.0 || panel_h <= 0.0 {
            return;
        }
        let n = self.len();
        if n == 0 {
            return;
        }

        let x_scale_bound = ctx.scale_for("x");
        let y_scale_bound = ctx.scale_for("y");
        let x2_scale_bound = ctx.scale_for("x2").or(x_scale_bound);
        let y2_scale_bound = ctx.scale_for("y2").or(y_scale_bound);
        let x_offset_scale = ctx.scale_for("x_offset");
        let y_offset_scale = ctx.scale_for("y_offset");
        let x2_offset_scale = ctx.scale_for("x2_offset");
        let y2_offset_scale = ctx.scale_for("y2_offset");
        let x_band_scale = ctx.scale_for("x_band");
        let y_band_scale = ctx.scale_for("y_band");
        let x2_band_scale = ctx.scale_for("x2_band");
        let y2_band_scale = ctx.scale_for("y2_band");
        let fill_scale = ctx.scale_for("fill");
        let stroke_scale = ctx.scale_for("stroke");
        let stroke_opacity_scale = ctx.scale_for("stroke_opacity");
        let linewidth_scale = ctx.scale_for("linewidth");
        let linetype_scale = ctx.scale_for("linetype");
        let dash_offset_scale = ctx.scale_for("dash_offset");
        let cap_scale = ctx.scale_for("cap");
        let join_scale = ctx.scale_for("join");
        let pick_id_scale = ctx.scale_for("pick_id");
        let clip_start_radius_scale = ctx.scale_for("clip_start_radius");
        let clip_end_radius_scale = ctx.scale_for("clip_end_radius");
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
        let (x2_col, x2_scale) = match channels.get("x2") {
            Some(Channel::Data(c)) => (c, x2_scale_bound),
            Some(Channel::RawData(c)) => (c, None),
            _ => return,
        };
        let (y2_col, y2_scale) = match channels.get("y2") {
            Some(Channel::Data(c)) => (c, y2_scale_bound),
            Some(Channel::RawData(c)) => (c, None),
            _ => return,
        };

        let x_offset_ch = channels.get("x_offset");
        let y_offset_ch = channels.get("y_offset");
        let x2_offset_ch = channels.get("x2_offset");
        let y2_offset_ch = channels.get("y2_offset");
        let x_band_ch = channels.get("x_band");
        let y_band_ch = channels.get("y_band");
        let x2_band_ch = channels.get("x2_band");
        let y2_band_ch = channels.get("y2_band");
        let fill_ch = channels.get("fill");
        let stroke_ch = channels.get("stroke");
        let stroke_opacity_ch = channels.get("stroke_opacity");
        let linewidth_ch = channels.get("linewidth");
        let linetype_ch = channels.get("linetype");
        let dash_offset_ch = channels.get("dash_offset");
        let cap_ch = channels.get("cap");
        let join_ch = channels.get("join");
        let pick_id_ch = channels.get("pick_id");
        let clip_start_radius_ch = channels.get("clip_start_radius");
        let clip_end_radius_ch = channels.get("clip_end_radius");
        let angle_ch = channels.get("angle");
        let start_marker_ch = channels.get("start_marker");
        let end_marker_ch = channels.get("end_marker");
        let start_marker_size_ch = channels.get("start_marker_size");
        let end_marker_size_ch = channels.get("end_marker_size");
        let start_marker_fill_ch = channels.get("start_marker_fill");
        let end_marker_fill_ch = channels.get("end_marker_fill");
        let start_marker_invert_ch = channels.get("start_marker_invert");
        let end_marker_invert_ch = channels.get("end_marker_invert");
        let start_marker_scale = ctx.scale_for("start_marker");
        let end_marker_scale = ctx.scale_for("end_marker");
        let start_marker_size_scale = ctx.scale_for("start_marker_size");
        let end_marker_size_scale = ctx.scale_for("end_marker_size");
        let start_marker_fill_scale = ctx.scale_for("start_marker_fill");
        let end_marker_fill_scale = ctx.scale_for("end_marker_fill");
        let start_marker_invert_scale = ctx.scale_for("start_marker_invert");
        let end_marker_invert_scale = ctx.scale_for("end_marker_invert");

        for i in 0..n {
            let stroke_color = override_alpha(
                resolve_color_channel_or_theme(
                    stroke_ch,
                    stroke_scale,
                    i,
                    ctx.theme.geom.segment.stroke.as_ref(),
                    &ctx.theme.palette,
                ),
                resolve_number_channel(stroke_opacity_ch, stroke_opacity_scale, i),
            );
            let stroke_color = match stroke_color {
                Some(c) => c,
                None => continue,
            };

            let x_band = resolve_number_channel_or(x_band_ch, x_band_scale, i, 0.0);
            let x2_band = resolve_number_channel_or(x2_band_ch, x2_band_scale, i, 0.0);
            let y_band = resolve_number_channel_or(y_band_ch, y_band_scale, i, 0.0);
            let y2_band = resolve_number_channel_or(y2_band_ch, y2_band_scale, i, 0.0);

            let x_frac = resolve_position(x_col.get(i), x_scale, x_band);
            let x2_frac = resolve_position(x2_col.get(i), x2_scale, x2_band);
            let y_frac = resolve_position(y_col.get(i), y_scale, y_band);
            let y2_frac = resolve_position(y2_col.get(i), y2_scale, y2_band);
            if !x_frac.is_finite()
                || !x2_frac.is_finite()
                || !y_frac.is_finite()
                || !y2_frac.is_finite()
            {
                continue;
            }

            let (px0, py0) = ctx.projection.project_to_panel_px(panel, &[x_frac, y_frac]);
            let (px20, py20) = ctx
                .projection
                .project_to_panel_px(panel, &[x2_frac, y2_frac]);
            let mut px = px0;
            let mut px2 = px20;
            let mut py = py0;
            let mut py2 = py20;

            if let Some(off) = resolve_number_channel(x_offset_ch, x_offset_scale, i) {
                px += pt_to_px(off, ctx.dpi);
            }
            if let Some(off) = resolve_number_channel(x2_offset_ch, x2_offset_scale, i) {
                px2 += pt_to_px(off, ctx.dpi);
            }
            if let Some(off) = resolve_number_channel(y_offset_ch, y_offset_scale, i) {
                py -= pt_to_px(off, ctx.dpi);
            }
            if let Some(off) = resolve_number_channel(y2_offset_ch, y2_offset_scale, i) {
                py2 -= pt_to_px(off, ctx.dpi);
            }

            let linewidth_pt = resolve_number_channel_or(
                linewidth_ch,
                linewidth_scale,
                i,
                ctx.theme.geom.segment.linewidth_pt,
            );
            let linewidth_px = pt_to_px(linewidth_pt, ctx.dpi);
            if !linewidth_px.is_finite() || linewidth_px <= 0.0 {
                continue;
            }

            let dash_pattern_pt = resolve_linetype_channel(linetype_ch, linetype_scale, i);
            let dash_offset_pt =
                resolve_number_channel_or(dash_offset_ch, dash_offset_scale, i, 0.0);
            let cap = resolve_cap_channel(cap_ch, cap_scale, i, ctx.theme.geom.segment.cap);
            let join = resolve_join_channel(join_ch, join_scale, i, ctx.theme.geom.segment.join);

            // Endpoint-marker constants. Resolved BEFORE the clip
            // calc so the auto-clip contribution can fold in below.
            let start_name = resolve_str_channel_or(start_marker_ch, start_marker_scale, i, "");
            let end_name = resolve_str_channel_or(end_marker_ch, end_marker_scale, i, "");
            let default_marker_size_pt = 3.0 * linewidth_pt;
            let start_marker_size_pt = resolve_number_channel_or(
                start_marker_size_ch,
                start_marker_size_scale,
                i,
                default_marker_size_pt,
            );
            let end_marker_size_pt = resolve_number_channel_or(
                end_marker_size_ch,
                end_marker_size_scale,
                i,
                default_marker_size_pt,
            );
            let start_invert = resolve_bool_channel_or(
                start_marker_invert_ch,
                start_marker_invert_scale,
                i,
                false,
            );
            let end_invert =
                resolve_bool_channel_or(end_marker_invert_ch, end_marker_invert_scale, i, false);

            // Endpoint clipping. `clip_*_radius` covers the explicit-
            // trim use case (graph node boundaries, breathing room
            // next to a data point); on top of that we automatically
            // add the forward extent of any endpoint marker so the
            // marker's tip lands at the user's clip boundary (or the
            // original endpoint when no user clip is set) without the
            // user having to compute the marker geometry themselves.
            let user_clip_start_pt =
                resolve_number_channel_or(clip_start_radius_ch, clip_start_radius_scale, i, 0.0);
            let user_clip_end_pt =
                resolve_number_channel_or(clip_end_radius_ch, clip_end_radius_scale, i, 0.0);
            let auto_clip_start_pt =
                auto_endpoint_clip_pt(&start_name, start_marker_size_pt, start_invert, ctx.shapes);
            let auto_clip_end_pt =
                auto_endpoint_clip_pt(&end_name, end_marker_size_pt, end_invert, ctx.shapes);
            let clip_start_pt = user_clip_start_pt + auto_clip_start_pt;
            let clip_end_pt = user_clip_end_pt + auto_clip_end_pt;
            let p0 = Point::new(px, py);
            let p1 = Point::new(px2, py2);
            let original = [p0, p1];

            // Under non-linear projections, insert interior sample
            // points along the channel-space segment so the stroked
            // segment follows the projected geodesic instead of cutting
            // across as a chord. Cartesian's `interpolate_segment` is
            // a no-op; `pre_clip` ends up as `[p0, p1]` and the path
            // collapses to the same `segment_path` as before.
            let is_linear = ctx.projection.is_linear();
            let mut pre_clip: Vec<Point> = Vec::with_capacity(2);
            pre_clip.push(p0);
            if !is_linear {
                let mut interior: Vec<(f64, f64)> = Vec::new();
                ctx.projection.interpolate_segment(
                    panel,
                    &[x_frac, y_frac],
                    &[x2_frac, y2_frac],
                    &mut interior,
                );
                for (ipx, ipy) in &interior {
                    pre_clip.push(Point::new(*ipx, *ipy));
                }
            }
            pre_clip.push(p1);

            let was_clipped = clip_start_pt > 0.0 || clip_end_pt > 0.0;
            let pts: Vec<Point> = if was_clipped {
                let start = (clip_start_pt > 0.0).then(|| EndClip::Circle {
                    center: p0,
                    radius: pt_to_px(clip_start_pt, ctx.dpi),
                });
                let end = (clip_end_pt > 0.0).then(|| EndClip::Circle {
                    center: p1,
                    radius: pt_to_px(clip_end_pt, ctx.dpi),
                });
                clip_polyline(&pre_clip, start, end)
            } else {
                pre_clip
            };
            if pts.len() < 2 {
                continue;
            }
            // Two-point fast path keeps Cartesian (and clipped-to-line
            // segments) on the simpler `segment_path` primitive. Three
            // or more points (only reachable under non-linear projections)
            // build a polyline.
            let path = if pts.len() == 2 {
                segment_path(pts[0], pts[1])
            } else {
                polyline_path(&pts, PolylineOptions::default())
            };
            let pick = resolve_pick_id(pick_id_ch, pick_id_scale, i);

            // Rotation around the segment midpoint. Math CCW from the
            // user → negate for kurbo (screen y-down).
            let angle = resolve_angle_channel(angle_ch, angle_scale, i);
            let xform = if angle == 0.0 {
                Affine::IDENTITY
            } else {
                let mx = 0.5 * (px + px2);
                let my = 0.5 * (py + py2);
                Affine::rotate_about(-angle, Point::new(mx, my))
            };

            let marker_fill = resolve_color_channel(fill_ch, fill_scale, i).unwrap_or(stroke_color);

            // Start marker emitted before the stroke; end marker
            // after — matching LineGeom's path-order convention.
            let marker_outline_px = linewidth_px.max(pt_to_px(0.5, ctx.dpi));

            if !start_name.is_empty() {
                let size_px = pt_to_px(start_marker_size_pt, ctx.dpi);
                let fill = resolve_color_channel(start_marker_fill_ch, start_marker_fill_scale, i)
                    .unwrap_or(marker_fill);
                let outward = endpoint_outward(&pts, &original, true, clip_start_pt > 0.0);
                emit_endpoint_marker(
                    scene,
                    pts[0],
                    outward,
                    start_invert,
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
                scene.stroke(
                    &stroke_spec,
                    xform,
                    &Brush::Solid(stroke_color),
                    None,
                    &path,
                    pick,
                );
            } else {
                // Segment is open: no gap distribution. Marker fill
                // comes from the `"fill"` channel, defaulting to the
                // resolved stroke colour. Marker outlines use the
                // stroke colour.
                let samplers = PolylineSampler::from_path(&path, 0.5);
                let solid_stroke_spec = Stroke::new(linewidth_px).with_caps(cap).with_join(join);
                let dash_offset_px = pt_to_px(dash_offset_pt, ctx.dpi);
                draw_linetype_with_markers(
                    scene,
                    &samplers,
                    &dash_pattern_pt,
                    dash_offset_px,
                    linewidth_px,
                    marker_fill,
                    stroke_color,
                    ctx.theme.geom.marker_outline_pt,
                    &solid_stroke_spec,
                    xform,
                    ctx.shapes,
                    ctx.dpi,
                    pick,
                    /* distribute */ false,
                );
            }

            if !end_name.is_empty() {
                let size_px = pt_to_px(end_marker_size_pt, ctx.dpi);
                let fill = resolve_color_channel(end_marker_fill_ch, end_marker_fill_scale, i)
                    .unwrap_or(marker_fill);
                let outward = endpoint_outward(&pts, &original, false, clip_end_pt > 0.0);
                emit_endpoint_marker(
                    scene,
                    pts[pts.len() - 1],
                    outward,
                    end_invert,
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
    use std::sync::Arc;

    use crate::color::Color;
    use crate::geometry::Rect as GeomRect;
    use crate::plot::geom::{linetype, DirectScaleResolver, Raw};
    use crate::plot::scale;
    use crate::plot::value::Value;
    use crate::scene::recording::{Op, RecordingScene};
    use kurbo::Shape;

    fn shapes() -> crate::shape::ShapeRegistry {
        crate::shape::ShapeRegistry::with_builtins()
    }

    fn ctx<'a>(
        panel: GeomRect,
        registry: &'a crate::shape::ShapeRegistry,
        scales: &'a DirectScaleResolver<'a>,
    ) -> GeomContext<'a> {
        GeomContext::new(panel, 96.0, registry, scales)
    }

    fn red() -> Color {
        Color::new([1.0, 0.0, 0.0, 1.0])
    }

    // ── build() validation ──

    #[test]
    fn builder_requires_all_four_positions() {
        let r = std::panic::catch_unwind(|| {
            SegmentGeom::builder()
                .set("x", vec![0.0_f64])
                .set("y", vec![0.0_f64])
                .set("x2", vec![1.0_f64])
                // y2 missing
                .build()
        });
        assert!(r.is_err());
    }

    #[test]
    #[should_panic(expected = "must be data, not constant")]
    fn builder_x_constant_panics() {
        SegmentGeom::builder()
            .set("x", 0.0_f64)
            .set("y", vec![0.0_f64])
            .set("x2", vec![1.0_f64])
            .set("y2", vec![1.0_f64])
            .build();
    }

    #[test]
    #[should_panic(expected = "does not match")]
    fn builder_length_mismatch_panics() {
        SegmentGeom::builder()
            .set("x", vec![0.0_f64, 1.0])
            .set("y", vec![0.0_f64, 1.0])
            .set("x2", vec![0.5_f64])
            .set("y2", vec![0.5_f64])
            .build();
    }

    // ── Drawing ──

    fn stroke_endpoints(scene: &RecordingScene) -> Option<(kurbo::Point, kurbo::Point)> {
        for op in &scene.ops {
            if let Op::Stroke { path, .. } = op {
                let elems: Vec<_> = path.elements().iter().collect();
                if elems.len() < 2 {
                    return None;
                }
                let start = match elems[0] {
                    kurbo::PathEl::MoveTo(p) => *p,
                    _ => return None,
                };
                let end = match elems[1] {
                    kurbo::PathEl::LineTo(p) => *p,
                    _ => return None,
                };
                return Some((start, end));
            }
        }
        None
    }

    #[test]
    fn draws_one_stroke_per_row() {
        let g = SegmentGeom::builder()
            .set("x", vec![0.1_f64, 0.2, 0.3])
            .set("y", vec![0.1_f64, 0.2, 0.3])
            .set("x2", vec![0.9_f64, 0.8, 0.7])
            .set("y2", vec![0.9_f64, 0.8, 0.7])
            .set("stroke", red())
            .build();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(GeomRect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales),
        );
        let strokes = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Stroke { .. }))
            .count();
        assert_eq!(strokes, 3);
    }

    #[test]
    fn endpoint_positions_match_data() {
        let g = SegmentGeom::builder()
            .set("x", vec![0.2_f64])
            .set("y", vec![0.0_f64])
            .set("x2", vec![0.8_f64])
            .set("y2", vec![1.0_f64])
            .set("stroke", red())
            .build();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(GeomRect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales),
        );
        let (start, end) = stroke_endpoints(&scene).expect("stroke");
        // x_frac=0.2 → 20 px; y flips: y_frac=0.0 → 100 px (bottom).
        // x2_frac=0.8 → 80 px; y2_frac=1.0 → 0 px (top).
        assert!((start.x - 20.0).abs() < 1e-6, "start.x = {}", start.x);
        assert!((start.y - 100.0).abs() < 1e-6, "start.y = {}", start.y);
        assert!((end.x - 80.0).abs() < 1e-6, "end.x = {}", end.x);
        assert!((end.y - 0.0).abs() < 1e-6, "end.y = {}", end.y);
    }

    #[test]
    fn no_stroke_emits_nothing() {
        let g = SegmentGeom::builder()
            .set("x", vec![0.1_f64])
            .set("y", vec![0.1_f64])
            .set("x2", vec![0.5_f64])
            .set("y2", vec![0.5_f64])
            .build();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(GeomRect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales),
        );
        assert!(scene.ops.is_empty());
    }

    #[test]
    fn nonfinite_position_skips_row() {
        let g = SegmentGeom::builder()
            .set("x", vec![0.1_f64, f64::NAN])
            .set("y", vec![0.1_f64, 0.1])
            .set("x2", vec![0.5_f64, 0.5])
            .set("y2", vec![0.5_f64, 0.5])
            .set("stroke", red())
            .build();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(GeomRect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales),
        );
        let strokes = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Stroke { .. }))
            .count();
        assert_eq!(strokes, 1);
    }

    #[test]
    fn band_offsets_per_endpoint_on_discrete_x() {
        // Two segments going from x="A" to x2="B" — diagonal connectors
        // across two bands. With both bands' offsets at 0, each endpoint
        // sits at its band centre.
        let x = scale::discrete(["A", "B"].into_iter().map(|s| Value::String(Arc::from(s))));
        let resolver = DirectScaleResolver::new().with("x", &x).with("x2", &x);
        let g = SegmentGeom::builder()
            .set("x", vec!["A"])
            .set("x2", vec!["B"])
            .set("y", vec![0.5_f64])
            .set("y2", vec![0.5_f64])
            .set("stroke", red())
            .build();
        let shapes = shapes();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(GeomRect::new(0.0, 0.0, 100.0, 100.0), &shapes, &resolver),
        );
        let (start, end) = stroke_endpoints(&scene).expect("stroke");
        // Band A centre = 0.25 * 100 = 25 px; band B centre = 0.75 * 100 = 75 px.
        assert!((start.x - 25.0).abs() < 1e-6);
        assert!((end.x - 75.0).abs() < 1e-6);
    }

    #[test]
    fn x_offset_translates_start_in_pt() {
        let g = SegmentGeom::builder()
            .set("x", vec![0.2_f64])
            .set("y", vec![0.5_f64])
            .set("x2", vec![0.8_f64])
            .set("y2", vec![0.5_f64])
            .set("stroke", red())
            .set("x_offset", 9.0_f64) // +12 px
            .build();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(GeomRect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales),
        );
        let (start, end) = stroke_endpoints(&scene).expect("stroke");
        // start.x = 20 + 12 = 32; end.x = 80 (no offset on x2).
        assert!((start.x - 32.0).abs() < 1e-6, "start.x = {}", start.x);
        assert!((end.x - 80.0).abs() < 1e-6, "end.x = {}", end.x);
    }

    #[test]
    fn linetype_dashes_stroke() {
        let g = SegmentGeom::builder()
            .set("x", vec![0.1_f64])
            .set("y", vec![0.5_f64])
            .set("x2", vec![0.9_f64])
            .set("y2", vec![0.5_f64])
            .set("stroke", red())
            .set("linetype", Value::Linetype(linetype::dashed()))
            .build();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(GeomRect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales),
        );
        let dashed = scene.ops.iter().any(|op| match op {
            Op::Stroke { stroke, .. } => !stroke.dash_pattern.is_empty(),
            _ => false,
        });
        assert!(dashed);
    }

    #[test]
    fn linewidth_pt_to_px() {
        let g = SegmentGeom::builder()
            .set("x", vec![0.1_f64])
            .set("y", vec![0.5_f64])
            .set("x2", vec![0.9_f64])
            .set("y2", vec![0.5_f64])
            .set("stroke", red())
            .set("linewidth", 2.0_f64) // 2pt at 96 dpi → 8/3 px
            .build();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(GeomRect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales),
        );
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
    fn declared_channels_alphabetical() {
        let g = SegmentGeom::builder()
            .set("x", vec![0.0_f64])
            .set("y", vec![0.0_f64])
            .set("x2", vec![1.0_f64])
            .set("y2", vec![1.0_f64])
            .set("stroke", red())
            .build();
        let names: Vec<&str> = g.declared_channels().iter().map(|d| d.name).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
        // Sanity: no "corner_radius" — segments don't round corners.
        // `"fill"` is declared for marker interiors (see linetype
        // markers) even though the segment itself isn't filled.
        assert!(!names.contains(&"corner_radius"));
    }

    #[test]
    fn pick_id_channel_passes_through_per_row() {
        let g = SegmentGeom::builder()
            .set("x", vec![0.1_f64, 0.2, 0.3])
            .set("y", vec![0.1_f64, 0.2, 0.3])
            .set("x2", vec![0.9_f64, 0.8, 0.7])
            .set("y2", vec![0.9_f64, 0.8, 0.7])
            .set("stroke", red())
            .set("pick_id", vec![11_i64, 22, 33])
            .build();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(GeomRect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales),
        );
        let picks: Vec<u32> = scene
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
        assert_eq!(picks, vec![11, 22, 33]);
    }

    #[test]
    fn bounding_box_matches_endpoints() {
        // Sanity check via path bbox: the stroke path's bounding box
        // should cover the endpoints.
        let g = SegmentGeom::builder()
            .set("x", vec![0.2_f64])
            .set("y", vec![0.3_f64])
            .set("x2", vec![0.7_f64])
            .set("y2", vec![0.6_f64])
            .set("stroke", red())
            .build();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(GeomRect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales),
        );
        for op in &scene.ops {
            if let Op::Stroke { path, .. } = op {
                let bb = path.bounding_box();
                assert!((bb.x0 - 20.0).abs() < 1e-6);
                assert!((bb.x1 - 70.0).abs() < 1e-6);
                return;
            }
        }
        panic!("no stroke op");
    }

    // ── clip_start_radius / clip_end_radius ──

    fn first_stroke_path(scene: &RecordingScene) -> Option<crate::path::Path> {
        scene.ops.iter().find_map(|op| match op {
            Op::Stroke { path, .. } => Some(path.clone()),
            _ => None,
        })
    }

    #[test]
    fn clip_start_radius_trims_segment_start() {
        // Segment from (0, 50) to (100, 50). clip_start_radius = 15pt
        // = 20 px → trimmed segment starts at (20, 50).
        let g = SegmentGeom::builder()
            .set("x", Raw(vec![0.0_f64]))
            .set("y", Raw(vec![0.5_f64]))
            .set("x2", Raw(vec![1.0_f64]))
            .set("y2", Raw(vec![0.5_f64]))
            .set("stroke", red())
            .set("clip_start_radius", 15.0_f64)
            .build();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(GeomRect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales),
        );
        let path = first_stroke_path(&scene).expect("stroke");
        match path.elements().first() {
            Some(kurbo::PathEl::MoveTo(p)) => {
                let expected = 15.0 * 96.0 / 72.0;
                assert!((p.x - expected).abs() < 1e-6, "start.x = {}", p.x);
            }
            other => panic!("expected MoveTo, got {other:?}"),
        }
    }

    #[test]
    fn clip_end_radius_trims_segment_end() {
        let g = SegmentGeom::builder()
            .set("x", Raw(vec![0.0_f64]))
            .set("y", Raw(vec![0.5_f64]))
            .set("x2", Raw(vec![1.0_f64]))
            .set("y2", Raw(vec![0.5_f64]))
            .set("stroke", red())
            .set("clip_end_radius", 15.0_f64)
            .build();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(GeomRect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales),
        );
        let path = first_stroke_path(&scene).expect("stroke");
        // Last LineTo is the trimmed endpoint.
        let last = path
            .elements()
            .iter()
            .rev()
            .find_map(|el| match el {
                kurbo::PathEl::LineTo(p) => Some(*p),
                _ => None,
            })
            .expect("LineTo");
        let expected = 100.0 - 15.0 * 96.0 / 72.0;
        assert!((last.x - expected).abs() < 1e-6, "end.x = {}", last.x);
    }

    #[test]
    fn clip_radii_overlap_skips_segment() {
        // 100-px segment with start+end clip radii summing to > segment
        // length → no stroke emitted.
        let g = SegmentGeom::builder()
            .set("x", Raw(vec![0.0_f64]))
            .set("y", Raw(vec![0.5_f64]))
            .set("x2", Raw(vec![1.0_f64]))
            .set("y2", Raw(vec![0.5_f64]))
            .set("stroke", red())
            .set("clip_start_radius", 80.0_f64)
            .set("clip_end_radius", 80.0_f64)
            .build();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(GeomRect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales),
        );
        let strokes = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Stroke { .. }))
            .count();
        assert_eq!(strokes, 0);
    }

    // ── Endpoint markers (Phase C.5) ──

    /// Build a horizontal segment from (0, 50) to (100, 50) in screen
    /// space, with the given endpoint-marker channel set.
    fn horizontal_segment_with(extra: impl FnOnce(&mut GeomBuilder<SegmentGeom>)) -> SegmentGeom {
        let mut b = SegmentGeom::builder();
        b.set("x", Raw(vec![0.0_f64]))
            .set("y", Raw(vec![0.5_f64]))
            .set("x2", Raw(vec![1.0_f64]))
            .set("y2", Raw(vec![0.5_f64]))
            .set("stroke", red())
            .set("linewidth", 2.0_f64);
        extra(&mut b);
        b.build()
    }

    fn draw_into_scene(g: &SegmentGeom) -> RecordingScene {
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(GeomRect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales),
        );
        scene
    }

    fn fills_after_strokes(scene: &RecordingScene) -> Vec<kurbo::Affine> {
        let mut found_stroke = false;
        let mut out = Vec::new();
        for op in &scene.ops {
            match op {
                Op::Stroke { .. } => found_stroke = true,
                Op::Fill { transform, .. } if found_stroke => out.push(*transform),
                _ => {}
            }
        }
        out
    }

    fn fills_before_strokes(scene: &RecordingScene) -> Vec<kurbo::Affine> {
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

    #[test]
    fn endpoint_marker_unset_emits_no_extra_ops() {
        let g = horizontal_segment_with(|_| {});
        let scene = draw_into_scene(&g);
        let fills = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Fill { .. }))
            .count();
        assert_eq!(fills, 0, "no markers → no fill ops");
    }

    #[test]
    fn endpoint_unknown_marker_name_is_silent_no_op() {
        let g = horizontal_segment_with(|b| {
            b.set("end_marker", "no-such-shape");
        });
        let scene = draw_into_scene(&g);
        let fills = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Fill { .. }))
            .count();
        assert_eq!(fills, 0);
    }

    #[test]
    fn end_marker_anchor_lands_at_last_vertex() {
        // Auto-clip trims `(bbox.x1 - anchor.x) * size_pt = 1.0 * 10 =
        // 10 pt` so the arrow's tip lands at the original endpoint.
        // The anchor (the back of the arrow body) therefore sits at
        // `(100 - 10pt_in_px, 50)`.
        let g = horizontal_segment_with(|b| {
            b.set("end_marker", "arrow-closed")
                .set("end_marker_size", 10.0_f64);
        });
        let scene = draw_into_scene(&g);
        let xforms = fills_after_strokes(&scene);
        assert!(!xforms.is_empty(), "expected an end-marker fill");
        let xf = xforms[0];
        let anchor = xf * kurbo::Point::new(-1.0, 0.0);
        let size_px = 10.0 * 96.0 / 72.0;
        assert!(
            (anchor.x - (100.0 - size_px)).abs() < 1e-6,
            "anchor.x = {} (expected {})",
            anchor.x,
            100.0 - size_px
        );
        assert!((anchor.y - 50.0).abs() < 1e-6, "anchor.y = {}", anchor.y);
    }

    #[test]
    fn end_marker_tip_extends_outward() {
        // Auto-clip lands the arrow tip exactly on the original
        // endpoint; the (0, 0) tip vertex in shape coords maps to
        // (100, 50) after the transform.
        let g = horizontal_segment_with(|b| {
            b.set("end_marker", "arrow-closed")
                .set("end_marker_size", 10.0_f64);
        });
        let scene = draw_into_scene(&g);
        let xforms = fills_after_strokes(&scene);
        let tip = xforms[0] * kurbo::Point::new(0.0, 0.0);
        assert!(
            (tip.x - 100.0).abs() < 1e-6,
            "tip.x = {} (expected 100)",
            tip.x
        );
        assert!((tip.y - 50.0).abs() < 1e-6, "tip.y = {}", tip.y);
    }

    #[test]
    fn start_marker_anchor_lands_at_first_vertex_and_tip_points_back() {
        // Mode B at start with auto-clip: trim shifts the anchor
        // forward into the segment by `size_pt × 1.0`, and the tip
        // (in -x outward direction) lands at the original first
        // vertex (0, 50).
        let g = horizontal_segment_with(|b| {
            b.set("start_marker", "arrow-closed")
                .set("start_marker_size", 10.0_f64);
        });
        let scene = draw_into_scene(&g);
        let xforms = fills_before_strokes(&scene);
        assert!(!xforms.is_empty(), "expected a start-marker fill");
        let xf = xforms[0];
        let anchor = xf * kurbo::Point::new(-1.0, 0.0);
        let size_px = 10.0 * 96.0 / 72.0;
        assert!(
            (anchor.x - size_px).abs() < 1e-6,
            "anchor.x = {} (expected {})",
            anchor.x,
            size_px
        );
        assert!((anchor.y - 50.0).abs() < 1e-6, "anchor.y = {}", anchor.y);
        let tip = xf * kurbo::Point::new(0.0, 0.0);
        assert!((tip.x - 0.0).abs() < 1e-6, "tip.x = {} (expected 0)", tip.x);
    }

    #[test]
    fn marker_size_default_is_three_times_linewidth() {
        // linewidth = 2 pt → default marker size = 6 pt = 8 px at 96 dpi.
        let g = horizontal_segment_with(|b| {
            b.set("end_marker", "arrow-closed");
        });
        let scene = draw_into_scene(&g);
        let xforms = fills_after_strokes(&scene);
        let xf = xforms[0];
        let coeffs = xf.as_coeffs();
        // Linear part determinant = size_px^2 for any rotation.
        let det = coeffs[0] * coeffs[3] - coeffs[1] * coeffs[2];
        let expected_size = 6.0 * 96.0 / 72.0;
        assert!(
            (det.abs() - expected_size * expected_size).abs() < 1e-6,
            "det = {}, expected ±{}",
            det,
            expected_size * expected_size
        );
    }

    #[test]
    fn marker_size_override_takes_pt() {
        let g = horizontal_segment_with(|b| {
            b.set("end_marker", "arrow-closed")
                .set("end_marker_size", 18.0_f64);
        });
        let scene = draw_into_scene(&g);
        let xf = fills_after_strokes(&scene)[0];
        let coeffs = xf.as_coeffs();
        let det = coeffs[0] * coeffs[3] - coeffs[1] * coeffs[2];
        let expected_size = 18.0 * 96.0 / 72.0;
        assert!((det.abs() - expected_size * expected_size).abs() < 1e-6);
    }

    #[test]
    fn marker_respects_clip_start_radius() {
        // Horizontal segment, clip_start_radius = 15 pt (= user trim
        // toward a node boundary, etc.) plus a size-10-pt arrow
        // contributing another 10 pt of auto-clip: effective trim =
        // 25 pt. The arrow anchor lands at (25pt_in_px, 50); the tip
        // (in the chord direction toward original first vertex)
        // lands at the user's clip boundary, 15 pt from the data
        // endpoint.
        let g = horizontal_segment_with(|b| {
            b.set("start_marker", "arrow-closed")
                .set("start_marker_size", 10.0_f64)
                .set("clip_start_radius", 15.0_f64);
        });
        let scene = draw_into_scene(&g);
        let xf = fills_before_strokes(&scene)[0];
        let anchor = xf * kurbo::Point::new(-1.0, 0.0);
        let user_clip_px = 15.0 * 96.0 / 72.0;
        let auto_clip_px = 10.0 * 96.0 / 72.0;
        let expected_anchor_x = user_clip_px + auto_clip_px;
        assert!(
            (anchor.x - expected_anchor_x).abs() < 1e-6,
            "anchor.x = {} (expected {})",
            anchor.x,
            expected_anchor_x
        );
        let tip = xf * kurbo::Point::new(0.0, 0.0);
        assert!(
            (tip.x - user_clip_px).abs() < 1e-6,
            "tip.x = {} (expected user clip boundary {})",
            tip.x,
            user_clip_px
        );
    }

    #[test]
    fn marker_fill_default_is_marker_fill_chain() {
        // Default: no fill, no start_marker_fill → marker fills with
        // stroke (red).
        let g = horizontal_segment_with(|b| {
            b.set("end_marker", "arrow-closed");
        });
        let scene = draw_into_scene(&g);
        let fill_color = scene
            .ops
            .iter()
            .find_map(|op| match op {
                Op::Fill {
                    brush: Brush::Solid(c),
                    ..
                } => Some(*c),
                _ => None,
            })
            .expect("end-marker fill op");
        assert_eq!(fill_color, red());

        // With fill=blue (no start_marker_fill): marker uses blue.
        let blue = Color::new([0.0, 0.0, 1.0, 1.0]);
        let g = horizontal_segment_with(|b| {
            b.set("end_marker", "arrow-closed").set("fill", blue);
        });
        let scene = draw_into_scene(&g);
        let fill_color = scene
            .ops
            .iter()
            .find_map(|op| match op {
                Op::Fill {
                    brush: Brush::Solid(c),
                    ..
                } => Some(*c),
                _ => None,
            })
            .unwrap();
        assert_eq!(fill_color, blue);

        // With end_marker_fill=green: green wins for end; start unaffected.
        let green = Color::new([0.0, 1.0, 0.0, 1.0]);
        let g = horizontal_segment_with(|b| {
            b.set("end_marker", "arrow-closed")
                .set("fill", blue)
                .set("end_marker_fill", green);
        });
        let scene = draw_into_scene(&g);
        let fill_color = scene
            .ops
            .iter()
            .find_map(|op| match op {
                Op::Fill {
                    brush: Brush::Solid(c),
                    ..
                } => Some(*c),
                _ => None,
            })
            .unwrap();
        assert_eq!(fill_color, green);
    }

    #[test]
    fn endpoint_fills_are_independently_controllable() {
        let green = Color::new([0.0, 1.0, 0.0, 1.0]);
        let orange = Color::new([1.0, 0.5, 0.0, 1.0]);
        let g = horizontal_segment_with(|b| {
            b.set("start_marker", "arrow-closed")
                .set("end_marker", "arrow-closed")
                .set("start_marker_fill", green)
                .set("end_marker_fill", orange);
        });
        let scene = draw_into_scene(&g);
        let start_fills = fills_before_strokes(&scene);
        let end_fills = fills_after_strokes(&scene);
        assert!(!start_fills.is_empty() && !end_fills.is_empty());
        // Order: start fill ops come before the stroke, end fill ops
        // come after. Look up each colour from the corresponding range.
        let start_color = scene
            .ops
            .iter()
            .find_map(|op| match op {
                Op::Fill {
                    brush: Brush::Solid(c),
                    ..
                } => Some(*c),
                _ => None,
            })
            .unwrap();
        assert_eq!(start_color, green);
        let end_color = scene
            .ops
            .iter()
            .rev()
            .find_map(|op| match op {
                Op::Fill {
                    brush: Brush::Solid(c),
                    ..
                } => Some(*c),
                _ => None,
            })
            .unwrap();
        assert_eq!(end_color, orange);
    }

    #[test]
    fn marker_invert_flips_rotation() {
        // Default end-marker on horizontal +x segment: tip at +x.
        // With end_marker_invert = true: tip points back along the line
        // (i.e., into the segment from the endpoint).
        let g = horizontal_segment_with(|b| {
            b.set("end_marker", "arrow-closed")
                .set("end_marker_size", 10.0_f64)
                .set("end_marker_invert", true);
        });
        let scene = draw_into_scene(&g);
        let xf = fills_after_strokes(&scene)[0];
        let tip = xf * kurbo::Point::new(0.0, 0.0);
        let size_px = 10.0 * 96.0 / 72.0;
        // Inverted: tip should land at (100 - size_px, 50), pointing inward.
        assert!(
            (tip.x - (100.0 - size_px)).abs() < 1e-6,
            "tip.x = {} (expected {})",
            tip.x,
            100.0 - size_px
        );
    }
}
