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
use crate::geometry::Shape as _;
use crate::geometry::{Affine, Rect};
use crate::layout::{Measure, WidthHint};
use crate::path::{FillRule, Path};
use crate::pick::PickId;
use crate::plot::chrome::axis::axis_side_to_channel_side;
use crate::plot::chrome::linear_axis::{pt_to_px, stroke_from_rect_border};
use crate::plot::chrome::text::{draw_text_element_in_rect, rotated_bbox, text_style_from};
use crate::plot::pick::{part_scope, PlotPart};
use crate::plot::theme::HAlign;
use crate::plot::theme::{
    rect_concrete_defaults, text_concrete_defaults, RectElement, Rotation, TextElement, Theme,
};
use crate::scales::chrome::AxisSide;
use crate::scene::SceneBuilder;
use crate::text::TextRun;

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
    let root_pt = crate::plot::chrome::root_text_pt(theme);
    let (mt, mr, mb, ml) = theme.strip_padding.resolve(root_pt);
    (
        pt_to_px(mt, dpi),
        pt_to_px(mr, dpi),
        pt_to_px(mb, dpi),
        pt_to_px(ml, dpi),
    )
}

/// Resolve a text element's own margin to a `(top, right, bottom,
/// left)` tuple in pixels. [`draw_text_element_in_rect`] insets by it
/// before wrapping, so the strip's reserved thickness has to carry it
/// too.
fn text_margin_px(el: &TextElement, root_pt: f64, dpi: f64) -> (f64, f64, f64, f64) {
    let margin = el
        .margin
        .or(text_concrete_defaults().margin)
        .expect("text margin default");
    let (mt, mr, mb, ml) = margin.resolve(root_pt);
    (
        pt_to_px(mt, dpi),
        pt_to_px(mr, dpi),
        pt_to_px(mb, dpi),
        pt_to_px(ml, dpi),
    )
}

/// Layout measurement for a facet strip. Reports the strip's
/// cross-panel thickness (row height for top / bottom strips, column
/// width for left / right strips) so the composition solver can
/// reserve room before the renderer paints the actual rect.
///
/// The thickness follows the label's line count, so a label too long
/// for the panel reserves every line [`draw_strip`] goes on to draw
/// instead of one.
pub(crate) struct StripMeasure {
    side: AxisSide,
    /// Shaped label, re-broken per query so the reserved thickness
    /// tracks the wrap the draw pass performs at the same width.
    /// Shaped through whichever pipeline the draw pass will use.
    run: StripRun,
    /// Text advance direction in degrees, resolved against the side's
    /// baseline.
    angle_deg: f32,
    /// Strip padding plus the text element's own margin, summed per
    /// edge, in px — everything between the strip rect and the text.
    inset_px: (f64, f64, f64, f64),
}

impl StripMeasure {
    /// Build a measure for `text` on `side`, consulting `theme` for
    /// strip text style and padding. Returns `None` when the theme's
    /// `strip_text` resolves to `Blank` for this side — the strip is
    /// text-driven, so suppressing text suppresses the whole strip
    /// (background included). Strips with a label but a blanked
    /// background still reserve the slot and draw the text.
    pub(crate) fn new(
        text: &str,
        side: AxisSide,
        theme: &Theme,
        dpi: f64,
        images: &std::sync::Arc<crate::image_registry::ImageRegistry>,
    ) -> Option<Self> {
        let text_el = resolved_text(theme, side)?;
        let root_pt = crate::plot::chrome::root_text_pt(theme);
        let (pt_top, pt_right, pt_bottom, pt_left) = strip_padding_px(theme, dpi);
        let (mt, mr, mb, ml) = text_margin_px(&text_el, root_pt, dpi);

        let style = text_style_from(&text_el, root_pt);
        let defaults = text_concrete_defaults();
        let run = if matches!(text_el.markdown, Some(true)) {
            let color = text_el
                .color
                .clone()
                .or_else(|| defaults.color.clone())
                .expect("text_concrete_defaults sets color");
            StripRun::Rich(crate::text::rich::RichTextRun::new_with_images(
                text,
                &style,
                color.resolve(&theme.palette),
                &theme.rich_text,
                &theme.palette,
                dpi,
                images,
            ))
        } else {
            StripRun::Plain(TextRun::new(text, &style, dpi))
        };
        let angle = text_el.angle.or(defaults.angle).expect("angle default");
        Some(Self {
            side,
            run,
            angle_deg: angle.resolve(baseline_deg(side)),
            inset_px: (pt_top + mt, pt_right + mr, pt_bottom + mb, pt_left + ml),
        })
    }

    /// Cross thickness when the strip's long axis spans `along_px`.
    /// `f64::INFINITY` reports the single-line thickness — the floor,
    /// since wrapping only ever adds lines.
    fn cross_px(&self, along_px: f64) -> f64 {
        let (top, right, bottom, left) = self.inset_px;
        let (along_inset, cross_inset) = if self.side.is_horizontal() {
            (left + right, top + bottom)
        } else {
            (top + bottom, left + right)
        };
        let interior = (along_px - along_inset).max(0.0);
        let wrap_px = self.wrap_width(interior);
        self.run.set_max_width(wrap_px);
        let (rotated_w, rotated_h) = rotated_bbox(
            self.run.content_width(),
            self.run.current_height(),
            self.angle_deg,
        );
        let text_dim_px = if self.side.is_horizontal() {
            rotated_h
        } else {
            rotated_w
        };
        text_dim_px + cross_inset
    }

    /// Extent the text has to break against, given `along_px` of
    /// interior along the strip's long axis. Text running across the
    /// strip gets `f64::INFINITY`: the only extent it could break
    /// against is the thickness this measure is deciding, so it lays
    /// out on one line and the thickness grows to fit it.
    fn wrap_width(&self, along_px: f64) -> f64 {
        let rel = ((self.angle_deg - baseline_deg(self.side)) as f64).to_radians();
        let cos = rel.cos().abs();
        if cos < 1e-3 {
            f64::INFINITY
        } else {
            along_px * cos
        }
    }

    /// True when the label carries a break opportunity. A single
    /// unbreakable cluster has a fixed thickness whatever the strip's
    /// length, which keeps it out of the solver's iteration loop.
    fn can_wrap(&self, dpi: f64) -> bool {
        match self.run.width_hint(dpi) {
            WidthHint::Min(w) => w < self.run.natural_width() - 0.5,
            WidthHint::NeedsHeight { .. } => true,
        }
    }
}

/// The label a [`StripMeasure`] reserves space for, shaped through the
/// same pipeline the draw pass uses so measured and drawn thickness
/// agree.
enum StripRun {
    /// Plain text.
    Plain(TextRun),
    /// Marquee-flavoured markdown.
    Rich(crate::text::rich::RichTextRun),
}

impl StripRun {
    fn set_max_width(&self, px: f64) {
        match self {
            StripRun::Plain(r) => {
                r.set_max_width(px as f32, HAlign::Start);
            }
            StripRun::Rich(r) => {
                r.set_max_width(px as f32, crate::style_vocab::HAlign::Start);
            }
        }
    }

    fn content_width(&self) -> f64 {
        match self {
            StripRun::Plain(r) => r.content_width(),
            StripRun::Rich(r) => r.content_width(),
        }
    }

    fn current_height(&self) -> f64 {
        match self {
            StripRun::Plain(r) => r.current_height(),
            StripRun::Rich(r) => r.current_height(),
        }
    }

    fn natural_width(&self) -> f64 {
        match self {
            StripRun::Plain(r) => r.natural_width(),
            StripRun::Rich(r) => r.natural_width(),
        }
    }

    fn width_hint(&self, dpi: f64) -> WidthHint {
        match self {
            StripRun::Plain(r) => r.width_hint(dpi),
            StripRun::Rich(r) => r.width_hint(dpi),
        }
    }
}

impl Measure for StripMeasure {
    fn width_hint(&self, dpi: f64) -> WidthHint {
        if !self.side.is_vertical() {
            return WidthHint::Min(0.0);
        }
        // A vertical strip's thickness depends on how many lines the
        // label breaks into along the panel edge — the iteration
        // protocol's case, seeded with the unwrapped thickness.
        let single_line = self.cross_px(f64::INFINITY);
        if self.can_wrap(dpi) {
            WidthHint::NeedsHeight { seed: single_line }
        } else {
            WidthHint::Min(single_line)
        }
    }

    fn height_at(&self, width: f64, _dpi: f64) -> f64 {
        if self.side.is_horizontal() {
            self.cross_px(width)
        } else {
            0.0
        }
    }

    fn width_at(&self, height: f64, _dpi: f64) -> f64 {
        if self.side.is_vertical() {
            self.cross_px(height)
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
#[allow(clippy::too_many_arguments)]
pub fn draw_strip(
    scene: &mut dyn SceneBuilder,
    text: &str,
    rect: Rect,
    side: AxisSide,
    theme: &Theme,
    dpi: f64,
    images: &std::sync::Arc<crate::image_registry::ImageRegistry>,
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
        scene.push_pick_scope(&part_scope(PlotPart::StripBackground));
        paint_strip_background(scene, el, path, theme, dpi);
        scene.pop_pick_scope();
    }

    let root_pt = crate::plot::chrome::root_text_pt(theme);
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
        let _ = run.set_max_width(f32::INFINITY, HAlign::Start);
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
    scene.push_pick_scope(&part_scope(PlotPart::StripLabel));
    draw_text_element_in_rect(
        scene,
        text,
        &concrete,
        interior,
        &theme.palette,
        root_pt,
        dpi,
        PickId::Skip,
        Some(&theme.rich_text),
        images,
    );
    if clipping.is_some() {
        scene.pop_layer();
    }
    scene.pop_pick_scope();
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

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plot::theme::{pt, Margin, Sided};
    use crate::scene::recording::{Op, RecordingScene};

    const DPI: f64 = 96.0;
    /// Wider than any panel the tests hand it, so it wraps.
    const LONG: &str = "Sepal width in (2.5, 5.0] millimetres of observed range";

    fn measure(text: &str, side: AxisSide, theme: &Theme) -> StripMeasure {
        StripMeasure::new(text, side, theme, DPI, &crate::image_registry::no_images())
            .expect("default theme draws strip text")
    }

    /// Default theme with the strip padding dropped, so a thickness
    /// reads as a plain multiple of the line height.
    fn unpadded_theme() -> Theme {
        Theme {
            strip_padding: Margin::ZERO,
            ..Theme::default()
        }
    }

    /// Screen-space glyph origins: the recorded positions pushed
    /// through each run's own transform, so a rotated strip reports
    /// where its label actually landed on the canvas.
    fn glyph_points(scene: &RecordingScene) -> Vec<crate::geometry::Point> {
        let mut out = Vec::new();
        for op in &scene.ops {
            if let Op::DrawGlyphs(run) = op {
                for g in &run.glyphs {
                    out.push(run.transform * crate::geometry::Point::new(g.x as f64, g.y as f64));
                }
            }
        }
        out
    }

    /// Distinct glyph baselines in draw order — one per line the draw
    /// pass emitted. Positions are pre-transform, so a rotated strip
    /// reports its lines the same way an upright one does.
    fn baselines(scene: &RecordingScene) -> Vec<f32> {
        let mut out: Vec<f32> = Vec::new();
        for op in &scene.ops {
            if let Op::DrawGlyphs(run) = op {
                for g in &run.glyphs {
                    let y = (g.y * 10.0).round() / 10.0;
                    if !out.contains(&y) {
                        out.push(y);
                    }
                }
            }
        }
        out
    }

    #[test]
    fn top_strip_thickness_follows_the_line_count() {
        let theme = unpadded_theme();
        let m = measure(LONG, AxisSide::Top, &theme);
        let one_line = m.height_at(2000.0, DPI);
        let wrapped = m.height_at(220.0, DPI);
        assert!(
            wrapped >= 2.0 * one_line - 0.5,
            "a label wrapping to several lines must reserve them: \
             one_line={one_line}, wrapped={wrapped}"
        );
    }

    #[test]
    fn top_strip_reserves_every_line_the_draw_pass_emits() {
        let theme = Theme::default();
        let m = measure(LONG, AxisSide::Top, &theme);
        let width = 220.0;
        let rect = Rect::new(0.0, 0.0, width, m.height_at(width, DPI));
        let mut scene = RecordingScene::default();
        draw_strip(
            &mut scene,
            LONG,
            rect,
            AxisSide::Top,
            &theme,
            DPI,
            &crate::image_registry::no_images(),
        );
        let lines = baselines(&scene);
        assert!(
            lines.len() > 1,
            "expected the label to wrap at {width}px — got {} line(s)",
            lines.len()
        );
        for y in &lines {
            assert!(
                (*y as f64) >= rect.y0 && (*y as f64) <= rect.y1,
                "line baseline {y} falls outside the reserved strip {rect:?}"
            );
        }
    }

    #[test]
    fn vertical_strip_wraps_along_the_panel_edge() {
        let theme = Theme::default();
        let m = measure("Two words", AxisSide::Right, &theme);
        let height = 400.0;
        let rect = Rect::new(0.0, 0.0, m.width_at(height, DPI), height);
        let mut scene = RecordingScene::default();
        draw_strip(
            &mut scene,
            "Two words",
            rect,
            AxisSide::Right,
            &theme,
            DPI,
            &crate::image_registry::no_images(),
        );
        assert_eq!(
            baselines(&scene).len(),
            1,
            "a label that fits the panel edge must not break across the \
             strip's thickness"
        );
    }

    #[test]
    fn vertical_strip_label_centres_along_the_panel_edge() {
        let theme = Theme::default();
        let text = "Adelie";
        let height = 420.0;
        for side in [AxisSide::Left, AxisSide::Right] {
            let m = measure(text, side, &theme);
            let rect = Rect::new(0.0, 0.0, m.width_at(height, DPI), height);
            let mut scene = RecordingScene::default();
            draw_strip(
                &mut scene,
                text,
                rect,
                side,
                &theme,
                DPI,
                &crate::image_registry::no_images(),
            );
            let ys: Vec<f64> = glyph_points(&scene).iter().map(|p| p.y).collect();
            let lo = ys.iter().cloned().fold(f64::INFINITY, f64::min);
            let hi = ys.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
            // Glyph origins stop at the last glyph's own start, so the
            // span they cover runs one advance shy of the label's ink.
            let mid = (lo + hi) * 0.5;
            assert!(
                (mid - rect.center().y).abs() < 8.0,
                "{side:?} strip label must sit at the centre of the panel \
                 edge, not one end of it: mid={mid}, centre={}",
                rect.center().y
            );
        }
    }

    #[test]
    fn rotated_strip_valign_moves_across_the_thickness() {
        use crate::plot::theme::VAlign;
        // A rotated label's alignment travels with the text: `valign`
        // stacks it across the strip's thickness, leaving its position
        // along the panel edge — `align`'s axis — untouched.
        let base = Theme::default()
            .strip_text
            .resolve(0, 0)
            .cloned()
            .expect("strip text");
        let themed = |valign| Theme {
            strip_text: Sided::new(TextElement {
                valign: Some(valign),
                ..base.clone()
            }),
            ..Theme::default()
        };
        let text = "Adelie";
        let height = 420.0;
        // Slack across the thickness, so the two valigns have somewhere
        // to differ.
        let slack = 40.0;
        let centroid = |valign| {
            let theme = themed(valign);
            let m = measure(text, AxisSide::Right, &theme);
            let rect = Rect::new(0.0, 0.0, m.width_at(height, DPI) + slack, height);
            let mut scene = RecordingScene::default();
            draw_strip(
                &mut scene,
                text,
                rect,
                AxisSide::Right,
                &theme,
                DPI,
                &crate::image_registry::no_images(),
            );
            let pts = glyph_points(&scene);
            let n = pts.len() as f64;
            (
                pts.iter().map(|p| p.x).sum::<f64>() / n,
                pts.iter().map(|p| p.y).sum::<f64>() / n,
            )
        };
        let (top_x, top_y) = centroid(VAlign::Top);
        let (bottom_x, bottom_y) = centroid(VAlign::Bottom);
        assert!(
            (top_y - bottom_y).abs() < 0.5,
            "valign must not move a rotated label along the panel edge: \
             top={top_y}, bottom={bottom_y}"
        );
        // Text rotated +90° has its local +y pointing at screen -x, so
        // the text's own top is the strip's outer edge.
        assert!(
            top_x - bottom_x > slack * 0.5,
            "valign must move a rotated label across the strip's \
             thickness: top={top_x}, bottom={bottom_x}"
        );
    }

    #[test]
    fn vertical_strip_thickness_follows_the_line_count() {
        let theme = unpadded_theme();
        let m = measure(LONG, AxisSide::Left, &theme);
        let tall = m.width_at(2000.0, DPI);
        let short = m.width_at(220.0, DPI);
        assert!(
            short >= 2.0 * tall - 0.5,
            "a vertical strip wraps against the panel's height: \
             tall={tall}, short={short}"
        );
    }

    #[test]
    fn unbreakable_label_keeps_a_stable_thickness() {
        let theme = Theme::default();
        let m = measure("Setosa", AxisSide::Left, &theme);
        // No break opportunity — the thickness can't depend on the
        // strip's length, so the cell stays out of the iteration loop.
        assert!(matches!(m.width_hint(DPI), WidthHint::Min(_)));
        let multi = measure(LONG, AxisSide::Left, &theme);
        assert!(matches!(
            multi.width_hint(DPI),
            WidthHint::NeedsHeight { .. }
        ));
    }

    #[test]
    fn strip_text_margin_thickens_the_slot() {
        let plain = Theme::default();
        let with_margin = Theme {
            strip_text: Sided::new(TextElement {
                margin: Some(Margin::all(pt(4.0))),
                ..plain.strip_text.resolve(0, 0).cloned().expect("strip text")
            }),
            ..Theme::default()
        };
        let bare = measure("Setosa", AxisSide::Top, &plain).height_at(400.0, DPI);
        let padded = measure("Setosa", AxisSide::Top, &with_margin).height_at(400.0, DPI);
        let expected = pt_to_px(8.0, DPI);
        assert!(
            (padded - bare - expected).abs() < 0.5,
            "top + bottom text margin belongs in the reserved thickness: \
             bare={bare}, padded={padded}, margin={expected}"
        );
    }
}
