//! Shared linear-axis renderer — used by the rectilinear axis (where
//! the baseline lies along a panel edge) and by the polar radius axis
//! (where the baseline is a radial spoke at some angle).
//!
//! A linear axis is fully described by:
//! - a baseline segment in pixel space (`start` → `end`),
//! - a unit vector perpendicular to the baseline indicating which side
//!   tick marks stick out into,
//! - a list of major breaks (frac along the segment, label text), and
//! - an optional list of minor breaks (frac, no labels).
//!
//! Labels sit beyond the tick mark in the tick direction, with their
//! **near edge** at `(tick_end + label_gap_px)`. The same quadrant-
//! aware alignment rules as the rectilinear axis apply, so cardinal
//! tick directions produce centred labels on the perpendicular axis
//! and diagonal directions produce corner alignment.

use crate::brush::Brush;
use crate::color::{rgb, Color};
use crate::geometry::{Affine, Point};
use crate::layout::{Measure, WidthHint};
use crate::path::Path;
use crate::pick::PickId;
use crate::scene::SceneBuilder;
use crate::stroke::Stroke;
use crate::text::{draw_text, Alignment, TextRun, TextStyle};

/// Major tick mark length, pt.
pub(crate) const TICK_LENGTH_PT: f64 = 4.0;
/// Minor tick mark length, pt (shorter than majors, unlabelled).
pub(crate) const MINOR_TICK_LENGTH_PT: f64 = 2.0;
/// Gap between the tick mark end and the label's near edge, pt.
pub(crate) const LABEL_GAP_PT: f64 = 2.0;
/// Tick label font size, pt.
pub(crate) const LABEL_FONT_SIZE_PT: f32 = 10.0;
/// Stroke width for baseline + tick marks, pt.
pub(crate) const STROKE_WIDTH_PT: f64 = 1.0;

/// Black ink for axis chrome.
pub(crate) fn axis_ink() -> Color {
    rgb(0.0, 0.0, 0.0)
}

pub(crate) fn pt_to_px(pt: f64, dpi: f64) -> f64 {
    pt * dpi / 72.0
}

/// Draw a linear axis along the segment `start` → `end`. Tick marks
/// stick out in `tick_direction` (a unit vector perpendicular to the
/// segment in screen coordinates).
///
/// Always strokes the baseline segment, even if it visually coincides
/// with a grid line drawn by the surrounding chrome — the axis line
/// is intrinsic to "this is an axis", and cartesian + polar radius
/// axes share that semantics.
pub(crate) fn draw_linear_axis_at(
    scene: &mut dyn SceneBuilder,
    start: Point,
    end: Point,
    tick_direction: (f64, f64),
    majors: &[(f64, String)],
    minors: &[f64],
    dpi: f64,
) {
    let tick_px = pt_to_px(TICK_LENGTH_PT, dpi);
    let minor_tick_px = pt_to_px(MINOR_TICK_LENGTH_PT, dpi);
    let gap_px = pt_to_px(LABEL_GAP_PT, dpi);
    let stroke_px = pt_to_px(STROKE_WIDTH_PT, dpi);
    let brush = Brush::Solid(axis_ink());
    let stroke = Stroke::new(stroke_px);
    let text_style = TextStyle::new(LABEL_FONT_SIZE_PT);

    let (tx, ty) = tick_direction;

    stroke_line(scene, &stroke, &brush, start, end);

    // Minor ticks first so a major drawn at the same frac wins.
    for &frac in minors {
        if !frac.is_finite() || !(0.0..=1.0).contains(&frac) {
            continue;
        }
        let pos = lerp(start, end, frac);
        let tick_end = Point::new(pos.x + minor_tick_px * tx, pos.y + minor_tick_px * ty);
        stroke_line(scene, &stroke, &brush, pos, tick_end);
    }

    for (frac, label) in majors {
        if !frac.is_finite() || !(0.0..=1.0).contains(frac) {
            continue;
        }
        let pos = lerp(start, end, *frac);
        let tick_end = Point::new(pos.x + tick_px * tx, pos.y + tick_px * ty);
        stroke_line(scene, &stroke, &brush, pos, tick_end);

        let anchor = Point::new(tick_end.x + gap_px * tx, tick_end.y + gap_px * ty);
        draw_axis_label(
            scene,
            label,
            &text_style,
            &brush,
            AxisLabelAt {
                anchor,
                direction: (tx, ty),
            },
            dpi,
        );
    }
}

/// Anchor + direction for [`draw_axis_label`]. `anchor` is where the
/// label's **near edge** should sit; `direction` is the unit vector
/// (screen space) pointing away from the axis line — the side of the
/// anchor the label extends into.
pub(crate) struct AxisLabelAt {
    pub anchor: Point,
    pub direction: (f64, f64),
}

/// Draw a label whose **near edge** sits at `at.anchor`, with the
/// label extending in `at.direction`. Quadrant-aware: cardinal
/// directions centre the label on the perpendicular axis; diagonal
/// directions anchor at a corner.
///
/// Used both by [`draw_linear_axis_at`] (after computing the per-tick
/// anchor / direction internally) and by the polar chrome's
/// angular-axis ticks, which need to place labels at a different
/// direction for each break.
pub(crate) fn draw_axis_label(
    scene: &mut dyn SceneBuilder,
    text: &str,
    style: &TextStyle,
    brush: &Brush,
    at: AxisLabelAt,
    dpi: f64,
) {
    let run = TextRun::new(text, style);
    let label_h = run.set_max_width(f32::INFINITY, Alignment::Start) as f64;
    let label_w = match run.width_hint(dpi) {
        WidthHint::Min(w) => w,
        WidthHint::NeedsHeight { seed } => seed,
    };

    // Dead-band around the cardinals so near-vertical / near-horizontal
    // directions don't jitter their alignment quadrant.
    const CARDINAL_EPS: f64 = 0.05;
    let (tx, ty) = at.direction;
    let dir_x = if tx > CARDINAL_EPS {
        1.0
    } else if tx < -CARDINAL_EPS {
        -1.0
    } else {
        0.0
    };
    let dir_y = if ty > CARDINAL_EPS {
        1.0
    } else if ty < -CARDINAL_EPS {
        -1.0
    } else {
        0.0
    };

    let label_cx = at.anchor.x + dir_x * label_w * 0.5;
    let label_cy = at.anchor.y + dir_y * label_h * 0.5;

    let x = label_cx - label_w * 0.5;
    let y = label_cy - label_h * 0.5;
    draw_text(scene, &run, x, y, brush, Affine::IDENTITY, PickId::Skip);
}

fn lerp(a: Point, b: Point, t: f64) -> Point {
    Point::new(a.x + t * (b.x - a.x), a.y + t * (b.y - a.y))
}

fn line_path(p0: Point, p1: Point) -> Path {
    let mut p = Path::new();
    p.move_to(p0);
    p.line_to(p1);
    p
}

fn stroke_line(scene: &mut dyn SceneBuilder, stroke: &Stroke, brush: &Brush, p0: Point, p1: Point) {
    let path = line_path(p0, p1);
    scene.stroke(stroke, Affine::IDENTITY, brush, None, &path, PickId::Skip);
}
