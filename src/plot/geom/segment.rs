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
use crate::primitives::{clip_polyline, segment as segment_path, EndClip, PolylineSampler};
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
use super::{BuildableGeom, Channel, ExpectedOutput, Geom, GeomBuilder, GeomContext};

// ─── Defaults ────────────────────────────────────────────────────────────────

const DEFAULT_LINEWIDTH_PT: f64 = 1.0;
const DEFAULT_CAP: Cap = Cap::Butt;
const DEFAULT_JOIN: Join = Join::Miter;

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

        for i in 0..n {
            let stroke_color = override_alpha(
                resolve_color_channel(stroke_ch, stroke_scale, i),
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

            let mut px = panel.x0 + x_frac * panel_w;
            let mut px2 = panel.x0 + x2_frac * panel_w;
            let mut py = panel.y1 - y_frac * panel_h;
            let mut py2 = panel.y1 - y2_frac * panel_h;

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

            let linewidth_pt =
                resolve_number_channel_or(linewidth_ch, linewidth_scale, i, DEFAULT_LINEWIDTH_PT);
            let linewidth_px = pt_to_px(linewidth_pt, ctx.dpi);
            if !linewidth_px.is_finite() || linewidth_px <= 0.0 {
                continue;
            }

            let dash_pattern_pt = resolve_linetype_channel(linetype_ch, linetype_scale, i);
            let dash_offset_pt =
                resolve_number_channel_or(dash_offset_ch, dash_offset_scale, i, 0.0);
            let cap = resolve_cap_channel(cap_ch, cap_scale, i, DEFAULT_CAP);
            let join = resolve_join_channel(join_ch, join_scale, i, DEFAULT_JOIN);

            // Optional endpoint clipping by a circle at the segment's
            // start / end. Trims the segment where it exits the circle;
            // when the radius is ≥ the segment length the whole segment
            // disappears (matches primitive behaviour).
            let clip_start_pt =
                resolve_number_channel_or(clip_start_radius_ch, clip_start_radius_scale, i, 0.0);
            let clip_end_pt =
                resolve_number_channel_or(clip_end_radius_ch, clip_end_radius_scale, i, 0.0);
            let path = if clip_start_pt > 0.0 || clip_end_pt > 0.0 {
                let p0 = Point::new(px, py);
                let p1 = Point::new(px2, py2);
                let start = (clip_start_pt > 0.0).then(|| EndClip::Circle {
                    center: p0,
                    radius: pt_to_px(clip_start_pt, ctx.dpi),
                });
                let end = (clip_end_pt > 0.0).then(|| EndClip::Circle {
                    center: p1,
                    radius: pt_to_px(clip_end_pt, ctx.dpi),
                });
                let pts = clip_polyline(&[p0, p1], start, end);
                if pts.len() < 2 {
                    continue;
                }
                segment_path(pts[0], pts[1])
            } else {
                segment_path(Point::new(px, py), Point::new(px2, py2))
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
                let marker_fill =
                    resolve_color_channel(fill_ch, fill_scale, i).unwrap_or(stroke_color);
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
                    &solid_stroke_spec,
                    xform,
                    ctx.shapes,
                    ctx.dpi,
                    pick,
                    /* distribute */ false,
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
}
