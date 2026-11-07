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
use crate::path::Path;
use crate::pick::PickId;
use crate::plot::chrome::text::{ChromeRun, RichChrome};
use crate::plot::geom::resolve::build_stroke_for_pattern;
use crate::plot::pick::{item_scope, part_scope, PlotPart};
use crate::plot::theme::{LineElement, RectElement, ResolvedAxis, Theme};
use crate::scene::SceneBuilder;
use crate::stroke::{Cap, Join, Stroke};
use crate::text::TextStyle;

/// Build a kurbo [`Stroke`] from a themed [`LineElement`] at `dpi`,
/// honoring linewidth, linetype (dash pattern), cap, and join.
/// Width is pt, converted to px via the standard `pt * dpi / 72`
/// factor; dash lengths are multiples of that width (see
/// `crate::linetype`). Width resolves against 1.0 pt parent
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
    build_stroke_for_pattern(width_px, cap, join, &linetype, 0.0, dpi)
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
    build_stroke_for_pattern(width_px, Cap::Butt, Join::Miter, &linetype, 0.0, dpi)
}

// Default measurements are the same constants the theme's concrete
// defaults builders use — so the `Length::Rel(_)` resolution parent
// at chrome time matches what `axis_concrete_defaults` / element
// defaults built into the theme.
pub(crate) use crate::plot::theme::{
    DEFAULT_LINEWIDTH_PT as STROKE_WIDTH_PT, DEFAULT_MINOR_TICK_LENGTH_PT as MINOR_TICK_LENGTH_PT,
    DEFAULT_TICK_GAP_PT as LABEL_GAP_PT, DEFAULT_TICK_LENGTH_PT as TICK_LENGTH_PT,
    DEFAULT_TITLE_GAP_PT as TITLE_GAP_PT,
};
/// Ink for an axis whose text element resolves to `Blank`. Labels
/// aren't drawn in that case, so the brush only has to be well-formed.
fn axis_ink() -> Color {
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
    /// Gap between the outer edge of the tick-label rail and the
    /// near edge of the axis title, already converted to px.
    pub title_gap_px: f64,
    pub text_style: TextStyle,
    pub text_brush: Brush,
    /// Outline pass for tick labels. `None` draws labels fill-only.
    pub text_outline: Option<crate::plot::chrome::text::TextOutline>,
    /// Markdown context for tick labels. `Some` when the axis's text
    /// element opts in, in which case labels shape through the rich
    /// pipeline and `text_outline` is carried on the sheet instead.
    pub rich: Option<RichChrome>,
    pub draw_labels: bool,
}

fn resolve_line_color(el: &LineElement, defaults: &LineElement) -> crate::plot::theme::ThemeColor {
    el.color
        .clone()
        .or_else(|| defaults.color.clone())
        .expect("line color default")
}

impl AxisChromeStyle {
    /// Construct from a `ResolvedAxis` against the theme's palette at
    /// the given dpi. `root_pt` is the parent size relative text sizes
    /// resolve against — see [`crate::plot::chrome::root_text_pt`].
    /// `images` is what a markdown break label resolves image tags
    /// against.
    pub fn from_resolved(
        resolved: &ResolvedAxis,
        theme: &Theme,
        dpi: f64,
        root_pt: f64,
        images: &std::sync::Arc<crate::image_registry::ImageRegistry>,
    ) -> Self {
        use crate::plot::theme::{line_concrete_defaults, text_concrete_defaults};
        let palette = &theme.palette;
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

        let (text_style, text_brush, text_outline, rich, draw_labels) = match &resolved.text {
            Some(el) => {
                let color = el
                    .color
                    .clone()
                    .or_else(|| text_defaults.color.clone())
                    .expect("text color default")
                    .resolve(palette);
                (
                    // The whole element, not just its size: family,
                    // weight, width, style, features, variations, line
                    // height, letter spacing and decorations are all
                    // themeable on tick labels.
                    crate::plot::chrome::text::text_style_from(el, root_pt),
                    mk_brush(color),
                    crate::plot::chrome::text::text_outline_from(el, palette, dpi),
                    crate::plot::chrome::text::rich_chrome_for(el, theme, dpi, images),
                    true,
                )
            }
            None => (
                TextStyle::new(root_pt as f32),
                mk_brush(axis_ink()),
                None,
                None,
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
            title_gap_px: pt_to_px(resolved.title_gap.resolve(TITLE_GAP_PT), dpi),
            text_style,
            text_brush,
            text_outline,
            rich,
            draw_labels,
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
///
/// `minors` carries `(break_index, frac)` per minor tick, indexed against
/// the scale's minor-break list the same way [`AxisTick::break_index`] is.
///
/// Pushes a pick scope per piece — line, tick, label — so a hit names which
/// part of the rail it landed on. Every caller gets that: the cartesian axis,
/// the polar radius axis, and both colorbar rails all come through here. The
/// parts are the `Axis*` ones whatever the caller, because it is literally
/// the same rail; the frame *enclosing* this call is what says whether it
/// belongs to an axis or a legend.
/// One major tick on an axis rail.
///
/// `break_index` addresses the scale's own break list, **not** the position
/// of this tick within the drawn set. Nulls and breaks the scale cannot map
/// are dropped on the way here, and an open-ended colorbar trims its first
/// or last tick, so the two differ whenever a scale emits a break that does
/// not survive. Carrying the true index is what lets a consumer recover the
/// domain value as `scale.breaks(..)[break_index]`.
pub(crate) struct AxisTick {
    pub break_index: usize,
    /// Position along the rail, `0..=1`.
    pub frac: f64,
    pub label: String,
}

/// The major ticks a scale contributes to a rail: every break it can map to
/// a finite position, paired with its formatted label.
///
/// The enumeration runs **before** the filters, so a break the scale drops —
/// a [`Value::Null`], or one it cannot map — leaves a gap in the reported
/// indices rather than renumbering everything after it.
pub(crate) fn axis_ticks(
    breaks: &[crate::scales::value::Value],
    scale: &crate::plot::scale::Scale,
) -> Vec<AxisTick> {
    breaks
        .iter()
        .enumerate()
        .filter(|(_, v)| !matches!(v, crate::scales::value::Value::Null))
        .filter_map(|(break_index, v)| {
            scale.map_break(v).as_number().map(|frac| AxisTick {
                break_index,
                frac,
                label: scale.format(v),
            })
        })
        .filter(|t| t.frac.is_finite())
        .collect()
}

/// The minor ticks a scale contributes, as `(break_index, frac)`. Indexed
/// against the scale's minor-break list exactly as [`axis_ticks`] is against
/// its major one.
pub(crate) fn axis_minor_ticks(scale: &crate::plot::scale::Scale, n: usize) -> Vec<(usize, f64)> {
    scale
        .minor_breaks(n)
        .into_iter()
        .enumerate()
        .filter(|(_, v)| !matches!(v, crate::scales::value::Value::Null))
        .filter_map(|(i, v)| scale.map_break(&v).as_number().map(|f| (i, f)))
        .filter(|(_, f)| f.is_finite())
        .collect()
}

/// Clearance kept between two surviving labels, px. Added to each
/// label box on all sides, so neighbours land at least
/// `2 * LABEL_PAD_PX` apart.
const LABEL_PAD_PX: f64 = 1.0;

/// One label's ink footprint in screen space, as an oriented box.
/// All labels on a rail share one rotation, so the boxes are directly
/// comparable and a separating-axis test on the box's own two axes is
/// exact — including under rotation, where an axis-aligned bounding
/// box would report false overlaps.
struct LabelBox {
    cx: f64,
    cy: f64,
    hw: f64,
    hh: f64,
    cos: f64,
    sin: f64,
}

impl LabelBox {
    /// True when the two boxes overlap. Assumes both carry the same
    /// rotation — the separating axes are `self`'s.
    fn overlaps(&self, other: &Self) -> bool {
        let dx = other.cx - self.cx;
        let dy = other.cy - self.cy;
        let along = (dx * self.cos + dy * self.sin).abs() <= self.hw + other.hw;
        let across = (dy * self.cos - dx * self.sin).abs() <= self.hh + other.hh;
        along && across
    }
}

/// Coarsest roughly-even subset of `boxes` that fits without overlap,
/// as a keep mask. Terminal labels always survive — they carry the
/// range — even when they overlap each other. Interior labels are
/// admitted by recursive bisection: try the label nearest the spatial
/// midpoint of each gap between kept labels, subdivide the gap only
/// when it lands, and drop the whole gap when it doesn't (squeezing a
/// label in off-centre would crowd one side). `fracs` are the labels'
/// positions along the rail, non-decreasing.
fn bisect_keep(boxes: &[LabelBox], fracs: &[f64]) -> Vec<bool> {
    let n = boxes.len();
    let mut keep = vec![false; n];
    if n == 0 {
        return keep;
    }
    keep[0] = true;
    if n == 1 {
        return keep;
    }
    keep[n - 1] = true;
    let mut stack = vec![(0usize, n - 1)];
    while let Some((lo, hi)) = stack.pop() {
        if hi - lo < 2 {
            continue;
        }
        let mid = (fracs[lo] + fracs[hi]) * 0.5;
        let m = (lo + 1..hi)
            .min_by(|&a, &b| (fracs[a] - mid).abs().total_cmp(&(fracs[b] - mid).abs()))
            .expect("gap of at least two holds an interior index");
        let fits = (0..n)
            .filter(|&i| keep[i])
            .all(|i| !boxes[m].overlaps(&boxes[i]));
        if fits {
            keep[m] = true;
            stack.push((lo, m));
            stack.push((m, hi));
        }
    }
    keep
}

/// Where the label at `pos` sits: the anchor its near edge aligns to,
/// and the outward direction it extends into. Distinct from the tick
/// direction when the tick length is negative (the tick grows inward)
/// — labels still go outward, the user-visible side.
fn label_anchor(
    pos: Point,
    tick_direction: (f64, f64),
    style: &AxisChromeStyle,
) -> (Point, (f64, f64)) {
    let (tx, ty) = tick_direction;
    let (outward_tx, outward_ty) = if style.tick_length_px < 0.0 {
        (-tx, -ty)
    } else {
        (tx, ty)
    };
    let outward_end = if style.tick_length_px < 0.0 {
        Point::new(
            pos.x - style.tick_length_px * tx,
            pos.y - style.tick_length_px * ty,
        )
    } else {
        Point::new(
            pos.x + style.tick_length_px * tx,
            pos.y + style.tick_length_px * ty,
        )
    };
    (
        Point::new(
            outward_end.x + style.gap_px * outward_tx,
            outward_end.y + style.gap_px * outward_ty,
        ),
        (outward_tx, outward_ty),
    )
}

/// Snap a label direction to its alignment quadrant. The dead-band
/// around the cardinals keeps near-vertical / near-horizontal
/// directions from jittering their alignment.
fn snapped_label_dir(direction: (f64, f64)) -> (f64, f64) {
    const CARDINAL_EPS: f64 = 0.05;
    let (tx, ty) = direction;
    let snap = |t: f64| {
        if t > CARDINAL_EPS {
            1.0
        } else if t < -CARDINAL_EPS {
            -1.0
        } else {
            0.0
        }
    };
    (snap(tx), snap(ty))
}

/// Ink box `tick`'s label would occupy, or `None` for a tick outside
/// the rail — it draws nothing and is no obstacle. Anchoring mirrors
/// [`draw_axis_label`]: the box is the shaped run's cap-height band
/// centred the way the draw pass centres it.
fn label_box(
    tick: &AxisTick,
    start: Point,
    end: Point,
    tick_direction: (f64, f64),
    style: &AxisChromeStyle,
    dpi: f64,
) -> Option<LabelBox> {
    if !tick.frac.is_finite() || !(0.0..=1.0).contains(&tick.frac) {
        return None;
    }
    let pos = lerp(start, end, tick.frac);
    let (anchor, direction) = label_anchor(pos, tick_direction, style);
    let run = ChromeRun::shape(&tick.label, &style.text_style, dpi, style.rich.as_ref());
    let w = run.width();
    let cap_h = run.cap_height();
    let (dir_x, dir_y) = snapped_label_dir(direction);
    Some(LabelBox {
        cx: anchor.x + dir_x * w * 0.5,
        cy: anchor.y + dir_y * cap_h * 0.5,
        hw: w * 0.5 + LABEL_PAD_PX,
        hh: cap_h * 0.5 + LABEL_PAD_PX,
        cos: 1.0,
        sin: 0.0,
    })
}

/// Keep mask over `majors`' labels. Ticks outside the rail draw no
/// label and stay `false`; the rest pass through [`bisect_keep`].
fn cull_overlapping_labels(
    majors: &[AxisTick],
    start: Point,
    end: Point,
    tick_direction: (f64, f64),
    style: &AxisChromeStyle,
    dpi: f64,
) -> Vec<bool> {
    let mut idx = Vec::new();
    let mut boxes = Vec::new();
    let mut fracs = Vec::new();
    for (i, tick) in majors.iter().enumerate() {
        if let Some(b) = label_box(tick, start, end, tick_direction, style, dpi) {
            idx.push(i);
            boxes.push(b);
            fracs.push(tick.frac);
        }
    }
    let kept = bisect_keep(&boxes, &fracs);
    let mut keep = vec![false; majors.len()];
    for (k, &i) in idx.iter().enumerate() {
        keep[i] = kept[k];
    }
    keep
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_linear_axis_at(
    scene: &mut dyn SceneBuilder,
    start: Point,
    end: Point,
    tick_direction: (f64, f64),
    majors: &[AxisTick],
    minors: &[(usize, f64)],
    style: &AxisChromeStyle,
    dpi: f64,
) {
    let (tx, ty) = tick_direction;

    // Baseline.
    if let Some(brush) = &style.line_brush {
        scene.push_pick_scope(&part_scope(PlotPart::AxisLine));
        stroke_line(scene, &style.line_stroke, brush, start, end);
        scene.pop_pick_scope();
    }

    // Minor ticks first so a major drawn at the same frac wins.
    if let Some(brush) = &style.minor_brush {
        // The part frame is hoisted out of the loop: every minor tick is the
        // same part, and one group per tick would nest the SVG output two
        // deep for nothing.
        scene.push_pick_scope(&part_scope(PlotPart::AxisMinorTick));
        for &(break_index, frac) in minors {
            if !frac.is_finite() || !(0.0..=1.0).contains(&frac) {
                continue;
            }
            let pos = lerp(start, end, frac);
            let tick_end = Point::new(
                pos.x + style.minor_tick_length_px * tx,
                pos.y + style.minor_tick_length_px * ty,
            );
            scene.push_pick_scope(&item_scope(break_index as u32));
            stroke_line(scene, &style.minor_stroke, brush, pos, tick_end);
            scene.pop_pick_scope();
        }
        scene.pop_pick_scope();
    }

    // Tick marks draw for every break; labels draw only where they
    // fit — culling drops the ones that would overlap a survivor.
    let label_keep = if style.draw_labels {
        cull_overlapping_labels(majors, start, end, tick_direction, style, dpi)
    } else {
        Vec::new()
    };

    // Major ticks + labels.
    for (i, tick) in majors.iter().enumerate() {
        let (frac, label) = (tick.frac, &tick.label);
        if !frac.is_finite() || !(0.0..=1.0).contains(&frac) {
            continue;
        }
        let pos = lerp(start, end, frac);
        let tick_end = Point::new(
            pos.x + style.tick_length_px * tx,
            pos.y + style.tick_length_px * ty,
        );
        let item = item_scope(tick.break_index as u32);
        if let Some(tick_brush) = &style.tick_brush {
            scene.push_pick_scope(&part_scope(PlotPart::AxisTick));
            scene.push_pick_scope(&item);
            stroke_line(scene, &style.tick_stroke, tick_brush, pos, tick_end);
            scene.pop_pick_scope();
            scene.pop_pick_scope();
        }

        if style.draw_labels && label_keep.get(i).copied().unwrap_or(false) {
            // Labels sit on the side the tick extends to, with a
            // small gap.
            let (anchor, direction) = label_anchor(pos, tick_direction, style);
            scene.push_pick_scope(&part_scope(PlotPart::AxisTickLabel));
            scene.push_pick_scope(&item);
            draw_axis_label(
                scene,
                label,
                &style.text_style,
                &style.text_brush,
                style.text_outline.as_ref(),
                style.rich.as_ref(),
                AxisLabelAt { anchor, direction },
                dpi,
            );
            scene.pop_pick_scope();
            scene.pop_pick_scope();
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
///
/// `outline`, when present, is emitted as a stroke-only pass behind
/// the fill.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_axis_label(
    scene: &mut dyn SceneBuilder,
    text: &str,
    style: &TextStyle,
    brush: &Brush,
    outline: Option<&crate::plot::chrome::text::TextOutline>,
    // `Some` reads the label as markdown. The sheet carries the
    // outline in that case, so `outline` applies to the plain path
    // only — see [`crate::plot::chrome::text::ChromeRun::draw`].
    rich: Option<&RichChrome>,
    at: AxisLabelAt,
    dpi: f64,
) {
    let run = ChromeRun::shape(text, style, dpi, rich);
    // Tick labels draw on one line, so the anchoring width is the
    // natural laid-out width. `width_hint` reports the longest
    // unbreakable cluster instead — a wrap lower bound that
    // undershoots any label carrying a space and slides it off its
    // tick.
    let label_w = run.width();
    // Use the cap-height band as the "visible height" for vertical
    // positioning — numeric and uppercase labels (the common case
    // for ticks + discrete key labels) then centre on their ink
    // rather than on the full font line-height (which reserves
    // descender space the glyphs don't occupy and shifts the
    // visual centre too low). `baseline - cap_h` is the distance
    // from the layout's top edge to the cap-top; the baseline
    // offset already absorbs any half-leading.
    let baseline = run.baseline_offset();
    let cap_h = run.cap_height();
    let cap_top_offset = baseline - cap_h;

    let (dir_x, dir_y) = snapped_label_dir(at.direction);

    // Anchor on the cap-band centre / edges:
    //   dir_y =  0 → cap centre lands on anchor.y
    //   dir_y >  0 → cap top    lands on anchor.y (label extends down)
    //   dir_y <  0 → baseline   lands on anchor.y (label extends up)
    let label_cx = at.anchor.x + dir_x * label_w * 0.5;
    let label_cy = at.anchor.y + dir_y * cap_h * 0.5;

    let x = label_cx - label_w * 0.5;
    let y = label_cy - cap_h * 0.5 - cap_top_offset;
    run.draw(scene, x, y, brush, outline, Affine::IDENTITY, PickId::Skip);
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

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::rgb;
    use crate::scene::recording::{Op, RecordingScene};

    #[test]
    fn a_dropped_break_leaves_a_gap_rather_than_shifting_later_ticks() {
        use crate::plot::scale;
        use crate::scales::value::Value;

        // Breaks 1 and 3 are unmappable on a continuous scale, so the rail
        // draws three ticks out of five.
        let sc = scale::continuous(0.0..=1.0).with_breaks(vec![
            Value::Number(0.0),
            Value::String("nope".into()),
            Value::Number(0.5),
            Value::Null,
            Value::Number(1.0),
        ]);
        let breaks = sc.breaks(5);
        let ticks = axis_ticks(&breaks, &sc);

        assert_eq!(ticks.len(), 3);
        // The survivors report where they sit in `breaks`, not where they
        // sit in the drawn set. Numbering the survivors would give 0, 1, 2.
        assert_eq!(
            ticks.iter().map(|t| t.break_index).collect::<Vec<_>>(),
            vec![0, 2, 4]
        );
        // And each index really does address the break it came from.
        for t in &ticks {
            assert_eq!(sc.format(&breaks[t.break_index]), t.label);
        }
    }

    const DPI: f64 = 96.0;
    /// A label with a break opportunity next to a same-shape control
    /// that has none — the pair isolates wrap-width effects from
    /// ordinary label width.
    const SPACED: &str = "Species: setosa";
    const UNSPACED: &str = "SpeciesZsetosa";

    fn style() -> TextStyle {
        TextStyle::new(9.0)
    }

    /// Laid-out width of `text` at the test style — the width the draw
    /// pass puts on screen.
    fn drawn_width(text: &str) -> f64 {
        ChromeRun::shape(text, &style(), DPI, None).width()
    }

    /// Leftmost glyph pen position across every emitted run, which is
    /// where the label's near edge landed.
    fn left_edge(scene: &RecordingScene) -> f64 {
        let mut min_x = f64::INFINITY;
        for op in &scene.ops {
            if let Op::DrawGlyphs(run) = op {
                for g in &run.glyphs {
                    min_x = min_x.min(g.x as f64);
                }
            }
        }
        assert!(min_x.is_finite(), "no glyphs emitted");
        min_x
    }

    fn draw(text: &str, direction: (f64, f64), anchor: Point) -> RecordingScene {
        let mut scene = RecordingScene::default();
        draw_axis_label(
            &mut scene,
            text,
            &style(),
            &Brush::Solid(rgb(0.0, 0.0, 0.0)),
            None,
            None,
            AxisLabelAt { anchor, direction },
            DPI,
        );
        scene
    }

    /// The markdown context a themed axis with `markdown` on resolves
    /// to, at the same style the plain helpers use.
    fn markdown_ctx() -> crate::plot::chrome::text::RichChrome {
        let theme = crate::plot::theme::Theme::default();
        let el = crate::plot::theme::TextElement {
            markdown: Some(true),
            ..Default::default()
        };
        crate::plot::chrome::text::rich_chrome_for(
            &el,
            &theme,
            DPI,
            &crate::image_registry::no_images(),
        )
        .expect("markdown is on")
    }

    fn draw_markdown(text: &str, direction: (f64, f64), anchor: Point) -> RecordingScene {
        let ctx = markdown_ctx();
        let mut scene = RecordingScene::default();
        draw_axis_label(
            &mut scene,
            text,
            &style(),
            &Brush::Solid(rgb(0.0, 0.0, 0.0)),
            None,
            Some(&ctx),
            AxisLabelAt { anchor, direction },
            DPI,
        );
        scene
    }

    fn glyph_count(scene: &RecordingScene) -> usize {
        scene
            .ops
            .iter()
            .filter_map(|op| match op {
                Op::DrawGlyphs(run) => Some(run.glyphs.len()),
                _ => None,
            })
            .sum()
    }

    #[test]
    fn a_markdown_break_label_reads_its_markers_as_syntax() {
        let anchor = Point::new(200.0, 100.0);
        let plain = glyph_count(&draw("**hi**", (0.0, 1.0), anchor));
        let md = glyph_count(&draw_markdown("**hi**", (0.0, 1.0), anchor));
        assert_eq!(plain, 6, "the plain path draws the asterisks");
        assert_eq!(md, 2, "the markdown path emboldens `hi` instead");
    }

    /// The markdown path anchors on the same cap band as the plain
    /// one, so a label doesn't jump when a theme turns markdown on.
    #[test]
    fn a_markdown_break_label_sits_where_the_plain_one_does() {
        let anchor = Point::new(200.0, 100.0);
        let plain = &draw("hi", (0.0, 1.0), anchor);
        let md = &draw_markdown("hi", (0.0, 1.0), anchor);
        let (px, py) = first_glyph(plain);
        let (mx, my) = first_glyph(md);
        assert!(
            (px - mx).abs() < 0.5 && (py - my).abs() < 0.5,
            "plain at ({px}, {py}), markdown at ({mx}, {my})"
        );
    }

    /// Screen position of the first glyph emitted. The rich path
    /// carries placement on the run's transform rather than in the
    /// glyph offsets, so both have to be composed to compare the two.
    fn first_glyph(scene: &RecordingScene) -> (f64, f64) {
        for op in &scene.ops {
            if let Op::DrawGlyphs(run) = op {
                if let Some(g) = run.glyphs.first() {
                    let p = run.transform * Point::new(g.x as f64, g.y as f64);
                    return (p.x, p.y);
                }
            }
        }
        panic!("no glyphs emitted");
    }

    #[test]
    fn bottom_axis_label_centres_on_its_tick() {
        let anchor = Point::new(200.0, 100.0);
        for text in [SPACED, UNSPACED] {
            let scene = draw(text, (0.0, 1.0), anchor);
            let centre = left_edge(&scene) + drawn_width(text) * 0.5;
            assert!(
                (centre - anchor.x).abs() < 0.5,
                "{text:?} centred at {centre}, tick at {}",
                anchor.x
            );
        }
    }

    #[test]
    fn left_axis_label_ends_at_its_tick() {
        let anchor = Point::new(200.0, 100.0);
        for text in [SPACED, UNSPACED] {
            let scene = draw(text, (-1.0, 0.0), anchor);
            let right = left_edge(&scene) + drawn_width(text);
            assert!(
                (right - anchor.x).abs() < 0.5,
                "{text:?} ends at {right}, tick at {}",
                anchor.x
            );
        }
    }

    // ── overlap culling ──

    fn axis_box(cx: f64, cy: f64, hw: f64, hh: f64) -> LabelBox {
        LabelBox {
            cx,
            cy,
            hw,
            hh,
            cos: 1.0,
            sin: 0.0,
        }
    }

    fn rotated_box(cx: f64, cy: f64, hw: f64, hh: f64, deg: f64) -> LabelBox {
        let rad = deg.to_radians();
        LabelBox {
            cx,
            cy,
            hw,
            hh,
            cos: rad.cos(),
            sin: rad.sin(),
        }
    }

    #[test]
    fn axis_aligned_boxes_overlap_only_when_they_touch() {
        let a = axis_box(0.0, 0.0, 1.0, 0.5);
        assert!(a.overlaps(&axis_box(1.5, 0.0, 1.0, 0.5)));
        assert!(!a.overlaps(&axis_box(3.0, 0.0, 1.0, 0.5)));
        assert!(!a.overlaps(&axis_box(0.0, 2.0, 5.0, 0.5)));
    }

    #[test]
    fn rotated_boxes_are_not_judged_by_their_bounding_boxes() {
        // Long thin labels at 45°: the axis-aligned bounding boxes
        // overlap well before the labels do, and keep overlapping
        // after the labels have separated along the text axis.
        let a = rotated_box(0.0, 0.0, 4.0, 0.5, 45.0);
        let near = rotated_box(2.2, 2.2, 4.0, 0.5, 45.0);
        let far = rotated_box(6.0, 6.0, 4.0, 0.5, 45.0);
        assert!(a.overlaps(&near), "centres 3.1 apart along the text axis");
        assert!(
            !a.overlaps(&far),
            "centres 8.5 apart along the text axis — the AABB still overlaps"
        );
    }

    #[test]
    fn bisection_keeps_an_even_subset() {
        // Five labels, each wide enough to overlap its neighbour but
        // not its next-neighbour: the even subset survives.
        let boxes: Vec<_> = (0..5)
            .map(|i| axis_box(i as f64 * 10.0, 0.0, 6.0, 1.0))
            .collect();
        let fracs: Vec<_> = (0..5).map(|i| i as f64 / 4.0).collect();
        let keep = bisect_keep(&boxes, &fracs);
        assert_eq!(keep, vec![true, false, true, false, true]);
    }

    #[test]
    fn bisection_keeps_terminal_labels_even_when_they_overlap() {
        let boxes = vec![axis_box(0.0, 0.0, 10.0, 1.0), axis_box(5.0, 0.0, 10.0, 1.0)];
        let fracs = vec![0.0, 1.0];
        assert_eq!(bisect_keep(&boxes, &fracs), vec![true, true]);
    }

    #[test]
    fn bisection_prefers_the_spatial_midpoint_over_the_index_midpoint() {
        // Breaks bunched at the left end: the first interior try is
        // the break nearest the gap's spatial midpoint (frac 0.5),
        // not the middle index.
        let fracs = vec![0.0, 0.1, 0.2, 0.5, 1.0];
        let boxes: Vec<_> = fracs
            .iter()
            .map(|f| axis_box(f * 100.0, 0.0, 1.0, 1.0))
            .collect();
        let keep = bisect_keep(&boxes, &fracs);
        assert!(
            keep[3],
            "the label at frac 0.5 is the gap's spatial midpoint"
        );
        assert!(keep.iter().all(|&k| k), "nothing overlaps, so all fit");
    }

    #[test]
    fn bisection_drops_a_gap_whose_midpoint_fails() {
        // The midpoint of (0, 1) overlaps the terminal at 0, so the
        // whole gap is dropped even though no other label exists.
        let boxes = vec![
            axis_box(0.0, 0.0, 10.0, 1.0),
            axis_box(9.0, 0.0, 1.0, 1.0),
            axis_box(100.0, 0.0, 1.0, 1.0),
        ];
        let fracs = vec![0.0, 0.09, 1.0];
        assert_eq!(bisect_keep(&boxes, &fracs), vec![true, false, true]);
    }

    /// An axis style resolved from the default theme, the way a
    /// Bottom x-axis sees it.
    fn chrome_style() -> (crate::plot::theme::Theme, AxisChromeStyle) {
        let theme = crate::plot::theme::Theme::default();
        let resolved = theme.resolved_axis(0, 0);
        let style = AxisChromeStyle::from_resolved(
            &resolved,
            &theme,
            DPI,
            crate::plot::chrome::root_text_pt(&theme),
            &crate::image_registry::no_images(),
        );
        (theme, style)
    }

    fn glyph_run_count(scene: &RecordingScene) -> usize {
        scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::DrawGlyphs(_)))
            .count()
    }

    #[test]
    fn crowded_labels_are_culled_but_their_ticks_still_draw() {
        let (_theme, style) = chrome_style();
        let long: String = "label-".repeat(4);
        let majors: Vec<AxisTick> = (0..8)
            .map(|i| AxisTick {
                break_index: i,
                frac: i as f64 / 7.0,
                label: format!("{long}{i}"),
            })
            .collect();
        let mut scene = RecordingScene::default();
        let start = Point::new(0.0, 0.0);
        let end = Point::new(400.0, 0.0);
        draw_linear_axis_at(
            &mut scene,
            start,
            end,
            (0.0, 1.0),
            &majors,
            &[],
            &style,
            DPI,
        );
        let runs = glyph_run_count(&scene);
        assert!(
            runs < majors.len(),
            "8 labels of ~150px on a 400px rail cannot all fit ({runs} drawn)"
        );
        assert!(runs >= 2, "at least the terminal labels survive");

        // Every break keeps its tick mark even when its label drops.
        let ticks = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Stroke { .. }))
            .count();
        assert_eq!(ticks, majors.len());

        // The terminal labels are among the survivors: some glyph
        // lands in the leftmost and rightmost label footprints.
        let (mut min_x, mut max_x) = (f64::INFINITY, f64::NEG_INFINITY);
        for op in &scene.ops {
            if let Op::DrawGlyphs(run) = op {
                for g in &run.glyphs {
                    min_x = min_x.min(g.x as f64);
                    max_x = max_x.max(g.x as f64);
                }
            }
        }
        assert!(
            min_x < 10.0,
            "first label drawn near the rail start ({min_x})"
        );
        assert!(
            max_x > 390.0,
            "last label drawn near the rail end ({max_x})"
        );
    }

    #[test]
    fn spaced_labels_all_survive() {
        let (_theme, style) = chrome_style();
        let majors: Vec<AxisTick> = (0..4)
            .map(|i| AxisTick {
                break_index: i,
                frac: i as f64 / 3.0,
                label: i.to_string(),
            })
            .collect();
        let mut scene = RecordingScene::default();
        draw_linear_axis_at(
            &mut scene,
            Point::new(0.0, 0.0),
            Point::new(400.0, 0.0),
            (0.0, 1.0),
            &majors,
            &[],
            &style,
            DPI,
        );
        assert_eq!(glyph_run_count(&scene), majors.len());
    }
}
