//! Shape a markdown source into a parley `Layout` with per-range
//! styles, and draw the result through `SceneBuilder`.
//!
//! [`RichTextRun::new`] runs the full pipeline: source → parser →
//! reducer → parley shaping, applying each [`InlineRun`]'s
//! [`StyleDelta`] via `RangedBuilder::push(prop, range)` on top of
//! the base [`TextStyle`]. Per-range colours land in parley's
//! `Style<B>::brush` field; [`draw_rich_text`] reads them back on the
//! way out.
//!
//! Baseline shifts (from `sup` / `sub` and custom baseline-em spans)
//! are held in a parallel [`Vec<BaselineRun>`] because parley has no
//! baseline-shift `StyleProperty`. At draw time [`draw_rich_text`]
//! offsets each glyph's `y` by the shift matching its cluster's byte
//! range.
//!
//! Block-level backgrounds and borders (code_block fills, custom
//! `.callout` boxes, blockquote borders) are computed by
//! [`crate::text::rich::block::compute_block_paints`] and emitted by
//! [`draw_rich_text`] before the glyph runs so the text sits on top.
//! See `block.rs` for what the pass does and doesn't cover — bullets,
//! blockquote left-edge bars, and hr lines are follow-up tasks.
//! Paragraph breaks land as `\n\n` in the source we hand to parley,
//! so multi-paragraph markdown line-breaks correctly.

use std::cell::RefCell;

use parley::{
    Alignment, AlignmentOptions, FontFamily, FontFamilyName, FontStyle, FontWeight, GenericFamily,
    LayoutContext, PositionedLayoutItem, StyleProperty,
};

use super::anchor::{LayoutBounds, RichAnchor};
use super::block::{compute_block_paints, BlockPaint};
use super::parser::{parse, ParseError};
use super::reduce::{reduce, BaselineRun, Block, BuiltRuns, InlineRun};
use super::style::{RichTextStyleSheet, StyleDelta};
use crate::brush::Brush;
use crate::color::Color;
use crate::geometry::Affine;
use crate::layout::{Measure, WidthHint};
use crate::pick::PickId;
use crate::plot::theme::{Length, Palette};
use crate::scene::{Font, Glyph, GlyphRun, SceneBuilder};
use crate::text::{FontFamilyEntry, GenericFamilyKind, LineHeight, TextStyle};

// ─── Brush newtype ──────────────────────────────────────────────────────────

/// Newtype wrapping [`Color`] so it satisfies parley's `Brush` trait
/// bounds (`Clone + PartialEq + Default + Debug`). `AlphaColor` from
/// the `color` crate is missing `Default`.
///
/// [`draw_rich_text`] reads the inner colour back on emission and
/// hands it to [`Brush::Solid`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RichBrush(pub Color);

impl Default for RichBrush {
    /// Opaque black — matches the default text fill used by
    /// [`crate::text::draw_text`] when no colour has been resolved.
    fn default() -> Self {
        RichBrush(Color::from_rgba8(0, 0, 0, 255))
    }
}

// ─── RichTextRun ────────────────────────────────────────────────────────────

/// Shaped rich text — parley `Layout` plus the sidecar tables the
/// draw pass needs (baseline shifts, block boundaries). Implements
/// [`Measure`] so it drops into a composition slot exactly like
/// [`crate::text::TextRun`].
///
/// Construct via [`RichTextRun::new`]; render via [`draw_rich_text`].
pub struct RichTextRun {
    layout: RefCell<parley::Layout<RichBrush>>,
    /// Baseline shifts, indexed by byte range into the reduced text.
    /// Consumed at draw time by [`draw_rich_text`].
    baseline_shifts: Vec<BaselineRun>,
    /// Block boundaries collected by the reducer. Consumed by the
    /// block-layout pass at draw time (see [`Self::block_paints`]).
    pub(crate) blocks: Vec<Block>,
    /// Base text size in pt at construction — cached so the block-
    /// layout pass can resolve `Length::Rel` padding / border-widths
    /// against the block's ambient em.
    base_size_pt: f32,
    /// Palette used for resolving [`crate::plot::theme::ThemeColor`]
    /// values on block backgrounds / borders. Stored so callers who
    /// only hold the run can still compute paints.
    palette: Palette,
    /// DPI captured at construction — used together with `base_size_pt`
    /// to convert pt-based padding / border widths to pixels.
    dpi: f64,
    natural_width: f32,
    natural_height: f32,
    min_width: f32,
    last_break_width: RefCell<Option<f32>>,
}

impl RichTextRun {
    /// Walk the current layout's lines and produce the bounds table
    /// used by [`RichAnchor`] resolution. `inline_min_coord`,
    /// `block_min_coord` etc. come from parley's per-line metrics.
    fn bounds(layout: &parley::Layout<RichBrush>) -> LayoutBounds {
        let width = layout.width();
        let height = layout.height();
        let mut ink_left = f32::INFINITY;
        let mut ink_right = f32::NEG_INFINITY;
        let mut ink_top = f32::INFINITY;
        let mut ink_bottom = f32::NEG_INFINITY;
        let mut first_baseline: Option<f32> = None;
        let mut last_baseline: f32 = 0.0;
        for line in layout.lines() {
            let m = line.metrics();
            ink_left = ink_left.min(m.inline_min_coord);
            ink_right = ink_right.max(m.inline_max_coord);
            ink_top = ink_top.min(m.block_min_coord);
            ink_bottom = ink_bottom.max(m.block_max_coord);
            if first_baseline.is_none() {
                first_baseline = Some(m.baseline);
            }
            last_baseline = m.baseline;
        }
        // Empty layouts (no lines) — collapse everything to zero so
        // downstream anchor math still produces finite offsets.
        if !ink_left.is_finite() {
            ink_left = 0.0;
        }
        if !ink_right.is_finite() {
            ink_right = 0.0;
        }
        if !ink_top.is_finite() {
            ink_top = 0.0;
        }
        if !ink_bottom.is_finite() {
            ink_bottom = 0.0;
        }
        LayoutBounds {
            width,
            height,
            ink_left,
            ink_right,
            ink_top,
            ink_bottom,
            first_baseline: first_baseline.unwrap_or(0.0),
            last_baseline,
        }
    }

    /// Current layout bounds — a snapshot used by [`draw_rich_text`]
    /// to resolve a [`RichAnchor`] into a top-left offset. Cheap
    /// (`O(lines)`); we recompute per draw call rather than caching
    /// so bounds stay consistent after [`Self::set_max_width`] calls.
    pub fn layout_bounds(&self) -> LayoutBounds {
        Self::bounds(&self.layout.borrow())
    }

    /// Compute per-block paint instructions against the current
    /// layout. Outer-first: containers paint underneath their content.
    /// See [`crate::text::rich::block`] for the shape returned.
    pub fn block_paints(&self) -> Vec<BlockPaint> {
        compute_block_paints(
            &self.layout.borrow(),
            &self.blocks,
            &self.palette,
            self.base_size_pt,
            self.dpi,
        )
    }
}

/// Wrap-width policy for a rich-text block. Mirrors marquee's
/// `marquee_grob(width = ...)` argument.
///
/// - [`RichTextWidth::Natural`] — no wrapping; the layout takes its
///   full unbreakable width. Matches marquee's `width = NA`.
/// - [`RichTextWidth::Fixed(px)`] — wrap at this pixel width. Matches
///   marquee's numeric `width`.
/// - Marquee also supports `width = NULL` ("parent container width").
///   In hephaestus that surfaces through the composition solver:
///   drop the run into a `Cell::measured` and it wraps at whatever
///   width the layout provides via [`Measure::height_at`], no
///   explicit width argument needed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RichTextWidth {
    /// Natural (no soft-wrap) width. Matches marquee's `NA`.
    Natural,
    /// Wrap at this pixel width. Matches marquee's numeric argument.
    Fixed(f32),
}

impl Default for RichTextWidth {
    /// [`RichTextWidth::Natural`] — no wrap. Matches
    /// [`crate::text::TextRun::new`]'s implicit "shape without a
    /// break constraint" behaviour.
    fn default() -> Self {
        RichTextWidth::Natural
    }
}

impl RichTextRun {
    /// Parse `source` as marquee-flavoured markdown, resolve every
    /// span through `sheet` + `palette`, and shape the result on top
    /// of `base_style`. `base_brush` is the fallback text colour for
    /// runs that don't have a resolved colour (i.e. plain text
    /// outside any coloured span).
    ///
    /// Shaped without a wrap constraint — the layout takes its
    /// natural (unbreakable) width. To wrap at a specific width, use
    /// [`Self::new_with_width`], or call [`Self::set_max_width`] /
    /// let [`Measure::height_at`] break lines on demand.
    ///
    /// Returns an error if the parser fails; returns a shaped
    /// [`RichTextRun`] otherwise.
    pub fn new(
        source: &str,
        base_style: &TextStyle,
        base_brush: Color,
        sheet: &RichTextStyleSheet,
        palette: &Palette,
        dpi: f64,
    ) -> Result<Self, ParseError> {
        Self::new_with_width(
            source,
            base_style,
            base_brush,
            sheet,
            palette,
            dpi,
            RichTextWidth::Natural,
        )
    }

    /// Like [`Self::new`] but takes an explicit
    /// [`RichTextWidth`] policy. Use this to mirror marquee's
    /// `marquee_grob(width = ...)` argument: `Fixed(px)` wraps at
    /// that width, `Natural` leaves the layout unwrapped.
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_width(
        source: &str,
        base_style: &TextStyle,
        base_brush: Color,
        sheet: &RichTextStyleSheet,
        palette: &Palette,
        dpi: f64,
        width: RichTextWidth,
    ) -> Result<Self, ParseError> {
        let events = parse(source)?;
        let runs = reduce(&events, sheet);
        let r = Self::shape(runs, base_style, base_brush, palette, dpi);
        if let RichTextWidth::Fixed(px) = width {
            r.set_max_width(px, parley::Alignment::Start);
        }
        Ok(r)
    }

    /// Alternative constructor — takes a pre-built [`BuiltRuns`].
    /// Useful in tests and when the caller wants to inspect / mutate
    /// the reducer output before shaping.
    pub fn from_built_runs(
        runs: BuiltRuns,
        base_style: &TextStyle,
        base_brush: Color,
        palette: &Palette,
        dpi: f64,
    ) -> Self {
        Self::shape(runs, base_style, base_brush, palette, dpi)
    }

    fn shape(
        runs: BuiltRuns,
        base_style: &TextStyle,
        base_brush: Color,
        palette: &Palette,
        dpi: f64,
    ) -> Self {
        let fcx_mutex = crate::text::font_context();
        let mut fcx = fcx_mutex.lock().expect("font context poisoned");
        let mut lcx = LayoutContext::<RichBrush>::new();
        let mut builder = lcx.ranged_builder(&mut fcx, &runs.text, 1.0, true);

        // ── Push defaults from the base TextStyle. ──
        push_base_defaults(&mut builder, base_style, base_brush, dpi);

        // ── Owned family name pool. parley's family entries borrow
        // from us via Cow, so any FontFamilyName<'_> pushed through
        // `push` must outlive the `build()` call. We buffer every
        // family string we plan to reference so the &str lifetimes
        // are stable across the whole builder scope. ──
        let mut family_pool: Vec<String> = Vec::new();

        // ── Apply each inline run's delta as ranged properties. ──
        for InlineRun { range, delta } in &runs.inline {
            apply_delta_range(
                &mut builder,
                range.clone(),
                delta,
                base_style,
                palette,
                dpi,
                &mut family_pool,
            );
        }

        let mut layout: parley::Layout<RichBrush> = builder.build(&runs.text);
        layout.break_all_lines(None);
        layout.align(Alignment::Start, AlignmentOptions::default());
        let widths = layout.calculate_content_widths();
        let natural_height = layout.height();

        Self {
            layout: RefCell::new(layout),
            baseline_shifts: runs.baseline_shifts,
            blocks: runs.blocks,
            base_size_pt: base_style.size_pt,
            palette: *palette,
            dpi,
            natural_width: widths.max,
            natural_height,
            min_width: widths.min,
            last_break_width: RefCell::new(None),
        }
    }

    /// Natural (unwrapped) content width in pixels.
    pub fn natural_width(&self) -> f64 {
        self.natural_width as f64
    }

    /// Natural (unwrapped) content height in pixels.
    pub fn natural_height(&self) -> f64 {
        self.natural_height as f64
    }

    /// Current laid-out height in pixels — reflects the most recent
    /// [`Self::set_max_width`] / [`Measure::height_at`] call.
    pub fn current_height(&self) -> f64 {
        self.layout.borrow().height() as f64
    }

    /// Re-break lines at `max_width_px`. Returns the new height.
    pub fn set_max_width(&self, max_width_px: f32, alignment: Alignment) -> f32 {
        let mut layout = self.layout.borrow_mut();
        layout.break_all_lines(Some(max_width_px));
        layout.align(alignment, AlignmentOptions::default());
        *self.last_break_width.borrow_mut() = Some(max_width_px);
        layout.height()
    }
}

impl Measure for RichTextRun {
    fn width_hint(&self, _dpi: f64) -> WidthHint {
        WidthHint::Min(self.min_width as f64)
    }

    fn height_at(&self, width: f64, _dpi: f64) -> f64 {
        self.set_max_width(width as f32, Alignment::Start) as f64
    }
}

// ─── Default / per-range property pushes ────────────────────────────────────

fn push_base_defaults(
    builder: &mut parley::RangedBuilder<'_, RichBrush>,
    style: &TextStyle,
    brush: Color,
    dpi: f64,
) {
    let size_px = (style.size_pt as f64 * dpi / 72.0) as f32;
    builder.push_default(StyleProperty::FontSize(size_px));
    builder.push_default(StyleProperty::FontWeight(FontWeight::new(
        style.weight as f32,
    )));
    builder.push_default(StyleProperty::FontWidth(parley::FontWidth::from_ratio(
        style.width,
    )));
    let parley_style = match style.style {
        crate::text::FontStyleKind::Normal => FontStyle::Normal,
        crate::text::FontStyleKind::Italic => FontStyle::Italic,
        crate::text::FontStyleKind::Oblique(angle) => FontStyle::Oblique(Some(angle)),
    };
    builder.push_default(StyleProperty::FontStyle(parley_style));
    let line_height = match style.line_height {
        LineHeight::Relative(mult) => parley::LineHeight::FontSizeRelative(mult),
        LineHeight::Absolute(pt) => parley::LineHeight::Absolute((pt as f64 * dpi / 72.0) as f32),
    };
    builder.push_default(StyleProperty::LineHeight(line_height));
    if style.letter_spacing_pt != 0.0 {
        let letter_spacing_px = (style.letter_spacing_pt as f64 * dpi / 72.0) as f32;
        builder.push_default(StyleProperty::LetterSpacing(letter_spacing_px));
    }
    if style.underline {
        builder.push_default(StyleProperty::Underline(true));
    }
    if style.strikethrough {
        builder.push_default(StyleProperty::Strikethrough(true));
    }
    builder.push_default(StyleProperty::Brush(RichBrush(brush)));
    if style.families.is_empty() {
        builder.push_default(StyleProperty::FontFamily(FontFamily::Single(
            FontFamilyName::Generic(GenericFamily::SansSerif),
        )));
    } else {
        // As with TextRun, resolve the family chain once. Names live
        // in `style.families` which outlives the builder, so we can
        // reference the &str directly.
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
}

fn apply_delta_range(
    builder: &mut parley::RangedBuilder<'_, RichBrush>,
    range: std::ops::Range<usize>,
    delta: &StyleDelta,
    base: &TextStyle,
    palette: &Palette,
    dpi: f64,
    family_pool: &mut Vec<String>,
) {
    // Size — resolves against the base size for both Abs and Rel to
    // match marquee's "relative to base" semantics.
    if let Some(size) = delta.size {
        let pt = match size {
            Length::Abs(v) => v,
            Length::Rel(m) => base.size_pt as f64 * m,
        };
        let px = (pt * dpi / 72.0) as f32;
        builder.push(StyleProperty::FontSize(px), range.clone());
    }
    if let Some(w) = delta.weight {
        builder.push(
            StyleProperty::FontWeight(FontWeight::new(w as f32)),
            range.clone(),
        );
    }
    if let Some(italic) = delta.italic {
        builder.push(
            StyleProperty::FontStyle(if italic {
                FontStyle::Italic
            } else {
                FontStyle::Normal
            }),
            range.clone(),
        );
    }
    if let Some(w) = delta.width {
        builder.push(
            StyleProperty::FontWidth(parley::FontWidth::from_ratio(w)),
            range.clone(),
        );
    }
    if let Some(color) = &delta.color {
        let c = color.resolve(palette);
        builder.push(StyleProperty::Brush(RichBrush(c)), range.clone());
    }
    if let Some(pt) = delta.tracking_pt {
        let px = (pt as f64 * dpi / 72.0) as f32;
        builder.push(StyleProperty::LetterSpacing(px), range.clone());
    }
    if let Some(u) = delta.underline {
        builder.push(StyleProperty::Underline(u), range.clone());
    }
    if let Some(s) = delta.strikethrough {
        builder.push(StyleProperty::Strikethrough(s), range.clone());
    }
    if let Some(family) = &delta.family {
        // Buffer the string so its lifetime survives `build()`.
        family_pool.push(family.clone());
        let name = family_pool.last().expect("just pushed");
        let entry = if let Some(generic) = generic_family_from_str(name) {
            FontFamily::Single(FontFamilyName::Generic(generic))
        } else {
            FontFamily::Single(FontFamilyName::named(name.as_str()))
        };
        builder.push(StyleProperty::FontFamily(entry), range.clone());
    }
    // Note: `baseline_em` is not pushed to parley — it's held on the
    // side and applied at draw time. See `draw_rich_text`.
    // Block-level fields (margin, padding, background, border, indent,
    // hanging, bullet) are not pushed here — they're consumed by the
    // block-layout pass (steps 4–8 of the plan).
}

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

/// Recognise the common CSS generic family names on a `family` string
/// so `sheet.set("code", { family: "monospace", ... })` resolves to
/// the generic monospace face rather than a face literally named
/// `"monospace"` (which usually doesn't exist).
fn generic_family_from_str(s: &str) -> Option<GenericFamily> {
    match s.to_ascii_lowercase().as_str() {
        "serif" => Some(GenericFamily::Serif),
        "sans-serif" | "sans" => Some(GenericFamily::SansSerif),
        "monospace" | "mono" => Some(GenericFamily::Monospace),
        "cursive" => Some(GenericFamily::Cursive),
        "fantasy" => Some(GenericFamily::Fantasy),
        "system-ui" | "systemui" | "ui" => Some(GenericFamily::SystemUi),
        _ => None,
    }
}

// ─── Draw ───────────────────────────────────────────────────────────────────

/// Emit the shaped [`RichTextRun`] into `scene` at `(x, y)`. The
/// `anchor` controls what point *on the laid-out text* coincides
/// with `(x, y)`: `RichAnchor::top_left()` (the default via
/// `RichAnchor::default()`) matches [`crate::text::draw_text`]'s
/// implicit top-left placement; `RichAnchor::center_ink()` centres
/// on the visible glyph column; `RichAnchor::first_line_baseline()`
/// aligns the caller's `y` to the first line's baseline. See the
/// [`crate::text::rich::anchor`] module for the full vocabulary,
/// which mirrors marquee's `hjust` / `vjust`.
///
/// Per-range brushes come from parley's `Style::brush`; baseline
/// shifts are applied per-run based on each parley run's byte range
/// (parley splits runs at every style boundary we push, including
/// the `FontSize` change that our `sup` / `sub` deltas always carry).
///
/// `transform` composes **around the anchor point**: rotating,
/// scaling, or skewing the transform pivots around `(x, y)` rather
/// than around the screen origin. Concretely, glyphs are laid out
/// with the anchor at glyph-space `(0, 0)` and then transformed by
/// `Affine::translate((x, y)) * transform` — pass
/// `Affine::IDENTITY` for an unrotated placement, or
/// `Affine::rotate(angle)` to rotate the whole block around `(x, y)`.
///
/// `pick_id` is applied to every emitted glyph run. Block-level
/// backgrounds and borders (`draw_rich_text` calls
/// [`RichTextRun::block_paints`] internally) are emitted with
/// [`PickId::Skip`] — decorative geometry beneath the text should not
/// occlude the glyphs' hit response.
#[allow(clippy::too_many_arguments)]
pub fn draw_rich_text<S: SceneBuilder + ?Sized>(
    scene: &mut S,
    run: &RichTextRun,
    x: f64,
    y: f64,
    anchor: RichAnchor,
    transform: Affine,
    pick_id: PickId,
) {
    let layout = run.layout.borrow();
    let bounds = RichTextRun::bounds(&layout);
    let offsets = bounds.resolve(anchor);
    // Compose the anchor + transform so rotating `transform`
    // implicitly rotates around `(x, y)`: place glyphs in
    // glyph-space with the anchor at `(0, 0)` — `g.x - ref_x`,
    // `g.y - ref_y` — and hand parley a `translate(x, y) *
    // transform` outer transform. If `transform = IDENTITY`, the
    // whole thing collapses to a simple translation and the glyphs
    // land at their expected screen positions.
    let final_transform = Affine::translate((x, y)) * transform;

    // ── Block-level auxiliary primitives (backgrounds, borders). ──
    //
    // Emitted before the glyph runs so text draws on top. Paints come
    // back outer-first, so simply iterating in order yields the right
    // z-stack. Every paint's rect lives in parley-layout coordinates
    // — we subtract the anchor offset here so the paint follows the
    // same glyph-space transform as the glyph runs.
    let paints = compute_block_paints(
        &layout,
        &run.blocks,
        &run.palette,
        run.base_size_pt,
        run.dpi,
    );
    for paint in &paints {
        emit_block_paint(scene, paint, offsets, final_transform);
    }

    for line in layout.lines() {
        for item in line.items() {
            let PositionedLayoutItem::GlyphRun(gr) = item else {
                continue;
            };
            let prun = gr.run();
            let font = Font(prun.font().clone());
            let brush_color = gr.style().brush.0;
            let brush = Brush::Solid(brush_color);
            let run_range = prun.text_range();
            let font_size = prun.font_size();

            // Baseline shift for the whole run. parley splits runs at
            // every style boundary that changes a `StyleProperty` we
            // push, which includes the `FontSize` change that our
            // `sup` / `sub` deltas always carry — so each parley run
            // sits inside at most one baseline entry. A custom class
            // that sets `baseline_em` without also changing size
            // wouldn't force a split; that edge case falls back to
            // no shift for v1.
            //
            // Positive `shift_em` means "up" — in screen coordinates
            // (y down) that's a subtraction.
            let shift_em = baseline_shift_for_range(&run.baseline_shifts, &run_range);
            let dy_px = shift_em * font_size;
            let glyphs: Vec<Glyph> = gr
                .positioned_glyphs()
                .map(|g| Glyph {
                    id: g.id,
                    x: g.x - offsets.ref_x,
                    y: g.y - offsets.ref_y - dy_px,
                })
                .collect();
            if glyphs.is_empty() {
                continue;
            }
            let glyph_run = GlyphRun {
                font: &font,
                font_size,
                transform: final_transform,
                glyph_transform: None,
                brush: &brush,
                brush_alpha: 1.0,
                hint: false,
                glyphs: &glyphs,
                style: None,
            };
            scene.draw_glyphs(&glyph_run, pick_id);
        }
    }
}

/// Emit one [`BlockPaint`] into `scene`. `offsets` and `outer` are the
/// same anchor-offset / outer-transform pair used for glyph runs, so
/// block boxes sit under the exact glyph coordinate space the text
/// draws in.
fn emit_block_paint<S: SceneBuilder + ?Sized>(
    scene: &mut S,
    paint: &BlockPaint,
    offsets: super::anchor::AnchorOffsets,
    outer: Affine,
) {
    let rect = kurbo::Rect::new(
        paint.outer_rect.x0 - offsets.ref_x as f64,
        paint.outer_rect.y0 - offsets.ref_y as f64,
        paint.outer_rect.x1 - offsets.ref_x as f64,
        paint.outer_rect.y1 - offsets.ref_y as f64,
    );
    let path = if paint.corner_radius > 0.0 {
        crate::primitives::rounded_rect(rect, paint.corner_radius as f64)
    } else {
        crate::primitives::rect(rect)
    };
    if let Some(color) = paint.background {
        scene.fill(
            crate::path::FillRule::NonZero,
            outer,
            &Brush::Solid(color),
            None,
            &path,
            PickId::Skip,
        );
    }
    if let Some(border) = paint.border {
        let stroke = crate::stroke::Stroke::new(border.width_px as f64);
        scene.stroke(
            &stroke,
            outer,
            &Brush::Solid(border.color),
            None,
            &path,
            PickId::Skip,
        );
    }
}

/// Find the baseline shift whose byte range overlaps `run_range`.
/// Returns `0.0` when no shift applies. Callers rely on parley
/// having already split runs at size boundaries, so every parley run
/// sits inside a single shift entry (or none).
fn baseline_shift_for_range(shifts: &[BaselineRun], run_range: &std::ops::Range<usize>) -> f32 {
    for bs in shifts {
        // Overlap on both ends: run.start < shift.end && shift.start < run.end.
        if run_range.start < bs.range.end && bs.range.start < run_range.end {
            return bs.shift_em;
        }
    }
    0.0
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plot::theme::Palette;
    use crate::scene::recording::{Op, RecordingScene};

    fn palette() -> Palette {
        Palette::new(
            Color::from_rgba8(255, 255, 255, 255),
            Color::from_rgba8(0, 0, 0, 255),
            Color::from_rgba8(51, 105, 232, 255),
        )
    }

    fn base_style() -> TextStyle {
        TextStyle::new(14.0)
    }

    #[test]
    fn plain_text_shapes_and_measures() {
        let sheet = RichTextStyleSheet::new();
        let run = RichTextRun::new(
            "hello world",
            &base_style(),
            Color::from_rgba8(0, 0, 0, 255),
            &sheet,
            &palette(),
            96.0,
        )
        .unwrap();
        assert!(run.natural_width() > 0.0);
        assert!(run.natural_height() > 0.0);
    }

    #[test]
    fn bold_widens_natural_width_vs_plain() {
        // Two runs at the same size, but the bold version has
        // 700-weight glyphs → wider natural width. Sanity-check
        // that per-range weight is actually reaching parley.
        let sheet = RichTextStyleSheet::new();
        let plain = RichTextRun::new(
            "hello world",
            &base_style(),
            Color::from_rgba8(0, 0, 0, 255),
            &sheet,
            &palette(),
            96.0,
        )
        .unwrap();
        let bold = RichTextRun::new(
            "**hello world**",
            &base_style(),
            Color::from_rgba8(0, 0, 0, 255),
            &sheet,
            &palette(),
            96.0,
        )
        .unwrap();
        // Bold glyphs are always at least as wide as regular in every
        // reasonable font; we allow equal because some system fonts
        // synthesise bold via a small horizontal offset that widens
        // by less than 1px at 14pt.
        assert!(
            bold.natural_width() >= plain.natural_width(),
            "bold width {} < plain width {}",
            bold.natural_width(),
            plain.natural_width()
        );
    }

    #[test]
    fn draw_emits_glyph_runs_with_per_range_brushes() {
        // Red span in the middle: expect at least two glyph runs
        // and at least one that uses a red brush.
        let sheet = RichTextStyleSheet::new();
        let run = RichTextRun::new(
            "a {.red word} b",
            &base_style(),
            Color::from_rgba8(0, 0, 0, 255),
            &sheet,
            &palette(),
            96.0,
        )
        .unwrap();
        let mut scene = RecordingScene::default();
        draw_rich_text(
            &mut scene,
            &run,
            0.0,
            0.0,
            RichAnchor::top_left(),
            Affine::IDENTITY,
            PickId::Skip,
        );
        let glyph_runs: Vec<_> = scene
            .ops
            .iter()
            .filter_map(|op| match op {
                Op::DrawGlyphs(gr) => Some(gr),
                _ => None,
            })
            .collect();
        assert!(glyph_runs.len() >= 2, "expected multiple glyph runs");
        let has_red = glyph_runs.iter().any(|gr| match &gr.brush {
            Brush::Solid(c) => {
                let [r, g, b, _] = c.components;
                (r - 1.0).abs() < 1e-3 && g < 0.1 && b < 0.1
            }
            _ => false,
        });
        assert!(has_red, "expected at least one red glyph run");
    }

    #[test]
    fn sup_offsets_glyphs_upward() {
        // The superscript "2" should be emitted with a smaller y
        // (higher on screen) than a baseline glyph.
        let sheet = RichTextStyleSheet::new();
        let run = RichTextRun::new(
            "a ^2^ b",
            &base_style(),
            Color::from_rgba8(0, 0, 0, 255),
            &sheet,
            &palette(),
            96.0,
        )
        .unwrap();
        let mut scene = RecordingScene::default();
        draw_rich_text(
            &mut scene,
            &run,
            0.0,
            100.0,
            RichAnchor::top_left(),
            Affine::IDENTITY,
            PickId::Skip,
        );
        let mut ys: Vec<f32> = Vec::new();
        for op in &scene.ops {
            if let Op::DrawGlyphs(gr) = op {
                for g in &gr.glyphs {
                    ys.push(g.y);
                }
            }
        }
        assert!(!ys.is_empty());
        let min_y = ys.iter().cloned().fold(f32::INFINITY, f32::min);
        let max_y = ys.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        assert!(
            max_y - min_y > 1.0,
            "expected sup to raise its glyphs; span was {min_y}..{max_y}"
        );
    }

    #[test]
    fn measure_impl_reports_positive_height() {
        let sheet = RichTextStyleSheet::new();
        let run = RichTextRun::new(
            "hello **bold** world",
            &base_style(),
            Color::from_rgba8(0, 0, 0, 255),
            &sheet,
            &palette(),
            96.0,
        )
        .unwrap();
        let h = run.height_at(200.0, 96.0);
        assert!(h > 0.0);
    }

    #[test]
    fn base_brush_flows_through_to_plain_runs() {
        let sheet = RichTextStyleSheet::new();
        let base_col = Color::from_rgba8(50, 100, 200, 255);
        let run = RichTextRun::new(
            "plain text",
            &base_style(),
            base_col,
            &sheet,
            &palette(),
            96.0,
        )
        .unwrap();
        let mut scene = RecordingScene::default();
        draw_rich_text(
            &mut scene,
            &run,
            0.0,
            0.0,
            RichAnchor::top_left(),
            Affine::IDENTITY,
            PickId::Skip,
        );
        let first = scene
            .ops
            .iter()
            .find_map(|op| match op {
                Op::DrawGlyphs(gr) => Some(gr),
                _ => None,
            })
            .expect("glyph run");
        match &first.brush {
            Brush::Solid(c) => {
                let [r, g, b, _] = c.components;
                assert!((r - 50.0 / 255.0).abs() < 1e-2);
                assert!((g - 100.0 / 255.0).abs() < 1e-2);
                assert!((b - 200.0 / 255.0).abs() < 1e-2);
            }
            _ => panic!("expected solid brush"),
        }
    }

    #[test]
    fn center_anchor_shifts_glyph_positions_by_half_width() {
        // Drawn with `RichAnchor::center()` at (100, 100), the glyphs
        // should straddle 100 in x — the leftmost glyph sits at ~
        // (100 - width/2), the rightmost at ~ (100 + width/2).
        let sheet = RichTextStyleSheet::new();
        let run = RichTextRun::new(
            "abcdef",
            &base_style(),
            Color::from_rgba8(0, 0, 0, 255),
            &sheet,
            &palette(),
            96.0,
        )
        .unwrap();
        let width = run.natural_width() as f32;
        let mut scene_center = RecordingScene::default();
        draw_rich_text(
            &mut scene_center,
            &run,
            100.0,
            100.0,
            RichAnchor::center(),
            Affine::IDENTITY,
            PickId::Skip,
        );
        // Take absolute glyph x's after the identity outer transform:
        // Affine::translate(100, 100) applied to (g.x - width/2).
        // We inspect the first glyph — should sit at ~100 - width/2.
        let first_op = scene_center
            .ops
            .iter()
            .find_map(|op| match op {
                Op::DrawGlyphs(gr) => Some(gr),
                _ => None,
            })
            .unwrap();
        // Because the transform was baked at the SceneBuilder level
        // (not into glyph coords), we look at the transform + first
        // glyph position combined.
        let first_g = first_op.glyphs.first().unwrap();
        let transformed_x = first_op.transform.as_coeffs()[4] as f32 + first_g.x;
        assert!(
            (transformed_x - (100.0 - width * 0.5)).abs() < 2.0,
            "first glyph should sit near (x - width/2), got {transformed_x} (expected ~{})",
            100.0 - width * 0.5
        );
    }

    fn last_glyph_abs_xy(scene: &RecordingScene) -> Option<(f32, f32)> {
        for op in scene.ops.iter().rev() {
            if let Op::DrawGlyphs(gr) = op {
                if let Some(g) = gr.glyphs.last() {
                    let coeffs = gr.transform.as_coeffs();
                    let a = coeffs[0] as f32;
                    let b = coeffs[1] as f32;
                    let c = coeffs[2] as f32;
                    let d = coeffs[3] as f32;
                    let tx = coeffs[4] as f32;
                    let ty = coeffs[5] as f32;
                    return Some((a * g.x + c * g.y + tx, b * g.x + d * g.y + ty));
                }
            }
        }
        None
    }

    #[test]
    fn rotation_pivots_around_anchor() {
        // A 180° rotation with `top_left` anchor at (100, 100) should
        // put the LAST glyph — which normally sits to the right of the
        // anchor — to the LEFT of the anchor after the flip.
        let sheet = RichTextStyleSheet::new();
        let run = RichTextRun::new(
            "abcdefghij",
            &base_style(),
            Color::from_rgba8(0, 0, 0, 255),
            &sheet,
            &palette(),
            96.0,
        )
        .unwrap();
        let mut scene_up = RecordingScene::default();
        draw_rich_text(
            &mut scene_up,
            &run,
            100.0,
            100.0,
            RichAnchor::top_left(),
            Affine::IDENTITY,
            PickId::Skip,
        );
        let mut scene_flipped = RecordingScene::default();
        draw_rich_text(
            &mut scene_flipped,
            &run,
            100.0,
            100.0,
            RichAnchor::top_left(),
            Affine::rotate(std::f64::consts::PI),
            PickId::Skip,
        );
        let (up_x, _) = last_glyph_abs_xy(&scene_up).expect("upright glyph");
        let (flipped_x, _) = last_glyph_abs_xy(&scene_flipped).expect("flipped glyph");
        assert!(
            up_x > 105.0,
            "upright last glyph should sit clearly right of the anchor, got {up_x}"
        );
        assert!(
            flipped_x < 95.0,
            "180°-rotated last glyph should sit clearly left of the anchor, got {flipped_x}"
        );
        // Symmetry: |up_x - 100| ≈ |100 - flipped_x|.
        let up_offset = up_x - 100.0;
        let flipped_offset = 100.0 - flipped_x;
        assert!(
            (up_offset - flipped_offset).abs() < 1.5,
            "rotation should be symmetric around anchor; up_offset={up_offset}, flipped_offset={flipped_offset}"
        );
    }

    #[test]
    fn first_line_anchor_places_baseline_on_y() {
        // A `first_line_baseline` anchor at (0, 100) places the first
        // line's baseline at screen y = 100. parley emits glyphs with
        // y = layout-baseline on the baseline itself, so the emitted
        // glyph struct's y should collapse to 0 in glyph space after
        // we subtract the first-line baseline.
        let sheet = RichTextStyleSheet::new();
        let run = RichTextRun::new(
            "hi",
            &base_style(),
            Color::from_rgba8(0, 0, 0, 255),
            &sheet,
            &palette(),
            96.0,
        )
        .unwrap();
        let mut scene = RecordingScene::default();
        draw_rich_text(
            &mut scene,
            &run,
            0.0,
            100.0,
            RichAnchor::first_line_baseline(),
            Affine::IDENTITY,
            PickId::Skip,
        );
        let first = scene
            .ops
            .iter()
            .find_map(|op| match op {
                Op::DrawGlyphs(gr) => Some(gr),
                _ => None,
            })
            .unwrap();
        let coeffs = first.transform.as_coeffs();
        assert!(
            (coeffs[4]).abs() < 1e-3,
            "expected tx = 0, got {}",
            coeffs[4]
        );
        assert!(
            (coeffs[5] - 100.0).abs() < 1e-3,
            "expected ty = 100, got {}",
            coeffs[5]
        );
        // Screen y for a baseline glyph = ty + g.y. With
        // first-line-baseline anchoring, that must equal exactly y.
        let g = first.glyphs.first().unwrap();
        let screen_y = coeffs[5] as f32 + g.y;
        assert!(
            (screen_y - 100.0).abs() < 0.5,
            "first baseline should land at y = 100; got screen y = {screen_y}"
        );
    }

    #[test]
    fn new_with_width_wraps_at_fixed_pixels() {
        // Long enough that natural width is way past 60 px.
        let sheet = RichTextStyleSheet::new();
        let unwrapped = RichTextRun::new(
            "one two three four five six seven eight",
            &base_style(),
            Color::from_rgba8(0, 0, 0, 255),
            &sheet,
            &palette(),
            96.0,
        )
        .unwrap();
        let wrapped = RichTextRun::new_with_width(
            "one two three four five six seven eight",
            &base_style(),
            Color::from_rgba8(0, 0, 0, 255),
            &sheet,
            &palette(),
            96.0,
            RichTextWidth::Fixed(60.0),
        )
        .unwrap();
        assert!(
            wrapped.current_height() > unwrapped.current_height(),
            "wrapped height {} should exceed unwrapped {} because wrapping produces more lines",
            wrapped.current_height(),
            unwrapped.current_height()
        );
    }

    #[test]
    fn heading_produces_larger_glyph_run_font_size() {
        let sheet = RichTextStyleSheet::new();
        let plain = RichTextRun::new(
            "Big",
            &base_style(),
            Color::from_rgba8(0, 0, 0, 255),
            &sheet,
            &palette(),
            96.0,
        )
        .unwrap();
        let heading = RichTextRun::new(
            "# Big",
            &base_style(),
            Color::from_rgba8(0, 0, 0, 255),
            &sheet,
            &palette(),
            96.0,
        )
        .unwrap();
        // Extract the max font_size across emitted glyph runs from
        // each; the heading version should have a larger max because
        // h1 pushes a Rel(2.0) size delta.
        let mut scene_plain = RecordingScene::default();
        draw_rich_text(
            &mut scene_plain,
            &plain,
            0.0,
            0.0,
            RichAnchor::top_left(),
            Affine::IDENTITY,
            PickId::Skip,
        );
        let mut scene_h = RecordingScene::default();
        draw_rich_text(
            &mut scene_h,
            &heading,
            0.0,
            0.0,
            RichAnchor::top_left(),
            Affine::IDENTITY,
            PickId::Skip,
        );
        let max_size = |s: &RecordingScene| {
            s.ops
                .iter()
                .filter_map(|op| match op {
                    Op::DrawGlyphs(gr) => Some(gr.font_size),
                    _ => None,
                })
                .fold(0.0_f32, f32::max)
        };
        let plain_sz = max_size(&scene_plain);
        let h_sz = max_size(&scene_h);
        assert!(
            h_sz > plain_sz * 1.5,
            "h1 font size should be > 1.5× plain (plain={plain_sz}, h={h_sz})"
        );
    }

    #[test]
    fn size_selector_produces_larger_run_height() {
        let sheet = RichTextStyleSheet::new();
        let plain = RichTextRun::new(
            "x",
            &base_style(),
            Color::from_rgba8(0, 0, 0, 255),
            &sheet,
            &palette(),
            96.0,
        )
        .unwrap();
        let big = RichTextRun::new(
            "{.36 x}",
            &base_style(),
            Color::from_rgba8(0, 0, 0, 255),
            &sheet,
            &palette(),
            96.0,
        )
        .unwrap();
        assert!(
            big.natural_height() > plain.natural_height(),
            "36pt should be taller than base 14pt (plain={}, big={})",
            plain.natural_height(),
            big.natural_height()
        );
    }

    #[test]
    fn code_block_emits_fill_before_glyphs() {
        // The `code_block` sheet default carries a background — the
        // recording scene should show a Fill op before any DrawGlyphs.
        let sheet = RichTextStyleSheet::new();
        let run = RichTextRun::new(
            "```\nlet x = 1;\n```",
            &base_style(),
            Color::from_rgba8(0, 0, 0, 255),
            &sheet,
            &palette(),
            96.0,
        )
        .unwrap();
        let mut scene = RecordingScene::default();
        draw_rich_text(
            &mut scene,
            &run,
            0.0,
            0.0,
            RichAnchor::top_left(),
            Affine::IDENTITY,
            PickId::Skip,
        );
        let first_fill = scene
            .ops
            .iter()
            .position(|op| matches!(op, Op::Fill { .. }));
        let first_glyphs = scene
            .ops
            .iter()
            .position(|op| matches!(op, Op::DrawGlyphs(_)));
        let (fi, gi) = (
            first_fill.expect("expected a Fill op"),
            first_glyphs.expect("expected a DrawGlyphs op"),
        );
        assert!(
            fi < gi,
            "code_block background (Fill at {fi}) should come before glyphs (DrawGlyphs at {gi})"
        );
    }

    #[test]
    fn plain_text_emits_no_block_paints() {
        let sheet = RichTextStyleSheet::new();
        let run = RichTextRun::new(
            "just a plain paragraph",
            &base_style(),
            Color::from_rgba8(0, 0, 0, 255),
            &sheet,
            &palette(),
            96.0,
        )
        .unwrap();
        let mut scene = RecordingScene::default();
        draw_rich_text(
            &mut scene,
            &run,
            0.0,
            0.0,
            RichAnchor::top_left(),
            Affine::IDENTITY,
            PickId::Skip,
        );
        let fill_count = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Fill { .. }))
            .count();
        let stroke_count = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Stroke { .. }))
            .count();
        assert_eq!(
            fill_count, 0,
            "plain paragraph should not emit any Fill ops"
        );
        assert_eq!(
            stroke_count, 0,
            "plain paragraph should not emit any Stroke ops"
        );
    }
}
