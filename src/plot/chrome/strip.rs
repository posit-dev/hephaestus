//! Facet strip rendering — labeled bands at the panel's edges that
//! identify the facet a plot belongs to.
//!
//! A strip is a horizontal (top / bottom) or vertical (left / right)
//! band drawn between the panel and any outer chrome. Each strip
//! consumes three theme entries — [`Theme::strip_background`],
//! [`Theme::strip_text`], and [`Theme::strip_padding`] — resolved
//! against the strip's `(channel, side)` pair via the standard
//! `Sided<_>` cascade.
//!
//! The text element's [`Rotation::Along`] default flows naturally on
//! both axes: horizontal strips draw the label horizontally, vertical
//! strips draw it parallel to the panel edge. Both background and
//! text honor `Element::Blank` — a strip with both blanked still
//! reserves the slot if a label was set, drawing only what the theme
//! permits.

use crate::blend::BlendMode;
use crate::brush::Brush;
use crate::geometry::{Affine, Rect};
use crate::layout::{Measure, WidthHint};
use crate::path::{FillRule, Path};
use crate::pick::PickId;
use crate::plot::chrome::axis::axis_side_to_channel_side;
use crate::plot::chrome::linear_axis::{pt_to_px, stroke_from_rect_border};
use crate::plot::plot::{draw_text_element_in_rect, text_style_from};
use crate::plot::theme::{
    rect_concrete_defaults, text_concrete_defaults, RectElement, Rotation, TextElement, Theme,
    DEFAULT_TEXT_SIZE_PT,
};
use crate::scales::chrome::AxisSide;
use crate::scene::SceneBuilder;
use crate::text::TextRun;
use kurbo::Shape;

/// Baseline orientation, in degrees, for an [`AxisSide`] when used
/// as a strip side. Mirrors `draw_axis_title`'s convention:
/// horizontal strips run along 0°, the left strip along −90°, the
/// right along +90°. `Rotation::Along` / `Across` resolve against
/// this baseline, so a `strip_text.angle = Along` draws text parallel
/// to whichever panel edge the strip sits beside.
fn baseline_deg(side: AxisSide) -> f32 {
    match side {
        AxisSide::Top | AxisSide::Bottom => 0.0,
        AxisSide::Left => -90.0,
        AxisSide::Right => 90.0,
    }
}

/// Resolved root text size in pt — used as the parent for any `Rel`
/// `Length` in the strip text element and for `Margin` resolution.
fn theme_root_text_pt(theme: &Theme) -> f64 {
    theme
        .text
        .size_pt
        .map(|l| l.resolve(DEFAULT_TEXT_SIZE_PT))
        .unwrap_or(DEFAULT_TEXT_SIZE_PT)
}

/// Look up the strip background element for `side`. `Blank` surfaces
/// as `None`.
fn resolved_background(theme: &Theme, side: AxisSide) -> Option<RectElement> {
    let (ch, side_idx) = axis_side_to_channel_side(side);
    theme.strip_background.resolve(ch, side_idx).cloned()
}

/// Look up the strip text element for `side`. `Blank` surfaces as
/// `None`. The result is the already-cascaded element; callers fall
/// through to [`text_concrete_defaults`] for any remaining `None`
/// fields.
fn resolved_text(theme: &Theme, side: AxisSide) -> Option<TextElement> {
    let (ch, side_idx) = axis_side_to_channel_side(side);
    theme.strip_text.resolve(ch, side_idx).cloned()
}

/// Resolve `theme.strip_padding` to a `(top, right, bottom, left)`
/// tuple in pixels.
fn strip_padding_px(theme: &Theme, dpi: f64) -> (f64, f64, f64, f64) {
    let root_pt = theme_root_text_pt(theme);
    let (mt, mr, mb, ml) = theme.strip_padding.resolve(root_pt);
    (
        pt_to_px(mt, dpi),
        pt_to_px(mr, dpi),
        pt_to_px(mb, dpi),
        pt_to_px(ml, dpi),
    )
}

/// Axis-aligned bbox dimensions of a single-line run rotated by
/// `angle_deg`. `text_w` / `text_h` are the run's natural (unrotated)
/// width / height in pixels.
fn rotated_bbox(text_w: f64, text_h: f64, angle_deg: f32) -> (f64, f64) {
    let theta = (angle_deg as f64).to_radians();
    let (cos_t, sin_t) = (theta.cos().abs(), theta.sin().abs());
    let rotated_w = text_w * cos_t + text_h * sin_t;
    let rotated_h = text_w * sin_t + text_h * cos_t;
    (rotated_w, rotated_h)
}

/// Layout measurement for a facet strip. Reports the strip's
/// cross-panel thickness (row height for top / bottom strips, column
/// width for left / right strips) so the composition solver can
/// reserve room before the renderer paints the actual rect.
pub(crate) struct StripMeasure {
    side: AxisSide,
    /// Resolved cross thickness: rotated text dimension + the relevant
    /// pair of strip padding edges.
    cross_px: f64,
}

impl StripMeasure {
    /// Build a measure for `text` on `side`, consulting `theme` for
    /// strip text style and padding. Returns `None` when the theme's
    /// `strip_text` resolves to `Blank` for this side — the strip is
    /// text-driven, so suppressing text suppresses the whole strip
    /// (background included). Strips with a label but a blanked
    /// background still reserve the slot and draw the text.
    pub(crate) fn new(text: &str, side: AxisSide, theme: &Theme, dpi: f64) -> Option<Self> {
        let text_el = resolved_text(theme, side)?;
        let root_pt = theme_root_text_pt(theme);
        let (pt_top, pt_right, pt_bottom, pt_left) = strip_padding_px(theme, dpi);

        let style = text_style_from(&text_el, root_pt);
        let run = TextRun::new(text, &style, dpi);
        let _ = run.set_max_width(f32::INFINITY, crate::text::Alignment::Start);
        let text_w = run.natural_width();
        let text_h = run.natural_height();
        let defaults = text_concrete_defaults();
        let angle = text_el.angle.or(defaults.angle).expect("angle default");
        let resolved_deg = angle.resolve(baseline_deg(side));
        let (rotated_w, rotated_h) = rotated_bbox(text_w, text_h, resolved_deg);
        let text_dim_px = if side.is_horizontal() {
            rotated_h
        } else {
            rotated_w
        };
        let padding_dim = if side.is_horizontal() {
            pt_top + pt_bottom
        } else {
            pt_left + pt_right
        };
        Some(Self {
            side,
            cross_px: text_dim_px + padding_dim,
        })
    }
}

impl Measure for StripMeasure {
    fn width_hint(&self, _dpi: f64) -> WidthHint {
        if self.side.is_vertical() {
            WidthHint::Min(self.cross_px)
        } else {
            WidthHint::Min(0.0)
        }
    }

    fn height_at(&self, _width: f64, _dpi: f64) -> f64 {
        if self.side.is_horizontal() {
            self.cross_px
        } else {
            0.0
        }
    }

    fn width_at(&self, _height: f64, _dpi: f64) -> f64 {
        if self.side.is_vertical() {
            self.cross_px
        } else {
            0.0
        }
    }
}

/// Paint the strip background, then draw the strip label inside the
/// padded interior. The strip is text-driven: `strip_text = Blank`
/// suppresses the entire strip (background included) so themes can
/// ship a default background that only appears when callers actually
/// install a label via [`Plot::strip`](crate::plot::Plot::strip).
/// `strip_background = Blank` still draws the label.
pub fn draw_strip(
    scene: &mut dyn SceneBuilder,
    text: &str,
    rect: Rect,
    side: AxisSide,
    theme: &Theme,
    dpi: f64,
) {
    if rect.x1 <= rect.x0 || rect.y1 <= rect.y0 {
        return;
    }
    let Some(text_el) = resolved_text(theme, side) else {
        return;
    };

    let bg = resolved_background(theme, side);
    let bg_path = bg.as_ref().map(|el| strip_background_path(el, rect, dpi));
    if let (Some(el), Some(path)) = (bg.as_ref(), bg_path.as_ref()) {
        paint_strip_background(scene, el, path, theme, dpi);
    }

    let root_pt = theme_root_text_pt(theme);
    let defaults = text_concrete_defaults();
    let angle = text_el.angle.or(defaults.angle).expect("angle default");
    let resolved_deg = angle.resolve(baseline_deg(side));

    // Ink-aware centering: shape the strip text and compute the offset
    // between the metric box's geometric center and the visible cap-
    // band center. Adjust the interior's padding asymmetrically along
    // the text's rotated descender direction so the visible cap-band
    // lands at the rect's geometric center, not the descender-padded
    // metric box. Mirrors the spirit of the `geom_label` descender
    // rebalance — same problem (empty descender space pushes the
    // visible glyphs off-center) handled with the cap-height metric
    // axis-labels already use, since the rect size is fixed here and
    // we can't reshape the background.
    let ink_offset_px = {
        let style = text_style_from(&text_el, root_pt);
        let run = TextRun::new(text, &style, dpi);
        let _ = run.set_max_width(f32::INFINITY, crate::text::Alignment::Start);
        run.baseline_offset() - run.cap_height() * 0.5 - run.natural_height() * 0.5
    };
    let (pt_top, pt_right, pt_bottom, pt_left) = strip_padding_px(theme, dpi);
    let (pt_top_eff, pt_right_eff, pt_bottom_eff, pt_left_eff) =
        padding_with_ink_offset(side, ink_offset_px, (pt_top, pt_right, pt_bottom, pt_left));
    let interior = Rect::new(
        rect.x0 + pt_left_eff,
        rect.y0 + pt_top_eff,
        (rect.x1 - pt_right_eff).max(rect.x0 + pt_left_eff),
        (rect.y1 - pt_bottom_eff).max(rect.y0 + pt_top_eff),
    );
    if interior.x1 <= interior.x0 || interior.y1 <= interior.y0 {
        return;
    }

    // Bake the resolved degree back so the layout-aware text renderer
    // sees a concrete rotation rather than trying to resolve `Along`
    // / `Across` against a baseline it doesn't know.
    let concrete = TextElement {
        angle: Some(Rotation::Degrees(resolved_deg)),
        ..text_el
    };
    // Clip the label to the background shape when one is present, so
    // an over-wide label respects the strip's rect and corner radius
    // even if shaping somehow exceeds the interior bounds.
    let clipping = bg_path.as_ref();
    if let Some(path) = clipping {
        scene.push_layer(BlendMode::default(), 1.0, Affine::IDENTITY, path);
    }
    draw_text_element_in_rect(
        scene,
        text,
        &concrete,
        interior,
        &theme.palette,
        root_pt,
        dpi,
        PickId::Skip,
    );
    if clipping.is_some() {
        scene.pop_layer();
    }
}

/// Shift the four-side padding so the visible cap-band centers in
/// the rect, given the text's ink offset and rotation.
///
/// `ink_offset = baseline_offset - cap_h/2 - text_h/2` is the cap-
/// band center's signed displacement from the metric box's geometric
/// center along the text's local +y axis (positive when the cap-band
/// sits below the metric center, the usual case). To put the cap-
/// band at the rect's geometric center we shift the inset center —
/// which is where the metric center pivots — by `-ink_offset` along
/// the text's local +y direction, rotated into screen space.
///
/// In screen space that means:
/// - Top / Bottom (no rotation): shift inset up. Top padding shrinks,
///   bottom grows.
/// - Left (text rotated -90°, local +y → screen +x): shift inset
///   left. Left padding shrinks, right grows.
/// - Right (text rotated +90°, local +y → screen -x): shift inset
///   right. Left padding grows, right shrinks.
///
/// Any side that would underflow zero is clamped — the strip rect's
/// interior never crosses itself.
fn padding_with_ink_offset(
    side: AxisSide,
    ink_offset: f64,
    (top, right, bottom, left): (f64, f64, f64, f64),
) -> (f64, f64, f64, f64) {
    let clamp = |v: f64| v.max(0.0);
    match side {
        AxisSide::Top | AxisSide::Bottom => (
            clamp(top - ink_offset),
            right,
            clamp(bottom + ink_offset),
            left,
        ),
        AxisSide::Left => (
            top,
            clamp(right + ink_offset),
            bottom,
            clamp(left - ink_offset),
        ),
        AxisSide::Right => (
            top,
            clamp(right - ink_offset),
            bottom,
            clamp(left + ink_offset),
        ),
    }
}

/// Build the strip background path, honoring `corner_radius`. Shared
/// by the fill + border pass and by the clip layer wrapping the
/// label, so both use the exact same shape.
fn strip_background_path(bg: &RectElement, rect: Rect, dpi: f64) -> Path {
    let defaults = rect_concrete_defaults();
    let radius_pt = bg
        .corner_radius
        .or(defaults.corner_radius)
        .map(|l| l.resolve(0.0))
        .unwrap_or(0.0);
    let radius_px = pt_to_px(radius_pt, dpi).max(0.0);
    if radius_px > 0.0 {
        crate::primitives::rounded_rect(rect, radius_px)
    } else {
        rect.to_path(0.0)
    }
}

/// Paint a strip's filled background + optional border using a
/// pre-built `path`.
fn paint_strip_background(
    scene: &mut dyn SceneBuilder,
    bg: &RectElement,
    path: &Path,
    theme: &Theme,
    dpi: f64,
) {
    let defaults = rect_concrete_defaults();
    if let Some(fill) = bg.fill.clone() {
        let brush = Brush::Solid(fill.resolve(&theme.palette));
        scene.fill(
            FillRule::NonZero,
            Affine::IDENTITY,
            &brush,
            None,
            path,
            PickId::Skip,
        );
    }
    let lw = bg
        .linewidth_pt
        .or(defaults.linewidth_pt)
        .expect("rect linewidth default")
        .resolve(1.0);
    if lw > 0.0 {
        let stroke = stroke_from_rect_border(bg, dpi);
        let color = bg
            .color
            .clone()
            .or(defaults.color)
            .expect("rect color default");
        let brush = Brush::Solid(color.resolve(&theme.palette));
        scene.stroke(&stroke, Affine::IDENTITY, &brush, None, path, PickId::Skip);
    }
}

/// Map an [`AxisSide`] to the matching [`Slot`](crate::composition::Slot)
/// for a strip rail.
pub(crate) fn strip_slot(side: AxisSide) -> crate::composition::Slot {
    use crate::composition::Slot;
    match side {
        AxisSide::Top => Slot::StripTop,
        AxisSide::Right => Slot::StripRight,
        AxisSide::Bottom => Slot::StripBottom,
        AxisSide::Left => Slot::StripLeft,
    }
}
