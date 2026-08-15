//! [`RichTextRun`] — a stack of per-block parley layouts plus the
//! metadata the wrap and draw passes read.
//!
//! **One parley layout per top-level leaf.** Paragraph, heading,
//! code-block, and rule blocks each shape as their own
//! `parley::Layout<RichBrush>` at their effective content width —
//! the outer max width minus any ancestor container `padding.left` /
//! `padding.right` (blockquote / div) minus this block's own
//! hanging or first-line indent. Blocks stack vertically ourselves;
//! see [`stack_blocks`] for the margin-collapse walk.
//!
//! **Everything is already resolved.** The reducer hands over a
//! [`ResolvedStyle`] per block and per inline run with every length
//! in points, so this layer only converts pt → px through `dpi`.
//! Nothing is implemented via whitespace injection — indents are
//! true pixel offsets applied at position / draw time.
//!
//! **Hanging.** A list item body with `hanging = 1.5em` shapes at
//! `content_width - 1.5em` and its continuation lines get shifted
//! right by 1.5em at draw time.
//!
//! **First-line indent.** A paragraph with `indent = 2em` shapes at
//! `content_width - 2em` and its first-line glyphs shift right by
//! 2em at draw time. The first line's effective usable width is
//! reduced by 2em — a small compromise for parley's single
//! shape-width limitation.
//!
//! **Nested containers.** A leaf inside a blockquote gets the
//! blockquote's `padding.left` added to its own left indent. Nested
//! blockquotes stack additively.
//!
//! **List items are containers.** The reducer emits every `ListItem`
//! as a container wrapping a synthetic Paragraph leaf that holds the
//! item's body, and hangs the item's marker off the `ListItem` block
//! for [`MarkerLayout`] to shape into the list's start gutter.
//!
//! **Baseline shifts** (`sup` / `sub`) live in a parallel
//! `Vec<BaselineRun>` per block layout (parley has no baseline-shift
//! `StyleProperty`). At draw time each parley run's shift is applied
//! per-run based on its byte range within the block-local text.

use std::cell::RefCell;
use std::ops::Range;

use parley::Alignment;

use super::anchor::LayoutBounds;
use super::block::{compute_block_paints, BlockPaint};
use super::draw::marker_x_range;
use super::parser::parse;
use super::reduce::{reduce, BaselineRun, Block, BlockKind, InlineRun};
use super::shape::shape_run;
use super::style::{ResolvedStyle, RichTextStyleSheet};
use super::wrap::EdgeSpacing;
use crate::color::Color;
use crate::layout::{Measure, WidthHint};
use crate::style_vocab::{HAlign, Palette};
use crate::text::TextStyle;

// ─── Brush newtype ──────────────────────────────────────────────────────────

/// Newtype wrapping [`Color`] so it satisfies parley's `Brush` trait
/// bounds (`Clone + PartialEq + Default + Debug`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RichBrush(pub Color);

impl Default for RichBrush {
    /// Opaque black — the default text fill when no colour resolves.
    fn default() -> Self {
        RichBrush(Color::from_rgba8(0, 0, 0, 255))
    }
}

// ─── RichTextWidth ──────────────────────────────────────────────────────────

/// Wrap-width policy for a rich-text block. Mirrors marquee's
/// `marquee_grob(width = ...)` argument.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub enum RichTextWidth {
    /// Natural (no soft-wrap) width. Matches marquee's `NA`.
    #[default]
    Natural,
    /// Wrap at this pixel width. Matches marquee's numeric argument.
    Fixed(f32),
}

// ─── MarkerLayout ───────────────────────────────────────────────────────────

/// A shaped list-item marker, placed in the list's start gutter
/// rather than in the item's text flow. Keeping it out of the flow is
/// what lets multi-digit ordinals right-align on their period and
/// keeps a justified item from stretching the space after its bullet.
pub(crate) struct MarkerLayout {
    /// The marker's own single-line layout.
    pub(crate) layout: parley::Layout<RichBrush>,
    /// Shaped width (px) — the marker's right edge sits `gap_px` to
    /// the start side of the block's content edge.
    pub(crate) width_px: f32,
    /// Space (px) between the marker and the content edge.
    pub(crate) gap_px: f32,
}

// ─── BlockLayout ────────────────────────────────────────────────────────────

/// One top-level leaf block's shaped layout + geometry, in the
/// [`RichTextRun`]'s local coordinate space (top-left = `(0, 0)`).
///
/// Vertical: `y_px` is the top of the block's shaped content.
/// Horizontal: `left_px` is the left edge of the shaped content
/// (already inset by ancestor and own left padding).
pub(crate) struct BlockLayout {
    /// The parley layout shaped for this block's text.
    pub(crate) layout: parley::Layout<RichBrush>,
    /// Baseline shifts, byte ranges relative to the block-local text.
    pub(crate) baseline_shifts: Vec<BaselineRun>,
    /// Byte range in the full reduced text this block covers.
    pub(crate) text_range: Range<usize>,
    /// Block kind (Paragraph / Heading / ListItem / CodeBlock).
    pub(crate) kind: BlockKind,
    /// Resolved style for the block, lengths in points.
    pub(crate) style: ResolvedStyle,
    /// Left edge of the block's shaped content (px).
    pub(crate) left_px: f32,
    /// Right inset from the outer width (px) — the block's usable
    /// content-width is `outer - left_px - right_inset_px`.
    pub(crate) right_inset_px: f32,
    /// Shape width the parley layout was broken at (px).
    pub(crate) shape_width_px: f32,
    /// Additional first-line-only right shift (px). Applied to line-0
    /// glyphs at draw time.
    pub(crate) first_line_shift_px: f32,
    /// Additional continuation-line right shift (px). Applied to
    /// line-1+ glyphs at draw time.
    pub(crate) continuation_shift_px: f32,
    /// Top y (px) of the block's shaped content.
    pub(crate) y_px: f32,
    /// Height (px) of the shaped layout.
    pub(crate) height_px: f32,
    /// Vertical spacing above the block's content, outermost box
    /// first. Each entry is one enclosing box this leaf opens, ending
    /// with the leaf's own.
    pub(crate) top_chain: Vec<EdgeSpacing>,
    /// Vertical spacing below the block's content, innermost box
    /// first — the leaf's own entry, then each enclosing box it
    /// closes.
    pub(crate) bottom_chain: Vec<EdgeSpacing>,
    /// Own top padding (px) applied inside the block's outer rect.
    pub(crate) padding_top_px: f32,
    /// Own right padding (px), already resolved to a physical side.
    pub(crate) padding_right_px: f32,
    /// Own bottom padding (px) applied inside the block's outer rect.
    pub(crate) padding_bottom_px: f32,
    /// Own left padding (px), already resolved to a physical side.
    pub(crate) padding_left_px: f32,
    /// Block-level horizontal alignment resolved from own +
    /// ancestor `align`. `None` = fall back to the caller-supplied
    /// alignment at re-break time (matches the outer's opinion).
    pub(crate) alignment_override: Option<Alignment>,
    /// Resolved block-axis direction: `true` = Rtl, `false` = Ltr.
    /// Sourced from the first non-`None` `text_direction` in the
    /// ancestor chain (child wins via overlay); for `Direction::Auto`
    /// or an unset field, this reads back
    /// [`parley::Layout::is_rtl()`] after shaping. Under Rtl the
    /// physical-side interpretation of block-level `padding` /
    /// `margin` / `border_width` is flipped, `HAlign::Start` /
    /// `End` map to physical Right / Left, and first-line / hanging
    /// indent apply from the right edge instead of the left.
    pub(crate) is_rtl: bool,
    /// Asymmetric-shift companion layout. `Some` when
    /// `first_line_shift_px != continuation_shift_px` and the block
    /// has more than one line's worth of content:
    ///   - `layout` above shapes the WHOLE content at the first
    ///     line's usable width (`block - first_line_shift`). Only
    ///     its first line is drawn.
    ///   - `continuation_layout` shapes the remainder (after the
    ///     first line's byte range) at the continuation's usable
    ///     width (`block - continuation_shift`). All its lines are
    ///     drawn.
    ///
    /// This lets first-line and continuation lines each reach the
    /// block's right edge, matching CSS hanging / text-indent
    /// semantics that a single-width parley layout can't achieve.
    pub(crate) continuation_layout: Option<parley::Layout<RichBrush>>,
    /// Baseline shifts for `continuation_layout` — byte ranges
    /// rebased to that layout's local text.
    pub(crate) continuation_baseline_shifts: Vec<BaselineRun>,
    /// Inline runs for `continuation_layout` — ranges rebased to
    /// that layout's local text. Needed at draw time so span
    /// backgrounds / borders / outlines apply on continuation
    /// lines too.
    pub(crate) continuation_inlines: Vec<InlineRun>,
    /// Height (px) of the first line only. Used to y-position the
    /// continuation layout when `continuation_layout` is Some.
    pub(crate) first_line_height_px: f32,
    /// Cached block-local source, used to re-shape when
    /// `set_max_width` produces an asymmetric-shift split. Empty
    /// text / vecs when the block never needs re-shape (e.g. hr
    /// blocks).
    pub(crate) source_text: String,
    /// Inline runs over `source_text`, in block-local byte ranges.
    pub(crate) source_inlines: Vec<InlineRun>,
    /// Baseline shifts over `source_text`, in block-local byte ranges.
    pub(crate) source_baselines: Vec<BaselineRun>,
    /// List-item marker, on the leaf that opens the item's body.
    pub(crate) marker: Option<MarkerLayout>,
}

impl BlockLayout {
    /// Outer rect (including own padding) in RichTextRun local coords.
    /// Used by the paint pass to draw backgrounds / borders.
    pub(crate) fn outer_rect(&self) -> kurbo::Rect {
        let x0 = self.left_px - self.padding_left_px;
        let y0 = self.y_px - self.padding_top_px;
        let x1 = self.left_px + self.shape_width_px + self.padding_right_px;
        let y1 = self.y_px + self.height_px + self.padding_bottom_px;
        kurbo::Rect::new(x0 as f64, y0 as f64, x1 as f64, y1 as f64)
    }
}

/// Values a run derives from its current break. Recomputing them per
/// draw would walk every block two or three more times.
pub(crate) struct Derived {
    /// Aggregate bounds across every block layout.
    pub(crate) bounds: LayoutBounds,
    /// Per-block background / border instructions, outer-first.
    pub(crate) paints: Vec<BlockPaint>,
}

// ─── RichTextRun ────────────────────────────────────────────────────────────

/// Shaped rich text — a stack of per-block parley layouts plus the
/// container blocks and metadata the draw pass needs.
pub struct RichTextRun {
    /// One shaped layout per top-level leaf, in document order.
    /// `RefCell` so `set_max_width` can re-break in place.
    pub(crate) blocks: RefCell<Vec<BlockLayout>>,
    /// Non-leaf containers (BlockQuote / Div / List) — for the paint
    /// pass. Stored in emission (close) order.
    pub(crate) containers: Vec<Block>,
    /// Base text style (captured at construction for re-shape).
    pub(crate) base_style: TextStyle,
    /// Palette used to resolve `ThemeColor` values on paints.
    pub(crate) palette: Palette,
    /// Base brush colour captured at construction — used when re-
    /// shaping asymmetric blocks in [`Self::set_max_width`].
    pub(crate) base_brush: Color,
    /// DPI captured at construction.
    pub(crate) dpi: f64,
    /// The `(width, alignment)` the blocks are currently broken to.
    /// `None` = the natural break from shaping. `set_max_width`
    /// early-returns when the request matches, which is what makes
    /// the layout solver's repeated probes cheap.
    pub(crate) last_break: RefCell<Option<(f32, HAlign)>>,
    /// Bounds and paints derived from the current break. Invalidated
    /// whenever the blocks move.
    pub(crate) derived: RefCell<Option<Derived>>,
    /// Cached natural width (px).
    pub(crate) natural_width_px: f32,
    /// Natural stacked height (px) at the unwrapped width. Fixed at
    /// construction — re-breaking narrows the run, it doesn't change
    /// what the natural layout was.
    pub(crate) natural_height_px: f32,
    /// Stacked height (px) at the width the blocks are currently
    /// broken to.
    pub(crate) current_height_px: RefCell<f32>,
    /// Cached min-content width (px).
    pub(crate) min_width_px: f32,
}

impl RichTextRun {
    /// Parse `source` as marquee-flavoured markdown, resolve every
    /// span through `sheet` + `palette`, and shape at natural width.
    pub fn new(
        source: &str,
        base_style: &TextStyle,
        base_brush: Color,
        sheet: &RichTextStyleSheet,
        palette: &Palette,
        dpi: f64,
    ) -> Self {
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

    /// Like [`Self::new`] but with an explicit wrap-width policy.
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_width(
        source: &str,
        base_style: &TextStyle,
        base_brush: Color,
        sheet: &RichTextStyleSheet,
        palette: &Palette,
        dpi: f64,
        width: RichTextWidth,
    ) -> Self {
        let events = parse(source);
        let base = ResolvedStyle::from_base(base_style);
        let runs = reduce(&events, sheet, &base);
        let r = shape_run(runs, base_style, base_brush, palette, dpi);
        if let RichTextWidth::Fixed(px) = width {
            r.set_max_width(px, HAlign::Start);
        }
        r
    }
    /// Natural (unwrapped) width in pixels.
    pub fn natural_width(&self) -> f64 {
        self.natural_width_px as f64
    }

    /// Natural (unwrapped) total stacked height in pixels. Unchanged
    /// by [`Self::set_max_width`].
    pub fn natural_height(&self) -> f64 {
        self.natural_height_px as f64
    }

    /// Total stacked height (px) at the current break width —
    /// includes any margins on the last block below its content.
    pub fn current_height(&self) -> f64 {
        *self.current_height_px.borrow() as f64
    }

    /// Current effective content width (px) — the max of every
    /// block's `left + shape_width + right`. Equal to the wrap width
    /// after [`Self::set_max_width`]; equal to the natural width
    /// otherwise.
    pub fn content_width(&self) -> f64 {
        self.layout_bounds().width as f64
    }
    /// Compute per-block paint instructions (backgrounds + borders on
    /// both leaves and containers).
    pub fn block_paints(&self) -> Vec<BlockPaint> {
        self.with_derived(|d| d.paints.clone())
    }

    /// Aggregate bounds across every block layout. Used by
    /// [`RichAnchor`](super::anchor::RichAnchor) resolution.
    pub fn layout_bounds(&self) -> LayoutBounds {
        self.with_derived(|d| d.bounds)
    }

    /// Run `f` against the values derived from the current break,
    /// computing them first if the blocks have moved since last time.
    pub(crate) fn with_derived<T>(&self, f: impl FnOnce(&Derived) -> T) -> T {
        {
            let cached = self.derived.borrow();
            if let Some(d) = cached.as_ref() {
                return f(d);
            }
        }
        let derived = Derived {
            bounds: self.compute_layout_bounds(),
            paints: compute_block_paints(self),
        };
        let out = f(&derived);
        *self.derived.borrow_mut() = Some(derived);
        out
    }

    fn compute_layout_bounds(&self) -> LayoutBounds {
        let blocks = self.blocks.borrow();
        if blocks.is_empty() {
            return LayoutBounds {
                width: 0.0,
                height: 0.0,
                ink_left: 0.0,
                ink_right: 0.0,
                ink_top: 0.0,
                ink_bottom: 0.0,
                first_baseline: 0.0,
                last_baseline: 0.0,
            };
        }
        let mut ink_left = f32::INFINITY;
        let mut ink_right = f32::NEG_INFINITY;
        let mut ink_top = f32::INFINITY;
        let mut ink_bottom = f32::NEG_INFINITY;
        let mut first_baseline: Option<f32> = None;
        let mut last_baseline: f32 = 0.0;
        let mut total_width: f32 = 0.0;
        let mut total_height: f32 = 0.0;
        for bl in blocks.iter() {
            total_width = total_width.max(bl.left_px + bl.shape_width_px + bl.right_inset_px);
            total_height = total_height.max(bl.y_px + bl.height_px + bl.padding_bottom_px);
            if let Some(marker) = &bl.marker {
                let (m_x0, m_x1) = marker_x_range(bl, marker);
                ink_left = ink_left.min(m_x0);
                ink_right = ink_right.max(m_x1);
            }
            for (line_index, line) in bl.layout.lines().enumerate() {
                let m = line.metrics();
                let shift = if line_index == 0 {
                    bl.first_line_shift_px
                } else {
                    bl.continuation_shift_px
                };
                let x_off = bl.left_px + shift;
                let y_off = bl.y_px;
                ink_left = ink_left.min(m.inline_min_coord + x_off);
                ink_right = ink_right.max(m.inline_max_coord + x_off);
                ink_top = ink_top.min(m.block_min_coord + y_off);
                ink_bottom = ink_bottom.max(m.block_max_coord + y_off);
                if first_baseline.is_none() {
                    first_baseline = Some(m.baseline + y_off);
                }
                last_baseline = m.baseline + y_off;
            }
        }
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
            width: total_width,
            height: total_height,
            ink_left,
            ink_right,
            ink_top,
            ink_bottom,
            first_baseline: first_baseline.unwrap_or(0.0),
            last_baseline,
        }
    }
}

impl Measure for RichTextRun {
    fn width_hint(&self, _dpi: f64) -> WidthHint {
        WidthHint::Min(self.min_width_px as f64)
    }

    fn height_at(&self, width: f64, _dpi: f64) -> f64 {
        self.set_max_width(width as f32, HAlign::Start) as f64
    }
}
