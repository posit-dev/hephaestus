//! The shaping pass: turn a reduced document into one positioned
//! [`BlockLayout`](super::run::BlockLayout) per leaf block.
//!
//! Leaves shape independently at their effective content width, then
//! [`stack_blocks`](super::wrap::stack_blocks) positions them
//! vertically with CSS margin collapsing. Container blocks
//! (blockquote / div / list / list item) never shape; they contribute
//! insets, spacing chains, and list markers to the leaves inside them.

use std::cell::RefCell;
use std::ops::Range;

use parley::{
    Alignment, AlignmentOptions, FontFamily, FontFamilyName, FontStyle, FontWeight, LayoutContext,
    StyleProperty,
};

use super::length::LineHeightSpec;
use super::reduce::{BaselineRun, Block, BlockKind, BuiltRuns, InlineRun};
use super::run::{BlockLayout, MarkerLayout, RichBrush, RichTextRun};
use super::style::ResolvedStyle;
use super::wrap::{stack_blocks, EdgeSpacing};
use crate::color::Color;
use crate::style_vocab::{HAlign, Palette};
use crate::text::shape_common::{generic_family_from_str, parley_features, push_style_defaults};
use crate::text::TextStyle;

/// Distance between a list-item marker and the item's content edge,
/// as a fraction of the item's em. Matches marquee.
const MARKER_GAP_EM: f64 = 0.25;

thread_local! {
    /// Parley's shaping scratch space, reused across blocks.
    /// `LayoutContext::new` allocates several caches, and a document
    /// shapes one block at a time, so a fresh context per block threw
    /// that work away every time.
    ///
    /// Held per thread rather than globally because it's `&mut` for
    /// the whole build; it pairs with the process-global
    /// [`font_context`](crate::text::font_context) mutex, which is
    /// taken first and released after the layout is built.
    static RICH_LAYOUT_CONTEXT: RefCell<LayoutContext<RichBrush>> =
        RefCell::new(LayoutContext::new());
}

pub(crate) fn shape_run(
    runs: BuiltRuns,
    base_style: &TextStyle,
    base_brush: Color,
    palette: &Palette,
    dpi: f64,
) -> RichTextRun {
    // Split blocks into leaves + containers. Leaves are the shape
    // units.
    let all_blocks = runs.blocks;
    let leaves = top_level_leaves(&all_blocks);
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

    RichTextRun {
        blocks: RefCell::new(layouts),
        containers,
        base_style: base_style.clone(),
        palette: *palette,
        base_brush,
        dpi,
        last_break: RefCell::new(None),
        derived: RefCell::new(None),
        natural_width_px: natural_width,
        natural_height_px: total_height,
        current_height_px: RefCell::new(total_height),
        min_width_px: min_width,
    }
}
// ─── Per-block shaping ──────────────────────────────────────────────────────

pub(crate) fn shape_block_layout(
    text: &str,
    inlines: &[InlineRun],
    base_style: &TextStyle,
    base_brush: Color,
    palette: &Palette,
    dpi: f64,
) -> parley::Layout<RichBrush> {
    let fcx_mutex = crate::text::font_context();
    let mut fcx = fcx_mutex.lock().expect("font context poisoned");
    RICH_LAYOUT_CONTEXT.with(|lcx| {
        let mut lcx = lcx.borrow_mut();
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
    })
}

pub(crate) fn push_base_defaults(
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
pub(crate) fn apply_style_range(
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
pub(crate) fn edge_spacing(style: &ResolvedStyle, side: usize, dpi: f64) -> EdgeSpacing {
    let padding_px = pt_to_px(style.padding_pt[side], dpi);
    let paints = style.background.is_some()
        || (style.border_color.is_some() && style.border_width_pt[side] > 0.0);
    EdgeSpacing {
        margin_px: pt_to_px(style.margin_pt[side], dpi),
        barrier: padding_px != 0.0 || paints,
        padding_px,
    }
}

pub(crate) fn is_leaf_kind(kind: &BlockKind) -> bool {
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
/// Filter `all` down to the leaf blocks that aren't nested inside
/// another leaf, in document order.
///
/// A sort plus a running-max sweep answers "is anything already
/// covering me" in one pass; the pairwise form is quadratic, and a
/// document with many short paragraphs has many leaves.
pub(crate) fn top_level_leaves(all: &[Block]) -> Vec<Block> {
    let mut leaves: Vec<&Block> = all.iter().filter(|b| is_leaf_kind(&b.kind)).collect();
    // Widest-first at a shared start, so a container-like leaf is seen
    // before anything it covers.
    leaves.sort_by_key(|b| (b.range.start, std::cmp::Reverse(b.range.end)));
    let mut out: Vec<Block> = Vec::with_capacity(leaves.len());
    // The furthest end any *strictly wider* leaf has reached so far.
    let mut covered_to: usize = 0;
    let mut covered_from: usize = 0;
    let mut seen_any = false;
    for b in leaves {
        let is_empty = b.range.start == b.range.end;
        let covered = if is_empty {
            // A zero-length block (a rule) is only nested when another
            // leaf strictly straddles it.
            seen_any && covered_from < b.range.start && covered_to > b.range.end
        } else {
            seen_any && covered_to >= b.range.end && covered_from <= b.range.start
        };
        if !covered {
            out.push(b.clone());
        }
        if !is_empty && (!seen_any || b.range.end > covered_to) {
            covered_from = b.range.start;
            covered_to = b.range.end;
            seen_any = true;
        }
    }
    out.sort_by_key(|b| b.range.start);
    out
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

pub(crate) fn ancestor_side_padding_pt(ancestors: &[&Block]) -> (f64, f64) {
    let mut left = 0.0;
    let mut right = 0.0;
    for a in ancestors {
        left += a.style.padding_pt[3];
        right += a.style.padding_pt[1];
    }
    (left, right)
}

pub(crate) fn pt_to_px(pt: f64, dpi: f64) -> f32 {
    (pt * dpi / 72.0) as f32
}

/// Map hephaestus's [`HAlign`] to parley's [`Alignment`] using our
/// own resolved block-axis direction. Uses parley's **physical**
/// `Left` / `Right` variants (never the direction-aware `Start` /
/// `End`) so an explicit
/// [`super::style::Direction::Ltr`] / [`super::style::Direction::Rtl`]
/// on a block wins even when parley's UBA infers the opposite from
/// the source text.
pub(crate) fn hal_to_alignment(a: HAlign, is_rtl: bool) -> Alignment {
    match (a, is_rtl) {
        (HAlign::Start, false) | (HAlign::End, true) => Alignment::Left,
        (HAlign::End, false) | (HAlign::Start, true) => Alignment::Right,
        (HAlign::Center, _) => Alignment::Center,
        (HAlign::Justify, _) => Alignment::Justify,
    }
}

/// Slice text + inline runs + baseline shifts to the block's byte
/// range, rebasing ranges to block-local coordinates.
pub(crate) fn slice_block(
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
