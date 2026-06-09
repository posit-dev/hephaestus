//! Axis rendering. Pre-shapes tick labels via [`crate::text::TextRun`],
//! reports their dimensions as a [`Measure`] for the composition solver,
//! and strokes the tick marks + labels at draw time.
//!
//! Gated behind `feature = "text"` — axes need a shaper to size and draw
//! labels. Without the feature the [`AxisSide`] enum still exists (in
//! [`chrome`](super::chrome)) but [`Scale::axis_measure`] /
//! [`Scale::draw_axis`] are unavailable.
//!
//! Conventions:
//!
//! - All tick labels are horizontal (no rotation). Wide labels on a
//!   vertical axis grow the axis chrome column.
//! - Tick mark length, label gap, and font size are fixed pt constants
//!   defined below; not per-scale themable.
//! - Tick labels use the scale's own [`Scale::format`], which renders
//!   numeric values via `{n}` Display and temporal values in calendar
//!   form (YYYY-MM-DD etc.).

use crate::brush::Brush;
use crate::color::{rgb, Color};
use crate::geometry::{Affine, Point, Rect};
use crate::layout::{Measure, WidthHint};
use crate::path::Path;
use crate::pick::PickId;
use crate::plot::scale::Scale;
use crate::scales::breaks::DEFAULT_BREAK_COUNT;
use crate::scales::value::Value;
use crate::scene::SceneBuilder;
use crate::stroke::Stroke;
use crate::text::{draw_text, Alignment, TextRun, TextStyle};
use kurbo::Shape;

use crate::scales::chrome::AxisSide;

// ─── Style constants (pt) ────────────────────────────────────────────────────

/// Tick mark length, pt. Strokes from the panel edge outward into the
/// axis chrome.
const TICK_LENGTH_PT: f64 = 4.0;
/// Minor tick length, pt. Shorter than major; unlabelled.
const MINOR_TICK_LENGTH_PT: f64 = 2.0;
/// Gap between the tick label and the tick mark / panel edge, pt.
const LABEL_GAP_PT: f64 = 2.0;
/// Tick label font size, pt.
const LABEL_FONT_SIZE_PT: f32 = 10.0;
/// Axis baseline + tick stroke width, pt.
const STROKE_WIDTH_PT: f64 = 1.0;
/// Black ink for axis chrome. Not currently themable.
fn axis_ink() -> Color {
    rgb(0.0, 0.0, 0.0)
}

// ─── pt → px ─────────────────────────────────────────────────────────────────

fn pt_to_px(pt: f64, dpi: f64) -> f64 {
    pt * dpi / 72.0
}

// ─── AxisMeasure ─────────────────────────────────────────────────────────────

/// Snapshot of an axis's chrome footprint. Holds shaped TextRuns for each
/// tick label so the layout solver can size the chrome track without
/// reshaping per query. The same labels are reshaped at draw time —
/// shaping is cheap, but a future per-channel cache (`Scale::generation`
/// hook) will eliminate the duplicate work.
pub(crate) struct AxisMeasure {
    side: AxisSide,
    /// (max label width across all ticks, in px at the dpi captured at
    /// construction). Computed once.
    max_label_w_px: f64,
    /// (max label height across all ticks, in px at the dpi captured at
    /// construction). Computed once.
    max_label_h_px: f64,
}

impl AxisMeasure {
    fn new(scale: &Scale, side: AxisSide, dpi: f64) -> Self {
        let style = TextStyle::new(LABEL_FONT_SIZE_PT);
        let breaks = scale.breaks(DEFAULT_BREAK_COUNT);
        let mut max_w: f64 = 0.0;
        let mut max_h: f64 = 0.0;
        for v in &breaks {
            if matches!(v, Value::Null) {
                continue;
            }
            let label = scale.format(v);
            let run = TextRun::new(&label, &style);
            // Lay out unconstrained to get the natural single-line width.
            let h = run.set_max_width(f32::INFINITY, Alignment::Start) as f64;
            // Width: ask for the min width (= longest unbreakable
            // cluster), then for a single-line label that's effectively
            // the label's full natural width.
            let w = match run.width_hint(dpi) {
                WidthHint::Min(w) => w,
                WidthHint::NeedsHeight { seed } => seed,
            };
            max_w = max_w.max(w);
            max_h = max_h.max(h);
        }
        AxisMeasure {
            side,
            max_label_w_px: max_w,
            max_label_h_px: max_h,
        }
    }

    /// Total px contribution along the axis's perpendicular direction.
    /// For Left/Right axes this is the column width; for Bottom/Top it's
    /// the row height.
    fn chrome_thickness_px(&self, dpi: f64) -> f64 {
        let tick = pt_to_px(TICK_LENGTH_PT, dpi);
        let gap = pt_to_px(LABEL_GAP_PT, dpi);
        let label_dim = if self.side.is_vertical() {
            self.max_label_w_px
        } else {
            self.max_label_h_px
        };
        tick + gap + label_dim
    }
}

impl Measure for AxisMeasure {
    fn width_hint(&self, dpi: f64) -> WidthHint {
        if self.side.is_vertical() {
            WidthHint::Min(self.chrome_thickness_px(dpi))
        } else {
            // Horizontal axes don't constrain column width.
            WidthHint::Min(0.0)
        }
    }

    fn height_at(&self, _width: f64, dpi: f64) -> f64 {
        if self.side.is_horizontal() {
            self.chrome_thickness_px(dpi)
        } else {
            // Vertical axes inherit panel height; their cell doesn't
            // contribute to row sizing.
            0.0
        }
    }
}

// ─── Scale::axis_measure + draw_axis ─────────────────────────────────────────

impl Scale {
    /// Pre-shape this scale's tick labels into a [`Measure`] cell suitable
    /// for dropping into a [`composition::Patch`](crate::composition::Patch)
    /// slot. The cell's dimensions reflect the scale's *current* state at
    /// call time; mutate the scale and call again to refresh.
    pub fn axis_measure(&self, side: AxisSide, dpi: f64) -> Box<dyn Measure> {
        Box::new(AxisMeasure::new(self, side, dpi))
    }

    /// Stroke tick marks and draw tick labels into `slot_rect`, mapping
    /// the scale's breaks into panel-space pixels via `panel_rect`.
    ///
    /// Conventions:
    /// - For position scales (no output range), `scale.map(break_value)`
    ///   returns a `Value::Number` in `[0, 1]`; the geom-side convention
    ///   `px = panel.x0 + frac * panel_w` (and `panel.y1 - frac * panel_h`
    ///   for y, to flip pixels-grow-downward) applies here too.
    /// - Tick marks stroke outward from the panel edge into `slot_rect`.
    /// - Labels are anchored toward `slot_rect`'s far edge from the panel.
    ///
    /// Silently does nothing if the scale has no input range, has empty
    /// breaks, or the slot rect is degenerate.
    pub fn draw_axis(
        &self,
        scene: &mut dyn SceneBuilder,
        slot_rect: Rect,
        panel_rect: Rect,
        side: AxisSide,
        dpi: f64,
    ) {
        let breaks = self.breaks(DEFAULT_BREAK_COUNT);
        if breaks.is_empty() {
            return;
        }
        let panel_w = panel_rect.x1 - panel_rect.x0;
        let panel_h = panel_rect.y1 - panel_rect.y0;
        if panel_w <= 0.0 || panel_h <= 0.0 {
            return;
        }

        let tick_px = pt_to_px(TICK_LENGTH_PT, dpi);
        let gap_px = pt_to_px(LABEL_GAP_PT, dpi);
        let stroke_px = pt_to_px(STROKE_WIDTH_PT, dpi);
        let brush = Brush::Solid(axis_ink());
        let stroke = Stroke::new(stroke_px);
        let style = TextStyle::new(LABEL_FONT_SIZE_PT);

        // Axis baseline along the panel-adjacent edge of the slot.
        let baseline = baseline_path(side, slot_rect, panel_rect);
        scene.stroke(
            &stroke,
            Affine::IDENTITY,
            &brush,
            None,
            &baseline,
            PickId::Skip,
        );

        // Minor ticks — short, unlabelled. Drawn before majors so the
        // major tick line draws on top if they happen to coincide.
        let minor_tick_px = pt_to_px(MINOR_TICK_LENGTH_PT, dpi);
        for minor_val in self.minor_breaks(DEFAULT_BREAK_COUNT) {
            if matches!(minor_val, Value::Null) {
                continue;
            }
            let frac = match self.map(&minor_val).as_number() {
                Some(f) if f.is_finite() && (0.0..=1.0).contains(&f) => f,
                _ => continue,
            };
            let (p0, p1) = match side {
                AxisSide::Bottom => {
                    let x = panel_rect.x0 + frac * panel_w;
                    (
                        Point::new(x, slot_rect.y0),
                        Point::new(x, slot_rect.y0 + minor_tick_px),
                    )
                }
                AxisSide::Top => {
                    let x = panel_rect.x0 + frac * panel_w;
                    (
                        Point::new(x, slot_rect.y1),
                        Point::new(x, slot_rect.y1 - minor_tick_px),
                    )
                }
                AxisSide::Left => {
                    let y = panel_rect.y1 - frac * panel_h;
                    (
                        Point::new(slot_rect.x1, y),
                        Point::new(slot_rect.x1 - minor_tick_px, y),
                    )
                }
                AxisSide::Right => {
                    let y = panel_rect.y1 - frac * panel_h;
                    (
                        Point::new(slot_rect.x0, y),
                        Point::new(slot_rect.x0 + minor_tick_px, y),
                    )
                }
            };
            stroke_line(scene, &stroke, &brush, p0, p1);
        }

        for break_val in &breaks {
            if matches!(break_val, Value::Null) {
                continue;
            }
            let frac = match self.map(break_val).as_number() {
                Some(f) if f.is_finite() => f,
                _ => continue,
            };

            let label = self.format(break_val);
            let run = TextRun::new(&label, &style);
            // Lay out unconstrained to get the single-line glyph runs at
            // their natural width.
            let label_h = run.set_max_width(f32::INFINITY, Alignment::Start) as f64;
            let label_w = match run.width_hint(dpi) {
                WidthHint::Min(w) => w,
                WidthHint::NeedsHeight { seed } => seed,
            };

            match side {
                AxisSide::Bottom => {
                    let x = panel_rect.x0 + frac * panel_w;
                    // Tick stroke: from panel edge (top of slot) down to tick_px.
                    let p0 = Point::new(x, slot_rect.y0);
                    let p1 = Point::new(x, slot_rect.y0 + tick_px);
                    stroke_line(scene, &stroke, &brush, p0, p1);
                    // Label: centred horizontally on the tick, top edge
                    // gap_px below the tick.
                    let label_x = x - label_w / 2.0;
                    let label_y = slot_rect.y0 + tick_px + gap_px;
                    draw_text(
                        scene,
                        &run,
                        label_x,
                        label_y,
                        &brush,
                        Affine::IDENTITY,
                        PickId::Skip,
                    );
                }
                AxisSide::Top => {
                    let x = panel_rect.x0 + frac * panel_w;
                    let p0 = Point::new(x, slot_rect.y1);
                    let p1 = Point::new(x, slot_rect.y1 - tick_px);
                    stroke_line(scene, &stroke, &brush, p0, p1);
                    // Label sits above the tick: its bottom edge is
                    // gap_px above the tick top (which is at y1 - tick).
                    let label_x = x - label_w / 2.0;
                    let label_y = slot_rect.y1 - tick_px - gap_px - label_h;
                    draw_text(
                        scene,
                        &run,
                        label_x,
                        label_y,
                        &brush,
                        Affine::IDENTITY,
                        PickId::Skip,
                    );
                }
                AxisSide::Left => {
                    // Y axes flip: frac=0 is at the bottom of the panel.
                    let y = panel_rect.y1 - frac * panel_h;
                    let p0 = Point::new(slot_rect.x1, y);
                    let p1 = Point::new(slot_rect.x1 - tick_px, y);
                    stroke_line(scene, &stroke, &brush, p0, p1);
                    // Label: right-aligned at slot_rect.x1 - tick - gap;
                    // vertically centred on the tick.
                    let label_x = slot_rect.x1 - tick_px - gap_px - label_w;
                    let label_y = y - label_h / 2.0;
                    draw_text(
                        scene,
                        &run,
                        label_x,
                        label_y,
                        &brush,
                        Affine::IDENTITY,
                        PickId::Skip,
                    );
                }
                AxisSide::Right => {
                    let y = panel_rect.y1 - frac * panel_h;
                    let p0 = Point::new(slot_rect.x0, y);
                    let p1 = Point::new(slot_rect.x0 + tick_px, y);
                    stroke_line(scene, &stroke, &brush, p0, p1);
                    let label_x = slot_rect.x0 + tick_px + gap_px;
                    let label_y = y - label_h / 2.0;
                    draw_text(
                        scene,
                        &run,
                        label_x,
                        label_y,
                        &brush,
                        Affine::IDENTITY,
                        PickId::Skip,
                    );
                }
            }
        }
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Path along the panel-adjacent edge of the axis slot.
fn baseline_path(side: AxisSide, slot: Rect, panel: Rect) -> Path {
    let (p0, p1) = match side {
        AxisSide::Bottom => (Point::new(panel.x0, slot.y0), Point::new(panel.x1, slot.y0)),
        AxisSide::Top => (Point::new(panel.x0, slot.y1), Point::new(panel.x1, slot.y1)),
        AxisSide::Left => (Point::new(slot.x1, panel.y0), Point::new(slot.x1, panel.y1)),
        AxisSide::Right => (Point::new(slot.x0, panel.y0), Point::new(slot.x0, panel.y1)),
    };
    line_path(p0, p1)
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

// Make sure `kurbo::Shape` is in scope for any future `to_path` calls on
// Rect (matches the layout module's conventions).
#[allow(dead_code)]
fn _shape_in_scope() {
    let _ = Rect::new(0.0, 0.0, 1.0, 1.0).to_path(0.1);
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plot::scale;
    use crate::scales::value::Value;
    use crate::scene::recording::{Op, RecordingScene};

    fn dpi_96() -> f64 {
        96.0
    }

    fn panel_400_300() -> Rect {
        Rect::new(50.0, 20.0, 450.0, 320.0)
    }

    // ── axis_measure ──

    #[test]
    fn bottom_axis_measure_reports_chrome_height() {
        let s = scale::continuous(0.0..=100.0);
        let m = s.axis_measure(AxisSide::Bottom, dpi_96());
        // Bottom axis: width contribution = 0, height = tick + gap + label_h.
        assert_eq!(m.width_hint(dpi_96()), WidthHint::Min(0.0));
        let h = m.height_at(400.0, dpi_96());
        assert!(h > 0.0, "axis height should be positive");
        // Sanity: at least the tick length in px.
        assert!(h >= pt_to_px(TICK_LENGTH_PT, dpi_96()) - 0.5);
    }

    #[test]
    fn left_axis_measure_reports_chrome_width() {
        let s = scale::continuous(0.0..=100.0);
        let m = s.axis_measure(AxisSide::Left, dpi_96());
        let w = match m.width_hint(dpi_96()) {
            WidthHint::Min(w) => w,
            WidthHint::NeedsHeight { seed } => seed,
        };
        assert!(w > 0.0, "axis width should be positive");
        // Left axis row contribution is zero (height is panel-driven).
        assert_eq!(m.height_at(w, dpi_96()), 0.0);
    }

    #[test]
    fn axis_chrome_grows_with_longer_labels() {
        // A scale whose labels are wider (8-digit numbers) should
        // produce a wider Left axis.
        let s_short = scale::continuous(0.0..=10.0);
        let s_long = scale::continuous(0.0..=100_000_000.0);
        let m_short = s_short.axis_measure(AxisSide::Left, dpi_96());
        let m_long = s_long.axis_measure(AxisSide::Left, dpi_96());
        let w_short = match m_short.width_hint(dpi_96()) {
            WidthHint::Min(w) => w,
            WidthHint::NeedsHeight { seed } => seed,
        };
        let w_long = match m_long.width_hint(dpi_96()) {
            WidthHint::Min(w) => w,
            WidthHint::NeedsHeight { seed } => seed,
        };
        assert!(
            w_long > w_short,
            "longer labels should grow the axis: short={w_short}, long={w_long}"
        );
    }

    // ── draw_axis op-emission ──

    fn count_strokes_and_glyph_runs(scene: &RecordingScene) -> (usize, usize) {
        let mut strokes = 0usize;
        let mut glyphs = 0usize;
        for op in &scene.ops {
            match op {
                Op::Stroke { .. } => strokes += 1,
                Op::DrawGlyphs(_) => glyphs += 1,
                _ => {}
            }
        }
        (strokes, glyphs)
    }

    #[test]
    fn bottom_axis_draws_baseline_and_ticks() {
        let s = scale::continuous(0.0..=10.0);
        let panel = panel_400_300();
        // Place the slot directly below the panel — height = chrome.
        let m = s.axis_measure(AxisSide::Bottom, dpi_96());
        let chrome_h = m.height_at(panel.x1 - panel.x0, dpi_96());
        let slot = Rect::new(panel.x0, panel.y1, panel.x1, panel.y1 + chrome_h);

        let mut scene = RecordingScene::default();
        s.draw_axis(&mut scene, slot, panel, AxisSide::Bottom, dpi_96());

        let (strokes, glyphs) = count_strokes_and_glyph_runs(&scene);
        let breaks = s.breaks(DEFAULT_BREAK_COUNT);
        let minors = s.minor_breaks(DEFAULT_BREAK_COUNT);
        let n_majors = breaks.iter().filter(|v| !matches!(v, Value::Null)).count();
        let n_minors = minors.iter().filter(|v| !matches!(v, Value::Null)).count();
        // 1 baseline + 1 stroke per major + 1 per minor.
        let expected_strokes = 1 + n_majors + n_minors;
        assert_eq!(
            strokes, expected_strokes,
            "expected {expected_strokes} strokes (1 baseline + {n_majors} majors + {n_minors} minors); got {strokes}"
        );
        assert!(
            glyphs >= n_majors,
            "expected at least one glyph-run per major label"
        );
    }

    #[test]
    fn left_axis_draws_baseline_and_ticks() {
        let s = scale::continuous(0.0..=1.0);
        let panel = panel_400_300();
        let m = s.axis_measure(AxisSide::Left, dpi_96());
        let chrome_w = match m.width_hint(dpi_96()) {
            WidthHint::Min(w) => w,
            WidthHint::NeedsHeight { seed } => seed,
        };
        let slot = Rect::new(panel.x0 - chrome_w, panel.y0, panel.x0, panel.y1);

        let mut scene = RecordingScene::default();
        s.draw_axis(&mut scene, slot, panel, AxisSide::Left, dpi_96());

        let (strokes, glyphs) = count_strokes_and_glyph_runs(&scene);
        let breaks = s.breaks(DEFAULT_BREAK_COUNT);
        let minors = s.minor_breaks(DEFAULT_BREAK_COUNT);
        let n_majors = breaks.iter().filter(|v| !matches!(v, Value::Null)).count();
        let n_minors = minors.iter().filter(|v| !matches!(v, Value::Null)).count();
        let expected_strokes = 1 + n_majors + n_minors;
        assert_eq!(strokes, expected_strokes);
        assert!(glyphs >= n_majors);
    }

    #[test]
    fn draw_axis_with_no_breaks_is_silent() {
        // Identity scale produces no breaks → no draws.
        let s = scale::identity();
        let panel = panel_400_300();
        let slot = Rect::new(panel.x0, panel.y1, panel.x1, panel.y1 + 30.0);

        let mut scene = RecordingScene::default();
        s.draw_axis(&mut scene, slot, panel, AxisSide::Bottom, dpi_96());
        assert!(
            scene.ops.is_empty(),
            "expected no ops for empty breaks; got {}",
            scene.ops.len()
        );
    }

    #[test]
    fn draw_axis_with_degenerate_panel_is_silent() {
        let s = scale::continuous(0.0..=10.0);
        // Zero-area panel.
        let panel = Rect::new(0.0, 0.0, 0.0, 0.0);
        let slot = Rect::new(0.0, 0.0, 10.0, 10.0);
        let mut scene = RecordingScene::default();
        s.draw_axis(&mut scene, slot, panel, AxisSide::Bottom, dpi_96());
        assert!(scene.ops.is_empty());
    }
}
