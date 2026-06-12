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
use crate::geometry::{Affine, Point, Rect};
use crate::layout::{Measure, WidthHint};
use crate::pick::PickId;
use crate::plot::chrome::linear_axis::{
    axis_ink, draw_axis_label, draw_linear_axis_at, pt_to_px, AxisLabelAt, LABEL_FONT_SIZE_PT,
    LABEL_GAP_PT, MINOR_TICK_LENGTH_PT, STROKE_WIDTH_PT, TICK_LENGTH_PT,
};
use crate::plot::projection::PolarProjection;
use crate::plot::scale::Scale;
use crate::primitives::segment;
use crate::scales::breaks::DEFAULT_BREAK_COUNT;
use crate::scales::chrome::AxisSide;
use crate::scales::value::Value;
use crate::scene::SceneBuilder;
use crate::stroke::Stroke;
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
