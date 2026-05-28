//! `RectGeom` — vectorised axis-aligned rectangles drawn at scaled
//! `(x, y) – (x2, y2)` diagonal corners.
//!
//! One rectangle per row (PointGeom-style: row == mark). The user
//! supplies two opposite corners; the geom computes `min`/`max`
//! internally so corner ordering doesn't matter.
//!
//! Channels consumed:
//!
//! - `"x"`, `"y"` — one corner of the rect (required; data; numeric).
//! - `"x2"`, `"y2"` — the opposite corner (required; data; numeric).
//! - `"x_offset"`, `"y_offset"`, `"x2_offset"`, `"y2_offset"` —
//!   absolute pt offsets added per edge after scale resolution.
//! - `"x_band"`, `"y_band"`, `"x2_band"`, `"y2_band"` — band-fraction
//!   offsets folded into the scale's `map_with_offset` per edge.
//!   **Defaults**: `x_band = -0.5`, `x2_band = +0.5`, `y_band = 0.0`,
//!   `y2_band = 0.0`. The non-zero x defaults give the canonical
//!   "bar fills its band" behaviour for discrete x scales: bind both
//!   `x` and `x2` to the same category column, and each mark fills
//!   the full band. Continuous x scales have `band_width = 0` so the
//!   defaults are no-ops there.
//! - `"corner_radius"` — uniform corner radius in pt (per-mark;
//!   default 0pt = sharp corners). Clamped by kurbo to at most half
//!   the shorter side.
//! - `"fill"`, `"stroke"`, `"fill_opacity"`, `"stroke_opacity"`,
//!   `"linewidth"`, `"linetype"`, `"dash_offset"`, `"cap"`, `"join"`
//!   — same styling set as LineGeom / PointGeom.
//!
//! Stroke is drawn around the rect outline. Fill and stroke are
//! independent — set one, the other, or both.

use crate::brush::Brush;
use crate::geometry::{Affine, Rect};
use crate::path::FillRule;
use crate::primitives::{rect as rect_path, rounded_rect};
use crate::scene::SceneBuilder;
use crate::stroke::{Cap, Join, Stroke};

use super::resolve::{
    override_alpha, pt_to_px, resolve_cap_channel, resolve_color_channel, resolve_join_channel,
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

/// Default band offset for `"x"`: `-0.5` so the left edge sits at the
/// band's left side on discrete x scales. No effect on continuous scales.
const DEFAULT_X_BAND: f64 = -0.5;
/// Default band offset for `"x2"`: `+0.5` so the right edge sits at the
/// band's right side on discrete x scales.
const DEFAULT_X2_BAND: f64 = 0.5;
/// Default band offset for `"y"`: `0.0`. Users on discrete y scales who
/// want band-filling bars set this explicitly.
const DEFAULT_Y_BAND: f64 = 0.0;
const DEFAULT_Y2_BAND: f64 = 0.0;

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
    ("corner_radius", ExpectedOutput::Numbers),
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
    ("pick_id", ExpectedOutput::Numbers),
];

// ─── RectGeom ────────────────────────────────────────────────────────────────

/// A vectorised rectangle geom. One rect per row.
pub struct RectGeom {
    pub(crate) state: GeomState,
}

crate::impl_geom_inherents!(RectGeom);

// ─── BuildableGeom impl ──────────────────────────────────────────────────────

impl BuildableGeom for RectGeom {
    fn build_from(builder: GeomBuilder<Self>) -> Self {
        let (keys_opt, channels) = builder.into_parts();

        let n = require_data_column("x", &channels, "RectGeom").len();
        for name in ["y", "x2", "y2"] {
            let len = require_data_column(name, &channels, "RectGeom").len();
            if len != n {
                panic!("RectGeom::build: \"{name}\" length {len} does not match \"x\" length {n}");
            }
        }
        validate_channel_lengths(&channels, n, "RectGeom");
        validate_pick_id_channel(&channels, "RectGeom");

        let declared = filter_declared(&channels, CHANNELS);
        let state = GeomState::from_builder(keys_opt, channels, n, KeysStrategy::PerRow, declared);
        RectGeom { state }
    }
}

// ─── Geom impl ───────────────────────────────────────────────────────────────

impl Geom for RectGeom {
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
        let corner_radius_scale = ctx.scale_for("corner_radius");
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
        let expand_scale = ctx.scale_for("expand");

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
        let corner_radius_ch = channels.get("corner_radius");
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
        let expand_ch = channels.get("expand");

        for i in 0..n {
            // ── Resolve the four corner positions. ──
            let x_band = resolve_number_channel_or(x_band_ch, x_band_scale, i, DEFAULT_X_BAND);
            let x2_band = resolve_number_channel_or(x2_band_ch, x2_band_scale, i, DEFAULT_X2_BAND);
            let y_band = resolve_number_channel_or(y_band_ch, y_band_scale, i, DEFAULT_Y_BAND);
            let y2_band = resolve_number_channel_or(y2_band_ch, y2_band_scale, i, DEFAULT_Y2_BAND);

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
            let mut py = panel.y1 - y_frac * panel_h; // y flips
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

            // Build a normalised rect — robust to corner ordering and y axis flip.
            // `expand` grows the rect outward (positive) / shrinks inward
            // (negative) on every side by the same pt amount.
            let expand_px = pt_to_px(
                resolve_number_channel_or(expand_ch, expand_scale, i, 0.0),
                ctx.dpi,
            );
            let x0 = px.min(px2) - expand_px;
            let y0 = py.min(py2) - expand_px;
            let x1 = px.max(px2) + expand_px;
            let y1 = py.max(py2) + expand_px;
            let r = Rect::new(x0, y0, x1, y1);
            if !r.is_finite() || r.width() <= 0.0 || r.height() <= 0.0 {
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
            let linewidth_pt =
                resolve_number_channel_or(linewidth_ch, linewidth_scale, i, DEFAULT_LINEWIDTH_PT);
            let linewidth_px = pt_to_px(linewidth_pt, ctx.dpi);
            let corner_radius_pt =
                resolve_number_channel_or(corner_radius_ch, corner_radius_scale, i, 0.0);
            let corner_radius_px = pt_to_px(corner_radius_pt, ctx.dpi).max(0.0);
            let dash_pattern_pt = resolve_linetype_channel(linetype_ch, linetype_scale, i);
            let dash_offset_pt =
                resolve_number_channel_or(dash_offset_ch, dash_offset_scale, i, 0.0);
            let cap = resolve_cap_channel(cap_ch, cap_scale, i, DEFAULT_CAP);
            let join = resolve_join_channel(join_ch, join_scale, i, DEFAULT_JOIN);

            // ── Build the path. ──
            let path = if corner_radius_px > 0.0 {
                rounded_rect(r, corner_radius_px)
            } else {
                rect_path(r)
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
                if linewidth_px.is_finite() && linewidth_px > 0.0 {
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
            RectGeom::builder()
                .set("x", vec![0.0_f64, 1.0])
                .set("y", vec![0.0_f64, 1.0])
                .set("x2", vec![0.5_f64, 1.5])
                // y2 missing
                .build()
        });
        assert!(r.is_err());
    }

    #[test]
    #[should_panic(expected = "must be data, not constant")]
    fn builder_x_constant_panics() {
        RectGeom::builder()
            .set("x", 0.0_f64)
            .set("y", vec![0.0_f64])
            .set("x2", vec![1.0_f64])
            .set("y2", vec![1.0_f64])
            .build();
    }

    #[test]
    #[should_panic(expected = "does not match")]
    fn builder_x2_length_mismatch_panics() {
        RectGeom::builder()
            .set("x", vec![0.0_f64, 1.0, 2.0])
            .set("y", vec![0.0_f64, 1.0, 2.0])
            .set("x2", vec![0.5_f64, 1.5]) // wrong length
            .set("y2", vec![1.0_f64, 2.0, 3.0])
            .build();
    }

    // ── Drawing ──

    fn first_fill_rect(scene: &RecordingScene) -> Option<GeomRect> {
        for op in &scene.ops {
            if let Op::Fill { path, .. } = op {
                let bbox = path.bounding_box();
                return Some(GeomRect::new(bbox.x0, bbox.y0, bbox.x1, bbox.y1));
            }
        }
        None
    }

    #[test]
    fn fill_only_at_continuous_corners() {
        // 100x100 panel, scaleless: fractions are values directly. A rect
        // from x=0.1..x2=0.4 and y=0.1..y2=0.4 should fill a 30x30 box
        // somewhere in the panel.
        let g = RectGeom::builder()
            .set("x", vec![0.1_f64])
            .set("y", vec![0.1_f64])
            .set("x2", vec![0.4_f64])
            .set("y2", vec![0.4_f64])
            .set("fill", red())
            .build();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(GeomRect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales),
        );
        let r = first_fill_rect(&scene).expect("fill");
        // Without scale binding, band defaults apply: x_band=-0.5,
        // x2_band=+0.5 with band_width = 0 (no scale) ⇒ no effect. So
        // x_frac = 0.1, x2_frac = 0.4.
        assert!((r.width() - 30.0).abs() < 1e-6, "width = {}", r.width());
        assert!((r.height() - 30.0).abs() < 1e-6, "height = {}", r.height());
    }

    #[test]
    fn band_fill_default_on_discrete_x_scale() {
        // Two categories, panel 100 wide → each band is 50 px. With
        // x and x2 both bound to "B" and default bands -0.5/+0.5, the
        // rect should span band B exactly (50 px wide).
        let x = scale::discrete(["A", "B"].into_iter().map(|s| Value::String(Arc::from(s))));
        let resolver = DirectScaleResolver::new().with("x", &x).with("x2", &x);
        let g = RectGeom::builder()
            .set("x", vec!["B"])
            .set("x2", vec!["B"])
            .set("y", vec![0.2_f64])
            .set("y2", vec![0.8_f64])
            .set("fill", red())
            .build();
        let shapes = shapes();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(GeomRect::new(0.0, 0.0, 100.0, 100.0), &shapes, &resolver),
        );
        let r = first_fill_rect(&scene).expect("fill");
        // Band B: x range [50, 100]. Width 50.
        assert!((r.width() - 50.0).abs() < 1e-6, "width = {}", r.width());
        assert!((r.x0 - 50.0).abs() < 1e-6, "x0 = {}", r.x0);
        assert!((r.x1 - 100.0).abs() < 1e-6, "x1 = {}", r.x1);
    }

    #[test]
    fn dodge_within_band_via_per_row_bands() {
        // Two groups in band B, side by side. group 0: [-0.5, 0.0],
        // group 1: [0.0, +0.5]. Each should be 25 px wide.
        let x = scale::discrete(["A", "B"].into_iter().map(|s| Value::String(Arc::from(s))));
        let resolver = DirectScaleResolver::new().with("x", &x).with("x2", &x);
        let g = RectGeom::builder()
            .set("x", vec!["B", "B"])
            .set("x2", vec!["B", "B"])
            .set("y", vec![0.2_f64, 0.2])
            .set("y2", vec![0.8_f64, 0.8])
            .set("x_band", vec![-0.5_f64, 0.0])
            .set("x2_band", vec![0.0_f64, 0.5])
            .set("fill", red())
            .build();
        let shapes = shapes();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(GeomRect::new(0.0, 0.0, 100.0, 100.0), &shapes, &resolver),
        );
        let widths: Vec<f64> = scene
            .ops
            .iter()
            .filter_map(|op| match op {
                Op::Fill { path, .. } => {
                    let bb = path.bounding_box();
                    Some(bb.x1 - bb.x0)
                }
                _ => None,
            })
            .collect();
        assert_eq!(widths.len(), 2);
        for w in &widths {
            assert!((w - 25.0).abs() < 1e-6, "width = {w}");
        }
    }

    #[test]
    fn continuous_scale_band_defaults_have_no_effect() {
        // band_width on continuous scale = 0, so the -0.5/+0.5 defaults
        // contribute 0 to the fraction. The rect spans [x, x2] verbatim.
        let x = scale::continuous(0.0..=10.0);
        let resolver = DirectScaleResolver::new().with("x", &x).with("x2", &x);
        let g = RectGeom::builder()
            .set("x", vec![2.0_f64])
            .set("y", vec![0.0_f64])
            .set("x2", vec![8.0_f64])
            .set("y2", vec![1.0_f64])
            .set("fill", red())
            .build();
        let shapes = shapes();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(GeomRect::new(0.0, 0.0, 100.0, 100.0), &shapes, &resolver),
        );
        let r = first_fill_rect(&scene).expect("fill");
        // x: 2/10 = 0.2 → 20 px; x2: 8/10 = 0.8 → 80 px.
        assert!((r.x0 - 20.0).abs() < 1e-6);
        assert!((r.x1 - 80.0).abs() < 1e-6);
    }

    #[test]
    fn diagonal_corner_order_doesnt_matter() {
        // x > x2, y > y2 — geom should still produce the same rect via
        // internal min/max.
        let g = RectGeom::builder()
            .set("x", vec![0.4_f64])
            .set("x2", vec![0.1_f64])
            .set("y", vec![0.4_f64])
            .set("y2", vec![0.1_f64])
            .set("fill", red())
            .build();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(GeomRect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales),
        );
        let r = first_fill_rect(&scene).expect("fill");
        assert!((r.width() - 30.0).abs() < 1e-6);
        assert!((r.height() - 30.0).abs() < 1e-6);
    }

    #[test]
    fn rect_with_corner_radius_uses_rounded_path() {
        let g = RectGeom::builder()
            .set("x", vec![0.1_f64])
            .set("y", vec![0.1_f64])
            .set("x2", vec![0.5_f64])
            .set("y2", vec![0.5_f64])
            .set("fill", red())
            .set("corner_radius", 4.0_f64)
            .build();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(GeomRect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales),
        );
        // Rounded paths contain curves; sharp rects are line-only.
        let curves = scene
            .ops
            .iter()
            .filter_map(|op| match op {
                Op::Fill { path, .. } => Some(path.elements().iter().any(|el| {
                    matches!(
                        el,
                        kurbo::PathEl::CurveTo(_, _, _) | kurbo::PathEl::QuadTo(_, _)
                    )
                })),
                _ => None,
            })
            .next();
        assert_eq!(curves, Some(true), "rounded rect should contain curves");
    }

    #[test]
    fn no_fill_no_stroke_emits_nothing() {
        let g = RectGeom::builder()
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
    fn stroke_uses_linewidth_pt_to_px() {
        let g = RectGeom::builder()
            .set("x", vec![0.1_f64])
            .set("y", vec![0.1_f64])
            .set("x2", vec![0.5_f64])
            .set("y2", vec![0.5_f64])
            .set("stroke", red())
            .set("linewidth", 2.0_f64) // 2pt at 96dpi = 8/3 px
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
    fn linetype_dashes_stroke() {
        let g = RectGeom::builder()
            .set("x", vec![0.1_f64])
            .set("y", vec![0.1_f64])
            .set("x2", vec![0.5_f64])
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
        let dashed = scene
            .ops
            .iter()
            .filter_map(|op| match op {
                Op::Stroke { stroke, .. } => Some(!stroke.dash_pattern.is_empty()),
                _ => None,
            })
            .any(|b| b);
        assert!(dashed);
    }

    #[test]
    fn x_offset_translates_left_edge_in_pt() {
        let g = RectGeom::builder()
            .set("x", vec![0.2_f64])
            .set("y", vec![0.0_f64])
            .set("x2", vec![0.8_f64])
            .set("y2", vec![1.0_f64])
            .set("fill", red())
            .set("x_offset", 9.0_f64) // 12 px right
            .build();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(GeomRect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales),
        );
        let r = first_fill_rect(&scene).expect("fill");
        // x_frac=0.2 → 20 px; +12 px offset → 32 px. x2_frac=0.8 → 80 px.
        assert!((r.x0 - 32.0).abs() < 1e-6, "x0 = {}", r.x0);
        assert!((r.x1 - 80.0).abs() < 1e-6, "x1 = {}", r.x1);
    }

    #[test]
    fn nonfinite_position_skips_row() {
        let g = RectGeom::builder()
            .set("x", vec![0.1_f64, f64::NAN])
            .set("y", vec![0.1_f64, 0.1])
            .set("x2", vec![0.5_f64, 0.5])
            .set("y2", vec![0.5_f64, 0.5])
            .set("fill", red())
            .build();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(GeomRect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales),
        );
        let fills = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Fill { .. }))
            .count();
        assert_eq!(fills, 1);
    }

    #[test]
    fn declared_channels_alphabetical() {
        let g = RectGeom::builder()
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
    }

    #[test]
    fn expand_grows_rect_on_all_sides() {
        use kurbo::Shape;
        // Rect from (20, 20) → (80, 80) in panel-fraction Raw, so
        // pixel bbox is 20..80 = 60 wide. expand = 3pt = 4 px → bbox
        // 16..84 = 68 wide.
        let g = RectGeom::builder()
            .set("x", Raw(vec![0.2_f64]))
            .set("y", Raw(vec![0.2_f64]))
            .set("x2", Raw(vec![0.8_f64]))
            .set("y2", Raw(vec![0.8_f64]))
            .set("fill", red())
            .set("expand", 3.0_f64)
            .build();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales),
        );
        let bb = scene
            .ops
            .iter()
            .find_map(|op| match op {
                Op::Fill { path, .. } => Some(path.bounding_box()),
                _ => None,
            })
            .expect("fill");
        let expand_px = 3.0 * 96.0 / 72.0;
        let expected = 60.0 + 2.0 * expand_px;
        assert!((bb.width() - expected).abs() < 0.5);
        assert!((bb.height() - expected).abs() < 0.5);
    }

    #[test]
    fn negative_expand_shrinks_rect() {
        use kurbo::Shape;
        let g = RectGeom::builder()
            .set("x", Raw(vec![0.2_f64]))
            .set("y", Raw(vec![0.2_f64]))
            .set("x2", Raw(vec![0.8_f64]))
            .set("y2", Raw(vec![0.8_f64]))
            .set("fill", red())
            .set("expand", -3.0_f64)
            .build();
        let shapes = shapes();
        let scales = DirectScaleResolver::new();
        let mut scene = RecordingScene::default();
        g.draw(
            &mut scene,
            &ctx(Rect::new(0.0, 0.0, 100.0, 100.0), &shapes, &scales),
        );
        let bb = scene
            .ops
            .iter()
            .find_map(|op| match op {
                Op::Fill { path, .. } => Some(path.bounding_box()),
                _ => None,
            })
            .expect("fill");
        let expand_px = 3.0 * 96.0 / 72.0;
        let expected = 60.0 - 2.0 * expand_px;
        assert!((bb.width() - expected).abs() < 0.5);
    }
}
