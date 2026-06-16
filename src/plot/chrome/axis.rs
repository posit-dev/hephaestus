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

use crate::geometry::{Point, Rect};
use crate::layout::{Measure, WidthHint};
use crate::plot::chrome::linear_axis::{
    draw_linear_axis_at, pt_to_px, AxisChromeStyle, LABEL_FONT_SIZE_PT, LABEL_GAP_PT,
    TICK_LENGTH_PT,
};
use crate::plot::scale::Scale;
use crate::plot::theme::Theme;
use crate::scales::breaks::DEFAULT_BREAK_COUNT;
use crate::scales::value::Value;
use crate::scene::SceneBuilder;
use crate::text::{Alignment, TextRun, TextStyle};
use kurbo::Shape;

use crate::scales::chrome::AxisSide;

// ─── Axis spec (high-level Plot surface) ─────────────────────────────────────

/// Stable identifier returned by [`crate::plot::Plot::add_axis`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AxisId(pub u32);

/// One axis attached to a [`Plot`](crate::plot::Plot). Built
/// manually by the caller — `Plot` doesn't infer any default axes
/// from its channel bindings.
///
/// Both the rail (`scale_name`) and the `title` are optional. A
/// title-only axis (rail = `None`, title = `Some`) reserves the
/// title slot for an axis whose rail has been suppressed.
#[derive(Clone, Debug)]
pub struct Axis {
    /// Scale that supplies the breaks + map for the rail. `None`
    /// means "no rail" — the title (if any) still renders.
    pub scale_name: Option<String>,
    pub placement: AxisPlacement,
    pub title: Option<String>,
}

/// Where an axis sits relative to its plot.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AxisPlacement {
    /// Standard rectilinear axis along one of the panel's four
    /// edges. Wired into the corresponding patch slot via
    /// [`Scale::axis_measure`] + [`Scale::draw_axis`].
    Cartesian(AxisSide),
    /// Radius axis along a spoke at `theta_frac ∈ [0, 1]` on a
    /// polar projection. The spoke direction is computed via the
    /// projection's `unit_position` so it follows the polygon edge
    /// on chord-style projections.
    PolarRadius { theta_frac: f64 },
    /// Angular axis along the projection's outer or inner ring.
    /// The inner variant is silently skipped when the projection
    /// has `inner_radius_frac == 0` (no hole, no inner ring), so a
    /// single axis definition works for both disk and ring layouts.
    PolarAngular(PolarRing),
}

/// Which ring an angular axis runs along.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PolarRing {
    Outer,
    Inner,
}

impl Axis {
    /// Axis with a rail driven by `scale_name`. Add a title via
    /// [`Self::title`].
    pub fn rail(scale_name: impl Into<String>, placement: AxisPlacement) -> Self {
        Self {
            scale_name: Some(scale_name.into()),
            placement,
            title: None,
        }
    }

    /// Title-only axis — reserves the matching title slot
    /// (cartesian) or renders an inline title (polar) without
    /// drawing a rail. Use when the rail would be visual noise but
    /// the slot still needs a label.
    pub fn title_only(title: impl Into<String>, placement: AxisPlacement) -> Self {
        Self {
            scale_name: None,
            placement,
            title: Some(title.into()),
        }
    }

    /// Attach a title to an existing axis. Cartesian axes draw the
    /// title into the matching patch
    /// [`Slot::AxisLeftTitle`](crate::composition::Slot) /
    /// `AxisBottomTitle` / etc.; polar axes render it inline.
    pub fn title(mut self, t: impl Into<String>) -> Self {
        self.title = Some(t.into());
        self
    }
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
    fn new(scale: &Scale, side: AxisSide, _dpi: f64) -> Self {
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
            // Tick labels render unwrapped — `natural_width` is the
            // actual draw width. `width_hint` returns the longest-
            // unbreakable-cluster bound (one word), which undershoots
            // multi-word labels and clips them at draw.
            let w = run.natural_width();
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
        theme: &Theme,
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

        // Translate the side + slot/panel layout into the linear-axis
        // primitives: baseline endpoints and the perpendicular tick
        // direction. `frac=0` corresponds to `start`; `frac=1` to
        // `end`. y axes flip so `frac=0` is at the panel BOTTOM.
        let (start, end, tick_direction) = match side {
            AxisSide::Bottom => (
                Point::new(panel_rect.x0, slot_rect.y0),
                Point::new(panel_rect.x1, slot_rect.y0),
                (0.0, 1.0),
            ),
            AxisSide::Top => (
                Point::new(panel_rect.x0, slot_rect.y1),
                Point::new(panel_rect.x1, slot_rect.y1),
                (0.0, -1.0),
            ),
            AxisSide::Left => (
                Point::new(slot_rect.x1, panel_rect.y1),
                Point::new(slot_rect.x1, panel_rect.y0),
                (-1.0, 0.0),
            ),
            AxisSide::Right => (
                Point::new(slot_rect.x0, panel_rect.y1),
                Point::new(slot_rect.x0, panel_rect.y0),
                (1.0, 0.0),
            ),
        };

        let majors: Vec<(f64, String)> = breaks
            .iter()
            .filter(|v| !matches!(v, Value::Null))
            .filter_map(|v| self.map(v).as_number().map(|f| (f, self.format(v))))
            .filter(|(f, _)| f.is_finite())
            .collect();
        let minors: Vec<f64> = self
            .minor_breaks(DEFAULT_BREAK_COUNT)
            .into_iter()
            .filter(|v| !matches!(v, Value::Null))
            .filter_map(|v| self.map(&v).as_number())
            .filter(|f| f.is_finite())
            .collect();

        // Resolve the (channel, side) axis from the theme. Channel
        // is determined by which axis side this is — Bottom/Top
        // belong to channel 0 (x), Left/Right to channel 1 (y).
        // Side 0 / 1 within the channel selects between the two
        // possible axes for that channel.
        let (ch, side_idx) = axis_side_to_channel_side(side);
        let resolved = theme.axis.resolve(ch, side_idx);
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
    }
}

/// Map an `AxisSide` to the projection-agnostic `(channel, side)`
/// indices the theme stores axis chrome under.
///
/// - `Bottom` → (0, 0): x-axis, primary side.
/// - `Top`    → (0, 1): x-axis, secondary side.
/// - `Left`   → (1, 0): y-axis, primary side.
/// - `Right`  → (1, 1): y-axis, secondary side.
pub(crate) fn axis_side_to_channel_side(side: AxisSide) -> (u8, u8) {
    match side {
        AxisSide::Bottom => (0, 0),
        AxisSide::Top => (0, 1),
        AxisSide::Left => (1, 0),
        AxisSide::Right => (1, 1),
    }
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
        s.draw_axis(
            &mut scene,
            slot,
            panel,
            AxisSide::Bottom,
            dpi_96(),
            &Theme::default(),
        );

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
        s.draw_axis(
            &mut scene,
            slot,
            panel,
            AxisSide::Left,
            dpi_96(),
            &Theme::default(),
        );

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
        s.draw_axis(
            &mut scene,
            slot,
            panel,
            AxisSide::Bottom,
            dpi_96(),
            &Theme::default(),
        );
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
        s.draw_axis(
            &mut scene,
            slot,
            panel,
            AxisSide::Bottom,
            dpi_96(),
            &Theme::default(),
        );
        assert!(scene.ops.is_empty());
    }
}
