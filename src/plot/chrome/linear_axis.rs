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
use crate::plot::geom::resolve::build_stroke_for_pattern;
use crate::plot::theme::{LineElement, Palette, RectElement, ResolvedAxis};
use crate::scene::SceneBuilder;
use crate::stroke::{Cap, Join, Stroke};
use crate::text::{draw_text, Alignment, TextRun, TextStyle};

/// Build a kurbo [`Stroke`] from a themed [`LineElement`] at `dpi`,
/// honoring linewidth, linetype (dash pattern), cap, and join.
/// Width and dash lengths are pt; converted to px via the standard
/// `pt * dpi / 72` factor. Width resolves against 1.0 pt parent
/// (the `theme.line` root width default). Any `None` field falls
/// through to `line_concrete_defaults()`.
pub(crate) fn stroke_from_line_element(el: &LineElement, dpi: f64) -> Stroke {
    use crate::plot::theme::line_concrete_defaults;
    let defaults = line_concrete_defaults();
    let width_pt = el
        .linewidth_pt
        .or(defaults.linewidth_pt)
        .expect("line linewidth default")
        .resolve(1.0);
    let width_px = pt_to_px(width_pt, dpi);
    let cap = el.cap.or(defaults.cap).expect("line cap default");
    let join = el.join.or(defaults.join).expect("line join default");
    let linetype = el
        .linetype
        .clone()
        .or(defaults.linetype)
        .expect("line linetype default");
    build_stroke_for_pattern(width_px, cap, join, &linetype, 0.0, width_pt, dpi)
}

/// Build a kurbo [`Stroke`] from a themed [`RectElement`]'s border
/// fields — width + linetype. RectElement has no cap/join surface
/// (closed paths don't expose endpoints); the helper picks
/// `Cap::Butt` + `Join::Miter` (the kurbo defaults for closed
/// strokes). Width resolves against the 1.0 pt root linewidth.
pub(crate) fn stroke_from_rect_border(el: &RectElement, dpi: f64) -> Stroke {
    use crate::plot::theme::rect_concrete_defaults;
    let defaults = rect_concrete_defaults();
    let width_pt = el
        .linewidth_pt
        .or(defaults.linewidth_pt)
        .expect("rect linewidth default")
        .resolve(1.0);
    let width_px = pt_to_px(width_pt, dpi);
    let linetype = el
        .linetype
        .clone()
        .or(defaults.linetype)
        .expect("rect linetype default");
    build_stroke_for_pattern(
        width_px,
        Cap::Butt,
        Join::Miter,
        &linetype,
        0.0,
        width_pt,
        dpi,
    )
}

/// Default major tick mark length, pt. Used by the axis-measure
/// codepath that needs a size estimate before theme info is
/// available; the actual stroke length at draw time is sourced from
/// the resolved axis theme.
pub(crate) const TICK_LENGTH_PT: f64 = 4.0;
/// Default minor tick mark length, pt.
pub(crate) const MINOR_TICK_LENGTH_PT: f64 = 2.0;
/// Default gap between the tick mark end and the label's near edge, pt.
pub(crate) const LABEL_GAP_PT: f64 = 2.0;
/// Default tick label font size, pt.
pub(crate) const LABEL_FONT_SIZE_PT: f32 = 10.0;
/// Default stroke width for baseline + tick marks, pt.
pub(crate) const STROKE_WIDTH_PT: f64 = 1.0;

/// Black ink for axis chrome — used as a fallback when no theme
/// is available (legacy `axis_measure` codepaths).
pub(crate) fn axis_ink() -> Color {
    rgb(0.0, 0.0, 0.0)
}

pub(crate) fn pt_to_px(pt: f64, dpi: f64) -> f64 {
    pt * dpi / 72.0
}

/// Resolved styling for one linear-axis draw call. Carries concrete
/// colors + widths (palette already applied) so the draw routine
/// itself touches no theme types.
pub(crate) struct AxisChromeStyle {
    pub line_brush: Option<Brush>,
    pub line_stroke: Stroke,
    pub tick_brush: Option<Brush>,
    pub tick_stroke: Stroke,
    pub minor_brush: Option<Brush>,
    pub minor_stroke: Stroke,
    pub tick_length_px: f64,
    pub minor_tick_length_px: f64,
    pub gap_px: f64,
    pub text_style: TextStyle,
    pub text_brush: Brush,
    pub draw_labels: bool,
}

fn resolve_line_color(el: &LineElement, defaults: &LineElement) -> crate::plot::theme::ThemeColor {
    el.color
        .clone()
        .or_else(|| defaults.color.clone())
        .expect("line color default")
}

impl AxisChromeStyle {
    /// Construct from a `ResolvedAxis` against the theme's palette
    /// at the given dpi.
    pub fn from_resolved(resolved: &ResolvedAxis, palette: &Palette, dpi: f64) -> Self {
        use crate::plot::theme::{line_concrete_defaults, text_concrete_defaults};
        let fallback_stroke = || Stroke::new(pt_to_px(STROKE_WIDTH_PT, dpi));
        let mk_brush = |c: Color| Brush::Solid(c);
        let line_defaults = line_concrete_defaults();
        let text_defaults = text_concrete_defaults();

        let line_color =
            |el: &LineElement| -> Color { resolve_line_color(el, &line_defaults).resolve(palette) };

        let (line_brush, line_stroke) = match &resolved.line {
            Some(el) => (
                Some(mk_brush(line_color(el))),
                stroke_from_line_element(el, dpi),
            ),
            None => (None, fallback_stroke()),
        };
        let (tick_brush, tick_stroke) = match &resolved.ticks {
            Some(el) => (
                Some(mk_brush(line_color(el))),
                stroke_from_line_element(el, dpi),
            ),
            None => (None, fallback_stroke()),
        };
        let (minor_brush, minor_stroke) = match &resolved.ticks_minor {
            Some(el) => (
                Some(mk_brush(line_color(el))),
                stroke_from_line_element(el, dpi),
            ),
            None => (None, fallback_stroke()),
        };

        let (text_style, text_brush, draw_labels) = match &resolved.text {
            Some(el) => {
                let size_pt = el
                    .size_pt
                    .or(text_defaults.size_pt)
                    .expect("text size_pt default")
                    .resolve(LABEL_FONT_SIZE_PT as f64) as f32;
                let color = el
                    .color
                    .clone()
                    .or_else(|| text_defaults.color.clone())
                    .expect("text color default")
                    .resolve(palette);
                (TextStyle::new(size_pt), mk_brush(color), true)
            }
            None => (
                TextStyle::new(LABEL_FONT_SIZE_PT),
                mk_brush(axis_ink()),
                false,
            ),
        };

        Self {
            line_brush,
            line_stroke,
            tick_brush,
            tick_stroke,
            minor_brush,
            minor_stroke,
            tick_length_px: pt_to_px(resolved.tick_length.resolve(TICK_LENGTH_PT), dpi),
            minor_tick_length_px: pt_to_px(
                resolved.tick_length_minor.resolve(MINOR_TICK_LENGTH_PT),
                dpi,
            ),
            gap_px: pt_to_px(resolved.tick_gap.resolve(LABEL_GAP_PT), dpi),
            text_style,
            text_brush,
            draw_labels,
        }
    }

    /// Defaults matching the pre-theme axis chrome. Used by callers
    /// without theme access (legacy axis_measure paths).
    pub fn legacy_default(dpi: f64) -> Self {
        let stroke = Stroke::new(pt_to_px(STROKE_WIDTH_PT, dpi));
        let brush = Brush::Solid(axis_ink());
        Self {
            line_brush: Some(brush.clone()),
            line_stroke: stroke.clone(),
            tick_brush: Some(brush.clone()),
            tick_stroke: stroke.clone(),
            minor_brush: Some(brush.clone()),
            minor_stroke: stroke,
            tick_length_px: pt_to_px(TICK_LENGTH_PT, dpi),
            minor_tick_length_px: pt_to_px(MINOR_TICK_LENGTH_PT, dpi),
            gap_px: pt_to_px(LABEL_GAP_PT, dpi),
            text_style: TextStyle::new(LABEL_FONT_SIZE_PT),
            text_brush: brush,
            draw_labels: true,
        }
    }
}

/// Draw a linear axis along the segment `start` → `end`. Tick marks
/// stick out in `tick_direction` (a unit vector perpendicular to the
/// segment in screen coordinates).
///
/// Always strokes the baseline segment, even if it visually coincides
/// with a grid line drawn by the surrounding chrome — the axis line
/// is intrinsic to "this is an axis", and cartesian + polar radius
/// axes share that semantics.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_linear_axis_at(
    scene: &mut dyn SceneBuilder,
    start: Point,
    end: Point,
    tick_direction: (f64, f64),
    majors: &[(f64, String)],
    minors: &[f64],
    style: &AxisChromeStyle,
    dpi: f64,
) {
    let (tx, ty) = tick_direction;

    // Baseline.
    if let Some(brush) = &style.line_brush {
        stroke_line(scene, &style.line_stroke, brush, start, end);
    }

    // Minor ticks first so a major drawn at the same frac wins.
    if let Some(brush) = &style.minor_brush {
        for &frac in minors {
            if !frac.is_finite() || !(0.0..=1.0).contains(&frac) {
                continue;
            }
            let pos = lerp(start, end, frac);
            let tick_end = Point::new(
                pos.x + style.minor_tick_length_px * tx,
                pos.y + style.minor_tick_length_px * ty,
            );
            stroke_line(scene, &style.minor_stroke, brush, pos, tick_end);
        }
    }

    // Major ticks + labels.
    for (frac, label) in majors {
        if !frac.is_finite() || !(0.0..=1.0).contains(frac) {
            continue;
        }
        let pos = lerp(start, end, *frac);
        let tick_end = Point::new(
            pos.x + style.tick_length_px * tx,
            pos.y + style.tick_length_px * ty,
        );
        if let Some(tick_brush) = &style.tick_brush {
            stroke_line(scene, &style.tick_stroke, tick_brush, pos, tick_end);
        }

        if style.draw_labels {
            // Labels sit on the side the tick extends to, with a
            // small gap. Distinct from tick direction: if the tick
            // length is negative (extends inward), labels still go
            // outward — that's the user-visible side.
            let outward_tx = if style.tick_length_px < 0.0 { -tx } else { tx };
            let outward_ty = if style.tick_length_px < 0.0 { -ty } else { ty };
            let outward_tick_end = if style.tick_length_px < 0.0 {
                Point::new(
                    pos.x - style.tick_length_px * tx,
                    pos.y - style.tick_length_px * ty,
                )
            } else {
                tick_end
            };
            let anchor = Point::new(
                outward_tick_end.x + style.gap_px * outward_tx,
                outward_tick_end.y + style.gap_px * outward_ty,
            );
            draw_axis_label(
                scene,
                label,
                &style.text_style,
                &style.text_brush,
                AxisLabelAt {
                    anchor,
                    direction: (outward_tx, outward_ty),
                },
                dpi,
            );
        }
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
    let run = TextRun::new(text, style, dpi);
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
