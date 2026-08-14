//! Shape marquee-flavoured markdown into a stack of per-block parley
//! layouts and draw them through `SceneBuilder`.
//!
//! **One parley layout per top-level leaf.** Paragraph, heading,
//! list-item, and code-block blocks each shape as their own
//! `parley::Layout<RichBrush>` at their effective content width —
//! the outer max width minus any ancestor container `padding.left` /
//! `padding.right` (blockquote / div) minus this block's own
//! hanging or first-line indent. Blocks stack vertically ourselves:
//! the layout pass accumulates y through `margin_top`, the block's
//! own `padding.top`, its height, its `padding.bottom`, and
//! `margin_bottom`.
//!
//! **Font-aware indents.** Every indent / padding / margin value
//! lives in pt (`Length::Rel` / `Abs`), resolved against
//! `base_style.size_pt` and converted to px through `dpi`. Nothing is
//! implemented via whitespace injection — indents are true pixel
//! offsets applied at position / draw time.
//!
//! **Hanging.** A list item with `hanging = 1.5em` shapes at
//! `content_width - 1.5em` and its continuation lines get shifted
//! right by 1.5em at draw time. The first-line marker (which the
//! reducer prepends) sits at the left, matching a classic hanging
//! indent.
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
//! item's own text (bullet marker + body). Nested lists inside an
//! item decompose into their own paragraph leaves. Each such nested
//! leaf's ancestor chain includes the outer item, so the outer's
//! `hanging` contributes to the nested leaf's `left_px` — the
//! nested bullet sits at exactly the outer's continuation position.
//! An item's own body ("first descendant leaf" of the item container)
//! instead absorbs the item's hanging as its `continuation_shift`,
//! giving the classic outdent effect where the bullet sits at the
//! outdent and wrapped body lines sit under the body.
//!
//! **Baseline shifts** (`sup` / `sub`) live in a parallel
//! `Vec<BaselineRun>` per block layout (parley has no baseline-shift
//! `StyleProperty`). At draw time each parley run's shift is applied
//! per-run based on its byte range within the block-local text.

use std::cell::RefCell;
use std::ops::Range;

use parley::{
    Alignment, AlignmentOptions, FontFamily, FontFamilyName, FontStyle, FontWeight, GenericFamily,
    LayoutContext, PositionedLayoutItem, StyleProperty,
};

use super::anchor::{LayoutBounds, RichAnchor};
use super::block::{compute_block_paints, BlockPaint};
use super::parser::{parse, ParseError};
use super::reduce::{reduce, BaselineRun, Block, BlockKind, BuiltRuns, InlineRun};
use super::style::{RichTextStyleSheet, StyleDelta};
use crate::brush::Brush;
use crate::color::Color;
use crate::geometry::Affine;
use crate::layout::{Measure, WidthHint};
use crate::pick::PickId;
use crate::plot::theme::{Length, Margin, Palette};
use crate::scene::{Font, Glyph, GlyphRun, SceneBuilder};
use crate::text::{FontFamilyEntry, GenericFamilyKind, LineHeight, TextStyle};

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
    #[allow(dead_code)]
    pub(crate) kind: BlockKind,
    /// Resolved style delta for the block.
    pub(crate) delta: StyleDelta,
    /// Container depth (0 = top-level).
    #[allow(dead_code)]
    pub(crate) depth: usize,
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
    /// Top margin (px) applied above `y_px`.
    pub(crate) margin_top_px: f32,
    /// Bottom margin (px) applied after `y_px + height_px`.
    pub(crate) margin_bottom_px: f32,
    /// Own padding (px, trbl) applied inside the block's outer rect.
    pub(crate) padding_top_px: f32,
    pub(crate) padding_right_px: f32,
    pub(crate) padding_bottom_px: f32,
    pub(crate) padding_left_px: f32,
    /// Extra top space contributed by ancestor containers whose
    /// first descendant this leaf is (their `padding.top` +
    /// `margin.top` fold into this value). Non-collapsing barrier
    /// that sits above the leaf's shaped content but inside any
    /// container-level paint.
    pub(crate) extra_top_px: f32,
    /// Symmetric bottom counterpart.
    pub(crate) extra_bottom_px: f32,
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

// ─── RichTextRun ────────────────────────────────────────────────────────────

/// Shaped rich text — a stack of per-block parley layouts plus the
/// container blocks and metadata the draw pass needs.
pub struct RichTextRun {
    /// Reduced source text. Retained for slicing on re-shape if a
    /// caller ever inspects it.
    #[allow(dead_code)]
    pub(crate) text: String,
    /// One shaped layout per top-level leaf, in document order.
    /// `RefCell` so `set_max_width` can re-break in place.
    pub(crate) blocks: RefCell<Vec<BlockLayout>>,
    /// Non-leaf containers (BlockQuote / Div / List) — for the paint
    /// pass. Stored in emission (close) order.
    pub(crate) containers: Vec<Block>,
    /// Base text style (captured at construction for re-shape).
    #[allow(dead_code)]
    pub(crate) base_style: TextStyle,
    /// Palette used to resolve `ThemeColor` values on paints.
    pub(crate) palette: Palette,
    /// Convenience mirror of `base_style.size_pt`.
    pub(crate) base_size_pt: f32,
    /// DPI captured at construction.
    pub(crate) dpi: f64,
    /// Last requested wrap width; `None` = natural.
    #[allow(dead_code)]
    last_break_width: RefCell<Option<f32>>,
    /// Cached natural width (px).
    natural_width_px: f32,
    /// Cached natural stacked height (px).
    natural_height_px: RefCell<f32>,
    /// Cached min-content width (px).
    min_width_px: f32,
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
    ) -> Result<Self, ParseError> {
        let events = parse(source)?;
        let runs = reduce(&events, sheet);
        let r = Self::shape(runs, base_style, base_brush, palette, dpi);
        if let RichTextWidth::Fixed(px) = width {
            r.set_max_width(px, Alignment::Start);
        }
        Ok(r)
    }

    /// Alternative constructor from a pre-built [`BuiltRuns`].
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
        // Split blocks into leaves + containers. Leaves are the shape
        // units.
        let all_blocks = runs.blocks;
        let mut leaves: Vec<Block> = all_blocks
            .iter()
            .filter(|b| is_leaf_kind(&b.kind))
            .filter(|b| !contained_in_another_leaf(b, &all_blocks))
            .cloned()
            .collect();
        leaves.sort_by_key(|b| b.range.start);
        let containers: Vec<Block> = all_blocks
            .into_iter()
            .filter(|b| !is_leaf_kind(&b.kind))
            .collect();

        // Shape each leaf; position vertically.
        let base_pt = base_style.size_pt as f64;
        // Precompute first + last descendant-leaf ranges per
        // container. First: for ListItem hanging routing (item's
        // first-body-leaf gets hanging as continuation_shift; deeper
        // content gets it as left_px). Also for spacing: the first-
        // descendant leaf absorbs the container's `padding.top` /
        // `margin.top` contribution, the last absorbs the `.bottom`
        // pair.
        let container_first_last: Vec<(Range<usize>, Range<usize>, Range<usize>)> = containers
            .iter()
            .filter_map(|c| {
                let contained: Vec<&Block> = leaves
                    .iter()
                    .filter(|l| l.range.start >= c.range.start && l.range.end <= c.range.end)
                    .collect();
                let first = contained.iter().min_by_key(|l| l.range.start)?;
                let last = contained.iter().max_by_key(|l| l.range.start)?;
                Some((c.range.clone(), first.range.clone(), last.range.clone()))
            })
            .collect();
        let mut layouts: Vec<BlockLayout> = Vec::with_capacity(leaves.len());
        // CSS-style margin-collapsing accumulator. Vertical margins
        // between adjacent flow blocks collapse per marquee's
        // documented `marquee_grob` rules ("Margin calculations
        // follows the margin collapsing rules of HTML"):
        // - Two adjacent positive margins collapse to their `max`.
        // - Two adjacent negative margins collapse to the `min` (most
        //   negative).
        // - Mixed: `max_positive + min_negative`.
        // - Padding / border between two margins breaks the collapse.
        //
        // Container top/bottom padding + margin contributions are
        // routed to first / last descendant leaves; padding acts as
        // a barrier that flushes the pending margin before its own
        // space adds. Container margin is a *deferred* margin that
        // participates in collapse with adjacent sibling / child
        // margins.
        let mut y_accum: f32 = 0.0;
        let mut pending_pos: f32 = 0.0;
        let mut pending_neg: f32 = 0.0;
        for leaf in leaves.iter() {
            // Ancestor padding + hanging → left indent / continuation.
            let ancestors = ancestors_of_range(&leaf.range, &containers);
            let (anc_left_pt, anc_right_pt) = ancestor_side_padding_pt(&ancestors, base_pt);
            // Walk ancestor ListItems: each one's `hanging` either
            // contributes to *this* leaf's continuation shift (when
            // the leaf is that item's first-descendant body) or to
            // this leaf's left indent (when the leaf is deeper
            // content — a nested list's body, a blockquote inside an
            // item, a second loose paragraph, etc.).
            let mut anc_hanging_left_pt = 0.0;
            let mut anc_hanging_cont_pt = 0.0;
            for anc in &ancestors {
                if !matches!(anc.kind, BlockKind::ListItem { .. }) {
                    continue;
                }
                let h = anc.delta.hanging.map(|l| l.resolve(base_pt)).unwrap_or(0.0);
                let is_first_body = container_first_last
                    .iter()
                    .any(|(cr, f, _)| cr == &anc.range && f == &leaf.range);
                if is_first_body {
                    anc_hanging_cont_pt += h;
                } else {
                    anc_hanging_left_pt += h;
                }
            }
            // Container spacing (routed to first / last descendant):
            //   - `padding.top/.bottom` acts as a BARRIER — sits
            //     between the collapsed outer margin and the child's
            //     content. Doesn't collapse.
            //   - `margin.top/.bottom` COLLAPSES with sibling and
            //     nested descendant margins (CSS rule linked from
            //     marquee's docs: two adjacent positive margins →
            //     max; two negatives → most-negative; mixed →
            //     max_pos + min_neg).
            //
            // Padding gets summed across ancestors (each container's
            // padding is its own barrier). Margins fold into the
            // pending pos/neg accumulator individually so the
            // pairwise max/min collapse composes.
            let mut anc_padding_top_pt = 0.0;
            let mut anc_padding_bottom_pt = 0.0;
            let mut anc_first_margins: Vec<f64> = Vec::new();
            let mut anc_last_margins: Vec<f64> = Vec::new();
            for anc in &ancestors {
                let (t_pad, _, b_pad, _) = anc
                    .delta
                    .padding
                    .map(|m| m.resolve(base_pt))
                    .unwrap_or((0.0, 0.0, 0.0, 0.0));
                let (t_marg, _, b_marg, _) = anc
                    .delta
                    .margin
                    .map(|m| m.resolve(base_pt))
                    .unwrap_or((0.0, 0.0, 0.0, 0.0));
                let is_first = container_first_last
                    .iter()
                    .any(|(cr, f, _)| cr == &anc.range && f == &leaf.range);
                let is_last = container_first_last
                    .iter()
                    .any(|(cr, _, l)| cr == &anc.range && l == &leaf.range);
                if is_first {
                    anc_padding_top_pt += t_pad;
                    anc_first_margins.push(t_marg);
                }
                if is_last {
                    anc_padding_bottom_pt += b_pad;
                    anc_last_margins.push(b_marg);
                }
            }
            // Own padding + margin. Padding contributes to insets +
            // vertical space around shape; margin is horizontally
            // additive on left/right, vertically participates in
            // sibling collapse via the pending accumulator.
            let (own_top_pt, own_right_pt, own_bottom_pt, own_left_pt) =
                margin_or_zero(&leaf.delta.padding, base_pt);
            let (mt_pt, m_right_pt, mb_pt, m_left_pt) = margin_or_zero(&leaf.delta.margin, base_pt);
            let anc_left_px = pt_to_px(anc_left_pt + anc_hanging_left_pt, dpi);
            let anc_right_px = pt_to_px(anc_right_pt, dpi);
            let own_left_px = pt_to_px(own_left_pt, dpi);
            let own_right_px = pt_to_px(own_right_pt, dpi);
            let own_top_px = pt_to_px(own_top_pt, dpi);
            let own_bottom_px = pt_to_px(own_bottom_pt, dpi);
            let margin_left_px = pt_to_px(m_left_pt, dpi);
            let margin_right_px = pt_to_px(m_right_pt, dpi);
            // Padding barrier from ancestor containers (non-collapsing).
            let extra_top_px = pt_to_px(anc_padding_top_pt, dpi);
            let extra_bottom_px = pt_to_px(anc_padding_bottom_pt, dpi);
            // Own hanging + first-line indent (both resolve against
            // the block's ambient em). Composes with any ancestor-
            // contributed hanging.
            let own_hanging_pt = leaf
                .delta
                .hanging
                .map(|l| l.resolve(base_pt))
                .unwrap_or(0.0);
            let hanging_px = pt_to_px((own_hanging_pt + anc_hanging_cont_pt).max(0.0), dpi);
            let first_line_indent_px = leaf
                .delta
                .indent
                .map(|l| pt_to_px(l.resolve(base_pt).max(0.0), dpi))
                .unwrap_or(0.0);
            // Slice text + inlines + baseline shifts to just this
            // block's byte range; rebase ranges to block-local coords.
            let (block_text, inlines, baselines) =
                slice_block(&runs.text, &runs.inline, &runs.baseline_shifts, &leaf.range);
            // Shape as its own parley layout. Natural break to start
            // — caller may re-break via `set_max_width`.
            let mut layout =
                shape_block_layout(&block_text, &inlines, base_style, base_brush, palette, dpi);
            layout.break_all_lines(None);
            layout.align(Alignment::Start, AlignmentOptions::default());
            let widths = layout.calculate_content_widths();
            let height_px = layout.height();
            let margin_top_px = pt_to_px(mt_pt, dpi);
            let margin_bottom_px = pt_to_px(mb_pt, dpi);
            // Sibling + first-descendant margin collapse. Each
            // ancestor container whose *first* descendant this leaf
            // is contributes its `margin.top` to the pending
            // accumulator — pairwise max/min collapse follows.
            for m_pt in &anc_first_margins {
                let m_px = pt_to_px(*m_pt, dpi);
                if m_px >= 0.0 {
                    pending_pos = pending_pos.max(m_px);
                } else {
                    pending_neg = pending_neg.min(m_px);
                }
            }
            if margin_top_px >= 0.0 {
                pending_pos = pending_pos.max(margin_top_px);
            } else {
                pending_neg = pending_neg.min(margin_top_px);
            }
            y_accum += pending_pos + pending_neg;
            pending_pos = 0.0;
            pending_neg = 0.0;
            // Container `padding.top` barriers (sum across ancestors)
            // sit above the leaf's shaped content but INSIDE any
            // container paint we emit.
            y_accum += extra_top_px + own_top_px;
            let y_px = y_accum;
            y_accum += height_px + own_bottom_px + extra_bottom_px;
            // Leaf's margin.bottom joins pending — collapses with
            // next sibling's margin.top and with any *last*-
            // descendant-container's margin.bottom below.
            if margin_bottom_px >= 0.0 {
                pending_pos = pending_pos.max(margin_bottom_px);
            } else {
                pending_neg = pending_neg.min(margin_bottom_px);
            }
            for m_pt in &anc_last_margins {
                let m_px = pt_to_px(*m_pt, dpi);
                if m_px >= 0.0 {
                    pending_pos = pending_pos.max(m_px);
                } else {
                    pending_neg = pending_neg.min(m_px);
                }
            }
            layouts.push(BlockLayout {
                layout,
                baseline_shifts: baselines,
                text_range: leaf.range.clone(),
                kind: leaf.kind.clone(),
                delta: leaf.delta.clone(),
                depth: leaf.depth,
                left_px: anc_left_px + own_left_px + margin_left_px,
                right_inset_px: anc_right_px + own_right_px + margin_right_px,
                shape_width_px: widths.max,
                first_line_shift_px: first_line_indent_px,
                continuation_shift_px: hanging_px,
                y_px,
                height_px,
                margin_top_px,
                margin_bottom_px,
                padding_top_px: own_top_px,
                padding_right_px: own_right_px,
                padding_bottom_px: own_bottom_px,
                padding_left_px: own_left_px,
                extra_top_px,
                extra_bottom_px,
            });
        }
        // Flush the trailing pending margin so it counts toward the
        // total run height.
        y_accum += pending_pos + pending_neg;
        // Natural width = max of (left + shape_width + right) across
        // non-Rule blocks (Rule blocks have empty text → zero shape
        // width; they stretch to whatever surrounding content
        // dictates, post-hoc below).
        let mut natural_width: f32 = 0.0;
        let mut min_width: f32 = 0.0;
        for bl in &layouts {
            if matches!(bl.kind, BlockKind::Rule) {
                continue;
            }
            let width_at_natural = bl.left_px + bl.shape_width_px + bl.right_inset_px;
            natural_width = natural_width.max(width_at_natural);
            let per_min = bl.left_px
                + bl.layout.calculate_content_widths().min
                + bl.right_inset_px
                + bl.first_line_shift_px.max(bl.continuation_shift_px);
            min_width = min_width.max(per_min);
        }
        // Rule blocks stretch to the run's natural width so the hr
        // line spans the same column the surrounding text occupies.
        // Zero natural width (e.g. a document containing nothing but
        // an hr) falls back to a reasonable placeholder — the base
        // text em × 20, roughly a typical column.
        let hr_placeholder = if natural_width > 0.0 {
            natural_width
        } else {
            pt_to_px(base_style.size_pt as f64 * 20.0, dpi)
        };
        for bl in &mut layouts {
            if matches!(bl.kind, BlockKind::Rule) {
                let content = (hr_placeholder - bl.left_px - bl.right_inset_px).max(1.0);
                bl.shape_width_px = content;
            }
        }
        natural_width = natural_width.max(hr_placeholder);

        Self {
            text: runs.text,
            blocks: RefCell::new(layouts),
            containers,
            base_style: base_style.clone(),
            palette: *palette,
            base_size_pt: base_style.size_pt,
            dpi,
            last_break_width: RefCell::new(None),
            natural_width_px: natural_width,
            natural_height_px: RefCell::new(y_accum),
            min_width_px: min_width,
        }
    }

    /// Natural (unwrapped) width in pixels.
    pub fn natural_width(&self) -> f64 {
        self.natural_width_px as f64
    }

    /// Natural (unwrapped) total stacked height in pixels.
    pub fn natural_height(&self) -> f64 {
        *self.natural_height_px.borrow() as f64
    }

    /// Current laid-out total height (px) — includes any margins on
    /// the last block below its content.
    pub fn current_height(&self) -> f64 {
        *self.natural_height_px.borrow() as f64
    }

    /// Current effective content width (px) — the max of every
    /// block's `left + shape_width + right`. Equal to the wrap width
    /// after [`Self::set_max_width`]; equal to the natural width
    /// otherwise.
    pub fn content_width(&self) -> f64 {
        self.layout_bounds().width as f64
    }

    /// Re-break every block at the given outer width, propagating the
    /// wrap constraint into each block's effective shape width
    /// (`outer - left - right - max(first_line, continuation)`).
    /// Returns the new stacked total height.
    pub fn set_max_width(&self, max_width_px: f32, alignment: Alignment) -> f32 {
        let mut blocks = self.blocks.borrow_mut();
        let mut y_accum: f32 = 0.0;
        let mut pending_pos: f32 = 0.0;
        let mut pending_neg: f32 = 0.0;
        for bl in blocks.iter_mut() {
            let block_avail = (max_width_px - bl.left_px - bl.right_inset_px).max(1.0);
            let max_shift = bl.first_line_shift_px.max(bl.continuation_shift_px);
            let target = (block_avail - max_shift).max(1.0);
            bl.layout.break_all_lines(Some(target));
            bl.layout.align(alignment, AlignmentOptions::default());
            bl.shape_width_px = target;
            bl.height_px = bl.layout.height();
            if bl.margin_top_px >= 0.0 {
                pending_pos = pending_pos.max(bl.margin_top_px);
            } else {
                pending_neg = pending_neg.min(bl.margin_top_px);
            }
            y_accum += pending_pos + pending_neg;
            pending_pos = 0.0;
            pending_neg = 0.0;
            y_accum += bl.extra_top_px + bl.padding_top_px;
            bl.y_px = y_accum;
            y_accum += bl.height_px + bl.padding_bottom_px + bl.extra_bottom_px;
            if bl.margin_bottom_px >= 0.0 {
                pending_pos = pending_pos.max(bl.margin_bottom_px);
            } else {
                pending_neg = pending_neg.min(bl.margin_bottom_px);
            }
        }
        y_accum += pending_pos + pending_neg;
        *self.last_break_width.borrow_mut() = Some(max_width_px);
        *self.natural_height_px.borrow_mut() = y_accum;
        y_accum
    }

    /// Compute per-block paint instructions (backgrounds + borders on
    /// both leaves and containers).
    pub fn block_paints(&self) -> Vec<BlockPaint> {
        compute_block_paints(self)
    }

    /// Aggregate bounds across every block layout. Used by
    /// [`RichAnchor`] resolution.
    pub fn layout_bounds(&self) -> LayoutBounds {
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
            total_height = total_height
                .max(bl.y_px + bl.height_px + bl.padding_bottom_px + bl.margin_bottom_px);
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
        self.set_max_width(width as f32, Alignment::Start) as f64
    }
}

// ─── Per-block shaping ──────────────────────────────────────────────────────

fn shape_block_layout(
    text: &str,
    inlines: &[InlineRun],
    base_style: &TextStyle,
    base_brush: Color,
    palette: &Palette,
    dpi: f64,
) -> parley::Layout<RichBrush> {
    let fcx_mutex = crate::text::font_context();
    let mut fcx = fcx_mutex.lock().expect("font context poisoned");
    let mut lcx = LayoutContext::<RichBrush>::new();
    let mut builder = lcx.ranged_builder(&mut fcx, text, 1.0, true);
    push_base_defaults(&mut builder, base_style, base_brush, dpi);
    let mut family_pool: Vec<String> = Vec::new();
    for InlineRun { range, delta } in inlines {
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
    builder.build(text)
}

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
    range: Range<usize>,
    delta: &StyleDelta,
    base: &TextStyle,
    palette: &Palette,
    dpi: f64,
    family_pool: &mut Vec<String>,
) {
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
        family_pool.push(family.clone());
        let name = family_pool.last().expect("just pushed");
        let entry = if let Some(generic) = generic_family_from_str(name) {
            FontFamily::Single(FontFamilyName::Generic(generic))
        } else {
            FontFamily::Single(FontFamilyName::named(name.as_str()))
        };
        builder.push(StyleProperty::FontFamily(entry), range.clone());
    }
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

// ─── Helpers: leaves, ancestors, slicing ────────────────────────────────────

fn is_leaf_kind(kind: &BlockKind) -> bool {
    // `ListItem` is a *container* — the reducer opens a Paragraph
    // leaf inside each item so nested lists decompose into their
    // own paragraph shape units. Hanging indent comes from the
    // enclosing ListItem via the ancestor chain, not the leaf's own
    // delta.
    matches!(
        kind,
        BlockKind::Paragraph
            | BlockKind::Heading(_)
            | BlockKind::CodeBlock { .. }
            | BlockKind::Rule
    )
}

/// True if another leaf-type block strictly contains `block`.
///
/// Zero-length blocks (decorative markers like [`BlockKind::Rule`])
/// sit *between* text-carrying blocks and share an endpoint with
/// their neighbours; for those we require strict containment — start
/// < point < end — so an hr at position P isn't classified as
/// "contained" in the paragraph ending at P.
fn contained_in_another_leaf(block: &Block, all: &[Block]) -> bool {
    let is_empty = block.range.start == block.range.end;
    for other in all {
        if !is_leaf_kind(&other.kind) {
            continue;
        }
        if std::ptr::eq(other, block) {
            continue;
        }
        if other.range == block.range {
            continue;
        }
        if is_empty {
            if other.range.start < block.range.start && other.range.end > block.range.end {
                return true;
            }
        } else if other.range.start <= block.range.start && other.range.end >= block.range.end {
            return true;
        }
    }
    false
}

/// Container blocks whose range contains `range`. Outermost first.
pub(crate) fn ancestors_of_range(range: &Range<usize>, containers: &[Block]) -> Vec<Block> {
    let mut out: Vec<Block> = containers
        .iter()
        .filter(|c| c.range.start <= range.start && c.range.end >= range.end)
        .cloned()
        .collect();
    out.sort_by(|a, b| {
        let a_size = a.range.end - a.range.start;
        let b_size = b.range.end - b.range.start;
        b_size.cmp(&a_size)
    });
    out
}

fn ancestor_side_padding_pt(ancestors: &[Block], base_pt: f64) -> (f64, f64) {
    let mut left = 0.0;
    let mut right = 0.0;
    for a in ancestors {
        if let Some(pad) = a.delta.padding {
            let (_, r, _, l) = pad.resolve(base_pt);
            left += l;
            right += r;
        }
    }
    (left, right)
}

fn margin_or_zero(m: &Option<Margin>, base_pt: f64) -> (f64, f64, f64, f64) {
    match m {
        Some(m) => m.resolve(base_pt),
        None => (0.0, 0.0, 0.0, 0.0),
    }
}

fn pt_to_px(pt: f64, dpi: f64) -> f32 {
    (pt * dpi / 72.0) as f32
}

/// Slice text + inline runs + baseline shifts to the block's byte
/// range, rebasing ranges to block-local coordinates.
fn slice_block(
    text: &str,
    inlines: &[InlineRun],
    baselines: &[BaselineRun],
    block_range: &Range<usize>,
) -> (String, Vec<InlineRun>, Vec<BaselineRun>) {
    let block_text = text[block_range.clone()].to_string();
    let s = block_range.start;
    let e = block_range.end;
    let mut out_inlines = Vec::new();
    for r in inlines {
        let start = r.range.start.max(s);
        let end = r.range.end.min(e);
        if start >= end {
            continue;
        }
        out_inlines.push(InlineRun {
            range: (start - s)..(end - s),
            delta: r.delta.clone(),
        });
    }
    let mut out_baselines = Vec::new();
    for b in baselines {
        let start = b.range.start.max(s);
        let end = b.range.end.min(e);
        if start >= end {
            continue;
        }
        out_baselines.push(BaselineRun {
            range: (start - s)..(end - s),
            shift_em: b.shift_em,
        });
    }
    (block_text, out_inlines, out_baselines)
}

// ─── Draw ───────────────────────────────────────────────────────────────────

/// Emit the shaped [`RichTextRun`] into `scene` at `(x, y)`.
///
/// `anchor` picks the point on the laid-out run that coincides with
/// `(x, y)`. `transform` composes around that anchor.
///
/// Block-level paints (backgrounds, borders) emit first with
/// [`PickId::Skip`]; glyph runs emit second with `pick_id`.
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
    let bounds = run.layout_bounds();
    let offsets = bounds.resolve(anchor);
    let final_transform = Affine::translate((x, y)) * transform;

    // Backgrounds + borders first.
    for paint in &run.block_paints() {
        emit_block_paint(scene, paint, offsets, final_transform);
    }

    // Glyphs.
    let blocks = run.blocks.borrow();
    for bl in blocks.iter() {
        let block_x = bl.left_px;
        let block_y = bl.y_px;
        for (line_index, line) in bl.layout.lines().enumerate() {
            let per_line_shift = if line_index == 0 {
                bl.first_line_shift_px
            } else {
                bl.continuation_shift_px
            };
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
                let shift_em = baseline_shift_for_range(&bl.baseline_shifts, &run_range);
                let dy_px = shift_em * font_size;
                let glyphs: Vec<Glyph> = gr
                    .positioned_glyphs()
                    .map(|g| Glyph {
                        id: g.id,
                        x: g.x + block_x + per_line_shift - offsets.ref_x,
                        y: g.y + block_y - offsets.ref_y - dy_px,
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
}

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
        if border.is_uniform() {
            let w = border.widths_px[0];
            if w > 0.0 {
                let stroke = crate::stroke::Stroke::new(w as f64);
                scene.stroke(
                    &stroke,
                    outer,
                    &Brush::Solid(border.color),
                    None,
                    &path,
                    PickId::Skip,
                );
            }
        } else {
            // Per-side widths — emit each side as its own segment.
            // `corner_radius` is intentionally ignored on the mixed
            // path (square corners; documented on
            // `StyleDelta::border_width`).
            let brush = Brush::Solid(border.color);
            let [wt, wr, wb, wl] = border.widths_px;
            let (x0, y0, x1, y1) = (rect.x0, rect.y0, rect.x1, rect.y1);
            let emit = |scene: &mut S, w: f32, a: (f64, f64), b: (f64, f64)| {
                if w <= 0.0 {
                    return;
                }
                let seg = crate::primitives::segment(
                    kurbo::Point::new(a.0, a.1),
                    kurbo::Point::new(b.0, b.1),
                );
                let stroke = crate::stroke::Stroke::new(w as f64);
                scene.stroke(&stroke, outer, &brush, None, &seg, PickId::Skip);
            };
            emit(scene, wt, (x0, y0), (x1, y0));
            emit(scene, wr, (x1, y0), (x1, y1));
            emit(scene, wb, (x0, y1), (x1, y1));
            emit(scene, wl, (x0, y0), (x0, y1));
        }
    }
}

fn baseline_shift_for_range(shifts: &[BaselineRun], run_range: &Range<usize>) -> f32 {
    for bs in shifts {
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

    fn make(src: &str) -> RichTextRun {
        let sheet = RichTextStyleSheet::new();
        RichTextRun::new(
            src,
            &base_style(),
            Color::from_rgba8(0, 0, 0, 255),
            &sheet,
            &palette(),
            96.0,
        )
        .unwrap()
    }

    fn draw(run: &RichTextRun) -> RecordingScene {
        let mut scene = RecordingScene::default();
        draw_rich_text(
            &mut scene,
            run,
            0.0,
            0.0,
            RichAnchor::top_left(),
            Affine::IDENTITY,
            PickId::Skip,
        );
        scene
    }

    fn glyph_x_at_line(scene: &RecordingScene, line_ordinal: usize) -> Option<f32> {
        // Return the leftmost x of the `line_ordinal`-th DrawGlyphs op
        // (approximation: parley emits one op per line for simple
        // text; heavier styling may split further).
        let mut ops = scene.ops.iter().filter_map(|op| match op {
            Op::DrawGlyphs(gr) => Some(gr),
            _ => None,
        });
        let gr = ops.nth(line_ordinal)?;
        gr.glyphs.iter().map(|g| g.x).fold(None::<f32>, |acc, x| {
            Some(match acc {
                None => x,
                Some(a) => a.min(x),
            })
        })
    }

    #[test]
    fn plain_text_shapes_and_measures() {
        let run = make("hello world");
        assert!(run.natural_width() > 0.0);
        assert!(run.natural_height() > 0.0);
    }

    #[test]
    fn bold_widens_natural_width_vs_plain() {
        let plain = make("hello world");
        let bold = make("**hello world**");
        assert!(bold.natural_width() >= plain.natural_width());
    }

    #[test]
    fn draw_emits_glyph_runs_with_per_range_brushes() {
        let run = make("a {.red word} b");
        let scene = draw(&run);
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
        assert!(has_red);
    }

    #[test]
    fn sup_offsets_glyphs_upward() {
        let run = make("a ^2^ b");
        let scene = draw(&run);
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
        assert!(max_y - min_y > 1.0);
    }

    #[test]
    fn measure_impl_reports_positive_height() {
        let run = make("hello **bold** world");
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
        let scene = draw(&run);
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
    fn heading_produces_larger_glyph_run_font_size() {
        let plain = make("Big");
        let heading = make("# Big");
        let max_size = |run: &RichTextRun| {
            let scene = draw(run);
            scene
                .ops
                .iter()
                .filter_map(|op| match op {
                    Op::DrawGlyphs(gr) => Some(gr.font_size),
                    _ => None,
                })
                .fold(0.0_f32, f32::max)
        };
        assert!(max_size(&heading) > max_size(&plain) * 1.5);
    }

    #[test]
    fn size_selector_produces_larger_run_height() {
        let plain = make("x");
        let big = make("{.36 x}");
        assert!(big.natural_height() > plain.natural_height());
    }

    #[test]
    fn new_with_width_wraps_at_fixed_pixels() {
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
        assert!(wrapped.current_height() > unwrapped.current_height());
    }

    #[test]
    fn blockquote_indents_its_paragraph() {
        // A blockquote's default padding.left = Rel(1.0) so its
        // paragraph child should sit ~1em (14pt) to the right of a
        // plain paragraph. In screen px: 14pt at 96dpi ≈ 18.67px.
        let plain = make("hello world");
        let quoted = make("> hello world");
        // The paragraph inside the blockquote should be at ~1em from
        // the left; the plain paragraph's leftmost glyph sits at ~0.
        let plain_x = glyph_x_at_line(&draw(&plain), 0).unwrap_or(0.0);
        let quoted_x = glyph_x_at_line(&draw(&quoted), 0).unwrap_or(0.0);
        assert!(
            quoted_x > plain_x + 10.0,
            "blockquote content should be indented (plain={plain_x}, quoted={quoted_x})"
        );
    }

    #[test]
    fn sibling_margins_collapse_via_max_not_sum() {
        // Both paragraphs have `margin.bottom = Rel(0.5)`. Between
        // two adjacent paragraphs, CSS collapses to `max(0.5, 0)`
        // (paragraph.margin.top = 0). With more paragraphs of the
        // same margin.bottom, each subsequent gap remains 0.5em —
        // not 1em. Compare a 3-paragraph run to a 5-paragraph run:
        // extra height = 2 × (line + 0.5em), NOT 2 × (line + 1em).
        let three = make("a\n\nb\n\nc");
        let five = make("a\n\nb\n\nc\n\nd\n\ne");
        let diff = five.natural_height() - three.natural_height();
        // Each extra paragraph adds line_height + collapsed 0.5em.
        // At 14pt base with default line-height, one line ≈ 22.4px;
        // 0.5em ≈ 9.3px. So each ≈ 31.7px per paragraph.
        // Without collapse it'd be ≈ 41px per paragraph.
        // Assert < 80 (would be 82+ without collapse).
        assert!(
            diff < 80.0,
            "2 extra paragraphs should add ≲ 64px under sibling collapse, got {diff}"
        );
    }

    #[test]
    fn adjacent_heading_paragraph_margins_collapse() {
        // Heading has `margin.bottom = Rel(0.3)` and paragraph has
        // `margin.top = 0` → collapse via `max(0.3, 0)` = 0.3em.
        // A bare `# H\n\ntext` should be shorter than a hypothetical
        // sum which would be 0.3+0 = 0.3em anyway. Test that headings
        // followed by paragraphs stack sanely.
        let with_heading = make("# Header\n\nBody");
        let no_heading = make("Header\n\nBody");
        assert!(with_heading.natural_height() > no_heading.natural_height());
    }

    #[test]
    fn horizontal_rule_emits_single_top_stroke() {
        // `---` produces a Rule block; the sheet's `hr` entry sets a
        // top-only 1pt border, so the paint pass emits exactly one
        // horizontal stroke.
        let run = make("above\n\n---\n\nbelow");
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
        let strokes: Vec<_> = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Stroke { .. }))
            .collect();
        assert_eq!(
            strokes.len(),
            1,
            "hr should emit exactly one stroke (top edge only), got {}",
            strokes.len()
        );
    }

    #[test]
    fn horizontal_rule_stretches_to_full_natural_width() {
        // The hr's paint outer_rect should span the same width as the
        // surrounding text — natural_width from the surrounding
        // paragraphs.
        let run = make("hello world hello world hello world\n\n---\n\nfollowing");
        let paints = run.block_paints();
        // Find the hr paint (border only, no background, small height).
        let hr = paints
            .iter()
            .find(|p| p.border.is_some() && p.background.is_none())
            .expect("expected hr paint");
        let width = hr.outer_rect.width();
        assert!(
            width > 50.0,
            "hr should span at least a paragraph's width; got {width}"
        );
        assert!(
            width as f32 >= run.natural_width() as f32 - 5.0,
            "hr should span ≥ natural width ({} vs {})",
            width,
            run.natural_width()
        );
    }

    #[test]
    fn horizontal_rule_reserves_vertical_space() {
        // hr has 0 height but non-zero top/bottom margins from the
        // sheet default. `above` and `below` should be separated by
        // more space than a plain paragraph break.
        let without_hr = make("above\n\nbelow");
        let with_hr = make("above\n\n---\n\nbelow");
        assert!(
            with_hr.natural_height() > without_hr.natural_height(),
            "hr's margins should add vertical space (without: {}, with: {})",
            without_hr.natural_height(),
            with_hr.natural_height()
        );
    }

    #[test]
    fn blockquote_emits_single_left_edge_stroke() {
        // The default `block_quote` sheet entry sets `border_width`
        // = Margin { left: 3pt, others: 0 }. The paint pass must
        // emit exactly ONE stroke op (for the left edge) — not a
        // full four-sided box.
        let run = make("> hello world");
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
        let strokes: Vec<_> = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Stroke { .. }))
            .collect();
        assert_eq!(
            strokes.len(),
            1,
            "blockquote should emit exactly one stroke (left edge only), got {}",
            strokes.len()
        );
    }

    #[test]
    fn uniform_border_emits_single_boxed_stroke() {
        // A uniform four-sided border should collapse to a single
        // rectangular stroke — no fan of segments.
        let mut sheet = RichTextStyleSheet::empty();
        sheet.set(
            "paragraph",
            crate::text::rich::style::StyleDelta {
                border_color: Some(crate::plot::theme::ThemeColor::Ink),
                border_width: Some(crate::plot::theme::Margin::all(
                    crate::plot::theme::Length::Abs(1.0),
                )),
                ..crate::text::rich::style::StyleDelta::empty()
            },
        );
        let run = RichTextRun::new(
            "boxed",
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
        let strokes = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Stroke { .. }))
            .count();
        assert_eq!(strokes, 1, "uniform border → one rectangular stroke");
    }

    #[test]
    fn loose_list_stacks_taller_than_tight_list() {
        // Same items, one written tight and one written loose. The
        // loose version should be strictly taller because each
        // paragraph body carries margin.bottom.
        let tight = make("- alpha\n- beta\n- gamma");
        let loose = make("- alpha\n\n- beta\n\n- gamma");
        assert!(
            loose.natural_height() > tight.natural_height() + 5.0,
            "loose ({}) should stack taller than tight ({})",
            loose.natural_height(),
            tight.natural_height()
        );
    }

    #[test]
    fn nested_list_item_sits_further_right_than_outer() {
        // A nested list should render its bullet indented under the
        // outer item's continuation position, i.e. outer's hanging
        // (~1.5em) to the right of the outer marker.
        let run = make("- outer\n  - inner");
        let scene = draw(&run);
        // Extract leftmost glyph x per line (approximate via y-bucket).
        let mut by_line: std::collections::BTreeMap<i32, f32> = std::collections::BTreeMap::new();
        for op in &scene.ops {
            if let Op::DrawGlyphs(gr) = op {
                if let Some(min_x) = gr.glyphs.iter().map(|g| g.x).fold(None::<f32>, |acc, x| {
                    Some(match acc {
                        None => x,
                        Some(a) => a.min(x),
                    })
                }) {
                    let y_key = gr.glyphs[0].y as i32;
                    by_line
                        .entry(y_key)
                        .and_modify(|v| *v = v.min(min_x))
                        .or_insert(min_x);
                }
            }
        }
        let xs: Vec<f32> = by_line.into_values().collect();
        assert!(xs.len() >= 2, "expected at least 2 lines, got {xs:?}");
        // First line = outer's bullet at x≈0; second = inner's bullet
        // at x≈1.5em (=1.5*14pt≈21pt→28px at 96dpi).
        assert!(
            xs[1] > xs[0] + 10.0,
            "nested item should be indented past outer (xs={xs:?})"
        );
    }

    #[test]
    fn list_item_hanging_shifts_continuation_lines() {
        // Force the list item to wrap so a continuation line appears,
        // then verify its leftmost glyph is right of the first line's.
        let src = "- one two three four five six seven eight nine";
        let sheet = RichTextStyleSheet::new();
        let run = RichTextRun::new_with_width(
            src,
            &base_style(),
            Color::from_rgba8(0, 0, 0, 255),
            &sheet,
            &palette(),
            96.0,
            RichTextWidth::Fixed(100.0),
        )
        .unwrap();
        let scene = draw(&run);
        // Extract all glyph runs and their minimum x + y. Group by y
        // to distinguish lines.
        let mut by_line: std::collections::BTreeMap<i32, f32> = std::collections::BTreeMap::new();
        for op in &scene.ops {
            if let Op::DrawGlyphs(gr) = op {
                if let Some(min_x) = gr.glyphs.iter().map(|g| g.x).fold(None::<f32>, |acc, x| {
                    Some(match acc {
                        None => x,
                        Some(a) => a.min(x),
                    })
                }) {
                    let y_key = gr.glyphs[0].y as i32;
                    by_line
                        .entry(y_key)
                        .and_modify(|v| *v = v.min(min_x))
                        .or_insert(min_x);
                }
            }
        }
        let xs: Vec<f32> = by_line.into_values().collect();
        assert!(xs.len() >= 2, "expected at least 2 lines, got {xs:?}");
        assert!(
            xs[1] > xs[0] + 5.0,
            "continuation line should be right of first (xs={xs:?})"
        );
    }

    #[test]
    fn code_block_emits_fill_before_glyphs() {
        let run = make("```\nlet x = 1;\n```");
        let scene = draw(&run);
        let first_fill = scene
            .ops
            .iter()
            .position(|op| matches!(op, Op::Fill { .. }));
        let first_glyphs = scene
            .ops
            .iter()
            .position(|op| matches!(op, Op::DrawGlyphs(_)));
        let (fi, gi) = (first_fill.expect("fill"), first_glyphs.expect("glyphs"));
        assert!(fi < gi, "Fill at {fi} should precede DrawGlyphs at {gi}");
    }

    #[test]
    fn plain_text_emits_no_block_paints() {
        let run = make("just plain");
        let scene = draw(&run);
        let fills = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Fill { .. }))
            .count();
        let strokes = scene
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Stroke { .. }))
            .count();
        assert_eq!(fills, 0);
        assert_eq!(strokes, 0);
    }

    #[test]
    fn center_anchor_shifts_glyph_positions_by_half_width() {
        let run = make("abcdef");
        let width = run.natural_width() as f32;
        let mut scene = RecordingScene::default();
        draw_rich_text(
            &mut scene,
            &run,
            100.0,
            100.0,
            RichAnchor::center(),
            Affine::IDENTITY,
            PickId::Skip,
        );
        let first_op = scene
            .ops
            .iter()
            .find_map(|op| match op {
                Op::DrawGlyphs(gr) => Some(gr),
                _ => None,
            })
            .unwrap();
        let first_g = first_op.glyphs.first().unwrap();
        let transformed_x = first_op.transform.as_coeffs()[4] as f32 + first_g.x;
        assert!(
            (transformed_x - (100.0 - width * 0.5)).abs() < 2.0,
            "first glyph should sit near (x - width/2), got {transformed_x} (expected ~{})",
            100.0 - width * 0.5
        );
    }

    #[test]
    fn first_line_anchor_places_baseline_on_y() {
        let run = make("hi");
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
        let g = first.glyphs.first().unwrap();
        let screen_y = coeffs[5] as f32 + g.y;
        assert!(
            (screen_y - 100.0).abs() < 0.5,
            "first baseline should land at y = 100; got screen y = {screen_y}"
        );
    }
}
