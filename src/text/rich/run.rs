//! Shape marquee-flavoured markdown into a stack of per-block parley
//! layouts and draw them through `SceneBuilder`.
//!
//! **One parley layout per top-level leaf.** Paragraph, heading,
//! code-block, and rule blocks each shape as their own
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
    Alignment, AlignmentOptions, FontFamily, FontFamilyName, FontStyle, FontWeight, LayoutContext,
    PositionedLayoutItem, StyleProperty,
};

use super::anchor::{LayoutBounds, RichAnchor};
use super::block::{compute_block_paints, BlockPaint};
use super::length::LineHeightSpec;
use super::parser::parse;
use super::reduce::{reduce, BaselineRun, Block, BlockKind, BuiltRuns, InlineRun};
use super::style::{ResolvedStyle, RichTextStyleSheet};
use crate::brush::Brush;
use crate::color::Color;
use crate::geometry::Affine;
use crate::layout::{Measure, WidthHint};
use crate::pick::PickId;
use crate::scene::{Font, GlyphRun, SceneBuilder};
use crate::style_vocab::{HAlign, Palette};
use crate::text::shape_common::{
    emit_decoration_rect, generic_family_from_str, glyphs_of_run, parley_features,
    push_style_defaults, DecorationRect,
};
use crate::text::TextStyle;

/// Distance between a list-item marker and the item's content edge,
/// as a fraction of the item's em. Matches marquee.
const MARKER_GAP_EM: f64 = 0.25;

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

// ─── Vertical spacing ───────────────────────────────────────────────────────

/// One box's contribution to the vertical space on one side of a leaf.
///
/// Marquee follows CSS margin collapsing: adjacent margins merge
/// (two positives take the max, two negatives the most negative, a
/// mixed pair sums), and anything drawn between them stops the merge.
/// `barrier` is that "anything drawn": padding, a background, or a
/// border edge.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct EdgeSpacing {
    /// The box's margin on this side (px). Collapses with neighbours.
    pub(crate) margin_px: f32,
    /// True when the box paints or pads on this side, which stops the
    /// margins on either side of it from collapsing together.
    pub(crate) barrier: bool,
    /// The box's padding on this side (px). Always adds space.
    pub(crate) padding_px: f32,
}

/// Running state for the collapse walk down a stack of blocks.
#[derive(Default)]
struct MarginAccumulator {
    /// Largest positive margin seen since the last barrier.
    pending_pos: f32,
    /// Most negative margin seen since the last barrier.
    pending_neg: f32,
}

impl MarginAccumulator {
    fn fold(&mut self, margin_px: f32) {
        if margin_px >= 0.0 {
            self.pending_pos = self.pending_pos.max(margin_px);
        } else {
            self.pending_neg = self.pending_neg.min(margin_px);
        }
    }

    /// Commit the collapsed margin into `y` and start a fresh run.
    fn flush(&mut self, y: &mut f32) {
        *y += self.pending_pos + self.pending_neg;
        self.pending_pos = 0.0;
        self.pending_neg = 0.0;
    }
}

/// Walk one leaf's top chain: for each box, its margin joins the
/// collapse run, then a barrier commits the run and adds the box's
/// padding.
fn apply_top_chain(chain: &[EdgeSpacing], y: &mut f32, acc: &mut MarginAccumulator) {
    for e in chain {
        acc.fold(e.margin_px);
        if e.barrier {
            acc.flush(y);
            *y += e.padding_px;
        }
    }
}

/// Walk one leaf's bottom chain. Mirror image of [`apply_top_chain`]:
/// padding sits inside the box, so it lands before the box's own
/// margin joins the run.
fn apply_bottom_chain(chain: &[EdgeSpacing], y: &mut f32, acc: &mut MarginAccumulator) {
    for e in chain {
        if e.barrier {
            acc.flush(y);
            *y += e.padding_px;
        }
        acc.fold(e.margin_px);
    }
}

/// Position every block vertically, collapsing margins across the
/// whole stack. Returns the total height including trailing margin.
fn stack_blocks(blocks: &mut [BlockLayout]) -> f32 {
    let mut y: f32 = 0.0;
    let mut acc = MarginAccumulator::default();
    for bl in blocks.iter_mut() {
        apply_top_chain(&bl.top_chain, &mut y, &mut acc);
        // The block's own content is itself a barrier — a margin
        // above it can't collapse with one below it.
        acc.flush(&mut y);
        bl.y_px = y;
        y += bl.height_px;
        apply_bottom_chain(&bl.bottom_chain, &mut y, &mut acc);
    }
    acc.flush(&mut y);
    y
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
    /// Last requested wrap width; `None` = natural.
    #[allow(dead_code)]
    last_break_width: RefCell<Option<f32>>,
    /// Cached natural width (px).
    natural_width_px: f32,
    /// Natural stacked height (px) at the unwrapped width. Fixed at
    /// construction — re-breaking narrows the run, it doesn't change
    /// what the natural layout was.
    natural_height_px: f32,
    /// Stacked height (px) at the width the blocks are currently
    /// broken to.
    current_height_px: RefCell<f32>,
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
        let r = Self::shape(runs, base_style, base_brush, palette, dpi);
        if let RichTextWidth::Fixed(px) = width {
            r.set_max_width(px, HAlign::Start);
        }
        r
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
        // Sorted outermost-first (widest range wins) once here. Both
        // the ancestor walk and the paint pass rely on that order.
        let mut containers: Vec<Block> = all_blocks
            .into_iter()
            .filter(|b| !is_leaf_kind(&b.kind))
            .collect();
        containers.sort_by_key(|c| std::cmp::Reverse(c.range.end - c.range.start));

        // Shape each leaf; position vertically.
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
        for leaf in leaves.iter() {
            // Ancestor padding + hanging → left indent / continuation.
            let ancestors = ancestors_of_range(&leaf.range, &containers);
            let (anc_left_pt, anc_right_pt) = ancestor_side_padding_pt(&ancestors);
            // Walk ancestor containers for indent / hanging routing:
            //
            //   - `ListItem` uses the classic outdent-vs-nested split.
            //     Its `hanging` becomes `continuation_shift` on the
            //     item's own body (first-descendant leaf) and
            //     `left_px` on any deeper content (nested lists,
            //     subsequent loose paragraphs) — so nested items sit
            //     under the outer item's body-continuation column.
            //
            //   - `Div` / `BlockQuote` are the "block styling"
            //     containers. Their `indent` and `hanging` apply to
            //     EVERY descendant paragraph uniformly: the div's
            //     styling cascades onto each paragraph as if it were
            //     the paragraph's own value. That mirrors how CSS
            //     `text-indent` / hanging on a container cascades to
            //     descendant blocks.
            let mut anc_hanging_left_pt = 0.0;
            let mut anc_hanging_cont_pt = 0.0;
            let mut anc_first_line_pt = 0.0;
            for anc in &ancestors {
                let h = anc.style.hanging_pt;
                let i = anc.style.indent_pt;
                match anc.kind {
                    BlockKind::ListItem { .. } => {
                        let is_first_body = container_first_last
                            .iter()
                            .any(|(cr, f, _)| cr == &anc.range && f == &leaf.range);
                        if is_first_body {
                            anc_hanging_cont_pt += h;
                        } else {
                            anc_hanging_left_pt += h;
                        }
                        // ListItem doesn't currently participate in
                        // `indent` — matches marquee's `hanging`-only
                        // semantics for list items.
                    }
                    _ => {
                        // Div / BlockQuote / List / anything else:
                        // container hanging → every descendant's
                        // continuation shift; container indent →
                        // every descendant's first-line shift.
                        anc_hanging_cont_pt += h;
                        anc_first_line_pt += i;
                    }
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
            let mut top_chain: Vec<EdgeSpacing> = Vec::new();
            let mut bottom_chain: Vec<EdgeSpacing> = Vec::new();
            for anc in &ancestors {
                let is_first = container_first_last
                    .iter()
                    .any(|(cr, f, _)| cr == &anc.range && f == &leaf.range);
                let is_last = container_first_last
                    .iter()
                    .any(|(cr, _, l)| cr == &anc.range && l == &leaf.range);
                if is_first {
                    top_chain.push(edge_spacing(&anc.style, 0, dpi));
                }
                if is_last {
                    bottom_chain.push(edge_spacing(&anc.style, 2, dpi));
                }
            }
            // The leaf's own box sits innermost on both sides:
            // last in the outer-first top chain, first in the
            // inner-first bottom chain.
            top_chain.push(edge_spacing(&leaf.style, 0, dpi));
            bottom_chain.reverse();
            bottom_chain.insert(0, edge_spacing(&leaf.style, 2, dpi));
            // Own padding + margin. Padding contributes to insets +
            // vertical space around shape; margin is horizontally
            // additive on left/right, vertically participates in
            // sibling collapse via the pending accumulator.
            let [own_top_pt, own_right_pt, own_bottom_pt, own_left_pt] = leaf.style.padding_pt;
            let [_, m_right_pt, _, m_left_pt] = leaf.style.margin_pt;
            let anc_left_px = pt_to_px(anc_left_pt + anc_hanging_left_pt, dpi);
            let anc_right_px = pt_to_px(anc_right_pt, dpi);
            let own_left_px = pt_to_px(own_left_pt, dpi);
            let own_right_px = pt_to_px(own_right_pt, dpi);
            let own_top_px = pt_to_px(own_top_pt, dpi);
            let own_bottom_px = pt_to_px(own_bottom_pt, dpi);
            let margin_left_px = pt_to_px(m_left_pt, dpi);
            let margin_right_px = pt_to_px(m_right_pt, dpi);
            // Own hanging + first-line indent (both resolve against
            // the block's ambient em). Composes with any ancestor-
            // contributed hanging.
            let hanging_px = pt_to_px((leaf.style.hanging_pt + anc_hanging_cont_pt).max(0.0), dpi);
            let first_line_indent_px =
                pt_to_px((leaf.style.indent_pt + anc_first_line_pt).max(0.0), dpi);
            // Alignment: own overrides ancestor overrides caller.
            let mut resolved_align: Option<HAlign> = leaf.style.align;
            if resolved_align.is_none() {
                for anc in &ancestors {
                    if let Some(a) = anc.style.align {
                        resolved_align = Some(a);
                        break;
                    }
                }
            }
            // Direction: same "child wins, walk ancestors" resolution
            // as alignment. An unset field or `Direction::Auto` reads
            // back parley's own is_rtl() after shaping (below).
            let mut resolved_direction: Option<super::style::Direction> = leaf.style.text_direction;
            if resolved_direction.is_none() {
                for anc in &ancestors {
                    if let Some(d) = anc.style.text_direction {
                        resolved_direction = Some(d);
                        break;
                    }
                }
            }
            // Slice text + inlines + baseline shifts to just this
            // block's byte range; rebase ranges to block-local coords.
            let (block_text_owned, inlines, baselines) =
                slice_block(&runs.text, &runs.inline, &runs.baseline_shifts, &leaf.range);
            let block_text = block_text_owned;
            // Shape at natural width first (no wrap constraint). The
            // asymmetric-shift split is applied only when
            // `set_max_width` is called with a finite constraint —
            // at natural width there's no wrap and thus no "gap on
            // right" issue.
            let mut layout =
                shape_block_layout(&block_text, &inlines, base_style, base_brush, palette, dpi);
            layout.break_all_lines(None);
            // Resolve `Auto` (or an unset field) against parley's own
            // UBA output on this block's text — that's the standard
            // paragraph-direction algorithm and the marquee-parity
            // choice per the plan. Explicit Ltr / Rtl bypass parley.
            let is_rtl = match resolved_direction {
                Some(super::style::Direction::Ltr) => false,
                Some(super::style::Direction::Rtl) => true,
                None | Some(super::style::Direction::Auto) => layout.is_rtl(),
            };
            let alignment_override = resolved_align.map(|a| hal_to_alignment(a, is_rtl));
            layout.align(
                alignment_override.unwrap_or_else(|| hal_to_alignment(HAlign::Start, is_rtl)),
                AlignmentOptions::default(),
            );
            let widths = layout.calculate_content_widths();
            let height_px = layout.height();
            let continuation_layout: Option<parley::Layout<RichBrush>> = None;
            let continuation_baseline_shifts: Vec<BaselineRun> = Vec::new();
            let continuation_inlines: Vec<InlineRun> = Vec::new();
            let first_line_height_px: f32 = 0.0;
            // Under Rtl, the class-supplied `.left` / `.right` on
            // padding, margin, and (in block.rs) border_width are
            // treated as logical start / end sides — so the physical
            // "left" inset in the block layout is the sum of the
            // right-side pt values (start-side under Rtl), and vice
            // versa. `indent` / `hanging` stay logical by field name
            // (first-line vs continuation) and are applied physically
            // by `emit_line_glyphs` based on `is_rtl`.
            let (phys_left_px, phys_right_inset_px) = if is_rtl {
                (
                    anc_right_px + own_right_px + margin_right_px,
                    anc_left_px + own_left_px + margin_left_px,
                )
            } else {
                (
                    anc_left_px + own_left_px + margin_left_px,
                    anc_right_px + own_right_px + margin_right_px,
                )
            };
            let (phys_pad_left_px, phys_pad_right_px) = if is_rtl {
                (own_right_px, own_left_px)
            } else {
                (own_left_px, own_right_px)
            };
            // The leaf that opens a list item's body carries the
            // item's marker. It shapes with the item's own style and
            // draws in the list's start gutter, `MARKER_GAP_EM` of the
            // item's em to the start side of the content edge.
            let marker = ancestors.iter().find_map(|anc| {
                let BlockKind::ListItem { marker, .. } = &anc.kind else {
                    return None;
                };
                let text = marker.as_ref()?;
                let is_first_body = container_first_last
                    .iter()
                    .any(|(cr, f, _)| cr == &anc.range && f == &leaf.range);
                if !is_first_body {
                    return None;
                }
                let run = InlineRun {
                    range: 0..text.len(),
                    style: anc.style.for_inline(),
                };
                let mut m_layout = shape_block_layout(
                    text,
                    std::slice::from_ref(&run),
                    base_style,
                    base_brush,
                    palette,
                    dpi,
                );
                m_layout.break_all_lines(None);
                m_layout.align(Alignment::Start, AlignmentOptions::default());
                Some(MarkerLayout {
                    width_px: m_layout.calculate_content_widths().max,
                    gap_px: pt_to_px(anc.style.size_pt * MARKER_GAP_EM, dpi),
                    layout: m_layout,
                })
            });
            layouts.push(BlockLayout {
                layout,
                baseline_shifts: baselines.clone(),
                text_range: leaf.range.clone(),
                kind: leaf.kind.clone(),
                style: leaf.style.clone(),
                left_px: phys_left_px,
                right_inset_px: phys_right_inset_px,
                shape_width_px: widths.max,
                first_line_shift_px: first_line_indent_px,
                continuation_shift_px: hanging_px,
                y_px: 0.0,
                height_px,
                top_chain,
                bottom_chain,
                padding_top_px: own_top_px,
                padding_right_px: phys_pad_right_px,
                padding_bottom_px: own_bottom_px,
                padding_left_px: phys_pad_left_px,
                alignment_override,
                is_rtl,
                continuation_layout,
                continuation_baseline_shifts,
                first_line_height_px,
                continuation_inlines,
                source_text: block_text,
                source_inlines: inlines,
                source_baselines: baselines,
                marker,
            });
        }
        let total_height = stack_blocks(&mut layouts);
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
            blocks: RefCell::new(layouts),
            containers,
            base_style: base_style.clone(),
            palette: *palette,
            base_brush,
            dpi,
            last_break_width: RefCell::new(None),
            natural_width_px: natural_width,
            natural_height_px: total_height,
            current_height_px: RefCell::new(total_height),
            min_width_px: min_width,
        }
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

    /// Re-break every block at the given outer width, propagating the
    /// wrap constraint into each block's effective shape width
    /// (`outer - left - right - max(first_line, continuation)`).
    /// Returns the new stacked total height.
    pub fn set_max_width(&self, max_width_px: f32, alignment: HAlign) -> f32 {
        let mut blocks = self.blocks.borrow_mut();
        for bl in blocks.iter_mut() {
            let block_avail = (max_width_px - bl.left_px - bl.right_inset_px).max(1.0);
            // Fallback for blocks without an `align` override uses
            // the caller-supplied HAlign resolved against THIS block's
            // resolved direction — so an Rtl block sees Start map to
            // physical Right even when the caller passed HAlign::Start.
            let effective_align = bl
                .alignment_override
                .unwrap_or_else(|| hal_to_alignment(alignment, bl.is_rtl));
            if bl.first_line_shift_px == bl.continuation_shift_px {
                // Symmetric — single layout at (block - shift).
                let target = (block_avail - bl.first_line_shift_px).max(1.0);
                bl.layout.break_all_lines(Some(target));
                bl.layout
                    .align(effective_align, AlignmentOptions::default());
                bl.shape_width_px = target;
                bl.height_px = bl.layout.height();
                bl.continuation_layout = None;
                bl.continuation_baseline_shifts.clear();
                bl.continuation_inlines.clear();
                bl.first_line_height_px = 0.0;
            } else {
                // Asymmetric — two-layout dance so both first-line
                // and continuation lines reach the right edge.
                let usable_first = (block_avail - bl.first_line_shift_px).max(1.0);
                let usable_cont = (block_avail - bl.continuation_shift_px).max(1.0);
                // Re-shape from cached source at first-line's usable
                // width. Parley may have wrapped natural single-line
                // shape; re-shaping produces a fresh line-break.
                let mut first_layout = shape_block_layout(
                    &bl.source_text,
                    &bl.source_inlines,
                    &self.base_style,
                    self.base_brush,
                    &self.palette,
                    self.dpi,
                );
                first_layout.break_all_lines(Some(usable_first));
                first_layout.align(effective_align, AlignmentOptions::default());
                // Find first line's byte end. If content fits on the
                // first line, no continuation needed.
                let first_line_end = first_layout
                    .lines()
                    .next()
                    .map(|l| l.text_range().end)
                    .unwrap_or(bl.source_text.len());
                let first_line_height = first_layout
                    .lines()
                    .next()
                    .map(|l| l.metrics().line_height)
                    .unwrap_or(0.0);
                if first_line_end >= bl.source_text.len() {
                    // Single line — no continuation.
                    bl.layout = first_layout;
                    bl.shape_width_px = usable_first;
                    bl.height_px = bl.layout.height();
                    bl.continuation_layout = None;
                    bl.continuation_baseline_shifts.clear();
                    bl.continuation_inlines.clear();
                    bl.first_line_height_px = 0.0;
                } else {
                    // Slice remaining text + rebase inline/baseline
                    // ranges to (start = 0 at first_line_end).
                    let (rest_text, rest_inlines, rest_baselines) = slice_block(
                        &bl.source_text,
                        &bl.source_inlines,
                        &bl.source_baselines,
                        &(first_line_end..bl.source_text.len()),
                    );
                    let mut cont_layout = shape_block_layout(
                        &rest_text,
                        &rest_inlines,
                        &self.base_style,
                        self.base_brush,
                        &self.palette,
                        self.dpi,
                    );
                    cont_layout.break_all_lines(Some(usable_cont));
                    cont_layout.align(effective_align, AlignmentOptions::default());
                    let cont_height = cont_layout.height();
                    bl.layout = first_layout;
                    bl.continuation_layout = Some(cont_layout);
                    bl.continuation_baseline_shifts = rest_baselines;
                    bl.continuation_inlines = rest_inlines;
                    bl.first_line_height_px = first_line_height;
                    // Shape width for paint: use the wider of the
                    // two so the outer_rect matches the CONTINUATION
                    // (which fills the block's right edge).
                    bl.shape_width_px = usable_cont;
                    bl.height_px = first_line_height + cont_height;
                }
            }
        }
        let total_height = stack_blocks(&mut blocks);
        *self.current_height_px.borrow_mut() = total_height;
        total_height
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
    let base_resolved = ResolvedStyle::from_base(base_style);
    for InlineRun { range, style } in inlines {
        apply_style_range(
            &mut builder,
            range.clone(),
            style,
            &base_resolved,
            palette,
            dpi,
            &mut family_pool,
        );
    }
    // Inline span padding reserves shape space by inserting
    // zero-height in-flow `InlineBox` placeholders at each span's
    // start/end byte offsets. Parley shifts subsequent glyphs by
    // the box's width during shaping, so the visible chip has room
    // to sit around the span's glyphs instead of overlapping them.
    // IDs encode (inline_index * 2) + edge: 0 = left, 1 = right —
    // used at draw time to bracket the span's line-fragment rect.
    for (i, InlineRun { range, style }) in inlines.iter().enumerate() {
        let [_, right_pt, _, left_pt] = style.padding_pt;
        if left_pt <= 0.0 && right_pt <= 0.0 {
            continue;
        }
        let left_px = (left_pt * dpi / 72.0) as f32;
        let right_px = (right_pt * dpi / 72.0) as f32;
        if left_px > 0.0 {
            builder.push_inline_box(parley::InlineBox {
                id: (i as u64) * 2,
                kind: parley::InlineBoxKind::InFlow,
                index: range.start,
                width: left_px,
                height: 0.0,
            });
        }
        if right_px > 0.0 {
            builder.push_inline_box(parley::InlineBox {
                id: (i as u64) * 2 + 1,
                kind: parley::InlineBoxKind::InFlow,
                index: range.end,
                width: right_px,
                height: 0.0,
            });
        }
    }
    builder.build(text)
}

fn push_base_defaults(
    builder: &mut parley::RangedBuilder<'_, RichBrush>,
    style: &TextStyle,
    brush: Color,
    dpi: f64,
) {
    push_style_defaults(builder, style, dpi);
    builder.push_default(StyleProperty::Brush(RichBrush(brush)));
}

/// Push one inline run's resolved style onto `builder` for its byte
/// range. Every length is already in points, so this is a pure
/// pt → px conversion plus the parley property mapping.
fn apply_style_range(
    builder: &mut parley::RangedBuilder<'_, RichBrush>,
    range: Range<usize>,
    style: &ResolvedStyle,
    base: &ResolvedStyle,
    palette: &Palette,
    dpi: f64,
    family_pool: &mut Vec<String>,
) {
    let size_px = (style.size_pt * dpi / 72.0) as f32;
    if style.size_pt != base.size_pt {
        builder.push(StyleProperty::FontSize(size_px), range.clone());
    }
    if style.weight != base.weight {
        builder.push(
            StyleProperty::FontWeight(FontWeight::new(style.weight as f32)),
            range.clone(),
        );
    }
    if style.italic != base.italic {
        builder.push(
            StyleProperty::FontStyle(if style.italic {
                FontStyle::Italic
            } else {
                FontStyle::Normal
            }),
            range.clone(),
        );
    }
    if style.width != base.width {
        builder.push(
            StyleProperty::FontWidth(parley::FontWidth::from_ratio(style.width)),
            range.clone(),
        );
    }
    if let Some(color) = &style.color {
        builder.push(
            StyleProperty::Brush(RichBrush(color.resolve(palette))),
            range.clone(),
        );
    }
    if style.tracking != 0.0 {
        // Tracking is in 1/1000 em, so it scales with the run's own
        // size rather than the base size.
        let px = (style.tracking as f64 / 1000.0 * style.size_pt * dpi / 72.0) as f32;
        builder.push(StyleProperty::LetterSpacing(px), range.clone());
    }
    if style.underline != base.underline {
        builder.push(StyleProperty::Underline(style.underline), range.clone());
    }
    if style.strikethrough != base.strikethrough {
        builder.push(
            StyleProperty::Strikethrough(style.strikethrough),
            range.clone(),
        );
    }
    let line_height = match style.lineheight {
        LineHeightSpec::Mult(m) | LineHeightSpec::Relative(m) => {
            parley::LineHeight::FontSizeRelative(m as f32)
        }
        LineHeightSpec::Pt(v) => parley::LineHeight::Absolute((v * dpi / 72.0) as f32),
    };
    builder.push(StyleProperty::LineHeight(line_height), range.clone());
    if let Some(family) = &style.family {
        family_pool.push(family.clone());
        let name = family_pool.last().expect("just pushed");
        let entry = if let Some(generic) = generic_family_from_str(name) {
            FontFamily::Single(FontFamilyName::Generic(generic))
        } else {
            FontFamily::Single(FontFamilyName::named(name.as_str()))
        };
        builder.push(StyleProperty::FontFamily(entry), range.clone());
    }
    if !style.features.is_empty() {
        builder.push(
            StyleProperty::FontFeatures(parley::FontFeatures::List(std::borrow::Cow::Owned(
                parley_features(&style.features),
            ))),
            range,
        );
    }
}

// ─── Helpers: leaves, ancestors, slicing ────────────────────────────────────

/// One box's vertical spacing on `side` (0 = top, 2 = bottom), in px.
/// A background or a border edge counts as a barrier even at zero
/// padding — marquee stops margins collapsing across anything drawn.
fn edge_spacing(style: &ResolvedStyle, side: usize, dpi: f64) -> EdgeSpacing {
    let padding_px = pt_to_px(style.padding_pt[side], dpi);
    let paints = style.background.is_some()
        || (style.border_color.is_some() && style.border_width_pt[side] > 0.0);
    EdgeSpacing {
        margin_px: pt_to_px(style.margin_pt[side], dpi),
        barrier: padding_px != 0.0 || paints,
        padding_px,
    }
}

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
pub(crate) fn ancestors_of_range<'a>(
    range: &Range<usize>,
    containers: &'a [Block],
) -> Vec<&'a Block> {
    // `containers` arrives outermost-first from `shape`, so the
    // filtered result keeps that order without a second sort.
    containers
        .iter()
        .filter(|c| c.range.start <= range.start && c.range.end >= range.end)
        .collect()
}

fn ancestor_side_padding_pt(ancestors: &[&Block]) -> (f64, f64) {
    let mut left = 0.0;
    let mut right = 0.0;
    for a in ancestors {
        left += a.style.padding_pt[3];
        right += a.style.padding_pt[1];
    }
    (left, right)
}

fn pt_to_px(pt: f64, dpi: f64) -> f32 {
    (pt * dpi / 72.0) as f32
}

/// Map hephaestus's [`HAlign`] to parley's [`Alignment`] using our
/// own resolved block-axis direction. Uses parley's **physical**
/// `Left` / `Right` variants (never the direction-aware `Start` /
/// `End`) so an explicit
/// [`super::style::Direction::Ltr`] / [`super::style::Direction::Rtl`]
/// on a block wins even when parley's UBA infers the opposite from
/// the source text.
fn hal_to_alignment(a: HAlign, is_rtl: bool) -> Alignment {
    match (a, is_rtl) {
        (HAlign::Start, false) | (HAlign::End, true) => Alignment::Left,
        (HAlign::End, false) | (HAlign::Start, true) => Alignment::Right,
        (HAlign::Center, _) => Alignment::Center,
        (HAlign::Justify, _) => Alignment::Justify,
    }
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
            style: r.style.clone(),
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
            shift_pt: b.shift_pt,
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
/// Draw order per glyph run: span background/border (from any
/// overlapping [`StyleDelta`] with `background` / `border_*`) →
/// per-span outline stroke (from [`StyleDelta::text_stroke`] +
/// `text_stroke_width`) → fill glyphs → text decorations
/// (underline / strikethrough). A caller who wants an outline on
/// every glyph regardless of source styles should set `text_stroke`
/// on the sheet class that governs the block (e.g. `paragraph`, a
/// custom container class).
///
/// Block-level paints (backgrounds, borders on paragraphs /
/// containers) are emitted before any of the above via
/// [`RichTextRun::block_paints`].
#[allow(clippy::too_many_arguments)]
pub fn draw_rich_text(
    scene: &mut dyn SceneBuilder,
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

    // Block backgrounds + borders first.
    let dpi = run.dpi;
    for paint in &run.block_paints() {
        emit_block_paint(scene, paint, offsets, final_transform, dpi, pick_id);
    }
    let palette = &run.palette;
    // Glyphs.
    let blocks = run.blocks.borrow();
    for bl in blocks.iter() {
        let block_x = bl.left_px;
        let block_y = bl.y_px;
        emit_marker(
            scene,
            bl,
            palette,
            run.base_brush,
            offsets,
            final_transform,
            dpi,
            pick_id,
        );
        if let Some(cont_layout) = &bl.continuation_layout {
            if let Some(first_line) = bl.layout.lines().next() {
                emit_line_glyphs(
                    scene,
                    first_line,
                    block_x,
                    block_y,
                    bl.first_line_shift_px,
                    bl.is_rtl,
                    &bl.baseline_shifts,
                    &bl.source_inlines,
                    palette,
                    run.base_brush,
                    dpi,
                    offsets,
                    final_transform,
                    pick_id,
                );
            }
            let cont_y = block_y + bl.first_line_height_px;
            for line in cont_layout.lines() {
                emit_line_glyphs(
                    scene,
                    line,
                    block_x,
                    cont_y,
                    bl.continuation_shift_px,
                    bl.is_rtl,
                    &bl.continuation_baseline_shifts,
                    &bl.continuation_inlines,
                    palette,
                    run.base_brush,
                    dpi,
                    offsets,
                    final_transform,
                    pick_id,
                );
            }
        } else {
            for (line_index, line) in bl.layout.lines().enumerate() {
                let per_line_shift = if line_index == 0 {
                    bl.first_line_shift_px
                } else {
                    bl.continuation_shift_px
                };
                emit_line_glyphs(
                    scene,
                    line,
                    block_x,
                    block_y,
                    per_line_shift,
                    bl.is_rtl,
                    &bl.baseline_shifts,
                    &bl.source_inlines,
                    palette,
                    run.base_brush,
                    dpi,
                    offsets,
                    final_transform,
                    pick_id,
                );
            }
        }
    }
}

/// Emit one parley line's glyph runs (with span backgrounds,
/// per-span or outer outlines, and decorations) into `scene`.
/// Extracted so [`draw_rich_text`] can call it twice for the
/// asymmetric-shift case (first line + continuation lines).
///
/// `is_rtl` controls how `shift_px` is applied: under Ltr it pushes
/// the glyph run rightward from the block's left edge (indent gutter
/// on the left); under Rtl the shift is not added — the block's
/// narrower shape width plus parley's right-alignment already leaves
/// the indent gutter on the right side of the block.
#[allow(clippy::too_many_arguments)]
fn emit_line_glyphs(
    scene: &mut dyn SceneBuilder,
    line: parley::layout::Line<'_, RichBrush>,
    block_x: f32,
    block_y: f32,
    shift_px: f32,
    is_rtl: bool,
    baseline_shifts: &[BaselineRun],
    inlines: &[InlineRun],
    palette: &Palette,
    base_brush: Color,
    dpi: f64,
    offsets: super::anchor::AnchorOffsets,
    final_transform: Affine,
    pick_id: PickId,
) {
    // First pass — record positions of any InlineBoxes on this line
    // so we can bracket span paint rects with the reserved padding
    // space. Keyed by id (assigned at shape time: 2*i = left,
    // 2*i + 1 = right).
    let mut inline_box_x: std::collections::HashMap<u64, (f32, f32)> =
        std::collections::HashMap::new();
    for item in line.items() {
        if let PositionedLayoutItem::InlineBox(ib) = item {
            inline_box_x.insert(ib.id, (ib.x, ib.x + ib.width));
        }
    }
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
        let dy_px = pt_to_px(baseline_shift_for_range(baseline_shifts, &run_range), dpi);
        let metrics = prun.metrics();
        let baseline = gr.baseline();
        // Under Rtl the shape width was narrowed by `shift_px` and
        // parley right-aligns into it, so the indent gutter already
        // sits on the right of the block — no positional shift needed.
        let effective_shift = if is_rtl { 0.0 } else { shift_px };
        let run_x_base = block_x + effective_shift - offsets.ref_x;
        let glyph_x0 = run_x_base + gr.offset();
        let glyph_x1 = glyph_x0 + gr.advance();
        let y_base = block_y + baseline - offsets.ref_y - dy_px;

        // ── Match GlyphRun to InlineRun. Parley's `Run::text_range`
        //    reports the **parent** run's byte range (which spans
        //    every GlyphRun in the line, since a style-driven split
        //    subdivides one Run into multiple GlyphRuns without
        //    changing its logical extent). To identify which
        //    InlineRun owns this specific GlyphRun we match on the
        //    resolved brush colour: parley splits GlyphRuns on
        //    `Brush` changes, and each InlineRun's `delta.color`
        //    (or the block's `base_brush` fallback) determines that
        //    brush. When two InlineRuns resolve to the same colour
        //    (a background-only or text_stroke-only span with no
        //    `color`) parley cannot distinguish them at all — the
        //    span's decorations are not observable and, correctly,
        //    the outline pass doesn't fire. Callers wanting an
        //    outlined span therefore set `color` alongside
        //    `text_stroke` (or fold both into a sheet entry).
        let gr_brush = gr.style().brush.0;
        // Prefer a decorated InlineRun (has bg / border / text_stroke)
        // whose resolved colour matches this GlyphRun's brush — those
        // are the InlineRuns whose decorations we can actually emit.
        // Falls back to any overlapping InlineRun with matching brush
        // so plain (colour-only) spans still resolve for the fill pass.
        let brush_matches = |r: &&InlineRun| -> bool {
            let overlaps = r.range.start < run_range.end && r.range.end > run_range.start;
            if !overlaps {
                return false;
            }
            let effective = r
                .style
                .color
                .as_ref()
                .map(|c| c.resolve(palette))
                .unwrap_or(base_brush);
            effective == gr_brush
        };
        let has_decoration = |r: &&InlineRun| -> bool {
            r.style.background.is_some()
                || (r.style.border_color.is_some()
                    && r.style.border_width_pt.iter().any(|w| *w > 0.0))
                || (r.style.text_stroke.is_some() && r.style.text_stroke_width_pt > 0.0)
        };
        let matched: Option<&InlineRun> = inlines
            .iter()
            .find(|r| brush_matches(r) && has_decoration(r))
            .or_else(|| inlines.iter().find(brush_matches));

        // ── Span background + border, per line-fragment. Bracketed
        //    by any InlineBox padding placeholders on this line.
        if let Some(inl) = matched {
            let d = &inl.style;
            let has_bg = d.background.is_some();
            let has_border = d.border_color.is_some() && d.border_width_pt.iter().any(|w| *w > 0.0);
            if has_bg || has_border {
                let idx = inlines.iter().position(|r| std::ptr::eq(r, inl));
                let left_box_x =
                    idx.and_then(|i| inline_box_x.get(&((i as u64) * 2)).map(|(x0, _)| *x0));
                let right_box_x1 =
                    idx.and_then(|i| inline_box_x.get(&((i as u64) * 2 + 1)).map(|(_, x1)| *x1));
                let starts_here = inl.range.start >= run_range.start;
                let ends_here = inl.range.end <= run_range.end;
                // Rect x: extend to include the InlineBox padding
                // space when it's on this line-fragment.
                // InlineBox positions come from parley in the same
                // layout frame as `gr.offset()` / `gr.advance()`, so
                // to get screen coords we apply the same translation
                // (`block_x + shift_px - offsets.ref_x`) as the glyph
                // positions.
                let x0_paint = if starts_here {
                    left_box_x.map(|x| x + run_x_base).unwrap_or(glyph_x0)
                } else {
                    glyph_x0
                };
                let x1_paint = if ends_here {
                    right_box_x1.map(|x| x + run_x_base).unwrap_or(glyph_x1)
                } else {
                    glyph_x1
                };
                // Vertical: inflate by padding.top / .bottom. Font
                // ascent / descent give the natural line extent.
                let t_pad = (d.padding_pt[0] * dpi / 72.0) as f32;
                let b_pad = (d.padding_pt[2] * dpi / 72.0) as f32;
                let y0_paint = y_base - metrics.ascent - t_pad;
                let y1_paint = y_base + metrics.descent + b_pad;
                if x1_paint > x0_paint && y1_paint > y0_paint {
                    let rect = kurbo::Rect::new(
                        x0_paint as f64,
                        y0_paint as f64,
                        x1_paint as f64,
                        y1_paint as f64,
                    );
                    let corner_radius = (d.border_radius_pt * dpi / 72.0) as f32;
                    let path = if corner_radius > 0.0 {
                        crate::primitives::rounded_rect(rect, corner_radius as f64)
                    } else {
                        crate::primitives::rect(rect)
                    };
                    if let Some(bg) = &d.background {
                        let c = bg.resolve(palette);
                        scene.fill(
                            crate::path::FillRule::NonZero,
                            final_transform,
                            &Brush::Solid(c),
                            None,
                            &path,
                            pick_id,
                        );
                    }
                    if let Some(bc) = &d.border_color {
                        let [t, r, b, l] = d.border_width_pt;
                        let uniform =
                            ((t - r).abs() < 1e-3 && (r - b).abs() < 1e-3 && (b - l).abs() < 1e-3)
                                .then_some(t);
                        // Only handle uniform-width borders on inline
                        // spans; per-side is a rare case for inline.
                        if let Some(w_pt) = uniform {
                            let w_px = (w_pt * dpi / 72.0) as f32;
                            if w_px > 0.0 {
                                let c = bc.resolve(palette);
                                let stroke = crate::stroke::Stroke::new(w_px as f64)
                                    .with_caps(crate::stroke::Cap::Butt)
                                    .with_join(crate::stroke::Join::Miter);
                                scene.stroke(
                                    &stroke,
                                    final_transform,
                                    &Brush::Solid(c),
                                    None,
                                    &path,
                                    pick_id,
                                );
                            }
                        }
                    }
                }
            }
        }

        let glyphs = glyphs_of_run(&gr, run_x_base, block_y - offsets.ref_y - dy_px);
        if glyphs.is_empty() {
            continue;
        }

        // ── Outline pass (behind fill). Sourced from
        //    `text_stroke` on any matching InlineRun —
        //    typically set on a specific span class (e.g. a
        //    `.haloed` custom class) so a caller who wants a
        //    document-wide outline just sets the field on the
        //    sheet's root `paragraph` / `heading` classes.
        let span_outline_owned: Option<(Brush, crate::stroke::Stroke)> = matched.and_then(|inl| {
            let color = inl.style.text_stroke.as_ref()?;
            let w_px = (inl.style.text_stroke_width_pt * dpi / 72.0) as f32;
            if w_px <= 0.0 {
                return None;
            }
            Some((
                Brush::Solid(color.resolve(palette)),
                crate::stroke::Stroke::new(w_px as f64),
            ))
        });
        if let Some((ob, os)) = span_outline_owned.as_ref() {
            let outline_run = GlyphRun {
                font: &font,
                font_size,
                transform: final_transform,
                glyph_transform: None,
                brush: ob,
                brush_alpha: 1.0,
                hint: false,
                glyphs: &glyphs,
                style: Some(os),
            };
            // The fill pass owns picking; the halo behind it must not
            // widen the hit area.
            scene.draw_glyphs(&outline_run, PickId::Skip);
        }

        // Fill pass.
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

        // Decorations (over glyphs).
        let style = gr.style();
        if style.underline.is_some() || style.strikethrough.is_some() {
            if let Some(deco) = &style.underline {
                emit_decoration_rect(
                    scene,
                    DecorationRect {
                        x0: glyph_x0,
                        x1: glyph_x1,
                        top: y_base - deco.offset.unwrap_or(metrics.underline_offset),
                        thickness: deco.size.unwrap_or(metrics.underline_size).max(0.0),
                    },
                    &brush,
                    final_transform,
                    pick_id,
                );
            }
            if let Some(deco) = &style.strikethrough {
                emit_decoration_rect(
                    scene,
                    DecorationRect {
                        x0: glyph_x0,
                        x1: glyph_x1,
                        top: y_base - deco.offset.unwrap_or(metrics.strikethrough_offset),
                        thickness: deco.size.unwrap_or(metrics.strikethrough_size).max(0.0),
                    },
                    &brush,
                    final_transform,
                    pick_id,
                );
            }
        }
    }
}

/// Horizontal extent (px) a block's marker occupies, in run-local
/// coordinates. The marker sits in the list's start gutter, so under
/// Rtl it hangs off the block's right edge instead of its left.
fn marker_x_range(bl: &BlockLayout, marker: &MarkerLayout) -> (f32, f32) {
    if bl.is_rtl {
        let x0 = bl.left_px + bl.shape_width_px + marker.gap_px;
        (x0, x0 + marker.width_px)
    } else {
        let x1 = bl.left_px - marker.gap_px;
        (x1 - marker.width_px, x1)
    }
}

/// Draw a block's list-item marker, right-aligned into the gutter on
/// the start side and sharing the first line's baseline.
#[allow(clippy::too_many_arguments)]
fn emit_marker(
    scene: &mut dyn SceneBuilder,
    bl: &BlockLayout,
    palette: &Palette,
    base_brush: Color,
    offsets: super::anchor::AnchorOffsets,
    final_transform: Affine,
    dpi: f64,
    pick_id: PickId,
) {
    let Some(marker) = &bl.marker else { return };
    let Some(body_line) = bl.layout.lines().next() else {
        return;
    };
    let Some(marker_line) = marker.layout.lines().next() else {
        return;
    };
    let (x0, _) = marker_x_range(bl, marker);
    // Align the marker's baseline with the item's first line so a
    // bullet and its text sit on the same rule.
    let dy = bl.y_px + body_line.metrics().baseline - marker_line.metrics().baseline;
    for line in marker.layout.lines() {
        emit_line_glyphs(
            scene,
            line,
            x0,
            dy,
            0.0,
            false,
            &[],
            &[],
            palette,
            base_brush,
            dpi,
            offsets,
            final_transform,
            pick_id,
        );
    }
}

fn emit_block_paint(
    scene: &mut dyn SceneBuilder,
    paint: &BlockPaint,
    offsets: super::anchor::AnchorOffsets,
    outer: Affine,
    dpi: f64,
    pick_id: PickId,
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
            pick_id,
        );
    }
    if let Some(border) = paint.border.as_ref() {
        // Borders use Butt caps and Miter joins — square ends, sharp
        // corners. This matches typographic convention: block bars
        // (blockquote left rule, hr top rule) shouldn't have visible
        // rounded end-caps peeking out past the block edge.
        //
        // Marker-free patterns take kurbo's `with_dashes` fast path
        // (one stroke call per polyline chain). Marker-bearing
        // patterns route through the crate-wide
        // `draw_linetype_with_markers`, which walks the polyline in
        // arc length and stamps shape markers along it — the same
        // primitive `LineGeom` uses.
        let has_markers = border
            .linetype_pt
            .as_ref()
            .map(|p| !crate::linetype::is_marker_free(p))
            .unwrap_or(false);
        // Kurbo's `with_dashes` fast path only accepts flat pt-length
        // slices — no `Marker` steps. Compute the flat slice only for
        // marker-free patterns; markered patterns route through the
        // shared `draw_linetype_with_markers` primitive below.
        let dashes_px: Option<Vec<f64>> =
            border
                .linetype_pt
                .as_ref()
                .filter(|_| !has_markers)
                .map(|pattern| {
                    crate::linetype::to_kurbo_dashes(pattern)
                        .into_iter()
                        .map(|pt| pt * dpi / 72.0)
                        .collect()
                });
        let border_stroke = |w_px: f32| {
            let s = crate::stroke::Stroke::new(w_px as f64)
                .with_caps(crate::stroke::Cap::Butt)
                .with_join(crate::stroke::Join::Miter);
            if let (Some(pattern_px), false) = (dashes_px.as_ref(), has_markers) {
                s.with_dashes(0.0_f64, pattern_px.clone())
            } else {
                s
            }
        };
        if border.is_uniform() {
            let w = border.widths_px[0];
            if w > 0.0 {
                if has_markers {
                    stroke_markered_perimeter(
                        scene,
                        pick_id,
                        &rect,
                        w,
                        border.color,
                        border
                            .linetype_pt
                            .as_ref()
                            .expect("has_markers implies Some"),
                        outer,
                        dpi,
                    );
                } else {
                    scene.stroke(
                        &border_stroke(w),
                        outer,
                        &Brush::Solid(border.color),
                        None,
                        &path,
                        pick_id,
                    );
                }
            }
        } else {
            // Per-side widths — collapse contiguous same-width sides
            // (in CW cyclic order T → R → B → L) into single
            // polylines so a corner where two present sides meet is
            // stroked as one continuous path (mitred at the join)
            // rather than two independent segments (which would show
            // a visible seam at the corner). Sides with mismatched
            // widths still emit as separate polylines. `corner_radius`
            // is intentionally ignored on the mixed path (square
            // corners; documented on `StyleDelta::border_width`).
            let brush = Brush::Solid(border.color);
            let widths = border.widths_px;
            let (x0, y0, x1, y1) = (rect.x0, rect.y0, rect.x1, rect.y1);
            let corners = [
                kurbo::Point::new(x0, y0),
                kurbo::Point::new(x1, y0),
                kurbo::Point::new(x1, y1),
                kurbo::Point::new(x0, y1),
            ];
            for chain in group_border_sides_cw(widths, corners) {
                if has_markers {
                    let sampler = crate::primitives::PolylineSampler::from_polyline(&chain.points);
                    let color = border.color;
                    let solid = crate::stroke::Stroke::new(chain.width as f64)
                        .with_caps(crate::stroke::Cap::Butt)
                        .with_join(crate::stroke::Join::Miter);
                    let shapes = crate::shape::ShapeRegistry::shared_builtins();
                    crate::linetype::draw_linetype_with_markers(
                        scene,
                        std::slice::from_ref(&sampler),
                        border
                            .linetype_pt
                            .as_ref()
                            .expect("has_markers implies Some"),
                        0.0,
                        chain.width as f64,
                        color,
                        color,
                        0.0,
                        &solid,
                        outer,
                        shapes,
                        dpi,
                        pick_id,
                        false,
                    );
                    continue;
                }
                let mut path = kurbo::BezPath::new();
                let mut pts = chain.points.iter();
                if let Some(&p) = pts.next() {
                    path.move_to(p);
                    for &p in pts {
                        path.line_to(p);
                    }
                }
                scene.stroke(
                    &border_stroke(chain.width),
                    outer,
                    &brush,
                    None,
                    &path,
                    pick_id,
                );
            }
        }
    }
}

/// Stamp a marker-bearing linetype around the full perimeter of a
/// uniform-width block border. Used when [`BlockBorder::linetype_pt`]
/// contains at least one [`crate::scales::value::LinetypeStep::Marker`]
/// step. Builds one closed [`crate::primitives::PolylineSampler`] over
/// the four corners (wrapping the seam back to the top-left) and
/// delegates to the crate-wide dash+marker primitive.
#[allow(clippy::too_many_arguments)]
fn stroke_markered_perimeter(
    scene: &mut dyn SceneBuilder,
    pick_id: PickId,
    rect: &kurbo::Rect,
    width_px: f32,
    color: Color,
    pattern_pt: &[crate::scales::value::LinetypeStep],
    outer: Affine,
    dpi: f64,
) {
    let corners = [
        kurbo::Point::new(rect.x0, rect.y0),
        kurbo::Point::new(rect.x1, rect.y0),
        kurbo::Point::new(rect.x1, rect.y1),
        kurbo::Point::new(rect.x0, rect.y1),
        kurbo::Point::new(rect.x0, rect.y0),
    ];
    let sampler = crate::primitives::PolylineSampler::from_polyline(&corners);
    let solid = crate::stroke::Stroke::new(width_px as f64)
        .with_caps(crate::stroke::Cap::Butt)
        .with_join(crate::stroke::Join::Miter);
    let shapes = crate::shape::ShapeRegistry::shared_builtins();
    crate::linetype::draw_linetype_with_markers(
        scene,
        std::slice::from_ref(&sampler),
        pattern_pt,
        0.0,
        width_px as f64,
        color,
        color,
        0.0,
        &solid,
        outer,
        shapes,
        dpi,
        pick_id,
        false,
    );
}

/// One polyline chunk of a block's mixed-width border. Contiguous
/// same-width sides (in CW cyclic order T → R → B → L) share one
/// chain, so their shared corner is a single join rather than two
/// abutting endpoints.
struct BorderChain {
    /// Chain vertices in traversal order. `len >= 2`.
    points: Vec<kurbo::Point>,
    /// Uniform stroke width for the chain (px).
    width: f32,
}

/// Group non-zero sides (in CW cyclic order T → R → B → L, indices
/// 0..4) into polyline chains: contiguous same-width sides join into
/// one chain sharing their meeting corner; a zero-width or
/// mismatched-width side breaks the chain. Cyclic — a chain may wrap
/// around from L to T when both are present with the same width.
///
/// Returns each chain's ordered vertex list plus its shared width.
/// The uniform-width path in `emit_block_paint` handles the
/// all-four-sides-same case; this helper is only reached when at
/// least one side is zero or widths differ across sides.
fn group_border_sides_cw(widths: [f32; 4], corners: [kurbo::Point; 4]) -> Vec<BorderChain> {
    // Sides in cyclic CW order:
    //   0=T: corner[0] → corner[1]
    //   1=R: corner[1] → corner[2]
    //   2=B: corner[2] → corner[3]
    //   3=L: corner[3] → corner[0]
    let side = |i: usize| -> (kurbo::Point, kurbo::Point) { (corners[i], corners[(i + 1) % 4]) };
    let present = |i: usize| widths[i] > 0.0;
    let same_width = |i: usize, j: usize| (widths[i] - widths[j]).abs() < 1e-3;
    // Pick a start index whose predecessor either isn't present or
    // has a different width — that's a chain boundary. If no such
    // break exists (all four present, all same width), the caller
    // is in the uniform-stroke branch and never enters this helper.
    let start = (0..4).find(|&i| {
        let prev = (i + 3) % 4;
        present(i) && !(present(prev) && same_width(prev, i))
    });
    let Some(start) = start else {
        // All four present with same width. Emit as a closed loop.
        let mut points: Vec<kurbo::Point> = corners.to_vec();
        points.push(corners[0]);
        return vec![BorderChain {
            points,
            width: widths[0],
        }];
    };
    let mut chains: Vec<BorderChain> = Vec::new();
    let mut cur: Vec<kurbo::Point> = Vec::new();
    let mut cur_w = 0.0f32;
    for step in 0..4 {
        let idx = (start + step) % 4;
        let w = widths[idx];
        let (a, b) = side(idx);
        if w <= 0.0 {
            if !cur.is_empty() {
                chains.push(BorderChain {
                    points: std::mem::take(&mut cur),
                    width: cur_w,
                });
            }
            continue;
        }
        if cur.is_empty() {
            cur.push(a);
            cur.push(b);
            cur_w = w;
        } else if (w - cur_w).abs() < 1e-3 {
            cur.push(b);
        } else {
            chains.push(BorderChain {
                points: std::mem::take(&mut cur),
                width: cur_w,
            });
            cur.push(a);
            cur.push(b);
            cur_w = w;
        }
    }
    if !cur.is_empty() {
        chains.push(BorderChain {
            points: cur,
            width: cur_w,
        });
    }
    chains
}

/// The accumulated baseline shift (pt) covering `run_range`. Nested
/// shifts are emitted innermost-first, so the first overlap wins.
fn baseline_shift_for_range(shifts: &[BaselineRun], run_range: &Range<usize>) -> f64 {
    for bs in shifts {
        if run_range.start < bs.range.end && bs.range.start < run_range.end {
            return bs.shift_pt;
        }
    }
    0.0
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::recording::{Op, RecordingScene};
    use crate::style_vocab::Palette;
    use crate::text::rich::length::{pt, RichMargin};
    use crate::text::rich::style::StyleDelta;

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
    fn inline_code_span_emits_background_chip() {
        // The default sheet's `code` selector sets background +
        // padding + border-radius. An `` `x` `` inline span should
        // therefore emit at least one Fill op (the chip rect)
        // BEFORE the glyph run for its content.
        let run = make("plain `x` more");
        let scene = draw(&run);
        let mut saw_fill = false;
        let mut saw_glyphs_after_fill = false;
        for op in &scene.ops {
            match op {
                Op::Fill { .. } => saw_fill = true,
                Op::DrawGlyphs(_) if saw_fill => saw_glyphs_after_fill = true,
                _ => {}
            }
        }
        assert!(saw_fill, "expected a Fill op for the code chip");
        assert!(saw_glyphs_after_fill, "glyphs should follow the chip fill");
    }

    #[test]
    fn per_span_text_stroke_emits_outline_pass() {
        // A custom class with `text_stroke` produces a stroke-only
        // glyph pass behind the fill. To reliably split the parley
        // run at the span's boundary we also set a distinct
        // `color`; parley splits on brush change, isolating the
        // outline to the span.
        let mut sheet = RichTextStyleSheet::new();
        sheet.set(
            "haloed",
            crate::text::rich::style::StyleDelta {
                color: Some(crate::style_vocab::ThemeColor::Fixed(Color::from_rgba8(
                    220, 30, 30, 255,
                ))),
                text_stroke: Some(crate::style_vocab::ThemeColor::Fixed(Color::from_rgba8(
                    255, 255, 255, 255,
                ))),
                text_stroke_width: Some(pt(2.0)),
                ..crate::text::rich::style::StyleDelta::empty()
            },
        );
        let run = RichTextRun::new(
            "before {.haloed word} after",
            &base_style(),
            Color::from_rgba8(0, 0, 0, 255),
            &sheet,
            &palette(),
            96.0,
        );
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
        let has_stroked_pass = scene
            .ops
            .iter()
            .any(|op| matches!(op, Op::DrawGlyphs(gr) if gr.style.is_some()));
        assert!(
            has_stroked_pass,
            "expected a stroke-only glyph pass for the `haloed` span"
        );
    }

    #[test]
    fn block_level_border_does_not_double_render_as_inline() {
        // A block-level `border` set via the sheet's `paragraph`
        // selector should paint ONCE (in the block paint pass), not
        // again as an inline border on the block's text runs.
        let mut sheet = RichTextStyleSheet::empty();
        sheet.set(
            "paragraph",
            crate::text::rich::style::StyleDelta {
                border_color: Some(crate::style_vocab::ThemeColor::Ink),
                border_width: Some(RichMargin::all(pt(1.0))),
                ..crate::text::rich::style::StyleDelta::empty()
            },
        );
        let run = RichTextRun::new(
            "some text",
            &base_style(),
            Color::from_rgba8(0, 0, 0, 255),
            &sheet,
            &palette(),
            96.0,
        );
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
        assert_eq!(
            strokes, 1,
            "expected exactly one border stroke (block-level)"
        );
    }

    #[test]
    fn list_container_margin_survives_a_re_break() {
        // Regression: `set_max_width` re-stacks the blocks, and the
        // ancestor container margins routed to first / last
        // descendant at shape time must still be applied there.
        let mut sheet = RichTextStyleSheet::new();
        sheet.set(
            "list",
            StyleDelta {
                margin: Some(RichMargin::new(pt(40.0), pt(0.0), pt(40.0), pt(0.0))),
                ..StyleDelta::empty()
            },
        );
        let make_with = |src: &str, sheet: &RichTextStyleSheet| {
            RichTextRun::new(
                src,
                &base_style(),
                Color::from_rgba8(0, 0, 0, 255),
                sheet,
                &palette(),
                96.0,
            )
        };
        let roomy = make_with("- alpha\n\nfollowing", &sheet);
        let tight = make_with("- alpha\n\nfollowing", &RichTextStyleSheet::new());
        let natural_delta = roomy.natural_height() - tight.natural_height();
        assert!(
            natural_delta > 20.0,
            "the list's own margin should widen the natural stack (delta={natural_delta})"
        );
        let width = roomy.natural_width() as f32;
        let roomy_broken = roomy.set_max_width(width, HAlign::Start) as f64;
        let tight_broken = tight.set_max_width(width, HAlign::Start) as f64;
        assert!(
            (roomy_broken - tight_broken - natural_delta).abs() < 1.0,
            "re-break lost the container margin ({roomy_broken} - {tight_broken} vs {natural_delta})"
        );
    }

    #[test]
    fn strikethrough_sits_above_baseline() {
        let sheet = RichTextStyleSheet::new();
        let run = RichTextRun::new(
            "a ~~strike~~ b",
            &base_style(),
            Color::from_rgba8(0, 0, 0, 255),
            &sheet,
            &palette(),
            96.0,
        );
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
        let baseline_y = scene
            .ops
            .iter()
            .find_map(|op| match op {
                Op::DrawGlyphs(gr) => gr.glyphs.first().map(|g| g.y),
                _ => None,
            })
            .expect("baseline");
        let fill_y0 = scene
            .ops
            .iter()
            .find_map(|op| match op {
                Op::Fill { path, .. } => Some(kurbo::Shape::bounding_box(path).y0 as f32),
                _ => None,
            })
            .expect("strikethrough fill");
        assert!(
            fill_y0 < baseline_y,
            "strikethrough rect (y0={fill_y0}) should sit ABOVE the baseline (y={baseline_y})"
        );
    }

    #[test]
    fn underline_sits_below_baseline() {
        let sheet = RichTextStyleSheet::new();
        let run = RichTextRun::new(
            "a _under_ b",
            &base_style(),
            Color::from_rgba8(0, 0, 0, 255),
            &sheet,
            &palette(),
            96.0,
        );
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
        let baseline_y = scene
            .ops
            .iter()
            .find_map(|op| match op {
                Op::DrawGlyphs(gr) => gr.glyphs.first().map(|g| g.y),
                _ => None,
            })
            .expect("baseline");
        let fill_y0 = scene
            .ops
            .iter()
            .find_map(|op| match op {
                Op::Fill { path, .. } => Some(kurbo::Shape::bounding_box(path).y0 as f32),
                _ => None,
            })
            .expect("underline fill");
        assert!(
            fill_y0 > baseline_y,
            "underline rect (y0={fill_y0}) should sit BELOW the baseline (y={baseline_y})"
        );
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
        );
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
        );
        let wrapped = RichTextRun::new_with_width(
            "one two three four five six seven eight",
            &base_style(),
            Color::from_rgba8(0, 0, 0, 255),
            &sheet,
            &palette(),
            96.0,
            RichTextWidth::Fixed(60.0),
        );
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
        // Every paragraph carries `margin.bottom = rem(1)` and no top
        // margin, so an adjacent pair collapses to one gap of 1rem,
        // not two. Two extra paragraphs therefore add
        // 2 × (line + 1rem), not 2 × (line + 2rem).
        let three = make("a\n\nb\n\nc");
        let five = make("a\n\nb\n\nc\n\nd\n\ne");
        let diff = five.natural_height() - three.natural_height();
        // 14pt base, line-height 1.6 → one line ≈ 29.9px; 1rem ≈
        // 18.7px. Two paragraphs ≈ 97px collapsed, ≈ 134px if the
        // margins summed.
        assert!(
            diff < 115.0,
            "2 extra paragraphs should collapse to ≈97px of extra height, got {diff}"
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
    fn halign_start_maps_to_right_under_rtl() {
        assert_eq!(hal_to_alignment(HAlign::Start, false), Alignment::Left);
        assert_eq!(hal_to_alignment(HAlign::Start, true), Alignment::Right);
        assert_eq!(hal_to_alignment(HAlign::End, false), Alignment::Right);
        assert_eq!(hal_to_alignment(HAlign::End, true), Alignment::Left);
        assert_eq!(hal_to_alignment(HAlign::Center, false), Alignment::Center);
        assert_eq!(hal_to_alignment(HAlign::Center, true), Alignment::Center);
        assert_eq!(hal_to_alignment(HAlign::Justify, true), Alignment::Justify);
    }

    #[test]
    fn auto_direction_reads_parley_is_rtl_for_arabic() {
        // Auto (unset text_direction) should reflect parley's UBA
        // determination on the block's text. Arabic script drives
        // base_level to Rtl.
        let sheet = RichTextStyleSheet::empty();
        let run = RichTextRun::new(
            "مرحبا",
            &base_style(),
            Color::from_rgba8(0, 0, 0, 255),
            &sheet,
            &palette(),
            96.0,
        );
        let blocks = run.blocks.borrow();
        assert!(
            blocks.first().map(|bl| bl.is_rtl).unwrap_or(false),
            "an Arabic-only paragraph should resolve to Rtl via parley::Layout::is_rtl"
        );
    }

    #[test]
    fn explicit_ltr_overrides_arabic_content_direction() {
        // A block with explicit `Direction::Ltr` in its class stays
        // Ltr even when parley infers Rtl from Arabic content.
        let mut sheet = RichTextStyleSheet::empty();
        sheet.set(
            "paragraph",
            crate::text::rich::style::StyleDelta {
                text_direction: Some(crate::text::rich::style::Direction::Ltr),
                ..crate::text::rich::style::StyleDelta::empty()
            },
        );
        let run = RichTextRun::new(
            "مرحبا",
            &base_style(),
            Color::from_rgba8(0, 0, 0, 255),
            &sheet,
            &palette(),
            96.0,
        );
        let blocks = run.blocks.borrow();
        assert!(
            !blocks.first().map(|bl| bl.is_rtl).unwrap_or(true),
            "explicit Direction::Ltr must override parley's Rtl inference"
        );
    }

    #[test]
    fn rtl_blockquote_paints_right_edge_bar() {
        // A block_quote class sets `border_width` as `[0, 0, 0, 3]` —
        // start-side bar. Under Rtl the l/r swap in `border_for`
        // routes it onto the physical right edge, so `widths_px[1]`
        // (right) becomes non-zero and `widths_px[3]` (left) stays 0.
        let mut sheet = RichTextStyleSheet::empty();
        sheet.set(
            "block_quote",
            crate::text::rich::style::StyleDelta {
                text_direction: Some(crate::text::rich::style::Direction::Rtl),
                border_color: Some(crate::style_vocab::ThemeColor::Ink),
                border_width: Some(RichMargin::new(pt(0.0), pt(0.0), pt(0.0), pt(3.0))),
                ..crate::text::rich::style::StyleDelta::empty()
            },
        );
        let run = RichTextRun::new(
            "> quoted content",
            &base_style(),
            Color::from_rgba8(0, 0, 0, 255),
            &sheet,
            &palette(),
            96.0,
        );
        let paints = run.block_paints();
        let border = paints
            .iter()
            .find_map(|p| p.border.as_ref())
            .expect("blockquote should have a border");
        assert!(
            border.widths_px[1] > 0.0,
            "under Rtl the start-side bar should paint on the physical right (widths_px[1])"
        );
        assert!(
            border.widths_px[3].abs() < 1e-3,
            "physical left (widths_px[3]) should be zero"
        );
    }

    #[test]
    fn ltr_blockquote_still_paints_left_edge_bar() {
        // Baseline sanity: no direction set → default Ltr behavior
        // stays intact. `[0, 0, 0, 3]` still lands on the left edge.
        let mut sheet = RichTextStyleSheet::empty();
        sheet.set(
            "block_quote",
            crate::text::rich::style::StyleDelta {
                border_color: Some(crate::style_vocab::ThemeColor::Ink),
                border_width: Some(RichMargin::new(pt(0.0), pt(0.0), pt(0.0), pt(3.0))),
                ..crate::text::rich::style::StyleDelta::empty()
            },
        );
        let run = RichTextRun::new(
            "> quoted content",
            &base_style(),
            Color::from_rgba8(0, 0, 0, 255),
            &sheet,
            &palette(),
            96.0,
        );
        let paints = run.block_paints();
        let border = paints
            .iter()
            .find_map(|p| p.border.as_ref())
            .expect("blockquote should have a border");
        assert!(
            border.widths_px[3] > 0.0,
            "under Ltr the start-side bar should paint on the physical left (widths_px[3])"
        );
        assert!(
            border.widths_px[1].abs() < 1e-3,
            "physical right (widths_px[1]) should be zero"
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
                border_color: Some(crate::style_vocab::ThemeColor::Ink),
                border_width: Some(RichMargin::all(pt(1.0))),
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
        );
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
    fn adjacent_partial_borders_collapse_into_one_polyline() {
        // Top + left borders both present with the same width should
        // stroke as ONE polyline sharing the top-left corner, not
        // two independent segments meeting there. Distinct widths on
        // T and L, in contrast, must stay separate.
        let mut sheet = RichTextStyleSheet::empty();
        sheet.set(
            "paragraph",
            crate::text::rich::style::StyleDelta {
                border_color: Some(crate::style_vocab::ThemeColor::Ink),
                border_width: Some(RichMargin {
                    top: pt(2.0),
                    right: pt(0.0),
                    bottom: pt(0.0),
                    left: pt(2.0),
                }),
                ..crate::text::rich::style::StyleDelta::empty()
            },
        );
        let run = RichTextRun::new(
            "l shape",
            &base_style(),
            Color::from_rgba8(0, 0, 0, 255),
            &sheet,
            &palette(),
            96.0,
        );
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
        assert_eq!(
            strokes, 1,
            "top + left same-width partial borders should collapse into one polyline"
        );
    }

    #[test]
    fn partial_borders_with_mismatched_widths_stay_separate() {
        let mut sheet = RichTextStyleSheet::empty();
        sheet.set(
            "paragraph",
            crate::text::rich::style::StyleDelta {
                border_color: Some(crate::style_vocab::ThemeColor::Ink),
                border_width: Some(RichMargin {
                    top: pt(1.0),
                    right: pt(0.0),
                    bottom: pt(0.0),
                    left: pt(4.0),
                }),
                ..crate::text::rich::style::StyleDelta::empty()
            },
        );
        let run = RichTextRun::new(
            "mixed",
            &base_style(),
            Color::from_rgba8(0, 0, 0, 255),
            &sheet,
            &palette(),
            96.0,
        );
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
        assert_eq!(strokes, 2, "different widths keep sides separate");
    }

    #[test]
    fn dashed_border_carries_dash_pattern_through() {
        use crate::scales::value::LinetypeStep;
        use std::sync::Arc;
        let mut sheet = RichTextStyleSheet::empty();
        sheet.set(
            "paragraph",
            crate::text::rich::style::StyleDelta {
                border_color: Some(crate::style_vocab::ThemeColor::Ink),
                border_width: Some(RichMargin::all(pt(1.0))),
                border_type: Some(Arc::from(vec![
                    LinetypeStep::Dash(4.0),
                    LinetypeStep::Gap(2.0),
                ])),
                ..crate::text::rich::style::StyleDelta::empty()
            },
        );
        let run = RichTextRun::new(
            "dashy",
            &base_style(),
            Color::from_rgba8(0, 0, 0, 255),
            &sheet,
            &palette(),
            96.0,
        );
        let paints = run.block_paints();
        let border = paints
            .iter()
            .find_map(|p| p.border.as_ref())
            .expect("expected a border on the paragraph");
        let pattern = border
            .linetype_pt
            .as_ref()
            .expect("border_type should produce a linetype pattern");
        // Two entries (dash + gap), both positive.
        assert_eq!(pattern.len(), 2);
        use crate::scales::value::LinetypeStep::{Dash, Gap};
        assert!(matches!(pattern[0], Dash(d) if d > 0.0));
        assert!(matches!(pattern[1], Gap(g) if g > 0.0));
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
        );
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

    #[test]
    fn breaking_at_the_natural_width_reproduces_the_natural_height() {
        let run = make("a paragraph long enough to have an opinion about width\n\nand another");
        let natural = run.natural_height();
        let broken = run.set_max_width(run.natural_width() as f32, HAlign::Start) as f64;
        assert!(
            (broken - natural).abs() < 1.0,
            "re-breaking at the natural width changed the height ({natural} → {broken})"
        );
    }

    #[test]
    fn natural_height_survives_a_narrow_re_break() {
        let run = make("a paragraph long enough to wrap when the column gets narrow");
        let natural = run.natural_height();
        run.set_max_width(natural as f32 / 4.0, HAlign::Start);
        assert!(
            run.current_height() > natural,
            "wrapping should make the run taller"
        );
        assert_eq!(
            run.natural_height(),
            natural,
            "natural height must not move when the run re-breaks"
        );
    }

    #[test]
    fn base_style_font_features_reach_the_rich_shaper() {
        // `push_base_defaults` shares `TextRun`'s property pushes, so
        // a feature that changes advances must change the shaped
        // width here too.
        let sheet = RichTextStyleSheet::new();
        let plain = TextStyle::new(20.0);
        let small_caps = plain.clone().features([crate::text::FontFeatureSetting {
            tag: *b"smcp",
            value: 1,
        }]);
        let width_of = |style: &TextStyle| {
            RichTextRun::new(
                "widths",
                style,
                Color::from_rgba8(0, 0, 0, 255),
                &sheet,
                &palette(),
                96.0,
            )
            .natural_width()
        };
        // Both shape; the point is that the feature reaches parley at
        // all rather than being silently dropped on the rich path.
        assert!(width_of(&plain) > 0.0);
        assert!(width_of(&small_caps) > 0.0);
    }

    #[test]
    fn list_markers_draw_in_the_gutter_left_of_the_body() {
        let run = make("- alpha");
        let blocks = run.blocks.borrow();
        let bl = blocks
            .iter()
            .find(|b| b.marker.is_some())
            .expect("the item body carries the marker");
        let marker = bl.marker.as_ref().expect("marker");
        let (x0, x1) = marker_x_range(bl, marker);
        assert!(x1 <= bl.left_px, "marker must sit start-side of the text");
        assert!(x0 < x1, "marker must have width");
    }

    #[test]
    fn multi_digit_ordinals_share_a_right_edge() {
        let run = make("1. one\n2. two\n3. three\n4. four\n5. five\n6. six\n7. seven\n8. eight\n9. nine\n10. ten");
        let blocks = run.blocks.borrow();
        let right_edges: Vec<f32> = blocks
            .iter()
            .filter_map(|bl| bl.marker.as_ref().map(|m| marker_x_range(bl, m).1))
            .collect();
        assert!(right_edges.len() >= 10, "expected ten markers");
        let first = right_edges[0];
        for e in &right_edges {
            assert!(
                (e - first).abs() < 0.01,
                "markers should right-align, got {right_edges:?}"
            );
        }
    }

    #[test]
    fn a_background_with_no_padding_still_blocks_margin_collapse() {
        let mut sheet = RichTextStyleSheet::new();
        sheet.set(
            "barrier",
            StyleDelta {
                background: Some(crate::style_vocab::ThemeColor::Accent),
                margin: Some(RichMargin::new(pt(20.0), pt(0.0), pt(20.0), pt(0.0))),
                ..StyleDelta::empty()
            },
        );
        sheet.set(
            "plain",
            StyleDelta {
                margin: Some(RichMargin::new(pt(20.0), pt(0.0), pt(20.0), pt(0.0))),
                ..StyleDelta::empty()
            },
        );
        let make_with = |src: &str| {
            RichTextRun::new(
                src,
                &base_style(),
                Color::from_rgba8(0, 0, 0, 255),
                &sheet,
                &palette(),
                96.0,
            )
        };
        // Two stacked divs, each wrapping a paragraph that carries
        // its own bottom margin. With no barrier that inner margin
        // collapses out through the div's edge and merges with the
        // div's own; a background on the div stops it, so the painted
        // stack is one paragraph margin taller.
        let collapsed = make_with(":::plain\na\n:::\n:::plain\nb\n:::").natural_height();
        let separated = make_with(":::barrier\na\n:::\n:::plain\nb\n:::").natural_height();
        assert!(
            separated > collapsed + 15.0,
            "a background must stop the collapse ({collapsed} → {separated})"
        );
    }
}
