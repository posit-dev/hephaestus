//! Polar axis chrome — the radius axis (baseline + ticks + labels
//! along the `theta_start` spoke, via the shared linear-axis helper)
//! and the angular axis (minor + major tick marks + labels around
//! the outer ring).
//!
//! The **in-panel** chrome — background fill, radial / angular grid
//! lines, panel outline — comes from
//! [`crate::plot::chrome::panel`] and is shared with the cartesian
//! projection. This module only handles what sits *on the edge* of
//! the plotting area (the axes proper).
//!
//! Drawn from `Plot::draw_chrome_into` because polar projections
//! use [`ChromeStrategy::InsidePanel`](crate::plot::projection::ChromeStrategy::InsidePanel).
//! Labels may extend outside the inscribed disk — for now they're not
//! clipped, and overflow into whatever space the panel rect has
//! around the disk.

use crate::brush::Brush;
use crate::geometry::{Affine, Point, Rect};
use crate::pick::PickId;
use crate::plot::chrome::linear_axis::{
    axis_ink, draw_axis_label, draw_linear_axis_at, pt_to_px, AxisLabelAt, LABEL_FONT_SIZE_PT,
    LABEL_GAP_PT, MINOR_TICK_LENGTH_PT, STROKE_WIDTH_PT, TICK_LENGTH_PT,
};
use crate::plot::projection::PolarProjection;
use crate::plot::scale::Scale;
use crate::primitives::segment;
use crate::scales::breaks::DEFAULT_BREAK_COUNT;
use crate::scales::value::Value;
use crate::scene::SceneBuilder;
use crate::stroke::Stroke;
use crate::text::TextStyle;

/// Draw polar chrome (arcs, spokes, labels, side caps) into `panel`.
/// Called by `Plot::draw_chrome_into` when the projection's chrome
/// strategy is `InsidePanel`.
///
/// `angle_scale` / `radius_scale` are the scales bound to the
/// projection's `angle_channel` / `radius_channel`. Either may be
/// `None`; in that case the corresponding chrome component is omitted.
pub fn draw_polar_chrome(
    scene: &mut dyn SceneBuilder,
    panel: Rect,
    polar: &PolarProjection,
    angle_scale: Option<&Scale>,
    radius_scale: Option<&Scale>,
    dpi: f64,
) {
    let g = polar.geometry(panel);
    if g.r_outer <= 0.0 {
        return;
    }

    let span = polar.theta_end - polar.theta_start;
    let is_full_circle = (span.abs() - std::f64::consts::TAU).abs() < 1e-6;

    let stroke_px = pt_to_px(STROKE_WIDTH_PT, dpi);
    let tick_px = pt_to_px(TICK_LENGTH_PT, dpi);
    let minor_tick_px = pt_to_px(MINOR_TICK_LENGTH_PT, dpi);
    let label_gap_px = pt_to_px(LABEL_GAP_PT, dpi);
    let brush = Brush::Solid(axis_ink());
    let stroke = Stroke::new(stroke_px);
    let style = TextStyle::new(LABEL_FONT_SIZE_PT);

    // ── Radius axis (baseline + ticks + labels along the
    // theta_start spoke, drawn through the shared linear-axis
    // helper — same convention as the cartesian axes) ──
    if let Some(scale) = radius_scale {
        let majors: Vec<(f64, String)> = scale
            .breaks(DEFAULT_BREAK_COUNT)
            .iter()
            .filter(|v| !matches!(v, Value::Null))
            .filter_map(|v| scale.map(v).as_number().map(|f| (f, scale.format(v))))
            .filter(|(f, _)| f.is_finite())
            .collect();
        let minors: Vec<f64> = scale
            .minor_breaks(DEFAULT_BREAK_COUNT)
            .into_iter()
            .filter(|v| !matches!(v, Value::Null))
            .filter_map(|v| scale.map(&v).as_number())
            .filter(|f| f.is_finite())
            .collect();

        // The axis is the spoke at `theta_start`. Use the
        // projection's `unit_position` so that for chord-style
        // full-circle radars (where `theta_start` isn't a polygon
        // vertex) the endpoints sit on the actual polygon edge,
        // not on the inscribing circle outside it.
        let (ux, uy) = polar.unit_position(0.0);
        let start = Point::new(g.cx + g.r_inner * ux, g.cy - g.r_inner * uy);
        let end = Point::new(g.cx + g.r_outer * ux, g.cy - g.r_outer * uy);
        // Tick direction: perpendicular to the spoke, rotated so it
        // points OUTSIDE the swept polar region. CCW sweep → CW
        // perpendicular; CW sweep → CCW perpendicular. Screen y
        // is flipped, baked into the formula below.
        let tick_direction = radius_axis_tick_direction(polar);

        draw_linear_axis_at(scene, start, end, tick_direction, &majors, &minors, dpi);
    }

    // ── Theta grid spokes + axis ticks + labels ──
    //
    // The angular axis is the outer ring (circle / polygon). Spokes
    // from `r_inner` to `r_outer` act as radial grid lines for the
    // angular position (analogous to grid lines crossing a cartesian
    // axis at each tick). On top, each break gets a **short tick
    // mark** extending radially OUTWARD from the outer ring — the
    // same "baseline + minor ticks + major ticks + labels" idiom as
    // the cartesian axis, just oriented radially per break.
    if let Some(scale) = angle_scale {
        // Minor ticks first so majors paint on top if they coincide.
        // Mirrors the cartesian convention: short unlabelled marks
        // perpendicular to the axis baseline (here the outer ring).
        for v in scale.minor_breaks(DEFAULT_BREAK_COUNT) {
            if matches!(v, Value::Null) {
                continue;
            }
            let theta_frac = match scale.map(&v).as_number() {
                Some(f) if f.is_finite() => f,
                _ => continue,
            };
            if !(0.0..=1.0).contains(&theta_frac) {
                continue;
            }
            if is_full_circle && theta_frac >= 1.0 - 1e-9 {
                continue;
            }
            let theta = polar.theta_for_frac(theta_frac);
            let p_outer = Point::new(
                g.cx + g.r_outer * theta.cos(),
                g.cy - g.r_outer * theta.sin(),
            );
            let (rx, ry) = (theta.cos(), -theta.sin());
            let tick_end = Point::new(
                p_outer.x + minor_tick_px * rx,
                p_outer.y + minor_tick_px * ry,
            );
            scene.stroke(
                &stroke,
                Affine::IDENTITY,
                &brush,
                None,
                &segment(p_outer, tick_end),
                PickId::Skip,
            );
        }

        let breaks = scale.breaks(DEFAULT_BREAK_COUNT);
        for v in &breaks {
            if matches!(v, Value::Null) {
                continue;
            }
            let theta_frac = match scale.map(v).as_number() {
                Some(f) if f.is_finite() => f,
                _ => continue,
            };
            if !(0.0..=1.0).contains(&theta_frac) {
                continue;
            }
            // For a full-circle polar, theta_frac=0 and theta_frac=1
            // are the same physical spoke — skip the duplicate at 1.
            if is_full_circle && theta_frac >= 1.0 - 1e-9 {
                continue;
            }
            let theta = polar.theta_for_frac(theta_frac);
            let p_outer = Point::new(
                g.cx + g.r_outer * theta.cos(),
                g.cy - g.r_outer * theta.sin(),
            );

            // Radial outward unit vector in screen coordinates.
            let (rx, ry) = (theta.cos(), -theta.sin());
            // Tick mark: short radial segment outside the outer ring.
            let tick_end = Point::new(p_outer.x + tick_px * rx, p_outer.y + tick_px * ry);
            scene.stroke(
                &stroke,
                Affine::IDENTITY,
                &brush,
                None,
                &segment(p_outer, tick_end),
                PickId::Skip,
            );

            // Label beyond the tick — same quadrant-aware "near edge
            // at anchor" placement as the cartesian axis labels.
            let anchor = Point::new(
                tick_end.x + label_gap_px * rx,
                tick_end.y + label_gap_px * ry,
            );
            let text = scale.format(v);
            draw_axis_label(
                scene,
                &text,
                &style,
                &brush,
                AxisLabelAt {
                    anchor,
                    direction: (rx, ry),
                },
                dpi,
            );
        }
    }
}

// ─── Helpers ────────────────────────────────────────────────────────────────

/// Screen-space unit vector perpendicular to the `theta_start` spoke,
/// rotated so it points OUTSIDE the swept polar region (away from the
/// sweep direction). For full-circle layouts the answer is one of the
/// two cardinal sides; for partial arcs it picks the "exterior" side.
fn radius_axis_tick_direction(polar: &PolarProjection) -> (f64, f64) {
    // sign = +1 for CCW sweep (theta_end > theta_start in math), -1 for CW.
    let sign = if polar.theta_end > polar.theta_start {
        1.0
    } else {
        -1.0
    };
    // CW perpendicular for CCW sweep / CCW perpendicular for CW sweep,
    // expressed in math convention and then flipped to screen y-down.
    (
        sign * polar.theta_start.sin(),
        sign * polar.theta_start.cos(),
    )
}
