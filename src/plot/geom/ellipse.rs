//! `EllipseGeom` — vectorised axis-aligned ellipses with independent
//! x and y radii.
//!
//! One ellipse per row (PointGeom-style: row == mark). The geom uses
//! the **centre + far-edge** convention: `(x, y)` is the centre and
//! `(x2, y2)` is the far edge along each axis. `rx = |x2 - x|`,
//! `ry = |y2 - y|`. The ellipse is always centred at `(x, y)` by
//! construction; off-centre bounding boxes aren't representable (a
//! deliberate constraint — ellipses are by definition centred).
//!
//! Channel naming matches RectGeom and SegmentGeom but the semantic
//! interpretation differs per the cross-geom convention: `(x, y)` is
//! always "the geom's natural anchor" — corner for Rect, start for
//! Segment, centre for Ellipse.
//!
//! Channels consumed:
//!
//! - `"x"`, `"y"` — centre position (required; data; numeric).
//! - `"x2"`, `"y2"` — far edge along each axis (required; data; numeric).
//! - `"x_offset"`, `"y_offset"`, `"x2_offset"`, `"y2_offset"` — per-edge
//!   absolute pt offsets after scale resolution. `x_offset` translates
//!   the centre; `x2_offset` translates the far edge. Independent
//!   offsets on centre vs far edge let you have, e.g., a centre at a
//!   data point with an absolute-pt radius (`x_offset = 0,
//!   x2_offset = r`).
//! - `"x_band"`, `"y_band"`, `"x2_band"`, `"y2_band"` — per-edge
//!   band-fraction offsets. All default to `0.0`. For band-filling
//!   ellipses on a discrete x scale, set `x2_band = 0.5` (centre at
//!   band centre, far edge at right of band → rx = half band width).
//! - `"fill"`, `"fill_opacity"`, `"stroke"`, `"stroke_opacity"`,
//!   `"linewidth"`, `"linetype"`, `"dash_offset"`, `"cap"`, `"join"` —
//!   same styling set as RectGeom.
//!
//! Rotation is not in v1.5 — pending the cross-geom rotation decision
//! flagged in the geom plan. v1.5 ellipses are axis-aligned.

use crate::brush::Brush;
use crate::geometry::{Affine, Point, Vec2};
use crate::path::FillRule;
use crate::primitives::ellipse as ellipse_path;
use crate::scene::SceneBuilder;
use crate::stroke::{Cap, Join, Stroke};

use super::resolve::{
    override_alpha, pt_to_px, resolve_cap_channel, resolve_color_channel, resolve_join_channel,
    resolve_linetype_channel, resolve_number_channel, resolve_number_channel_or, resolve_position,
};
use super::state::{
    filter_declared, require_data_column, validate_channel_lengths, GeomState, KeysStrategy,
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
    ("fill_opacity", ExpectedOutput::Numbers),
    ("stroke_opacity", ExpectedOutput::Numbers),
    ("linewidth", ExpectedOutput::Numbers),
    ("linetype", ExpectedOutput::Linetypes),
    ("dash_offset", ExpectedOutput::Numbers),
    ("cap", ExpectedOutput::Strings),
    ("join", ExpectedOutput::Strings),
];

// ─── EllipseGeom ─────────────────────────────────────────────────────────────

/// A vectorised ellipse geom. One ellipse per row.
pub struct EllipseGeom {
    pub(crate) state: GeomState,
}

crate::impl_geom_inherents!(EllipseGeom);

// ─── BuildableGeom impl ──────────────────────────────────────────────────────

impl BuildableGeom for EllipseGeom {
    fn build_from(builder: GeomBuilder<Self>) -> Self {
        let (keys_opt, channels) = builder.into_parts();

        let n = require_data_column("x", &channels, "EllipseGeom").len();
        for name in ["y", "x2", "y2"] {
            let len = require_data_column(name, &channels, "EllipseGeom").len();
            if len != n {
                panic!(
                    "EllipseGeom::build: \"{name}\" length {len} does not match \"x\" length {n}"
                );
            }
        }
        validate_channel_lengths(&channels, n, "EllipseGeom");

        let declared = filter_declared(&channels, CHANNELS);
        let state = GeomState::from_builder(keys_opt, channels, n, KeysStrategy::PerRow, declared);
        EllipseGeom { state }
    }
}

// ─── Geom impl ───────────────────────────────────────────────────────────────

impl Geom for EllipseGeom {
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

        let x_scale = ctx.scale_for("x");
        let y_scale = ctx.scale_for("y");
        let x2_scale = ctx.scale_for("x2").or(x_scale);
        let y2_scale = ctx.scale_for("y2").or(y_scale);
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
        let fill_opacity_scale = ctx.scale_for("fill_opacity");
        let stroke_opacity_scale = ctx.scale_for("stroke_opacity");
        let linewidth_scale = ctx.scale_for("linewidth");
        let linetype_scale = ctx.scale_for("linetype");
        let dash_offset_scale = ctx.scale_for("dash_offset");
        let cap_scale = ctx.scale_for("cap");
        let join_scale = ctx.scale_for("join");

        let channels = &self.state.channels;
        let x_col = match channels.get("x") {
            Some(Channel::Data(c)) => c,
            _ => return,
        };
        let y_col = match channels.get("y") {
            Some(Channel::Data(c)) => c,
            _ => return,
        };
        let x2_col = match channels.get("x2") {
            Some(Channel::Data(c)) => c,
            _ => return,
        };
        let y2_col = match channels.get("y2") {
            Some(Channel::Data(c)) => c,
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
        let fill_opacity_ch = channels.get("fill_opacity");
        let stroke_opacity_ch = channels.get("stroke_opacity");
        let linewidth_ch = channels.get("linewidth");
        let linetype_ch = channels.get("linetype");
        let dash_offset_ch = channels.get("dash_offset");
        let cap_ch = channels.get("cap");
        let join_ch = channels.get("join");

        for i in 0..n {
            // ── Resolve centre + far edge in pixel space. ──
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

            let mut cx = panel.x0 + x_frac * panel_w;
            let mut x2_px = panel.x0 + x2_frac * panel_w;
            let mut cy = panel.y1 - y_frac * panel_h;
            let mut y2_px = panel.y1 - y2_frac * panel_h;

            if let Some(off) = resolve_number_channel(x_offset_ch, x_offset_scale, i) {
                cx += pt_to_px(off, ctx.dpi);
            }
            if let Some(off) = resolve_number_channel(x2_offset_ch, x2_offset_scale, i) {
                x2_px += pt_to_px(off, ctx.dpi);
            }
            if let Some(off) = resolve_number_channel(y_offset_ch, y_offset_scale, i) {
                cy -= pt_to_px(off, ctx.dpi);
            }
            if let Some(off) = resolve_number_channel(y2_offset_ch, y2_offset_scale, i) {
                y2_px -= pt_to_px(off, ctx.dpi);
            }

            let rx = (x2_px - cx).abs();
            let ry = (y2_px - cy).abs();
            if !rx.is_finite() || !ry.is_finite() || rx <= 0.0 || ry <= 0.0 {
                continue;
            }

            // ── Resolve styling. ──
            let fill_color = override_alpha(
                resolve_color_channel(fill_ch, fill_scale, i),
                resolve_number_channel(fill_opacity_ch, fill_opacity_scale, i),
            );
            let stroke_color = override_alpha(
                resolve_color_channel(stroke_ch, stroke_scale, i),
                resolve_number_channel(stroke_opacity_ch, stroke_opacity_scale, i),
            );
            if fill_color.is_none() && stroke_color.is_none() {
                continue;
            }

            let path = ellipse_path(Point::new(cx, cy), Vec2::new(rx, ry));
            let pick = ctx.pick_id_for_row(i);

            if let Some(fc) = fill_color {
                scene.fill(
                    FillRule::NonZero,
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
                    i,
                    DEFAULT_LINEWIDTH_PT,
                );
                let linewidth_px = pt_to_px(linewidth_pt, ctx.dpi);
                if linewidth_px.is_finite() && linewidth_px > 0.0 {
                    let dash_pattern_pt = resolve_linetype_channel(linetype_ch, linetype_scale, i);
                    let dash_offset_pt =
                        resolve_number_channel_or(dash_offset_ch, dash_offset_scale, i, 0.0);
                    let cap = resolve_cap_channel(cap_ch, cap_scale, i, DEFAULT_CAP);
                    let join = resolve_join_channel(join_ch, join_scale, i, DEFAULT_JOIN);
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
    use crate::plot::geom::{linetype, DirectScaleResolver};
    use crate::plot::scale;
    use crate::plot::value::Value;
    use crate::scene::recording::{Op, RecordingScene};
    use kurbo::Shape;

    fn shapes() -> crate::shape::ShapeRegistry {
        crate::shape::ShapeRegistry::with_builtins()
    }

    fn ctx<'a>(
        panel: Rect,
        registry: &'a crate::shape::ShapeRegistry,
        scales: &'a DirectScaleResolver<'a>,
    ) -> GeomContext<'a> {
        let mut c = GeomContext::new(panel, 96.0, registry, scales);
        c.ticket_base = Some(0);
        c
    }

    fn red() -> Color {
        Color::new([1.0, 0.0, 0.0, 1.0])
    }

    fn first_fill_bbox(scene: &RecordingScene) -> Option<Rect> {
        for op in &scene.ops {
            if let Op::Fill { path, .. } = op {
                let bb = path.bounding_box();
                return Some(Rect::new(bb.x0, bb.y0, bb.x1, bb.y1));
            }
        }
        None
    }

    // ── build() ──

    #[test]
    fn builder_requires_all_four_positions() {
        let r = std::panic::catch_unwind(|| {
            EllipseGeom::builder()
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
        EllipseGeom::builder()
            .set("x", 0.0_f64)
            .set("y", vec![0.0_f64])
            .set("x2", vec![1.0_f64])
            .set("y2", vec![1.0_f64])
            .build();
    }

    #[test]
    #[should_panic(expected = "does not match")]
    fn builder_length_mismatch_panics() {
        EllipseGeom::builder()
            .set("x", vec![0.0_f64, 1.0])
            .set("y", vec![0.0_f64, 1.0])
            .set("x2", vec![1.0_f64])
            .set("y2", vec![1.0_f64])
            .build();
    }

    // ── Drawing: bbox checks ──

    #[test]
    fn ellipse_centre_and_radii_from_far_edge() {
        // Centre at (50, 50), far edge at (70, 60) → rx = 20, ry = 10.
        // Panel 100x100; x_frac 0.5 = 50 px, x2_frac 0.7 = 70 px.
        // y_frac 0.5 = 50 px (panel.y1 - 0.5*100 = 50), y2_frac 0.6 = 40
        // px (panel.y1 - 0.6*100 = 40). So ry = |40 - 50| = 10.
        let g = EllipseGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("x2", vec![0.7_f64])
            .set("y2", vec![0.6_f64])
            .set("fill", red())
            .build();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales),
        );
        let bb = first_fill_bbox(&scene).expect("fill");
        // Ellipse centred at (50, 50) with rx=20, ry=10 → bbox (30, 40, 70, 60).
        assert!((bb.x0 - 30.0).abs() < 1e-6, "x0 = {}", bb.x0);
        assert!((bb.x1 - 70.0).abs() < 1e-6, "x1 = {}", bb.x1);
        assert!((bb.y0 - 40.0).abs() < 1e-6, "y0 = {}", bb.y0);
        assert!((bb.y1 - 60.0).abs() < 1e-6, "y1 = {}", bb.y1);
    }

    #[test]
    fn ellipse_inverted_far_edge_still_works() {
        // x2 < x — abs() should produce the same ellipse.
        let g = EllipseGeom::builder()
            .set("x", vec![0.7_f64])
            .set("y", vec![0.6_f64])
            .set("x2", vec![0.5_f64])
            .set("y2", vec![0.4_f64])
            .set("fill", red())
            .build();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales),
        );
        let bb = first_fill_bbox(&scene).expect("fill");
        // Centre at (70, 40), rx = |50-70| = 20, ry = |60-40| = 20.
        // bbox = (50, 20, 90, 60).
        assert!((bb.x0 - 50.0).abs() < 1e-6, "x0 = {}", bb.x0);
        assert!((bb.x1 - 90.0).abs() < 1e-6, "x1 = {}", bb.x1);
    }

    #[test]
    fn band_fill_on_discrete_x() {
        // Centre at band centre ("B"), x2_band = 0.5 → far edge at right
        // of band. rx = half band width.
        let x = scale::discrete(["A", "B"].into_iter().map(|s| Value::String(Arc::from(s))));
        let resolver = DirectScaleResolver::new().with("x", &x).with("x2", &x);
        let g = EllipseGeom::builder()
            .set("x", vec!["B"])
            .set("x2", vec!["B"])
            .set("y", vec![0.5_f64])
            .set("y2", vec![0.7_f64])
            .set("x2_band", vec![0.5_f64])
            .set("fill", red())
            .build();
        let shapes = shapes();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &resolver),
        );
        let bb = first_fill_bbox(&scene).expect("fill");
        // Band B centre = 75 px; right edge = 100 px. rx = 25 px.
        // bbox.x0 = 50, bbox.x1 = 100.
        assert!((bb.x0 - 50.0).abs() < 1e-6, "x0 = {}", bb.x0);
        assert!((bb.x1 - 100.0).abs() < 1e-6, "x1 = {}", bb.x1);
    }

    #[test]
    fn pt_offset_on_far_edge_only_grows_radius() {
        // Centre at data point, no x_offset; x2 = x (same column),
        // x2_offset = +r pt → rx = r px (after pt→px conversion).
        // 9 pt at 96 dpi = 12 px.
        let g = EllipseGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("x2", vec![0.5_f64])
            .set("y2", vec![0.5_f64])
            .set("x2_offset", vec![9.0_f64])
            .set("y2_offset", vec![9.0_f64])
            .set("fill", red())
            .build();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales),
        );
        let bb = first_fill_bbox(&scene).expect("fill");
        // Centre = 50, rx = ry = 12 → bbox (38, 38, 62, 62). For y, sign
        // flips but |delta| = 12.
        assert!((bb.x0 - 38.0).abs() < 1e-6);
        assert!((bb.x1 - 62.0).abs() < 1e-6);
        assert!((bb.y1 - bb.y0 - 24.0).abs() < 1e-6);
    }

    #[test]
    fn zero_radius_skips_row() {
        // x == x2 with no offsets → rx = 0 → row skipped.
        let g = EllipseGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("x2", vec![0.5_f64])
            .set("y2", vec![0.5_f64])
            .set("fill", red())
            .build();
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
    fn nonfinite_position_skips_row() {
        let g = EllipseGeom::builder()
            .set("x", vec![0.5_f64, f64::NAN])
            .set("y", vec![0.5_f64, 0.5])
            .set("x2", vec![0.7_f64, 0.7])
            .set("y2", vec![0.6_f64, 0.6])
            .set("fill", red())
            .build();
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
    fn no_fill_no_stroke_emits_nothing() {
        let g = EllipseGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("x2", vec![0.7_f64])
            .set("y2", vec![0.6_f64])
            .build();
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
    fn stroke_uses_linewidth_pt_to_px() {
        let g = EllipseGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("x2", vec![0.7_f64])
            .set("y2", vec![0.6_f64])
            .set("stroke", red())
            .set("linewidth", 2.0_f64)
            .build();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales),
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
    fn linetype_dashes_stroke() {
        let g = EllipseGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("x2", vec![0.7_f64])
            .set("y2", vec![0.6_f64])
            .set("stroke", red())
            .set("linetype", Value::Linetype(linetype::dashed()))
            .build();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales),
        );
        let dashed = scene.ops.iter().any(|op| match op {
            Op::Stroke { stroke, .. } => !stroke.dash_pattern.is_empty(),
            _ => false,
        });
        assert!(dashed);
    }

    #[test]
    fn declared_channels_alphabetical() {
        let g = EllipseGeom::builder()
            .set("x", vec![0.0_f64])
            .set("y", vec![0.0_f64])
            .set("x2", vec![1.0_f64])
            .set("y2", vec![1.0_f64])
            .set("fill", red())
            .build();
        let names: Vec<&str> = g.declared_channels().iter().map(|d| d.name).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
        // No "corner_radius" — that belongs to RectGeom.
        assert!(!names.contains(&"corner_radius"));
    }

    #[test]
    fn unique_pick_ids_per_row() {
        let g = EllipseGeom::builder()
            .set("x", vec![0.3_f64, 0.5, 0.7])
            .set("y", vec![0.5_f64, 0.5, 0.5])
            .set("x2", vec![0.35_f64, 0.55, 0.75])
            .set("y2", vec![0.6_f64, 0.6, 0.6])
            .set("fill", red())
            .build();
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
        assert_eq!(picks, vec![1, 2, 3]);
    }
}
