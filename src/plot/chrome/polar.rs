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
    axis_ink, draw_axis_label, draw_linear_axis_at, AxisChromeStyle, AxisLabelAt,
};
use crate::plot::projection::PolarProjection;
use crate::plot::scale::Scale;
use crate::plot::theme::Theme;
use crate::primitives::{segment, PolylineSampler};
use crate::scales::breaks::DEFAULT_BREAK_COUNT;
use crate::scales::chrome::AxisSide;
use crate::scales::value::Value;
use crate::scene::SceneBuilder;
use crate::scene::{Glyph, GlyphRun};
use crate::text::run_layout_glyphs;
use crate::text::{Alignment, TextRun, TextStyle};

/// Draw a radius axis along the spoke at `theta_frac` ∈ [0, 1].
/// Baseline + minor ticks + major ticks + labels via the shared
/// linear-axis helper, so the rail matches the cartesian + colorbar
/// axes pixel-for-pixel. Endpoints use `polar.unit_position` so for
/// chord-style projections (radar) they sit on the actual polygon
/// edge rather than the inscribing circle.
#[allow(clippy::too_many_arguments)]
pub fn draw_radius_axis(
    scene: &mut dyn SceneBuilder,
    panel: Rect,
    polar: &PolarProjection,
    scale: &Scale,
    theta_frac: f64,
    dpi: f64,
    title: Option<&str>,
    theme: &Theme,
) {
    let g = polar.geometry(panel);
    if g.r_outer <= 0.0 {
        return;
    }
    let majors: Vec<(f64, String)> = scale
        .breaks(DEFAULT_BREAK_COUNT)
        .iter()
        .filter(|v| !matches!(v, Value::Null))
        .filter_map(|v| {
            scale
                .map(v)
                .as_number()
                .map(|f| (f, scale.format(v, &theme.locale)))
        })
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
    // Polar radius axis = channel 1, primary spoke (side 0).
    let resolved = theme.axis.resolve(1, 0);
    let style = AxisChromeStyle::from_resolved(&resolved, &theme.palette, dpi);
    draw_linear_axis_at(
        scene,
        start,
        end,
        tick_direction,
        &majors,
        &minors,
        &style,
        dpi,
    );

    if let Some(title_text) = title {
        if let Some(title_el) = resolved.title.as_ref() {
            // Largest label width / height — used to budget the title's
            // outward distance from the spoke's outer tip. Reuse the
            // chrome-resolved label style so the label-extent
            // calculation matches what the rail itself drew.
            let label_style = style.text_style.clone();
            let (max_label_w, max_label_h) =
                majors
                    .iter()
                    .fold((0.0_f64, 0.0_f64), |(mw, mh), (_, label)| {
                        let run = TextRun::new(label, &label_style, dpi);
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
                title_el,
                &theme.palette,
                style.tick_length_px,
                style.gap_px,
                style.title_gap_px,
                dpi,
            );
        }
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
#[allow(clippy::too_many_arguments)]
pub fn draw_angular_axis(
    scene: &mut dyn SceneBuilder,
    panel: Rect,
    polar: &PolarProjection,
    scale: &Scale,
    ring: AngularRing,
    dpi: f64,
    title: Option<&str>,
    theme: &Theme,
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

    // Resolve from theme: angular axis = channel 0; outer ring is
    // side 0 (the conventional "primary" side), inner ring is side 1.
    let side_idx = match ring {
        AngularRing::Outer => 0_u8,
        AngularRing::Inner => 1_u8,
    };
    let resolved = theme.axis.resolve(0, side_idx);
    let chrome_style = AxisChromeStyle::from_resolved(&resolved, &theme.palette, dpi);
    let tick_px = chrome_style.tick_length_px;
    let minor_tick_px = chrome_style.minor_tick_length_px;
    let label_gap_px = chrome_style.gap_px;
    let style = chrome_style.text_style.clone();

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
        if let Some(minor_brush) = chrome_style.minor_brush.as_ref() {
            scene.stroke(
                &chrome_style.minor_stroke,
                Affine::IDENTITY,
                minor_brush,
                None,
                &segment(on_ring, tick_end),
                PickId::Skip,
            );
        }
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
        if let (Some(tick_brush), tick_stroke) =
            (chrome_style.tick_brush.as_ref(), &chrome_style.tick_stroke)
        {
            scene.stroke(
                tick_stroke,
                Affine::IDENTITY,
                tick_brush,
                None,
                &segment(on_ring, tick_end),
                PickId::Skip,
            );
        }
        let anchor = Point::new(
            tick_end.x + label_gap_px * rx,
            tick_end.y + label_gap_px * ry,
        );
        let text = scale.format(v, &theme.locale);
        draw_axis_label(
            scene,
            &text,
            &style,
            &chrome_style.text_brush,
            AxisLabelAt {
                anchor,
                direction: (rx, ry),
            },
            dpi,
        );
    }

    if let Some(title_text) = title {
        if let Some(title_el) = resolved.title.as_ref() {
            // Largest label height — used to push the title beyond
            // the angular tick labels.
            let (max_label_w, max_label_h) = scale
                .breaks(DEFAULT_BREAK_COUNT)
                .iter()
                .filter(|v| !matches!(v, Value::Null))
                .fold((0.0_f64, 0.0_f64), |(mw, mh), v| {
                    let label = scale.format(v, &theme.locale);
                    let run = TextRun::new(&label, &style, dpi);
                    let h = run.set_max_width(f32::INFINITY, Alignment::Start) as f64;
                    let w = run.natural_width();
                    (mw.max(w), mh.max(h))
                });
            let label_max = max_label_w.max(max_label_h);
            // Outer ring titles sit further out past the label rail;
            // inner ring titles sit further in (toward the centre).
            match ring {
                AngularRing::Outer => {
                    draw_angular_title(
                        scene,
                        panel,
                        polar,
                        label_max,
                        title_text,
                        title_el,
                        &theme.palette,
                        chrome_style.tick_length_px,
                        chrome_style.gap_px,
                        chrome_style.title_gap_px,
                        dpi,
                    );
                }
                AngularRing::Inner => {
                    // Inner-ring title placement isn't implemented:
                    // labels point inward and a curved title there
                    // would compete for centre space. Silently skip.
                }
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
///
/// Axis titles ([`BleedAxis::title`]) push the bleed further past the
/// label rail when present. Outer-angular titles sit at the arc
/// midpoint, curving along an arc; their radial extent past `r_outer`
/// is `tick + gap + label_max + title_gap + title_h` and is
/// distributed to cardinal sides by the midpoint direction.
pub fn compute_polar_bleed(axes: &[BleedAxis], dpi: f64, theme: &Theme) -> PolarBleed {
    // Resolve each polar axis-type's chrome style once; pick per-label
    // by `BleedLabelKind`. Angular labels (outer ring) live on
    // channel 0 / side 0; radius labels live on channel 1 / side 0.
    let angular_style =
        AxisChromeStyle::from_resolved(&theme.axis.resolve(0, 0), &theme.palette, dpi);
    let radial_style =
        AxisChromeStyle::from_resolved(&theme.axis.resolve(1, 0), &theme.palette, dpi);
    let title_style_for = |kind: &BleedLabelKind| match kind {
        BleedLabelKind::Radius => &radial_style,
        _ => &angular_style,
    };
    let mut bleed = PolarBleed {
        top_px: 0.0,
        right_px: 0.0,
        bottom_px: 0.0,
        left_px: 0.0,
    };
    // Each label is described by the tick `direction` it offsets by
    // from its anchor on the polar boundary. After `draw_axis_label`'s
    // quadrant-aware quantisation, the label's bounding box spans
    //   horizontal: anchor.x + (dx_q - 1) * w/2 .. anchor.x + (dx_q + 1) * w/2
    //   vertical:   anchor.y + (dy_q - 1) * h/2 .. anchor.y + (dy_q + 1) * h/2
    // where dir_q ∈ {-1, 0, 1} after the cardinal dead-band, and the
    // anchor itself sits `anchor_offset * (dx, dy)` past the polar
    // boundary in the (raw, non-quantised) tick direction.
    //
    // The conservative bleed past a cardinal panel edge assumes the
    // label's anchor sits on the matching bbox edge — over-reserves by
    // up to (1 - n) * scaled_bbox px for labels whose anchor lies
    // inside the bbox in that direction, but never under-reserves.
    // For full-circle layouts the cardinal anchors land exactly on the
    // bbox edges so the over-reservation collapses to zero; partial
    // arcs whose sweep crosses a cardinal direction between two breaks
    // (no break exactly at the cardinal) rely on this conservative
    // contribution to reserve space for the near-cardinal labels.
    const CARDINAL_EPS: f64 = 0.05;
    for axis in axes {
        for label in &axis.labels {
            let label_style = title_style_for(&label.kind);
            let run = TextRun::new(&label.text, &label_style.text_style, dpi);
            let h = run.set_max_width(f32::INFINITY, Alignment::Start) as f64;
            let w = match run.width_hint(dpi) {
                WidthHint::Min(w) => w,
                WidthHint::NeedsHeight { seed } => seed,
            };
            let anchor_offset = match label.kind {
                BleedLabelKind::OuterAngular | BleedLabelKind::Radius => {
                    label_style.tick_length_px + label_style.gap_px
                }
                BleedLabelKind::InnerAngular => continue,
            };
            let (dx, dy) = label.direction;
            let dx_q = if dx > CARDINAL_EPS {
                1.0
            } else if dx < -CARDINAL_EPS {
                -1.0
            } else {
                0.0
            };
            let dy_q = if dy > CARDINAL_EPS {
                1.0
            } else if dy < -CARDINAL_EPS {
                -1.0
            } else {
                0.0
            };
            let b_right = dx * anchor_offset + (dx_q + 1.0) * w * 0.5;
            if b_right > 0.0 {
                bleed.right_px = bleed.right_px.max(b_right);
            }
            let b_left = -dx * anchor_offset + (1.0 - dx_q) * w * 0.5;
            if b_left > 0.0 {
                bleed.left_px = bleed.left_px.max(b_left);
            }
            let b_bottom = dy * anchor_offset + (dy_q + 1.0) * h * 0.5;
            if b_bottom > 0.0 {
                bleed.bottom_px = bleed.bottom_px.max(b_bottom);
            }
            let b_top = -dy * anchor_offset + (1.0 - dy_q) * h * 0.5;
            if b_top > 0.0 {
                bleed.top_px = bleed.top_px.max(b_top);
            }
        }
        if let Some(title) = &axis.title {
            match title.kind {
                BleedTitleKind::OuterAngular {
                    direction: (dx, dy),
                    label_max_px,
                } => {
                    // Outer angular title — uses the angular axis's
                    // resolved text style (and the resolved tick /
                    // gap / title_gap) so measured bleed matches the
                    // draw helper exactly. `Blank` short-circuits:
                    // no draw → no bleed reservation.
                    let Some(title_text_style) =
                        angular_title_text_style(&theme.axis.resolve(0, 0))
                    else {
                        continue;
                    };
                    let run = TextRun::new(&title.text, &title_text_style, dpi);
                    let title_h = run.set_max_width(f32::INFINITY, Alignment::Start) as f64;
                    let title_w = run.natural_width();
                    // Radial extent past r_outer at the arc midpoint.
                    // Mirrors `draw_angular_title`'s placement formula.
                    let radial = angular_style.tick_length_px
                        + angular_style.gap_px
                        + label_max_px
                        + angular_style.title_gap_px
                        + title_h;
                    // The title curves along an arc, but its dominant
                    // bleed is the radial component at the midpoint.
                    // Distribute to cardinal sides by direction sign.
                    if dx > 0.0 {
                        bleed.right_px = bleed.right_px.max(dx * radial);
                    }
                    if dx < 0.0 {
                        bleed.left_px = bleed.left_px.max(-dx * radial);
                    }
                    if dy > 0.0 {
                        bleed.bottom_px = bleed.bottom_px.max(dy * radial);
                    }
                    if dy < 0.0 {
                        bleed.top_px = bleed.top_px.max(-dy * radial);
                    }
                    // Tangential half-extent — at a cardinal midpoint
                    // the curved title's endpoints lean ±title_w/2 to
                    // the perpendicular cardinal sides. Conservative.
                    let tangential = title_w * 0.5;
                    if dx.abs() > dy.abs() {
                        bleed.top_px = bleed.top_px.max(tangential);
                        bleed.bottom_px = bleed.bottom_px.max(tangential);
                    } else {
                        bleed.left_px = bleed.left_px.max(tangential);
                        bleed.right_px = bleed.right_px.max(tangential);
                    }
                }
            }
        }
    }
    bleed
}

/// One axis's labels (and optional title) for the bleed computer.
pub struct BleedAxis {
    pub labels: Vec<BleedLabel>,
    /// Title contribution to the bleed, if the axis has one. Drives
    /// the title-past-the-label-rail reservation; only outer angular
    /// titles bleed past the panel in v1.
    pub title: Option<BleedTitle>,
}

/// One axis title's contribution to the bleed.
pub struct BleedTitle {
    pub text: String,
    pub kind: BleedTitleKind,
}

/// How an axis title is placed relative to the polar geometry.
#[derive(Clone, Copy, Debug)]
pub enum BleedTitleKind {
    /// Outer angular axis title — curves along an arc just past the
    /// label rail, centred at the arc midpoint.
    OuterAngular {
        /// Arc-midpoint direction in screen-y-down coords:
        /// `(cos(θ_mid_math), -sin(θ_mid_math))`.
        direction: (f64, f64),
        /// Largest tick-label dimension on this axis, in px. Pushes
        /// the title past the label rail — same value the renderer
        /// uses in [`draw_angular_axis`].
        label_max_px: f64,
    },
}

/// One label's contribution to the bleed.
///
/// `direction` is the screen-space unit vector the chrome offsets the
/// label by from its anchor (mirrors the `AxisLabelAt::direction`
/// `draw_axis_label` uses): a radius label at a horizontal bottom
/// spoke has direction `(0, 1)`, pushing the entire label below the
/// anchor.
pub struct BleedLabel {
    pub text: String,
    pub kind: BleedLabelKind,
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

// Number of segments to sample an angular title's arc into a polyline
// for the text-along-path layout.
const ANGULAR_TITLE_ARC_SEGMENTS: usize = 32;

/// Build a `TextStyle` for the angular axis title from a
/// `ResolvedAxis`. `None` when the title element is `Blank` — both
/// the bleed measure and the draw site short-circuit instead of
/// reserving / drawing nothing.
fn angular_title_text_style(resolved: &crate::plot::theme::ResolvedAxis) -> Option<TextStyle> {
    resolved
        .title
        .as_ref()
        .map(|el| crate::plot::plot::text_style_from(el, 10.0))
}

/// Render a radius axis title past the outer end of the tick labels,
/// offset perpendicular to the spoke (matching the cartesian Y-axis
/// convention: parallel to the axis, past the label rail). Centred
/// along the visible spoke segment between `r_inner` and `r_outer`.
/// Rotates to align with the spoke and auto-flips if rendering would
/// place it upside-down on screen.
///
/// `label_extent_px` is the projected extent of the outermost tick
/// label onto the tick direction — it pushes the title past the
/// label rail.
#[allow(clippy::too_many_arguments)]
fn draw_radius_title(
    scene: &mut dyn SceneBuilder,
    panel: Rect,
    polar: &PolarProjection,
    theta_frac: f64,
    label_extent_px: f64,
    title: &str,
    title_el: &crate::plot::theme::TextElement,
    palette: &crate::plot::theme::Palette,
    tick_px: f64,
    label_gap_px: f64,
    title_gap_px: f64,
    dpi: f64,
) {
    let g = polar.geometry(panel);
    if g.r_outer <= 0.0 {
        return;
    }
    let (ux, uy) = polar.unit_position(theta_frac);
    // Spoke direction in screen-y-down coords.
    let (sx, sy) = (ux, -uy);
    // Perpendicular direction (tick direction) pointing OUTSIDE the
    // swept polar region — the same vector the tick labels offset by.
    let (tx, ty) = radius_axis_tick_direction(polar, theta_frac);
    // Rotation angle (screen coords, y-down).
    let mut theta_spoke = sy.atan2(sx);

    let root_pt = crate::plot::theme::DEFAULT_TEXT_SIZE_PT;
    let style = crate::plot::plot::text_style_from(title_el, root_pt);
    let run = crate::text::TextRun::new(title, &style, dpi);
    let title_w = run.natural_width();
    let title_h = run.set_max_width(f32::INFINITY, crate::text::Alignment::Start) as f64;
    let glyphs = run_layout_glyphs(&run);
    if glyphs.is_empty() {
        return;
    }
    let baseline_ref = glyphs[0].y as f64;

    let _ = palette; // title brush sourced below via title_el.color
                     // Perpendicular distance from the spoke to the title's bbox
                     // centre: tick + label_gap + label extent + title_gap clears the
                     // label rail; the extra title_h / 2 puts the title's near edge
                     // at exactly title_gap past the labels.
    let perp_distance = tick_px + label_gap_px + label_extent_px + title_gap_px + title_h * 0.5;

    // Midpoint of the visible spoke segment, offset perpendicular by
    // `perp_distance` along the tick direction.
    let mid_r = (g.r_inner + g.r_outer) * 0.5;
    let body_center_x = g.cx + mid_r * ux + perp_distance * tx;
    let body_center_y = g.cy - mid_r * uy + perp_distance * ty;

    // Place the rotated glyph origin so the title body centres on
    // (body_center_x, body_center_y). The body runs from local x = 0
    // to local x = title_w; the centre is at half that distance from
    // the origin along the rotated x-axis (= the spoke direction).
    let anchor_x = body_center_x - (title_w * 0.5) * sx;
    let anchor_y = body_center_y - (title_w * 0.5) * sy;

    // Upright correction: if the title's natural baseline orientation
    // would render upside-down (sin(θ) > 0 in screen-y-down ⇒ baseline
    // normal points down ⇒ glyphs appear upside-down), add π. The
    // body-centred placement above keeps the title in the same world
    // location regardless of the flip.
    let upside_down = theta_spoke.sin() > 0.0;
    let (origin_x, origin_y) = if upside_down {
        theta_spoke += std::f64::consts::PI;
        let cos_t = theta_spoke.cos();
        let sin_t = theta_spoke.sin();
        (anchor_x - title_w * cos_t, anchor_y - title_w * sin_t)
    } else {
        (anchor_x, anchor_y)
    };

    // Centre the title across the rotated baseline (perpendicular axis)
    // by offsetting half its height. In glyph-local y-down space
    // the baseline is at `baseline_ref`, so a y-offset of
    // `-title_h * 0.5 + baseline_ref` puts the title bbox centre on
    // the rotated baseline.
    let perp_offset = -title_h * 0.5 + baseline_ref;

    let title_color = title_el
        .color
        .clone()
        .map(|c| c.resolve(palette))
        .unwrap_or_else(axis_ink);
    let brush = Brush::Solid(title_color);
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
#[allow(clippy::too_many_arguments)]
fn draw_angular_title(
    scene: &mut dyn SceneBuilder,
    panel: Rect,
    polar: &PolarProjection,
    label_max_px: f64,
    title: &str,
    title_el: &crate::plot::theme::TextElement,
    palette: &crate::plot::theme::Palette,
    tick_px: f64,
    label_gap_px: f64,
    title_gap_px: f64,
    dpi: f64,
) {
    let g = polar.geometry(panel);
    if g.r_outer <= 0.0 {
        return;
    }
    let root_pt = crate::plot::theme::DEFAULT_TEXT_SIZE_PT;
    let style = crate::plot::plot::text_style_from(title_el, root_pt);
    let run = crate::text::TextRun::new(title, &style, dpi);
    let text_w = run.natural_width();
    let _title_h = run.set_max_width(f32::INFINITY, crate::text::Alignment::Start) as f64;
    let glyphs = run_layout_glyphs(&run);
    if glyphs.is_empty() || text_w <= 0.0 {
        return;
    }
    let baseline_ref = glyphs[0].y as f64;
    let descent_px = run.last_line_descender();
    let ascent_px = run.natural_height() - descent_px;

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
    // The angular title computes `r_title` directly from the label
    // rail + title_gap, so the baseline must land exactly on the
    // sampled arc. Unlike `TextPathGeom`, we don't need the
    // bbox-preservation rebasing — that would shift the baseline
    // by `ascent - descent` (radially inward for top-of-circle
    // titles) and steal back the `title_gap` we just budgeted.
    let _ = ascent_px;
    let _ = descent_px;
    let effective_vjust = 0.0;

    let title_color = title_el
        .color
        .clone()
        .map(|c| c.resolve(palette))
        .unwrap_or_else(axis_ink);
    let brush = Brush::Solid(title_color);
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
