//! Polar axis renderers. Per-axis entry points called from
//! [`Plot::draw_chrome_into`] for each `Axis` whose placement is
//! polar-shaped. Cartesian axes go through
//! [`crate::plot::chrome::axis`]; the in-panel grid (rings + spokes
//! + background) goes through [`crate::plot::chrome::panel`].
//!
//! Drawn unclipped — labels may extend outside the inscribed disk
//! and overflow into whatever space the panel rect has around it.
//! The "reserve a bleed strip" follow-up noted on
//! `ChromeStrategy::InsidePanel` is not implemented yet.

use crate::brush::Brush;
use crate::geometry::{Affine, Point, Rect, Vec2};
use crate::layout::{Measure, WidthHint};
use crate::pick::PickId;
use crate::plot::chrome::linear_axis::{
    axis_ink, draw_axis_label, draw_linear_axis_at, pt_to_px, AxisLabelAt, LABEL_FONT_SIZE_PT,
    LABEL_GAP_PT, MINOR_TICK_LENGTH_PT, STROKE_WIDTH_PT, TICK_LENGTH_PT,
};
use crate::plot::projection::PolarProjection;
use crate::plot::scale::Scale;
use crate::primitives::{segment, PolylineSampler};
use crate::scales::breaks::DEFAULT_BREAK_COUNT;
use crate::scales::chrome::AxisSide;
use crate::scales::value::Value;
use crate::scene::SceneBuilder;
use crate::scene::{Glyph, GlyphRun};
use crate::stroke::Stroke;
use crate::text::run_layout_glyphs;
use crate::text::{Alignment, TextRun, TextStyle};

/// Draw a radius axis along the spoke at `theta_frac` ∈ [0, 1].
/// Baseline + minor ticks + major ticks + labels via the shared
/// linear-axis helper, so the rail matches the cartesian + colorbar
/// axes pixel-for-pixel. Endpoints use `polar.unit_position` so for
/// chord-style projections (radar) they sit on the actual polygon
/// edge rather than the inscribing circle.
pub fn draw_radius_axis(
    scene: &mut dyn SceneBuilder,
    panel: Rect,
    polar: &PolarProjection,
    scale: &Scale,
    theta_frac: f64,
    dpi: f64,
    title: Option<&str>,
) {
    let g = polar.geometry(panel);
    if g.r_outer <= 0.0 {
        return;
    }
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

    let (ux, uy) = polar.unit_position(theta_frac);
    let start = Point::new(g.cx + g.r_inner * ux, g.cy - g.r_inner * uy);
    let end = Point::new(g.cx + g.r_outer * ux, g.cy - g.r_outer * uy);
    let tick_direction = radius_axis_tick_direction(polar, theta_frac);
    draw_linear_axis_at(scene, start, end, tick_direction, &majors, &minors, dpi);

    if let Some(title_text) = title {
        // Largest label width / height — used to budget the title's
        // outward distance from the spoke's outer tip.
        let style = TextStyle::new(LABEL_FONT_SIZE_PT);
        let (max_label_w, max_label_h) =
            majors
                .iter()
                .fold((0.0_f64, 0.0_f64), |(mw, mh), (_, label)| {
                    let run = TextRun::new(label, &style);
                    let h = run.set_max_width(f32::INFINITY, Alignment::Start) as f64;
                    let w = run.natural_width();
                    (mw.max(w), mh.max(h))
                });
        // Projecting the label's bbox onto the tick direction picks
        // whichever axis the label is offset along. Equivalent to the
        // cartesian title placement past the longest label.
        let (tx, ty) = tick_direction;
        let label_extent = max_label_w * tx.abs() + max_label_h * ty.abs();
        draw_radius_title(
            scene,
            panel,
            polar,
            theta_frac,
            label_extent,
            title_text,
            dpi,
        );
    }
}

/// Draw an angular axis around the outer or inner ring of a polar
/// projection. Tick marks stick radially OUTWARD from the outer
/// ring, INWARD from the inner ring (so they fall in the negative
/// space, not into the data area). Labels follow the tick direction
/// using the same quadrant-aware placement as cartesian axes.
///
/// The inner variant is a no-op when `polar.inner_radius_frac == 0`
/// — there's no inner ring to label.
pub fn draw_angular_axis(
    scene: &mut dyn SceneBuilder,
    panel: Rect,
    polar: &PolarProjection,
    scale: &Scale,
    ring: AngularRing,
    dpi: f64,
    title: Option<&str>,
) {
    let g = polar.geometry(panel);
    if g.r_outer <= 0.0 {
        return;
    }
    let (ring_r, tick_sign) = match ring {
        AngularRing::Outer => (g.r_outer, 1.0_f64),
        AngularRing::Inner => {
            if g.r_inner <= 0.0 {
                return;
            }
            (g.r_inner, -1.0)
        }
    };

    let span = polar.theta_end - polar.theta_start;
    let is_full_circle = (span.abs() - std::f64::consts::TAU).abs() < 1e-6;

    let stroke_px = pt_to_px(STROKE_WIDTH_PT, dpi);
    let tick_px = pt_to_px(TICK_LENGTH_PT, dpi);
    let minor_tick_px = pt_to_px(MINOR_TICK_LENGTH_PT, dpi);
    let label_gap_px = pt_to_px(LABEL_GAP_PT, dpi);
    let brush = Brush::Solid(axis_ink());
    let stroke = Stroke::new(stroke_px);
    let style = TextStyle::new(LABEL_FONT_SIZE_PT);

    // Minor ticks first so majors paint on top if they coincide.
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
        let on_ring = Point::new(g.cx + ring_r * theta.cos(), g.cy - ring_r * theta.sin());
        let (rx, ry) = (tick_sign * theta.cos(), -tick_sign * theta.sin());
        let tick_end = Point::new(
            on_ring.x + minor_tick_px * rx,
            on_ring.y + minor_tick_px * ry,
        );
        scene.stroke(
            &stroke,
            Affine::IDENTITY,
            &brush,
            None,
            &segment(on_ring, tick_end),
            PickId::Skip,
        );
    }

    for v in &scale.breaks(DEFAULT_BREAK_COUNT) {
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
        if is_full_circle && theta_frac >= 1.0 - 1e-9 {
            continue;
        }
        let theta = polar.theta_for_frac(theta_frac);
        let on_ring = Point::new(g.cx + ring_r * theta.cos(), g.cy - ring_r * theta.sin());
        let (rx, ry) = (tick_sign * theta.cos(), -tick_sign * theta.sin());
        let tick_end = Point::new(on_ring.x + tick_px * rx, on_ring.y + tick_px * ry);
        scene.stroke(
            &stroke,
            Affine::IDENTITY,
            &brush,
            None,
            &segment(on_ring, tick_end),
            PickId::Skip,
        );
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

    if let Some(title_text) = title {
        // Largest label height — used to push the title beyond the
        // angular tick labels.
        let (max_label_w, max_label_h) = scale
            .breaks(DEFAULT_BREAK_COUNT)
            .iter()
            .filter(|v| !matches!(v, Value::Null))
            .fold((0.0_f64, 0.0_f64), |(mw, mh), v| {
                let label = scale.format(v);
                let run = TextRun::new(&label, &style);
                let h = run.set_max_width(f32::INFINITY, Alignment::Start) as f64;
                let w = run.natural_width();
                (mw.max(w), mh.max(h))
            });
        let label_max = max_label_w.max(max_label_h);
        // Outer ring titles sit further out past the label rail;
        // inner ring titles sit further in (toward the centre).
        match ring {
            AngularRing::Outer => {
                draw_angular_title(scene, panel, polar, label_max, title_text, dpi);
            }
            AngularRing::Inner => {
                // Inner-ring title placement isn't implemented:
                // labels point inward and a curved title there
                // would compete for centre space. Silently skip.
            }
        }
    }
}

/// Which ring an angular axis runs along.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AngularRing {
    Outer,
    Inner,
}

// ─── Bleed reservation ──────────────────────────────────────────────────────

/// Per-side bleed (in px) the polar chrome wants reserved outside the
/// panel so axis labels don't get clipped at the panel boundary.
/// Computed at wire time from the projection's axes and dropped into
/// the patch's [`AxisSide`] slots — the same slots cartesian plots
/// use for their axis chromes — so the layout solver shrinks the
/// panel to leave room. The conservative bound is panel-independent:
/// the pt-sized parts (tick + gap + label-half) upper-bound the real
/// bleed for axis-aligned labels and over-reserve slightly for
/// diagonal ones.
#[derive(Clone, Debug)]
pub struct PolarBleed {
    pub top_px: f64,
    pub right_px: f64,
    pub bottom_px: f64,
    pub left_px: f64,
}

impl PolarBleed {
    /// Bleed in px on `side`. Used by [`PolarBleedMeasure`] to
    /// report the per-slot reservation.
    pub fn on(&self, side: AxisSide) -> f64 {
        match side {
            AxisSide::Top => self.top_px,
            AxisSide::Right => self.right_px,
            AxisSide::Bottom => self.bottom_px,
            AxisSide::Left => self.left_px,
        }
    }
}

/// Layout [`Measure`] that reserves `bleed.on(side)` on its
/// principal axis (width for Left/Right, height for Top/Bottom) and
/// zero on the cross axis — matching how the cartesian axis
/// measure reports its chrome thickness.
pub struct PolarBleedMeasure {
    pub side: AxisSide,
    pub bleed: PolarBleed,
}

impl Measure for PolarBleedMeasure {
    fn width_hint(&self, _dpi: f64) -> WidthHint {
        if self.side.is_vertical() {
            WidthHint::Min(self.bleed.on(self.side))
        } else {
            WidthHint::Min(0.0)
        }
    }
    fn height_at(&self, _width: f64, _dpi: f64) -> f64 {
        if self.side.is_horizontal() {
            self.bleed.on(self.side)
        } else {
            0.0
        }
    }
}

/// Compute per-side polar bleed from a plot's polar axes.
///
/// For each label, only sides the label *actually projects toward*
/// contribute. A label at `θ` with `cos θ > 0` contributes to the
/// right; `cos θ < 0` to the left; `sin θ > 0` to the top;
/// `sin θ < 0` to the bottom. Cardinal labels contribute to exactly
/// one side; diagonals contribute to two (their corner bleeds into
/// both).
///
/// Per-label contribution depends on the placement kind:
///
/// - **Outer angular** (anchored at near edge past
///   `r_outer + tick + gap`): full label width / height in the
///   cardinal direction (matches the axis-aligned worst case).
/// - **Radius axis** (centred on the spoke): half the label width
///   / height — the label sits *on* the spoke, never past it by
///   more than half its size.
/// - **Inner angular** (anchored on the inner ring, pointing
///   inward toward the polar centre): never bleeds past the panel
///   boundary; skipped here.
pub fn compute_polar_bleed(axes: &[BleedAxis], dpi: f64) -> PolarBleed {
    let tick_px = pt_to_px(TICK_LENGTH_PT, dpi);
    let gap_px = pt_to_px(LABEL_GAP_PT, dpi);
    let style = TextStyle::new(LABEL_FONT_SIZE_PT);
    let mut bleed = PolarBleed {
        top_px: 0.0,
        right_px: 0.0,
        bottom_px: 0.0,
        left_px: 0.0,
    };
    // Each label is described by its anchor in bbox-normalised panel
    // coords plus the tick `direction` it's offset by. The label's
    // centre sits at `anchor + dir * size / 2`, and the text box
    // extends ± size / 2 from that centre. Outer-angular labels
    // anchor `tick + gap` past the bbox boundary in `dir`; radius
    // labels anchor on the spoke itself.
    //
    // For a label whose normalised anchor sits on a given panel
    // edge, the bleed past that edge is the full distance from the
    // edge to the far side of the text box in that direction. The
    // formulas below derive directly from substituting `dir_x ∈
    // {-1, 0, 1}` (or fractional) into the box-extent equations,
    // matching the placement logic in `draw_axis_label`.
    const EDGE_EPS: f64 = 1e-3;
    for axis in axes {
        for label in &axis.labels {
            let run = TextRun::new(&label.text, &style);
            let h = run.set_max_width(f32::INFINITY, Alignment::Start) as f64;
            let w = match run.width_hint(dpi) {
                WidthHint::Min(w) => w,
                WidthHint::NeedsHeight { seed } => seed,
            };
            let anchor_offset = match label.kind {
                BleedLabelKind::OuterAngular | BleedLabelKind::Radius => tick_px + gap_px,
                BleedLabelKind::InnerAngular => continue,
            };
            let (nx, ny) = label.outer_pos;
            let (dx, dy) = label.direction;
            if nx <= EDGE_EPS {
                let b = anchor_offset * -dx + (1.0 - dx) * w * 0.5;
                if b > 0.0 {
                    bleed.left_px = bleed.left_px.max(b);
                }
            }
            if nx >= 1.0 - EDGE_EPS {
                let b = anchor_offset * dx + (1.0 + dx) * w * 0.5;
                if b > 0.0 {
                    bleed.right_px = bleed.right_px.max(b);
                }
            }
            if ny <= EDGE_EPS {
                let b = anchor_offset * -dy + (1.0 - dy) * h * 0.5;
                if b > 0.0 {
                    bleed.top_px = bleed.top_px.max(b);
                }
            }
            if ny >= 1.0 - EDGE_EPS {
                let b = anchor_offset * dy + (1.0 + dy) * h * 0.5;
                if b > 0.0 {
                    bleed.bottom_px = bleed.bottom_px.max(b);
                }
            }
        }
    }
    bleed
}

/// One axis's labels for the bleed computer.
pub struct BleedAxis {
    pub labels: Vec<BleedLabel>,
}

/// One label's contribution to the bleed.
///
/// `outer_pos` is the anchor's position in **bbox-normalised** panel
/// coordinates — `(0, 0)` is top-left of the projection's bounding
/// box (which matches the panel rect when `fit_to_bbox = true`),
/// `(1, 1)` bottom-right. `direction` is the screen-space unit
/// vector the chrome offsets the label by from its anchor (mirrors
/// the `AxisLabelAt::direction` `draw_axis_label` uses): a radius
/// label at a horizontal bottom spoke has direction `(0, 1)`,
/// pushing the entire label below the anchor.
pub struct BleedLabel {
    pub text: String,
    pub kind: BleedLabelKind,
    pub outer_pos: (f64, f64),
    pub direction: (f64, f64),
}

/// How a label is placed relative to the polar geometry; drives
/// whether it can bleed past the panel and by how much.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BleedLabelKind {
    /// Outer angular axis label — anchored at near edge past
    /// `r_outer + tick + gap` in the direction of `theta`.
    OuterAngular,
    /// Inner angular axis label — sits on the inner ring pointing
    /// inward. Never bleeds.
    InnerAngular,
    /// Radius axis label — centred on the spoke at `theta`.
    Radius,
}

// ─── Helpers ────────────────────────────────────────────────────────────────

/// Screen-space unit vector perpendicular to the spoke at
/// `theta_frac`, rotated so it points OUTSIDE the swept polar
/// region (away from the sweep direction). For full-circle layouts
/// the answer is one of the two cardinal sides; for partial arcs it
/// picks the "exterior" side of the swept wedge.
fn radius_axis_tick_direction(polar: &PolarProjection, theta_frac: f64) -> (f64, f64) {
    // sign = +1 for CCW sweep (theta_end > theta_start in math), -1 for CW.
    let sign = if polar.theta_end > polar.theta_start {
        1.0
    } else {
        -1.0
    };
    let theta = polar.theta_for_frac(theta_frac);
    // CW perpendicular for CCW sweep / CCW perpendicular for CW sweep,
    // expressed in math convention and then flipped to screen y-down.
    (sign * theta.sin(), sign * theta.cos())
}

// Gap (pt) between the outermost axis label and the title text.
const TITLE_GAP_PT: f64 = 6.0;
// Number of segments to sample an angular title's arc into a polyline
// for the text-along-path layout.
const ANGULAR_TITLE_ARC_SEGMENTS: usize = 32;

/// Render a radius axis title past the outer end of the spoke. The
/// title rotates to align with the spoke and auto-flips if rendering
/// would place it upside-down on screen.
///
/// `label_extent_px` is the projected extent of the outermost tick
/// label onto the tick direction — it pushes the title past the
/// label rail.
fn draw_radius_title(
    scene: &mut dyn SceneBuilder,
    panel: Rect,
    polar: &PolarProjection,
    theta_frac: f64,
    label_extent_px: f64,
    title: &str,
    dpi: f64,
) {
    let g = polar.geometry(panel);
    if g.r_outer <= 0.0 {
        return;
    }
    let (ux, uy) = polar.unit_position(theta_frac);
    // Spoke direction in screen-y-down coords.
    let (sx, sy) = (ux, -uy);
    // Rotation angle (math screen coords, y-down).
    let mut theta_spoke = sy.atan2(sx);

    let style = crate::plot::plot::axis_title_style();
    let run = crate::text::TextRun::new(title, &style);
    let title_w = run.natural_width();
    let title_h = run.set_max_width(f32::INFINITY, crate::text::Alignment::Start) as f64;
    let glyphs = run_layout_glyphs(&run);
    if glyphs.is_empty() {
        return;
    }
    let baseline_ref = glyphs[0].y as f64;

    let tick_px = pt_to_px(TICK_LENGTH_PT, dpi);
    let label_gap_px = pt_to_px(LABEL_GAP_PT, dpi);
    let title_gap_px = pt_to_px(TITLE_GAP_PT, dpi);
    let outer_offset = tick_px + label_gap_px + label_extent_px + title_gap_px;

    // Anchor sits past the outer tick + label rail, along the spoke
    // from the polar centre.
    let outer_r = g.r_outer + outer_offset;
    let anchor_x = g.cx + outer_r * ux;
    let anchor_y = g.cy - outer_r * uy;

    // Upright correction: if the title's natural baseline orientation
    // would render upside-down (sin(θ) > 0 in screen-y-down ⇒ baseline
    // normal points down ⇒ glyphs appear upside-down), add π and slide
    // the anchor along the rotated baseline so the title still
    // occupies the outer portion of the spoke.
    let upside_down = theta_spoke.sin() > 0.0;
    let (origin_x, origin_y) = if upside_down {
        theta_spoke += std::f64::consts::PI;
        // After flipping, the title's local x runs back along the
        // spoke (inward). Slide the origin outward by title_w + 2 *
        // baseline body so the body sits in the same world location.
        let cos_t = theta_spoke.cos();
        let sin_t = theta_spoke.sin();
        (anchor_x - title_w * cos_t, anchor_y - title_w * sin_t)
    } else {
        (anchor_x, anchor_y)
    };

    // Centre the title across the spoke (along the perpendicular)
    // by offsetting half its height. In glyph-local y-down space
    // the baseline is at `baseline_ref`, so a y-offset of
    // `-title_h * 0.5 + baseline_ref` puts the title bbox centre on
    // the spoke.
    let perp_offset = -title_h * 0.5 + baseline_ref;

    let brush = Brush::Solid(axis_ink());
    for g_glyph in &glyphs {
        let y_above_baseline = g_glyph.y as f64 - baseline_ref;
        let xform = Affine::translate(Vec2::new(origin_x, origin_y))
            * Affine::rotate(theta_spoke)
            * Affine::translate(Vec2::new(g_glyph.x as f64, perp_offset + y_above_baseline));
        let stamp = Glyph {
            id: g_glyph.id,
            x: 0.0,
            y: 0.0,
        };
        let glyph_run = GlyphRun {
            font: &g_glyph.font,
            font_size: g_glyph.font_size,
            transform: xform,
            glyph_transform: None,
            brush: &brush,
            brush_alpha: 1.0,
            hint: false,
            glyphs: std::slice::from_ref(&stamp),
        };
        scene.draw_glyphs(&glyph_run, PickId::Skip);
    }
}

/// Render an angular axis title curving along an arc just past the
/// outer ring. The title spans an angular extent of `text_w / r_title`
/// centred at the arc midpoint. Uses text-on-path layout with the
/// `upright` flip so the title reads right-side-up regardless of
/// which side of the panel the arc midpoint lands on.
fn draw_angular_title(
    scene: &mut dyn SceneBuilder,
    panel: Rect,
    polar: &PolarProjection,
    label_max_px: f64,
    title: &str,
    dpi: f64,
) {
    let g = polar.geometry(panel);
    if g.r_outer <= 0.0 {
        return;
    }
    let style = crate::plot::plot::axis_title_style();
    let run = crate::text::TextRun::new(title, &style);
    let text_w = run.natural_width();
    let _title_h = run.set_max_width(f32::INFINITY, crate::text::Alignment::Start) as f64;
    let glyphs = run_layout_glyphs(&run);
    if glyphs.is_empty() || text_w <= 0.0 {
        return;
    }
    let baseline_ref = glyphs[0].y as f64;
    let descent_px = run.last_line_descender();
    let ascent_px = run.natural_height() - descent_px;

    let tick_px = pt_to_px(TICK_LENGTH_PT, dpi);
    let label_gap_px = pt_to_px(LABEL_GAP_PT, dpi);
    let title_gap_px = pt_to_px(TITLE_GAP_PT, dpi);
    let r_title = g.r_outer + tick_px + label_gap_px + label_max_px + title_gap_px;
    if r_title <= 0.0 {
        return;
    }

    // Arc midpoint angle (math convention).
    let span = polar.theta_end - polar.theta_start;
    let is_full_circle = (span.abs() - std::f64::consts::TAU).abs() < 1e-6;
    let theta_mid_math = if is_full_circle {
        std::f64::consts::FRAC_PI_2 // 12 o'clock for full circles.
    } else {
        (polar.theta_start + polar.theta_end) * 0.5
    };
    // Arc length the title spans, in radians.
    let arc_radians = text_w / r_title;
    if !arc_radians.is_finite() || arc_radians <= 0.0 {
        return;
    }
    // Sweep direction matches the polar's natural sweep (CCW or CW).
    let sweep_sign = if polar.theta_end > polar.theta_start {
        -1.0
    } else {
        1.0
    };
    let start_math = theta_mid_math - sweep_sign * arc_radians * 0.5;
    let end_math = theta_mid_math + sweep_sign * arc_radians * 0.5;

    // Sample the arc into a polyline in screen-y-down coords.
    let n = ANGULAR_TITLE_ARC_SEGMENTS;
    let mut points: Vec<Point> = Vec::with_capacity(n + 1);
    for i in 0..=n {
        let t = i as f64 / n as f64;
        let theta = start_math + (end_math - start_math) * t;
        points.push(Point::new(
            g.cx + r_title * theta.cos(),
            g.cy - r_title * theta.sin(),
        ));
    }
    let sampler = PolylineSampler::from_polyline(&points);
    let path_length = sampler.total_length();
    if path_length <= 0.0 {
        return;
    }

    // Upright detection: count glyph tangents pointing into the left
    // half-plane against a threshold; reverse the arc if the majority
    // do. Same logic as TextPathGeom's `upright = true` mode.
    let natural_shift = (path_length - text_w) * 0.5; // hjust = 0.5
    let mut upside_down = 0usize;
    let mut counted = 0usize;
    for gph in &glyphs {
        let half_advance = gph.advance as f64 * 0.5;
        let d = natural_shift + gph.x as f64 + half_advance;
        if !d.is_finite() {
            continue;
        }
        let d_clamped = d.clamp(0.0, path_length);
        if let Some(s) = sampler.sample_at(d_clamped) {
            counted += 1;
            if s.tangent.x < 0.0 {
                upside_down += 1;
            }
        }
    }
    let flipped = counted > 0 && upside_down * 2 > counted;

    let hjust_shift = if flipped {
        // hjust = 0.5 inverts to 0.5 (centre), so the shift is the same.
        natural_shift
    } else {
        natural_shift
    };
    // When flipped, the glyph body extends "downward" from the
    // baseline; rebase by (ascent - descent) so its world-bbox
    // matches the unflipped case.
    let effective_vjust = if flipped { ascent_px - descent_px } else { 0.0 };

    let brush = Brush::Solid(axis_ink());
    for gph in &glyphs {
        let half_advance = gph.advance as f64 * 0.5;
        let d_glyph = hjust_shift + gph.x as f64 + half_advance;
        if !d_glyph.is_finite() || d_glyph < 0.0 || d_glyph > path_length {
            continue;
        }
        let d_sample = if flipped {
            path_length - d_glyph
        } else {
            d_glyph
        };
        let sample = match sampler.sample_at(d_sample) {
            Some(s) => s,
            None => continue,
        };
        let tangent = if flipped {
            -sample.tangent
        } else {
            sample.tangent
        };
        let theta = tangent.y.atan2(tangent.x);
        let y_above_baseline = gph.y as f64 - baseline_ref;
        let xform = Affine::translate(Vec2::new(sample.point.x, sample.point.y))
            * Affine::rotate(theta)
            * Affine::translate(Vec2::new(-half_advance, effective_vjust + y_above_baseline));
        let stamp = Glyph {
            id: gph.id,
            x: 0.0,
            y: 0.0,
        };
        let glyph_run = GlyphRun {
            font: &gph.font,
            font_size: gph.font_size,
            transform: xform,
            glyph_transform: None,
            brush: &brush,
            brush_alpha: 1.0,
            hint: false,
            glyphs: std::slice::from_ref(&stamp),
        };
        scene.draw_glyphs(&glyph_run, PickId::Skip);
    }
}
