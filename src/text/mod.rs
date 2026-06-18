//! **Scaffolding** text shaping/layout backed by [`parley`].
//!
//! Gated behind the `text` cargo feature. Intended to be temporary — the host
//! crate is expected to bring its own shaper. While this module exists it
//! provides:
//!
//! - [`TextStyle`] — font / size (pt, DPI-independent) / weight / width /
//!   style (italic / oblique) / OpenType features / variations descriptor.
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

/// Generic font-family categories mirroring the CSS surface. The host
/// shaper resolves each to a concrete face — independent of any specific
/// named family.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GenericFamilyKind {
    /// Serifed faces — Times-like.
    Serif,
    /// Sans-serif faces — Helvetica-like.
    SansSerif,
    /// Monospaced faces.
    Mono,
    /// Cursive / script faces.
    Cursive,
    /// Decorative / fantasy faces.
    Fantasy,
    /// Operating-system UI font.
    SystemUi,
}

/// One entry in a [`TextStyle::families`] fallback chain. Named entries
/// reference a specific face by string; generic entries pick a CSS-style
/// category.
#[derive(Clone, Debug, PartialEq)]
pub enum FontFamilyEntry {
    /// A specific named family (e.g. `"Helvetica"`).
    Named(String),
    /// A generic family category.
    Generic(GenericFamilyKind),
}

/// Style axis — upright, italic, or oblique with a slant angle in degrees.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum FontStyleKind {
    /// Upright glyphs.
    #[default]
    Normal,
    /// Italic — a distinct set of slanted glyphs.
    Italic,
    /// Oblique — upright glyphs slanted by the given angle in degrees.
    Oblique(f32),
}

/// OpenType feature setting (4-byte tag + `u16` value).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FontFeatureSetting {
    /// OpenType feature tag — e.g. `*b"liga"`.
    pub tag: [u8; 4],
    /// Feature value (0 = off, 1 = on, stylistic-set indices for `ssXX`).
    pub value: u16,
}

/// Variable-font axis assignment (4-byte tag + `f32` value).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FontVariationSetting {
    /// Variable-font axis tag — e.g. `*b"wght"`.
    pub tag: [u8; 4],
    /// Axis position (units are axis-specific).
    pub value: f32,
}

/// Line-height specification. Mirrors parley's `LineHeight` minus the
/// metrics-relative variant — chrome callers either want a font-size
/// multiplier (the CSS `line-height: 1.2` style) or an absolute pt
/// value, never the metrics-relative form.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LineHeight {
    /// Multiplier of the font size. `Relative(1.2)` = 120% of `size_pt`.
    Relative(f32),
    /// Absolute line height in points (DPI-independent).
    Absolute(f32),
}

impl Default for LineHeight {
    /// `Relative(1.2)` — matches the CSS / typesetting default.
    fn default() -> Self {
        LineHeight::Relative(1.2)
    }
}

/// Shaper-facing text style. Carries every axis the scaffolding shaper
/// can plumb through to parley — size, family chain, weight, width
/// (CSS `font-width` ratio), style (italic / oblique), OpenType feature
/// toggles, and variable-font variations.
///
/// Sizes are in **points** (1pt = 1/72 inch) — DPI-independent. The
/// conversion to pixels happens inside [`TextRun::new`] using the DPI
/// passed at shaping time.
///
/// Letter spacing, line height, decorations belong in the host shaper —
/// add them here only if a composition test or chrome path actually
/// needs them while this scaffolding is still in place.
#[derive(Clone, Debug)]
pub struct TextStyle {
    /// Font size in points (1pt = 1/72 inch).
    pub size_pt: f32,
    /// Ordered fallback chain of font families. Empty falls back to a
    /// generic sans-serif at shape time.
    pub families: Vec<FontFamilyEntry>,
    /// CSS-style font weight (400 = normal, 700 = bold).
    pub weight: u16,
    /// CSS `font-width` ratio — 1.0 = normal, 0.5 = ultra-condensed,
    /// 2.0 = ultra-expanded.
    pub width: f32,
    /// Style axis — upright, italic, or oblique with a slant angle.
    pub style: FontStyleKind,
    /// Line height — relative-to-size multiplier or absolute pt.
    pub line_height: LineHeight,
    /// OpenType feature toggles applied to the whole run.
    pub features: Vec<FontFeatureSetting>,
    /// Variable-font axis values applied to the whole run.
    pub variations: Vec<FontVariationSetting>,
}

impl TextStyle {
    /// Construct a style with the given point size, empty family chain
    /// (defaults to sans-serif at shape time), weight `400`, normal
    /// width and style, no features, no variations.
    pub fn new(size_pt: f32) -> Self {
        Self {
            size_pt,
            families: Vec::new(),
            weight: 400,
            width: 1.0,
            style: FontStyleKind::Normal,
            line_height: LineHeight::default(),
            features: Vec::new(),
            variations: Vec::new(),
        }
    }

    /// Set the preferred font family to a single named face. Replaces
    /// any previously-set chain.
    pub fn family(mut self, name: impl Into<String>) -> Self {
        self.families = vec![FontFamilyEntry::Named(name.into())];
        self
    }

    /// Replace the family fallback chain with the given iterator.
    pub fn families(mut self, entries: impl IntoIterator<Item = FontFamilyEntry>) -> Self {
        self.families = entries.into_iter().collect();
        self
    }

    /// Append a generic family category to the fallback chain.
    pub fn generic_family(mut self, kind: GenericFamilyKind) -> Self {
        self.families.push(FontFamilyEntry::Generic(kind));
        self
    }

    /// Set the CSS-style font weight (400 = normal, 700 = bold).
    pub fn weight(mut self, w: u16) -> Self {
        self.weight = w;
        self
    }

    /// Set the CSS `font-width` ratio (1.0 = normal).
    pub fn width(mut self, ratio: f32) -> Self {
        self.width = ratio;
        self
    }

    /// Convenience — toggle the `Italic` style. `true` sets the style
    /// to `Italic`; `false` sets it back to `Normal`.
    pub fn italic(mut self, yes: bool) -> Self {
        self.style = if yes {
            FontStyleKind::Italic
        } else {
            FontStyleKind::Normal
        };
        self
    }

    /// Set the style axis directly (Normal / Italic / Oblique).
    pub fn style(mut self, kind: FontStyleKind) -> Self {
        self.style = kind;
        self
    }

    /// Set the line height.
    pub fn line_height(mut self, lh: LineHeight) -> Self {
        self.line_height = lh;
        self
    }

    /// Replace the OpenType feature settings.
    pub fn features(mut self, items: impl IntoIterator<Item = FontFeatureSetting>) -> Self {
        self.features = items.into_iter().collect();
        self
    }

    /// Replace the variable-font axis assignments.
    pub fn variations(mut self, items: impl IntoIterator<Item = FontVariationSetting>) -> Self {
        self.variations = items.into_iter().collect();
        self
    }
}

impl Default for TextStyle {
    fn default() -> Self {
        Self::new(14.0)
    }
}

/// Translate a local [`GenericFamilyKind`] to parley's [`GenericFamily`].
fn generic_family_to_parley(kind: GenericFamilyKind) -> GenericFamily {
    match kind {
        GenericFamilyKind::Serif => GenericFamily::Serif,
        GenericFamilyKind::SansSerif => GenericFamily::SansSerif,
        GenericFamilyKind::Mono => GenericFamily::Monospace,
        GenericFamilyKind::Cursive => GenericFamily::Cursive,
        GenericFamilyKind::Fantasy => GenericFamily::Fantasy,
        GenericFamilyKind::SystemUi => GenericFamily::SystemUi,
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
    /// Shape `text` with `style` at `dpi` (typically 96 for screen output).
    /// The point-size on `style` is converted to pixels via
    /// `size_px = size_pt * dpi / 72` before parley shapes the glyphs.
    /// Full shaping cost is paid here; later calls to
    /// [`Measure::height_at`] and [`draw_text`] only re-break lines.
    pub fn new(text: &str, style: &TextStyle, dpi: f64) -> Self {
        let fcx_mutex = font_context();
        let mut fcx = fcx_mutex.lock().expect("font context poisoned");
        let mut lcx = LayoutContext::<B>::new();
        let mut builder = lcx.ranged_builder(&mut fcx, text, 1.0, true);

        let size_px = (style.size_pt as f64 * dpi / 72.0) as f32;
        builder.push_default(StyleProperty::FontSize(size_px));
        builder.push_default(StyleProperty::FontWeight(FontWeight::new(
            style.weight as f32,
        )));
        builder.push_default(StyleProperty::FontWidth(parley::FontWidth::from_ratio(
            style.width,
        )));
        let parley_style = match style.style {
            FontStyleKind::Normal => FontStyle::Normal,
            FontStyleKind::Italic => FontStyle::Italic,
            FontStyleKind::Oblique(angle) => FontStyle::Oblique(Some(angle)),
        };
        builder.push_default(StyleProperty::FontStyle(parley_style));
        let line_height = match style.line_height {
            LineHeight::Relative(mult) => parley::LineHeight::FontSizeRelative(mult),
            LineHeight::Absolute(pt) => {
                parley::LineHeight::Absolute((pt as f64 * dpi / 72.0) as f32)
            }
        };
        builder.push_default(StyleProperty::LineHeight(line_height));
        // Owned families list — parley borrows from us via `Cow`s, so
        // the source strings must outlive `build()`. Constructing the
        // names eagerly and pushing them keeps the lifetimes local.
        if style.families.is_empty() {
            builder.push_default(StyleProperty::FontFamily(FontFamily::Single(
                FontFamilyName::Generic(GenericFamily::SansSerif),
            )));
        } else {
            let names: Vec<FontFamilyName<'_>> = style
                .families
                .iter()
                .map(|entry| match entry {
                    FontFamilyEntry::Named(name) => FontFamilyName::named(name),
                    FontFamilyEntry::Generic(kind) => {
                        FontFamilyName::Generic(generic_family_to_parley(*kind))
                    }
                })
                .collect();
            builder.push_default(StyleProperty::FontFamily(if names.len() == 1 {
                FontFamily::Single(names[0].clone())
            } else {
                FontFamily::List(std::borrow::Cow::Owned(names))
            }));
        }
        if !style.features.is_empty() {
            let parley_features: Vec<parley::FontFeature> = style
                .features
                .iter()
                .map(|f| parley::FontFeature::new(parley::setting::Tag::from_bytes(f.tag), f.value))
                .collect();
            builder.push_default(StyleProperty::FontFeatures(parley::FontFeatures::List(
                std::borrow::Cow::Owned(parley_features),
            )));
        }
        if !style.variations.is_empty() {
            let parley_variations: Vec<parley::FontVariation> = style
                .variations
                .iter()
                .map(|v| {
                    parley::FontVariation::new(parley::setting::Tag::from_bytes(v.tag), v.value)
                })
                .collect();
            builder.push_default(StyleProperty::FontVariations(parley::FontVariations::List(
                std::borrow::Cow::Owned(parley_variations),
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

    /// Offset from the layout's top edge to the baseline of the
    /// first line, in pixels. Differs from the font's `ascent`
    /// metric when the resolved line height includes leading — the
    /// baseline sits below the typographic ascent by half the
    /// leading. Used by chrome labels to convert between
    /// baseline-anchored and top-anchored positioning.
    pub fn baseline_offset(&self) -> f64 {
        let layout = self.layout.borrow();
        let mut iter = layout.lines();
        match iter.next() {
            Some(line) => line.metrics().baseline as f64,
            None => 0.0,
        }
    }

    /// Cap-height of the first run, in pixels — distance from the
    /// baseline to the top of capital letters. Falls back to
    /// `x_height` (and then `0.7 × ascent` as a last resort) when
    /// the font doesn't report cap-height. Used by axis / legend
    /// label centering: a numeric or uppercase label centered on
    /// `cap_height` looks visually balanced against its tick or
    /// swatch, whereas centering on the full `natural_height`
    /// reserves descender space the glyphs don't occupy and shifts
    /// the visual centre off-target.
    pub fn cap_height(&self) -> f64 {
        let layout = self.layout.borrow();
        let mut lines_iter = layout.lines();
        let line = match lines_iter.next() {
            Some(l) => l,
            None => return 0.0,
        };
        let ascent_fallback = line.metrics().ascent as f64;
        let mut result: Option<f64> = None;
        for item in line.items() {
            if let PositionedLayoutItem::GlyphRun(gr) = item {
                let m = gr.run().metrics();
                if let Some(h) = m.cap_height.or(m.x_height) {
                    result = Some(h as f64);
                    break;
                }
            }
        }
        result.unwrap_or(ascent_fallback * 0.7)
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

    /// Y position of the first line's ascender top, relative to the
    /// layout's top edge. Equivalent to the top half-leading on the
    /// first line — the empty pixels above the visible glyphs that
    /// the line-box reserves on its way to `line-height`.
    pub fn first_line_ascender_offset(&self) -> f64 {
        let layout = self.layout.borrow();
        let result = layout
            .lines()
            .next()
            .map(|line| {
                let m = line.metrics();
                (m.baseline as f64) - (m.ascent as f64)
            })
            .unwrap_or(0.0);
        result
    }

    /// Y position of the last line's descender bottom, relative to
    /// the layout's top edge. Equivalent to `current_height - bottom
    /// half-leading on the last line`.
    pub fn last_line_descender_offset_from_top(&self) -> f64 {
        let layout = self.layout.borrow();
        let result = layout
            .lines()
            .last()
            .map(|line| {
                let m = line.metrics();
                (m.baseline as f64) + (m.descent as f64)
            })
            .unwrap_or(0.0);
        result
    }

    /// Inked height of the current layout — from the first line's
    /// ascender top to the last line's descender bottom, with
    /// leading appearing only *between* lines (not above the first
    /// or below the last). The natural text box.
    pub fn inked_height(&self) -> f64 {
        (self.last_line_descender_offset_from_top() - self.first_line_ascender_offset()).max(0.0)
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
        // Re-break at the requested width, then report the *inked*
        // height (first-line ascender top → last-line descender
        // bottom). Leading lives only between lines, not above the
        // first or below the last, so a chrome slot sized off this
        // measure hugs the visible glyphs instead of inheriting the
        // empty half-leading the line-box reserves.
        let _ = self.set_max_width(width as f32, Alignment::Start);
        self.inked_height()
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
    // Probe shapes at a known pixel size so the returned em-space
    // bbox divides cleanly back to em units; the marker is resampled
    // to the caller's `size_pt` at draw time. Shape with DPI = 96 so
    // `PROBE_PT * 96 / 72 = PROBE_PX = 64`.
    const PROBE_PX: f32 = 64.0;
    const PROBE_PT: f32 = PROBE_PX * 72.0 / 96.0;
    let probe = TextStyle {
        size_pt: PROBE_PT,
        families: style.families.clone(),
        weight: style.weight,
        width: style.width,
        style: style.style,
        line_height: style.line_height,
        features: style.features.clone(),
        variations: style.variations.clone(),
    };
    let run = TextRun::new(text, &probe, 96.0);
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

/// [`Measure`] adapter that pads an inner measure with per-edge
/// margins (in pixels). Reported widths include `left + right`;
/// reported heights include `top + bottom`. The inner measure sees
/// the constrained inner width when `height_at` queries it.
///
/// Used by chrome text slots so the layout solver reserves space for
/// the text **plus** its theme-defined margin, rather than shrinking
/// the text area inside an unchanged slot.
pub struct WithMargin {
    inner: Box<dyn Measure>,
    /// Margins in pixels: `(top, right, bottom, left)`.
    pub margins_px: (f64, f64, f64, f64),
}

impl WithMargin {
    /// Wrap `inner` with per-edge px margins.
    pub fn new(inner: Box<dyn Measure>, margins_px: (f64, f64, f64, f64)) -> Self {
        Self { inner, margins_px }
    }
}

impl Measure for WithMargin {
    fn width_hint(&self, dpi: f64) -> WidthHint {
        let (_, r, _, l) = self.margins_px;
        match self.inner.width_hint(dpi) {
            WidthHint::Min(w) => WidthHint::Min(w + l + r),
            WidthHint::NeedsHeight { seed } => WidthHint::NeedsHeight { seed: seed + l + r },
        }
    }

    fn height_at(&self, width: f64, dpi: f64) -> f64 {
        let (t, r, b, l) = self.margins_px;
        let inner_w = (width - l - r).max(0.0);
        self.inner.height_at(inner_w, dpi) + t + b
    }

    fn width_at(&self, height: f64, dpi: f64) -> f64 {
        let (t, r, b, l) = self.margins_px;
        let inner_h = (height - t - b).max(0.0);
        self.inner.width_at(inner_h, dpi) + l + r
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
        let run = TextRun::new("Hello, world!", &style, 96.0);
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
        let men = TextRun::new("men", &style, 96.0);
        let jay = TextRun::new("jay", &style, 96.0);
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
        let run = TextRun::new("Hello", &style, 96.0);
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
            96.0,
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
            .slot(
                Slot::AxisLeft,
                Cell::measured(TextRun::new("8888", &style, 96.0)),
            )
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
