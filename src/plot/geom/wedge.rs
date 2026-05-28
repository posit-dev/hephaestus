//! `WedgeGeom` — vectorised circular wedges (pie slices / annular slices)
//! drawn at scaled `(x, y)` centres.
//!
//! One wedge per row (PointGeom-style: row == mark). The geom is
//! **Cartesian-positioned with polar shape parameters**: `(x, y)` is a
//! regular scaled position in panel space; the wedge's shape is
//! parameterised by `radius`, `radius2`, `theta`, `theta2`. This makes
//! "small pie chart per data point" trivial — `(x, y)` is the data
//! point, the polar dims define the slice.
//!
//! Channels consumed:
//!
//! - `"x"`, `"y"` — wedge centre (required; data; numeric). Standard
//!   `x_offset` / `y_offset` / `x_band` / `y_band` companions apply
//!   uniformly to the centre, translating the whole wedge.
//! - `"radius"` — outer radius in **pt** (optional; default 0).
//!   Converted to px at draw via `dpi / 72`.
//! - `"radius2"` — inner radius in pt (optional; default 0). Set > 0
//!   for annular wedges (donut slices). When 0, the wedge is solid (a
//!   pie slice). Clamped to `<= radius` at draw.
//! - `"radius_band"` — outer-radius contribution as a fraction of the
//!   wedge's containing band. The "band" is the smallest non-zero band
//!   width in panel pixels across the x and y scales at the wedge's
//!   centre. Single discrete axis → uses that axis. Both discrete →
//!   uses the smaller (so the wedge fits inside its cell on both axes).
//!   Both continuous → 0 (no band contribution). Optional; default 0.
//!   `radius_band = 0.5` makes the wedge fit its band — the canonical
//!   "pie chart per category" pattern.
//! - `"radius2_band"` — same for the inner radius, useful for donuts
//!   whose inner radius is a fraction of the outer band-sized radius.
//!
//! All three sizing modes sum in pixel space:
//! ```text
//! radius_px = pt_to_px(radius_pt) + radius_band * band_px(x[i], y[i])
//! ```
//! where `band_px` is the smallest non-zero band width (in panel
//! pixels) across the bound x and y scales. So
//! `radius_band = 0.4, radius = 2` gives "40% of the relevant band
//! plus a 2pt margin".
//!
//! - `"theta"`, `"theta2"` — start and end angles in **radians**
//!   (optional; defaults `0` and `TAU`). **Math convention** with the
//!   plot's y-axis pointing up: `0` is along `+x` (3 o'clock),
//!   positive angles sweep counter-clockwise as the user sees them.
//!   The geom negates angles internally when calling the primitive
//!   (which uses screen-pixel-space convention).
//! - `"fill"`, `"fill_opacity"`, `"stroke"`, `"stroke_opacity"`,
//!   `"linewidth"`, `"linetype"`, `"dash_offset"`, `"cap"`, `"join"` —
//!   same styling set as RectGeom.
//!
//! Default `(theta = 0, theta2 = TAU)` makes a wedge with no angle
//! channels set degenerate into a full circle of `radius`.
//!
//! Polar rotation as a whole (the cross-geom rotation concept flagged
//! in the geom plan) is not in v1.5. To rotate a slice, the user
//! offsets `theta` and `theta2` by the same amount.

use std::f64::consts::TAU;

use crate::brush::Brush;
use crate::geometry::{Affine, Point};
use crate::path::FillRule;
use crate::primitives::{annular_wedge as annular_wedge_path, wedge as wedge_path};
use crate::scene::SceneBuilder;
use crate::stroke::{Cap, Join, Stroke};

use super::resolve::{
    band_width_at, override_alpha, pt_to_px, resolve_cap_channel, resolve_color_channel,
    resolve_join_channel, resolve_linetype_channel, resolve_number_channel,
    resolve_number_channel_or, resolve_pick_id, resolve_position, smallest_nonzero,
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

const DEFAULT_RADIUS2_PT: f64 = 0.0;
const DEFAULT_THETA: f64 = 0.0;
const DEFAULT_THETA2: f64 = TAU;

const CHANNELS: &[(&str, ExpectedOutput)] = &[
    ("x", ExpectedOutput::Numbers),
    ("y", ExpectedOutput::Numbers),
    ("x_offset", ExpectedOutput::Numbers),
    ("y_offset", ExpectedOutput::Numbers),
    ("x_band", ExpectedOutput::Numbers),
    ("y_band", ExpectedOutput::Numbers),
    ("radius", ExpectedOutput::Numbers),
    ("radius2", ExpectedOutput::Numbers),
    ("radius_band", ExpectedOutput::Numbers),
    ("radius2_band", ExpectedOutput::Numbers),
    ("theta", ExpectedOutput::Numbers),
    ("theta2", ExpectedOutput::Numbers),
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

// ─── WedgeGeom ───────────────────────────────────────────────────────────────

/// A vectorised wedge geom. One wedge per row.
pub struct WedgeGeom {
    pub(crate) state: GeomState,
}

crate::impl_geom_inherents!(WedgeGeom);

// ─── BuildableGeom impl ──────────────────────────────────────────────────────

impl BuildableGeom for WedgeGeom {
    fn build_from(builder: GeomBuilder<Self>) -> Self {
        let (keys_opt, channels) = builder.into_parts();

        let n = require_data_column("x", &channels, "WedgeGeom").len();
        let y_len = require_data_column("y", &channels, "WedgeGeom").len();
        if y_len != n {
            panic!("WedgeGeom::build: \"y\" length {y_len} does not match \"x\" length {n}");
        }
        validate_channel_lengths(&channels, n, "WedgeGeom");
        validate_pick_id_channel(&channels, "WedgeGeom");

        let declared = filter_declared(&channels, CHANNELS);
        let state = GeomState::from_builder(keys_opt, channels, n, KeysStrategy::PerRow, declared);
        WedgeGeom { state }
    }
}

// ─── Geom impl ───────────────────────────────────────────────────────────────

impl Geom for WedgeGeom {
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
        let x_offset_scale = ctx.scale_for("x_offset");
        let y_offset_scale = ctx.scale_for("y_offset");
        let x_band_scale = ctx.scale_for("x_band");
        let y_band_scale = ctx.scale_for("y_band");
        let radius_scale = ctx.scale_for("radius");
        let radius2_scale = ctx.scale_for("radius2");
        let radius_band_scale = ctx.scale_for("radius_band");
        let radius2_band_scale = ctx.scale_for("radius2_band");
        let theta_scale = ctx.scale_for("theta");
        let theta2_scale = ctx.scale_for("theta2");
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
        let radius_ch = channels.get("radius");
        let radius2_ch = channels.get("radius2");
        let radius_band_ch = channels.get("radius_band");
        let radius2_band_ch = channels.get("radius2_band");
        let theta_ch = channels.get("theta");
        let theta2_ch = channels.get("theta2");
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

        for i in 0..n {
            let x_band = resolve_number_channel_or(x_band_ch, x_band_scale, i, 0.0);
            let y_band = resolve_number_channel_or(y_band_ch, y_band_scale, i, 0.0);
            let x_frac = resolve_position(x_col.get(i), x_scale, x_band);
            let y_frac = resolve_position(y_col.get(i), y_scale, y_band);
            if !x_frac.is_finite() || !y_frac.is_finite() {
                continue;
            }

            let mut cx = panel.x0 + x_frac * panel_w;
            let mut cy = panel.y1 - y_frac * panel_h;

            if let Some(off) = resolve_number_channel(x_offset_ch, x_offset_scale, i) {
                cx += pt_to_px(off, ctx.dpi);
            }
            if let Some(off) = resolve_number_channel(y_offset_ch, y_offset_scale, i) {
                cy -= pt_to_px(off, ctx.dpi);
            }

            // ── Radii: pt contribution + band contribution. ──
            //
            // Two orthogonal sizing modes that sum in pixel space:
            //   radius_px = pt_to_px(radius_pt) + radius_band * band_px
            //
            // band_px is the smallest non-zero band width across x and y
            // at the wedge's centre, in panel pixels. Single-discrete
            // case → that axis's band. Both-discrete → smaller of the
            // two (so the wedge fits the cell on both axes). Both
            // continuous → 0 (band contribution drops out).
            let x_raw = x_col.get(i);
            let y_raw = y_col.get(i);
            let x_band_px = band_width_at(x_scale, &x_raw) * panel_w;
            let y_band_px = band_width_at(y_scale, &y_raw) * panel_h;
            let band_px = smallest_nonzero(x_band_px, y_band_px);

            let radius_pt = resolve_number_channel_or(radius_ch, radius_scale, i, 0.0);
            let r_b = resolve_number_channel_or(radius_band_ch, radius_band_scale, i, 0.0);
            let radius_px = pt_to_px(radius_pt, ctx.dpi) + r_b * band_px;
            if !radius_px.is_finite() || radius_px <= 0.0 {
                continue;
            }

            let radius2_pt =
                resolve_number_channel_or(radius2_ch, radius2_scale, i, DEFAULT_RADIUS2_PT);
            let r2_b = resolve_number_channel_or(radius2_band_ch, radius2_band_scale, i, 0.0);
            let radius2_px = (pt_to_px(radius2_pt, ctx.dpi) + r2_b * band_px).clamp(0.0, radius_px);

            // Angles: math convention (CCW positive with y-up) → negate
            // when calling the primitive, which uses screen-pixel-space
            // convention (CCW positive with y-down, which looks CW on
            // screen).
            let theta = resolve_number_channel_or(theta_ch, theta_scale, i, DEFAULT_THETA);
            let theta2 = resolve_number_channel_or(theta2_ch, theta2_scale, i, DEFAULT_THETA2);
            if !theta.is_finite() || !theta2.is_finite() {
                continue;
            }
            let prim_start = -theta;
            let prim_sweep = -(theta2 - theta);
            if prim_sweep == 0.0 {
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

            let centre = Point::new(cx, cy);
            let path = if radius2_px > 0.0 {
                annular_wedge_path(centre, radius2_px, radius_px, prim_start, prim_sweep)
            } else {
                wedge_path(centre, radius_px, prim_start, prim_sweep)
            };

            let pick = resolve_pick_id(pick_id_ch, pick_id_scale, i);

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

// ─── Helpers ─────────────────────────────────────────────────────────────────

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
    use std::f64::consts::{PI, TAU};
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
        GeomContext::new(panel, 96.0, registry, scales)
    }

    fn red() -> Color {
        Color::new([1.0, 0.0, 0.0, 1.0])
    }

    // ── build() ──

    #[test]
    #[should_panic(expected = "missing required channel \"x\"")]
    fn missing_x_panics() {
        WedgeGeom::builder()
            .set("y", vec![0.5_f64])
            .set("radius", vec![10.0_f64])
            .build();
    }

    #[test]
    fn missing_radius_is_no_op() {
        // No `radius` and no band channels → all radius contributions
        // are 0 → row skipped at draw.
        let g = WedgeGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
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
    #[should_panic(expected = "does not match")]
    fn length_mismatch_panics() {
        WedgeGeom::builder()
            .set("x", vec![0.5_f64, 0.7])
            .set("y", vec![0.5_f64])
            .set("radius", vec![10.0_f64, 10.0])
            .build();
    }

    // ── Drawing ──

    fn first_fill_bbox(scene: &RecordingScene) -> Option<Rect> {
        for op in &scene.ops {
            if let Op::Fill { path, .. } = op {
                let bb = path.bounding_box();
                return Some(Rect::new(bb.x0, bb.y0, bb.x1, bb.y1));
            }
        }
        None
    }

    #[test]
    fn full_circle_with_default_angles() {
        // No theta/theta2 → defaults 0..TAU → full circle. radius=24pt
        // at 96dpi = 32 px.
        let g = WedgeGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("radius", vec![24.0_f64])
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
        // Centred at (50, 50), radius 32 px → bbox roughly (18, 18, 82, 82).
        let cx = (bb.x0 + bb.x1) * 0.5;
        let cy = (bb.y0 + bb.y1) * 0.5;
        assert!((cx - 50.0).abs() < 1e-3, "cx = {cx}");
        assert!((cy - 50.0).abs() < 1e-3, "cy = {cy}");
        assert!((bb.width() - 64.0).abs() < 1.0, "width = {}", bb.width());
        assert!((bb.height() - 64.0).abs() < 1.0, "height = {}", bb.height());
    }

    #[test]
    fn top_right_quarter_via_math_convention() {
        // theta=0 (3 o'clock), theta2=PI/2 (12 o'clock, math convention).
        // Wedge should occupy the top-right quadrant of the bounding
        // circle. Bbox should be roughly (cx, cy - r, cx + r, cy).
        let g = WedgeGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("radius", vec![24.0_f64])
            .set("theta", vec![0.0_f64])
            .set("theta2", vec![PI / 2.0])
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
        // Centre 50,50; radius 32 px. Top-right quarter:
        //   x0 = cx = 50 (left edge is centre)
        //   x1 = cx + r = 82
        //   y0 = cy - r = 18 (top edge in pixels, where +y in math is up = smaller pixel y)
        //   y1 = cy = 50 (bottom edge is centre)
        assert!((bb.x0 - 50.0).abs() < 1.0, "x0 = {}", bb.x0);
        assert!((bb.x1 - 82.0).abs() < 1.0, "x1 = {}", bb.x1);
        assert!((bb.y0 - 18.0).abs() < 1.0, "y0 = {}", bb.y0);
        assert!((bb.y1 - 50.0).abs() < 1.0, "y1 = {}", bb.y1);
    }

    #[test]
    fn annular_wedge_when_radius2_set() {
        let g = WedgeGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("radius", vec![24.0_f64])
            .set("radius2", vec![12.0_f64])
            .set("fill", red())
            .build();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales),
        );
        // Annular wedge as a full sweep (default theta range) — bbox
        // matches the outer circle. Path should have moves (one move
        // per ring start) — at least 2 moves for outer + inner arcs.
        for op in &scene.ops {
            if let Op::Fill { path, .. } = op {
                let move_count = path
                    .elements()
                    .iter()
                    .filter(|el| matches!(el, kurbo::PathEl::MoveTo(_)))
                    .count();
                assert!(
                    move_count >= 1,
                    "annular wedge should have at least one move; got {move_count}"
                );
                return;
            }
        }
        panic!("no fill emitted");
    }

    #[test]
    fn radius2_clamped_to_radius() {
        // radius2 > radius should clamp internally — no panic, no
        // degenerate result. We assert the path is non-empty.
        let g = WedgeGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("radius", vec![12.0_f64])
            .set("radius2", vec![100.0_f64]) // > radius
            .set("fill", red())
            .build();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales),
        );
        // Doesn't panic; whether it produces a visible fill is an
        // implementation detail (a thin annulus). The point is no
        // crash.
        let _ = scene;
    }

    #[test]
    fn zero_radius_skips_row() {
        let g = WedgeGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("radius", vec![0.0_f64])
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
    fn zero_sweep_skips_row() {
        // theta == theta2 → no arc → row skipped.
        let g = WedgeGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("radius", vec![24.0_f64])
            .set("theta", vec![0.5_f64])
            .set("theta2", vec![0.5_f64])
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
        let g = WedgeGeom::builder()
            .set("x", vec![0.5_f64, f64::NAN])
            .set("y", vec![0.5_f64, 0.5])
            .set("radius", vec![24.0_f64, 24.0])
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
        let g = WedgeGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("radius", vec![24.0_f64])
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
    fn pie_slice_three_wedges_share_centre() {
        // Three wedges at the same centre, splitting the circle into
        // three equal sectors. Pie chart at one point.
        let third = TAU / 3.0;
        let g = WedgeGeom::builder()
            .set("x", vec![0.5_f64, 0.5, 0.5])
            .set("y", vec![0.5_f64, 0.5, 0.5])
            .set("radius", vec![24.0_f64, 24.0, 24.0])
            .set("theta", vec![0.0_f64, third, 2.0 * third])
            .set("theta2", vec![third, 2.0 * third, TAU])
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
        assert_eq!(fills, 3);
    }

    #[test]
    fn stroke_uses_linewidth_pt_to_px() {
        let g = WedgeGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("radius", vec![24.0_f64])
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
        let g = WedgeGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("radius", vec![24.0_f64])
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
        let g = WedgeGeom::builder()
            .set("x", vec![0.0_f64])
            .set("y", vec![0.0_f64])
            .set("radius", vec![10.0_f64])
            .set("fill", red())
            .build();
        let names: Vec<&str> = g.declared_channels().iter().map(|d| d.name).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
        // Polar dims live alongside the standard ones.
        assert!(names.contains(&"radius"));
        // x2/y2 are NOT WedgeGeom channels.
        assert!(!names.contains(&"x2"));
        assert!(!names.contains(&"y2"));
    }

    #[test]
    fn pick_id_channel_passes_through_per_row() {
        let g = WedgeGeom::builder()
            .set("x", vec![0.3_f64, 0.5, 0.7])
            .set("y", vec![0.5_f64, 0.5, 0.5])
            .set("radius", vec![15.0_f64, 15.0, 15.0])
            .set("fill", red())
            .set("pick_id", vec![7_i64, 8, 9])
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
        assert_eq!(picks, vec![7, 8, 9]);
    }

    #[test]
    fn radius_band_sizes_to_discrete_x_band() {
        // Two-category x scale on a 100-px-wide panel → band width = 50 px.
        // radius_band = 0.5 → radius = 25 px. Centred at band B (75 px).
        let x_scale = scale::discrete(["A", "B"].into_iter().map(|s| Value::String(Arc::from(s))));
        let resolver = DirectScaleResolver::new().with("x", &x_scale);
        let g = WedgeGeom::builder()
            .set("x", vec!["B"])
            .set("y", vec![0.5_f64])
            .set("radius_band", vec![0.5_f64])
            .set("fill", red())
            .build();
        let shapes = shapes();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &resolver),
        );
        let bb = first_fill_bbox(&scene).expect("fill");
        // Centred at 75 (band B), radius 25 px → bbox (50, 25, 100, 75).
        let cx = (bb.x0 + bb.x1) * 0.5;
        assert!((cx - 75.0).abs() < 1.0, "cx = {cx}");
        assert!((bb.width() - 50.0).abs() < 1.0, "width = {}", bb.width());
    }

    #[test]
    fn radius_band_no_op_on_continuous_axes() {
        // band_width = 0 on continuous → radius_band contributes 0.
        // With no pt radius set, the row is skipped.
        let x_scale = scale::continuous(0.0..=10.0);
        let resolver = DirectScaleResolver::new().with("x", &x_scale);
        let g = WedgeGeom::builder()
            .set("x", vec![5.0_f64])
            .set("y", vec![0.5_f64])
            .set("radius_band", vec![0.5_f64])
            .set("fill", red())
            .build();
        let shapes = shapes();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &resolver),
        );
        assert!(scene.ops.is_empty(), "expected no draw on continuous x");
    }

    #[test]
    fn radius_pt_and_band_sum() {
        // pt = 9 (12 px at 96 dpi), x_band on 2-cat scale = 50 px wide,
        // radius_band = 0.5 → 25 px band contribution.
        // Total radius = 12 + 25 = 37 px.
        let x_scale = scale::discrete(["A", "B"].into_iter().map(|s| Value::String(Arc::from(s))));
        let resolver = DirectScaleResolver::new().with("x", &x_scale);
        let g = WedgeGeom::builder()
            .set("x", vec!["A"])
            .set("y", vec![0.5_f64])
            .set("radius", vec![9.0_f64])
            .set("radius_band", vec![0.5_f64])
            .set("fill", red())
            .build();
        let shapes = shapes();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &resolver),
        );
        let bb = first_fill_bbox(&scene).expect("fill");
        let radius_px = bb.width() * 0.5;
        assert!((radius_px - 37.0).abs() < 1.0, "radius_px = {radius_px}");
    }

    #[test]
    fn radius2_band_creates_band_sized_donut() {
        // Outer radius via band sizing (0.5 → 25 px); inner radius via
        // band sizing (0.25 → 12.5 px). Donut between 12.5 and 25 px.
        let x_scale = scale::discrete(["A", "B"].into_iter().map(|s| Value::String(Arc::from(s))));
        let resolver = DirectScaleResolver::new().with("x", &x_scale);
        let g = WedgeGeom::builder()
            .set("x", vec!["A"])
            .set("y", vec![0.5_f64])
            .set("radius_band", vec![0.5_f64])
            .set("radius2_band", vec![0.25_f64])
            .set("fill", red())
            .build();
        let shapes = shapes();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &resolver),
        );
        let bb = first_fill_bbox(&scene).expect("fill");
        // Outer radius dominates the bbox: width should be ~50 px.
        assert!((bb.width() - 50.0).abs() < 1.5);
        // Path should contain multiple arcs (outer + inner) — sanity check
        // by counting MoveTo elements (>=1 expected for annulus).
        for op in &scene.ops {
            if let Op::Fill { path, .. } = op {
                let moves = path
                    .elements()
                    .iter()
                    .filter(|el| matches!(el, kurbo::PathEl::MoveTo(_)))
                    .count();
                assert!(moves >= 1, "annular path expected");
                return;
            }
        }
        panic!("no fill");
    }

    #[test]
    fn x_offset_translates_centre_in_pt() {
        let g = WedgeGeom::builder()
            .set("x", vec![0.5_f64])
            .set("y", vec![0.5_f64])
            .set("radius", vec![24.0_f64])
            .set("x_offset", vec![9.0_f64]) // +12 px
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
        // Centre 50 + 12 = 62, radius 32 → x0 ≈ 30, x1 ≈ 94.
        let cx = (bb.x0 + bb.x1) * 0.5;
        assert!((cx - 62.0).abs() < 1e-3, "cx = {cx}");
    }
}
