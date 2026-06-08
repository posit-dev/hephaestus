//! **Scaffolding** text shaping/layout backed by [`parley`].
//!
//! Gated behind the `text` cargo feature. Intended to be temporary — the host
//! crate is expected to bring its own shaper. While this module exists it
//! provides:
//!
//! - [`TextStyle`] — a minimal font/size/weight/italic style descriptor.
//! - [`TextRun`] — a shaped string + cached parley layout that implements
//!   [`crate::layout::Measure`], so it drops into a
//!   [`crate::composition::Patch`] slot directly.
//! - [`draw_text`] — bridge from a positioned [`TextRun`] to
//!   [`crate::scene::SceneBuilder::draw_glyphs`].
//!
//! Font discovery uses parley's [`FontContext::new()`] which enumerates
//! system fonts; on machines without common families the layout still works
//! but the rendered glyphs depend on what fontique finds.

use std::cell::RefCell;
use std::sync::{Mutex, OnceLock};

use parley::{
    AlignmentOptions, FontContext, FontFamily, FontFamilyName, FontStyle, FontWeight,
    GenericFamily, LayoutContext, PositionedLayoutItem, StyleProperty,
};

/// Line justification within the text box. Re-exported from parley so
/// downstream geoms can construct one without depending on parley directly.
///
/// Geom-facing string aliases (used by the `justify_x` channel parser):
/// `"start"` → [`Alignment::Start`], `"center"` → [`Alignment::Center`],
/// `"end"` → [`Alignment::End`], `"justify"` → [`Alignment::Justify`].
pub use parley::Alignment;

use crate::brush::Brush;
use crate::geometry::{Affine, Point, Rect};
use crate::layout::{Measure, WidthHint};
use crate::pick::PickId;
use crate::scene::{Font, Glyph, GlyphRun, SceneBuilder};
use crate::shape::Shape;

/// Placeholder brush type for parley — real brushes are passed at draw time.
type B = ();

/// Lazy, process-global [`FontContext`]. Constructed on first use; locked for
/// shaping. Single mutex is fine since shaping is cheap and rare relative to
/// per-frame work, and the `text` feature is positioned as scaffolding.
fn font_context() -> &'static Mutex<FontContext> {
    static FC: OnceLock<Mutex<FontContext>> = OnceLock::new();
    FC.get_or_init(|| Mutex::new(FontContext::new()))
}

// ─── TextStyle ───────────────────────────────────────────────────────────────

/// Minimal text style: size in pixels, optional family, weight, italic.
///
/// More properties (letter spacing, line height, decorations) belong in a
/// real shaper. Add them here only if a composition test actually needs them
/// before the user's shaper lands.
#[derive(Clone, Debug)]
pub struct TextStyle {
    pub size_px: f32,
    pub family: Option<String>,
    /// CSS-style font weight (400 = normal, 700 = bold).
    pub weight: u16,
    pub italic: bool,
}

impl TextStyle {
    /// Construct a style with the given pixel size, default family,
    /// weight `400`, and italic `false`.
    pub fn new(size_px: f32) -> Self {
        Self {
            size_px,
            family: None,
            weight: 400,
            italic: false,
        }
    }

    /// Set the preferred font family.
    pub fn family(mut self, name: impl Into<String>) -> Self {
        self.family = Some(name.into());
        self
    }

    /// Set the CSS-style font weight (400 = normal, 700 = bold).
    pub fn weight(mut self, w: u16) -> Self {
        self.weight = w;
        self
    }

    /// Toggle italic.
    pub fn italic(mut self, yes: bool) -> Self {
        self.italic = yes;
        self
    }
}

impl Default for TextStyle {
    fn default() -> Self {
        Self::new(14.0)
    }
}

// ─── TextRun ─────────────────────────────────────────────────────────────────

/// Shaped text — built once, re-laid-out cheaply on width changes.
///
/// Implements [`Measure`] so it can be dropped into a
/// [`crate::composition::Patch::slot`] via
/// [`crate::layout::Cell::measured`].
pub struct TextRun {
    layout: RefCell<parley::Layout<B>>,
    min_width: f32,
    /// Natural unwrapped content width — the layout's width when broken
    /// at no constraint. Used by label-style geoms (TextGeom) that want
    /// to anchor against the text's intrinsic dimensions.
    natural_width: f32,
    /// Natural unwrapped content height — single-line height for a
    /// single-line text.
    natural_height: f32,
    /// Width passed to the last `break_all_lines` call — `None` means
    /// "haven't broken yet". `height_at` mutates this; `draw_text` reads it
    /// to know whether the layout is ready to render.
    last_break_width: RefCell<Option<f32>>,
}

impl TextRun {
    /// Shape `text` with `style`. The full shaping cost is paid here; later
    /// calls to [`Measure::height_at`] and [`draw_text`] only re-break lines.
    pub fn new(text: &str, style: &TextStyle) -> Self {
        let fcx_mutex = font_context();
        let mut fcx = fcx_mutex.lock().expect("font context poisoned");
        let mut lcx = LayoutContext::<B>::new();
        let mut builder = lcx.ranged_builder(&mut fcx, text, 1.0, true);

        builder.push_default(StyleProperty::FontSize(style.size_px));
        builder.push_default(StyleProperty::FontWeight(FontWeight::new(
            style.weight as f32,
        )));
        if style.italic {
            builder.push_default(StyleProperty::FontStyle(FontStyle::Italic));
        }
        if let Some(family) = &style.family {
            builder.push_default(StyleProperty::FontFamily(FontFamily::named(family)));
        } else {
            builder.push_default(StyleProperty::FontFamily(FontFamily::Single(
                FontFamilyName::Generic(GenericFamily::SansSerif),
            )));
        }

        let mut layout: parley::Layout<B> = builder.build(text);
        // Initial unconstrained break — gives us valid line data so
        // `calculate_content_widths` returns meaningful numbers and `lines()`
        // works for callers that draw without solving a composition first.
        layout.break_all_lines(None);
        layout.align(Alignment::Start, AlignmentOptions::default());
        let widths = layout.calculate_content_widths();
        // The unconstrained natural height — typically the single-line
        // height for one paragraph of text.
        let natural_height = layout.height();

        Self {
            layout: RefCell::new(layout),
            min_width: widths.min,
            natural_width: widths.max,
            natural_height,
            last_break_width: RefCell::new(None),
        }
    }

    /// Re-break lines at `max_width` pixels, applying `alignment` to the
    /// resulting layout. Equivalent to `Measure::height_at(max_width, _)`
    /// but exposed for callers that want to draw without first running
    /// through a composition solve.
    ///
    /// `alignment` controls justification within the wrap box: `Start`
    /// is the historical default (every line flush-left within the wrap
    /// width). `Middle` / `End` / `Justified` apply matching parley
    /// alignments.
    pub fn set_max_width(&self, max_width: f32, alignment: Alignment) -> f32 {
        let mut layout = self.layout.borrow_mut();
        layout.break_all_lines(Some(max_width));
        layout.align(alignment, AlignmentOptions::default());
        *self.last_break_width.borrow_mut() = Some(max_width);
        layout.height()
    }

    /// Natural unwrapped content width in pixels — the width the text
    /// would occupy if laid out on a single line per paragraph break in
    /// the source. Computed once at construction; stable regardless of
    /// subsequent [`Self::set_max_width`] calls. Used by label-style
    /// geoms to anchor the text against its intrinsic dimensions.
    pub fn natural_width(&self) -> f64 {
        self.natural_width as f64
    }

    /// Natural unwrapped content height in pixels. Stable across
    /// [`Self::set_max_width`] calls.
    pub fn natural_height(&self) -> f64 {
        self.natural_height as f64
    }

    /// Current laid-out height in pixels — reflects the most recent
    /// [`Self::set_max_width`] / [`Measure::height_at`] call. Equals
    /// [`Self::natural_height`] when no wrap has been requested.
    pub fn current_height(&self) -> f64 {
        self.layout.borrow().height() as f64
    }

    /// Actual rendered content width in pixels — the widest line in
    /// the current layout. Reflects the most recent line-break, so
    /// when [`Self::set_max_width`] has been called the result is the
    /// actual wrapped width (usually less than the constraint, since
    /// parley breaks at word boundaries). When no wrap has been
    /// requested the layout is single-line and this matches
    /// [`Self::natural_width`].
    pub fn content_width(&self) -> f64 {
        let layout = self.layout.borrow();
        let mut max_w = 0.0_f32;
        for line in layout.lines() {
            let w = line.metrics().advance;
            if w > max_w {
                max_w = w;
            }
        }
        max_w as f64
    }

    /// Font descender of the last line in the current layout, in
    /// pixels. Used by background-rect geoms to apply the
    /// ggplot2 `geom_label`-style padding rebalance — bump top padding
    /// up to at least the descender and reduce bottom padding by the
    /// same — so visible glyphs centre vertically in the rect even
    /// when the last line has no descenders ("men" vs "jay").
    pub fn last_line_descender(&self) -> f64 {
        let layout = self.layout.borrow();
        layout
            .lines()
            .last()
            .map(|line| line.metrics().descent as f64)
            .unwrap_or(0.0)
    }
}

impl Measure for TextRun {
    fn width_hint(&self, _dpi: f64) -> WidthHint {
        // The min width is the longest unbreakable cluster — a safe lower
        // bound for any wrap width. The Auto column will pick the max of
        // this and other contributions.
        WidthHint::Min(self.min_width as f64)
    }

    fn height_at(&self, width: f64, _dpi: f64) -> f64 {
        // Measure is alignment-agnostic — solver only cares about layout
        // height. Alignment is reapplied at draw time by callers that
        // care.
        self.set_max_width(width as f32, Alignment::Start) as f64
    }
}

// ─── Per-glyph access ────────────────────────────────────────────────────────

/// One positioned glyph extracted from a [`TextRun`] alongside the font
/// and font-size needed to emit a [`GlyphRun`] containing it. Used by
/// callers that need per-glyph affine transforms — e.g. `TextPathGeom`
/// placing each glyph at a different point along a curve.
///
/// `x` / `y` are cumulative offsets from the run origin (the same units
/// the parley layout uses); `advance` is the glyph's horizontal advance
/// (how much arc length the glyph occupies in flow direction).
#[derive(Clone)]
pub struct LaidGlyph {
    pub id: u32,
    pub x: f32,
    pub y: f32,
    pub advance: f32,
    pub font: Font,
    pub font_size: f32,
}

/// Walk `run`'s laid-out lines and yield every glyph as a [`LaidGlyph`].
/// [`TextRun::new`] does not currently produce inline boxes, but the
/// iteration accepts whatever parley emits, ignoring non-glyph items.
pub fn run_layout_glyphs(run: &TextRun) -> Vec<LaidGlyph> {
    let layout = run.layout.borrow();
    let mut out = Vec::new();
    for line in layout.lines() {
        for item in line.items() {
            let PositionedLayoutItem::GlyphRun(gr) = item else {
                continue;
            };
            let prun = gr.run();
            let font = Font(prun.font().clone());
            let font_size = prun.font_size();
            for g in gr.positioned_glyphs() {
                out.push(LaidGlyph {
                    id: g.id,
                    x: g.x,
                    y: g.y,
                    advance: g.advance,
                    font: font.clone(),
                    font_size,
                });
            }
        }
    }
    out
}

// ─── Glyph markers ──────────────────────────────────────────────────────────

/// Shape `text` with `style` and return a single-glyph [`Shape`]
/// suitable for registering as a [`crate::shape::ShapeRegistry`] entry
/// and using as a [`crate::plot::geom::PointGeom`] marker or
/// linetype-pattern stamp.
///
/// Most callers pass a single character (letter, common symbol, single-
/// codepoint emoji). Multi-codepoint sequences are also accepted —
/// e.g. country-flag emoji like 🇩🇰 (regional indicator D + K) — as
/// long as the resolved font ligates them into one composite glyph
/// (Apple Color Emoji, Noto Color Emoji, … all do this for flags).
/// Shaping happens once here; the draw path is a single
/// `scene.draw_glyphs` call with no per-frame shaping cost.
///
/// The em-space bbox / glyph origin are computed via a fixed-size probe
/// shaping (64 px) and divided back to em-units, so the returned shape
/// is independent of the size at which it is eventually rendered.
///
/// The default anchor is `(-0.5, 0)` — back-edge convention, matching
/// vector point shapes — so the marker drops into mode-B endpoint
/// placement sensibly. Mode-A placements (PointGeom, linetype markers
/// centred on the curve) ignore the anchor.
///
/// # Panics
///
/// Panics if shaping `text` produces zero or more than one glyph. A
/// non-ligated multi-codepoint sequence (the font lacks the
/// substitution, or the input is two separate characters like `"AB"`)
/// will trip this — marker shapes are intentionally restricted to a
/// single glyph.
pub fn glyph_marker(text: &str, style: &TextStyle) -> Shape {
    const PROBE_PX: f32 = 64.0;
    let probe = TextStyle {
        size_px: PROBE_PX,
        family: style.family.clone(),
        weight: style.weight,
        italic: style.italic,
    };
    let run = TextRun::new(text, &probe);
    let laid = run_layout_glyphs(&run);
    assert_eq!(
        laid.len(),
        1,
        "glyph_marker({text:?}): shaped to {} glyphs, but markers require exactly 1 \
         (multi-codepoint inputs must ligate to a single composite glyph in the resolved font)",
        laid.len()
    );
    let g = &laid[0];
    let s = PROBE_PX as f64;
    let em_origin = Point::new(g.x as f64 / s, g.y as f64 / s);
    // em_bbox is `(0, _, advance, _)` horizontally and `(centre_y -
    // h/2, centre_y + h/2)` vertically, with:
    //  - `centre_y = natural_height / 2` — the layout middle, which
    //    coincides with the visible centre for emoji whose bitmap is
    //    positioned to span the full line (Apple Color Emoji and
    //    similar). Latin caps end up slightly above this centre but
    //    close enough for marker use.
    //  - `h = ascender (= em_origin.y)` — gives a sizing reference that
    //    matches the vector-shape convention: `linewidth_px /
    //    bbox.height()` becomes `linewidth_px / ascender`, and
    //    `GLYPH_BBOX_REFERENCE / bbox.height()` in PointGeom matches
    //    vector circle's effective height. The extra ~1.18× boost
    //    needed to make linetype glyphs fill the linewidth visually
    //    (visible ink is ~85% of the ascender) is applied inside
    //    `emit_marker_shape`'s glyph branch, so PointGeom stays at the
    //    natural circle-matched size.
    let advance_em = run.content_width() / s;
    let natural_h_em = run.natural_height() / s;
    let centre_y = natural_h_em / 2.0;
    let h = em_origin.y;
    let em_bbox = Rect::new(0.0, centre_y - h / 2.0, advance_em, centre_y + h / 2.0);
    let anchor = Point::new(-0.5, 0.0);
    Shape::glyph(g.font.clone(), g.id, em_bbox, em_origin, anchor)
}

// ─── Drawing ────────────────────────────────────────────────────────────────

/// Draw `run` at `(x, y)` (top-left of the layout box) into `scene`,
/// tagging every emitted glyph with `pick_id`.
///
/// Requires that [`TextRun::set_max_width`] or [`Measure::height_at`] has
/// been called at least once with the desired wrap width; otherwise the
/// layout is laid out unconstrained (one line per paragraph break in the
/// source text).
///
/// `transform` is applied to the glyph run as a whole (used by rotated
/// labels — pass `Affine::IDENTITY` for unrotated text). The transform
/// runs after glyph placement, so it rotates / scales the entire laid-out
/// box around its own pivot rather than per-glyph.
///
/// Pass [`PickId::Skip`] for non-interactive chrome (titles, axis
/// labels); pass `PickId::Id(ticket)` for picking-enabled labels (e.g.
/// `TextGeom` rows).
pub fn draw_text<S: SceneBuilder + ?Sized>(
    scene: &mut S,
    run: &TextRun,
    x: f64,
    y: f64,
    brush: &Brush,
    transform: Affine,
    pick_id: PickId,
) {
    let layout = run.layout.borrow();
    for line in layout.lines() {
        for item in line.items() {
            let PositionedLayoutItem::GlyphRun(gr) = item else {
                continue; // inline boxes — unsupported
            };
            let prun = gr.run();
            let font = Font(prun.font().clone());
            let glyphs: Vec<Glyph> = gr
                .positioned_glyphs()
                .map(|g| Glyph {
                    id: g.id,
                    x: x as f32 + g.x,
                    y: y as f32 + g.y,
                })
                .collect();
            if glyphs.is_empty() {
                continue;
            }
            let glyph_run = GlyphRun {
                font: &font,
                font_size: prun.font_size(),
                transform,
                glyph_transform: None,
                brush,
                brush_alpha: 1.0,
                hint: false,
                glyphs: &glyphs,
            };
            scene.draw_glyphs(&glyph_run, pick_id);
        }
    }
}

/// Convenience: draw `run` aligned to the top-left of `rect`. The run's
/// lines are re-broken at `rect`'s width before drawing. All glyphs are
/// tagged with `pick_id`.
pub fn draw_text_in_rect<S: SceneBuilder + ?Sized>(
    scene: &mut S,
    run: &TextRun,
    rect: Rect,
    brush: &Brush,
    pick_id: PickId,
) {
    run.set_max_width((rect.x1 - rect.x0) as f32, Alignment::Start);
    draw_text(
        scene,
        run,
        rect.x0,
        rect.y0,
        brush,
        Affine::IDENTITY,
        pick_id,
    );
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_run_has_finite_min_width() {
        let style = TextStyle::new(16.0);
        let run = TextRun::new("Hello, world!", &style);
        assert!(
            run.min_width.is_finite() && run.min_width > 0.0,
            "min width should be positive and finite (got {})",
            run.min_width
        );
    }

    /// Sanity check: a label with no descenders ("men") should reserve
    /// the same vertical space as a label with descenders + tall
    /// ascenders ("jay"). If this fails, `Layout::height()` is glyph-
    /// ink-based and we need an explicit font-metric path.
    #[test]
    fn text_run_height_is_font_metric_not_ink() {
        let style = TextStyle::new(16.0);
        let men = TextRun::new("men", &style);
        let jay = TextRun::new("jay", &style);
        assert!(
            (men.natural_height() - jay.natural_height()).abs() < 0.01,
            "expected font-metric height (descender always reserved): \
             men.h={}, jay.h={}",
            men.natural_height(),
            jay.natural_height()
        );
    }

    #[test]
    fn text_run_natural_width_is_positive() {
        let style = TextStyle::new(16.0);
        let run = TextRun::new("Hello", &style);
        assert!(
            run.natural_width() > 0.0 && run.natural_width().is_finite(),
            "natural_width = {}",
            run.natural_width()
        );
        assert!(run.natural_height() > 0.0);
    }

    #[test]
    fn text_run_height_grows_when_wrapped() {
        let style = TextStyle::new(16.0);
        let run = TextRun::new(
            "Lorem ipsum dolor sit amet, consectetur adipiscing elit, \
             sed do eiusmod tempor incididunt ut labore et dolore magna aliqua.",
            &style,
        );
        let tall = run.height_at(60.0, 96.0);
        let short = run.height_at(2000.0, 96.0);
        assert!(
            tall > short,
            "wrapping at 60px should be taller than at 2000px (got tall={tall}, short={short})"
        );
    }

    #[test]
    fn text_run_measure_via_composition_slot() {
        use crate::composition::{Patch, Slot};
        use crate::layout::Cell;

        let style = TextStyle::new(20.0);
        let p = Patch::new("p")
            .slot(Slot::AxisLeft, Cell::measured(TextRun::new("8888", &style)))
            .slot(Slot::Panel, Cell::empty());
        let layout = p.solve(crate::geometry::Size::new(400.0, 200.0), 96.0);
        let panel = layout.get("p", Slot::Panel).unwrap();
        // We don't pin a specific axis width (font-dependent), but it must
        // be positive and leave room for the panel.
        assert!(panel.x0 > 0.0, "axis should consume some width");
        assert!(
            panel.x1 > panel.x0,
            "panel should still have positive width"
        );
    }

    #[test]
    fn glyph_marker_returns_finite_em_metrics() {
        use crate::shape::ShapeKind;
        let style = TextStyle::new(16.0);
        let shape = glyph_marker("A", &style);
        match shape.kind() {
            ShapeKind::Glyph {
                glyph_id,
                em_bbox,
                em_origin,
                ..
            } => {
                assert!(glyph_id > 0, "expected a non-zero glyph id for 'A'");
                assert!(em_bbox.width() > 0.0 && em_bbox.width().is_finite());
                assert!(em_bbox.height() > 0.0 && em_bbox.height().is_finite());
                assert!(em_origin.x.is_finite() && em_origin.y.is_finite());
                // The probe shapes 'A' at 64 px; em-space dimensions
                // should be roughly < 2 ems (sane font metrics).
                assert!(em_bbox.height() < 2.5, "em height = {}", em_bbox.height());
            }
            _ => panic!("expected glyph variant"),
        }
    }

    #[test]
    fn glyph_marker_anchor_default_is_back_edge() {
        let style = TextStyle::new(16.0);
        let shape = glyph_marker("A", &style);
        let a = shape.anchor();
        assert_eq!(a.x, -0.5);
        assert_eq!(a.y, 0.0);
    }

    #[test]
    #[should_panic(expected = "shaped to 2 glyphs")]
    fn glyph_marker_panics_on_non_ligated_sequence() {
        let style = TextStyle::new(16.0);
        // "AB" is two separate letters; no font ligates this. Should
        // panic so the caller can't accidentally register a multi-glyph
        // marker.
        let _ = glyph_marker("AB", &style);
    }
}
